use loom::sync::Arc;
use std::mem::ManuallyDrop;
use std::task::{RawWaker, RawWakerVTable, Waker};

pub trait Wake {
    fn wake(this: Arc<Self>) {
        Self::wake_by_ref(&this);
    }

    fn wake_by_ref(this: &Arc<Self>);
}

pub fn waker<W: Wake + Send + Sync + 'static>(w: Arc<W>) -> Waker {
    unsafe { Waker::from_raw(raw_waker(w)) }
}

#[inline(always)]
fn raw_waker<W: Wake + Send + Sync + 'static>(waker: Arc<W>) -> RawWaker {
    // Increment the reference count of the arc to clone it.
    unsafe fn clone_waker<W: Wake + Send + Sync + 'static>(waker: *const ()) -> RawWaker {
        Arc::increment_strong_count(waker as *const W);
        RawWaker::new(
            waker as *const (),
            &RawWakerVTable::new(
                clone_waker::<W>,
                wake::<W>,
                wake_by_ref::<W>,
                drop_waker::<W>,
            ),
        )
    }

    // Wake by value, moving the Arc into the Wake::wake function
    unsafe fn wake<W: Wake + Send + Sync + 'static>(waker: *const ()) {
        let waker = Arc::from_raw(waker as *const W);
        <W as Wake>::wake(waker);
    }

    // Wake by reference, wrap the waker in ManuallyDrop to avoid dropping it
    unsafe fn wake_by_ref<W: Wake + Send + Sync + 'static>(waker: *const ()) {
        let waker = ManuallyDrop::new(Arc::from_raw(waker as *const W));
        <W as Wake>::wake_by_ref(&waker);
    }

    // Decrement the reference count of the Arc on drop
    unsafe fn drop_waker<W: Wake + Send + Sync + 'static>(waker: *const ()) {
        Arc::decrement_strong_count(waker as *const W);
    }

    RawWaker::new(
        Arc::into_raw(waker) as *const (),
        &RawWakerVTable::new(
            clone_waker::<W>,
            wake::<W>,
            wake_by_ref::<W>,
            drop_waker::<W>,
        ),
    )
}
