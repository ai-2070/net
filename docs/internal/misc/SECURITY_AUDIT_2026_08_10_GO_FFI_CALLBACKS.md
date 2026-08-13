# Go/FFI Callback and Lifecycle Security Audit — 2026-08-10

## Status

**Verdict: HOLD.** Two high-confidence process-availability defects were retained. No memory-corruption primitive was confirmed.

```text
Audited commit: 43b66dbc740381cf97e6cc1e19fa52fb7bf9c99a
Branch: security-4
Upstream: origin/security-4
Divergence during audit: 0 ahead / 0 behind
Repository: C:\Users\chief\Documents\git\net
```

This is a frozen follow-on packet to `SECURITY_AUDIT_2026_08_10_CROSS_SUBSYSTEM.md`. It records the completed extended Go/FFI review that was still pending when the cross-subsystem report was frozen.

## Scope

The review inventoried 17 Go `//export` trampolines and traced Rust→C→Go callbacks, Go→C→Rust handles, finalizers, explicit free paths, callback quiescence, pointer/length conversion, and allocator ownership across Compute, MeshOS, RPC/Org, MeshDB, Deck, and MCP bindings.

MeshDB, Deck, and MCP FFI crates expose no Rust→Go callback types or Go exports at this commit. Python/pyo3 and Node/napi surfaces do not use equivalent raw callback bridges in the reviewed paths.

## FFI-01 — Uncontained Go callback panics can terminate the process

**Severity:** High availability  
**CWE:** CWE-248, Uncaught Exception  
**Confidence:** High  
**Status:** Confirmed by complete cross-boundary source trace; child-process witness required

### Violated invariant

Every foreign-language callback must convert language panics/exceptions into an ABI status. A Go panic must never cross an exported cgo callback boundary into C or Rust.

### Affected callbacks

Ten user-callback trampolines lack top-level panic containment:

- Compute: `Process`, `Snapshot`, `Restore`;
- MeshOS: `Process`, `Snapshot`, `Restore`, `OnControl`, `Health`, `Saturation`;
- typed RPC observer.

Go trampoline evidence:

```text
net/crates/net/go/compute_dispatch.go:240-280
net/crates/net/go/compute_dispatch.go:283-320
net/crates/net/go/compute_dispatch.go:323-342
net/crates/net/go/meshos.go:767-893
net/crates/net/go/mesh_rpc_typed.go:926-933
```

Rust invokes these callbacks directly:

```text
net/crates/net/bindings/go/compute-ffi/src/lib.rs:1080-1156
net/crates/net/bindings/go/meshos-ffi/src/lib.rs:506-603
net/crates/net/bindings/go/rpc-ffi/src/lib.rs:3614-3650
```

None of the affected paths installs a deferred `recover`. Rust `catch_unwind` cannot translate a panic originating in Go across cgo.

### Reachability and prerequisites

An application has installed one of the affected callbacks and that callback contains a panic-reachable path. A mesh peer can shape Compute or MeshOS event payloads to trigger ordinary Go panics such as indexing an empty payload, a failed type assertion, or a nil-map write. The observer path requires completion of an outbound RPC.

This is not merely “user code can crash itself.” The same package explicitly contains panics in unary RPC, streaming RPC, Org handlers, and the Compute factory to prevent a buggy user callback from terminating the process:

```text
net/crates/net/go/mesh_rpc.go:482-518
net/crates/net/go/mesh_rpc.go:2209-2243
net/crates/net/go/mesh_rpc.go:2262-2285
net/crates/net/go/mesh_rpc.go:2321-2340
net/crates/net/go/org.go:814-868
net/crates/net/go/compute_dispatch.go:365-375
```

The uncovered daemon and observer trampolines violate that established package policy.

### Impact

A peer-shaped input can trigger whole-process termination, taking down unrelated daemons, RPC services, and in-process state.

### Required inverse witnesses

Run each callback in a child-process integration test with a deliberately panicking implementation. At minimum:

```go
func (d *Daemon) Process(event Event) ([]Event, error) {
    _ = event.Payload[0]
    return nil, nil
}
```

Deliver an empty peer-controlled payload. Current code must demonstrate child-process termination. Repaired code must:

1. map the panic to the callback's documented error/default;
2. leave all output pointers in a valid initialized state;
3. remain alive for a second callback in the same process.

The observer witness should prove that a panic drops only that observation and increments a panic/drop metric.

### Minimal repair boundary

Install a top-level deferred `recover` around the entire body of every affected trampoline, including handle lookup and payload conversion. Initialize output pointers before invoking user code. Convert panic to the callback-specific failure/default. Do not rely on Rust `catch_unwind` for a foreign Go panic.

## FFI-02 — MeshOS teardown can invalidate `cgo.Handle` before an admitted callback uses it

**Severity:** High availability  
**CWE:** CWE-362, Race Condition  
**Confidence:** High; source-proven interleaving  
**Status:** Confirmed interleaving; deterministic barrier witness required

### Violated invariant

C/Rust may retain and use a `cgo.Handle` token until every callback carrying that token has completed. Free must either quiesce callbacks or defer deletion until the final callback reference is gone.

### Exact interleaving

1. `DaemonRegistry::deliver` clones the host `Arc`, locks it, and passes `guard_identity`:

   ```text
   net/crates/net/src/adapter/net/compute/registry.rs:297-302
   ```

