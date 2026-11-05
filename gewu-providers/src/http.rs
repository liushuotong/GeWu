//! Shared HTTP transport hardening for LLM API access.
//!
//! LLM API calls occasionally stall: a request is sent over the wire but no
//! response (not even an error) ever comes back. Without timeouts, such a call
//! hangs forever — the ask never terminates, the agent lock is held, and the
//! GUI tab stays busy with no output. This module bounds every phase of the
//! request and adds a small retry so transient stalls self-heal.

use reqwest::Client;
use std::future::Future;
use std::time::Duration;

/// Connect timeout for the initial TCP/TLS handshake.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum time to wait between bytes once the response has started.
///
/// This is an *idle* timeout, not a total timeout: a healthy stream that keeps
/// producing tokens (or SSE keep-alive comments) never trips it, but a stalled
/// connection is bounded instead of hanging forever.
const READ_TIMEOUT: Duration = Duration::from_secs(120);

/// Base delay between retry attempts; each retry waits four times longer than
/// the previous one (1s, 4s, ...).
pub(crate) const RETRY_BACKOFF: Duration = Duration::from_secs(1);

/// Total attempts per LLM request: the initial try plus two retries.
const MAX_ATTEMPTS: u32 = 3;

/// Builds an HTTP client with timeouts suited for LLM API streaming.
///
/// Deliberately no total `timeout()`: streaming completions can legitimately
/// run for many minutes, so only the connect phase and inter-read gaps are
/// bounded.
pub(crate) fn client() -> Client {
    Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .build()
        .expect("static HTTP client configuration should never fail")
}

/// Runs `op`, retrying up to [`MAX_ATTEMPTS`] times when it fails with a
/// retryable (transport-level) error.
///
/// Only errors for which `is_retryable` returns true are retried; API-level
/// errors (bad request, auth, rate limit) fail immediately. Retrying is safe
/// here because the operation is only attempted before any response bytes
/// have been consumed, so no partial output can be duplicated.
pub(crate) async fn with_retries<T, E, Fut, F>(
    backoff: Duration,
    mut op: F,
    is_retryable: fn(&E) -> bool,
) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        match op().await {
            Ok(value) => return Ok(value),
            Err(e) if attempt < MAX_ATTEMPTS && is_retryable(&e) => {
                let delay = backoff * 4u32.pow(attempt - 1);
                tracing::warn!(
                    attempt,
                    delay_secs = delay.as_secs(),
                    error = %e,
                    "transient LLM request failure; retrying"
                );
                tokio::time::sleep(delay).await;
            }
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Backoff small enough that retry sleeps don't slow the test suite.
    const TEST_BACKOFF: Duration = Duration::from_millis(1);

    /// Counts calls and fails with a "transient" error for the first
    /// `failures` attempts.
    fn counting_op(
        calls: Arc<AtomicUsize>,
        failures: usize,
    ) -> impl FnMut() -> std::pin::Pin<Box<dyn Future<Output = Result<u32, String>> + Send>> {
        move || {
            let calls = calls.clone();
            Box::pin(async move {
                let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
                if n <= failures {
                    Err(format!("transient {n}"))
                } else {
                    Ok(n as u32)
                }
            })
        }
    }

    #[tokio::test]
    async fn retries_transient_failures_then_succeeds() {
        let calls = Arc::new(AtomicUsize::new(0));
        let result: Result<u32, String> =
            with_retries(TEST_BACKOFF, counting_op(calls.clone(), 2), |e| {
                e.starts_with("transient")
            })
            .await;
        assert_eq!(result.unwrap(), 3);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn does_not_retry_permanent_failures() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let result: Result<u32, String> = with_retries(
            TEST_BACKOFF,
            || {
                let c = c.clone();
                Box::pin(async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Err::<u32, _>("fatal: bad request".to_string())
                })
                    as std::pin::Pin<Box<dyn Future<Output = Result<u32, String>> + Send>>
            },
            |e| e.starts_with("transient"),
        )
        .await;
        assert_eq!(result.unwrap_err(), "fatal: bad request");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn gives_up_after_max_attempts() {
        let calls = Arc::new(AtomicUsize::new(0));
        let result: Result<u32, String> =
            with_retries(TEST_BACKOFF, counting_op(calls.clone(), usize::MAX), |e| {
                e.starts_with("transient")
            })
            .await;
        assert!(result.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), MAX_ATTEMPTS as usize);
    }
}
