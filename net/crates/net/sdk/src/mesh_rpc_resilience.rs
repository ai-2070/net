//! Caller-side resilience helpers for nRPC calls.
//!
//! These are thin wrappers around the underlying typed call APIs
//! that add operational concerns the raw `call_typed` /
//! `call_service_typed` paths leave to the user:
//!
//! - **`call_with_retry` / `call_typed_with_retry`** — re-issue
//!   transient failures with exponential backoff + jitter.
//!
//! Each helper composes with the others (and with the underlying
//! `CallOptions` deadline / routing policy) without special
//! plumbing — they're regular async wrappers, not a separate
//! pipeline. Use them when you need them; pay nothing when you
//! don't.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde::{de::DeserializeOwned, Serialize};

use crate::mesh::Mesh;
use crate::mesh_rpc::{CallOptions, CallOptionsTyped, RpcError, RpcReply, RpcStatus};

// ============================================================================
// Retry policy.
// ============================================================================

/// What counts as "this should be retried" for a given [`RpcError`].
/// Defaults to [`default_retryable`] which retries transient
/// infrastructure failures (timeout, server-side internal /
/// backpressure, transport) and skips terminal ones (no route,
/// application errors, unknown-version).
pub type RetryablePredicate = Arc<dyn Fn(&RpcError) -> bool + Send + Sync>;

/// Backoff + retry policy for [`Mesh::call_with_retry`] and friends.
/// Defaults: 3 attempts total, 50ms initial backoff, doubling per
/// attempt, capped at 1s, full jitter on. Override the predicate
/// via [`Self::with_retryable`] to retry application errors,
/// non-transient failures, etc.
#[derive(Clone)]
pub struct RetryPolicy {
    /// Total number of attempts (NOT additional retries). 1 means
    /// "no retry"; 3 means "original + up to 2 retries". Must be
    /// >= 1; values below 1 are treated as 1.
    pub max_attempts: u32,
    /// Backoff before the first retry. Subsequent backoffs scale
    /// by `backoff_multiplier`, capped by `max_backoff`.
    pub initial_backoff: Duration,
    /// Upper bound on the per-attempt backoff (before jitter).
    /// Stops exponential growth from blowing past the underlying
    /// call's deadline budget.
    pub max_backoff: Duration,
    /// Multiplicative growth factor between attempts. `2.0` is the
    /// canonical "exponential backoff" default; values < 1.0 are
    /// clamped to 1.0 (so backoff never shrinks).
    pub backoff_multiplier: f64,
    /// When `true` (the default), each backoff is multiplied by a
    /// uniform random factor in `[0.5, 1.0]` to decorrelate retry
    /// storms across callers. When `false`, backoffs are
    /// deterministic.
    pub jitter: bool,
    /// Decides whether a given error is retryable. Default:
    /// [`default_retryable`].
    pub retryable: RetryablePredicate,
}

impl std::fmt::Debug for RetryPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RetryPolicy")
            .field("max_attempts", &self.max_attempts)
            .field("initial_backoff", &self.initial_backoff)
            .field("max_backoff", &self.max_backoff)
            .field("backoff_multiplier", &self.backoff_multiplier)
            .field("jitter", &self.jitter)
            .field("retryable", &"<fn>")
            .finish()
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_backoff: Duration::from_millis(50),
            max_backoff: Duration::from_secs(1),
            backoff_multiplier: 2.0,
            jitter: true,
            retryable: Arc::new(default_retryable),
        }
    }
}

impl RetryPolicy {
    /// Replace the retryable predicate. Use this to extend (or
    /// narrow) what's considered worth retrying — e.g. retry only
    /// `Timeout`, or also retry a specific application error code.
    pub fn with_retryable<F: Fn(&RpcError) -> bool + Send + Sync + 'static>(
        mut self,
        predicate: F,
    ) -> Self {
        self.retryable = Arc::new(predicate);
        self
    }
}

