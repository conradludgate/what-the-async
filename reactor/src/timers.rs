use std::{
    cmp::Reverse,
    pin::Pin,
    sync::{Mutex, MutexGuard, PoisonError, Arc},
    task::{Context, Poll, Waker},
    time::{Duration, Instant}, cell::RefCell,
};

use futures::Future;
use wta_executor::Park;

#[derive(Default)]
pub struct Driver<P: Park> {
    queue: Arc<Queue>,
    park: P,
}

impl<P: Park> Driver<P> {
    fn park_internal(&mut self, duration: Option<Duration>) {
        let time = {
            let lock = self.queue.0.lock().unwrap_or_else(PoisonError::into_inner);
            match lock.first() {
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

        for task in &*self.queue {
            // dbg!("timer task finished");
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
        // dbg!("timer park");
        self.park_internal(None);
    }

    fn park_timeout(&mut self, duration: Duration) {
        // dbg!("timer park", &duration);
        self.park_internal(Some(duration));
    }

    fn register(&self) {
        TIMERS.with(|r| *r.borrow_mut() = Some(self.queue.clone()));
    }
}

#[derive(Default)]
pub(crate) struct Queue(Mutex<Vec<(Instant, Waker)>>);

impl Queue {
    pub fn insert(&self, instant: Instant, task: Waker) {
        let mut queue = self.0.lock().unwrap();
        let index = match queue.binary_search_by_key(&Reverse(instant), |e| Reverse(e.0)) {
            Ok(index) | Err(index) => index,
        };
        queue.insert(index, (instant, task));
    }
}

impl<'a> IntoIterator for &'a Queue {
    type Item = Waker;
    type IntoIter = QueueIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        QueueIter(self.0.lock().unwrap(), Instant::now())
    }
}

pub(crate) struct QueueIter<'a>(MutexGuard<'a, Vec<(Instant, Waker)>>, Instant);
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
            context(|q| q.insert(self.instant, cx.waker().clone()));
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
    static TIMERS: RefCell<Option<Arc<Queue>>> = RefCell::new(None);
}

pub(crate) fn context<R>(f: impl FnOnce(&Arc<Queue>) -> R) -> R {
    TIMERS.with(|r| {
        let r = r.borrow();
        let r = r.as_ref().expect("called outside of an reactor context");
        f(r)
    })
}
