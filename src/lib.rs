#![forbid(unsafe_code)]
#![warn(clippy::pedantic)]
#![allow(clippy::missing_panics_doc)]

use std::{
    future::Future,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    task::{Context, Wake, Waker},
};

use futures::pin_mut;
use wta_executor::Executor;
pub use wta_executor::{spawn, JoinHandle};
use wta_reactor::Reactor;
pub use wta_reactor::{net, timers};

#[derive(Clone)]
pub struct Runtime {
    executor: Arc<Executor>,
    reactor: Arc<Reactor>,
}

impl Default for Runtime {
    fn default() -> Self {
        let executor: Arc<Executor> = Arc::default();
        let reactor: Arc<Reactor> = Arc::default();
        let this = Self { executor, reactor };

        let n = std::thread::available_parallelism().map_or(4, std::num::NonZeroUsize::get);
        for i in 0..n {
            this.spawn_worker(format!("wta-worker-{}", i));
        }

        this
    }
}

/// Spawns a blocking function in a new thread, and returns an async handle to it's result.
pub fn spawn_blocking<F, R>(f: F) -> JoinHandle<F::Output>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    let (sender, handle) = JoinHandle::new();

    std::thread::Builder::new()
        .spawn(move || sender.send(f()).unwrap_or_default())
        .unwrap();

    // return the handle to the reciever so that it can be `await`ed with it's output value
    handle
}

impl Runtime {
    fn register(&self) {
        self.executor.register();
        self.reactor.register();
    }

    pub fn spawn_worker(&self, name: String) {
        let this = self.clone();
        std::thread::Builder::new()
            .name(name)
            .spawn(move || {
                this.register();
                loop {
                    this.executor.clone().poll_once();
                }
            })
            .unwrap();
    }

    pub fn block_on<F>(&self, fut: F) -> F::Output
    where
        F: Future,
    {
        self.register();

        let ready = Arc::new(AtomicBool::new(true));
        let waker = Waker::from(Arc::new(MainWaker {
            ready: ready.clone(),
        }));
        let mut cx = Context::from_waker(&waker);

        pin_mut!(fut);

        loop {
            // see if the future is ready, replacing it with false
            if ready.load(Ordering::Relaxed) {
                match fut.as_mut().poll(&mut cx) {
                    std::task::Poll::Ready(r) => break r,
                    std::task::Poll::Pending => ready.store(false, Ordering::SeqCst),
                }
            } else {
                self.reactor.book_keeping();
            }
        }
    }

    pub fn spawn<F>(&self, fut: F) -> JoinHandle<F::Output>
    where
        F: Future + Sync + Send + 'static,
        F::Output: Send,
    {
        self.executor.spawn(fut)
    }
}

struct MainWaker {
    ready: Arc<AtomicBool>,
}

impl Wake for MainWaker {
    fn wake(self: Arc<Self>) {
        self.ready.store(true, Ordering::Relaxed);
    }
}
