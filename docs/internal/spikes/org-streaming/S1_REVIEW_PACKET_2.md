# Stage 1 repair (S1_R) independent review packet — round 2.1

**Reviewer:** S1Review2 (independent; HOLD authority, no edit authority).
**Reviewed head (pinned):** `cc15f4d665d232188ec2d61837134cd41d2bf831` on
`LZL0/org-streaming` (the S1_R repair round: `60c287d1e` witnesses rows 1–8,
`4ab5738cf` inverse receipts, `74c9bd99c` report §3, `dd2a31c8e` CI floor
re-pin 27→30, `3620fed0b` receipt R-A5′ + hash side-by-side + the Row-5 doc
amendment, `08dd849e2` resolution note, `cc15f4d66` record + F-9 label fix).
**Predecessor packet:** `S1_REVIEW_PACKET.md` (HOLD at `e25ac28bf`).
**Repair brief:** `spikes/org-streaming/S1_R_BRIEF.md` @`27d7a72ee`.
**Probe worktree:** `C:/Users/chief/orca/workspaces/net/org-streaming-s1rev2`
(detached at `cc15f4d66`; `CARGO_TARGET_DIR=target-s1rev2`;
`CARGO_INCREMENTAL=0` — a stated build-cache policy after the §0 F16-class
disk hazard recurred at round start; semantics unaffected). Date 2026-09-22.
Windows host only. Every mutation ran ONLY in that worktree; every cycle
ended sha-proven byte-identical to the pinned head and with a green leg.

**Citation convention:** source citations are **pristine-at-`cc15f4d66`**
line numbers. Every quoted panic states its numbering: my mutations in a
file OTHER than the asserting file leave the quoted lines pristine; where a
mutation shifts its own file the observed number and the delta are given
(A3: −4 lines above the unit ⇒ observed `:11804:9` = pristine `:11808:9`;
A3b: −1 ⇒ observed `:11873:9` = pristine `:11874:9`; all other mutations are
line-neutral or in another file). Run-specific values (session ids) are
labelled as such.

---

## 1. Verdict: **ACCEPT**

**There is no hold — this is not a CI-only question and there is no open
production defect in the repair.** The full estate reproduced at the pinned
head (`org_rpc_streaming` 30/30, the in-source 3-module filter 219/219,
preserved+controls+org_ownership 121/121, roster 30 source == 30 executed),
all nine findings F-1…F-9 are closed **by their verbatim closure
properties** (F-5 by the Main-ruled amended observable-seam wording + the
R-A5′ discrimination — §2), the Exit paragraph's first claim now holds as
**one executed content-correlated observation per authority mode** (§2,
final question), all six of my predecessor's inverses re-run RED at the new
witnesses' named assertions (five of the repair's own receipts re-executed
**verbatim**), and **five fresh lane-unused inverses I wrote for this round
all redden their targets** — no witness weakened, nothing re-pinned (§5).
The two adjudication items are closed below: F-S1R-2 is a **documented
limitation** (with a Stage-2 rider question for the owner — §3.1); F-S1R-3
is resolved for the repair's reproducible procedure and the record is now
accurate (§3.2).

Acceptance vs authorization: this ACCEPT governs Stage 1's acceptance and
the Stage 2 gate per the assignment; it does not authorize merge. Preserved
credit (§6) stands untouched — do not rework it. No rewrite, no new
framework, no scope expansion is requested anywhere in this packet.

## 2. F-1 … F-9 closure answers (by verbatim closure property)

Weakening check (the four — precondition fixed / window widened / assertion
relaxed / witness deleted) per row: **NONE applies to any new witness.**
All three integration witnesses and the three in-source units are
additions (`tests/org_rpc_streaming.rs` +445/−0 — the 27 preserved tests
byte-untouched; `cortex/rpc.rs` +282/−0 in two hunks at `:11760`/`:12040`,
both inside `#[cfg(test)] mod tests` (`:8132`) — no production-path edit);
their windows are the preserved 200 ms darkness window plus bounded waits;
their assertions are exact `==` on bytes/values/positions; nothing was
deleted, renamed, or loosened.

**F-1 — CLOSED.** Verbatim property: *"a named witness in which a handler
queues ≥2 items under zero credit, returns, a valid `STREAM_GRANT` arrives,
and the items publish IN ORDER before one `Completed(Ok)` terminal —
reddening under A2b."* Witness `completed_stream_drains_queued_items_in_order_with_content_and_end_terminal`
(`tests/org_rpc_streaming.rs:2851`): `QueueChunksAndReturn` queues two
content-labelled items under a ZERO credit window (`Some(0)`) and RETURNS
(`returned == 1` awaited; the pre-grant darkness window `:2920-2923` pins
"zero credit publishes nothing before the grant"); a valid `STREAM_GRANT`
(2 credits) arrives; the items publish IN ORDER — asserted by body bytes
AND position (`(seen[0], seen[1]) == (b"item-A-CONTENT-17",
b"item-B-content-29")`, `:2950-2960`) — never counted (see G6); then ONE
`Completed(Ok)` terminal (`:2978-2990`) with `seen.len() == 3` and the
250 ms re-check `:2994` ("exactly one terminal frame, ever"). Reddens under
A2b at its own named assertion `:2939:5` (my re-run, §5.1).

