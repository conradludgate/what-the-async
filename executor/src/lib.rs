// #![forbid(unsafe_code)]
#![warn(clippy::pedantic)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]

use std::{
    cell::RefCell,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, Wake, Waker},
    thread::Thread,
};

use crossbeam_queue::SegQueue;
use futures::{channel::oneshot, FutureExt};

pub type Task = MaybeBoxMut<'static, dyn Future<Output = ()> + Send + Sync>;

pub enum MaybeBoxMut<'a, F: ?Sized + 'a> {
    Boxed(Pin<Box<F>>),
    Borrow(Pin<&'a mut F>),
}
impl<'a, F: ?Sized + 'a> MaybeBoxMut<'a, F> {
    fn as_pin_mut(&mut self) -> Pin<&mut F> {
        match *self {
            MaybeBoxMut::Boxed(ref mut b) => b.as_mut(),
            MaybeBoxMut::Borrow(ref mut b) => b.as_mut(),
        }
    }
}

thread_local! {
    static EXECUTOR: RefCell<Option<Arc<Executor>>> = RefCell::new(None);
}

pub(crate) fn context<R>(f: impl FnOnce(&Arc<Executor>) -> R) -> R {
    EXECUTOR.with(|e| {
        let e = e.borrow();
        let e = e
            .as_ref()
            .expect("spawn called outside of an executor context");
        f(e)
    })
}

pub fn spawn<F>(fut: F) -> JoinHandle<F::Output>
where
    F: Future + Send + Sync + 'static,
    F::Output: Send,
{
    context(|e| e.spawn(fut))
}

/// # Safety
/// The future must not drop while the task is running
pub unsafe fn spawn_mut<'a, F>(fut: Pin<&'a mut F>)
where
    F: Future<Output = ()> + Send + Sync + 'a,
{
    context(|e| e.spawn_mut(fut));
}

#[derive(Default)]
pub struct Executor {
    tasks: SegQueue<Task>,
    threads: SegQueue<Thread>,
}

impl Executor {
    /// register this executor on the current thread
    pub fn register(self: &Arc<Self>) {
        EXECUTOR.with(|exec| *exec.borrow_mut() = Some(self.clone()));
    }

    fn wake(&self, task: Task) {
        self.tasks.push(task);
        // self.condvar.notify_one();
        if let Some(t) = self.threads.pop() {
            t.unpark();
        };
    }

    pub fn poll_once(self: Arc<Self>) {
        // Take one task from the queue.
        let mut task = {
            loop {
                // try acquire a task from the queue
                if let Some(task) = self.tasks.pop() {
                    break task;
                }
                // park this thread
                self.threads.push(std::thread::current());
                std::thread::park();
            }
        };

        let wake = Arc::new(TaskWaker {
            task: Mutex::new(None),
            executor: self,
        });
        let waker = Waker::from(wake.clone());
        let mut cx = Context::from_waker(&waker);

        if task.as_pin_mut().poll(&mut cx).is_pending() {
            wake.task.lock().unwrap().replace(task);
        }
    }

    pub fn spawn<F>(&self, fut: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + Sync + 'static,
        F::Output: Send,
    {
        let (sender, handle) = JoinHandle::new();

        // Pin the future. Also wrap it s.t. it sends it's output over the channel
        let fut = Box::pin(fut.map(|out| sender.send(out).unwrap_or_default()));
        // insert the task into the runtime and signal that it is ready for processing
        self.wake(MaybeBoxMut::Boxed(fut));

        // return the handle to the spawner so that it can be `await`ed with it's output value
        handle
    }

    /// # Safety
    /// The future must not drop while the task is running
    pub unsafe fn spawn_mut<'a, F>(&self, fut: Pin<&'a mut F>)
    where
        F: Future<Output = ()> + Send + Sync + 'a,
    {
        let static_fut = std::mem::transmute::<
            Pin<&'a mut (dyn Future<Output = ()> + Send + Sync)>,
            Pin<&'static mut (dyn Future<Output = ()> + Send + Sync)>,
        >(fut);
        self.wake(MaybeBoxMut::Borrow(static_fut));
    }
}

struct TaskWaker {
    executor: Arc<Executor>,
    task: Mutex<Option<Task>>,
}

impl Wake for TaskWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        if let Some(task) = self.task.lock().unwrap().take() {
            self.executor.wake(task);
        }
    }
}

pub struct JoinHandle<R>(oneshot::Receiver<R>);

impl<R> Unpin for JoinHandle<R> {}

impl<R> JoinHandle<R> {
    #[must_use]
    pub fn new() -> (oneshot::Sender<R>, Self) {
        let (sender, receiver) = oneshot::channel();
        (sender, Self(receiver))
    }
}

impl<R> Future for JoinHandle<R> {
    type Output = R;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // poll the inner channel for the spawned future's result
        self.0.poll_unpin(cx).map(Result::unwrap)
    }
}
