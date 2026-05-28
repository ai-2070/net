# Security audit — `net` crate (2026-05-28)

Branch: `master`.
Scope: full-surface security pass over the `net` crate (~285k LOC Rust + Go/Python/TypeScript bindings). Attack surfaces audited: wire-protocol parsing, crypto primitives, the C-ABI FFI boundary, identity/token/auth, on-disk storage (RedEX + Dataforts blob), and the client SDKs.

Findings are organised by severity. File paths are relative to repo root; line numbers reflect `master` at audit time and may drift. The codebase is unusually hardened — most classic traps already carry explicit guards and regression tests (it references prior "Cubic AI" findings and FFI handle-quiescing audits). The findings below cluster where code diverges from the project's own established safety protocols.

| ID | Severity | Area | One-line |
|----|----------|------|----------|
| H1 | High | FFI | `MeshBlobAdapterHandle` has no `HandleGuard` → use-after-free / double-free on concurrent `_free` |
| H2 | High | Go binding | Inbound peer length truncated `size_t → C.int` → remote DoS / payload desync |
| H3 | High | Identity | `PermissionToken` TTL is unbounded and revocation is advisory-only |
| M1 | Medium | FFI | Registry/fold-query/register-channel + blob-adapter ctor deref the node without `try_enter` |
| M2 | Medium | Identity | Clock skew is unbounded — widens every token's validity window |
| L1 | Low | FFI | Only `NetHandle` checks pointer alignment; blob-adapter accessors lack `catch_unwind` |
| L2 | Low | Identity | `WriteToken` is unsigned by design (footgun if a consumer trusts `origin_hash`) |
| L3 | Low | Storage | Blob `fetch`/`exists` read path doesn't re-canonicalize (TOCTOU symlink swap) |

---

## HIGH

### H1 — FFI blob-adapter handle has no `HandleGuard`: use-after-free / double-free
`net/crates/net/src/ffi/blob.rs:886` (struct), `:1037` (`net_mesh_blob_adapter_free`), ops at `:1083` (`_store`), `:1121` (`_fetch`), `:1155` (`_exists`).

`MeshBlobAdapterHandle` carries only the inner Arc, with **no** embedded `HandleGuard`:

```rust
pub struct MeshBlobAdapterHandle {
    inner: ManuallyDrop<Arc<InnerMeshBlobAdapter>>,
}
```

`net_mesh_blob_adapter_free` does an unconditional `Box::from_raw` + `ManuallyDrop::drop`:

```rust
pub unsafe extern "C" fn net_mesh_blob_adapter_free(handle: *mut MeshBlobAdapterHandle) {
    if handle.is_null() { return; }
    let mut boxed = unsafe { Box::from_raw(handle) };   // deallocates the box
    unsafe { ManuallyDrop::drop(&mut boxed.inner) };
}
```

The ops only null-check, then deref + clone the inner:

```rust
let adapter = unsafe { (*handle).inner.clone() };
let result = block_on(async move { (*adapter).fetch(&blob_ref).await });
```

This directly contradicts the per-handle quiescing protocol the codebase documents for exactly this hazard — `net/crates/net/src/ffi/handle_guard.rs:9-45`:

> Each handle struct embeds [a `HandleGuard`] inline; every `extern "C"` op gates on `HandleGuard::try_enter`; every `_free` drives `HandleGuard::begin_free`. … **never deallocate the handle box once it has been handed to C.**

Every other mesh/cortex/redis handle follows this. `MeshBlobAdapterHandle` was left out, and its `_free` performs the exact `Box::from_raw` deallocation the module says must never happen.

- **Trigger**: a Go cgo / Python / Node thread inside `_store`/`_fetch`/`_exists` while another thread calls `_free` on the same handle. Thread A reads `(*handle).inner` after thread B's `Box::from_raw` has deallocated the box → use-after-free. A second `_free` is a double-free.
- **Impact**: memory corruption / crash. Reachable for any multithreaded foreign caller that shares the adapter handle (the documented fan-out model).
- **Fix**: embed `HandleGuard` in `MeshBlobAdapterHandle`; gate every op on `try_enter`; have `_free` drive `begin_free` and leak the box (drop only the inner), matching `cortex` / `mesh` / `redis` handles.

### H2 — Go binding truncates inbound peer length `size_t → C.int`
`net/crates/net/bindings/go/net/mesh_rpc.go:479` and `:2255`; `net/crates/net/bindings/go/net/meshos.go:860` and `:921`.

