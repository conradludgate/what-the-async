// #![forbid(unsafe_code)]
#![warn(clippy::pedantic)]
#![allow(clippy::missing_panics_doc)]

use std::{
    future::Future,
    panic::{resume_unwind, AssertUnwindSafe},
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    task::{Context, Wake, Waker},
};

use futures::{pin_mut, FutureExt};
use wta_executor::Executor;
pub use wta_executor::{spawn, JoinHandle};
use wta_reactor::Reactor;
pub use wta_reactor::{net, timers};

mod oneshot;
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

pub async fn spawn2<F1, F2>(f1: F1, f2: F2) -> (F1::Output, F2::Output)
where
    F1: Future + Send + Sync,
    F1::Output: Send,
    F2: Future,
{
    let ([o1], o2) = spawn_n([f1], f2).await;
    (o1, o2)
}

pub async fn spawn_n<F1, F2, const N: usize>(f1: [F1; N], f2: F2) -> ([F1::Output; N], F2::Output)
where
    F1: Future + Send + Sync,
    F1::Output: Send,
    F2: Future,
{
    use cl_generic_vec::ArrayVec;
    let (o1, o2) = spawn_storage(ArrayVec::from(f1), f2).await;
    (unsafe { o1.into_array_unchecked() }, o2)
}

pub async fn spawn_vec<F1, F2, const N: usize>(f1: Vec<F1>, f2: F2) -> (Vec<F1::Output>, F2::Output)
where
    F1: Future + Send + Sync,
    F1::Output: Send,
    F2: Future,
{
    use cl_generic_vec::HeapVec;
    let (o1, o2) = spawn_storage(HeapVec::from(f1), f2).await;
    (o1.into(), o2)
}

use cl_generic_vec::{raw::StorageWithCapacity, SimpleVec};

#[repr(transparent)]
pub struct Exclusive<T: ?Sized> {
    inner: T,
}
unsafe impl<T: ?Sized> Sync for Exclusive<T> {}

pub async fn spawn_storage<S, F2>(
    f1: SimpleVec<S>,
    f2: F2,
) -> (
    SimpleVec<S::Storage<<S::Item as Future>::Output>>,
    F2::Output,
)
where
    S: StorageWithCapacity,
    S::Item: Future + Send + Sync,
    <S::Item as Future>::Output: Send,
    F2: Future,
{
    use oneshot::Inner;
    use wta_executor::spawn_mut;

    let n = f1.len();

    let inners: SimpleVec<S::Storage<Inner<<S::Item as Future>::Output>>> =
        (0..n).map(|_| Inner::new()).collect();
    let mut futs = SimpleVec::<S::Storage<_>>::with_capacity(n);

    for (f1, inner) in f1.into_iter().zip(&inners) {
        let sender = inner.sender();
        let fut = async move { sender.send(f1.await) };
        unsafe {
            futs.push_unchecked(fut);
        }
    }

    for fut in &mut futs {
        unsafe { spawn_mut(Pin::new_unchecked(fut)) }
    }

    let result = Exclusive{ inner: AssertUnwindSafe(f2).catch_unwind().await };

    let mut spawn_results = SimpleVec::<S::Storage<_>>::with_capacity(n);
    for inner in &inners {
        unsafe {
            spawn_results.push_unchecked(inner.recv().await);
        }
    }

    let o2 = match result.inner {
        Ok(o2) => o2,
        Err(e) => resume_unwind(e),
    };

    let mut o1s = SimpleVec::<S::Storage<_>>::with_capacity(n);
    for result in spawn_results {
        match result {
            Ok(o1) => unsafe {
                o1s.push_unchecked(o1);
            },
            _ => panic!("a scoped task panicked"),
        }
    }

    (o1s, o2)
}