**F-2 — CLOSED.** Verbatim property: *"the F-1 witness also asserts the
terminal frame's exact wire content (status + `nrpc-streaming: end`),
reddening under A2c."* The same witness asserts the terminal's exact wire
tuple `(RpcStatus::Ok, [(nrpc-streaming, [101,110,100])], [])` at `seen[2]`
(`:2978-2990`). Reddens under A2c at its own named assertion `:2978:9`
(my re-run — verbatim receipt re-execution, §5.2).

**F-3 — CLOSED.** Verbatim property: *"the byte unit (or a sibling) drives
a node-refusal after two successful level increments and asserts both
rolled back — reddening under A3."* Witness
`node_budget_refusal_rolls_back_call_and_caller_reservations`
(`cortex/rpc.rs` test module `:11772`): the refused item passes the CALL
level (400+200 ≤ 1 000) and the CALLER level (400+200 ≤ 1 500) — two
successful increments — then hits the NODE level (1 900+200 > 2 000);
both counters asserted back to EXACT values and the node total unmoved
(`400 / 400 / 1 900`, asserts pristine `:11808/:11813/:11818`). Reddens
under A3 at `:11804:9` observed (pristine `:11808:9`, mutation −4):
`left: 600 / right: 400` — byte-identical to repair receipt R-A3 (§5.3).

