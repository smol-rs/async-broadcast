use event_listener::{Event, EventListener};
use futures_core::ready;
use std::collections::VecDeque;
use std::convert::TryInto;
use std::fmt::Debug;
use std::future::Future;
use std::ops::{AddAssign, Deref, SubAssign};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use crate::{RecvError, SendError, TryRecvError, TrySendError};

/// An object that produces weights for other objects
pub trait Scale<T: ?Sized> {
    /// The weight of an item.
    type Weight: Clone + AddAssign + SubAssign + Ord + Debug;

    /// Weigh the given item.
    fn weigh(&self, item: &T) -> Self::Weight;
}

/// A [`Scale`] based on simple memory size for objects like [`Box`], [`Vec`], or [`Arc`]: an
/// owning object that derefs to a heap object.
#[derive(Copy, Clone, Debug)]
pub struct BoxlikeScale;

impl<T: ?Sized + Deref> Scale<T> for BoxlikeScale {
    type Weight = usize;
    fn weigh(&self, item: &T) -> usize {
        std::mem::size_of_val(item) + std::mem::size_of_val(&**item)
    }
}

#[derive(Debug)]
struct Inner<T, S: Scale<T>> {
    scale: S,
    queue: VecDeque<(T, usize)>,
    space: S::Weight,

    head_pos: u64,
    sender_count: usize,
    receiver_count: usize,

    overflow: bool,
    closed: bool,

    send_ops: Event,
    recv_ops: Event,
}

/// The sending side of the broadcast channel.
#[derive(Debug)]
pub struct Sender<T, S: Scale<T>> {
    inner: Arc<Mutex<Inner<T, S>>>,
}

/// The receiving side of the broadcast channel.
#[derive(Debug)]
pub struct Receiver<T, S: Scale<T>> {
    inner: Arc<Mutex<Inner<T, S>>>,
    pos: u64,
}

/// Create a new broadcast channel with the given scale and capacity.
///
/// # Example
///
/// ```
/// use async_broadcast::{TrySendError,TryRecvError,weighted::*};
/// let (send, mut recv) = weighted_broadcaster(BoxlikeScale, 200);
///
/// let item = vec![0u8;100];
/// send.try_broadcast(item).unwrap();
///
/// let item = vec![0u8;100];
/// let fail = send.try_broadcast(item);
/// assert!(matches!(fail, Err(TrySendError::Full(_))));
///
/// let smol = vec![0u8;10];
/// send.try_broadcast(smol).unwrap();
///
/// assert_eq!(recv.try_recv().unwrap().len(), 100);
/// assert_eq!(recv.try_recv().unwrap().len(), 10);
/// assert_eq!(recv.try_recv(), Err(TryRecvError::Empty));
/// ```
pub fn weighted_broadcaster<T, S: Scale<T>>(
    scale: S,
    capacity: S::Weight,
) -> (Sender<T, S>, Receiver<T, S>) {
    let inner = Arc::new(Mutex::new(Inner {
        scale,
        queue: VecDeque::new(),
        space: capacity,

        head_pos: 0,
        sender_count: 1,
        receiver_count: 1,

        overflow: false,
        closed: false,

        send_ops: Event::new(),
        recv_ops: Event::new(),
    }));

    (
        Sender {
            inner: inner.clone(),
        },
        Receiver { inner, pos: 0 },
    )
}

impl<T, S: Scale<T>> Inner<T, S> {
    fn new_sender(&mut self, inner: Arc<Mutex<Self>>) -> Sender<T, S> {
        self.sender_count += 1;
        Sender { inner }
    }

    fn new_receiver(&mut self, inner: Arc<Mutex<Self>>) -> Receiver<T, S> {
        self.receiver_count += 1;
        let pos = self.head_pos + self.queue.len() as u64;
        Receiver { inner, pos }
    }

    fn try_send(&mut self, msg: T) -> Result<(), TrySendError<T>> {
        if self.closed {
            return Err(TrySendError::Closed(msg));
        }

        let weight = self.scale.weigh(&msg);
        while weight > self.space {
            if self.overflow {
                if let Some((elt, _)) = self.queue.pop_front() {
                    let weight = self.scale.weigh(&elt);
                    self.head_pos += 1;
                    self.space += weight;
                    continue;
                }
            }
            return Err(TrySendError::Full(msg));
        }

        self.space -= weight;
        let item = (msg, self.receiver_count);
        self.queue.push_back(item);
        self.send_ops.notify(1);
        self.recv_ops.notify(usize::MAX);
        Ok(())
    }

