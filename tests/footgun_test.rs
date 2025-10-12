//! Test demonstrating ergonomic patterns with async-broadcast.

use async_broadcast;
use futures_lite::future::block_on;
use std::sync::Arc;

fn setup_broadcast_channel() -> async_broadcast::Sender<i32> {
    let (tx, _rx) = async_broadcast::broadcast(10);
    tx
}

#[test]
fn test_setup_function_pattern() {
    block_on(async {
        let tx = setup_broadcast_channel();
        let mut rx = tx.new_receiver();
        tx.broadcast(42).await.unwrap();
        assert_eq!(rx.recv().await.unwrap(), 42);
    });
}

#[test]
fn test_return_sender_pattern() {
    block_on(async {
        let (tx, _) = async_broadcast::broadcast::<Arc<String>>(10);
        let mut rx1 = tx.new_receiver();
        let mut rx2 = tx.new_receiver();
        tx.broadcast(Arc::new("hello".to_string())).await.unwrap();
        assert_eq!(*rx1.recv().await.unwrap(), "hello");
        assert_eq!(*rx2.recv().await.unwrap(), "hello");
    });
}

#[test]
fn test_multiple_senders_scenario() {
    block_on(async {
        let (tx1, _) = async_broadcast::broadcast::<i32>(100);
        let tx2 = tx1.clone();
        let tx3 = tx1.clone();
        let mut rx = tx1.new_receiver();
        tx1.broadcast(1).await.unwrap();
        tx2.broadcast(2).await.unwrap();
        tx3.broadcast(3).await.unwrap();
        assert_eq!(rx.recv().await.unwrap(), 1);
        assert_eq!(rx.recv().await.unwrap(), 2);
        assert_eq!(rx.recv().await.unwrap(), 3);
    });
}

#[test]
fn test_workaround_keep_receiver_alive() {
    block_on(async {
        let (tx, _dummy_rx) = async_broadcast::broadcast::<i32>(10);
        let mut rx = tx.new_receiver();
        tx.broadcast(42).await.unwrap();
        assert_eq!(rx.recv().await.unwrap(), 42);
    });
}

#[test]
fn test_what_users_expect_to_work() {
    block_on(async {
        let (tx, _) = async_broadcast::broadcast::<i32>(10);
        let mut rx1 = tx.new_receiver();
        let mut rx2 = tx.new_receiver();
        tx.broadcast(42).await.unwrap();
        assert_eq!(rx1.recv().await.unwrap(), 42);
        assert_eq!(rx2.recv().await.unwrap(), 42);
    });
}

#[test]
fn test_reopen_after_close() {
    block_on(async {
        let (tx, _) = async_broadcast::broadcast::<i32>(10);
        tx.close();
        let mut rx = tx.new_receiver();
        tx.broadcast(42).await.unwrap();
        assert_eq!(rx.recv().await.unwrap(), 42);
    });
}
