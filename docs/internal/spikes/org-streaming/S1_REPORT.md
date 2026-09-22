# Stage 1 report — core protected server-streaming

**Brief:** `spikes/org-streaming/S1_BRIEF.md`, pinned at `096f54009`.
**Base:** `096f54009` (Stage 0 accepted at `736469448`; packet
`S0_REVIEW_PACKET.md`). **Authorized scope:** ledger C1–C9 only; additive to
the unary gate; no Stage 2+.

Lanes append their numbered sections below (`## 1. S1Session — slice 1.1`,
`## 2. S1Core — slices 1.2–1.6`); the coordinator appends integration and
review-verification subsections. Every claim carries its executed vs
source-established label and its receipt per the brief's evidence rules.

## 0. Coordinator record (Main)

- **2026-09-22, S1.1 verified and the hold relayed.** `S1.1` landed at
  `0d4bbfb24` + `4d159e484` (wire lib tests 275 → 278; three witnesses with
  individual inverse receipts; wasm32 check green). `mesh.rs` released and
  `S1Core` unblocked for 1.2.
- **F2 ruling (contract 1 spelling).** `NetSession::with_binding(keys,
  handshake_hash, peer_addr, pool_size, default_reliable)` is **accepted as
  contract-1-conformant**. The brief's 2-arg spelling was a drafting error:
  `NetSession::new` already requires `peer_addr`/`pool_size`/`default_reliable`
  and the contract's intent (additive carriage; hand-built sessions stay
  `None`) never meant to remove them. The binding properties, not the arity,
  are the contract.
- **F1 assignment — slice 1.1a (priority, hard blocker).** S1.1's finding that
  the 11 real handshake-completion sites (`mesh.rs:23390,23869,28335,28415,
  28592,28635,47560,48054`; `adapter/net/mod.rs:889,1152,1459`) still use
  `into_session_keys` + `NetSession::new` is a contract hole: live sessions
  carry `binding: None`, so §1.3 would refuse every protected opening and no
  1.2+ fixture could pass. Assigned back to `S1Session` as **1.1a** with an
  exclusive `mesh.rs` handshake-site window (`S1Core` holds all `mesh.rs`
  writes — 1.4 included — until "F1 landed" is relayed). Witness + inverse
  required: a real completed handshake stores its binding and
  `peer_session_binding` returns `Some(the real hash)`.
- **F3 noted.** The brief's `net/crates/net/mesh.rs` path shorthand was a
  drafting error; the real path is `net/crates/net/src/adapter/net/mesh.rs`
  (both lanes resolved it correctly). Correction recorded; the pinned brief is
  not rewritten under a dispatched lane.
