# Stage 1 independent review packet — ORG_SCOPED_STREAMING_PLAN

**Reviewer:** S1Review (independent; HOLD authority, no edit authority).
**Reviewed head:** `e25ac28bf2b3805bbb65990ac0647b739928936e` on `LZL0/org-streaming`
(base of the reviewed patch: `096f54009` — the pinned S1 brief's commit; 29 files,
+15050/−978). **Probe worktree:**
`C:/Users/chief/orca/workspaces/net/org-streaming-s1rev` (detached at `e25ac28bf`,
own `CARGO_TARGET_DIR`s `target-s1rev` / `target-s1probe`). Every mutation ran only
there; every probe cycle ended sha-proven byte-identical to the pinned head and with
a green leg. Date: 2026-09-22. Windows host only.

**Citation convention (as requested):** source citations are **pristine-at-`e25ac28bf`**
line numbers. Every quoted panic states its numbering: my mutations are in files
OTHER than the test file holding the assertion, so test-file panic lines are pristine
except where a `+N` mutation delta is stated; the lane receipts I re-executed quote
pre-format baselines, so their numbers differ by the formatting delta — each receipt
below gives both numbers and the assertion identity is byte-compared.

---

## 1. Verdict: **HOLD**

**This is not a CI-only hold and not a production-defect hold.** Every one of the
seven deferred receipts I re-executed reproduced its quoted red exactly, the
retirement/refusal half of the §2.2/§2.3/§3 contract is superbly pinned (each of my
inverses there reddened a named witness), and I found no misbehavior in the delivered
code. The hold is on **the Stage 1 Exit paragraph's positive claim and the completion
half of the contract**: "same-org and cross-org live native server-streaming calls
produce multiple correlated items and explicit completion" has no content-correlated
executed witness, and **six of my own inverses at production seams go green across the
entire estate** (post-close drain, success-terminal wire shape, §2.7 node-rollback,
§2.4 incarnation fencing, the emitter session fence, and the permit handoff). The
closure is witness work only — no rewrite, no new framework, no scope expansion is
asked for. Rows 1.1–1.6 themselves all pass (§2).

Acceptance vs authorization: this HOLD governs Stage 1's acceptance and the Stage 2
gate per the assignment; it does not block any separately authorized work.

## 2. Per-row acceptance answers

**1.1 Session binding — MET.** Witnesses (a)/(b)/(c) exist in `wire/src/session.rs`
(`session::tests::…`, present in the executed 278-roster) with genuinely discriminating
observations: (a) compares the stored binding to the FULL independently captured
transcript hash on both sides, (b) asserts difference across re-handshakes with `!=`,
(c) exact `None`. Inverse receipts R-a/R-b/R-c in the lane report; I re-executed **R-b**
(§9): red matches (`a fresh establishment must bind differently`, zero-vectors both
sides, summary `1 passed, 2 failed` byte-matching the receipt, collateral (a) as
disclosed). Four weakenings: none. Coordinator's R-a spot-check retained.

**1.1a handshake migration (F1 assignment) — MET.** `a_real_completed_handshake_stores_its_binding_and_peer_session_binding_returns_it`
(`org_routing_wiring_tests.rs:7055`, assert at `:7115`) drives a real routed handshake
and asserts `peer_session_binding == Some(the independently captured hash)`.
I re-executed **R-1.1a** in full (§9): the three-hunk old-path reversion at
`mesh.rs:28491/:28668/:28712` reddens it at `:7115:5` with the exact quoted assertion
(`left: None / right: Some([…32 bytes])`). The coordinator's earlier attempt was
invalidated (compile error = never a red); mine is a clean assertion red. Carve
compliance spot-checked: the four `None`-binding slots in the floor-pinned file are
signature adaptation only (source-audited).

