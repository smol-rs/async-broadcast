//! Async broadcast channel
//!
//! An async multi-producer multi-consumer broadcast channel, where each consumer gets a clone of every
//! message sent on the channel. For obvious reasons, the channel can only be used to broadcast types
//! that implement [`Clone`].
//!
//! A channel has the [`Sender`] and [`Receiver`] side. Both sides are cloneable and can be shared
//! among multiple threads.
//!
//! When all `Sender`s or all `Receiver`s are dropped, the channel becomes closed. When a channel is
//! closed, no more messages can be sent, but remaining messages can still be received.
//!
//! The channel can also be closed manually by calling [`Sender::close()`] or [`Receiver::close()`].
//!
//! ## Examples
//!
//! ```rust
//! use async_broadcast::{broadcast, TryRecvError};
//! use futures_lite::{future::block_on, stream::StreamExt};
//!
//! block_on(async move {
//!     let (s1, mut r1) = broadcast(2);
//!     let s2 = s1.clone();
//!     let mut r2 = r1.clone();
//!
//!     // Send 2 messages from two different senders.
//!     s1.broadcast(7).await.unwrap();
//!     s2.broadcast(8).await.unwrap();
//!
//!     // Channel is now at capacity so sending more messages will result in an error.
//!     assert!(s2.try_broadcast(9).unwrap_err().is_full());
//!     assert!(s1.try_broadcast(10).unwrap_err().is_full());
//!
//!     // We can use `recv` method of the `Stream` implementation to receive messages.
//!     assert_eq!(r1.next().await.unwrap(), 7);
//!     assert_eq!(r1.recv().await.unwrap(), 8);
//!     assert_eq!(r2.next().await.unwrap(), 7);
//!     assert_eq!(r2.recv().await.unwrap(), 8);
//!
//!     // All receiver got all messages so channel is now empty.
//!     assert_eq!(r1.try_recv(), Err(TryRecvError::Empty));
//!     assert_eq!(r2.try_recv(), Err(TryRecvError::Empty));
//!
//!     // Drop both senders, which closes the channel.
//!     drop(s1);
//!     drop(s2);
//!
//!     assert_eq!(r1.try_recv(), Err(TryRecvError::Closed));
//!     assert_eq!(r2.try_recv(), Err(TryRecvError::Closed));
//! })
//! ```
//!
//! ## Difference with `async-channel`
//!
//! This crate is similar to [`async-channel`] in that they both provide an MPMC channel but the
//! main difference being that in `async-channel`, each message sent on the channel is only received
//! by one of the receivers. `async-broadcast` on the other hand, delivers each message to every
//! receiver (IOW broadcast) by cloning it for each receiver.
//!
//! [`async-channel`]: https://crates.io/crates/async-channel
//!
//! ## Difference with other broadcast crates
//!
//! * [`broadcaster`]: The main difference would be that `broadcaster` doesn't have a sender and
//!   receiver split and both sides use clones of the same BroadcastChannel instance. The messages
//!   are sent are sent to all channel clones. While this can work for many cases, the lack of
//!   sender and receiver split, means that often times, you'll find yourself having to drain the
//!   channel on the sending side yourself.
//!
//! * [`postage`]: this crate provides a [broadcast API][pba] similar to `async_broadcast`. However,
//!   it:
//!   - (at the time of this writing) duplicates [futures] API, which isn't ideal.
//!   - Does not support overflow mode nor has the concept of inactive receivers, so a slow or
//!     inactive receiver blocking the whole channel is not a solvable problem.
//!   - Provides all kinds of channels, which is generally good but if you just need a broadcast
//!     channel, `async_broadcast` is probably a better choice.
//!
//! * [`tokio::sync`]: Tokio's `sync` module provides a [broadcast channel][tbc] API. The differences
//!    here are:
//!   - While this implementation does provide [overflow mode][tom], it is the default behavior and not
//!     opt-in.
//!   - There is no equivalent of inactive receivers.
//!   - While it's possible to build tokio with only the `sync` module, it comes with other APIs that
//!     you may not need.
//!
//! [`broadcaster`]: https://crates.io/crates/broadcaster
//! [`postage`]: https://crates.io/crates/postage
//! [pba]: https://docs.rs/postage/0.4.1/postage/broadcast/fn.channel.html
//! [futures]: https://crates.io/crates/futures
//! [`tokio::sync`]: https://docs.rs/tokio/1.6.0/tokio/sync
//! [tbc]: https://docs.rs/tokio/1.6.0/tokio/sync/broadcast/index.html
//! [tom]: https://docs.rs/tokio/1.6.0/tokio/sync/broadcast/index.html#lagging
//!
#![forbid(unsafe_code, future_incompatible, rust_2018_idioms)]
#![deny(missing_debug_implementations, nonstandard_style)]
#![warn(missing_docs, missing_doc_code_examples, unreachable_pub)]

#[cfg(doctest)]
mod doctests {
    doc_comment::doctest!("../README.md");
}

use std::collections::VecDeque;
use std::convert::TryInto;
use std::error;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use event_listener::{Event, EventListener};
use futures_core::{ready, stream::Stream};

/// Create a new broadcast channel.
///
/// The created channel has space to hold at most `cap` messages at a time.
///
/// # Panics
///
/// Capacity must be a positive number. If `cap` is zero, this function will panic.
///
/// # Examples
///
/// ```
/// # futures_lite::future::block_on(async {
/// use async_broadcast::{broadcast, TryRecvError, TrySendError};
///
/// let (s, mut r1) = broadcast(1);
/// let mut r2 = r1.clone();
///
/// assert_eq!(s.broadcast(10).await, Ok(None));
/// assert_eq!(s.try_broadcast(20).unwrap_err().into_inner(), 20);
///
/// assert_eq!(r1.recv().await, Ok(10));
/// assert_eq!(r2.recv().await, Ok(10));
/// assert_eq!(r1.try_recv(), Err(TryRecvError::Empty));
/// assert_eq!(r2.try_recv(), Err(TryRecvError::Empty));
/// # });
/// ```
pub fn broadcast<T>(cap: usize) -> (Sender<T>, Receiver<T>) {
    assert!(cap > 0, "capacity cannot be zero");

    let inner = Arc::new(Mutex::new(Inner {
        queue: VecDeque::with_capacity(cap),
        capacity: ElementCountLimiter(cap),
        overflow: false,
        receiver_count: 1,
        inactive_receiver_count: 0,
        sender_count: 1,
        head_pos: 0,
        is_closed: false,
        send_ops: Event::new(),
        recv_ops: Event::new(),
    }));

    let s = Sender {
        inner: inner.clone(),
    };
    let r = Receiver {
        inner,
        pos: 0,
        listener: None,
    };

    (s, r)
}

/// Create a new broadcast channel with the given limiter.
///
/// A limiter tracks the contents of the channel and determines if sending an element to the
/// channel will block or cause an overflow.  This allows implementing more complex limits such as
/// a limit on the total memory size of elements in this channel.  The ususal solution of using a
/// separate semaphore to manage such limits does not work with broadcast channels as only the
/// channel tracks when the last copy of a message is removed.  See [`CapacityLimiter`] for more
/// details.
pub fn broadcast_with_limiter<T, L>(capacity: L) -> (Sender<T, L>, Receiver<T, L>)
where
    L: CapacityLimiter<T>,
{
    let inner = Arc::new(Mutex::new(Inner {
        queue: VecDeque::new(),
        capacity,
        overflow: false,
        receiver_count: 1,
        inactive_receiver_count: 0,
        sender_count: 1,
        head_pos: 0,
        is_closed: false,
        send_ops: Event::new(),
        recv_ops: Event::new(),
    }));

    let s = Sender {
        inner: inner.clone(),
    };
    let r = Receiver {
        inner,
        pos: 0,
        listener: None,
    };

    (s, r)
}

/// A limit on the capacity of a channel.
///
/// This allows for more complex limits on a channel's capacity, beyond the number of messages.
/// Each broadcast channel contains a limiter that is consulted prior to adding a message to the
/// channel and notified when a message is removed.
///
/// A limiter may also reject some elements unconditionally by returning an error that is
/// designated as fatal.  This may be useful if not all instances of a given type are suitable for
/// sending to this channel.
pub trait CapacityLimiter<T: ?Sized> {
    /// An "over capacity" error.
    ///
    /// Prefer setting this to [`FullError`] if you have no details to add.
    type Error: error::Error + 'static;