**F-4 — CLOSED** (realization named; see the note). Verbatim property:
*"a named witness that reuses `(caller, call_id)` while the old record's
async cleanup is still armed and asserts the successor's survival —
reddening under A4."* Witness
`late_retire_against_a_reused_key_is_a_no_op_for_the_successor`
(`cortex/rpc.rs:12058`): the first record is installed, `confirm`ed
(ownership transferred to its supervisor), retired, and its single removal
is driven directly (`complete`); the SAME key is reused **immediately** —
the witness contains no `record_count()` wait or poll — while the first
incarnation's cleanup-owner handles stay armed and late-op-capable across
the reuse; every late op carrying the stale incarnation (retire / complete
/ release / commit_check) is asserted inert (`:12137/:12141/:12145/:…`);
the successor survives end to end (phase `Running`, `terminal_reason
== None`, owner signal untaken, on-retire hook unfired,
`commit_check(successor) == Proceed`, `removals == 0`) and then completes
exactly once (`removals(&key, second_incarnation) == 1`). Reddens under A4
at `:12137:9` (pristine; my re-run verbatim = receipt R-A4, §5.5).
*Realization note (the repair's §3.4(4), tested):* the phrase "while the
old record's async cleanup is still armed" is realized as **the cleanup's
owner handles armed while its single record-removal step is driven
synchronously** — the only schedule the production registry admits:
`reserve` refuses `AdmissionDenied::ActiveCallOwned` while ANY record
exists for the key (`cortex/rpc.rs:4674-4676`; the sibling witness
`active_call_id_reuse_after_replay_window_is_refused`'s doc states the
same), so a successor cannot install before the removal runs. The
property's other reading (reuse with the removal still pending) is
unsatisfiable against production without weakening that fence — which this
review does not ask for and will not propose. The property's named impact
("a late retire/complete can settle or remove a successor call") is
precisely what the witness pins and A4 reddens.

**F-5 — CLOSED by the amended observable-seam wording + the R-A5′
discrimination** (Main's S1R ruling, sharpened (a); amended, not
weakened). Amended property: *"the EMIT-SESSION SELECTION the test drives —
a response emitted after a session replacement is carried by, delivered
on, and attributed to the session LIVE at emission — read receiver-side at
the ingress attribution (`RpcInboundEvent::session_id`, the R2
carrying-incarnation stamp): the LIVE session's endpoint receives it while
the REPLACED session's endpoint receives NOTHING — receiver-side
attribution asserted on BOTH endpoints."* Witness
`response_after_session_replacement_reaches_only_the_live_session`
(`tests/org_rpc_streaming.rs:3169`): the response is emitted strictly
AFTER the replacement; every recorded event is asserted
`ev.session_id == live_endpoint` (`:3264`) and none may carry
`replaced_endpoint` (`:3270`), with the body-content assert tying the
observation to the post-replacement item. My re-run of **R-A5′** (the three
`RpcInboundEvent` attribution stamps at `mesh.rs:30512/:30537/:30549`
forced to `0`; the `StreamLifetime` stamp at `:30758` untouched) REDS it at
its own named assertion `:3264:9` (`left: 0 / right:
14977886970936825178` — run-specific session id). The repair's receipt-7
quote (`:3256:9`, `right: 17873330928486580960`) is its pre-amendment
baseline: `3620fed0b`'s own +11/−3 doc-comment amendment to this witness
(the resolution-note text) shifted the file +8 below it — assertion
identity byte-identical. The brief's **A5** (`receiving_session_id = 0` at
`mesh_rpc.rs:4771`) re-runs **30/30 GREEN** — correctly classified as
**feature-graph-unreachable, not a witness weakness**: the value's only
consumers are inside `#[cfg(feature = "webrtc")]` (`mesh_rpc.rs:3581`'s
enrollment block — `enrollment_reservation_owner` /
`promote_on_enrollment_response` / `note_enrollment_rejected` /
`retire_enrollment_call`) plus tracing fields (`:4834/:4845/:4858`) and
the parameter's own `cfg_attr(not(feature = "webrtc"),
allow(unused_variables))` (`:3508`); both sanctioned warm graphs exclude
`webrtc`. The witness stands on its receiver-side property at the named
ingress seam.

**F-6 — CLOSED** (Main's selected closure: witness the handoff; do NOT
delete — Stage 2 is `transfer`'s named consumer). Verbatim property (the
selected branch): the S0 model's `cancel_dequeue_handoff_consumes_one_permit`
shape at the production site — *"source permit consumed, target owns,
exactly-one release across the pair, another call's live bytes stay
charged."* Witness `item_permit_transfer_consumes_once_across_the_handoff`
(`cortex/rpc.rs:11846`): `transfer()` consumes the source (`unsettled_drops
== 0` — consumed by the handoff, not dropped unsettled, `:11879`), the
bytes STAY charged across the handoff (`call_bytes == 100`, `:11874`),
exactly one release settles the pair (`target.release()` ⇒ `0`, `:11887`),
and another call's live bytes stay charged throughout (`node_bytes == 700`,
`:11893`). Reddens under A3b at `:11873:9` observed (pristine `:11874:9`,
mutation −1): `left: 0 / right: 100` — byte-identical to receipt R-A3b,
followed by its disclosed cascade (`:3963:14` pristine, "byte permit
released twice, or against the wrong call" → abort) in the same order
(§5.4).

**F-7 — CLOSED.** Verbatim property: *"one named executed witness per
authority mode (same-org and cross-org) in which a protected
server-streaming call through the real bridge delivers MULTIPLE items —
asserted by body content and order, not counted — and then EXPLICIT
COMPLETION whose terminal frame is asserted at the authenticated receiving
endpoint (status `Ok` + the `nrpc-streaming: end` marker); the witness must
redden under (a) a post-close discard of queued chunks (A2b) and (b) a
success-terminal shape change (A2c)."* Same-org mode = the F-1 witness
(owner-delegated intent via `fixture::owner_delegated_intent` +
`serve_rpc_owner_scoped_streaming`); cross-org mode =
`cross_org_completed_stream_drains_correlated_items_with_end_terminal`
(`:3011`; the granted/other-org intent shape — `fixture::cross_org_intent`
B→A `INVOKE` + `serve_rpc_granted_streaming`; its own content/order assert
`:3103-3113`, terminal assert `:3139`, one-terminal re-check `:3144`).
Both observations are content-correlated (body bytes + position) at the
**authenticated receiving endpoint** — the caller's OWN registered
reply-channel dispatcher, over the real handshake + signed entity pins
(`fixture::bring_up` = `connect`/`accept` + both signed announcements),
with every response leaving through the REAL transport ("genuine wire
delivery at real endpoint nodes", `s15.rs:1-13`; the injected part is only
the REQUEST/GRANT ingress at the bridge's own bounded mpsc — the
dispatcher's exact hand-off point — and the opening is minted through the
REAL caller-side mint helper `test_sign_admission_proof`, `s14.rs:128-137`).
Both witnesses redden under **A2b AND A2c at their own named assertions**
(my re-runs: `:2939:5`/`:3092:5` and `:2978:9`/`:3129:9`).

**F-8 — CLOSED.** Verbatim property: *"name the re-indent in the module
doc, or restore the original indentation inside the verbatim block."*
Named in the module docs: `old_serve.rs` adaptation 2 — "that block is
RE-INDENTED — every one of its five lines shifted +8 columns (4/8 → 12/16)
to sit at the shim's nesting depth. WHITESPACE ONLY (S1 review finding F-8
named this gap; the whitespace-insensitive provenance statement and
trimmed-hash evidence are below)" — plus the trimmed-hash evidence
paragraph, and `frozen_85ecc77c9.rs:19-22` names it at the top level.
Vendored bodies untouched (the file diff is +15/−1, all doc lines; the
five-line body's provenance re-derived in §3.2). Documentation only, per
Main's ruling.

**F-9 — CLOSED.** Verbatim property: *"relabel the claim to the filter (or
split by module)."* `S1_REPORT.md:2132-2134` and `:2230-2231` now read
"`adapter::net::mesh_rpc` filter tests (39 in `mesh_rpc.rs` + 13 in
`mesh_rpc_metrics.rs`; F-9 label fix 2026-09-22)" — split by module AND
attributed to the filter. (Executed check of the underlying split is
retained from the predecessor's §9 census; the label fix itself is the
record edit verified here.)

**Exit paragraph, first claim — now HOLDS.**
*"same-org and cross-org live native server-streaming calls produce
multiple correlated items and explicit completion"* is now established as
**one executed content-correlated observation per authority mode** —
exactly the §6 closure shape: two items delivered in order asserted by
body bytes (never counted — proven load-bearing by G6, §5.10), then
exactly one terminal with the exact `Ok` + `nrpc-streaming: end` content,
at the authenticated receiving endpoint, in the same observation — and the
observation reddens under both named inverses (A2b, A2c) at its own
assertions. Reproduced by me: `org_rpc_streaming` 30/30 exit 0 at the
pinned head.

## 3. Adjudications

### 3.1 F-S1R-2 — **documented limitation** (NOT a production defect
requiring closure before acceptance); owner question surfaced with a
Stage-2 recommendation.

**My probe (executed).** Temporary `tests/s1rev2_probe.rs` in my worktree
(141 lines, added → run → removed; tree proven clean). Construction per
iteration (3 iterations, fresh node pairs): real handshake
(`connect_no_start`) + entity pin + the caller's dispatch loop running; a
PROTECTED owner-scoped call (`serve_rpc_owner_scoped_streaming`, real
`test_sign_admission_proof` mint, zero-credit opening) that PARKS with its
sink handed out (`SinkHolder`); the caller's recorder on its reply channel;
then the Row-5 replacement (`connect_no_start` again) and an 8 s bounded
observation window. Result — **3/3 iterations identical**:

```
S1R2-PROBE iter=0 record_count_before=1 after=0 handler_dropped=true arrived=false frames=0 session_replaced_terminals=0 attribs=[]
S1R2-PROBE iter=1 record_count_before=1 after=0 handler_dropped=true arrived=false frames=0 session_replaced_terminals=0 attribs=[]
S1R2-PROBE iter=2 record_count_before=1 after=0 handler_dropped=true arrived=false frames=0 session_replaced_terminals=0 attribs=[]
S1R2-PROBE SUMMARY iterations=3 retired=3 any_frame_within_bound=0 session_replaced_terminals=0
```

The retire lands every time (`record_count 1→0`, handler future dropped —
the terminal's emit side runs through the shared `stream_terminal_payload`
+ terminal drainer), yet **zero frames — not the `SessionReplaced` terminal
(`Cancelled` + `b"peer session replaced"`), not anything — reach the
caller's recording endpoint within 8 s**, sharpening the repair's own
0/1 within 30 s to a deterministic 0/3. Positive control (the same
recorder/delivery path is alive when the transition is not in flight): the
twins' items+terminal deliver 30/30, and Row-5's explicitly
post-replacement response delivers.

**Mechanism.** The drop paths are source-established:
`try_publish_to_peer` (`mesh.rs:41806-41860`) returns `NoSession`
(pre-send) or `SendFailed` on `TxAdmit::SessionSuperseded | StreamClosed |
WindowFull` with the contract note "MUST NOT be retried on the roster";
the streaming terminal drainer logs "terminal publish failed at the
transport seam (not retried on the roster)" and drops (`mesh_rpc.rs:4908`);
the DirectOnly `NoSession` route drops by policy (`:3658-3665`). The
timing that makes the loss deterministic is executed in the sibling
witness: `session_replacement_retires_old_call` pins that "the displaced
branch retires the old session's calls **before the transition returns**"
(`tests/org_rpc_streaming.rs:1848-1853`) — the terminal's emit is armed
inside the transition window every time. Which precise drop variant fires
per run remains **[INFERENCE]** (no tracing captured).

**Classification reasoning.** (i) The callee-side contract holds — exactly
one terminal is emitted after pump stop, on the shared path the twins
prove deliverable. (ii) The undeliverable-at-transition outcome is the
design's OWN documented, deliberately-bounded case: `mesh_rpc.rs:7893-7897`
— "Dropping the frame in both cases is correct — the caller times out,
which is the same outcome it already gets from a full response drainer
channel" (the two cases are route-cache eviction and **`NoSession` at send
time** — this case), and `:4846-4848` names the §2.8 outcome "the peer
observes interruption or its deadline". (iii) Impact boundary: a displaced
protected call's caller loses the more-precise `Cancelled` / "peer session
replaced" reason and terminates at interruption-or-its-deadline; no item
loss (the call is being torn down), no authority/corruption exposure, no
silent data divergence. Stage 1's Exit claim covers replacement
**retirement** (held, executed) — not terminal-delivery-across-transition.
Therefore: **documented limitation, not closure-blocking.**

**Owner question (surfaced; a reviewer does not rule).** The repair's
proposed follow-up property — "a post-replacement terminal that cannot be
silently dropped" — is a contract improvement needing design at the R2
seam (defer the emit past the transition, or re-target to the live session
composed with the receiving-incarnation fence and the exactly-one-terminal
rule). **Recommendation: name it as a Stage 2 item** (enhancement class),
together with the never-executed last mile: the caller-side fold's local
termination behaviour after its OWN session replacement.

### 3.2 F-S1R-3 — **the repair's procedure (`f31eb08e…`) is coherent with
the packet's own provenance claim; the record is now accurate; the
packet's `a1c518dd…` is superseded as unreproducible on the record.**

The packet's provenance claim is: the five-line denial-shape piece is
content-identical to the extraction modulo whitespace and the ONE named
prefix retarget. A provenance record must be checkable by a reader, so the
coherent procedure is one that (a) states its trim convention and digest,
(b) reproduces on BOTH sides, (c) discriminates the raw windows. My
re-derivations (all executed; the window is `git show
85ecc77c9:net/crates/net/src/adapter/net/mesh_rpc.rs | sed -n '872,876p'` —
its raw sha256 reproduces the recorded extraction hash exactly, so my
stream is byte-identical to the packet author's):

| step | value |
|---|---|
| extraction raw sha256 | `fa275454b5e7293f7e3090ed0775d1193d520b00cbab9e3f68f5b43deccc5960` == recorded |
| extraction, each line trimmed both sides, sha256 | `f31eb08ec127c32f1a4ccf1892ec4dadf0fac151af89aad03cce65b60ad327a7` == the module doc's |
| vendored five lines (`old_serve.rs:222-226`) trimmed, sha256 | `bfb3bb90a4d6954b2a2dcf60188005dd7c69e2edde6b600088c58ed41cfd52ba` == the repair's recorded single-delta value |
| vendored trimmed + the named retarget reversed, sha256 | `f31eb08e…` == and `cmp` CLEAN (byte-identical) |
| raw vendored window sha256 | `cb95d6a30ec50d95a236b7bd9f77bcf81ec426ee934d595c8dcc3385f50dc9ea` ≠ `fa275454…` (whitespace + the named prefix — exactly as claimed) |

The repair's procedure (the one recorded in the frozen module doc: trim
per line, reverse the one named retarget, sha256) satisfies (a)–(c). The
packet's `a1c518dd…` satisfies none verifiably: it names no trim
convention and no digest, and it reproduces under **neither** the repair's
12 framings **nor my 8 additional attempts** (trimmed sha256 of windows
871-876 / 872-877 / 870-876 / 872-875 / 873-877 = `2429dbc3…` /
`0395b18e…` / `00d4bb53…` / `7cddae9b…` / `48bebd05…`; trailing-newline-
stripped `15e60977…`; whitespace-collapsed `ca0510eb…`; CRLF-joined
`594703fa…` — none begins `a1c518dd`). **Answer to the question posed:**
the repair's `f31eb08e…` procedure is the coherent one — the record (the
frozen module docs) now carries exactly it with its evidence, so **the
record is now accurate**; the packet's bare `a1c518dd…` should be read as
an underspecified historical value (a record-quality defect in the packet —
mine — not in the repair). F-8's closure ("name the re-indent") holds
independently of the hash question.

## 4. Reproduce ledger (executed at `cc15f4d66`, `--no-tests=fail --retries 0`)

| Leg | Command (from `net/crates/net/`) | Result |
|---|---|---|
| org_rpc_streaming | `cargo tf --retries 0 --test org_rpc_streaming` | **30/30**, exit 0 |
| in-source 3-module filter | `cargo tfl --retries 0 adapter::net::cortex::rpc adapter::net::mesh_rpc org_stream` | **219/219** (5622 skipped), exit 0 |
| preserved+controls+org_ownership | `cargo tf --retries 0 --test org_ownership --test nrpc_streaming_gate --test integration_nrpc_streaming --test integration_nrpc_client_streaming --test integration_nrpc_duplex --test nrpc_registration_order --test integration_nrpc_protected --test org_admission_wire --test subnet_org_boundary` | **121/121** (9 binaries), exit 0 |
| roster | source extraction (`#[test]`/`#[tokio::test]` fns of `tests/org_rpc_streaming.rs`; helper modules carry none) == `cargo nextest list` == the report's 30-name list | **30 == 30 == 30** |