**1.2 streaming proof — MET.** All six named witnesses exist and discriminate
(source-read in full): `unary_context_proof_is_binding_invalid_on_stream_registration`
asserts the typed distinction (`BindingInvalid` vs the truncation's `MalformedProof` —
the two-leg exact-reason assert defeats the "any exception" oracle weakness);
`stream_opening_admits_{same_org,cross_org}` assert five-field/four-party attribution;
`stream_proof_on_unary_registration_is_not_supported` pins typed `StreamingUnsupported`
+ coarse `NotSupported`; `replayed_opening_on_new_session_is_session_binding_mismatch`
has an in-test positive control (first arrival admits) and pins the fence's ORDER
(`SessionBindingMismatch`, not `Replay`). Receipts A/A2/A3/B/C/D in the report; I
re-executed **1.2C** (frozen step-4 removal) with an exact red match (§9) and probed
the frozen decoder's tolerance (**A7**, §3/§9). Findings 1–2 of the lane (the
transcript-truncation inverse reach; C3's two-input shape term) are honest disclosures,
not defects.

**1.3 fold ownership + lifetime — MET.** All six named + three regression witnesses
exist with strong oracles (source-read in full): ordered content asserts
(`[open, live]`, body `b"openlive"`), exact deadline math with `assert_eq!` AND
`assert_ne!` in `requested_deadline_within_cap_is_honoured` (defeats coincidence),
handler-entry preconditions before every expiry observation (defeats "subsystem dead"),
non-vacuous drop flags (the witness explicitly waits for handler poll before the drop),
"exactly one terminal, ever" re-checked after a settle. Receipts R1–R6; I re-executed
**R2** (§9) with an exact match including the Δ = 3300 s arithmetic. The A2 combined
inverse (my own) also reds two of these witnesses (§4). C7's corrected premise
(F-S1.3-1) is properly recorded in the plan text.

