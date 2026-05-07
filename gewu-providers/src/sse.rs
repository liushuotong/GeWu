//! Shared SSE (Server-Sent Events) parsing utilities.
//!
//! This module provides common infrastructure for parsing SSE streams.
//! Each provider implements their own parsing strategy for their specific event format.

use bytes::Bytes;
use futures::Stream;
use std::pin::Pin;
use std::task::{Context, Poll};

/// Shared buffer for SSE parsing.
///
/// This handles the common pattern of:
/// - Buffering bytes from a stream
/// - Extracting lines terminated by `\n`
/// - Handling UTF-8 validation
pub struct SseBuffer<S> {
    stream: Pin<Box<S>>,
    buffer: String,
}

impl<S> SseBuffer<S>
where
    S: Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
{
    /// Creates a new SSE buffer wrapping the given stream.
    pub fn new(stream: S) -> Self {
        Self {
            stream: Box::pin(stream),
            buffer: String::new(),
        }
    }

    /// Returns the next complete line from the buffer or stream.
    ///
    /// Returns `Poll::Ready(Some(Ok(line)))` when a line is available.
    /// Returns `Poll::Ready(None)` when the stream has ended and buffer is empty.
    /// Returns `Poll::Pending` when more data is needed.
    pub fn poll_next_line(&mut self, cx: &mut Context<'_>) -> Poll<Option<Result<String, SseError>>> {
        loop {
            if let Some(pos) = self.buffer.find('\n') {
                let line = self.buffer[..pos].to_string();
                self.buffer = self.buffer[pos + 1..].to_string();
                let line = line.trim();
                if line.is_empty() || line.starts_with(':') {
                    continue;
                }
                    return Poll::Ready(Some(Ok(line.to_string())));
            }

            match self.stream.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(bytes))) => {
                    if let Ok(s) = std::str::from_utf8(&bytes) {
                        self.buffer.push_str(s);
                    }
                }
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Some(Err(SseError::Http(e))));
                }
                Poll::Ready(None) => {
                    if self.buffer.is_empty() {
                        return Poll::Ready(None);
                    }
                    let remaining = self.buffer.clone();
                    self.buffer.clear();
                    if remaining.trim().is_empty() {
                        return Poll::Ready(None);
                    }
                    return Poll::Ready(Some(Ok(remaining)));
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

/// Errors that can occur during SSE parsing.
#[derive(Debug, thiserror::Error)]
pub enum SseError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("SSE parse error: {0}")]
    Parse(String),
}
