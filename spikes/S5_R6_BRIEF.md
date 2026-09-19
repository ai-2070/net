# Stage 5 sixth round — Kyra's fourth and fifth HOLDs (2026-09-17)

Packets, both governing:

- **Fourth review**, HOLD at `516a45c33`:
  `C:/Users/chief/AppData/Local/hermes/cache/webrtc-stage5-round4-516a45c33/KYRA_STAGE5_ROUND4_REVIEW.md`
  (four lane reports; `parent-round4-final.rs` 10 tests, 4 fail / 6
  controls; `native-mechanism/` wrapper 8 sessions green / 9 sessions
  red). **This packet was never forwarded** — the round-5 brief and
  report said round 4 was unreviewed. That was false and is corrected
  in both files; the omission is the record-keeper's, not yours.
- **Fifth review**, HOLD at `60e120609`:
  `C:/Users/chief/Downloads/KYRA_STAGE5_ROUND5_EVIDENCE/KYRA_STAGE5_ROUND5_REVIEW.md`
  (three lane reports; the unchanged round-4 packet re-executed with
  the same 4/6 and 72/64 results; `parent-close-probe.mjs` PASS/FAIL).

Credit already earned and to be preserved (her §2 in both): the R3
boundary design, L5, epochs/credit, classification, native
abandonment/heartbeat/epoch work, first-poll serialization removed, the
Go `-118` mapping, the eight missing pins, the four owner rulings, the
fragmentation sender with its two-factor gate and the 64,832 shared
ceiling. Her two exact-head CI reds at `60e120609` are already green
via `aa925dd6b`, `795c208b1`, `683735478` (CI green `0772a8745`); she
has not reviewed those — say so in the report, do not claim closure.

**Reproduced at `6d0896f8f` (code == `0772a8745`) by the record-keeper
before this brief was written:**

| Lane | Her result | At head |
|---|---|---|
| `parent-round4-final.rs` | 6 pass / 4 fail | **6 / 4, same four names, same messages** (`ACK 0` vs `2` on R4-2) |
| native wrapper, 8 sessions | 64 owed / 64 retained | 64 / 64 |
| native wrapper, 9 sessions | 72 owed / 64 retained / 8 lost | **72 / 64 / 8** |
| `parent-close-probe.mjs` | PASS false / FAIL true | **PASS / FAIL**, `bResult:"pending"`, `parentClosed:false` |

The leaf packet needed the S6-01 establishment-proof plumbing in its
`pair()` helper — applied exactly as `76c2ca8cc` applied it to the four
earlier reviewer suites, helper only. It is already in the tree at
`leaf/tests/kyra_round4_parent.rs`, uncommitted; the verbatim original
is `spikes/kyra/kyra_5_round4_parent_probes.rs`.

## Order of work

1. **Commit the landed probes first, red.** `leaf/tests/kyra_round4_parent.rs`
   as-is (10 tests; 4 red at this commit). The native wrapper
   `spikes/kyra/kyra_5_round4_native_terminal_wrapper.rs` becomes a
   pinned test in the RTC binary that owns `rtc/fragment.rs` — same
   property, same counts (8 sessions → 64/64; 9 → 72/72 required), not
   a rewrite; if the `#[path]` trick is not needed inside the crate,
   drop it and call the module directly, keep the assertion message.
   `spikes/kyra/kyra_5_round5_close_probe.mjs` becomes a `browser-ts`
   vitest against the bundled `BrowserNode` with her injected inner
   objects — both values, `throwFirst=true` asserted to end the second
   iterator and close the parent. Preservation: the 42 earlier reviewer
   tests stay byte-identical to `516a45c33`'s copies except for the
   S6-01 helper hunk already reviewed by her as such.
2. One commit per item below, prefix `fix(net): stage 5 repair R4-n —`
   / `R5-xx —`. §13 of `S5_REPORT.md` gains a round-6 section with raw
   receipts (applied diff, command, exit, restored output, source SHA)
   — her round-5 evidence item 4 is explicit that author-reported
   excerpts and unretained fragmentation receipts do not count.
3. Full pre-push checklist; report only on all-green exact-head CI.
   No Stage 6, no Stage 7, no version bump.

## Executed defects (R4-1..R4-5) — rerun her packet green

