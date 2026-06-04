# Error Codes

This page enumerates every error type the core crate surfaces. Errors are organized by the operation they come from — ingestion, consumption, and adapter — and each variant includes the conditions under which it fires and the right response from a caller.

The crate uses `thiserror` throughout, so every variant has a Display impl and (where it wraps another error) a working `source()` chain. Pattern match on the variant when you need to make a decision; format the Display when you need to log.

## `IngestionError`

Returned from `EventBus::ingest()` and `EventBus::ingest_raw()`.

| Variant                  | Display                              | When it fires                                                                 | What to do                                                              |
|--------------------------|--------------------------------------|-------------------------------------------------------------------------------|-------------------------------------------------------------------------|
| `Backpressure`           | "backpressure: ring buffer full"     | Shard's ring buffer full + backpressure policy rejected the event             | Apply your retry policy; the bus will not surface this if `Block` mode  |
| `Sampled`                | "event dropped due to sampling"      | Sampling/decimation policy dropped the event before it reached a shard         | Expected under sampling; no caller action needed                        |
| `Unrouted`               | "event has no routable shard"        | Hashed shard id is not in the routing table (e.g. mid-scaling)                | Back off briefly and retry — topology stabilizes within milliseconds    |
| `ShuttingDown`           | "event bus is shutting down"         | Bus is in shutdown; new ingests rejected                                       | Stop ingesting; flush downstream state and exit                         |
| `Serialization(_)`       | "serialization error: ..."           | Event payload couldn't be serialized                                            | Bug — investigate the payload; the error's source chain points at the underlying `serde_json::Error` |

`Unrouted` is distinct from `Backpressure` so callers can apply the right remediation. Backpressure says "the destination is full"; unrouted says "there's no destination right now." Pre-fix versions of the bus collapsed these into one variant, and callers applied back-off-and-retry to unrouted errors that wouldn't be fixed by waiting — they needed to retry until the topology settled, which is a different shape of retry.

## `ConsumerError`

Returned from `EventBus::poll()`.

| Variant                  | Display                              | When it fires                                                                 | What to do                                                              |
|--------------------------|--------------------------------------|-------------------------------------------------------------------------------|-------------------------------------------------------------------------|
| `Adapter(_)`             | "adapter error: ..."                 | Underlying adapter failed; the wrapped error is the adapter's                  | See `AdapterError` below; `is_retryable()` says whether to retry        |
| `InvalidCursor(_)`       | "invalid cursor: ..."                | Cursor in the request couldn't be decoded                                      | Don't pass that cursor again; start from current tail with no cursor    |
| `InvalidFilter(_)`       | "invalid filter: ..."                | Filter in the request couldn't be parsed or evaluated                          | Bug — investigate the filter; the message includes a parse position    |

A `ConsumerError::Adapter` wraps an `AdapterError`, so the full classification surface is available through the wrapped error. Use `From<AdapterError>` to convert, or pattern match on the wrapper.

## `AdapterError`

Returned from adapter operations (`Adapter::on_batch`, `Adapter::poll_shard`, `Adapter::flush`, `Adapter::shutdown`). Also wrapped in `ConsumerError`.

| Variant                  | Display                              | When it fires                                                                 | Classification                                                          |
|--------------------------|--------------------------------------|-------------------------------------------------------------------------------|-------------------------------------------------------------------------|
| `Transient(_)`           | "transient error: ..."               | Retryable failure (timeout, transient network issue)                          | `is_retryable() == true`                                                |
| `Fatal(_)`               | "fatal error: ..."                   | Unrecoverable state                                                            | `is_fatal() == true`                                                    |
| `Backpressure`           | "backend backpressure"               | Backend rejected for capacity reasons (Redis MAXLEN, JetStream MaxBytes, etc.) | `is_retryable() == true`                                                |
| `Connection(_)`          | "connection error: ..."              | Connection-level failure (refused, broken, reset)                              | Not retryable by default — covers both transient ("send failed") and permanent ("not initialized") cases without distinguishing |
| `Shutdown`               | "adapter is shut down"               | Adapter was asked to stop and is no longer accepting work                      | `is_shutdown() == true`; distinct from `Connection` so callers can tell "we asked it to stop" from "transport failure" |
| `Serialization(_)`       | "serialization error: ..."           | Adapter couldn't serialize/deserialize event data                              | Not retryable; bug in payload or adapter codec                          |

