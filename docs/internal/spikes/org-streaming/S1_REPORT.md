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
- **2026-09-22, coordinator spot-check incident (self-inflicted, disclosed).**
  An attempted independent reproduction of `S1Session`'s R-1.1a receipt
  mutated `mesh.rs` **in the shared tree while `S1Core` was building it**. The
  mutation's `replace_all` caught only the AcceptRotation arm (the Vacant
  arm's block differs in whitespace), leaving a dangling `handshake_hash` at
  `mesh.rs:28689` and breaking the lib-test build (E0425) for the sibling
  lane — which correctly held per its no-touch directive and disclosed the
  break. Response: `git restore` to committed pristine
  (`MESH_RESTORED_PRISTINE_AT_HEAD`, `git diff --quiet` clean — the only
  uncommitted `mesh.rs` changes were the coordinator's), and full disclosure
  to `S1Core` as a coordinator-caused phantom, not a code defect. The
  attempted spot-check is **invalidated** — a compile error is never a red —
  so R-1.1a stands on the lane's raw receipt plus review round 2's
  independent re-execution in the reviewer's isolated worktree. **Process
  rule adopted:** coordinator production-site mutations run only in an
  isolated worktree or between lanes, never while a sibling builds the shared
  tree.
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

## 2. S1Core — slices 1.2–1.6

### 2.1 Slice 1.2 — streaming proof (contract 2)

**Landed (executed):** `0ed6ecd27` — `S1.2: add streaming call proofs and
shape-aware admission` (12 files, +3123/−134) on `LZL0/org-streaming`; this
record rides in the following `S1.2:` commit. Base `16b9f99e3` (after
S1Session's `S1.1`/`S1.1a` landings and the coordinator record). All green
claims below are at `0ed6ecd27`'s exact tree.

**What landed** (source-established; executed via the runs below):

- `behavior/org_call.rs` — `OrgStreamCallProof` (the five unary prefix fields
  in the same order, then `kind: u8` (1 SS / 2 CS / 3 DX; 0 never emitted),
  then `session_binding: [u8; 32]`; §1.1) with a STRICT full-consumption
  `decode` (unknown kind, truncation and trailing bytes refused);
  `StreamCallBinding` (the 11 `CallBinding` fields + kind + session_binding —
  13 fields, 337 B — under `blake3::derive_key("net-org-stream-call-v1", …)`;
  §1.2); `RpcCallShape { Unary, ServerStreaming, ClientStreaming, Duplex }`
  (C3, `#[non_exhaustive]`) with `from_streaming_flags`/`stream_kind`/
  `from_stream_kind`; shared `check_proof_expiry_at`; the §1.2 `with_capacity`
  fix at the transcript builder (240 → 304, one realloc per sign/verify
  removed). The unary `OrgCallProof`/`CallBinding`/`"net-org-call-v1"` are
  byte-unchanged (wire semantics preserved).
- `behavior/org_admission.rs` — C4: `AdmissionDenied` + 7 variants
  (`ShapeMismatch`, `SessionBindingMismatch`, `DeadlineExceedsPolicy`,
  `ActiveCallOwned`, `ActiveStreamCapacity`, `Revoked`, `ResourceExhausted`)
  + `#[non_exhaustive]`, with the EXHAUSTIVE `coarse()` mapping onto the
  frozen byte set `{Denied, NotSupported, Unavailable}`
  (`ResourceExhausted`/`ActiveStreamCapacity` → `Unavailable`, per contract
  2; `StreamingUnsupported` → `NotSupported`; all else → `Denied`).
  C3: `AdmissionContext.is_unary` → `shape: RpcCallShape` +
  `registered_shape: RpcCallShape` + `session_binding: Option<[u8; 32]>`,
  `#[non_exhaustive]` + the stable `AdmissionContext::new(…)` (no second
  derivable `is_unary` anywhere). `verify_org_admission`: decode under the
  REGISTERED shape's decoder (a unary registration keeps the frozen
  trailing-tolerant `OrgCallProof::decode` — §1.4's old-provider semantics
  preserved verbatim on that path; a streaming registration requires the full
  streaming value); §1.5 step 4 (a — unary registration + streaming flags ⇒
  `StreamingUnsupported`, preserved; b — streaming registration + flags ≠
  registered shape ⇒ `ShapeMismatch`; c — `proof.kind ≠ shape` ⇒
  `ShapeMismatch`); step 9 shape-split call binding (unary
  `"net-org-call-v1"` transcript unchanged / streaming
  `"net-org-stream-call-v1"`); step 9b session fence (§1.3: the RECEIVING
  session's binding must equal `proof.session_binding`, `None` fail-closed —
  placed before the replay insert so the typed reason is
  `SessionBindingMismatch`, not `Replay`); step 11's policy closure keeps its
  unary `&OrgCallProof` signature (a streaming proof presents its five prefix
  fields via `unary_prefix()`).
- `mesh_rpc.rs` — the C3 shape derivation at the old `:1126` site (flags →
  `RpcCallShape`, this seam registered `Unary`); `sign_admission_proof`
  reshaped into the shared mint helper (contract 2: one helper for unary and
  streaming kinds; a streaming mint requires the session binding, refused
  locally without one) + the `#[cfg(any(test, feature = "fixtures"))]`
  `test_sign_admission_proof` seam (the witnesses feed providers literally the
  caller-side mint output); 12 call sites mechanically adapted
  (`RpcCallShape::Unary, None`).
- `tests/org_rpc_streaming.rs` + `tests/org_rpc_streaming/**` — the fixture
  COPIED from `tests/integration_nrpc_protected.rs:71-372` (adapted for
  module form) + additions: `cross_org_intent`, `opening_request`, and
  `session_bindings` — two bindings read back from EXPLICIT
  `NetSession::with_binding` establishments, per Main's F1 interim fixture
  rule (never a live accessor returning `None`).
- `guards/org_api_probe` (Main-authorized, the named-break doctrine):
  `pin_admission_context`'s literal → the stable constructor (the C3 break
  realized — updated, not deleted), `pin_admission_denied` extended with the
  7 new arms + the acknowledged `#[non_exhaustive]` fallback arm (NO existing
  arm deleted), `AdmissionContext.shape` in the report line, MANIFEST
  +`net::adapter::net::behavior::org_call::RpcCallShape`. The C1
  (`RpcStreamingContext`) literal remains for 1.3's commit as directed.