    /// Attempt to add the given item to the channel.
    ///
    /// Return `Ok` if the item may be added.  Any updates to internal state should only be done
    /// if this function returns `Ok`.  An overflowing channel will retry the add after removing an
    /// element if the returned error is not fatal.
    fn try_add<'a, I>(&mut self, item: &T, contents: I) -> Result<(), Self::Error>
    where
        T: 'a,
        I: ExactSizeIterator<Item = &'a T>;

    /// Remove an item from the channel.
    ///
    /// The item removed and the rest of the items in the channel are provided in order to allow
    /// updating internal state.  The default implementation does nothing.
    fn remove<'a, I>(&mut self, item: &T, contents: I)
    where
        T: 'a,
        I: ExactSizeIterator<Item = &'a T>,
    {
        let _ = (item, contents);
    }

    /// Hint if an error is fatal.
    ///
    /// Non-fatal errors will cause the channel to begin discarding elements (if the channel is
    /// overflowing) or block on send (if not) until either the channel is empty or the `try_add`
    /// succeeds.  If it is known that `try_add` will never succeed, return `true` here to cause
    /// send operations to immediately return a `Rejected` error variant.
    ///
    /// The default implementation returns false.
    fn error_is_fatal(&self, error: &Self::Error) -> bool {
        let _ = error;
        false
    }

    /// Hint if the channel is full.
    ///
    /// If this method returns true, waiting send operations will not be unblocked.  The default
    /// implementation always returns false, which may result in an extra wakeup but does not
    /// impact correctness.
    fn is_full<'a, I>(&self, contents: I) -> bool
    where
        T: 'a,
        I: ExactSizeIterator<Item = &'a T>,
    {
        let _ = contents;
        false
    }
}

/// An adjustable limit on the capacity of a channel.
pub trait AdjustableCapacityLimiter<T: ?Sized>: CapacityLimiter<T> {
    /// An adjustment message
    type Adjustment;

    /// Try to adjust the capacity according to the given message.
    ///
    /// Return `Ok` if the adjustment was successful.  Any updates to internal state should be done
    /// only if this function returns `Ok`.  This may be called multiple times with successively
    /// fewer elements if the returned error is non-fatal.
    fn try_adjust<'a, I>(
        &mut self,
        msg: &mut Self::Adjustment,
        contents: I,
    ) -> Result<(), Self::Error>
    where
        T: 'a,
        I: ExactSizeIterator<Item = &'a T>;
}

/// A [`CapacityLimiter`] based on the number of elements in the channel.
#[derive(Debug)]
pub struct ElementCountLimiter(pub usize);

impl<T: ?Sized> CapacityLimiter<T> for ElementCountLimiter {
    type Error = FullError;

    fn try_add<'a, I>(&mut self, _: &T, contents: I) -> Result<(), FullError>
    where
        T: 'a,
        I: ExactSizeIterator<Item = &'a T>,
    {
        // the current len does not yet include the new element
        if self.0 > contents.len() {
            Ok(())
        } else {
            Err(FullError)
        }
    }

    fn is_full<'a, I>(&self, contents: I) -> bool
    where
        T: 'a,
        I: ExactSizeIterator<Item = &'a T>,
    {
        self.0 <= contents.len()
    }
}

impl<T: ?Sized> AdjustableCapacityLimiter<T> for ElementCountLimiter {
    type Adjustment = usize;

    fn try_adjust<'a, I>(
        &mut self,
        msg: &mut Self::Adjustment,
        contents: I,
    ) -> Result<(), FullError>
    where
        T: 'a,
        I: ExactSizeIterator<Item = &'a T>,
    {
        if *msg >= contents.len() {
            self.0 = *msg;
            Ok(())
        } else {
            Err(FullError)
        }
    }
}

#[derive(Debug)]
struct Inner<T, L> {
    queue: VecDeque<(T, usize)>,
    capacity: L,
    receiver_count: usize,
    inactive_receiver_count: usize,
    sender_count: usize,
    /// Send sequence number of the front of the queue
    head_pos: u64,
    overflow: bool,

    is_closed: bool,

    /// Send operations waiting while the channel is full.
    send_ops: Event,

    /// Receive operations waiting while the channel is empty and not closed.
    recv_ops: Event,
}

impl<T, L: CapacityLimiter<T>> Inner<T, L> {
    /// Try receiving at the given position, returning either the element or a reference to it.
    ///
    /// Result is used here instead of Cow because we don't have a Clone bound on T.
    fn try_recv_at(&mut self, pos: &mut u64) -> Result<Result<T, &T>, TryRecvError> {
        let i = match pos.checked_sub(self.head_pos) {
            Some(i) => i
                .try_into()
                .expect("Head position more than usize::MAX behind a receiver"),
            None => {
                let count = self.head_pos - *pos;
                *pos = self.head_pos;
                return Err(TryRecvError::Overflowed(count));
            }
        };

        let last_waiter;
        if let Some((_elt, waiters)) = self.queue.get_mut(i) {
            *pos += 1;
            *waiters -= 1;
            last_waiter = *waiters == 0;
        } else {
            debug_assert_eq!(i, self.queue.len());
            if self.is_closed {
                return Err(TryRecvError::Closed);
            } else {
                return Err(TryRecvError::Empty);
            }
        }

        // If we read from the front of the queue and this is the last receiver reading it
        // we can pop the queue instead of cloning the message
        if last_waiter {
            // Only the first element of the queue should have 0 waiters
            assert_eq!(i, 0);

            // Remove the element from the queue, adjust space, and notify senders
            let elt = self.queue.pop_front().unwrap().0;
            self.capacity
                .remove(&elt, self.queue.iter().map(|(m, _)| m));
            self.head_pos += 1;
            if !self.overflow {
                // Notify 1 awaiting senders that there is now room. If there is still room in the
                // queue, the notified operation will notify another awaiting sender.
                self.send_ops.notify(1);
            }

            Ok(Ok(elt))
        } else {
            Ok(Err(&self.queue[i].0))
        }
    }

    fn is_full(&self) -> bool {
        self.capacity.is_full(self.queue.iter().map(|(m, _)| m))
    }

    /// Closes the channel and notifies all waiting operations.
    ///
    /// Returns `true` if this call has closed the channel and it was not closed already.
    fn close(&mut self) -> bool {
        if self.is_closed {
            return false;
        }

        self.is_closed = true;
        // Notify all waiting senders and receivers.
        self.send_ops.notify(usize::MAX);
        self.recv_ops.notify(usize::MAX);

        true
    }

    /// Close the channel if there aren't any receivers present anymore
    fn close_channel(&mut self) {
        if self.receiver_count == 0 && self.inactive_receiver_count == 0 {
            self.close();
        }
    }
}

impl<T, L: AdjustableCapacityLimiter<T>> Inner<T, L> {
    /// Adjust the channel capacity.
    ///
    /// There are times when you need to change the channel's capacity after creating it. If
    /// needed, the oldest messages will be dropped to shrink the channel.
    fn adjust_capacity(&mut self, mut msg: L::Adjustment) -> Result<(), L::Error> {
        while let Err(e) = self
            .capacity
            .try_adjust(&mut msg, self.queue.iter().map(|(m, _)| m))
        {
            if !self.capacity.error_is_fatal(&e) {
                if let Some((elt, _)) = self.queue.pop_front() {
                    self.head_pos += 1;
                    self.capacity
                        .remove(&elt, self.queue.iter().map(|(m, _)| m));
                } else {
                    // Failed to adjust and the channel is empty => fatal
                    return Err(e);
                }
            } else {
                return Err(e);
            }
        }
        Ok(())
    }
}

/// The sending side of the broadcast channel.
///
/// Senders can be cloned and shared among threads. When all senders associated with a channel are
/// dropped, the channel becomes closed.
///
/// The channel can also be closed manually by calling [`Sender::close()`].
#[derive(Debug)]
pub struct Sender<T, L: CapacityLimiter<T> = ElementCountLimiter> {
    inner: Arc<Mutex<Inner<T, L>>>,
}

