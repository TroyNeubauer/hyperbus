use crate::{prelude::*, reader_cleanup, ReaderInfo, RecvError, Shared, TryRecvError};

pub struct BusReceiver<T>
where
    T: Send + Clone,
{
    shared: Arc<Shared<T>>,

    /// Unique id of the reader
    info: Arc<ReaderInfo>,

    /// The next index to read from
    next: usize,
}

impl<T> BusReceiver<T>
where
    T: Send + Clone,
{
    pub(crate) fn new(shared: Arc<Shared<T>>, info: Arc<ReaderInfo>, next: usize) -> Self {
        Self { shared, info, next }
    }

    /// Read another broadcast message from the bus, and suspend if none are available.
    ///
    /// If the corresponding `BusSender` has been dropped, or it is dropped while this call is awaiting,
    /// this call will complete with `Err` to indicate that no more messages can ever be
    /// received on this channel. However, since channels are buffered, messages sent before the
    /// disconnect will still be properly received.
    pub fn recv(&mut self) -> Recv<T> {
        Recv {
            inner: Pin::new(self),
        }
    }

    pub fn poll_recv(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<T, RecvError>> {
        match self.as_mut().try_recv_inner() {
            Ok(t) => Poll::Ready(Ok(t)),
            Err(TryRecvError::Disconnected) => Poll::Ready(Err(RecvError)),
            Err(TryRecvError::Empty) => {
                // TODO: check again? Orderings here and sprious wakeups
                self.info.waker.register(cx.waker());
                Poll::Pending
            }
        }
    }

    /// Tries to read a value from the bus without blocking
    pub fn try_recv(&mut self) -> Result<T, TryRecvError> {
        Pin::new(self).try_recv_inner()
    }

    /// Tries to remove an element from the bus, without handling wakeups
    fn try_recv_inner(mut self: Pin<&mut Self>) -> Result<T, TryRecvError> {
        let head = self.shared.head.load(Ordering::Acquire);
        if self.next >= head {
            if Arc::strong_count(&self.info) == 1 {
                return Err(TryRecvError::Disconnected);
            } else {
                // TODO: make sure we dont race with the writer being dropped here, miss a wakeup and hang
                return Err(TryRecvError::Empty);
            }
        }

        #[cfg(debug_assertions)]
        {
            let tail = self.shared.tail.load(Ordering::Acquire);
            assert!(self.next >= tail);
        }
        let idx = self.next % self.shared.slots.len();
        let v = unsafe { self.shared.take(idx) };
        self.next += 1;

        Ok(v)
    }

    pub fn leave(self) {
        // Drop impl handles cleanup
    }
}

impl<T> futures_core::Stream for BusReceiver<T>
where
    T: Send + Clone,
{
    type Item = T;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Ready(ready!(self.poll_recv(cx)).ok())
    }
}

impl<T> Drop for BusReceiver<T>
where
    T: Send + Clone,
{
    fn drop(&mut self) {
        // Any unreceived items in the bus need to be accounted for to avoid leaking memory and
        // deadlocking the internal buffer.
        // Store our next element so that if the writer is still around it can release our
        // unreceived elements. Otherwise we must clean up ourselves.
        self.info.next.store(self.next, Ordering::Release);

        let mut state = self.info.cleanup_state.load(Ordering::Acquire);

        while state == reader_cleanup::RUNNING {
            match self.info.cleanup_state.compare_exchange_weak(
                state,
                reader_cleanup::WRITER_CLEANUP,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(new) => {
                    state = new;
                }
                Err(new_state) => state = new_state,
            }
        }

        // TODO: document and double check
        std::sync::atomic::fence(Ordering::Acquire);

        if state == reader_cleanup::READER_CLEANUP {
            // we need to clean our own stuff - writer exited already
            let head = self.shared.head.load(Ordering::Acquire);
            let tail = self.next;

            for i in tail..head {
                let idx = i % self.shared.slots.len();
                // SAFETY:
                // TODO
                unsafe { self.shared.cleanup(idx) };
            }
        } else if state == reader_cleanup::WRITER_CLEANUP {
            // TODO: possible race with writer finding us, since it looks for `left_reads_count >= 1`,
            // but here we already commited to having the writer free our elements but it might not know we
            // need its help
            dbg!(self.shared.left_reads_count.fetch_add(1, Ordering::AcqRel));

            // TODO: maybe wake writer? It may be able to make progress now that our elements can
            // be freed. Maybe solved with scan flag in `try_cleanup_readers`
        } else {
            unreachable!()
        }
    }
}

#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct Recv<'b, T>
where
    T: Send + Clone,
{
    inner: Pin<&'b mut BusReceiver<T>>,
}

impl<T> Future for Recv<'_, T>
where
    T: Send + Clone,
{
    type Output = Result<T, RecvError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> std::task::Poll<Self::Output> {
        self.inner.as_mut().poll_recv(cx)
    }
}
