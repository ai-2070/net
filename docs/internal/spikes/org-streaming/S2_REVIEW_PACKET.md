# Stage 2 (rows 2.1–2.4 + row 5) independent review packet

**Reviewer:** S2Review (independent; HOLD authority, no edit authority).
**Reviewed head (pinned):** `017e7148a2100e9a28ad9dd7e0170c2ab2c7abf9` on
`LZL0/org-streaming` (session tree `C:/Users/chief/orca/workspaces/net/org-streaming`).
**Probe worktree:** `C:/Users/chief/orca/workspaces/net/org-streaming-s2rev`
(detached at `017e7148a`; `CARGO_TARGET_DIR=target-s2rev`;
`CARGO_INCREMENTAL=0` — stated build-cache policy after the prior F16-class
disk hazard; semantics unaffected). **Second probe worktree:**
`C:/Users/chief/orca/workspaces/net/org-streaming-s2rv16` (detached at
`16cd67e85`) for the F-S2.1-3 reconstructed-tree run. Every mutation ran ONLY
in the s2rev worktree; every cycle ended sha-proven byte-identical to the
pinned head and with a green leg. Date 2026-09-22. Windows host only. Every
run `--retries 0 --no-tests=fail` on the warm aliases (`cargo tf` /
`cargo tfl`, graphs pinned in `net/crates/net/.cargo/config.toml`), from
`net/crates/net/`.

**Sources of truth:** the plan's Stage 2 table + Exit + §1/§2/§3/§4.3 +
compatibility ledger C5–C8/C11 (`docs/internal/plans/ORG_SCOPED_STREAMING_PLAN.md`);
`docs/internal/spikes/org-streaming/S2_BRIEF.md` @`87f89c8da` (incl. the owner rider ruling);
`docs/internal/spikes/org-streaming/S1_REPORT.md` §4 + §0's Stage-2 entry;
`S1_REVIEW_PACKET.md` / `S1_REVIEW_PACKET_2.md` (the evidence standard in
practice). Recorded rulings observed throughout: the F-S1R-2 rider DECLINED
(no terminal-retarget / caller-last-mile / exactly-once work is in scope and
none is demanded here); `Revoked → Denied`; the frozen coarse bytes
`{Denied, NotSupported, Unavailable}`; the Q1 limits; NC2 `DirectOnly`;
F-6's handoff ruling. Stages 3+ are out of scope.

**Citation convention:** source citations are **pristine-at-`017e7148a`**
line numbers. Every quoted panic states its numbering: my mutations live in
files other than the asserting file (quoted test lines pristine; mutation
line delta 0) except where stated — R-S2.5 and G4 mutate `mesh_rpc.rs`
one-line-for-one-line (delta 0); the S2REV probe test is APPENDED after the
pristine 5256-line test file and quotes its own numbering. Thread ids in
quotes are run-specific.

---

## 1. Verdict: **ACCEPT** (with three findings — repair work named in §7/§9)

**There is no hold.** This is not a CI-only question and there is no open
production defect at the pinned head: the estate reproduces exactly
(§2), every row's named witnesses discriminate (all eight named inverses
re-executed by me red at their own named assertions, byte-identical to the
report's receipts; two fresh inverses also red), the Stage 2 **Exit holds on
all three clauses** (§3.6), and both disclosures are adjudicated as
acceptable/correct with my own executions (§4). Two contract rules of the
§2.6 table remain unwitnessed — executed green-under-my-inverse (F-S2R-1,
§7) — and the F-S2.2-5 repair closes every reachable refusal but not the
leak CLASS structurally (F-S2R-2, §4.3/§7, with an executed orphan
demonstration). Those three are **witness/repair work only**: no rewrite, no
new framework, no scope expansion is asked for anywhere in this packet, and
none of them reopens a row, the Exit, or the F-S2.2-5 instance.

Distinction from the S1 round-1 hold: that round held on an UNESTABLISHED
Exit claim plus green inverses on the rows' own claimed seams. Here the Exit
is established (executed, per-shape) and every row's named witness set
discriminates; the open items sit on adjacent §2.6 rules and on the
fix-class question the assignment itself posed.

Acceptance vs authorization: this ACCEPT governs Stage 2's acceptance and
the Stage 3 gate per the assignment; it does **not** authorize merge.
Preserved credit (§8) stands untouched — do not rework it. The owner may
schedule §7's repair work before or alongside Stage 3; nothing in Stage 3's
scope depends on it.

## 2. Reproduce ledger (executed at `017e7148a`, `--no-tests=fail --retries 0`)

| Leg | Command (from `net/crates/net/`) | Result |
|---|---|---|
| org_rpc_streaming | `cargo tf --retries 0 --test org_rpc_streaming` | **41 run / 41 passed / 0 skipped**, exit 0 |
| preserved + public regression control | ONE `cargo tf --retries 0` invocation, 10 × `--test` (nrpc_streaming_gate, integration_nrpc_streaming, integration_nrpc_client_streaming, integration_nrpc_duplex, nrpc_registration_order, integration_nrpc_protected, org_admission_wire, subnet_org_boundary, org_ownership, cross_lang_wire) | **134/134** (10 binaries), exit 0 |
| cross-lang + gate | ONE `cargo tf --retries 0` invocation, 4 × `--test` (cross_lang_capability_fixtures, integration_nrpc_cross_lang, integration_nrpc_cross_lang_streaming, org_admission_gate) | **32/32** (4 binaries), exit 0 |
| in-source three-module filter | `cargo tfl --retries 0 adapter::net::cortex::rpc adapter::net::mesh_rpc org_stream` | **220/220** (5622 skipped), exit 0 |
| Stage 0 models | `cargo tfl --retries 0 org_stream` | **76/76** (5766 skipped), exit 0 |
| roster from source | `#[test]`/`#[tokio::test]` fns of `tests/org_rpc_streaming.rs` = **41**; the five helper modules (s2/s13/s14/s15/fixture) carry **0** test attributes | **41 source == 41 executed == 41 pinned** (CI `run_binary org_rpc_streaming 41` at `d214adc56`, names extended in the same commit, floor raised deliberately 30→41) |
| models untouched | `git log 87f89c8da..HEAD -- "net/crates/net/src/adapter/net/behavior/org_stream_*"` (and `*org_stream_{lifecycle,registry}.rs`) | **EMPTY** (0 commits) — the models are untouched and green at 76/76 |

The report's estate claims reproduce exactly (41 / 220 / 134 / 32 / 76);
no suite selected zero tests.

## 3. Row-by-row audit (2.1, 2.2, 2.3, 2.4, 5) and the Exit paragraph

**Weakening check (the four — precondition fixed / window widened / assertion
relaxed / witness deleted) per row: NONE applies to any Stage 2 witness.**

