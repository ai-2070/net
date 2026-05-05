//! Caller-side resilience helpers for nRPC calls.
//!
//! These are thin wrappers around the underlying typed call APIs
//! that add operational concerns the raw `call_typed` /
//! `call_service_typed` paths leave to the user:
//!
//! - **`call_with_retry` / `call_typed_with_retry`** — re-issue
//!   transient failures with exponential backoff + jitter.
//! - **`call_with_hedge_to` / `call_service_with_hedge`** — fire
//!   a backup request after a delay; race the responses; first
//!   one wins. Bounds tail latency at the cost of duplicated
//!   work on the loser.
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

// ============================================================================
// Hedge policy.
// ============================================================================

/// Hedge configuration for [`Mesh::call_with_hedge_to`] and
/// friends. **Fire-then-race** semantics: issue the primary
/// request immediately, then after `delay` issue one or more
/// backup requests in parallel and return whichever finishes
/// first.
///
/// **What hedging buys you.** Bounds tail latency. A single slow
/// replica (GC pause, cold cache, hostile NIC) stops dominating
/// the p99 — once `delay` elapses without an answer, a healthy
/// peer gets a chance and the first reply back wins.
///
/// **What it costs you.** The losers' work is wasted: the server
/// runs the handler, publishes a response, and the client
/// silently discards it on arrival. Pick `delay` close to your
/// observed p95 (so most calls don't trigger a hedge) and
/// `hedges` small (1 covers the common slow-replica case;
/// values >1 multiply your server-side load).
///
/// **Cancellation note.** Loser hedges are NOT explicitly
/// cancelled today — when the winner returns, the in-flight
/// loser futures are dropped, but the underlying server-side
/// handlers continue to completion. The reply payloads come
/// back, find no pending entry, and are silently discarded.
/// Bandwidth is paid; correctness is preserved. A future
/// enhancement will wire CANCEL emission into the unary call's
/// drop path; for now, set realistic deadlines so a slow loser
/// short-circuits via `Timeout`.
#[derive(Debug, Clone)]
pub struct HedgePolicy {
    /// Wait this long after the primary call before firing the
    /// first hedge. Subsequent hedges (if `hedges > 1`) fire at
    /// `delay * idx` after the primary. Default: 50ms.
    pub delay: Duration,
    /// Number of hedge requests to fire IN ADDITION to the
    /// primary. `1` is the canonical "primary + one backup"
    /// shape; `0` disables hedging (the wrapper degrades to a
    /// straight call). Default: 1.
    pub hedges: u32,
}

impl Default for HedgePolicy {
    fn default() -> Self {
        Self {
            delay: Duration::from_millis(50),
            hedges: 1,
        }
    }
}

// ============================================================================
// Mesh extensions: hedge.
// ============================================================================

impl Mesh {
    /// Hedge across an explicit set of target node ids. The first
    /// element of `targets` is the primary (fired immediately);
    /// subsequent elements are hedges fired at `policy.delay * idx`.
    /// Whichever call resolves first wins (Ok or Err). Losing
    /// in-flight calls are dropped on the caller side.
    ///
    /// Returns `RpcError::NoRoute` if `targets` is empty. If every
    /// candidate fails, returns the LAST observed error (after
    /// all hedges have been awaited).
    pub async fn call_with_hedge_to(
        &self,
        targets: &[u64],
        service: &str,
        payload: Bytes,
        opts: CallOptions,
        policy: &HedgePolicy,
    ) -> std::result::Result<RpcReply, RpcError> {
        if targets.is_empty() {
            return Err(RpcError::NoRoute {
                target: 0,
                reason: "call_with_hedge_to: targets is empty".into(),
            });
        }
        let total = (1 + policy.hedges as usize).min(targets.len());
        let chosen: Vec<u64> = targets[..total].to_vec();
        hedge_race(self, &chosen, service, payload, opts, policy.delay).await
    }

    /// Hedge across `1 + policy.hedges` candidates picked from the
    /// service registry. Candidates are sorted (so the picks are
    /// deterministic for a stable registry) and the prefix is
    /// taken. If fewer candidates exist than requested, hedges
    /// degrade to whatever's available (no error if `hedges=2` but
    /// only 1 candidate exists — you just get a straight call).
    ///
    /// `opts.routing_policy` is ignored (hedge picks its own
    /// candidates from the service registry).
    /// `opts.filter_unhealthy` is also ignored: hedge's whole
    /// premise is "be robust to per-node slowness" — filtering
    /// unhealthy candidates reduces the redundancy that hedge
    /// buys you. If you want health-aware single-target dispatch,
    /// use `call_service` directly with a routing policy.
    pub async fn call_service_with_hedge(
        &self,
        service: &str,
        payload: Bytes,
        opts: CallOptions,
        policy: &HedgePolicy,
    ) -> std::result::Result<RpcReply, RpcError> {
        let candidates = self.resolve_hedge_candidates(service)?;
        let total = (1 + policy.hedges as usize).min(candidates.len());
        let chosen = &candidates[..total];
        hedge_race(self, chosen, service, payload, opts, policy.delay).await
    }

