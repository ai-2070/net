# Stage 5 repairs — Kyra's HOLD at `9f8bde0c7` (R1–R16)

Packet: `C:/Users/chief/Downloads/KYRA_STAGE5_REVIEW_PACKET/` —
`KYRA_STAGE5_REVIEW.md` governs; four lane reports
(`leaf-protocol-review.md`, `control-identity-review.md`,
`leader-ts-review.md`, `core-browser-review.md`) carry the line-level
schedules; probes `parent-leaf-probes.rs` (15 tests, public API only),
`parent-js-probes.mjs`, `firefox-delta-probe.mjs`. Reviewer reproduced
the leaf suite at `3051d3e61`: **12 fail / 3 controls pass**, names
identical. "This is not a CI-only hold": R16's Firefox repair is
credited and the descendant run 34904478362 is 55/55 with Firefox
19/19, which closes R16's execution gate — R1–R15 are production
defects in the leaf, the leader lifecycle, the TS ABI and one native
seam.

Credit retained (do not rework): the packages themselves, wire/codec
sharing and fixture parity, the real Chromium handshake → enrollment →
nRPC path, fire-and-forget elision, the UDP healthy/blocked/restored
controls, two-tab coverage, the non-relaying mock, the same-origin
threat model, all 12 Stage 4b browser names, the install CAS/deadline
structures. **First commit:** land her 15 probes verbatim as
`leaf/tests/kyra_review.rs` (feature `mock-control-plane`; pinned in
the leaf CI job with all 15 names) and her JS probes under
`browser-ts/tests/`, assertions untouched, public-API adaptation only
if unavoidable and explained in the report. Then one commit per row,
prefix `fix(net): stage 5 repair Rn —`; `S5_REPORT.md` §10 with the
per-row table: production change or explicit scoped disposition, the
discriminating witness, the inverse that reaches the original defect,
restored positive evidence. Full pre-push checklist every push; report
only on green exact-head CI with both browser engines. No Stage 6.

## R1 (P1) — announcement signatures do not bind the claimed node id

`leaf/src/announce.rs:281–308, 329–352, 375–395`, `node.rs:755–762,
987–1013`. An attacker signs with its own entity key but the victim's
`node_id` and a later version; the leaf replaces the honest record and
its verification key, then accepts attacker-signed signals attributed
to the victim. Native already checks the entity→node-id derivation
(`mesh.rs:36879–36894`). Closure: derive the node id from the signing
entity key and refuse any announcement whose claimed id differs,
before store replacement and before any authority-producing use; keep
full signed-body verification. Kyra's probe + the honest control.

## R2 (P1) — RPC completion not bound to peer and reply route

`node.rs:583–587, 918–933`, `rpc.rs:62–65, 151–178`. A matching call
id from another established peer completes the call; so does the
expected peer on a different reply channel. Closure: the pending entry
records `(expected peer node id, session incarnation, reply channel)`;
a response is matched on all three **before** the entry is consumed;
wrong-owner input neither completes nor removes it, and the correct
response still succeeds afterwards. Both probes + the control.

## R3 (P1) — sharing `NetSession` does not implement reliability/credit

`session.rs:304–327`, `node.rs:397–410, 445–475, 791–912`. A reliable
build retains no retransmit owner (`has_unacked()` stays false);
send/receive/tick never register retransmit descriptors, debit credit,
return consumed-byte grants/ACKs, or drive retransmit timers — the wire
APIs are passive and native drives them explicitly. Closure: drive the
existing wire machinery from the leaf's actual callers (register on
reliable send, ACK/grant on receive, retransmit on tick, typed terminal
failure on exhaustion); witnesses: deliberate loss + reorder over the
mock with delivery proven; **sustained native → leaf traffic beyond
the configured window** with replenishment observed (through native
admission, not a packet-building fixture); synchronous bounded refusal
preserved.

## R4 (P1) — fragmented delivery broken at composition and native boundaries

