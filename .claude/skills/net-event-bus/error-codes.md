# Error Codes — The Full Taxonomy

Read this when the user needs to **pattern-match on a specific error variant** to decide what to do — retry vs. drop vs. re-auth vs. fix-a-bug — or is seeing an error name this skill's other files don't explain (`TokenError::Revoked`, `TagMatcherError::RegexNotBuiltIn`, `ScalingError::ShardInUse`, `StreamError::Reset`, `RpcError::NoMatchingServer`).

**Relationship to `runtime.md`:** `runtime.md` covers the **SDK-facing** error surface each binding exposes (`SdkError`, TS/Python exceptions) and the practical "only `Backpressure` is blindly retry-safe" policy. This file is the **fuller core-crate taxonomy underneath** it, plus the subsystem errors (token/auth, tag matching, scaling, config, reliable-stream, nRPC) that don't fit the bus emit/poll path. If you just need "what do I retry on the bus," `runtime.md` is enough; come here when a variant name shows up that you need to classify.

All core errors use `thiserror` — every variant has a `Display` and, where it wraps another error, a working `source()` chain. **Pattern-match the variant to decide; format the `Display` to log.**

---

## The core trio

### `IngestionError` — from `EventBus::ingest()` / `ingest_raw()`

| Variant | When it fires | What to do |
|---|---|---|
| `Backpressure` | ring buffer full + policy rejected the event | apply your retry policy (not surfaced under `Block` mode) |
| `Sampled` | sampling/decimation dropped it before a shard | expected under sampling; no action |
| `Unrouted` | hashed shard id not in the routing table (mid-scaling) | back off *briefly* and retry — topology settles in ms |
| `ShuttingDown` | bus is shutting down; new ingests rejected | stop ingesting; flush and exit |
| `Serialization(_)` | payload couldn't be serialized | **bug** — inspect payload; `source()` → the `serde_json::Error` |

**`Unrouted` ≠ `Backpressure`.** Backpressure means "the destination is full" (wait). Unrouted means "there's no destination *right now*" (retry until topology stabilizes). They're distinct variants precisely so you apply the right remediation — don't blanket-backoff an `Unrouted`.

### `ConsumerError` — from `EventBus::poll()`

| Variant | When it fires | What to do |
|---|---|---|
| `Adapter(_)` | underlying adapter failed | see `AdapterError`; `is_retryable()` decides |
| `InvalidCursor(_)` | cursor couldn't be decoded | drop that cursor; restart from current tail with no cursor |
| `InvalidFilter(_)` | filter couldn't be parsed/evaluated | **bug** — inspect the filter; message includes a parse position (see `filter-dsl.md`) |

`ConsumerError::Adapter` wraps an `AdapterError`, so the full classification below is reachable through it.

### `AdapterError` — from adapter ops (`on_batch`, `poll_shard`, `flush`, `shutdown`); also wrapped in `ConsumerError`

| Variant | When it fires | Classification |
|---|---|---|
| `Transient(_)` | retryable failure (timeout, transient network) | `is_retryable() == true` |
| `Fatal(_)` | unrecoverable state | `is_fatal() == true` |
| `Backpressure` | backend rejected for capacity (Redis MAXLEN, JetStream MaxBytes) | `is_retryable() == true` |
| `Connection(_)` | connection-level failure (refused/broken/reset) | **not** retryable by default |
| `Shutdown` | adapter asked to stop, no longer accepting work | `is_shutdown() == true` |
| `Serialization(_)` | adapter codec failed | not retryable; bug in payload or codec |

Classification methods: `is_retryable()`, `is_fatal()`, `is_shutdown()`. The bus's dispatch loop reads them:

- **Retryable** → requeue with exponential backoff, bounded attempts.
- **Fatal** → drop the batch, record the drop in stats, log at error.
- **Shutdown** → drop the batch, halt ingestion (shutdown presumed in flight).
- **Connection (default)** → conservatively non-retryable: skip retries, drop immediately, don't burn the retry budget on a backend that's gone. **If you know your backend's connection errors are transient, return `AdapterError::Transient(...)` from your adapter instead** so they get retried.

## Subsystem errors

### `ScalingError` — from `add_shards()` / `remove_shards()`
`AlreadyAtLimit` (at the shard ceiling) · `ShardInUse` (removal target still has in-flight work) · `NoSuchShard` (unknown id) · `Internal(_)` (invariant violation — investigate).

### `ConfigError` — from `EventBusConfigBuilder::build()`
`InvalidShardCount` (`shards` out of range) · `InvalidBatchConfig` (inconsistent batch sizing, e.g. `max_events == 0`) · `IncompatibleFeatures` (a feature requested but not compiled in, e.g. `redis` adapter without the Cargo feature).

### Adapter-specific errors
Each shipped adapter has its own error type, surfaced up through `AdapterError::{Connection,Transient,Fatal}` so **you match on `AdapterError`, not the inner backend error**, unless you have a backend-specific reason:
- **`NetAdapter`** — `SessionFailed`, `RoutingFailed`, `AuthRejected`.
- **`RedisAdapter`** — wraps `redis::RedisError` (retryable: read timeouts, replica failover; fatal: auth).
- **`JetStreamAdapter`** — wraps `async-nats::Error`, similar classification.

