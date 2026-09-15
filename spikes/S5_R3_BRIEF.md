# Stage 5 third-round repairs — Kyra's HOLD at `b66752643`

Packet: `C:/Users/chief/Downloads/KYRA_STAGE5_ROUND2_b66752643/`
(`KYRA_STAGE5_ROUND2_REVIEW.md` governs; `leaf-review.md`,
`leader-review.md`, `native-review.md`, `evidence-review.md`;
`parent-round2-probes.rs` 5 tests; `leader-pin-audit.json`). Reviewer
reproduced 3/2 at `b66752643`.

**Credit widened:** all 25 reviewer probes green; F1/F4/F7 original
cases closed; L2/L4/L5/L6/L7 and N2/N4/N5 credited; wasm_leaf 15/15,
anchorless 2/2, leader 24/24; Chromium **and Firefox 21/21**. The one
CI red is the invented leader roster (18 of 24 pinned names do not
exist) — the item already dispatched; land it **first**, from the
file, with a pre-run self-check that fails on any pinned name absent
from the file (same for wasm_leaf / anchorless / native / browser
rosters, and raise the stale floors 22/4 and 19 to the real counts).

Then `parent-round2-probes.rs` verbatim as
`leaf/tests/kyra_round2_review.rs`, pinned. One commit per item,
prefix `fix(net): stage 5 repair P-n —` / `… X-n —`; `S5_REPORT.md`
§12 with **raw** inverse receipts (Kyra: "mutation descriptions … are
not complete attributable diffs, commands, exits and restored logs" —
supply the source diff, the exact command, its exit, and the restored
run's output, per row; existing artifacts by path). Full pre-push
checklist; report only on all-green exact-head CI. No Stage 6.

## Executed (all reproduced)

**P1 (P1) — an acknowledged fragment's expiry silently abandons
delivery.** `node.rs:604, 1204–1242`, `frame.rs:344–347, 391–396,
439–454, 557–564`. Head fragment received and ACKed (sender retires its
descriptor); receiver clock → TTL+1; tail arrives and is ACKed into an
orphan group; `events=[]`, no `StreamFailed`. Closure: an acknowledged
group is owned — expiry/capacity/malformed cleanup of a group with any
ACKed piece must either keep it deliverable (retain ACKed pieces past
TTL while the stream is live) or emit a typed terminal disposition for
that stream; a tail can never be ACKed into a group that no longer has
its head. TTL-1 control stays green.

**P2 (P1) — same-session close/reopen strands the peer's next
sequence.** `node.rs:795–824, 845–858`. Close deletes the consumer
cursor while wire receive state and the peer's TX sequence stay live;
reopen waits for 0, the peer sends 1. Closure: either close retains the
receive cursor for the stream's lifetime within the session (reopen
resumes at the peer's next sequence) or reopen of a closed id is a
typed refusal — pick one, document it, keep the other TX half intact.
The WASM `close` makes this public.

**P3 (P1) — control overdraft forgotten on a partial grant.**
`wire/src/session.rs:1743–1753, 1826–1878`; native caller
`mesh.rs:551–568, 40201–40214`. 100-byte window, 100 app bytes
outstanding, 30-byte unconditional control debit floors credit at 0,
a 30-byte grant refunds application credit that is still owed.
Closure: retain overdraft as debt (`overdraft: u64`) retired before
application credit reopens; grants pay debt first; keep the
below-window repair; never put feedback behind the window it refills.

## Source-established — leaf

- **X1** RESET leaves the old receive lifetime's partial groups
  (`node.rs:1517–1525`): retire the stream's fragments on reset.
- **X2** `send_subprotocol` bypasses the `send_failed` terminal guard
  (`node.rs:618–732`): terminal disposition at the shared admission
  path, not only on handles.
- **X3** `PieceMeta` lacks subprotocol/reliability provenance;
  completion takes them from the finishing packet (`frame.rs:152–160`,
  `node.rs:1278–1285, 1319–1333`): bind them group-wide like
  stream/origin/channel.
- **X4** per-session retransmit stamp cap (1 024, `session.rs:58,
  534–574`) can evict admitted ownership across streams: stamps sized
  to the sum of per-stream descriptor bounds, or admission refuses
  before the stamp table would.

## Source-established — leadership

- **X5** cancelling the Rust bootstrap does not close partially created
  RTC resources before lock release: `wasm::LeafNode::connect`
  (`wasm.rs:318–442`) cleans only error branches; the offering
  transport holds a strong cycle (`rtc.rs:159–175, 537–551`). Closure:
  a drop guard on the connect future that runs `close_all` on
  cancellation, before the lock is released.
- **X6** an unchanged follower announcement overwrites the union
  (`leader.rs:1285–1295`, `leader_session.rs:1303–1331`): route follower
  declarations through the authoritative union publisher only.
- **X7** proxied synchronous sends hold `Shared.server` while the pump
  can emit another stream's terminal event that reborrows it
  (`leader_session.rs:586–592, 1363–1365, 1514–1525, 1809–1815`):
  release/defer the server-facing broadcast; never suppress the event.

## Source-established — native

- **X8** the pre-send control debit has no refund for
  never-admitted work (`mesh.rs:40209–40214`): lifetime-scoped refund
  on refusal/cancellation; never refund transport-owned loss.
- **X9** native group retirement not atomic with ingress
  (`rtc/fragment.rs:170–239`): one guard for check + insert
  (marker-then-remove under the same lock), bounded tombstones.
- **X10** replacement installer (`mesh.rs:23333–23408`) and dead-peer
  sweep (`31022–31068`) skip reassembly cleanup: retire on every
  lifetime end.
- **X11** native fragment metadata absent (`Partial`/`accept` cannot
  bind stream/channel/origin/sequence/reliability) and native
  ACK-before-capacity/expiry lacks complete-or-terminal disposition:
  mirror the leaf's F6 + P1 repairs on the native side.

## Expiry — an owner question, not a silent change

`announce.rs:144–146` pins `now_secs <= floor(ts_ns/1e9) + ttl`
(TTL-zero authority within the issuing second); native
`CapabilityAnnouncement::is_expired` expires at `age_secs >= ttl`
(TTL-zero expired at age zero). Opposite outcomes. Do **not** flip
`<`/`<=`: either match native exactly (nanosecond precision,
`age >= ttl`) or write a one-paragraph divergence proposal for the
owner in §12. Reviewer's default: match native.

## N4 residue — owner disposition needed

`StreamStats` struct literals and exhaustive `StreamError` matches are
explicit source-compatibility breaks for downstream Rust (new fields /
variants). Options for the owner, stated in §12: `#[non_exhaustive]` +
a constructor (a break now, none later) or a minor-version bump with a
release note. Do not call it additive.

## Evidence (witnesses, artifacts, claims)

1. Rosters/floors as above; upload the Firefox log.
2. Native control oracle: "empty pending ∧ `max_consumed_seen ==
   tx_bytes_sent`" does not distinguish ACK from give-up — observe the
   real ACK frontier and the terminal/reset disposition; keep the
   flaky zero-retransmit assertion retired.
3. Retirement: the quiet-peer WASM test still uses a custom backend —
   production shutdown/cancellation-sensitive evidence with exact
   generation equality, not substring matching.
4. ABI: real direct/proxied stream callback/iterator/options through
   the built package (no hand-driven inner stream objects); sustained
   native → leaf traffic beyond the window; deliberate reliable
   loss/reorder; both-direction large-message public APIs.
5. R14/D1: correct the docs (the adapter does not carry envelopes);
   pin the production `AnchorControlPlane` refusal.
6. Uncharged event-batch producers predate Stage 5 (Kyra verified) —
   document the scoped byte-conservation limitation; do not attribute
   to N1.
7. Anchorless / mDNS / native-control fixture diagnoses: supply the raw
   traces the §11.8 causality claims rest on, or restate them as
   inference.

## Validation

All 30 reviewer probes green as committed; rosters verified by the
pre-run self-check; leaf native + wasm + both engines; native RTC
binaries and listener; narrow matrix; bindings; `--lib` floors; export
checker; consumer diff with the N4 disposition. Final SHA, clean status,
CI URL, §12.
