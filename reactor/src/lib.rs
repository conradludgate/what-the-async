#![forbid(unsafe_code)]
#![warn(clippy::pedantic)]
#![allow(clippy::missing_errors_doc)]

use std::{cell::RefCell, sync::Arc};

mod io;
pub mod net;
pub mod timers;

#[derive(Default)]
pub struct Reactor {
    os: io::Os,
    timers: timers::Queue,
}

impl Reactor {
    // this is run by any thread that currently is not busy.
    // It manages the timers and OS polling in order to wake up tasks
    pub fn book_keeping(&self) {
        // get the current task timers that have elapsed and insert them into the ready tasks
        for task in &self.timers {
            task.wake();
        }

        // get the OS events
        self.os.process();
    }

    /// register this executor on the current thread
    pub fn register(self: &Arc<Self>) {
        REACTOR.with(|reactor| *reactor.borrow_mut() = Some(self.clone()));
    }
}

thread_local! {
    static REACTOR: RefCell<Option<Arc<Reactor>>> = RefCell::new(None);
}

pub(crate) fn context<R>(f: impl FnOnce(&Arc<Reactor>) -> R) -> R {
    REACTOR.with(|r| {
        let r = r.borrow();
        let r = r.as_ref().expect("called outside of an reactor context");
        f(r)
    })
}