impl<T> Sender<T> {
    /// Returns the channel capacity.
    ///
    /// # Examples
    ///
    /// ```
    /// use async_broadcast::broadcast;
    ///
    /// let (s, r) = broadcast::<i32>(5);
    /// assert_eq!(s.capacity(), 5);
    /// ```
    pub fn capacity(&self) -> usize {
        self.inner.lock().unwrap().capacity.0
    }

    /// Set the channel capacity.
    ///
    /// There are times when you need to change the channel's capacity after creating it. If the
    /// `new_cap` is less than the number of messages in the channel, the oldest messages will be
    /// dropped to shrink the channel.
    ///
    /// # Examples
    ///
    /// ```
    /// use async_broadcast::{broadcast, TrySendError, TryRecvError};
    ///
    /// let (mut s, mut r) = broadcast::<i32>(3);
    /// assert_eq!(s.capacity(), 3);
    /// s.try_broadcast(1).unwrap();
    /// s.try_broadcast(2).unwrap();
    /// s.try_broadcast(3).unwrap();
    ///
    /// s.set_capacity(1);
    /// assert_eq!(s.capacity(), 1);
    /// assert_eq!(r.try_recv(), Err(TryRecvError::Overflowed(2)));
    /// assert_eq!(r.try_recv().unwrap(), 3);
    /// assert_eq!(r.try_recv(), Err(TryRecvError::Empty));
    /// s.try_broadcast(1).unwrap();
    /// assert!(matches!(s.try_broadcast(2), Err(TrySendError::Full { msg: 2, .. })));
    ///
    /// s.set_capacity(2);
    /// assert_eq!(s.capacity(), 2);
    /// s.try_broadcast(2).unwrap();
    /// assert!(matches!(s.try_broadcast(2), Err(TrySendError::Full { msg: 2, .. })));
    /// ```
    pub fn set_capacity(&mut self, new_cap: usize) {
        let _ = self.inner.lock().unwrap().adjust_capacity(new_cap);
    }
}

impl<T, L: CapacityLimiter<T>> Sender<T, L> {
    /// If overflow mode is enabled on this channel.
    ///
    /// # Examples
    ///
    /// ```
    /// use async_broadcast::broadcast;
    ///
    /// let (s, r) = broadcast::<i32>(5);
    /// assert!(!s.overflow());
    /// ```
    pub fn overflow(&self) -> bool {
        self.inner.lock().unwrap().overflow
    }

    /// Set overflow mode on the channel.
    ///
    /// When overflow mode is set, broadcasting to the channel will succeed even if the channel is
    /// full. It achieves that by removing the oldest message from the channel.
    ///
    /// # Examples
    ///
    /// ```
    /// use async_broadcast::{broadcast, TrySendError, TryRecvError};
    ///
    /// let (mut s, mut r) = broadcast::<i32>(2);
    /// s.try_broadcast(1).unwrap();
    /// s.try_broadcast(2).unwrap();
    /// assert!(matches!(s.try_broadcast(3), Err(TrySendError::Full { msg: 3, .. })));
    /// s.set_overflow(true);
    /// assert_eq!(s.try_broadcast(3).unwrap(), Some(1));
    /// assert_eq!(s.try_broadcast(4).unwrap(), Some(2));
    ///
    /// assert_eq!(r.try_recv(), Err(TryRecvError::Overflowed(2)));
    /// assert_eq!(r.try_recv().unwrap(), 3);
    /// assert_eq!(r.try_recv().unwrap(), 4);
    /// assert_eq!(r.try_recv(), Err(TryRecvError::Empty));
    /// ```
    pub fn set_overflow(&mut self, overflow: bool) {
        self.inner.lock().unwrap().overflow = overflow;
    }

    /// Closes the channel.
    ///
    /// Returns `true` if this call has closed the channel and it was not closed already.
    ///
    /// The remaining messages can still be received.
    ///
    /// # Examples
    ///
    /// ```
    /// # futures_lite::future::block_on(async {
    /// use async_broadcast::{broadcast, RecvError};
    ///
    /// let (s, mut r) = broadcast(1);
    /// s.broadcast(1).await.unwrap();
    /// assert!(s.close());
    ///
    /// assert_eq!(r.recv().await.unwrap(), 1);
    /// assert_eq!(r.recv().await, Err(RecvError::Closed));
    /// # });
    /// ```
    pub fn close(&self) -> bool {
        self.inner.lock().unwrap().close()
    }

    /// Returns `true` if the channel is closed.
    ///
    /// # Examples
    ///
    /// ```
    /// # futures_lite::future::block_on(async {
    /// use async_broadcast::{broadcast, RecvError};
    ///
    /// let (s, r) = broadcast::<()>(1);
    /// assert!(!s.is_closed());
    ///
    /// drop(r);
    /// assert!(s.is_closed());
    /// # });
    /// ```
    pub fn is_closed(&self) -> bool {
        self.inner.lock().unwrap().is_closed
    }

    /// Returns `true` if the channel is empty.
    ///
    /// # Examples
    ///
    /// ```
    /// # futures_lite::future::block_on(async {
    /// use async_broadcast::broadcast;
    ///
    /// let (s, r) = broadcast(1);
    ///
    /// assert!(s.is_empty());
    /// s.broadcast(1).await;
    /// assert!(!s.is_empty());
    /// # });
    /// ```
    pub fn is_empty(&self) -> bool {
        self.inner.lock().unwrap().queue.is_empty()
    }

    /// Returns `true` if the channel is full.
    ///
    /// # Examples
    ///
    /// ```
    /// # futures_lite::future::block_on(async {
    /// use async_broadcast::broadcast;
    ///
    /// let (s, r) = broadcast(1);
    ///
    /// assert!(!s.is_full());
    /// s.broadcast(1).await;
    /// assert!(s.is_full());
    /// # });
    /// ```
    pub fn is_full(&self) -> bool {
        self.inner.lock().unwrap().is_full()
    }

    /// Returns the number of messages in the channel.
    ///
    /// # Examples
    ///
    /// ```
    /// # futures_lite::future::block_on(async {
    /// use async_broadcast::broadcast;
    ///
    /// let (s, r) = broadcast(2);
    /// assert_eq!(s.len(), 0);
    ///
    /// s.broadcast(1).await;
    /// s.broadcast(2).await;
    /// assert_eq!(s.len(), 2);
    /// # });
    /// ```
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().queue.len()
    }

    /// Returns the number of receivers for the channel.
    ///
    /// This does not include inactive receivers. Use [`Sender::inactive_receiver_count`] if you
    /// are interested in that.
    ///
    /// # Examples
    ///
    /// ```
    /// use async_broadcast::broadcast;
    ///
    /// let (s, r) = broadcast::<()>(1);
    /// assert_eq!(s.receiver_count(), 1);
    /// let r = r.deactivate();
    /// assert_eq!(s.receiver_count(), 0);
    ///
    /// let r2 = r.activate_cloned();
    /// assert_eq!(r.receiver_count(), 1);
    /// assert_eq!(r.inactive_receiver_count(), 1);
    /// ```
    pub fn receiver_count(&self) -> usize {
        self.inner.lock().unwrap().receiver_count
    }

    /// Returns the number of inactive receivers for the channel.
    ///
    /// # Examples
    ///
    /// ```
    /// use async_broadcast::broadcast;
    ///
    /// let (s, r) = broadcast::<()>(1);
    /// assert_eq!(s.receiver_count(), 1);
    /// let r = r.deactivate();
    /// assert_eq!(s.receiver_count(), 0);
    ///
    /// let r2 = r.activate_cloned();
    /// assert_eq!(r.receiver_count(), 1);
    /// assert_eq!(r.inactive_receiver_count(), 1);
    /// ```
    pub fn inactive_receiver_count(&self) -> usize {
        self.inner.lock().unwrap().inactive_receiver_count
    }

    /// Returns the number of senders for the channel.
    ///
    /// # Examples
    ///
    /// ```
    /// # futures_lite::future::block_on(async {
    /// use async_broadcast::broadcast;
    ///
    /// let (s, r) = broadcast::<()>(1);
    /// assert_eq!(s.sender_count(), 1);
    ///
    /// let s2 = s.clone();
    /// assert_eq!(s.sender_count(), 2);
    /// # });
    /// ```
    pub fn sender_count(&self) -> usize {
        self.inner.lock().unwrap().sender_count
    }
}

