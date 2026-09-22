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
- **2026-09-22, slice 1.4 accepted (executed verification).** Landed
  `62f4358bc` + `bd5de3b5d` via worker `S1Core.S14RegistryRevocation` (its
  parent's run ended mid-stage; the handoff is recorded in its final result).
  Verified: paper audit of report §2.3 (24/24 in `org_rpc_streaming`,
  preserved+controls 89/89, in-source 216/216 with the Stage 0 models green
  and byte-untouched, frozen-decoder provenance hashes unchanged,
  step10/step11 byte-unmodified with the unary pins green) plus an
  **independent coordinator reproduction of the node-shutdown carve receipt**
  in the between-lanes window: removing the `org_registry_retire_all` call at
  `mesh.rs:48938` reddened `node_shutdown_retires_live_protected_streams`
  with the named assertion ("the live protected stream must be retired by
  node shutdown", bounded 30.2 s timeout — `:2310:5` observed under this
  coordinator's −1-line mutation; the worker's receipt quotes `:2268:5`
  under its own mutation's delta; assertion text identical), restore proven
  `RESTORED_BYTE_IDENTICAL`, restored run green. The worker's R6 disclosure
  (a partial-inverse green, then the true inverse red — the four-weakenings
  rule honored) is credited as the evidence discipline working as designed.
  CI floor re-pinned 15 → **24** at `8ad588171` (roster checker: 24 names
  exist in source).
- **2026-09-22, ruling — F-S1.4-1 (model-vs-C4 `Revoked` mapping).** Plan
  §4.3 governs: "midstream retirement arrives as the stream's final
  `Err(AdmissionDenied(Denied))` (revocation)" — a revocation refusal shares
  the frozen `Denied` coarse byte. The Stage 0 model's
  `Denial::coarse()` mapped `Revoked → Unavailable`, contradicting §4.3 —
  a Stage 0 acceptance defect (same class as F1, caught here by slice 1.4's
  contract audit). Production followed §4.3 throughout and is unchanged. The
  model is corrected at `org_stream_registry.rs` (mapping moved to the
  `Denied` arm with the ruling cited) and the pinned assertion in
  `raise_between_reserve_and_install_denies_with_zero_effects` is a **named
  rewrite** (it pinned the mapping the ruling corrected — F1 precedent:
  strengthened to the owner-approved property, never silently re-pinned).

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

### 2.3 Slice 1.4 — registry + revocation

**Landed (executed):** `62f4358bc` — `S1.4: wire the protected-call
registry, revocation, and byte accounting` (6 files, +4632/−384) on
`LZL0/org-streaming`; this record rides in the following `S1.4:` commit.
Base `95845205c` (the S1 CI pin after 1.3). All green claims below are at
`62f4358bc`'s exact tree (the scoped rustfmt pass is whitespace-only and
landed inside it; the receipt cycles ran on the pre-format tree — the
1.1a precedent — with post-format shas recorded below).

**What landed** (source-established; executed via the runs below):

- `cortex/rpc.rs` — **the §3/§2.3/§2.7 production registry**, the
  production mirror of the Stage 0 model
  `behavior/org_stream_registry.rs` (whose file is byte-untouched — the
  "production module(s) the model mirrors" shape of the 1.3 precedent,
  the model's own doc naming `admit_and_dispatch_protected` as the
  ordering it models; the dispatch's "your call" clause): `ProtectedCallKey`,
  `SessionIdentity` (the exact `(peer, session_id, establishment)`
  triple — production carries the establishment WHOLE, the full Noise
  handshake hash, where the model abstracts a `u64`),
  `ViewMovement`/`stamp_movement` (the §2.3 discriminator — generation-
  only movement requalifies and REFRESHES, it never retires a sibling),
  `CallLimits`/`ByteLimits` (Q1 defaults 4096/64/512 and 16/64/512 MiB,
  `validate()` at construction), `ByteBudgets`/`ItemPermit`/`SharedPermit`/
  `ChargedChunk` (§2.7: call → caller → node check-before-increment with
  rollback, release-once permits, checked arithmetic — never
  `fetch_add`-then-check), `ProtectedCallRegistry` (reserve → install →
  confirm/transfer → begin_commit → retire → release/complete — the two
  mutually-exclusive incarnation-conditional removal paths of §2.4),
  `ReservationGuard`/`ProtectedCallLease` (the bridge's rollback owner
  before transfer; Drop-releases exactly once), `CommitTxn`/`CommitVerdict`
  (check + enqueue as ONE registry-locked ownership operation), the
  per-node registry map with the five mesh.rs hook fns
  (`org_registry_store_installed`, `org_registry_retire_session`,
  `org_registry_retire_all`, `org_registry_node_dropped`) and the
  fixtures seams (`set_protected_call_registry_for_node`, tiny-limit
  constructors). The §2.3 raise feed is the registry's own
  `RaiseSubscription` (the second `subscribe_floors_raised`), re-created
  on store/authority replacement with the old guard dropped OUTSIDE the
  registry lock (its Drop drains in-flight callbacks).
  **§2.7 at both enqueue boundaries:** `RpcResponseSink` carries
  `RegistryCallRef` for protected records — `send`/`send_wait` reserve
  bytes before submission is acknowledged, `send_wait` parks only on
  satisfiable bounds and is interruptible by retirement, an unsatisfiable
  item fails promptly and LATCHES `ResourceExhausted` + retires (never a
  drop-then-success), and the queue admission (reserved `mpsc::Permit`) +
  the §2.3 check run under one registry lock; the pump and
  `RequestStream` release each item's permit at publish/yield;
  `apply_request_chunk_to_senders` accounts `payload.len()` before the
  queue allocation for charged records (over budget / full mpsc / closed
  sender ⇒ retire `ResourceExhausted` + zero further delivery).
  The supervisor's forced branch settles the registry side first
  (terminal + queued permits + owner signal), and `StreamCallRegistration`
  completes the registry record at the §2.4 single-removal point.
- `cortex/rpc.rs` — **contract 5's lease-carrying seam, early** (the
  F-S1.3-7 shared-helper preference): `apply_inbound_admitted` gains the
  `Option<ProtectedCallLease>` fourth parameter and TRANSFERS ownership
  before any fold effect (§3 step 5); `ProtectedStreamCall` routes
  `retire()` through the registry and observes `retire_reason()` (the
  synchronous §2.3 boundary — "retirement reached the owner"); the unary
  fold's `apply_inbound_admitted` takes the lease too, with a
  `ConfirmedOpening` scope guard completing the record at the spawned
  task's end (or at an effect-boundary refusal — no orphaned `Running`
  records), its retire hook cancelling the token and the response
  override mapping the registry's typed reason through
  `stream_terminal_payload` (byte-identical for plain `Cancelled`).