- **R4-1 (P1) native producers ignore the mode boundary.**
  `kyra_native_flags_cannot_false_ack_lost_reliable_head`: native FAF 0,
  lose reliable 1, old-handle FAF 2, reliable 3 → ACK 4. Native send
  takes reliability per handle (`mesh.rs` around `44786–44838`), emits
  only RELIABLE/NONE, registers no descriptor for the still-FAF
  handle; the shared receiver's fallback (`wire/src/session.rs:2374–2385`)
  has no boundary statement to work from. The opposite schedule — FAF
  0, lose FAF 1, native reliable 2 onward — strands the consumer on an
  unrebuildable prefix. **Outcome:** every producer of the shared
  sequence space obeys the leaf's boundary/inheritance contract (the
  signalled boundary and old-handle inheritance you built in round 4 —
  apply it to the native producer), or incompatible simultaneous use is
  refused typed before success. No inferred ACK concessions; batching,
  backpressure and UDP untouched.
- **R4-2 (P2) RESET keeps the old receive boundary.**
  `kyra_reset_replaces_receive_boundary_not_send_half`: reliable 0 with
  boundary 0; `reset_rx_stream`; fresh lifetime's reliable 1 with
  boundary 1 after lost FAF 0 → ACK 0 not 2, TX correctly still 1.
  `reset_rx_stream` (`wire/src/session.rs:1319`) resets cursors and
  ranges but not `rx_mode_boundary` / `rx_boundary_signalled`, so
  `promote_rx` ignores the new statement. **Outcome:** boundary
  authority follows the reset receive lifetime; send ownership stays
  independent.
- **R4-3 (P2) a duplicate of a completed fragmented message creates a
  phantom terminal.** `kyra_completed_fragment_duplicate_does_not_terminal`:
  deliver a two-fragment reliable message once; withhold feedback; the
  sender rebuilds fresh-counter retransmits; deliver only the duplicate
  head; return the repeated ACK; expire the receiver's new partial →
  `StreamFailed{ReassemblyAbandoned}` and the later `after-complete`
  payload is lost. `leaf/src/node.rs` continues wire-refused sequences
  into reassembly; completed groups leave no completed-owner memory in
  `leaf/src/frame.rs`; the duplicate head opens a fresh partial;
  expiry terminals it. **Outcome:** duplicates of completed messages
  create no new abandonment obligation. Not a blanket drop of every
  wire-refused packet — her delayed-boundary and still-open-partial
  controls pass today and must keep passing.
- **R4-4 (P2) an admitted application stream is silently shadowed by
  RPC ownership.** `kyra_rpc_carrier_cannot_silently_shadow_admitted_stream`:
  make and expire a real call, derive its reply carrier, open that
  explicit application id on both peers — `open_stream` succeeds, then
  ordinary opaque bytes come back `Dropped{UnknownCall}`. The unowned
  carrier control passes. **Outcome:** resolve contradictory owners at
  registration (a typed reservation conflict on `open_stream` is
  enough) or an unambiguous coexistence rule. Successful admission
  followed by silent plane priority is the defect; arbitrary traffic on
  reserved carriers is not required.
- **R4-5 (P1) one expiry pass drops terminal owners.** Nine sessions ×
  eight tiny incomplete reliable groups; one `expire` → 72 distinct
  owners, 64 terminals (`rtc/fragment.rs:1188` `terminals.pop_front()`
  on the authoritative queue). **Outcome:** every distinct outstanding
  terminal owner is retained or safely coalesced within a bounded
  design (bound *admission* — refuse the 65th outstanding group with a
  disposition — or coalesce per owner; a diagnostic ring may be lossy,
  complete-or-terminal ownership may not). "The consumer stopped
  draining" does not cover loss inside one producer call.

## Source-established (R4-6..R4-10) — coupled production witnesses required

Each needs a witness at the real admission/disposition boundary that
is red before and green after; her lane reports carry the schedules.

- **R4-6 (P1) native receive abandonment resets the opposite
  direction.** `mesh.rs:395–407` resets local RX and queues a receive
  terminal regardless of the group's reliability; `30901–30943` sends
  it as the same `StreamReset` used for local TX exhaustion; both
  receivers treat that frame as "peer's send half ended" and clear
  their own RX. So B→A reassembly loss resets healthy A→B progress at
  B, and ordinary FAF partial loss takes the same path. The new
  fragment sender's comment cites the leaf's reliable guard; native
  disposal does not have it. **Outcome:** settle the losing receive
  owner, preserve reverse-direction progress, preserve FAF loss
  semantics. Reusing one frame type is not directional coherence.
