# nRPC SDK Consistency Audit

**Date:** 2026-08-08  
**Repository:** `ai-2070/net`  
**Audited commit:** `d6d6225fc6e19da66477e900ef04ee59b71fb947`  
**Branch:** `sdk-bugs`  
**Scope:** nRPC registration, unary and streaming calls, cancellation, flow control, channel-policy prerequisites, C ABI, type stubs, error projection, documentation, and test evidence across Rust, Node/TypeScript, Python, Go, and C.

## Executive summary

The non-Rust server surfaces are not currently viable as advertised. Node registration reaches `tokio::spawn` without an entered runtime and terminates the process; Go/C registration reaches the same prerequisite violation through its synchronous FFI path. The published C header also declares an older ABI with incompatible cancellation signatures. Even after those are repaired, non-Rust serve paths omit mandatory strict-channel policy installation, so default configurations still reject nRPC traffic.

The existing “cross-language” suites are codec/mocking fixtures and did not exercise real native service registration, which is why they remain green while a live Node `serve()` crashes immediately.

## 1. Critical — Node and Go/C service registration call `tokio::spawn` without an entered Tokio runtime

Core service registration directly spawns its bridge task:

- `net/crates/net/src/adapter/net/mesh_rpc.rs:3682-3692`

Node's synchronous registration methods call core directly without entering a runtime:

- unary: `net/crates/net/bindings/node/src/mesh_rpc.rs:1697-1717`
- client-streaming: `net/crates/net/bindings/node/src/mesh_rpc.rs:1941`
- duplex: `net/crates/net/bindings/node/src/mesh_rpc.rs:1974`
- server-streaming: `net/crates/net/bindings/node/src/mesh_rpc.rs:2015`

Go/C FFI does the same:

- unary: `net/crates/net/bindings/go/rpc-ffi/src/lib.rs:1090-1127`
- client-streaming: `net/crates/net/bindings/go/rpc-ffi/src/lib.rs:3231`
- server-streaming: `net/crates/net/bindings/go/rpc-ffi/src/lib.rs:3398`
- duplex: `net/crates/net/bindings/go/rpc-ffi/src/lib.rs:3455`

Python explicitly enters its runtime before each equivalent call:

- `net/crates/net/bindings/python/src/mesh_rpc.rs:2226,2498,2526,2556`

A live Node reproduction of `MeshRpc.serve()` terminated with:

```text
there is no reactor running, must be called from the context of a Tokio 1.x runtime
```

at core `mesh_rpc.rs:3692`, with process exit 127. The published TypeScript serve example therefore crashes before serving:

- `web/src/content/docs/sdk/invoke/typescript.md:25-47`

The C FFI wraps calls in its panic guard, so the exact process-level outcome differs, but registration still cannot succeed without entering the runtime.

### Required closure

- Enter the binding-owned runtime around every synchronous registration path, as Python already does.
- Ensure spawned tasks remain attached to a runtime whose lifetime exceeds the serve handle.
- Add live native serve/call witnesses for every call shape in Node and Go/C.
- Treat any registration panic as a release blocker.

## 2. Critical — Published C header is ABI-incompatible with the implementation

The checked-in public header declares:

- ABI `0x0002`: `net/crates/net/include/net_rpc.h:90-106`

The implementation declares breaking ABI `0x0004`:

- `net/crates/net/bindings/go/rpc-ffi/src/lib.rs:171-181`

Cancellation signatures disagree:

| Operation | Header | Implementation |
|---|---|---|
| Reserve token | `net_rpc_reserve_cancel_token(void)` | `net_rpc_reserve_cancel_token(MeshRpcHandle*)` at `net/crates/net/bindings/go/rpc-ffi/src/lib.rs:275` |
| Cancel call | `net_rpc_cancel_call(uint64_t)` | `net_rpc_cancel_call(MeshRpcHandle*, uint64_t)` at `net/crates/net/bindings/go/rpc-ffi/src/lib.rs:311` |

ABI `0x0004` explicitly records the leading-handle argument as breaking. A consumer compiled against the current header can call the implementation with invalid register/stack arguments interpreted as a mesh pointer.

The compatibility helper only checks whether the runtime ABI is greater than or equal to the expected value. A stale `0x0002` header checking an `0x0004` runtime therefore passes despite the breaking signatures.