### `TokenError` — channel-auth issuance/verification (`net::adapter::net::identity`)

| Variant | When it fires | What to do |
|---|---|---|
| `Invalid` | doesn't deserialize or signature fails | reject — forged/corrupted |
| `Expired` | `not_after` in the past (modulo clock-skew window) | re-issue from the current holder |
| `NotYetValid` | `not_before` in the future | wait, or re-issue with an earlier window |
| `ScopeInsufficient` | scope doesn't cover the operation | request a token with the right scope (publish/subscribe/admin/delegate) |
| `ChannelMismatch` | `channel_hash` is for a different channel | reject |
| `DelegationDepthExhausted` | `delegation_depth == 0` and re-delegated | chain ran out of delegation hops |
| `Revoked` | nonce is in the revocation list | re-issue |
| `RootNotTrusted` | chain doesn't root at any of the channel's `token_roots` | rooted at the wrong issuer; check the channel's trust roots |
| `TtlTooLong` | requested TTL > one-year ceiling (`MAX_TOKEN_TTL_SECS`) | issue inside the bound; infallible `issue_token` soft-clamps, `try_issue` returns this so you can decide |

The one-year TTL cap is a hard limit on the auth surface. Long-lived grants need periodic re-issue (which re-checks the signing key + current policy). See `concepts.md` § Identity for how token chains gate channels.

### `TagMatcherError` — capability-tag matcher compile/eval
`InvalidPattern(string)` (malformed pattern — fix it) · `RegexNotBuiltIn { pattern }` (a `TagMatcher::Regex` used against a build **without `--features regex`** — rebuild with it, or use a non-regex matcher). The `regex` feature is **off by default** (~1.1 MiB on binding artifacts). Pre-v0.24 a regex-less build silently returned empty matches; it's now this structured error, so a misconfigured query no longer looks like "nothing matched."

### nRPC — `RpcError` / `RpcAppError`
From `call_typed`, `call_streaming_typed`, `call_client_stream_typed`, `call_duplex_typed`:

| Variant | When it fires |
|---|---|
| `RpcError::NoServer` | no node currently serves this service name |
| `RpcError::NoMatchingServer` | a `net-where:` predicate ruled out every advertising server |
| `RpcError::Timeout` | call exceeded its configured timeout |
| `RpcError::Canceled` | a `Mesh::cancel(token)` aborted the in-flight call |
| `RpcError::Panic` | handler panicked; caught and surfaced typed |
| `RpcError::Codec` | request/response encode/decode failed (`CodecEncode` / `CodecDecode`) |
| `RpcAppError(code, detail)` | handler returned a typed application error |

`RpcAppError` is **wire-stable across languages** — codes like `NRPC_TYPED_BAD_REQUEST` / `NRPC_TYPED_HANDLER_ERROR` are part of the cross-language fixture; each binding's typed wrapper maps `Codec*` to an idiomatic native error. Full surface + retries/hedging in `nrpc.md`.

### Reliable-stream — `StreamError` (from the reliable-stream API on `MeshNode`)

| Variant | When it fires |
|---|---|
| `WindowFull` | tx-credit window exhausted — `send_with_retry` handles it automatically |
| `Backpressure` | scheduler queue full for a scheduled stream |
| `Closed` | stream closed locally or by the peer |
| `Reset` | peer sent `SUBPROTOCOL_STREAM_RESET` after exhausting retransmit retries; payload carries the reason (blob transfer maps this to a higher-level error promptly instead of stalling to timeout) |
| `Timeout` | stream operation exceeded its timeout |

These are **per-peer stream** errors, not bus errors — see `streams.md`. `WindowFull` / `Backpressure` are the retry-safe ones; `Closed` / `Reset` are terminal.

## The one-line policy

Everything above refines a single rule from `runtime.md`: **`Backpressure` (and `Unrouted`, briefly) is what you retry. `Serialization`, `Config`, `InvalidFilter`, `InvalidCursor` are bugs. Auth/`Token*` errors need a new credential. `Connection`/`Closed`/`Reset`/`Shutdown` are state changes retrying won't fix.**

## A note on credentials in URLs

Adapter constructors and their `Debug` impls **scrub `user:password@` from connection URLs** before logging — the redactor finds the rightmost `@` in the authority and replaces the userinfo with `[REDACTED]`. So a password accidentally put in a Redis/NATS URL won't leak into log sinks. This isn't part of the error API, but it shows up in every adapter config's `Debug` output and is worth knowing when reading logs.

## Cross-references

- `runtime.md` — SDK-facing errors per binding + the practical retry policy. Start there for bus emit/poll.
- `nrpc.md` — full nRPC error model, status codes, resilience helpers.
- `streams.md` — reliable per-peer streams and `StreamError` in context.
- `filter-dsl.md` — what produces `ConsumerError::InvalidFilter`.
- `concepts.md` § Identity — token chains behind `TokenError`.
