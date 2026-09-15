# Stage 5 second-round repairs — Kyra's HOLD at `886064aba` (F1–F7 + residuals)

Packet: `C:/Users/chief/Downloads/KYRA_STAGE5_REPAIR_EVIDENCE/` —
`KYRA_STAGE5_REPAIR_REVIEW.md` governs; lane reports
`leaf-repair-review.md`, `leader-abi-repair-review.md`,
`native-repair-review.md`, `evidence-repair-review.md`; probes
`parent-followup.rs` (10 tests, 7 fail / 3 controls). Reviewer
reproduced 7/3 at `a020feb22` with identical names.

**Credit is substantial and must not be lost** (Kyra §"Original repairs
that must not be lost"): entity-derived announcement identity;
peer/incarnation/inner-route RPC ownership; live reliability
descriptors and ACK/grant/NACK callers; loss-free fragmentation;
replacement retirement; channel/stream classification; reorder
metadata; distinct candidates and read-time freshness; installed-leader
retirement; first-generation Attach; explicit follower deadlines;
TS decoding/options; IndexedDB commit; native handles bound to
incarnation with a real busy-install witness; unsupported signalling
now refuses. All 15 original probes pass (191 Rust, 158 TS). Chromium
**and** Firefox 20/20 at the reviewed head.

**First commit:** land `parent-followup.rs` verbatim as
`leaf/tests/kyra_followup.rs`, pinned by name in the leaf CI job beside
`kyra_review.rs`. Then one commit per item, prefix
`fix(net): stage 5 repair F-n —` / `… stage 5 repair L-n —` /
`… stage 5 repair N-n —`; `S5_REPORT.md` §11 with, per row, the
bounded source diff of the inverse, the behavioural RED, and the
restored positive — Kyra: "mutation descriptions are not raw inverse
receipts". Full pre-push checklist including the narrow-feature matrix
every push; report only on all-green exact-head CI. No Stage 6.

## Executed (all reproduced)

**F1 (P1) — subscribe then publish stalls on one sequence space.**
`node.rs:662–695, 1033–1055, 1105–1126`, `stream.rs:201–241`. The
membership frame consumes sequence 0 but only event-plane messages
advance the reorder cursor, so publication seq 1 is held forever.
Closure: one sequence-disposition model — every non-feedback
subprotocol that shares the stream advances the cursor when consumed;
feedback (ACK/grant/NACK) stays outside it. Do not exempt membership.

**F2 (P1) — a legitimate retransmission destroys the retained partial
group.** `node.rs:1033–1102`, `frame.rs:333–353`; native analogue
`rtc/fragment.rs:175–192`. Head delivered, its ACK and the tail lost,
sender retransmits both; the duplicate head passes AEAD, repeats its
ACK, then the exact-overlap fragment wipes the partial. Closure: a
duplicate fragment (same group, same index, identical bytes) re-ACKs
and is otherwise a no-op; conflicting bytes are a typed refusal;
capacity refusal and expiry of already-ACKed groups get explicit
terminal/recovery semantics. Fix the native analogue the same way.

**F3 (P1) — reliable reorder overflow silently loses data.**
`stream.rs:201–241`, `node.rs:1109–1129`. 66 reliable messages, deliver
1–65 then 0: 65 records from 1, no `StreamFailed`. Closure: reliable
overflow is either complete ordered delivery (grow within the
configured bound, apply backpressure via credit) or a **typed terminal
failure** of the stream — never a counter. Also (source): byte-credit
admission can accept more tiny packets than the retransmit-descriptor
bound (`session.rs:434–483`, `wire/src/reliability.rs:727–764`); bound
packets independently of bytes and probe it.

**F4 (P1) — a correct inner route authorizes the wrong carrier
channel.** `node.rs:1287–1312`, `rpc.rs:198–217`. A RESPONSE on an
unrelated channel with the expected reply route stamped inside completes
the call. Closure: completion binds to the **authenticated carrier
channel** *and* the inner route *and* the peer/incarnation, checked
before the entry is removed; define forwarded logical-origin semantics
explicitly rather than treating every transport anchor as the logical
publisher.

**F5 (P1) — a stale leaf stream handle sends through the successor.**
`node.rs:201–212, 707–740, 445–468`. Closure: the public handle carries
its opening incarnation and send/close refuse on mismatch (the same
shape as the native R12 fix); clearing internal maps is not enough for
a retained value.

**F6 (P2) — fragment groups accept conflicting provenance.**
`frame.rs:296, 333–413`. Closure: group-wide delivery metadata
(stream, origin, channel) fixed by the head and enforced on every
fragment; unique contiguous sequence ownership — min/max span cannot
claim an intervening non-fragment record.

**F7 (P2) — live replay protection evicted under capacity pressure.**
`signal.rs:184–248`, `node.rs:1389–1415`. Closure: at capacity refuse
**new** work (typed, counted), never evict an unexpired entry; or a
bounded scheme that keeps every unexpired entry (per-sender windows
keyed by `not_after` bucket). Keep the bounded-storage assertion.

## Source-established — leadership / TS / WASM (repair, witness)

- **L1 (P1)** close during promotion: `leader_session.rs:770–848` holds
  the granted lock and lease in the suspended factory stack; close
  cannot cancel or release until the factory completes. Closure: the
  bootstrap future is owned by `Shared`, cancellable, and the lock is
  released only after retirement (never over a live old bootstrap).
- **L2 (P1)** abrupt leader loss strands TS stream iterators:
  `session.ts:103–115, 245–256` ends queues only on explicit loss.
  Closure: end on generation change too; fence an `openStream` result
  that crosses the loss notification before registration.
- **L3 (P2)** announcement reconciliation: `announce(C)` replaces A∪B
  with C; empty-union does not withdraw the last follower's
  capabilities; union subscriptions leak into leader-local intent.
- **L4 (P2)** a `None` follower timeout stays unbounded; arm the
  default locally, keep explicit-timeout Indeterminate/no-retry.
- **L5 (P2)** `next_generation().await?` exits before the factory retry
  path; a surfaced abort must lead to recovery or a documented terminal
  state.
- **L6 (P2)** direct callbacks invoked under `Inner`'s mutable borrow
  (`wasm.rs:178–196, 273–277, 719–764`): synchronous `send`/`close` from
  `onMessage` reborrows. Closure: drop the borrow before invoking
  callbacks (collect, release, dispatch). The descendant's anchorless
  RefCell panic at fixture line 253 is in a fixture — diagnose it
  separately and honestly.
- **L7** deferred `ProxyStream.close` checks generation at spawn, not
  dispatch; document admission-return vs transport refusal vs ACK.

## Source-established — native (repair, witness; consumers untouched
except as stated)