- `tests/subnet_org_boundary.rs` (Main's bounded carve) — the mechanical C3
  migration (literal → constructor; `is_unary: true` →
  `Unary/Unary/None`-equivalent; zero names/assertions/counts touched).

**Frozen old provider (the §1.4 mixed-version execution).**
`tests/org_rpc_streaming/frozen_85ecc77c9/` vendors, at revision `85ecc77c9`:
`OrgCallProof`+`CallBinding` (`org_call.rs:50-359`, extraction sha256
`64adefbc5b30b10ba75186572efab97a51aefcd4ce23d8b8541835d1d72f4d2c`),
`verify_org_admission`+its context/denial/coarse types
(`org_admission.rs:64-662`, `ef756fdb76c4ba2eaf7548a437bce8cc897eb60e45955b2cca423b2f6c3fd646`),
and `serve_rpc_protected` (`mesh_rpc.rs:3400-3438`,
`2b1234f4d377685a39ee0db58218d69c76c8c128fb8bd8b542b4a1a914935292`)
plus the frozen flag derivation (`mesh_rpc.rs:1124-1127`, `f74300ce…`) and
the frozen denial wire shape (`mesh_rpc.rs:872-876`, `fa275454…`). Bodies are
byte-identical to the `git show | sed -n` extractions (proven by sha256
pair-match on the assembled files; each module doc records its command, hash
and every adaptation: import retargets, the `pub(crate) current_timestamp`
mirror of `org.rs:963-967`, one `crate::`→`net::` prefix on the denial-shape
line, and the two named `Self` shims `node_authority`/`serve_rpc_unary_impl`
the vendored body calls — the latter captures the frozen
`UnaryAdmission::Protected` and sources the `ServeHandle` from the harness
node's UNCHANGED v0.4 public registrar, making no admission decision). The
three vendored modules carry `#[rustfmt::skip]` on their declarations so a
formatting pass can never destroy the recorded provenance.

**Executed frozen result:** `frozen_old_provider_refuses_stream_proof_with_not_supported`
PASSES. The frozen path — fed the NEW caller's real mint bytes (streaming
flags) — yields exactly `AdmissionDenied::StreamingUnsupported` →
`CoarseAdmissionReason::NotSupported` → `RpcStatus::AdmissionDenied` + body
`[1]` (the NotSupported coarse byte). Typed `NotSupported` observed, not a
generic denial: **the prefix design HOLDS; it is not withdrawn.** The frozen
registration gates are executed too (`ProtectedAuthorityRequired`,
`InvalidProtectedRegistration`, then the happy registration).

**Witnesses and counts (executed):** `cargo t --retries 0 --test
org_rpc_streaming` → `Summary 6 tests run: 6 passed, 0 skipped`, exit 0 —
`stream_opening_admits_same_org`, `stream_opening_admits_cross_org`,
`unary_context_proof_is_binding_invalid_on_stream_registration`,
`stream_proof_on_unary_registration_is_not_supported`,
`frozen_old_provider_refuses_stream_proof_with_not_supported`,
`replayed_opening_on_new_session_is_session_binding_mismatch`.
Supporting units (new): `stream_proof_bytes_prefix_match_the_unary_encoding`
(structural prefix recovery + the 33-byte suffix + pre-signature byte
identity) and `stream_decode_rejects_truncation_trailing_bytes_and_unknown_kinds`
(strict full-consumption) in `org_call.rs`'s test module.

Regression surface (executed): `cargo tfl behavior::org_call
behavior::org_admission` → **55 run / 55 passed**; `cargo tfl
adapter::net::mesh_rpc` → **52 run / 52 passed** (includes the preserved
`client_stream_bridge_rejects_before_fold_end_to_end`,
`duplex_bridge_rejects_before_fold_end_to_end`,
`reject_relayed_flow_controlled_request_rejects_only_relayed_flow_controlled_uploads`
— `mesh_rpc.rs:7922,8118,7739` — and the still-green, still-refusing
`org_proof_intent_rejected_on_streaming_and_capability_mismatch`); the
preserved+control batch — one invocation, `cargo t --retries 0` over
`org_rpc_streaming`, `integration_nrpc_streaming`,
`integration_nrpc_client_streaming`, `integration_nrpc_duplex`,
`nrpc_streaming_gate`, `nrpc_registration_order`,
`integration_nrpc_protected`, `org_admission_wire`, `subnet_org_boundary` →
**95 run / 95 passed / 0 skipped**. The remaining preserved trio
(`client_streaming_denies_unauthorized_caller`,
`duplex_denies_unauthorized_caller`,
`denial_is_not_fanned_out_to_the_reply_roster`) is source-located in
`tests/nrpc_streaming_gate.rs:153,191,246` and ran inside that batch's
`nrpc_streaming_gate` binary. The Stage 0 models (`org_stream_lifecycle` /
`org_stream_registry`) are untouched and green inside the 5779-skipped unit
surface (their 76 run at `cargo tl org_stream` — stage-end, Main's).

**CI pin data for Main (never edited here):** binary `org_rpc_streaming`,
floor **6**, REQUIRED names exactly the six above (suggested filter-set:
`stream_opening_admits_same_org stream_opening_admits_cross_org
unary_context_proof_is_binding_invalid_on_stream_registration
stream_proof_on_unary_registration_is_not_supported
frozen_old_provider_refuses_stream_proof_with_not_supported
replayed_opening_on_new_session_is_session_binding_mismatch`).

**Inverse receipts (executed, raw).** Each mutation is a bounded diff at the
PRODUCTION site; commands are `cargo t --retries 0 --test org_rpc_streaming`
from `net/crates/net/` (`echo EXIT=$?` captured); restores are sha256-proven
byte-identical and each closes with a green run.

- **A — transcript truncation** (the brief's prescribed inverse) at
  `org_call.rs` `StreamCallBinding::transcript_hash`: the `kind`/
  `session_binding` buf lines removed and the label collapsed — the streaming
  transcript IS the unary transcript. **Exit 100.** Verbatim red:
  `unary_context_proof_is_binding_invalid_on_stream_registration`
  (`tests/org_rpc_streaming.rs:241`) — `assertion 'left == right' failed: a
  unary-domain signature must never authorize a streaming opening / left:
  Ok(Admitted { caller: EntityId(58936604abda112b…), acting_org:
  OrgId(2152f8d19b791d24…), provider_org: OrgId(2152f8d19b791d24…), provider:
  EntityId(9999999999999999…), capability: CapabilityAuthorityId(d6a20d04fe7d3ff0)
  }) / right: Err(BindingInvalid)`; `Summary 6 tests run: 5 passed, 1 failed`.
  Restore `845daea39cd261de…` == baseline both sides. Green leg: 6/6.
  **Green under this diff (the brief's "a green is a finding" case — analyzed,
  see finding 1):** `stream_opening_admits_same_org`,
  `stream_opening_admits_cross_org`,
  `replayed_opening_on_new_session_is_session_binding_mismatch`.
- **A2 — session-fence removal** at `org_admission.rs` step 9b (the compare
  deleted). **Exit 100.** Verbatim red:
  `replayed_opening_on_new_session_is_session_binding_mismatch`
  (`tests/org_rpc_streaming.rs:486`) — `assertion 'left == right' failed: a
  replayed opening on a new session must surface SessionBindingMismatch, not
  Replay / left: Err(Replay) / right: Err(SessionBindingMismatch)` (the flip
  to `Replay` proves the fence AND its check-order before the replay insert).
  Restore `f346f694fdb81c66…`. Green leg: 6/6.
- **A3 — verify-side binding reconstruction drops the provider-known
  `request_digest`** at `org_call.rs` `binding_for_stream_verify`. **Exit
  100.** Verbatim reds: `stream_opening_admits_same_org`
  (`tests/org_rpc_streaming.rs:128`) and `stream_opening_admits_cross_org`
  (`:178`), both at their `.expect("…admits")`; collateral
  `replayed_opening_on_new_session_is_session_binding_mismatch` (`:470`, first
  leg); `Summary 6 tests run: 3 passed, 3 failed`. Restore
  `845daea39cd261de…`. Green leg: 6/6.
- **B — step-4a removal** at `org_admission.rs` (the unary-registration
  streaming refusal). **Exit 100.** Verbatim red:
  `stream_proof_on_unary_registration_is_not_supported`
  (`tests/org_rpc_streaming.rs:294`) — `assertion 'left == right' failed /
  left: Err(BindingInvalid) / right: Err(StreamingUnsupported)` (the typed
  NotSupported degrades without step 4a). Restore `f346f694fdb81c66…`. Green
  leg: 6/6 (one leg re-run after Main's disclosed coordinator-mutation
  incident broke the shared tree mid-run — the incident is in §0; my green leg
  then ran against the pristine tree, `git diff --quiet` clean).
- **C — frozen step-4 removal** at `frozen_85ecc77c9/old_org_admission.rs`
  (the vendored `if !ctx.is_unary` block). **Exit 100.** Verbatim red:
  `frozen_old_provider_refuses_stream_proof_with_not_supported`
  (`tests/org_rpc_streaming.rs:405`) — `assertion 'left == right' failed: the
  frozen path must carry the typed StreamingUnsupported refusal / left:
  BindingInvalid / right: StreamingUnsupported`. Restore
  `d5faa9fe82f76db3…`. Green leg: 6/6.
- **D — decoder substitution** (the brief's pre-named check) at
  `frozen_85ecc77c9/old_org_admission.rs`: the NEW strict streaming decoder
  substituted ahead of the frozen decode. **Exit 0 — all six GREEN, the
  frozen witness included.** This is exactly the outcome the brief pre-named
  ("replacing the vendored old decoder with the new one → the frozen witness
  no longer discriminates (must be caught by the report)"): both decoders
  ACCEPT a well-formed full streaming proof (the frozen one ignores the
  suffix; the new one consumes it) and the frozen step 4 refuses either way,
  so the yield is identical. The decoders diverge only on truncation /
  trailing bytes / unknown kind — pinned at the production decoder by
  `stream_decode_rejects_truncation_trailing_bytes_and_unknown_kinds`.
  Recorded as the brief's NON-DISCRIMINATING inverse, run and disclosed (not
  hidden, not re-pinned). Restore `d5faa9fe82f76db3…`. Green leg: 6/6.

**Findings (state, not decide):**

1. **F-S1.2-1 — the prescribed transcript-truncation inverse reaches only one
   of the two named mismatch witnesses (executed + source-established).** The
   brief: "remove `kind`/`session_binding` from the transcript → the two
   mismatch witnesses must go red (a green is a finding)". Executed literally
   (receipt A), `unary_context_proof_is_binding_invalid_on_stream_registration`
   reddened but `replayed_opening_on_new_session_is_session_binding_mismatch`
   did not. Two source reasons (citations): (i) the latter witness's
   production site is the session-binding COMPARE at `org_admission.rs`
   step 9b (pristine `:727-730`), which the transcript diff does not touch —
   its own inverse is the compare's removal (receipt A2, red verbatim above);
   (ii) caller and provider share ONE `StreamCallBinding::transcript_hash`
   (`org_call.rs:478`), so a same-tree transcript mutation moves signer and
   verifier TOGETHER and same-domain proofs keep verifying — only the
   cross-domain witness (its signature made through `CallBinding::sign`) can
   see such a mutation at all. Per the brief's rule this is the declared
   "green is a finding" case for the three greens; classification against the
   four weakenings: **none applies** — the diff is not those witnesses' own
   inverse, and each of them holds a discriminating red at its own production
   site (A2/A3). Stated per "findings beat workarounds": the brief's
   expectation is wrong against the source for one of the two witnesses; the
   code was not contorted to match it. No witness deleted or re-pinned.
2. **F-S1.2-2 — C3's single-field shape term vs step 4's two inputs (state;
   ruling territory).** Contract 2/§1.5 say `shape: RpcCallShape` replaces
   `AdmissionContext.is_unary`, "derived from (registration shape, payload
   flags)", while step 4's three checks need BOTH the registration shape and
   the flags-derived shape as separate facts. Implemented with `shape` (the
   flags-derived call shape — the literal `is_unary` replacement) and
   `registered_shape` (new) plus `session_binding`; the stable constructor
   takes the registration shape and the two flag bits and derives `shape`.
   If the ruling intended ONE field, the spelling is Main's decision — the
   behavior named in §1.5 is fully implemented as stated here.
3. **F-S1.2-3 — `tests/subnet_org_boundary.rs` broke under C3 (executed;
   adapted).** An unnamed in-repo consumer of the C3 literal surface; migrated
   mechanically to the stable constructor under Main's bounded carve (values
   identical). 15/15 green in the 95-batch.
4. **F-S1.2-4 — the probe's C3/C4 update landed at 1.2, not 1.3 (executed).**
   The named-break doctrine ("the probe is updated in the same commit as the
   break") binds C3/C4 as it binds C1: leaving `guards/org_api_probe` broken
   between slices would break "green at your exact head". The C1
   (`RpcStreamingContext`) literal update remains 1.3's, as directed.
5. **F-S1.2-5 — `call_streaming`'s `org_proof_intent` widening lands with the
   1.6 inversion (ordering note).** Contract 2 names "mint helper shared by
   unary and `call_streaming`": the shared helper is delivered here for both
   shapes and its streaming branch is EXECUTED here (through
   `test_sign_admission_proof`, the real caller-side mint path), but
   `call_streaming`'s refusal (`mesh_rpc.rs:5052`) is pinned green by
   `org_proof_intent_rejected_on_streaming_and_capability_mismatch`, whose
   rewrite into `call_streaming_mints_a_stream_proof` is 1.6's NAMED work.
   Moving the widening earlier would make 1.2–1.5 red per-slice. The
   `call_streaming` call site lands in 1.6's inversion commit.
6. **F-S1.2-6 — `target/nextest/default/junit.xml` collides across concurrent
   lanes (executed observation; F14-family).** My batch's junit was
   overwritten by S1Session's later run before name-scraping (the file held
   their `org_routing_wiring_tests::…` entry). Rosters here come from source
   (name grep + filter match + run summaries), never from intent; CI's
   `check-witness-results.py` reads the junit single-lane and is unaffected.
   Sibling half addressed by Main's process rule (§0).
7. **F-S1.2-7 — fmt drift in my new hunks, converged (executed).** S1Session's
   F5 named `org_admission.rs:511,547,591` + `org_call.rs:669,1126`; a scoped
   `rustfmt --config skip_children=true` check showed the drift was confined
   to my own hunks (incl. `mesh_rpc.rs` mint-helper and adapted call sites)
   and was applied to my seven files only. The three verbatim vendored
   modules are protected by `#[rustfmt::skip]` on their declarations
   (provenance hashes intact after the pass: `d5faa9fe…`, `0ab7e915…`,
   `9c5963e8…`).

**Contract-1 verification (Main's relayed request, source-established):**
`NetSession::handshake_binding() -> Option<[u8; 32]>`
(`wire/src/session.rs:460`) and `MeshNode::peer_session_binding(node_id: u64)
-> Option<[u8; 32]>` (`mesh.rs:19633`, beside `peer_session_id`) match the
frozen contract exactly; `with_binding`'s 5-arg spelling is Main-ruled
contract-conformant (F2 ruling, §0) — not a finding here.

**What never ran here (complete):**

- `cargo fmt -p <crate> -- --check`, clippy ×4, rustdoc ×5, `cargo check
  --workspace --all-targets` — Main's stage-end list (mid-flight rule). My
  files were converged with scoped `rustfmt --check`/apply (above) but the
  gate command itself never executed here.
- `cargo tl` / `cargo t` full suites, the wire suite, `tests/cross_lang_*`,
  the browser/SDK surfaces — stage-end (Main's) or other lanes'.
- CI itself (branch unpushed; nobody pushes but Main).
- The 1.1/1.1a wire and handshake witnesses (S1Session's — re-run at stage
  end).
- Windows workstation only: `#[cfg(unix)]` legs never compile here; no
  Linux/macOS execution.

### 2.2 Slice 1.3 — fold ownership + lifetime

**Landed (executed):** `b4fc7933f` — `S1.3: own streaming folds' call keys
and supervise protected streams` (5 files, +2648/−313) on
`LZL0/org-streaming`; this record rides in the following `S1.3:` commit.
Base `e0b76e05e` (S1Session's `S1.1`/`S1.1a`, the 1.2 landing + record,
and Main's `S1 CI pin` commit). All green claims below are at
`b4fc7933f`'s exact tree.

**What landed** (source-established; executed via the runs below):

- `cortex/rpc.rs` —
  - **C5:** `StreamCallKey = (from_node, receiving_session_id, origin,
    call_id)` unified across the server folds — `InFlightCalls`,
    `RequestChunkSenders` and `FlowControlMap` all keyed on it (the
    unary-only `UnaryInFlightCalls` alias folded in; one key shape now
    that the keys match), `self.session_id` set from the event in every
    `apply_inbound` (public + protected), and
    `apply_request_chunk_to_senders` takes the receiving incarnation.
    The test accessors (`in_flight_keys`/`sender_keys`/
    `flow_control_permits`) return the four-part keys and are
    `fixtures`-visible. A late chunk / CANCEL / GRANT from a replaced
    session with the same `(node, origin, call_id)` misses the map.
  - **The SS REQUEST arm's flag check** (contract 4, unified with the
    CS/DX discipline): flags whose derived shape is not exactly
    server-streaming (`FLAG_RPC_STREAMING_RESPONSE` set,
    `FLAG_RPC_CLIENT_STREAMING_REQUEST` clear) are refused cleanly
    before any call state exists.
  - **C1/Q2 named break:** `RpcStreamingContext` gains `org_admission:
    Option<Admitted>`, becomes `#[non_exhaustive]`, and gets the stable
    `new()` constructor (creates NO admitted facts — admission facts
    originate at the verifier); both fold construction sites migrated.
  - **§2.1 production implementation** (mirror of the Stage 0
    `org_stream_lifecycle` model, which stays `#[cfg(test)]`):
    `StreamLifetimePolicy` (`q1_defaults()` = 300 s / 3600 s + startup
    `validate`), `ResolvedStreamDeadline`/`StreamDeadlineBound`,
    `StreamDeadlineRefusal`, `resolve_stream_deadline` (the default is
    reached ONLY through the omitted-deadline arm and never caps an
    explicit request; an explicit end over `now + max_live` is REFUSED;
    credential validity clamps with `<=` so an exact tie reports
    `Credential`; an already-elapsed end is refused), and
    `StreamCallLifetime { policy, credential_ends_ns, clock }` — the
    admission's ONE `ClockSample`, with `monotonic_deadline_for`
    translating the resolved wall end into the record's monotonic
    deadline.
  - **§2.2/§2.6 production implementation:** `StreamCallRecord`
    (independent halves; server-streaming starts `input = Ended`;
    `handler_returned` → `Draining(result)` and never terminal;
    `pump_exited` → `Completed(result)` vs `PumpFailed`; `retire`
    first-writer-wins; `record_emission` one-shot), `StreamRetireSignal`
    (register-before-recheck), `StreamProducerGate` (§2.2's
    producer-finished gate), `ProtectedStreamCall` (the
    supervisor-owned record handle: resolved deadline + bound,
    terminal, emission, retire), `ProtectedStreamOwners` (the
    registration's live protected set + `retire_all`),
    `StreamCallRegistration` (the §2.4 single removal point),
    `stream_terminal_payload` (reason → wire: `Completed(Ok)` → `Ok` +
    `nrpc-streaming: end` verbatim; `Completed(Err)` → the handler's own
    status/body; `Cancelled`/`ServeHandleDropped` → `Cancelled`;
    `SessionReplaced` → `Cancelled`; `Timeout` → `Timeout` (C7);
    `CredentialExpired`/`Revoked`/`AuthorityUnavailable` →
    `AdmissionDenied` + coarse `Denied` byte `[0]`; `ResourceExhausted`
    → `AdmissionDenied` + `[2]`; `PumpFailed` → `Internal`) — and
    `run_stream_call_supervisor`: the persistent `biased` `select!` over
    {retire, `sleep_until(resolved)`, handler, pump join} that STAYS in
    the loop after the handler returns (producer finished is not
    terminal); on retirement: first-writer `retire`, the handler's
    cancellation token signalled, the flow semaphore `close()`d, the
    pump `abort()`ed and **awaited**, then exactly one terminal AFTER
    pump stop (queued-data policy: only `Completed(_)` drains — every
    retirement discards with the aborted receiver). §2.8's
    `StreamTerminalDisposition` is recorded exactly once on the
    `Terminal` (see F-S1.3-5 for the `Queued` mapping at this layer).
  - **Contract 4's fold seam** — `apply_inbound_admitted(frame,
    admitted, lifetime) -> Result<Arc<ProtectedStreamCall>,
    AdmissionDenied>` on the SS fold (at 1.3 the `lease` slot carries
    the `Admitted` facts; the registry's exact-incarnation lease is
    1.4's and the four-part key does the session fencing). Refusals are
    typed and land BEFORE any handler effect — non-REQUEST →
    `NotOrgProtected`; a live-key duplicate → `ActiveCallOwned` before
    the payload decode (§3); malformed → `MalformedProof`; flags ≠ SS →
    `ShapeMismatch`; the §2.1 deadline refusal → `DeadlineExceedsPolicy`
    (the specific `StreamDeadlineRefusal` is logged). The fold emits
    NOTHING on refusal — the bridge owes exactly one bounded denial
    through the unchanged `emit_admission_denial` (slice 1.5). On
    admission the raw `net-org-admission` header is stripped (E1.6) and
    the supervisor is spawned owning handler, pump, semaphore and the
    one terminal.
  - **Public SS keeps Q3's shared repairs only:** a NONZERO
    `deadline_ns` is enforced (C6) with a `Timeout` terminal (C7) via
    the same bounded call shape; `deadline_ns == 0` still means no
    deadline; the documented CANCEL-wins terminal override is preserved;
    no retire signal / producer gate (public lossy-sink and
    outstanding-call behavior unchanged). The CANCEL arm routes
    PROTECTED records through the supervisor (single removal point — a
    re-REQUEST cannot race the retired call's cleanup) and keeps the
    legacy map cleanup for public. The GRANT arm classifies through the
    record (§2.6: credit survives `Draining`, stops at terminal/`Ended`).
  - **CS/DX shared repairs:** C5 keying + `session_id` (the DX fold gains
    the field; the CS fold's dormant field is now set) and C7 — a
    deadline expiry's terminal is typed `Timeout` (see F-S1.3-1: the
    pre-change source emitted `Cancelled`, not `Internal`, because the
    deadline's own `cancel_for_deadline.cancel()` fed the CANCEL-wins
    override). A caller CANCEL that fired before the deadline still
    wins (the preserved public semantic).
  - In-source fold witnesses migrated mechanically (four-part key
    literals; the AV-1 server-streaming fixture's opening flags brought
    under the new flag check); names and assertions unchanged.
- `mesh_rpc.rs` — `ServeHandle` gains `protected_streams:
  Option<ProtectedStreamOwners>`: the SS registration wires
  `fold.protected_owners()` and `Drop` retires exactly those records
  with `ServeHandleDropped` (whose emitted terminal is `Cancelled`, per
  §2.2's queued-data table). Q3/C9 scope: protected-only on handle drop
  — PUBLIC calls are not in the set and keep their documented
  outstanding-call behavior; §2.2's async-cleanup boundary is honored
  (signals fire synchronously; cleanup completes after). CS/DX handles
  carry `None` (their protected admitted seam is Stage 2's — no org
  admission on CS/DX at 1.3). A `fixtures`-gated
  `ServeHandle::streaming_fold_for_test()` (the `origin_node_cache`
  precedent) lets a witness drive the fold's admitted seam through a
  REAL registration.
- `guards/org_api_probe` (the C1 named break, landed in the SAME commit
  as the break): `pin_rpc_streaming_context`'s struct literal STOPPED
  compiling and is UPDATED — not deleted — to the stable constructor
  plus a no-admitted-facts assertion; the report line adjusted ("3
  ledger surfaces constructed … org_admission.is_none()=true" — probe
  run output captured). Every other pin untouched. Executed green:
  `cargo metadata --locked`, `cargo check --locked` (its own lockfile),
  and a probe run.

**Witnesses and counts (executed):** `cargo t --retries 0 --test
org_rpc_streaming` → `Summary 15 tests run: 15 passed, 0 skipped`, exit
0 — `late_chunk_from_replaced_session_is_dropped`,
`omitted_deadline_gets_default_and_expires_idle`,
`requested_deadline_over_cap_is_refused_with_zero_effects`,
`requested_deadline_within_cap_is_honoured`,
`pump_parked_on_zero_credit_is_retired_at_deadline_with_one_terminal`,
`serve_handle_drop_retires_live_stream_and_sibling_survives` (the six
NAMED witnesses), plus `credential_clamp_expiry_is_admission_denied_not_timeout`
(§2.1 bound 3 + the tie rule),
`public_ss_nonzero_deadline_expires_with_typed_timeout` (C6),
`public_client_stream_deadline_expiry_is_typed_timeout` (C7) — and the
1.2 six still green: `stream_opening_admits_same_org`,
`stream_opening_admits_cross_org`,
`unary_context_proof_is_binding_invalid_on_stream_registration`,
`stream_proof_on_unary_registration_is_not_supported`,
`frozen_old_provider_refuses_stream_proof_with_not_supported`,
`replayed_opening_on_new_session_is_session_binding_mismatch`.
Floor becomes 6+N = **15** (N = 3).

Regression surface (executed): the preserved + control batch — one
invocation, `cargo t --retries 0 --test nrpc_streaming_gate --test
integration_nrpc_streaming --test integration_nrpc_client_streaming
--test integration_nrpc_duplex --test nrpc_registration_order --test
integration_nrpc_protected --test org_admission_wire --test
subnet_org_boundary` → **89 run / 89 passed / 0 skipped** (the preserved
trio `client_streaming_denies_unauthorized_caller`,
`duplex_denies_unauthorized_caller`,
`denial_is_not_fanned_out_to_the_reply_roster` ran inside
`nrpc_streaming_gate`). In-source, one invocation
(`cargo tfl adapter::net::cortex::rpc adapter::net::mesh_rpc
org_stream`) → **214 run / 214 passed / 5620 skipped** — the edited
AV-1/fold unit witnesses; the preserved bridge trio
(`client_stream_bridge_rejects_before_fold_end_to_end`,
`duplex_bridge_rejects_before_fold_end_to_end`,
`reject_relayed_flow_controlled_request_rejects_only_relayed_flow_controlled_uploads`
at `mesh_rpc.rs`'s test mod); and the Stage 0 models
(`org_stream_lifecycle` / `org_stream_registry`) green and untouched.

Also executed: `cargo check --lib` (the `t`/integration feature graph)
clean; scoped `rustfmt --edition 2021 --config skip_children=true` on my
five files, `--check` clean (F-S1.2-7's precedent); the three verbatim
vendored frozen files' provenance hashes UNCHANGED
(`d5faa9fe82f76db3…`, `0ab7e915e835533c…`, `9c5963e8b14d003a…`);
probe `cargo metadata --locked` + `cargo check --locked` + run (above).

**CI pin data for Main (never edited here):** binary
`org_rpc_streaming`, floor **15**, REQUIRED names exactly:
`stream_opening_admits_same_org`,
`stream_opening_admits_cross_org`,
`unary_context_proof_is_binding_invalid_on_stream_registration`,
`stream_proof_on_unary_registration_is_not_supported`,
`frozen_old_provider_refuses_stream_proof_with_not_supported`,
`replayed_opening_on_new_session_is_session_binding_mismatch`,
`late_chunk_from_replaced_session_is_dropped`,
`omitted_deadline_gets_default_and_expires_idle`,
`requested_deadline_over_cap_is_refused_with_zero_effects`,
`requested_deadline_within_cap_is_honoured`,
`pump_parked_on_zero_credit_is_retired_at_deadline_with_one_terminal`,
`serve_handle_drop_retires_live_stream_and_sibling_survives`,
`credential_clamp_expiry_is_admission_denied_not_timeout`,
`public_ss_nonzero_deadline_expires_with_typed_timeout`,
`public_client_stream_deadline_expiry_is_typed_timeout`
(suggested filter-set: the same fifteen space-separated).

**Inverse receipts (executed, raw).** Each mutation is a bounded diff at
the PRODUCTION site (R1/R5 are the brief's two named inverses; R2/R3/R4/R6
invert the other four named properties at their production sites). The
command is `cargo t --retries 0 --test org_rpc_streaming` from
`net/crates/net/` (`echo EXIT=$?` captured); restores are cp-back and
sha256-proven byte-identical on both sides (`cortex/rpc.rs`
`77d0b338bd5a572e…`, `mesh_rpc.rs` `50fc387fdcd4cac0…`), and each
closes with a green 15/15 run.

- **R1 — the named keying inverse: restore the pre-C5 session-less key**
  (`self.session_id = ev.session_id;` removed at the three streaming
  folds' `apply_inbound`, `cortex/rpc.rs`). **Exit 100.** Verbatim red:
  `late_chunk_from_replaced_session_is_dropped`
  (`tests/org_rpc_streaming.rs:600`) — `assertion `left == right`
  failed: the handler must see ONLY the live incarnation's chunks — the
  replaced session's late chunk must be dropped, not admitted into the
  stream / left: [[111,112,101,110],[108,97,116,101],[108,105,118,101]]
  (open, **late**, live) / right:
  [[111,112,101,110],[108,105,118,101]] (open, live)`;
  `Summary 15 tests run: 14 passed, 1 failed`. Restore `77d0b338…` ==
  baseline. Green leg: 15/15. The prescribed outcome: under the
  session-less key the replaced session's frame **is admitted** into
  the stream.
- **R2 — the omitted deadline no longer receives the provider default**
  (`resolve_stream_deadline`'s `None` arm fills `max_live`,
  `cortex/rpc.rs`). **Exit 100.** Verbatim red:
  `omitted_deadline_gets_default_and_expires_idle`
  (`tests/org_rpc_streaming.rs:679`) — `assertion `left == right`
  failed: omitted ⇒ exactly the Q1 300 s provider default / left:
  1790060688158400500 / right: 1790057388158400500` (Δ = 3 300 s =
  3600 − 300). `Summary … 14 passed, 1 failed`. Restore + green 15/15.
- **R3 — over-cap is clamped instead of refused** (`resolve_stream_deadline`'s cap check → `end.min(cap)`,
  `cortex/rpc.rs`). **Exit 100.** Verbatim red:
  `requested_deadline_over_cap_is_refused_with_zero_effects`
  (`tests/org_rpc_streaming.rs:769`) — `panicked …: an explicit
  deadline over the provider cap must be refused` (the refusal
  expectation: the opening is now ADMITTED). `Summary … 14 passed, 1
  failed`. Restore + green 15/15.
- **R4 — the one-`min` clamp bug** (`Some(end)` arm → `end.min(now +
  default_live)`, `cortex/rpc.rs`). **Exit 100.** Verbatim red:
  `requested_deadline_within_cap_is_honoured`
  (`tests/org_rpc_streaming.rs:817`) — `assertion `left == right`
  failed: the requested 900 s end is honoured verbatim / left:
  1790057626258090700 / right: 1790058226258090700` (Δ = 600 s =
  900 − 300 — the explicit request was silently clamped to the
  default). Collateral red:
  `credential_clamp_expiry_is_admission_denied_not_timeout`.
  `Summary … 13 passed, 2 failed`. Restore + green 15/15.
- **R5 — the named lifetime inverse: wrap ONLY the handler in the
  timeout** (`run_stream_call_supervisor`: the call-wide `sleep_until`
  arm parked and the handler future wrapped in
  `tokio::time::timeout(remaining, …)`, `cortex/rpc.rs`). **Exit 100.**
  Verbatim red:
  `pump_parked_on_zero_credit_is_retired_at_deadline_with_one_terminal`
  (`tests/org_rpc_streaming.rs:884`) — `panicked …: the parked pump
  must be retired at the deadline with a Timeout terminal`, raised
  after the witness's 30 s bound elapsed (`FAIL [30.088s]`): the parked
  pump is never retired and the call runs past its bound (surfaced as
  the witness's bounded red; under an unbounded wait the nextest
  terminate-after would name it — the hang is the prescribed symptom).
  Collateral reds: `credential_clamp_expiry_is_admission_denied_not_timeout`,
  `omitted_deadline_gets_default_and_expires_idle`,
  `public_ss_nonzero_deadline_expires_with_typed_timeout` (every
  whole-call deadline lost its retirement).
  `Summary … 11 passed, 4 failed`. Restore + green 15/15.
- **R6 — `ServeHandle::drop` without retire** (the `retire_all` call
  removed from the Drop body, `mesh_rpc.rs`). **Exit 100.** Verbatim
  red: `serve_handle_drop_retires_live_stream_and_sibling_survives`
  (`tests/org_rpc_streaming.rs:996`) — `panicked …: the dropped
  registration's stream must retire with ServeHandleDropped`, raised
  after the 30 s bound. `Summary … 14 passed, 1 failed`. Restore
  `50fc387f…` == baseline. Green leg: 15/15.

**Findings (state, not decide):**

1. **F-S1.3-1 — the C7 row's premise mischaracterizes the source
   (executed + source-established).** The plan's Context says CS/DX
   "expiry emits `RpcStatus::Internal`, not `Timeout`". Against the
   source at `e0b76e05e` that is wrong: the deadline guards fire
   `cancel_for_deadline.cancel()` on the SHARED cancellation token
   (`cortex/rpc.rs:3405`, `:3418` CS; `:3858`, `:3871` DX), and the
   terminal selection's CANCEL-wins override (`let terminal = if
   cancel_probe.is_cancelled()`, `:3440` CS / `:3894` DX) then wins — a
   deadline expiry emitted `RpcStatus::Cancelled` with the "server
   observed CANCEL during … handler execution" body; the constructed
   `Internal("… deadline_ns exceeded")` outcome was shadowed. C7's
   RULING ("→ `Timeout`") is unambiguous and is implemented as ruled:
   the expiry is recorded before the deadline's own cancel signal and
   the terminal selection orders `Timeout` first, while a caller CANCEL
   that preceded the deadline keeps its documented override. Per the
   brief's rule the wrong premise is reported with citations, not coded
   around. Witnesses:
   `public_client_stream_deadline_expiry_is_typed_timeout` +
   `public_ss_nonzero_deadline_expires_with_typed_timeout`.
2. **F-S1.3-2 — node-shutdown retirement is unreachable from the files
   this slice owns (source-established).** Q3/C9 require node shutdown
   to retire all node-owned calls/tasks (public + protected). The
   shutdown paths are `Adapter::shutdown`
   (`src/adapter/net/mesh.rs:48891`) and `Drop for MeshNode`
   (`mesh.rs:49060`) — both inside `src/adapter/net/mesh.rs`, on this
   slice's MUST-NOT-TOUCH list ("never, even for compilation fixes").
   The signals that could carry the hook (`tasks`, `shutdown`,
   `shutdown_notify`, `mesh.rs:13018-13022`) are private fields of
   `MeshNode`, and `mod mesh` is private
   (`src/adapter/net/mod.rs:60`), so `mesh_rpc.rs`/`cortex/rpc.rs`
   cannot observe shutdown (only the polled `is_shutdown()` flag is
   `pub`). Landed and reachable here: `ServeHandle::drop`'s
   protected-only retirement + `ProtectedStreamOwners::retire_all` as
   the single entry point a shutdown hook would call. The remaining
   wiring is one call from the shutdown path — unowned here. None of
   the six named witnesses covers shutdown (none is prescribed), so
   this is UNWITNESSED as well as unwired. Stated, not decided.
3. **F-S1.3-3 — the `DeadlineRefusal` identity collapses at the
   `AdmissionDenied` mapping (state).** §2.1 names
   `AdmissionDenied::DeadlineExceedsPolicy` only for the over-cap
   refusal; the model keeps `DeadlineRefusal::{ExceedsPolicy,
   AlreadyElapsed, Overflow}` distinct and no C4 variant covers the
   latter two. Implemented mapping: all three → `DeadlineExceedsPolicy`
   (coarse `Denied` either way), with the distinct
   `StreamDeadlineRefusal` preserved at the seam and logged at the
   refusal. A distinct wire reason would be a C4-extension ruling —
   none exists.
4. **F-S1.3-4 — `SessionReplaced`'s wire status is not spelled by the
   plan (state).** §2.2's queued-data table gives its disposition
   ("terminal attempted `DirectOnly`, dropped if the session is gone")
   but no `RpcStatus`; §4.3 names only `AdmissionDenied(Denied)` /
   `Timeout` / `Cancelled` for midstream outcomes. Implemented as
   `RpcStatus::Cancelled` ("peer session replaced" body), beside
   `ServeHandleDropped` whose `Cancelled` IS specified. No witness
   here — session-replacement retirement is slice 1.4's `retire_session`.
5. **F-S1.3-5 — the terminal handoff must be non-blocking (executed
   observation + source).** With the terminal emit awaited inside the
   supervisor, ownership completion became hostage to transport: on a
   live node the `serve_rpc_streaming` emit closure's
   `publish_response_to_caller(…).await` did not complete (no peer),
   the supervisor never reached scope end, and the handler future was
   not dropped within the observation bound (first run of
   `serve_handle_drop_retires_live_stream_and_sibling_survives`, a 10 s
   bounded red at the drop-flag assertion). §2.2 ("Bound all
   library-controlled waits … rather than hang") and §2.8 ("a `try_send`
   attempt is **not** peer receipt") resolve it exactly as the model
   does — `ctl.try_send` + record + return: the supervisor now hands
   the terminal to the response-emitter seam non-blocking (spawn) and
   records `StreamTerminalDisposition::Queued`. `Sent`/`Refused`/
   `Unreachable` become separately distinguishable at slice 1.5's
   bounded `RpcResponseJob` drainer / route layer (documented on the
   enum). Wire order is preserved: the pump is joined before the
   handoff, so the terminal starts strictly after the last chunk emit.
6. **F-S1.3-6 — a retire that wins before the supervisor's first poll
   never enters the handler (executed observation).** The `biased`
   `select!` takes an already-ready retire arm on its first poll, so
   the handler future is never polled and its body never exists
   (including its drop-guards) — "zero handler effects" holds even
   earlier than §3 step 5's wording. The witnesses that observe
   handler-drop therefore wait for handler entry first; noted for
   1.4/1.5's witness design (a "handler stayed dark" observation is
   stronger than "handler did not drop").
7. **F-S1.3-7 — interpretation note: where the protected SS records are
   fed (state).** The assignment's "wire the protected SS records'
   supervisor spawn at the serve seam (~4078-4290) feeding
   `apply_inbound_admitted`" is implemented as the fold seam plus its
   serve-side wiring (the `ServeHandle` ownership set and the
   fixtures-gated fold handle); the actual FEED of admitted records is
   `admit_and_dispatch_protected_stream` at the `Proceed` seam — the
   plan names THAT as slice 1.5's target (`mesh_rpc.rs:4242-4247`).
   At 1.3 the seam is exercised by the unit-witness idiom (fold-level
   driving through REAL `ServeHandle` registrations in
   `serve_handle_drop_retires_live_stream_and_sibling_survives`).
8. **F-S1.3-8 — the model's byte-permit semaphore and the sink
   refusal-latch land with 1.4 (state).** §2.2's table closes "byte-
   permit semaphore (2.7)" beside the flow semaphore and §2.7's
   response direction wants a refused protected item to latch
   `ResourceExhausted`; the brief assigns §2.7 byte accounting to slice
   1.4 ("byte accounting per §2.7 at both enqueue boundaries"). The 1.3
   supervisor closes the semaphores the call owns (the flow-control
   credit one) and the protected sink carries §2.2's producer-finished
   gate only.

**What never ran here (complete):**

- `cargo fmt -p <crate> -- --check`, the four clippy invocations, the
  five rustdoc lines and `cargo check --workspace --all-targets` —
  Main's stage-end list (mid-flight rule). My five files were converged
  with scoped `rustfmt --check`/apply (clean) and the lib compiled
  clean on the integration feature graph, but the gate commands
  themselves never executed here.
- `cargo tl` / `cargo t` full suites; `tests/cross_lang_*`; the wire
  suite; the browser/SDK surfaces; the 1.1/1.1a wire and handshake
  witnesses (S1Session's — re-run at stage end).
- CI itself (branch unpushed; nobody pushes but Main).
- Node-shutdown retirement (F-S1.3-2 — not wired at its only call
  sites and therefore not witnessed; the `ServeHandle::drop` half is
  landed and witnessed).
- Authenticated ENDPOINT receipt of the terminal frames. The
  fold-level witnesses capture at the response-emitter seam (the §2.8
  "network receipt is a separate observation" boundary); live endpoint
  receipt belongs to slice 1.5's bridge witnesses.
  `serve_handle_drop_retires_live_stream_and_sibling_survives` drives
  real registrations on a live node but observes the records' handles
  and fold maps, not network delivery.
- CS/DX PROTECTED admission (Stage 2 by contract) and
  `call_streaming`'s `org_proof_intent` widening (slice 1.6's named
  inversion per F-S1.2-5 — `mesh_rpc.rs:5052`'s refusal untouched and
  still green under
  `org_proof_intent_rejected_on_streaming_and_capability_mismatch`).
- The `Revoked`/`AuthorityUnavailable`/`ResourceExhausted` terminal
  wire mappings (`stream_terminal_payload`) — constructed per §2.2/
  §2.7/§4.3 but their retirement triggers are slice 1.4's
  revocation/byte-accounting work; no witness reaches them here.
- Windows workstation only: `#[cfg(unix)]` legs never compile here; no
  Linux/macOS execution.