impl<T, L: CapacityLimiter<T> + Clone> Sender<T, L> {
    /// Returns the current channel capacity limiter.
    pub fn capacity_limiter(&self) -> L {
        self.inner.lock().unwrap().capacity.clone()
    }
}

impl<T, L: AdjustableCapacityLimiter<T>> Sender<T, L> {
    /// Adjust the channel capacity.
    ///
    /// There are times when you need to change the channel's capacity after creating it. If
    /// needed, the oldest messages will be dropped to shrink the channel.
    ///
    /// # Examples
    ///
    /// ```
    /// use async_broadcast::{broadcast, TrySendError, TryRecvError};
    ///
    /// let (mut s, mut r) = broadcast::<i32>(3);
    /// s.try_broadcast(1).unwrap();
    /// s.try_broadcast(2).unwrap();
    /// s.try_broadcast(3).unwrap();
    ///
    /// s.adjust_capacity(1);
    /// assert_eq!(r.try_recv(), Err(TryRecvError::Overflowed(2)));
    /// assert_eq!(r.try_recv().unwrap(), 3);
    /// assert_eq!(r.try_recv(), Err(TryRecvError::Empty));
    /// s.try_broadcast(1).unwrap();
    /// assert!(matches!(s.try_broadcast(2), Err(TrySendError::Full { msg: 2, .. })));
    ///
    /// s.adjust_capacity(2);
    /// s.try_broadcast(2).unwrap();
    /// assert!(matches!(s.try_broadcast(2), Err(TrySendError::Full { msg: 2, .. })));
    /// ```
    pub fn adjust_capacity(&mut self, adjustment: L::Adjustment) {
        let _ = self.inner.lock().unwrap().adjust_capacity(adjustment);
    }
}

impl<T: Clone, L: CapacityLimiter<T>> Sender<T, L> {
    /// Broadcasts a message on the channel.
    ///
    /// If the channel is full, this method waits until there is space for a message unless overflow
    /// mode (set through [`Sender::set_overflow`]) is enabled. If the overflow mode is enabled it
    /// removes the oldest message from the channel to make room for the new message. The removed
    /// message is returned to the caller.
    ///
    /// If the channel is closed, this method returns an error.
    ///
    /// # Examples
    ///
    /// ```
    /// # futures_lite::future::block_on(async {
    /// use async_broadcast::{broadcast, SendError};
    ///
    /// let (s, r) = broadcast(1);
    ///
    /// assert_eq!(s.broadcast(1).await, Ok(None));
    /// drop(r);
    /// assert_eq!(s.broadcast(2).await, Err(SendError::Closed(2)));
    /// # });
    /// ```
    pub fn broadcast(&self, msg: T) -> Send<'_, T, L> {
        Send {
            sender: self,
            listener: None,
            msg: Some(msg),
        }
    }

    /// Attempts to broadcast a message on the channel.
    ///
    /// If the channel is full, this method returns an error unless overflow mode (set through
    /// [`Sender::set_overflow`]) is enabled. If the overflow mode is enabled, it removes the
    /// oldest message from the channel to make room for the new message. The removed message
    /// is returned to the caller.
    ///
    /// If the channel is closed, this method returns an error.
    ///
    /// # Examples
    ///
    /// ```
    /// use async_broadcast::{broadcast, TrySendError};
    ///
    /// let (s, r) = broadcast(1);
    ///
    /// assert_eq!(s.try_broadcast(1), Ok(None));
    /// assert!(matches!(s.try_broadcast(2), Err(TrySendError::Full { msg: 2, .. })));
    ///
    /// drop(r);
    /// assert_eq!(s.try_broadcast(3), Err(TrySendError::Closed(3)));
    /// ```
    pub fn try_broadcast(&self, msg: T) -> Result<Option<T>, TrySendError<T, L::Error>> {
        let mut ret = None;
        let mut inner = match self.inner.lock() {
            Ok(i) => i,
            Err(_) => return Err(TrySendError::Closed(msg)),
        };
        let inner = &mut *inner;
        if inner.is_closed {
            return Err(TrySendError::Closed(msg));
        } else if inner.receiver_count == 0 {
            assert!(inner.inactive_receiver_count != 0);

            return Err(TrySendError::Inactive(msg));
        }

        while let Err(error) = inner
            .capacity
            .try_add(&msg, inner.queue.iter().map(|(m, _)| m))
        {
            let fatal = inner.queue.is_empty() || inner.capacity.error_is_fatal(&error);
            if inner.overflow && !fatal {
                // Make room by popping a message.
                ret = inner.queue.pop_front().map(|(m, _)| m);
                if let Some(elt) = &ret {
                    inner.head_pos += 1;
                    inner
                        .capacity
                        .remove(&elt, inner.queue.iter().map(|(m, _)| m));
                    // retry the add
                    continue;
                }
            }
            if fatal {
                return Err(TrySendError::Rejected { msg, error });
            } else {
                return Err(TrySendError::Full { msg, error });
            }
        }
        let receiver_count = inner.receiver_count;
        inner.queue.push_back((msg, receiver_count));

        // Notify all awaiting receive operations.
        inner.recv_ops.notify(usize::MAX);

        Ok(ret)
    }
}

impl<T, L: CapacityLimiter<T>> Drop for Sender<T, L> {
    fn drop(&mut self) {
        let mut inner = match self.inner.lock() {
            Ok(i) => i,
            Err(_) => return,
        };
        inner.sender_count -= 1;

        if inner.sender_count == 0 {
            inner.close();
        }
    }
}

impl<T, L: CapacityLimiter<T>> Clone for Sender<T, L> {
    fn clone(&self) -> Self {
        self.inner.lock().unwrap().sender_count += 1;

        Sender {
            inner: self.inner.clone(),
        }
    }
}

/// The receiving side of a channel.
///
/// Receivers can be cloned and shared among threads. When all (active) receivers associated with a
/// channel are dropped, the channel becomes closed. You can deactivate a receiver using
/// [`Receiver::deactivate`] if you would like the channel to remain open without keeping active
/// receivers around.
#[derive(Debug)]
pub struct Receiver<T, L: CapacityLimiter<T> = ElementCountLimiter> {
    inner: Arc<Mutex<Inner<T, L>>>,
    pos: u64,

    /// Listens for a send or close event to unblock this stream.
    listener: Option<EventListener>,
}

impl<T> Receiver<T> {
    /// Returns the channel capacity.
    ///
    /// # Examples
    ///
    /// ```
    /// use async_broadcast::broadcast;
    ///
    /// let (_s, r) = broadcast::<i32>(5);
    /// assert_eq!(r.capacity(), 5);
    /// ```
    pub fn capacity(&self) -> usize {
        self.inner.lock().unwrap().capacity.0
    }

    /// Set the channel capacity.
    ///
    /// There are times when you need to change the channel's capacity after creating it. If the
    /// `new_cap` is less than the number of messages in the channel, the oldest messages will be
    /// dropped to shrink the channel.
    ///
    /// # Examples
    ///
    /// ```
    /// use async_broadcast::{broadcast, TrySendError, TryRecvError};
    ///
    /// let (s, mut r) = broadcast::<i32>(3);
    /// assert_eq!(r.capacity(), 3);
    /// s.try_broadcast(1).unwrap();
    /// s.try_broadcast(2).unwrap();
    /// s.try_broadcast(3).unwrap();
    ///
    /// r.set_capacity(1);
    /// assert_eq!(r.capacity(), 1);
    /// assert_eq!(r.try_recv(), Err(TryRecvError::Overflowed(2)));
    /// assert_eq!(r.try_recv().unwrap(), 3);
    /// assert_eq!(r.try_recv(), Err(TryRecvError::Empty));
    /// s.try_broadcast(1).unwrap();
    /// assert!(matches!(s.try_broadcast(2), Err(TrySendError::Full { msg: 2, .. })));
    ///
    /// r.set_capacity(2);
    /// assert_eq!(r.capacity(), 2);
    /// s.try_broadcast(2).unwrap();
    /// assert!(matches!(s.try_broadcast(2), Err(TrySendError::Full { msg: 2, .. })));
    /// ```
    pub fn set_capacity(&mut self, new_cap: usize) {
        let _ = self.inner.lock().unwrap().adjust_capacity(new_cap);
    }
}