    fn recv_at(&mut self, pos: &mut u64) -> Result<Result<T, &T>, TryRecvError> {
        let i = match pos.checked_sub(self.head_pos) {
            Some(i) => i
                .try_into()
                .expect("Head position more than usize::MAX behind a receiver"),
            None => {
                *pos = self.head_pos;
                return Err(TryRecvError::Overflowed);
            }
        };

        let last_waiter;
        if let Some((_elt, waiters)) = self.queue.get_mut(i) {
            *pos += 1;
            *waiters -= 1;
            last_waiter = *waiters == 0;
        } else {
            debug_assert_eq!(i, self.queue.len());
            if self.closed {
                return Err(TryRecvError::Closed);
            } else {
                return Err(TryRecvError::Empty);
            }
        }

        if last_waiter {
            // Only the first element of the queue should have 0 waiters
            assert_eq!(i, 0);

            // Remove the element from the queue, adjust space, and notify senders
            let elt = self.queue.pop_front().unwrap().0;
            let weight = self.scale.weigh(&elt);
            self.head_pos += 1;
            self.space += weight;
            self.send_ops.notify(1);

            Ok(Ok(elt))
        } else {
            Ok(Err(&self.queue[i].0))
        }
    }

    fn auto_close(&mut self) {
        if !self.closed && self.sender_count == 0 && self.receiver_count == 0 {
            self.closed = true;
            self.send_ops.notify(usize::MAX);
            self.recv_ops.notify(usize::MAX);
        }
    }
}

impl<T, S: Scale<T>> Clone for Sender<T, S> {
    fn clone(&self) -> Self {
        self.new_sender()
    }
}

impl<T, S: Scale<T>> Drop for Sender<T, S> {
    fn drop(&mut self) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.sender_count -= 1;
            inner.auto_close();
        }
    }
}

/// A future returned by [`Sender::broadcast`]
#[derive(Debug)]
pub struct Send<T, S: Scale<T>> {
    inner: Arc<Mutex<Inner<T, S>>>,
    item: Option<T>,
    waiter: Option<EventListener>,
}

impl<T, S: Scale<T>> Unpin for Send<T, S> {}

impl<T, S: Scale<T>> Sender<T, S> {
    /// Return a new sender attached to this channel.
    ///
    /// This is identical to [`Self::clone`]
    pub fn new_sender(&self) -> Sender<T, S> {
        let mut inner = self.inner.lock().unwrap();
        inner.new_sender(self.inner.clone())
    }

    /// Return a new receiver attached to this channel.
    ///
    /// The new receiver will start with no elements pending.
    ///
    /// Note: this will not re-open a channel if it was closed due to all receivers being dropped.
    pub fn new_receiver(&self) -> Receiver<T, S> {
        let mut inner = self.inner.lock().unwrap();
        inner.new_receiver(self.inner.clone())
    }

    /// Broadcasts a message on the channel.
    pub fn broadcast(&self, msg: T) -> Send<T, S> {
        Send {
            inner: self.inner.clone(),
            item: Some(msg),
            waiter: None,
        }
    }

    /// Attempts to broadcast a message on the channel.
    pub fn try_broadcast(&self, msg: T) -> Result<(), TrySendError<T>> {
        let mut inner = self.inner.lock().unwrap();
        inner.try_send(msg)
    }

    /// If overflow mode is enabled on this channel.
    pub fn overflow(&self) -> bool {
        self.inner.lock().unwrap().overflow
    }

    /// Set overflow mode on the channel.
    pub fn set_overflow(&self, overflow: bool) {
        self.inner.lock().unwrap().overflow = overflow;
    }

    /// Closes the channel.
    pub fn close(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.closed = true;
        inner.send_ops.notify(usize::MAX);
        inner.recv_ops.notify(usize::MAX);
    }

    /// Return the current amount of space in the channel.
    pub fn space(&self) -> S::Weight {
        self.inner.lock().unwrap().space.clone()
    }

    /// Add space to the channel.
    pub fn add_space(&self, weight: S::Weight) {
        self.inner.lock().unwrap().space += weight;
    }

    /// Remove space from the channel.
    ///
    /// Returns true if the space was removed or false if there was less space in the channel than
    /// the amount to remove (in which case the space is unchanged).
    pub fn remove_space(&self, weight: S::Weight) -> bool {
        let mut inner = self.inner.lock().unwrap();
        if weight > inner.space {
            return false;
        }
        inner.space -= weight;
        true
    }
}

