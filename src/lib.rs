#![forbid(unsafe_code)]
#![warn(clippy::pedantic)]
#![allow(clippy::missing_panics_doc)]

use std::{cell::RefCell, future::Future, sync::atomic::Ordering, task::Context};

#[cfg(loom)]
use loom::{
    sync::{atomic::AtomicBool, Arc},
    thread, thread_local,
};
#[cfg(not(loom))]
use std::{
    sync::{atomic::AtomicBool, Arc},
    thread, thread_local,
};

mod driver;

use crossbeam_deque::{Injector, Worker};
use driver::{Parker, Unparker};
use futures_util::pin_mut;
pub use wta_executor::JoinHandle;
use wta_executor::{GlobalExecutor, Handle, LocalExecutor, Park, Unpark};
#[cfg(feature = "io")]
pub use wta_reactor::net;
pub use wta_reactor::timers;
use wta_reactor::Driver;

#[derive(Clone)]
pub struct Runtime {
    global: GlobalExecutor<Unparker>,
    handle: <Driver as Park>::Handle,
    parker: Parker,
}

impl Default for Runtime {
    fn default() -> Self {
        let n = std::thread::available_parallelism().map_or(4, std::num::NonZeroUsize::get);
        Self::new(n)
    }
}

impl Runtime {
    #[must_use]
    pub fn new(n_workers: usize) -> Self {
        let injector = Arc::new(Injector::new());
        let mut stealers = vec![];
        let mut unparkers = vec![];

        let mut workers = vec![];

        let driver = Driver::default();
        let handle = driver.handle();
        let parker = Parker::new(driver);

        for _ in 0..n_workers {
            let worker = Worker::new_fifo();
            stealers.push(worker.stealer());

            let parker = parker.clone();
            unparkers.push(parker.unpark());

            workers.push((worker, parker));
        }

        let global = GlobalExecutor::new(injector, stealers, unparkers);

        for (i, (worker, parker)) in workers.into_iter().enumerate() {
            let mut local = LocalExecutor::new(i, worker, global.clone(), parker);
            let global = global.clone();
            let handle = handle.clone();
            thread::Builder::new()
                .name(format!("wta-worker-{}", i))
                .spawn(move || {
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

        Self {
            global,
            handle,
            parker,
        }
    }
}

/// Spawns a blocking function in a new thread, and returns an async handle to it's result.
pub fn spawn_blocking<F, R>(f: F) -> JoinHandle<F::Output>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    let (sender, handle) = JoinHandle::new();

    thread::Builder::new()
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
        self.handle.clone().register();

        let ready = Arc::new(AtomicBool::new(true));
        let waker = Arc::new(MainWaker {
            ready: ready.clone(),
            thread: self.parker.unpark(),
        });
        #[cfg(not(loom))]
        let waker = std::task::Waker::from(waker);
        #[cfg(loom)]
        let waker = wta_executor::loom_compat::waker(waker);
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
                self.parker.park();
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
    thread: Unparker,
}

#[cfg(not(loom))]
impl std::task::Wake for MainWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.ready.store(true, Ordering::Relaxed);
        self.thread.unpark();
    }
}
#[cfg(loom)]
impl wta_executor::loom_compat::Wake for MainWaker {
    fn wake_by_ref(this: &Arc<Self>) {
        this.ready.store(true, Ordering::Relaxed);
        this.thread.unpark();
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

#[cfg(loom)]
#[cfg(test)]
mod test_loom;

#[cfg(loom)]
#[cfg(test)]
mod loom_tests {
    #[test]
    fn timers() {
        use futures_util::future::join_all;
        use std::time::Duration;

        async fn spawn_task(i: usize) -> Duration {
            crate::spawn(async move {
                let dur = Duration::from_millis(100) * (i as u32);
                println!(
                    "task {i}, thread {thread_id:?}, dur {dur:?}",
                    thread_id = std::thread::current().id()
                );
                crate::timers::Sleep::duration(dur).await;
                println!(
                    "task {i}, thread {thread_id:?}, done",
                    thread_id = std::thread::current().id()
                );
                dur
            })
            .await
        }

        crate::test_loom::model(|| {
            let mut runtime = crate::Runtime::new(2);
            let dur: Duration = runtime.block_on(async {
                let mut handles = vec![];
                for i in 0..10 {
                    let handle = spawn_task(i);
                    handles.push(handle);
                }
                join_all(handles).await.into_iter().sum()
            });

            assert_eq!(dur, Duration::from_millis(4500));
        });
    }
}