impl<T, L: CapacityLimiter<T>> Receiver<T, L> {
    /// If overflow mode is enabled on this channel.
    ///
    /// # Examples
    ///
    /// ```
    /// use async_broadcast::broadcast;
    ///
    /// let (_s, r) = broadcast::<i32>(5);
    /// assert!(!r.overflow());
    /// ```
    pub fn overflow(&self) -> bool {
        self.inner.lock().unwrap().overflow
    }

    /// Set overflow mode on the channel.
    ///
    /// When overflow mode is set, broadcasting to the channel will succeed even if the channel is
    /// full. It achieves that by removing the oldest message from the channel.
    ///
    /// # Examples
    ///
    /// ```
    /// use async_broadcast::{broadcast, TrySendError, TryRecvError};
    ///
    /// let (s, mut r) = broadcast::<i32>(2);
    /// s.try_broadcast(1).unwrap();
    /// s.try_broadcast(2).unwrap();
    /// assert!(matches!(s.try_broadcast(3), Err(TrySendError::Full { msg: 3, .. })));
    /// r.set_overflow(true);
    /// assert_eq!(s.try_broadcast(3).unwrap(), Some(1));
    /// assert_eq!(s.try_broadcast(4).unwrap(), Some(2));
    ///
    /// assert_eq!(r.try_recv(), Err(TryRecvError::Overflowed(2)));
    /// assert_eq!(r.try_recv().unwrap(), 3);
    /// assert_eq!(r.try_recv().unwrap(), 4);
    /// assert_eq!(r.try_recv(), Err(TryRecvError::Empty));
    /// ```
    pub fn set_overflow(&mut self, overflow: bool) {
        self.inner.lock().unwrap().overflow = overflow;
    }

    /// Closes the channel.
    ///
    /// Returns `true` if this call has closed the channel and it was not closed already.
    ///
    /// The remaining messages can still be received.
    ///
    /// # Examples
    ///
    /// ```
    /// # futures_lite::future::block_on(async {
    /// use async_broadcast::{broadcast, RecvError};
    ///
    /// let (s, mut r) = broadcast(1);
    /// s.broadcast(1).await.unwrap();
    /// assert!(s.close());
    ///
    /// assert_eq!(r.recv().await.unwrap(), 1);
    /// assert_eq!(r.recv().await, Err(RecvError::Closed));
    /// # });
    /// ```
    pub fn close(&self) -> bool {
        self.inner.lock().unwrap().close()
    }

    /// Returns `true` if the channel is closed.
    ///
    /// # Examples
    ///
    /// ```
    /// # futures_lite::future::block_on(async {
    /// use async_broadcast::{broadcast, RecvError};
    ///
    /// let (s, r) = broadcast::<()>(1);
    /// assert!(!s.is_closed());
    ///
    /// drop(r);
    /// assert!(s.is_closed());
    /// # });
    /// ```
    pub fn is_closed(&self) -> bool {
        self.inner.lock().unwrap().is_closed
    }

    /// Returns `true` if the channel is empty.
    ///
    /// # Examples
    ///
    /// ```
    /// # futures_lite::future::block_on(async {
    /// use async_broadcast::broadcast;
    ///
    /// let (s, r) = broadcast(1);
    ///
    /// assert!(s.is_empty());
    /// s.broadcast(1).await;
    /// assert!(!s.is_empty());
    /// # });
    /// ```
    pub fn is_empty(&self) -> bool {
        self.inner.lock().unwrap().queue.is_empty()
    }

    /// Returns `true` if the channel is full.
    ///
    /// # Examples
    ///
    /// ```
    /// # futures_lite::future::block_on(async {
    /// use async_broadcast::broadcast;
    ///
    /// let (s, r) = broadcast(1);
    ///
    /// assert!(!s.is_full());
    /// s.broadcast(1).await;
    /// assert!(s.is_full());
    /// # });
    /// ```
    pub fn is_full(&self) -> bool {
        self.inner.lock().unwrap().is_full()
    }

    /// Returns the number of messages in the channel.
    ///
    /// # Examples
    ///
    /// ```
    /// # futures_lite::future::block_on(async {
    /// use async_broadcast::broadcast;
    ///
    /// let (s, r) = broadcast(2);
    /// assert_eq!(s.len(), 0);
    ///
    /// s.broadcast(1).await;
    /// s.broadcast(2).await;
    /// assert_eq!(s.len(), 2);
    /// # });
    /// ```
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().queue.len()
    }

    /// Returns the number of receivers for the channel.
    ///
    /// This does not include inactive receivers. Use [`Receiver::inactive_receiver_count`] if you
    /// are interested in that.
    ///
    /// # Examples
    ///
    /// ```
    /// use async_broadcast::broadcast;
    ///
    /// let (s, r) = broadcast::<()>(1);
    /// assert_eq!(s.receiver_count(), 1);
    /// let r = r.deactivate();
    /// assert_eq!(s.receiver_count(), 0);
    ///
    /// let r2 = r.activate_cloned();
    /// assert_eq!(r.receiver_count(), 1);
    /// assert_eq!(r.inactive_receiver_count(), 1);
    /// ```
    pub fn receiver_count(&self) -> usize {
        self.inner.lock().unwrap().receiver_count
    }

    /// Returns the number of inactive receivers for the channel.
    ///
    /// # Examples
    ///
    /// ```
    /// use async_broadcast::broadcast;
    ///
    /// let (s, r) = broadcast::<()>(1);
    /// assert_eq!(s.receiver_count(), 1);
    /// let r = r.deactivate();
    /// assert_eq!(s.receiver_count(), 0);
    ///
    /// let r2 = r.activate_cloned();
    /// assert_eq!(r.receiver_count(), 1);
    /// assert_eq!(r.inactive_receiver_count(), 1);
    /// ```
    pub fn inactive_receiver_count(&self) -> usize {
        self.inner.lock().unwrap().inactive_receiver_count
    }

    /// Returns the number of senders for the channel.
    ///
    /// # Examples
    ///
    /// ```
    /// # futures_lite::future::block_on(async {
    /// use async_broadcast::broadcast;
    ///
    /// let (s, r) = broadcast::<()>(1);
    /// assert_eq!(s.sender_count(), 1);
    ///
    /// let s2 = s.clone();
    /// assert_eq!(s.sender_count(), 2);
    /// # });
    /// ```
    pub fn sender_count(&self) -> usize {
        self.inner.lock().unwrap().sender_count
    }

    /// Downgrade to a [`InactiveReceiver`].
    ///
    /// An inactive receiver is one that can not and does not receive any messages. Its only purpose
    /// is keep the associated channel open even when there are no (active) receivers. An inactive
    /// receiver can be upgraded into a [`Receiver`] using [`InactiveReceiver::activate`] or
    /// [`InactiveReceiver::activate_cloned`].
    ///
    /// [`Sender::try_broadcast`] will return [`TrySendError::Inactive`] if only inactive
    /// receivers exists for the associated channel and [`Sender::broadcast`] will wait until an
    /// active receiver is available.
    ///
    /// # Examples
    ///
    /// ```
    /// # futures_lite::future::block_on(async {
    /// use async_broadcast::{broadcast, TrySendError};
    ///
    /// let (s, r) = broadcast(1);
    /// let inactive = r.deactivate();
    /// assert_eq!(s.try_broadcast(10), Err(TrySendError::Inactive(10)));
    ///
    /// let mut r = inactive.activate();
    /// assert_eq!(s.broadcast(10).await, Ok(None));
    /// assert_eq!(r.recv().await, Ok(10));
    /// # });
    /// ```
    pub fn deactivate(self) -> InactiveReceiver<T, L> {
        // Drop::drop impl of Receiver will take care of `receiver_count`.
        self.inner.lock().unwrap().inactive_receiver_count += 1;

        InactiveReceiver {
            inner: self.inner.clone(),
        }
    }
}

