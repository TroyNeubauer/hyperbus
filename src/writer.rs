use crate::{prelude::*, reader_cleanup};
use crate::{BusReceiver, ReaderInfo, Shared};

pub struct Bus<T>
where
    T: Send + Clone,
{
    shared: Arc<Shared<T>>,
    readers: Vec<Arc<ReaderInfo>>,
    next_reader_id: usize,
}

impl<T> Bus<T>
where
    T: Send + Clone,
{
    pub fn new(size: usize) -> Self {
        Self {
            shared: Arc::new(Shared::new(size)),
            readers: Vec::new(),
            next_reader_id: 0,
        }
    }

    /// Broadcasts a value on the bus to all consumers.
    ///
    /// This function will block until space in the internal buffer becomes available.
    ///
    /// Note that a successful send does not guarantee that the receiver will ever see the data if there is a buffer on this channel.
    /// Items may be enqueued in the internal buffer for the receiver to receive at a later time.
    /// Furthermore, in contrast to regular channels, a bus is not considered closed if there are no consumers, and thus broadcasts will continue to succeed.
    pub fn broadcast(&mut self, val: T) -> Broadcast<'_, T> {
        Broadcast {
            sender: Pin::new(self),
            val: Some(val),
        }
    }

    /// Attempt to broadcast the given value to all consumers, but does not block if full.
    ///
    /// Note that, in contrast to regular channels, a bus is *not* considered closed if there are
    /// no consumers, and thus broadcasts will continue to succeed. Thus, a successful broadcast
    /// occurs as long as there is room on the internal bus to store the value.
    /// Note that a return value of `Err` means that the data will never be received (by any consumer),
    /// but a return value of Ok does not mean that the data will be received by a given consumer.
    /// It is possible for a receiver to hang up immediately after this function returns Ok.
    ///
    /// This method will never block the current thread.
    pub fn try_broadcast(&mut self, val: T) -> Result<(), T> {
        let mut opt = Some(val);
        match Pin::new(self).try_broadcast_inner(&mut opt) {
            Ok(()) => Ok(()),
            Err(()) => Err(opt.unwrap()),
        }
    }

    fn poll_broadcast(
        mut self: Pin<&mut Self>,
        val: &mut Option<T>,
        cx: &mut Context<'_>,
    ) -> Poll<()> {
        match self.as_mut().try_broadcast_inner(val) {
            Ok(()) => {
                // TODO: optimize?
                for reader in &self.readers {
                    reader.waker.wake();
                }
                Poll::Ready(())
            }

            Err(()) => {
                self.shared.writer_waker.register(cx.waker());
                Poll::Pending
            }
        }
    }

    /// Tries to insert val into the inner buffer, without handling wakeups.
    ///
    /// On success `val` s moved into the buffer and is set to `None`.
    ///
    /// # Panics
    /// This function panics if `val` is `None`
    fn try_broadcast_inner(mut self: Pin<&mut Self>, val: &mut Option<T>) -> Result<(), ()> {
        let head = self.shared.head.load(Ordering::Acquire);

        // we want to check if the next element over is free to ensure that we always leave one
        // empty space between the head and the tail. This is necessary so that readers can
        // distinguish between an empty and a full list. If the fence seat is free, the seat at
        // tail must also be free
        let fence = (head + 1) % self.shared.slots.len();

        let remaining_readers = self.shared.slots[fence].remaining.load(Ordering::Acquire);
        if remaining_readers != 0 {
            // Fence slot still has readers waiting on it,
            // or some readers have left and we need to account for them
            self.try_cleanup_readers(Some(fence), false);

            if self.shared.slots[fence].remaining.load(Ordering::Acquire) != 0 {
                // Slot still taken after trying cleanup (full)
                return Err(());
            }
        }

        // `idx` is free!
        let idx = head % self.shared.slots.len();
        debug_assert_eq!(self.shared.slots[idx].remaining.load(Ordering::Acquire), 0);

        // SAFETY:
        // `remaining` is 0 for the fence slot, therefore we have exclusive access because all
        // readers have finished reading this slot
        unsafe { &mut *self.shared.slots[idx].inner.get() }.write(val.take().unwrap());

        self.shared.slots[idx]
            .remaining
            .store(self.readers.len(), Ordering::Release);

        // We are the only reader, therefore `self.head` can't change under us
        self.shared.head.store(head + 1, Ordering::Release);

        Ok(())
    }

    pub fn add_rx(&mut self) -> BusReceiver<T> {
        // Next cant change because we are the only writer
        // We establish this (id, next) pair atomically, therefore all future slots >= next must be
        // cleaned up (received, or accounted for by the writer if the reader leaves)
        let next = self.shared.head.load(Ordering::Acquire);

        let id = self.next_reader_id;
        self.next_reader_id += 1;

        let info = Arc::new(ReaderInfo {
            waker: AtomicWaker::new(),
            id,
            cleanup_state: AtomicU8::new(reader_cleanup::RUNNING),
            next: AtomicUsize::new(0),
        });
        self.readers.push(Arc::clone(&info));

        BusReceiver::new(Arc::clone(&self.shared), info, next)
    }

    /// Tries to cleanup readers which have left since the last attempt
    ///
    /// Returns early if `waiting_on_fence` is set to an index whose `remaining` count becomes zero
    /// as part of cleanup
    fn try_cleanup_readers(&mut self, waiting_on_fence: Option<usize>, scan: bool) {
        while self.shared.left_reads_count.load(Ordering::Acquire) >= 1 || scan {
            // TODO: store id of least recently left reader as optimization to avoid scanning
            // when readers are less often than when `poll_broadcast` is called
            let Some(left_reader_index) = self.readers.iter().position(|r| {
                r.cleanup_state.load(Ordering::Acquire) == reader_cleanup::WRITER_CLEANUP
            }) else {
                if scan {
                    // no more readers to clean
                    break;
                } else {
                    // Should be impossible because we release increment `left_reads_count` after
                    // doing a release store of WRITER_CLEANUP to `cleanup_state`,
                    // therefore if we observe `left_reads_count >=1`, there must be at least one
                    // reader whole elements can be freed
                    unreachable!(
                        "readers_left_count non-zero, but failed to find a reader with flag set!"
                    );
                }
            };

            let info = self.readers.remove(left_reader_index);

            // The release store(true) to `has_left` happens after the release store to `left_with_next`
            // Because `has_left` is not reused after being set by a leaving reader we are
            // seeing the reader's next value at the time it left
            let reader_tail = info.next.load(Ordering::Acquire);
            let reader_head = self.shared.head.load(Ordering::Acquire);

            // All slots between reader_tail and reader_head need to be cleaned up (decremented
            // by one, dropping the value if last).
            // We already removed from `self.readers`, so all future slots' `remaining` count
            // will be correct.
            // Also because we have exclusive access to self and there is only
            // one reader, there is no race possible between head and `readers.len()`

            let mut fence_ready = false;
            for i in reader_tail..reader_head {
                let idx = i % self.shared.slots.len();
                // SAFETY:
                // TODO
                let readers_remaining = unsafe { self.shared.cleanup(idx) };

                if let Some(fence) = waiting_on_fence {
                    if idx == fence && readers_remaining == 0 {
                        fence_ready = true;
                    }
                }
            }

            self.shared.left_reads_count.fetch_sub(1, Ordering::AcqRel);

            if fence_ready {
                // Don't continue to cleanup crap if our fence is now ready
                // For now return here to decrease broadcast latency
                // However there may be more readers we could cleanup right now, and doing so
                // might reduce the amount of cleanup this thread would do in the future, (by
                // having `remaining` actually set to the number of live readers)
                // TODO: benchmark tradeoff
                break;
            }
        }
    }
}

