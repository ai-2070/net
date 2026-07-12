# Error Codes — The Full Taxonomy

Read this when the user needs to **pattern-match on a specific error variant** to decide what to do — retry vs. drop vs. re-auth vs. fix-a-bug — or is seeing an error name this skill's other files don't explain (`TokenError::Revoked`, `TagMatcherError::RegexNotBuiltIn`, `ScalingError::InCooldown`, `StreamError::Backpressure`, `RpcError::NoMatchingServer`).

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
`InvalidPolicy(_)` (the scaling policy was rejected — the string says why) · `AtMaxShards` (already at the shard ceiling) · `AtMinShards` (already at the floor) · `InCooldown` (a scale op is still inside its cooldown window — retry later) · `ShardCreationFailed(_)` (the new shard couldn't be built — investigate).

### `ConfigError` — from `EventBusConfigBuilder::build()`
One variant: `InvalidValue(_)` — the string names the offending setting (a `shards` count out of range, inconsistent batch sizing such as `max_events == 0`, a feature requested but not compiled in, …). Match the variant; read the string to see which knob.

### Adapter-specific errors
Each shipped adapter has its own error type, surfaced up through `AdapterError::{Connection,Transient,Fatal}` so **you match on `AdapterError`, not the inner backend error**, unless you have a backend-specific reason:
- **`NetAdapter`** — `SessionFailed`, `RoutingFailed`, `AuthRejected`.
- **`RedisAdapter`** — wraps `redis::RedisError` (retryable: read timeouts, replica failover; fatal: auth).
- **`JetStreamAdapter`** — wraps `async-nats::Error`, similar classification.

### `TokenError` — channel-auth issuance/verification (`net::adapter::net::identity`)

| Variant | When it fires | What to do |
|---|---|---|
| `InvalidSignature` | the token's signature doesn't verify | reject — forged/corrupted |
| `InvalidFormat` | wire bytes too short or malformed | reject — corrupted/garbage |
| `Expired` | `not_after` in the past (modulo clock-skew window) | re-issue from the current holder |
| `NotYetValid` | `not_before` in the future | wait, or re-issue with an earlier window |
| `NotAuthorized` | no valid token covers the requested action | request a token with the right scope (publish/subscribe/admin/delegate) |
| `DelegationNotAllowed` | the token lacks the `DELEGATE` scope but tried to re-delegate | issue from a token that carries delegate authority |
| `DelegationExhausted` | delegation depth hit zero and was re-delegated | chain ran out of delegation hops |
| `Revoked` | a chain link is at/below its issuer's revocation floor | re-issue — kept distinct from `NotAuthorized` so you can tell a revoked credential from a never-authorized one |
| `ReadOnly` | signing attempted with a public-only (zeroized / read-only) keypair | you hold a verify-only key — can't sign |
| `ZeroTtl` | `duration_secs == 0` passed to `try_issue` | issue with a non-zero TTL (a 0-TTL token is instantly `Expired`) |
| `TtlTooLong` | requested TTL > ceiling (`MAX_TOKEN_TTL_SECS`) | issue inside the bound; `issue_token` soft-clamps, `try_issue` returns this so you can decide |

The one-year TTL cap is a hard limit on the auth surface. Long-lived grants need periodic re-issue (which re-checks the signing key + current policy). See `concepts.md` § Identity for how token chains gate channels.

### `TagMatcherError` — capability-tag matcher compile/eval
One variant: `RegexNotBuiltIn { pattern }` — a `TagMatcher::Regex` used against a build **without `--features regex`** (rebuild with it, or use a non-regex matcher); it carries the offending pattern. The `regex` feature is **off by default** (~1.1 MiB on binding artifacts). Pre-v0.24 a regex-less build silently returned empty matches; it's now this structured error, so a misconfigured query no longer looks like "nothing matched."

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

### Per-peer stream — `StreamError` (from the stream API on `MeshNode`)

| Variant | When it fires |
|---|---|
| `Backpressure` | the stream's outbound queue is full — no packets were enqueued; retry, drop, or surface further |
| `NotConnected` | the underlying session is gone (peer disconnected, never connected, or the stream was closed) |
| `Transport(_)` | underlying transport failure (socket / encryption error); wraps the originating adapter-level error's message |

These are **per-peer stream** errors, not bus errors — see `streams.md`. `Backpressure` is the retry-safe one; `NotConnected` / `Transport` are terminal for that stream. (`WindowFull` and stream-reset live below this surface — the tx-credit admittance value and the wire reset message, respectively — not `StreamError` variants.)

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
