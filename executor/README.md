# Executor

This is a simple executor. There is a single `Arc<Executor>` type that can be shared between many worker threads.

Each worker will continuously run `executor.poll_once()` which will

1. Pop a task from the shared task queue
    * Parking the thread if no tasks are available
2. Poll that task

The task will be polled with a context that will reference that task (using a `Mutex<Option<Task>>`),
and a copy of the executor.

Calling wake on this context will then call `executor.wake(task)`. If there as threads sleeping, one will be woken up.