Warm graphs exactly as pinned in `net/crates/net/.cargo/config.toml`
(`cargo tf` / `cargo tfl`); `webrtc` excluded by construction.

## 5. Inverse executions (all cycles: bounded diff at a production site →
named red → `git checkout --` restore `sha256sum ==` baseline → green)

Baselines (== the repair's recorded baselines): `cortex/rpc.rs`
`35b003e55f5f75fe25350312ae5107bc3a11ed189b82f4d119a9ee5ddb8c0c92`;
`mesh_rpc.rs` `eda1c392eff64b26dd98c90f5a7b5df998af5c3f40b879cc1b6be8350e9d8f47`;
`mesh.rs` `a55818907d0bfe52f5e1ec285fe7f6216e451c58c7366fc79038c662c7ca98fd`;
`org_admission.rs` `db305c207ff45c43478f46c32270534f1f197dd32d67f9546a2abab45bdb95c7`.

### 5.1 A2b (prior inverse #1 = repair receipt R-A2b, re-executed VERBATIM)

Mutation (`cortex/rpc.rs` supervisor pump, the send-order anchor):

```diff
             // Await per-chunk publish so chunks for one call_id reach
             // the network in send order.
+            if !pump_gate.as_ref().is_some_and(|g| g.is_finished()) {
             pump_emit(from_node, caller_origin, call_id, resp).await;
+            }
```

Run: `cargo tf --retries 0 --test org_rpc_streaming -E
'test(=completed_stream_drains_queued_items_in_order_with_content_and_end_terminal)
+ test(=cross_org_completed_stream_drains_correlated_items_with_end_terminal)'`
— exit 100 (2 run, 2 failed), each at its own named assertion, **byte-
identical to the repair's R-A2b quotes**:

```
tests\org_rpc_streaming.rs:2939:5: the same-org queued items publish IN ORDER after the grant — a post-close discard of queued chunks leaves the caller with its terminal only
tests\org_rpc_streaming.rs:3092:5: the cross-org queued items publish IN ORDER after the grant — a post-close discard of queued chunks leaves the caller with its terminal only
```

Restore `35b003e5…` == baseline; green 2/2 exit 0.

### 5.2 A2c (prior #2 = receipt R-A2c, re-executed VERBATIM)

Mutation: `stream_terminal_payload`'s `Completed(Ok)` arm
`HEADER_NRPC_STREAMING_END.to_vec(),` → `HEADER_NRPC_STREAMING_CONTINUE.to_vec(),`.
Same run — exit 100, each twin at its own named assertion with the exact
wire diff, **byte-identical to R-A2c**:

```
tests\org_rpc_streaming.rs:2978:9: assertion `left == right` failed: the same-org terminal frame's exact wire content is status Ok + the `nrpc-streaming: end` marker
  left: (Ok, [("nrpc-streaming", [99, 111, 110, 116, 105, 110, 117, 101])], [])
 right: (Ok, [("nrpc-streaming", [101, 110, 100])], [])
tests\org_rpc_streaming.rs:3129:9: assertion `left == right` failed: the cross-org terminal frame's exact wire content is status Ok + the `nrpc-streaming: end` marker
  (same left/right)
```

Restore `35b003e5…`; green 2/2 exit 0.

### 5.3 A3 (prior #3 = receipt R-A3, re-executed VERBATIM)

Mutation: both counter restorations deleted from `ByteBudgets::reserve`'s
`NodeBudgetFull` arm (the `return Err(ByteRefusal::NodeBudgetFull);` kept).
Run: `cargo tfl --retries 0 -E
'test(=adapter::net::cortex::rpc::tests::node_budget_refusal_rolls_back_call_and_caller_reservations)'`
— exit 100:

```
src\adapter\net\cortex\rpc.rs:11804:9: assertion `left == right` failed: the call counter was rolled back
  left: 600
 right: 400
```

(`:11804:9` observed under the mutation's −4 delta; pristine
`:11808:9`.) Byte-identical to R-A3. Restore `35b003e5…`; green 1/1.

### 5.4 A3b (prior #4 = receipt R-A3b, re-executed VERBATIM incl. its
disclosure)