- **R4-7 (P1) a predecessor fragment terminal destroys its successor.**
  Fragment provenance/terminal identity is session + stream id, no
  epoch (`rtc/fragment.rs:209–257,329–346`). Close/reopen replaces the
  wire epoch without retiring the old group; later expiry resolves the
  current stream by id and resets/retires it. Sequential, no race.
  **Outcome:** exact owner identity (epoch) through admission,
  retirement and terminal consumption; predecessor cleanup never acts
  on current-by-id state.
- **R4-8 (P1, carry-forward) retired check precedes guarded
  insertion.** The permanent retired check (`mesh.rs:~37625`) sits
  before the actual reassembler admission; pause after it, retire the
  session, resume after the bounded marker expires → `accept`
  recreates old-session state. The landed pause seam is before the
  last check and misses this interval. **Outcome:** the non-expiring
  lifetime authority governs at the guarded insertion itself; no lock
  across awaits; a pre-lock check nearby is not sufficient. This is
  obsolete-owner admission, not cross-session byte merge — say so.
- **R4-9 (P1, carry-forward) a positively ACKed frame loses ownership
  at eviction-before-hold.** Ingress accepts out-of-order reliable 1
  under E and releases the guard (`mesh.rs:30427–30563`); feedback can
  emit the positive SACK; cap eviction removes E with an empty reorder
  buffer so no terminal registers (`wire/src/session.rs:1498–1505,
  1544–1559`); `hold_in_order_frame` refuses absent E and the caller
  only logs. **Outcome:** every positively acknowledged accepted frame
  retains delivery-or-terminal ownership across that interval. The
  witness must deliver the SACK, not merely observe local acceptance.
- **R4-10 (P1) peer-addressed streams erase receive ownership before
  callbacks — owner ruled 2026-09-17: option (b).** `StreamData`
  (`leaf/src/node.rs:78–86,199–205`) carries `stream_id`, `seq`,
  `payload` and no peer; every direct WASM wrapper filters the node-wide
  event vector by numeric id alone (`leaf/src/wasm.rs:2411–2424`) and
  so does TS (`browser-ts/src/stream.ts:182–194`). Two wrappers with
  the same id on distinct peers both receive one peer's payload; a page
  that echoes amplifies it (`S6_REPORT.md` §6.1). **Ruling:** put
  `peer_node` (and the session incarnation) on `StreamData`, key the
  WASM and TS filters on `(peer, stream_id)`, additive field on the
  page-facing event. Not (a) folding the peer into the id derivation,
  not (c) refusing the second peer. Witness: B with streams to A and C
  under one label; exact nonce correlation per inbox, not counts; the
  disclosed pair-specific-label workaround in the Stage 6 rows stays as
  it is (it is not the fix and must not be presented as one).

## Round-5 additions

- **R5-N1 (P1) concurrent sends can build a non-contiguous fragment
  group.** `flush_stream_fragment_group` sequences each piece through a
  separately awaited `flush_stream_batch` (`mesh.rs:~45177–45206`,
  yields on byte credit / delivery pressure at `~45006–45063`); a small
  concurrent send can take sequence 1 between A's pieces 0 and 2; both
  succeed; both receivers require contiguity and native rejects that
  exact shape (`rtc/fragment.rs:1052–1074`); retransmission preserves
  it. The comment at `mesh.rs:44716–44720` calling this "the same
  guarantee as batching" is not a repair. **Outcome:** whole-group
  sequence ownership (reserve the range before the first piece is
  committed, or serialize group flushes per stream) or refuse
  unsupported composition typed before success. No new guard held
  across awaits. Witness: two concurrent sends on one stream, one
  fragmented; receiver delivers both intact.
- **R5-N2 (P1) an oversized send overruns descriptor headroom.**
  `send_on_stream` checks `can_send()` once; each fragment then takes
  byte admission only; `ReliableStream::on_send` evicts the oldest
  unacknowledged descriptor at capacity (`wire/src/reliability.rs:
  1053–1090`) with no disposition. With 31 pending single-packet sends
  and `window_bytes(0)` (capacity 32), a two-piece event passes the
  gate then needs 33 slots; the second fragment evicts a lost head;
  later SACKs empty the queue while the receiver waits for that head.
  **Outcome:** descriptor admission for the whole group before the
  first piece commits, or a typed refusal; untracked eviction is never
  a successful send policy. Keep the unbounded byte-window option.
