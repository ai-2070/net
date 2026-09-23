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
- **2026-09-22, Stage 3 verified (executed).** Landed over six commits
  (`d63e9c615`…`b7aaacdc5`, rows 3.1–3.3 pairs; §6). Estate at head:
  `org_streaming` **10/10** at the plan's named command, SDK org estate
  338/338, probe exact commands exit 0/0 (`metadata --locked`,
  `check --locked`), `cargo fmt -p net-mesh-sdk -- --check` exit 0. The
  REQUIRED 3.1 pin receipt executed: "resolve a second provider mid-call"
  at `OrgClientStreamCall::send` → red at the named pin assertion (exit 100),
  sha-proven restore `bf302b5e…` == baseline, re-green 1/1. **F-S3.2-1
  resolved per the Main ruling:** the five `pub(crate)` `from_raw`
  constructors landed with zero field-visibility moves and the probe pin
  passed UNCHANGED at 3.2's head (the no-public-change proof). CI floor
  pinned: the `org_streaming` JUnit roster gate (floor 10 + the ten names,
  `--self-test` first) beside the SDK sweep that executes the binary.
  **F-S3.1-2 stated for review adjudication (source-established + executed
  with the drop witness):** the protected-path handler cannot be a sound
  cancel-observer — the retire supervisor may drop the handler future
  without a final poll; the live contract is the retirement observables, and
  no core change is proposed or needed. Also recorded: the session's third
  ENOSPC episode (F16-class) — the casualty was a failed CREATE of
  `tests/org_streaming.rs` (nothing corrupted; the three pre-failure writes
  green-verified at `cargo check -p net-mesh-sdk --features "net cortex
  fixtures"` before the failure and re-checked after), the lane reclaimed
  regenerable caches under the ruling that junit/floor artifacts are not
  evidence (`run_binary` regenerates them per run), and the
  verify-size-after-write rule held throughout. Stage 3 proceeds to
  independent review.
- **2026-09-22, S2R repair round verified (executed).** All three
  S2Review ACCEPT findings closed at `1d26bc4ba` (commits `c17572f03` code,
  `1432c6c03` receipts, `1d26bc4ba` record). Row 2's guard landed as the
  `ConfirmedStreamOpening` Drop-guard (the unary `ConfirmedOpening`
  precedent) in BOTH `apply_inbound_admitted` CS/DX seams — armed at
  `registry.confirm`, Drop runs the release-once `complete` + every map
  entry, defused at the spawn transfer — with the inline F-S2.2-5
  settlement FOLDED into it (one mechanism) and R-S2.2c's fail-pre-fix
  property preserved byte-identically (R-S2.2c-v2). Coordinator spot-check
  SCR1 reproduced the Row-2 probe receipt: the guard's settlement removed →
  `post_transfer_scope_guard_never_orphans_a_running_record` red (the orphan
  manifests), `git checkout`-restored clean. Row 1's REQUIRED PAIR both red
  at the same named no-latch assertion (`:5379:5`); Row 2's probe red at
  the reviewer's §6.5 orphan outcome verbatim (`left: 1 / right: 0`); four
  weakenings NONE; one superseded first cycle (red at an `.expect` belt →
  assertion restructured to map-form → every cycle re-ran post-amend)
  disclosed. Row 3's wording fixed with the normalisation digests (F-S2.4-2
  scoped). Estate at head: `org_rpc_streaming` 42/42 (CI floor 42),
  in-source 221/221, preserved+controls 134/134, cross-lang 32/32, Stage 0
  models untouched 76/76. The owner-declined rider untouched. Stage 2 is
  fully closed; Stage 3 proceeds.
- **2026-09-22, Stage 2 verified (executed).** Landed over ten commits
  (`16cd67e85`…`98df0bb0b`, S2.1–S2.5 pairs; §4). Coordinator spot-checks
  reproduced two Stage-2 receipts with the full cycle: SC3 — R-S2.2c (the
  F-S2.2-5 fix removed at the CS seam) reddened
  `opening_body_budget_refusal_completes_the_record` (the pre-supervisor
  refusal's record leak), restored clean; SC4 — R-S2.1 (the body digest
  skipped in `org_request_digest`'s canonical construction at
  `src/adapter/net/org_admission_gate.rs`) reddened
  `client_stream_opening_binds_first_chunk`, restored clean. **F-S2.2-5
  credited:** a real pre-supervisor opening-body refusal leak found AND
  fixed in the CS/DX seams (the unary `ConfirmedOpening` scope-guard
  precedent) with the fail-pre-fix regression witness and its receipt.
  Disclosures accepted: **F-S2.1-3** (the S2.1/S2.2 commit boundary is
  RECONSTRUCTED — one lane implemented both before committing; each
  reconstructed tree probe-run green at the named probe worktree) and
  **F-S2.4-1** (`retire_unblocks_both_directions` green under the
  single-layer inverse because the property is doubly defended — the TRUE
  two-layer inverse R-S2.4c-v2 reds at the named assertion; the
  overdetermination pattern, disclosed with its discriminating receipt, as
  in Stage 0's F9). The F-S1R-2 rider (owner-declined) was not needed by
  any closure. Estate at head: `org_rpc_streaming` 41/41 (CI floor 41),
  in-source 220/220 three-module, preserved + controls 134/134, cross-lang
  32/32, Stage 0 models untouched at 76/76. Stage-end validation sweep
  next; Stage 2 proceeds to independent review.
- **2026-09-22, STAGE 1 ACCEPTED — independent review round 2.1 (verdict
  packet `S1_REVIEW_PACKET_2.md`, pinned head `cc15f4d66`).** All nine
  predecessor findings F-1…F-9 closed by their verbatim closure properties
  (F-5 via the Main-ruled amended seam wording + R-A5′ red at `:3264:9`;
  the A5 green correctly classified as webrtc-gated, source-established).
  The Exit paragraph's first claim now HOLDS as one executed
  content-correlated observation per authority mode. Estate reproduced
  (30/30, 219/219, 121/121, roster 30==30); all six prior inverses re-run
  red at named assertions; 5 repair receipts re-executed verbatim; **five
  fresh lane-unused inverses all red — zero green-under-inverse, zero
  weakenings.** Adjudications: **F-S1R-2 = documented limitation** (the
  reviewer's own probe: 3/3 retire yet 0/3 terminals delivered within 8 s;
  the NoSession-at-send drop is design-blessed at `mesh_rpc.rs:7893-7897`
  per §2.8) with a **Stage-2 rider question surfaced for the owner** (the
  "post-replacement terminal that cannot be silently dropped" enhancement,
  to be composed with the receiving-incarnation fence and the
  exactly-one-terminal rule, plus the never-executed caller-side last mile:
  the caller fold's local termination after its OWN session replacement);
  **F-S1R-3 resolved for the repair's `f31eb08e…` procedure** — the record
  is accurate, the packet's `a1c518dd…` is superseded (20 framings refute
  it). Stage 2 proceeds as written in the plan; the rider is owner-pending
  (no ruling implies its inclusion).
- **2026-09-22, repair round (S1_R) verified (executed).** The HOLD closure
  landed over five commits (`60c287d1e` witnesses rows 1–8, `4ab5738cf` six
  Appendix-inverse receipts, `74c9bd99c` §3, `3620fed0b`+`08dd849e2` R-A5'
  and the §3.4 hash side-by-side). Coordinator spot-checks in the isolated
  worktree at `74c9bd99c` reproduced two repair receipts: SC1 — A2c
  (`end`→`continue`) reddened
  `completed_stream_drains_queued_items_in_order_with_content_and_end_terminal`;
  SC2 — A3 (node-arm rollback deleted) reddened
  `node_budget_refusal_rolls_back_call_and_caller_reservations`; both
  `git checkout`-restored with a clean tree. **F-S1R-1 resolved (a)-sharpened,
  executed:** R-A5' flips the three `RpcInboundEvent` attribution stamps at
  the observable ingress seam the witness reads → red at its own named
  assertion (`:3256:9`, `left: 0 / right: 17873330928486580960`), sha-restored
  to the packet's own `a5581890…` mesh.rs baseline — the witness
  discriminates and stands on its receiver-side property with the closure
  wording naming the observable seam (amended, not weakened). The lane also
  corrected the coordinator's suggested inverse with evidence: no retained
  stale session exists at any selectable seam (`install_peer_locked` consumes
  the displaced `NetSession` and its `session_id_to_node` entry; ingress
  verifies the claimed id against the carrying one), so the attribution flip
  is the discriminating mutation available — credited as a correction.
  F-9's 52-count label fixed above (filter semantics: 39 + 13). F-S1R-2
  (protected `SessionReplaced` terminal emitted-but-undelivered across a 30 s
  executed construction; mechanism [INFERENCE]) and F-S1R-3 (the packet's
  `a1c518dd…` framing unreproducible across 6 digests × 3 framings vs the
  recorded reproducible `f31eb08e…` procedure, raw `fa275454…` exact) are
  recorded with side-by-side evidence for review round 2.1 adjudication.
- **2026-09-22, coordinator spot-checks (between-lanes window, executed).**
  Two deferred receipts independently reproduced with the full cycle
  (mutated red → `RESTORED_BYTE_IDENTICAL` → green):
  (1) **S1.5's REQUIRED DirectOnly flip** — the protected arm of
  `ProtectedAdmission::response_route_fallback` (`mesh_rpc.rs:7901-7904`)
  flipped to `RosterOnStaleDirect` redden
  `streaming_denial_is_not_fanned_out_to_the_reply_roster` byte-identically
  to the lane's quote ("1 frame(s) reached the roster subscriber — a
  protected response was fanned out / left: 1 / right: 0" at
  `tests/org_rpc_streaming/s15.rs:202:9`).
  (2) **S1.6's Revoked-mapping receipt** — `AdmissionDenied::coarse()`'s
  `Revoked` moved to `Unavailable` redden
  `every_denial_maps_to_a_defined_coarse_reason` ("left: Unavailable /
  right: Denied" at `org_admission.rs:1895:13`) — the production-side proof
  of the F-S1.4-1 ruling and its extended pin. Slices 1.5 and 1.6 accepted
  at coordinator level. The remaining deferred receipts (R-1.1a; 1.2's six;
  1.3's six; 1.4's ten; 1.5's other two; 1.6's other three) are queued for
  review round 2's isolated-worktree re-executions.

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

### 2.4 Slice 1.5 — bridge wiring + routing

**Landed (executed):** `444a4aab9` — `S1.5: wire protected streaming
admission into the serve bridges` (4 files, +1483/−91) on
`LZL0/org-streaming`; this record rides in the following `S1.5:` commit.
Base `cddb063a3` (Main's `Correct model Revoked coarse mapping per plan
4.3 (F-S1.4-1)` + its §0 record — landed in the shared tree at 11:16:53,
BEFORE this slice's first compile; the `org_stream_registry` model file is
byte-untouched BY this slice and every green claim below includes Main's
alignment). The §2.4 unit counts were re-verified at the exact head
(`9e4642e0b`'s tree): 216/216 + 19/19 unchanged. All green claims below
are at
`444a4aab9`'s exact tree (the scoped rustfmt pass is whitespace-only and
landed inside it; the receipt cycles ran on the pre-format tree — the
1.1a/1.4 precedent — with post-format shas recorded below).

**What landed** (source-established; executed via the runs below):

- `mesh_rpc.rs` — contract 5 in full:
  - **`admit_and_dispatch_protected_stream`** at the streaming bridge's
    fold-drive seam (the brief's `BridgePreflight::Proceed(frame)` seam),
    consuming 1.4's `admit_protected_opening` +
    `apply_inbound_admitted` unchanged: reserve (before decode/signature
    work) → decode/digest → provider self-verify → `verify_org_admission`
    with its UNCHANGED step order (the §9.5 recheck, the replay insert at
    step 10, the provider policy at step 11) → rollback-on-`Err` (the
    reservation guard; the Q4 veto slot stays consumed) → install with the
    §2.3 requalification → the fold seam's ownership TRANSFER. Admission
    is synchronous in the SAME bridge iteration as the fold drive (the
    unary bridge's exact `match reg.admission()` shape: public →
    `bridge_preflight` + `apply_inbound`; protected → the §3 transaction).
    §1.3: `session_binding = mesh.peer_session_binding(from_node)` (the
    receiving session's full Noise handshake hash; `None` fail-closed at
    step 9b). Every refusal is exactly one bounded denial through the
    byte-unchanged `emit_admission_denial` (`DirectOnly`, NC2); the fold
    emits nothing on refusal.
  - **`serve_rpc_owner_scoped_streaming` / `serve_rpc_granted_streaming`**
    `(service, Arc<H>, OrgProviderPolicy)` — both land in
    **`serve_rpc_streaming_impl(shape, admission)`** beside
    `serve_rpc_unary_impl` (the public `serve_rpc_streaming` wrapper's
    signature is unchanged — `net_sdk` and `guards/org_api_probe`
    unaffected; executed green below). The seams require an installed node
    authority (`ProtectedAuthorityRequired`) and construct the same
    `RegisteredRpcService` shapes the unary seams do (owner-scoped =
    `OwnerDelegated` + `OwnerScoped` visibility; granted =
    `CrossOrgGranted` + `GrantedAudience`).
  - **`UnaryAdmission` → `ProtectedAdmission`** (C10) with the E1.8 doc
    line rewritten: server-streaming HAS a protected form now (contract
    5); client-streaming and duplex still have none. Its
    `response_route_fallback` / `visibility` are semantically untouched
    (every protected mode stays `DirectOnly`).
  - **Protected emitters (NC2):** the SS emit closure routes `DirectOnly`
    naming the authenticated session peer explicitly (`Some(from_node)`)
    and carries the record's REAL `session_id` in `RpcResponseJob` —
    the receiving incarnation captured at admission (R2-A: "the
    incarnation that received the request"), now stored as the route
    cache's `(target node, receiving session_id)` pair. PROTECTED
    terminals ride the §8a `RpcResponseJob` drainer the streaming impl now
    owns — the F-S1.3-5 deferral: `Sent` / `Refused` / `Unreachable` are
    separately distinguishable at the drainer/route layer (structured
    logs; `Refused` is the §2.8 bounded failure disposition — the peer
    observes interruption or its deadline, never synthetic success).
    Chunks publish inline (the pump's per-chunk await keeps wire order and
    releases each §2.7 byte permit at the actual publish). Public emitters
    keep the AV-5 `RosterOnStaleDirect` inline path, byte-compatible.
  - **§2.1 inputs:** `ProtectedOpeningOutcome::Admitted` carries
    `credential_ends_ns` — the membership / dispatcher / (optional)
    capability-grant `not_after` values plus the installed owner cert's
    end ("include the provider's required authority validity"), each a
    checked seconds→ns normalization — feeding
    `StreamCallLifetime { policy: q1_defaults(), .. }` (300 s / 3600 s)
    at the fold seam.
  - **ServeHandle fixtures seams** (`#[cfg(any(test, feature =
    "fixtures"))]`, the `origin_node_cache`/`streaming_fold_for_test`
    precedent): `inject_inbound_for_test` (the bridge dispatcher's exact
    bounded-mpsc hand-off — preflight, §3 admission and fold drive run in
    production order in one bridge iteration) and
    `request_fold_for_test` / `duplex_fold_for_test` (the input-delivery
    map observation below).
- `tests/org_rpc_streaming.rs` + `tests/org_rpc_streaming/s15.rs` (new) —
  the three witnesses below (driving REAL registrations + REAL wire
  endpoints: the NC2 probe idiom from `nrpc_streaming_gate.rs:268-305`);
  `s14.rs` adapted mechanically (the outcome destructure).

**Witnesses and counts (executed):** `cargo t --retries 0 --test
org_rpc_streaming` → `Summary 27 tests run: 27 passed, 0 skipped`,
**exit 0** — the twenty-four prior witnesses still green plus the three
named:

| # | Witness | Property proved |
|---|---|---|
| (a) | `forbidden_stream_opening_causes_zero_handler_effects` | a structurally perfect but MODE-forbidden opening (an unexpected cross-org capability grant at an OwnerDelegated registration — §1.5/step 6) causes ZERO effects: handler entry and SINK SENDS stay 0 through the bounded darkness window, the wire carries EXACTLY the one coarse denial (no chunk, no grant, no other terminal), the roster subscriber sees nothing, `in_flight_keys()` is empty, no flow/grant semaphore is installed, and the §3 reservation rolled back (registry `record_count`/`active_node` 0). The plan's `sender_keys` channel is observed too: the same opening shape at real client-streaming and duplex registrations leaves `in_flight_keys()` AND `sender_keys()` empty (their folds refuse the shape before any call state) |
| (b) | `streaming_denial_is_not_fanned_out_to_the_reply_roster` | the streaming NC2 witness (the `nrpc_streaming_gate.rs:268-305` bystander probe against `serve_rpc_owner_scoped_streaming`): leg (a) — a denied opening on the caller's REAL session is told to the caller EXACTLY once (0x0009 + the coarse `Denied` byte `[0]`; §T7's "the caller must be told") and the bystander subscribed to the caller's reply-channel roster sees NOTHING across a 1 s window; leg (b) — the NC2 reflection trigger (the same no-proof opening claiming the caller's origin from an UNROUTABLE peer: the denial's direct route is gone — the `NoSession` trigger `response_route_fallback`'s doc names) is DROPPED by `DirectOnly`, never roster-fanned onto the claimed origin's channel |
| (c) | `provider_policy_veto_denies_before_effects` | Q4 + the §3 step-10/11 order: a fully VALID owner-delegated proof the provider policy vetoes denies before effects (zero items, zero handler entry across the darkness window, the roster silent) AND the active reservation is RELEASED while the replay slot stays CONSUMED — the byte-identical re-submission is refused at the replay insert (step 10, before the policy at step 11) without re-running the policy (`policy_calls` stays exactly 1) and with zero effects again |

Green at this head (executed):

- Preserved + public-streaming-regression + `subnet_org_boundary` batch,
  ONE invocation (`cargo t --retries 0 --test nrpc_streaming_gate --test
  integration_nrpc_streaming --test integration_nrpc_client_streaming
  --test integration_nrpc_duplex --test nrpc_registration_order --test
  integration_nrpc_protected --test org_admission_wire --test
  subnet_org_boundary`) → **89 run / 89 passed / 0 skipped**, exit 0. The
  preserved trio (`client_streaming_denies_unauthorized_caller`,
  `duplex_denies_unauthorized_caller`,
  `denial_is_not_fanned_out_to_the_reply_roster`) ran inside
  `nrpc_streaming_gate` (the roster's last PASS names the third).
- In-source units, ONE invocation (`CARGO_INCREMENTAL=0 cargo tfl
  adapter::net::cortex::rpc adapter::net::mesh_rpc org_stream`) →
  **216 run / 216 passed / 5620 skipped** — count-continuous with 1.4's
  216 (this slice adds NO in-source units): the preserved in-source bridge
  trio (`client_stream_bridge_rejects_before_fold_end_to_end`,
  `duplex_bridge_rejects_before_fold_end_to_end`,
  `reject_relayed_flow_controlled_request_rejects_only_relayed_flow_controlled_uploads`)
  inside `adapter::net::mesh_rpc`, and the Stage 0 models
  (`org_stream_lifecycle` / `org_stream_registry`) green and byte-untouched BY THIS SLICE (the registry model carries Main's `cddb063a3` ruling alignment).
- The admission unit pins (`cargo tfl behavior::org_admission::`) →
  **19 run / 19 passed / 5817 skipped** (incl. the 1.6 pin-region units
  `malformed_and_streaming_are_distinct`,
  `stability_recheck_runs_after_credential_checks`,
  `every_denial_maps_to_a_defined_coarse_reason` — green and unmodified).
- Post-format green at the exact commit tree: `cargo t --retries 0 --test
  org_rpc_streaming` → **27/27**, exit 0.
- Frozen provenance hashes UNCHANGED: `d5faa9fe82f76db3…`
  (old_org_admission.rs), `0ab7e915e835533c…` (old_org_call.rs),
  `9c5963e8b14d003a…` (old_serve.rs).
- Scoped rustfmt (`rustfmt --edition 2021 --config skip_children=true`)
  on the four touched files; `--check` clean. Post-format shas:
  `mesh_rpc.rs` `15fa31b9fe11bf5e…` (pre-format receipt baseline
  `06cd466bc423f3b7…`), `tests/org_rpc_streaming.rs`
  `1152cf264405663b…`, `s14.rs` `708bb1142657af83…`, `s15.rs`
  `0b741589fb6c5fda…`. `behavior/org_admission.rs` restored
  byte-identical at `33892cead2e82dde…` (receipt INV-C's baseline).

**Inverse receipts (executed, raw).** Each mutation is a bounded diff at
the PRODUCTION site; commands run from `net/crates/net/`; restores are
edits-reversed + sha256-proven byte-identical to the pre-format
baselines (`mesh_rpc.rs` `06cd466bc423f3b7…`, `org_admission.rs`
`33892cead2e82dde…`), each closing with a green `cargo t --retries 0
--test org_rpc_streaming` → `27 tests run: 27 passed`, exit 0.

**INV-A — the prescribed inverse (REQUIRED): `DirectOnly` flipped to
`RosterOnStaleDirect`.** Bounded diff at `mesh_rpc.rs`
`ProtectedAdmission::response_route_fallback`'s protected arm (the
`:7836` literal). Command: `cargo t --retries 0 --test
org_rpc_streaming -E
'test(=streaming_denial_is_not_fanned_out_to_the_reply_roster)'`.
**Exit 100.** Verbatim failure:

```
thread 'streaming_denial_is_not_fanned_out_to_the_reply_roster' (160944) panicked at tests\org_rpc_streaming\s15.rs:202:9:
assertion `left == right` failed: the bystander for a denial whose direct route is gone: 1 frame(s) reached the roster subscriber — a protected response was fanned out
  left: 1
 right: 0
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
Summary [   1.218s] 1 test run: 0 passed, 1 failed, 26 skipped
```

The prescribed outcome exactly: the bystander (a same-origin roster
subscriber of the claimed origin's reply channel) RECEIVES the terminal
the moment the fallback flips. Restore: diff reversed;
`06cd466bc423f3b7…` == baseline. Restored green: 27/27, exit 0.

**INV-B — the forbidden opening dispatched despite its refusal.**
Bounded diff at `mesh_rpc.rs` `admit_and_dispatch_protected_stream`'s
`Err(OpeningRefusal::Denied(_))` arm (`let _ =
fold.lock().apply_inbound(inbound);` — the worst-case admission failure:
"denies to the caller but still executes the handler"). Command:
`cargo t --retries 0 --test org_rpc_streaming -E
'test(=forbidden_stream_opening_causes_zero_handler_effects)'`.
**Exit 100.** Verbatim failure:

```
thread 'forbidden_stream_opening_causes_zero_handler_effects' (170024) panicked at tests\org_rpc_streaming.rs:2455:5:
the caller is told exactly one denial
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
Summary [  10.171s] 1 test run: 0 passed, 1 failed, 26 skipped
```

Analysis (stated per the rules): the mutation's handler effects surface
on the WIRE first (the denial plus the handler's two chunks and its
terminal), so the witness trips at its first effects observation — the
"exactly one denial frame" property — before reaching
`assert_handler_stays_dark`. The witness catches its own inverse at the
first place effects become observable; four weakenings: **none applies**
(the diff IS the property's own inverse and the witness reddens; nothing
was weakened). Restore: diff reversed; `06cd466bc423f3b7…` == baseline.
Restored green: 27/27.

**INV-C — steps 10/11 swapped (the Q4/ordering inverse).** Bounded diff
at `org_admission.rs` `verify_org_admission`: the provider-policy veto
moved BEFORE the replay insert (two hunks — the policy block relocated
above `let binding_digest`, its step-11 tail removed). Command:
`cargo t --retries 0 --test org_rpc_streaming -E
'test(=provider_policy_veto_denies_before_effects)'`. **Exit 100.**
Verbatim failure:

```
thread 'provider_policy_veto_denies_before_effects' (168920) panicked at tests\org_rpc_streaming.rs:2812:5:
assertion `left == right` failed: the replay slot stayed CONSUMED — the vetoed proof never reaches the policy again (step 10's insert precedes step 11's veto)
  left: 2
 right: 1
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
```

The property's own inverse: with the veto ahead of the insert, the
vetoed proof consumes no slot and REACHES THE POLICY AGAIN on
re-submission (`policy_calls` 2 vs the pinned 1). Restore: both hunks
reversed; `33892cead2e82dde…` == baseline. Restored green: 27/27.

No witness stayed green under its own inverse.

**Findings** (state, not decide):

1. **F-S1.5-1 — the `Proceed(frame)` seam is realized as the unary
   bridge's admission-mode branch (interpretation, source-established).**
   The brief says "admission at the `Proceed(frame)` seam before
   `fold.lock().apply_inbound(&frame)`"; the protected path cannot run
   `bridge_preflight` (its `may_admit` allow-list is the PUBLIC gate —
   "the caller's authorization is the org proof, never the announcement
   allow-list") and cannot land on `apply_inbound` (admitted openings
   must take `apply_inbound_admitted`). Implemented as the unary
   bridge's exact structure: `match reg.admission()` with the protected
   arm running `admit_and_dispatch_protected_stream` at the same
   loop position — admission synchronous in the same bridge iteration
   that used to fold-drive the frame. The brief's base-commit line
   numbers (`mesh_rpc.rs:4242-4247`) name that fold-drive site.
2. **F-S1.5-2 — `sender_keys` is not a server-streaming map
   (source-established).** `RpcServerStreamingFold`'s per-call maps are
   `in_flight` / `flow_control` / `protected_calls`; the
   `RequestChunkSenders` map behind `sender_keys()` lives on the
   client-streaming and duplex folds (there is no SS request-chunk
   direction). The plan's "`in_flight_keys()`/`sender_keys()`"
   observation is realized across the fold family: witness (a) observes
   `in_flight_keys()` + `flow_control_permits` on the protected SS fold
   AND `in_flight_keys()`/`sender_keys()` on REAL client-streaming and
   duplex registrations fed the same forbidden opening shape (via the
   new fixtures fold handles). Every named map is observed empty.
3. **F-S1.5-3 — the record's real `session_id` is cache-carried
   (source-established).** `cache_authenticated_response_destination`
   stores `(AEAD-verified target node, receiving session_id)` — the
   value is exactly the `SessionIdentity.session_id` the §3 record binds
   ("the incarnation that received the request", R2-A). An exact
   registry query (`session_for(key)`) is not reachable from
   `mesh_rpc.rs` — the registry record's fields are private to
   `cortex/rpc.rs` (F-S1.4-2's same API-addition boundary, ruling
   territory). On a bounded route-cache eviction the job carries `0` and
   `DirectOnly` drops it — the eviction trigger
   `response_route_fallback`'s doc already names.
4. **F-S1.5-4 — the lifetime policy at the bridge is `q1_defaults()`
   (source-established).** Q1's "provider-configurable initial defaults
   that must be validated at startup" has no per-registration or
   per-node configuration surface at this stage; adding one is
   API-addition territory. The `300 s` / `3600 s` defaults are wired
   through `StreamCallLifetime` and the §2.1 resolution (1.3's
   machinery) enforces them.
5. **F-S1.5-5 — the `Sent`/`Refused`/`Unreachable` dispositions live at
   the drainer/route layer, not on the call record (source-established
   + executed).** F-S1.3-5's deferral is realized at the bounded
   `RpcResponseJob` drainer (protected terminals) with structured logs
   per disposition (`Refused` = the §2.8 bounded failure disposition
   when the drainer is full). The call record's one-shot emission stays
   `Queued` — its documented boundary ("the control path took the job").
   Recording the transport disposition INTO `StreamCallRecord` would
   need a `cortex/rpc.rs` seam (untouchable here) and a second
   `record_emission` write past its one-shot guard — ruling territory.
6. **F-S1.5-6 — `emit_admission_denial` is byte-unchanged
   (source-established).** The protected terminal jobs reuse
   `RpcResponseJob`'s EXISTING `session_id` field, so no struct change
   was needed and the denial path — routing, job literal, coarse-byte
   body — is exactly the 1.4 code. (Denial jobs keep `session_id: 0`:
   "a denial answers no reservation", unchanged and correct.)
7. **F-S1.5-7 — `ProtectedOpeningOutcome::Admitted` gained
   `credential_ends_ns` (seam shape).** The §2.1 clamp inputs (the
   proof's credential ends + the installed owner cert's end, checked
   seconds→ns) ride the outcome out of the shared helper's existing
   proof decode. The unary bridge ignores them (unary records keep
   `deadline: None`, F-S1.4-4 — unary behavior unchanged) and `s14`'s
   destructure adapted mechanically.
8. **F-S1.5-8 — `call_streaming`'s `org_proof_intent` widening is NOT
   here (ordering note).** The named1.6 inversion
   (`call_streaming_mints_a_stream_proof`) owns it (F-S1.2-5). The 1.5
   witnesses therefore drive their minted openings through the bridge's
   exact hand-off seam with the REAL caller-side mint output
   (`test_sign_admission_proof` — the witnesses feed the provider
   literally the caller's bytes) and observe every response at real wire
   endpoints.

**CI pin data for Main (never edited here):** binary
`org_rpc_streaming`, floor **27** (24 prior + 3 named), REQUIRED names
exactly the twenty-seven (the twenty-four of §2.3 plus):

`forbidden_stream_opening_causes_zero_handler_effects`,
`streaming_denial_is_not_fanned_out_to_the_reply_roster`,
`provider_policy_veto_denies_before_effects`.

(suggested filter-set: the same twenty-seven space-separated.)

**What never ran here (complete):**

- `cargo fmt -p <crate> -- --check`, the four clippy invocations, the
  five rustdoc lines and `cargo check --workspace --all-targets` —
  Main's stage-end list (mid-flight rule). My four files were converged
  with scoped `rustfmt` + `--check` (clean) but the gate command itself
  never executed here.
- `cargo tl` / `cargo t` full suites, `tests/cross_lang_*`, the wire
  suite, the browser/SDK surfaces — stage-end (Main's) or other lanes'.
- The 1.1/1.1a wire and handshake witnesses (S1Session's — re-run at
  stage end).
- CI itself (branch unpushed; nobody pushes but Main).
- A live end-to-end `call_streaming` carrying an org proof over the
  transport: `call_streaming` still refuses `org_proof_intent` until
  slice 1.6's named inversion (`call_streaming_mints_a_stream_proof`,
  F-S1.2-5/F-S1.5-8). The 1.5 witnesses' openings ride the bridge's
  exact dispatcher hand-off with the real caller-side mint output; every
  response they assert on rides the real transport to real endpoints.
- CS/DX PROTECTED admission (Stage 2 by contract) and a per-node/
  per-registration lifetime-policy knob (F-S1.5-4).
- Windows workstation only: `#[cfg(unix)]` legs never compile here; no
  Linux/macOS execution.
