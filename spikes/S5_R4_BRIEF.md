# Stage 5 fourth-round repairs — Kyra's HOLD at `f50454106`

Packet: `C:/Users/chief/Downloads/KYRA_STAGE5_ROUND3_f50454106/`
(`KYRA_STAGE5_ROUND3_REVIEW.md` governs; five lane reports;
`parent-round3-probes.rs`, 12 tests, 8 fail / 4 controls). Reviewer
reproduced 8/4 at `f50454106`. Exact-head CI **55/55**; Chromium and
Firefox **26/26**; hosted native 5 805 units + 119 RTC; all 30 prior
probes preserved and green. Credit is now most of the stage: P1/P2/P3
originals, X1–X10 substantially, X6/X7 closed, real ABI traffic,
sustained native → leaf across eight credit windows, real above-SCTP
loss/reorder recovery, native oversize refusal, production signalling
refusal.

**Record correction, first commit:** `477bc345e` and `S5_REPORT.md`
§12.2 attribute the N4 `#[non_exhaustive]` decision to "the owner".
No owner ruling was given — the owner was paused; the brief asked for
a §12 write-up of options. Kyra credited the policy on its merits
("accepted in principle … release/version work owner-managed"), and
the reviewer would have recommended it, so the **code stands**; the
**attribution does not**. Rewrite §12.2 and the commit's rationale in
the report as "implementer's choice, reviewer-endorsed, pending owner
confirmation"; never again attribute a decision to the owner that the
owner did not make. The expiry question (§12.1) remains genuinely
open and is stated correctly.

Then `parent-round3-probes.rs` verbatim as
`leaf/tests/kyra_round3_review.rs`, pinned. One commit per item,
prefix `fix(net): stage 5 repair R3-n —`; §13 with raw receipts. Full
pre-push checklist; report only on all-green exact-head CI. No Stage 6.

## Executed — the reliable/FAF mode boundary (R3-1..4 are one design)

The round-3 "first-open-wins promotion" made a stream reliable once a
reliable handle opens on it, but left three owners disagreeing about
what the boundary means: wire ACK accounting, the consumer reorder
cursor, and still-open FAF handles.

- **R3-1 (P1)** promotion cumulatively ACKs a missing reliable
  sequence: FAF 0, reliable 1–9, withhold 1 → `ack=10`, `delivered=[]`,
  no terminal. Correction from Kyra: a *late* copy of 1 still delivers
  (control passes) — so the wire refuses the sequence but the consumer
  path accepts it; the loss case is what is red. Closure: no ACK may
  claim an unreceived reliable sequence; the promotion boundary is an
  explicit, signalled sequence (the first reliable sequence is stamped
  as the boundary in the packet header, or a mode-change control
  frame), not an arrival-count heuristic.
- **R3-2 (P1)** a genuinely lost FAF prefix strands the reliable
  suffix: FAF 0, lose FAF 1, reliable 2–9 → nothing delivered. Closure:
  the consumer cursor advances past FAF sequences the wire has conceded
  (FAF sequences below the boundary are skippable), reliable ones are
  not.
- **R3-3 (P1)** mixed-mode with all fragments arriving discards the
  reliable message: the still-valid older FAF handle sends seq 3
  between head 1 and tail 2; the FAF consumer skips ahead, then calls
  the assembled reliable message a duplicate. Closure: after promotion
  every producer on the stream is reliable (older handles inherit the
  mode — their FAF flag is a *request*, the stream's mode is the
  contract), or FAF sends on a promoted stream are refused typed.
  Reliable fragment obligation applies at **admission** of the head.
- **R3-4 (P2)** an expected FAF fragment loss permanently kills the
  stream (`StreamFailed(ReassemblyAbandoned)`). Closure: P1's
  complete-or-terminal rule applies to **reliable** groups only; an
  abandoned FAF group is dropped and counted, the stream continues.

Design it once (a `StreamMode` on the receive side with a boundary
sequence; consumer, wire and producers all read it), then all four
probes plus the three controls.

## Executed — wire credit ownership

- **R3-5 (P1)** a predecessor control debit rewinds its successor:
  implicit streams reuse epoch 0, so an uncommitted debit guard from
  the closed predecessor refunds the successor's committed bytes and
  reuses its sequence (`(30,65506,1)` → `(0,65536,0)`). Closure: every
  replaceable stream gets a unique lifetime id at creation (implicit
  included); the debit guard holds that id and refunds only if it
  still matches. No map guard across the transport await.
- **R3-6 (P2)** a large cumulative grant strands control debt: the
  consumed delta is truncated to `u32` before debt repayment. Closure:
  settle debt from the full `u64` consumed amount, then clamp the
  application-credit representation.

## Executed — classification

