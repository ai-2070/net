# CODE REVIEW 2026-07-22 — Organization Capability SDK / language bindings (`org-capability-sdk`)

> **Status: REVIEW COMPLETE — findings OPEN, no remediation performed.**
>
> This is a first review pass of `org-capability-sdk` at head `20572959b`,
> diffed against `master` at `07820a9de` (merge-base `07820a9de`): 63 files,
> ~14,897 insertions, 33 commits. The branch is the **language-SDK / bindings**
> layer over the closed OA admission substrate — the verb facade (`mesh.org()`
> → `org.call` / `serve_org`), plus Go/C, Node, and Python bindings, a
> cross-language error-vocabulary contract, and a live cross-org scenario
> generator. It is distinct from the two prior `org-capability-auth` reviews
> (`CODE_REVIEW_2026_07_20_ORG_CAPABILITY_AUTH.md` and its `..._07_21_..._PASS2`),
> which covered the admission core.
>
> **The security-critical substance holds.** The authority decision is
> transport-blind, discovery is not authority, a remote denial can never be read
> as a local refusal (or vice versa), the coarse denial byte leaks no credential
> oracle, the node-keyed audience lease is race-safe, and no FFI
> use-after-free / double-free was found. The previously-recorded two serve-path
> bugs (serve-without-a-runtime; provider-invisible) are fixed and present. See
> the [Verified-clean register](#verified-clean-register).
>
> **Every finding below lives in the binding / build periphery, not the auth
> core.** Two are HIGH and reachable from ordinary use of the shipped bindings
> (a Go-FFI resource leak on error paths; the Node `org` feature missing from
> the package's own build scripts). One MEDIUM (Node handler timeout does not
> bound async work). The rest are LOW / informational.
>
> **Nothing here has been remediated or independently re-checked.** Line numbers
> are as of `20572959b`.
>
> ---
>
> ## Findings summary
>
> | § | Sev | Area | Finding | Disposition |
> |---|---|---|---|---|
> | §1 | **High** | Go FFI | `mesh_arc` (`Box<Arc<MeshNode>>`) leaked on input-validation early returns | OPEN |
> | §2 | **High** | Node build | `org` feature enabled in no package script; committed test hard-fails locally | OPEN |
> | §3 | **Med** | Node napi | `handlerTimeoutMs` does not bound async handler execution (stage-2 unbounded) | OPEN |
> | §4 | Low | Rust core | Windows secret loader validates by path, not the open handle (TOCTOU) | OPEN |
> | §5 | Low | Rust fixtures | Scenario generator: chmod-after-create race, unscrubbed key buffer, ships in release | OPEN |
> | §6 | Low | Node napi | `org:closed` / `org:serve_failed` misclassify as `Unclassified` (`isLocal=false`) | OPEN |
> | §7 | Low | Python | Blocking file I/O + signature verification under the GIL | OPEN |
> | §8 | Low | Python | `OrgUnclassifiedError` exported but never raised by native code | OPEN |
> | §9 | Info | Python | Handler timeout can't cancel the blocking call (shared w/ vetted `PyRpcHandler`) | OPEN |
> | §10 | Info | Go FFI | `from_raw_parts` array counts not bounds-checked vs `isize::MAX` (raw-C only) | OPEN |
> | §11 | Info | Python | Provisioning errors bypass `org:` vocabulary; `serve_org` doesn't validate callable; surface reachability | OPEN |

---

## Methodology

One reviewer drove the security-critical core personally — the adapter
enforcement (`mesh.rs` leases, `org_authority.rs` secret loader, the discovery
queries), the SDK `call` / `serve` / `client` / `error` orchestration, and the
FFI identity-provenance path — and delegated the four binding/credential
surfaces to focused subagents (Go+C FFI; Node+napi; Python+PyO3;
credentials/error/lease/provision/types). Every HIGH and MEDIUM was
re-verified against the actual code before inclusion here. "Verified" below
means read against the source at `20572959b`, not merely reported.

---

## §1 — HIGH — Go FFI leaks a whole `Arc<MeshNode>` on input-validation error paths

**File:** `net/crates/net/bindings/go/org-ffi/src/lib.rs` (verified in
`net_org_install_provider_grant_audience`, lib.rs:1032–1040); same shape in
`net_org_serve`, `net_org_install_authority`, `net_org_bind`.

`mesh_arc` is a heap `Box<Arc<MeshNode>>` minted per-call by
`net_mesh_arc_clone`, and the FFI contract (header + Go side) documents it as
**consumed** — "a fresh clone per call; Go must NOT free it" (lib.rs:1013). The
consuming line is `let node = *Box::from_raw(mesh_arc);`, but in four functions
it sits *after* input validation that can `return` first. On those paths the
`Box` — and its strong `Arc<MeshNode>` reference — is neither consumed nor
freed: a permanent leak of an entire node (sockets, runtime, tasks never drop).

Consumption lines and the early returns that precede them:
- `net_org_serve` — consumes @ lib.rs:892; leaks on non-UTF-8/empty service (:867), no-dispatcher (:874), `handler_id==0` (:881), invalid access mode (:888).
- `net_org_install_authority` — consumes @ :999; leaks on non-UTF-8 `dir` (:997).
- `net_org_install_provider_grant_audience` — consumes @ :1040; leaks on NULL/empty grant bytes (:1034), non-UTF-8 secret path (:1038).
- `net_org_bind` — consumes @ :699; leaks on NULL credentials slot (:690, raw-C only).

**Failure scenario (reachable from safe Go):**
`InstallProviderGrantAudience(node, nil, path)` → `bytesToCBytes` yields
`{nil, 0}` → `copy_bytes_required(null, 0)` returns `None` → returns
`NET_ORG_ERR_NULL` at lib.rs:1034, before the consume at 1040 → one
`Arc<MeshNode>` pinned forever. `ServeOrgBytes(node, "", access, h)` (empty
service) and an out-of-range `access` do the same. A retry-on-error loop leaks
unboundedly and defeats `node.Shutdown()`.

**Fix:** hoist `let node = *Box::from_raw(mesh_arc);` to immediately after the
`mesh_arc.is_null()` guard in all four functions, so any later validation
failure drops the owned `node` naturally. This mirrors the correct sibling
`net_rpc_new` (rpc-ffi/src/lib.rs:688–696).

---

## §2 — HIGH — Node `org` feature is enabled in none of the package's own build/test scripts

**Files:** `net/crates/net/bindings/node/package.json:89–91`;
`net/crates/net/bindings/node/Cargo.toml` default set `:33–51`, `org` def `:188`.

`org` is in neither the `default` feature list nor the `build` / `build:debug`
/ `build:test` scripts, and none pass `--no-default-features`. So any `.node`
produced by a package script contains **no** org symbols — `OrgClient`,
`OrgCredentials`, `serveOrg`, `installOrgAuthority`,
`installProviderGrantAudience`, `OrgAccess` are all `undefined` on
`require('./index')`. The `@net-mesh/core/org` entry advertised in `exports`
(package.json:16–19) is dead for anyone building from these scripts.

**Failure scenario:** the documented dev loop `npm run build:test && npm test`
builds a no-org module. The committed `test/org_binding.test.ts` has **no**
skip guard — it imports `OrgCredentials` and calls `OrgCredentials.create(...)`
unconditionally (contrast `org_live.test.ts:38,83`, which gates on `HAS_ORG`
via `describe.skipIf`). So `OrgCredentials.create(...)` is a method call on
`undefined` → `TypeError` → `classifyOrgError` passes it through →
`expect(...).toBeInstanceOf(OrgCredentialsError)` fails. Only CI masks the gap
by hand-rolling `napi build --no-default-features --features …,org,test-helpers`
(ci.yml:679, :747), so CI is green while the package's own scripts are wrong.

**Fix:** add `org` to `default` (and/or to the three `build*` feature args) so
the package scripts, the committed unguarded test, and CI agree.

---

## §3 — MEDIUM — Node `handlerTimeoutMs` does not bound async handler execution

**File:** `net/crates/net/bindings/node/src/org.rs:461` (stage-1 timeout) vs
`:482` (stage-2 `promise.await`, unbounded); doc claims "Both are bounded" at
`:421–424`.

`handler_timeout_ms` wraps only **stage 1** — waiting for the JS callback to
*return* the Promise (`tokio::time::timeout(timeout, rx)`, line 461). **Stage
2** — awaiting that Promise to resolve (`promise.await`, line 482) — has no
timeout. Because org handlers are always async (the typed wrapper wraps in
`async`, and `serveOrg`'s signature is `Promise<Buffer>`), stage 1 returns
essentially instantly and all real latency lives in stage 2, so
`handlerTimeoutMs` is effectively a no-op for actual handler work.

**Failure scenario:** a handler returning a never-resolving promise
(`() => new Promise(() => {})`, or an awaited resource that hangs) wedges the
request indefinitely — the caller never gets a reply and a worker on the shared
`org_serve_runtime` is held forever. The "Both are bounded" comment is
factually wrong.

**Fix:** wrap stage 2 in a timeout, e.g. `tokio::time::timeout(remaining,
promise)`. The streaming handlers in `mesh_rpc.rs:1456/1538/1630` already do
this via `timeout_at(deadline, promise)`. (The org unary path mirrors a
pre-existing gap in `mesh_rpc.rs`'s unary handler at line 355, but org's
comment overclaims where mesh_rpc's stays silent.)

---

## §4 — LOW — Windows secret loader validates by path, not by the open handle

**File:** `net/crates/net/src/adapter/net/behavior/org_authority.rs` —
`load_grant_audience_secret` → `validate_audience_file_acl(path)` (:2505).

On Unix the loader validates `file.metadata()` on the already-open descriptor
(TOCTOU-safe, exactly as the function's own doc-comment prescribes: "validate
the file's OWN descriptor"). On Windows it instead calls
`validate_audience_file_acl(path)`, re-resolving the *path* between
`open_regular_nofollow(path)` and the subsequent `read_exact`. An attacker able
to swap the file in that window would have the ACL validated against a
different object than the one read.

Inherited (the helper predates this branch; `read_audience_checked` uses it
too) and within the branch's documented Windows threat boundary ("operators
placing grant secrets outside a protected directory on Windows must manage the
ancestor chain out of band"). Flagged because the new loader adopts it while
its own prose claims the stronger open-handle guarantee. Align the Windows
check with the Unix path (validate the descriptor) if the platform API allows.

---

## §5 — LOW — Scenario/fixture generator: create-then-chmod race, unscrubbed key buffer, ships in release

**File:** `net/crates/net/sdk/src/org/fixtures.rs`; exported unconditionally via
`#[doc(hidden)] pub mod fixtures;` and `pub use fixtures::{...}` in
`net/crates/net/sdk/src/org.rs`.

Three related gaps, all LOW because this is doc-hidden scaffolding operating on
freshly-minted throwaway keys (used by the `gen_org_scenario` example and the
Rust `live_cross_org_call_from_a_generated_scenario` test):

1. **Create-then-chmod race** — `write_secret_file` (:319–331) does
   `File::create` (umask-dependent, typically 0644) → `write_all` →
   `set_permissions(0o600)`. There is a brief window where the audience secret
   is group/other-readable. Create with mode `0o600` up front:
   `OpenOptions::new().mode(0o600).create_new(true)`.
2. **Unscrubbed key buffer** — `secret_bytes = secret.encode_config()` (:399),
   written at :411 and :415, is never scrubbed, contrary to `encode_config`'s
   §28 "scrub the returned buffer" obligation. Raw discovery-key bytes linger
   in heap after the function returns.
3. **Ships in the production surface** — the module is exported without a
   `#[cfg(test)]` / feature gate, so the generator is part of the release
   library. Gate it if the intent is test-only.

---

## §6 — LOW — Node `org:closed` / `org:serve_failed` misclassify as `OrgUnclassifiedError` (`isLocal=false`)

**Files:** minted at `net/crates/net/bindings/node/src/org.rs:166,178,188`
(`org:closed`) and `:395` (`org:serve_failed`); classified by
`errors.ts:287–317`.

These use the `org:` prefix but a domain token (`closed`, `serve_failed`)
outside the four known domains, so `classifyOrgError` hits `default` →
`OrgUnclassifiedError` (domain `'unknown'`, `isLocal === false`). Calling a
closed client (`TypedOrgClient.call` after `close()`) is a purely local
lifecycle error, but surfaces as the vocabulary-mismatch class whose `isLocal`
is false — implying the request may have reached a provider, which mis-steers
retry/audit logic.

**Fix:** either drop the `org:` prefix for these local strings (mirroring
provisioning errors, which deliberately surface as plain non-`org:` errors per
`sdk/src/org/provision.rs:16–19`), or give them a real local domain so
`isLocal` is true. Contrast the well-formed `org:credentials:already_consumed`
(org.rs:140), which classifies correctly.

---

## §7 — LOW — Python blocking file I/O and signature verification under the GIL

**Files:** `net/crates/net/bindings/python/src/org.rs:99–118`
(`OrgCredentials.__init__` → `from_parts`, opens/validates secret files +
verifies signatures); `src/org_serve.rs:200–218` (`install_org_authority`,
`install_provider_grant_audience` open the authority dir / secret file).

These run synchronously while holding the GIL — no `py.detach` — whereas the
rest of the crate releases the GIL around blocking work (`a2a.rs`, `blob.rs`,
`aggregator.rs`). A multi-threaded Python app stalls other threads during these
calls. They are one-time startup/setup ops on small files, so impact is minor,
but inconsistent with the crate convention. Wrap the blocking body in
`py.detach(|| …)` (paths/bytes are already owned — mechanical change).

---

## §8 — LOW — Python `OrgUnclassifiedError` is exported but never raised by native code

**File:** `net/crates/net/bindings/python/src/org.rs:98` (`org_err_to_py`).

`org_err_to_py` maps `OrgErrorDomain::Unclassified => OrgUnclassifiedError`,
but `OrgSdkError::domain()` (sdk/src/org/error.rs:153–159) never returns
`Unclassified` — Rust says so explicitly ("Never produced by Rust"). So the
arm is dead and the exception — registered in `lib.rs`, `__all__`, and the stub
— can never fire. A user writing `except OrgUnclassifiedError` to catch ABI
drift catches nothing; the only "unclassified" mechanism is the pure-Python
`parse_org_error(...).domain == "unknown"`, which returns a value rather than
raising. Document as reserved, remove from the public surface, or wire a native
path that raises it.

---

## §9 — INFO — Python handler timeout can't cancel the blocking call

**File:** `net/crates/net/bindings/python/src/org_serve.rs:150–175`.

`tokio::time::timeout` wraps a `tokio::task::spawn_blocking`, which is not
cancellable. On timeout, `run_py_org_handler` returns
`OrgHandlerError::Internal("did not respond within N ms")`, but the Python
handler keeps running on the blocking-pool thread until it returns on its own.
Enough stuck handlers exhaust tokio's blocking pool (default 512) and stall all
serving. **Not an org-specific regression** — identical to the vetted
`PyRpcHandler` (mesh_rpc.rs:586–623); recorded for completeness, same
severity/behavior as the reference.

---

## §10 — INFO — Go FFI `from_raw_parts` array counts not bounds-checked vs `isize::MAX`

**File:** `net/crates/net/bindings/go/org-ffi/src/lib.rs:576–577, 595`.

`net_org_credentials_new` does `std::slice::from_raw_parts(grant_ptrs,
grant_count)` / `(audience_secret_paths, audience_secret_count)` without
checking `count * size_of::<ptr>() <= isize::MAX` (the individual buffer
helpers check their own lengths, but not these outer array lengths). Only
reachable via a raw C caller passing an absurd count — Go bounds it via
`len(...)`. Add the guard for parity with the `len > isize::MAX` checks
elsewhere.

---

## §11 — INFO — Python surface consistency

**File:** `net/crates/net/bindings/python/src/org_serve.rs`, `org.py`,
`__init__.py`.

- `install_org_authority` / `install_provider_grant_audience` map failures to
  `PyRuntimeError` (org_serve.rs:204, 217), so they don't carry the `org:` wire
  vocabulary and won't classify through `parse_org_error`. Arguably fine
  (operator-setup, not a call-time domain), but inconsistent with the module.
  The test only asserts `pytest.raises(Exception)`, so it doesn't pin the type.
- `serve_org` does not validate that `handler` is callable at registration; a
  non-callable is only detected per-request inside `run_py_org_handler`,
  surfacing as an Application error (`0x8001`) rather than failing fast.
- `TypedOrgClient`, `serve_org_typed`, `parse_org_error`, `ParsedOrgError` are
  reachable only via `net.org.*`, not re-exported at `net.*` / `__init__.__all__`
  — confirm this matches the Node/Go binding discoverability contract.
- Stub looseness (nit): `_net.pyi` types `serve_org`'s `handler`/`mesh` as
  `Any`; the `handler_timeout_ms=0` → "effectively infinite" convention
  (org_serve.rs:145) isn't documented in the stub.

---

## Verified-clean register

Re-checkable properties confirmed against the source at `20572959b`. Listed so
a re-reviewer can spot-check rather than re-derive.

- **Authority is transport-blind.** `call.rs::authorized_targets` decides the
  invocable set purely from credentials + discovery planes; reachability
  (`plan()`) is a separate stage — no transport state can promote an
  unauthorized provider or demote an authorized one. Deterministic selection by
  lowest `EntityId`.
- **Discovery is not authority.** `granted_capability_providers` /
  `owner_private_capability_providers` return candidates; every returned
  provider still admits only on a valid per-call proof.
- **Denial ↔ success cannot invert, coarse denial stays coarse.**
  `AdmissionDenied` is produced only on wire status `0x0009` (`call.rs::map_rpc_error`);
  `OrgSdkError::to_wire()` emits `org:admission_denied:<bucket>` with no detail
  (no credential oracle, OA2-E2); `parse_org_wire` maps any unfamiliar string to
  `Unclassified` and never impersonates a domain; `is_local()` is conservatively
  `false` for `Unclassified`. The coarse byte (0/1/2) round-trips through the
  lossy `String::from_utf8` message path; an undecodable body falls back to
  `Denied` (a denial stays a denial). Success bypasses `map_rpc_error` entirely.
- **Node-keyed consumer-audience lease is race-safe.** `OrgAudienceLeases` +
  `install_seq` (mesh.rs); `acquire_consumer_audience_leases` rolls back every
  reference taken so far on partial failure; the lock spans read+install and the
  1→0 removal; lock ordering (`entries` → `consumer_grant_mu`) is consistent, no
  deadlock; `install_seq` starts at 1 so no lease collides with a seq-0 record;
  `remove_consumer_grant_audience_if_current` compares seq under the mutex so a
  remove-then-install can't slip a different record in.
- **Identity provenance set in all three constructors.** `configured_identity`
  in the SDK builder (`mesh.rs`), the napi/PyO3 node config, and the C FFI
  `net_mesh_new` (`ffi/mesh.rs`, witnessed by
  `net_mesh_new_records_identity_provenance`). Closes the gap that refused
  seeded callers in Node/Python.
- **The two prior serve-path bugs are fixed.** Serve-without-a-runtime (each
  binding now enters a runtime for registration) and provider-invisible
  (`install_org_authority_node` enables owner-cert emission) — both present in
  the diff (`8bf457234`).
- **FFI memory safety.** No UAF / double-free: double-pointer NULL-on-free
  (`credentials_free`, `client_free`, `serve_handle_free`) paired with Go
  `atomic.Bool`/mutex guards; matched `Box<[u8]>` alloc/free for responses;
  `catch_unwind` on every entry point; offset-tested `NetOrgCaller` layout (160
  bytes, header↔Rust mirror test).
- **Node/Python lifecycle.** `ArcSwapOption` snapshot-before-use prevents a
  `close()` from tearing the node/lease out of an in-flight call (both
  bindings); one-shot credential consumption is thread-safe; the Python network
  round-trip releases the GIL (`py.detach` around `block_on`); napi TSFN usage
  matches the vetted `mesh_rpc.rs` pattern; no floating promises / unhandled
  rejections.
- **SDK credentials/error core clean.** No secret in any `Debug`/`Display` or
  error message; `OrgCredentials` is neither `Clone` nor `Serialize` (compile
  guard); `OrgAudienceSecret` redacts + zeroizes on drop; secrets cross FFI as
  paths only, loaded via the hardened `load_grant_audience_secret`; no
  `unwrap`/`expect`/`panic` on caller- or remote-controlled input; hex helpers
  infallible.

---

## Recommendation

Merge-ready on the authorization core. Gate release on §1 (Go arc leak), §2
(Node feature/build gap), and §3 (Node handler timeout) — all three are
reachable from ordinary use of the shipped bindings. §4–§11 are good
follow-ups and none blocks correctness of the auth path.