/// Default predicate for [`RetryPolicy::retryable`]. Retries:
/// `Timeout`, `Transport`, and `ServerError` for the canonical
/// transient statuses (`Internal`, `Backpressure`, server-observed
/// `Timeout`). Does NOT retry: `NoRoute`, `ServerError` for
/// `Application` / `NotFound` / `Unauthorized` / `UnknownVersion` /
/// `Cancelled` (those are caller-fixable or terminal).
pub fn default_retryable(err: &RpcError) -> bool {
    match err {
        RpcError::NoRoute { .. } => false,
        RpcError::Timeout { .. } => true,
        RpcError::Transport(_) => true,
        RpcError::ServerError { status, .. } => {
            *status == RpcStatus::Internal.to_wire()
                || *status == RpcStatus::Backpressure.to_wire()
                || *status == RpcStatus::Timeout.to_wire()
        }
    }
}

// ============================================================================
// Mesh extensions.
// ============================================================================

impl Mesh {
    /// Direct-addressed raw call with retry. Re-issues on transient
    /// failures per `policy`; the last error from the final attempt
    /// is returned on exhaustion. The underlying [`CallOptions`] is
    /// re-used for every attempt — note that `opts.deadline` is an
    /// absolute `Instant` and does NOT advance across retries, so
    /// the total wall-clock window is bounded by the initial
    /// deadline plus the sum of backoffs.
    pub async fn call_with_retry(
        &self,
        target_node_id: u64,
        service: &str,
        payload: Bytes,
        opts: CallOptions,
        policy: &RetryPolicy,
    ) -> std::result::Result<RpcReply, RpcError> {
        retry_loop(policy, |attempt| {
            let payload = payload.clone();
            let opts = opts.clone();
            let _ = attempt;
            async move { self.call(target_node_id, service, payload, opts).await }
        })
        .await
    }

    /// Service-name raw call with retry. Each attempt re-runs the
    /// capability-index lookup + routing-policy selection — useful
    /// when a server failover happens mid-retry-window: the next
    /// attempt naturally lands on a different node. To pin a single
    /// target across retries, use [`Self::call_with_retry`].
    pub async fn call_service_with_retry(
        &self,
        service: &str,
        payload: Bytes,
        opts: CallOptions,
        policy: &RetryPolicy,
    ) -> std::result::Result<RpcReply, RpcError> {
        retry_loop(policy, |attempt| {
            let payload = payload.clone();
            let opts = opts.clone();
            let _ = attempt;
            async move { self.call_service(service, payload, opts).await }
        })
        .await
    }

    /// Direct-addressed typed call with retry. Encodes once
    /// (the request bytes are reused across attempts), retries
    /// per `policy`, decodes the final reply.
    pub async fn call_typed_with_retry<Req, Resp>(
        &self,
        target_node_id: u64,
        service: &str,
        request: &Req,
        opts: CallOptionsTyped,
        policy: &RetryPolicy,
    ) -> std::result::Result<Resp, RpcError>
    where
        Req: Serialize,
        Resp: DeserializeOwned,
    {
        let codec = opts.codec;
        let body = codec.encode(request).map_err(|e| RpcError::ServerError {
            status: RpcStatus::Internal.to_wire(),
            message: format!("client encode: {e}"),
        })?;
        let body = Bytes::from(body);
        let reply = self
            .call_with_retry(target_node_id, service, body, opts.raw, policy)
            .await?;
        codec
            .decode(&reply.body)
            .map_err(|e| RpcError::ServerError {
                status: RpcStatus::Internal.to_wire(),
                message: format!("client decode: {e}"),
            })
    }