impl<T, S: Scale<T>> Future for Send<T, S> {
    type Output = Result<(), SendError<T>>;

    fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        let Self {
            item,
            inner,
            waiter,
        } = self.get_mut();
        loop {
            if let Some(future) = waiter {
                ready!(Pin::new(future).poll(ctx));
                *waiter = None;
            }

            let mut inner = inner.lock().unwrap();
            match item.take().map(|msg| inner.try_send(msg)) {
                None => return Poll::Ready(Ok(())),
                Some(Ok(())) => return Poll::Ready(Ok(())),
                Some(Err(TrySendError::Full(msg))) | Some(Err(TrySendError::Inactive(msg))) => {
                    *item = Some(msg);
                    *waiter = Some(inner.send_ops.listen());
                }
                Some(Err(TrySendError::Closed(msg))) => {
                    return Poll::Ready(Err(SendError(msg)));
                }
            }
        }
    }
}

/// A future returned by [`Receiver::recv`]
#[derive(Debug)]
pub struct Recv<'a, T, S: Scale<T>> {
    recv: &'a mut Receiver<T, S>,
    waiter: Option<EventListener>,
}

impl<'a, T, S: Scale<T>> Unpin for Recv<'a, T, S> {}

impl<T, S: Scale<T>> Drop for Receiver<T, S> {
    fn drop(&mut self) {
        if let Ok(mut inner) = self.inner.lock() {
            loop {
                match inner.recv_at(&mut self.pos) {
                    Ok(_) => continue,
                    Err(TryRecvError::Overflowed) => continue,
                    Err(TryRecvError::Closed) => break,
                    Err(TryRecvError::Empty) => break,
                }
            }
            inner.receiver_count -= 1;
            inner.auto_close();
        }
    }
}

impl<T, S: Scale<T>> Receiver<T, S> {
    /// Return a new sender attached to this channel.
    ///
    /// Note: this will not re-open the channel if it was closed due to all senders being dropped.
    pub fn new_sender(&self) -> Sender<T, S> {
        let mut inner = self.inner.lock().unwrap();
        inner.new_sender(self.inner.clone())
    }

    /// Return a new receiver attached to this channel.
    ///
    /// The new receiver will start with no elements pending.
    pub fn new_receiver(&self) -> Receiver<T, S> {
        let mut inner = self.inner.lock().unwrap();
        inner.new_receiver(self.inner.clone())
    }

    /// If overflow mode is enabled on this channel.
    pub fn overflow(&self) -> bool {
        self.inner.lock().unwrap().overflow
    }

    /// Set overflow mode on the channel.
    pub fn set_overflow(&self, overflow: bool) {
        self.inner.lock().unwrap().overflow = overflow;
    }

    /// Closes the channel.
    pub fn close(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.closed = true;
        inner.send_ops.notify(usize::MAX);
        inner.recv_ops.notify(usize::MAX);
    }
}

impl<T: Clone, S: Scale<T>> Receiver<T, S> {
    /// Receives a message from the channel.
    pub fn recv(&mut self) -> Recv<'_, T, S> {
        Recv {
            recv: self,
            waiter: None,
        }
    }

    /// Attempts to receive a message from the channel.
    pub fn try_recv(&mut self) -> Result<T, TryRecvError> {
        let mut inner = self.inner.lock().unwrap();
        inner
            .recv_at(&mut self.pos)
            .map(|r| r.unwrap_or_else(T::clone))
    }
}

impl<'a, T: Clone, S: Scale<T>> Future for Recv<'a, T, S> {
    type Output = Result<T, RecvError>;

    fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        let Self { recv, waiter } = self.get_mut();
        loop {
            if let Some(future) = waiter {
                ready!(Pin::new(future).poll(ctx));
                *waiter = None;
            }

            let mut inner = recv.inner.lock().unwrap();
            return Poll::Ready(match inner.recv_at(&mut recv.pos) {
                Ok(e) => Ok(e.unwrap_or_else(T::clone)),
                Err(TryRecvError::Overflowed) => Err(RecvError::Overflowed),
                Err(TryRecvError::Closed) => Err(RecvError::Closed),
                Err(TryRecvError::Empty) => {
                    *waiter = Some(inner.recv_ops.listen());
                    continue;
                }
            });
        }
    }
}
