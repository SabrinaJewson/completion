use std::cell::RefCell;
use std::marker::PhantomData;
use std::mem;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
use std::thread::{self, Thread};

use completion_core::CompletionFuture;

/// Blocks the current thread on a completion future.
///
/// # Examples
///
/// ```
/// use completion_util::{future, FutureExt};
///
/// assert_eq!(future::block_on(async { 5 + 6 }.must_complete()), 11);
/// ```
pub fn block_on<F: CompletionFuture>(mut future: F) -> F::Output {
    let mut fut = unsafe { Pin::new_unchecked(&mut future) };

    thread_local! {
        static CACHE: RefCell<(Parker, Waker)> = RefCell::new(wake_pair());
    }

    CACHE.with(|cache| {
        let guard_storage;
        let new_pair_storage;

        let (parker, waker) = match cache.try_borrow_mut() {
            Ok(guard) => {
                guard_storage = guard;
                (&guard_storage.0, &guard_storage.1)
            }
            Err(_) => {
                new_pair_storage = wake_pair();
                (&new_pair_storage.0, &new_pair_storage.1)
            }
        };

        let mut cx = Context::from_waker(waker);

        loop {
            if let Poll::Ready(output) = unsafe { fut.as_mut().poll(&mut cx) } {
                return output;
            }
            parker.park();
        }
    })
}

fn wake_pair() -> (Parker, Waker) {
    let inner = Arc::new(WakerInner {
        woken: AtomicBool::new(false),
        sleeping_thread: thread::current(),
    });
    (
        Parker {
            inner: Arc::clone(&inner),
            not_send_or_sync: PhantomData,
        },
        unsafe { Waker::from_raw(RawWaker::new(Arc::into_raw(inner) as *const _, &VTABLE)) },
    )
}

struct Parker {
    inner: Arc<WakerInner>,
    not_send_or_sync: PhantomData<*mut ()>,
}

impl Parker {
    fn park(&self) {
        while !self.inner.woken.swap(false, Ordering::SeqCst) {
            thread::park();
        }
    }
}

struct WakerInner {
    woken: AtomicBool,
    sleeping_thread: Thread,
}

unsafe fn waker_clone(ptr: *const ()) -> RawWaker {
    let inner = Arc::from_raw(ptr as *const WakerInner);
    mem::forget(Arc::clone(&inner));
    mem::forget(inner);
    RawWaker::new(ptr, &VTABLE)
}
unsafe fn waker_wake(ptr: *const ()) {
    waker_wake_by_ref(ptr);
    waker_drop(ptr);
}
unsafe fn waker_wake_by_ref(ptr: *const ()) {
    let inner = &*(ptr as *const WakerInner);
    if !inner.woken.swap(true, Ordering::SeqCst) {
        inner.sleeping_thread.unpark();
    }
}
unsafe fn waker_drop(ptr: *const ()) {
    Arc::from_raw(ptr);
}

const VTABLE: RawWakerVTable =
    RawWakerVTable::new(waker_clone, waker_wake, waker_wake_by_ref, waker_drop);
