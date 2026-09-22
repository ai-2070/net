# Stage 2 — core protected client-streaming and duplex

**Authorization and limits.** Authorized 2026-09-22 by the Stage 1 ACCEPT
(independent review round 2.1 at `cc15f4d66`, packet
`docs/internal/spikes/org-streaming/S1_REVIEW_PACKET_2.md`) and this pinned
brief. **Stage 2 as written in the plan (rows 2.1–2.4) — nothing more.**
Explicitly NOT this stage: facade/SDK verbs, bindings, payments, serverless,
browser/leaf work, replay-guard semantic changes, resume/migration, Stage 3+.
**OWNER RULING (2026-09-22, supersedes the owner-pending note):** the
F-S1R-2 rider — the "post-replacement terminal that cannot be silently
dropped" enhancement (terminal-retarget composed with the
receiving-incarnation fence and the exactly-one-terminal rule), the
caller-side last mile and the delivery-exactly-once composition note — is
**NOT included; the plan-as-written governs.** The documented limitation
stands as recorded in `S1_REVIEW_PACKET_2.md` §3.1. If any closure here
would require the rider, that is a finding ("state, do not decide") — the
owner has declined it and it is out of scope.

## Source of truth — read first, in this order

1. The plan's **Stage 2 table** (rows 2.1–2.4 carry the full witness texts),
   §1 (proofs), §2.2/§2.4/§2.5/§2.6/§2.7/§2.8 (the lifecycle, half-close,
   accounting and terminal contracts), §3 (the admission transaction), §4.3
   (error mapping — `Revoked → Denied`), the compatibility ledger (esp. C5–C8,
   C11), and the Stage 1 rows 1.2–1.5 (the seams this stage rides).
2. `docs/internal/spikes/org-streaming/S1_REVIEW_PACKET_2.md` (what the
   evidence rules demand in practice) and `S1_REPORT.md` §0–§3 (the landed
   seams and every recorded ruling).
3. The Stage 0 models `src/adapter/net/behavior/org_stream_{lifecycle,registry}.rs`
   — still the semantic contract for the supervisor and registry; the
   client-streaming single-response rule (`client_stream_single_response_
   completes_without_pump`) and the §2.6 half-close table govern rows 2.2/2.4.
4. `AGENTS.md` / `TESTS.md` (feature sets, silent-skip, floors, Windows traps).

## Lanes and file ownership (strict)

One lane (`S2Core`), slices 2.1 → 2.4 **in order**, each landed green before
the next. Owns exclusively: `net/crates/net/src/adapter/net/mesh_rpc.rs`,
`net/crates/net/src/adapter/net/cortex/rpc.rs`, the CS/DX regions of
`net/crates/net/src/adapter/net/behavior/org_admission.rs` ONLY if the
CS/DX refusal-pin inversion needs them (row 5 below),
`net/crates/net/tests/org_rpc_streaming.rs` + `net/crates/net/tests/org_rpc_streaming/**`,
`net/crates/net/docs/ORGANIZATIONS.md` (the CS/DX verb note). Nothing else —
`wire/**`, `mesh.rs`, `behavior/org_call.rs`, the model files, `ci.yml`,
`.config/nextest.toml`, `spikes/**`, `docs/internal/**` are Main's or
verified work. A needed production change outside this set = finding, STOP.

## Frozen contracts and the Stage 1 seams (wire against these)

Contract 2 (proofs: `OrgStreamCallProof` kinds 2/3 over the finalized lazy
initial REQUEST — §1 binds the opening request INCLUDING the body),
contract 3 (registry API), contract 4 (fold seam: `apply_inbound_admitted`,
4-tuple keys, `RpcStreamingContext.org_admission`), contract 5 (bridge:
`admit_protected_opening` + `admit_and_dispatch_protected_stream` at the
`Proceed` seam, `ProtectedAdmission`, `DirectOnly` + real session id) — all
landed in Stage 1 and UNCHANGED here except where row 2.2 extends the same
seam to the CS/DX folds. The §2.2 supervisor semantics as modeled. The
recorded Main rulings bind: `Revoked → Denied`, `Timeout` vs
`CredentialExpired`, the Q1 limits (300 s default / 3600 s cap), NC2
`DirectOnly`, coarse bytes `{Denied, NotSupported, Unavailable}` frozen.

## Slices as dispatched (full witness texts in the plan's Stage 2 table)

**2.1 Lazy-opening mint.** Target: `mesh_rpc.rs:1880-1935`
(`publish_initial_request`), `:4544-4660`, `:4898-5040`. Change: mint
`OrgStreamCallProof` (kind 2/3) over the **finalized** initial REQUEST at
first `send`/`finish`; `JustOpened` drop still sends nothing. Witness:
`client_stream_opening_binds_first_chunk` (alter the first chunk after
signing → `BindingInvalid`, zero handler effects). Inverse: skip the body
digest in the transcript → the witness must be red-when-green.