impl<T: Clone, L: CapacityLimiter<T>> Receiver<T, L> {
    /// Receives a message from the channel.
    ///
    /// If the channel is empty, this method waits until there is a message.
    ///
    /// If the channel is closed, this method receives a message or returns an error if there are
    /// no more messages.
    ///
    /// If this receiver has missed a message (only possible if overflow mode is enabled), then
    /// this method returns an error and readjusts its cursor to point to the first available
    /// message.
    ///
    /// # Examples
    ///
    /// ```
    /// # futures_lite::future::block_on(async {
    /// use async_broadcast::{broadcast, RecvError};
    ///
    /// let (s, mut r1) = broadcast(1);
    /// let mut r2 = r1.clone();
    ///
    /// assert_eq!(s.broadcast(1).await, Ok(None));
    /// drop(s);
    ///
    /// assert_eq!(r1.recv().await, Ok(1));
    /// assert_eq!(r1.recv().await, Err(RecvError::Closed));
    /// assert_eq!(r2.recv().await, Ok(1));
    /// assert_eq!(r2.recv().await, Err(RecvError::Closed));
    /// # });
    /// ```
    pub fn recv(&mut self) -> Recv<'_, T, L> {
        Recv {
            receiver: self,
            listener: None,
        }
    }

    /// Attempts to receive a message from the channel.
    ///
    /// If the channel is empty or closed, this method returns an error.
    ///
    /// If this receiver has missed a message (only possible if overflow mode is enabled), then
    /// this method returns an error and readjusts its cursor to point to the first available
    /// message.
    ///
    /// # Examples
    ///
    /// ```
    /// # futures_lite::future::block_on(async {
    /// use async_broadcast::{broadcast, TryRecvError};
    ///
    /// let (s, mut r1) = broadcast(1);
    /// let mut r2 = r1.clone();
    /// assert_eq!(s.broadcast(1).await, Ok(None));
    ///
    /// assert_eq!(r1.try_recv(), Ok(1));
    /// assert_eq!(r1.try_recv(), Err(TryRecvError::Empty));
    /// assert_eq!(r2.try_recv(), Ok(1));
    /// assert_eq!(r2.try_recv(), Err(TryRecvError::Empty));
    ///
    /// drop(s);
    /// assert_eq!(r1.try_recv(), Err(TryRecvError::Closed));
    /// assert_eq!(r2.try_recv(), Err(TryRecvError::Closed));
    /// # });
    /// ```
    pub fn try_recv(&mut self) -> Result<T, TryRecvError> {
        let mut inner = match self.inner.lock() {
            Ok(i) => i,
            Err(_) => return Err(TryRecvError::Closed),
        };

        inner
            .try_recv_at(&mut self.pos)
            .map(|cow| cow.unwrap_or_else(T::clone))
    }
}

impl<T, L: CapacityLimiter<T> + Clone> Receiver<T, L> {
    /// Returns the current channel capacity limiter.
    pub fn capacity_limiter(&self) -> L {
        self.inner.lock().unwrap().capacity.clone()
    }
}

impl<T, L: AdjustableCapacityLimiter<T>> Receiver<T, L> {
    /// Adjust the channel capacity.
    ///
    /// There are times when you need to change the channel's capacity after creating it. If
    /// needed, the oldest messages will be dropped to shrink the channel.
    ///
    /// # Examples
    ///
    /// ```
    /// use async_broadcast::{broadcast, TrySendError, TryRecvError};
    ///
    /// let (mut s, mut r) = broadcast::<i32>(3);
    /// s.try_broadcast(1).unwrap();
    /// s.try_broadcast(2).unwrap();
    /// s.try_broadcast(3).unwrap();
    ///
    /// r.adjust_capacity(1);
    /// assert_eq!(r.try_recv(), Err(TryRecvError::Overflowed(2)));
    /// assert_eq!(r.try_recv().unwrap(), 3);
    /// assert_eq!(r.try_recv(), Err(TryRecvError::Empty));
    /// s.try_broadcast(1).unwrap();
    /// assert!(matches!(s.try_broadcast(2), Err(TrySendError::Full { msg: 2, .. })));
    ///
    /// r.adjust_capacity(2);
    /// s.try_broadcast(2).unwrap();
    /// assert!(matches!(s.try_broadcast(2), Err(TrySendError::Full { msg: 2, .. })));
    /// ```
    pub fn adjust_capacity(&mut self, adjustment: L::Adjustment) {
        let _ = self.inner.lock().unwrap().adjust_capacity(adjustment);
    }
}

impl<T, L: CapacityLimiter<T>> Drop for Receiver<T, L> {
    fn drop(&mut self) {
        let mut inner = match self.inner.lock() {
            Ok(i) => i,
            Err(_) => return,
        };

        // Remove ourself from each item's counter
        loop {
            match inner.try_recv_at(&mut self.pos) {
                Ok(_) => continue,
                Err(TryRecvError::Overflowed(_)) => continue,
                Err(TryRecvError::Closed) => break,
                Err(TryRecvError::Empty) => break,
            }
        }

        inner.receiver_count -= 1;

        inner.close_channel();
    }
}

impl<T, L: CapacityLimiter<T>> Clone for Receiver<T, L> {
    fn clone(&self) -> Self {
        let mut inner = self.inner.lock().unwrap();
        inner.receiver_count += 1;
        Receiver {
            inner: self.inner.clone(),
            pos: inner.head_pos + inner.queue.len() as u64,
            listener: None,
        }
    }
}

impl<T: Clone, L: CapacityLimiter<T>> Stream for Receiver<T, L> {
    type Item = T;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            // If this stream is listening for events, first wait for a notification.
            if let Some(listener) = self.listener.as_mut() {
                ready!(Pin::new(listener).poll(cx));
                self.listener = None;
            }

            loop {
                // Attempt to receive a message.
                match self.try_recv() {
                    Ok(msg) => {
                        // The stream is not blocked on an event - drop the listener.
                        self.listener = None;
                        return Poll::Ready(Some(msg));
                    }
                    Err(TryRecvError::Closed) => {
                        // The stream is not blocked on an event - drop the listener.
                        self.listener = None;
                        return Poll::Ready(None);
                    }
                    Err(TryRecvError::Overflowed(_)) => continue,
                    Err(TryRecvError::Empty) => {}
                }

                // Receiving failed - now start listening for notifications or wait for one.
                match self.listener.as_mut() {
                    None => {
                        // Start listening and then try receiving again.
                        self.listener = {
                            let inner = match self.inner.lock() {
                                Ok(i) => i,
                                Err(_) => return Poll::Ready(None),
                            };

                            Some(inner.recv_ops.listen())
                        };
                    }
                    Some(_) => {
                        // Go back to the outer loop to poll the listener.
                        break;
                    }
                }
            }
        }
    }
}

impl<T: Clone, L: CapacityLimiter<T>> futures_core::stream::FusedStream for Receiver<T, L> {
    fn is_terminated(&self) -> bool {
        let inner = self.inner.lock().unwrap();

        inner.is_closed && inner.queue.is_empty()
    }
}

/// An error caused by exceeding a channel's capacity.
///
/// No further details are available.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub struct FullError;

impl error::Error for FullError {}

impl fmt::Display for FullError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "channel is full")
    }
}

/// An error returned from [`Sender::broadcast()`].
#[derive(PartialEq, Eq, Clone, Copy)]
pub enum SendError<T, E = FullError> {
    /// Received because the channel is closed.
    Closed(T),

    /// A fatal capacity error occurred.
    ///
    /// At least one of the following is true:
    ///
    ///  - The error returned by the [`CapacityLimiter`] was considered fatal.
    ///  - There are no elements in the channel and the limiter returned an error from its
    ///    `try_add` function.
    Rejected {
        /// The message that was not able to be sent.
        msg: T,
        /// Details about the capacity error.
        error: E,
    },
}

impl<T, E> SendError<T, E> {
    /// Unwraps the message that couldn't be sent.
    pub fn into_inner(self) -> T {
        match self {
            Self::Closed(msg) => msg,
            Self::Rejected { msg, .. } => msg,
        }
    }
}

