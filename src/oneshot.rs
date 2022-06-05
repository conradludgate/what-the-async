//! A channel for sending a single message between asynchronous tasks.
//!
//! This is a single-producer, single-consumer channel.

use core::fmt;
use core::pin::Pin;
use core::sync::atomic::AtomicBool;
use core::sync::atomic::Ordering::SeqCst;
use std::future::Future;
use std::sync::Mutex;
use std::task::{Context, Poll, Waker};

use futures::future::FusedFuture;

/// A future for a value that will be provided by another asynchronous task.
///
/// This is created by the [`channel`](channel) function.
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct Receiver<'a, T> {
    inner: &'a Inner<T>,
}

/// A means of transmitting a single value to another task.
///
/// This is created by the [`channel`](channel) function.
pub struct Sender<'a, T> {
    inner: &'a Inner<T>,
}

// The channels do not ever project Pin to the inner T
impl<T> Unpin for Receiver<'_, T> {}
impl<T> Unpin for Sender<'_, T> {}

/// Internal state of the `Receiver`/`Sender` pair above. This is all used as
/// the internal synchronization between the two for send/recv operations.
pub struct Inner<T> {
    /// Indicates whether this oneshot is complete yet. This is filled in both
    /// by `Sender::drop` and by `Receiver::drop`, and both sides interpret it
    /// appropriately.
    ///
    /// For `Receiver`, if this is `true`, then it's guaranteed that `data` is
    /// unlocked and ready to be inspected.
    ///
    /// For `Sender` if this is `true` then the oneshot has gone away and it
    /// can return ready from `poll_canceled`.
    complete: AtomicBool,

    /// The actual data being transferred as part of this `Receiver`. This is
    /// filled in by `Sender::complete` and read by `Receiver::poll`.
    ///
    /// Note that this is protected by `Lock`, but it is in theory safe to
    /// replace with an `UnsafeCell` as it's actually protected by `complete`
    /// above. I wouldn't recommend doing this, however, unless someone is
    /// supremely confident in the various atomic orderings here and there.
    data: Mutex<Option<T>>,

    /// Field to store the task which is blocked in `Receiver::poll`.
    ///
    /// This is filled in when a oneshot is polled but not ready yet. Note that
    /// the `Lock` here, unlike in `data` above, is important to resolve races.
    /// Both the `Receiver` and the `Sender` halves understand that if they
    /// can't acquire the lock then some important interference is happening.
    rx_task: Mutex<Option<Waker>>,
}

// /// Creates a new one-shot channel for sending a single value across asynchronous tasks.
// ///
// /// The channel works for a spsc (single-producer, single-consumer) scheme.
// ///
// /// This function is similar to Rust's channel constructor found in the standard
// /// library. Two halves are returned, the first of which is a `Sender` handle,
// /// used to signal the end of a computation and provide its value. The second
// /// half is a `Receiver` which implements the `Future` trait, resolving to the
// /// value that was given to the `Sender` handle.
// ///
// /// Each half can be separately owned and sent across tasks.
// ///
// /// # Examples
// ///
// /// ```
// /// use futures::channel::oneshot;
// /// use std::{thread, time::Duration};
// ///
// /// let (sender, receiver) = oneshot::channel::<i32>();
// ///
// /// thread::spawn(|| {
// ///     println!("THREAD: sleeping zzz...");
// ///     thread::sleep(Duration::from_millis(1000));
// ///     println!("THREAD: i'm awake! sending.");
// ///     sender.send(3).unwrap();
// /// });
// ///
// /// println!("MAIN: doing some useful stuff");
// ///
// /// futures::executor::block_on(async {
// ///     println!("MAIN: waiting for msg...");
// ///     println!("MAIN: got: {:?}", receiver.await)
// /// });
// /// ```
// pub fn channel<T>(inner: &Inner<T>) -> (Sender<'_, T>, Receiver<'_, T>) {
//     let receiver = Receiver { inner };
//     let sender = Sender { inner };
//     (sender, receiver)
// }

impl<T> Inner<T> {
    pub fn new() -> Self {
        Self {
            complete: AtomicBool::new(false),
            data: Mutex::new(None),
            rx_task: Mutex::new(None),
        }
    }