Mutation: `pub fn transfer(mut self)` + `self.settled = true;` → `pub fn
transfer(self)` (the ownership-consumption removed). Run:
`cargo tfl … -E 'test(=adapter::net::cortex::rpc::tests::item_permit_transfer_consumes_once_across_the_handoff)'`
— exit 100 (aborted after the second panic). Named assertion FIRST, then
the disclosed cascade — both byte-identical to R-A3b:

```
src\adapter\net\cortex\rpc.rs:11873:9: assertion `left == right` failed: transfer is not memory reclamation — the bytes stay charged across the handoff
  left: 0
 right: 100
src\adapter\net\cortex\rpc.rs:3963:14: byte permit released twice, or against the wrong call
```

(`:11873:9` observed under −1; pristine `:11874:9`; `:3963:14` pristine —
above the mutation.) Restore `35b003e5…`; green 1/1.

### 5.5 A4 (prior #5 = receipt R-A4, re-executed VERBATIM)

Mutation: `record.incarnation != incarnation ||` dropped from
`retire_locked`'s guard. Run: `cargo tfl … -E
'test(=adapter::net::cortex::rpc::tests::late_retire_against_a_reused_key_is_a_no_op_for_the_successor)'`
— exit 100:

```
src\adapter\net\cortex\rpc.rs:12137:9: a LATE retire carrying the old incarnation must be a no-op for the successor (§2.4)
```

Byte-identical to R-A4. Restore `35b003e5…`; green 1/1.

### 5.6 R-A5′ (prior #6 per the amended seam) + A5 classification

Mutation: the three `RpcInboundEvent` attribution stamps
(`session_id: session.session_id(),` at `mesh.rs:30512/:30537/:30549`) →
`session_id: 0,`. Run: `cargo tf … -E
'test(=response_after_session_replacement_reaches_only_the_live_session)'`
— exit 100, red at the witness's own named assertion:

```
tests\org_rpc_streaming.rs:3264:9: assertion `left == right` failed: the LIVE session's endpoint receives the post-replacement response
  left: 0
 right: 14977886970936825178
