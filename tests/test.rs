use futures_util::stream::StreamExt;
use async_broadcast::*;

#[test]
fn sync() {
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
fn r#async() {
    pollster::block_on(async {
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
