use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use mio::{event::Source, Token};
use sharded_slab::Slab;
use std::{
    ops::{Deref, DerefMut},
    sync::{Arc, RwLock},
    time::Duration,
};

use crate::{context, Reactor};

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
    pub poll: RwLock<mio::Poll>,
    pub events: RwLock<mio::Events>,
    pub tasks: Slab<UnboundedSender<Event>>,
}

impl Default for Os {
    fn default() -> Self {
        Self {
            // token: AtomicUsize::new(0),
            poll: RwLock::new(mio::Poll::new().unwrap()),
            events: RwLock::new(mio::Events::with_capacity(128)),
            tasks: Slab::new(),
        }
    }
}

impl Os {
    /// Polls the OS for new events, and dispatches those to any awaiting tasks
    pub(crate) fn process(&self) {
        {
            let mut events = self.events.write().unwrap();
            let mut poll = self.poll.write().unwrap();
            poll.poll(&mut events, Some(Duration::from_micros(100)))
                .unwrap();
        }

        for event in &*self.events.read().unwrap() {
            if let Some(sender) = self.tasks.get(event.token().0) {
                if sender.unbounded_send(event.into()).is_err() {
                    // error means the receiver was dropped
                    // which means the registration should have been de-registered
                    self.tasks.remove(event.token().0);
                }
            }
        }
    }
}

pub(crate) struct Registration<S: Source> {
    pub reactor: Arc<Reactor>,
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
        let reactor = context(Arc::clone);
        let (sender, events) = unbounded();
        let token = Token(reactor.os.tasks.insert(sender).unwrap());
        {
            let poll = reactor.os.poll.read().unwrap();
            poll.registry().register(&mut source, token, interests)?;
        }
        Ok(Self {
            reactor,
            events,
            token,
            source,
        })
    }
}

impl<S: Source> Drop for Registration<S> {
    fn drop(&mut self) {
        // deregister the source from the OS
        let poll = self.reactor.os.poll.read().unwrap();
        poll.registry().deregister(&mut self.source).unwrap();
        // remove the event dispatcher
        self.reactor.os.tasks.remove(self.token.0);
    }
}
