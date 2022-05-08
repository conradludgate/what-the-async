#![forbid(unsafe_code)]

use std::{
    future::Future,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    task::{Context, Wake, Waker},
};

use futures::pin_mut;
use wta_executor::{Executor, JoinHandle};

pub struct Runtime {
    executor: Arc<Executor>,
}

impl Default for Runtime {
    fn default() -> Self {
        let executor: Arc<Executor> = Arc::default();
        let n = std::thread::available_parallelism().map_or(4, |t| t.get());
        for i in 0..n {
            let executor = executor.clone();
            std::thread::Builder::new()
                .name(format!("wta-{}", i))
                .spawn(move || {
                    executor.register();
                    loop {
                        executor.clone().poll_once()
                    }
                })
                .unwrap();
        }

        Self { executor }
    }
}

pub fn spawn<F>(fut: F) -> JoinHandle<F::Output>
where
    F: Future + Send + Sync + 'static,
    F::Output: Send,
{
    wta_executor::spawn(fut)
}

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
    pub fn block_on<F>(&self, fut: F) -> F::Output
    where
        F: Future,
    {
        self.executor.register();

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
        self.ready.store(true, Ordering::Relaxed)
    }
}
