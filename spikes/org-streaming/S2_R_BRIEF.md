# S2_R — Stage 2 repair round (ACCEPT findings closure)

**Authorization and limits.** The S2Review ACCEPT packet
(`docs/internal/spikes/org-streaming/S2_REVIEW_PACKET.md`, pinned head
`017e7148a`) and this pinned brief. **REPAIR ONLY (3 rows):** two
witness/guard closures and one record-wording fix. The ACCEPT stands; these
are its named non-blocking findings. NOTHING else: no features, no
refactors, no scope expansion. The owner-declined F-S1R-2 rider remains out
of scope (do not demand or build it). Row 2 authorizes exactly ONE
production change — the `ConfirmedOpening`-shape scope guard the plan's §3
step 5 already mandates ("Scope guards handle scheduling/installation
failure without orphaning a Running record") — it is a contract fix, not new
scope; anything beyond it is a finding, STOP.

## Rows (citations → executed evidence → VERBATIM closure property)

**Row 1 — F-S2R-1 (P2, witness gap): the §2.6 late-input disposition.**
Cite: `cortex/rpc.rs:3213-3218` (the input-Closed-on-return assignment at
`handler_returned`) and the chunk-path record gate in
`apply_request_chunk_to_senders`. Executed: removing the assignment AND,
separately, neutralizing the gate each leave the WHOLE estate green
(41/41 + 220/220 both runs) — and without the rule a late chunk after an
early handler return latches `ResourceExhausted` at the dropped receiver,
exactly the resource-overflow failure §2.6 forbids. **Closure (packet §7
verbatim): "a named witness in which a PROTECTED client-streaming or duplex
handler returns EARLY (before the caller's END) with its input half still
`Open`, a request chunk then arrives for the call, and the observed outcome
is: the chunk is refused/discarded — never delivered, never retained — no
`ResourceExhausted` latched, the handler's own result remaining the
terminal (exact wire content) with the record completing exactly once,
reddening under both mutations."** Witness name:
`early_handler_return_refuses_late_input_without_resource_exhausted`. Both
mutations are its required inverse pair (each must redden at its own named
assertion). Also correct the record's overclaim: §4 F-S2.4-2's "binds the
full §2.6 table" must be scoped to what its pair actually binds.

**Row 2 — F-S2R-2 (P2, fix class): the post-transfer scope guard.**
Cite: `cortex/rpc.rs:7543-7551` (both `apply_inbound_admitted` CS/DX seams'
post-`registry.confirm` window). Executed: the F-S2.2-5 repair settles the
record INLINE at the deliver-refusal site (correct there — release-once
complete + map cleanup, fail-pre-fix proven by R-S2.2c) but the window has
no Drop guard; the reviewer's synthetic installation-failure probe orphans
the Running record (`record_count` left: 1 — the key pinned forever as
`ActiveCallOwned`, one quota slot leaked per occurrence). Class exposure on
future paths and panics, not a head defect (the window is synchronous
today). **Closure (packet §7 verbatim): "any exit after `registry.confirm`
settles the record's release-once complete and every map entry via a
ConfirmedOpening-shape guard, demonstrated by a probe-witness in the shape
of the executed one asserting `record_count == 0` and empty
`in_flight_keys`, failing without the guard and passing with it."**
Witness name: `post_transfer_scope_guard_never_orphans_a_running_record`
(the probe-witness: insert the same synthetic installation failure the
reviewer used at the same window point; assert `record_count == 0` +
`in_flight_keys` empty + exactly one removal). The unary `ConfirmedOpening`
precedent is the shape to mirror (its own doc: "this scope guard stands in
for it so no Running record is ever orphaned").

**Row 3 — F-S2R-3 (P3, record quality): the proof-method wording.**
Cite: `docs/internal/spikes/org-streaming/S1_REPORT.md` §4 F-S2.1-4's
parenthetical — "the reflow-only hunks are `git diff -w`-empty (no semantic
change)". The substance is TRUE (the reviewer verified it) but the METHOD
claim mis-states its evidence: line re-wraps survive `-w` as line-count
changes. **Closure (packet §7 verbatim): "state the normalisation proof
(whitespace-stripped body digests) instead of the `-w` emptiness claim, or
restore the original wrapping. No code impact."** Replace the claim with
the reviewer's executed normalisation proof (whitespace-stripped body
digests at packet §3: the 30-roster preserved semantically byte-for-byte;
the three re-wrapped S1_R witnesses whitespace-identical). Documentation
only; no runtime inverse — say so.

## Preserved credit (packet §6 — do not rework)

The 30-roster is semantically byte-for-byte (executed normalisation); row
5's delete-and-replace protocol held (exactly the two C11-removed legs
deleted); the kept refusals, both content-identity twins and the
cross-direction matrix all verified; record quality is high with Row 3's
excepted wording.

## Ownership, evidence rules, acceptance

Files (exclusive): `net/crates/net/src/adapter/net/cortex/rpc.rs` (Row 2's
guard in the two CS/DX seams + the test-module probe-witness; Row 1's
input-gate locations only if the witness needs a test seam — no other
production change), `net/crates/net/tests/org_rpc_streaming.rs` +
`tests/org_rpc_streaming/**` (Row 1's witness), and
`docs/internal/spikes/org-streaming/S1_REPORT.md` §4 (Row 3's wording +
your `## 5. Repair round (S2_R)` record). Everything else is Main's or
verified work. Evidence rules verbatim (raw inverse receipts at the
production site — Row 1's PAIR required, Row 2's probe required,
four weakenings, rosters from source, `--retries 0 --no-tests=fail`, warm
aliases, executed vs source-established, never-executed lists, disk
discipline). Commit prefixes `S2R:` (implementation + receipts + record
pairs). Report the binary's new count + full name list to Main for a
same-commit CI floor re-pin. Owner-pending: none. Report only on green at
your exact head. NO stage n+1.
