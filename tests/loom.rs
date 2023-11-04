#![cfg(loom)]

pub use hyperbus::prelude::*;
use std::sync::{atomic::Ordering, Arc};

macro_rules! loom_async_model {
    ($block:expr) => {
        loom::model(|| loom::future::block_on($block));
    };
}

fn loom_spawn<F>(f: F) -> loom::thread::JoinHandle<()>
where
    F: futures::Future<Output = ()> + 'static,
{
    loom::thread::spawn(move || {
        loom::future::block_on(f);
    })
}

#[test]
fn one_rx() {
    loom_async_model!(async {
        let mut tx = Bus::new(16);
        let mut rx1 = tx.add_rx();

        tx.broadcast(123).await;

        loom_spawn(async move {
            assert_eq!(rx1.recv().await.unwrap(), 123);
        });
    });
}

#[test]
fn two_rxs() {
    loom_async_model!(async {
        let mut tx = Bus::new(16);
        let mut rx1 = tx.add_rx();
        let mut rx2 = tx.add_rx();

        tx.broadcast(123).await;

        loom_spawn(async move {
            assert_eq!(rx1.recv().await.unwrap(), 123);
        });
        loom_spawn(async move {
            assert_eq!(rx2.recv().await.unwrap(), 123);
        });
    });
}

#[test]
fn leave1() {
    loom_async_model!(async {
        let mut tx = Bus::<CountDrops>::new(16);
        let mut rx1 = tx.add_rx();

        let drops0 = CountDrops::new(0);
        let ndrops0 = drops0.counter();

        let drops1 = CountDrops::new(1);
        let ndrops1 = drops1.counter();

        tx.broadcast(drops0).await;
        tx.broadcast(drops1).await;

        let t1 = loom_spawn(async move {
            assert_eq!(rx1.recv().await.unwrap().val, 0);
            assert_eq!(ndrops0.load(Ordering::Acquire), 1);
            rx1.leave();
        });

        drop(tx);

        t1.join().unwrap();

        assert_eq!(ndrops1.load(Ordering::Acquire), 1);
    });
}

#[test]
fn leave2() {
    loom_async_model!(async {
        let mut tx = Bus::<CountDrops>::new(16);

        let mut rx1 = tx.add_rx();
        let drops0 = CountDrops::new(0);
        let ndrops0 = drops0.counter();
        tx.broadcast(drops0).await;

        let mut rx2 = tx.add_rx();
        let drops1 = CountDrops::new(1);
        let ndrops1 = drops1.counter();
        tx.broadcast(drops1).await;

        let t1 = loom_spawn(async move {
            assert_eq!(rx1.recv().await.unwrap().val, 0);
            assert_eq!(rx1.recv().await.unwrap().val, 1);
            rx1.leave();
        });

        let t2 = loom_spawn(async move {
            assert_eq!(rx2.recv().await.unwrap().val, 1);
            rx2.leave();
        });

        drop(tx);

        t1.join().unwrap();
        t2.join().unwrap();

        assert_eq!(ndrops0.load(Ordering::Acquire), 1);
        // value received on two threads and dropped twice (after clone), or more than twice due to
        // semantics of take
        assert!(ndrops1.load(Ordering::Acquire) >= 2);
    });
}

#[derive(Clone)]
pub struct CountDrops {
    counter: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    val: usize,
}

impl CountDrops {
    pub fn new(val: usize) -> Self {
        Self {
            counter: Default::default(),
            val,
        }
    }

    pub fn counter(&self) -> Arc<std::sync::atomic::AtomicUsize> {
        Arc::clone(&self.counter)
    }
}

impl Drop for CountDrops {
    fn drop(&mut self) {
        self.counter.fetch_add(1, Ordering::SeqCst);
    }
}