### 2.5 Slice 1.6 — deleted pins

**Landed (executed):** `c245a29f0` — `S1.6: invert the streaming refusal pins
and widen call_streaming` (3 files, +416/−49) on `LZL0/org-streaming`; this
record rides in the following `S1.6:` commit. Base `21efd0319` (the slice
1.5 record chain on Main's CI pin `86c7ef358`). All green claims below are
at `c245a29f0`'s exact tree (the scoped rustfmt pass is whitespace-only and
landed inside it; the receipt cycles ran on the pre-format tree with
post-format shas recorded below). Post-format shas:
`behavior/org_admission.rs` `db305c207ff45c43…`, `mesh_rpc.rs`
`a133189bc35cc6ee…`; the receipt baselines they were restored against are
the pre-format `ef5fe07246503e44…` / `d4bde8ccb31b40fe…`.

**What landed** (source-established):
- `behavior/org_admission.rs` tests (the named pin regions ONLY):
  - `malformed_and_streaming_are_distinct` SPLIT into three named tests:
    `malformed_proof_is_refused` (the malformed-proof refusal),
    `unary_registration_streaming_flags_are_streaming_unsupported` (the
    SURVIVING unary denial invariant — step 4a's typed `StreamingUnsupported`,
    NOT deleted), and `streaming_registration_admits_the_supported_shape`
    (the new supported-shape positive: a streaming registration with coherent
    flags admits a full streaming proof whose `kind` matches and whose
    `session_binding` matches the receiving session — asserted on the
    four-party attribution).
  - `stability_recheck_runs_after_credential_checks` REWRITTEN for the new
    step-4 shape check, the ordering property KEPT and strengthened: (i) a
    step-4 `ShapeMismatch` preempts the recheck; (ii) a credential failure
    (`ProofExpired`) preempts the recheck; (iii) everything valid + an
    unstable view denies `AuthorityChanged` AND consumes no replay slot.
  - `every_denial_maps_to_a_defined_coarse_reason` EXTENDED to the seven C4
    variants (`ShapeMismatch`, `SessionBindingMismatch`,
    `DeadlineExceedsPolicy`, `ActiveCallOwned`, `ActiveStreamCapacity`,
    `Revoked`, `ResourceExhausted`) AND to the two pre-existing unlisted
    variants (`PerOrganizationReplayCapacity`, `ExternalPoolReplayCapacity`)
    so the enumeration matches its "EVERY variant" claim. Bucket anchors:
    `Revoked` → `Denied` (Main's §4.3 ruling), `ActiveStreamCapacity` /
    `ResourceExhausted` → `Unavailable`, the rest of the new ones → `Denied`.
- `mesh_rpc.rs`:
  - `call_streaming` WIDENED (C11's SS half): `org_proof_intent` is accepted
    and mints a streaming call proof through the shared mint helper
    (`RpcCallShape::ServerStreaming` + the receiving session's Noise
    handshake hash, §1.3) — the unary `call`'s verbatim mint block (pinned
    provider binding, exactly-one-header discipline, finalized wire bounds,
    the one-packet measurement). A streaming mint without a binding fails
    LOCAL, fail-closed. `call_client_stream`/`call_duplex` keep refusing
    (their protected admission is Stage 2's) and `call_service_streaming`
    keeps refusing (capability-index routing cannot pin a provider entity).
  - `org_proof_intent_rejected_on_streaming_and_capability_mismatch` INVERTED
    into `call_streaming_mints_a_stream_proof`: the positive proves the
    minted bytes are a FULL `OrgStreamCallProof` (strict decode) with
    `kind = server-streaming` and `session_binding` = the exact live
    session's binding, observed at the provider's request dispatcher (the
    caller-side mint output on the wire). The capability-mismatch half is
    KEPT verbatim (local refusal) and the CS/DX/service-routed refusal legs
    are KEPT (production behavior until their stages).
- `docs/ORGANIZATIONS.md`: the check-order step 4 rewritten for the shape
  term (unary registration + streaming flags → `NotSupported`; streaming
  registration + flags ≠ shape or proof kind ≠ shape → `Denied`) + the
  streaming-decode note at step 3; the verbs section records slice 1.5's
  protected server-streaming core seams and the caller-side mint.

**Unit totals before/after by module (executed):**
- `behavior/org_admission.rs` tests: `19 → 21` (+2 = the split's
  1→3; the rewrite and the extension are count-neutral; 2 pre-existing
  unlisted variants added to the extension's enumeration).
- `adapter::net::mesh_rpc` filter tests (39 in `mesh_rpc.rs` + 13 in
  `mesh_rpc_metrics.rs`; F-9 label fix 2026-09-22): `52 → 52` (the inversion is a
  named rewrite, 1 test in, 1 test out).
- `cortex/rpc.rs`, the Stage 0 models, `behavior/org_call.rs`: unchanged.

**Inverse receipts (executed, raw).** Baselines at receipt time:
`org_admission.rs` `ef5fe07246503e44…`, `mesh_rpc.rs` `d4bde8ccb31b40fe…`;
restores are edits-reversed + sha256-proven byte-identical.

**R1 — the supported-shape positive's inverse: the step-4 kind check
inverted** (`org_admission.rs:556` guard `== Some(ctx.shape)` → `!=`): a
proof whose kind MATCHES the shape is refused. Command: `CARGO_INCREMENTAL=0
cargo tfl --retries 0
adapter::net::behavior::org_admission::tests::streaming_registration_admits_the_supported_shape`.
**Exit 100.** Verbatim:

```
thread 'adapter::net::behavior::org_admission::tests::streaming_registration_admits_the_supported_shape' (155352) panicked at src\adapter\net\behavior\org_admission.rs:1221:28:
the coherent streaming shape admits: ShapeMismatch
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
error: test run failed
```

Collateral red under the same mutation (one run, disclosed):
`stability_recheck_runs_after_credential_checks` leg (ii) (`left:
Err(ShapeMismatch) / right: Err(ProofExpired)` — the mutated kind guard
preempts the expiry check for the matching-kind expired proof). Restore:
`ef5fe07246503e44…` == baseline.

**R2 — the mint's own inverse: the mint shape flipped to `Unary`**
(`mesh_rpc.rs` `call_streaming`'s `sign_admission_proof` call):
`call_streaming` mints UNARY-format bytes instead of a streaming proof.
Command: `CARGO_INCREMENTAL=0 cargo tfl --retries 0
adapter::net::mesh_rpc::roster_fallback_tests::call_streaming_mints_a_stream_proof`.
**Exit 100.** Verbatim:

```
thread 'adapter::net::mesh_rpc::roster_fallback_tests::call_streaming_mints_a_stream_proof' (168512) panicked at src\adapter\net\mesh_rpc.rs:9928:14:
the minted bytes are a FULL streaming proof (strict decode): InvalidFormat
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
error: test run failed
```

The strict streaming decoder refuses the unary-format value (`InvalidFormat`
— the 1.2 decoder's truncation refusal), so the witness discriminates "mints
a STREAM proof" exactly. Restore: `d4bde8ccb31b40fe…` == baseline.

**R3 — the ordering property's inverse: the §9.5 recheck hoisted above the
shape/credential checks** (`org_admission.rs` — the `if !stability_recheck()`
block moved to the top of `verify_org_admission`, its step-9.5 site removed;
two bounded hunks). Command: `CARGO_INCREMENTAL=0 cargo tfl --retries 0
adapter::net::behavior::org_admission::tests::stability_recheck_runs_after_credential_checks`.
**Exit 100.** Verbatim:

```
thread 'adapter::net::behavior::org_admission::tests::stability_recheck_runs_after_credential_checks' (55052) panicked at src\adapter\net\behavior\org_admission.rs:1705:9:
assertion `left == right` failed: step 4 runs BEFORE the recheck — a shape refusal wins over an unstable view
  left: Err(AuthorityChanged)
 right: Err(ShapeMismatch)
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
error: test run failed
```

The property's own inverse exactly: hoisted, `AuthorityChanged` masks the
shape refusal the ordering guarantees must win. Restore (both hunks) +
green.

**R4 — the ruling pin's inverse: `Revoked` mapped to `Unavailable`**
(`org_admission.rs` `AdmissionDenied::coarse` — the variant moved from the
`Denied` arm group to the `Unavailable` group, the Stage 0 model's corrected
ruling inverted; two bounded hunks). Command: `CARGO_INCREMENTAL=0 cargo tfl
--retries 0
adapter::net::behavior::org_admission::tests::every_denial_maps_to_a_defined_coarse_reason`.
**Exit 100.** Verbatim:

```
thread 'adapter::net::behavior::org_admission::tests::every_denial_maps_to_a_defined_coarse_reason' (169488) panicked at src\adapter\net\behavior\org_admission.rs:1881:13:
assertion `left == right` failed
  left: Unavailable
 right: Denied
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
error: test run failed
```

The §4.3 ruling (Main, `cddb063a3`) pinned: `Revoked` must share the frozen
`Denied` coarse byte — the old model mapping reddens the anchor. Restore
(both hunks) + green.

The two halves MOVED by the split (`malformed_proof_is_refused`,
`unary_registration_streaming_flags_are_streaming_unsupported`) keep their
discriminative history: 1.2's receipts B (step-4a removal → the
`StreamingUnsupported` red) and the malformed decode refusals pinned by
`stream_decode_rejects_truncation_trailing_bytes_and_unknown_kinds`; the
1.6 pass moved the assertions without weakening them (four weakenings:
none).

**Witnesses and counts (executed):** the four named pin operations above, at
`--retries 0` (unit totals: `behavior/org_admission.rs` tests `19 → 21`,
`adapter::net::mesh_rpc` filter tests (39 in `mesh_rpc.rs` + 13 in
  `mesh_rpc_metrics.rs`; F-9 label fix 2026-09-22) `52 → 52`):

| Pin | Kind | Proves |
|---|---|---|
| `malformed_proof_is_refused` | split part 1 (moved assertion) | garbage proof bytes → `MalformedProof`, never a shape refusal |
| `unary_registration_streaming_flags_are_streaming_unsupported` | split part 2 — the SURVIVING unary denial invariant, NOT deleted | step 4a's typed `StreamingUnsupported` on a unary registration |
| `streaming_registration_admits_the_supported_shape` | split part 3 (the new supported-shape positive) | the coherent streaming shape ADMITS: full streaming proof, matching kind, matching session binding, four-party attribution asserted |
| `stability_recheck_runs_after_credential_checks` | rewritten for the new step-4 shape check (ordering property KEPT + strengthened) | step 4 and credential checks precede the §9.5 recheck; the recheck precedes the replay insert (a stale view consumes no slot) |
| `every_denial_maps_to_a_defined_coarse_reason` | extended | all 37 variants round-trip their coarse byte; anchors incl. `Revoked` → `Denied` (Main's §4.3 ruling) and `ActiveStreamCapacity`/`ResourceExhausted` → `Unavailable` |
| `call_streaming_mints_a_stream_proof` | inverted | `call_streaming` + `org_proof_intent` puts a FULL `OrgStreamCallProof` (kind SS + the live session's binding) on the wire; the capability-mismatch half and the CS/DX/service-routed refusals KEPT |

**Green at this head (executed):**

- `cargo t --retries 0 --test org_rpc_streaming` → **27 tests run: 27
  passed, 0 skipped**, exit 0 (the binary is count-unchanged by 1.6 — its
  floor stays **27**; the CI pin needs no movement from this slice).
- Preserved + public-streaming-regression + `subnet_org_boundary` batch,
  ONE invocation → **89 run / 89 passed / 0 skipped**, exit 0 (the
  preserved trio inside `nrpc_streaming_gate`).
- In-source units (`CARGO_INCREMENTAL=0 cargo tfl --retries 0
  adapter::net::cortex::rpc adapter::net::mesh_rpc org_stream`) → **216
  run / 216 passed / 5622 skipped**, exit 0 — count-continuous (the 1.6
  unit deltas live in `behavior::org_admission::`, outside this filter).
- The 1.6 pin modules, each green post-format:
  `behavior::org_admission::` → **21 run / 21 passed** (was 19),
  `adapter::net::mesh_rpc` → **52 run / 52 passed** (unchanged).
- Scoped rustfmt (`rustfmt --edition 2021 --config skip_children=true`) on
  the two touched source files; `--check` clean.
- The renamed unit (`org_proof_intent_rejected_on_streaming_and_capability_mismatch`
  → `call_streaming_mints_a_stream_proof`) is NOT pinned in `ci.yml`
  (grep: 0 matches) — no roster movement beyond the report's naming.

**Findings** (state, not decide):

1. **F-S1.6-1 — two pre-existing enumeration gaps closed by the named
   extension (source-established).** `every_denial_maps_to_a_defined_coarse_reason`
   claimed to enumerate EVERY variant but omitted
   `PerOrganizationReplayCapacity` and `ExternalPoolReplayCapacity` (both
   constructed by `verify_org_admission`'s replay-outcome mapping). The
   extension made the enumeration complete (counted in the same test's edit).
2. **F-S1.6-2 — `call_client_stream`/`call_duplex` keep refusing
   `org_proof_intent` (source-established, ordering).** C11 names all of
   `call_streaming`/`call_client_stream`/`call_duplex` widening; the named
   1.6 work inverts only `call_streaming` (the CS/DX protected admission is
   Stage 2's). Their refusal legs are KEPT inside
   `call_streaming_mints_a_stream_proof` — a red-at-hand pin for the Stage 2
   widening to invert.
3. **F-S1.6-3 — `(:124-135 with 1.5's verbs)` is realized as the core-seam
   names (interpretation).** The doc's verbs section documents the SDK
   surface; 1.5 landed CORE seams (`serve_rpc_owner_scoped_streaming` /
   `serve_rpc_granted_streaming`) while the SDK-level
   `call_streaming`/`serve_org_streaming` verbs are release-train surface.
   The new paragraph names the landed seams + the caller-side mint and
   points the SDK verbs at the release train rather than promising verbs
   that do not exist yet.

**What never ran (complete):** `cargo fmt -p <crate> -- --check`, the four
clippy invocations, the five rustdoc lines, `cargo check --workspace
--all-targets` (Main's stage-end list; the touched files were converged with
scoped rustfmt); `cargo tl` / `cargo t` full suites; `tests/cross_lang_*`;
the wire suite; the browser/SDK surfaces; the 1.1/1.1a witnesses
(S1Session's — re-run at stage end); CI itself (unpushed; nobody pushes but
Main); CS/DX protected admission and the `call_service_streaming` widening
(later stages by contract); Windows workstation only — `#[cfg(unix)]` legs
never compile here, no Linux/macOS execution.

---

## 3. Repair round (S1_R)

**Lane:** S1Repair. **Pinned brief:** `spikes/org-streaming/S1_R_BRIEF.md`
@`27d7a72ee`. HOLD closed over: `e25ac28bf` (the packet's reviewed head).
Date: 2026-09-22. Windows host only. Every run `--retries 0 --no-tests=fail`
on the warm aliases (`cargo tf` / `cargo tfl`, the graphs pinned in
`net/crates/net/.cargo/config.toml`) from `net/crates/net/`. Claims are
labelled executed vs source-established.

### 3.1 Landed commits

| commit | content |
|---|---|
| `60c287d1e` | the acceptance witnesses (rows 1–8): 3 new tests in `tests/org_rpc_streaming.rs`, 3 new in-source units in the `cortex/rpc.rs` test module, and the Row-8 doc naming (4 files, +738/−3) |
| `4ab5738cf` | the per-row inverse receipts — an empty tree-change BY CONSTRUCTION (every mutation restored sha-proven); the receipts raw in the commit message |
| (this commit) | this section |

File ownership held exactly (executed: `git status` clean at both prior
commits' trees): `net/crates/net/tests/org_rpc_streaming.rs`,
`net/crates/net/tests/org_rpc_streaming/**` (incl. the frozen module docs),
and the TEST MODULES ONLY of
`net/crates/net/src/adapter/net/cortex/rpc.rs`. NO production-path edits:
`mesh_rpc.rs` is sha-identical to `e25ac28bf`'s
(`eda1c392eff64b26dd98c90f5a7b5df998af5c3f40b879cc1b6be8350e9d8f47`),
and `cortex/rpc.rs`'s three additions live inside `#[cfg(test)] mod tests`.

### 3.2 Witnesses, counts, and the per-row map

`org_rpc_streaming`'s new count: **30** — the existing 27 preserved verbatim
plus 3 new. Roster FROM SOURCE (30 `#[test]`/`#[tokio::test]` fns in
`tests/org_rpc_streaming.rs`; the helper modules carry none) == 30 executed
(`cargo tf --retries 0 --test org_rpc_streaming`, 30/30, exit 0). Full name
list (sorted) — for Main's same-commit CI floor re-pin 27 → 30:

```
active_call_id_reuse_after_replay_window_is_refused
completed_stream_drains_queued_items_in_order_with_content_and_end_terminal
credential_clamp_expiry_is_admission_denied_not_timeout
cross_org_completed_stream_drains_correlated_items_with_end_terminal
floor_raise_retires_blocked_stream_before_publish_returns
forbidden_stream_opening_causes_zero_handler_effects
frozen_old_provider_refuses_stream_proof_with_not_supported
late_chunk_from_replaced_session_is_dropped
node_shutdown_retires_live_protected_streams
omitted_deadline_gets_default_and_expires_idle
poisoned_store_retires_all_protected_streams
provider_policy_veto_denies_before_effects
public_client_stream_deadline_expiry_is_typed_timeout
public_ss_nonzero_deadline_expires_with_typed_timeout
pump_parked_on_zero_credit_is_retired_at_deadline_with_one_terminal
queued_bytes_over_call_budget_park_send_wait_and_wake_on_retire
raise_between_reserve_and_install_denies_with_zero_effects
replayed_opening_on_new_session_is_session_binding_mismatch
requested_deadline_over_cap_is_refused_with_zero_effects
requested_deadline_within_cap_is_honoured
response_after_session_replacement_reaches_only_the_live_session
serve_handle_drop_retires_live_stream_and_sibling_survives
session_replacement_retires_old_call
sibling_stream_of_other_org_sends_next_item_after_publication
store_replacement_retires_all_and_resubscribes
stream_opening_admits_cross_org
stream_opening_admits_same_org
stream_proof_on_unary_registration_is_not_supported
streaming_denial_is_not_fanned_out_to_the_reply_roster
unary_context_proof_is_binding_invalid_on_stream_registration
```

New in-source units (test module of `adapter/net/cortex/rpc.rs`): the
three-module in-source filter (`adapter::net::cortex::rpc
adapter::net::mesh_rpc org_stream`) runs **219/219** = the previous 216 +
these 3, exit 0 (executed).

| row | witness | where | asserted observation (the closure property) | named inverse |
|---|---|---|---|---|
| 1+2+7 same-org | `completed_stream_drains_queued_items_in_order_with_content_and_end_terminal` | integration, real bridge + REAL wire endpoint (the caller's reply-channel recorder) | a handler queues 2 CONTENT-LABELLED items under ZERO credit and RETURNS; a valid `STREAM_GRANT` arrives; the items publish IN ORDER (body bytes + wire sequence, never counted); then EXACTLY ONE terminal frame at the authenticated receiving endpoint with the exact wire content (status `Ok` + `nrpc-streaming: end`) — and stays one terminal ever | A2b (the items assertion) + A2c (the terminal assertion) |
| 7 cross-org | `cross_org_completed_stream_drains_correlated_items_with_end_terminal` | integration, `serve_rpc_granted_streaming` + the granted/other-org intent (`fixture::cross_org_intent`) | the same observations under the cross-org authority intent shape | A2b + A2c (its own copies of both assertions) |
| 3 | `node_budget_refusal_rolls_back_call_and_caller_reservations` | in-source sibling of `byte_reservation_rolls_back_in_order_and_releases_exactly_once` | a node-level refusal AFTER two successful level increments (call + caller); BOTH counters rolled back to exact values (400/400) and the node total unmoved | A3 |
| 4 | `late_retire_against_a_reused_key_is_a_no_op_for_the_successor` | in-source unit (production `ProtectedCallRegistry`) | the S0 model's `late_operations_with_a_stale_incarnation_cannot_touch_the_successor` shape: the first record is retired and its supervisor-side single removal is driven directly (`complete`) — the key is reused IMMEDIATELY (NO `record_count()` wait or poll anywhere) while the first incarnation's cleanup-owner handles stay armed across the reuse; every late op carrying the stale incarnation (retire/complete/release/commit) is inert; the successor survives end to end (unsettled, un-signalled, own retire+complete exactly once) | A4 |
| 5 | `response_after_session_replacement_reaches_only_the_live_session` | integration, real re-handshake + real wire endpoint | **AMENDED CLOSURE WORDING (Main's S1R ruling, sharpened (a); amended, not weakened):** the EMIT-SESSION SELECTION the test drives — a response emitted after a session replacement is carried by, delivered on, and attributed to the session LIVE at emission — read receiver-side at the ingress attribution (`RpcInboundEvent::session_id`, the R2 carrying-incarnation stamp): the LIVE session's endpoint receives it while the REPLACED session's endpoint receives NOTHING — receiver-side attribution asserted on BOTH endpoints | A5' (receipt 7 — discriminating, red at the witness's own named assertion). The brief's A5 stays green by construction — F-S1R-1 |
| 6 | `item_permit_transfer_consumes_once_across_the_handoff` | in-source unit (production `ItemPermit`) | the S0 model's `cancel_dequeue_handoff_consumes_one_permit` semantics at the production permit: source consumed, target owns (handoff is not memory reclamation — bytes stay charged), exactly ONE release across the pair, another call's live bytes stay charged throughout | A3b |
| 8 | (documentation) | `frozen_85ecc77c9/old_serve.rs` + `frozen_85ecc77c9.rs` module docs | the denial-shape block's re-indentation is NAMED (whitespace-insensitive provenance statement + trimmed-hash evidence; vendored bodies untouched — their extraction sha256s remain the record) | no runtime inverse (R-Row8 below) |

Preserved + controls (executed, one invocation): `org_ownership` 32/32 and
the 8 preserved/control binaries (nrpc_streaming_gate,
integration_nrpc_streaming, integration_nrpc_client_streaming,
integration_nrpc_duplex, nrpc_registration_order, integration_nrpc_protected,
org_admission_wire, subnet_org_boundary) 89/89 — **121/121, exit 0**.

### 3.3 The six Appendix receipts (raw) + receipt 7 (R-A5′)

Baselines: `cortex/rpc.rs`
`35b003e55f5f75fe25350312ae5107bc3a11ed189b82f4d119a9ee5ddb8c0c92`;
`mesh_rpc.rs`
`eda1c392eff64b26dd98c90f5a7b5df998af5c3f40b879cc1b6be8350e9d8f47`.
Each receipt: bounded diff at the production site → the row's NAMED RED (an
assertion failure in every case — a compile error is never a red) →
`git checkout --` restore proven `sha256sum ==` the baseline → restored
green run. Identical text is committed verbatim as `4ab5738cf`'s message.

**R-A2b (rows 1+2+7)** — supervisor pump post-close publish. Mutation
(`cortex/rpc.rs` `run_stream_call_supervisor`, the pump's send-order comment
anchor):

```diff
-            pump_emit(from_node, caller_origin, call_id, resp).await;
+            if !pump_gate.as_ref().is_some_and(|g| g.is_finished()) {
+                pump_emit(from_node, caller_origin, call_id, resp).await;
+            }
```

Run: `cargo tf --retries 0 --test org_rpc_streaming -E
'test(=completed_stream_drains_queued_items_in_order_with_content_and_end_terminal)
+ test(=cross_org_completed_stream_drains_correlated_items_with_end_terminal)'`
— exit 100 (2 run: 0 passed, 2 failed), each witness red at ITS OWN named
assertion:

```
tests/org_rpc_streaming.rs:2939:5: the same-org queued items publish IN ORDER
  after the grant — a post-close discard of queued chunks leaves the caller
  with its terminal only
tests/org_rpc_streaming.rs:3092:5: the cross-org queued items publish IN
  ORDER after the grant — a post-close discard of queued chunks leaves the
  caller with its terminal only
```

Restore sha `35b003e5…` == baseline; restored green: same `-E` run, 2/2.

**R-A2c (rows 1+2+7)** — the `Completed(Ok)` terminal's wire shape. Mutation
(`cortex/rpc.rs` `stream_terminal_payload`, the `Completed(Ok)` arm):

```diff
-                HEADER_NRPC_STREAMING_END.to_vec(),
+                HEADER_NRPC_STREAMING_CONTINUE.to_vec(),
```

Run: same `-E` pair — exit 100 (2 run, 2 failed), each witness red at ITS
OWN named assertion with the exact wire diff visible:

```
tests/org_rpc_streaming.rs:2978:9: assertion `left == right` failed: the
  same-org terminal frame's exact wire content is status Ok + the
  `nrpc-streaming: end` marker
  left: (Ok, [("nrpc-streaming", [99, 111, 110, 116, 105, 110, 117, 101])], [])
 right: (Ok, [("nrpc-streaming", [101, 110, 100])], [])
tests/org_rpc_streaming.rs:3129:9: assertion `left == right` failed: the
  cross-org terminal frame's exact wire content is status Ok + the
  `nrpc-streaming: end` marker (same left/right)
```

Restore sha `35b003e5…` == baseline; restored green: same `-E` run, 2/2.

**R-A3 (row 3)** — `ByteBudgets::reserve`'s `NodeBudgetFull` arm rollback.
Mutation (`cortex/rpc.rs`, both counter restorations deleted; the
`return Err(ByteRefusal::NodeBudgetFull);` kept):

```diff
             Some(_) => {
-                state.per_caller.insert(key.caller.clone(), caller_cur);
-                state
-                    .per_call
-                    .insert((key.clone(), incarnation, direction), call_cur);
                 return Err(ByteRefusal::NodeBudgetFull);
             }
```

Run: `cargo tfl --retries 0 -E
'test(=adapter::net::cortex::rpc::tests::node_budget_refusal_rolls_back_call_and_caller_reservations)'`
— exit 100 (1 run, 1 failed):

```
src/adapter/net/cortex/rpc.rs:11804:9: assertion `left == right` failed:
  the call counter was rolled back
  left: 600
 right: 400
```

Restore sha `35b003e5…` == baseline; restored green: same `-E` run, 1/1.

**R-A3b (row 6)** — `ItemPermit::transfer` ownership consumption. Mutation
(`cortex/rpc.rs`):

```diff
-    pub fn transfer(mut self) -> ItemPermit {
-        self.settled = true;
+    pub fn transfer(self) -> ItemPermit {
         ItemPermit {
```

Run: `cargo tfl --retries 0 -E
'test(=adapter::net::cortex::rpc::tests::item_permit_transfer_consumes_once_across_the_handoff)'`
— exit 100 (1 run, 1 failed/ABORT). FIRST failure is the named assertion:

```
src/adapter/net/cortex/rpc.rs:11873:9: assertion `left == right` failed:
  transfer is not memory reclamation — the bytes stay charged across the
  handoff
  left: 0
 right: 100
```

Disclosure (verbatim, after the named red): the unwinding drop of the source
permit then trips the release-once guard —
`panicked at src/adapter/net/cortex/rpc.rs:3963:14: byte permit released
twice, or against the wrong call` — the process aborts (0xc0000409, fail-fast
during the second panic). The named assertion at `11873` fired FIRST; the
cascade is the release-once invariant confirming the pair's exactly-once
semantics. Restore sha `35b003e5…` == baseline; restored green: same `-E`
run, 1/1.

**R-A4 (row 4)** — `retire_locked`'s incarnation fence. Mutation
(`cortex/rpc.rs` `ProtectedCallRegistry::retire_locked`):

```diff
-        if record.incarnation != incarnation || record.phase == RegistryPhase::Terminal {
+        if record.phase == RegistryPhase::Terminal {
```

Run: `cargo tfl --retries 0 -E
'test(=adapter::net::cortex::rpc::tests::late_retire_against_a_reused_key_is_a_no_op_for_the_successor)'`
— exit 100 (1 run, 1 failed):

```
src/adapter/net/cortex/rpc.rs:12137:9: a LATE retire carrying the old
  incarnation must be a no-op for the successor (§2.4)
```

(one `unused variable: incarnation` warning; compiled otherwise clean — the
red is the assertion, not a build failure.) Restore sha `35b003e5…` ==
baseline; restored green: same `-E` run, 1/1.

**R-A5 (row 5)** — the streaming emitter's receiving-incarnation
attribution. Mutation (`mesh_rpc.rs` `serve_rpc_streaming_impl`'s async emit
closure):

```diff
-                let receiving_session_id = cached.map(|(_node, session)| session).unwrap_or(0);
+                let receiving_session_id = 0;
```

Run: `cargo tf --retries 0 --test org_rpc_streaming` (whole binary) —
**exit 0: 30/30 GREEN, inclusive of `response_after_session_replacement_
reaches_only_the_live_session`. GREEN UNDER ITS OWN INVERSE — finding
F-S1R-1 below.** Restore sha `eda1c392…` == baseline; restored green:
`cargo tf --retries 0 --test org_rpc_streaming -E
'test(=response_after_session_replacement_reaches_only_the_live_session)'`,
1/1.

**R-Row8 (row 8)** — NO runtime inverse (documentation only): the re-indent
naming is provenance text, receipted by the whitespace-trimmed sha256
equality recorded in the frozen module's doc
(`f31eb08ec127c32f1a4ccf1892ec4dadf0fac151af89aad03cce65b60ad327a7` on both
sides after reversing the one named prefix retarget; `cmp` clean). The
vendored bodies are untouched; their extraction sha256s remain the record.

**Receipt 7 — R-A5′ (row 5; the lane's own discriminating inverse at the
amended closure's observable seam).** Mutation (`mesh_rpc.rs` `dispatch_packet`,
the three `RpcInboundEvent` attribution stamps — the R2 carrying-incarnation
point the witness reads; the `StreamLifetime` site at `:30758` is a different
concern and deliberately left untouched):

```diff
                         disp(crate::adapter::net::cortex::RpcInboundEvent {
                             // R2: the incarnation that carried this
                             // request, not whatever is installed
                             // when the bridge drains it.
-                            session_id: session.session_id(),
+                            session_id: 0, // A5' MUTATION
                             channel_hash: canonical,
```

(x3 — the `Snapshot::Single` site at `mesh.rs:30512` and the two
`Snapshot::Many` sites at `:30537`/`:30549`.) Framing: the ingress delivers
the frame under NO session attribution instead of the AEAD-verified carrying
incarnation — the observable-attribution form of "delivered to the wrong
endpoint instead of the live session's". The packet ruling's literal example
("select/deliver to the stale or cached session instead of the live one") has
NO retained stale session to select at any seam (source-established, executed
inspection of `install_peer_locked`'s displaced branch: the displaced
`NetSession` is consumed and dropped, `session_id_to_node`'s stale entry is
removed, `peers` holds exactly one session per peer; and the wire-claimed
`NetHeader.session_id` is verified EQUAL to the carrying session's id at the
ingress resolution, so stamping the claim is observationally identical) —
the attribution flip above is the discriminating mutation available at the
observed seam.

Run: `cargo tf --retries 0 --test org_rpc_streaming -E
'test(=response_after_session_replacement_reaches_only_the_live_session)'`
— exit 100 (1 run, 1 failed) — red at the WITNESS'S OWN named assertion:

```
tests/org_rpc_streaming.rs:3256:9: assertion `left == right` failed:
  the LIVE session's endpoint receives the post-replacement response
  left: 0
 right: 17873330928486580960
```

Restore: `git checkout -- net/crates/net/src/adapter/net/mesh.rs`; sha256
`a55818907d0bfe52f5e1ec285fe7f6216e451c58c7366fc79038c662c7ca98fd` ==
pre-mutation baseline (and == the packet's own `mesh.rs` baseline — the file
is byte-identical to `e25ac28bf`'s). Restored green: same `-E` run, 1/1.

### 3.4 Findings (stated, not decided)

1. **F-S1R-1 — Row 5's named closure is unsatisfiable against A5 as written
   (the witness is GREEN under its own inverse; P2; four weakenings: NONE
   applies — the witness asserts the closure's literal observables and was
   not widened, relaxed, deleted, or precondition-fixed).**
   Source-established: the A5 site's value (`receiving_session_id`,
   `mesh_rpc.rs:4771`) flows only into `RpcResponseJob.session_id` and
   `publish_response_to_caller`'s same-named parameter, and those are
   consumed ONLY inside `#[cfg(feature = "webrtc")]` — the enrollment-
   promotion block (`enrollment_reservation_owner` /
   `promote_on_enrollment_response` / `note_enrollment_rejected` /
   `retire_enrollment_call`), itself further gated on `reply_channel ==
   rtc::enroll_reply_channel(origin)` (unreachable from `<service>.replies.*`
   reply channels) — plus `tracing` fields. Both sanctioned warm-alias
   graphs deliberately exclude `webrtc` (`net/crates/net/.cargo/config.toml`:
   "deliberately absent because it is the one feature with a C toolchain
   dependency"). No receiver-side attribution surface — wire, emit seam, or
   fold key — reads that value in any sanctioned graph, so "reddening under
   A5" cannot be realized there.
   **RESOLVED BY MAIN'S S1R RULING (sharpened (a)) — decided by the owner,
   recorded here:** the discovery is credited (the packet's
   reddening-under-A5 was unsatisfiable BY CONSTRUCTION in the executable
   graphs — the green is a property of the feature graph, not of the
   witness); witness discipline is absolute, so the witness STANDS on its
   receiver-side-attribution property (the fence's actual protective effect:
   post-replacement response reaches the live session's endpoint, the
   replaced endpoint receives nothing) with the closure wording AMENDED
   (not weakened) to name the observable seam — the EMIT-SESSION SELECTION
   the test drives, read at the ingress attribution
   (`RpcInboundEvent::session_id`) — and it is QUALIFIED by its own
   discriminating inverse: receipt 7 (R-A5′, §3.3) reds it at its own named
   assertion ("the LIVE session's endpoint receives the post-replacement
   response") and is sha-restored green. Four weakenings: NONE applied to
   the witness. (Correction note: `3620fed0b`'s message states this
   resolution; its body edit missed its anchor and landed in this commit.)
2. **F-S1R-2 — a displaced PROTECTED call's `SessionReplaced` terminal is
   not a reliable wire observation (executed observation; mechanism
   [INFERENCE]).** During Row-5 construction (executed iterations at this
   head's parent): with the protected registration, the re-handshake DID
   rotate the establishment and retire the call (the registry's record was
   retired-and-removed inside the transition window — `record_count() == 0`
   and no `terminal_reason` remained by probe time), yet the terminal frame
   never reached a recording endpoint across a 30 s bound. [INFERENCE] The
   terminal's publish races `install_peer_locked`'s peer transition
   (`try_publish_to_peer` resolves the peer's session mid-transition;
   `TxAdmit::SessionSuperseded` / `NoSession` at that instant are logged and
   dropped, "not retried on the roster"). The witness therefore drives a
   PUBLIC call whose response is emitted STRICTLY AFTER the replacement —
   the deterministic shape of the closure's observables. The racy protected
   terminal is left as an observation for Main (a possible follow-up
   property: a post-replacement terminal that cannot be silently dropped).
3. **F-S1R-3 — the HOLD packet's `a1c518dd…` trimmed hash is not
   reproducible with standard tooling (record coherence; P3).** The two
   procedures, side by side, for review-2.1 adjudication on evidence:

   **Packet procedure (S1_REVIEW_PACKET §4 F-8 / §9):** the claim is
   "the five lines are re-indented (trimmed hashes match `a1c518dd…`;
   raw windows differ)". The packet states NO trim convention and NO
   digest algorithm for `a1c518dd…` — only the value.

   **This lane's reproduction attempts (all executed; window = `git show
   85ecc77c9:net/crates/net/src/adapter/net/mesh_rpc.rs | sed -n
   '872,876p'` — its raw sha256 `fa275454…` reproduces the packet's
   recorded extraction hash exactly, so the window and its line endings
   are certainly the packet's):**

   | framing of the five lines | digest | value |
   |---|---|---|
   | each line trimmed both sides (`sed 's/^[[:space:]]*//;s/[[:space:]]*$//'`) | sha256 | `f31eb08ec127c32f1a4ccf1892ec4dadf0fac151af89aad03cce65b60ad327a7` |
   | " | sha512 | `00b91cbcbd685257e64e350e86d839cb47246058351c80ba1c791ae81b1301acc62d32ca95fa127b01044f0fd8caec707876ef321812493ffd2b9ae1d84a28ce` |
   | " | sha1 | `530fd949fc604abe25bf4ee18702009748872e7c` |
   | " | md5 | `d6ff091901c5dd64bf4e440449a565cf` |
   | " | blake2b-512 (`b2sum`) | `6cb82e9d88e51f7b77e6da2d7cb1464ac9ca0f15cacd24addb493c5314c2a2b380da4e0f54d4d9922b2357b5cea5f7fff27a504b5ea20625c70017f419f34010` |
   | " | blake2b-256 (`b2sum -l 256`) | `a6fe6bcd3afd1ff7429ffbfdf12b85879095b8d1516a389441f858ea797ba396` |
   | all whitespace removed (`sed 's/[[:space:]]//g'`) | sha256 | `38c8e808de561f8c95ede1f4863c7edf4baacb03529ab34e6f49fe562c5744b0` |
   | " | blake2b-512 | `d3fc3c3fc7e7853448890d8358148fc2…` (32-char prefix observed) |
   | leading-whitespace-only trim | sha256 | `f31eb08ec127c32f1a4ccf1892ec4dadf0fac151af89aad03cce65b60ad327a7` |

   None begins `a1c518dd`. (Also attempted without a match: a
   newline-joined variant; `b3sum` is not installed on this host.)

   **This lane's procedure (the one recorded in the frozen module doc):**
   step 1: `git show 85ecc77c9:net/crates/net/src/adapter/net/mesh_rpc.rs |
   sed -n '872,876p' | sed 's/^[[:space:]]*//;s/[[:space:]]*$//'` → sha256
   `f31eb08ec127c32f1a4ccf1892ec4dadf0fac151af89aad03cce65b60ad327a7`;
   step 2: `sed -n '208,212p' tests/org_rpc_streaming/frozen_85ecc77c9/old_serve.rs
   | sed 's/^[[:space:]]*//;s/[[:space:]]*$//' | sed
   's/net::adapter/crate::adapter/'` (the one NAMED prefix retarget
   reversed) → sha256
   `f31eb08ec127c32f1a4ccf1892ec4dadf0fac151af89aad03cce65b60ad327a7`;
   step 3: `cmp` of the two streams → clean (byte-identical). Without
   reversing the named retarget, the vendored five trimmed lines hash to
   sha256 `bfb3bb90a4d6954b2a2dcf60188005dd7c69e2edde6b600088c58ed41cfd52ba`
   — the expected single-content delta.

   Both procedures agree the re-indentation is whitespace-only modulo the
   one named prefix; they differ only in the digest/trim convention behind
   the cited hash value. Reviewer adjudication requested on which value the
   record should carry.
4. **Row-4 interpretation note (stated).** The closure phrase "reuses
   `(caller, call_id)` while the old record's async cleanup is still armed
   (do NOT wait for `record_count() == 0`)" is realized as the S0 model's
   own §2.4 shape (`late_operations_with_a_stale_incarnation_cannot_touch_
   the_successor`, named by the brief's Row-6 precedent): the first
   record's supervisor-side single removal is driven DIRECTLY (`complete`)
   and the same key is reused immediately — the witness contains no
   `record_count()` wait or poll — while the first incarnation's
   cleanup-owner handles stay armed (alive, late-op-capable) across the
   reuse. (Source-established constraint on any other reading: production
   `reserve` refuses `ActiveCallOwned` while ANY record exists for the key,
   model and production alike — "a terminal record not yet reclaimed still
   owns its key" — so a successor cannot install before the cleanup runs.)

### 3.5 Never executed here (complete)

- **The `webrtc` feature graph** — the only graph where the A5 value is
  consumed (F-S1R-1). Never executed here: both warm aliases exclude it (C
  toolchain dependency), and this round stayed on the pinned warm graphs by
  the brief's evidence rules.
- Linux/macOS and `#[cfg(unix)]` legs (Windows host only).
- CI itself (branch unpushed; nobody pushes but Main) — the
  `org_rpc_streaming` floor re-pin 27 → 30 (full name list in §3.2) is
  delivered to Main with this round for a same-commit pin.
- The benches (0.1 baselines), the wasm32 runner, `tests/cross_lang_*`, the
  wire suite, the browser/SDK/facade surfaces.
- The Stage-1 deferred receipts and the coordinator/reviewer spot-checks
  (packet §5 preserved credit — not re-litigated here; nothing in this round
  touches their code paths: `mesh_rpc.rs` production is sha-identical and
  `cortex/rpc.rs` production is untouched).
- The stage-end lint battery (`cargo fmt --check`, the clippy invocations,
  the rustdoc lines, `cargo check --workspace --all-targets`) — Main's
  stage-end list, outside this repair round's scoped proof.

**Session-tree writes:** `S1_REPORT.md` §3 (this section) only.

## 4. Stage 2 — core protected client-streaming and duplex

**Lane:** S2Core (one lane). **Pinned brief:** `spikes/org-streaming/S2_BRIEF.md`
@`87f89c8da`. **Base:** `87f89c8da` (Stage 1 accepted at `cc15f4d66`). Date
2026-09-22. Windows host only. Every run `--retries 0 --no-tests=fail` on the
warm aliases (`cargo tf` / `cargo tfl`, graphs pinned in
`net/crates/net/.cargo/config.toml`) from `net/crates/net/`. Claims are
labelled executed vs source-established. This section grows one subsection
per slice (4.1–4.5 = slices 2.1–2.4 + row 5), each landing with its own
`S2.n:` commit pair.

**Owner ruling on the F-S1R-2 rider (relayed by Main during S2.1; recorded):**
the rider is **DECLINED** — the terminal-retarget enhancement, the
caller-side last mile and the delivery-exactly-once composition note are NOT
in Stage 2; the plan-as-written governs and the documented limitation stands
as recorded in `S1_REVIEW_PACKET_2.md` §3.1. Nothing in this stage needs the
rider; if a closure had required it, it would be a finding, not a build.

**Commit-slice note (stated, F-S2.1-3 below):** the S2.1/S2.2 boundary is
RECONSTRUCTED into per-slice commits — one lane implemented both slices in
one working tree before committing. The implementation ORDER was 2.1 then
2.2 (the dispatch order) and every green claim below is executed, but
commit-granular "landed green before the next" was not observable at the
interim moment. The probe worktree
`C:/Users/chief/orca/workspaces/net/org-streaming-s2probe` (detached at
`16cd67e85`, own `target/`) executed each reconstructed tree's green run.

### 4.1 Slice 2.1 — lazy-opening mint

**Landed (executed):** `16cd67e85` — `S2.1: mint the CS/DX lazy opening over
the finalized initial REQUEST` (3 files; `net/crates/net/target/s21.patch` is
the boundary cut). Base `87f89c8da`. All green claims below are executed at
`16cd67e85`'s exact tree (probe worktree, exit codes captured).

**What landed (source-established):**

- **The C11 caller half + the lazy mint (contract 2).**
  `call_client_stream` / `call_duplex` ACCEPT an `org_proof_intent` and mint
  `OrgStreamCallProof` kinds 2/3 LAZILY — at the first `send`/`finish`, over
  the **finalized** initial REQUEST whose body IS the first chunk, so the
  signed opening binds the opening request INCLUDING its body (§1.2's
  transcript runs `org_request_digest`, which covers `req.body`).
  `JustOpened` drop still sends and signs nothing (preserved verbatim —
  the Drop state check is untouched). On `send` (the `&mut self` paths) the
  initial headers are cloned, not taken, so a refused mint leaves the handle
  intact rather than silently publishing a header-less retry.
- **Shared mint helpers (one truth, byte-compatible).** The unary `call` and
  `call_streaming` inline mint blocks move into `check_provider_binding` +
  `attach_signed_admission` (pinned-provider clause, exactly-one-header
  discipline, `sign_admission_proof`, `validate_wire_bounds`, the one-packet
  measurement — same order, same message strings); the capability/TTL
  clauses extract into `validate_org_proof_intent` (called by
  `sign_admission_proof` AND by the CS/DX entries, so a mismatched intent
  fails LOCAL at the call exactly as the eager mints' — the mirror legs'
  requirement). `call_streaming`'s behavior is unchanged (its pin test and
  `a_protected_call_refuses_a_finalized_frame_over_one_packet` stay green).
- **Row-5 pin inversion, first half (delete; replace at S2.5):**
  `call_streaming_mints_a_stream_proof`'s CS/DX intent-refusal legs are
  DELETED (delete-and-replace — the named replacement witness
  `client_stream_and_duplex_mint_their_stream_proofs` lands at
  S2.5 per the dispatch), with the capability-mismatch half and the
  service-routed refusal leg KEPT verbatim and the doc comment naming the
  inversion.

**Witness + counts (executed):**

| Witness | Where | Proves | Named inverse |
|---|---|---|---|
| `client_stream_opening_binds_first_chunk` | `tests/org_rpc_streaming.rs` (integration; the REAL lazy mint + the production §3 transaction `admit_protected_opening`) | the opening's first chunk is bound: altering it AFTER signing is refused with the TYPED `AdmissionDenied::BindingInvalid` and ZERO effects (the §3 reservation rolls back — `record_count` 0, `active_node` 0), while the UNALTERED twin (its own call) admits — the refusal is the alteration, not the construction | R-S2.1 (skip the body digest in the transcript → the altered opening admits → red at its own named assertion) |

- `org_rpc_streaming` binary: **30 → 31** (the 30-roster preserved verbatim +
  the new witness).
- `adapter::net::mesh_rpc` in-source units: **52 → 52** (the pin surgery is a
  named in-place rewrite of `call_streaming_mints_a_stream_proof`, 1 in / 1
  out). The three-module in-source filter
  (`adapter::net::cortex::rpc adapter::net::mesh_rpc org_stream`): **219 →
  219** (Stage 2 adds no in-source units at 2.1 — count-continuous with §3's
  219).
- Green at `16cd67e85` (probe worktree, exit-captured):
  `cargo fmt -p net-mesh -- --check` → exit 0; `cargo tf --retries 0 --test
  org_rpc_streaming` → **31 run / 31 passed / 0 skipped**, exit 0;
  `CARGO_INCREMENTAL=0 cargo tfl --retries 0 adapter::net::mesh_rpc
  adapter::net::cortex::rpc org_stream` → **219 run / 219 passed / 5622
  skipped**, exit 0.

**Inverse receipt R-S2.1 (executed, raw; post-format, exit-captured).**
Production site: `net/crates/net/src/adapter/net/org_admission_gate.rs`
`org_request_digest`'s canonical construction — the transcript skips the
body digest (both mint and verify then derive the body-less digest):

```diff
     let canonical = RpcRequestPayload {
         service: req.service.clone(),
         deadline_ns: req.deadline_ns,
         flags: req.flags,
         headers,
         // `Bytes` clone is a refcount bump, not a copy.
-        body: req.body.clone(),
+        body: req.body.slice(0..0), // R-S2.1 MUTATION: skip the body digest
     };
```

Run (exact): `cargo tf --retries 0 --test org_rpc_streaming -E
'test(=client_stream_opening_binds_first_chunk)'` — **exit 100**. Verbatim:

```
thread 'client_stream_opening_binds_first_chunk' (32740) panicked at tests\org_rpc_streaming.rs:3418:18:
altering the first chunk after signing must be refused with the TYPED `BindingInvalid` — the signed opening binds the first chunk; got Ok("Admitted")
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

error: test run failed
```

The prescribed outcome exactly: without the body digest the altered opening
verifies and ADMITS (`got Ok("Admitted")`). Restore: edit-reversed;
sha256 `69b81aae4532ef73dc2b3364eb3a3ad15680b5c3cb088d630181368040601537`
== the pre-mutation baseline. Restored green (same `-E` run): **exit 0**, 1/1.
(Four weakenings: NONE — the witness is the diff's own inverse and reddens
at its named assertion; nothing was precondition-fixed, widened, relaxed or
deleted.)

**Findings (stated, not decided):**

1. **F-S2.1-1 — row 5's pin inversion is SPLIT across S2.1 and S2.5
   (delete-and-replace preserved; stated).** The CS/DX intent-refusal legs
   inside `call_streaming_mints_a_stream_proof` pin production behavior that
   C11's acceptance REMOVES: they cannot survive the lazy-mint landing, so
   the DELETE rides with the change that breaks them (this slice) and their
   named replacement witness
   `client_stream_and_duplex_mint_their_stream_proofs` lands at its
   dispatched slice (S2.5) with the mirror construction (positive kinds 2/3
   mints + the capability-mismatch half + the service-routed leg). The
   capability-mismatch half and the service-routed refusal leg are kept
   verbatim across both commits. (The alternative — holding the legs until
   S2.5 — would leave the lazy mint unreachable or the estate red.)
2. **F-S2.1-2 — `client_stream_opening_binds_first_chunk`'s "zero handler
   effects" is realized at the §3 transaction boundary (interpretation,
   source-established).** The typed `BindingInvalid` and the reservation
   rollback (`record_count` / `active_node` both 0) are the refusal's
   observable effects at 2.1's green point; a refused opening is never
   fold-driven (no handler, no in-flight entry, no sender, no semaphore).
   The CS/DX protected BRIDGE wiring — where handler darkness is directly
   observable through a live registration — lands with slice 2.2, whose
   witnesses carry the live-bridge darkness probes
   (`pre_admission_chunks_are_never_delivered` et al.). The witness's
   positive control keeps the refusal's cause pinned to the alteration.
3. **F-S2.1-3 — the S2.1/S2.2 commit boundary is RECONSTRUCTED (execution
   note).** One lane implemented both slices in one working tree before
   committing — a deviation from commit-granular "each landed green before
   the next" (the implementation ORDER was the dispatch order and every
   green claim here is executed). The boundary was cut by hunk
   classification into `net/crates/net/target/s21.patch` (2.1's caller-side
   + pin-surgery hunks + the whitespace reflows below; 2.2's hunks and
   `tests/org_rpc_streaming/s2.rs` ride the S2.2 commit) and each
   reconstructed tree's green run was executed in the probe worktree named
   above.
4. **F-S2.1-4 — scoped rustfmt reflowed PRE-EXISTING code in the owned
   files (whitespace only).** The S1-precedent scoped pass
   (`rustfmt --edition 2021 --config skip_children=true` on the touched
   files) reflowed S1_R-era witness/unit whitespace inside
   `src/adapter/net/cortex/rpc.rs` and `tests/org_rpc_streaming.rs` — the
   `cargo fmt -p net-mesh -- --check` gate cannot pass with those hunks
   unformatted. The reflow-only hunks have NO semantic change and ride
   the S2.1 commit — proven by NORMALISATION (whitespace-stripped body
   digests), not by `git diff -w` emptiness (line re-wraps survive `-w`
   as line-count changes; S2_R Row 3 / F-S2R-3 corrects the earlier METHOD
   claim): the three re-wrapped S1_R witnesses are whitespace-identical at
   `87f89c8da` and `017e7148a` —
   `completed_stream_drains_queued_items_in_order_with_content_and_end_terminal`
   (`edaf83163c8fab877fafe5c1cee89a7dd2e486e4675fe069e69c9b5ea5c5b83a`),
   `cross_org_completed_stream_drains_correlated_items_with_end_terminal`
   (`d4f69c897960bf32dd0544e9acd3e6036b9f8eb123e204d866519d96c14df3b9`),
   `response_after_session_replacement_reaches_only_the_live_session`
   (`0ff9d805480e2660a972ca00c8fde8bbf3bcc40736de2b1babb7801d34f80fee`)
   — and the 30-roster is preserved semantically byte-for-byte (the S2
   review's executed normalisation, `S2_REVIEW_PACKET.md` §3).

**Receipt-baseline shas (post-format, the committed trees):**
`cortex/rpc.rs` `961d9529cc16254253e2496c67fac79582bd58607a7d1121131d3670807a4fd0`;
`mesh_rpc.rs` `d71463e5dd0174a80993888661902180b8c73e7802c35f345788a71970e627a8`;
`tests/org_rpc_streaming.rs`
`5a8788e5da27655d96357779be329d2aef3b4292d8bc6db007f45f08b73a8b6c`;
`org_admission_gate.rs`
`69b81aae4532ef73dc2b3364eb3a3ad15680b5c3cb088d630181368040601537`.
(Three earlier receipt cycles ran on the PRE-format tree with exit codes
masked by the output pipeline; they were SUPERSEDED by the post-format,
exit-captured re-runs recorded here and in §4.2 — only the latter are the
evidence.)

**What never ran at 4.1 (complete):** `cargo fmt --all -- --check` (the
evidence rule is per-crate only); the clippy battery, the rustdoc lines and
`cargo check --workspace --all-targets` (Main's stage-end list, per §2.5's
precedent); `cargo tl` / `cargo t` full suites; `tests/cross_lang_*`; the
wire suite; the browser/SDK/facade surfaces (later stages by contract); the
benches; the `webrtc` feature graph; Linux/macOS and `#[cfg(unix)]` legs
(Windows host only); CI itself (branch unpushed; nobody pushes but Main).

### 4.2 Slice 2.2 — CS/DX admission at the `Proceed` seam

**Landed (executed):** `4db0f8a5f` — `S2.2: admit protected client-streaming
and duplex at the Proceed seam` (4 files). Base `f68fc453f`. The commit was
AMENDED once (pre-push) to carry the F-S2.2-5 fix + its regression witness
(found while writing this record). All green claims below are executed at
`4db0f8a5f`'s exact tree; all receipts run against the frozen formatted
code (post-rustfmt shas in §4.1's baseline block are superseded by:
`cortex/rpc.rs`
`22b0ce9029d96bf676d29e28c71f4260c9698152910bd0b470e9e36105e99638`;
`tests/org_rpc_streaming.rs`
`989dc2c77b16033bb458fce7b94977ecabd80eb3c1ce93605822f361d196d8f7`;
`mesh_rpc.rs` unchanged from §4.1's
`d71463e5dd0174a80993888661902180b8c73e7802c35f345788a71970e627a8`;
`org_admission_gate.rs` unchanged
`69b81aae4532ef73dc2b3364eb3a3ad15680b5c3cb088d630181368040601537`).

**What landed (source-established):**

- **One admission seam for every streaming shape.** `ProtectedStreamFold`
  (the SS/CS/DX folds' shared `apply_inbound` / `apply_inbound_admitted`
  shape) generalizes `admit_and_dispatch_protected_stream`; the four
  `serve_rpc_{owner_scoped,granted}_{client_stream,duplex}` seams land
  beside the SS twins (node authority required, `RegisteredRpcService`
  construction + the node-owned replay guard identical to the unary
  bridge), and each CS/DX bridge now branches on the registration's
  admission mode exactly as the unary/SS bridges (public = `bridge_preflight`
  + Gate-3 + fold drive, byte-unchanged; protected = the §3 transaction in
  the same bridge iteration as the fold drive).
- **NC2/§2.8 on the CS/DX emissions.** Protected emissions name the
  authenticated session peer explicitly (`DirectOnly`, `Some(from_node)`)
  and carry the record's REAL receiving incarnation (the route cache's
  `(node, receiving session_id)` pair — R2-A); denials and the protected
  terminal (a CS call's single response IS its terminal; a DX call's `end`
  frame) ride the §8a `RpcResponseJob` drainer (§2.8's `Sent` / `Refused` /
  `Unreachable` disposition seam); `ServeHandle::drop` retires the
  registration's protected set (Q3/C9) and every CS/DX handle now carries
  `protected_streams: Some(..)`.
- **The §2.6/§2.2 machinery on the CS/DX folds.** `apply_inbound_admitted`
  mirrors the SS seam verbatim (§2.1 resolution before any handler effect,
  `DeadlineExceedsPolicy` refusal, the E1.6 strip, §3 step-5's ownership
  TRANSFER via `registry.confirm`); `StreamCallRecord::new_client_streaming`
  / `new_duplex` (both halves `Open`) + `end_input` (END closes input ONCE,
  idempotent, never touching output); `run_client_stream_call` (§2.2's
  bounded supervision for the single-response shape — its bounded emission
  IS the drain-complete event: `handler_returned` ⇒ `Draining` + input
  `Closed`, then `pump_exited` ⇒ `Completed(result)`, the handler's own
  payload emitted verbatim) and `SupervisedHandler::Duplex` (one supervisor
  body owning handler/pump/semaphore/terminal for SS + DX); protected
  CANCEL retires through the supervisor (first-writer-wins terminal, the
  registration's single removal covers `in_flight` / `flow_control` /
  `senders` / the protected set / the registry record).
- **Shape + direction gating before delivery/credit.** `cs_request_flags_ok`
  / `dx_request_flags_ok` refuse `ShapeMismatch` at admission; the
  request-chunk path is 4-tuple (`from_node`, receiving incarnation,
  origin, call_id) keyed and record-gated (terminal or non-`Open` input ⇒
  dropped, no delivery, no credit); `deliver_protected_body` performs the
  §2.7 byte accounting + the §2.3 check-and-commit as ONE ownership
  operation (the opening body reserves like any chunk — §2.7 "refuse the
  call, never truncate it", `ResourceExhausted` ⇒ wire `Unavailable`); the
  DX fold gains the `flow_control` map + `STREAM_GRANT` arm (the record's
  `credit_grantable` gate: credit survives the handler's return for the
  drain and stops at terminal/ended-output; the cross-direction grant kind
  `DISPATCH_RPC_REQUEST_GRANT` never reaches the arm).
- **F-S2.2-5's fix** (below): a pre-supervisor opening-body refusal settles
  the record's release-once `complete` in the fold.

**Witnesses + counts (executed):** `org_rpc_streaming` binary **31 → 37**.
The five dispatched witnesses plus the F-S2.2-5 regression:

| Witness | Asserted observation | Named inverse |
|---|---|---|
| `client_stream_aggregate_with_valid_proof` | a VALID owner-delegated kind-2 opening + one continuation chunk + END aggregate into the call's ONE response (content-joined in order) at the authenticated receiving endpoint, with the four-party attribution observed on `RpcStreamingContext::org_admission` and the proof header stripped (E1.6) — and it stays one response ever (§2.6's single-response rule) | — |
| `duplex_exchange_with_valid_proof` | the CROSS-ORG shape (`serve_rpc_granted_duplex` + the B→A grant intent): echoed bodies IN ORDER, a content-labelled TAIL after input EOF (independent halves), then EXACTLY ONE terminal with the exact wire content (status `Ok` + `nrpc-streaming: end`) | — |
| `pre_admission_chunks_are_never_delivered` | chunks + an END for a call with NO admitted opening are never delivered (handler dark, empty `in_flight_keys()`/`sender_keys()`, endpoint silent through the bounded darkness window); when the real opening arrives the aggregate is EXACTLY the post-admission bodies | — |
| `end_cannot_cancel_another_stream_or_reopen_terminal_half` | two live CS calls: B's END closes ONLY B's input half (`input_half() == Ended`) while A's stays `Open` and A keeps receiving; a SECOND END never reopens B's half (still `Ended`, no sender reappears) and a late B chunk is discarded; both calls' remaining output completes exactly once (body-content asserted) | — |
| `wrong_session_grant_does_not_release_credit` | a zero-credit protected DX window under 4-tuple keys: a WRONG-session `STREAM_GRANT` never releases credit (named) AND a wrong-session REQUEST_CHUNK is never delivered (named — the brief's prescribed inverse's target); the exact session's grant releases and the exchange completes (positive control both ways) | R-S2.2a (the brief's: chunks keyed 3-tuple) + R-S2.2b |
| `opening_body_budget_refusal_completes_the_record` (F-S2.2-5 regression) | an opening body over the per-call byte budget is refused `ResourceExhausted` (0x0009 + coarse `Unavailable`) with ZERO delivery and the §3 record COMPLETED (`record_count` 0, `active_node` 0) | R-S2.2c (the fix removed — the PRE-FIX leak reproduces) |

Green at `4db0f8a5f` (executed, exit-captured): `cargo fmt -p net-mesh --
--check` → exit 0; `cargo tf --retries 0 --test org_rpc_streaming` →
**37 run / 37 passed / 0 skipped**, exit 0; the three-module in-source
filter → **219 / 219**, exit 0 (Stage 2 adds no in-source units here —
count-continuous with §3's 219).

**Inverse receipts (executed, raw; the frozen formatted tree).** Each
cycle: bounded diff at the PRODUCTION site → the witness's named assertion
red (a compile error is never a red) → edit-reversed restore, sha256-proven
== baseline → restored green (same `-E` run). Exact command shape: `cargo
tf --retries 0 --test org_rpc_streaming -E 'test(=<witness>)'`, from
`net/crates/net/`.

**R-S2.2a — the brief's prescribed inverse: request chunks delivered on
`(node, origin, call_id)` only (3-tuple).** The sender map's insert keys
drop the receiving-incarnation term and the chunk-path lookup drops it too
(the faithful "3-tuple keying" scheme — 3 bounded hunks in
`cortex/rpc.rs`):

```diff
-    let key = (from_node, session_id, meta.origin_hash, meta.seq_or_ts);
+    let key = (from_node, 0, meta.origin_hash, meta.seq_or_ts); // R-S2.2a MUTATION: 3-tuple chunk key
     let is_end = payload.flags & FLAG_RPC_REQUEST_END != 0;
```
```diff
-                key,
+                (key.0, 0, key.2, key.3), // R-S2.2a MUTATION: 3-tuple chunk key
                 RequestChunkSender {
                     tx,
                     charge: call_ref.clone(),
```
(×2 — the CS and DX admitted seams)

**exit 100.** Verbatim:

```
thread 'wrong_session_grant_does_not_release_credit' (187368) panicked at tests\org_rpc_streaming.rs:4277:5:
assertion `left == right` failed: a wrong-session REQUEST_CHUNK is never delivered — the opening body is all the handler ever sees
  left: 2
 right: 1
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
```

The prescribed outcome exactly: the wrong-session chunk WAS delivered
(`left: 2`) and the witness reds at its own named assertion. Restore:
edit-reversed; sha256
`22b0ce9029d96bf676d29e28c71f4260c9698152910bd0b470e9e36105e99638` ==
baseline. Restored green: exit 0, 1/1.

**R-S2.2b — the grant-credit lookup ignores its key** (the wrong-session
grant's own inverse; `cortex/rpc.rs`, both arms' shared line):

```diff
-if let Some(sem) = self.flow_control.lock().get(&key).cloned() {
+if let Some(sem) = self.flow_control.lock().values().next().cloned() { // R-S2.2b MUTATION: lookup ignores the key
```

**exit 100.** Verbatim:

```
thread 'wrong_session_grant_does_not_release_credit' (186036) panicked at tests\org_rpc_streaming.rs:4283:5:
assertion `left == right` failed: a wrong-session STREAM_GRANT does not release credit
  left: Some(4)
 right: Some(0)
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
```

The wrong-session grant released credit (`Some(4)` = 5 granted − 1 consumed
by the parked pump's queued echo). Restore: edit-reversed; same sha ==
baseline. Restored green: exit 0, 1/1.

**R-S2.2c — F-S2.2-5's fix removed (the pre-fix shape; `cortex/rpc.rs`,
the CS seam):**

```diff
                         self.in_flight.lock().remove(&key);
-                        charge.registry.complete(&charge.key, charge.incarnation);
+                        // R-S2.2c MUTATION: the fix removed (pre-fix shape)
                         return Err(AdmissionDenied::ResourceExhausted);
```

**exit 100.** Verbatim:

```
thread 'opening_body_budget_refusal_completes_the_record' (184388) panicked at tests\org_rpc_streaming.rs:4455:5:
assertion `left == right` failed: the pre-supervisor refusal completes the record's single removal
  left: 1
 right: 0
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
```

The pre-fix leak reproduces exactly (`record_count` stuck at 1 — the
terminal record owning its key forever). Restore: edit-reversed; same sha
== baseline. Restored green: exit 0, 1/1. (Four weakenings on all three:
NONE — each witness is its diff's own inverse and reddens at its named
assertion.)

**Findings (stated, not decided):**

1. **F-S2.2-1 — the 2.2/2.3 production split on the DX response
   flow-control (stated).** The plan's rows overlap on this region (2.3's
   named `cortex/rpc.rs` targets are a SUBSET of 2.2's). The `flow_control`
   map + `STREAM_GRANT` arm + the protected window install land here —
   2.2's "GRANT checked … before credit" gate needs the credit to gate
   (`wrong_session_grant_does_not_release_credit` observes it through
   `flow_control_permits`) — while the PUBLIC window honouring (C8's
   approved public change: install at the public arm + the public pump's
   per-chunk acquire) lands at 2.3 with its named witnesses.
2. **F-S2.2-2 — `wrong_session_grant_does_not_release_credit` carries BOTH
   wrong-session probes (interpretation).** The brief's name covers the
   grant half while its prescribed inverse ("deliver chunks on `(node,
   origin, call_id)` only") targets CHUNK delivery; the witness pins both
   named assertions (chunk non-delivery first — R-S2.2a's red target; grant
   non-release second — R-S2.2b's).
3. **F-S2.2-3 — the CS/DX emitters carry the cache-derived receiving
   incarnation uniformly (stated).** The protected path requires the
   record's real `session_id` (NC2/contract 5); the closures mirror the SS
   emitter exactly, including passing the cache-derived value on the public
   inline path (previously hardcoded `0`). F-S1R-1 established that value's
   only consumers are `webrtc`-gated code and tracing fields — no
   sanctioned-graph observable change.
4. **F-S2.2-4 — `SupervisedHandler` realizes the §2.2 supervisor across the
   pumping shapes (seam shape).** One supervisor body owns the §2.2 table's
   pieces for SS + DX (the handler future via an owned async block — the
   `async_trait` self-borrow); client-streaming runs
   `run_client_stream_call` per §2.2's own carve-out ("a single-response
   emitter, not an SS/DX pump").
5. **F-S2.2-5 — a pre-supervisor opening-body refusal needed the fold as
   cleanup owner (found + FIXED with regression witness + receipt).**
   `apply_inbound_admitted` transfers ownership (§3 step-5) BEFORE the
   opening body's §2.7 delivery; a budget refusal there postdates the
   transfer but predates the supervisor, so the release-once `complete`
   had no owner — the terminal registry record kept its key forever
   (`record_count` stuck at 1, `ActiveCallOwned` for any successor).
   Fixed in the CS and DX seams (the unary `ConfirmedOpening` scope-guard
   precedent); `opening_body_budget_refusal_completes_the_record` is the
   reproduction (fails pre-fix under R-S2.2c, passes post-fix).

**What never ran at 4.2 (complete):** the same list as §4.1's (the fmt
gate IS executed here — exit 0; `cargo fmt --all`, clippy, rustdoc,
`--workspace --all-targets` remain Main's stage-end list), plus: the full
preserved + public-regression-control batch (§4.5's stage-end run), the
Stage 0 model suite as such (inside the 219), and every later-stage
surface. Two earlier receipt cycles (an R-S2.2a/b pair) were lost to a
same-file parallel-mutation race and are NOT cited anywhere — they were
superseded by the sequential re-runs recorded above (the race's outputs
are discarded, not merged).

### 4.3 Slice 2.3 — duplex response flow control (C8)

**Landed (executed):** `S2.3: honor the duplex response window on the
public fold (C8)` (3 files) on base `5e1b079c0`. All receipts run against
the frozen formatted tree; `cortex/rpc.rs` receipt baseline for this
slice: `70b59a13884054675a2aeefd8f30803fe15be232497c9a18454fa9d0a81a70d7`.

**What landed (source-established):** the C8 public-half completion (the
protected machinery landed at 2.2, F-S2.2-1): the public duplex REQUEST
arm installs the opted-in `nrpc-stream-window-initial` response
semaphore into the `flow_control` map and the public pump pays one credit
per chunk before emitting (the SS pump's acquire clause, including its
closed-semaphore `break`); the public CANCEL arm drops the window entry
(the SS clause). `DISPATCH_RPC_STREAM_GRANT` remains the ONLY credit
source — the cross-direction `DISPATCH_RPC_REQUEST_GRANT` kind
(server → caller upload grants) never reaches the arm.

**Witnesses + counts (executed):** `org_rpc_streaming` binary **37 → 39**.

| Witness | Asserted observation | Named inverse |
|---|---|---|
| `duplex_response_window_blocks_until_grant` | C8's regression witness, PROTECTED and PUBLIC legs: with zero initial credit the response pump PARKS (the queued echo publishes NOTHING through the bounded darkness window on both folds; `flow_control_permits == Some(0)`), and a `STREAM_GRANT` on the exact session releases it — the echo and then ONE terminal (exact wire content asserted on the public leg) complete | R-S2.3a ("remove the grant arm → the initial block occurs but never releases") + R-S2.3b ("bypass the semaphore → the pre-grant blocking assertion fails") |
| `cross_direction_grant_is_ignored` | the direction half: an inbound `REQUEST_GRANT` (the upload kind — the wrong direction for a response window) never releases response credit (window stays `Some(0)`, endpoint silent) while the `STREAM_GRANT` kind does (positive control); the shape half: a `STREAM_GRANT` at a client-streaming call (no response pump to credit) is a no-op — its single response completes grant or not | — |

Green (executed, exit-captured): `cargo fmt -p net-mesh -- --check` →
exit 0; `cargo tf --retries 0 --test org_rpc_streaming` → **39 run / 39
passed / 0 skipped**, exit 0.

**Inverse receipts (executed, raw).** Exact command shape: `cargo tf
--retries 0 --test org_rpc_streaming -E 'test(=duplex_response_window_blocks_until_grant)'`.

**R-S2.3a — the grant arm removed (`cortex/rpc.rs`, the DX fold's
`DISPATCH_RPC_STREAM_GRANT` arm):**

```diff
             DISPATCH_RPC_STREAM_GRANT => {
+                return Ok(()); // R-S2.3a MUTATION: the grant arm is removed
                 // Response-direction credit (Stage 2 slices 2.2/2.3, C8):
```

**exit 100.** Verbatim:

```
thread 'duplex_response_window_blocks_until_grant' (188144) panicked at tests\org_rpc_streaming.rs:4576:5:
the grant releases the blocked protected output — echo + terminal complete
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
```

The prescribed outcome exactly: the block occurred and NEVER released.
Restore: edit-reversed; sha256
`70b59a13884054675a2aeefd8f30803fe15be232497c9a18454fa9d0a81a70d7`
== baseline. Restored green: exit 0, 1/1.

**R-S2.3b — bypass the semaphore (`cortex/rpc.rs`, the public pump's
credit):**

```diff
-                let pump_flow = flow_sem.clone();
+                let pump_flow: Option<Arc<tokio::sync::Semaphore>> = None; // R-S2.3b MUTATION: bypass the semaphore
```

**exit 100.** Verbatim (the red lands in the shared darkness helper
`s15.rs:202:9`, carrying the witness's named observation string):

```
thread 'duplex_response_window_blocks_until_grant' (189740) panicked at tests\org_rpc_streaming\s15.rs:202:9:
assertion `left == right` failed: public zero credit publishes nothing before a grant (C8): 1 frame(s) reached the roster subscriber — a protected response was fanned out
  left: 1
 right: 0
```

The prescribed outcome exactly: with the semaphore bypassed the
PRE-GRANT blocking assertion fails (the echo published without credit).
Restore: edit-reversed; same sha == baseline. Restored green: exit 0,
1/1. (Four weakenings on both: NONE.)

**Findings (stated, not decided):**

1. **F-S2.3-1 — the public credit-parked pump after CANCEL matches the SS
   public fold's existing shape (stated).** The public CANCEL arm removes
   the map entry (SS parity) but does not `close()` a removed semaphore —
   a public pump already parked on credit with a queued chunk under a
   never-granted window stays parked (the SS public fold has the same
   shape since Stage 1; §2.2's closed-semaphore wake is named as
   protected-supervisor behavior). PROTECTED records are unaffected (the
   supervisor closes the semaphore at retire). Not changed here: fixing
   the public SS/DX shape is public-behavior territory beyond C5–C8.

**What never ran at 4.3 (complete):** the §4.1 list unchanged (the fmt
gate IS executed — exit 0). Two witness-construction iterations preceded
the recorded state (missing upload ENDs in the new witnesses — test-side
construction, caught by the witnesses' own timeouts, fixed before any
receipt ran; and one fuzzy-edit corruption of the DX seam's `impl` header
— repaired, compile-caught, never committed).

### 4.4 Slice 2.4 — half-close + both-direction retirement

**Landed (executed):** `S2.4: half-close discipline and both-direction
retirement` (3 files) on base `98d5ddfd4`. `cortex/rpc.rs` receipt
baseline for this slice:
`d3da02cfcec9198f67ad5444a6adaafe16539c1149c5c84ab4fd1c8ee372e6ee`.

**What landed (source-established):** the §2.6 table's production delta
completes with `RequestStream`'s retire-awareness (§2.2's
request-chunk-queue row: a PROTECTED `poll_next` honors the RETIRED call
BEFORE yielding — buffered items discarded through the queue owner;
`RequestStream::new_protected` wires the retire signal at the CS/DX
admitted seams; public uploads unchanged) — the rest of the slice's named
change (END closes input ONCE; retire closes both halves; independent
halves under one record) rode 2.2's record machinery (`end_input`, the
record gates in `apply_request_chunk_to_senders`, the supervisors' forced
paths) and is BOUND here by its named witnesses (F-S2.4-2).

**Witnesses + counts (executed):** `org_rpc_streaming` binary **39 → 41**.

| Witness | Asserted observation | Named inverse |
|---|---|---|
| `upload_end_then_remaining_output_completes` | the §2.6 half-close independence under one record: the upload END closes input ONCE (`input_half() == Ended`, observed on the LIVE record held by its credit-parked output) and a second END / late post-END chunk change nothing (never reopens, never delivered); the handler's REMAINING OUTPUT — echoes IN ORDER then the post-EOF tail — and ONE terminal (exact wire content: `Ok` + `nrpc-streaming: end`) complete after the early END | R-S2.4a (END retires both halves instead of half-closing) |
| `retire_unblocks_both_directions` | a retire closes BOTH halves: the INPUT-side waiter (the handler parked on request reads) is released — its owned future drops (§2.2's "cancellation signaled and the owned future dropped", `DropFlag`) — and the input admission is closed through the queue owner (`sender_keys()` empty); the OUTPUT-side waiter (the credit-parked pump holding a queued echo) is stopped: exactly ONE `Cancelled` terminal, and a LATE `STREAM_GRANT` after the terminal never resurrects the pump ("no chunk is published after the terminal" — §2.2's abort+join invariant) | R-S2.4b (the input admission not closed) + R-S2.4c-v2 (the true two-layer output inverse) |

Green (executed, exit-captured): `cargo fmt -p net-mesh -- --check` →
exit 0; `cargo tf --retries 0 --test org_rpc_streaming` → **41 run / 41
passed / 0 skipped**, exit 0.

**Inverse receipts (executed, raw).** Command shape: `cargo tf --retries 0
--test org_rpc_streaming -E 'test(=<witness>)'`.

**R-S2.4a — END retires both halves instead of half-closing
(`cortex/rpc.rs`, `apply_request_chunk_to_senders`' END path):**

```diff
         if let Some(record) = sender.record.as_ref() {
-            record.lock().end_input();
+            record.lock().retire(StreamTerminalReason::Cancelled); // R-S2.4a MUTATION: END retires both halves
         }
```

**exit 100.** Verbatim:

```
thread 'upload_end_then_remaining_output_completes' (184712) panicked at tests\org_rpc_streaming.rs:5012:5:
the upload END closes the input half
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
```

The input half never reports its one-time close (the mutation's retire
touches no input state). Restore: edit-reversed; sha256
`d3da02cfcec9198f67ad5444a6adaafe16539c1149c5c84ab4fd1c8ee372e6ee`
== baseline. Restored green: exit 0, 1/1.

**R-S2.4b — the input admission is not closed through the queue owner
(`cortex/rpc.rs`, `StreamCallRegistration::complete`):**

```diff
-        if let Some(senders) = self.senders.as_ref() {
-            senders.lock().remove(&self.key);
-        }
+        // R-S2.4b MUTATION: the input admission is not closed (senders kept)
+        let _ = &self.senders;
```

**exit 100.** Verbatim:

```
thread 'retire_unblocks_both_directions' (189580) panicked at tests\org_rpc_streaming.rs:5203:5:
the retire closed the input admission through the queue owner
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
```

Restore: edit-reversed; same sha == baseline. Restored green: exit 0, 1/1.

**R-S2.4c-v2 — the true two-layer output inverse (the pump never stopped
AND the window entry kept live).** Layer 1 = the forced path's
`sem.close()` + abort/join removed; layer 2 = `complete()`'s
`flow_control` removal removed:

```diff
-        if let Some(sem) = flow_sem.as_ref() {
-            sem.close();
-        }
-        if !pump_done {
-            pump_task.as_mut().abort();
-            let _ = pump_task.as_mut().await;
-        }
+        // R-S2.4c-v2 MUTATION (layer 1): the pump is never stopped
```
```diff
-        if let Some(flow_control) = self.flow_control.as_ref() {
-            flow_control.lock().remove(&self.key);
-        }
+        // R-S2.4c-v2 MUTATION (layer 2): the window entry is kept live
```

**exit 100.** Verbatim:

```
thread 'retire_unblocks_both_directions' (175592) panicked at tests\org_rpc_streaming.rs:5250:5:
assertion `left == right` failed: no chunk is published after the terminal — the late grant never resurrects the stopped pump
  left: 2
 right: 1
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
```

The late grant resurrected the unstopped pump and the queued echo
published AFTER the terminal (`left: 2`). Restore: edit-reversed (both
layers); same sha == baseline. Restored green: exit 0, 1/1.

**Findings (stated, not decided):**

1. **F-S2.4-1 — `retire_unblocks_both_directions` is GREEN under the
   SINGLE-layer pump-stop inverse (disclosed; four weakenings: NONE apply
   to the witness — its assertions are exact and unmoved).** Removing only
   the forced path's `sem.close()` + abort leaves the witness green: the
   late `STREAM_GRANT` cannot reach the parked pump because
   `StreamCallRegistration::complete()`'s `flow_control` removal is an
   INDEPENDENT second defense (the grant's map lookup misses). The
   named property ("no chunk is published after the terminal") holds via
   the layered defenses; its true inverse removes BOTH output-side layers
   (R-S2.4c-v2, red at the witness's own named assertion). Recorded
   because the brief's inverse-column discipline would otherwise read the
   single-layer cycle as a non-discriminating witness; it is not — the
   single-layer mutation is not the property's inverse.
2. **F-S2.4-2 — most of slice 2.4's named change rode 2.2's record
   machinery (stated).** The plan's rows overlap here exactly as in
   F-S2.2-1: `end_input` (END-once), the record gates (late input
   refused) and the supervisors' forced paths (retire closes both halves)
   landed at 2.2 because its own witnesses bind them
   (`end_cannot_cancel_another_stream_or_reopen_terminal_half`); this
   slice adds the §2.2 queue-owner row's `poll_next` retire-awareness and
   binds what its named pair actually observes — END closes input once and
   retire closes both halves (`upload_end_then_remaining_output_completes`,
   `retire_unblocks_both_directions`) — NOT the full §2.6 table (S2_R
   Row 1 / F-S2R-1's record-quality correction): the handler-return
   late-input disposition (§2.6's input-`Closed` rule and the chunk-path
   record gate) was UNWITNESSED until §5's
   `early_handler_return_refuses_late_input_without_resource_exhausted`.

**What never ran at 4.4 (complete):** the §4.1 list unchanged (the fmt
gate IS executed — exit 0).

### 4.5 Row 5 — the CS/DX intent-refusal pins invert (S2.5) + stage close

**Landed (executed):** `1dcbf2545` — `S2.5: the CS/DX mint pins invert (row
5's replacement witness)` (1 file: `mesh_rpc.rs`) on base `89b498aa0`.
`mesh_rpc.rs` receipt baseline for this slice:
`59a4c6a9a7cd085136486308cfe1a6dd7aca886c936464a5f1af872adbb1aa89`.

**What landed (source-established):** F-S1.6-2's inversion COMPLETES
(delete-and-replace — F-S2.1-1's second half):
`client_stream_and_duplex_mint_their_stream_proofs` mirrors 1.6's
`call_streaming_mints_a_stream_proof` EXACTLY. Positive halves: the LAZY
mints' output observed at the provider's request dispatcher — full
strict-decoded `OrgStreamCallProof` (the strict decoder's refusal of
truncated/trailing/unknown-kind values is the 1.2 pin's), kind =
client-streaming (2) / duplex (3), `session_binding` = the exact live
session's Noise handshake hash (§1.3), each minted over the FINALIZED
initial REQUEST at the first `send` (contract 2). Kept halves (verbatim
mirror): the capability-mismatch leg (`call_client_stream`/`call_duplex`
against a DIFFERENT service fail LOCAL with `RpcError::Codec`) and the
service-routed refusal leg (`call_service_streaming` rejects the intent
at the TOP, before discovery).

**Unit totals before/after by module (row 5's requirement; executed):**

| Module / binary | before Stage 2 | after Stage 2 | delta |
|---|---|---|---|
| `behavior::org_admission::` tests | 21 | 21 | count-unchanged |
| `adapter::net::mesh_rpc` filter (39 in `mesh_rpc.rs` + 13 in `mesh_rpc_metrics.rs`) | 52 | 53 | +1 (row 5's mirror; S2.1's pin surgery was a named in-place rewrite, 1 in / 1 out) |
| `adapter::net::cortex::rpc` in-source tests | 91 | 91 | count-unchanged (Stage 2 adds no in-source units) |
| `org_stream` (the Stage 0 models) | 76 | 76 | UNTOUCHED and green |
| `org_rpc_streaming` integration binary | 30 | 41 | +11 (one named witness per dispatched2.1–2.4 property + the F-S2.2-5 regression) |

**Inverse receipt R-S2.5 (executed, raw; the mirror of 1.6's R2).**
Production site: `mesh_rpc.rs` `attach_signed_admission`'s mint call — the
mint shape flipped to `Unary`:

```diff
-    let header = sign_admission_proof(intent, call_id, req, call_shape, session_binding)?;
+    let header = sign_admission_proof(intent, call_id, req, RpcCallShape::Unary, session_binding)?; // R-S2.5 MUTATION: the mint shape is Unary
```

Run (exact): `cargo tfl --retries 0
adapter::net::mesh_rpc::roster_fallback_tests::client_stream_and_duplex_mint_their_stream_proofs`
— **exit 100**. Verbatim:

```
thread 'adapter::net::mesh_rpc::roster_fallback_tests::client_stream_and_duplex_mint_their_stream_proofs' (189364) panicked at src\adapter\net\mesh_rpc.rs:10764:14:
the minted bytes are a FULL streaming proof (strict decode): InvalidFormat
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
```

The strict streaming decoder refuses the unary-format value (the 1.6 R2
outcome shape exactly) — the witness discriminates "mints a STREAM proof"
for both kinds. Restore: edit-reversed; sha256
`59a4c6a9a7cd085136486308cfe1a6dd7aca886c936464a5f1af872adbb1aa89`
== baseline. Restored green (same run): exit 0, 1/1. (Four weakenings:
NONE.)

**Stage 2 close — green at the exact head (executed, exit-captured):**

- `cargo fmt -p net-mesh -- --check` → exit 0.
- `cargo tf --retries 0 --test org_rpc_streaming` → **41 run / 41
  passed / 0 skipped**, exit 0. Roster FROM SOURCE (41 `#[test]` /
  `#[tokio::test]` fns in `tests/org_rpc_streaming.rs`; the four helper
  fns carry no test attribute; the helper modules carry none) == 41
  executed. The 30-roster of §3.2 is preserved VERBATIM (byte-untouched
  bodies); the 11 new names:
  `client_stream_opening_binds_first_chunk`,
  `client_stream_aggregate_with_valid_proof`,
  `duplex_exchange_with_valid_proof`,
  `pre_admission_chunks_are_never_delivered`,
  `end_cannot_cancel_another_stream_or_reopen_terminal_half`,
  `wrong_session_grant_does_not_release_credit`,
  `opening_body_budget_refusal_completes_the_record`,
  `duplex_response_window_blocks_until_grant`,
  `cross_direction_grant_is_ignored`,
  `upload_end_then_remaining_output_completes`,
  `retire_unblocks_both_directions`.
  **CI floor re-pin for Main (never edited here): `org_rpc_streaming`
  30 → 41; the full name list is §3.2's 30 verbatim + the 11 above.**
- In-source units (exit-captured, each its own run):
  `adapter::net::behavior::org_admission::` → **21 / 21**, exit 0;
  `adapter::net::mesh_rpc` → **53 / 53**, exit 0 (the in-source bridge
  trio preserved: `client_stream_bridge_rejects_before_fold_end_to_end`,
  `duplex_bridge_rejects_before_fold_end_to_end`,
  `reject_relayed_flow_controlled_request_rejects_only_relayed_flow_controlled_uploads`,
  plus `call_streaming_mints_a_stream_proof` with its kept halves and the
  row-5 mirror);
  `adapter::net::cortex::rpc` → **91 / 91**, exit 0;
  `org_stream` → **76 / 76**, exit 0. The three-module filter = **220**
  (was 219).
- Preserved + public regression control, ONE invocation (10 binaries):
  **134 run / 134 passed / 0 skipped**, exit 0 —
  `nrpc_streaming_gate` (incl. the preserved trio
  `client_streaming_denies_unauthorized_caller`,
  `duplex_denies_unauthorized_caller`,
  `denial_is_not_fanned_out_to_the_reply_roster`),
  `integration_nrpc_streaming`, `integration_nrpc_client_streaming`,
  `integration_nrpc_duplex`, `nrpc_registration_order`,
  `integration_nrpc_protected`, `org_admission_wire`,
  `subnet_org_boundary`, `org_ownership`, `cross_lang_wire`.
- The remaining cross_lang-named controls + `org_admission_gate`, ONE
  invocation (4 binaries): **32 / 32**, exit 0 —
  `cross_lang_capability_fixtures`, `integration_nrpc_cross_lang`,
  `integration_nrpc_cross_lang_streaming`, `org_admission_gate`.
- Stage 0 models UNTOUCHED (executed: `git log 87f89c8da..HEAD --
  org_stream_{lifecycle,registry}.rs` is EMPTY) and green (76/76).
- Ten inverse receipt cycles (R-S2.1, R-S2.2a/b/c, R-S2.3a/b,
  R-S2.4a/b/c-v2, R-S2.5) — each red at its own named assertion (a
  compile error never counted as a red), each sha-proven restored, each
  closed with a green re-run. Plus one disclosed GREEN observation
  (F-S2.4-1's single-layer cycle — not the property's inverse).

**What never ran at 4.5 (complete):** `cargo fmt --all -- --check` (the
evidence rule is per-crate only — the per-crate gate IS executed, exit
0); the clippy battery (4 invocations), the rustdoc lines and `cargo
check --workspace --all-targets` (Main's stage-end list, §2.5's
precedent); `cargo tl` / `cargo t` full suites (the named filters above
are the executed scope); the benches; the `webrtc` feature graph;
Linux/macOS and `#[cfg(unix)]` legs (Windows host only); the browser /
SDK / facade surfaces (Stage 3+ by contract); CI itself (branch unpushed;
nobody pushes but Main — the floor re-pin rides with Main's same-commit
pin). Owner-pending boundary: the F-S1R-2 rider was DECLINED mid-stage
(§4's header); no closure here requires it — the §2.8
session-replacement terminal-drop limitation stands as documented in
`S1_REVIEW_PACKET_2.md` §3.1.

## 5. Repair round (S2_R)

The S2 review's ACCEPT (`S2_REVIEW_PACKET.md` @`017e7148a`) stands; this
round closes its three non-blocking findings per the PINNED REPAIR BRIEF
`spikes/org-streaming/S2_R_BRIEF.md` @`4bf24aad7` — F-S2R-1 (the §2.6
late-input disposition witness), F-S2R-2 (the post-transfer
`ConfirmedOpening`-shape scope guard — the ONE authorized production
change: the plan §3 step-5 mandate) and F-S2R-3 (the proof-method
wording). Nothing else changes. The S2R code commit is `c17572f03`.

### 5.1 What landed (executed)

**Row 1 — F-S2R-1 (witness gap: the §2.6 late-input disposition).**
Closure property (packet §7, verbatim): "a named witness in which a
PROTECTED client-streaming or duplex handler returns EARLY (before the
caller's END) with its input half still `Open`, a request chunk then
arrives for the call, and the observed outcome is: the chunk is
refused/discarded — never delivered, never retained — no `ResourceExhausted`
is latched, and the handler's own result remains the terminal (exact wire
content asserted) with the record completing exactly once — reddening
under (a) the input-Closed-on-return rule removed (`handler_returned`)
and (b) the chunk-path record gate removed." (Plus its belt discipline:
"the sender-map removal at `complete()` must not be able to satisfy this
witness alone.")

MET by `early_handler_return_refuses_late_input_without_resource_exhausted`
(`tests/org_rpc_streaming.rs:5278`): a PROTECTED duplex handler
(`s2::EarlyReturnDX`) returns EARLY — before the caller's END (none is
ever sent) with its input half still `Open` at return — carrying its
TYPED error `Application(0x007E, "ER-early-9")`; the zero-credit response
window parks the pump holding its one queued echo, so the call stays LIVE
(its §2.4 single removal has not run) while the late request chunk
`ER-LATE-7` arrives. Observed (executed, green): the chunk is
refused/discarded — the handler's delivered aggregate is exactly
`[ER-req-1]` (the late chunk is nowhere) and the endpoint stays silent
through the 200 ms darkness window (never delivered, never retained); no
`ResourceExhausted` is latched (THE named assertion — the parked record
reads `(is_live, terminal) == (true, None)`); the handler's own result is
the terminal's exact wire content (`Application(0x007E)`, no headers,
`ER-early-9`) after the queued echo publishes first; and the record
completes exactly once (`removals(&key, 1) == 1`, `record_count() == 0`,
`active_node() == 0`, both fold maps empty, "exactly one terminal, ever"
over a 250 ms re-check). Belt discipline held: at the injection moment
`sender_keys()` still contains the call's key — the §2.4 single removal
has not run and cannot be what refuses the chunk. Both PAIR mutations
redden at the named no-latch assertion (§5.2, R-S2R-1a/1b). Determinism
(executed reasoning + run shape): the chunk is injected only after the
handler's own return signal, and on the `#[tokio::test]` current-thread
runtime the supervisor's `handler_returned` runs in the same task poll
that completes the handler future — the injection is strictly
post-return, with no sleep-based coincidence.

**Row 2 — F-S2R-2 (fix class: the post-transfer scope guard).** Closure
property (packet §7, verbatim): "the CS and DX `apply_inbound_admitted`
post-transfer windows are covered by a `ConfirmedOpening`-shape scope
guard such that ANY exit after `registry.confirm` (a delivery refusal, a
scheduling/installation failure, or a panic) settles the record's
release-once `complete` and every map entry — demonstrated by a witness in
the shape of §6.5's probe (a post-transfer installation failure) asserting
`record_count() == 0` and an empty `in_flight_keys()`, failing (orphan)
without the guard and passing with it; the F-S2.2-5 witness stays green
throughout."

MET by `ConfirmedStreamOpening` (`cortex/rpc.rs`) in BOTH seams — the
unary `ConfirmedOpening` precedent (its own doc: "this scope guard stands
in for it so no `Running` record is ever orphaned"), completed to the §2.4
single removal point: armed at `registry.confirm`, its Drop runs
`StreamCallRegistration::complete` (the release-once `complete` + every
map entry — in-flight/sender/flow/window), and the `tokio::spawn`
transfer defuses it (the supervisor's own registration remains the single
removal point exactly as before). The inline F-S2.2-5 settlement is
FOLDED into the guard (ONE settlement mechanism — the brief's "your call"
option) and its fail-pre-fix property survives VERBATIM: §5.2's
R-S2.2c-v2 reds at `tests/org_rpc_streaming.rs:4455:5` byte-identically
to R-S2.2c's receipt quote. Demonstrated by
`post_transfer_scope_guard_never_orphans_a_running_record` (the in-source
probe-witness, the lib filter's 221st test): the reviewer's §6.5 probe
made permanent — a synthetic installation failure at the SAME window
point (right after the opening-body block) on BOTH seams, through a
one-shot `#[cfg(test)]` thread-local injection seam (test builds only;
production compiles neither the flag nor its checks — the S2R guard is
the ONLY production change in this round). Asserted: `record_count() == 0`
(the named orphan assertion), `in_flight_keys()` empty, exactly one
removal (`removals(&key, incarnation) == 1` — the probe mints its own
lease so the incarnation is known exactly), and (the DX leg) the
flow-window entry settled. Fails without the guard — §5.2's R-S2R-2 reds
with `record_count` left 1 (the reviewer's outcome verbatim) — and passes
with it on both seams; the F-S2.2-5 witness is green throughout.

**Row 3 — F-S2R-3 (record quality: the proof-method wording).** Closure
property (packet §7, verbatim): "state the normalisation proof
(whitespace-stripped body digests) instead of the `-w` emptiness claim,
or restore the original wrapping. No code impact." MET in §4's F-S2.1-4
entry (the parenthetical replaced with the reviewer's executed
normalisation digests) — documentation only; NO runtime inverse (§5.2).

**Record-quality note closed with Row 1.** §4's F-S2.4-2 overclaim is
scoped to what its pair actually binds (packet §7: the "binds the full
§2.6 table" claim overstates).

**Estate at the S2R tree (executed, `--retries 0 --no-tests=fail`, warm
aliases, from `net/crates/net/`):** `org_rpc_streaming` **42/42** (was
41); preserved + public regression controls **134/134** (10 binaries);
cross-lang + gate **32/32** (4 binaries); the in-source three-module
filter **221/221** (was 220 — + the probe); Stage 0 models **76/76**
(untouched); `cargo fmt -p net-mesh -- --check` exit 0. Roster from
source: the 42 `#[test]`/`#[tokio::test]` fns of
`tests/org_rpc_streaming.rs` == 42 executed; the five helper modules
carry 0 test attributes. The 41 preserved names are UNCHANGED (the
file's edits are append-only past the pristine 5256 lines, plus the new
witness's own in-place restructure); the ONE added name is the Row-1
witness. **For Main's same-commit CI floor re-pin — `run_binary
org_rpc_streaming 41 → 42` — the full name list (source order):**

```
 1. stream_opening_admits_same_org
 2. stream_opening_admits_cross_org
 3. unary_context_proof_is_binding_invalid_on_stream_registration
 4. stream_proof_on_unary_registration_is_not_supported
 5. frozen_old_provider_refuses_stream_proof_with_not_supported
 6. replayed_opening_on_new_session_is_session_binding_mismatch
 7. late_chunk_from_replaced_session_is_dropped
 8. omitted_deadline_gets_default_and_expires_idle
 9. requested_deadline_over_cap_is_refused_with_zero_effects
10. requested_deadline_within_cap_is_honoured
11. pump_parked_on_zero_credit_is_retired_at_deadline_with_one_terminal
12. serve_handle_drop_retires_live_stream_and_sibling_survives
13. credential_clamp_expiry_is_admission_denied_not_timeout
14. public_ss_nonzero_deadline_expires_with_typed_timeout
15. public_client_stream_deadline_expiry_is_typed_timeout
16. floor_raise_retires_blocked_stream_before_publish_returns
17. sibling_stream_of_other_org_sends_next_item_after_publication
18. poisoned_store_retires_all_protected_streams
19. store_replacement_retires_all_and_resubscribes
20. session_replacement_retires_old_call
21. active_call_id_reuse_after_replay_window_is_refused
22. raise_between_reserve_and_install_denies_with_zero_effects
23. queued_bytes_over_call_budget_park_send_wait_and_wake_on_retire
24. node_shutdown_retires_live_protected_streams
25. forbidden_stream_opening_causes_zero_handler_effects
26. streaming_denial_is_not_fanned_out_to_the_reply_roster
27. provider_policy_veto_denies_before_effects
28. completed_stream_drains_queued_items_in_order_with_content_and_end_terminal
29. cross_org_completed_stream_drains_correlated_items_with_end_terminal
30. response_after_session_replacement_reaches_only_the_live_session
31. client_stream_opening_binds_first_chunk
32. client_stream_aggregate_with_valid_proof
33. duplex_exchange_with_valid_proof
34. pre_admission_chunks_are_never_delivered
35. end_cannot_cancel_another_stream_or_reopen_terminal_half
36. wrong_session_grant_does_not_release_credit
37. opening_body_budget_refusal_completes_the_record
38. duplex_response_window_blocks_until_grant
39. cross_direction_grant_is_ignored
40. upload_end_then_remaining_output_completes
41. retire_unblocks_both_directions
42. early_handler_return_refuses_late_input_without_resource_exhausted
```

**What never ran at 5 (complete):** the clippy battery, rustdoc runs and
`cargo check --workspace --all-targets` (Main's stage-end list);
`cargo fmt --all -- --check` (the per-crate gate IS executed, exit 0);
`cargo tl` / `cargo t` full suites (the named filters above are the
executed scope); the wire suite; the benches; the `webrtc` feature graph;
Linux/macOS and `#[cfg(unix)]` legs (Windows host only); CI itself
(branch unpushed — the floor re-pin is Main's); the browser/SDK/facade
and bindings surfaces (Stage 3+ by contract); anything in the F-S1R-2
rider territory (owner-declined, out of scope — never demanded here).
Owner-pending: NONE.

### 5.2 Inverse receipts (executed, raw)

Restore-equality baselines at `c17572f03` (the S2R code commit):

```
b6b161cd355fe9c350cd41e65577831103a81381272af57cf90d0d2cb1a525f3  src/adapter/net/cortex/rpc.rs
54b533d48d151d8f663d97aa51d569e89939465460fe06b5dfedede69af5bb3c  tests/org_rpc_streaming.rs
```

Every mutation ran in the session tree at the PRODUCTION site; every
cycle ended sha-proven byte-identical to these and with a green leg. All
runs `--retries 0 --no-tests=fail` on the warm aliases (`cargo tf` /
`tfl`, `net/crates/net/.cargo/config.toml`), from `net/crates/net/`.
Thread ids are run-specific; panic numbering is pristine-at-`c17572f03`
with each mutation's line delta stated (a compile error is never a red).

**R-S2R-1a — Row 1's PAIR (a): the `handler_returned` input-`Closed`-on-return
rule removed (`cortex/rpc.rs`, delta −2 to that file, 0 to the test file):**

```diff
         self.output = StreamCallOutput::Draining(result);
-        if self.input == StreamCallInput::Open {
-            self.input = StreamCallInput::Closed;
-        }
+        // R-S2R-1a MUTATION: the input-Closed-on-return rule is removed
         true
```

Run: `cargo tf --retries 0 --test org_rpc_streaming -E
'test(=early_handler_return_refuses_late_input_without_resource_exhausted)'`
— **exit 100**. Verbatim:

```
thread 'early_handler_return_refuses_late_input_without_resource_exhausted' (153720) panicked at tests\org_rpc_streaming.rs:5379:5:
assertion `left == right` failed: no `ResourceExhausted` is latched — a late request chunk after an early handler return is refused/discarded without replacing the handler's result
  left: None
 right: Some((true, None))
```

The named no-latch assertion is the red and the forbidden outcome is
exactly the brief's: without the rule the late chunk reaches delivery,
its failure at the dropped receiver latches `ResourceExhausted`, and the
retirement replaces the handler's result (the forced path's single
removal erases the record — hence `left: None`). Restore:
`git checkout --`; sha256 `b6b161cd…` == baseline. Restored green: exit
0, 1/1.

**R-S2R-1b — Row 1's PAIR (b): the chunk-path record gate neutralized
(`apply_request_chunk_to_senders`; delta −5 to `cortex/rpc.rs`):**

```diff
-    if let Some(record) = sender.record.as_ref() {
-        let rec = record.lock();
-        if rec.terminal.is_some() || rec.input != StreamCallInput::Open {
-            return;
-        }
-    }
+    // R-S2R-1b MUTATION: the chunk-path record gate is neutralized
```

Same command — **exit 100**. Verbatim (the same named assertion, the
same forbidden outcome):

```
thread 'early_handler_return_refuses_late_input_without_resource_exhausted' (194296) panicked at tests\org_rpc_streaming.rs:5379:5:
assertion `left == right` failed: no `ResourceExhausted` is latched — a late request chunk after an early handler return is refused/discarded without replacing the handler's result
  left: None
 right: Some((true, None))
```

Restore + green 1/1 as above.

**R-S2R-2 — Row 2's probe receipt: the guard removed (its settlement
neutralized at `ConfirmedStreamOpening`'s Drop; `cortex/rpc.rs`, line-neutral
— the probe's own numbering unaffected):**

```diff
     fn drop(&mut self) {
         if self.armed {
-            self.registration.complete();
+            // R-S2R-2 MUTATION: the guard's settlement is removed
         }
     }
```

Run: `cargo tfl --retries 0 -E
'test(=adapter::net::cortex::rpc::tests::post_transfer_scope_guard_never_orphans_a_running_record)'`
— **exit 100**. Verbatim:

```
thread 'adapter::net::cortex::rpc::tests::post_transfer_scope_guard_never_orphans_a_running_record' (161100) panicked at src\adapter\net\cortex\rpc.rs:13400:9:
assertion `left == right` failed: a post-transfer installation failure must not orphan the Running record (the ConfirmedOpening scope-guard precedent)
  left: 1
 right: 0
```

The orphan: `record_count` left 1 — the reviewer's §6.5 probe outcome,
verbatim. Under the same mutation the F-S2.2-5 witness also fails
(belt-first, `tests/org_rpc_streaming.rs:4443:9` "the refused opening
leaves no in-flight state" — exit 100), the whole settlement being the
guard now. Restore + green BOTH (the probe 1/1 AND
`opening_body_budget_refusal_completes_the_record` 1/1 — "the F-S2.2-5
witness stays green throughout", packet §7).

**R-S2.2c-v2 — R-S2.2c's fail-pre-fix property, PRESERVED (the registry's
release-once `complete` removed from `StreamCallRegistration::complete` —
its fix line's post-fold home; `cortex/rpc.rs`, delta −3):**

```diff
         self.protected.remove(&self.key);
         if let Some(senders) = self.senders.as_ref() {
             senders.lock().remove(&self.key);
         }
-        if let Some(call_ref) = self.registry.as_ref() {
-            call_ref
-                .registry
-                .complete(&call_ref.key, call_ref.incarnation);
-        }
+        // R-S2.2c-v2 MUTATION: the registry's release-once `complete` is removed
+        let _ = &self.registry;
```

Run: `cargo tf --retries 0 --test org_rpc_streaming -E
'test(=opening_body_budget_refusal_completes_the_record)'` — **exit 100**.
Verbatim:

```
thread 'opening_body_budget_refusal_completes_the_record' (145816) panicked at tests\org_rpc_streaming.rs:4455:5:
assertion `left == right` failed: the pre-supervisor refusal completes the record's single removal
  left: 1
 right: 0
```

BYTE-IDENTICAL to R-S2.2c's receipt quote (§4.2) — with the F-S2.2-5
settlement folded into the guard the witness remains a genuine
fail-pre-fix regression witness (its named assertion, its exact
observation). Restore + green 1/1.

**Row 3 (F-S2R-3): NO runtime inverse — documentation only.** The §4
wording fix and this record carry no executable behavior to invert;
stated as such per the brief.

**Disclosure — one superseded cycle and one pre-push amend.** The FIRST
R-S2R-1a attempt red at the witness's `.expect("the record is alive …")`
belt (the latch's forced path erases the record map entry before a
recorded terminal can be read) — a real red but NOT at the named
assertion. The named observation was restructured to map-form
(`owners.get(&key).map(|call| (call.is_live(), call.terminal())) ==
Some((true, None))` — no `expect`, no green observation changed), the
code commit was AMENDED pre-push to carry it (`d1f072bda` →
`c17572f03`), and EVERY cycle recorded above re-ran after the
restructure. The first attempt's output is superseded, not cited
anywhere. Four weakenings on every recorded cycle: **NONE** — no
precondition fixed, no window widened, no assertion relaxed, no witness
deleted (both witnesses are additions; the restructure strengthened red
placement only).

## 6. Stage 3

**Lane:** S3Facade (one lane). **Pinned brief:** `spikes/org-streaming/S3_BRIEF.md`
@`0071fd1fc`. **Base:** `0071fd1fc` (Stage 2 accepted at `017e7148a`; S2R
closure verified at `1d26bc4ba`/`636d80d69`). Rows 3.1 → 3.3 in order, each
landed green before the next. Owner-pending: none.

### 6.1 Row 3.1 — caller verbs (S3.1)

**Landed (executed):** `d63e9c615` — `S3.1: add the org streaming caller verbs
(spec 4.3) with live facade witnesses` (4 files, +1789/−3) on
`LZL0/org-streaming`; this record rides in the following `S3.1:` commit.

**What landed (source-established, line numbers pristine-at-`d63e9c615`):**

- The §4.3 caller rows verbatim: `call_streaming<Req,Resp>(service, &Req) ->
  Result<OrgStream<Resp>, OrgSdkError>` (`OrgStream<Resp>: Stream<Item =
  Result<Resp, OrgSdkError>>`, wrapping `RpcStreamTyped`);
  `call_streaming_bytes -> OrgStreamRaw`; `call_client_stream<Req,Resp>(service)
  -> OrgClientStreamCall<Req,Resp>` (`send(&Req)`, `finish(self) ->
  Result<Resp,_>`); `call_duplex<Req,Resp>(service) -> OrgDuplexCall<Req,Resp>`
  (`send`, `finish_sending`, `into_split`, `Stream` — the split halves
  `OrgDuplexSink<Req>`/`OrgDuplexStream<Resp>` wrap the typed halves the public
  `DuplexCallTyped::into_split` returns). All four wrapping types map every
  error surface through the unary verb's own `map_rpc_error`.
- The three `*_bytes_deadline` binding seams (`#[doc(hidden)]`), the same seam
  contract as `call_bytes_deadline` (neither argument is an authorization
  input): `deadline_ms == 0` ⇒ `DEFAULT_LIFETIME_MS = 300_000` (Owner Q1),
  **never "no deadline"** — D3's finite-lifetime rule; `cancel_token == 0` ⇒
  uncancellable. **Interpretation, stated:** §4.3 pins the seam NAMES without
  return types; they return the EXISTING raw handles (`RpcStream`,
  `ClientStreamCallRaw`, `DuplexCallRaw`) per §4.4's binding rule ("over the
  `*_bytes_deadline` seams and the existing public stream/sink handle types —
  no new stream wrapper per binding"), and the public `call_streaming_bytes ->
  OrgStreamRaw` wraps the seam's stream in the facade error vocabulary.
- The pin, structural: `streaming_opening` runs ONE `plan()` per call (the
  unary instrumentation block records `last_selected_provider`), and the call
  rides `intent.provider.node_id()` only. CS's core handle opens at the first
  `send`/`finish` — core's own lazy-initial-REQUEST contract — against the
  verb's PINNED opening (`PinnedOpening`); DX opens at the verb because
  `into_split`/`Stream` are synchronous.
- `OrgClient` gains ONE crate-internal field (`typed: Arc<Mesh>`, built at bind
  over the SAME `Arc<MeshNode>` via the public `Mesh::from_node_arc`):
  `Mesh::call_*_typed` is the only constructor of the typed streaming handles
  before Row 3.2's `from_raw` seam lands (F-S3.2-1's ruling); it is removed the
  moment that seam takes over the construction. The frozen type list
  (`OrgCaller`/`OrgSdkError`/`OrgHandlerError`/`OrgAccess`/
  `CoarseAdmissionReason`/`OrgProofIntent`/`CallOptions`) gains NOTHING; the
  unary/public surfaces are unchanged (verified by the existing probe build +
  the regression legs below).

**Witnesses and counts (executed).** NEW suite `sdk/tests/org_streaming.rs`
(the fixture shape of `src/org/tests_live.rs:176-273`): **8/8**, exit 0, at the
plan's named command —

```sh
cargo nextest run --no-fail-fast --no-tests=fail --retries 0 -p net-mesh-sdk \
  --features "net cortex dataforts testing compute nat-traversal port-mapping \
aggregator tool macros fixtures" --test org_streaming
```

Roster FROM SOURCE (8 `#[tokio::test]` fns):
`live_same_org_streaming_through_the_facade`,
`live_same_org_client_stream_through_the_facade` (**the pin witness**),
`live_same_org_duplex_through_the_facade`,
`live_cross_org_streaming_through_the_facade`,
`live_cross_org_client_stream_through_the_facade`,
`live_cross_org_duplex_through_the_facade`,
`facade_stream_against_unary_only_provider_is_not_supported`,
`dropping_org_stream_emits_one_cancel`.

CI floor note (Main pins, never a lane): `--suite org_streaming` floor = **8**,
the eight names above in the same commit.

**Inverse receipt (executed, raw) — R-S3.1-pin, at the production site.**
Baseline at `d63e9c615`'s tree: `sha256 bf302b5e0a23c29b2645c4e5d94d993939ed20c4948bea33450d4d7b53c9587a
sdk/src/org/call.rs`. The applied inverse is the brief's verbatim — **resolve a
second provider mid-call** — a bounded diff at `OrgClientStreamCall::send`
(delta +11/−1 to `call.rs`, 0 to the test file):

```diff
     pub async fn send(&mut self, value: &Req) -> Result<(), OrgSdkError> {
+        // S3.1 PIN MUTATION: resolve a SECOND provider mid-call and re-open
+        // against it instead of reusing the pinned plan.
+        let (fresh_provider, fresh_opening) =
+            self.pinned
+                .client
+                .streaming_opening(&self.pinned.service, 0, 0)?;
+        self.pinned.provider = fresh_provider;
+        self.pinned.opening = fresh_opening;
+        self.inner = None;
         self.ensure_opened().await?;
```

(The pin witness's scenario resolves its second provider mid-call by design:
only the pinned provider is resolvable at the verb; after chunk one, the
lower-entity-id provider — which wins EVERY fresh deterministic selection —
joins and converges.)

Run: `cargo nextest run --no-fail-fast --no-tests=fail --retries 0 -p
net-mesh-sdk --features "net cortex dataforts testing compute nat-traversal
port-mapping aggregator tool macros fixtures" --test org_streaming -E
'test(=live_same_org_client_stream_through_the_facade)'` — **exit 100** (two
runs, identical red). Verbatim:

```
thread 'live_same_org_client_stream_through_the_facade' (192516) panicked at sdk\tests\org_streaming.rs:619:5:
assertion `left == right` failed: the provider is pinned per call: chunk two and the terminal land on the planned provider even though a second provider resolved mid-call
  left: UploadSummary { chunks: 1, seen: [20], served_by: 13883170235434775928 }
 right: UploadSummary { chunks: 2, seen: [10, 20], served_by: 5999853971703539794 }
```

The NAMED pin assertion is the red and the forbidden outcome is exactly the
brief's: under the mutation the send re-resolved mid-call, the second provider
won the fresh selection, and chunk two + the terminal landed there
(`chunks: 1, seen: [20]`, the second provider's node id) instead of the
planned provider. Restore: reverse edit; sha256 `bf302b5e…` == baseline
(byte-identical). Restored green: exit 0, 1/1. Four weakenings: **NONE** — no
precondition fixed, no window widened, no assertion relaxed, no witness
deleted.

**`dropping_org_stream_emits_one_cancel` — disclosure and the observable
contract (executed + source-established).** First draft asserted the HANDLER's
`ctx.cancellation` observation count and red (`left: 0, right: 1`). The reason
is source-established at `cortex/rpc.rs` `run_stream_call_supervisor`: on
the protected path the handler future is polled INSIDE the retire supervisor
and is dropped at scope end on a `forced` retirement ("The owned handler
future drops at scope end"), so a handler-side `cancelled()` observation is
RACY there — the public-path house witness (`rpc_streaming_drop_cancels_handler`)
spawns the handler as its own task, which is why it can observe. Cooperation is
best-effort by contract (§2.2 `forced`). The handler-counter assertion was
therefore REMOVED as an unsound instrument and replaced with STRICTER direct
observables (this is a pre-landing development restructure, not a re-pin of a
landed witness): the drop's ONE cancel retires EXACTLY the one dropped call
(the fold's `in_flight_keys()` goes 2 → 1 and stays 1), that call's emission
freezes (no zombie producer), and the sibling keeps producing and delivering
(a stray cancel reaching it would freeze it and drain its record). Known limit,
stated: a DUPLICATE wire CANCEL frame is indistinguishable at these seams (the
protected record's retire is exactly-once and the token latches), so "one" is
defended at the live level as "exactly one call retired, once, and only that
call"; the double-emit failure mode is not discriminated here. The second
draft's first run red was the draft's own mistimed production sample (a queued
chunk satisfies `next()` instantly); the sample window was widened to span
several tick periods BEFORE landing — the only window change in this row's
history, and it strengthened the discriminator.

**Regressions (executed at `d63e9c615`'s tree):**

- The existing 42-roster: `cargo tf --retries 0 --test org_rpc_streaming` —
  **42/42**, exit 0.
- The existing SDK org estate: `cargo nextest run --no-fail-fast
  --no-tests=fail --retries 0 -p net-mesh-sdk --features "…" --lib --test
  org_exact_sensing` — **338/338**, exit 0.
- `cargo fmt -p net-mesh-sdk -- --check` — exit 0.

**Findings (state, not decide):**

- **F-S3.2-1 — the §4.3 typed serve rows need crate-internal construction of
  `RequestStreamTyped`/`ResponseSinkTyped` (source-established; RESOLVED BY
  MAIN RULING).** Those types' fields are private to `sdk/src/mesh_rpc.rs` and
  the only construction sites are the private `Typed*RpcHandler` adapters
  behind `Mesh::serve_rpc_*_typed`, which drop `RpcContext`/
  `RpcStreamingContext` — so they cannot project `OrgCaller`, and
  `sdk/src/org/**` alone cannot produce the verbatim handler signatures. Main
  ruled (option A): five `pub(crate) fn from_raw(inner, codec) -> Self`
  constructors (`RpcStreamTyped`, `ClientStreamCallTyped`, `DuplexCallTyped`,
  `RequestStreamTyped`, `ResponseSinkTyped`) land in S3.2's commit with their
  consumer (same-commit doctrine), `pub(crate)` as the ceiling, constructors
  only, zero public-API change, the probe's frozen-surface pin to still pass
  unchanged.
- **F-S3.1-2 — the protected-path handler cannot be a cancel-observer
  (source-established, executed above).** Stated with the drop witness: the
  retire supervisor may drop the handler future without a final poll. No core
  change proposed or needed — the live contract is the retirement observables.

**Never executed here (complete):**

- The `org_streaming` suite on any feature set other than the plan's named one;
  any other host/OS (all runs: this workstation, Windows).
- The `webrtc`/wasm graphs, the benches, the bindings, `guards/org_api_probe`'s
  rebuild (Row 3.3's named witness), `ci.yml`/nextest floors (Main's).
- A wire-level CANCEL-frame COUNT (no seam exposes frame counts; see the drop
  witness's stated limit).
- Disclosure (disk): one `write` of `sdk/tests/org_streaming.rs` failed mid-work
  with ENOSPC (detected and broadcast immediately per the hazard rule); free
  space recovered before the retry (66 GiB observed), the file was rewritten in
  full and verified under the F16 rule (size + sha256/line-count after every
  write; the four touched files re-verified before this commit pair).

### 6.2 Row 3.2 — provider verbs (S3.2)

**Landed (executed):** `c69d69761` — `S3.2: add the org streaming provider
verbs (spec 4.3) over serve_org_*_bytes_node` (6 files, +741/−79) on
`LZL0/org-streaming`; this record rides in the following `S3.1:`-prefixed
record commit pair convention (`S3.2:`).

**What landed (source-established):**

- The §4.3 serve rows verbatim: `Mesh::serve_org_streaming(.., Fn(OrgCaller,
  Req, ResponseSinkTyped<Resp>) -> Fut<Result<(),String>>)`,
  `Mesh::serve_org_client_stream(.., Fn(OrgCaller, RequestStreamTyped<Req>) ->
  Fut<Result<Resp,String>>)`, `Mesh::serve_org_duplex(.., Fn(OrgCaller,
  RequestStreamTyped<Req>, ResponseSinkTyped<Resp>) -> Fut<Result<(),String>>)`
  — each over its bytes row (`serve_org_{streaming,client_stream,duplex}_bytes`)
  and its `#[doc(hidden)]` node seam
  (`serve_org_{streaming,client_stream,duplex}_bytes_node`), exactly the unary
  `serve_org` → `serve_org_bytes` → `serve_org_bytes_node` layering. The typed
  row IS the bytes row plus JSON — one dispatch path per shape.
- The `OrgCaller` projection: ONE function (`project_caller`) converts the
  admission-verified `Admitted` into the handler-facing type for all four
  shapes (the unary bridge refactored onto it); `None` admission is the same
  loud invariant refusal as today, never fabricated attribution. One error
  classification (`From<OrgHandlerError> for RpcHandlerError`) across all
  rows.
- The facade policy `|_| true` in every row (the provider veto stays the
  caller's extension point on the low-level API, as today); access implies
  visibility; registration before provisioning.
- The call-side clean cutover enabled by the same seam: `call_streaming` and
  `call_duplex` are now ONE PATH over their `*_bytes_deadline` seams plus
  `from_raw` (the typed verb IS the bytes seam plus JSON, matching the unary
  doctrine), `OrgClientStreamCall`'s deferred open constructs from the PINNED
  opening via `from_raw`, and the temporary bind-time `Arc<Mesh>` shim from
  Row 3.1 is REMOVED (its only consumer migrated).

**F-S3.2-1 — RESOLVED BY MAIN RULING (the §4.3 typed rows need
crate-internal construction).** The private `Typed*RpcHandler` adapters behind
`Mesh::serve_rpc_*_typed` drop `RpcContext`/`RpcStreamingContext`, so they
cannot project `OrgCaller`; `RequestStreamTyped`/`ResponseSinkTyped` (and the
three call-side typed handles) have private fields and no constructors
reachable from `sdk/src/org/**`. Main ruled (option A, same-commit doctrine):
the five constructors below land in THIS commit with their consumers, as
`pub(crate)` ONLY (the ceiling), constructors only — **no field visibility
moved**, no behavior change, no field reshaping, zero public-API change. The
five signatures, verbatim as landed in `sdk/src/mesh_rpc.rs`:

```rust
pub(crate) fn from_raw(inner: RpcStream, codec: Codec) -> Self           // RpcStreamTyped<Resp>
pub(crate) fn from_raw(inner: ClientStreamCallRaw, codec: Codec) -> Self // ClientStreamCallTyped<Req, Resp>
pub(crate) fn from_raw(inner: DuplexCallRaw, codec: Codec) -> Self       // DuplexCallTyped<Req, Resp>
pub(crate) fn from_raw(inner: RequestStream, codec: Codec) -> Self       // RequestStreamTyped<Req>
pub(crate) fn from_raw(inner: RpcResponseSink, codec: Codec) -> Self     // ResponseSinkTyped<Resp>
```

Main's conditions 1–4: (1) `pub(crate)` ceiling respected, no field
visibility moved (stated here); (2) constructors only, exactly the five types;
(3) this record names them and the ruling; (4) the probe's frozen-surface pin
passes UNCHANGED at this head — **executed**: `cargo metadata --locked` exit 0
and `cargo check --locked` exit 0 in `guards/org_api_probe` at `c69d69761`'s
tree (the no-public-change claim's proof). Condition 5 (a sixth type or any
`pub`): not needed — `OrgDuplexCall::into_split` wraps the halves the PUBLIC
`DuplexCallTyped::into_split` already returns.

**Witnesses and counts (executed).** `sdk/tests/org_streaming.rs` is now
**10/10** at the plan's named command (§6.1's), exit 0. The row's two named
witnesses:

- `handler_receives_verified_org_caller_not_origin` — the three facade serve
  rows (`serve_org_streaming`/`_client_stream`/`_duplex`, typed) each assert
  their `OrgCaller`'s FIVE verified fields against the expected attribution
  (the ed25519 entity id — the handler never sees `caller_origin`, so what it
  receives can only be the admission-verified identity); the caller round-trips
  all three shapes.
- `revocation_surfaces_as_final_admission_denied_item` — a live stream is
  revoked MID-STREAM by a real floor raise through the provider's installed
  store (`OrgRevocationBundle::try_issue` + `apply_bundle`, membership
  generation 1 → floor 2); the stream's FINAL item is
  `Err(AdmissionDenied(Denied))` — the frozen `Revoked → Denied` coarse byte —
  and the stream then ends. The raise retires synchronously (the core's §2.3
  boundary), so the observation is deterministic, not polled-into.

CI floor note (Main pins): `--suite org_streaming` floor = **10**, the ten
names in the same commit.

**Regressions (executed at `c69d69761`'s tree):**

- The existing 42-roster: `cargo tf --retries 0 --test org_rpc_streaming` —
  **42/42**, exit 0.
- The existing SDK org estate: `cargo nextest run … --lib --test
  org_exact_sensing` — **338/338**, exit 0 (this run also covers the shim
  removal and the `from_raw` migration: the whole lib recompiled and every
  org unit witness re-passed).
- `cargo fmt -p net-mesh-sdk -- --check` — exit 0.

**Findings (state, not decide):** F-S3.2-1 resolved by Main's ruling (above);
no new findings at this row.

**Never executed here (complete):**

- The probe's build on any feature set other than its own (`cargo metadata
  --locked` + `cargo check --locked` are its exact commands); `ci.yml`'s floor
  edits (Main's).
- A wire-level duplicate-CANCEL discrimination (§6.1's stated limit, unchanged).
- The `serve_org_*` rows against a non-`fixtures` build — the suite's runs use
  the plan's named feature set.

### 6.3 Row 3.3 — docs + probe (S3.3), and the stage exit

**Landed (executed):** `020afe614` — `S3.3: the full org verb set in
ORGANIZATIONS.md and the probe's Stage 3 pins` (5 files, +261/−19) on
`LZL0/org-streaming`; this record rides in the following `S3.3:` commit.

**What landed (source-established):**

- `docs/ORGANIZATIONS.md`'s "two verbs" text (the `:124-135` band) becomes
  **the full verb set** — bind + the four call shapes + the four provider
  verbs — with the per-handle error vocabulary (opening refusal
  `AdmissionDenied(coarse)`; midstream revocation the stream's final
  `AdmissionDenied(Denied)`; deadline/cancel retirement `Rpc(Timeout)`/
  `Rpc(Cancelled)`; drop = one CANCEL) and **the deadline rule** verbatim from
  Owner Q1: the `*_bytes_deadline` seams' `deadline_ms == 0` is the facade
  default **300 s**, never "no deadline"; `cancel_token == 0` is
  uncancellable; neither is an authorization input. The stale "ride the SDK
  release train" sentence is gone — the verbs are the shipped surface now.
- `guards/org_api_probe` compiles the new verbs AND still the unary ones:
  MANIFEST grows by **exactly** the 22 new pins (7 `OrgClient::call_*` rows +
  seams, 6 `Mesh::serve_org_*` verbs, 3 `serve_org_*_bytes_node` seams, 6
  wrapping types) grouped beside their families; `main.rs` gains four pin
  functions (`pin_org_streaming_calls`, `pin_org_streaming_serves`,
  `pin_org_streaming_node_seams`, `pin_org_stream_wrappers`) that reference
  each verb as a value AND apply it with fully annotated handler closures —
  `OrgCaller` FIRST in every one — and pin the stream handles' ITEM vocabulary
  (`Result<_, OrgSdkError>`) by annotated `next()` awaits. **No exhaustive
  match arm deleted** (`pin_org_sdk_error`, `pin_org_access`,
  `pin_org_handler_error`, `pin_coarse_admission_reason`,
  `pin_admission_denied`, `pin_org_admission` untouched) and the
  `#[non_exhaustive]` fallback arm (`_ => "future_variant"`) stays. One dep
  edge (`futures = "0.3"` with its own `[dependencies]` section) joins the
  probe's graph so an external consumer's stream-drain is expressible; the
  crate is already in the lock through `net_sdk`, so `Cargo.lock`'s growth is
  the probe's own package edge only (regenerated, then pinned by `--locked`).

**The row's witness (executed) — the probe's own green build at its exact
commands, at `020afe614`'s tree:**

```sh
cd net/crates/net/guards/org_api_probe
cargo metadata --locked --format-version 1    # exit 0
cargo check --locked                          # exit 0
```

The probe is the witness: it compiles the seven caller verbs, the six serve
verbs, the three node seams and the six wrapping types from OUTSIDE the
workspace, so any signature/context/enum break in the frozen surface (the
probe's unchanged pins included) fails this exact run.

**Stage 3 exit (the plan's wording, mapped to executed evidence).** Real Rust
caller AND provider through the public facade for all four shapes (unary +
the three streaming), same-org and granted, with revocation/cancellation/
ownership witnesses — `org_streaming` **10/10**: the six `live_*` witnesses
(same-org and granted × streaming/client-stream/duplex), the pin witness
(mid-call second-provider resolution), `facade_stream_against_unary_only_provider_is_not_supported`,
`dropping_org_stream_emits_one_cancel` (ownership: one record retired, once),
`handler_receives_verified_org_caller_not_origin` (the verified projection,
all three serve rows), and
`revocation_surfaces_as_final_admission_denied_item` (the frozen `Revoked →
Denied`). The probe catches unary/public API breakage (this row). Regressions
at the stage's final head: the 42-roster **42/42** and the SDK org estate
**338/338** (both at `c69d69761`'s tree; rows 3.3's five files are one
non-compiled doc + the probe's own workspace — the sdk/net trees are
byte-identical since, verified by `git diff --stat` scope), `org_streaming`
re-run **10/10** at the final head, `cargo fmt -p net-mesh-sdk -- --check`
exit 0.

**Correction (S3_R; the packet §5 adjudication, applied here).** The exit
sentence above claims "Real Rust caller AND provider through the public facade
for all four shapes …, same-org and granted" over the landed 10/10 — but at
Stage 3's close the granted PROVIDER cells × the three streaming shapes rode
the CORE seams (`serve_rpc_granted_*`, witnesses 4–6), not the facade serve
verbs: the matrix held on landed evidence in **13 of 16 cells**, the other
three capability-proven by the review's probe only (F-S3R-1). §7.2's
`granted_facade_streaming_serve_rows_complete_cross_org` closes exactly those
three cells on landed evidence (16/16). Everything else in the paragraph
stands as written.

**CI floor note (Main pins, never a lane):** `--suite org_streaming` floor =
**10** with the ten names, and the `org_rpc_streaming` floor stays **42**.

**Findings (state, not decide):** none new at Row 3.3. Stage-wide: F-S3.2-1
(raised and resolved by Main's ruling, §6.2) and F-S3.1-2 (the protected
handler is not a sound cancel-observer; the retirement observables are the
contract, §6.1).

**Never executed here (complete):**

- `ci.yml`/`.config/nextest.toml` floor re-pins (Main's by contract).
- The probe on any toolchain/OS but this workstation's; the `webrtc`/wasm
  graphs, the benches, and every binding (Stage 4's rows by contract).
- A wire-level duplicate-CANCEL-frame count (§6.1's stated limit).

## 7. Repair round (S3_R)

**Lane:** S3Repair. **Pinned brief:** `spikes/org-streaming/S3_R_BRIEF.md`
@`635d31cc1`. The S3Review ACCEPT (`S3_REVIEW_PACKET.md` @`2225de011`)
**stands**; this round closes its three non-blocking findings (packet §8)
against their closure properties VERBATIM. Date: 2026-09-23, Windows host
only. Owner-pending: none. NO stage n+1.

### 7.1 The corrected Row-3 premise (Main's S3R ruling): a PURE WITNESS ROW

The brief's Row 3 authorized "exactly ONE production change —
`OrgStreamRaw::poll_next`'s `Err` arm surfacing `Ready(Some(Err(_)))`" and
described "the current `Ready(None)` swallow". **The landed code already
conforms** (sha-verified): at the pinned head `call.rs`'s sha is
`04b756081d6997fd5fa6dc4788061752ee8cb9c14fb6f38da800e2fb21933c29` —
byte-identical to the review's own pristine baseline — and the `Err` arm at
`call.rs:245-247` reads `Poll::Ready(Some(Err(map_rpc_error(e))))`, the §4.3
item shape. The packet agrees with the bytes: its §4 audit records "wrappers
map every surface through the unary `map_rpc_error`", and its §8.3 inverse
arrow is `Ready(Some(Err(_)))` → `Ready(None)` — **FROM = the landed form**.
The brief's "swallow" phrase describes the reviewer's **M8 mutation state**,
not the landed code. Main ruled (S3R, in-round): Row 3 is a pure witness row —
**no production change**; the named inverse is the M8 swallow itself. This
round is therefore test-only + record: `call.rs`, `serve.rs`, `error.rs`,
`mesh_rpc.rs`, and every core file are byte-unchanged from `2225de011`
(restore-equality verified throughout §7.4).

### 7.2 What landed (executed)

Commit pair + this record:

| commit | content |
|---|---|
| `0c580ca46` | the four witnesses in `sdk/tests/org_streaming.rs` (+765/−0) |
| `b5143865c` | the per-row inverse receipts — an empty tree-change BY CONSTRUCTION (every mutation restored sha-proven); receipts raw in the commit message |
| (this commit) | the §6 correction + this `## 7` |

**Row 1 — F-S3R-1.** Closure property (packet §8.1, verbatim): "a named
witness in which a caller holding a cross-org capability grant completes a
server-streaming call, a client-streaming upload, and a duplex exchange
through handlers registered via `Mesh::serve_org_streaming` /
`serve_org_client_stream` / `serve_org_duplex` with `OrgAccess::Granted`,
asserting exact payloads and the four-party attribution; the inverse — each
row's `OrgAccess::Granted` arm resolving to `serve_rpc_owner_scoped_*` —
reddens that witness at its named assertion." Witness
`granted_facade_streaming_serve_rows_complete_cross_org`: org B's provider,
org A's caller, three services `customer.read.{stream,upload,duplex}` (a
service name holds ONE registration — the first attempt at a single service
red `AlreadyServing("customer.read")`, development-disclosed), one
DISCOVER|INVOKE grant per capability, provisioned exactly like
`granted_fixture`; the three facade verbs registered with `OrgAccess::Granted`
and driven in three legs (streaming → client-streaming → duplex). Each leg
carries **its own named assertion** over the exact five-tuple
`(resolved, completed, exact payloads, four-party attribution (S acted for A
under B's grant on exact P), ran == 1)` — the leg's own grant-plane resolution
folded in. The packet §5 Exit matrix's three ⚠️ cells (granted-provider × the
three streaming shapes) now hold on landed evidence: **16/16**.

**Row 2 — F-S3R-2.** Closure property (packet §8.2, verbatim): "a named
witness in which a streaming call issued through a facade verb (which passes
`deadline_ms == 0`) against a provider whose `default_live` is materially
shorter than 300 s keeps delivering past that shorter bound — the facade's
300 s lifetime in force — and the inverse (the `deadline_ms == 0` arm
producing no deadline) reddens that witness at its named assertion." Witness
`facade_default_deadline_at_zero_outlives_a_shorter_provider_default`.
**Instrument disclosure (source-established + executed).** A provider's
lifetime policy is NOT configurable on any live wire path: the production
bridges construct `StreamCallLifetime` with
`StreamLifetimePolicy::q1_defaults()` at the only two production sites
(`mesh_rpc.rs:1846`, `:1876` — its own comment: "The provider-configurable
knob Q1 names is startup configuration; a per-registration lifetime policy
would be API-addition territory (stated in the report, not added)"), and
`MeshNodeConfig` carries no lifetime field. The witness fuses the packet
§8.2's own alternative instrument ("or read the effective deadline directly")
with the closure's short-bound clause: **(a) live leg** — a real `call_streaming`
(`deadline_ms == 0`) against a live provider whose handler captures the
call's REAL `RpcRequestPayload` verbatim (the effective deadline read
directly at `ctx.payload.deadline_ns`); **(b) short-default leg** — that exact
captured payload admitted at a provider whose `default_live_ns = 400 ms`
(`max_live_ns = 3600 s`) via the preserved `org_rpc_streaming` s13 fold idiom
(`RpcServerStreamingFold::apply_inbound_admitted` takes the §2.1 lifetime
inputs directly), whose handler emits item 10 at +0 ms and item 11 at +800 ms.
The named assertion is the closure's clause: item 11 — past the 400 ms bound —
IS delivered, with `deadline_end_ns() ==` the facade's explicit `deadline_ns`
verbatim and bound `Deadline` (never the provider default). The named inverse
reddens exactly there. Executed vs source-established: the `deadline_ms == 0`
⇒ 300 s mapping and its wire effect are EXECUTED (captured + resolved
verbatim); "the provider knob was not added" is SOURCE-ESTABLISHED (the
bridge comment, the construction sites, `MeshNodeConfig`'s field set).
Limit, stated: clause (b) runs at the fold seam over the facade call's real
request payload, not over a second live wire call — impossible by
construction while the bridges pin `q1_defaults()`.

**Row 3 — F-S3R-3 (pure witness row, §7.1).** Closure property (packet §8.3,
verbatim): "a named witness that drains `OrgStreamRaw` through a midstream
retirement and observes the final `Err(AdmissionDenied(Denied))` item (never a
swallowed clean end), plus one drive of `call_client_stream_bytes_deadline` to
a typed terminal; the inverse (`Ready(Some(Err(_)))` → `Ready(None)` in
`OrgStreamRaw::poll_next`) reddens the first at its named assertion."
`org_stream_raw_surfaces_midstream_errors_as_items`: `call_streaming_bytes` →
`OrgStreamRaw` drained through a real mid-stream floor raise (the frozen
`Revoked → Denied` byte) — pre-retirement chunks all `Ok` exact bytes, the
FINAL ITEM `Err(AdmissionDenied(Denied))` at the named match, then end.
`call_client_stream_bytes_deadline_reaches_a_typed_terminal`: one drive of the
CS bytes seam (two chunks, `finish()`) to the typed terminal — the exact
`UploadSummary` decoded from the seam's terminal reply (`finish` maps a non-Ok
server status to `Err(RpcError::ServerError)` before returning, so `Ok(reply)`
is the Ok typed terminal).

### 7.3 Counts and rosters (executed)

`org_streaming` **10 → 14** at the plan's named command, exit 0. Roster FROM
SOURCE (14 `#[tokio::test]` fns) == executed == the ten landed names preserved
verbatim + the four new: `granted_facade_streaming_serve_rows_complete_cross_org`,
`facade_default_deadline_at_zero_outlives_a_shorter_provider_default`,
`org_stream_raw_surfaces_midstream_errors_as_items`,
`call_client_stream_bytes_deadline_reaches_a_typed_terminal`.

CI floor note (Main pins, never a lane): `--suite org_streaming` floor = **14**.
Arithmetic note for the re-pin: the brief's "(floor 10 → expected 13)" counts
three rows; the closure names FOUR witnesses and the goal's acceptance says
"all four witnesses" — 10 + 4 = **14** in the one binary.

### 7.4 Inverse receipts (executed, raw)

Raw, verbatim, in `b5143865c`'s commit message (the S2R empty-tree-change
convention). Cycle per receipt: bounded diff at the PRODUCTION site →
narrowed run (target + control) → named red (an assertion, never a compile
error) → `git checkout --` restore + sha256 == pristine baseline → restored
green. Baselines: `call.rs 04b75608…`, `serve.rs e1f7083c…` (identical to the
packet §7's; never changed by this round).

| receipt | site (+/−) | named red | control (PASS) |
|---|---|---|---|
| R1a | `serve.rs:650` +1/−1 | leg 1's own named assert, `org_streaming.rs:1717:5` | `handler_receives_verified_org_caller_not_origin` |
| R1b | `serve.rs:675` +1/−1 | leg 2's own named assert, `:1768:5` | same |
| R1c | `serve.rs:700` +1/−1 | leg 3's own named assert, `:1825:5` | same |
| R2 | `call.rs:185-187` +3/−1 | the Row-2 named assert, `:2076:5` | `live_same_org_streaming_through_the_facade` |
| R3 | `call.rs:245` +1/−3 | the Row-3a named panic, `:2200:18` | `revocation_surfaces_as_final_admission_denied_item` |

Four weakenings (precondition fixed / window widened / assertion relaxed /
witness deleted): **NONE** applies to any cycle. Pre-receipt development
disclosure (strengthening): the Row-1 witness's first instrument kept one
shared `converge` precondition and R1a's flip reddened that
(`:1691:9`, "precondition: the grantee privately resolved the B-owned provider
for customer.read.stream"); before any receipt was taken the instrument was
tightened — each leg's resolution folded into its own named assertion (scope
grew; nothing fixed, widened, relaxed, or deleted) — and the suite re-ran
14/14 green at `0c580ca46`.

### 7.5 §6 corrections required by the reviewer's adjudications

1. **§6.3's stage-exit overstatement — corrected inline above** (the packet
   §5 matrix: 13/16 landed at Stage 3 close, the three granted-provider ×
   streaming-shape cells probe-only / F-S3R-1; now 16/16 via §7.2's Row-1
   witness).
2. **The `619:5`/`632:5` panic-quote question — no correction required**
   (packet §7.1: both correct at their stated bases; the reviewer retired the
   suspicion).
3. **F-S3.1-2's wording — untouched** (packet §6.1 adjudication: accurate and
   already executed; the Stage-4 rider stays out of this round's scope).

### 7.6 The estate at this round's head (executed)

All with `--retries 0 --no-tests=fail`, `CARGO_PROFILE_DEV_DEBUG=0
CARGO_PROFILE_TEST_DEBUG=0`, from `net/crates/net/`:

| leg | command | result |
|---|---|---|
| org_streaming | the plan's named command (`-p net-mesh-sdk`, the named SDK feature set, `--test org_streaming`) | **14/14**, exit 0 |
| SDK org estate | same base, `--lib --test org_exact_sensing` | **338/338**, exit 0 |
| org_rpc_streaming | `cargo tf --retries 0 --test org_rpc_streaming` | **42/42**, exit 0 |
| fmt | `cargo fmt -p net-mesh-sdk -- --check` | exit 0 |
| probe | `cd guards/org_api_probe && cargo metadata --locked --format-version 1` / `cargo check --locked` | exit 0 / 0 (the pin UNCHANGED — Row 3's premise is behavioural, and this round made no production change) |

### 7.7 Never executed here (complete)

- Any host but this Windows workstation; any feature set other than the named
  SDK set for the org_streaming runs and the `cargo tf` alias set for
  `org_rpc_streaming`.
- `ci.yml`/nextest floor re-pins and `check-witness-results.py --self-test` /
  `check-roster.py` (Main's by contract); CI itself (branch unpushed).
- The `webrtc`/wasm graphs, the benches, and every binding (Stage 4's rows by
  contract). The F-S1R-2 rider and the F-S3.1-2 Stage-4 rider (out of scope
  by the brief).
- A wire-level duplicate-CANCEL-frame count (§6.1's stated limit, unchanged).
- Row 2's short-default clause over a second LIVE wire call — impossible by
  construction while the bridges pin `q1_defaults()` (§7.2's disclosure);
  executed at the fold seam over the facade call's real captured payload.
- `cargo fmt`/clippy/doc beyond `cargo fmt -p net-mesh-sdk -- --check`.
