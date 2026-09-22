# S1_R — Stage 1 repair round (HOLD closure)

**Authorization and limits.** The S1Review HOLD packet
(`docs/internal/spikes/org-streaming/S1_REVIEW_PACKET.md`, reviewed head
`e25ac28bf`) and this pinned brief. **REPAIR ONLY**: witness work (rows 1–7),
one documentation fix (row 8), and record/CI bookkeeping. NO new features, NO
refactors, NO production-path changes, NO scope expansion. The packet's
**§5 Preserved credit (10 items) is verified work — touch none of it** except
where a closure row explicitly names a location. Owner-approved design is
unchanged: §1–§4 of the plan and the recorded Main rulings govern.

**Premises (coordinator-reproduced).** The six green-under-inverse probes were
reproduced at `e25ac28bf` in an isolated worktree with byte-identical restores:
A2b/A2c/A5 → `org_rpc_streaming` 27/27 GREEN; A3 →
`byte_reservation_rolls_back_in_order_and_releases_exactly_once` GREEN; A3b →
the byte/permit filter GREEN (135/135 observed, superset of the packet's
53-run); A4 → `adapter::net::cortex::rpc` filter GREEN (88/88 observed; the
packet's 161-run adds mesh_rpc units — claims consistent). The reviewer's
verdict and closure properties below are binding as written.

## Rows (each: citation → executed evidence → VERBATIM closure property)

**Row 1 — F-1 (P2, witness gap): the §2.2 completion drain.**
Cite: `cortex/rpc.rs:5572-5573/5604` (supervisor pump). Executed: inverse
A2b (post-close chunks consumed but not published, pump alive — the exact
mutation in the Appendix) passes 27/27. **Closure (packet verbatim): "a named
witness in which a handler queues ≥2 items under zero credit, returns, a valid
`STREAM_GRANT` arrives, and the items publish IN ORDER before one
`Completed(Ok)` terminal — reddening under A2b."** Witness name:
`completed_stream_drains_queued_items_in_order_with_content_and_end_terminal`
(same-org authority mode; it also carries Row 2's content assert and one leg
of Row 7).

**Row 2 — F-2 (P2, witness gap): the success-terminal wire shape.**
Cite: `cortex/rpc.rs:3452-3459` (`stream_terminal_payload`'s `Completed(Ok)`
arm). Executed: inverse A2c (`end` → `continue` at `:3456`) passes 27/27.
**Closure (packet verbatim): "the F-1 witness also asserts the terminal
frame's exact wire content (status + `nrpc-streaming: end`), reddening under
A2c."** Fold into Row 1's witness (both inverses must redden it, each at its
own named assertion).

**Row 3 — F-3 (P2, witness gap): §2.7 node-budget rollback.**
Cite: `cortex/rpc.rs:3881-3887` (`ByteBudgets::reserve`'s `NodeBudgetFull`
arm). Executed: inverse A3 (both counter restorations deleted) leaves
`byte_reservation_rolls_back_in_order_and_releases_exactly_once` GREEN.
**Closure (packet verbatim): "the byte unit (or a sibling) drives a
node-refusal after two successful level increments and asserts both rolled
back — reddening under A3."** Witness name:
`node_budget_refusal_rolls_back_call_and_caller_reservations` (in-source
sibling of the named byte unit, same test module).

**Row 4 — F-4 (P2, witness gap): §2.4 incarnation fencing at retire.**
Cite: `cortex/rpc.rs:5088-5090` (`retire_locked`). Executed: inverse A4
(the `record.incarnation != incarnation ||` clause dropped) passes 27/27 and
the cortex unit run. **Closure (packet verbatim): "a named witness that
reuses `(caller, call_id)` while the old record's async cleanup is still
armed and asserts the successor's survival — reddening under A4."** Witness
name: `late_retire_against_a_reused_key_is_a_no_op_for_the_successor`.

**Row 5 — F-5 (P2, witness gap): the R2-A emitter session fence.**
Cite: `mesh_rpc.rs:4771` (`receiving_session_id`). Executed: inverse A5
(forced `= 0`) passes 27/27. **Closure (packet verbatim): "a witness where a
response arrives after session replacement and the replaced session's
endpoint receives nothing while the live one does — reddening under A5."**
Witness name: `response_after_session_replacement_reaches_only_the_live_session`.

**Row 6 — F-6 (P3, dead surface): `ItemPermit::transfer`'s handoff.**
Cite: `cortex/rpc.rs:4033-4040`. Executed: inverse A3b (dropping
`self.settled = true`) passes the byte/permit filter GREEN (the F-S1.4-9
disclosure: `transfer` has no production caller yet).
**Closure (packet verbatim): "either wire-and-witness the handoff (the S0
model's `cancel_dequeue_handoff_consumes_one_permit` shape at the production
site) or delete `transfer` until a caller exists."**
**MAIN RULING (selected closure):** witness the handoff semantics at the
production `ItemPermit` — a unit witness of the S0 model's
`cancel_dequeue_handoff_consumes_one_permit` shape (source permit consumed,
target owns, exactly-one release across the pair, another call's live bytes
stay charged). Do NOT invent a Stage-2 caller and do NOT delete the method:
Stage 2's request-direction queues are its named consumer and §2.7 names the
handoff semantics. Witness name:
`item_permit_transfer_consumes_once_across_the_handoff`. If the witness
cannot discriminate A3b from correct code, STOP and state it (do not decide).

