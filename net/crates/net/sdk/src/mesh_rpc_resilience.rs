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
//! - **[`CircuitBreaker`]** — a long-lived stateful guard that
//!   trips after N consecutive failures, short-circuits while
//!   open, and probes for recovery via a half-open state.
//!   Compose around any async call (raw, typed, retried,
//!   hedged) via `breaker.call(|| async { ... }).await`.
//!
//! Each helper composes with the others (and with the underlying
//! `CallOptions` deadline / routing policy) without special
//! plumbing — they're regular async wrappers, not a separate
//! pipeline. Use them when you need them; pay nothing when you
//! don't.

use parking_lot::Mutex;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde::{de::DeserializeOwned, Serialize};

use crate::mesh::Mesh;
use crate::mesh_rpc::{
    CallOptions, CallOptionsTyped, CodecDirection, RpcError, RpcReply, RpcStatus,
};

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
/// `Timeout`). Does NOT retry: `NoRoute`, `Codec` (caller-fixable
/// local bug), or `ServerError` for `Application` / `NotFound` /
/// `Unauthorized` / `UnknownVersion` / `Cancelled` (those are
/// caller-fixable or terminal).
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
        // Codec failures are caller-fixable bugs (wrong codec,
        // schema drift, malformed `Serialize`/`Deserialize` impl).
        // Retrying burns the backoff budget on the same
        // deterministic failure and risks tripping the circuit
        // breaker on a local-only fault.
        RpcError::Codec { .. } => false,
        // v0.4 capability-auth: the target's signed policy denies
        // this caller — retry can't change the verdict until the
        // target publishes a new (more permissive) announcement.
        // Treat as terminal so the retry budget isn't wasted on
        // a deterministic deny.
        RpcError::CapabilityDenied { .. } => false,
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
        let body = codec.encode(request).map_err(|e| RpcError::Codec {
            direction: CodecDirection::Encode,
            message: format!("client encode: {e}"),
        })?;
        let body = Bytes::from(body);
        let reply = self
            .call_with_retry(target_node_id, service, body, opts.raw, policy)
            .await?;
        codec.decode(&reply.body).map_err(|e| RpcError::Codec {
            direction: CodecDirection::Decode,
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
        let body = codec.encode(request).map_err(|e| RpcError::Codec {
            direction: CodecDirection::Encode,
            message: format!("client encode: {e}"),
        })?;
        let body = Bytes::from(body);
        let reply = self
            .call_service_with_retry(service, body, opts.raw, policy)
            .await?;
        codec.decode(&reply.body).map_err(|e| RpcError::Codec {
            direction: CodecDirection::Decode,
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
    Err(last_err.unwrap_or_else(|| {
        RpcError::Transport(net::error::AdapterError::Connection(
            "retry_loop: exhausted with no error captured (bug)".into(),
        ))
    }))
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
/// **Cancellation.** When the winner returns, loser futures are
/// dropped — and the underlying [`Mesh::call`](crate::mesh::Mesh::call)
/// future has a `UnaryCallGuard` whose `Drop` fires a CANCEL to
/// the corresponding server. The server's handler observes the
/// cancellation on its `RpcContext::cancellation` token and can
/// short-circuit cooperatively. CANCEL publish is best-effort
/// (fire-and-forget) so a momentary network blip doesn't leave
/// loser handlers running indefinitely.
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
        let body = codec.encode(request).map_err(|e| RpcError::Codec {
            direction: CodecDirection::Encode,
            message: format!("client encode: {e}"),
        })?;
        let reply = self
            .call_with_hedge_to(targets, service, Bytes::from(body), opts.raw, policy)
            .await?;
        codec.decode(&reply.body).map_err(|e| RpcError::Codec {
            direction: CodecDirection::Decode,
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
        let body = codec.encode(request).map_err(|e| RpcError::Codec {
            direction: CodecDirection::Encode,
            message: format!("client encode: {e}"),
        })?;
        let reply = self
            .call_service_with_hedge(service, Bytes::from(body), opts.raw, policy)
            .await?;
        codec.decode(&reply.body).map_err(|e| RpcError::Codec {
            direction: CodecDirection::Decode,
            message: format!("client decode: {e}"),
        })
    }

    fn resolve_hedge_candidates(&self, service: &str) -> std::result::Result<Vec<u64>, RpcError> {
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

    // Build one future per target. Each yields `(target_idx,
    // result)` so the race below can attribute errors back to the
    // original target position regardless of completion order.
    // The first fires immediately; subsequent ones wait
    // `delay * idx`.
    let mut futures: Vec<
        futures::future::BoxFuture<'static, (usize, std::result::Result<RpcReply, RpcError>)>,
    > = targets
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
                let r = node.call(target, &service, payload, opts).await;
                (idx, r)
            }
            .boxed()
        })
        .collect();

    // Race them. Drop losers as they're left in `remaining`. If the
    // first to resolve is `Ok`, return immediately. If every hedge
    // errors, surface the **primary's** error deterministically —
    // `select_all` resolves in completion order, so a naive "last
    // error wins" semantic flips across runs depending on which
    // hedge happened to lose its race. Tracking errors by their
    // original target index and returning the lowest-indexed one
    // makes the diagnostic stable.
    let mut errors: Vec<Option<RpcError>> = (0..targets.len()).map(|_| None).collect();
    while !futures.is_empty() {
        let ((target_idx, result), _select_idx, remaining) =
            futures::future::select_all(futures).await;
        match result {
            Ok(reply) => return Ok(reply),
            Err(e) => {
                if target_idx < errors.len() {
                    errors[target_idx] = Some(e);
                }
                futures = remaining;
            }
        }
    }
    // Prefer the primary's error (target_idx 0) when present;
    // otherwise the lowest-indexed hedge's error. Either way the
    // choice is deterministic across runs given the same target
    // list.
    let chosen = errors.into_iter().flatten().next();
    Err(chosen.unwrap_or_else(|| {
        RpcError::Transport(net::error::AdapterError::Connection(
            "hedge_race: drained with no error captured (bug)".into(),
        ))
    }))
}

/// `min(max_backoff, initial * multiplier^(attempt-1))`, optionally
/// scaled by a uniform random factor in `[0.5, 1.0]` (full-half
/// jitter — bounded enough to keep p99 predictable, randomized
/// enough to break thundering-herd correlation across callers).
///
/// **Ceiling semantic:** `max_backoff` is a HARD ceiling on the
/// returned duration — applied *after* jitter — so callers who
/// configure e.g. `max_backoff = 1s` never sleep longer than 1s
/// regardless of jitter or exponential growth. The previous
/// implementation applied the cap before jitter, which let an
/// `(initial=1s, max=1s, jitter=true)` policy sleep up to 1s
/// (the doc was honest but the surprise was real).
///
/// **Jitter source:** decorrelated across simultaneously-failing
/// callers by mixing in `Instant::now()` ticks (high-resolution
/// monotonic; not subject to NTP step-back), `thread::current()
/// .id()` (separates threads even when their wall-clocks alias on
/// low-resolution Windows clocks), and the address of a stack
/// local (separates different call sites within one thread). The
/// previous implementation used `SystemTime::now().nanos` only —
/// on Windows the wall-clock has ~15 ms resolution, so two
/// callers in the same tick produced identical jitter and retried
/// in lockstep, defeating the whole point.
fn compute_backoff(policy: &RetryPolicy, attempt: u32) -> Duration {
    let mult = policy.backoff_multiplier.max(1.0);
    // attempt is 1-indexed; the backoff applies AFTER the first
    // failure, so the "exponent" is `attempt - 1`.
    let exp = (attempt.saturating_sub(1)) as i32;
    let scaled = policy.initial_backoff.as_secs_f64() * mult.powi(exp);
    let max_secs = policy.max_backoff.as_secs_f64();
    let pre_cap = scaled.min(max_secs);
    let jittered = if policy.jitter {
        // Mix three independent sources so two callers (a) in the
        // same SystemTime nanosecond bucket on a low-res Windows
        // clock, (b) on different threads, and (c) from different
        // call sites within one thread all decorrelate.
        //
        // Pre-fix this read `Instant::now().elapsed().as_nanos()`
        // — but `.elapsed()` on a freshly-constructed `Instant`
        // measures only the gap between construction and the
        // very next call (a few ns of single-digit jitter), so
        // the `now_ns` term contributed essentially zero entropy
        // and parallel retries from the same call site stayed
        // correlated. Anchor on a process-epoch `Instant` so we
        // measure elapsed from process start instead.
        static PROCESS_EPOCH: OnceLock<Instant> = OnceLock::new();
        let epoch = PROCESS_EPOCH.get_or_init(Instant::now);
        let now_ns = epoch.elapsed().as_nanos() as u64;
        let thread_id_bits = {
            // ThreadId is opaque; hash its Debug print to extract
            // bits cheaply. Stable within the process lifetime.
            let mut s = std::collections::hash_map::DefaultHasher::new();
            std::hash::Hash::hash(&std::thread::current().id(), &mut s);
            std::hash::Hasher::finish(&s)
        };
        let stack_addr = (&attempt as *const u32) as usize as u64;
        let seed = now_ns
            ^ thread_id_bits
            ^ stack_addr.rotate_left(17)
            ^ (attempt as u64).wrapping_mul(0x9E3779B97F4A7C15);
        let mixed = seed
            .wrapping_mul(0x100000001B3)
            .wrapping_add(0xCBF29CE484222325);
        // Top 32 bits → [0, u32::MAX]; map to [0.5, 1.0].
        let frac = ((mixed >> 32) as u32) as f64 / u32::MAX as f64;
        pre_cap * (0.5 + 0.5 * frac)
    } else {
        pre_cap
    };
    // True ceiling: after jitter, clamp again so `max_backoff`
    // is the absolute upper bound.
    let final_secs = jittered.min(max_secs).max(0.0);
    Duration::from_secs_f64(final_secs)
}

// ============================================================================
// Circuit breaker.
// ============================================================================

/// Per-call decision: does this `RpcError` count as a failure for
/// the breaker? Defaults to [`default_breaker_failure`], which
/// treats the same set of "transient infrastructure" errors as
/// [`default_retryable`] — `Timeout`, `Transport`, `Internal`,
/// `Backpressure`, server-observed `Timeout`. Application errors
/// don't trip the breaker (they're caller-fixable bugs, not server
/// health signals).
pub type BreakerFailurePredicate = Arc<dyn Fn(&RpcError) -> bool + Send + Sync>;

/// Configuration for [`CircuitBreaker`].
#[derive(Clone)]
pub struct CircuitBreakerConfig {
    /// Consecutive failures while `Closed` before tripping to
    /// `Open`. Must be >= 1; values below 1 are treated as 1.
    /// Default: 5.
    pub failure_threshold: u32,
    /// Consecutive `HalfOpen` probe successes before transitioning
    /// back to `Closed`. Must be >= 1. Default: 1 (one good probe
    /// is enough).
    pub success_threshold: u32,
    /// `Open` cooldown — how long the breaker rejects every call
    /// before transitioning to `HalfOpen` and allowing one probe.
    /// Default: 30 seconds.
    pub reset_after: Duration,
    /// Predicate deciding which errors count toward the failure
    /// counter. Default: [`default_breaker_failure`].
    pub failure_predicate: BreakerFailurePredicate,
}

impl std::fmt::Debug for CircuitBreakerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CircuitBreakerConfig")
            .field("failure_threshold", &self.failure_threshold)
            .field("success_threshold", &self.success_threshold)
            .field("reset_after", &self.reset_after)
            .field("failure_predicate", &"<fn>")
            .finish()
    }
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            success_threshold: 1,
            reset_after: Duration::from_secs(30),
            failure_predicate: Arc::new(default_breaker_failure),
        }
    }
}