**1.4 registry + revocation — MET.** All nine named witnesses exist with discriminating
observations: synchronous-boundary captures at `apply_bundle`/install return, both-branch
asserts (victim retires AND sibling doesn't), generation-refresh `assert_ne!` +
`CommitVerdict::Proceed`, exactly-once `removals(&key, 1) == 1`, the W6 clock pairing
documented in-test. Receipts INV-A/INV-B/R1–R7; I re-executed **INV-A** (§9) with an
exact red match (`the sibling stream of another org sends its next item after
publication: RpcSinkClosed`). The two disclosed partial inverses (R6(1), the 1.2-D
decoder substitution) are the four-weakenings discipline working as designed.

**1.5 bridge wiring + routing — MET.** All three named witnesses exist;
`forbidden_stream_opening_causes_zero_handler_effects` is the plan's strong reading
literally (handler entry + sink sends + wire frames + `in_flight_keys`/`sender_keys` +
flow permits + registry rollback, plus CS/DX legs and bounded `assert_stays_empty`);
the NC2 witness has both legs (told-exactly-once AND reflection-drop). Receipts
INV-A/INV-B/INV-C; I re-executed **INV-C** (§9) with an exact red match (`left: 2 /
right: 1` at the replay-slot assert). One gap inside this row's own claim is finding
F-5 (the `session_id` half of the row's "real `session_id` in `RpcResponseJob`" demand
is green under my inverse).

**1.6 deleted pins — MET.** The protocol held: `malformed_and_streaming_are_distinct`
split 1→3 (`malformed_proof_is_refused`, `unary_registration_streaming_flags_are_streaming_unsupported`
— the SURVIVING unary denial invariant present and green in my 21-run — and the new
`streaming_registration_admits_the_supported_shape` positive); `stability_recheck_runs_after_credential_checks`
rewritten with the ordering property kept and strengthened (three legs); the coarse
extension complete (37 variants incl. the two previously unlisted). In
`call_streaming_mints_a_stream_proof` the capability-mismatch half and the CS/DX/
service-routed refusal legs are kept verbatim (source-read at `mesh_rpc.rs:9935-9970`).
Unit totals verified by my runs: `org_admission` 21, `mesh_rpc` filter 52 (finding F-9
on the label), `org_stream` models intact. I re-executed **1.6-R2** (§9) with an exact
red match (`InvalidFormat` at the strict decode).

## 3. Exit paragraph answers (and the §1.4 gate)

1. **"same-org and cross-org live native server-streaming calls produce multiple
   correlated items and explicit completion" — NOT ESTABLISHED (finding F-1/F-2/F-7).**
   The fragments execute: same-org and cross-org admission with real mints through the
   production verifier (`stream_opening_admits_*`; `sibling_stream_of_other_org_sends_next_item_after_publication`
   is a cross-org call that SENDS two content-labelled items); explicit completion is
   observed as `Completed(_)` + a two-frame emit capture in three successor/sibling
   witnesses. But no single witness combines them, no SS success path asserts item
   CONTENT (the two-frame asserts count `chunk + terminal`; `sibling_stream…` discards
   its output capture at `tests/org_rpc_streaming.rs:1554`), and my inverses prove the
   missing pieces are silently regressable (F-1: post-close chunks can be discarded;
   F-2: the success terminal can lose its `end` marker). The multi-item queued-drain
   schedule (Stage 0 model obligation (c)) is model-only.
2. **"forbidden openings cause zero handler effects" — HOLDS.** Executed witnesses with
   the strong observation set; INV-B red in the record; my A1/A2 inverses' collateral
   behaviour consistent.
3. **"expiry/revocation/replacement/drop retirement and backpressure-blocked retirement
   are executed witnesses" — HOLDS.** All six families named and witnessed
   (`omitted_deadline…`, `credential_clamp…`, `pump_parked…`; `floor_raise…`,
   `poisoned_store…`, `store_replacement…`; `session_replacement…`;
   `serve_handle_drop…`, `node_shutdown…`; `queued_bytes_over_call_budget…`) — and
   discriminating (my §2.2 inverse reds two of them).
4. **"unsupported peers fail closed with `NotSupported`" — HOLDS.** Typed + coarse byte
   pinned at the new provider and executed against the frozen old provider.

**§1.4 mixed-version gate — genuinely executed against verbatim frozen `85ecc77c9`
code (executed + hash-proven).** All five recorded extraction sha256s reproduce from my
own `git show 85ecc77c9 | sed -n` extractions: `64adefbc…` (org_call 50–359),
`ef756fdb…` (org_admission 64–662), `2b1234f4…` (serve 3400–3438), `f74300ce…`
(1124–1127), `fa275454…` (872–876). The vendored bodies are byte-identical windows of
those extractions (`old_org_call.rs:41-350`, `old_org_admission.rs:34-632`,
`old_serve.rs:111-149`, `:187-190`) — EXCEPT the five-line denial-shape piece, which is
content-identical modulo an **unnamed re-indentation** and the ONE named `crate::`→`net::`
retarget (finding F-8). The vendored decoder really is the trailing-byte-tolerant
`postcard::from_bytes` one (`old_org_call.rs:320`): (i) re-run green of
`frozen_old_provider_refuses_stream_proof_with_not_supported` in my 27; (ii) my **A7**
probe (§9) — strictifying the vendored decode flips the frozen outcome to
`left: MalformedProof / right: StreamingUnsupported`, i.e. the typed `NotSupported`
REQUIRES the suffix tolerance; (iii) the **1.2C** red (`left: BindingInvalid`) proves
the frozen decode ACCEPTED the 33-byte streaming suffix. The prefix design holds; it is
not withdrawn.

## 4. Findings

Classification labels follow the review ledger: **witness gap** (a property with no
discriminating witness), **evidence composition** (the stage's claim not backed by one
executed observation), **record quality** (the record over/mis-states its own evidence).
Weakening attribution: for every green-under-inverse finding, **none of the four
weakenings applies** — no witness was widened, relaxed, deleted, or precondition-fixed;
the property simply has no witness (negative-space gap). Nothing was re-pinned.

**F-1 (witness gap; executed green-under-own-inverse; P2).** §2.2's `Completed(Ok)` /
`Completed(Err)` drain row ("drained in order, then terminal") is unwitnessed in
production. My inverse A2b (discard chunks at producer close instead of draining, pump
kept alive — `cortex/rpc.rs:5572-5573` seam) passes **27/27, exit 0**. The steady-state
completions publish their single chunk before handler return (a `ParkUntilReleased`
sends then parks: `s13.rs:282-284`), and the zero-credit witnesses end in retirement
(theirs is the DISCARD row, pinned). Trigger: any regression that drops queued items at
producer close. Impact: silent item loss on the success path. Closure property: a named
witness in which a handler queues ≥2 items under zero credit, returns, a valid
`STREAM_GRANT` arrives, and the items publish IN ORDER before one `Completed(Ok)`
terminal — reddening under A2b.

**F-2 (witness gap; executed green-under-own-inverse; P2).** The success-terminal wire
shape is unpinned. `stream_terminal_payload`'s `Completed(Ok)` arm
(`cortex/rpc.rs:3452-3459`) is the sole emitter of `Ok` + `nrpc-streaming: end`; my
inverse A2c (`end` → `continue` at `:3456`) passes **27/27, exit 0**. Every RETIREMENT
arm is content-pinned (`Timeout`/`AdmissionDenied`+[0]/[2]); the completion arm is only
frame-counted. Trigger: a mapping regression. Impact: callers' terminal detection
(`streaming_terminal_detection…`) never sees a terminal — a hang-class failure on the
happy path. Closure property: the F-1 witness also asserts the terminal frame's exact
wire content (status + `nrpc-streaming: end`), reddening under A2c.

**F-3 (witness gap; executed green-under-own-inverse; P2).** §2.7's "rolling back
acquired reservations on later refusal" (call → caller → node) is unwitnessed at the
node leg in production. My inverse A3 (deleting both counter restorations in
`ByteBudgets::reserve`'s `NodeBudgetFull` arm, `cortex/rpc.rs:3883-3887`) leaves
`byte_reservation_rolls_back_in_order_and_releases_exactly_once` GREEN (exit 0).
Impact: a refused item permanently inflates the caller/call byte counters until retire.
Closure property: the byte unit (or a sibling) drives a node-refusal after two
successful level increments and asserts both rolled back — reddening under A3.

**F-4 (witness gap; executed green-under-own-inverse; P2).** §2.4's "a late retire
against a reused key is a no-op" is unwitnessed for the production registry. My inverse
A4 (dropping `record.incarnation != incarnation ||` in `retire_locked`,
`cortex/rpc.rs:5088-5090`) passes **27/27 and the 161 unit run, exit 0**. The witnesses
that reuse a key first wait for `record_count() == 0`, so the late-op window is never
open. Impact: a late retire/complete can settle or remove a successor call. Closure
property: a named witness that reuses `(caller, call_id)` while the old record's async
cleanup is still armed and asserts the successor's survival — reddening under A4.

**F-5 (witness gap; executed green-under-own-inverse; P2).** Slice 1.5's own "carries
the record's REAL `session_id` in `RpcResponseJob`" (the R2-A receiving-incarnation
fence) is unwitnessed. My inverse A5 (forcing `receiving_session_id = 0` at
`mesh_rpc.rs:4771`) passes **27/27, exit 0**. The DirectOnly/fallback half IS pinned
(INV-A red); the session half is not. Impact: a chunk/terminal could settle on a
replaced session. Closure property: a witness where a response arrives after session
replacement and the replaced session's endpoint receives nothing while the live one
does — reddening under A5.

**F-6 (witness gap / dead surface; executed green-under-own-inverse; P3).**
`ItemPermit::transfer`'s release-once consumption (`cortex/rpc.rs:4033-4040`) is green
under my inverse A3b (dropping `self.settled = true`) — consistent with the report's own
F-S1.4-9 disclosure that `transfer` has no production caller. It is dead code today but
is the named handoff for Stage 2's request-direction queues; un-witnessed dead code that
is scheduled to go live is a trap. Closure property: either wire-and-witness the handoff
(the S0 model's `cancel_dequeue_handoff_consumes_one_permit` shape at the production
site) or delete `transfer` until a caller exists.

**F-7 (evidence composition; executed + source; P2).** The Exit's positive claim has no
content-correlated witness (detail in §3.1): the two-frame asserts count rather than
identify ("counting where the claim is identity"), and `sibling_stream…` explicitly
discards its output capture (`tests/org_rpc_streaming.rs:1554`). Same-org and cross-org
MULTIPLE items and explicit completion never appear in one observation. Closure
property: one named witness per authority mode (same-org, cross-org) whose asserted
observation is item bodies + order + the exact terminal frame at an authenticated
endpoint — with F-1/F-2 inverses red.

**F-8 (record quality; executed hashes; P3).** The frozen provenance claim ("Bodies are
byte-identical … Adaptations (named, exhaustive — everything else is byte-identical)",
`old_serve.rs:15`) is inexact for the denial-shape piece (`old_serve.rs:206-212`): the
five lines are re-indented (trimmed hashes match `a1c518dd…`; raw windows differ) and
that adaptation is not named. Behaviour is unaffected (whitespace-insensitive; the only
content change is the named `crate::`→`net::` retarget on `:208`). Closure property:
name the re-indent in the module doc, or restore the original indentation inside the
verbatim block.

**F-9 (record quality; source-established; P3).** `S1_REPORT.md:2105-2106` (and
`:2201-2202`) attributes `52 → 52` tests to `adapter/net/mesh_rpc.rs`, but the filter
`adapter::net::mesh_rpc` selects 39 tests in `mesh_rpc.rs` + 13 in `mesh_rpc_metrics.rs`
(my `nextest list` + per-file attribute counts). The count is honest for the filter; the
module label is not. Closure property: relabel the claim to the filter (or split by
module).

## 5. Preserved credit — verified holds; do not rework

1. **All seven reproduce legs pass at the pinned head** (executed): `org_rpc_streaming`
   27/27 (roster regenerated from source — 27 `#[test]`/`#[tokio::test]` fns == 27
   executed names), `org_ownership` 32/32, preserved+controls 89/89 (preserved trio
   present), wire 278/278, `behavior::org_admission::` 21/21, `adapter::net::mesh_rpc`
   52/52, in-source 216/216 (5622 skipped — byte-matching the report).
2. **All seven deferred receipts I re-executed reproduce their quoted reds exactly**
   (§9): R-b, R-1.1a, 1.2C, R2, INV-A, INV-C, 1.6-R2. Assertion identities byte-equal.
3. **The §1.4 mixed-version gate is real** (hash-proven verbatim + executed typed
   `NotSupported` + tolerance probes A7/1.2C). The frozen modules carry
   `#[rustfmt::skip]` provenance protection and the disallowed-methods carve rationale.
4. **The retirement/refusal half of the lifecycle contract is excellently pinned** —
   my inverses at the tie rule (A1), the close/producer-finished invariant (A2), the
   drain policy's discard twin, the raise/replacement/poison paths all red at named
   assertions (A1/A2 reds recorded in §9).
5. **Zero-effects witnesses use the strong reading** (handler + sends + wire + maps +
   registry) with bounded `assert_handler_stays_dark` whose window is byte-identical to
   `integration_nrpc_protected.rs`'s 200 ms/10 ms original — the "do not shrink" rule
   held.
6. **The 1.6 deleted-pin protocol held**: the surviving unary denial invariant, the
   capability-mismatch half, and the CS/DX/service refusal legs are all kept and green.
7. **The two named rewrites are strengthenings** (executed for rewrite 2 via A6b's red
   at `org_stream_registry.rs:2362:9`; source-audited for rewrite 1: exact `== 2`
   preconditions, `== 0` teardown, the inertness loop covering BOTH snapshotted
   callbacks). The F-S1.4-1 ruling (`Revoked → Denied` per §4.3) is correctly realized
   in both model and production.
8. **No lint-dodging in the 24 lint fixes** (source-audited): all 7 `#[expect(...)]`
   carry honest invariant reasons on the `wire/src/crypto.rs` precedent (byte-permit
   release-once, record-presence-under-lock, Q1-by-construction); the `#[allow]`s are
   style (`too_many_arguments`, repo convention), small dead helpers, or the documented
   frozen-module `disallowed_methods` provenance carve. The `non_exhaustive` fallback in
   the probe deletes no arm.
9. **Probe discipline intact** (executed): `cargo metadata --locked` exit 0,
   `cargo check --locked` exit 0 at its exact CI commands; the C1 literal pin was
   updated-not-deleted in the same commit as the break.
10. **Receipt hygiene of the lanes is high** (red-kind census + my 7 reproductions:
    zero compile-error "reds", all restores sha-gated).

## 6. HOLD rows and closure properties

No table row 1.1–1.6 is held. The **Exit paragraph's first claim** is held; its closure
property (not an implementation): **one named executed witness per authority mode
(same-org and cross-org) in which a protected server-streaming call through the real
bridge delivers MULTIPLE items — asserted by body content and order, not counted — and
then EXPLICIT COMPLETION whose terminal frame is asserted at the authenticated
receiving endpoint (status `Ok` + the `nrpc-streaming: end` marker); the witness must
redden under (a) a post-close discard of queued chunks (A2b) and (b) a success-terminal
shape change (A2c).** F-3/F-4/F-5 close with the per-finding properties in §4 (each is
a bounded unit/witness addition at the named production site, reddening under the named
inverse). F-6 closes by wiring+watching the handoff or deleting the dead surface.

## 7. Never executed here (complete)

- Linux/macOS and `#[cfg(unix)]` legs (Windows host).
- CI itself (branch unpushed); the exact-head CI run by `headSha` is unverifiable from
  here — the stage-end checklist claims in the plan Review log were verified only where
  named in my reproduce list (§8), not re-litigated wholesale.
- The benches (0.1 baseline numbers — report-attested only).
- The wasm32 test runner (only `cargo check`-class evidence exists).
- The coordinator's five spot-checked receipts (R-a, the shutdown carve R7, the 1.5
  DirectOnly flip INV-A, the Revoked-mapping R4 of 1.6, the org_ownership disengage
  receipt) — retained from the coordinator's executed spot-checks plus my source audit,
  not re-executed here except where noted.