**2.2 CS/DX admission.** Target: `mesh_rpc.rs:4462-4486`, `:4838-4862`;
`cortex/rpc.rs:3100-3525`, `:3560-3975`. Change: the same `Proceed`-seam
admission as 1.5 for the CS/DX folds;
`serve_rpc_{owner_scoped,granted}_{client_stream,duplex}`; every
CHUNK/END/CANCEL/GRANT checked against the record's shape AND direction
before delivery/credit. Witnesses: `client_stream_aggregate_with_valid_proof`;
`duplex_exchange_with_valid_proof`; `pre_admission_chunks_are_never_delivered`;
`end_cannot_cancel_another_stream_or_reopen_terminal_half`;
`wrong_session_grant_does_not_release_credit`. Inverse: deliver chunks on
`(node, origin, call_id)` only (3-tuple) → the wrong-session witness must
red.

**2.3 Duplex response flow control.** Target: `cortex/rpc.rs:3560-3572`,
`:3949-3965`. Change: the `flow_control` map + `STREAM_GRANT` arm for duplex
as SS (the §1 gap map's duplex-response row: today's duplex response
direction is unbounded); the caller's `stream_window_initial` header
honoured. Witnesses: `duplex_response_window_blocks_until_grant`;
`cross_direction_grant_is_ignored`. Inverses: remove the grant arm → the
initial block never releases; bypass the semaphore → the pre-grant blocking
assertion fails.

**2.4 Half-close + both-direction retirement.** Target: the CS/DX folds.
Change: END closes input once; retire closes both halves; independent halves
under one record (the §2.6 table and its model witnesses bind). Witnesses:
`upload_end_then_remaining_output_completes`; `retire_unblocks_both_directions`.

**Row 5 — the CS/DX intent-refusal pins invert (from F-S1.6-2).** The
refusal legs kept pinned in 1.6 for this inversion: `call_client_stream` and
`call_duplex` accept `org_proof_intent` and mint their kinds (C11 widening —
mirror 1.6's `call_streaming_mints_a_stream_proof` exactly: keep the
capability-mismatch half and the service-routed refusal legs). Witness:
`client_stream_and_duplex_mint_their_stream_proofs`. Unit totals
before/after by module in the report.

**Preserved witnesses (stay green, unpinned-name list):** every Stage 1 name
(the 30-roster), the Stage 0 models, `client_streaming_denies_unauthorized_caller`,
`duplex_denies_unauthorized_caller`,
`denial_is_not_fanned_out_to_the_reply_roster`,
`client_stream_bridge_rejects_before_fold_end_to_end`,
`duplex_bridge_rejects_before_fold_end_to_end`,
`reject_relayed_flow_controlled_request_rejects_only_relayed_flow_controlled_uploads`.
**Public regression control (unchanged and green):**
`integration_nrpc_streaming`, `integration_nrpc_client_streaming`,
`integration_nrpc_duplex`, `nrpc_streaming_gate`, `nrpc_registration_order`,
`integration_nrpc_protected`, `org_admission_wire`, `tests/cross_lang_*`.
Q3's shared public-fold repairs land WITH their own regression witnesses
(the C7 `Timeout` classification for CS/DX, the C8 duplex window honouring —
public and protected, per Q3).

## Evidence rules, CI, report (verbatim from `S1_BRIEF.md`)

Raw inverse receipts at the production site (bounded diff, exact command,
exit, verbatim assertion red — a compile error is never a red — sha-proven
restore, restored green); a witness green under its own inverse is a FINDING
(delete-and-replace, never re-pin; name which of the four weakenings);
rosters from source; `--retries 0 --no-tests=fail`; warm aliases only;
`cargo fmt -p <crate> -- --check` per crate; executed vs source-established;
"never executed here" over inference; disk discipline (verify size+sha after
every write under 5 GB free). Commit prefixes `S2.1:`…`S2.5:` (implementation
+ report record pairs). Append `## 4. Stage 2` to
`docs/internal/spikes/org-streaming/S1_REPORT.md` (or a `S2_REPORT.md` —
Main's choice at dispatch). Report the `org_rpc_streaming` binary's new
count + full name list to Main for a same-commit CI floor re-pin (Main pins;
never edit ci.yml). Owner-pending: the rider ONLY — anything else new is
"state in the report, do not decide". Report only on green at your exact
head. NO stage n+1.

## Exit (plan wording)

Client-streaming aggregate and duplex exchange execute with valid proofs;
each shape has its own zero-effect denial, wrong-session and control-frame
probes; SS success is not evidence for CS/DX. Internal checkpoint only.
