## Handle it — Rust

### The five witnesses

`RpcError` is the type that carries all five. Match on it and each one has its own
variant:

```rust
use net_sdk::mesh_rpc::{RpcError, CodecDirection};

match caller.call_service_typed::<Req, Resp>("summarize", &req, opts).await {
    Ok(resp) => { /* 1 — succeeded */ }

    // WITNESS 1 & 2 — denied, and revocation takes effect here too.
    Err(RpcError::CapabilityDenied { target, capability }) => {
        // The target does not authorize `nrpc:<capability>` for this caller.
        // Get a credential. Retrying re-asks a question already answered.
        eprintln!("denied: {target:#x} does not authorize nrpc:{capability}");
    }

    // WITNESS 3 & 5 — deadline elapsed. Outcome UNKNOWN, not failed.
    Err(RpcError::Timeout { elapsed_ms }) => {
        eprintln!("no answer in {elapsed_ms}ms — the work may or may not have run");
    }

    // WITNESS 4 — a typed remote error crossed the boundary.
    Err(RpcError::ServerError { status, message, headers }) => {
        eprintln!("provider returned {status:#06x}: {message}");
        // `headers` is the structured sidecar — e.g. a `net-failure-schematic`
        // verdict beside the human diagnostic.
    }

    Err(RpcError::NoRoute { target, reason }) => { /* retry briefly */ }
    Err(RpcError::Codec { direction: CodecDirection::Encode, .. }) => { /* your bug */ }
    Err(RpcError::Cancelled) => { /* you did this */ }
    Err(e) => return Err(e.into()),
}
```

`CapabilityDenied` is raised **before the request hits the wire** by the
caller-side gate, and again on receipt of a `CapabilityDenied` status from the
callee's defence-in-depth path. Both arrive as the same variant, which is what
lets you handle authority once.

`Codec` carries a `direction`: `Encode` means it never left, `Decode` means the
reply landed and you could not read it. The second one means the work ran.

### The bus error surface

Bus and lifecycle calls return `net_sdk::error::SdkError`:

```rust
use net_sdk::error::SdkError;

match node.emit(&event) {
    Ok(receipt) => { /* accepted into the ring buffer */ }
    Err(SdkError::Backpressure) => { /* the only blindly-retry-safe case */ }
    Err(SdkError::Unrouted) => { /* topology settling — retry briefly */ }
    Err(SdkError::Serialization(_)) | Err(SdkError::Config(_)) => { /* a bug */ }
    Err(SdkError::Shutdown) => { /* state change — stop */ }
    Err(e) => return Err(e.into()),
}
```

| Variant | Fires when | Retry? |
|---|---|---|
| `Backpressure` | Ring buffer or stream window full | Yes, with backoff |
| `Unrouted` | Hashed shard not in the routing table — usually mid-scaling | Yes — settles in ms |
| `Sampled` | A sampling policy dropped the event | No — expected |
| `Ingestion(_)` | Ingest rejected for another reason | Depends on the inner error |
| `Poll(_)` | Consumption failed | Depends |
| `Adapter(_)` | The backing adapter failed | Depends |
| `Serialization(_)` | Payload would not serialize | No — bug |
| `Config(_)` | Invalid configuration | No — bug |
| `Shutdown` | Bus is shutting down | No — state change |
| `NoMesh` | Operation needed mesh transport and none is configured | No — build error |
| `NotConnected` | Session gone | No — state change |
| `ChannelRejected(reason)` | Subscribe/unsubscribe refused, with the reason | No — usually authorization |
| `Traversal(_)` | Direct-path upgrade failed | No — the routed path still works |

`Unrouted` is deliberately distinct from `Backpressure`. Backpressure means the
destination is full; unrouted means there is no destination *right now*. Both
retry, with different shapes.

### Subsystem error types

Each surface has its own enum, all implementing `std::error::Error`, so `source()`
chains and `?` composes:

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

### Recover a call

```rust
use net_sdk::mesh_rpc_resilience::{RetryPolicy, HedgePolicy, CircuitBreaker, CircuitBreakerConfig};

let resp: Resp = caller
    .call_typed_with_retry(node_id, "svc", &req, opts, &RetryPolicy::default())
    .await?;

let resp: Resp = caller
    .call_service_with_hedge("svc", &req, opts, &HedgePolicy::default())
    .await?;

let breaker = CircuitBreaker::new(CircuitBreakerConfig::default());
```

The default retry predicate deliberately skips `Codec`, `CapabilityDenied` and
`Cancelled` — none of them get better on a second attempt.

Next: back to [the SDK index](/docs/sdk/rust).
