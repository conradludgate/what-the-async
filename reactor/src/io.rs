use mio::{event::Source, Token};
use sharded_slab::Slab;
use std::{
    cell::RefCell,
    ops::{Deref, DerefMut},
    time::Duration,
};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

#[cfg(loom)]
use loom::{sync::Arc, thread_local};
#[cfg(not(loom))]
use std::{sync::Arc, thread_local};

use wta_executor::{Handle, Park, Unpark};

const WAKE_TOKEN: Token = Token(usize::MAX);

pub struct Driver {
    os: Os,
    waker: Arc<mio::Waker>,
}

impl Default for Driver {
    fn default() -> Self {
        let os = Os::default();
        os.driver()
    }
}

impl Driver {
    pub fn handle(&self) -> Arc<Registry> {
        self.os.registry.clone()
    }
}

impl Park for Driver {
    type Unpark = Waker;

    fn unpark(&self) -> Self::Unpark {
        Waker(self.waker.clone())
    }

    fn park(&mut self) {
        self.os.process(None);
    }

    fn park_timeout(&mut self, duration: Duration) {
        self.os.process(Some(duration));
    }

    type Handle = RegistryHandle;

    fn handle(&self) -> Self::Handle {
        RegistryHandle(self.os.registry.clone())
    }
}

pub struct Waker(Arc<mio::Waker>);
impl Unpark for Waker {
    fn unpark(&self) {
        dbg!("io wakeup");
        self.0.wake().unwrap();
    }
}

#[derive(Debug)]
pub(crate) struct Event(u8);
impl Event {
    pub fn is_readable(&self) -> bool {
        self.0 & 1 != 0 || self.is_read_closed()
    }
    pub fn is_writable(&self) -> bool {
        self.0 & 2 != 0 || self.is_write_closed()
    }
    pub fn is_read_closed(&self) -> bool {
        self.0 & 4 != 0
    }
    pub fn is_write_closed(&self) -> bool {
        self.0 & 8 != 0
    }
}

impl From<&mio::event::Event> for Event {
    fn from(e: &mio::event::Event) -> Self {
        let mut event = 0;
        event |= u8::from(e.is_readable());
        event |= u8::from(e.is_writable()) << 1;
        event |= u8::from(e.is_read_closed()) << 2;
        event |= u8::from(e.is_write_closed()) << 3;
        Event(event)
    }
}

pub(crate) struct Os {
    pub poll: mio::Poll,
    pub events: mio::Events,
    registry: Arc<Registry>,
}

impl Default for Os {
    fn default() -> Self {
        let poll = mio::Poll::new().unwrap();
        let registry = poll.registry().try_clone().unwrap();
        Self {
            poll,
            events: mio::Events::with_capacity(128),
            registry: Arc::new(Registry {
                registry,
                tasks: Slab::new(),
            }),
        }
    }
}

impl Os {
    pub fn driver(self) -> Driver {
        let waker = Arc::new(mio::Waker::new(&self.registry.registry, WAKE_TOKEN).unwrap());
        Driver { os: self, waker }
    }

    /// Polls the OS for new events, and dispatches those to any awaiting tasks
    pub(crate) fn process(&mut self, duration: Option<Duration>) {
        dbg!("io process", &duration);
        match self.poll.poll(&mut self.events, duration) {
            Ok(_) => {}
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => return,
            Err(e) => Err(e).unwrap(),
        }

        for event in &self.events {
            dbg!("os event found", event.token());
            if let Some(sender) = self.registry.tasks.get(event.token().0) {
                if sender.send(event.into()).is_err() {
                    // error means the receiver was dropped
                    // which means the registration should have been de-registered
                    self.registry.tasks.remove(event.token().0);
                }
            }
        }
    }
}

pub(crate) struct Registration<S: Source> {
    registry: Arc<Registry>,
    pub events: UnboundedReceiver<Event>,
    pub token: mio::Token,
    pub source: S,
}

// allow internal access to the source
impl<S: Source> Deref for Registration<S> {
    type Target = S;

    fn deref(&self) -> &Self::Target {
        &self.source
    }
}
impl<S: Source> DerefMut for Registration<S> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.source
    }
}

impl<S: Source> Registration<S> {
    pub fn new(mut source: S, interests: mio::Interest) -> std::io::Result<Self> {
        let (sender, events) = unbounded_channel();
        context(|r| {
            let token = Token(r.tasks.insert(sender).unwrap());
            r.registry.register(&mut source, token, interests)?;
            Ok(Self {
                registry: r.clone(),
                events,
                token,
                source,
            })
        })
    }
}

impl<S: Source> Drop for Registration<S> {
    fn drop(&mut self) {
        // deregister the source from the OS
        self.registry.registry.deregister(&mut self.source).unwrap();
        // remove the event dispatcher
        self.registry.tasks.remove(self.token.0);
    }
}

pub struct Registry {
    registry: mio::Registry,
    tasks: Slab<UnboundedSender<Event>>,
}

thread_local! {
    static OS: RefCell<Option<Arc<Registry>>> = RefCell::new(None);
}

#[derive(Clone)]
pub struct RegistryHandle(Arc<Registry>);
impl Handle for RegistryHandle {
    fn register(self) {
        OS.with(|r| *r.borrow_mut() = Some(self.0));
    }
}

fn context<R>(f: impl FnOnce(&Arc<Registry>) -> R) -> R {
    OS.with(|r| {
        let r = r.borrow();
        let r = r.as_ref().expect("called outside of an reactor context");
        f(r)
    })
}
