#![cfg(not(loom))]

use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use hyperbus::prelude::*;

#[test]
fn feels_good() {
    futures::executor::block_on(async {
        let mut tx = Bus::new(16);
        let mut rx1 = tx.add_rx();
        let mut rx2 = tx.add_rx();
        let mut rx3 = tx.add_rx();

        tx.broadcast(123).await;

        assert_eq!(rx1.recv().await.unwrap(), 123);
        assert_eq!(rx2.recv().await.unwrap(), 123);
        assert_eq!(rx3.recv().await.unwrap(), 123);

        drop(tx);

        assert!(rx1.recv().await.is_err());
        assert!(rx2.recv().await.is_err());
        assert!(rx3.recv().await.is_err());
        assert!(rx1.recv().await.is_err());
        assert!(rx2.recv().await.is_err());
        assert!(rx3.recv().await.is_err());
    });
}

#[test]
fn it_fails_when_full() {
    let mut c = Bus::new(1);
    let r1 = c.add_rx();
    assert!(c.try_broadcast(String::from("bus")).is_ok());
    assert!(c.try_broadcast(String::new()).is_err());
    drop(r1);
}

#[test]
fn it_succeeds_when_not_full() {
    let mut c = Bus::new(1);
    let mut r1 = c.add_rx();
    assert!(c.try_broadcast(String::from("bus")).is_ok());
    assert!(c.try_broadcast(String::new()).is_err());

    assert_eq!(r1.try_recv(), Ok("bus".into()));
    assert!(c.try_broadcast(String::from("bus")).is_ok());
}

#[test]
fn it_fails_when_empty() {
    let mut c = Bus::<bool>::new(10);
    let mut r1 = c.add_rx();
    let mut r2 = c.add_rx();
    assert_eq!(r1.try_recv(), Err(TryRecvError::Empty));
    assert_eq!(r2.try_recv(), Err(TryRecvError::Empty));
}

#[test]
fn it_reads_when_full() {
    let mut c = Bus::new(1);
    let mut r1 = c.add_rx();
    assert_eq!(c.try_broadcast(String::from("bus")), Ok(()));
    assert_eq!(r1.try_recv(), Ok("bus".into()));
}

#[test]
fn it_detects_closure() {
    let mut tx = Bus::new(1);
    let mut rx = tx.add_rx();
    assert_eq!(tx.try_broadcast(String::from("bus")), Ok(()));
    assert_eq!(rx.try_recv(), Ok("bus".into()));
    assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    drop(tx);
    assert_eq!(rx.try_recv(), Err(TryRecvError::Disconnected));
}

#[test]
fn it_recvs_after_close() {
    let mut tx = Bus::new(1);
    let mut rx = tx.add_rx();
    assert_eq!(tx.try_broadcast(String::from("bus")), Ok(()));
    drop(tx);
    assert_eq!(rx.try_recv(), Ok("bus".into()));
    assert_eq!(rx.try_recv(), Err(TryRecvError::Disconnected));
}

#[test]
fn it_handles_leaves() {
    let mut c = Bus::new(1);
    let mut r1 = c.add_rx();
    let r2 = c.add_rx();
    assert_eq!(c.try_broadcast(String::from("bus")), Ok(()));
    drop(r2);
    assert_eq!(r1.try_recv(), Ok("bus".into()));
    assert_eq!(c.try_broadcast("bus".into()), Ok(()));
}

#[tokio::test]
async fn it_runs_blocked_writes() {
    let mut c = Box::new(Bus::new(1));
    let mut r1 = c.add_rx();
    c.try_broadcast(true).unwrap(); // this is fine

    // buffer is now full
    assert_eq!(c.try_broadcast(false), Err(false));

    // start other thread that blocks
    let c = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        c.broadcast(false).await;
    });

    // unblock writer by receiving
    assert_eq!(r1.recv().await, Ok(true));
    // drop r1 to release other thread and safely drop c
    drop(r1);
    c.await.unwrap();
}

#[tokio::test]
async fn it_runs_blocked_reads() {
    let mut tx = Box::new(Bus::new(1));
    let mut rx = tx.add_rx();
    // buffer is now empty
    assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    // start other task that blocks
    let c = tokio::spawn(async move {
        rx.recv().await.unwrap();
    });

    tokio::time::sleep(Duration::from_millis(10)).await;
    // unblock receiver by broadcasting
    tx.broadcast(String::from("bus")).await;
    // check that thread now finished
    c.await.unwrap();
}

