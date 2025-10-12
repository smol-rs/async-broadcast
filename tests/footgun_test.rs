//! Test demonstrating the ergonomic patterns that now work with async-broadcast
//!
//! These tests verify that common usage patterns work as users would expect:
//! 1. Create a channel and return just the sender (common pattern)
//! 2. Create receivers later as needed
//! 3. Send messages
//!
//! Before the ergonomic fix: These would panic because the channel was auto-closed
//! After the ergonomic fix: These work as users expect!

use async_broadcast;
use futures_lite::future::block_on;
use std::sync::Arc;

/// Common pattern: Create channel in setup, return sender for use elsewhere
fn setup_broadcast_channel() -> async_broadcast::Sender<i32> {
    let (tx, _rx) = async_broadcast::broadcast(10);
    // _rx is dropped here, auto-closing the channel
    // But when we create a new receiver, the channel will re-open!
    tx
}

#[test]
fn test_setup_function_pattern() {
    block_on(async {
        // This is a reasonable pattern: get a sender from a setup function
        let tx = setup_broadcast_channel();

        // Create a receiver to use
        let mut rx = tx.new_receiver();

        // This now works! The channel re-opens when new_receiver() is called
        tx.broadcast(42).await.unwrap();

        assert_eq!(rx.recv().await.unwrap(), 42);
    });
}

#[test]
fn test_return_sender_pattern() {
    block_on(async {
        // Common pattern: Create channel, use sender elsewhere
        let (tx, _) = async_broadcast::broadcast::<Arc<String>>(10);
        // Initial receiver dropped - channel auto-closed

        // Later, create receivers as needed
        let mut rx1 = tx.new_receiver();
        let mut rx2 = tx.new_receiver();

        // This now works! Channel re-opened when first new_receiver() was called
        tx.broadcast(Arc::new("hello".to_string())).await.unwrap();

        assert_eq!(*rx1.recv().await.unwrap(), "hello");
        assert_eq!(*rx2.recv().await.unwrap(), "hello");
    });
}

#[test]
fn test_multiple_senders_scenario() {
    block_on(async {
        // Scenario: Multiple producers, late subscriber pattern
        let (tx1, _) = async_broadcast::broadcast::<i32>(100);
        let tx2 = tx1.clone();
        let tx3 = tx1.clone();

        // Later, a consumer joins
        let mut rx = tx1.new_receiver();

        // All senders work! Channel re-opened when new_receiver() was called
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
        // This still works: Keep a dummy receiver alive
        let (tx, _dummy_rx) = async_broadcast::broadcast::<i32>(10);

        let mut rx = tx.new_receiver();

        // This works because _dummy_rx is still alive (channel never closed)
        tx.broadcast(42).await.unwrap();

        assert_eq!(rx.recv().await.unwrap(), 42);
    });
}

#[test]
fn test_what_users_expect_to_work() {
    block_on(async {
        // What users reasonably expect to work now works!
        // 1. Create channel
        let (tx, _) = async_broadcast::broadcast::<i32>(10);

        // 2. Create receiver(s) as needed
        let mut rx1 = tx.new_receiver();
        let mut rx2 = tx.new_receiver();

        // 3. Send messages - this now works!
        tx.broadcast(42).await.unwrap();

        // Verify receivers get the message
        assert_eq!(rx1.recv().await.unwrap(), 42);
        assert_eq!(rx2.recv().await.unwrap(), 42);
    });
}

#[test]
fn test_manual_close_stays_closed() {
    block_on(async {
        // Verify that manually closing the channel keeps it closed
        let (tx, _) = async_broadcast::broadcast::<i32>(10);

        // Manually close the channel
        tx.close();

        // Try to create a new receiver - channel should stay closed
        let _rx = tx.new_receiver();

        // Broadcasting should fail because of manual close
        let result = tx.broadcast(42).await;
        assert!(result.is_err(), "Manual close should keep channel closed");
    });
}