- All eleven named witnesses are ADDITIONS (`tests/org_rpc_streaming.rs`
  +2049 across the stage) — there is no prior red state to hide a weakening
  in; their assertions are exact `==` on typed error variants, body bytes,
  wire tuples and map snapshots; their bounded waits are 10 s with 250 ms
  exactly-once re-checks; their darkness windows are the preserved 200 ms
  idiom (`fixture::assert_handler_stays_dark` = SETTLE 200 ms / STEP 10 ms,
  "the plan forbids shrinking it"; `s15::assert_stays_empty` called with
  200 ms at every Stage-2 site) — nothing widened, nothing shrunk.
- The 30 preserved witnesses are semantically byte-untouched: the three S1_R
  witnesses whose lines rustfmt re-wrapped
  (`completed_stream_drains_queued_items_in_order_with_content_and_end_terminal`,
  `cross_org_completed_stream_drains_correlated_items_with_end_terminal`,
  `response_after_session_replacement_reaches_only_the_live_session`) are
  whitespace-identical under normalisation — my executed check (awk body
  extraction + `tr -d '[:space:]' | sha256sum`, old vs new):
  `edaf83163c8fab877fafe5c1cee89a7dd2e486e4675fe069e69c9b5ea5c5b83a`,
  `d4f69c897960bf32dd0544e9acd3e6036b9f8eb123e204d866519d96c14df3b9`,
  `0ff9d805480e2660a972ca00c8fde8bbf3bcc40736de2b1babb7801d34f80fee` —
  each IDENTICAL at `87f89c8da` and `017e7148a`. The other 27 have zero
  hunks in the stage.
- The one witness DELIBERATELY changed — `call_streaming_mints_a_stream_proof`
  — lost exactly its two CS/DX intent-refusal legs (my normalized diff: 12
  lines removed, 135→123 body lines, NO other delta; the kept halves — the
  capability-mismatch leg, the service-routed refusal leg, the positive —
  are verbatim). That is row 5's named delete-and-replace (F-S2.1-1): the
  replacement `client_stream_and_duplex_mint_their_stream_proofs` landed at
  `1dcbf2545` with its own discriminating inverse (R-S2.5 re-executed §5.8),
  and the CI floor was adjusted deliberately in the same stage. The deletion
  and replacement ride different commits within the stage — adjudicated
  under F-S2.1-3 (§4.1).

### 3.1 Row 2.1 — lazy-opening mint: **MET**

`client_stream_opening_binds_first_chunk` (`tests/org_rpc_streaming.rs:3319`)
observes exactly the demanded text: the REAL lazy mint (`call_client_stream`
+ first `send`) is captured at the provider's request dispatcher as a full
kind-2 `OrgStreamCallProof` (strict decode; kind asserted); the first chunk
is altered AFTER signing; the production §3 transaction
(`admit_protected_opening`) refuses with the TYPED
`AdmissionDenied::BindingInvalid` (exact-variant match — the "any exception"
oracle weakness is defeated) and ZERO effects at the admission boundary
(`record_count() == 0`, `active_node() == 0` — the reservation rolled back);
the UNALTERED twin — its own call — ADMITS, pinning causation to the
alteration (positive control). "Zero handler effects" is realized at the §3
transaction boundary here (F-S2.1-2's named realization: a refused opening
is never fold-driven), with the live-bridge darkness probes carried by the
2.2 witnesses. The plan's `JustOpened`-drop clause is preserved verbatim
(source: the Drop state check untouched). Named inverse R-S2.1 re-executed
(§5.1) — red at its own named assertion. Four weakenings: NONE.

### 3.2 Row 2.2 — CS/DX admission: **MET**

All five named witnesses exist with the demanded observations (bodies read
line-by-line at the pinned hash):

- `client_stream_aggregate_with_valid_proof` (`:3509`) — a VALID
  owner-delegated kind-2 opening (body IS the first chunk) + continuation +
  END **aggregate into the call's ONE response** at the authenticated
  receiving endpoint: `(RpcStatus::Ok, b"AGG-first-7AGG-second-11")` —
  content-joined in order, never counted; four-party attribution observed on
  `RpcStreamingContext::org_admission` (the probe compares caller, acting
  org, provider org, provider AND capability EXACTLY — `s2.rs`
  `AttributionProbes::observe`); the raw proof header stripped (E1.6);
  §2.6's single-response rule (250 ms re-check: "exactly one response frame,
  ever"); handler ran exactly once.
- `duplex_exchange_with_valid_proof` (`:3642`) — the CROSS-ORG shape
  (`serve_rpc_granted_duplex` + the B→A grant intent): echoed bodies IN
  ORDER (`EX-req-1`, `EX-req-2`), a content-labelled TAIL after input EOF
  (`EX-tail-9` — independent halves under one record), then EXACTLY ONE
  terminal with the exact wire tuple `(Ok, [(nrpc-streaming, [101,110,100])],
  [])` at the authenticated receiving endpoint; same attribution + stripping
  probes; "exactly one terminal frame, ever".
- `pre_admission_chunks_are_never_delivered` (`:3803`) — chunks + an END
  for a call with NO admitted opening: handler dark
  (`assert_handler_stays_dark`), endpoint silent through the bounded 200 ms
  window, `in_flight_keys()` and `sender_keys()` both empty; when the real
  opening later arrives the aggregate is EXACTLY
  `b"OPEN-body-3post-body-5"` — the pre-admission junk is nowhere.
- `end_cannot_cancel_another_stream_or_reopen_terminal_half` (`:3959`) — two
  live protected CS calls: B's END closes ONLY B's input half
  (`input_half() == Ended`) while A's stays `Open` and A keeps receiving; a
  SECOND END never reopens B's half (still `Ended`; B's request sender never
  reappears); a late B chunk after END is discarded (B's aggregate is
  `B-1-B-2-` — it stops at ITS first END); both calls' remaining output
  completes exactly once (body-content asserted: `A-1-A-2-A-3-` /
  `B-1-B-2-`); "exactly one response per call, ever".