    /// Service-name typed call with retry. Same caveat as
    /// [`Self::call_service_with_retry`] — each attempt re-resolves
    /// the candidate set, so failover is automatic.
    pub async fn call_service_typed_with_retry<Req, Resp>(
        &self,
        service: &str,
        request: &Req,
        opts: CallOptionsTyped,
        policy: &RetryPolicy,
    ) -> std::result::Result<Resp, RpcError>
    where
        Req: Serialize,
        Resp: DeserializeOwned,
    {
        let codec = opts.codec;
        let body = codec.encode(request).map_err(|e| RpcError::ServerError {
            status: RpcStatus::Internal.to_wire(),
            message: format!("client encode: {e}"),
        })?;
        let body = Bytes::from(body);
        let reply = self
            .call_service_with_retry(service, body, opts.raw, policy)
            .await?;
        codec
            .decode(&reply.body)
            .map_err(|e| RpcError::ServerError {
                status: RpcStatus::Internal.to_wire(),
                message: format!("client decode: {e}"),
            })
    }
}

// ============================================================================
// Internals: retry loop + backoff.
// ============================================================================

/// Run `attempt_fn` up to `policy.max_attempts` times, sleeping
/// per-attempt backoff between failed retryable attempts. Returns
/// the first `Ok`, or the last `Err` on exhaustion / the first
/// non-retryable `Err` immediately.
async fn retry_loop<T, F, Fut>(
    policy: &RetryPolicy,
    mut attempt_fn: F,
) -> std::result::Result<T, RpcError>
where
    F: FnMut(u32) -> Fut,
    Fut: std::future::Future<Output = std::result::Result<T, RpcError>>,
{
    let max = policy.max_attempts.max(1);
    let mut last_err: Option<RpcError> = None;
    for attempt in 1..=max {
        match attempt_fn(attempt).await {
            Ok(value) => return Ok(value),
            Err(e) => {
                let retryable = (policy.retryable)(&e);
                let is_last = attempt == max;
                if !retryable || is_last {
                    return Err(e);
                }
                let backoff = compute_backoff(policy, attempt);
                last_err = Some(e);
                if !backoff.is_zero() {
                    tokio::time::sleep(backoff).await;
                }
            }
        }
    }
    // Loop body always returns on the final iteration; the
    // unreachable here is a safety net for a future refactor that
    // changes the bounds.
    Err(last_err.unwrap_or_else(|| RpcError::Transport(
        net::error::AdapterError::Connection(
            "retry_loop: exhausted with no error captured (bug)".into(),
        ),
    )))
}

/// `min(max_backoff, initial * multiplier^(attempt-1))`, optionally
/// scaled by a uniform random factor in `[0.5, 1.0]` (full-half
/// jitter — bounded enough to keep p99 predictable, randomized
/// enough to break thundering-herd correlation across callers).
fn compute_backoff(policy: &RetryPolicy, attempt: u32) -> Duration {
    let mult = policy.backoff_multiplier.max(1.0);
    // attempt is 1-indexed; the backoff applies AFTER the first
    // failure, so the "exponent" is `attempt - 1`.
    let exp = (attempt.saturating_sub(1)) as i32;
    let scaled = policy.initial_backoff.as_secs_f64() * mult.powi(exp);
    let capped = scaled.min(policy.max_backoff.as_secs_f64());
    let final_secs = if policy.jitter {
        // Cheap 32-bit-style PRNG seeded from wall-clock nanos +
        // attempt, mapped to a [0.5, 1.0] factor. Quality is
        // adequate for jitter (the goal is decorrelation, not
        // unpredictability); the alternative would be pulling in
        // the `rand` crate just for one uniform sample.
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0)
            ^ (attempt as u64).wrapping_mul(0x9E3779B97F4A7C15);
        let mixed = seed
            .wrapping_mul(0x100000001B3)
            .wrapping_add(0xCBF29CE484222325);
        // Top 32 bits → [0, u32::MAX]; map to [0.5, 1.0].
        let frac = ((mixed >> 32) as u32) as f64 / u32::MAX as f64;
        capped * (0.5 + 0.5 * frac)
    } else {
        capped
    };
    Duration::from_secs_f64(final_secs.max(0.0))
}
