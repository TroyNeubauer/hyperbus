#![deny(unsafe_op_in_unsafe_fn)]
//#![warn(missing_docs)]
#![warn(rustdoc::broken_intra_doc_links, rust_2018_idioms)]
#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(not(feature = "std"))]
extern crate alloc;

mod atomic_waker;
mod reader;
mod writer;

pub mod prelude {
    pub(crate) use core::{
        cell::UnsafeCell,
        fmt::{self, Debug},
        future::Future,
        mem::MaybeUninit,
        pin::Pin,
        ptr,
        sync::atomic::Ordering,
        task::{Context, Poll, Waker},
    };
    pub(crate) use futures_core::ready;

    #[cfg(loom)]
    pub(crate) mod atomic {
        pub use loom::sync::{
            atomic::{fence, AtomicBool, AtomicU8, AtomicUsize},
            Arc,
        };
    }

    #[cfg(not(loom))]
    pub(crate) mod atomic {
        #[cfg(not(feature = "std"))]
        pub use alloc::{sync::Arc, vec::Vec};
        #[cfg(feature = "std")]
        pub use std::{sync::Arc, vec::Vec};

        pub use core::sync::atomic::{fence, AtomicBool, AtomicU8, AtomicUsize};
    }

    pub(crate) use atomic::*;

    pub use crate::{
        atomic_waker::AtomicWaker,
        reader::{BusReceiver, Recv},
        writer::{Broadcast, Bus},
        RecvError, TryRecvError,
    };
}

use prelude::*;

/// This enumeration is the list of the possible reasons that a try recv operation could fail
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum TryRecvError {
    Empty,
    Disconnected,
}

/// An error returned from the async recv function
///
/// An async recv operation can only fail if the broadcasting half of a bus is disconnected,
/// implying that no further messages will ever be received.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub struct RecvError;

pub(crate) struct Shared<T>
where
    T: Send,
{
    slots: Arc<[Slot<T>]>,

    /// Used to awaken the writer when it is waiting for capacity in `slots`.
    writer_waker: AtomicWaker,

    /// Location next write will happen, modulo `slots.len()`.
    ///
    /// Only modified by the writer.
    head: AtomicUsize,

    /// Location of the most recently initialized element, modulo `slots.len()`.
    /// Queue is empty when `head == tail`, as we always leave an empty slot to detect empty vs full
    ///
    /// Modified by readers or the writer during leave cleanup.
    tail: AtomicUsize,

    /// The number of readers that have left and not been accounted for by the writer.
    left_reads_count: AtomicUsize,
}

