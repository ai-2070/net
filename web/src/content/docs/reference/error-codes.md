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

## A note on credentials in URLs

Adapter constructors and `Debug` impls scrub `user:password@` from connection URLs before logging or rendering. A misconfigured operator who put a password directly in the URL won't leak it into log sinks — the redactor identifies the rightmost `@` in the authority component and replaces the userinfo with `[REDACTED]`.

This is per-adapter behavior, not part of the error API itself, but it shows up in `Debug` output of every adapter config and is worth knowing about when reading logs.
