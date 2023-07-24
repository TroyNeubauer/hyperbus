//! Non miri, non loom tests
#![cfg(all(not(loom), not(miri)))]

pub use hyperbus::prelude::*;
use futures::StreamExt;

#[tokio::test]
async fn it_streams() {
    let mut tx = Bus::new(2);
    let mut rx = tx.add_rx();
    let j = tokio::spawn(async move {
        for i in 0..1_000 {
            tx.broadcast(i).await;
        }
    });

    let mut ii = 0;
    while let Some(i) = rx.next().await {
        assert_eq!(i, ii);
        ii += 1;
    }

    j.await.unwrap();
    assert_eq!(ii, 1_000);
    assert_eq!(rx.try_recv(), Err(TryRecvError::Disconnected));
}

#[tokio::test]
async fn aggressive_iteration() {
    for _ in 0..1_000 {
        let mut tx = Bus::new(2);
        let mut rx = tx.add_rx();
        let j = tokio::spawn(async move {
            for i in 0..1_000 {
                tx.broadcast(i).await;
            }
        });

        let mut ii = 0;
        while let Some(i) = rx.next().await {
            assert_eq!(i, ii);
            ii += 1;
        }

        j.await.unwrap();
        assert_eq!(ii, 1_000);
        assert_eq!(rx.try_recv(), Err(TryRecvError::Disconnected));
    }
}

#[tokio::test]
async fn it_can_count_to_1000000() {
    let mut c = Bus::new(2);
    let mut r1 = c.add_rx();
    let j = tokio::spawn(async move {
        for i in 0..1_000_000 {
            c.broadcast(i).await;
        }
    });

    for i in 0..1_000_000 {
        assert_eq!(r1.recv().await, Ok(i));
    }

    j.await.unwrap();
    assert_eq!(r1.try_recv(), Err(TryRecvError::Disconnected));
}