- The other deferred receipts not in my seven: 1.1 R-c; 1.2 A/A2/A3/B/D; 1.3 R1/R3/R4/
  R5/R6; 1.4 INV-B/R1/R2/R3/R4a/R4b/R5/R6/R7; 1.5 INV-B; 1.6 R1/R3 — audited by
  red-kind census and my own adjacent probes, not re-executed.
- CS/DX protected admission (Stage 2 by contract), the facade/SDK/binding surfaces.
- Live two-process transport receipts (the fold-level witnesses capture at the
  emit seam; the s15 trio rides real wire endpoints for the refusal paths only — the
  F-7 gap is exactly this).
- The 24 lint fixes' clean clippy run (the stage-end `-D warnings` runs were not
  re-executed; the fix SHAPES were audited).

## 8. Reproduce ledger (all executed at `e25ac28bf`, `--no-tests=fail --retries 0`)

| Leg | Command (from `net/crates/net/` unless noted) | Result |
|---|---|---|
| org_rpc_streaming | `cargo nextest run --no-tests=fail --retries 0 --features "cortex tool fixtures" --test org_rpc_streaming` | 27/27, exit 0 |
| org_ownership | same, `--test org_ownership` | 32/32, exit 0 |
| preserved+controls | same, 8× `--test` (nrpc_streaming_gate, integration_nrpc_streaming, integration_nrpc_client_streaming, integration_nrpc_duplex, nrpc_registration_order, integration_nrpc_protected, org_admission_wire, subnet_org_boundary) | 89/89, exit 0 |
| wire | from `wire/`: `cargo nextest run -p net-mesh-wire --no-tests=fail --retries 0` | 278/278, exit 0 |
| org_admission units | `cargo nextest run … --lib --features "<UNIT>" -E 'test(/^adapter::net::behavior::org_admission::tests::/)'` | 21/21, exit 0 |
| mesh_rpc units | `… --lib <UNIT> adapter::net::mesh_rpc` | 52/52, exit 0 |
| in-source | `… --lib <UNIT> adapter::net::cortex::rpc adapter::net::mesh_rpc org_stream` | 216/216 (5622 skipped), exit 0 |
| probe | `guards/org_api_probe/`: `cargo metadata --locked --format-version 1`; `cargo check --locked --message-format short` | exit 0; exit 0 |