```

(Pristine `:3264:9`; the repair's `:3256:9` is its pre-amendment baseline —
`3620fed0b`'s +11/−3 doc amendment shifted the file +8 below it;
`right:` is the run-specific live session id.) Restore `a5581890…`; green
1/1. **A5** (the brief's original: `let receiving_session_id = …
.unwrap_or(0)` → `0` at `mesh_rpc.rs:4771`): whole binary **30/30 GREEN** —
feature-graph-unreachable (consumers `#[cfg(feature = "webrtc")]`-gated,
§2/F-5), classification confirmed as correct. Restore `eda1c392…`; green.

**Receipt re-executions of the repair's own receipts: 5 of 7 verbatim
(R-A2b, R-A2c, R-A3, R-A3b, R-A4 — each red byte-identical), R-A5′
re-executed at the amended seam (line delta named), R-Row8 has no runtime
inverse (documentation-only; its trimmed-hash evidence re-derived in
§3.2).**

### 5.7 Fresh lane-unused inverses (the attack — 5, requirement ≥3)

All at production sites none of the lane's receipts touched; every one
RED at a named assertion; controls where designed stayed green in the same
run. **Any-green = 0. The four weakenings: NONE applies to anything in
this section (no window widened, no assertion relaxed, no witness deleted,
no precondition "fixed").**