impl<T, E: error::Error + 'static> error::Error for SendError<T, E> {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match self {
            SendError::Rejected { error, .. } => Some(error),
            _ => None,
        }
    }
}

impl<T, E: fmt::Debug> fmt::Debug for SendError<T, E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed(_) => write!(f, "SendError::Closed(..)"),
            Self::Rejected { error, .. } => write!(f, "SendError::Rejected({:?})", error),
        }
    }
}

impl<T, E: fmt::Display> fmt::Display for SendError<T, E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed(_) => write!(f, "sending into a closed channel"),
            Self::Rejected { error, .. } => write!(f, "channel capacity error: {}", error),
        }
    }
}

/// An error returned from [`Sender::try_broadcast()`].
#[derive(PartialEq, Eq, Clone, Copy)]
pub enum TrySendError<T, E = FullError> {
    /// The channel is full but not closed.
    Full {
        /// The message that did not fit.
        msg: T,
        /// Details about the capacity error.
        error: E,
    },

    /// The message was rejected; retrying will not help.
    ///
    /// At least one of the following is true:
    ///
    ///  - The error returned by the [`CapacityLimiter`] was considered fatal.
    ///  - There are no elements in the channel and the limiter returned an error from its
    ///    `try_add` function.
    Rejected {
        /// The message that did not fit.
        msg: T,
        /// Details about the error
        error: E,
    },

    /// The channel is closed.
    Closed(T),

    /// There are currently no active receivers, only inactive ones.
    Inactive(T),
}

impl<T, E> TrySendError<T, E> {
    /// Unwraps the message that couldn't be sent.
    pub fn into_inner(self) -> T {
        match self {
            TrySendError::Full { msg, .. } => msg,
            TrySendError::Rejected { msg, .. } => msg,
            TrySendError::Closed(t) => t,
            TrySendError::Inactive(t) => t,
        }
    }

    /// Returns `true` if the channel is full but not closed.
    pub fn is_full(&self) -> bool {
        matches!(self, TrySendError::Full { .. } | TrySendError::Rejected { .. })
    }

    /// Returns `true` if retrying the send might succeed later.
    ///
    /// Prefer using [`Sender::broadcast`] if you plan to retry the operation.
    pub fn can_retry(&self) -> bool {
        matches!(self, TrySendError::Full { .. } | TrySendError::Inactive { .. })
    }

    /// Returns `true` if the channel is closed.
    pub fn is_closed(&self) -> bool {
        matches!(self, TrySendError::Closed { .. })
    }

    /// Returns `true` if the channel is inactive.
    pub fn is_disconnected(&self) -> bool {
        matches!(self, TrySendError::Inactive { .. })
    }

    fn map_send_error(self) -> Result<T, SendError<T, E>> {
        match self {
            TrySendError::Full { msg, .. } => Ok(msg),
            TrySendError::Rejected { msg, error } => Err(SendError::Rejected { msg, error }),
            TrySendError::Closed(msg) => Err(SendError::Closed(msg)),
            TrySendError::Inactive(msg) => Ok(msg),
        }
    }
}

impl<T, E: error::Error + 'static> error::Error for TrySendError<T, E> {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match self {
            TrySendError::Full { error, .. } => Some(error),
            TrySendError::Rejected { error, .. } => Some(error),
            _ => None,
        }
    }
}

impl<T, E: fmt::Debug> fmt::Debug for TrySendError<T, E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TrySendError::Full { error, .. } => write!(f, "Full({:?})", error),
            TrySendError::Rejected { error, .. } => write!(f, "Rejected({:?})", error),
            TrySendError::Closed(..) => write!(f, "Closed(..)"),
            TrySendError::Inactive(..) => write!(f, "Inactive(..)"),
        }
    }
}

impl<T, E: fmt::Display> fmt::Display for TrySendError<T, E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TrySendError::Full { error, .. } => write!(f, "channel capacity error: {}", error),
            TrySendError::Rejected { error, .. } => {
                write!(f, "channel rejected message: {}", error)
            }
            TrySendError::Closed(..) => write!(f, "sending into a closed channel"),
            TrySendError::Inactive(..) => write!(f, "sending into the void (no active receivers)"),
        }
    }
}

/// An error returned from [`Receiver::recv()`].
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum RecvError {
    /// The channel has overflowed since the last element was seen.  Future recv operations will
    /// succeed, but some messages have been skipped.
    ///
    /// Contains the number of messages missed.
    Overflowed(u64),

    /// The channel is empty and closed.
    Closed,
}

impl error::Error for RecvError {}

impl fmt::Display for RecvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Overflowed(n) => write!(f, "receiving skipped {} messages", n),
            Self::Closed => write!(f, "receiving from an empty and closed channel"),
        }
    }
}

/// An error returned from [`Receiver::try_recv()`].
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum TryRecvError {
    /// The channel has overflowed since the last element was seen.  Future recv operations will
    /// succeed, but some messages have been skipped.
    Overflowed(u64),

    /// The channel is empty but not closed.
    Empty,

    /// The channel is empty and closed.
    Closed,
}

impl TryRecvError {
    /// Returns `true` if the channel is empty but not closed.
    pub fn is_empty(&self) -> bool {
        match self {
            TryRecvError::Empty => true,
            TryRecvError::Closed => false,
            TryRecvError::Overflowed(_) => false,
        }
    }

    /// Returns `true` if the channel is empty and closed.
    pub fn is_closed(&self) -> bool {
        match self {
            TryRecvError::Empty => false,
            TryRecvError::Closed => true,
            TryRecvError::Overflowed(_) => false,
        }
    }

    /// Returns `true` if this error indicates the receiver missed messages.
    pub fn is_overflowed(&self) -> bool {
        match self {
            TryRecvError::Empty => false,
            TryRecvError::Closed => false,
            TryRecvError::Overflowed(_) => true,
        }
    }
}

impl error::Error for TryRecvError {}

impl fmt::Display for TryRecvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            TryRecvError::Empty => write!(f, "receiving from an empty channel"),
            TryRecvError::Closed => write!(f, "receiving from an empty and closed channel"),
            TryRecvError::Overflowed(n) => {
                write!(f, "receiving operation observed {} lost messages", n)
            }
        }
    }
}

/// A future returned by [`Sender::broadcast()`].
#[derive(Debug)]
#[must_use = "futures do nothing unless .awaited"]
pub struct Send<'a, T, L: CapacityLimiter<T> = ElementCountLimiter> {
    sender: &'a Sender<T, L>,
    listener: Option<EventListener>,
    msg: Option<T>,
}

impl<'a, T, L: CapacityLimiter<T>> Unpin for Send<'a, T, L> {}

impl<'a, T: Clone, L: CapacityLimiter<T>> Future for Send<'a, T, L> {
    type Output = Result<Option<T>, SendError<T, L::Error>>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = Pin::new(self);

        loop {
            let msg = this.msg.take().unwrap();

            // Attempt to send a message.
            match this.sender.try_broadcast(msg) {
                Ok(msg) => {
                    let inner = this.sender.inner.lock().unwrap();

                    if !inner.is_full() {
                        // Notify the next awaiting sender.
                        inner.send_ops.notify(1);
                    }

                    return Poll::Ready(Ok(msg));
                }
                Err(e) => {
                    this.msg = Some(e.map_send_error()?);
                }
            }

            // Sending failed - now start listening for notifications or wait for one.
            match &mut this.listener {
                None => {
                    // Start listening and then try sending again.
                    let inner = this.sender.inner.lock().unwrap();
                    this.listener = Some(inner.send_ops.listen());
                }
                Some(l) => {
                    // Wait for a notification.
                    ready!(Pin::new(l).poll(cx));
                    this.listener = None;
                }
            }
        }
    }
}

/// A future returned by [`Receiver::recv()`].
#[derive(Debug)]
#[must_use = "futures do nothing unless .awaited"]
pub struct Recv<'a, T, L: CapacityLimiter<T> = ElementCountLimiter> {
    receiver: &'a mut Receiver<T, L>,
    listener: Option<EventListener>,
}

