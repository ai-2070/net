# Code review: the WebRTC transport repair pass (`LZL0/webrtc-fixes` vs `master`)

An independent review of the branch that claims to close all 130 findings of
[`WEBRTC_TRANSPORT_MERGE_REVIEW_2026_09_22.md`](WEBRTC_TRANSPORT_MERGE_REVIEW_2026_09_22.md).
Pinned head `193f8affe0d8652815d304b7728b552294891d11`, base `ef4a7e0f84d2e25a24bdf311061a4eec81aa9e37`
(master tip; merge-base = master). Diff under review: 84 files, +6364/−821, 18 commits.
Review executed 2026-09-22 on a Windows 11 workstation.

Verdict: **HOLD — this is not a CI-only hold. Production defects remain even if every
CI job goes green.** Four independent layers hold the candidate:

1. **Nine High findings, seven of them production defects inside the repair's own
   scope** (#1–#6, #9), plus two executed regressions/gaps (#7, #8).
2. **Three executed REDs at the pinned head in the candidate's own suites** (#7, #8),
   one of them branch-introduced and CI-visible, one of them a pre-existing production
   defect the branch's own repaired witness exposed and the ledger never records.
3. **The headline claim is contradicted at source and at execution.** "All 130 findings
   are closed" fails its own ledger arithmetic (#29), and the closures of MR#5, MR#25,
   the incarnation-fence claim, the tripwire-escape claim and MR#116's "discriminating"
   claim are each contradicted by code or by the witness's own reach (claims table below).
4. **CI corroborates**: the `ci.yml` workflow concluded `failure` at every pushed commit
   (078e72a41, 78880312c, db6dd0a2, 91ae3747) across nine jobs, while master's latest
   run is `success`. See "CI at the reviewed head".

This is a review verdict on the candidate: it does not block a separately authorized
next stage, and it does not authorize merge. Nothing here asks for a rewrite, a new
framework, or a scope expansion — the architecture and the repair strategy stand. The
merge review's finding bodies remain frozen records and are not re-litigated.

**Finding numbers below (#1–#45) are this review's own.** References to the merge
review's findings are written `MR#N`.

**Severity.** *High* = a shipped contract breaks, a security boundary fails open, a
security gate's witness cannot fail, or an executed red sits in a required suite.
*Medium* = robustness and lifecycle defects, contract drift, bounded resource and
diagnostic defects, and witnesses that cannot discriminate their claimed outcome.
*Low* = accounting, allocation, stale-doc and wording defects.

**Evidence labels.** *executed* = a probe ran against unchanged production code and its
assertion fired, or a suite ran and failed; *source-established* = the defect is visible
in the code and was not driven. These are never blurred. Confidence is stated per finding.

---

## Findings at a glance

| # | Finding | Severity | Area | Status |
|---|---|---|---|---|
| 1 | §12 fail-closed repair incomplete: two of five gate-1 sites still skip | High | anchor | Open |
| 2 | Dialog re-insert across two locks can displace a live row (the `ice_pending()` drift MR#37-46 claims closed) | High | rtc | Open |
| 3 | The duplicate-open refusal path re-creates MR#25 (kills the survivor's receive half) | High | leaf | Open |
| 4 | Membership refusal rollback is keyed by channel, not by request | High | leaf | Open |
| 5 | `host.setState` publishes beside the open transaction through the public API | High | store | Open |
| 6 | Transition holds are unbounded in the answered-but-never-installing case | High | store | Open |
| 7 | B never cleans up its peer entry after A's forced direct loss (executed; pre-existing; unrecorded in the 130) | High | rtc | Open |
| 8 | Two pre-existing membership witnesses are red under `webrtc` (executed; branch-introduced; CI-visible) | High | anchor | Open |
| 9 | Transport-loss drops are fenced on an incarnation key that cannot express channel identity | High | leaf | Open |
| 10 | Duplicate-Offer `Ignored` leaks the bootstrap ingress reservation | Medium | rtc | Open |
| 11 | Endpoint-unresolvable refusals are counted nowhere | Medium | anchor | Open |
| 12 | `OfferAccepted`'s redacting `Debug` prints the full SDP (with `ice-ufrag`/`ice-pwd`) | Medium | leaf | Open |
| 13 | The SDK's browser-credential URL gate disagrees with WHATWG in the leak direction (residual of MR#15's class) | Medium | sdk | Open |
| 14 | The SDK invite nonce prints one field below the redaction (residual of MR#16's class) | Medium | sdk | Open |
| 15 | Audience-encode refusals escape as bare `RangeError` | Medium | store | Open |
| 16 | The admissible-value scan retains serialization residues | Medium | store | Open |
| 17 | Superseded audience waiters resolve late success | Medium | store | Open |
| 18 | The message-1 probe clobbers an existing provisional admission | Medium | leaf | Open |
| 19 | The frozen-tab probe's fence row compares against the probe's own write | Medium | tests | Open |
| 20 | §12 witness legs (a)–(d) cannot detect "counted the refusal and acted anyway" | Medium | tests | Open |
| 21 | Loopback order witness: stream leg window-limited; zero delivery is a skip | Medium | tests | Open |
| 22 | Duplicate-open witness cannot see the survivor's receive half | Medium | tests | Open |
| 23 | Handshake-sweep witness lacks a pre-deadline control | Medium | tests | Open |
| 24 | Membership-Ack witness never exercises correlation | Medium | tests | Open |
| 25 | Unknown-subprotocol arm pins neither the returned classification | Medium | tests | Open |
| 26 | The structural tripwires have constructed escapes; two cannot fail on their claim | Medium | tests | Open |
| 27 | Three store repairs ship with zero coverage — including the delta-feed re-authorization fence | Medium | store | Open |
| 28 | Dialog witnesses use f64-exact fixtures | Medium | tests | Open |
| 29 | The Resolution ledger cannot support "All 130 findings are closed" | Medium | mixed | Open |
| 30 | The MR#116 Go backpressure guard still passes under the exact −117 fold its commit names | Medium | tests | Open |
| 31 | `check-abi-commit.py`'s self-test over-claims "every rejection path fires" | Medium | ci | Open |
| 32 | Overflow-warn witness counts any `console.error` in the whole leg | Medium | tests | Open |
| 33 | The witness fixture sink still models the pre-fix drop | Medium | tests | Open |
| 34 | `check-abi-commit.py` cannot see multi-line extern-C signature changes | Medium | ci | Open |
| 35 | The five token pins still accept printed names and `<name>::sub`; the JS ABI pin was loosened | Medium | ci | Open |
| 36 | `on.push.paths` omits ten scripts this workflow executes | Medium | ci | Open |
| 37 | `check-witness-results.py --min` counts nameless testcases | Medium | ci | Open |
| 38 | The deck floor counts test attributes, not the tests the step can run — a deterministic false red | Medium | ci | Open |
| 39 | AGENTS.md's one-line fix for MR#130 is itself stale | Low | mixed | Open |
| 40 | TESTS.md + PERF_AUDIT counts are right; all seven ci.yml coordinates are review-commit numbering | Low | mixed | Open |
| 41 | dataforts.md's capacity claim is contradicted by `runtime.rs` | Low | mixed | Open |
| 42 | dataforts.md's C/cgo bullet names symbols that exist in no C header or Go source | Low | mixed | Open |
| 43 | Both release-note copies retain stale claims and phantom names | Low | mixed | Open |
| 44 | The rewritten store design doc declares an option that does not exist | Low | mixed | Open |
| 45 | The kyra suites' "landed VERBATIM" headers are now false | Low | tests | Open |
| 46 | `act` sends its request frame before its `capacity` refusal is observed — a budget-refused act still reaches the owner (surfaced during the repair pass) | Medium | store | Open |
| 47 | `setAudience` mutates `#desired` and clears the view before `encodeMessage` can throw (surfaced during the repair pass) | Low | store | Open |
| 48 | `cancelWaiter`'s fence fires only in state `installing`; a last cancellation during `joining` does not fence (surfaced during the repair pass) | Low | store | Open |
| 49 | A refused provisional stream allocation strands that stream id — the next frame parks behind a sequence that never comes (surfaced during the repair pass) | Medium | anchor | Open |
| 50 | Gate 4 keys on the ADJACENT sender, not the announcement's ORIGIN — a provisional peer's announcement relayed by an admitted third party is ingested (surfaced during the repair pass; owner adjudication) | Medium | anchor | Open |
| 51 | A send half that gives up in two waves reports its death twice — duplicate reset, retire and count (surfaced during verification) | Medium | leaf | Closed |
| 52 | The browser node-id-spelling witness failed at the engine-marshaling layer on both engines — root cause: a stale JS-number dialog literal in the harness page panics wasm-bindgen's string marshaling before any parser runs (surfaced at the merge gate) | Medium | tests | Closed |
| 53 | The Windows security step ran under pwsh with bash syntax — it failed before executing a single test (surfaced at the merge gate) | Medium | ci | Closed |
| 54 | Two leader-supersession witnesses failed in the wasm runner — root cause: `stand_down` recycled the failed-promotion recovery, re-attaching the supersession away and re-taking the lock (surfaced at the merge gate) | High | leaf | Closed |
| 55 | Three wasm-lib witnesses fail on this host's runner — invariant under every fix of this pass (control-proven) — adjudicated at `8dfcf1d56` as three real production defects in the attempt/establishment/session lifecycle (the ICE-classification and harness-timing candidates were eliminated when the trio also failed on Linux, 20/23 identical) (surfaced during the #54 round) | Medium | leaf | Closed |
| 56 | The §12 leg-(a) positive control sampled the TOFU pin at a fixed instant after the send while the announcement ingest installs key and pin through its own async transition — a witness that cannot fail reliably (green at `68e2c19b0`, red at `5e0e69cdf`, zero net-crate changes between) | Medium | tests | Closed |

### Claims of the repair pass, contradicted

| claim | verdict | finding |
|---|---|---|
| "All 130 findings are closed" | CONTRADICTED — ledger arithmetic (4/5/6/29/30/110 named twice; 120/121/126 never named; `fc1503437` cited nowhere) and the incomplete closures below | #29 |
| f52edc4cf: "§12 gates 1/3/4 now fail closed … all three gate families" | CONTRADICTED — two of five gate-1 sites still skip | #1 |
| 4be9d419f: duplicate-Offer idempotence closes the `ice_pending()` drift | CONTRADICTED — the completion-owner re-insert re-creates it in a two-lock window | #2 |
| 67c32d25c: "closing one kills its sibling's receive half" is fixed | CONTRADICTED — the refusal path itself kills the survivor's receive half | #3 |
| 89c47ead7: drop_session "fenced … a predecessor's late close cannot remove a successor's entry" | CONTRADICTED at the system level — the node-level fence is real and witnessed, but the wasm wiring feeds it the successor's number and the named trigger (`closed`) files nothing | #9 |
| f2827a081: "either commits one revision or discards everything … through the public API" | CONTRADICTED — `host.setState` emits beside the transaction | #5 |
| f2827a081: "all encode-failure paths" wrap as `StoreError`; "the interval [1557, 5676]" | CONTRADICTED — aud-encodes bare; the true broken window is [1560, 5675] (the code's boundary is exact; the prose is off by 3/1) | #15 |
| 36cb4c0d6: tripwire escapes closed; "an implementation that forwards publish/call/subscribe must name its node" | CONTRADICTED — type-alias forwarding, neutral-named `Vec<u8>`, lowercase alias, exempt-module `web_sys`, TOML single-quote rename (the first two constructed independently by two review lanes) | #26 |
| 78880312c: "Both new values are re-measured in this tree (275 and 35)" | CONTRADICTED for 35 — the attribute count is not the count the step's own command reports (33 of 35 runnable; two are feature-gated). 275 holds | #38 |
| dea3a2c81: "make the backpressure guard discriminating" (for the −117 fold) | CONTRADICTED for the named defect — it guards a weaker closed-stream property | #30 |
| f2827a081 / 89f1ed53f: "No existing test changed or weakened" | CONTRADICTED as written (six existing tests edited); "weakened" HOLDS — every edit is strict or an honest inversion, none of the four weakenings | preserved credit |
| 67c32d25c: the frozen-tab probe is fixed | PARTLY — genuine system-state read-backs were added; the fence row still reads back the probe's own write | #19 |

---

## Findings

### High

**#1 — §12 fail-closed repair incomplete: two of five gate-1 sites still skip.**
`net/crates/net/src/adapter/net/mesh.rs:40357-40371` and `:39618-39623`.
Source-established (confidence 0.85). MR#5's required closure asked for the fail-closed
treatment on all five gate-1 sites; three were converted to
`let Some(endpoint) = … else { return; }` and two remain the `if let Some(…)` skip shape:
`forward_capability_announcement`'s sender gate and `forward_punch_ack`'s destination
gate. A sender entry vanishing between gate 4's `endpoint_of` and the flood gate's
lookup (two lookups with decode and ingest between them) skips the gate and re-floods
the announcement to every peer — the "megaphone" §12 F5 prohibits; the punch-ack skip
leaves entry-absent-at-check/present-at-get as a relay to a just-installed unenrolled
target. The hunk's own comment ("the two arms are now equivalent") and the Resolution
entry for MR#5 are contradicted by the sibling arm's shape.
*Impact boundary:* control flow and an incomplete repair are demonstrated; the primary
deterministic eviction-race scenario IS closed (gates 3/4 return before both ingest and
re-flood); no end-to-end exploit was driven.
*Required:* on every §12 gate-1/3/4 path an unresolvable endpoint refuses (and is
counted — see #11) instead of skipping: no announcement frame is flooded and no punch
relay is emitted for a sender or destination with no peer entry at any check on that
path.

**#2 — Dialog re-insert across two locks can displace a live row: the `ice_pending()`
drift MR#37-46 claims closed.**
`net/crates/net/src/adapter/net/rtc/engine.rs:63-69` (the new `DialogTable::insert` doc)
vs `net/crates/net/src/adapter/net/mesh.rs:27004-27024` (`spawn_dialog_completion`).
Source-established (0.70). The new doc asserts "Every in-tree caller short-circuits
before it can displace", but the completion owner's lost-claim path re-inserts the
successor row with a bare `table.insert(...)` across two separate
`rtc_dialogs.lock().await` acquisitions (remove at 27004-27006, insert at 27023). An
Offer for the same `(peer, dialog)` landing in that gap sees no row, passes the new
duplicate short-circuit (`engine.rs:194`), mints a fresh session, inserts its row and
moves `note_ice_attempted`; the completion owner's re-insert then silently replaces that
row — the displaced `Option` discarded — leaking the just-created ICE session and
stranding its attempt with no dialog row, so no terminal owner can ever claim it and
`ice_pending()` stays above zero for the process lifetime: the exact drift the
duplicate-Offer fix claims to close. The window is reachable: dialog ids in inbound
Offers are peer-chosen and reusable once a row dies.
*Impact boundary:* the interleaving's control flow only, no executed demonstration; the
non-interleaved re-insert and the claim-won path are correct.
*Required:* the restore cannot displace — re-insert only if the key is still vacant (or
remove+insert share one lock hold) — and any displaced entry is treated as a live
attempt (session closed and terminal-charged) rather than dropped.

**#3 — The duplicate-open refusal path re-creates MR#25.**
`net/crates/net/leaf/src/stream_ownership.rs:218` → `leader_session.rs:2131` →
`wasm.rs:4148-4163` → `node.rs:1978-1983`. Source-established (0.85). `adopt`'s new
veto correctly refuses a second open of one `(wire_id, peer, incarnation)`, but the
refusal's mandated disposition `backend.close(refusal.into_stream())` runs the
production close chain whose `rx_closed.insert` and `stream_kinds.remove` are keyed on
the **shared** `(incarnation, stream_id)` — destroying the survivor's node-level receive
half on a mere duplicate-open attempt that returns `Err` (subsequent arrivals counted
`StreamClosed` while `stream_send` keeps working). This is MR#25's exact stated failure
mode, now reachable from the refusal. The witness cannot see it: `SpyStream::close` is
an identity-private counter and the oracle inspects only the ownership map (#22).
*Impact boundary:* source tracing across four files; the ownership map stays consistent;
the destroyed state is the survivor's receive state.
*Required:* a refused open leaves the survivor's node-level receive state exactly as it
was — dispose of the refused wrapper without the shared close path, refuse before shared
teardown runs, or make `close_stream`'s effects handle-scoped/refcounted. Keep the
duplicate veto.

**#4 — Membership refusal rollback is keyed by channel, not by request.**
`net/crates/net/leaf/src/node.rs:3057-3063`. Source-established (0.70).
`handle_membership`'s refusal rollback removes `stream_kinds`, `rpc_reply_carriers` and
`reply_subscriptions` entries keyed by `(from, pending.stream_id)`, but the refusal
belongs to one nonce: when two registrations of one channel id overlap — `subscribe`
called twice for one name (it does not dedup), or a `publish` re-registering the id
between a subscribe and its Ack — and the earlier request's Ack is a refusal, the
rollback removes the LATER registration's claim. The anchor admits the surviving
Subscribe while the local claim is gone, so the channel's later payloads dispatch
`Dropped { UnknownCall }`; the same keying can retire an nRPC reply-carrier reservation
re-established after an earlier refusal. MR#14's witness holds at most one pending per
channel and cannot see this.
*Required:* a refusal rolls back only the registration its own request created (gate
removal on this nonce still being the id's latest request, or on the registration being
the one this nonce recorded); a newer request's claim or an admission must survive.

**#5 — `host.setState` publishes beside the open transaction through the public API.**
`net/crates/net/browser-ts/src/store/core.ts:247-256`, via `owner.commit`. The core fix
is correct and witnessed (both `applySnapshot` and `applyOwnerUpdate` join
`#transaction`), but the claim "either commits one revision or discards everything …
through the public API" is contradicted: `host.setState` inside a handler emits its
delta frame beside the transaction and rollback never retracts it — a shipped
staged-world delta and permanent silent divergence between the replicas and the owner's
document.
*Required:* no frame leaves for state the transaction may discard (defer
`host.setState` emissions to the transaction's commit).

**#6 — Transition holds are unbounded in the answered-but-never-installing case.**
`net/crates/net/browser-ts/src/store/join.ts:419-425`. Source-established. The
request-deadline exemption for transitions is right and the silence case is bounded
(`(MAX_JOIN_REASKS+1) × ASSEMBLY_DEADLINE_MS` ≈ 41 s to the ladder's fence) — but an
owner (or the lossy-chunk failure model this commit itself names, where the small `man`
survives and the multi-KB chunks do not) that answers every `resync`/`join` with a fresh
`man` whose assembly never completes restarts the 10 s `Assembly.openedAt` clock each
time: `#abandon → #resync → man` loops forever with no fence. Each stuck transition
holds one of 64 `MAX_OUTSTANDING` slots plus its waiter indefinitely; 64 of them turn
`act`/`setAudience` into `capacity` refusals until an unrelated installation, `fail()`
or `close()`. The design paragraph the commits cite promises "each call runs to its own
request deadline. A timed-out call removes only that waiter" — `joinStore` never calls
`cancelWaiter` and implements no per-call deadline.
*Required:* a named wall-clock bound on every transition, or the design's per-call
deadline plus `cancelWaiter`.

**#7 — B never cleans up its peer entry after A's forced direct loss.**
`net/crates/net/tests/rtc_signalling.rs:653` (the assertion) over the driver/mesh
teardown path. **Executed, deterministic** — the witness fails identically in the full
family run and in isolation at the pinned head, and identically at the merge base once
the branch's repaired assertion is applied there (the base's own assertion,
`is_none() || is_some()`, is true by construction and is the only reason the base is
green). The branch's witness repair (inside 4be9d419f, unmentioned in its message) is
correct discipline and exposed a real production lifecycle defect that appears nowhere
in the 130-finding ledger: after `rtc_driver().close(id_a)` on A, B's
`peer_endpoint(a)` stays `Some` past the 60 s failure budget — the repair comment's
premise ("the RTC driver reports a dead channel to the mesh on close … a healthy run
settles this in milliseconds") is falsified on this host — and the §9 sequence's
Phase-4 routed restoration never runs.
*Impact boundary:* Windows host, loopback-RTC harness, `--retries 0`; Linux/CI behavior
is unverified and no claim is made about it; bounded to the direct-loss teardown path.
*Required:* B's peer entry for the lost direct session is removed event-driven (or the
documented timeout path fires within budget), the §9 sequence passes end to end, and the
defect gets a ledger entry.

**#8 — Two pre-existing membership witnesses are red under `webrtc`, in a CI-visible
step.**
`net/crates/net/src/adapter/net/mesh.rs:34147-34161` (the gate) vs `mesh.rs:59775` and
`:59824` (the witnesses). **Executed** — both fail at the pinned head and both pass at
the merge base with the same feature set (branch-introduced). Mechanism: f52edc4cf's
§12 gate-3 fail-closed early return (`#[cfg(feature = "webrtc")]`) silently drops
membership messages from senders with no `ctx.peers` entry — the bare-peer shape both
witnesses model — so `accepted_subscribe_is_reprocessed_not_replayed` never sees its
subscribe land ("first subscribe lands") and
`retransmitted_subscribe_is_charged_to_the_auth_budget_once` never sees its first
rejection charge the budget (0 ≠ 1). CI's `Unit tests (UNIT_FEATURES + webrtc)` step
(ci.yml:5531-5535) compiles that gate and runs those tests. Both witnesses are
pre-existing and unmodified (`git log -S` empty over the branch): their named claims
(per-request charge dedupe; accepted-reprocess-not-replay) are legitimate properties
whose harnesses predate MR#5's intended refusal. The repair turned them red with no
ledger entry and no harness update — the same inversion playbook it correctly applied
to three other witnesses.
*Owner question (with recommendation):* if entry-less-but-live-session shapes are
production-legitimate (session-only/routed peers), the gate is over-broad and needs an
allow path for them. Recommendation: the narrower reading — the state is the documented
eviction race; update both harnesses to model resolvable peers and keep the claims —
but this is the implementer's adjudication.
*Required:* the suite is green under the webrtc graph with both claims still witnessed,
or the gate's contract change is recorded as a finding with its own witness.

**#9 — Transport-loss drops are fenced on an incarnation key that cannot express
channel identity.**
`net/crates/net/leaf/src/wasm.rs:998-1007` and `:893-898`, vs `rtc.rs:340, 377-379`.
Source-established (0.65). `channel_installations` is a single-slot map per peer,
overwritten at each `direct_installed`, while the loss reports it is paired with
(`take_ice_failures`) are keyed by peer alone with no channel identity. The harvest
therefore fences against whichever incarnation was installed LAST, not "the incarnation
THAT channel's handshake installed" its comment claims: a predecessor channel's
`disconnected`→`failed` report still queued after a successor channel's handshake
installs (the re-attempt flow produces exactly this second `direct_installed` for one
peer) pops the SUCCESSOR's number, passes `drop_session_if_incarnation` (the node-level
fence at `node.rs:1377-1388` is real and separately witnessed) and drops the working
session, failing its in-flight calls `SessionLost`. The comment's named trigger — "a
predecessor channel's late close" — files nothing at all (`closed` maps to "not a loss",
`rtc.rs:822-826`): the fence is vacuous for the scenario its comment names and wrong
for the trigger that exists. The window is one tick (the drain runs only from `tick`),
and the standard auto-repair path consumes the report before installing — argued, not
observed.
*Required:* a loss report must be fenced on the incarnation (or channel slot) the
REPORTING channel installed — dropping a session whose installation postdates the report
must be impossible — with a witness that delivers a predecessor's report after a
successor's install and asserts the successor survives.

### Medium

**#10 — Duplicate-Offer `Ignored` leaks the bootstrap ingress reservation.**
`engine.rs:194-196` + `mesh.rs:26380-26428`. Source-established (0.80). The Offer arm
now answers `SignalOutcome::Ignored` for a live dialog id, and the native loop handles
it — but `accept_bootstrap_offer_keyed` charges `admit_signal_frame` FIRST under a fresh
per-attempt `budget_key`, so `admit` pushes a NEW dialog slot and its `match` releases
nothing on the `other =>` arm ("unexpected outcome for a bootstrap offer"). A retried or
duplicated browser Offer — same `(claimed_node_id, dialog)`, new attempt key — leaves a
`SenderState.dialogs` slot no terminal path will ever `end_dialog` (the expiry sweep's
`rtc_attempt_keys` lookup cannot see a key this path never inserted): one slot per
duplicate, pruned only by `forget` on session close, which a per-attempt key never
reaches. Trigger: an honest Offer retry after a lost HTTP response, or a replay.
*Impact boundary:* bounded per call (the caller's own per-attempt key) but unbounded in
count across repeats; it does not exhaust a victim peer's budget and does not touch the
`0x0D02` path.
*Required:* every outcome of a paired `admit_signal_frame` take either hands the
reservation to a named terminal owner or releases it before returning, so a duplicate
Offer leaves `open_dialogs(key)` at its pre-call value.

**#11 — Endpoint-unresolvable refusals are counted nowhere.**
`mesh.rs:34162-34164`. Source-established (0.60). The fail-closed conversions `return`
before the admission counters run, so a frame refused at the eviction race moves no
counter at all, while a policy refusal on the same arm is counted; on the source-keyed
gates the two cases share one counter. The accounting cannot attribute "endpoint
unresolvable" versus "policy refusal" in either direction — against `AdmissionRefusal`'s
own rule ("a refusal that is not counted is a refusal nobody can operate on") and
against this patch's own router.rs half, which added a counter for its silent
no-transport drop for exactly this reason.
*Required:* an unresolvable-endpoint refusal moves its own counter (or its own reason
label) on every gate family.

**#12 — `OfferAccepted`'s redacting `Debug` prints the full SDP (with `ice-ufrag`/
`ice-pwd`).**
`net/crates/net/leaf/src/bootstrap.rs:400-408`. Source-established (0.55). The new
redacting `Debug` removes `attempt_token` but formats the full SDP. Its own doc claims
"Same treatment, same reason, as `OfferRequest`/`OfferResponse` and `Credential`" — but
both named siblings print `sdp_bytes` (the length) instead of `sdp`
(`sdk/src/rtc_bootstrap.rs:382`, `:428`). The anchor's SDP answer carries
`a=ice-ufrag`/`a=ice-pwd`, the dialog's STUN short-term credentials: a holder can forge
binding requests that pass this dialog's MESSAGE-INTEGRITY — the same ICE-hijack
surface the redaction doc cites for `attempt_token`.
*Impact boundary:* no call site formats `OfferAccepted` today, so the exposure is latent
in the formatter contract — precisely the event the doc anticipates.
*Required:* `{:?}` of an `OfferAccepted` must not emit the SDP body or its ICE
credentials while still showing `dialog`, matching both sibling impls.

**#13 — The SDK's browser-credential URL gate disagrees with WHATWG in the leak
direction (residual of MR#15's class, outside the repaired files).**
`net/crates/net/sdk/src/bootstrap_credential.rs:576-596` (`check_browser_url`, called
from `from_bytes` at :507). Source-established (0.75). Host extraction is
`rest.split(['/', ':']).next()` — no `@`, `\`, `?`, `#` handling. `http://127.0.0.1:8080@evil.example/…`
and `http://localhost:@evil.example/…` read as loopback here while a WHATWG browser
routes to `evil.example` (the last `@` splits userinfo from host). The leaf gate fixed
by 3769ad697 (`is_loopback_bootstrap_url`) is exact — a 20-shape attack table and an
independent probe found no leak-direction divergence there — but this parallel copy of
the same policy (the type is `BrowserBootstrapCredential`; its doc cites the browser
secure-context exception) still carries MR#15's bug shape.
*Impact boundary:* source-established split semantics; no runtime probe. The
bearer-exfiltration consequence needs a consumer that transmits the credential to
`bootstrap_url`; the only in-tree production caller at this head is the listener's offer
parser (`sdk/src/rtc_bootstrap.rs:1129`), so this is a fail-open boundary, not a
demonstrated leak.
*Required:* one host matcher shared by both gates (or `is_loopback_bootstrap_url`
ported verbatim), with the host-not-prefix witness's attack shapes run against the SDK
gate.

**#14 — The SDK invite nonce prints one field below the redaction (residual of MR#16's
class, outside the repaired files).**
`net/crates/net/sdk/src/enrollment.rs:215-229` (`#[derive(Debug)] InviteToken` with
`pub nonce: [u8; 16]`) via `sdk/src/bootstrap_credential.rs:554`
(`.field("invite", &self.invite)` in the hand-written `BrowserBootstrapCredential`
`Debug`, whose doc claims "The PSK is the one field in this crate that must never be
printed"). `{:?}` of any SDK credential prints the proof-of-invite nonce directly below
`.field("psk", &"<redacted>")` — MR#16's exact shape, fixed in leaf's `Invite` by manual
redaction ("fixes Credential's and every other formatting site at once" holds within
leaf, not across the polyglot copies). Corroborated latent sibling: `Attempts`' derived
`Debug` over a token-keyed map (`sdk/src/rtc_bootstrap.rs:520-523`).
*Required:* a manual redacting `Debug` on `InviteToken` mirroring
`leaf/src/enroll.rs:149-161`, and a no-bearer-secret witness over the SDK credential
type.

**#15 — Audience-encode refusals escape as bare `RangeError`.**
`net/crates/net/browser-ts/src/store/join.ts:558-565`. Source-established. `act` and
`input` wrap encode refusals as `StoreError` with `cause` (both sites correct), but the
`aud`-carrying encodes (construction, `setAudience`, reconnect) do not — against
"Invalid data still throws StoreError". *Required:* every public-surface encode refusal
is `StoreError('invalid-data')`.

**#16 — The admissible-value scan retains serialization residues.**
`net/crates/net/browser-ts/src/store/chunker.ts:124-135`. Source-established. The named
NaN/Infinity/undefined class is closed with parity across act/in/res/delta payloads and
the snapshot document, but `toJSON`/`Map`/symbol-key transforms and a pre-scan
`TypeError` escape, and the scan runs after `stringify` rather than before. *Required:*
refuse as `StoreError` before serialization everything `stringify` would transform or
throw on.

**#17 — Superseded audience waiters resolve late success.**
`net/crates/net/browser-ts/src/store/join.ts:323-328`. Source-established. A superseded
`setAudience` resolves success over the superseding installation, contra "aborted, never
reported successful by a late callback". *Required:* resolve only on that transition's
installation; reject on supersession.

**#18 — The message-1 probe clobbers an existing provisional admission.**
`net/crates/net/leaf/src/wasm.rs:730-739`. Source-established (0.55). The new msg-1/msg-2
discriminator probes `accept_handshake` non-destructively on REFUSAL as claimed, but on
SUCCESS it inserts `provisional[peer]` (`node.rs:1122-1129`) — `HashMap::insert`
destroys an admission already parked for that peer — and the winner branch's
`retire_provisional` removes the probe's own entry, which cannot restore the destroyed
one. A later establishment proof finds no admission (`admit_establishment_proof`) and
the pair's session never installs on this side while the peer believes it established.
"Our pending is left exactly as it was" is true of `handshakes`, not `provisional`.
Trigger: provisional pending + is_handshaking + a further message 1 (the re-attempt
shape).
*Required:* the probe must leave `provisional` exactly as it found it (park the probe's
admission in a scratch slot, or refuse to probe when an admission exists), with a
witness that parks an admission, drives the racing message 1, and asserts the parked
admission still verifies its proof.

**#19 — The frozen-tab probe's fence row compares against the probe's own write.**
`net/crates/net/leaf/tests/leader_e2e/frozen_tab_probe.mjs:198-201`. Source-established
(0.80). The repaired probe's read-backs are genuine where they matter — freeze/resume
verdicts gate on `Page.getWebLifecycleState` read off the page with an INCONCLUSIVE
gate, and the tab's belief is read from the tab's own `window.myGeneration` — but step 5
has the probe perform "the successor's act" (`window.nextGeneration()`) and then read
that same store back one line later (`window.currentGeneration()`). With no writer in
between, the compared values agree by construction, so `resumedTabIsStale` cannot fail
on staleness: the probe both causes and measures the delta.
*Required:* the store move the fence compares against must be produced by something
other than the fence row's own probe step (e.g. an independent writer's move observed
post-hoc), or the row must claim only what it measures (tab-side bookkeeping).

**#20 — §12 witness legs (a)–(d) cannot detect "counted the refusal and acted anyway".**
`net/crates/net/tests/rtc_admission.rs:253-262`. Source-established (0.85). The
absence checks cover only leg (e): legs (a)–(d)'s named actions move neither
`ice_pending()` nor `admission_promoted()` (an ingested announcement installs routes
and the TOFU pin; an acted-on Subscribe reaches the roster; an acted-on call is
delivered; an acted-on transit probe is relayed), so "counted the refusal and acted"
stays green on all four; legs (b) and (c) still call the pure gate predicate and never
drive the anchor, so a dispatch-level skip of gates 3/5 — MR#5's exact class — remains
invisible, which MR#6's required closure asked for. Leg (e) is fixed as claimed (strict
`>` via `wait_for` plus absence) and is discriminating.
*Required:* each leg's named action gets its own negative observable asserted after it
(ingest/pin state for (a), roster/channel state for (b), service-visible delivery for
(c), third-party receipt for (d)), and legs (b)/(c) drive the anchor's dispatch.

**#21 — Loopback order witness: the stream leg is window-limited and skips on zero
delivery.**
`net/crates/net/tests/rtc_loopback.rs:205-214`. Source-established (0.80). The batch
leg is now exact identity (duplicates and reorderings fail it). The stream leg's
exactness is window-limited: the drain loop exits when the batch count is reached, so
stream events still in flight are never drained and a per-source reorder confined to
that undrained tail fails nothing; and `if !stream_indices.is_empty()` skips the
assertion entirely when no stream event was drained, making "each input's own sequence
exactly" vacuous for that input.
*Required:* the stream input's collection window is bounded by that input's own
completion (or the deadline), and zero stream delivery is an assertion failure rather
than a skip.

**#22 — Duplicate-open witness cannot see the survivor's receive half.**
`net/crates/net/leaf/src/stream_ownership.rs:811-815`. Source-established (0.80). The
oracle checks the ownership map and a `SpyStream::close` counter — an identity-private
object that cannot model the production close chain's shared receive teardown. "The
survivor is untouched" therefore holds in the model whether or not the refused close
kills the survivor's receive half — the exact defect the test guards (#3).
*Required:* an oracle whose close reaches shared receive state, or direct
post-conditions on `rx_closed`/`stream_kinds` after the refused open.

**#23 — Handshake-sweep witness lacks a pre-deadline control.**
`net/crates/net/leaf/src/node.rs:6395-6404`. Source-established (0.75). One tick at
deadline+1 s cannot distinguish a deadline sweep from an unconditional reaper (which
would kill every in-flight handshake on unrelated traffic). *Required:*
`tick(clock::now())` with `assert!(is_handshaking(…))` before the post-deadline tick.

**#24 — Membership-Ack witness never exercises correlation.**
`net/crates/net/leaf/src/node.rs:6546-6555`. Source-established (0.80). Each phase
injects one Ack carrying the only live nonce, so a nonce-blind implementation passes;
the correlation the claim names is never exercised. *Required:* an Ack with a foreign
nonce and one from the wrong peer must leave the pending's claim, counters and events
unmoved.

**#25 — The unknown-subprotocol arm pins neither the returned classification.**
`net/crates/net/leaf/src/dispatch.rs:298-305`. Source-established (0.70). `is_err()`
plus the counter passes even if the returned `Err` named `Unparsable` while the counter
moved `UnknownSubprotocol` — and `handle_event` publishes the returned value as the
event reason, mislabelling the event exactly as MR#58 describes. *Required:*
`assert_eq!(dispatch_event(…), Err(DropReason::UnknownSubprotocol))`.

**#26 — The structural tripwires have constructed escapes; two of them cannot fail on
their claim.**
`net/crates/net/leaf/tests/control_plane_boundary.rs` and `dependency_boundary.rs`.
Source-established (two lanes constructed the same escapes independently). The literal
name-level assertions can fail, but the property-level claims their names make have
documented escape classes: (1) root-module type-alias forwarding
(`pub(crate) type PeerTable = crate::node::LeafNode;` in `lib.rs`, never scanned, +
`table.publish(…)`) defeats `no_control_plane_implementation_touches_the_data_path` —
"an implementation that forwards publish/call/subscribe must name its node, one way or
the other" is false; (2) a neutral-named `Vec<u8>` parameter smuggles net-packet bytes
past `no_net_packet_type_is_nameable_in_the_trait` — a name-level ban with a
property-level hole. Both tripwires, as named, cannot fail on the properties they
claim. Further passing escapes: a lowercase module-level alias
(`type h = ::crate::session::LeafSession;`) defeats the `::`-ban plus vocabulary
closure; `web_sys::WebSocket::new("wss://…")` from `leader_session.rs`/`wasm_witnesses.rs`
(or any fetch in `storage.rs`, `rtc.rs`, `src/<subdir>/`) defeats the helper-module HTTP
scan (non-recursive `read_dir`, four exemptions incl. `storage.rs` which
`dependency_boundary.rs` simultaneously allow-lists for `web_sys`, and no `checked`
floor); TOML single quotes (`rt = { package = 'tokio' }`) defeat the renamed-dependency
scan. `NoAnchor` verdict: narrowed, not replaced — the ZST size assert remains a compile
marker that cannot see parameter types, while the new signature scan (no `::`,
capitalized tokens ⊆ vocabulary) does fail on named parameter types; the lowercase alias
passes all scans.
*Required:* enforce the property the names claim (type-normalized scanning including
aliases and non-capitalized spellings; a recursive, floored module walk with every
exempt module fenced by an asserted guard; a TOML-aware dependency parse) — or narrow
the claims to name-level and say so in the test names.

**#27 — Three store repairs ship with zero coverage — including the security fence.**
`net/crates/net/browser-ts/src/store/owner.ts:302-312` (the delta-feed
re-authorization, b0f6ae11c's entire point), `host.ts:327-336` (the failed-open cache
clears), `assembly.ts:335-344` (the non-destructive open refusal). The repairs are
correct as written, but no witness can fail on their regression. *Required:* a witness
per repair; the re-authorize fence must fail on a revoked-peer regression.

**#28 — Dialog witnesses use f64-exact fixtures.**
`net/crates/net/browser-ts/test/node.test.ts:285-294` and `test/leader.test.ts:167-169`.
Source-established (0.70). The added comment names the property as the `as f64 as u64`
rounding class, but the fixtures (`'…0003'`, `'…0004'`) are exactly f64-representable
and re-encode identically through any `Number`→re-pad hop — the assertion is green while
the named property is false for a spelling-preserving numeric re-encode. (The seam
itself is verified lossless end to end; this is a fixture gap only.) *Required:* one
fixture whose u64 is not f64-representable (e.g. `'0011223344556677'`) asserted
verbatim end to end — the discipline the sibling `generation()` fixture already applies.

**#29 — The Resolution ledger cannot support "All 130 findings are closed".**
`docs/internal/misc/WEBRTC_TRANSPORT_MERGE_REVIEW_2026_09_22.md` (the live index and
Resolution sections; the frozen finding bodies are untouched and excluded). Findings
4/5/6/29/30/110 are named twice in the closure rows; **120, 121 and 126 are never
named**; `fc1503437` is cited nowhere though its documentation closures are claimed;
and the rows numbered "29"/"30" carry findings 120/121's titles (a numbering
collision). One glance/Resolution status mismatch remains (the stale-floors row reads
Closed in the glance and Partly closed in the Resolution). *Required:* the tally
reconciles exactly — closed + deferred + noted = 130, each finding named once with its
commit — and the glance column matches the Resolution rows in both directions.

**#30 — The MR#116 Go backpressure guard still passes under the exact −117 fold its
commit names.**
`go/stream_close_test.go:122-131`. Source-established (0.85). The repaired guard is
discriminating for "a closed stream must not be swallowed or misclassified as
backpressure" — a real property (executed green in this review) — but the defect the
commit message names ("a retry wrapper folding −117 into the backpressure loop stayed
green") still stays green: no superseded (−117) error is ever sent through the wrapper
(the deferred comment at :125-131 concedes a displaced session cannot be built from the
Go surface) and `SendBlocking` is never called at all. A wrapper that retries
`ErrSessionSuperseded` forever, or folds it into the backpressure loop, passes this test
unchanged. The headline claim is therefore contradicted for its named defect.
*Required:* a −117-classified terminal surfaces from `SendWithRetry` AND `SendBlocking`
(or the comment and claim are narrowed to the closed-stream property actually driven).

**#31 — `check-abi-commit.py`'s self-test over-claims "every rejection path fires".**
`.github/scripts/check-abi-commit.py:343-344`. Source-established (0.70). The
extern-C case asserts detection (`extern_c and not consts`) and never calls
`violations()` with an extern-C-only trigger, so a regression such as
`if not consts: return []` — silently ignoring extern-C changes — would leave the whole
self-test green. *Required:* the self-test fails when `violations(set(), True, {ffi-file})`
returns empty.

**#32 — The overflow-warn witness counts any `console.error` in the whole leg.**
`net/crates/net/leaf/src/wasm_witnesses.rs:1590-1599`. Source-established (0.55). The
"dropped with a warning" claim counts every `console.error` across the entire second
leg (the shim is armed before `pair()`/`install_session()` and read only at the end), so
an unrelated complaint satisfies `warned >= 1` while the overflow drop warns nothing —
the monotone-counter shape the witness doctrine names. The bounded-retention half of the
claim is proven exactly. *Required:* the count attributable to the overflow itself
(shim armed only around the overflow deliveries, or assert the drop's own text) so
removing the warn at the drop site turns the assertion red.

**#33 — The witness fixture sink still models the pre-fix drop.**
`net/crates/net/leaf/src/wasm_witnesses.rs:139-144`. Source-established (0.60). The
claimed fix (the inbound sink defers a datagram on borrow conflict instead of dropping
it) lives in production at `wasm.rs:1804-1825`, but the fixture sink edited in the same
patch still pushes inside `try_borrow_mut` — modeling the pre-fix drop. No witness
drives delivery through a borrow conflict, so the claimed repair cannot fail and the
fixture would mask its regression. *Required:* the fixture sink matches production
(queue unconditionally, deliver opportunistically) and a witness queues a datagram while
the node cell is held and asserts delivery rather than loss.

**#34 — `check-abi-commit.py` cannot see multi-line extern-C signature changes.**
`.github/scripts/check-abi-commit.py:123-124`. Source-established (0.80). The trigger
is line-local (`_EXTERN_C.search(body)` fires only when a CHANGED line itself contains
`extern "C"`; the `const`/`#define`/enum patterns match definition lines only). An
ABI-breaking edit to a parameter on a continuation line of a multi-line `extern "C"`
signature (e.g. `len: u32` → `len: u64`) changes no matched line and `violations()`
returns `[]` — the commit passes despite being exactly the "extern-C change" the rule
and its commit message name. *Required:* a commit that changes an extern-C signature —
including continuation lines — or a `NET_*` definition without Rust + both headers + a
Go ABI test in the same commit exits 1.

**#35 — The five token pins still accept printed names and `<name>::sub`; the JS ABI
pin was loosened.**
`.github/workflows/ci.yml:332` (identically at :381, :480, :525, :6298, :7043).
**Executed** regex matrix (same semantics as the grep used). The token regex
`(^|[^A-Za-z0-9_])<name>([^A-Za-z0-9_]|$)` rejects `<name>_extra`, `x<name>`, `<name>x`
and `pre<name>post` (the real win of the anchoring commit) but is satisfied by any line
that merely PRINTS the witness name as a token and by `test <name>::helper … ok`, where
the pinned name is a module segment, not the test. The sibling line-anchors
(`^test <name> \.\.\. ok$`, `^RTCB PASS <name> `, `^--- PASS: <name> `) reject both
spoofs — per-format anchors are the known-good shape. The JS ABI leg moved backwards:
the old `grep -q "\"$required\""` required the name in quotes — a strictly tighter
predicate the JSON log always satisfies — so that pin now accepts bare prints the
previous grep rejected: a pin **weakened** by the "anchoring" commit.
*Impact boundary:* proven as residual capacity; no current pin is shown spoofed in-tree.
*Required:* no line that is not the witness's own run record satisfies a pin.

**#36 — `on.push.paths` omits ten scripts this workflow executes.**
`.github/workflows/ci.yml:34-36`. Source-established (0.85). The new comment states the
rule — "editing one of these must re-run the workflow it gates, or a loosened predicate
lands green" — and three checkers are added, but ten other scripts the workflow executes
are absent from `on.push.paths`: `check-ffi-exports.py` (edited in this very patch;
invoked at ci.yml:4060), `check-callback-buffer-ownership.py` (:3988),
`check-rpc-abi-parity.py` (:4077), `go-test-with-native-stacks.sh` (:4153),
`check-script-permissions.py` (:3540), `check-npm-peer-range.py` (:3545),
`check-npm-platform-packages.py` (:3560), `check-ts-consumer.sh` (:3624),
`check-skill-example-ts.sh` (:3637), `check-python-wheel-features.py` (:3670). Editing
any of them triggers no run at all — exactly the failure the comment describes.
*Required:* editing any file a ci.yml step executes re-runs the workflow (listed entries
or a `.github/scripts/**` glob).

**#37 — `check-witness-results.py --min` counts nameless testcases.**
`.github/scripts/check-witness-results.py:120-122`. Source-established (0.65).
`counted = len(cases)` includes bare `<testcase/>` elements (bucketed under the empty
name), so a doctored or replayed JUnit artifact padded with nameless cases satisfies a
floor while naming zero witnesses — the same element-padding class the docstring's
threat model claims to close ("a doctored attribute cannot satisfy it"). Real nextest
output never carries nameless cases, which bounds the exposure. *Required:* only named,
executed testcases count toward `--min` (or empty-named cases are rejected outright).

**#38 — The deck floor counts test attributes, not the tests the step can run: a
deterministic false red.**
`.github/workflows/ci.yml` ("Run Deck unit tests": `cargo test -p net-deck --bins` with
`grep -c '^test .* \.\.\. ok$'` against floor 35) vs `net/crates/net/deck/Cargo.toml`,
`deck/src/main.rs:23-24`, `deck/src/app.rs:4133-4135`. **Executed + source-established.**
78880312c raised the deck floor 31 → 35 from an attribute scan of `deck/src` (the
commit itself describes "the attribute scan used here" as its measurement basis) — but
the step counts the tests that RUN under `cargo test -p net-deck --bins` with default
features, and two of the 35 are feature-gated out of that command by design:
`the_rollup_the_anchor_column_renders_carries_an_ingested_anchor` is
`#[cfg(feature = "webrtc")]` (`app.rs:4133-4135`) and `demo_boots_and_logs_appear` lives
in `mod demo`, which is `#[cfg(feature = "demo")]` (`main.rs:23-24`) — and net-deck's
`[features] default = []`, with `webrtc` documented "Off by default" and `demo`
"Dev-only; never ship to crates.io as a default feature" (`deck/Cargo.toml`).
Executed: `cargo test -p net-deck --bins` runs and passes exactly **33** tests, so
`33 < 35` false-reds deterministically (the step's error reads `net-deck ran N tests,
floor is 35`), matching the red CI job; the old floor 31 was green at 33.
*Impact boundary:* the failure direction is a false red — loud, not silent — so no
coverage is lost; but the gate cannot go green on this step, and the floor's purpose
(catching a suite that silently shrinks) is not served by a number the pinned command
cannot reach. Wire's `MIN=275` is unaffected and sound (275 attributes, 0 `#[ignore]`,
no feature-gated modules — and the step counts run lines of that same set).
*Required:* the floor equals the number of tests the pinned command actually runs (33
today), measured on the runner's own counting shape with the step's own feature set; a
feature-gated test either runs in the step (features added deliberately) or is excluded
from the floor deliberately and named. "Floors follow measured counts" means the counts
the gate itself measures.

### Low

**#39 — AGENTS.md's one-line fix for MR#130 is itself stale.** `AGENTS.md` cites
"MIN=93 at ci.yml:241"; the truth at this head is ci.yml:247. A stale-coordinate fix
introduced a fresh stale coordinate. *Required:* the citation matches the merged tree.

**#40 — TESTS.md + PERF_AUDIT counts are right; all seven ci.yml coordinates are
review-commit numbering.** The 16 per-name fan-out loops and five integration families
verify exactly against ci.yml, but every cited coordinate is off by +6..+9 against the
merged tree (e.g. TESTS.md's "ci.yml:2554-2558" is truth :2563-2569). *Required:*
coordinates counted against the merged tree.

**#41 — dataforts.md's capacity claim is contradicted by `runtime.rs`.**
`.claude/skills/net-event-bus/dataforts.md:89` ("capacity mixes cache-full with
budget-exhausted") vs `runtime.rs:793-795` and its witness (`BandwidthExhausted` bumps
`bandwidth`). *Required:* the claim matches the reason labels the runtime emits.

**#42 — dataforts.md's C/cgo bullet names symbols that exist in no C header or Go
source.** `NetBlobAdapterVtable` and `NET_ERR_BLOB_BACKEND` are Rust-FFI-only; the
anti-phantom-constant fix introduced new phantom names in prose. *Required:* the bullet
names only symbols reachable from the C headers / Go surface, or says it is describing
the Rust FFI layer.

**#43 — Both release-note copies retain stale claims and phantom names.**
`net/crates/net/docs/releases/RELEASE_v0.15_REBEL_YELL.md` and the web mirror: the
Small URI is still described as "length-prefixed", plus three phantom names
(`BlobRef::MAX_SIZE`, `RedexFileConfig::blob_max_size`,
`dataforts_greedy_admit_throttled_bandwidth_total`). (The two copies are otherwise in
lockstep — no drift beyond the web frontmatter and link transform.) *Required:* both
copies corrected together.

**#44 — The rewritten store design doc declares an option that does not exist.**
`docs/internal/plans/BROWSER_GAME_STORE_API_DESIGN.md` (hostStore contract: `key: string`)
vs `HostStoreOptions { store?: string }` and no `key` (the key is a per-caller join
token) — the rewrite's own defect class. *Required:* the documented surface matches the
shipped types.

**#45 — The kyra suites' "landed VERBATIM" headers are now false.**
`net/crates/net/leaf/tests/kyra_followup.rs:4-20`, `kyra_round3_review.rs:1-6`,
`kyra_round4_parent.rs:3-8` vs their 36cb4c0d6 edits (which rewrote bodies, added
helpers and changed assertions inside files whose headers state "No name, helper,
assertion or message changed"). The provenance claim these files use as their authority
is falsified. *Required:* each header's provenance claims match the file's actual
authorship state, via declared exceptions in the established style.

### Surfaced during the repair pass

Five pre-existing defects the repair lanes found while fixing #1–#45. They are
outside the original review's file sets and were not adjudicated in it; each is
recorded here with the evidence its finder produced.

**#46 — `act` sends its request frame before its `capacity` refusal is
observed.** `net/crates/net/browser-ts/src/store/join.ts` (`act`).
Source-established (0.65; surfaced by the store lane). `await send([frame])`
runs before the `correlate` rejection is observed, so a budget-refused `act`
still reaches the owner while the caller receives `capacity` — an action can
execute remotely whose acceptance was refused locally. *Impact boundary:* the
ordering is read at source; the owner-side execution consequence was not driven
here. *Required:* `act` observes its correlation verdict before sending (a
refused act never reaches the owner), or the intended semantics is documented
and the owner's execution of unaccepted acts is adjudicated.

**#47 — `setAudience` mutates `#desired` and clears the view before
`encodeMessage` can throw.** `net/crates/net/browser-ts/src/store/join.ts`
(`StoreReplica.setAudience`). Source-established (0.6; store lane). The
mutation precedes the encode that may refuse — the #15 typed-refusal path is
reachable precisely because of this order — while the clear-the-view-immediately
doctrine (§1.7) makes part of the ordering deliberate. *Required:* the mutation
follows the encode (with §1.7 restated), or the doctrine names the
mutation-on-refusal consequence.

**#48 — `cancelWaiter`'s fence fires only in state `installing`.**
`net/crates/net/browser-ts/src/store/replica.ts:363-368`. Source-established
(0.7; store lane). The last cancellation during `joining` does not fence. The
witnessed `#abandon → #resync → man` loop lives in `installing`, where the fence
does fire. *Required:* the fence covers `joining` too, or the claim names the
state scope.

**#49 — A refused provisional stream allocation strands that stream id.**
`net/crates/net/src/adapter/net/mesh.rs:30980-30982`
(`account_inbound_stream_packet`). Source-established (0.6; net lane; observed
in-run for ≥ 10 s). A new stream from a provisional sender refused at the R3
allocation (`MAX_PROVISIONAL_STREAMS = 2`) never consumes the sender's sequence
for that stream id at the receiver; the next frame on the same id parks in
`hold_inbound_in_order` behind a sequence that will never come — persisting even
after the sender is promoted. *Required:* the refused allocation consumes or
advances the receive frontier (or records the refusal) so a later frame on that
id is not parked behind a ghost sequence, witnessed by a
provisional-refusal-then-promotion witness.

**#50 — Gate 4 keys on the ADJACENT sender, not the announcement's ORIGIN.**
`net/crates/net/src/adapter/net/mesh.rs` (gate 4 / announcement ingest).
Source-established + in-run proof (0.7; net lane). A provisional peer's signed
announcement relayed (hop > 0) by an admitted third party IS ingested by the
anchor, installing its announced Noise key and TOFU pin (it poisoned §12
witness leg (a)'s ingest negative in-run until the third party moved to leg
(d)). Whether §12's "a provisional peer's announcement is neither ingested nor
flooded" should veto ingest by ORIGIN provisional-ness is an owner adjudication.
*Impact boundary:* ingest proven through the relayed hop>0 path; no pin-abuse
exploit driven. *Required:* the ingest rule names its keying (adjacent vs
origin) and a witness pins whichever behavior is adjudicated.

**#51 — A send half that gives up in two waves reports its death twice.**
`net/crates/net/leaf/src/node.rs` (`drive_reliability`'s send-half terminal; the
wire's take-and-clear `failed` flag at `wire/src/reliability.rs:1353-1354`).
Source-established + executed (0.85; surfaced by the integrated-tree verification
run, fixed by the leaf-core lane in `ba7b0aef9`). The terminal had no latch: one
`StreamFailed { RetransmitsExhausted }` per give-up pass, and the wire's flag is
re-set by every later give-up — so a stream whose descriptors are born over a span
wider than one sweep cadence (routine under CPU load) splits its retry ladder and
exhausts in two waves, producing a duplicate terminal event, a duplicate
`StreamReset`, a duplicate `retire_stream_stamps` and a duplicate count (executed:
the consumer received 9 terminal reports over 8 owners, failing
`an_exhausted_stream_returns_its_stamps_so_a_fresh_id_is_admitted`'s precondition
intermittently). *Impact boundary:* the duplicate reporting is executed; the
pre-fix failure reproduced under load and once in the integrated run. *Required —*
met: the first flag declares the terminal once (reset, retire, count and event
exactly once; later waves are stragglers), witnessed by
`a_send_half_that_gives_up_in_two_waves_is_reported_once` with a constructed
two-wave split (inverse: unlatched red `left: 2, right: 1` on "the consumer is
told the send half died ONCE"; restored green).

**#52 — The browser node-id-spelling witness fails at the engine-marshaling
layer on both engines.**
`net/crates/net/tests/rtc_browser/runner/src/stage6.rs`
(`stage6_one_node_id_spelling_across_both_signalling_surfaces`), over the
page-facing `node.signal(peer, …)` surfaces. Source-established + CI-executed
(surfaced at the merge gate; pre-existing — the same three browser steps failed
at `91ae3747`, and the witness and its `parse_peer_id` fix live outside this
repair's files). The witness drives five node-id spellings and expects two to
parse and three to be refused "by name"; observed instead: on Firefox every
spelling — including `"nine"` — reports `index out of bounds` (the call dies in
the wasm/JS argument marshaling before any parser runs), and on Chromium every
spelling returns a silent `false` with empty reason. Two engines, two symptoms,
one fact: the string never reaches `parse_peer_id`. The witness's own text
records that both surfaces were moved onto `parse_peer_id` to close a
pre-existing spelling split — the fix evidently does not reach the boundary
layer where the argument is marshaled. *Impact boundary:* the failure is
executed in CI's browser matrix only (the harness needs real engines); no claim
about which side of the boundary is at fault. *Required:* node-id strings reach
`parse_peer_id` intact on both `LeafNode.signal` and the leader-proxied
`MeshSession.signal` in both engines (marshal the peer id as a string or
BigInt-safe type end to end), with the five-spelling witness green on Chromium
and Firefox.

**#53 — The Windows security step ran under pwsh with bash syntax.**
`.github/workflows/ci.yml:5184` ("Org + authority unit tests"). Source-established
+ CI-executed (surfaced at the merge gate; pre-existing wiring). The step's `run:`
block opens with `set -euo pipefail`, but a Windows `run:` defaults to pwsh, which
parses that as `Set-Variable -euo …` — "A parameter cannot be found that matches
parameter name 'euo'" — so the step exited 1 before executing a single test. The
tests it wraps are healthy: the same two nextest invocations pass locally on this
Windows host (467 + 9, all green, `--features net --lib`). *Required —* met: the
step declares `shell: bash` (this pass), and the two nextest invocations run.

**#54 — Two leader-supersession witnesses fail in the wasm runner.**
`net/crates/net/leaf/tests/wasm_leader.rs:1508` and `:1956`, over
`leader_session`'s supersession semantics. **Executed** (CI's "wasm test runner
(headless Chromium)" step at `68e2c19b0`: 27 passed, 2 failed; surfaced at the
merge gate — this step never ran earlier in the branch's life because the Leaf
job's `Formatting` step failed first, so the failures were masked until the fmt
sweep unmasked the step chain). Both are pinned roster witnesses (the
`LEADER_WITNESSES` names, predating this repair):

1. `a_superseded_session_is_refused_by_the_leader_and_by_storage` —
   `expect_err` on the proxied send of a superseded session received
   `Text("[]")` (a successful round trip) instead of the required refusal
   ("a superseded session must refuse").
2. `a_leader_revalidates_its_generation_against_the_store_and_stands_down` —
   after the run's own logs ("the store records generation 2; this tab holds 1",
   "generation 1 was superseded by 2; standing down", "generation 1 was fenced"),
   `role()` still reads `Leader` where the witness requires `Follower`
   ("a leader whose recorded generation moved is not the leader").

Both sit in the leader-lifecycle surface the repair's `stand_down` →
`resume_as_follower` rework (67c32d25c) changed. That rework's credited
property — no zombie tab after `stand_down` — stands (the zombie witnesses pass
both host-side and in this run), but these two show the rework also altered
supersession semantics the pre-existing witnesses pin: this **qualifies the
`stand_down` credit above**. Whether the two failures are deterministic
regressions from the rework or racy under the headless runner cannot be settled
on the reviewing host — the wasm runner needs a browser and is unavailable here —
and `cargo test` carries no retry, so no flake absorption hides either way.
*Impact boundary:* two executed failures with their assertions quoted; the
regression-vs-race classification and the fix both need one round in the wasm
lane. *Required:* a superseded session's proxied send fails typed (its storage
write refused too), and a leader whose recorded generation moved reports
`Follower` after stand-down — both green in the wasm runner, with the
stand_down credit restated for whichever semantics is adjudicated.

**#55 — Three wasm-lib witnesses fail on this host's runner.**
`net/crates/net/leaf/src/wasm_witnesses.rs:1815`, `:706`, `:1571`
(`an_unnamed_open_resolves_to_the_anchor_not_peer_zero`,
`an_expired_attempts_deadline_settlement_cannot_charge_its_successor`,
`a_promotion_whose_attempt_was_superseded_is_credited_to_nobody`).
Source-established + executed locally (surfaced during the #54 round when the
local wasm runner was first enabled; 20 passed / 3 failed of 23). **Invariant
under every fix of this pass** — a full-revert control of the #54 fix's entire
behavioral surface yields the byte-identical 3-name red set, the names are
symbol-disjoint from both fix lanes' diffs, and one failure's shape is an ICE
failure-classification mismatch (`left: "failed" / right: "iceTimeout"`).
Candidates: engine-classification divergence (headless Chromium 141 on Windows
vs CI's Linux Chromium) or harness timing. CI's wasm step has never reached the
lib run on this branch (it died earlier in the step chain at `wasm_leader` until
this pass fixed that), so their Linux status is unknown. *Impact boundary:* the
invariance is control-proven; the engine-vs-defect classification is not settled
on this host. *Required:* classify the three on CI's engines — if they pass
there, record them as Windows-runner divergence with a skipped-with-reason
marker or a widened classification assertion that names both engine outcomes; if
they fail there too, they are regressions needing their own repair round.

**#56 — A positive control that cannot fail reliably.** The §12
leg-(a) positive control
(`every_denied_action_is_refused_at_its_named_gate_and_counted`) sampled the
TOFU pin at a fixed instant after the promote+send, while the announcement
ingest installs the announced key and the pin through its own async
transition: the code polled the key and then read the pin. Under CI load the
pin read raced its install — green at `68e2c19b0`, red at `5e0e69cdf`, zero
net-crate changes between the heads (208/208 locally on this host). A
witness whose green depends on a timing window is not a witness.

---

## Preserved credits

Do NOT rework the following; each was verified against code, and the two marked
(**inverse-proven**) additionally by mutation: apply the defect's inverse at the
production site, watch the named witness fail for its own stated reason, restore,
watch it pass.

- **Leaf `complete_handshake` / message-2 handling (89c47ead7)** — non-destructive
  reads; the superseded one-slot message-2 classifier; the 30 s handshake sweep added
  to `tick` without regressing the call-deadline/reassembly/provisional sweeps; the call
  slot released on the request-encode error path; the reply-carrier fence covering
  Stream AND Channel owners in both directions before any frame is queued; the
  membership-Ack nonce correlation mechanics; `DropReason`/overflow/replay
  classification. (**inverse-proven**:
  `a_bad_or_stale_message_2_leaves_the_live_handshake_in_flight` — mutation red at
  `node.rs:6354` "a failed read must take neither the pending nor the entry", restore
  green.) The snow `read_message` residual: the deferral is **honest** — named in the
  commit, the source and the ledger; containment verified (failed reads take nothing,
  entries bounded by the sweep, no session builds, nothing mis-routes); severity
  correctly bounded as per-attempt availability.
- **`applySnapshot` transaction join + the store witness batch (f2827a081)** — the
  core transaction rule now holds on both paths; all 11 branch-new store witnesses are
  discriminating with a complete would-fail mapping (none vacuous); "no test weakened"
  holds in substance (six existing tests edited, all strictly tightened or honest
  inversions; none of the four weakenings); `assertChunkingFits`'s boundary is exact
  (4113 = ⌈1048576/255⌉) even though the commit's prose interval is miscounted.
  (**inverse-proven:** the `applySnapshot` pair — mutation red on exactly the two mapped
  witnesses, restore green 32/32.)
- **Credential host gate (3769ad697)** — `is_loopback_bootstrap_url` mirrors the
  WHATWG split (authority delimited at `/ ? #` and backslash, userinfo to the LAST `@`,
  exact host set with case handling, bounded ports); a 20-shape attack table plus an
  independent probe found no leak-direction divergence; the host-not-prefix witness
  fails the old prefix match on seven shapes, each for the admission reason.
  Zeroization covers exactly psk/encoded/invite-nonce. `Invite`/`Credential` redaction
  correct. Dialog-0 reservation interop-safe (the listener numbers from 1).
- **Trickle/dialog lifecycle (3769ad697, 4be9d419f)** — per-socket pending queues
  flushed only by their own `on_open` (fenced by construction); `end_attempt` closes the
  socket it names; `retire_socket(token, gen)` generation-fenced (witness discriminating
  in both directions over real double 101 upgrades); ACME `key_matches_leaf` closes the
  permanent brick with both tear directions witnessed and a matching-pair positive
  control; the four-charge enrollment invariant exact on every path (reserve-then-charge
  with rollback; three incarnation-fenced terminal releases).
- **Reassembly/§12 core (4be9d419f, f52edc4cf)** — the charge-refusal
  `AbandonedGroup { charged: false }` accounting nets to zero per taken slot across
  accept/abandon/take_terminals/coalesce (every destruction path enumerated); the
  budget arithmetic verified (8 × 64,864 B = 518,912 B — a zero-margin but sufficient
  fit); duplicate Offer/Answer idempotent outside #2's window with the `ice_pending()`
  ledger correct on the non-racing paths; `on_batch` → `send_to_peer_node` (total
  `PeerAddr` match, order-independent); `has_username`'s conservative default correct
  (incomplete walk reads as credentialed).
- **Dialog seam u64 (67c32d25c)** — inventoried independently from both sides of the
  boundary: 14 JS↔wasm crossings exact (16-hex strings in `wasm.rs:2370/2854/3317` and
  `leader_session.rs:2365/2385/2471`; `wasm.rs:2437/2800/2869-2873/3300/3320` out;
  proxy hops `leader_session.rs:2011/2041/2052` and `leader.rs` decimal-string with
  `u64_field` decode); `parse_dialog_id` exact; the `cast_precision_loss` allow and both
  rounding casts deleted; TS side carries zero `number` transits and the fakes enforce
  the boundary kind.
- **`stand_down` re-attach (67c32d25c) — restated after #54.** The zombie-tab
  property is preserved and witnessed (the client-less `NotLeader`-forever strand is
  gone; the TS layer holds no role-derived state to go stale). Its supersession
  semantics were wrong and are fixed in #54: the rework recycled the
  FAILED-PROMOTION recovery (`resume_as_follower`) as the supersession recovery,
  which re-attached the fence away and re-took the lock. The credited shape is now:
  a stood-down tab re-attaches as a functioning follower and queues its acquisition
  ONCE IT HAS MET the successor it would replace — the blocking lock request is the
  close-or-crash detector. Deliberately not preserved: self-promotion after a
  supersession-into-silence (the pinned contract of
  `a_leader_revalidates_its_generation_against_the_store_and_stands_down`), and the
  successor-loss re-promotion leg remains unwitnessed — an owner may pin it with a
  new witness plus a CI roster floor move.
- **Effect-ordering repairs (67c32d25c)** — `accept_answer`'s fence verified against
  entry-time connection resolution; per-item fenced Answer application with pushback
  restore (the peer's verified Answer included); the settle-bool Reject/deadline
  fencing; the retained-Closure lifecycle (park-during-dispatch /
  clear-when-no-snapshot); `StreamOwnership::with` releasing the map borrow before the
  backend action; `RxStream::promote`'s span-aware handoff with exact concession
  arithmetic.
- **Witness-repair corpus (36cb4c0d6)** — the three-probe provenance witness
  (`kyra_fragment_group_cannot_change_stream_or_provenance`) binds exactly one dimension
  per probe against the production reassembly-key semantics (binding-set analysis
  complete); the round3 `|| StreamFailed` disjunction removal; the round4 typed-pin
  rewrite (stage-granular and reword-safe); the u128::MAX tautology deletion; the
  stale-dialog queue oracle with same-path positive controls; the routed-retirement
  witness on the real 5 s failure path.
- **Store convergence core (89f1ed53f, b0f6ae11c, 078e72a41)** — the generation-ahead
  resync coalesces for real (state + `#behind`) with ask-once/apply-nothing witnessed;
  `closed` is handle death in both directions (a legitimate `closed` is never dropped;
  a forged/stale `closed` cannot kill a live handle — every `forget` site traced); the
  delta-feed re-authorize runs before projection or overflow-install on every propagate
  path (witness gap at #27); failed-open cache clears without a stampede.
- **Documentation corrections** — the majority verify TRUE against code: the BlobRef
  Small-URI wire format, the DirStats shape, the single admit-rejected counter with
  reason label, `net_transport.h` shipping, the tmp-suffix pattern, the FIFO/in-order
  wording now matching `TRANSPORT.md`, the receive path and the rustdoc together, the
  three source pointers after the wire move, the `SignedPayloadCanonical` anchors
  (capability.rs:2527/2816), the `iceServers`/`rtc_stun_addr` split, the AEAD numbers
  against their own tables, the 16 fan-out loops and five integration families counted
  exactly, and the rewritten store design doc's §2/§4 against the shipped handles
  (including the transaction and admissible-value rules).
- **CI floor re-measurement (78880312c), wire half** — `MIN=275` re-measures exactly
  (275 test attributes, 0 `#[ignore]`, no feature-gated modules — and the step counts
  run lines of that same set). The deck half of that claim is contradicted (#38). The
  go ABI PASS-line assertion is sound.

---

## CI at the reviewed head

The `ci.yml` workflow concluded **`failure` at every pushed commit on this branch**
(078e72a41 @ 01:06Z, 78880312c @ 01:24Z, db6dd0a2 @ 01:32Z, 91ae3747 @ 04:24Z) while
master's latest run (2e45cc59) concluded `success`. Failure map at 91ae3747 (which is
code-identical to the pinned head except one documentation file), job → failing step(s):

| job | failing step(s) | attributed |
|---|---|---|
| Unit tests | Run Deck unit tests | #38 — deterministic false red (executed 33 < floor 35) |
| Python wheel (installed from the wheel) | Run the full suite against the installed wheel | not attributed in this pass |
| Leaf crate + wasm test runner | Formatting | not attributed in this pass |
| Rust SDK tests | Run Rust SDK tests; Sensing — consumer observation lifecycle (S1); Sensing — organization exact production call edge (OA-6) | not attributed in this pass |
| WebRTC feature (native driver) | Documentation; Unit tests (UNIT_FEATURES + webrtc); RTC harnesses; RTC witness inventory | #8 (units), #7 (harnesses/inventory); Documentation not attributed |
| Format | Check formatting | not attributed in this pass |
| Windows security tests | Org + authority unit tests | not attributed in this pass |
| Documentation | Build documentation | not attributed in this pass |
| Browser matrix (Chromium + Firefox) | Browser harness — Chromium; Browser harness — Firefox; Browser witness inventory (both gate engines) | not attributed in this pass |

At the pinned head `193f8affe` no main-CI run existed at review close; the auxiliary
workflows were green (Docs API, Skills, Coverage, natsim, CLA) with Web queued. Any
"green at head" claim must name its run by `headSha` with job counts.

---

## Method and limits

Read-only review of the three-dot diff `master...HEAD` at pinned head `193f8affe`, on a
Windows 11 workstation (i9-14900K). The candidate tree was never edited during the
review; both worktrees used in this review are proven clean (`git status --porcelain`
empty at close). This document is the review's single addition to the tree, committed
afterwards as a documentation-only change. Nine parallel read-only review lanes over
disjoint file sets produced
source analyses (findings above carry their confidence), plus the reviewer's own
execution and mutation probes. Per-test causes for the CI failures marked "not
attributed" above were not chased. No Linux execution was performed — nothing in this
document claims Linux behavior. The wasm witnesses were compile-verified only (browser
execution is CI's browser matrix, which is red). Go integration tests gated on
`RUN_INTEGRATION_TESTS=1`, and the net-cli / aggregator-daemon / mcp suites, were not
run.

**Execution record** (all at the pinned head unless noted; `--retries 0` where the
runner supports it):

| surface | command | result |
|---|---|---|
| browser-ts | `npx tsc -p tsconfig.test.json` + `npx vitest run` | tsc clean; 23 files, **695/695** passed (686 baseline + 9, as claimed) |
| leaf host | `cargo test` (leaf workspace) | **334/334** across 16 suites (lib 253, control_plane_boundary 8, dependency_boundary 5, establishment_identity 11, fixture_parity 4, kyra_followup 10, kyra_review 15, kyra_round2 5, kyra_round3 12, kyra_round4 10, ts_abi_fixture 1); the four `wasm_*` binaries select 0 tests on host (wasm32-gated) |
| leaf wasm32 | `cargo check --target wasm32-unknown-unknown --features mock-control-plane --test wasm_{leaf,anchorless,leader,rtc_conservation}` | clean (compile only) |
| RTC family | `cargo nextest run --no-fail-fast --no-tests=fail --retries 0 --features "webrtc fixtures cortex nat-traversal"` over the 15 binaries ci.yml pins | 205 run: **204 passed, 1 failed** — `the_full_section_9_sequence_with_the_three_part_witness` (#7), re-run in isolation failing identically |
| lib units (rtc graph) | `cargo nextest run --lib … --features "webrtc fixtures cortex nat-traversal"` | 5716 run: **5714 passed, 2 failed** — the two `membership_failure_tests` (#8), 2 skipped |
| SDK listener | `cargo nextest run --features rtc-bootstrap --test rtc_bootstrap_listener --test bootstrap_dep_boundary` | **28/28** |
| Go | `go test ./... -count=1` (after building `net-ffi`; see note) | `ok github.com/ai-2070/net/go 112.067s`, incl. `TestMeshSendBackpressureSurfaces`, `TestMeshSendWithRetryAbsorbsBackpressure`, `TestSessionSupersededSentinelMatchesTheHeaderCode`; 6 skips (5 × `RUN_INTEGRATION_TESTS`-gated, 1 × meshos live-SDK note) |
| deck floor basis (#38) | `cargo test -p net-deck --bins` (the step's exact command) | **33 passed** — of 35 test attributes in `deck/src`; two cannot run under this command (feature-gated), so floor 35 false-reds deterministically |
| attribution (#7) | the branch's repaired assertion applied to a detached worktree at `ef4a7e0f` (merge base) | **failed identically** (61.5 s, same panic) → pre-existing, not a branch regression; the base's own assertion is true by construction |
| attribution (#8) | the two witnesses at `ef4a7e0f`, same feature set | **2 passed** → branch-introduced |
| inverse probe (A) | mutation at `leaf/src/node.rs` re-introducing "a failed read destroys the pending" | `a_bad_or_stale_message_2_leaves_the_live_handshake_in_flight` **failed** at `node.rs:6354` "a failed read must take neither the pending nor the entry" (1 failed / 252 filtered); restored (diff captured, `git checkout`), re-run **passed** |
| inverse probe (B) | mutation at `browser-ts/src/store/core.ts` re-introducing `applySnapshot`'s unconditional commit | `core.test.ts` **failed ×2** — "discards a nested full snapshot when the handler throws" (listener fired 1×, must be 0) and "commits a nested full snapshot with the transaction instead of reverting it" (heading 4 ≠ 5); restored (diff captured), re-run **passed** 32/32 |

Go note: the cgo suite needs the `net-ffi` cdylib (`cargo build --release -p net-ffi`,
~6 min) and, on this host, a `VCRUNTIME140.dll` beside `net.dll` (the MSVC CRT is absent
from System32 here) with both DLLs loadable by the test binary. The staged copies were
removed after the run; nothing in the tree changed.

Restoration discipline: both mutation probes and both attribution probes were applied
only in the named trees, their diffs captured verbatim, and the trees restored and
proven byte-identical (clean `git diff` + clean `git status --porcelain`).

---

## Resolution status

**The repair pass landed as twelve commits (`552a24bb7` … `e3128fbce`)** and this
record. Local verification is green across every affected surface, so the HOLD's
production criteria are cleared in the tree; merge stays gated on the `ci.yml` run
at the merged head.

| findings | commit | closed by |
|---|---|---|
| #1, #2, #7, #8, #10, #11, #20, #21 | `552a24bb7` | the §12 gate-1 conversions with per-family unresolvable counters; one-lock dialog restore (+ `retire_displaced_attempt` at the other call sites); the duplicate-Offer reservation release; `accept_rtc`'s detector arming with a per-endpoint-class inactivity budget; resolvable-sender harnesses + the entry-less-refusal witness; per-leg negative observables over real dispatch; completion-bounded loopback drain |
| #3, #4, #22, #23, #24, #25, #51 | `ba7b0aef9` | `AdoptRefusal::dispose` (the duplicate wrapper discarded, never closed); the `channel_claims` nonce stamp gating rollback; the shared-state spy oracle (one declared inversion); the pre-deadline sweep control; the foreign-nonce/wrong-peer Ack controls; the typed unknown-subprotocol pin; the send-half "told once" latch |
| #9, #12, #18, #19, #32, #33 | `7c1e6faa1` | channel-identified loss reports (`IceLoss = (NodeId, u64)` + `InstalledChannel`); the pure `PendingHandshake::respond` probe; `sdp_bytes` in `OfferAccepted`'s Debug; the post-hoc frozen-tab fence row with a writer-side control; per-drop overflow-warn attribution; the production-shaped fixture sink + mid-borrow witness |
| #26, #45 | `1623cb8c4` | type-normalized tripwire scans (aliases, renames, wrappers, byte-buffer shapes, TOML quotes, recursive floored walk with one fenced exemption) + the kyra header exceptions |
| #5, #6, #15, #16, #17, #27, #28 | `7910a04b5` | transaction-staged request-side writes with deferred emission; `TRANSITION_DEADLINE_MS` + per-call deadline/`cancelWaiter` with slot release; typed aud-encode refusals; the pre-stringify admissible scan; supersession rejects its callers; the three missing witnesses; the non-f64 dialog fixture |
| #13, #14 | `68430d777` | the WHATWG host matcher in the SDK gate + the 33-row attack table; redacting `InviteToken`/`Attempts` Debug + the no-bearer-secret witness |
| #31, #34, #35, #36, #37, #38 | `95ee65677` | run-record pin anchors + the 25-row must-fail matrix; the signature-span extern-C trigger; the extern-C-only self-test case; the `.github/scripts/**` watch; named-only `--min`; the deck floor at the measured 33 |
| #30 | `c0bfa84d0` | the displaced-session construction from Go and the −117 terminal driven through both retry wrappers |
| #29, #39, #40, #41, #42, #43, #44 | `d1f3ee81e` | the ledger reconciliation (128 + 1 + 1 = 130, frozen bodies byte-identical); every coordinate re-counted; the reason-label and C/cgo corrections; the release-note pair in lockstep; the shipped store option |
| (comment) | `e3128fbce` | `BandwidthExhausted`'s doc names the `bandwidth` counter the runtime bumps |

Plus `cd4d9d5e6` (the MR#126 two-directional enroll fixture pin — closing the
merge review's "Noted" gap as real test work) and `ded4c8d25` (a rustfmt sweep of
four files no repair touched).

**Per-finding notes worth the record.**

- **#7's root cause (executed).** Three layers: the closer's `RtcSignal::Close`
  drops its `str0m::Rtc` without a wire teardown (no SCTP/DTLS close reaches the
  peer, so `Event::ChannelClose` never fires there); ICE disconnection is mapped
  away by `drain_session`'s catch-all while `reap`'s liveness filter cannot see it
  (str0m 0.23's `is_alive()` is `state != Closed`, and ICE disconnection never sets
  `Closed`); and the documented fallback was doubly dead — `accept_rtc` omitted the
  `failure_detector.heartbeat_for_incarnation` arming every sibling install arm
  runs, and the sweep's inactivity gate was `session_timeout × 30` (150 s in the
  §9 harness). Fixed at the mesh layer (detector armed; `PeerAddr::Rtc` budget =
  `session_timeout × miss_threshold`); the section-9 sequence passes end to end and
  the far-side cleanup is pinned by `a_forced_direct_loss_is_cleaned_up_on_the_far_side_too`.
- **#5's mechanism, corrected.** The finding's literal trace (in-handler
  `host.setState`) did not reproduce at the repair head — `commit`'s
  `Object.is(previous, current)` short-circuit made that path accidentally safe.
  The Required property was violated through the input-PARSE seam (application
  parse code ran outside `core.transact`, so its `commit` shipped beside the
  action), and the repair makes the deferral structural for every case.
- **#28's fixture, corrected.** The Required line's suggested
  `'0011223344556677'` is itself f64-exact (`int(float(v)) == v`) and proves
  nothing; the repair uses `'0123456789abcdef'`, which `as f64 as u64` rounds to
  `…896` (re-spelling the dialog to `…abcdf0`) so the verbatim assertion genuinely
  fails on a numeric re-encode. The Required property is what stands.
- **#8's owner question — resolved** by the narrow gate contract: the state is the
  documented eviction race; both witnesses' harnesses now model resolvable
  senders with their assertions byte-identical, and a new witness pins the
  entry-less refusal (counted, never rostered, never charged) against a
  resolvable-sender positive control.
- **#7's owner question — resolved on this host**: the far-side cleanup fires
  within the failure budget (executed, §9 green end to end); the Linux/CI leg is
  exercised by CI's section-9 pin at the merged head.

**Residuals named, not hidden.**

1. `net/crates/net/src/adapter/net/rtc/driver.rs`'s event-driven far-side close is
   the root completion for #7 and is NOT applied (unowned file; the Required
   property is satisfied by the documented timeout). Proposed arm: map ICE
   terminality to `session.closed = true` in `drain_session`'s event match —
   keying on `Failed`/`Closed` only, since ICE `Disconnected` is recoverable and
   must not tear down on transient blips.
2. #11's counters live in mesh.rs; their `RtcStats` promotion (three fields +
   `counter!` rows in `rtc/stats.rs`, three names + inapplicable rows in
   `leaf/src/counters.rs`) is specified and not applied.
3. The §12 witness's (c) delivery-positive is scope-stated: the short-form sibling
   test carries the invocation observable, while the long-harness streaming bridge
   timing is unattributed (`bridge_preflight`'s captured-service/origin binding is
   the named candidate). Five experiment classes eliminated before the scope note.

**Open, needing owner adjudication:** #46 (`act` ordering — whether a
capacity-refused act may still execute at the owner), #47, #48, #49, and #50
(gate-4 keying: adjacent sender vs announcement origin). #46–#50's Required lines
above are the work items; #51 is closed here.

**Verified after the pass** (executed, this Windows host): browser-ts `tsc` clean
+ **722/722** vitest; RTC family **208/208** (the section-9 sequence end to end);
net `--lib` **5719/5719** (the two membership witnesses green); sdk lib
**335/335**; sdk integration **31/31** (enroll parity 3/3); leaf **337/337** across
16 suites + wasm32 targets compile clean; the Go suite green (with the rebuilt
cdylib); both checker self-tests green (16 + 24 predicates); rustfmt clean across
all 16 net packages and the leaf crate; clippy clean in four configurations
including `--all-targets`; rustdoc clean (root, sdk, payments, wire). Witness
discrimination: ~36 lane inverse probes with four receipts each, plus a parent
spot re-proof (#4's rollback gate — red for its own stated reason, restored
green).

**Merge-gate fixes (`0b817cee9`, `b184e9720`).** CI at the repair head went from
nine red jobs to three: the pass turned Unit tests (the deck floor), the Python
wheel suite, the Rust SDK tests, all four WebRTC-feature steps, Format and
Documentation green. The gate then surfaced two more defects, both fixed here:
the Skills drift checker caught the phantom symbol the C/cgo relabel had
introduced (`net_blob_register_adapter` is `net_blob_register_callback_adapter` —
`0b817cee9`, all three documents), and `b184e9720` covers **#53**'s `shell: bash`
declaration plus three clippy lints in wasm-only code (`unnecessary_map_or`,
`unnecessary_get_then_check`, `useless_format` — visible only on the wasm32
target, where those test modules actually compile). The single red left at the
gate is **#52**, pre-existing and outside this repair's files; its witness failure
is executed in CI's browser matrix and recorded above with the required closure.

**Endgame at `68e2c19b0`.** The gate unpeeled in layers — each fix exposing the
next step's latent failure (fmt had been failing first all along): the phantom
symbol (`0b817cee9`), the pwsh/bash step + three wasm-target clippy lints
(`b184e9720`), and the private-link doc fix (`68e2c19b0`). The run at `68e2c19b0`
is green in **every** job except two steps in the browser-execution lane: the
Browser matrix (`#52`, pre-existing) and the wasm test runner (`#54`, two
leader-supersession witnesses — executed, and latent in the repair's own leader
rework). Everything else — the 58-job roster including Unit tests with the Deck
floor, the WebRTC feature job's Documentation/Unit-tests/RTC-harnesses/RTC-
witness-inventory steps, the sensing S1/OA-6 steps, the Windows org tests, Format,
Clippy, Documentation, Go, Python wheel and the SDK suites — is green at this
head. **Merge recommendation: hold** until `#54` is adjudicated and fixed in the
wasm lane (it is production supersession semantics), with `#52` and the two
owner-adjudication items (`#46`, `#50`) tracked alongside; the review's original
findings `#1`–`#51` are fixed and witnessed in this tree.

**The `#52`/`#54` round.** Both were root-caused and fixed with the local wasm
runner first enabling true red/green cycles in the browser-executed suites (the
host recipe: chromedriver 141.0.7390.37 against ms-playwright chromium-1194, plus
`CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner`,
`WASM_BINDGEN_TEST_TIMEOUT=120` (CI's env — the 20s default kills the driver
mid-suite) and a `webdriver.json` browser pin, which is a LOCAL-ONLY artifact and
is not committed).

- **#54 — fixed** (`fix(leaf): a superseded session refuses typed and a stood-down
  leader stays down`): `stand_down` had recycled `resume_as_follower` — the
  FAILED-PROMOTION recovery — as its supersession recovery, so `attach_as_follower`
  installed a `ProxyClient` whose gate adopted the successor's generation (the
  fenced session then SERVED through its successor — `Ok(Text("[]"))` where a typed
  refusal belongs) and the unconditional `await_promotion` re-queue re-took the
  lock `stand_down`'s own doc says it must not reclaim (role read `Leader` after
  its recorded generation moved). The fix introduces a `Supersession` marker
  refused at the single `Lifecycle::request` funnel (cleared only by
  `take_leadership` of a NEW generation) and queues the acquisition when the tab
  first MEETS its successor. Leader-session only; both witnesses untouched and
  green (29/29 in the wasm runner, inverse probes red-then-green per site, the
  zombie-tab witnesses preserved). The `stand_down` credit above is restated
  accordingly.
- **#52 — fixed** (`fix(browser): carry signal ids as 16-hex strings and refuse
  non-strings by name`): the uniform failure was the DIALOG argument, not the
  spellings — `page/leaf5.js:1829` passed the bare JS number `0` (the stale f64-seam
  call shape) and wasm-bindgen's string marshaling died (`assert!(old_size > 0)`
  via NaN lengths) before any parser ran, which also explains the engine asymmetry
  (the panic text surfaces per-engine: Firefox's `index out of bounds` is this
  panic family's prefix — reproduced by putting a slicing panic through the same
  mapping — while Chromium's silent `(false, "")` is the panic escaping the page's
  catch arm). Fixed at four layers: the harness call now spells the reserved
  no-attempt sentinel as 16 hex zeros (page-wide sweep found exactly one stale
  shape); the TS bindings refuse non-string ids BY NAME (never coerce);
  `parse_peer_id`/`parse_dialog_id` are pinned panic-free by a 12-shape panic-bait
  witness plus the reserved-zero and non-f64-rounding bit pins; and the refusal
  text originates at the binding so it reaches both engines identically. The
  stage6 end-to-end witness's call shape is reviewed statically (the run.sh matrix
  is Linux-only) and is CI-verified-only.

**#55 — open, recorded.** The three wasm-lib reds surfaced when the local runner
was first enabled; they are invariant under this pass's fixes (control-proven) and
await classification on CI's engines (the CI step has never reached the wasm-lib
run on this branch — it died earlier in the chain until this pass fixed that).

Post-round verification (executed, this host): browser-ts `tsc` clean + **726/726**;
leaf host **337/337**; `wasm_leader` **29/29** (both `#54` witnesses named green);
`wasm_leaf` **16/16**; the wasm lib **20/23** with exactly the `#55` trio red and
all three `id_parse_witnesses` green.

**The `#55` round and #56.** The Leaf chain's wasm test runner reached the
wasm lib for the first time at `8dfcf1d56` and classified the trio
immediately: `test result: FAILED. 20 passed; 3 failed` on Linux headless
Chromium too, 20/23 identical to this host. Both environmental candidates
died with that line — the trio is three real production defects in the
attempt/establishment/session lifecycle, root-caused and fixed in
`ec514cbef` (wasm.rs only; `node.rs` restored byte-identical; **all
witnesses byte-identical**):

- **The unnamed-open rule was unreadable.** The anchor resolution
  (`options.peer.unwrap_or(guard.anchor)`) was already correct, but
  `node::open_stream`'s no-session incarnation gate refused the open before
  any handle existed. The page surface now mints the unnamed handle itself
  (resolved peer, zero incarnation) and the gate stays byte-identical under
  its named-refusal pins — a wrong-site cut through the gate red-discriminated
  against itself at `establishment_identity.rs:154`, which is what moved the
  fix to the page surface.
- **A deadline settlement kept the wrong cause.** A settlement for a
  superseded dialog reported a bare `failed` (the terminal entry is gone,
  because `supersede` removes it) instead of the probe's own disposition; the
  refused arm now reports the disposition and charges nothing.
- **A retired establishment installs nothing — and its displaced session goes
  with it.** The §9-step-4 displacement now completes destructively (the
  entry the admitting direct establishment was replacing is torn down with
  it), the answer falls back to `ControlPlane::signal` with the fixture's
  drop-and-log floor when the session carrier cannot take it, and the
  successor's relay addressing is installed *after* the supersession so the
  predecessor's teardown cannot sweep it. Three independent inverse receipts
  pin the pieces: removing the carrier fallback reds `wasm_witnesses.rs:443`;
  removing the relay move reds `wasm_witnesses.rs:1554`; removing the
  displacement teardown reds `wasm_witnesses.rs:1571`'s `!has_session`.

Green at the round's end: wasm lib **23/23**, wasm_leader **29/29**,
wasm_leaf **16/16**, host **337** (lib 256 + `establishment_identity` 11
under its named-refusal pins). One behavior change named for the record: the
sibling witness `a_retired_attempt_admits_no_late_establishment` now also
loses the displaced session at its explicit `settle(Failed)` — unconstrained
by its oracles, but a change its text does not assert.

**#56** was root-caused in the same window: the leg-(a) positive polled the
announced key and then read the TOFU pin at a fixed instant, though both
install through the ingest's async transition. The read now polls *both*
observables in a bounded `wait_for` before the named asserts — no window
widened, negatives untouched, 3/3 consecutive integration runs (49/49 each)
plus the mesh-lib filter (421/421) (`8dfcf1d56`). The same-class audit over
the #20/#21 additions found every other read-after-async-effect already
wait-based: the (b) leg strictly sequenced, the (d) leg wait/join-based end
to end, and #21's drain loop itself completion-bounded.

**The gate's last unpeeling (merge-fix ledger).** Each step masked the next,
three CI rounds of them: `a156dcd83` formatted the #52/#54 fix files (the
Formatting step was failing before anything behind it ran); `48ede0446`
rewrote the parse witnesses' eleven `ok_expect`/`err_expect` forms to
`expect`/`expect_err` (wasm-target clippy; semantics identical); `ec514cbef`
is the `#55` round above; and `17f7325db` fixed the trio fix's own
`await_holding_refcell_ref` — its no-session carrier fallback held
`self.inner.borrow()` across `control.signal(...).await`, a real re-entrancy
hazard (the future yields back into page callbacks that borrow `inner`); the
cheap `AnchorControlPlane` handle now clones out under the borrow, the guard
drops, then the future is polled — the `offer_peer` pattern. Verified at
`17f7325db` across the full gate set locally: fmt, both CI-shape clippys,
host lib 256/256, wasm lib 23/23 (including the fallback's own witness and
the parse trio). The merge is the owner's to make.

---

## Open owner questions

1. **(asked at review time — #8's gate contract; RESOLVED in `552a24bb7`)** The
   narrow reading was adopted: the state is the documented eviction race. Both
   witnesses' harnesses model resolvable senders with their assertions unchanged,
   and a new witness pins the entry-less refusal against a resolvable-sender
   positive control. No further adjudication needed.
2. **(asked at review time — #7's platform split; RESOLVED on this host in
   `552a24bb7`)** The far-side cleanup now fires within the failure budget
   (executed here; the section-9 sequence green end to end). CI's section-9 pin
   exercises the Linux leg at the merged head; if it diverges there, #7's body
   platform note is the starting point.
3. **#46's `act` ordering — open.** May a `capacity`-refused `act` still execute
   at the owner? Recommendation: no — observe the correlation verdict before
   sending, and witness the refusal-with-no-frame; if executing unaccepted acts is
   deliberate, document it at the call site.
4. **#50's ingest keying — open.** Should §12 gate 4 veto announcement ingest by
   the ORIGIN's provisional-ness rather than the adjacent sender's? Recommendation:
   adjudicate against the §12 text ("a provisional peer's announcement is neither
   ingested nor flooded") and pin whichever behavior stands — today the relayed
   hop>0 path ingests and installs the TOFU pin (#50's body carries the in-run
   proof).