**G2 — cross-org authority path** (fresh). Mutation (`behavior/org_admission.rs`
CrossOrgGranted arm): `if grant.issuer_org != ctx.provider_owner_org {` →
`==` (a flipped comparison at the cross-org grant's issuer check). Run: the
twin pair — **1 passed, 1 failed**, exit 100:

```
tests\org_rpc_streaming.rs:3067:5: the cross-org handler queued its items under zero credit and returned
```

The cross-org twin dies at its own named assertion (its granted admission
now refuses) while **the same-org twin PASSES in the same run** (the
discriminating control) — proving the cross-org twin genuinely rides the
`CrossOrgGranted` verification path (if it rode the owner-delegated path,
this mutation would leave it green). Restore `db305c20…`; green 2/2.

**G3 — the successor-salvage fence's second path** (fresh; A4 covered only
`retire_locked`). Mutation (`ProtectedCallRegistry::remove`):
`is_some_and(|r| r.incarnation == incarnation && r.cleanup == expected)` →
`is_some_and(|r| r.cleanup == expected)`. Run: the row-4 unit — exit 100:

```
src\adapter\net\cortex\rpc.rs:12141:9: a LATE complete carrying the old incarnation must be a no-op
```

The witness's LATE-COMPLETE leg (distinct from A4's LATE-RETIRE leg at
`:12137:9`) catches it — the "every late op … is inert" breadth is
load-bearing on both the retire and the remove paths. Restore
`35b003e5…`; green 1/1.