- **R5-G1 (P2) Go fabricates the limit.** `go/mesh.go:167–179`
  reconstructs every `EventTooLarge` from the fixed 8,104 accessor and
  the first payload above it. A tagged-RTC 64,833-byte send reports
  limit 8,104; a tagged batch `[9000, 64833]` refused for the second is
  attributed to the first as `{9000, 8104}`. **Outcome:** carry the
  actual attributed size and limit across the C boundary (an additive
  last-error accessor pair or an out-param on the send entry points —
  export baseline changes explained, headers and Go in one commit);
  `MaxEventSize()` may stay the single-packet constant. Parity tests
  for tagged and mixed-batch cases in `go/`, run with cgo actually on.
- **R5-L1 (P2) one throwing child aborts parent teardown.**
  `browser-ts/src/node.ts:~746–853`: `close` latches, clears the set,
  then aborts the loop on the first throwing inner close; the second
  iterator stays pending, the native parent never closes, a retry
  returns immediately. **Outcome:** finish every owned child, hub and
  parent despite one error, then surface the error (aggregate).
  Witness: her probe, both values.
- **R5-L2 (P2) retained callbacks.** `LeafStream.on_message` forgets
  its closure into `Inner.listeners` (`leaf/src/wasm.rs:~3325`); close
  never removes it; the closure captures the TS wrapper. **Outcome:**
  owned subscription cancellation on stream close (token returned by
  `on_message`, removed on close). Witness: repeated open/close leaves
  the listener count flat; no heap benchmark.
- Her "not elevated" BroadcastChannel note: establish the production
  schedule before claiming a race; nothing to fix unless you find one.

## Evidence and compatibility (both packets' §5)

1. **Above-one-event public interoperability, both directions.** The
   native→leaf real-browser leg is now green after `aa925dd6b`; the
   leaf→native "test" hand-builds leaf-shaped fragments with a native
   fixture and does not execute the public leaf producer. Add
   successful browser→native public stream delivery above one event,
   both engines. Keep the native mechanisms as complementary coverage.
2. **Real direct-close browser witness.** Parked iterator completion
   and post-close typed refusal through the live package in both
   engines — two distinct outcomes, two assertions.
3. **Pins.** `a_locally_deactivated_session_still_receives_its_peers_frames`
   (still unpinned since round 4); the two new close ABI probes; the
   expiry production witness; the leaf floor is 266 while the count is
   ≥270 — raise it to the actual count (it will move again this round).
4. **Inverse receipts.** The six fragmentation inverses and the
   expiry/iterator sections have author-reported excerpts, not retained
   packets. Rerun only the missing bounded entries and retain diff,
   command, exit, RED output, restoration identity, GREEN output. Do
   not reconstruct.
5. **`RetransmitDescriptor.fragment` is a public source break**
   (`wire/src/reliability.rs:116–130`, re-exported at
   `src/adapter/net/mod.rs:268`). Record its release disposition in the
   report independently of N4; no version change.
6. **Stale prose.** Runner comments still say 96-KiB bidirectional
   reassembly and handler-entry ordering; PASS text says no native node
   reassembles leaf fragments (`tests/rtc_browser/runner/src/stage5.rs`
   around `144–169`, `1577–1583`, `2105–2111`). Align with the actual
   predicates without weakening any assertion.
7. **Compatibility accounting** (round 4 item 7): required methods
   added to the injected WASM interface and the stricter 16-digit
   signal ids are input-contract changes — list them in the report's
   release section as such.
8. Round-4 item 4 (ConnectGuard arming / X7 follower observation
   wiring witnesses) — add the two witnesses; they are missing wiring
   proof, not reproduced leaks.

## Validation

All 42 + 10 + 1 + 1 reviewer probes green as committed; rosters
generated from source with the self-check; leaf native + wasm check +
both engines; native RTC binaries; the narrow matrix; bindings
including Go with cgo on and the new size/limit parity; `--lib`
floors; export checker with the baseline change explained; consumer
diff. Final SHA, clean status, CI URL, §13 round-6 receipts. No plan
document edits — the record-keeper writes the plan.