`node.rs:818–865`, `session.rs:304–325`, `frame.rs`, `stream.rs:124–190`.
(1) Loss-free reliable fragmented payloads never reach the consumer:
reassembly completes on the last fragment's sequence while reorder
still waits for earlier fragment sequences never offered to it.
(2) The reassembler accepts a group missing prefix coverage and bytes
past the declared end. (3) **Native ingress does not reassemble leaf
fragments** — the enrollment body limit alone reaches this, so it is
not a bulk-transfer-only gap. Closure: one sequence-ownership model
(fragments consume the sequences they carry; reassembly hands the
reorder buffer the group's *first* sequence), full coverage validation,
and native reassembly at the RTC ingress (`frag_flags` read in the
core, feature-gated, bounded per session by the existing byte budget)
— both directions witnessed against a native peer. A typed pre-enqueue
refusal is containment, not fulfilment, unless the owner changes scope
explicitly.

## R5 (P1) — session replacement retains receive state and pending calls

`node.rs` handshake completion. After same-peer replacement, the new
session's sequence-zero traffic is suppressed by retained reorder
state, and the old pending call is not failed. Closure: receive,
reorder, reassembly, stream and RPC state keyed by session incarnation;
retire the old incarnation's work exactly once (typed failure), leave
successor state untouched; witnesses for first-message delivery and
old-call failure across replacement, plus partial-fragment retention.

## R6 (P1) — channel publications misclassified as streams

`channel.rs:39–48`, `stream.rs:81–110`, `node.rs:935–950`. Publisher
ids are bit 48 OR a full-width hash; a valid hash can carry bit 49,
which the receiver reads alone to select `StreamData`. Closure:
classify by the registered namespace (is this id a subscribed channel /
an opened stream), not by one bit of a hash, keeping native wire
compatibility; witness: a subscribed channel whose hash has bit 49 set
reaches the channel consumer.

## R7 (P2) — reordered events lose their own sequence/metadata

`stream.rs:115–118, 175–190`, `node.rs:816, 853–865, 918–949`. Arrival
2,0,1 emits labels 0,1,1; buffered state keeps only bytes. Closure:
buffer `(seq, origin, channel, bytes)` and deliver each with its own
provenance; test labels and mixed metadata.

## R8 (P2) — replay/expiry/bounds

`signal.rs:173–199`, `announce.rs:342–395`, `node.rs:397–410, 774–781,
987–1013`, `stream.rs:166–183`. The `(from, dialog, kind)` seen-set
rejects the second legitimate candidate in a dialog; an expired signed
announcement stays discoverable and authorizes signals; the replay map
is uncapped and linearly scanned; the reorder buffer does work
proportional to a sequence gap. Closure: replay key includes a payload
digest (exact replay only); freshness enforced for discovery and key
authority (expired → not discoverable, not authority, recovers on
re-announce); a bounded replay window (ring/LRU with a hard cap); gap
handling O(1) per packet with a max-gap refusal. Preserve
multi-candidate traffic.

## R9 (P1) — leadership release does not retire the network owner

`leader_session.rs:433–514, 556–607, 782–815, 847–928`,
`wasm.rs:118–141, 542–559, 799–821`. `NodeBackend::perform` spawns ops
holding the real node; stand-down drops the proxy and releases the
lock without closing the node, cancelling those ops, or failing the
node's pending calls; leader-local repliers settle without checking
the old generation; `IdentityVault::fence` has no outbound caller.
Closure: stand-down = fence the generation **first**, then close the
node (cancelling in-flight ops, failing pending calls once with a
typed error), then release the lock; every outbound effect (send,
reply, ticker) checks the fence. Witness with a real pending service
call and an operation parked before dispatch — a recording backend
cannot substitute.

## R10 (P1/P2) — startup, promotion and restoration as one lifecycle

`leader_session.rs:353–375, 485–607, 685–724`, `leader.rs:1137–1259`,
`browser-ts/src/leader/session.ts:219–267`. Attach during initial
backend await is dropped (subscriptions vanish); close during promotion
can yield a lock-holding non-functional leader; stale proxy stream
objects lack their opening generation and can address a same-id
successor, and waiting TS iterators are never ended; dynamic
`announce()` does not update restoration state; follower calls wait on
an unbounded oneshot while only the leader executes the timeout.
Closure: queue attaches until the backend exists and replay them on the
first Leadership broadcast; check `closed` at publish time and
requeue/abort on factory failure; stream handles carry their
generation and are ended on lifecycle loss; restoration state follows
current intent; followers own a local deadline with an honest
"indeterminate remote execution" outcome — never a silent replay.
Witnesses for each schedule.

## R11 (P1/P2) — TS declarations vs the Rust ABI

`wasm.rs:597–610, 171, 407, 896–907`, `leader_session.rs:1309–1321`,
`browser-ts/src/stream.ts:50–52, 107–123`, `node.ts:381–388`. Rust
stream callbacks emit a JSON string, TS forwards it as `Uint8Array`;
`channelHash` is text in TS, a number in Rust; `RTCIceServer[]`
objects are passed where Rust reads strings; the package's fake WASM
agrees with the declarations, not the real interface. Closure: one
contract (emit bytes from Rust or decode in TS; numeric hash; parse
`RTCIceServer`), and the ABI tested **through the real built package**
(bytes, ids, ice servers, effective configuration), direct and
leader-proxied. Kyra's JS probes as witnesses.

## R12 (P1) — native `Stream` handle aliasing under busy replacement

`mesh.rs:22853–22881, 43214–43256`, `stream_handle.rs:102–123`,
`wire/src/session.rs:195, 305–314, 778–786`. A handle stores peer,
stream id and per-session epoch, not session incarnation; the epoch
restarts per session, so a successor with the same id/ordinal accepts
the predecessor's handle. `4bb03a069`'s responder displacement makes
this load-bearing. Closure: handles carry the session incarnation;
send/close/credit refuse on mismatch (typed); witness: busy-responder
Noise → install → same-id reopen → old-handle send/close refused.
Preserve initiator/routed refusal and the install CAS.

## R13 (P2) — IndexedDB completion boundary

`storage.rs:165–183, 224–246, 437–482`. The put request is awaited,
the transaction's complete/abort is not. Closure: await `complete`,
propagate `abort`; witnesses: abort-after-put, concurrent first
creation, ciphertext tamper, wrapping-key export refusal.

## R14 (P2) — signed-signal API vs D1's claim

`anchor_control_plane.rs:302–318`, `sdk/src/rtc_bootstrap.rs:1347–1359`,
`wasm.rs:455–477, 638–683`. The adapter sends `type: signal` frames the
native listener silently ignores; local send success is reported as
delivery. Closure: either the listener carries envelopes (a `/rtc/signal`
route or WS frame type with delivery ACK) or the adapter refuses the
operation with a typed unsupported error; correct D1's "v1 implements"
paragraph to what is shipped. Stage 6 keeps generic peer coordination.

## R15 (P1/P2) — witness predicates

`stage5.rs:635–693, 954–1259`, `rtc_repairs.rs:1064–1119`,
`rtc_routed_restore.rs:201–224, 346–369`, `udp_block.rs:210–280`.
Two-tab PASS does not gate follower RPC success or leader-close /
pending-work / restoration; the native busy-responder test stops
before Noise/install; the browser reconnect leg can pass the
absent-session branch; sequential RPCs ≠ retransmission/reorder
correctness; new native repairs not pinned by name; Firefox logs not
in the artifact list; the nft profile matches the port on all
addresses. Closure: add the production-coupled schedules and exact
assertions (follower RPC through a real leader close; a busy-responder
witness that reaches Noise/install; reconnect requiring an extant busy
incumbent; pin the new names; upload Firefox logs; narrow the nft
rule to address+port). No coverage reduction, retries, floor changes,
or timeout inflation.

## R16 — credited

The Firefox helper repair (`92eb3337d`) is credited; run 34904478362
(Firefox 19/19) closes its execution gate. Nothing to do beyond keeping
it green.

## Secondary audit notes

Adjudicate in the report, not as new subsystems: wasm32 unchecked
length arithmetic; bootstrap URL prefix/override validation; credential
`Debug` redaction in the leaf; JS dialog safe-integer validation; probe
setup failure vs negative network evidence; reduced-fold / canonical
verifier parity; packet-builder lease reset. Fix the cheap ones; state
the rest.

## Validation

Kyra's 15 leaf probes and JS probes green as committed tests; leaf
native + wasm runner + both browser engines green in CI; every inverse
applied-red-reverted with hash check; the native RTC binaries and the
listener suite unchanged in count or green with the R12 witness added;
`--lib` floors; export checker; consumer diff. Return the final SHA,
clean status, the CI URL, and the §10 ledger.
