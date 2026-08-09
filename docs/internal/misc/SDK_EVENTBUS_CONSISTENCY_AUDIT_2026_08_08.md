# EventBus SDK Consistency Audit

**Date:** 2026-08-08  
**Repository:** `ai-2070/net`  
**Audited commit:** `d6d6225fc6e19da66477e900ef04ee59b71fb947`  
**Branch:** `sdk-bugs`  
**Scope:** EventBus construction, reliability, ingestion, polling, binary payloads, receipts, timestamps, flush, batch errors, and statistics across Rust, Node/TypeScript, Python, Go, and C. Named-channel findings are excluded and remain in the separate channel audit.

## Executive summary

The EventBus surfaces are broadly recognizable across bindings, but several differences change correctness rather than merely syntax. The most serious are fail-open reliability parsing and a C poll contract that documents ordering while silently ignoring it. TypeScript loses binary data and nanosecond precision at its ergonomic boundary; Python lacks a reusable delivery barrier; and Go silently omits typed batch elements that fail serialization.

## 1. P1 — Reliability typos silently disable retransmission in Node, Python, C, and Go

Rust uses the closed `ReliabilityConfig::{None, Light, Full}` enum:

- `net/crates/net/src/adapter/net/config.rs:9-25`

The string-based boundaries map every unknown value to `ReliabilityConfig::None`:

- Node: `net/crates/net/bindings/node/src/lib.rs:978-984`
- Python/PyO3: `net/crates/net/bindings/python/src/lib.rs:597-602`
- C parser: `net/crates/net/src/ffi/mod.rs:728-734`
- Go exposes an unchecked string passed to C: `go/net.go:137-138`

A value such as `"ful"`, `"FULL"`, or a future unsupported spelling therefore constructs successfully but changes delivery from acknowledgement/retransmission to fire-and-forget. Role and backpressure parsing already reject unsupported values, so reliability's fallback is inconsistent even within the same constructors.

### Required closure

Reject every value outside the documented vocabulary at all string boundaries. Add inverse tests for case errors, whitespace, near-miss spellings, and unknown future values.

## 2. P1 — C documents poll ordering but silently ignores it; Go cannot request it

`net_poll` documents a request such as:

```json
{"limit":100,"ordering":"InsertionTs"}
```

at `net/crates/net/src/ffi/mod.rs:1162-1169`.

But `parse_poll_request_json` reads only `limit` and `cursor` and constructs `ConsumeRequest` without ordering, filtering, or shard selection:

- `net/crates/net/src/ffi/mod.rs:1072-1103`

The call succeeds, so the caller receives results without the requested ordering and without an error. Go hard-codes the reduced limit/cursor shape:

- `go/net.go:339-427`

Other surfaces expose richer semantics:

- Rust filter, ordering, and shards: `net/crates/net/sdk/src/net.rs:35-48,165-180`
- TypeScript filter and ordering: `net/crates/net/sdk-ts/src/types.ts:68-78`
- Python filter and ordering: `net/crates/net/sdk-py/src/net_sdk/node.py:189-197`

Filtering and shard selection are feature gaps. Silently accepting and ignoring documented C ordering is the correctness defect.

### Required closure

Either implement ordering in the C parser and expose it in Go, or reject unknown/unsupported request fields and correct the documented shape. A successful request must not silently weaken requested ordering.

## 3. P2 — TypeScript advertises binary ingestion but drops non-UTF-8 payloads at the ergonomic poll boundary

The TypeScript wrapper exposes:

- `emitBuffer(Buffer)`: `net/crates/net/sdk-ts/src/node.ts:80-85`

The native binding preserves binary payloads in `rawBytes` and leaves `raw` empty when bytes are not UTF-8:

- native `StoredEvent`: `net/crates/net/bindings/node/src/lib.rs:362-379`
- native poll mapping: `net/crates/net/bindings/node/src/lib.rs:674-686`

The ergonomic SDK omits `rawBytes` from its type and mapping:

- public type: `net/crates/net/sdk-ts/src/types.ts:56-66`
- one-shot poll: `net/crates/net/sdk-ts/src/node.ts:128-137`
- stream projection: `net/crates/net/sdk-ts/src/stream.ts:69-75`

A non-UTF-8 payload accepted through the wrapper's explicit binary path cannot be recovered through that same wrapper.

### Required closure

Carry `rawBytes` through `StoredEvent`, `poll`, and streaming. Define whether JSON/typed helpers reject binary events or return an explicit payload union.

## 4. P2 — Python exposes no reusable flush/delivery barrier

Flush exists in:

- Rust: `net/crates/net/sdk/src/net.rs:229-233`
- TypeScript/Node: `net/crates/net/sdk-ts/src/node.ts:203-206`
- Go: `go/net.go:470-481`
- C: `net/crates/net/include/net.h:278-283`

It is absent from both Python layers:

- PyO3 lifecycle jumps from stats to shutdown: `net/crates/net/bindings/python/src/lib.rs:803-829`
- ergonomic wrapper lifecycle: `net/crates/net/sdk-py/src/net_sdk/node.py:281-298`
- public stub: `net/crates/net/bindings/python/python/net/_net.pyi`

Python shutdown drains, but shutdown is terminal and cannot be used as a barrier before continuing to publish.

### Required closure

Expose `flush()` through PyO3, the ergonomic wrapper, and the `.pyi` contract. Add a test that publishes, flushes, observes adapter progress, then publishes again.

## 5. P2 — Node receipts and stored timestamps lose nanosecond precision

Core, Python, and C retain exact integer timestamp values:

- Rust receipt: `net/crates/net/sdk/src/net.rs:15-22`
- Python: `net/crates/net/bindings/python/src/lib.rs:137-145`
- C: `net/crates/net/include/net.h:93-97`

Node casts timestamps to signed `i64` and then exposes JavaScript `number`:

- native receipt and event types: `net/crates/net/bindings/node/src/lib.rs:362-399,547-555,684-685`
- ergonomic SDK types: `net/crates/net/sdk-ts/src/types.ts:48-65`

Unix-epoch nanoseconds already exceed JavaScript's exact integer ceiling, `2^53 - 1`. A boundary reproduction with `9007199254740993` returned `9007199254740992` after a `number` round trip.

Node already uses `BigInt` for large counters and stream timestamps, so this is inconsistent with its own numeric policy.

### Required closure

Expose receipt and stored-event nanoseconds as `BigInt` throughout the native and ergonomic TypeScript surfaces. Treat this as a breaking type correction and add values around `2^53` plus current epoch nanoseconds.

## 6. P2 — Go typed batch ingestion silently omits serialization failures

Go's typed batch helper skips values for which `json.Marshal` fails and ingests the remainder:

- `go/net.go:324-337`

It returns only the accepted count. A caller cannot distinguish serialization omissions from ring-buffer/backpressure drops.

Rust, TypeScript, and Python serialize the complete batch before native ingestion, so serialization failure aborts instead of silently deleting selected items:

- Rust: `net/crates/net/sdk/src/net.rs:147-155`
- TypeScript: `net/crates/net/sdk-ts/src/node.ts:88-100`
- Python: `net/crates/net/sdk-py/src/net_sdk/node.py:155-181`

### Required closure

Return an error containing the failed index, or return explicit per-item outcomes that distinguish serialization rejection from ingestion rejection. Do not collapse the two into one short count.

## 7. P3 — Node and Python omit dispatch-progress statistics

Rust, C, and Go expose `batches_dispatched`:

- Rust: `net/crates/net/sdk/src/net.rs:24-33,207-216`
- C: `net/crates/net/include/net.h:117-122`
- Go: `go/net.go:183-188`

Node and Python expose only ingested and dropped counters:

- Node: `net/crates/net/bindings/node/src/lib.rs:401-415,708-728`
- Python: `net/crates/net/bindings/python/src/lib.rs:247-265,803-818`

This removes the direct observer for adapter dispatch progress from two SDKs.

### Required closure

Expose the complete statistics record consistently or explicitly version and document reduced binding projections.

## Verification

Executed against the audited commit:

- `cargo test -p net-mesh-sdk --test shutdown_regression`: **2 passed**.
- `cargo test -p net-mesh --test ffi_poll_buffer --features ffi`: **3 passed**.
- `npm test -- --run test/ingest_failure.test.ts`: **7 passed**.
- Numeric precision and source-boundary reproductions confirmed the documented conversion behavior.

### Limitations

- `npm run build` failed because the checked-in SDK/core generated contract is version-skewed; this is assigned to the separate packaging audit rather than this EventBus report.
- `go test ./...` could not exercise the cgo runtime in this environment because cgo-backed definitions were unavailable.
- Language-native sync/async style, iterator syntax, and error carriers were not classified as defects by themselves.

## Conclusion

EventBus parity needs a fail-closed boundary policy: reject unknown configuration values, never accept options that are ignored, preserve payload bytes and integer precision, and distinguish serialization from ingestion failure. The current method-name parity is insufficient because several bindings silently weaken or lose caller intent.