- `wrong_session_grant_does_not_release_credit` (`:4169`) — BOTH named
  wrong-session probes at a zero-credit protected DX window keyed 4-tuple: a
  wrong-session REQUEST_CHUNK is never delivered (`seen_bodies == 1` — "the
  opening body is all the handler ever sees") AND a wrong-session
  `STREAM_GRANT` never releases credit (`flow_control_permits == Some(0)`);
  positive controls both ways (the exact session's grant releases to
  deterministically `Some(4)` — 5 granted − 1 consumed — and the exchange
  completes); endpoint silence during the wrong-session probes asserted.

Dispatched inverses re-executed by me: R-S2.2a (3-tuple keying) and R-S2.2b
(the grant lookup's key) — both verbatim (§5.2, §5.3); R-S2.2c (the
F-S2.2-5 fix removed) verbatim (§5.4, §4.3). Four weakenings: NONE.

### 3.3 Row 2.3 — duplex response flow control (C8): **MET**

- `duplex_response_window_blocks_until_grant` (`:4478`) — park-and-release
  on BOTH folds: with `Some(0)` initial credit the response pump PARKS
  (nothing publishes through the 200 ms darkness window;
  `flow_control_permits == Some(0)` named), and a `STREAM_GRANT` on the
  exact session releases it — echo + ONE terminal complete; the PUBLIC leg
  asserts the exact terminal wire tuple.
- `cross_direction_grant_is_ignored` (`:4709`) — the cross-direction
  negatives BOTH ways: an inbound `DISPATCH_RPC_REQUEST_GRANT` (the upload
  kind — wrong direction for a response window) never releases response
  credit (window stays at initial zero; endpoint silent) while the
  `STREAM_GRANT` kind does (positive control); and a `STREAM_GRANT` at a
  client-streaming call (no response pump to credit) is a no-op — its
  single response completes grant-or-not (the shape half).

Dispatched inverses R-S2.3a (grant arm removed) and R-S2.3b (semaphore
bypassed) re-executed verbatim (§5.5, §5.6). Four weakenings: NONE.

### 3.4 Row 2.4 — half-close + both-direction retirement: **MET**

- `upload_end_then_remaining_output_completes` (`:4922`) — the upload END
  closes the input half ONCE (`input_half() == Ended` observed on the LIVE
  record held by its credit-parked output); a SECOND END and a late post-END
  chunk change NOTHING (half never reopens; "the late chunk is nowhere" —
  body-content assert `UT-req-1/UT-req-2/UT-tail-5`); the handler's
  REMAINING OUTPUT — echoes IN ORDER then the post-EOF tail — and ONE
  terminal with the exact wire tuple `(Ok, [(nrpc-streaming,
  [101,110,100])], [])` complete after the early END; "exactly one terminal,
  ever".
- `retire_unblocks_both_directions` (`:5121`) — retire (the caller's CANCEL)
  closes BOTH halves: the INPUT-side waiter's owned future DROPS (a
  `DropFlag` flips — non-vacuous), the input admission is closed through the
  queue owner (`sender_keys()` no longer contains the key), and the
  OUTPUT-side waiter (the credit-parked pump holding a queued echo) is
  stopped: EXACTLY ONE `Cancelled` terminal — and a LATE `STREAM_GRANT`
  after the terminal never resurrects the stopped pump (the named "no chunk
  is published after the terminal" assertion) plus a late input chunk
  discarded.

R-S2.4a re-executed (§5.7); the F-S2.4-1 two-layer pair executed (§4.2).
Four weakenings: NONE.

### 3.5 Row 5 — the CS/DX intent-refusal pins invert: **MET**

`client_stream_and_duplex_mint_their_stream_proofs` (`mesh_rpc.rs:10688`,
the exact 1.6 mirror): positive halves observe the LAZY mints' output at the
provider's request dispatcher — full strict-decoded `OrgStreamCallProof`,
kind == 2 (CS) and kind == 3 (DX), and `session_binding` == the exact live
session's Noise handshake hash (§1.3), each minted over the FINALIZED
initial REQUEST at the first `send` (contract 2); kept halves verbatim-
mirrored: the capability-mismatch leg (both verbs fail LOCAL with
`RpcError::Codec` on a different service) and the service-routed refusal leg
(`call_service_streaming` rejects the intent at the TOP, before discovery).
Named inverse R-S2.5 re-executed (§5.8) and my G4 attack (§6.2) reds the
kind assertion itself. Unit totals match the report (mesh_rpc filter 52→53;
all others count-unchanged). Four weakenings: NONE.

### 3.6 Exit paragraph — **HOLDS on all three clauses**

> "Client-streaming aggregate and duplex exchange execute with valid proofs;
> each shape has its own zero-effect denial, wrong-session and control-frame
> probes; SS success is not evidence for CS/DX."

1. **"client-streaming aggregate and duplex exchange execute with valid
   proofs" — HOLDS (executed).** `client_stream_aggregate_with_valid_proof`
   and `duplex_exchange_with_valid_proof` are single content-correlated
   observations at the authenticated receiving endpoint (§3.2).
2. **"each shape has its own zero-effect denial, wrong-session and
   control-frame probes" — HOLDS.** Per-shape matrix (every probe drives its
   own shape's seam; nothing borrowed):

   | probe kind | client-streaming | duplex |
   |---|---|---|
   | zero-effect denial | `client_stream_opening_binds_first_chunk` (typed `BindingInvalid` + §3 rollback), `opening_body_budget_refusal_completes_the_record` (typed `ResourceExhausted`, 0x0009 + `Unavailable`, handler dark, zero maps, record completed), `pre_admission_chunks_are_never_delivered`, preserved `forbidden_stream_opening_causes_zero_handler_effects` CS leg (shape-refusal class: handler dark + empty CS maps) | preserved `forbidden_stream_opening_causes_zero_handler_effects` DX leg (shape-refusal class: handler dark + empty DX maps) |
   | wrong-session | `client_streaming_fold_foreign_session_cannot_feed_or_cancel` (in-source, the CS fold's OWN probe: a forged chunk from another session never enters the victim's stream — identity-exact `[a, b, c]` with `ATTACK` nowhere — and a forged CANCEL never cancels) | `duplex_fold_foreign_session_cannot_feed_or_cancel` (the DX fold's own twin) + `wrong_session_grant_does_not_release_credit` (protected: chunk non-delivery + grant no-credit, both named) |
   | control-frame | `end_cannot_cancel_another_stream_or_reopen_terminal_half` (END ×2 + late chunk), `cross_direction_grant_is_ignored` shape half (GRANT at CS is a no-op) | `cross_direction_grant_is_ignored` direction half, `upload_end_then_remaining_output_completes` (END ×2 + late chunk), `retire_unblocks_both_directions` (CANCEL + late GRANT) |

3. **"SS success is not evidence for CS/DX" — HOLDS (independence
   verified).** No CS/DX assertion observes an SS registration, SS handler
   or SS fixture outcome: each witness drives its own CS/DX serve seam and
   asserts at its own endpoint/fold maps. The shared helpers (s13/s15/
   fixture/s2) are observation infrastructure (frames, recorders, darkness
   windows, mint wrapper over the real `test_sign_admission_proof`), not SS
   evidence. `duplex_response_window_blocks_until_grant`'s two legs are both
   duplex (protected + public).

## 4. Adjudications

### 4.1 F-S2.1-3 — the reconstructed S2.1/S2.2 commit boundary: **ACCEPTABLE; the history reads honestly**

The disclosure: one lane implemented slices 2.1 and 2.2 in one working tree
before committing; the S2.1/S2.2 boundary is RECONSTRUCTED into per-slice
commits; each reconstructed tree's green run was executed in the named probe
worktree.

My verification (executed + source):

1. **The history is honest.** The `16cd67e85` commit message ITSELF
   discloses the reconstruction: "Commit-slice note: the S2.1/S2.2 boundary
   is RECONSTRUCTED into per-slice commits (one lane implemented both slices
   in one working tree); this commit's tree is exactly the slice-2.1 content
   and is green in the S2.1 probe run recorded in the report." The plan
   Review-log entry (`017e7148a`) and `S1_REPORT.md` §4 repeat it. Nothing
   in the history pretends commit-granular landing happened.
2. **The hunk classification is real** (executed):
   `git show 16cd67e85:net/crates/net/tests/org_rpc_streaming/s2.rs` → does
   NOT exist (2.2's helper rides the S2.2 commit, as claimed);
   `16cd67e85`'s `cortex/rpc.rs` delta is three re-wrapped assertions in
   `mod tests` (my diff: `assert_eq!(budgets.node_bytes(), 1_900, …)`
   collapse + a `registry.reserve(opening())` wrap + two `assert_eq!`
   wraps — token-identical, zero semantic change); the tests-file delta is
   the new witness + the same re-wrap class as §3 (normalized shas prove the
   three preserved witnesses semantically untouched).
3. **The reconstructed S2.1 tree is green — MY OWN probe run** (executed in
   `org-streaming-s2rv16` at `16cd67e85`, shared target cache, sequential):
   `cargo fmt -p net-mesh -- --check` → exit 0; `cargo tf --retries 0 --test
   org_rpc_streaming` → **31 run / 31 passed / 0 skipped**, exit 0 (the
   30-roster + `client_stream_opening_binds_first_chunk`); `cargo tfl
   --retries 0 adapter::net::cortex::rpc adapter::net::mesh_rpc org_stream`
   → **219/219**, exit 0. Exactly the S2.1 claims.
4. What I did NOT run: the S2.2 interim tree (`4db0f8a5f`) and the later
   interim trees' green runs (the lane's probe-worktree runs stand as its
   evidence; the head's 41/41 supersedes their content — every one of their
   witnesses is in my §2/§5 runs).

**Answer:** the reconstructed boundary with probe-run green trees is
SUFFICIENT for acceptance here — the stage's claims are all executed at the
head, the boundary commit is green under my own run, the commit content
matches the claimed slice classification, and the deviation from
commit-granular "landed green before the next" is disclosed in the report,
the commit message AND the plan record. The related F-S2.1-1 split (the
row-5 pin deletion at S2.1, its replacement at S2.5) is the four-weakenings
"deletion" case handled correctly at stage scope (replacement landed with
its own discriminating inverse; floor adjusted deliberately); the
same-COMMIT preference is a process default the disclosure + green interim
trees satisfy in substance. The F-S2.1-4 proof-method parenthetical
("`git diff -w` is empty for those hunks") is technically imprecise — the
re-wraps survive `-w` as line-count changes — but its substance ("pure
formatting, no semantic change") is TRUE and now verified by normalisation
(P3 note, §7).

### 4.2 F-S2.4-1 — `retire_unblocks_both_directions` green under the single-layer inverse: **correctly treated as OVERDETERMINATION; R-S2.4c-v2 is the property's true inverse**

My executions (both cycles re-run, `cortex/rpc.rs` sites):

1. **Single-layer (layer 1 only — the forced path's `sem.close()` +
   `pump_task.abort()/await` removed, `rpc.rs:5890-5896`): the witness
   stays GREEN** (1/1, exit 0) — reproducing the disclosed observation. The
   late `STREAM_GRANT` cannot reach the parked pump because
   `StreamCallRegistration::complete()`'s `flow_control` removal
   (`:3515-3516`) is an INDEPENDENT second defense (the grant's map lookup
   misses).
2. **R-S2.4c-v2 (BOTH layers — pump never stopped AND the window entry kept
   live): RED** at `tests\org_rpc_streaming.rs:5250:5` (pristine numbering;
   mutations in `cortex/rpc.rs`, delta 0 to the test file):
   `assertion 'left == right' failed: no chunk is published after the
   terminal — the late grant never resurrects the stopped pump / left: 2 /
   right: 1` — byte-identical to the report's receipt. The late grant
   resurrected the unstopped pump and the queued echo published AFTER the
   terminal. Restore sha-proven; restored green.

**Judgment.** The claimed property is the OBSERVABLE ("no chunk is published
after the terminal" — §2.2's abort+join invariant as witnessed). It is
defended in depth: (1) pump stop at the forced path, (2) window-entry
removal at the single removal point. Falsifying the observable requires
defeating BOTH — which is exactly what R-S2.4c-v2 does (two bounded hunks,
the smallest change that makes the property false). Therefore R-S2.4c-v2 IS
the property's true inverse, and green-under-single-layer is
**overdetermination** (redundant defenses), NOT a weakened witness: the
single-layer mutation is not the property's inverse, the assertions are
exact and unmoved (counts `== 1`, body-content identity), and no window or
precondition changed (four weakenings: NONE). The disclosure + the
discriminating two-layer receipt is exactly the treatment witness-discipline
prescribes for layered defenses — without it the inverse-column discipline
would misread the single-layer cycle as a non-discriminating witness. The
disclosure is ACCEPTED as recorded.

### 4.3 F-S2.2-5 — the pre-supervisor opening-body leak repair as a production fix

**The defect and the fix (source + executed).** Ownership transfers at
§3 step 5 (`registry.confirm`) BEFORE the opening body's §2.7 delivery in
both `apply_inbound_admitted` seams; a budget refusal there postdates the
transfer but predates the supervisor, so the release-once `complete` had no
owner — the terminal registry record kept its key forever (`record_count`
stuck at 1; any successor refused `ActiveCallOwned`). The fix settles the
record's release-once `complete` (plus every map entry) in the fold at that
refusal and refuses `ResourceExhausted` (wire 0x0009 + the coarse
`Unavailable` — §4.3 mapping) with zero further delivery, in BOTH seams (CS
`cortex/rpc.rs:7549-7551`; DX `:8391-8393`).

**Fail-pre-fix reproduced by me (executed, verbatim receipt R-S2.2c, §5.4):**
fix removed → `opening_body_budget_refusal_completes_the_record` reds at its
named assertion `tests\org_rpc_streaming.rs:4455:5` (pristine numbering;
mutation in `cortex/rpc.rs`, delta 0): `assertion 'left == right' failed:
the pre-supervisor refusal completes the record's single removal / left: 1 /
right: 0` — byte-identical to the report's receipt. Restored green.

**Is the fix correct and scoped?** Correct: release-once `complete` on the
exact `(key, incarnation)`, in-flight/flow/sender map cleanup, typed
refusal, zero delivery; the witness observes handler-dark + empty maps +
`record_count == 0` + `active_node == 0`. Scoped faithfully to the §2.7
rule ("refuse the call, never truncate it") — it refuses the OPENING, does
not truncate.

**Does it close the leak CLASS or only its instance?** It closes the
instance AND its same-site siblings — `deliver_protected_body` fails for
three reasons (byte reserve; queue slot; the §2.3 check-and-commit) and all
three funnel into the ONE `if !deliver_protected_body(...)` cleanup
statement (source-established; the budget sub-reason executed). But it does
NOT close the class structurally. The cited unary `ConfirmedOpening`
precedent (`rpc.rs:5578-5597`) is a **Drop scope guard** whose doc names the
class exactly: "if the opening is refused at the effect boundary after the
transfer, this scope guard stands in for it so no `Running` record is ever
orphaned." The CS/DX realization is INLINE cleanup at one site, not a guard;
any OTHER exit in the post-transfer window orphans the record. Plan §3
step 5 mandates the guard shape: "Scope guards handle scheduling/installation
failure without orphaning a `Running` record."

**Sibling-path probe (executed).** No additional refusal path is reachable
today (I examined every statement in the post-transfer window: it contains
no other `return`/`?`/fallible call — the window is fully synchronous, so a
pre-supervisor cancellation is also unreachable via the public API; a
concurrent retire failing `begin_commit` lands at the fixed site). So I
probed the plan's NAMED sibling scenario — a **scheduling/installation
failure** — with a synthetic post-transfer `return
Err(AdmissionDenied::ShapeMismatch)` inserted after the opening-body block
(`cortex/rpc.rs:7564`) and a temporary probe test (appended → run → removed)
asserting the record settles. Result (executed): the probe reds at
`tests\org_rpc_streaming.rs:5290:5` (appended-probe numbering; the pristine
file is 5256 lines): `S2REV-PROBE: a post-transfer installation failure
must not orphan the Running record (the ConfirmedOpening scope-guard
precedent) / left: 1 / right: 0` — **the `Running` record IS orphaned
(`record_count` stuck at 1)**, exactly the F-S2.2-5 leak class, at a
different exit than the fixed one. With the unary guard shape this scenario
cannot orphan (the guard's Drop settles the record on any exit). Probe and
mutation both removed; tree sha-proven pristine (§11).

**Judgment:** correct at its site; scoped to every currently reachable
refusal; the CLASS stays open on the CS/DX seams (finding **F-S2R-2**, §7).
The regression witness is genuine (it fails pre-fix at its named assertion
under my own re-run).

## 5. Named inverse re-executions (8 of the 10 named + the disclosed single-layer cycle)

Cycle discipline per run: bounded diff at the PRODUCTION site → the named
red (verbatim; a compile error is never a red) → `git checkout --` restore,
`sha256sum` == baseline → restored green (same command). Exact command
shape: `cargo tf --retries 0 --test org_rpc_streaming -E 'test(=W)'` (or
`tfl` where noted), from `net/crates/net/`. Thread ids are run-specific.

Baselines at `017e7148a` (restore-equality shas):
`org_admission_gate.rs` `69b81aae4532ef73dc2b3364eb3a3ad15680b5c3cb088d630181368040601537`;
`cortex/rpc.rs` `4666855b90a8326316581af08e8f77584b577a901cbd674e933f52d4a5ee9998`;
`mesh_rpc.rs` `797e2954d6a9134748d31dacd0a7e79e767819ae1c5863ec7e78bff11ed7ce6c`;
`tests/org_rpc_streaming.rs` `51673d3b988819ac3b15603bb579a5513babd10fab69fc7271c0ff1fafc9e1d3`.
All four confirmed equal at end of round (§11).

### 5.1 R-S2.1 (row 2.1's prescribed inverse) — RED, verbatim

Mutation (`org_admission_gate.rs:85`, `org_request_digest`'s canonical
construction — the transcript skips the body digest):

```diff
-        body: req.body.clone(),
+        body: req.body.slice(0..0), // R-S2.1 MUTATION: skip the body digest
```

Run: `cargo tf --retries 0 --test org_rpc_streaming -E
'test(=client_stream_opening_binds_first_chunk)'` — **exit 100**. Verbatim
(pristine test-file numbering; mutation in another file, delta 0):

```
thread 'client_stream_opening_binds_first_chunk' (186876) panicked at tests\org_rpc_streaming.rs:3418:18:
altering the first chunk after signing must be refused with the TYPED `BindingInvalid` — the signed opening binds the first chunk; got Ok("Admitted")
```

Byte-identical to the report's R-S2.1 quote. Restore sha == baseline;
restored green 1/1, exit 0.

### 5.2 R-S2.2a (row 2.2's prescribed inverse: 3-tuple chunk keying) — RED, verbatim

Mutation (3 hunks in `cortex/rpc.rs`): the shared chunk-path key
(`:6840`) and the CS/DX admitted seams' sender-map insert keys (`:7568`,
`:8410`) drop the receiving-incarnation term.

Run: `-E 'test(=wrong_session_grant_does_not_release_credit)'` — **exit
100**. Verbatim (pristine numbering; delta 0):

```
thread 'wrong_session_grant_does_not_release_credit' (191564) panicked at tests\org_rpc_streaming.rs:4277:5:
assertion `left == right` failed: a wrong-session REQUEST_CHUNK is never delivered — the opening body is all the handler ever sees
  left: 2
 right: 1
```

Byte-identical to the report's R-S2.2a quote. Restore sha == baseline;
restored green 1/1.

### 5.3 R-S2.2b (the wrong-session grant's own inverse) — RED, verbatim

Mutation (`cortex/rpc.rs`, both grant arms' shared line `:6650`/`:8224`):
the lookup ignores the key.

Run: `-E 'test(=wrong_session_grant_does_not_release_credit)'` — **exit
100**. Verbatim (pristine numbering; delta 0):

```
thread 'wrong_session_grant_does_not_release_credit' (182228) panicked at tests\org_rpc_streaming.rs:4283:5:
assertion `left == right` failed: a wrong-session STREAM_GRANT does not release credit
  left: Some(4)
 right: Some(0)
```

Byte-identical to the report's R-S2.2b quote. Restore + green.

### 5.4 R-S2.2c (the F-S2.2-5 fix removed — the repair receipt) — RED, verbatim

Mutation (`cortex/rpc.rs:7550`, the CS seam's fix line removed — the
pre-fix shape).

Run: `-E 'test(=opening_body_budget_refusal_completes_the_record)'` —
**exit 100**. Verbatim (pristine numbering; delta 0):

```
thread 'opening_body_budget_refusal_completes_the_record' (192200) panicked at tests\org_rpc_streaming.rs:4455:5:
assertion `left == right` failed: the pre-supervisor refusal completes the record's single removal
  left: 1
 right: 0
```

Byte-identical to the report's R-S2.2c quote. Restore + green.

### 5.5 R-S2.3a (row 2.3's inverse: the grant arm removed) — RED, verbatim

Mutation (`cortex/rpc.rs`, the DX fold's `DISPATCH_RPC_STREAM_GRANT` arm):
`return Ok(());` inserted at the arm's entry.

Run: `-E 'test(=duplex_response_window_blocks_until_grant)'` — **exit 100**.
Verbatim (pristine numbering; delta 0):

```
thread 'duplex_response_window_blocks_until_grant' (191036) panicked at tests\org_rpc_streaming.rs:4576:5:
the grant releases the blocked protected output — echo + terminal complete
```

Byte-identical to the report's R-S2.3a quote (the block occurred and never
released). Restore + green.

### 5.6 R-S2.3b (row 2.3's inverse: the semaphore bypassed) — RED, verbatim

Mutation (`cortex/rpc.rs:8011`, the public duplex pump's credit):
`let pump_flow = flow_sem.clone();` → `None`.

Run: `-E 'test(=duplex_response_window_blocks_until_grant)'` — **exit 100**.
Verbatim (the red lands in the shared darkness helper `s15.rs`, its
numbering pristine — `s15.rs` unmutated):

```
thread 'duplex_response_window_blocks_until_grant' (185208) panicked at tests\org_rpc_streaming\s15.rs:202:9:
assertion `left == right` failed: public zero credit publishes nothing before a grant (C8): 1 frame(s) reached the roster subscriber — a protected response was fanned out
  left: 1
 right: 0
```

Byte-identical to the report's R-S2.3b quote. Restore + green.

### 5.7 R-S2.4a (END retires both halves instead of half-closing) — RED, verbatim

Mutation (`cortex/rpc.rs:6910`, the shared chunk path's END):
`record.lock().end_input();` → `record.lock().retire(StreamTerminalReason::Cancelled);`.

Run: `-E 'test(=upload_end_then_remaining_output_completes)'` — **exit 100**.
Verbatim (pristine numbering; delta 0):

```
thread 'upload_end_then_remaining_output_completes' (157380) panicked at tests\org_rpc_streaming.rs:5012:5:
the upload END closes the input half
```

Byte-identical to the report's R-S2.4a quote. Restore + green.

### 5.8 R-S2.5 (row 5's inverse: the mint shape flipped to Unary) — RED, verbatim

Mutation (`mesh_rpc.rs:8153`, `attach_signed_admission`'s mint call:
`call_shape` → `RpcCallShape::Unary`; one line for one line).

Run (exact receipt shape): `cargo tfl --retries 0
adapter::net::mesh_rpc::roster_fallback_tests::client_stream_and_duplex_mint_their_stream_proofs`
— **exit 100**. Verbatim (pristine numbering — the mutation is in the SAME
file but line-neutral, delta 0):

```
thread 'adapter::net::mesh_rpc::roster_fallback_tests::client_stream_and_duplex_mint_their_stream_proofs' (55204) panicked at src\adapter\net\mesh_rpc.rs:10764:14:
the minted bytes are a FULL streaming proof (strict decode): InvalidFormat
```

Byte-identical to the report's R-S2.5 quote (the strict streaming decoder
refuses the unary-format value). Restore + green.

### 5.9 The disclosed single-layer cycle + R-S2.4c-v2 (§4.2's pair) — green + RED

Single-layer (layer 1 only): **GREEN** (`retire_unblocks_both_directions`
1/1, exit 0) — the disclosed overdetermination, reproduced.
R-S2.4c-v2 (both layers): **exit 100**, verbatim (pristine numbering):

```
thread 'retire_unblocks_both_directions' (154704) panicked at tests\org_rpc_streaming.rs:5250:5:
assertion `left == right` failed: no chunk is published after the terminal — the late grant never resurrects the stopped pump
  left: 2
 right: 1
```

Byte-identical to the report's R-S2.4c-v2 quote. Restore + green.

**Receipt re-executions: 8 of the 10 named inverse receipts re-run VERBATIM
(R-S2.1, R-S2.2a, R-S2.2b, R-S2.2c, R-S2.3a, R-S2.3b, R-S2.4a, R-S2.5 —
each red byte-identical to its quote), R-S2.4c-v2 re-run VERBATIM as part of
the F-S2.4-1 adjudication (9 of 10 in total), the single-layer disclosed
green reproduced. Only R-S2.4b was not re-run (its target — the input
admission's queue-owner close — is adjacent to my G1′/G3 coverage).** All
cycles sha-proven restored with green legs.

## 6. Fresh lane-unused inverses (the attack — 4, requirement ≥3)

All at production sites none of the lane's ten receipts touched. The four
weakenings: **NONE applies to anything in this section** (no window moved,
no assertion relaxed, no witness deleted, no precondition "fixed"). Any
green = finding, named below.

### 6.1 G2 — the DX fold's request-vs-response window separation (RED — discriminates)

Mutation (`cortex/rpc.rs:8197` + `:8210`, line-neutral): the
`DISPATCH_RPC_STREAM_GRANT` arm also accepts the cross-direction kind
(`DISPATCH_RPC_REQUEST_GRANT`, decoded with `decode_request_grant`) — i.e.
the wrong-direction grant kind reaches the response arm and releases
response credit.

Run: `-E 'test(=cross_direction_grant_is_ignored)'` — **exit 100**. Verbatim
(pristine numbering):

```
thread 'cross_direction_grant_is_ignored' (188716) panicked at tests\org_rpc_streaming.rs:4802:5:
assertion `left == right` failed: a cross-direction REQUEST_GRANT does not release response credit
  left: None
 right: Some(0)
```

The named assertion fires (observed `left: None` rather than `Some(5)`:
the released credit let the parked pump complete its drain, after which the
single removal point dropped the window entry — the assertion detects the
release either way). Restore + green 1/1. **Discriminating.**

### 6.2 G4 — the lazy-mint's kind discriminator (RED — discriminates)

Mutation (`mesh_rpc.rs` in `sign_admission_proof`, line-neutral): the
streaming kind derivation is pinned to 2 —
`call_shape.stream_kind()` → `Some(2u8)`.

Run (the row-5 mirror, `cargo tfl`): **exit 100**. Verbatim (pristine
numbering; same-file mutation line-neutral, delta 0):

```
thread 'adapter::net::mesh_rpc::roster_fallback_tests::client_stream_and_duplex_mint_their_stream_proofs' (189828) panicked at src\adapter\net\mesh_rpc.rs:10805:9:
assertion `left == right` failed: the DX mint's kind is duplex (3)
  left: 2
 right: 3
```

The mint's kind discriminator is witnessed at the mirror's DX-kind
assertion (distinct from R-S2.5's decode red). Restore + green. **Discriminating.**

### 6.3 G3 — the half-close's input-Closed-on-return rule (GREEN — FINDING F-S2R-1)

Mutation (`cortex/rpc.rs:3216`, `handler_returned`, line-neutral): the
§2.6 rule "an open input half becomes `Closed` — its consumer is gone" is
removed (the input half stays `Open` at handler return).

Runs: `cargo tf --retries 0 --test org_rpc_streaming` → **41 run / 41
passed**, exit 0; `cargo tfl --retries 0 adapter::net::cortex::rpc
adapter::net::mesh_rpc org_stream` → **220/220**, exit 0. **GREEN across the
entire estate.** Weakening attribution: NONE of the four applies — there is
NO witness for this property at all (the negative-space gap class): no
witness drives a handler that returns EARLY (before the caller's END) and
then receives a late request chunk. §2.6's own words make the rule
load-bearing ("Late input after a legitimate early handler return is not a
resource-overflow failure: its input half is already closed, so it is
refused/discarded without replacing the handler's result") — without the
rule, the late chunk's delivery attempt fails at the dropped receiver and
`latch_exhausted()` turns it into a `ResourceExhausted` retirement,
precisely the resource-overflow failure §2.6 forbids. Restore + green.
**FINDING F-S2R-1 (part a).**

### 6.4 G1′ — the CS/DX chunk path's record gate ("checked against the record's shape and direction before delivery") (GREEN — FINDING F-S2R-1)

Mutation (`cortex/rpc.rs` `apply_request_chunk_to_senders`, line-neutral):
the record gate `if rec.terminal.is_some() || rec.input !=
StreamCallInput::Open { return; }` — row 2.2's "every CHUNK/END/CANCEL/GRANT
checked against the record's shape and direction before delivery/credit"
for the CHUNK direction — is neutralized.

Runs: `cargo tf --retries 0 --test org_rpc_streaming` → **41/41**, exit 0;
the three-module filter → **220/220**, exit 0. **GREEN across the entire
estate.** Weakening attribution: NONE applies — the gate is belt-only
behind the sender-map removals (at END and at `complete()`), which ARE
witnessed; the gate's own reachable window (sender alive + input not Open —
i.e. the early-handler-return drain window of §6.3) is never driven.
Restore + green. **FINDING F-S2R-1 (part b).**

### 6.5 The F-S2.2-5 sibling probe (§4.3) — the orphan executes

Synthetic installation failure (`cortex/rpc.rs:7564` post-transfer return)
+ a temporary probe test (appended → run → removed): **exit 100** at the
probe's own assertion `tests\org_rpc_streaming.rs:5290:5` (appended-probe
numbering; the pristine test file is 5256 lines):

```
thread 's2rev_probe_installation_failure_must_not_orphan_the_record' (193460) panicked at tests\org_rpc_streaming.rs:5290:5:
assertion `left == right` failed: S2REV-PROBE: a post-transfer installation failure must not orphan the Running record (the ConfirmedOpening scope-guard precedent)
  left: 1
 right: 0
```

**FINDING F-S2R-2.** Probe and mutation removed; restored green (the
F-S2.2-5 witness 1/1).

### 6.6 Attack summary

Four fresh lane-unused inverses: two RED at named assertions (G2, G4 — the
cross-direction negative and the kind discriminator both discriminate),
two GREEN across the entire estate (G3, G1′ — one unwitnessed §2.6 rule at
both implementing sites), plus the sibling probe whose red demonstrates the
F-S2.2-5 leak class is structurally open. Zero re-pins; zero weakenings.

## 7. Findings classification

All three findings: **P2** (witness-gap / fix-class — bounded
correctness-relevant rules on failure paths; no misbehavior is reproducible
at the pristine head). Sources cited pristine-at-`017e7148a`.

**F-S2R-1 (P2, witness gap; executed green-under-own-inverse ×2).** The
§2.6 rule "Handler return ⇒ … input = Closed if it was Open … no further
request chunks are admitted, delivered or retained" and its CHUNK-side
enforcement (the `apply_request_chunk_to_senders` record gate — row 2.2's
"checked against the record's shape and direction before delivery/credit")
are UNWITNESSED: both mutations (G3 at `cortex/rpc.rs:3216`
`handler_returned`; G1′ at the record gate in `apply_request_chunk_to_senders`)
stay green across `org_rpc_streaming` 41/41 and the three-module filter
220/220 (§6.3, §6.4). Trigger: a regression that lets late input after a
legitimate early handler return reach delivery — the delivery then fails at
the dropped receiver and `latch_exhausted()` replaces the handler's result
with `ResourceExhausted`, the exact outcome §2.6 forbids. The report's
F-S2.4-2 claim ("binds the full §2.6 table with its named pair") overstates
accordingly (record-quality note). Closure property: **a named witness in
which a PROTECTED client-streaming or duplex handler returns EARLY (before
the caller's END) with its input half still `Open`, a request chunk then
arrives for the call, and the observed outcome is: the chunk is
refused/discarded — never delivered, never retained — no `ResourceExhausted`
is latched, and the handler's own result remains the terminal (exact wire
content asserted) with the record completing exactly once — reddening under
(a) the input-Closed-on-return rule removed (`handler_returned`) and (b) the
chunk-path record gate removed.** (Belt discipline: the sender-map removal
at `complete()` must not be able to satisfy this witness alone.)

**F-S2R-2 (P2, production fix class; executed orphan demonstration).** The
F-S2.2-5 repair settles the record inline at the `deliver_protected_body`
refusal site (CS `cortex/rpc.rs:7549-7551`, DX `:8391-8393`) but the
post-transfer window has NO scope guard, although plan §3 step 5 mandates
one ("Scope guards handle scheduling/installation failure without orphaning
a `Running` record") and the cited unary `ConfirmedOpening` precedent
(`rpc.rs:5578-5597`) IS one ("this scope guard stands in for it so no
`Running` record is ever orphaned"). Executed: a synthetic installation
failure in the window orphans the record (`record_count` stuck at 1 —
§6.5's `left: 1`); today no OTHER exit is reachable in the window (every
statement examined; the window is fully synchronous), so this is a class
exposure on future paths and panics, not a reproducible head defect.
Impact boundary: an orphaned `Running` record pins its `(caller, call_id)`
forever (`ActiveCallOwned` for successors) and leaks one active-call quota
slot per occurrence; no authority bypass or data effect is shown. Closure
property: **the CS and DX `apply_inbound_admitted` post-transfer windows are
covered by a `ConfirmedOpening`-shape scope guard such that ANY exit after
`registry.confirm` (a delivery refusal, a scheduling/installation failure,
or a panic) settles the record's release-once `complete` and every map
entry — demonstrated by a witness in the shape of §6.5's probe (a
post-transfer installation failure) asserting `record_count() == 0` and an
empty `in_flight_keys()`, failing (orphan) without the guard and passing
with it; the F-S2.2-5 witness stays green throughout.**

**F-S2R-3 (P3, record quality).** `S1_REPORT.md` §4 F-S2.1-4's proof-method
parenthetical — "the reflow-only hunks are `git diff -w`-empty (no semantic
change)" — mis-states its own evidence method: line re-wraps survive
`git diff -w` as line-count changes (my `-w` diff shows the residue). The
SUBSTANCE ("pure formatting, no semantic change") is TRUE and now verified
by normalisation (§3). Closure property: state the normalisation proof
(whitespace-stripped body digests) instead of the `-w` emptiness claim, or
restore the original wrapping. No code impact.

## 8. Preserved credit — verified holds; do not rework

1. **The full estate reproduces at the pinned head** (executed): 41/41,
   134/134, 32/32, 220/220, 76/76 — byte-matching the report's §4.5 close
   (§2); roster 41 source == 41 executed == 41 CI-pinned.
2. **The 30-roster is preserved semantically byte-for-byte** (executed
   normalisation, §3) — no assertion, window, or value moved; the three
   re-wrapped S1_R witnesses are whitespace-identical.
3. **Row 5's delete-and-replace protocol held** (executed diff): the deleted
   content is exactly the two C11-removed intent-refusal legs; the kept
   halves are verbatim; the replacement mirror landed with its own
   discriminating inverse (R-S2.5 + G4); the CI floor was raised
   deliberately with the name list (`d214adc56`).
4. **Nine of the ten named inverse receipts re-executed VERBATIM this
   round** (§5) — each red byte-identical to the report's quote, each
   sha-proven restored, each closed with a green leg; the single-layer
   disclosed-green reproduced.
5. **The observation infrastructure is the proven S1 idiom, reused
   unchanged**: real bridge hand-off (`inject_inbound_for_test`), real
   transport to real endpoint recorders, the 200 ms / 10 ms darkness
   discipline, exact wire-tuple asserts, exact five-field attribution
   compares, content-identity (never counted) item asserts.
6. **Record quality of the Stage 2 report is high** (with F-S2R-3's
   exception): every number and quote I checked reproduced; the two
   disclosures are precisely scoped and honestly named in the report, the
   commit messages AND the plan Review log; the F-S2.2-5 defect is disclosed
   as found-and-fixed with a genuine fail-pre-fix witness.
7. **The Stage 1 acceptances, the §1.4 frozen gate, the recorded rulings and
   the prior packets' closed findings** stand as accepted there (not
   re-litigated).

## 9. HOLD rows and closure properties

**None — nothing is held.** This ACCEPT holds no rows. For the record, the
findings' closure properties are stated verbatim in §7 (F-S2R-1's early-
handler-return witness; F-S2R-2's scope-guard coverage; F-S2R-3's record
wording) — each is bounded witness/repair work at the named sites, none
reopens a row, the Exit paragraph, or the F-S2.2-5 instance.

## 10. Never executed here (complete)

- **Linux/macOS and `#[cfg(unix)]` legs** (Windows host only).
- **CI itself** (branch unpushed; nobody pushes but Main) — the floor
  re-pin `d214adc56` is record-inspected only; the stage-end validation
  chains A and B are taken as recorded-green per the assignment (verified
  where named — e.g. the `unused_mut` and rustdoc fixes at `017e7148a` are
  source-inspected and semantics-neutral — not re-litigated).
- **The clippy battery, rustdoc runs, `cargo check --workspace
  --all-targets`, `cargo fmt --all`** (the stage-end sweep; out of scope).
- **`cargo tl` / `cargo t` full suites** (the stage close's 7089/7089 is
  record-inspected, not re-run; my filters are the named binaries and
  modules).
- **R-S2.4b** (the one named inverse I did not re-run — its input-admission
  queue-owner target is adjacent to my G1′/G3 coverage; the report's quote
  stands as lane evidence).
- **The interim green runs at `4db0f8a5f` and the later slice commits**
  (the S2.1 reconstructed tree at `16cd67e85` I DID run; the others' 37/39
  content is subsumed by the head's 41/41 and their inverse receipts).
- **The lane's own probe-worktree runs as such** (my 16cd67e85 run is
  independent).
- **The `webrtc` feature graph**, the benches, the wasm32 runner, the
  Go/Python/Node bindings, `guards/org_api_probe`, the browser/SDK/facade
  surfaces (Stage 3+ by contract).
- **Live two-process transport outside the harness's real wire endpoints.**
- **Anything in the F-S1R-2 rider territory** (terminal retarget, caller-side
  last mile, delivery-exactly-once) — declined by the owner, out of scope;
  the §2.8 session-replacement terminal-drop limitation stands as documented
  in `S1_REVIEW_PACKET_2.md` §3.1.

## 11. Final-verification record

- **Pinned HEAD:** `017e7148a2100e9a28ad9dd7e0170c2ab2c7abf9` (session tree;
  probe worktrees `org-streaming-s2rev` @`017e7148a` and `org-streaming-s2rv16`
  @`16cd67e85`, all detached).
- **Diff-check:** `git status --short` EMPTY in the session tree and in
  s2rv16; in s2rev only the untracked `net/crates/net/target-s2rev/` build
  cache. Tracked files byte-pristine at the pinned head.
- **Mutations:** 15 mutation cycles + 1 appended probe test, EVERY one
  restored via `git checkout --` and sha-proven == baseline at end of round
  (`cortex/rpc.rs` `4666855b…`, `mesh_rpc.rs` `797e2954…`,
  `org_admission_gate.rs` `69b81aae…`, `tests/org_rpc_streaming.rs`
  `51673d3b…` — all four confirmed after the last cycle).
- **Probe removal:** the S2REV probe test (35 lines, appended → executed →
  REMOVED) and its synthetic-failure mutation removed; the worktree is
  byte-pristine.
- **Final restored-green:** `cargo tf --retries 0 --test org_rpc_streaming`
  → 41/41 exit 0 at the pristine tree (post-G3 restore), and the
  `opening_body_budget_refusal_completes_the_record` green leg post-probe.
- **Environment note (disclosed):** `CARGO_TARGET_DIR=target-s2rev` and
  `CARGO_INCREMENTAL=0` (the prior round's stated build-cache policy);
  C: at ~42 GB free at round start; no tracked file anywhere was touched by
  build-cache use.
- **Session-tree writes:** this packet only (written via bash heredoc — the
  harness `write` device is `xd://`-only and cannot write filesystem paths).
