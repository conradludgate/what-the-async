use std::{
    cell::RefCell,
    cmp::Reverse,
    future::Future,
    pin::Pin,
    task::{Context, Poll, Waker},
    time::{Duration, Instant},
};

#[cfg(loom)]
use loom::{
    sync::{Arc, Mutex},
    thread_local,
};
#[cfg(not(loom))]
use std::sync::{Arc, Mutex};

use wta_executor::{Handle, Park, Unpark};

#[derive(Default)]
pub struct Driver<P: Park> {
    timers: Queue,
    global: Arc<Mutex<Queue>>,
    park: P,
}

impl<P: Park> Driver<P> {
    fn park_internal(&mut self, duration: Option<Duration>) {
        {
            self.timers.0.append(&mut self.global.lock().unwrap().0);
        }
        let time = {
            match self.timers.0.first() {
                Some((instant, _)) => instant.checked_duration_since(Instant::now()),
                None => None,
            }
        };
        // compute min
        let time = match (time, duration) {
            (Some(time), Some(duration)) => Some(time.min(duration)),
            (None, Some(duration)) => Some(duration),
            (Some(time), None) => Some(time),
            (None, None) => None,
        };
        match time {
            Some(time) => self.park.park_timeout(time),
            None => self.park.park(),
        }

        for task in self.timers.iter() {
            dbg!("timer task finished");
            task.wake();
        }
    }
}

impl<P: Park> Park for Driver<P> {
    type Unpark = P::Unpark;

    fn unpark(&self) -> Self::Unpark {
        self.park.unpark()
    }

    fn park(&mut self) {
        dbg!("timer park");
        self.park_internal(None);
    }

    fn park_timeout(&mut self, duration: Duration) {
        dbg!("timer park", &duration);
        self.park_internal(Some(duration));
    }

    type Handle = (TimerHandle, P::Handle);

    fn handle(&self) -> Self::Handle {
        let unpark: std::sync::Arc<dyn Unpark> = std::sync::Arc::new(self.park.unpark());
        #[cfg(loom)]
        let unpark = Arc::from_std(unpark);
        (
            TimerHandle {
                timers: self.global.clone(),
                unpark,
            },
            self.park.handle(),
        )
    }
}

#[derive(Default)]
struct Queue(Vec<(Instant, Waker)>);

impl Queue {
    pub(crate) fn insert(&mut self, instant: Instant, task: Waker) {
        let index = match self
            .0
            .binary_search_by_key(&Reverse(instant), |e| Reverse(e.0))
        {
            Ok(index) | Err(index) => index,
        };
        self.0.insert(index, (instant, task));
    }

    pub(crate) fn iter(&mut self) -> QueueIter<'_> {
        QueueIter(&mut self.0, Instant::now())
    }
}

pub struct QueueIter<'a>(&'a mut Vec<(Instant, Waker)>, Instant);
impl<'a> Iterator for QueueIter<'a> {
    type Item = Waker;

    fn next(&mut self) -> Option<Self::Item> {
        let (time, task) = self.0.pop()?;
        if time > self.1 {
            self.0.push((time, task));
            None
        } else {
            Some(task)
        }
    }
}

/// Future for sleeping fixed amounts of time.
/// Does not block the thread
pub struct Sleep {
    instant: Instant,
}

impl Unpin for Sleep {}

impl Future for Sleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // if the future is not yet ready
        if self.instant > Instant::now() {
            context(|h| {
                h.timers
                    .lock()
                    .unwrap()
                    .insert(self.instant, cx.waker().clone());
                h.unpark.unpark();
            });
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    }
}

impl Sleep {
    /// sleep until a specific point in time
    #[must_use]
    pub fn until(instant: Instant) -> Sleep {
        Self { instant }
    }
    /// sleep for a specific duration of time
    #[must_use]
    pub fn duration(duration: Duration) -> Sleep {
        Sleep::until(Instant::now() + duration)
    }
}

thread_local! {
    static TIMERS: RefCell<Option<TimerHandle>> = RefCell::new(None);
}

#[derive(Clone)]
pub struct TimerHandle {
    timers: Arc<Mutex<Queue>>,
    unpark: Arc<dyn Unpark>,
}
impl Handle for TimerHandle {
    fn register(self) {
        TIMERS.with(|r| *r.borrow_mut() = Some(self));
    }
}

fn context<R>(f: impl FnOnce(&TimerHandle) -> R) -> R {
    TIMERS.with(|r| {
        let r = r.borrow();
        let r = r.as_ref().expect("called outside of an reactor context");
        f(r)
    })
}