### Required closure

Generate or synchronize the public header from the implementation contract. Make ABI compatibility distinguish additive version advance from breaking incompatibility; a simple `runtime >= header` comparison is insufficient after a breaking change. Add compile-and-call C/Go cancellation witnesses against packaged headers and libraries.

## 3. High — Non-Rust serve paths omit mandatory nRPC channel-policy installation

Node and Python create an installed, strict empty channel registry by default:

- Node: `net/crates/net/bindings/node/src/lib.rs:1641-1651`
- Python: `net/crates/net/bindings/python/src/lib.rs:1439-1445`

Unknown channels are rejected:

- `net/crates/net/src/adapter/net/mesh.rs:26473-26475`

nRPC needs policies for `<service>.requests` and `<service>.replies.*`, installed by:

- `install_rpc_service_defaults`: `net/crates/net/src/adapter/net/channel/config.rs:821-903`

The Rust high-level SDK installs these defaults before serving. Node, Python, and Go/C wrappers call bare `MeshNode::serve_rpc*` and do not invoke the installer.

Therefore, after fixing the runtime defect, default binding configurations still cannot complete nRPC traffic unless callers enable globally permissive channels or manually recreate an internal dynamic-channel policy that the advertised serve API should own.

This finding depends on the channel subsystem but is included here because it makes the nRPC serve contract impossible.

### Required closure

Route every public serve method through one shared install-if-absent policy helper. Preserve operator-provided stricter ACLs. Add default-strict registration and end-to-end request/reply witnesses for every binding.

## 4. High — Node `AbortSignal` cancellation is ineffective for server-streaming calls

TypeScript's `CallOptions` exposes `signal` as the caller-driven cancellation surface:

- `net/crates/net/bindings/node/mesh_rpc.ts:57-80`

Unary, client-streaming, and duplex methods call `wireAbortSignal`. Direct and service-addressed server-streaming methods do not:

- `callStreaming`: `net/crates/net/bindings/node/mesh_rpc.ts:642-655`
- `callServiceStreaming`: `net/crates/net/bindings/node/mesh_rpc.ts:666-680`

They pass the JavaScript `AbortSignal` object directly to the raw native options. No cancel token is reserved and no abort listener invokes `cancelCall`.

A focused mock reproduction observed after abort:

```text
reserve = 0
cancel = 0
cancelToken = null
```

### Required closure

Use `wireAbortSignal` for both server-streaming methods and transfer the detach callback into the returned typed stream so listener lifetime ends on EOF/close/error.

## 5. High — TypeScript's public type blocks native request-side flow control

Native Node `CallOptions` exposes `requestWindowInitial` and maps it into core:

- `net/crates/net/bindings/node/src/mesh_rpc.rs:149-220`

The public TypeScript `CallOptions` interface omits the property:

- `net/crates/net/bindings/node/mesh_rpc.ts:52-92`

Yet the TypeScript implementation and docs refer to it for client-streaming and duplex calls, including `mesh_rpc.ts:175-177,886,904`.

A focused TypeScript compile reproduction failed with:

```text
TS2353: 'requestWindowInitial' does not exist in type 'CallOptions'
```

Typed callers cannot enable upload flow control without an unsafe cast.

### Required closure

Add the field with explicit zero/undefined/default semantics and compile examples for both client-streaming and duplex flow control.

## 6. High — Top-level nRPC documentation promises unimplemented wire and durability semantics

`README.md:243` describes the wire format as JSON. Raw nRPC is byte-oriented; Rust layers can use JSON, postcard, or custom codecs.

The same section says deadlines, retries, hedging, circuit breakers, and cancellation work the same across Rust, TypeScript, Python, and Go. Go has deadlines/cancellation but no nRPC retry, hedge, or circuit-breaker helpers.

`README.md:247` promises durable audit logs, replay after crashes, fold rehydration, in-flight migration, and at-least-once handler execution.

Current serve state is an in-memory bounded `mpsc` plus `RpcServerFold`:

- `net/crates/net/src/adapter/net/mesh_rpc.rs:3421-3426,3551-3561`

Full queues drop requests/responses and callers time out:

- `mesh_rpc.rs:3423-3425,3462-3465`