impl<T> Shared<T>
where
    T: Send + Clone,
{
    pub fn new(size: usize) -> Self {
        let slots: Vec<_> = (0..size).map(|_| Slot::default()).collect();

        #[cfg(loom)]
        let slots = Arc::from_std(std::sync::Arc::from(slots));
        #[cfg(not(loom))]
        let slots = slots.into();

        Self {
            slots,
            writer_waker: AtomicWaker::new(),
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
            left_reads_count: AtomicUsize::new(0),
        }
    }

    /// Removes the item at index `idx` by cloning (or moving if last reader).
    ///
    /// Adjusts `Slot::remaining` on idx plus handles writer wakeups and advancing tail.
    ///
    /// # Safety
    ///
    /// 1. `idx` must be a valid index the current reader is allowed to access
    ///     (must be in the read section of the internal buffer `idx >= (tail % len)`)
    /// 2. `remove(idx)` must only be called once per reader for a particular location
    unsafe fn take(&self, idx: usize) -> T {
        let remaining = self.slots[idx].remaining.load(Ordering::Acquire);
        debug_assert_ne!(remaining, 0);

        let is_last = remaining == 1;

        let val = if is_last {
            println!("Last reader to take() element at index {idx}");
            // SAFETY:
            // By our contract, this function is only called once per reader and each reader has
            // permission to read from this slot (each reader was accounted for when
            // `slots[idx].remaining` was initialized
            // Therefore when we are the last reader we have exclusive access to `idx`
            unsafe { ptr::read(self.slots[idx].inner.get() as *const T) }
        } else {
            // SAFETY:
            // 1. The caller guaranteed that we can access `idx`
            // 2. `remaining` is at least two, therefore no reader can move out of `idx`
            // 3. `remaining` is at least one, therefore no writer can mutate `idx`
            //
            // Therefore we can access `idx` through a shared reference
            let val = unsafe { &*self.slots[idx].inner.get() };

            // SAFETY:
            // 1. The caller guaranteed that we can access `idx`, therefore it is initialized
            Clone::clone(unsafe { val.assume_init_ref() })
        };

        // We are done with this slot
        let old_remaining = self.slots[idx].remaining.fetch_sub(1, Ordering::AcqRel);

        let missed_last_initally = !is_last && old_remaining == 1;
        // Its possible for multiple readers to race on `remaining.fetch_sub(1)`, observing
        // `remaining > 1` and `is_last == false` even if we are the last to take `idx`
        // When this is the case, we have to drop the element in the buffer in addition to normal
        // last cleanup below
        if missed_last_initally {
            println!("take({idx}): Read {remaining} initially but was {old_remaining} before decrementing remaining for index {idx}");

            // SAFETY:
            // 1. By our contract, `idx` is in the read section
            // 2. This is the last time `idx` will be accessed because we observed `remaining == 1`
            //    and no more calls to `cleanup` or `take` on the same index are possible due to our contract
            // 3. We avoid races with the writer since tail hasn't been incremented yet
            //
            // Therefore we have exclusive access to `idx`
            unsafe { ptr::drop_in_place(self.slots[idx].inner.get() as *mut T) }
        }

        if is_last || missed_last_initally {
            let old_tail_idx = self.tail.fetch_add(1, Ordering::Release) % self.slots.len();
            debug_assert_eq!(old_tail_idx, idx);

            //println!("Waking writer");
            // Wake waker now that remaining is zero
            self.writer_waker.wake();
        }

        val
    }

    /// Cleanups the value at index `idx` when readers have left by decrementing the remaining count.
    ///
    /// Similar to `take`, but without writer wakeups or giving the value back to the caller.
    ///
    /// Also handles dropping the value once the remaining count has reached zero.
    ///
    /// Returns the number of readers waiting on `idx` _after_ this cleanup
    ///
    /// # Safety
    ///
    /// 1. `idx` must be a valid index in the read section of the buffer (`idx >= (tail % len)`)
    /// 2. The number of calls to `cleanup(idx)` and `take(idx)` must sum to exactly the number
    ///    of readers that existed when slot `idx` was initialized
    unsafe fn cleanup(&self, idx: usize) -> usize {
        let remaining = self.slots[idx].remaining.load(Ordering::Acquire);
        debug_assert_ne!(remaining, 0, "not already freed");

        let is_last = remaining == 1;
        if is_last {
            // SAFETY:
            // 1. By our contract, `idx` is in the read section
            // 2. This is the last time `idx` will be accessed because we observed `remaining == 1`
            //    and no more calls to `cleanup` or `take` on the same index are possible due to our contract
            // 3. We avoid races with the writer since our `remaining` count hasn't reached zero yet
            //
            // Therefore we have exclusive access to `idx`
            unsafe { ptr::drop_in_place(self.slots[idx].inner.get() as *mut T) }
        }

        // we are done with this slot
        let old_remaining = self.slots[idx].remaining.fetch_sub(1, Ordering::AcqRel);

        let missed_last_initally = !is_last && old_remaining == 1;

        if missed_last_initally {
            println!("cleanup({idx}): Read {remaining} initially but was {old_remaining} before decrementing remaining for index {idx}");

            // SAFETY:
            // 1. By our contract, `idx` is in the read section
            // 2. This is the last time `idx` will be accessed because we observed `remaining == 1`
            //    and no more calls to `cleanup` or `take` on the same index are possible due to our contract
            // 3. We avoid races with the writer since tail hasn't been incremented yet
            //
            // Therefore we have exclusive access to `idx`
            unsafe { ptr::drop_in_place(self.slots[idx].inner.get() as *mut T) }
        }

        if is_last || missed_last_initally {
            let old_tail_idx = self.tail.fetch_add(1, Ordering::Release) % self.slots.len();
            debug_assert_eq!(old_tail_idx, idx);
        }

        old_remaining - 1
    }
}

pub(crate) mod reader_cleanup {
    pub const RUNNING: u8 = 0;
    pub const READER_CLEANUP: u8 = 1;
    pub const WRITER_CLEANUP: u8 = 2;
}

/// Information shared between a reader and the writer
struct ReaderInfo {
    /// Waker used to wake reader
    waker: AtomicWaker,

    /// Reader id
    id: usize,

    /// `true` if this reader has left and will not receive any more values
    cleanup_state: AtomicU8,

    /// Communicates the readers next index when it has left
    ///
    /// Only used by the reader if `cleanup_state == WRITER_CLEANUP`
    next: AtomicUsize,
}

impl Debug for ReaderInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReaderInfo")
            .field("id", &self.id)
            .field(
                "cleanup_state",
                match self.cleanup_state.load(Ordering::Acquire) {
                    reader_cleanup::RUNNING => &"running",
                    reader_cleanup::READER_CLEANUP => &"reader cleanup",
                    reader_cleanup::WRITER_CLEANUP => &"writer cleanup",
                    _ => unreachable!(),
                },
            )
            .field("left_with_next", &self.next.load(Ordering::Acquire))
            .finish()
    }
}

struct Slot<T>
where
    T: Send,
{
    inner: UnsafeCell<MaybeUninit<T>>,
    /// The number of remaining readers which can take recv `inner`
    remaining: AtomicUsize,
}

unsafe impl<T> Send for Slot<T> where T: Send {}
unsafe impl<T> Sync for Slot<T> where T: Send {}

impl<T> Default for Slot<T>
where
    T: Send,
{
    fn default() -> Self {
        Self {
            inner: UnsafeCell::new(MaybeUninit::uninit()),
            remaining: AtomicUsize::new(0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_reader_info() {
        let r = super::ReaderInfo {
            waker: AtomicWaker::new(),
            id: 0,
            cleanup_state: AtomicU8::new(reader_cleanup::RUNNING),
            next: AtomicUsize::new(0),
        };
        // ReaderInfo's Debug implementation is not currently hit. However, we want to keep the
        // Debug implementation for future internal debugging
        let _ = format!("{r:?}");
    }
}
