//! Steering: user messages sent to the agent while it is running.
//!
//! The ask stream borrows the agent exclusively, so callers clone a
//! [`Steering`] handle out of the agent before starting a run and send
//! messages through it from anywhere (another task, an input loop).
//! The agent loop drains the buffer at safe points — after the current
//! round's tool results are recorded, before the next LLM request — and
//! appends them to the conversation as ordinary user messages.
//!
//! Injection never lands between an assistant turn and its tool results
//! (strict providers reject that history shape), mirroring opencode's
//! steering behavior: the in-flight provider stream is not aborted; the
//! agent adapts at the next round boundary.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// Handle for queuing user messages into a running agent conversation.
///
/// Cloneable; every clone feeds the same agent. Sending never blocks and
/// succeeds even while the agent is idle — messages queued between runs
/// are delivered at the start of the next `ask`.
#[derive(Clone)]
pub struct Steering {
    inner: Arc<Inner>,
}

struct Inner {
    tx: mpsc::UnboundedSender<String>,
    rx: Mutex<mpsc::UnboundedReceiver<String>>,
    queued: AtomicUsize,
}

impl std::fmt::Debug for Steering {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Steering")
            .field("pending", &self.is_pending())
            .finish()
    }
}

impl Default for Steering {
    fn default() -> Self {
        Self::new()
    }
}

impl Steering {
    /// Creates an empty steering buffer.
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            inner: Arc::new(Inner {
                tx,
                rx: Mutex::new(rx),
                queued: AtomicUsize::new(0),
            }),
        }
    }

    /// Queues a message for injection into the conversation.
    ///
    /// The send only fails when the owning agent has been dropped.
    pub fn send(&self, message: impl Into<String>) -> Result<(), mpsc::error::SendError<String>> {
        let message = message.into();
        self.inner.tx.send(message).inspect(|_| {
            self.inner.queued.fetch_add(1, Ordering::Relaxed);
        })
    }

    /// Takes all queued messages, oldest first.
    pub fn drain(&self) -> Vec<String> {
        let mut rx = self.inner.rx.lock().unwrap();
        let mut messages = Vec::new();
        while let Ok(message) = rx.try_recv() {
            messages.push(message);
        }
        self.inner
            .queued
            .fetch_sub(messages.len(), Ordering::Relaxed);
        messages
    }

    /// Whether at least one message is queued.
    pub fn is_pending(&self) -> bool {
        self.inner.queued.load(Ordering::Relaxed) > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_drain_and_is_pending() {
        let steering = Steering::new();
        assert!(!steering.is_pending());
        steering.send("first").unwrap();
        steering.send("second").unwrap();
        assert!(steering.is_pending());
        assert_eq!(steering.drain(), vec!["first", "second"]);
        assert!(!steering.is_pending());
        assert!(steering.drain().is_empty());
    }

    #[test]
    fn clones_share_one_buffer() {
        let steering = Steering::new();
        steering.clone().send("via clone").unwrap();
        assert_eq!(steering.drain(), vec!["via clone"]);
    }

    #[test]
    fn debug_shows_pending_state() {
        let steering = Steering::new();
        assert!(format!("{steering:?}").contains("pending: false"));
        steering.send("x").unwrap();
        assert!(format!("{steering:?}").contains("pending: true"));
    }
}
