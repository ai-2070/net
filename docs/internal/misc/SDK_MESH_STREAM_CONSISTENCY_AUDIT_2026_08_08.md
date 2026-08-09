# Mesh Stream SDK Consistency Audit

**Date:** 2026-08-08  
**Repository:** `ai-2070/net`  
**Audited commit:** `d6d6225fc6e19da66477e900ef04ee59b71fb947`  
**Branch:** `sdk-bugs`  
**Scope:** Mesh construction, addressing, receive/shard behavior, stream windows, stream closure, PSK examples, and shutdown across Rust, Node/TypeScript, Python, Go, and C. Authenticated channel findings are excluded.

## Executive summary

The central correctness defect is receive reachability: inbound traffic is assigned by `stream_id % num_shards`, while several SDK surfaces expose only shard-0 polling or no receive operation at all. With the default four shards, most ordinary stream IDs cannot be consumed through TypeScript or Python. Additional gaps make ephemeral addresses unusable, make Go's documented unbounded window impossible to request, and prevent C/Go callers from closing core streams.

## 1. P1 — TypeScript and Python cannot receive most default-sharded stream traffic

Inbound events are assigned by stream ID:

```text
shard_id = stream_id % num_shards
```

at `net/crates/net/src/adapter/net/mesh.rs:19108-19113`.

Rust `Mesh::recv()` claims to return events from all shards but polls only shard 0:

- `net/crates/net/sdk/src/mesh.rs:694-700`

Only `recv_shard` is complete:

- `net/crates/net/sdk/src/mesh.rs:703-706`

The ergonomic TypeScript and Python `MeshNode` wrappers expose no receive API. Their native poll methods also hard-code shard 0:

- Node: `net/crates/net/bindings/node/src/lib.rs:1799-1807`
- Python: `net/crates/net/bindings/python/src/lib.rs:1804-1810`

With the default four shards, stream IDs congruent to 1, 2, or 3 modulo 4 are unreadable through those surfaces. The stream guide contains impossible examples:

- stream `7` is opened while the receiver is told to poll shard 0; `7 % 4 == 3`;
- `0xCAFE` is paired with shard 0, although `0xCAFE % 4 == 2`.

Relevant guide locations include `.claude/skills/net-event-bus/streams.md:79,103,202,295`.

Existing tests acknowledge the real mapping and poll all four shards:

- Rust: `net/crates/net/sdk/tests/mesh_channels.rs:86-97`
- Go: `go/mesh_test.go:143-153`

### Required closure

- Make Rust `recv()` actually merge all shards or rename it to disclose shard 0.
- Expose `recv_shard` and/or merged receive in Node, TypeScript, PyO3, and Python.
- Correct every stream example to poll the derived shard or use a true all-shard receive API.
- Add inverse tests with stream IDs mapping to every default shard.

## 2. P1 — Go's documented unbounded stream window cannot be requested

Go declares:

```go
WindowBytes uint32 `json:"window_bytes,omitempty"`
```

and documents `0` as disabling backpressure:

- `go/mesh.go:222-230`

Because the field uses `omitempty`, zero is omitted. The C/Rust parser receives `None` and applies the 64 KiB default:

- `net/crates/net/src/ffi/mesh.rs:1567-1574,1631`

A Go caller requesting the documented unbounded mode therefore silently receives bounded backpressure.

### Required closure

Use `*uint32`, an explicit mode field, or a custom marshaler so absent and explicit zero remain distinct. Add a round-trip test proving both default and unbounded configurations reach Rust correctly.

## 3. P1 — Ephemeral bind addresses are unusable through several advertised SDK layers

Rust exposes the resolved local address:

- `net/crates/net/sdk/src/mesh.rs:522-525`

Native Node and PyO3 expose it:

- Node `localAddr()`: `net/crates/net/bindings/node/src/lib.rs:1682-1690`
- Python `local_addr`: `net/crates/net/bindings/python/src/lib.rs:1501-1507`

