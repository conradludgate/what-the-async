use std::{
    cmp::Reverse,
    pin::Pin,
    sync::{Mutex, MutexGuard},
    task::{Context, Poll, Waker},
    time::{Duration, Instant},
};

use futures::Future;

use crate::context;

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
            context(|r| r.timers.insert(self.instant, cx.waker().clone()));
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
