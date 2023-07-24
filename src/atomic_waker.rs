//! Annoyingly loom's `AtomicWaker` mock has a different api then the one in the actual crate.
//! This wrapper type bridges the gap
use crate::prelude::*;

pub struct AtomicWaker {
    #[cfg(not(any(loom, doc)))]
    inner: atomic_waker::AtomicWaker,
    #[cfg(all(loom, not(doc)))]
    inner: loom::future::AtomicWaker,
}

impl AtomicWaker {
    #[cfg(not(loom))]
    pub const fn new() -> Self {
        Self {
            inner: atomic_waker::AtomicWaker::new(),
        }
    }

    #[cfg(loom)]
    pub fn new() -> Self {
        Self {
            inner: loom::future::AtomicWaker::new(),
        }
    }

    pub fn register(&self, waker: &Waker) {
        #[cfg(not(loom))]
        self.inner.register(waker);
        #[cfg(loom)]
        self.inner.register_by_ref(waker);
        // Why did loom decide to call this `register_by_ref`!?
        // `loom::future::AtomicWaker::register` takes a `Waker` by value
    }

    pub fn wake(&self) {
        self.inner.wake();
    }
}