But the ergonomic TypeScript and Python `MeshNode` wrappers do not re-export it. Go and C expose no local-address ABI.

Their READMEs nevertheless construct nodes on `127.0.0.1:0`:

- TypeScript: `net/crates/net/sdk-ts/README.md:63-64`
- Python: `net/crates/net/sdk-py/README.md:53-56`
- Go: `go/README.md:89-96`

A node bound to port zero cannot tell another process which OS-selected port to connect to through those advertised APIs. Go tests work around this with a bind-close-rebind port probe:

- `go/mesh_test.go:15-35`

That workaround introduces a time-of-check/time-of-use race.

### Required closure

Expose the resolved local address consistently through every public layer, then add real `:0` handshake examples and tests. Until then, examples that require another node to connect must use a chosen port.

## 4. P2 — Go and C cannot close underlying core streams

Rust, Node, and Python expose core stream closure. C exports only `net_mesh_stream_free`:

- `net/crates/net/src/ffi/mesh.rs:1654-1670`

That operation drops the FFI handle and `Arc`; it does not call `MeshNode::close_stream` or remove core stream state. Go's `MeshStream.Close()` only invokes that handle-free function:

- `go/mesh.go:780-793`

No `net_mesh_close_stream` ABI exists.

Long-lived C/Go nodes therefore cannot:

- eagerly release stream state;
- enforce a close/reopen epoch;
- reopen the same stream ID with a new configuration.

The original “first open wins” configuration remains until node shutdown.

### Required closure

Add an explicit core close operation to the C ABI and route Go `Close()` through it before freeing the handle. Test state removal, repeated close, stale handles, and reopen with changed configuration.

## 5. P2 — Published TypeScript quickstart supplies the wrong PSK type

The TypeScript configuration requires a hex string:

- `MeshNodeConfig.psk`: `net/crates/net/sdk-ts/src/mesh.ts:219-224`
- creation forwards that value to native hex decoding: `net/crates/net/sdk-ts/src/mesh.ts:431-443`

The README supplies a `Uint8Array`:

- `net/crates/net/sdk-ts/README.md:63-64`

The snippet is type-invalid and does not satisfy the runtime constructor.

### Required closure

Use a 64-character hex string in the README or intentionally change the public API to accept bytes and normalize them before the native boundary. Add snippet compilation to the documentation gate.

## 6. P2 — TypeScript shutdown documentation falsely promises idempotence

The runtime guide says TypeScript shutdown is idempotent and safe to call twice:

- `.claude/skills/net-event-bus/runtime.md:39`

The high-level wrapper forwards directly:

- `net/crates/net/sdk-ts/src/mesh.ts:966-969`

The native Node binding returns `"already shut down"` on a second call:

- `net/crates/net/bindings/node/src/lib.rs:2283-2287`

Python, Go, and C expose idempotent behavior, making this an actual cross-language difference as well as false TypeScript documentation.

### Required closure

Either make TypeScript/native shutdown idempotent or correct the guide and expose a stable typed already-shutdown outcome. Add double-shutdown tests at both native and ergonomic layers.

## Verification

Executed against the audited commit:

- `cargo test -p net-mesh-sdk --features net --test mesh_stream_backpressure`: **2 passed**.
- Source verification confirmed exact stream-to-shard mapping and the documented example mappings.

### Limitations

- TypeScript full typecheck was blocked by stale/generated local `@net-mesh/core` declarations; this belongs to the separate packaging audit.
- Go runtime tests could not link because `gcc` was unavailable; with CGO disabled, mesh files are excluded.
- Python runtime introspection was blocked by feature skew in the existing `.pyd` (`AsyncPinStore` missing), so Python wrapper findings were verified against source and stubs.

## Conclusion

The SDK spine needs one explicit receive contract: either merged all-shard delivery or caller-visible shard selection. Stream IDs cannot be opaque while receive APIs silently expose only shard 0. Address resolution, zero-window encoding, core closure, and shutdown semantics should then be aligned around executable cross-binding witnesses.
