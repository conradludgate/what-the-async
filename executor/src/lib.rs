#![warn(clippy::pedantic)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![feature(layout_for_ptr, set_ptr_value, ptr_const_cast)]

use std::{
    cell::RefCell,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, Wake, Waker, RawSpawner, RawSpawnerVTable, Spawner},
    thread::Thread, mem::ManuallyDrop, alloc::Layout,
};

use crossbeam_queue::SegQueue;
use futures::{channel::oneshot, FutureExt};

pub type Task = Pin<Box<dyn Future<Output = ()> + Send + Sync + 'static>>;

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

    pub fn spawner(self: Arc<Self>) -> Spawner {
        unsafe { Spawner::from_raw(raw_spawner(self)) }
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
            executor: self.clone(),
        });
        let waker = Waker::from(wake.clone());
        let spawner = self.spawner();
        let mut cx = Context::from_waker(&waker).with_spawner(&spawner);

        if task.as_mut().poll(&mut cx).is_pending() {
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
        self.wake(fut);

        // return the handle to the spawner so that it can be `await`ed with it's output value
        handle
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

fn raw_spawner(spawner: Arc<Executor>) -> RawSpawner {
    // Increment the reference count of the arc to clone it.
    unsafe fn clone_spawner(spawner: *const ()) -> RawSpawner {
        unsafe { Arc::increment_strong_count(spawner as *const Executor) };
        new(spawner)
    }

    // Wake by value, moving the Arc into the Wake::wake function
    unsafe fn spawn(spawner: *const (), future: *const (dyn Future<Output = ()> + Send + Sync + 'static)) {
        let spawner = unsafe { Arc::from_raw(spawner as *const Executor) };
        let future = Pin::new_unchecked(copy_to_box(future));
        spawner.wake(future);
    }

    // Wake by reference, wrap the spawner in ManuallyDrop to avoid dropping it
    unsafe fn spawn_by_ref(spawner: *const (), future: *const (dyn Future<Output = ()> + Send + Sync + 'static)) {
        let spawner = unsafe { ManuallyDrop::new(Arc::from_raw(spawner as *const Executor)) };
        let future = Pin::new_unchecked(copy_to_box(future));
        spawner.wake(future);
    }

    // Decrement the reference count of the Arc on drop
    unsafe fn drop_spawner(spawner: *const ()) {
        unsafe { Arc::decrement_strong_count(spawner as *const Executor) };
    }

    fn new(spawner: *const ()) -> RawSpawner {
        RawSpawner::new(
            spawner,
            &RawSpawnerVTable::new(clone_spawner, spawn, spawn_by_ref, drop_spawner),
        )
    }
    new(Arc::into_raw(spawner) as *const ())
}

unsafe fn copy_to_box<T: ?Sized>(ptr: *const T) -> Box<T> {
    let layout = Layout::for_value_raw(ptr);
    let allocation = std::alloc::alloc(layout);
    if allocation.is_null() {
        std::alloc::handle_alloc_error(layout);
    }
    std::ptr::copy_nonoverlapping(ptr.cast::<u8>(), allocation, layout.size());
    unsafe { Box::from_raw(allocation.with_metadata_of(ptr.as_mut())) }
}