impl<T> Drop for Bus<T>
where
    T: Send + Clone,
{
    fn drop(&mut self) {
        // We need to negotiate with all negotiate with all readers and decide who is going to
        // clean up their unreceived elements. As we are being drop and want to keep the latency
        // reasonable, try to force readers to clean up after themselves, if this fails (we race to
        // decide or they have already left) then we will clean up after them

        for reader in &self.readers {
            let mut state = reader.cleanup_state.load(Ordering::Acquire);
            while state == reader_cleanup::RUNNING {
                match reader.cleanup_state.compare_exchange_weak(
                    state,
                    reader_cleanup::READER_CLEANUP,
                    Ordering::AcqRel,
                    Ordering::Relaxed,
                ) {
                    Ok(v) => state = v,
                    Err(new) => state = new,
                }
            }
        }

        // TODO: Stronger barrier?
        // Ensure that readers leaving don't race with dropping self.
        atomic::fence(Ordering::Release);

        // Now clean up each the elements for each reader we are responsible for

        self.try_cleanup_readers(None, true);

        // the only remaining readers are responsible for cleaning up after themselves
        debug_assert_eq!(
            self.readers
                .iter()
                .filter(|r| r.cleanup_state.load(Ordering::Acquire) == reader_cleanup::RUNNING)
                .count(),
            0
        );

        debug_assert_eq!(
            self.readers
                .iter()
                .filter(
                    |r| r.cleanup_state.load(Ordering::Acquire) == reader_cleanup::WRITER_CLEANUP
                )
                .count(),
            0
        );
    }
}

#[must_use = "futures do nothing unless you `.await` or poll them"]
#[pin_project::pin_project]
pub struct Broadcast<'b, T>
where
    T: Send + Clone,
{
    sender: Pin<&'b mut Bus<T>>,
    val: Option<T>,
}

impl<'b, T> Future for Broadcast<'b, T>
where
    T: Send + Clone,
{
    type Output = ();

    fn poll(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Self::Output> {
        let mut this = self.project();
        this.sender.as_mut().poll_broadcast(&mut this.val, cx)
    }
}