- **2026-09-22, ownership clarifications (the C3 rename's mechanical
  fallout).** Three rulings on files outside the brief's ownership table:
  (a) `tests/subnet_org_boundary.rs` — `S1Core`'s mechanical C3 migration
  (literal → stable constructor, identical values) is within the carve:
  names, assertions and counts byte-stable, floors untouched;
  (b) `guards/org_api_probe` — `S1Core` is authorized its C3/C4 named-break
  update **despite** the file's absence from the table: the brief's 1.3
  requires the probe updated in the same commit as the Q2 break. Doctrine
  held: no exhaustive-match arm deleted, MANIFEST discipline kept;
  (c) `org_routing_wiring_tests.rs` (+88, attributed by `S1Core` to
  `S1Session`'s F1 fallout) — bounded carve: mechanical signature adaptation
  only; this is a CI floor-pinned file (MIN=93 + REQUIRED names) so NO test
  may be deleted, renamed, weakened or re-pinned and floors stay Main's;
  `S1Core` is barred from touching it even for compilation fixes.
- **2026-09-22, disk hazard (F16-class) recurred and was resolved by the
  owner.** Transient ENOSPC during `S1Core`'s 1.2 build (rustc rmeta write);
  the lane reclaimed `target/debug/incremental` (3.7 GB, regenerable) and
  retried correctly; the owner then freed the volume (73 GB free at
  confirmation). Coordinator integrity sweep of all 10 in-flight files showed
  sane deltas — no truncation. Both lanes carry the verify-before-trust rule
  for the pressure window.
- **2026-09-22, ruling — the 1.1a witness may live in the floor-pinned file
  (Option A, blessed).** `S1Session` STOPped correctly at the carve: the 1.1a
  witness (a real routed-handshake completion storing its binding) needs
  `handle_routed_handshake`/`dispatch_ctx` — mesh-module-private, reachable
  only from the module tree. Ruling: the +1 new test lands in
  `org_routing_wiring_tests.rs`; floor discipline permits addition by
  construction (MIN=93 is a minimum, the REQUIRED-names roster is unaffected,
  and floor raises are Main's at CI-pin time) and the prohibition binds
  deletion/rename/weakening only. Conditions: the 4 mechanical None-binding
  slots stay minimum-adaptation with names/assertions byte-stable and are
  named as mechanical in the lane report; the one-line fix to the lane's OWN
  new assertion is blessed; the 1.1a receipt (inverse at a migrated handshake
  site reddening the new witness) still lands as assigned. Option C's blocker
  is recorded as a finding for Stage 3+ API review: no public getter exists
  for the node's own static X25519 key (`peer_static_x25519` is peers-only),
  so a public-API handshake witness would require a new API and still would
  not cover the routed F1 sites.
- **2026-09-22, S1.1 coordinator spot-check (executed).** Receipt R-a
  reproduced independently: the prescribed widening inverse at
  `wire/src/crypto.rs:437` → red at `wire/src/session.rs:3443:9`
  byte-identical to the lane's quote (left = widened session id + 24 zeros /
  right = full transcript hash), restore proven byte-identical against
  `0d4bbfb24` (`git diff --quiet`), restored run green (278-test wire suite
  unaffected). Slice 1.1 accepted at coordinator level pending stage-end
  validation; slice 1.1a (F1 migration) in flight.

## 1. S1Session — slice 1.1

**Landed (executed):** `0d4bbfb24` — `S1.1: retain the full Noise handshake
hash as the session binding` (4 files, +208/−29) on `LZL0/org-streaming`; this
record rides in the following `S1.1:` commit. Base `096f54009` (the pinned
brief's commit). Files touched, per the lane's exclusive set: `wire/src/crypto.rs`,
`wire/src/session.rs`, `src/adapter/net/mesh.rs` (accessor region only),
`docs/TRANSPORT.md`. `SessionKeys` is byte-unchanged (Q7/C2 additive path).

### What landed

Source-established (line numbers pristine-at-`0d4bbfb24`):

- `NoiseHandshake::into_session_keys_with_binding(self) -> Result<(SessionKeys,
  [u8; 32]), CryptoError>` (`wire/src/crypto.rs:352`) — the full 32-byte
  handshake hash is returned beside the keys before `into_transport_mode`
  consumes the state; `NoiseHandshake::into_session_keys` (`crypto.rs:451-454`)
  is a compatible wrapper that drops the binding. `handshake_hash()`'s rationale
  comment updated for the new projection (`crypto.rs:283-294`).
- `NetSession.handshake_binding: Option<[u8; 32]>` (`wire/src/session.rs:75`);
  `NetSession::with_binding(keys, handshake_hash, peer_addr, pool_size,
  default_reliable)` (`session.rs:291-301`) stores the hash verbatim;
  `NetSession::handshake_binding() -> Option<[u8; 32]>` (`session.rs:460-462`);
  `NetSession::new` keeps its signature and initializes `handshake_binding:
  None` (`session.rs:254`). No existing call site was migrated or taught to
  fabricate bindings (finding F1).
- `MeshNode::peer_session_binding(node_id: u64) -> Option<[u8; 32]>`
  (`src/adapter/net/mesh.rs:19627-19637`), directly beside `peer_session_id`
  (`:19623`) and reading `NetSession::handshake_binding` of the live
  incarnation.
- `docs/TRANSPORT.md:49-96` — the `SessionKeys` block updated to its real six
  fields, a session-binding paragraph added (binding vs. the 8-byte session
  *name*; `None` fail-closed semantics; re-handshake ⇒ different binding), the
  `NetSession` block updated to real fields including `handshake_binding`, and
  the `SessionManager` text refreshed (it was stale: the old block showed
  `tx_cipher: Mutex<PacketCipher>`, `pool: SharedPacketPool`, `origin_hash:
  u32`, none of which exist on `NetSession` today).

### Witnesses and counts (executed)

Wire in-file units in `wire/src/session.rs` (`session::tests`), named in the
plan's acceptance language. Wire lib tests **275 → 278** (3 added, 0 removed,
0 renamed — source count from the executed rosters: every filtered run reports
`3 run / 275 skipped`, the full suite `278 run`).

| # | Witness (pristine) | Property proved |
|---|---|---|
| (a) | `the_stored_binding_is_the_full_independently_captured_noise_handshake_hash` (`session.rs:3428`) | the stored binding equals the **full** Noise handshake hash captured independently from the transcript (`NoiseHandshake::handshake_hash()` read **before** finalization — not the finalizer's own return), asserted on both sides against their captured values, not merely peer-vs-peer |
| (b) | `a_re_handshake_yields_a_different_binding` (`session.rs:3456`) | a second fresh handshake between the same peer identities stores a **different** binding |
| (c) | `a_hand_built_session_carries_no_binding` (`session.rs:3476`) | a `NetSession::new` hand-built session is observably `None` and the accessor reports it faithfully (the "never admits" half is slice 1.2's witness; no admission built here) |

Green (executed), run from `net/crates/net/wire/`:

- `cargo nextest run -p net-mesh-wire --no-tests=fail --retries 0` →
  `Summary [  11.577s] 278 tests run: 278 passed, 0 skipped`, **exit 0** (all
  three witnesses appear in the roster by name).
- Focused three-witness command (below): `Summary 3 tests run: 3 passed, 275
  skipped`, **exit 0**.

Wasm32 cleanliness (executed): `cargo check -p net-mesh-wire --target
wasm32-unknown-unknown` → `Finished \`dev\` profile … in 18.40s`, **exit 0** —
wire remains tokio-free and wasm32-clean with the carriage (no new deps; the
additions are plain data and methods).

### Inverse receipts (executed; raw)

Every mutation is a bounded diff at a **production** site; line numbers in
panic quotes are observed-under-mutation with the mutation's line delta given
(pristine numbers in parentheses). Baseline shas, sha-gated restores
(`sha256sum` after every restore):

```
8e1f95e5bbdf15d8e65ed1fa162ee7b2a5999d24587a1ead3cce90e69580435e  src/crypto.rs   (88 124 B)
bb5c5db2ba0c28d2302ca0e7b3f9fddad3f6cb77b5e9929c946a7ead369a0bec  src/session.rs  (249 267 B)
```

Exact focused command (run from `net/crates/net/wire/`; `; echo "EXITCODE=$?"`
appended in every invocation below purely to capture the exit code):

```
cargo nextest run -p net-mesh-wire --no-tests=fail --retries 0 -E "test(=session::tests::the_stored_binding_is_the_full_independently_captured_noise_handshake_hash) + test(=session::tests::a_re_handshake_yields_a_different_binding) + test(=session::tests::a_hand_built_session_carries_no_binding)"
```

**R-a — the prescribed inverse (must redden witness (a)).** Bounded diff at
the production site — `wire/src/crypto.rs`, `into_session_keys_with_binding`'s
return (`:437`): return the 8-byte session id widened to 32 bytes in place of
the full hash:

```diff
             },
-            handshake_hash,
+            {
+                // INVERSE R-a: widen the 8-byte session id.
+                let mut widened = [0u8; 32];
+                widened[..8].copy_from_slice(&session_id.to_le_bytes());
+                widened
+            },
         ))
```

Command above. **Exit 100.** Verbatim failure:

```
thread 'session::tests::the_stored_binding_is_the_full_independently_captured_noise_handshake_hash' (164288) panicked at wire\src\session.rs:3443:9:
assertion `left == right` failed: the stored binding must be the full transcript hash, not a projection of it
  left: Some([203, 131, 122, 244, 11, 206, 174, 48, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])
 right: Some([203, 131, 122, 244, 11, 206, 174, 48, 177, 82, 9, 229, 65, 45, 114, 137, 0, 200, 139, 187, 112, 84, 217, 148, 5, 196, 237, 105, 197, 228, 174, 55])
...
Cancelling due to test failure
Summary [   0.141s] 3 tests run: 2 passed, 1 failed, 275 skipped
EXITCODE=100
```

(`:3443:9` = pristine (0 delta — the mutation is in `crypto.rs`).) Collateral:
(b) and (c) stay green under R-a — widened ids still differ across handshakes
and (c) is untouched; the discriminator is the transcript comparison, exactly
as designed. Restore: diff reversed; `sha256sum src/crypto.rs` =
`8e1f95e5…` == baseline. Restored green (same command): `Summary 3 tests run:
3 passed, 275 skipped`, **exit 0**.

**R-b — binding dropped at the storage site.** Bounded diff at the production
site — `wire/src/session.rs`, `NetSession::with_binding` (`:298-299`): store a
constant instead of the carried hash:

```diff
         let mut session = Self::new(keys, peer_addr, pool_size, default_reliable);
-        session.handshake_binding = Some(handshake_hash);
+        let _ = handshake_hash;
+        session.handshake_binding = Some([0u8; 32]); // INVERSE R-b: constant binding
         session
```

Command above. **Exit 100.** Verbatim failure (witness (b), the property under
test):

```
thread 'session::tests::a_re_handshake_yields_a_different_binding' (160892) panicked at wire\src\session.rs:3465:9:
assertion `left != right` failed: a fresh establishment must bind differently
  left: Some([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])
 right: Some([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])
```

(`:3465:9` observed under +1-line delta; pristine `:3464`.) Expected collateral
red, witness (a) (the mutation breaks both properties — listed, not hidden):

```
thread 'session::tests::the_stored_binding_is_the_full_independently_captured_noise_handshake_hash' (165464) panicked at wire\src\session.rs:3444:9:
assertion `left == right` failed: the stored binding must be the full transcript hash, not a projection of it
  left: Some([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])
 right: Some([224, 101, 5, 227, 112, 22, 97, 53, 92, 84, 106, 239, 62, 96, 154, 35, 192, 179, 119, 155, 228, 105, 222, 227, 41, 12, 157, 35, 88, 62, 160, 10])
...
Summary [   0.223s] 3 tests run: 1 passed, 2 failed, 275 skipped
EXITCODE=100
```

(`:3444:9` under the same +1 delta; pristine `:3443`.) (c) green. Restore:
diff reversed; `sha256sum src/session.rs` = `bb5c5db2…` == baseline. Restored
green (same command): `3 tests run: 3 passed, 275 skipped`, **exit 0**.

**R-c — hand-built sessions fabricated a binding.** Bounded diff at the
production site — `wire/src/session.rs`, `NetSession::new`'s field init
(`:254`):

```diff
-            handshake_binding: None,
+            handshake_binding: Some([0u8; 32]), // INVERSE R-c: fabricated binding
```

Command above. **Exit 100.** Verbatim failure (witness (c)):

```
thread 'session::tests::a_hand_built_session_carries_no_binding' (151964) panicked at wire\src\session.rs:3483:9:
assertion `left == right` failed: a session with no establishment binding must read as None, faithfully
  left: Some([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])
 right: None
...
Summary [   0.225s] 3 tests run: 2 passed, 1 failed, 275 skipped
EXITCODE=100
```

(`:3483:9` = pristine (0 delta — one line replaced by one line).) (a) and (b)
green under R-c (`with_binding` overwrites `new`'s initializer — clean
single-property discrimination). Restore: diff reversed; `sha256sum
src/session.rs` = `bb5c5db2…` == baseline. Restored green (same command): `3
tests run: 3 passed, 275 skipped`, **exit 0** (also R-b's restored-green run —
same invocation, recorded here once).

No witness stayed green under its own inverse; no weakening applies to any of
the three (each red is an assertion failure at the named property, not a
compile error).

### Findings (state, not decide)

**F1 — the real handshake-completion sites are unmigrated and unowned
(source-established).** Every production session is still built by
`into_session_keys()` + `NetSession::new(…)`:
`src/adapter/net/mesh.rs:23390,23869; 28335,28415,28592,28635; 47560,48054`;
`src/adapter/net/mod.rs:889,1152,1459` (plus test helpers
`org_routing_wiring_tests.rs:6999,7017`, `leaf/src/session.rs:231,302`,
`tests/rtc_browser/leaf/src/lib.rs:155`). Live sessions therefore carry
`handshake_binding == None` and `MeshNode::peer_session_binding` returns `None`
for every peer until those sites switch to `into_session_keys_with_binding` +
`NetSession::with_binding`. Under contract 1 such sessions can never admit a
protected stream, so the Stage 1 exit (live same-org/cross-org protected
server-streaming) requires this migration. Those sites are outside S1Session's
file ownership (`mesh.rs` accessor region only; `src/adapter/net/mod.rs` and
`leaf/**` are in no lane's table). Stated for Main to rule: who migrates them
and in which slice.

**F2 — contract 1's `with_binding(keys, handshake_hash)` spelling cannot build
a complete session (source-established).** `NetSession` additionally requires
`peer_addr`, `pool_size`, `default_reliable` (`wire/src/session.rs:231-236`);
a two-argument constructor would need a builder or partial construction,
neither of which is additive carriage. Implemented as `with_binding(keys,
handshake_hash, peer_addr, pool_size, default_reliable)` — the contract's pair
first, in contract order, followed by `new`'s three parameters unchanged. If
Main rules the literal two-argument spelling binding, that is a redesign to
rule on, not one this lane made unilaterally.

**F3 — brief path citation (source-established).** The brief and plan cite
`mesh.rs:19623`, and the brief's ownership row says `net/crates/net/mesh.rs` —
that file does not exist. The cited line matches exactly in
`net/crates/net/src/adapter/net/mesh.rs` (61 198 lines): `peer_session_id` at
`:19623`. Ownership was applied to that file's accessor region; noted so the
record's citations resolve.

### What never ran (complete)

- **Root-crate compile of the `peer_session_binding` accessor** (`net` crate) —
  never executed here. Mid-flight lane rules forbid project-wide builds (the
  sibling lane is editing other files of the same crate right now; a build
  would report phantom failures). The accessor is source-established by
  construction beside `peer_session_id` (`self.peers.get(&node_id)` →
  `p.session.handshake_binding()`); Main's stage-end `cargo check --workspace
  --all-targets` compiles it.
- **`cargo fmt` / clippy ×4 / rustdoc ×5** — never executed here (mid-flight
  rule: formatters and linters are Main's stage-end list). Code hand-conformed
  to `rustfmt.toml` (max_width 100, 4-space, Unix newlines).
- **`cargo tl`, `cargo t`, the preserved-witness list, the public streaming
  regression control** — Main's stage-end list, never executed here.
- **The wasm32 test target** (`wire/tests/wasm_wire.rs`, wasm-bindgen-test —
  `wasm-pack test --node` / wasm runner) — never executed here; only the
  wasm32 **check** ran (above).
- Windows workstation only: `#[cfg(unix)]` legs never compile here.

Rosters above are taken from the executed run outputs (the 278-line suite
roster and the filtered `3 run / 275 skipped` counts), not from intent.

### 1.1a — F1 migration: handshake completion carries the binding

**Landed (executed):** `cc3a1faa3` — `S1.1a: carry the session binding through
handshake completion` (3 files, +220/−69) on `LZL0/org-streaming`; this record
rides in the following `S1.1a:` commit. Scope: Main's F1 assignment (the 11
handshake-completion sites) under the explicit-and-bounded carve (`mod.rs`
fully; `mesh.rs` at the handshake sites with an exclusive window;
`org_routing_wiring_tests.rs` per the Option-A ruling below).
`tests/subnet_org_boundary.rs` is **S1Core's** C3 mechanical edit, not this
lane's (scope confirmed by Main).

**What landed** (line numbers pristine-at-`cc3a1faa3`; source-established +
executed via compile):

| Site (the 11) | Migration |
|---|---|
| `mesh.rs:28387` routed case 1 (msg2 for pending initiator) | closure now returns `(SessionKeys, [u8; 32])` via `into_session_keys_with_binding`; `PendingHandshake.tx` type follows (`:3887`) |
| `mesh.rs:28467` routed case 2 (msg1 responder) extraction | `into_session_keys_with_binding` |
| `mesh.rs:28644`, `:28688` routed case 2 install arms | `NetSession::with_binding(keys, handshake_hash, …)` |
| `mesh.rs:23409` `accept_rtc` extraction | `into_session_keys_with_binding` + `install_direct_fenced(…, Some(hash), …)` |
| `mesh.rs:47632` `handshake_initiator` | returns `(SessionKeys, [u8; 32])`; `connect`/`connect_rtc` pass `Some(hash)` |
| `mesh.rs:48126` `try_handshake_responder` | returns `(SessionKeys, [u8; 32], PeerAddr)`; `handshake_responder`/`accept` pass `Some(hash)` |
| `mod.rs:890` `perform_handshake` initiator arm | returns `(keys, handshake_hash, addr)` |
| `mod.rs:1153` `try_handshake_responder` (NetAdapter) | same tuple |
| `mod.rs:1460` `init` | `NetSession::with_binding` |

The install funnels (`install_direct`, `install_routed`,
`install_peer_transition(+inner/locked)`, `install_direct_fenced`) carry
`handshake_hash: Option<[u8; 32]>`; the central construction at
`install_peer_locked` (`mesh.rs:23906`) binds via `with_binding` when present
and keeps `NetSession::new` → `None` otherwise, so hand-built/test sessions
remain fail-closed exactly per contract 1 (the remaining `into_session_keys()`
callers are test key factories only — verified by source sweep).

**Witness + counts** (executed): `a_real_completed_handshake_stores_its_binding_and_peer_session_binding_returns_it`
(`org_routing_wiring_tests.rs:7055`, `#[tokio::test]`) drives a REAL routed
handshake end-to-end through the production path: a manual initiator mirroring
`try_connect_via_once` exactly (prologue convention, msg1 payload, packet
wrapping) against the production `handle_routed_handshake` Case 2 responder;
the production `msg2` is received over the wire, the initiator finishes its own
Noise state, and the expected binding is captured **independently** as the
initiator's final transcript hash (`handshake_hash()`, never through the
production finalizer). Asserts
`node.peer_session_binding(initiator_node_id) == Some(captured)` at `:7115`.
The msg2 send succeeding also disarms the install's rollback guard, so the
registration is durable, not raced. Net-mesh lib tests **5833 → 5834** (+1,
zero removed/renamed); `org_routing_wiring_tests` module **93 → 94** (the 93
floor-pinned tests byte-stable per the carve conditions below).

Green at this head (executed): `CARGO_INCREMENTAL=0 cargo tfl
org_routing_wiring_tests --retries 0` → `Summary [9.076s] 94 tests run: 94
passed, 5740 skipped`, **exit 0**.

**Inverse receipt R-1.1a** (raw; per Main's condition (c)):

Bounded diff at the **production** sites the witness drives — leave the routed
case-2 site on the OLD path (the pre-migration F1 shape), 3 hunks:
`mesh.rs:28467` extraction back to `let keys = match noise.into_session_keys()`,
and both install arms (`:28644`, `:28688`) back to `NetSession::new(…)` with
the `handshake_hash` argument removed.

Command (from `net/crates/net/`): `CARGO_INCREMENTAL=0 cargo tfl
a_real_completed_handshake_stores_its_binding_and_peer_session_binding_returns_it
--retries 0; echo "EXITCODE=$?"`. **Exit 100.** Verbatim failure:

```
thread 'adapter::net::mesh::org_routing_wiring_tests::a_real_completed_handshake_stores_its_binding_and_peer_session_binding_returns_it' (166248) panicked at src\adapter\net\org_routing_wiring_tests.rs:7107:5:
assertion `left == right` failed: the installed session must carry the full handshake hash of the establishment that created it, and peer_session_binding must return it
  left: None
 right: Some([146, 233, 143, 29, 139, 208, 141, 201, 150, 188, 6, 211, 129, 204, 215, 130, 161, 114, 88, 39, 210, 92, 189, 37, 4, 232, 132, 228, 179, 188, 61, 45])
...
Summary [   0.111s] 1 test run: 0 passed, 1 failed, 5833 skipped
EXITCODE=100
```

Numbering: `:7107:5` is **observed-under-mutation** on the pre-format tree;
the same `assert_eq!` is **`:7115:5` pristine-at-`cc3a1faa3`** (+8 pure
formatting delta — the rustfmt-conformance pass split calls at `:7080`/`:7092`
and above; both numbers quoted explicitly, per the numbering convention).

Restore: the 3 hunks reversed; `sha256sum mesh.rs` =
`b84a98904063f141e034467c164c2397d453d045c657522eec295af375db42b5` ==
the pre-mutation baseline (byte-identical). Restored green (same command):
`Summary [0.212s] 1 test run: 1 passed, 5833 skipped`, **exit 0**. No
green-under-inverse; the red is an assertion failure at the named property.

**Carve compliance** (Main's conditions (a)/(b), executed + source-established):
the four mechanical binding slots in the floor-pinned file are exactly
signature adaptation — `org_routing_wiring_tests.rs:7608`, the `let lost =
node.install_direct(…)` block at `:7621-7628`, `:8265`, `:8317-8318` — each a
`None` (no-binding) argument added to an `install_direct` call; every existing
test name and assertion byte-stable. The one-line fix to the NEW witness's own
assertion (msg2 sender compared to `node.local_addr()` instead of the test's
own socket — Main's condition (b)) is the only assertion this lane wrote in
that file. Floor `MIN=93` unaffected: 94 > 93, and the floor raise is Main's
at CI-pin time.

**Ruling recorded** (Main, 2026-09-22 — condition (d)): **Option A** blessed
the +1 witness in `org_routing_wiring_tests.rs` — the mesh module tree is the
only honest home for a witness that drives the production
`handle_routed_handshake`/`dispatch_ctx` seam (mesh-module-private); the
floor rules bind deletion/rename/weakening, which this addition does not do.

**Findings** (state, not decide):

**F4 — no public getter for a node's own static X25519 key (source-established;
Main-flagged for Stage 3+ API review).** The Option-C redesign (a public-API
witness in `mod.rs` driving `accept()`) is blocked because a manual initiator
cannot complete a real handshake without the responder's static key:
`MeshNode::peer_static_x25519` (`mesh.rs:19566`) exposes **peers'** keys only,
and `MeshNode::static_keypair` has no public accessor (`node_id()` `:14434`
and `local_addr()` `:20207` do exist). Any future external-consumer test of a
handshake, and the Stage 3+ probe story, will want a read-only
`static_pubkey()`-style accessor; that is an API addition = ruling territory.

**F5 — sibling fmt drift observed, untouched (executed observation).** The
scoped `rustfmt --check` reports pre-existing formatting drift in S1Core's
in-flight files: `behavior/org_admission.rs:511,547,591` and
`behavior/org_call.rs:669,1126` (multi-line match-arm and method-chain
shapes). Left untouched (sibling lane's files); named so the stage-end
`cargo fmt` pass knows exactly where drift lives.

**Verification hygiene** (executed): F16 discipline held through the ENOSPC
window — every file this lane wrote carries recorded size+sha256 (mesh.rs
`b84a9890…` → post-format `08c5f3cf…`; mod.rs `38af5581…` → `b1b8d8e4…`;
org_routing_wiring_tests.rs → post-format `4a6b290b…`), and a hunk-level
`git diff --numstat` audit against `0d4bbfb24` confirmed every change sits at
an expected migration site (no truncation, no out-of-band hunks). Two
build-infrastructure transients during the shared-disk window (an incremental
cache copy error, and one real E0061 arity error at a call site the AST pass
missed — fixed) — the former was the shared event Main broadcast, not a code
defect; all subsequent runs used `CARGO_INCREMENTAL=0`.

**What never ran** (complete):

- `cargo fmt -p <crate> -- --check`, clippy ×4, rustdoc ×5, `cargo check
  --workspace --all-targets`, `cargo tl`, `cargo t`, the preserved-witness
  list, the public streaming regression control — Main's stage-end list,
  never executed here. Formatting was converged with `rustfmt --check`/apply
  scoped to this lane's three files via `--config skip_children=true` (no
  submodule traversal; sibling files proven byte-untouched by sha/byte-form
  probes).
- Direct witness coverage: the witness exercises routed case 2 end-to-end
  (extraction `:28467`, arms `:28644`/`:28688`, peers install,
  `peer_session_binding`). Routed case 1 (`:28387`), `handshake_initiator`
  (`:47632`), `try_handshake_responder` (`:48126`) and the `mod.rs` trio are
  migrated on the same carriage shape and compile-verified (full lib-test
  surface + the `net,webrtc` check) but have no dedicated witness — stated,
  not inferred.
- `tests/subnet_org_boundary.rs` and `guards/org_api_probe` runs — S1Core's
  scope.
- Windows workstation only: `#[cfg(unix)]` legs.