- **N1 (P1)** UDP byte conservation: `mesh.rs:29679–29687, 40049–40063`
  — recognized control packets add receive-consumed bytes while
  native subprotocol send did not debit TX bytes. Closure: charge and
  debit symmetrically; witness: a prior uncharged control plus one
  withheld application packet, conservation asserted at a settled
  boundary.
- **N2 (P1)** same-session close race: `mesh.rs:43483–43490` drops the
  stream lookup before unconditional removal; a concurrent same-id
  reopen can be removed by the old handle. Closure: conditional
  removal keyed by the stream's actual lifetime (epoch **and**
  incarnation) under one guard, no guard across the graceful-close
  wait.
- **N3 (P2)** native partial groups not retired with the session
  (`rtc/fragment.rs:93–95, 143–168, 230–246`); expiry depends on a new
  group arriving; late tails bypass TTL; admitted old packets must not
  recreate retired groups.
- **N4 — compatibility gate:** core/SDK `close`/graceful-close
  signatures were replaced and the public `NetSession` epoch-admission
  API removed. Closure: add the handle-fenced APIs **beside** compatible
  id-addressed entrypoints (the old signatures delegating with the
  current-session incarnation, documented as unfenced) — or bring the
  owner an explicit versioned break. Do not weaken the fence.
- **N5** Go typed error: `go/mesh.go:57–98` maps -117 to a generic
  unknown error and `Close` discards status. Closure: a typed
  `ErrSessionSuperseded` sentinel + a parity test; Node/Python
  id-addressed close stays intentionally unscoped and says so.
- **Expiry boundary:** `announce.rs:113–123, 421–436` inclusive at
  exact expiry incl. TTL-zero current second; require exact semantics
  and a production authority-lookup witness.

## Evidence gaps (witnesses, not code)

- Pin every new native repair name and the lifecycle/storage schedules
  by exact name; upload the Firefox log.
- Browser reliability: add deliberate reliable loss/reorder and
  sustained native → leaf traffic beyond the credit window with
  replenishment observed (a native caller, not a packet fixture);
  both-direction large-message public-API witnesses.
- Real-package ABI: direct and proxied callbacks and option forwarding
  through the built package with caller-sensitive inverses, not
  hand-driven stream doubles.
- Retirement: keep the old page **alive**, park real production work,
  assert exact failure and generation, release delayed work after
  recovery, observe predecessor silence and native execution count.
- R14: correct D1/WASM docs (the adapter does **not** carry envelopes);
  pin the production `AnchorControlPlane` refusal, not the mock's.
- The `hedge_loser_handler_observes_cancellation` SDK failure at the
  reviewed head (`mesh_rpc_hedge.rs:290`, 3/3 attempts, file unchanged):
  diagnose causality against Stage 5's core changes; neither "flaky"
  nor assumed regression until shown.
- Anchorless `wasm_anchorless.rs:647` ("nothing on the direct session
  should have been dropped", 1 ≠ 0) plus the fixture RefCell panic at
  :253 — diagnose without weakening the assertion.

## Validation

Kyra's 10 follow-up probes + 15 originals green as committed; leaf
native + wasm runner + both engines green in CI with the new names
pinned; native RTC binaries and listener green; narrow-feature matrix;
Go/Python/Node bindings tests; `--lib` floors; export checker
(constants are not symbols — 568 should hold); consumer diff file by
file with the N4 disposition stated. Return the final SHA, clean
status, the CI URL and §11.
