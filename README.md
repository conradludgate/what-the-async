# what-the-async

This is an async runtime following the executor-reactor model.

## Executor

Executors are incharge with polling tasks and spawning futures.
They provide the contexts that let tasks be woken up correctly.

## Reactors

Reactors handle side effects, like OS events or timers.
Leaf futures will put their wakers onto the reactor,
in order to wake up when the resources are ready