/// Default breaker-failure predicate. Returns `true` for the same
/// "transient infrastructure" errors that [`default_retryable`]
/// considers retryable — these are the signals that a downstream
/// is unhealthy. Application errors and routing failures do NOT
/// trip the breaker.
pub fn default_breaker_failure(err: &RpcError) -> bool {
    default_retryable(err)
}

/// Operational state of a [`CircuitBreaker`]. Exposed via
/// [`CircuitBreaker::state`] for diagnostics / observability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakerState {
    /// Normal operation — calls go through. Failures are counted;
    /// once `failure_threshold` consecutive failures land,
    /// transitions to `Open`.
    Closed,
    /// Tripped — calls are short-circuited with
    /// [`BreakerError::Open`]. After `reset_after` elapsed since
    /// the trip, the next call transitions to `HalfOpen`.
    Open,
    /// Probing — at most ONE call may pass through to test
    /// recovery. Concurrent calls during `HalfOpen` short-circuit
    /// with [`BreakerError::Open`]. The probe's outcome decides:
    /// success → consecutive_successes++ (transition to `Closed`
    /// when threshold met); failure → back to `Open` with
    /// cooldown reset.
    HalfOpen,
}

/// What [`CircuitBreaker::call`] returns on failure: either the
/// breaker rejected the call (`Open`) or the underlying call
/// returned an error (`Inner`). Pattern-match to distinguish
/// "I should fall back" (Open) from "the actual call failed and
/// I should handle the error" (Inner).
#[derive(Debug, thiserror::Error)]
pub enum BreakerError {
    /// Breaker is currently `Open` (or `HalfOpen` with a probe
    /// already in flight). The wrapped call did NOT execute.
    #[error("circuit breaker is open")]
    Open,
    /// The wrapped call executed and returned this error. The
    /// breaker recorded it (per `failure_predicate`) before
    /// surfacing.
    #[error("inner: {0}")]
    Inner(#[from] RpcError),
}

impl BreakerError {
    /// Convert to the underlying `RpcError`, mapping the `Open`
    /// short-circuit to an `RpcError::NoRoute` so callers that
    /// don't care about the breaker distinction can flatten.
    pub fn into_rpc_error(self) -> RpcError {
        match self {
            BreakerError::Open => RpcError::NoRoute {
                target: 0,
                reason: "circuit breaker is open".into(),
            },
            BreakerError::Inner(e) => e,
        }
    }
}

/// Three-state circuit breaker: `Closed` → `Open` → `HalfOpen` →
/// `Closed`. Long-lived; instantiate once per logical downstream
/// (one per service, or one per (service, target) pair, depending
/// on how granular you want failure isolation to be) and reuse
/// across calls.
///
/// **Thread-safety**: the breaker is `Send + Sync`; share via
/// `Arc<CircuitBreaker>` across tasks. State transitions take a
/// brief blocking lock — never held across `await`.
///
/// **Composition**: pass any closure returning a `Future<Output =
/// Result<T, RpcError>>`. The breaker is generic over `T` so it
/// works with raw [`RpcReply`], typed `Resp`, `Vec<RpcReply>`
/// (hedge results), etc.
///
/// ```ignore
/// use std::sync::Arc;
/// use net_sdk::mesh_rpc_resilience::{CircuitBreaker, CircuitBreakerConfig};
///
/// let breaker = Arc::new(CircuitBreaker::new(CircuitBreakerConfig::default()));
/// let result = breaker.call(|| async {
///     mesh.call_typed::<MyReq, MyResp>(target, "svc", &req, opts).await
/// }).await;
/// ```
pub struct CircuitBreaker {
    config: CircuitBreakerConfig,
    inner: Mutex<BreakerInner>,
}

struct BreakerInner {
    state: BreakerState,
    consecutive_failures: u32,
    consecutive_successes: u32,
    /// When `state == Open`, the instant the trip happened;
    /// transitions to `HalfOpen` after `reset_after` elapsed.
    /// `None` outside of `Open`.
    opened_at: Option<std::time::Instant>,
    /// True while a `HalfOpen` probe is in flight. Other calls
    /// arriving during HalfOpen short-circuit on this flag.
    probe_in_flight: bool,
}

impl CircuitBreaker {
    /// Construct a breaker in the `Closed` state.
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            config,
            inner: Mutex::new(BreakerInner {
                state: BreakerState::Closed,
                consecutive_failures: 0,
                consecutive_successes: 0,
                opened_at: None,
                probe_in_flight: false,
            }),
        }
    }

    /// Lock the inner state. parking_lot::Mutex has no poison concept,
    /// so the previous std::sync poison-recovery dance is unnecessary.
    fn lock_inner(&self) -> parking_lot::MutexGuard<'_, BreakerInner> {
        self.inner.lock()
    }

    /// Current operational state. Cheap snapshot — useful for
    /// metrics / logging. Note that `Open` may have actually
    /// elapsed its cooldown; the next `call` will transition to
    /// `HalfOpen` on entry.
    pub fn state(&self) -> BreakerState {
        self.lock_inner().state
    }

    /// Snapshot of the consecutive-failure counter (resets to 0
    /// on success or transition out of `Closed`). Useful for
    /// alerting "we're approaching the trip threshold".
    pub fn consecutive_failures(&self) -> u32 {
        self.lock_inner().consecutive_failures
    }

    /// Test-only / operator override: force the breaker back to
    /// `Closed` and zero all counters. Useful for runbooks
    /// ("we manually verified the downstream is healthy, reset")
    /// or test setup.
    pub fn reset(&self) {
        let mut g = self.lock_inner();
        g.state = BreakerState::Closed;
        g.consecutive_failures = 0;
        g.consecutive_successes = 0;
        g.opened_at = None;
        g.probe_in_flight = false;
    }

    /// Wrap an async call. Returns:
    ///
    /// - `Ok(T)` if the inner call succeeded (and the breaker
    ///   recorded the success).
    /// - `Err(BreakerError::Open)` if the breaker rejected the
    ///   call without running it (state was `Open` within
    ///   cooldown, OR `HalfOpen` with a probe in flight).
    /// - `Err(BreakerError::Inner(e))` if the inner call returned
    ///   an error (recorded per `failure_predicate`).
    ///
    /// Successes always reset `consecutive_failures` to 0 (in
    /// `Closed`) or increment `consecutive_successes` (in
    /// `HalfOpen`, transitioning to `Closed` when threshold met).
    /// Failures matching the predicate increment counters per
    /// state.
    pub async fn call<F, Fut, T>(&self, f: F) -> std::result::Result<T, BreakerError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = std::result::Result<T, RpcError>>,
    {
        // Admission decision — short, no awaits.
        let admitted_as = self.try_admit();
        let admitted_as = match admitted_as {
            AdmissionOutcome::Closed => Admission::Closed,
            AdmissionOutcome::HalfOpenProbe => Admission::HalfOpenProbe,
            AdmissionOutcome::Reject => return Err(BreakerError::Open),
        };

        // RAII probe-in-flight guard. If `f().await` panics, the
        // guard's Drop clears `probe_in_flight=false` AND re-opens
        // the breaker with a fresh cooldown — without this, a
        // panicking probe wedges the breaker in HalfOpen forever
        // (every subsequent call short-circuits with `Open`,
        // unrecoverable except via `reset()`).
        let _probe_guard = ProbeGuard {
            breaker: self,
            admission: admitted_as,
            disarmed: std::cell::Cell::new(false),
        };

        // Run the inner call.
        let outcome = f().await;

        // Outcome bookkeeping — short, no awaits. Disarm the
        // probe guard before mutating state ourselves so we don't
        // double-write `probe_in_flight=false`.
        _probe_guard.disarmed.set(true);
        let mut g = self.lock_inner();
        match (&outcome, admitted_as) {
            (Ok(_), Admission::Closed) => {
                g.consecutive_failures = 0;
            }
            (Ok(_), Admission::HalfOpenProbe) => {
                g.probe_in_flight = false;
                g.consecutive_successes = g.consecutive_successes.saturating_add(1);
                if g.consecutive_successes >= self.config.success_threshold.max(1) {
                    g.state = BreakerState::Closed;
                    g.consecutive_failures = 0;
                    g.consecutive_successes = 0;
                    g.opened_at = None;
                }
            }
            (Err(e), admission) => {
                let counts = (self.config.failure_predicate)(e);
                if matches!(admission, Admission::HalfOpenProbe) {
                    g.probe_in_flight = false;
                }
                if counts {
                    match admission {
                        Admission::Closed => {
                            g.consecutive_failures = g.consecutive_failures.saturating_add(1);
                            if g.consecutive_failures >= self.config.failure_threshold.max(1) {
                                g.state = BreakerState::Open;
                                g.opened_at = Some(std::time::Instant::now());
                                g.consecutive_successes = 0;
                            }
                        }
                        Admission::HalfOpenProbe => {
                            // Single bad probe → re-open with a
                            // fresh cooldown.
                            g.state = BreakerState::Open;
                            g.opened_at = Some(std::time::Instant::now());
                            g.consecutive_failures = 0;
                            g.consecutive_successes = 0;
                        }
                    }
                }
                // If the predicate didn't classify this as a
                // failure (e.g. application error), leave counters
                // unchanged — the breaker treats it as a no-op
                // signal.
            }
        }
        drop(g);

        outcome.map_err(BreakerError::Inner)
    }

    /// Pure admission decision. Returns one of:
    /// - `Closed` — call goes through, count successes/failures.
    /// - `HalfOpenProbe` — this caller becomes the probe.
    /// - `Reject` — short-circuit with `Open`.
    fn try_admit(&self) -> AdmissionOutcome {
        let mut g = self.lock_inner();
        match g.state {
            BreakerState::Closed => AdmissionOutcome::Closed,
            BreakerState::Open => {
                let elapsed = g.opened_at.map(|i| i.elapsed()).unwrap_or(Duration::ZERO);
                if elapsed >= self.config.reset_after {
                    g.state = BreakerState::HalfOpen;
                    g.consecutive_successes = 0;
                    g.probe_in_flight = true;
                    AdmissionOutcome::HalfOpenProbe
                } else {
                    AdmissionOutcome::Reject
                }
            }
            BreakerState::HalfOpen => {
                if g.probe_in_flight {
                    AdmissionOutcome::Reject
                } else {
                    g.probe_in_flight = true;
                    AdmissionOutcome::HalfOpenProbe
                }
            }
        }
    }
}