**Row 7 — F-7 (P2, evidence composition): the Exit's positive claim.**
Cite: `tests/org_rpc_streaming.rs:1554` (`sibling_stream…` discards its
output capture) and the two-frame count-only asserts.
**Closure (packet verbatim): "one named executed witness per authority mode
(same-org and cross-org) in which a protected server-streaming call through
the real bridge delivers MULTIPLE items — asserted by body content and order,
not counted — and then EXPLICIT COMPLETION whose terminal frame is asserted at
the authenticated receiving endpoint (status `Ok` + the `nrpc-streaming: end`
marker); the witness must redden under (a) a post-close discard of queued
chunks (A2b) and (b) a success-terminal shape change (A2c)."**
Same-org mode = Row 1's witness (already named). Add the cross-org twin:
`cross_org_completed_stream_drains_correlated_items_with_end_terminal`, same
observations, cross-org authority (the granted/other-org intent shape). Both
witnesses must each redden under A2b AND A2c at their own named assertions.

**Row 8 — F-8 (P3, record quality): frozen provenance wording.**
Cite: `old_serve.rs:15` (the "byte-identical … Adaptations (named,
exhaustive)" claim) vs `old_serve.rs:206-212` (the unnamed re-indentation;
trimmed hashes match `a1c518dd…`). **Closure (packet verbatim): "name the
re-indent in the module doc, or restore the original indentation inside the
verbatim block."** MAIN RULING: **name the re-indent** in the module doc
(whitespace-insensitive provenance statement + the trimmed-hash evidence) —
documentation only; the vendored bodies stay untouched (their extraction
sha256s are the record).

**Row 9 — F-9 (P3, record quality): the 52-count label.** Cite:
`S1_REPORT.md:2105-2106,2201-2202`. **Closure (packet verbatim): "relabel the
claim to the filter (or split by module)."** MAIN RULING: **Main fixes this
in the record commit** (the mislabel is the coordinator's record, not the
lane's) — the lane does NOT edit `S1_REPORT.md` §2.1/§2.5 text.

## Appendix — the six named inverses, verbatim (the receipt mutations)

All at `net/crates/net/src/adapter/net/…`; each receipt = apply → named red →
restore (sha-proven) → green. **A2b** (supervisor pump ONLY; anchor includes
the send-order comment): wrap `pump_emit(from_node, caller_origin, call_id,
resp).await;` in `if !pump_gate.as_ref().is_some_and(|g| g.is_finished()) {
… }`. **A2c**: `HEADER_NRPC_STREAMING_END.to_vec(),` →
`HEADER_NRPC_STREAMING_CONTINUE.to_vec(),` (the `Completed(Ok)` arm).
**A3**: delete both counter restorations in the `NodeBudgetFull` arm
(`state.per_caller.insert(…, caller_cur);` and the `state.per_call.insert(…,
call_cur);`), keeping the `return Err(ByteRefusal::NodeBudgetFull);`.
**A3b**: delete `self.settled = true;` from `ItemPermit::transfer`.
**A4**: drop `record.incarnation != incarnation ||` from `retire_locked`'s
guard. **A5**: `let receiving_session_id = cached.map(…).unwrap_or(0);` →
`let receiving_session_id = 0;`.

## Ownership, evidence rules, acceptance

Files (exclusive): `net/crates/net/tests/org_rpc_streaming.rs`,
`net/crates/net/tests/org_rpc_streaming/**` (incl. the `frozen_85ecc77c9.rs`
module doc for Row 8), and the **TEST MODULES ONLY** of
`net/crates/net/src/adapter/net/cortex/rpc.rs` (Rows 3/4/6 in-source units —
no production-path edit anywhere; a closure needing one is a finding, STOP).
Everything else is Main's or verified work. Evidence rules verbatim from
`S1_BRIEF.md` (raw inverse receipts at the production site; four weakenings;
rosters from source; `--retries 0 --no-tests=fail`; warm aliases; executed vs
source-established; "never executed here" over inference; disk discipline).
Order: land the witnesses first (Rows 1–7) as one commit — they are GREEN on
current code by design (only the witnesses were missing) — then one commit of
the inverse receipts per row (each: named red under its Appendix inverse →
restored green, i.e. the row's red-to-green), then the report section
`## 3. Repair round (S1_R)` appended to `docs/internal/spikes/org-streaming/S1_REPORT.md`
with landed hashes, witnesses + counts, receipts, findings, never-executed.
Report the `org_rpc_streaming` binary's new count + full name list to Main for
a same-commit CI floor re-pin (Main pins; never edit ci.yml). Report only on
green at your exact head. NO stage n+1, no Stage 2 work.