**G4 — release-once's zero-release side** (fresh; A3b covered the
double-release side). Mutation (`ItemPermit::transfer`'s construction):
the returned permit `settled: false` → `settled: true` (the target is born
consumed). Run: the row-6 unit — exit 100:

```
src\adapter\net\cortex\rpc.rs:11887:9: assertion `left == right` failed: exactly one release settles the pair
  left: 100
 right: 0
```

The handoff's "exactly ONE release across the pair" is watched in BOTH
directions (A3b: zero→double caught; G4: zero caught). Restore
`35b003e5…`; green 1/1.

**G6 — content identity (the "not counted" leg)** (fresh). Mutation (the
supervisor pump's chunk payload): `body: chunk.body` → `body:
Bytes::new()` — counts unchanged (2 items + 1 terminal still flow), bodies
destroyed. Run: the twin pair — exit 100, each at its content assert:

```
tests\org_rpc_streaming.rs:2950:9: assertion `left == right` failed: the same-org items publish IN ORDER — body bytes and wire sequence
  left: ([], [])
 right: ([105, 116, 101, 109, 45, 65, 45, 67, 79, 78, 84, 69, 78, 84, 45, 49, 55], [105, 116, 101, 109, 45, 66, 45, 99, 111, 110, 116, 101, 110, 116, 45, 50, 57])
tests\org_rpc_streaming.rs:3103:9: (the cross-org twin — left ([],[]), right the cross-item byte arrays)
```

A counting oracle would stay green here (the item count and the terminal
are intact) — the witnesses' body-bytes equality is what discriminates
("counting where the claim is identity" defeated). Restore `35b003e5…`;
green 2/2.

**G7 — items-before-terminal ordering** (fresh). Mutation (the supervisor's
`handler_returned` arm): an additional `tokio::spawn(emit(...,
stream_terminal_payload(&Completed(Ok))))` at gate-finish — the completion
terminal now fires BEFORE the credit-blocked drain. Run: the twin pair —
exit 100, each at the pre-grant darkness window:

```
tests\org_rpc_streaming\s15.rs:202:9: assertion `left == right` failed: zero credit publishes nothing before the grant: 1 frame(s) reached the roster subscriber — a protected response was fanned out
  left: 1
 right: 0
```

The chain's first discriminating leg fires (a terminal may not precede the
queued items' publish window). Restore `35b003e5…`; green 2/2.

### 5.8 Attack summary

Five fresh lane-unused inverses across the new witnesses and seams
(granted-authority path; the `remove`-fence; the handoff's release-once;
the content-identity oracle; the items-vs-terminal ordering) — **all five
red at named assertions, zero green-under-inverse, zero weakenings, zero
re-pins.** The new witnesses discriminate every property they claim.

## 6. Preserved credit — verified holds; do not rework

Retained from the predecessor's §5 (10 items — re-litigation out of scope
by assignment) with these incidental confirmations from this round:
the 27 preserved tests are byte-untouched (the repair's test-file diff is
+445/−0); `mesh_rpc.rs` production is sha-identical to `e25ac28bf`'s
(`eda1c392…`); `cortex/rpc.rs`'s additions are confined to `#[cfg(test)]
mod tests`; the frozen gate's extraction provenance hashes re-derive
exactly (§3.2); the estate counts match the report (30 / 219 / 121). The
retirement/refusal half of the lifecycle contract, the zero-effects
readings, the §1.4 frozen gate, the 1.6 pin protocol and the receipt
hygiene all remain as credited there.

## 7. Findings classification

**No findings meeting the ledger criteria (provable + actionable +
unintentional + introduced-in-patch) this round.** The two named items are
adjudications, not findings: F-S1R-2 is a deliberate, documented-bounded
design behaviour (fails "unintentional") — §3.1; F-S1R-3 is a
record-quality defect in the predecessor's packet, not in the patch —
§3.2. One observation, recorded and NOT raised as a finding (outside the
verbatim closure properties): the Row-5 witness does not assert
delivery-exactly-once to the live endpoint (a duplicate attributed live
would pass its two asserts) — noted for the Stage-2 composition work
where the terminal-retarget design would make it relevant.

## 8. HOLD rows and closure properties

**None — nothing is held.** For the record, the Exit paragraph's first
claim's closure property (the predecessor's §6, verbatim) —

> one named executed witness per authority mode (same-org and cross-org) in
> which a protected server-streaming call through the real bridge delivers
> MULTIPLE items — asserted by body content and order, not counted — and
> then EXPLICIT COMPLETION whose terminal frame is asserted at the
> authenticated receiving endpoint (status `Ok` + the `nrpc-streaming:
> end` marker); the witness must redden under (a) a post-close discard of
> queued chunks (A2b) and (b) a success-terminal shape change (A2c).

— is **ESTABLISHED** (executed; §2 final answer + §5.1-5.2 + §5.7 G6).

## 9. Never executed here (complete)

- **The `webrtc` feature graph** — the only graph consuming the A5 value
  (F-S1R-1's classification is source-established + the A5-green
  execution; no webrtc code ran anywhere this round).
- Linux/macOS and `#[cfg(unix)]` legs (Windows host only).
- CI itself (branch unpushed; nobody pushes but Main) — the floor re-pin
  27 → 30 (`dd2a31c8e`) is record-inspected only; its CI run is not
  executed here. The stage-end checklist was per assignment taken as
  recorded-and-green, verified where named, not re-litigated.
- The benches (0.1 baselines), the wasm32 runner, `tests/cross_lang_*`,
  the browser/SDK/facade surfaces, the Go/Python/Node bindings.
- Live two-process transport outside the test harness's real wire
  endpoints.
- R-Row8 (no runtime inverse exists — documentation-only row; its
  trimmed-hash evidence re-derived instead, §3.2).
- The Stage-1 deferred receipts not re-run this round (predecessor §7's
  list; nothing in the repair touches their code paths).
- **The F-S1R-2 last mile**: the caller-side fold's local termination
  behaviour after its own session replacement (named in the §3.1 rider);
  and anything about terminal delivery beyond my 8 s × 3 bounds (the
  repair's separate 30 s bound is its own executed evidence).
- Any tracing capture of which precise drop variant
  (`NoSession`/`SessionSuperseded`/`StreamClosed`) fires per displaced
  terminal (recorded as [INFERENCE], §3.1).

## 10. Final-verification record

- **Pinned HEAD:** `cc15f4d665d232188ec2d61837134cd41d2bf831` (detached
  probe worktree `org-streaming-s1rev2`).
- **Diff-check:** `git diff --stat cc15f4d66` EMPTY at end of round — the
  worktree is byte-pristine at the pinned head.
- **Probe removal:** `tests/s1rev2_probe.rs` (141 lines, F-S1R-2 probe)
  added → executed → REMOVED; `git status --porcelain` shows only the
  untracked `target-s1rev2/` build cache.
- **Mutations:** 12 cycles, every one restored `git checkout --` and
  `sha256sum ==` its pre-mutation baseline (`35b003e5…` ×8, `eda1c392…` ×1,
  `a5581890…` ×1, `db305c20…` ×1 — A3b ran twice, first capture truncated
  its named red and was re-run whole), each with a restored green leg.
- **Final restored-green:** `cargo tf --retries 0 --test org_rpc_streaming`
  → `30 tests run: 30 passed`, exit 0 at the pristine tree.
- **Environment note (disclosed):** the round started at ~1 GB free on C:
  (the §0 F16-class hazard); I reclaimed the CONCLUDED predecessor
  review's untracked build caches (`org-streaming-s1rev/target-s1rev` +
  `target-s1probe` — regenerable, no tracked file involved) and built with
  `CARGO_INCREMENTAL=0` (stated above). No tracked file anywhere was
  touched by the reclaim.
- **Session-tree writes:** this packet only.
