# Reactor

Reactors react to side effects in the tasks. These side effects could be running timers or listening to OS events.

## Timers

The timers this reactor currently exposes are simple and naive. It stores all Instant + Waker pairs in a priority queue,
the reactor book-keeping will run and attempts to wake any elapsed timers.

For a more complex and faster timer setup, take a look at https://tokio.rs/blog/2018-03-timers

## OS Events

This reactor makes use of [mio](https://crates.io/crates/mio) which was written for use in tokio, but was built into a separate crate. It allows for registering data sources for events, and then polling for those events.

We store use an async-channel per event source registration. Any events that are relevent are pushed to the channel. If an async-read/write future is called, it will retrieve those events from the channel and handle them accordingly.
