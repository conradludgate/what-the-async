#![forbid(unsafe_code)]
#![warn(clippy::pedantic)]
#![allow(clippy::missing_panics_doc)]

use std::{
    cell::RefCell,
    future::Future,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    task::{Context, Wake, Waker},
    thread::{self, Thread},
};

mod driver;

use crossbeam::deque::{Injector, Worker};
use driver::{Parker, Unparker};
use futures::pin_mut;
pub use wta_executor::JoinHandle;
use wta_executor::{GlobalExecutor, LocalExecutor, Park, Handle};
use wta_reactor::Driver;
pub use wta_reactor::{net, timers};

#[derive(Clone)]
pub struct Runtime {
    global: GlobalExecutor<Unparker>,
    handle: <Driver as Park>::Handle,
    // parker: Parker,
}

impl Default for Runtime {
    fn default() -> Self {
        // queues
        let injector = Arc::new(Injector::new());
        let mut stealers = vec![];
        let mut unparkers = vec![];

        let mut workers = vec![];

        let driver = Driver::default();
        let handle = driver.handle();
        let parker = Parker::new(driver);

        // let executor: Arc<Executor> = Arc::default();
        // let reactor: Arc<Reactor> = Arc::default();
        // let this = Self { executor, reactor };

        let n = std::thread::available_parallelism().map_or(4, std::num::NonZeroUsize::get);
        for _ in 0..n {
            let worker = Worker::new_fifo();
            stealers.push(worker.stealer());

            let parker = parker.clone();
            unparkers.push(parker.unpark());

            workers.push((worker, parker));

            // this.spawn_worker(format!("wta-worker-{}", i));
        }

        let global = GlobalExecutor::new(injector, stealers, unparkers);

        for (i, (worker, parker)) in workers.into_iter().enumerate() {
            let mut local = LocalExecutor::new(i, worker, global.clone(), parker);
            let global = global.clone();
            let handle = handle.clone();
            std::thread::Builder::new()
                .name(format!("wta-worker-{}", i))
                .spawn(move || {
                    // eprintln!("thread {thread:?} {i}", thread = std::thread::current());
                    // this.register();
                    GLOBAL_EXECUTOR.with(|cell| *cell.borrow_mut() = Some(global));
                    handle.register();

                    let mut i = 0;
                    loop {
                        local.poll_once();

                        i += 1;
                        i %= 61;
                        if i == 0 {
                            local.maintenance();
                        }
                    }
                })
                .unwrap();
        }

        Self { global, handle }
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

pub fn spawn<F>(fut: F) -> JoinHandle<F::Output>
where
    F: Future + Send + Sync + 'static,
    F::Output: Send,
{
    context(|e| e.spawn(fut))
}

impl Runtime {
    pub fn block_on<F>(&mut self, fut: F) -> F::Output
    where
        F: Future,
    {
        GLOBAL_EXECUTOR.with(|cell| *cell.borrow_mut() = Some(self.global.clone()));
        self.handle.register();

        let ready = Arc::new(AtomicBool::new(true));
        let waker = Waker::from(Arc::new(MainWaker {
            ready: ready.clone(),
            thread: std::thread::current(),
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
                thread::park();
            }
        }
    }

    pub fn spawn<F>(&self, fut: F) -> JoinHandle<F::Output>
    where
        F: Future + Sync + Send + 'static,
        F::Output: Send,
    {
        self.global.spawn(fut)
    }
}

struct MainWaker {
    ready: Arc<AtomicBool>,
    thread: Thread,
}

impl Wake for MainWaker {
    fn wake(self: Arc<Self>) {
        self.ready.store(true, Ordering::Relaxed);
        self.thread.unpark();
    }
}

thread_local! {
    static GLOBAL_EXECUTOR: RefCell<Option<GlobalExecutor<Unparker>>> = RefCell::new(None);
}

fn context<R>(f: impl FnOnce(&GlobalExecutor<Unparker>) -> R) -> R {
    GLOBAL_EXECUTOR.with(|e| {
        let e = e.borrow();
        let e = e
            .as_ref()
            .expect("spawn called outside of an executor context");
        f(e)
    })
}
