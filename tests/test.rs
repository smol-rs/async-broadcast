use std::thread::sleep;
use std::time::Duration;

use futures_util::stream::StreamExt;
use async_broadcast::*;

use easy_parallel::Parallel;
use futures_lite::future::block_on;

fn ms(ms: u64) -> Duration {
    Duration::from_millis(ms)
}

#[test]
fn basic_sync() {
    let (s, mut r1) = broadcast(10);
    let mut r2 = r1.clone();

    s.try_broadcast(7).unwrap();
    assert_eq!(r1.try_recv().unwrap(), 7);
    assert_eq!(r2.try_recv().unwrap(), 7);

    let mut r3 = r1.clone();
    s.try_broadcast(8).unwrap();
    assert_eq!(r1.try_recv().unwrap(), 8);
    assert_eq!(r2.try_recv().unwrap(), 8);
    assert_eq!(r3.try_recv().unwrap(), 8);
}

#[test]
fn basic_async() {
    block_on(async {
        let (s, mut r1) = broadcast(10);
        let mut r2 = r1.clone();

        s.broadcast(7).await.unwrap();
        assert_eq!(r1.recv().await.unwrap(), 7);
        assert_eq!(r2.recv().await.unwrap(), 7);

        // Now let's try the Stream impl.
        let mut r3 = r1.clone();
        s.broadcast(8).await.unwrap();
        assert_eq!(r1.next().await.unwrap(), 8);
        assert_eq!(r2.next().await.unwrap(), 8);
        assert_eq!(r3.next().await.unwrap(), 8);
    });
}

#[test]
fn parallel() {
    let (s, mut r1) = broadcast(2);
    let mut r2 = r1.clone();

    Parallel::new()
        .add(move || {
            sleep(ms(10));
            s.try_broadcast(7).unwrap();
        })
        .add(move || {
            assert_eq!(r1.try_recv(), Err(TryRecvError::Empty));
            assert_eq!(r2.try_recv(), Err(TryRecvError::Empty));
            sleep(ms(15));
            assert_eq!(r1.try_recv().unwrap(), 7);
            assert_eq!(r2.try_recv().unwrap(), 7);
            sleep(ms(5));
            assert_eq!(r1.try_recv(), Err(TryRecvError::Closed));
            assert_eq!(r2.try_recv(), Err(TryRecvError::Closed));
        })
        .run();
}

#[test]
fn parallel_async() {
    let (s, mut r1) = broadcast(2);
    let mut r2 = r1.clone();

    Parallel::new()
        .add(move || block_on(async move {
            sleep(ms(10));
            s.broadcast(7).await.unwrap();
        }))
        .add(move || block_on(async move {
            assert_eq!(r1.try_recv(), Err(TryRecvError::Empty));
            assert_eq!(r2.try_recv(), Err(TryRecvError::Empty));
            assert_eq!(r1.next().await.unwrap(), 7);
            assert_eq!(r2.recv().await.unwrap(), 7);
            assert_eq!(r1.recv().await, Err(RecvError));
            assert_eq!(r2.recv().await, Err(RecvError));
        }))
        .run();
}