/// RAII guard that ensures a HalfOpen probe correctly clears its
/// `probe_in_flight` flag and re-opens the breaker if the inner
/// future panics. Disarmed via `disarmed.set(true)` before the
/// normal bookkeeping path so the success / inner-error paths
/// don't double-write the flag.
struct ProbeGuard<'a> {
    breaker: &'a CircuitBreaker,
    admission: Admission,
    disarmed: std::cell::Cell<bool>,
}

impl Drop for ProbeGuard<'_> {
    fn drop(&mut self) {
        if self.disarmed.get() {
            return;
        }
        // The future panicked between admission and bookkeeping.
        // For Closed admissions there's nothing to clean up — we
        // just propagate the panic. For HalfOpenProbe admissions
        // we MUST clear `probe_in_flight=false` (otherwise every
        // subsequent caller short-circuits with `Open` forever)
        // AND re-open with a fresh cooldown (a panicking probe is
        // a failed probe — same semantic as a probe that returned
        // a counted error).
        if matches!(self.admission, Admission::HalfOpenProbe) {
            let mut g = self.breaker.lock_inner();
            g.probe_in_flight = false;
            g.state = BreakerState::Open;
            g.opened_at = Some(std::time::Instant::now());
            g.consecutive_failures = 0;
            g.consecutive_successes = 0;
        }
    }
}

#[derive(Clone, Copy)]
enum AdmissionOutcome {
    Closed,
    HalfOpenProbe,
    Reject,
}

#[derive(Clone, Copy)]
enum Admission {
    Closed,
    HalfOpenProbe,
}
