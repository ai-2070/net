---
title: Errors
description: "The Rust SDK's error taxonomy: failure is a typed outcome you can branch on, never a silence."
---
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

### Every `SdkError` variant

| Variant | Fires when | Retry? |
|---|---|---|
| `Backpressure` | Ring buffer or stream window full | Yes, with backoff |
| `Unrouted` | Hashed shard isn't in the routing table — usually mid-scaling | Yes — topology settles in ms |
| `Sampled` | A sampling policy dropped the event before a shard saw it | No — expected |
| `Ingestion(_)` | Ingest rejected for another reason | Depends on the inner error |
| `Poll(_)` | Consumption failed | Depends |
| `Adapter(_)` | The backing adapter failed | Depends |
| `Serialization(_)` | Payload wouldn't serialize | No — bug |
| `Config(_)` | Invalid configuration | No — bug |
| `Shutdown` | Bus is shutting down | No — state change |
| `NoMesh` | An operation needed mesh transport and none is configured | No — build error |
| `NotConnected` | Session gone | No — state change |
| `ChannelRejected(reason)` | Subscribe/unsubscribe refused, with the reason | No — usually authorization |
| `Traversal(_)` | Direct-path upgrade failed | No — the routed path still works |

`Unrouted` is deliberately distinct from `Backpressure`: backpressure means the
destination is full, unrouted means there is no destination *right now*. Both
retry, but with different shapes — wait-and-retry versus retry-until-topology-settles.

### Subsystem error types

Beyond `SdkError`, each surface has its own enum. All implement
`std::error::Error`, so `source()` chains work and `?` composes:

| Type | Surface |
|---|---|
| `BreakerError` | Circuit breaker open |
| `DaemonError` / `ClusterError` / `OperatorError` | Compute, cluster and operator control |
| `GroupError` / `JoinError` / `JoinFlowError` | Replica, fork and standby groups |
| `TransferError` | Blob and directory transfer |
| `CapabilityIdError` | Malformed capability identifiers |
| `ToolCallParseError` | Tool descriptors and call payloads |
| `OrgError`, `OrgSdkError`, `OrgCredentialError`, `OrgDiscoveryError`, `OrgProvisionError`, `OrgHandlerError` | Organization capability auth |
| `DeviceEnrollmentError` / `DeviceRegistryError` / `EnrollmentError` | Device enrollment |
| `PinStoreError` / `RevocationStoreError` | MCP pin approvals and revocation |

The core-crate types these wrap — `IngestionError`, `ConsumerError`,
`AdapterError`, `TokenError`, `ScalingError`, `StreamError` — are in the
[Error Codes](/docs/reference/error-codes) reference.

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