#[tokio::test]
async fn test_busy() {
    // start a bus with limited space
    let mut bus = Bus::new(1);

    // first receiver only receives 5 items
    let mut rx1 = bus.add_rx();
    let t1 = tokio::spawn(async move {
        for _ in 0..5 {
            rx1.recv().await.unwrap();
        }
        drop(rx1);
    });

    // second receiver receives 10 items
    let mut rx2 = bus.add_rx();
    let t2 = tokio::spawn(async move {
        for _ in 0..10 {
            rx2.recv().await.unwrap();
        }
        drop(rx2);
    });

    // let receivers start
    tokio::time::sleep(Duration::from_millis(500)).await;

    // try to send 25 items -- should work fine
    for i in 0..25 {
        bus.broadcast(i).await;
    }

    // done sending -- wait for receivers (which should already be done)
    t1.await.unwrap();
    t2.await.unwrap();
}

#[tokio::test]
async fn in_order() {
    let num_elements = 1000;
    let threads = 10;

    let mut bus = crate::Bus::<u32>::new(4);
    let rxs: Vec<_> = (0..threads)
        .map(|_| {
            let mut rx = bus.add_rx();
            std::thread::spawn(move || {
                futures::executor::block_on(async move {
                    for i in 0..num_elements {
                        let val = rx.recv().await.unwrap();
                        assert_eq!(val, i as u32);
                    }
                });
            })
        })
        .collect();

    for i in 0..num_elements {
        bus.broadcast(i as u32).await;
        if i % 1_000_00 == 0 {
            println!("{i}");
        }
    }
    rxs.into_iter().for_each(|t| t.join().unwrap());
}

pub struct CountShallowDrops(Arc<AtomicUsize>);

impl Clone for CountShallowDrops {
    fn clone(&self) -> Self {
        // Create a new independent value,
        // We only want to count Drops done in the internal buffer _outside_ of when cloned values
        // are dropped after being received by readers
        Self(Arc::new(AtomicUsize::new(0)))
    }
}

impl Drop for CountShallowDrops {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn basic_leaving() {
    futures::executor::block_on(async {
        let mut tx = Bus::new(4);
        let mut rx0 = tx.add_rx();
        let mut rx1 = tx.add_rx();
        let mut rx2 = tx.add_rx();

        let count_0 = Arc::new(AtomicUsize::new(0));
        let count_1 = Arc::new(AtomicUsize::new(0));
        let count_2 = Arc::new(AtomicUsize::new(0));

        tx.broadcast(CountShallowDrops(Arc::clone(&count_0))).await;
        tx.broadcast(CountShallowDrops(Arc::clone(&count_1))).await;
        tx.broadcast(CountShallowDrops(Arc::clone(&count_2))).await;

        // value not dropped yet...
        assert_eq!(rx0.recv().await.unwrap().0.load(Ordering::Acquire), 0);
        assert_eq!(rx1.recv().await.unwrap().0.load(Ordering::Acquire), 0);
        assert_eq!(rx2.recv().await.unwrap().0.load(Ordering::Acquire), 0);

        // Dropped now (moved when rx3 recvs), then dropped in assert above
        assert_eq!(count_0.load(Ordering::Acquire), 1);
        // others not dropped
        assert_eq!(count_1.load(Ordering::Acquire), 0);
        assert_eq!(count_2.load(Ordering::Acquire), 0);

        rx0.leave();

        // value not dropped yet...
        assert_eq!(rx1.recv().await.unwrap().0.load(Ordering::Acquire), 0);
        assert_eq!(rx2.recv().await.unwrap().0.load(Ordering::Acquire), 0);

        // Cant be dropped yet since rx1 leaving hasnt been cleaned up yet...
        assert_eq!(count_1.load(Ordering::Acquire), 0);
        // others not dropped
        assert_eq!(count_2.load(Ordering::Acquire), 0);

        // value not dropped yet...
        assert_eq!(rx1.recv().await.unwrap().0.load(Ordering::Acquire), 0);
        assert_eq!(rx2.recv().await.unwrap().0.load(Ordering::Acquire), 0);

        let count_3 = Arc::new(AtomicUsize::new(0));
        tx.broadcast(CountShallowDrops(Arc::clone(&count_3))).await;

        let count_4 = Arc::new(AtomicUsize::new(0));
        tx.broadcast(CountShallowDrops(Arc::clone(&count_4))).await;

        drop(tx);

        // have readers leave _after_ to test closed cleanup logic
        rx1.leave();
        rx2.leave();

        // ensure all dropped
        assert_eq!(count_0.load(Ordering::Acquire), 1);
        assert_eq!(count_1.load(Ordering::Acquire), 1);
        assert_eq!(count_2.load(Ordering::Acquire), 1);
        assert_eq!(count_3.load(Ordering::Acquire), 1);
        assert_eq!(count_4.load(Ordering::Acquire), 1);
    });
}
