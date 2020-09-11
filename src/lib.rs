//! Async broadcast channels
//!
//! # Examples
//!
//! ```
//! // tbi
//! ```

#![forbid(unsafe_code, future_incompatible, rust_2018_idioms)]
#![deny(missing_debug_implementations, nonstandard_style)]
#![warn(missing_docs, missing_doc_code_examples, unreachable_pub)]

use std::collections::VecDeque;
use std::error;
use std::fmt;
use std::sync::{Arc, Mutex};

/// Create a new broadcast channel.
pub fn broadcast<T>(cap: usize) -> (Sender<T>, Receiver<T>) {
    let inner = Arc::new(Mutex::new(Inner {
        queue: VecDeque::with_capacity(cap),
        receiver_count: 1,
        send_count: 0,
    }));
    let s = Sender {
        inner: inner.clone(),
        capacity: cap,
    };
    let r = Receiver {
        inner: inner,
        capacity: cap,
        recv_count: 0,
    };
    (s, r)
}

#[derive(Debug)]
struct Inner<T> {
    queue: VecDeque<(T, usize)>,
    receiver_count: usize,
    send_count: usize,
}

// The sending side of a channel.
#[derive(Debug, Clone)]
pub struct Sender<T> {
    inner: Arc<Mutex<Inner<T>>>,
    capacity: usize,
}

impl<T> Sender<T> {
    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

impl<T: Clone> Sender<T> {
    pub async fn broadcast(&self, msg: T) -> Result<(), SendError<T>> {
        todo!();
    }

    pub fn try_broadcast(&self, msg: T) -> Result<(), TrySendError<T>> {
        let mut inner = self.inner.lock().unwrap();
        if inner.queue.len() == self.capacity {
            return Err(TrySendError::Full(msg));
        }
        let receiver_count = inner.receiver_count;
        inner.queue.push_back((msg, receiver_count));
        inner.send_count += 1;
        Ok(())
    }
}

/// The receiving side of a channel.
#[derive(Debug)]
pub struct Receiver<T> {
    inner: Arc<Mutex<Inner<T>>>,
    capacity: usize,
    recv_count: usize,
}

impl<T: Clone> Receiver<T> {
    pub async fn recv(&self) -> Result<T, RecvError> {
        todo!();
    }

    pub fn try_recv(&mut self) -> Result<T, TryRecvError> {
        let mut inner = self.inner.lock().unwrap();
        let msg_count = inner.send_count - self.recv_count;
        if msg_count == 0 {
            return Err(TryRecvError::Empty);
        }
        let len = dbg!(inner.queue.len());
        let msg = inner.queue[len - msg_count].0.clone();
        inner.queue[len - msg_count].1 -= 1;
        if dbg!(inner.queue[len - msg_count].1) == 0 {
            inner.queue.pop_front();
        }
        self.recv_count += 1;
        Ok(msg)
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        let mut inner = self.inner.lock().unwrap();
        let msg_count = dbg!(inner.send_count) - dbg!(self.recv_count);
        let len = inner.queue.len();

        for i in dbg!(len) - dbg!(msg_count)..len {
            inner.queue[i].1 -= 1;
        }
        while let Some((_, 0)) = inner.queue.front() {
            inner.queue.pop_front();
        }
        inner.receiver_count -= 1;
    }
}

impl<T> Clone for Receiver<T> {
    fn clone(&self) -> Self {
        let mut inner = self.inner.lock().unwrap();
        inner.receiver_count += 1;
        Receiver {
            inner: self.inner.clone(),
            capacity: self.capacity,
            recv_count: inner.send_count,
        }
    }
}

/// An error returned from [`Sender::send()`].
///
/// Received because the channel is closed.
#[derive(PartialEq, Eq, Clone, Copy)]
pub struct SendError<T>(pub T);

impl<T> SendError<T> {
    /// Unwraps the message that couldn't be sent.
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> error::Error for SendError<T> {}

impl<T> fmt::Debug for SendError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SendError(..)")
    }
}

impl<T> fmt::Display for SendError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sending into a closed channel")
    }
}

/// An error returned from [`Sender::try_send()`].
#[derive(PartialEq, Eq, Clone, Copy)]
pub enum TrySendError<T> {
    /// The channel is full but not closed.
    Full(T),

    /// The channel is closed.
    Closed(T),
}

impl<T> TrySendError<T> {
    /// Unwraps the message that couldn't be sent.
    pub fn into_inner(self) -> T {
        match self {
            TrySendError::Full(t) => t,
            TrySendError::Closed(t) => t,
        }
    }

    /// Returns `true` if the channel is full but not closed.
    pub fn is_full(&self) -> bool {
        match self {
            TrySendError::Full(_) => true,
            TrySendError::Closed(_) => false,
        }
    }

    /// Returns `true` if the channel is closed.
    pub fn is_closed(&self) -> bool {
        match self {
            TrySendError::Full(_) => false,
            TrySendError::Closed(_) => true,
        }
    }
}

impl<T> error::Error for TrySendError<T> {}

impl<T> fmt::Debug for TrySendError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            TrySendError::Full(..) => write!(f, "Full(..)"),
            TrySendError::Closed(..) => write!(f, "Closed(..)"),
        }
    }
}

impl<T> fmt::Display for TrySendError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            TrySendError::Full(..) => write!(f, "sending into a full channel"),
            TrySendError::Closed(..) => write!(f, "sending into a closed channel"),
        }
    }
}

/// An error returned from [`Receiver::recv()`].
///
/// Received because the channel is empty and closed.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub struct RecvError;

impl error::Error for RecvError {}

impl fmt::Display for RecvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "receiving from an empty and closed channel")
    }
}

/// An error returned from [`Receiver::try_recv()`].
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum TryRecvError {
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
        }
    }

    /// Returns `true` if the channel is empty and closed.
    pub fn is_closed(&self) -> bool {
        match self {
            TryRecvError::Empty => false,
            TryRecvError::Closed => true,
        }
    }
}

impl error::Error for TryRecvError {}

impl fmt::Display for TryRecvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            TryRecvError::Empty => write!(f, "receiving from an empty channel"),
            TryRecvError::Closed => write!(f, "receiving from an empty and closed channel"),
        }
    }
}

#[test]
fn smoke() {
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