- `mesh_rpc.rs` — **the shared §3 helper** `admit_protected_opening`
  (+`ProtectedOpeningOutcome`/`OpeningRefusal`): reserve (before decode,
  before any signature work, on `(resolve_direct_caller`'s entity,
  `EventMeta`'s `call_id)`) → decode → digest → provider self-verify →
  `verify_org_admission` with its UNCHANGED step order (the §9.5
  stability recheck, the replay insert at step 10, the provider policy at
  step 11 — guard and policy untouched) → rollback-on-`Err` (the
  reservation guard; the Q4 policy-veto guard slot stays consumed) →
  install under the registry lock with the §2.3 requalification.
  `admit_and_dispatch_protected` (the unary protected bridge) consumes it
  — its previous inline sequence moved verbatim (order preserved,
  including the §6 throttle's position and disposition) — and slice 1.5's
  `admit_and_dispatch_protected_stream` consumes the same helper next.
  `ServeHandle::drop` also settles `retire_registration` for the
  registry's records (protected unary included).
- `mesh.rs` — exactly the five authorized sites: the second
  `subscribe_floors_raised` subscriber at the store install site
  (`install_org_revocation_store_locked`'s tail — bind +
  replacement-retire + re-subscribe, before the install returns), the
  `install_peer_locked` displaced branch, the dead-peer sweep (one
  `registry_node_id` capture beside the sweep's other handles), and
  Main's carve — `Adapter::shutdown` + `Drop for MeshNode` retire-all.
- `tests/org_rpc_streaming.rs` + `tests/org_rpc_streaming/s14.rs` (new) —
  the nine witnesses below; the fifteen prior witnesses adapted
  mechanically (the seam's new `None` lease argument, byte-stable
  assertions).
- `docs/ORGANIZATIONS.md` — §2.3's explicit limitation documented at the
  revocation-floors section: floors cover membership certificates only;
  cross-org capability/dispatcher grant revocation is NOT actively
  enforced during a call (bounded by grant `not_after` + provider policy).

**Witnesses and counts (executed):** `cargo t --retries 0 --test
org_rpc_streaming` → `Summary 24 tests run: 24 passed, 0 skipped`,
**exit 0** — the fifteen prior witnesses still green plus the nine named:

| # | Witness | Property proved |
|---|---|---|
| (a) | `floor_raise_retires_blocked_stream_before_publish_returns` | §2.3's quantified raise boundary: a floor raise for the call's member retires its credit-BLOCKED stream BEFORE `apply_bundle` returns (observed at `retire_reason()` captured at the return), one typed `Revoked` terminal after pump stop |
| (b) | `sibling_stream_of_other_org_sends_next_item_after_publication` | the §2.3 requalification is StampMovement-aware, never whole-stamp equality: a raise for an org-B member (moving EVERY captured generation) leaves the org-A sibling live, its NEXT item commits after the publication, and the commit REFRESHES the captured generation (`CommitVerdict::Proceed` afterwards) |
| (c) | `poisoned_store_retires_all_protected_streams` | poison fails closed: the active stream's next item is refused at the §2.3 commit boundary (poisoned ⇒ `Unusable`) and retires `AuthorityUnavailable`; the empty-slice authority wake retires ALL streams (idle included) before it returns; recovery does not resume them |
| (d) | `store_replacement_retires_all_and_resubscribes` | a `(authority_ptr, store_ptr)` replacement retires every record captured under the old pair before the install returns, and the registry RE-SUBSCRIBES (the new store's raises retire; the replaced store's do not) |
| (e) | `session_replacement_retires_old_call` | the `install_peer_locked` displaced branch retires exactly the replaced establishment (`SessionReplaced` at the transition boundary); the successor — SAME `(caller, call_id)` on the new establishment — is unaffected and completes (the same-id reuse rides the W6 monotonic jump: the guard's retained window refuses same-id reuse inside it with `CallIdCollision`, §3's classification) |
| (f) | `active_call_id_reuse_after_replay_window_is_refused` | D4: a live call id cannot be reused even after the replay guard's window lapsed (`ActiveCallOwned` from `reserve`, before decode — the guard never consulted), while the identical reuse ADMITS (not `Replay`) once the call is gone — proving the refusal was active ownership. The window lapse rides the documented clock pairing (freshness reads `wall_ns`, retention derives from `monotonic`; `ClockSample{ wall_ns, monotonic + 400 s }` is "after expiry + 300 s") |
| (g) | `raise_between_reserve_and_install_denies_with_zero_effects` | §3 step 4: a raise landing between `reserve` and `install` denies `Revoked` with ZERO effects (no lease, org quota never charged, record terminal), and the bridge's rollback frees the key (exactly one removal; reusable) |
| (h) | `queued_bytes_over_call_budget_park_send_wait_and_wake_on_retire` | §2.7 response direction: three in-budget items queue (zero-credit pump keeps every permit charged), the over-budget item PARKS in `send_wait` (satisfiable bound only), and retire WAKES it with the closed-sink refusal |
| (i) | `node_shutdown_retires_live_protected_streams` | Q3/C9 (Main's carve): node shutdown retires that node's live protected streams with the typed `ServeHandleDropped` before the call returns, and a SIBLING node's streams survive and complete normally |

Green at this head (executed):

- The preserved + public-streaming-regression + `subnet_org_boundary`
  batch, ONE invocation (`cargo t --retries 0 --test nrpc_streaming_gate
  --test integration_nrpc_streaming --test
  integration_nrpc_client_streaming --test integration_nrpc_duplex --test
  nrpc_registration_order --test integration_nrpc_protected --test
  org_admission_wire --test subnet_org_boundary`) → **89 run / 89 passed
  / 0 skipped**, exit 0. The preserved trio
  (`client_streaming_denies_unauthorized_caller`,
  `duplex_denies_unauthorized_caller`,
  `denial_is_not_fanned_out_to_the_reply_roster`) ran inside
  `nrpc_streaming_gate` (the roster's last PASS names the third).
- In-source units, ONE invocation (`CARGO_INCREMENTAL=0 cargo tfl
  adapter::net::cortex::rpc adapter::net::mesh_rpc org_stream`) →
  **216 run / 216 passed / 5620 skipped** (214 prior + 2 new §2.7
  units: `byte_reservation_rolls_back_in_order_and_releases_exactly_once`,
  `request_chunk_accounting_retires_the_call_and_stops_delivery`), the
  Stage 0 models green and byte-untouched, the preserved in-source bridge
  trio (`client_stream_bridge_rejects_before_fold_end_to_end`,
  `duplex_bridge_rejects_before_fold_end_to_end`,
  `reject_relayed_flow_controlled_request_rejects_only_relayed_flow_controlled_uploads`)
  inside `adapter::net::mesh_rpc`.
- Frozen provenance hashes UNCHANGED: `d5faa9fe82f76db3…` (old_org_admission.rs),
  `0ab7e915e835533c…` (old_org_call.rs), `9c5963e8b14d003a…` (old_serve.rs).
- The step-10/step-11 ordering is demonstrably untouched: `behavior/org_admission*.rs`
  and `behavior/org_admission_replay.rs` are byte-unmodified by this
  slice (git's change set names no file under `behavior/`), the helper
  calls `verify_org_admission` with the same argument order as the old
  inline sequence (recheck closure, policy closure last), and the
  unit-set's `stability_recheck_runs_after_credential_checks` +
  `every_denial_maps_to_a_defined_coarse_reason` stay green inside
  `org_stream`.
- Scoped rustfmt (`rustfmt --edition 2021 --config skip_children=true`)
  on the five touched files; `mesh.rs` is byte-unchanged by the pass
  (`a55818907d0bfe52…` both sides); post-format shas:
  `cortex/rpc.rs` `d43f31815daa1782…` (pre-format `34b54f634801c751…`),
  `mesh_rpc.rs` `ed3f0d644b4b20e3…` (pre `aca52bcbbae6230a…`),
  `tests/org_rpc_streaming.rs` `7a9fe57eee0c08ab…` (pre `9a4b7066d32f8336…`),
  `s14.rs` `328bfada4ba4b530…` (pre `94be50933f3546ff…`).

**Inverse receipts (executed, raw).** Each mutation is a bounded diff at
the PRODUCTION site; commands run from `net/crates/net/`; restores are
edits-reversed + sha256-proven byte-identical to the pre-format
baseline (`cortex/rpc.rs` `34b54f634801c751…`, `mesh.rs`
`a55818907d0bfe52…`), each closing with a green
`cargo t --retries 0 --test org_rpc_streaming` → `24 tests run: 24
passed`, exit 0.

**INV-A — the prescribed inverse: whole-stamp comparison at commit
points.** Bounded diff at `cortex/rpc.rs` `commit_check_locked`'s
`GenerationOnly` arm (the floor compare + refresh replaced by a retire):
the commit point compares the WHOLE stamp. Command:
`cargo t --retries 0 --test org_rpc_streaming -E
'test(=sibling_stream_of_other_org_sends_next_item_after_publication)'`.
**Exit 100.** Verbatim failure:

```
thread 'sibling_stream_of_other_org_sends_next_item_after_publication' (164876) panicked at tests\org_rpc_streaming.rs:1504:10:
the sibling stream of another org sends its next item after publication: RpcSinkClosed
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
Summary [   0.181s] 1 test run: 0 passed, 1 failed, 23 skipped
```

The prescribed outcome exactly: the sibling of another org is retired at
its NEXT commit after the publication ("the sibling witness retires the
wrong call"), its item refused. Restore: diff reversed; `34b54f63…` ==
baseline. Restored green: 24/24, exit 0.

**INV-B — the prescribed inverse: the raise subscription disconnected.**
Bounded diff at `cortex/rpc.rs` `bind_store` (the
`subscribe_floors_raised` guard dropped immediately — the bind survives,
the raise feed does not). Command: `cargo t --retries 0 --test
org_rpc_streaming`. **Exit 100.** Verbatim failure (the named witness —
a bounded-window TIMEOUT, the prescribed shape):

```
thread 'floor_raise_retires_blocked_stream_before_publish_returns' (51376) panicked at tests\org_rpc_streaming.rs:1342:5:
the blocked stream must be retired by the raise
note: run with `RUST_BACKTRACE=1` environment variable to display a back trace
Summary: FAIL [  30.203s] (24/24) net-mesh::org_rpc_streaming floor_raise_retires_blocked_stream_before_publish_returns
```

Grouped reds at their own subscription-dependent properties (one
mutation, one run): (b) `the raised member's stream retires at the
publication boundary / left: None / right: Some(Revoked)`;
(c) `the empty-slice wake retires ALL protected streams before it
returns / left: None / right: Some(AuthorityUnavailable)`;
(d) `the registry is subscribed to the NEW store / left: None / right:
Some(Revoked)`; (g) `the notified callback moved the epoch / left: 1 /
right: 2`. Restore: both shas == baseline. Restored green: 24/24.
(A first, coarser bounded diff — commenting out the whole site-1 hook —
is disclosed: it reddened eight witnesses at their ADMISSIONS
(`ProviderAuthorityUnavailable` — the unbound registry refuses) rather
than at the raise boundary, so it is not the prescribed inverse; the
refined diff above is.)

**R1 — `reserve`'s live-key refusal removed** (`if false &&
contains_key`). Command: `-E
'test(=active_call_id_reuse_after_replay_window_is_refused)'`. **Exit
100.** Verbatim: `panicked at tests\org_rpc_streaming.rs:1971:6: an
ACTIVE call id cannot be reused after the replay window:
(b"\x10\0\0\0\x11\x11…")` (the `expect_err` received the ADMITTED
lease). Restore + green 24/24.

**R2 — `install`'s §2.3 requalification neutered** (`if false &&` on
the movement branch). Command: `-E
'test(=raise_between_reserve_and_install_denies_with_zero_effects)'`.
**Exit 100.** Verbatim: `panicked at tests\org_rpc_streaming.rs:2064:10:
install must deny after the raise: ProtectedCallLease { key:
ProtectedCallKey { caller: EntityId(58936604abda112b...), call_id: 42 },
incarnation: 1, registration: 1, session: SessionIdentity { peer: 10,
session_id: 81, establishment: Some([161 × 32]) } }`. Restore + green.

**R3 — the `install_peer_locked` displaced-branch hook removed.**
Command: `-E 'test(=session_replacement_retires_old_call)'`. **Exit
100.** Verbatim: `assertion 'left == right' failed: the displaced
session's call retires at the replacement boundary / left: None / right:
Some(SessionReplaced)`. Restore (`mesh.rs` `a5581890…` == baseline) +
green.

**R4a — the empty-slice retire-all removed** (`if false &&
raised.is_empty()` in `on_floors_raised`). Command: `-E
'test(=poisoned_store_retires_all_protected_streams)'`. **Exit 100.**
Verbatim: `assertion 'left == right' failed: the empty-slice wake
retires ALL protected streams before it returns / left: None / right:
Some(AuthorityUnavailable)`. Restore + green.

**R4b — the poison terms dropped from `stamp_movement`** (poison no
longer makes the view unusable). Same command. **Exit 100.** Verbatim:
`panicked at tests\org_rpc_streaming.rs:1600:5: a poisoned store refuses
further items` (the commit-point refusal is gone — the item goes
through). Restore + green.

**R5 — the replacement's retire-old-pair selection removed** (`&& false`
in `bind_store`'s victims filter). Command: `-E
'test(=store_replacement_retires_all_and_resubscribes)'`. **Exit 100.**
Verbatim: `assertion 'left == right' failed: records captured under the
old (authority_ptr, store_ptr) retire before the install call returns /
left: None / right: Some(AuthorityUnavailable)`. Restore + green.

**R6 — the §2.7 wake.** Two bounded diffs, disclosed in order. (1)
Removing ONLY the terminal wake in `retire_locked` left the witness
**GREEN** (`Exit 0`, 1/1): retirement also wakes the parked producer
through the queued permits' `release_charge` notify — the two wake paths
are redundant for a call with queued items, and the diff is therefore
**not the property's own inverse** (the F-S1.2-1 classification: none of
the four weakenings applies — the witness is not weakened; the mutation
was partial). (2) The property's true inverse — BOTH notifies removed
(`release_charge`'s and the terminal wake): Command: `-E
'test(=queued_bytes_over_call_budget_park_send_wait_and_wake_on_retire)'`.
**Exit 100.** Verbatim: `FAIL [  10.392s]` … `panicked at
tests\org_rpc_streaming.rs:2163:10: the parked send_wait must wake on
retire: Elapsed(())` — a wake-on-retire TIMEOUT past its 10 s bound.
Restore (both hunks) + green.

**R7 — the node-shutdown hook removed (Main's named inverse).** Bounded
diff at `mesh.rs` `Adapter::shutdown` (the `org_registry_retire_all`
call commented). Command: `-E
'test(=node_shutdown_retires_live_protected_streams)'`. **Exit 100.**
Verbatim: `FAIL [  30.244s]` … `panicked at
tests\org_rpc_streaming.rs:2268:5: the live protected stream must be
retired by node shutdown` — the bounded wait elapsed. Restore (`mesh.rs`
== baseline) + green.

No witness stayed green under its own inverse except R6(1), which is
analyzed above as a partial (non-)inverse with its true inverse red.

**Findings** (state, not decide):

1. **F-S1.4-1 — the model's `Denial` vocabulary collapses onto C4
   `AdmissionDenied`, and the two coarse mappings disagree on `Revoked`
   (source-established).** The production mappings used:
   model `ActiveCallOwned`/`Replay`/`CallIdCollision`/`PolicyVetoed`
   stay inside `verify_org_admission` (untouched);
   `ActiveStreamCapacity(scope)`/`CounterOverflow` →
   `AdmissionDenied::ActiveStreamCapacity` (the C4 variant is
   unscoped); `AuthorityUnavailable` → `ProviderAuthorityUnavailable`;
   `VerificationDeadlineExpired`/`SessionCurrentnessExhausted`/`NotAdmitted`
   → `AuthorityChanged` (all coarse `Unavailable` as §3 requires);
   `Revoked` → `AdmissionDenied::Revoked`. The inconsistency: the model's
   `Denial::coarse()` (`behavior/org_stream_registry.rs`) sends `Revoked`
   → `Unavailable`, while the C4 pinned mapping sends it → `Denied`
   (`behavior/org_admission.rs:317-319`, pinned by
   `every_denial_maps_to_a_defined_coarse_reason`); production follows
   C4 (`org_admission*.rs` is must-not-touch for this slice). A distinct
   wire bucket — or the model's alignment — is a ruling.
2. **F-S1.4-2 — the `SessionCurrentness` generation is unreachable from
   the admission sites (source-established).** `SessionCurrentness` is
   `pub(crate)` (`behavior/org_routing_registry.rs:492-537`) behind
   `mesh.rs`'s private `NodeSessionRouting.currentness` (`mesh.rs:9579`);
   no `MeshNode` accessor exists. The seam carries the check
   (`OpeningRequest.session_generation: None` ⇒ refuse
   `AuthorityChanged`, coarse `Unavailable`, exactly §3's
   `u64::MAX`-exhausted rule) and the witnesses pass real values, but
   `admit_and_dispatch_protected` feeds `Some(0)` ("a live,
   non-exhausted generation") — the refusal is wired and never fed the
   live generation at the unary site. A `MeshNode` accessor would close
   it; that is API-addition/ruling territory.
3. **F-S1.4-3 — §2.3's "Poison: same boundary via the empty-slice
   notify" is only partly true of the real store
   (source-established + executed).** The empty-slice wake
   (`notify_authority_changed`) fires on the production durability-
   uncertain mark path (`org_revocation.rs:2163-2167`), on
   `init`/`open_existing` over a poisoned path (`:1572-1575`,
   `:1628-1631`) and on `apply_bundle` recovery (`:2137-2139`); the
   fixtures seam `mark_poisoned_for_test` (`:1706-1709`) MARKS without
   waking. Witness (c) therefore drives the mark boundary through the
   §2.3 commit point (poisoned ⇒ `Unusable` ⇒ refuse + retire — "further
   items refused", receipts R4b) and the retire-ALL through the real
   recovery wake (receipt R4a). A test-only wake-at-mark seam would make
   the mark boundary directly drivable — stated, not added.
4. **F-S1.4-4 — `VerifiedFacts.deadline` is `Option` in production where
   the model requires it (source-established).** The model's
   `VerifiedFacts.deadline` is mandatory; production carries
   `Option<ResolvedStreamDeadline>` — `None` for unary records at this
   slice, because the §2.1 machinery is streaming-scoped (1.3's
   `StreamCallLifetime`) and enforcing it on unary would change unary
   behavior ("preserving unary behavior"). Streaming records resolve at
   the fold as before.
5. **F-S1.4-5 — the production session identity is more precise than the
   model's (source-established).** The model's
   `SessionRef.establishment: u64` abstracts "the exact handshake that
   produced this session"; production carries it whole
   (`NetSession::handshake_binding`, the full Noise transcript hash) in
   `SessionIdentity::establishment: Option<[u8; 32]>`. The semantics the
   model proves (exact-triple matching; a bare truncated session id
   never retires a bystander) are preserved verbatim.
6. **F-S1.4-6 — Main's carve phrasing is realized one layer down
   (source-established).** The carve named "ONE call to
   `ProtectedStreamOwners::retire_all`" at each shutdown site; the calls
   landed are `org_registry_retire_all` / `org_registry_node_dropped`
   (one call per site). Those fire the SAME `ProtectedStreamCall::retire`
   signals `ProtectedStreamOwners::retire_all` fires, plus the
   synchronous registry-record marking the §2.3 commit-point exclusion
   requires — a bare `retire_all` would leave registry records unmarked
   until the supervisors' async cleanup, reopening a commit-after-
   shutdown window. The record's owner hook IS the 1.3
   `StreamRetireSignal` (same handle `retire_all` drives).
7. **F-S1.4-7 — §2.7's "unknown sender for an admitted call ⇒ retire" is
   not attributable from the wire key (source-established).** The
   request-chunk boundary can retire a call whose sender entry exists
   (the charge rides the entry: over budget / full mpsc / closed sender
   all retire `ResourceExhausted` + stop delivery), but a TRULY unknown
   key drops exactly as the plan's pre-admission rule requires
   ("Unknown-key CHUNK dropped without allocation") — the frame carries
   `(origin_hash, call_id)`, not the registry's authenticated
   `(EntityId, call_id)`, and `origin_hash` is a collidable u64
   (`origin_hash_to_node` is first-write-wins), so an unknown key cannot
   be safely attributed to a record. The charged request paths become
   reachable with Stage 2's protected CS/DX records; until then they are
   covered by the in-source unit
   `request_chunk_accounting_retires_the_call_and_stops_delivery`.
8. **F-S1.4-8 — seam shape (the F-S1.3-7 precedent, stated).** The
   §3 transaction lives in the shared helper `admit_protected_opening`
   (the unary bridge consumes it now; 1.5's stream bridge consumes it
   next) and contract 5's lease-carrying `apply_inbound_admitted` landed
   early with `Option<ProtectedCallLease>` — `None` preserves the 1.3
   witnesses' registry-less synthetic idiom, `Some` is the production
   path.
9. **F-S1.4-9 — implementation home of the model mirror
   (source-established).** The registry semantics are implemented in
   "the production module(s) the model mirrors" (`cortex/rpc.rs` +
   `mesh_rpc.rs` — the model's own doc names `admit_and_dispatch_protected`
   as the ordering it models), the 1.3 precedent (lifecycle →
   `cortex/rpc.rs`); `behavior/org_stream_registry.rs` is byte-untouched,
   so the frozen model surface needed no Main ruling. The model-mirror
   operations not yet wired to a production caller (kept as the faithful
   surface, named in the never-executed list): `reap_expired_openings`,
   `ItemPermit::transfer`, `cancel_queued`, `queued_items`,
   `store_poisoned`, `SharedPermit::is_consumed`, `cleanup_owner`.
10. **F-S1.4-10 — the `Revoked` coarse byte reaches the wire as `Denied`
    for mid-stream retirement too (source-established).**
    `stream_terminal_payload` maps `Revoked`/`AuthorityUnavailable` →
    `AdmissionDenied` + coarse `[0]` (`Denied`) and `ResourceExhausted` →
    `[2]` (`Unavailable`), consistent with the C4 mapping of F-S1.4-1;
    the §3 denial text's parenthetical "`Revoked` (`Unavailable`)" (the
    model's `coarse()`) is the disagreement named there.

**CI pin data for Main (never edited here):** binary
`org_rpc_streaming`, floor **24** (15 prior + 8 named + 1 from Main's
shutdown carve — `15+N+1`), REQUIRED names exactly the twenty-four:

`stream_opening_admits_same_org`, `stream_opening_admits_cross_org`,
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
`public_client_stream_deadline_expiry_is_typed_timeout`,
`floor_raise_retires_blocked_stream_before_publish_returns`,
`sibling_stream_of_other_org_sends_next_item_after_publication`,
`poisoned_store_retires_all_protected_streams`,
`store_replacement_retires_all_and_resubscribes`,
`session_replacement_retires_old_call`,
`active_call_id_reuse_after_replay_window_is_refused`,
`raise_between_reserve_and_install_denies_with_zero_effects`,
`queued_bytes_over_call_budget_park_send_wait_and_wake_on_retire`,
`node_shutdown_retires_live_protected_streams`.

(suggested filter-set: the same twenty-four space-separated.)

**What never ran here (complete):**

- `cargo fmt -p <crate> -- --check`, the four clippy invocations, the
  five rustdoc lines and `cargo check --workspace --all-targets` —
  Main's stage-end list (mid-flight rule). The five touched files were
  converged with scoped `rustfmt --config skip_children=true` and the
  lib/units/integration graphs compile clean on the alias feature sets.
- `cargo tl` / `cargo t` full suites; `tests/cross_lang_*`; the wire
  suite; the browser/SDK surfaces — stage-end or other lanes'.
- CI itself (branch unpushed; nobody pushes but Main).
- **The request-direction charged paths through a REAL protected CS/DX
  record** — Stage 2's seam feeds them; at 1.4 they are wired at
  `apply_request_chunk_to_senders` and covered only by the in-source
  unit above.
- **The unary mid-handler retirement's typed terminal** (the registry
  reason mapped through the unary response override) — constructed per
  §2.2/§2.3 and reachable (the retire hook cancels the token; the
  override maps `stream_terminal_payload`), but no NAMED witness drives
  a unary record's mid-handler revocation.
- **`session_generation: None`'s refusal** (the `u64::MAX`
  `SessionCurrentness` exhaustion, F-S1.4-2) — never executed at any
  seam; `admit_and_dispatch_protected` feeds `Some(0)`.
- **`reap_expired_openings`** (§3's lost-bridge reaper) — no production
  caller at 1.4 (every bridge reservation is RAII-released inline);
  `ItemPermit::transfer`, `cancel_queued`, `queued_items`,
  `store_poisoned`, `SharedPermit::is_consumed`, `cleanup_owner` — the
  faithful model surface of F-S1.4-9, compiled, never executed.
- The `ReservationGuard`/`ProtectedCallLease` double-release panics
  (the checked-subtraction `expect`s) — by construction unreachable; no
  witness drives a double release (driving one would be the bug the
  `expect` exists to catch).
- Windows workstation only: `#[cfg(unix)]` legs never compile here; no
  Linux/macOS execution.