### Classification methods

```rust
impl AdapterError {
    pub fn is_retryable(&self) -> bool;
    pub fn is_fatal(&self) -> bool;
    pub fn is_shutdown(&self) -> bool;
}
```

The bus's dispatch loop reads these to decide what to do with a failed batch:

- **Retryable.** The batch is requeued with an exponential backoff up to a bounded number of attempts.
- **Fatal.** The batch is dropped, the bus's stats record the drop, and the error is logged at error level.
- **Shutdown.** The batch is dropped and ingestion is halted; the bus's shutdown is presumed to be in flight.
- **Connection (default).** Conservatively non-retryable. The bus skips the retry loop and drops the batch immediately. This avoids burning the retry budget on a backend that's gone for good.

The default decision for `Connection` errors is conservative on purpose. If you know your backend's connection errors are transient and you want them retried, return `AdapterError::Transient(...)` from your adapter instead.

## Subsystem-specific errors

Beyond the core trio, individual subsystems define their own error types. The ones most likely to surface in application code:

### `ScalingError`

Returned from `EventBus::add_shards()` and `EventBus::remove_shards()`.

| Variant                  | When it fires                                                                            |
|--------------------------|------------------------------------------------------------------------------------------|
| `AlreadyAtLimit`         | The bus is at its configured shard ceiling and can't add more                            |
| `ShardInUse`             | A shard requested for removal still has in-flight work                                   |
| `NoSuchShard`            | A shard id passed for removal doesn't exist                                              |
| `Internal(_)`            | Internal invariant violation; investigate                                                |

### `ConfigError`

Returned from `EventBusConfigBuilder::build()`.

| Variant                  | When it fires                                                                            |
|--------------------------|------------------------------------------------------------------------------------------|
| `InvalidShardCount`      | `shards` outside the supported range                                                     |
| `InvalidBatchConfig`     | Batch sizing values inconsistent (max_events == 0, etc.)                                 |
| `IncompatibleFeatures`   | A feature was requested that isn't compiled in (e.g. `redis` adapter without the feature) |

### Adapter-specific errors

Each shipped adapter has its own error type with backend-specific variants. The most useful ones from each:

- **`NetAdapter`** — `NetAdapterError::SessionFailed`, `NetAdapterError::RoutingFailed`, `NetAdapterError::AuthRejected`.
- **`RedisAdapter`** — wraps `redis::RedisError` with classification ("retryable" for read timeouts and replica failovers, "fatal" for auth failures).
- **`JetStreamAdapter`** — wraps `async-nats::Error` with similar classification.

These are surfaced through `AdapterError::Connection`, `AdapterError::Transient`, or `AdapterError::Fatal` as appropriate, so callers don't need to know the specific backend to apply the right policy. Match on the `AdapterError` variant, not on the inner error, unless you have a backend-specific reason.

### `TokenError`

Returned from the channel-auth token issuance and verification paths in `net::adapter::net::identity`.

| Variant                          | When it fires                                                                              | What to do                                                                 |
|----------------------------------|--------------------------------------------------------------------------------------------|----------------------------------------------------------------------------|
| `Invalid`                        | Token doesn't deserialize or its signature doesn't verify                                  | Reject; the credential is forged or corrupted                              |
| `Expired`                        | Token's `not_after` is in the past, modulo the configured clock-skew window                | Re-issue from the current holder; tokens are time-bound on purpose         |
| `NotYetValid`                    | Token's `not_before` is in the future                                                      | Wait, or re-issue with an earlier validity window                          |
| `ScopeInsufficient`              | Token's scope doesn't cover the requested operation                                        | Request a token with the right scope (publish, subscribe, admin, delegate) |
| `ChannelMismatch`                | Token's `channel_hash` doesn't match the channel being accessed                            | Reject; the token is for a different channel                               |
| `DelegationDepthExhausted`       | Token has `delegation_depth == 0` and is being re-delegated                                | The chain has run out of remaining delegation hops                         |
| `Revoked`                        | Token's nonce is in the revocation list                                                    | Re-issue                                                                   |
| `RootNotTrusted`                 | Token chain doesn't root at any of the channel's `token_roots`                             | The chain is rooted at the wrong issuer; check the channel's configured trust roots |
| `TtlTooLong`                     | Requested TTL exceeds the one-year ceiling (`MAX_TOKEN_TTL_SECS`)                          | Issue inside the bound; the cap is intentional. The SDK's infallible `issue_token` helper soft-clamps; `try_issue` returns this error so callers can decide |

