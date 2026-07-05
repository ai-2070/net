# Code review — `mcp-sdk` branch (consent/pins → net-mesh-sdk + language bindings)

**Date:** 2026-07-05
**Branch:** `mcp-sdk`
**Base:** `master`
**Scope:** 54 files, +7,150 / −1,298 LOC.
**Plans:** [`MCP_BRIDGE_SDK_PLAN.md`](../plans/MCP_BRIDGE_SDK_PLAN.md)

The branch graduates the local **consent** vocabulary (`CapabilityId`,
`CredentialStatus`, `ConsentPolicy`) and the persistent **pin store** out of the
`net-mesh-mcp` adapter into `net-mesh-sdk` (`sdk/src/consent.rs`,
`sdk/src/pins.rs`), so there is one implementation and the adapter + bindings
consume it (bridge doctrine #1: no logic in adapters or bindings). On top of the
move it adds:

- **Language bindings** for the consent/pin surface + the pure `classify` /
  `lower_tool` helpers: Python (PyO3, `bindings/python/src/{consent,mcp_helpers}.rs`),
  Node (napi-rs, `bindings/node/src/{consent,mcp_helpers}.rs`), and a Go/C-ABI
  cdylib (`bindings/go/mcp-ffi/src/lib.rs` + `go/mcp.go` + `include/net_mcp.h`).
- The `serve/*` and `wrap/credentials.rs` adapter modules reduced to
  `pub use net_sdk::…` re-exports.
- A large rewrite of `wrap/session.rs` (the `ServerPublisher` / shared-publisher
  merge model) and cross-language golden-vector conformance tests.

> **Status — OPEN (2026-07-05).** Findings below are unfixed at the time of
> review. File/line anchors point at the code **as reviewed** (`master...HEAD`
> HEAD). F1–F5 are all in the Go/cgo path, which per the branch notes is
> **Linux-CI-only** (the cdylib can't build on the Windows dev box), so it is the
> least-exercised surface on the branch — verify these on Linux CI.

---

## Overall assessment

The **graduation itself is clean**. The move to the SDK is verbatim (compile-time
`identity` fns in `adapters/mcp/tests/dependency_boundary.rs` prove every
re-export *is* the SDK type — no forked reimplementation), and the security-core
invariants carried over intact. Verified as correct (not findings):

- **Consent is fail-closed and the trust boundary holds.** An empty
  `ConsentPolicy` gates *every* discovered capability; `CredentialStatus::from_wire`
  (`sdk/src/consent.rs:170`) maps a wire-declared `"none"` — like anything
  unrecognised — to the gated `Unknown`, never the ungated `None`. The
  supply-side-only trusted parse `from_label` (`consent.rs:188`) is used *only* on
  locally-produced labels in the three `lower_*` helpers; the demand-side
  `requires_consent` shims and the FFI consent gate all use `from_wire`.
- **Classifier accuracy is irrelevant to the gate.** `classify`
  (`wrap/credentials.rs`) floors at `Unknown` (gated); the ungated `None` is
  reachable *only* via `CredentialOverride::NoCredentials` gated on `force`, i.e.
  an explicit operator downgrade. A missed secret var can only over-gate.
- **Pin-store integrity preserved.** `PinStore::save` is atomic (per-pid temp +
  rename, `0o600` from creation on Unix), `load` is fail-closed on a parse error
  (`Corrupt`, never a silent reset), and every read-modify-write goes through the
  locked `mutate` transaction on a sidecar `.lock` OFD.
- **Node and Python bindings are disciplined thin delegations.** No re-implemented
  consent/pin/credential logic; correct GIL release (`py.detach` around
  `block_on`) and napi async; error mapping via stable `consent:` / `pins:`
  prefixes; `.pyi` stub matches the Rust signatures; the exported JS surface
  matches the test contracts.
- **ABI parity is one-for-one.** `include/net_mcp.h` matches the `extern "C"`
  signatures and the `go/mcp.go` cgo block (arg count, const-ness, `int` vs
  `char*` return, NULL/ownership contracts); `CString::into_raw` ↔
  `net_mcp_free_string` reclaim is paired; every `char*`/`int`-returning entry
  point is `catch_unwind`-guarded; the golden-vector canonicalization + wire-`none`
  gating agree across Rust/Python/Node/Go.

The defects therefore concentrate in the **Go binding** — the FFI/concurrency
glue — rather than in the graduated core.

Method: 10 independent finder angles (line-by-line, removed-behavior, cross-file
tracer, language-pitfall, wrapper/proxy, reuse, simplification, efficiency,
altitude, CLAUDE.md conventions) over the `master...HEAD` diff, then each
surviving candidate verified by reading the implicated code path directly
(`go/mcp.go`, `mcp-ffi/src/lib.rs`, `wrap/session.rs`, `python/src/consent.rs`).

---

## Findings

### F1 (High) — cgo thread-migration corrupts the thread-local last-error channel

`go/mcp.go:95` (`lastMcpError`); `mcp-ffi/src/lib.rs:63` (`thread_local!`),
`:95`/`:105`/`:114` (getters/clear).

The FFI records the last-error message + kind in a Rust `thread_local!` and every
entry point clears it at the top; the detail is drained by
`net_mcp_last_error_message` / `_kind` on **the calling thread**. But `lastMcpError()`
reads and clears that thread-local via *three separate* cgo calls with **no
`runtime.LockOSThread`**:

```go
if p := C.net_mcp_last_error_message(); p != nil { … }
if p := C.net_mcp_last_error_kind(); p != nil { … }
C.net_mcp_clear_last_error()
```

Between the failing FFI call and this drain (and between the three calls), the Go
scheduler may migrate the goroutine to a different OS thread — the intervening
Go function calls and the `&McpError{}` allocation are async-preemption / GC
points. The getters then read a *different* thread's slot.

**Failure scenario:** `C.net_mcp_classify(...)` fails on OS thread T and sets the
error on T. The goroutine is rescheduled onto T′ before `lastMcpError()` runs its
getter calls. T′'s slot is either empty (every entry point `clear_last_error`s, so
detail is lost → `nil` error — see **F2**) or holds a *stale* error from an
unrelated op that last ran on T′ (wrong message/kind returned). The trailing
`net_mcp_clear_last_error()` then wipes T′'s slot, destroying a pending error a
*different* goroutine was about to drain. This is a Go-specific hazard the
Python/Node bindings don't have (1:1 thread / GIL). Independently surfaced by the
Go-client and FFI-safety angles.

**Fix direction:** don't rely on a thread-local across separate cgo calls — either
`runtime.LockOSThread()`/`UnlockOSThread()` spanning the FFI call *and* its error
drain, or (better) return the error detail through out-params on the failing call
itself so no second thread-scoped call is needed.

### F2 (High) — `ConsentPolicy.RequiresApproval` fails OPEN on an empty-error decide failure

`go/mcp.go:332` (`RequiresApproval`), `:319` (`Decide`).

```go
func (p *ConsentPolicy) Decide(capID, credentialStatus string) (string, error) {
    s, ok := takeString(C.net_mcp_consent_policy_decide(p.ptr, cCap, cStatus))
    if !ok { return "", lastMcpError() }   // NULL → ("", err)
    return s, nil
}
func (p *ConsentPolicy) RequiresApproval(capID, credentialStatus string) (bool, error) {
    decision, err := p.Decide(capID, credentialStatus)
    if err != nil { return false, err }
    return decision == "requires_approval", nil   // "" != "requires_approval" → false
}
```

When `net_mcp_consent_policy_decide` returns NULL, `takeString` yields `("", false)`
and `Decide` returns `("", lastMcpError())`. If `lastMcpError()` returns `nil`
(empty thread-local slot — reachable via **F1**), `Decide` returns `("", nil)` and
`RequiresApproval` returns `(false, nil)` — i.e. **"does not require approval" =
allowed** — for a capability that should be gated.

I confirmed the FFI side is defensive on the same thread: `decide` `set_last_error`s
on *every* NULL path (null handle `lib.rs:588`, bad cap id / status `:592`, parse
failure `:598`), so absent the thread race it fails *closed*. It is F1's cross-thread
read that opens the gate. But the mapping is independently fragile: a security gate
must never treat a missing decision as "allowed".

**Failure scenario:** a peer advertises a malformed capability id (non-numeric
provider, empty half). `RequiresApproval(id, status)` → `decide` returns NULL +
sets an `invalid_arg` error on T; the goroutine migrates; the drain reads T′'s
empty slot → `nil`; `RequiresApproval` returns `(false, nil)` → the caller invokes
an untrusted, unparseable capability as if allowed.

**Fix direction:** default-deny — treat any non-`"allowed"` / empty / error result
as `requires_approval` (`return decision != "allowed", err`, and never return a
`nil` error alongside an empty decision). Independently, F1's fix removes the
empty-slot trigger.

### F3 (High) — Use-after-free: `ConsentPolicy` methods omit `runtime.KeepAlive`

`go/mcp.go:258` (finalizer), `:274` / `:305` / `:319` / `:337` (cgo call sites).

`NewConsentPolicy` registers `runtime.SetFinalizer(p, (*ConsentPolicy).Close)`, and
`Close` frees the Rust `Box` (`net_mcp_consent_policy_free` →
`drop(Box::from_raw(policy))`, `lib.rs:477`). The methods pass `p.ptr` into cgo but
never call `runtime.KeepAlive(p)` afterward. When `p`'s only liveness is the
method receiver, Go's liveness analysis can mark it dead immediately after the
`p.ptr` load; a GC during the in-flight C call runs the finalizer and frees the
handle while the C code is still dereferencing it (`(*policy).inner`, `lib.rs:601`).

**Failure scenario:** `NewConsentPolicy() → p.Decide(id, s)` where `p` is not
retained past the call. Mid-`C.net_mcp_consent_policy_decide`, `p` becomes
unreachable → finalizer fires on the GC goroutine → `Box` freed → the C call reads
freed memory (heap corruption / crash). GC-timing-dependent, so intermittent.

**Fix direction:** add `runtime.KeepAlive(p)` after every cgo call that dereferences
`p.ptr` (in `mutate`, `IsPinned`, `Decide`, `Pinned`). This is the canonical
`SetFinalizer` contract from the `runtime` docs.

### F4 (High, concurrent use) — Go `ConsentPolicy` handle is not thread-safe: data race + double-free

`mcp-ffi/src/lib.rs:505` (`policy_mutate` → `f(&mut (*policy).inner, id)`);
`go/mcp.go:246` (struct), `:263` (`Close`).

`ConsentPolicyHandle` is a plain `Box<CoreConsentPolicy>` with **no interior
lock**; `allow`/`pin`/`unpin` take `&mut (*policy).inner`. The Go `ConsentPolicy`
is an ordinary struct with **no mutex** — unlike the Node binding, which
deliberately wraps the core policy in a `parking_lot::Mutex`
(`node/src/consent.rs:157`, "serialize through a mutex"). Go objects are
idiomatically shared across goroutines.

**Failure scenario:** two goroutines call `p.Allow(a)` and `p.Pin(b)` (or a mutator
racing a `Decide`/`IsPinned` read) on the same `*ConsentPolicy` → two live
`&mut`/`&` to the same `HashSet` at once → aliasing-`&mut` UB / heap corruption /
lost entry. Separately, two goroutines calling `p.Close()` both observe
`p.ptr != nil` (non-atomic check-then-free at `:264-266`) → `net_mcp_consent_policy_free`
runs twice → double `drop(Box::from_raw)` despite the doc calling `Close`
"Idempotent". (The explicit-`Close` vs finalizer path is safe — `Close` clears the
finalizer — only *concurrent* `Close` double-frees.)

**Fix direction:** give the Go `ConsentPolicy` a `sync.Mutex` guarding `ptr` (matching
the Node binding's posture), or document the handle as single-goroutine-owned and
make `Close` idempotent under races (e.g. atomically swap `ptr` to nil before free).

### F5 (Low, correctness) — wrap `refresh()` mutates shared state with no rollback on failure

`wrap/session.rs:474-507` (`ServerPublisher::refresh`); contrast
`publish_server` rollback at `:331-370`.

On a `tools/list_changed` notification, `refresh()`:

1. swaps this publication's contribution into the **shared** `contributions` map
   (`shared.insert(...)`, `:474`),
2. re-announces the merged union (`shared.sync_mesh(...)?`, `:481`),
3. *then* serves newly-appeared tools (`serve_rpc(...)?`, `:499-503`) and withdraws
   vanished ones.

Steps 2 and 3 can fail via `?` **after** the shared map and the mesh announcement
have already been mutated, and there is **no rollback** — unlike `publish_server`,
which removes its contribution and re-syncs on any failure.

**Failure scenario:** co-located publications A and B share the node's mesh. B's
wrapped server emits `tools/list_changed` adding a tool whose channel-safe
`tool_id` collides with one A already serves. `refresh()` swaps B's contribution and
announces the merged union (now advertising + describing B's new tool under B's
scope), then `serve_rpc(tool_id)` fails ("already registered") and returns `Err` at
`:503` with no revert. The CLI loop (`cli/commands/wrap.rs:214`) logs "refresh
failed" and keeps serving. The mesh now advertises/describes the tool under B's
scope, but every invoke routes to A's handler — a silent cross-publication
misroute — and because the stale entry lives in the *shared* map, any sibling's
next publish/refresh/withdraw re-announces it. A transient `sync_mesh` announce
error at `:481` is the same shape: vanished tools stay served, never-served tools
get propagated by a later sibling re-announce.

**Fix direction:** mirror `publish_server` — capture the prior contribution before
`insert`, and on any `?` failure after `:474` restore it and re-sync the merged
union before returning the error.

### F6 (Medium, altitude/reuse) — wire-vocabulary enum↔string tables hand-written in all three bindings

`node/src/consent.rs:68` & `:218`; `python/src/consent.rs:76` & `:206`;
`go/mcp-ffi/src/lib.rs:203` & `:601`; plus `go/mcp.go:332`/`:481` literal compares;
and `credential_override` at `node/mcp_helpers.rs:80` / `python/mcp_helpers.rs:45` /
`go/mcp-ffi:282`; `substitutability` at the same three helpers.

`CredentialStatus` correctly exposes `as_str()` on the SDK enum and every binding
reuses it — good altitude. But `ConsentDecision` (`"allowed"` / `"requires_approval"`),
`PinState` (`"pending"` / `"approved"`), `CredentialOverride`
(`"detect"`/`"credentialed"`/`"no-credentials"`), and `Substitutability` have **no
SDK string method**, so each binding hand-writes the table — three (or four) times.
Every binding's own doc-comment says the decision string is "the SDK enum's stable
string form — never re-derive it" while literally re-deriving it. This is exactly
the per-binding drift the graduation (doctrine #1) exists to eliminate.

**Cost / failure scenario:** renaming a variant or adding a decision/state means
editing three hand-written matches (plus Go string-compares no compiler links
together, e.g. `decision == "requires_approval"` at `go/mcp.go:332`); a typo in one
binding passes that binding's own golden-vector test while breaking cross-binding
byte-equality. `PinState` even already derives `#[serde(rename_all="snake_case")]`
for the same spellings — a fourth, independent definition of the wire form.

**Fix direction:** add `ConsentDecision::as_str()` and `PinState::as_str()` to
`net_sdk::consent`/`pins` (next to `CredentialStatus::as_str`), and
`CredentialOverride::from_wire`/`Substitutability::from_label` on the adapter enums;
have every binding delegate.

### F7 (Medium, extreme concurrency) — pin-store `block_on` can deadlock via blocking-pool exhaustion

`mcp-ffi/src/lib.rs:181` (`runtime()` — `new_multi_thread`), `:664` (and the other
pin fns) `runtime().block_on(PinStore::mutate(...))`; `sdk/src/pins.rs:84`
(`spawn_blocking { lock_exclusive() }`).

Every pin mutation `block_on`s the shared multi-threaded runtime. `PinStore::mutate`
acquires the cross-process flock via `spawn_blocking`, which **blocks a blocking-pool
thread** until the OS grants the exclusive lock; the lock holder's subsequent
`load()`/`save()` use `tokio::fs`, which *also* draws blocking-pool threads. Tokio's
default `max_blocking_threads` is 512.

**Failure scenario:** with ≥512 concurrent contending mutations on the *same* store
file, all 512 blocking threads sit blocked on `lock_exclusive()`; the current holder
cannot get a blocking thread to run its `load`/`save`, so it never completes and
never releases the lock → permanent hang of all pin operations. Needs extreme
fan-out on one local file, but it is a latent scalability trap for a high-throughput
embedding.

**Fix direction:** don't hold a blocking-pool thread parked on `lock_exclusive()`
for the lock's whole duration — use `try_lock` with async backoff, or a dedicated
thread/semaphore for lock acquisition separate from the fs pool, so the holder's I/O
can always make progress. (Same `mutate` is reachable from Node/Python runtimes; the
shared FFI runtime is the most exposed.)

### F8 (Medium, efficiency) — `PublisherShared::merged()` rebuilds the whole catalog on every single-publication change

`wrap/session.rs:~183` (`merged`), reached from `sync_mesh` on every
publish/refresh/withdraw.

`merged()` deep-clones every `LoweredTool` (descriptor + metadata) of *every*
co-located publication into a throwaway `all` Vec to concatenate for
`build_capability_set`, then `build_catalog` re-parses each tool's JSON schema — for
*unchanged* publications too. So a single publication's `tools/list_changed`
refresh does O(all tools across all publications) clone + schema-parse work.

**Cost:** wasted CPU + allocation that scales with total co-located tool count on
every mutation; matters on a node hosting many wrapped servers (the many-node
target).

**Fix direction:** have `build_capability_set` take `impl Iterator<Item = &LoweredTool>`
to drop the `all` clone, and/or cache each contribution's built catalog part so only
the mutated publication is rebuilt.

### F9 (Low, docs) — Python `credential_requires_consent` docstring inverts its return value

`python/src/consent.rs:143`.

The docstring reads: *"a wire `\"none\"` is NOT trusted (it gates like `\"unknown\"`),
so this returns `False` for no wire value at all"* — but the body
`CredentialStatus::from_wire(status).requires_consent()` returns **`True`** for
`""`/`"none"`/unknown (the golden vector `consent_vectors.json` and Node's twin doc,
"returns `true`", confirm `True` is correct). Runtime behavior is safe and
test-verified; only the doc is wrong — and it's wrong on a security gate in the
fail-open direction. A Python integrator reading `help(credential_requires_consent)`
is told an absent/unknown status does *not* require consent and could write
`if not credential_requires_consent(status): allow()`.

**Fix direction:** one word — the docstring should say it returns `True` for an
absent/unrecognised wire value (over-gates, never bypasses). The `.pyi` stub is
already correct.

---

## Minor notes (not ranked findings)

- **`opt_cstr` swallows invalid UTF-8** (`mcp-ffi/src/lib.rs:161`): a non-UTF-8
  optional arg becomes `None` (vs the required `cstr`, which reports an
  `invalid_arg` error). For `args_json`/`envs_json` this silently drops to empty,
  and `credential_override`/`substitutability` fall to their safe defaults
  (`Detect`/`ProviderLocal`). **Not** a consent bypass — `classify` still floors at
  `Unknown` (gated) — but the inconsistency is worth aligning (error, don't swallow),
  since the C ABI is callable by non-Go consumers that may pass raw bytes.
- **`cstr`/`opt_cstr` return `&'a str` with an unbounded, caller-chosen lifetime**
  (`mcp-ffi/src/lib.rs:139`, `:156`): borrows the C buffer with a lifetime the
  pointer's real validity doesn't constrain. Not triggerable by current callers
  (all convert to owned before returning), but a future FFI fn could hold the `&str`
  past the call → UAF with no compiler error. Consider returning an owned `String`,
  or a lifetime tied to the pointer.
- **`wrap.rs` shutdown gated on `Arc::try_unwrap(mesh)`** (`cli/commands/wrap.rs:251`):
  the old code owned `mesh` and always awaited `shutdown()`; now a lingering
  `Arc<Mesh>` clone would silently skip the graceful path with no diagnostic. Low
  confidence — the `Arc` is created and consumed locally, so `try_unwrap` should
  succeed today — but a future clone (or an SDK-internal retained task) would
  regress it silently. Consider logging when `try_unwrap` fails.

---

## Priority

Fix before the Go binding is relied on: **F2** (fail-open consent gate) and **F3**
(use-after-free) are the two to block on; **F1** is their shared root and
independently corrupts all Go-side error reporting; **F4** matters as soon as a
`ConsentPolicy` is shared across goroutines. **F5** is a real correctness gap in the
wrap refresh path. **F6/F8** are altitude/efficiency; **F7/F9** and the minors are
lower-stakes. F1–F4 need Linux CI to exercise (the cdylib doesn't build on the
Windows dev box).