Inbound nRPC request bodies and MeshOS causal-event / snapshot-restore payloads copy from a native buffer using a 32-bit signed length cast:

```go
req := C.GoBytes(unsafe.Pointer(reqPtr), C.int(reqLen))   // reqLen is C.size_t (64-bit)
```

`reqLen`/`payloadLen` originate from an inbound mesh message. Casting `C.size_t` → `C.int`:
- a length with bit 31 set becomes **negative** → `C.GoBytes` panics (`cgo argument has negative length`);
- a length ≥ 4 GiB **mod 2³²** yields a **short copy** → the handler sees a truncated body whose framing claims more (parse-desync / partial-read primitive).

The codebase already knows this hazard and guards it correctly in one place — `meshdb.go:659-679` checks `uint64(length) > uint64(math.MaxInt)` and uses `unsafe.Slice`. The inbound trampolines were not updated. The `recover` in `safeCallHandler` wraps only the user handler, **not** the `C.GoBytes` call that runs before it — so the panic is unrecovered and crashes the process.

- **Trigger**: a peer sends an nRPC request (or MeshOS causal event) whose body length crosses the 32-bit boundary.
- **Impact**: remote, peer-reachable process crash (DoS), or silent payload truncation feeding malformed data to the handler.
- **Fix**: route all four sites through a guarded helper (reject `> math.MaxInt`, then `unsafe.Slice` + `bytes.Clone`), as `meshdb.go` already does.

### H3 — `PermissionToken` TTL is unbounded; revocation is advisory-only
`net/crates/net/src/adapter/net/identity/token.rs:271` (`try_issue` / `issue`).

```rust
let not_after = now.saturating_add(duration_secs);
```

`duration_secs` is accepted up to `u64::MAX` with no max-TTL clamp anywhere in this layer — there is even a regression test (`issue_with_huge_ttl_saturates_rather_than_panics`, ~`token.rs:2246`) asserting a never-expiring token is acceptable. The only revocation mechanism is the per-issuer `RevocationRegistry` floor (`token.rs:686`), which must be distributed out-of-band and is advisory in `TokenCache::check`.

- **Trigger**: any entity that can mint or delegate a token issues an effectively immortal credential.
- **Impact**: a leaked or over-scoped long-TTL token cannot be expired and is hard to revoke on a node that never learns to bump the floor — long-lived credential-compromise / replay vector. Exploitation requires being a valid issuer, but the blast radius is real.
- **Fix**: enforce a hard max-TTL clamp at issue time; consider making revocation-floor distribution mandatory rather than advisory for high-scope tokens.

---

## MEDIUM

### M1 — FFI handles deref the mesh/redex node without `try_enter`
`net/crates/net/src/ffi/aggregator.rs:149` (`net_registry_client_new`), `:417` (`net_fold_query_client_new`), `:448` (`net_register_channel`); `net/crates/net/src/ffi/blob.rs:1019` (`net_mesh_blob_adapter_new`).

```rust
let mesh_arc = unsafe { super::mesh::mesh_node_arc(&*mesh_handle) };  // after only is_null()
```

These deref the inner node after only a null check, with no `try_enter` gate. A concurrent `net_mesh_free` that wins its `begin_free` race and takes `inner` out leaves these reading a dropped `ManuallyDrop`. `net_mesh_blob_adapter_new` (`blob.rs:1019`, `(*redex).redex_arc()`) has the analogous gap against `RedexHandle` — it skips the `guard.try_enter()` that every other `net_redex_*` op uses.

- **Impact**: use-after-free read of the inner Arc → crash. Same class as H1 but narrower: these are constructor/registration calls usually made before the handle is shared.
- **Fix**: gate each on the relevant handle's `try_enter` (the guard is already available).

### M2 — Clock skew is unbounded; widens every token's validity window
`net/crates/net/src/adapter/net/identity/token.rs:335` (`is_valid_with_skew`), `:732` / `:759` (`with_clock_skew` / `set_clock_skew`).

```rust
// rejects only when now >= not_after.saturating_add(skew_secs)
```

`with_clock_skew` / `set_clock_skew` accept any `u64` with no ceiling. A large skew symmetrically widens every token's validity window — an expired token stays accepted for `skew` extra seconds. The default is strict (0), so this is misconfiguration-gated, not default-on, but there is no guardrail.

- **Impact**: expired-token replay window proportional to a misconfigured skew.
- **Fix**: clamp skew to a small maximum (e.g. a few minutes).

