# Rust — Errors and Recovery

Failure is a typed outcome, not a silence. The SDK gives you enough structure to
decide, per error, whether to retry, reroute, or give up — and the golden rule is
that **only backpressure is safe to retry blindly.**

## The SDK error surface

Bus and lifecycle calls return `net_sdk::error::SdkError`. Match the variant to
decide:

```rust
use net_sdk::error::SdkError;

match node.emit(&event) {
    Ok(receipt) => { /* accepted into the ring buffer */ }
    Err(SdkError::Backpressure) => { /* the only blindly-retry-safe case */ }
    Err(SdkError::Serialization(_)) | Err(SdkError::Config(_)) => { /* a bug — fix, don't retry */ }
    Err(SdkError::Shutdown) => { /* state change — stop, don't retry */ }
    Err(e) => return Err(e.into()),
}
```

- **`Backpressure`** — the ring buffer/window was full. Retry with backoff, or slow
  the producer. This is the *only* error a blind retry can fix.
- **`Serialization` / `Config`** — a bug in the payload or setup. Retrying reruns
  the bug.
- **`Shutdown` / not-connected** — a state change. Retrying won't undo it.

The full taxonomy — the core `IngestionError` / `ConsumerError` / `AdapterError`
trio, plus subsystem errors (`TokenError`, `ScalingError`, `StreamError`, …) — is
the [Error Codes](/docs/reference/error-codes) reference.

## Recovering an nRPC call

nRPC surfaces typed failures (`RpcError`: no server, timeout, canceled, handler
error, codec). The resilience helpers wrap the raw call:

```rust
use net_sdk::mesh_rpc_resilience::{RetryPolicy, HedgePolicy, CircuitBreaker, CircuitBreakerConfig};

// retry only retryable failures, with bounded attempts + backoff
let resp: Resp = caller
    .call_typed_with_retry(node_id, "svc", &req, opts, &RetryPolicy::default())
    .await?;

// or race a second provider when latency matters more than duplicate work
let resp: Resp = caller
    .call_service_with_hedge("svc", &req, opts, &HedgePolicy::default())
    .await?;

// or fast-fail a sick provider instead of waiting on every deadline
let breaker = CircuitBreaker::new(CircuitBreakerConfig::default());
```

Calling by **service name** (`call_service_typed` / the hedge/retry service
variants) lets the mesh pick a provider, so a substitutable capability fails over
to a standby when the primary dies. The end-to-end patterns are in
[Recover a Failed Workflow](/docs/guides/recover-failed-workflow).

## The one rule

> Retry `Backpressure` (and a transient `Unrouted`, briefly). Treat
> `Serialization` / `Config` as bugs, auth errors as "get a new credential," and
> `Shutdown` / `NotConnected` / `Closed` as state changes retrying won't fix.

## Back to the spine

You've walked the whole loop: [announce](/docs/sdk/rust/announce) →
[discover](/docs/sdk/rust/discover) → [invoke](/docs/sdk/rust/invoke) →
[watch](/docs/sdk/rust/watch) → [artifacts](/docs/sdk/rust/artifacts) → recover.
The same spine runs in every binding — [SDK index](/docs/sdk/rust).
