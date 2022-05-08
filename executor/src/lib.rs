#![forbid(unsafe_code)]

use std::{
    cell::RefCell,
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::{Arc, Condvar, Mutex},
    task::{Context, Poll, Wake, Waker},
};

use futures::{channel::oneshot, FutureExt};

pub type Task = Pin<Box<dyn Future<Output = ()> + Send + Sync + 'static>>;

thread_local! {
    static EXECUTOR: RefCell<Option<Arc<Executor>>> = RefCell::new(None);
}

pub(crate) fn context<R>(f: impl FnOnce(&Arc<Executor>) -> R) -> R {
    EXECUTOR.with(|exec| {
        let exec = exec.borrow();
        let exec = exec
            .as_ref()
            .expect("spawn called outside of an executor context");
        f(exec)
    })
}

pub fn spawn<F>(fut: F) -> JoinHandle<F::Output>
where
    F: Future + Send + Sync + 'static,
    F::Output: Send,
{
    context(|e| e.spawn(fut))
}

#[derive(Default)]
pub struct Executor {
    tasks: Mutex<VecDeque<Task>>,
    condvar: Condvar,
}

impl Executor {
    /// register this executor on the current thread
    pub fn register(self: &Arc<Self>) {
        EXECUTOR.with(|exec| *exec.borrow_mut() = Some(self.clone()));
    }

    fn wake(&self, task: Task) {
        self.tasks.lock().unwrap().push_back(task);
        self.condvar.notify_one();
    }

    pub fn poll_once(self: Arc<Self>) {
        // Take one task from the queue.
        let mut task = {
            // wait for the tasks queue to be unlocked
            let mut tasks = self.tasks.lock().unwrap();
            loop {
                // try acquire a task from the front
                if let Some(task) = tasks.pop_front() {
                    break task;
                } else {
                    // if no tasks, pause this thread until someone wakes us back up
                    tasks = self.condvar.wait(tasks).unwrap();
                }
            }
        };

        let task_ref = Arc::new(Mutex::new(None));
        let waker = Waker::from(Arc::new(TaskWaker {
            task: task_ref.clone(),
            executor: self,
        }));
        let mut cx = Context::from_waker(&waker);

        if task.as_mut().poll(&mut cx).is_pending() {
            task_ref.lock().unwrap().replace(task);
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
        self.wake(fut);

        // return the handle to the spawner so that it can be `await`ed with it's output value
        handle
    }
}

struct TaskWaker {
    executor: Arc<Executor>,
    task: Arc<Mutex<Option<Task>>>,
}

impl Wake for TaskWaker {
    fn wake(self: Arc<Self>) {
        if let Some(task) = self.task.lock().unwrap().take() {
            self.executor.wake(task);
        }
    }
}

pub struct JoinHandle<R>(oneshot::Receiver<R>);

impl<R> Unpin for JoinHandle<R> {}

impl<R> JoinHandle<R> {
    pub fn new() -> (oneshot::Sender<R>, Self) {
        let (sender, receiver) = oneshot::channel();
        (sender, Self(receiver))
    }
}

impl<R> Future for JoinHandle<R> {
    type Output = R;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // poll the inner channel for the spawned future's result
        self.0.poll_unpin(cx).map(|x| x.unwrap())
    }
}