No persistence, snapshot restore, or durable replay integration backs the advertised guarantees.

### Required closure

Rewrite the top-level contract to distinguish raw bytes, optional codecs, implemented resilience helpers per SDK, and current in-memory request state. Durable semantics should not be claimed until backed by explicit storage/recovery paths and crash witnesses.

## 7. Medium — Python native stubs omit implemented async nRPC methods

`AsyncMeshRpc` stubs omit at least:

- `serve_streaming`;
- `call_service_streaming`;
- lifecycle methods available on several async stream and duplex handles.

Stub location:

- `net/crates/net/bindings/python/python/net/_net.pyi:2515-2569`

Runtime implementation exists around:

- `net/crates/net/bindings/python/src/mesh_rpc.rs:2845-3082`

Static type checking rejects runtime-supported operations.

### Required closure

Generate or parity-check stubs from the actual exported method inventory. Add a type-check fixture for every runtime async method and handle lifecycle operation.

## 8. Medium — Go application errors are documented as structured code/body but expose only text

Go documentation says callers receive application error code and body:

- `web/src/content/docs/sdk/invoke/go.md:78-93`

Go's `RpcError` contains only `Kind` and `Message`:

- `go/mesh_rpc.go:388-418`

It does not expose structured application status/body fields. Go also lacks a `RpcKindCancelled` discriminator:

- `go/mesh_rpc.go:377-386`

Context cancellation instead returns `context.Canceled` or `context.DeadlineExceeded`.

### Required closure

Either expose application status/body as structured fields or correct the guide. Document context cancellation separately from protocol cancellation.

## 9. Medium — “Cross-language” tests are codec fixtures and mocks, not interoperability tests

Node tests instantiate a loopback mock:

- `net/crates/net/bindings/node/test/cross_lang_compat.test.ts:191-224`

Python does likewise:

- `net/crates/net/bindings/python/tests/test_cross_lang_compat.py:139-164`

These tests pin JSON shape and status constants but do not connect two processes or two language bindings. The Node mock suites remained green while the first real native `serve()` call terminated at runtime.

Go docs acknowledge the lack of a live tool registration/call round trip:

- `web/src/content/docs/sdk/invoke/go.md:111-113`

### Required closure

Rename codec fixtures accurately and add a native interoperability matrix: Rust↔Node, Rust↔Python, Rust↔Go/C, plus at least one non-Rust pair. Exercise unary, every streaming shape, cancellation, flow control, strict channel defaults, application errors, and shutdown.

## Support boundaries observed

- **Rust SDK:** richest typed/raw unary and streaming surface, routing, resilience helpers, observer/metrics, and protected/org unary modes.
- **Node:** intended raw/JSON unary and four streaming shapes, but registration is currently blocked and several typed options drift.
- **Python:** comparable direct call-shape coverage with explicit runtime entry and asyncio cancellation bridges; stubs lag runtime.
- **Go:** raw and typed JSON direct calls plus streaming; context cancellation/deadlines but no retry/hedge/breaker or structured response metadata parity.
- **C:** raw-byte ABI only; currently blocked by stale header and registration runtime prerequisites.
- Client-streaming and duplex service-name routing are absent in core as well as bindings; this is a supported-boundary limitation, not a binding regression.
- Protected/org registration is unary-only in core by design.

## Verification

Executed against the audited commit:

- `cargo test -p net-rpc-ffi`: **39 passed**.
- Node nRPC and cross-language mock suites in the audit lane: **77 passed**.
- Python wrapper and cross-language mock suites: **74 passed**.
- Node TypeScript typecheck: passed except for the intentional missing-option reproduction.
- Live Node native `serve()` reproduction: deterministic no-runtime termination.
- Focused ABI test confirmed implementation ABI `0x0004`.

### Limitations

- Full Go tests could not build without the repository's required native feature/build configuration.
- Native Python live-mesh testing was unavailable in the active interpreter.
- The green unit/mock suites do not constitute native interoperability evidence.

## Conclusion

nRPC should remain on HOLD for non-Rust serving until runtime entry, strict-channel policy installation, and the C ABI are repaired with live witnesses. Cancellation and flow-control type fixes are smaller, but the top-level durability prose and test labels must also be corrected so green mocks are not mistaken for production cross-language execution.