impl<'a, T, L: CapacityLimiter<T>> Unpin for Recv<'a, T, L> {}

impl<'a, T: Clone, L: CapacityLimiter<T>> Future for Recv<'a, T, L> {
    type Output = Result<T, RecvError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = Pin::new(self);

        loop {
            // Attempt to receive a message.
            match this.receiver.try_recv() {
                Ok(msg) => return Poll::Ready(Ok(msg)),
                Err(TryRecvError::Closed) => return Poll::Ready(Err(RecvError::Closed)),
                Err(TryRecvError::Overflowed(n)) => {
                    return Poll::Ready(Err(RecvError::Overflowed(n)))
                }
                Err(TryRecvError::Empty) => {}
            }

            // Receiving failed - now start listening for notifications or wait for one.
            match &mut this.listener {
                None => {
                    // Start listening and then try receiving again.
                    this.listener = {
                        let inner = match this.receiver.inner.lock() {
                            Ok(i) => i,
                            Err(_) => return Poll::Ready(Err(RecvError::Closed)),
                        };

                        Some(inner.recv_ops.listen())
                    };
                }
                Some(l) => {
                    // Wait for a notification.
                    ready!(Pin::new(l).poll(cx));
                    this.listener = None;
                }
            }
        }
    }
}

/// An inactive  receiver.
///
/// An inactive receiver is a receiver that is unable to receive messages. It's only useful for
/// keeping a channel open even when no associated active receivers exist.
#[derive(Debug)]
pub struct InactiveReceiver<T, L: CapacityLimiter<T> = ElementCountLimiter> {
    inner: Arc<Mutex<Inner<T, L>>>,
}

impl<T, L: CapacityLimiter<T>> InactiveReceiver<T, L> {
    /// Convert to an activate [`Receiver`].
    ///
    /// Consumes `self`. Use [`InactiveReceiver::activate_cloned`] if you want to keep `self`.
    ///
    /// # Examples
    ///
    /// ```
    /// use async_broadcast::{broadcast, TrySendError};
    ///
    /// let (s, r) = broadcast(1);
    /// let inactive = r.deactivate();
    /// assert_eq!(s.try_broadcast(10), Err(TrySendError::Inactive(10)));
    ///
    /// let mut r = inactive.activate();
    /// assert_eq!(s.try_broadcast(10), Ok(None));
    /// assert_eq!(r.try_recv(), Ok(10));
    /// ```
    pub fn activate(self) -> Receiver<T, L> {
        self.activate_cloned()
    }

    /// Create an activate [`Receiver`] for the associated channel.
    ///
    /// # Examples
    ///
    /// ```
    /// use async_broadcast::{broadcast, TrySendError};
    ///
    /// let (s, r) = broadcast(1);
    /// let inactive = r.deactivate();
    /// assert_eq!(s.try_broadcast(10), Err(TrySendError::Inactive(10)));
    ///
    /// let mut r = inactive.activate_cloned();
    /// assert_eq!(s.try_broadcast(10), Ok(None));
    /// assert_eq!(r.try_recv(), Ok(10));
    /// ```
    pub fn activate_cloned(&self) -> Receiver<T, L> {
        let mut inner = self.inner.lock().unwrap();
        inner.receiver_count += 1;

        if inner.receiver_count == 1 {
            // Notify 1 awaiting senders that there is now a receiver. If there is still room in the
            // queue, the notified operation will notify another awaiting sender.
            inner.send_ops.notify(1);
        }

        Receiver {
            inner: self.inner.clone(),
            pos: inner.head_pos + inner.queue.len() as u64,
            listener: None,
        }
    }

    /// If overflow mode is enabled on this channel.
    ///
    /// See [`Receiver::overflow`] documentation for examples.
    pub fn overflow(&self) -> bool {
        self.inner.lock().unwrap().overflow
    }

    /// Set overflow mode on the channel.
    ///
    /// When overflow mode is set, broadcasting to the channel will succeed even if the channel is
    /// full. It achieves that by removing the oldest message from the channel.
    ///
    /// See [`Receiver::set_overflow`] documentation for examples.
    pub fn set_overflow(&mut self, overflow: bool) {
        self.inner.lock().unwrap().overflow = overflow;
    }

    /// Closes the channel.
    ///
    /// Returns `true` if this call has closed the channel and it was not closed already.
    ///
    /// The remaining messages can still be received.
    ///
    /// See [`Receiver::close`] documentation for examples.
    pub fn close(&self) -> bool {
        self.inner.lock().unwrap().close()
    }

    /// Returns `true` if the channel is closed.
    ///
    /// See [`Receiver::is_closed`] documentation for examples.
    pub fn is_closed(&self) -> bool {
        self.inner.lock().unwrap().is_closed
    }

    /// Returns `true` if the channel is empty.
    ///
    /// See [`Receiver::is_empty`] documentation for examples.
    pub fn is_empty(&self) -> bool {
        self.inner.lock().unwrap().queue.is_empty()
    }

    /// Returns `true` if the channel is full.
    ///
    /// See [`Receiver::is_full`] documentation for examples.
    pub fn is_full(&self) -> bool {
        self.inner.lock().unwrap().is_full()
    }

    /// Returns the number of messages in the channel.
    ///
    /// See [`Receiver::len`] documentation for examples.
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().queue.len()
    }

    /// Returns the number of receivers for the channel.
    ///
    /// This does not include inactive receivers. Use [`InactiveReceiver::inactive_receiver_count`]
    /// if you're interested in that.
    ///
    /// # Examples
    ///
    /// ```
    /// use async_broadcast::broadcast;
    ///
    /// let (s, r) = broadcast::<()>(1);
    /// assert_eq!(s.receiver_count(), 1);
    /// let r = r.deactivate();
    /// assert_eq!(s.receiver_count(), 0);
    ///
    /// let r2 = r.activate_cloned();
    /// assert_eq!(r.receiver_count(), 1);
    /// assert_eq!(r.inactive_receiver_count(), 1);
    /// ```
    pub fn receiver_count(&self) -> usize {
        self.inner.lock().unwrap().receiver_count
    }

    /// Returns the number of inactive receivers for the channel.
    ///
    /// # Examples
    ///
    /// ```
    /// use async_broadcast::broadcast;
    ///
    /// let (s, r) = broadcast::<()>(1);
    /// assert_eq!(s.receiver_count(), 1);
    /// let r = r.deactivate();
    /// assert_eq!(s.receiver_count(), 0);
    ///
    /// let r2 = r.activate_cloned();
    /// assert_eq!(r.receiver_count(), 1);
    /// assert_eq!(r.inactive_receiver_count(), 1);
    /// ```
    pub fn inactive_receiver_count(&self) -> usize {
        self.inner.lock().unwrap().inactive_receiver_count
    }

    /// Returns the number of senders for the channel.
    ///
    /// See [`Receiver::sender_count`] documentation for examples.
    pub fn sender_count(&self) -> usize {
        self.inner.lock().unwrap().sender_count
    }
}

impl<T> InactiveReceiver<T> {
    /// Returns the channel capacity.
    ///
    /// See [`Receiver::capacity`] documentation for examples.
    pub fn capacity(&self) -> usize {
        self.inner.lock().unwrap().capacity.0
    }

    /// Set the channel capacity.
    ///
    /// There are times when you need to change the channel's capacity after creating it. If the
    /// `new_cap` is less than the number of messages in the channel, the oldest messages will be
    /// dropped to shrink the channel.
    ///
    /// See [`Receiver::set_capacity`] documentation for examples.
    pub fn set_capacity(&mut self, new_cap: usize) {
        let _ = self.inner.lock().unwrap().adjust_capacity(new_cap);
    }
}

impl<T, L: CapacityLimiter<T>> Clone for InactiveReceiver<T, L> {
    fn clone(&self) -> Self {
        if let Ok(mut inner) = self.inner.lock() {
            inner.inactive_receiver_count += 1;
        }

        InactiveReceiver {
            inner: self.inner.clone(),
        }
    }
}

impl<T, L: CapacityLimiter<T>> Drop for InactiveReceiver<T, L> {
    fn drop(&mut self) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.inactive_receiver_count -= 1;

            inner.close_channel();
        }
    }
}
