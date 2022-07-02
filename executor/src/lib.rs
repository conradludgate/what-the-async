#![cfg_attr(not(loom), forbid(unsafe_code))]
#![warn(clippy::pedantic)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]

use std::{
    future::Future,
    pin::Pin,
    sync::PoisonError,
    task::{Context, Poll},
    time::Duration,
};

#[cfg(loom)]
use loom::sync::{Arc, Mutex};
#[cfg(loom)]
pub mod loom_compat;
#[cfg(not(loom))]
use std::sync::{Arc, Mutex};

fn arc_slice<T>(v: Vec<T>) -> Arc<[T]> {
    let arc: std::sync::Arc<[T]> = v.into();
    #[cfg(loom)]
    let arc = Arc::from_std(arc);
    arc
}

use crossbeam_deque::{Injector, Steal, Stealer, Worker};
pub use parker::{Handle, Park, Unpark};

mod oneshot;
mod parker;
pub mod thread;

pub type Task = Pin<Box<dyn Future<Output = ()> + Send + Sync + 'static>>;

pub struct LocalExecutor<P: Park> {
    id: usize,
    queue: Worker<Task>,
    global: GlobalExecutor<P::Unpark>,
    parker: P,
}
impl<P: Park> LocalExecutor<P> {
    #[must_use]
    pub fn new(
        id: usize,
        queue: Worker<Task>,
        global: GlobalExecutor<P::Unpark>,
        parker: P,
    ) -> Self {
        Self {
            id,
            queue,
            global,
            parker,
        }
    }

    pub fn maintenance(&mut self) {
        self.parker.park_timeout(Duration::from_secs(0));
    }
}

pub struct GlobalExecutor<U> {
    queue: Arc<Injector<Task>>,
    stealers: Arc<[Stealer<Task>]>,
    threads: Arc<[U]>,
    idle: Arc<Mutex<Vec<bool>>>,
}

impl<U> Clone for GlobalExecutor<U> {
    fn clone(&self) -> Self {
        Self {
            queue: self.queue.clone(),
            stealers: self.stealers.clone(),
            threads: self.threads.clone(),
            idle: self.idle.clone(),
        }
    }
}

impl<U: Unpark> GlobalExecutor<U> {
    #[must_use]
    pub fn new(queue: Arc<Injector<Task>>, stealers: Vec<Stealer<Task>>, threads: Vec<U>) -> Self {
        let len = threads.len();
        Self {
            queue,
            stealers: arc_slice(stealers),
            threads: arc_slice(threads),
            idle: Arc::new(Mutex::new(vec![false; len])),
        }
    }

    pub fn spawn<F>(&self, fut: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + Sync + 'static,
        F::Output: Send,
    {
        let (sender, handle) = JoinHandle::new();

        // Pin the future. Also wrap it s.t. it sends it's output over the channel
        let fut = Box::pin(async {
            drop(sender.send(fut.await));
        });
        // insert the task into the runtime and signal that it is ready for processing
        self.wake_one(fut);

        // return the handle to the spawner so that it can be `await`ed with it's output value
        handle
    }

    fn wake_one(&self, task: Task) {
        self.queue.push(task);
        let i = {
            let mut lock = self.idle.lock().unwrap_or_else(PoisonError::into_inner);
            let mut iter = lock.iter_mut().enumerate();
            loop {
                if let Some((i, state)) = iter.next() {
                    if *state {
                        break Some(i);
                    }
                } else {
                    break None;
                }
            }
        };
        if let Some(i) = i {
            self.threads[i].unpark();
        }
    }

    fn steal(&self, queue: &Worker<Task>) -> Steal<Task> {
        dbg!("steal");
        self.queue
            .steal_batch_and_pop(queue)
            .or_else(|| self.stealers.iter().map(Stealer::steal).collect())
    }
}

impl<P: Park> LocalExecutor<P> {
    fn try_get_task(&self) -> Option<Task> {
        if let Some(local) = self.queue.pop() {
            return Some(local);
        }

        loop {
            let stolen = self.global.steal(&self.queue);

            return match stolen {
                Steal::Empty => None,
                Steal::Success(t) => Some(t),
                Steal::Retry => continue,
            };
        }
    }

    fn get_task(&mut self) -> Task {
        loop {
            // try acquire a task from the queue
            if let Some(task) = self.try_get_task() {
                break task;
            }
            // park this thread
            {
                let mut lock = self
                    .global
                    .idle
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                lock[self.id] = true;
            }
            self.parker.park();
        }
    }

    pub fn poll_once(&mut self) {
        // Take one task from the queue.
        let mut task = self.get_task();

        let wake = Arc::new(TaskWaker {
            task: Mutex::new(None),
            global: self.global.clone(),
        });
        #[cfg(not(loom))]
        let waker = std::task::Waker::from(wake.clone());
        #[cfg(loom)]
        let waker = loom_compat::waker(wake.clone());
        let mut cx = Context::from_waker(&waker);

        if task.as_mut().poll(&mut cx).is_pending() {
            wake.task.lock().unwrap().replace(task);
        }
    }
}

struct TaskWaker<U> {
    global: GlobalExecutor<U>,
    task: Mutex<Option<Task>>,
}

#[cfg(not(loom))]
impl<U: Unpark> std::task::Wake for TaskWaker<U> {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        dbg!("wake task2");
        if let Some(task) = self.task.lock().unwrap().take() {
            self.global.wake_one(task);
        }
    }
}
#[cfg(loom)]
impl<U: Unpark> loom_compat::Wake for TaskWaker<U> {
    fn wake_by_ref(this: &Arc<Self>) {
        dbg!("wake task2");
        if let Some(task) = this.task.lock().unwrap().take() {
            this.global.wake_one(task);
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
        dbg!("poll join");
        // poll the inner channel for the spawned future's result
        Pin::new(&mut self.0).poll(cx).map(Result::unwrap)
    }
}