`<UNIT>` = `net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex`.

## 9. Re-executed receipts and probes (executed; bounded diffs at production sites)

Restore discipline per cycle: `git checkout -- <file>` then `sha256sum` == the
pre-mutation baseline (each baseline below), then a green leg. Two aborted attempts
count for nothing and are disclosed: an A2b first run hit 2 anchor matches (guard died
before any write), a second compiled-error variant (E0425 — a compile error is never a
red), and A4's first attempt hit a perl delimiter parse error (no write).

| # | Receipt/probe | Mutation (site) | Observed red / result | Baseline sha (restore ==) |
|---|---|---|---|---|
| 1 | **R-b** (1.1) | `with_binding` stores `[0u8;32]` (`wire/src/session.rs`) | exit 100: `a fresh establishment must bind differently` both `Some([0×32])` at `session.rs:3468:9` (observed-under-mutation +1; pristine :3467; lane quote `:3465:9`/pristine `:3464` at its pre-format baseline — assertion byte-identical); collateral (a) as disclosed; summary `1 passed, 2 failed` matches | `47d6f9c4…` |
| 2 | **R-1.1a** (1.1a, REQUIRED) | routed case-2 old path, 3 hunks (`mesh.rs:28491,28668,28712`) | exit 100: `the installed session must carry the full handshake hash… / left: None / right: Some([210, 197, …])` at `org_routing_wiring_tests.rs:7115:5` (pristine; lane quote identical text at `:7107:5` observed / `:7115:5` pristine) | `a5581890…` |
| 3 | **1.2C** (1.2 frozen, REQUIRED) | vendored step-4 `if !ctx.is_unary` removed (`old_org_admission.rs`) | exit 100: `the frozen path must carry the typed StreamingUnsupported refusal / left: BindingInvalid / right: StreamingUnsupported` at `org_rpc_streaming.rs:429:13` (pristine; lane `:405` pre-format — assertion byte-identical) | `d5faa9fe…` |
| 4 | **R2** (1.3) | `None` arm fills `max_live` (`cortex/rpc.rs:2951`) | exit 100: `omitted ⇒ exactly the Q1 300 s provider default / left: …292133200 / right: …292133200` (Δ 3300 s exactly as quoted) at `org_rpc_streaming.rs:689:5` | `630f8cb4…` |
| 5 | **INV-A** (1.4) | `GenerationOnly` arm retires unconditionally (`cortex/rpc.rs` `commit_check_locked`) | exit 100: `the sibling stream of another org sends its next item after publication: RpcSinkClosed` at `org_rpc_streaming.rs:1531:10` (lane `:1504:10` pre-format — assertion byte-identical) | `630f8cb4…` |
| 6 | **INV-C** (1.5) | step-11 policy block relocated before the step-10 insert (`org_admission.rs`) | exit 100: `the replay slot stayed CONSUMED … / left: 2 / right: 1` at `org_rpc_streaming.rs:2805:5` (lane `:2812:5` pre-format — assertion byte-identical) | `db305c20…` |
| 7 | **1.6-R2** (1.6) | `call_streaming` mints `Unary, None` (`mesh_rpc.rs:6057-6063`) | exit 100: `the minted bytes are a FULL streaming proof (strict decode): InvalidFormat` at `mesh_rpc.rs:9925:14` (lane `:9928:14` pre-format — assertion byte-identical) | `eda1c392…` |
| 8 | **A1** (attack, §2.1 tie rule) | `<=` → `<` (`cortex/rpc.rs:2959`) | exit 100: `an exact tie reports credential expiry, not timeout / left: Deadline / right: Credential` at `org_rpc_streaming.rs:1147:5` — witness discriminates | `630f8cb4…` |
| 9 | **A2** (attack, close-arm combined) | discard-at-close + early pump exit (`cortex/rpc.rs:5573`) | exit 100: `pump_parked_on_zero_credit…` + `floor_raise…` red ("producer finished is not terminal" pins discriminate) | `630f8cb4…` |
| 10 | **A2b** (attack, F-1) | post-close chunks discarded, pump alive (`cortex/rpc.rs:5604`) | **exit 0: 27/27 GREEN** — finding F-1 | `630f8cb4…` |
| 11 | **A2c** (attack, F-2) | `Completed(Ok)` header `end`→`continue` (`cortex/rpc.rs:3456`) | **exit 0: 27/27 GREEN** — finding F-2 | `630f8cb4…` |
| 12 | **A3** (attack, F-3) | node-arm rollback removed (`cortex/rpc.rs:3883-3887`) | **exit 0: `byte_reservation_rolls_back_in_order_and_releases_exactly_once` GREEN** — finding F-3 | `630f8cb4…` |
| 13 | **A3b** (attack, F-6) | `transfer` not consuming ownership (`cortex/rpc.rs:4034`) | **exit 0: 53/53 byte/permit-filtered units GREEN** (dead path — F-S1.4-9) — finding F-6 | `630f8cb4…` |
| 14 | **A4** (attack, F-4) | `retire_locked` incarnation fencing dropped (`cortex/rpc.rs:5088`) | **exit 0: 27/27 + 161/161 GREEN** — finding F-4 | `630f8cb4…` |
| 15 | **A5** (attack, F-5) | emitter `receiving_session_id = 0` (`mesh_rpc.rs:4771`) | **exit 0: 27/27 GREEN** — finding F-5 | `eda1c392…` |
| 16 | **A7** (probe, §1.4 tolerance) | vendored decode strictified (`old_org_call.rs:320`) | exit 100: `left: MalformedProof / right: StreamingUnsupported` at `org_rpc_streaming.rs:429:13` — trailing tolerance is load-bearing | `0ab7e915…` |
| 17 | **A6** (probe, disclosed non-discriminator) | whole rewrite-2 reverted (mapping + pin together) | exit 0 GREEN — self-consistent pre-state; disclosed as non-discriminating by construction | `caef8ef7…` |
| 18 | **A6b** (probe, rewrite-2 discrimination) | mapping only flipped back (`org_stream_registry.rs`) | exit 100: `assertion 'left == right' failed / left: Unavailable / right: Denied` at `org_stream_registry.rs:2362:9` (pristine) — the rewritten pin discriminates | `caef8ef7…` |

Frozen provenance hashes I re-derived myself (all match the recorded values):
extraction `64adefbc…`, `ef756fdb…`, `2b1234f4…`, `f74300ce…`, `fa275454…`; body
windows `41-350`/`34-632`/`111-149`/`187-190` byte-exact; the denial piece trimmed
`a1c518dd…` both sides (F-8).

## 10. Final-verification record

- Pinned HEAD: `e25ac28bf2b3805bbb65990ac0647b739928936e` (detached probe worktree).
- Diff-check: `git diff --stat 096f54009..e25ac28bf` = 29 files, +15050/−978 (the
  reviewed patch).
- Probe removal: every mutation restored sha-proven (§9 baselines); the worktree's
  tracked files are byte-identical to the pinned head (`git status --porcelain` shows
  only the two untracked `CARGO_TARGET_DIR` build caches).
- Final restored-green: `org_rpc_streaming` 27/27 exit 0 at the pristine tree.
- Session-tree writes: this packet only.