    /// Typed counterpart of [`Self::call_with_hedge_to`]. Encodes
    /// once, hedges, decodes the winner's reply.
    pub async fn call_typed_with_hedge_to<Req, Resp>(
        &self,
        targets: &[u64],
        service: &str,
        request: &Req,
        opts: CallOptionsTyped,
        policy: &HedgePolicy,
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
        let reply = self
            .call_with_hedge_to(targets, service, Bytes::from(body), opts.raw, policy)
            .await?;
        codec
            .decode(&reply.body)
            .map_err(|e| RpcError::ServerError {
                status: RpcStatus::Internal.to_wire(),
                message: format!("client decode: {e}"),
            })
    }

    /// Typed counterpart of [`Self::call_service_with_hedge`].
    pub async fn call_service_typed_with_hedge<Req, Resp>(
        &self,
        service: &str,
        request: &Req,
        opts: CallOptionsTyped,
        policy: &HedgePolicy,
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
        let reply = self
            .call_service_with_hedge(service, Bytes::from(body), opts.raw, policy)
            .await?;
        codec
            .decode(&reply.body)
            .map_err(|e| RpcError::ServerError {
                status: RpcStatus::Internal.to_wire(),
                message: format!("client decode: {e}"),
            })
    }

    fn resolve_hedge_candidates(
        &self,
        service: &str,
    ) -> std::result::Result<Vec<u64>, RpcError> {
        let mut candidates = self.find_service_nodes(service);
        if candidates.is_empty() {
            return Err(RpcError::NoRoute {
                target: 0,
                reason: format!("no nodes advertise `nrpc:{service}`"),
            });
        }
        // Sort so the prefix taken by the hedge is deterministic
        // for a stable registry. Composes with caller-side
        // observability (the same call always picks the same
        // primary unless the registry has churned).
        candidates.sort_unstable();
        Ok(candidates)
    }
}

// ============================================================================
// Internals: hedge race.
// ============================================================================

async fn hedge_race(
    mesh: &Mesh,
    targets: &[u64],
    service: &str,
    payload: Bytes,
    opts: CallOptions,
    delay: Duration,
) -> std::result::Result<RpcReply, RpcError> {
    use futures::future::FutureExt;

    // Clone the underlying Arc<MeshNode> once; each spawned future
    // owns a clone for its `call(...)` invocation. Cheap (Arc bump).
    let node = mesh.node_arc();
    let service_owned = service.to_string();

    // Build one future per target. The first fires immediately; each
    // subsequent one waits `delay * idx` before invoking. Boxed +
    // pinned so they share a `select_all`-compatible type.
    let mut futures: Vec<futures::future::BoxFuture<'static, std::result::Result<RpcReply, RpcError>>> = targets
        .iter()
        .copied()
        .enumerate()
        .map(|(idx, target)| {
            let node = Arc::clone(&node);
            let service = service_owned.clone();
            let payload = payload.clone();
            let opts = opts.clone();
            let wait = delay.saturating_mul(idx as u32);
            async move {
                if !wait.is_zero() {
                    tokio::time::sleep(wait).await;
                }
                node.call(target, &service, payload, opts).await
            }
            .boxed()
        })
        .collect();

    // Race them. Drop losers as they're left in `remaining`. If the
    // first to resolve is `Ok`, return immediately (drop the rest).
    // If `Err`, keep waiting on the remaining hedges before
    // surfacing — a fast Err shouldn't disqualify slower-but-Ok
    // alternates.
    let mut last_err: Option<RpcError> = None;
    while !futures.is_empty() {
        let (result, _idx, remaining) = futures::future::select_all(futures).await;
        match result {
            Ok(reply) => return Ok(reply),
            Err(e) => {
                last_err = Some(e);
                futures = remaining;
            }
        }
    }
    Err(last_err.unwrap_or_else(|| RpcError::Transport(
        net::error::AdapterError::Connection(
            "hedge_race: drained with no error captured (bug)".into(),
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