    pub fn sender(&self) -> Sender<'_, T> {
        Sender { inner: self }
    }

    fn send(&self, t: T) {
        let mut slot = self.data.lock().unwrap();
        *slot = Some(t);
    }

    fn drop_tx(&self) {
        // Flag that we're a completed `Sender` and try to wake up a receiver.
        // Whether or not we actually stored any data will get picked up and
        // translated to either an item or cancellation.
        //
        // Note that if we fail to acquire the `rx_task` lock then that means
        // we're in one of two situations:
        //
        // 1. The receiver is trying to block in `poll`
        // 2. The receiver is being dropped
        //
        // In the first case it'll check the `complete` flag after it's done
        // blocking to see if it succeeded. In the latter case we don't need to
        // wake up anyone anyway. So in both cases it's ok to ignore the `None`
        // case of `try_lock` and bail out.
        //
        // The first case crucially depends on `Lock` using `SeqCst` ordering
        // under the hood. If it instead used `Release` / `Acquire` ordering,
        // then it would not necessarily synchronize with `inner.complete`
        // and deadlock might be possible, as was observed in
        // https://github.com/rust-lang/futures-rs/pull/219.
        self.complete.store(true, SeqCst);

        let task = self.rx_task.lock().unwrap().take();
        if let Some(task) = task {
            task.wake();
        }
    }

    pub async fn recv(&self) -> Result<T, Canceled> {
        Receiver { inner: self }.await
    }

    fn poll_recv(&self, cx: &mut Context<'_>) -> Poll<Result<T, Canceled>> {
        // Check to see if some data has arrived. If it hasn't then we need to
        // block our task.
        //
        // Note that the acquisition of the `rx_task` lock might fail below, but
        // the only situation where this can happen is during `Sender::drop`
        // when we are indeed completed already. If that's happening then we
        // know we're completed so keep going.
        let done = if self.complete.load(SeqCst) {
            true
        } else {
            let task = cx.waker().clone();
            let _ = self.rx_task.lock().unwrap().insert(task);
            false
        };

        // If we're `done` via one of the paths above, then look at the data and
        // figure out what the answer is. If, however, we stored `rx_task`
        // successfully above we need to check again if we're completed in case
        // a message was sent while `rx_task` was locked and couldn't notify us
        // otherwise.
        //
        // If we're not done, and we're not complete, though, then we've
        // successfully blocked our task and we return `Pending`.
        if done || self.complete.load(SeqCst) {
            // If taking the lock fails, the sender will realise that the we're
            // `done` when it checks the `complete` flag on the way out, and
            // will treat the send as a failure.
            let mut slot = self.data.lock().unwrap();
            if let Some(data) = slot.take() {
                return Poll::Ready(Ok(data));
            }
            Poll::Ready(Err(Canceled))
        } else {
            Poll::Pending
        }
    }
}

impl<'a, T> Sender<'a, T> {
    /// Completes this oneshot with a successful result.
    ///
    /// This function will consume `self` and indicate to the other end, the
    /// [`Receiver`](Receiver), that the value provided is the result of the
    /// computation this represents.
    ///
    /// If the value is successfully enqueued for the remote end to receive,
    /// then `Ok(())` is returned. If the receiving end was dropped before
    /// this function was called, however, then `Err(t)` is returned.
    pub fn send(self, t: T) {
        self.inner.send(t);
    }
}

impl<'a, T> Drop for Sender<'a, T> {
    fn drop(&mut self) {
        self.inner.drop_tx();
    }
}

impl<T: fmt::Debug> fmt::Debug for Sender<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Sender")
            .field("complete", &self.inner.complete)
            .finish()
    }
}

/// Error returned from a [`Receiver`](Receiver) when the corresponding
/// [`Sender`](Sender) is dropped.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Canceled;

impl fmt::Display for Canceled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "oneshot canceled")
    }
}

impl<T> Future for Receiver<'_, T> {
    type Output = Result<T, Canceled>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<T, Canceled>> {
        self.inner.poll_recv(cx)
    }
}

impl<T> FusedFuture for Receiver<'_, T> {
    fn is_terminated(&self) -> bool {
        if self.inner.complete.load(SeqCst) {
            if let Ok(slot) = self.inner.data.try_lock() {
                if slot.is_some() {
                    return false;
                }
            }
            true
        } else {
            false
        }
    }
}

impl<T: fmt::Debug> fmt::Debug for Receiver<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Receiver")
            .field("complete", &self.inner.complete)
            .finish()
    }
}