2. Concurrent handle destruction drops `MeshOsDaemonHandle`. Its `Drop` unregisters without waiting for the host mutex; registry removal explicitly permits in-flight `Arc` clones to continue:

   ```text
   net/crates/net/src/adapter/net/meshos/sdk.rs:547-567
   net/crates/net/src/adapter/net/compute/registry.rs:182-186
   net/crates/net/src/adapter/net/compute/registry.rs:213-237
   ```

3. `net_meshos_handle_free` returns after dropping only the owning handle:

   ```text
   net/crates/net/bindings/go/meshos-ffi/src/lib.rs:923-933
   ```

4. The Go free closure immediately calls `cgoHandle.Delete()`:

   ```text
   net/crates/net/go/meshos.go:511-516
   net/crates/net/go/meshos.go:634-652
   ```

5. The already-admitted callback can then call `h.Value()`:

   ```text
   net/crates/net/go/meshos.go:758-760
   ```

6. Go treats use of a deleted handle as an invalid-handle panic:

   ```text
   Go runtime/cgo/handle.go:124-131
   ```

The existing `streamHandleGuard` counts Go→Rust handle operations only. Rust→Go callbacks do not enter it, so it cannot quiesce this race.

### Prerequisites and impact

Explicit `Free` or GC finalization races an in-flight MeshOS callback. Remote event traffic can enlarge the window. The invalid-handle panic occurs at an uncontained cgo boundary and can terminate the process. This composes directly with FFI-01.

### Required deterministic witness

Add test-only barriers:

1. after Rust `guard_identity` admits the callback;
2. in `meshosHandleFromCtx` immediately before `Value()`.

Start delivery, wait until both Rust admission and Go trampoline entry are established, invoke `Free`, then release the second barrier. Current code must reach invalid-handle misuse. Repaired code must either finish safely or return a typed closing result.

A hookless stress witness may race repeated delivery against explicit `Free`, dropped final references, and forced `runtime.GC()`, but the deterministic two-barrier witness is the acceptance oracle.

### Minimal repair boundary

Prefer coupling `cgo.Handle.Delete` to actual `CDaemonBridge` destruction after the final in-flight host `Arc` is gone, for example through a vtable destructor callback. Alternatively implement a callback-entry/closing protocol that:

- acquires an in-flight reference before `Value()`;
- rejects new callback entries after close begins;
- waits for all callbacks to leave before deleting the handle.

Extending only the existing Go→Rust guard is insufficient.

## Latent robustness issue — Compute `C.GoBytes` narrowing

**Classification:** Local robustness inconsistency; not a live remote vulnerability.

Narrowing remains at:

```text
net/crates/net/go/compute_dispatch.go:253,337
net/crates/net/go/compute.go:529,666
```

`DaemonRuntime.Deliver` accepts arbitrary same-process Go bytes and can reach the conversion through:

```text
net/crates/net/go/compute.go:623-650
net/crates/net/bindings/go/compute-ffi/src/lib.rs:1855-1899
```

A local caller capable of allocating at least 2 GiB can reach `size_t`→`C.int` narrowing, and user daemon code can return oversized snapshots/outputs. That caller already controls executable code and process memory.

Remote event framing remains approximately 8 KiB, and migration reassembly is capped at 64 MiB:

```text
net/crates/net/src/adapter/net/compute/orchestrator.rs:674-707
```

Replace the inconsistent conversions with `goBytesChecked` for parity, but do not describe this as remotely exploitable without a new path exceeding `MaxInt32`.

## Ruled out

- Unary and streaming RPC and Org handlers already recover user panics.
- RPC/Org registry deletion is memory-safe because `sync.Map.Load` retains the handler interface for an entered callback.
- Streaming wrappers are invalidated before Rust frees callback-frame handles.
- Compute and MeshOS emitters copy Go bytes into Rust-owned `Bytes` before return.
- Compute snapshots pair `C.malloc` with Rust `libc::free` and handle non-null/zero-length and oversized Rust-slice cases.
- MeshDB, Deck, and MCP use pull/poll/owned-handle APIs rather than Rust→Go callbacks in the audited FFI crates.
- No memory-corruption primitive was established.

## Verification performed

Successful nonzero tests:

```text
cargo test --manifest-path net/crates/net/bindings/go/meshos-ffi/Cargo.toml
9 passed

cargo test --manifest-path net/crates/net/bindings/go/compute-ffi/Cargo.toml
3 passed

cargo test --manifest-path net/crates/net/bindings/go/rpc-ffi/Cargo.toml runtime_entry -- --nocapture
5 passed, 40 filtered

cargo test --manifest-path net/crates/net/Cargo.toml --lib test_unregister -- --nocapture
2 passed, 5694 filtered
```

A Go test command failed to compile because the host configuration omitted cgo-defined package surfaces and is not evidence. One broad Rust integration build encountered a Rust 1.97.1 compiler ICE; focused library runs were used instead.

## Acceptance gate

This report remains HOLD until:

- deterministic child-process panic witnesses exist for all affected callback classes;
- the MeshOS two-barrier teardown witness fails on the audited implementation;
- repairs are additive and bounded to callback containment/lifetime ownership;
- positive callbacks still work after panic recovery and after orderly close;
- focused Go/cgo and Rust FFI tests pass with nonzero counts;
- the exact repair head has clean Git state, `git diff --check`, and green required CI.