---

## LOW / informational

### L1 — Inconsistent FFI pointer-alignment + `catch_unwind` discipline
`net/crates/net/src/ffi/mod.rs:384` gates `NetHandle` with an `is_multiple_of(align_of::<NetHandle>())` alignment check; every other handle (e.g. `mesh.rs:609`, `cortex.rs:365`, `blob.rs:1041`) does `is_null()` only. A foreign caller passing a misaligned non-null pointer produces immediate UB on `&*handle`. Theoretical for correct callers, but the documented "valid + aligned" contract is enforced in exactly one place. Separately, the blob-adapter metric/config accessors (`blob.rs:1187`, `:1210`, `:1231`, `:1253`) lack the `catch_unwind` their `net_blob_publish`/`resolve` siblings carry — a panic there would unwind across `extern "C"`.

### L2 — `WriteToken` is unsigned by design
`net/crates/net/src/adapter/net/redex/write_token.rs:42-63`. `WriteToken { origin_hash, seq }` is plain public data; `FromStr` parses any `<16-hex>:<u64>`. The module documents that authenticity rests on the adapter's `wait_for_token` `WrongOrigin` check, not the token. This is a footgun, not a reachable bypass in audited code — flagged so reviewers confirm no consumer trusts a `WriteToken`'s `origin_hash` without routing through an origin-bound adapter.

### L3 — Blob read path does not re-canonicalize (TOCTOU symlink swap)
`net/crates/net/src/adapter/net/dataforts/blob/fs.rs:71-81` (module doc acknowledges this). `store` canonicalizes and rejects escapes (`fs.rs:223-231`, with a regression test), but `fetch`/`exists` do not re-run the check, leaving a post-canonicalize symlink-swap window. Reachable only by an attacker with local FS write access inside `root` (same trust level needed to corrupt the bytes directly), and mitigated on reads by BLAKE3 hash verification. Marginal impact.

---

## Audited and found clean

- **Crypto** (`src/adapter/net/crypto.rs`): Noise `NKpsk0`, ChaCha20-Poly1305 with counter nonces, sliding-window replay protection. Carefully reasoned (prologue binds `(src,dest)` node ids; nonce prefix folds hi^lo; replay window caps forward jumps to avoid bitmap-zeroing; `u64::MAX` counter rejected to prevent receive-path poisoning). Extensive regression tests.
- **Wire protocol** (`src/adapter/net/protocol.rs`, `transport.rs`): header parse bounds-checks `data.len() < HEADER_SIZE` and validates `payload_len`/`event_count` ceilings; `EventFrame::read_events` caps pre-allocation to what the buffer can hold; `NackPayload::from_bytes` rejects trailing bytes; AAD authenticates all header fields except the nonce and the (mutable) hop_count.
- **RX decrypt path** (`src/adapter/net/mesh.rs:3267-3398`, `:3741-3785`): AEAD verify precedes replay-counter admission; replays rejected at commit; routed-packet slices are bounds-guarded by `data.len() >= ROUTING_HEADER_SIZE + HEADER_SIZE` before slicing; the wire nonce prefix is ignored on RX (receiver uses its own session prefix), so tampering it is inert.
- **Identity/auth** (`identity/token.rs`, `envelope.rs`, `entity.rs`, `subnet/assignment.rs`): ed25519 `verify_strict` (rejects malleability); envelope `open` verifies attestation before AEAD, binds transcript to target pubkey + chain link, cross-checks decrypted entity id; `delegate` re-verifies the parent and intersects scope; subnet capability lookup is exact-match (no prefix escalation); no key-id/algorithm field is trusted from the message.
- **Storage** (`redex/disk.rs`, `segment.rs`, `entry.rs`, Dataforts blob): `ChannelName` validation blocks path traversal (`.`/`..`/`/`/NUL rejected at construction); length prefixes bounded against buffer/file size; `offset+len` widened to u64 with `saturating_add` + bounds-checks; checksums recomputed and enforced (corrupt records dropped); `net-blob get --out` uses `create_new(true)`.
- **SDKs** (`sdk-py`, `sdk-ts`, Rust `sdk`): overwhelmingly thin FFI/napi wrappers. No command injection, no `pickle`/`yaml.load`/`eval`/`Function()`, no prototype pollution, no SSRF/TLS-disable, no secrets logged. Wire-header predicate decode in TS is bounds-checked; pattern matching is substring containment (no ReDoS). The one real SDK issue is H2 (Go cgo).