The TTL ceiling is a hard cap on the auth surface — issuing a token past one year is rejected on the fallible path and clamped on the SDK's infallible path. Long-lived grants need periodic re-issue, which re-checks the issuer's signing key and current policy.

### `TagMatcherError`

Returned from capability-tag matchers when the requested matcher can't be compiled or evaluated.

| Variant                                          | When it fires                                                                          | What to do                                                                 |
|--------------------------------------------------|----------------------------------------------------------------------------------------|----------------------------------------------------------------------------|
| `InvalidPattern(string)`                         | Pattern is syntactically malformed                                                     | Fix the pattern                                                            |
| `RegexNotBuiltIn { pattern }`                    | A `TagMatcher::Regex` variant was used against a build of the crate compiled without `--features regex` | Rebuild with `--features regex` or use a different matcher kind         |

The `regex` Cargo feature is off by default — regex matching adds about 1.1 MiB to binding artifacts, and most callers don't need it. Builds that do can opt in. Pre-v0.24 the regex-less fallback silently returned empty matches, which made misconfigured queries look indistinguishable from "no entries match"; v0.24 replaced that with the structured error above.

### nRPC errors (`RpcError`, `RpcAppError`)

Returned from `call_typed`, `call_streaming_typed`, `call_client_stream_typed`, `call_duplex_typed`, and the underlying `MeshRpc` surface.

| Variant                | When it fires                                                                          |
|------------------------|----------------------------------------------------------------------------------------|
| `RpcError::NoServer`   | No node is currently serving this service name                                         |
| `RpcError::NoMatchingServer` | A `net-where:` predicate ruled every advertising server out                       |
| `RpcError::Timeout`    | The call exceeded the configured timeout                                              |
| `RpcError::Canceled`   | A `Mesh::cancel(token)` aborted the in-flight call                                    |
| `RpcError::Panic`      | The handler panicked; caught and surfaced typed                                       |
| `RpcError::Codec`      | Request or response failed to encode/decode (sub-classified: `CodecEncode`, `CodecDecode`) |
| `RpcAppError(code, detail)` | Handler returned a typed application error                                       |

The codec error sub-classification is used by every binding's typed wrapper to surface the failure as a binding-native error type (TS / Python / Go all have idiomatic equivalents). The `RpcAppError` shape is wire-stable across languages — codes like `NRPC_TYPED_BAD_REQUEST` and `NRPC_TYPED_HANDLER_ERROR` are part of the cross-language fixture.

### Reliable-stream errors (`StreamError`)

Returned from the reliable-stream API on `MeshNode`.

| Variant                        | When it fires                                                                          |
|--------------------------------|----------------------------------------------------------------------------------------|
| `StreamError::WindowFull`      | Tx-credit window is exhausted; `send_with_retry` handles this automatically            |
| `StreamError::Backpressure`    | Scheduler queue is full for a scheduled stream                                         |
| `StreamError::Closed`          | Stream is closed locally or by the peer                                                |
| `StreamError::Reset`           | Peer sent a `SUBPROTOCOL_STREAM_RESET` after exhausting retransmit retries; payload includes the reason. Blob-transfer and other consumers map this to a higher-level error promptly instead of stalling to the caller's timeout |
| `StreamError::Timeout`         | Stream operation exceeded its configured timeout                                       |

## A note on credentials in URLs

Adapter constructors and `Debug` impls scrub `user:password@` from connection URLs before logging or rendering. A misconfigured operator who put a password directly in the URL won't leak it into log sinks — the redactor identifies the rightmost `@` in the authority component and replaces the userinfo with `[REDACTED]`.

This is per-adapter behavior, not part of the error API itself, but it shows up in `Debug` output of every adapter config and is worth knowing about when reading logs.