- **R3-7 (P2)** application bytes beginning `0x13` with the channel's
  route at 24..32 are still classified as RPC (`UnknownCall`).
  Closure: classify from **plane ownership** — an RPC frame is one the
  RPC plane sent (a registered call id in the pending table, or the
  RPC subprotocol id on the frame), never from payload shape; opaque
  channel bytes are always `ChannelMessage`. Keep the reply-route /
  carrier checks for frames that *are* RPC.

## Source-established — native (P1/P2)

- **NR2 (P1)** native `take_abandoned()` has **no production
  consumer**: abandonment is diagnostic only; a first-piece capacity
  refusal creates neither a record nor a fence; later tails form
  headless groups. Closure: the RTC ingress drains abandonment into a
  stream failure / RESET for that stream (mirror the leaf's production
  drain), fences the group id after refusal, and idle expiry runs on a
  timer, not only on later fragment traffic.
- **NR3 (P1)** retirement markers expire (2 s) or are churn-evicted
  while a captured session Arc/frame is still admitted; shutdown stops
  the close consumer before later driver-drop notifications and does
  not retire mesh-owned partials. Closure: bound capture-to-dispatch
  (a frame captured under a retired incarnation is refused at
  dispatch, not by marker lookup); retire partials on shutdown.
- **NR4 (P1)** the new native reorder holds reacquire by stream id
  without epoch, so close/reopen between receipt and hold insertion
  lets an old frame into the replacement's buffer; ACK-owned holds
  leave via eviction without terminal disposition. Closure: carry the
  epoch (and incarnation) from receipt to hold insertion; eviction of
  an ACK-owned hold is a typed terminal for that stream.
- **NR6 (P2)** native byte coverage lacks contiguous
  sequence-to-offset provenance; RESET does not retire native groups
  for that receive lifetime. Mirror the leaf.
- **L5 (P2)** terminal leaf sends leak their hard stamp reservation:
  exhausted streams latch failure without retiring stamps. Closure:
  the terminal path retires the stream's stamps with the exact owner;
  the cap stays.

## nRPC handler-entry serialization — dispose, do not extend

`cortex/rpc.rs:2072–2146`: the transport reorder repair is warranted;
the **first-poll serialization of handlers** is a separate semantic
expansion (one long first poll holds every successor; `prev.await`
does not race cancellation/deadline/shutdown, so a cancelled queued
request can later enter its handler; the sweep constant does not
bound task-owned queued requests; Go/Python adapters enqueue before
Pending, so it proves nothing cross-language). Kyra's recommendation,
which the reviewer adopts: **remove the per-source handler
serialization** in a bounded change and measure ordering at the
transport/dispatch boundary; if you believe application-level
serialization is wanted, write it up as an owner question with the
bounded-ownership + cancellation design — do not ship it as a side
effect of an oracle.

## Error parity

C `-118 EventTooLarge` has no Go arm (`unknown error`); C headers
promise a detail string the send functions do not expose; size/limit
discarded. Closure: Go sentinel + parity test; expose the limit in the
typed error across C/Go/Node/Python without generalizing the preflight
to paths that bypass `send_on_stream`.

## Evidence corrections

1. Pin the six unpinned native repair names (`evidence-review.md` §3);
   routed floor/names.
2. Roster checker: state honestly that it is a lexical preflight, and
   that it does not run before every suite; keep the exact-run checks.
3. ABI label checks: observe the forwarded label at the inner call.
4. Leaf oversize witness: require the exact `EventTooLarge` kind, not
   any error.
5. Large-message witness: proves near-ceiling + refusal, **not**
   successful multi-fragment interoperability in both directions —
   supply that leg or an explicit accepted API bound (owner question).
6. Retirement: production `ConnectGuard` arming and actual
   `NodeBackend` shutdown, not a custom backend; X7 follower broadcast
   observed.
7. Browser pending-error generation: exact equality, not `contains`.
8. The below-SCTP drop hook has no caller: either land the A/B run or
   remove the hook.
9. Missing artifacts: `suite-3x.log`, the P3 lane receipt, the
   old-oracle scratch source, intermediate-tree provenance.
10. Backoff arithmetic: the ladder sums to 7.15 s, not 5.15 s; derive
    from `give_up_horizon`.

## Owner questions (state, do not decide)

- Announcement expiry: still open; recommendation unchanged — match
  native precision and inclusive expiry incl. TTL-zero/fractional.
- N4 policy: confirm `#[non_exhaustive]` + `empty()` (see correction
  above) and the version/release handling.
- Direct `BrowserNode.close` iterator lifetime (pre-existing: direct
  wrappers are not ended by parent close).
- Whether above-one-event fragmentation in both directions is a Stage
  5 contract or an explicit bound.

## Validation

All 42 reviewer probes green as committed; rosters; leaf native + wasm
+ both engines; native RTC binaries; narrow matrix; bindings incl. the
Go `-118` parity; `--lib` floors; export checker; consumer diff. Final
SHA, clean status, CI URL, §13.
