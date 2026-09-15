# Stage 5 — `net-leaf` + `@net-mesh/browser`

Brief: [`spikes/S5_BRIEF.md`](../../../spikes/S5_BRIEF.md). Plan:
[`BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md`](../plans/BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md)
§7, §8, §9, Stage 5, Follow-on. Base: `c40357404` (Stage 4b second
repair round; CI + natsim green).

*Sections 1–6 and the ledgers are filled in as the slices land; the
two design sections below were written before the code they govern,
because both were review conditions on Stage 5 rather than
consequences of it.*

---

## D1. The session-independent signalling path (slice 2, design)

**The problem Kyra pinned.** §5's native sequence establishes a
routed A↔B Noise session *first* and sends `0x0D02` frames inside it.
The frames therefore inherit the session's authentication: an anchor
forwarding them blind cannot forge one, because the payload is sealed
to a key it does not hold. Serverless Tier A has no relay, so it
cannot establish that session — and the same frames, carried by a
room object over a WebSocket, would arrive with nothing
authenticating them. A `ControlPlane` trait alone does not reconcile
that: it moves the carrier, not the trust.

**The specification.** A signalling message that authenticates
*itself*, so the carrier is irrelevant:

```
SignalEnvelope {
    from:       NodeId,     // signer
    to:         NodeId,     // recipient
    dialog:     DialogId,   // the exchange
    kind:       Offer | Answer | Candidate | Reject,
    payload:    bytes,      // SDP text, candidate line, or reason
    not_after:  u64,        // unix seconds
    signature:  Ed25519 over signing_bytes()
}

signing_bytes() = "net.signal.v1\0"
                || from_le || to_le || dialog_le || kind_tag
                || not_after_le || payload_len_le || payload
```

Five properties, and why each is needed:

1. **Self-authenticating.** The signature is the sender's *entity*
   key — the one already bound to its node id by the signed
   capability announcement the receiver holds (§5 Layer 1: key
   discovery precedes signalling in both the native and the
   serverless world). No session, no PSK, no shared secret with the
   carrier.
2. **Domain-separated and fully bound.** The context prefix plus
   `from`/`to`/`dialog`/`kind` in the signed body mean a signature
   minted for one dialog, kind or recipient verifies for nothing
   else. `payload` is length-prefixed, so a payload cannot run into
   the next field and forge it. Both are asserted by
   `control_plane::tests`.
3. **Replay-bounded.** `not_after` is inside the signature; the
   receiver refuses an expired envelope and keeps a
   `(from, dialog, kind)` seen-set for the remaining window. An
   envelope cannot be replayed after it expires, and cannot be
   replayed inside the window either.
4. **Carrier-blind.** The carrier learns `from`, `to`, `dialog` and
   the SDP — the same metadata an anchor already sees today — and can
   drop or delay, which is a liveness failure, not an authenticity
   one. It cannot inject, modify or redirect.
5. **Not a new trust root.** It reuses the entity key and the signed
   announcement that already exist. Nothing new has to be
   provisioned, which is exactly why Tier A can run it with a dumb
   store.

**What v1 implements — corrected.** This paragraph originally said
`AnchorControlPlane` carries envelopes for the bootstrap dialog it
already owns. **It does not, and R14 is why.** The Stage 4b anchor
listener discards a `type: "signal"` frame, so serialising one and
reporting delivery told the caller something false; the shipped
behaviour is a typed REFUSAL naming the peer, and the adapter's route
table says "no route" rather than listing a signalling route. Carrying
envelopes end to end needs a forwarding route with an acknowledgement
— generic peer coordination, which is Stage 6's, not this stage's.
What v1 does implement is the SHAPE the follow-on depends on:
`ControlPlane::signal` takes a peer id and an envelope, *not* a
session, and the anchorless mock carries the identical envelopes for
real, so the seam is exercised even though the anchor refuses. The
production refusal is now pinned by
`the_anchor_control_plane_refuses_to_carry_a_signalling_envelope`
(`wasm_leaf.rs`), so the corrected claim has a witness rather than a
promise. The native side's `0x0D02` in-session frames are untouched:
Stage 5 changes no native signalling behaviour.

**The honest residue.** An envelope proves who *sent* the SDP; it
does not prove the sender is reachable, nor does it replace admission.
A Tier A deployment still needs an admission decision (who may join a
room), which the follow-on's own scoping owns — this section closes
the *authentication* gap only, which is the one the plan named.

---

## D2. Leader lifecycle (slice 3, specification)

§8's decision is one node per origin, leader-elected over a Web Lock,
with the node on the main thread (S0b: `RTCPeerConnection` is
undefined in workers). Kyra required the lifecycle to be specified,
not just implemented. It is:

**Generations and fencing.** The leader holds Web Lock
`net-mesh/<origin>/<identity-fingerprint>`. On acquisition it reads a
monotonically increasing `generation` from IndexedDB, increments it,
writes it back inside the same transaction, and stamps that value on
**every** message it sends to followers and on every control-plane
operation it performs. A follower refuses any message from a
generation below the highest it has seen. A tab that was suspended
and resumes therefore cannot act as leader: its lock is gone, and any
message it emits carries a stale generation that every follower and
the storage layer reject. That is stale-leader fencing, and the test
drives it with Playwright's CDP `Page.setWebLifecycleState`.

**Interruption budget.** From leader loss to a new leader having
re-bootstrapped its anchor session: target ≤ 2 s on a warm page
(lock handoff is immediate; the cost is one bootstrap round trip plus
the ICE + Noise handshake). Peers observe the old DataChannel dying
on ICE disconnect, which browsers report on the order of 5–30 s, so
the mesh-visible interruption is bounded by Net's own failure
detection, not by the tab handoff. Both numbers are measured and
recorded rather than asserted as a promise.

**Pending work, on leader loss.** Nothing is silently retried:

| In flight | Disposition |
|---|---|
| nRPC call awaiting a reply | `RpcError::LeaderLost { generation }` to the caller |
| Stream send not yet acked | typed failure to the sender; the stream is closed, not resumed |
| Channel subscription | re-established by the new leader, and the follower is told it was re-established |
| Announcement | re-published by the new leader under the same identity |

**Restoration.** The new leader re-bootstraps with the *same*
identity, which the mesh handles as an address-independent identity
rebind — the existing path, not a new one. It then re-subscribes the
union of its followers' declared subscriptions and re-publishes the
announcement. Streams are **not** resurrected: a stream is a
session-scoped object and pretending otherwise would hide a real
interruption.

**Follower proxy.** A real SDK surface, not glue: followers get the
same `LeafNode` API over `BroadcastChannel`/`MessagePort`, with nRPC
futures, stream objects and events proxied. Every proxied message
carries the generation, so a follower that survives a leader change
sees typed failures for the old generation's work and a clean
re-attach for the new one.

---

---

## 1. Commits

| Slice | Commit | What landed |
| --- | --- | --- |
| 1 | `44c5d28df` | the `net-mesh-leaf` crate over `net-mesh-wire`; `stream_window` and `channel::{membership,name}` MOVED into the wire crate |
| 4 | `d85f548d7` | `@net-mesh/browser`, a sibling package, with the corrected failure typing |
| 2 + 6 | `623dbc985` | the `ControlPlane` boundary, `AnchorControlPlane`, and the anchorless mock |
| 3 | `acec21e8b` | identity at rest, Web Lock leader election, the D2 lifecycle, the follower proxy |
| — | `e2039d8cb`, `2caf333c1`, `42efb9ef1` | the CI jobs, and two CI-only breakages the landing caused |
| — | `4bb03a069` | three merged meanings the matrix found (see §4) |
| 5 | `696c78be0` | one browser runner, two gate engines, the 4b witnesses inside it |

## 2. Exit criteria

| Criterion | Status | Evidence |
| --- | --- | --- |
| `cargo check --target wasm32-unknown-unknown -p net-leaf` | met | clean, and clippy on BOTH targets — the wasm one caught three `RefCell` borrows held across an await |
| `cross_lang_wire` replayed INSIDE the wasm runner | met | `leaf/tests/wasm_leaf.rs`, 14 tests in headless Chromium |
| the anchorless mock drives one direct browser ↔ browser session | met | `leaf/tests/wasm_anchorless.rs`; ledger in §5 |
| all scenarios green on Chromium and Firefox | **Chromium met, Firefox CI-only** | 19/19 on Chromium here; Firefox is a CI gate and has never run on this Windows host (no NSS `certutil` on PATH) |
| Safari best-effort, recorded | met | WebKit is a recorded, non-gating leg; on Windows it is refused with a named reason, never skipped |
| wasm ≤ 1.5 MB gzipped | met | 553 668 B raw / 225 369 B gz — 6.7× under |
| sizes recorded | met | §6 |
| default build untouched | met | the leaf is its own workspace; `leaf/tests/dependency_boundary.rs` asserts it from both manifests |

## 3. What each slice actually proves

- **Slice 1.** 145 native + 14 wasm tests. The protocol half is sans-IO, so fragmentation, reorder, the dispatcher's routing table and the call table's dispositions are all natively testable; `web_sys` is confined to three modules and the confinement is a test.
- **Slice 2.** The boundary is asserted by eight tripwires, not by prose: no anchor type and no Net packet type is nameable in the trait, shown red by deliberately leaking one.
- **Slice 3.** Six headless-Chromium witnesses: the lock, the generation gate, the proxy round trip, leader loss, and the frozen tab that cannot act beside its successor.
- **Slice 4.** `UdpBlocked` is a pure function of two observations, mirrored from the Rust side; the STUN probe was measured in real Chromium against a live local responder rather than assumed.
- **Slice 5.** 19 witnesses on one runner. The runner stays the witness authority because every verdict is read on a live `MeshNode` in-process.
- **Slice 6.** See §5.

## 4. What the matrix found in the core (`4bb03a069`)

Three meanings had been merged into one event, and two of them were
already failing Stage 4b witnesses on Linux CI:

1. **"the reservation may be released" ≠ "the attempt is over".** The
   completion owner released the signalling budget at
   DataChannel-open; since 4b's R2 that also retires the attempt's
   accounting identity, so every candidate trickled after the channel
   opened became `signalling frame for an unknown dialog`, the
   listener closed the socket with a typed 4404, and the Noise
   handshake riding that channel died. Now: out of the expiry table
   at channel-open, budget released after the install resolves.
2. **A late candidate is not an unknown one.** While the attempt is
   live it is accepted and dropped.
3. **Quiescence was role-blind.** A tab that went away left six
   streams on an endpoint the anchor still believed open, so the same
   identity's next attempt deferred for ever — the only party who
   could close those streams was the one being refused. The gate now
   takes a role: the Initiator still defers, a Responder facing a
   routed incumbent still defers, a Responder facing a busy incumbent
   on a *different* RTC endpoint displaces it.

A fourth, page-side: the anchor retires a bootstrap dialog by design
and a successful connect completes in 210 ms while the trickle
WebSocket is still in CONNECTING, so a page will *always* see a bare
1006 on a fast path. The leaf now decides channel-openness before it
decides the dialog's end, and treats the dialog's end as diagnosis.

## 5. The anchorless ledger (slice 6)

```
#  kind                direction      bytes
1  announcement        A -> mock        515
2  announcement        mock -> B        515
3  announcement        B -> mock        516
4  announcement        mock -> A        516
5  signal:offer        A -> B           573
6  signal:answer       B -> A           572
7  signal:candidate    A -> B           277
8  signal:candidate    B -> A           277
8 message(s), 3761 byte(s) total
```

Eight legs, zero packets, for a flow that reached a direct session, a
reliable round trip in both directions and a completed nRPC call. The
test asserts the kind SET exactly; the byte totals are reported as
observed (ICE lines differ by a digit between runs). `refuse_net_packets`
both refuses a Net packet and RECORDS the attempt as a non-signalling
kind, so a caller that swallowed the error still reddens the ledger.

**An unplanned control.** A `0x0A00` membership frame was added to the
data path *after* this witness was written, and the ledger did not
move — the assertion was not tuned to the traffic it was later asked
to exclude.

## 6. Sizes

| Artefact | Raw | Gzipped |
| --- | --- | --- |
| `net_leaf_bg.wasm` | 553 668 | 225 369 |
| wasm-bindgen glue | 43 026 | 8 684 |
| `@net-mesh/browser` ESM | 50 451 | 17 478 |
| single-file bundle | 16 063 | 5 542 |
| **total a page downloads** | 612 757 | **239 595** |
| S0a baseline (wire only) | 576 461 | 160 694 |

22 KiB smaller raw than the S0a bundle and 63 KiB larger gzipped:
ed25519, x25519, serde_json, base64 and the `web_sys` glue.

## 7. Inverse ledger

| Witness | Inverse applied | Result |
| --- | --- | --- |
| `the_rollup_the_anchor_column_renders_carries_an_ingested_anchor` (4b R6) | dropped `.with_mesh(...)` | red |
| `control_plane_boundary` | leaked an anchor type across the trait | red |
| anchorless ledger | asked the mock to carry a Net packet | refused AND recorded, ledger red |
| `an_attempt_whose_channel_just_opened_is_not_over_yet` | moved the release back above the install | red, verbatim the string CI reported |
| leader fencing | removed the generation stamp | red |
| Windows TLS pin | launched without the SPKI pin | `ERR_CERT_AUTHORITY_INVALID` — the pin is what makes the difference |
| UDP-blocked profile | ran the same STUN probe with the profile off | flips unanswered → reflexive |

## 8. Named gaps

1. **Firefox has never run on this host.** Its `certutil` must be
   NSS's; a Windows PATH offers Microsoft's. The runner refuses that
   leg up front rather than running untrusted. Firefox is a CI gate.
2. **The routable-interface leg is unproven on Windows by default.**
   Every socket binds loopback so no firewall dialog is raised;
   `mdns_on_pair_formed` is therefore measured on the loopback pair
   locally, and the run prints a banner saying so.
   `--use-routable-interface` opts in.
3. **Two deliberate second copies**, both pinned in both directions by
   `cross_lang_wire` fixtures: the nRPC CLIENT codec (~200 lines) and
   the announcement WRITER. Extracting the full `cortex/rpc.rs` codec
   is a clean follow-on: the codec is lines 1..~1110 *plus*
   1227..~1500 of a 7 756-line file with a core-only dispatch region
   between them, and `cortex/meta.rs` (7+ consumers) would move with
   it. The announcement VERIFIER is codec-free — it canonicalises
   inbound JSON without enumerating a field — which is what makes the
   writer's copy tolerable.
4. **No plain native node reassembles fragments.** `leaf/src/frame.rs`
   and the native RTC ingress (`src/adapter/net/rtc/fragment.rs`) are
   the only readers of `frag_flags` in the tree, so an over-cap
   payload works leaf ↔ leaf and leaf → RTC ingress, while a plain
   native peer sees N partial events. The reverse direction is no
   longer a gap: nothing on the native side fragments, so
   `send_on_stream` refuses an event above `MAX_EVENT_SIZE` with a
   typed `StreamError::EventTooLarge` naming the limit instead of
   answering `Ok` and delivering nothing (§9.2). The harness keeps
   payloads under 8 104 B and GATES the refusal above it.
5. **`ControlPlane::query_capability` has no v1 anchor endpoint.** The
   4b listener serves three routes, so `query` answers from the
   announcements the dispatcher verified — §7's own mechanism. The
   trait's shape is what lets a serverless Tier A answer directly.
6. **Browser ↔ browser from a page is Stage 6's.** The leaf side is
   witnessed in-crate (two leaves, signed envelopes only, real ICE,
   one direct session carrying a round trip and an nRPC call). Two
   isolated tabs would add cross-*context* ICE, tab isolation of the
   identity and lock state, and the page-facing drive loop — evidence
   about the browser's process model, not about the protocol. Stage 6
   needs four `#[wasm_bindgen]` methods: `peer_offer`,
   `peer_accept_offer`, `peer_candidate`, `peer_handshake` — and
   `peer_handshake` must take a peer id and nothing else, because a
   page that can supply a Noise key can supply *any* key.
7. **Close-to-eviction latency is unbounded by anything but ICE.** The
   leaf's `close()` tears down locally and tells the anchor nothing;
   the harness's instrument still prints that the anchor holds a
   session 20 s after the page closed it. Not gated.
8. **`publish_stream_id` ORs bit 48 into a full 64-bit hash**, so for a
   channel whose hash already has that bit the discriminator is a
   no-op. Both ends agree, so nothing is broken; the comment reads as
   if the bit were reserved. Core-side, latent, at hash-collision
   probability.
9. **`enroll_exchange.json` is pinned leaf-side only** — the core's
   `cross_lang_wire` does not yet decode it from the other direction.

---

## 9. What covers the changed native paths, and what payload-size interoperability is claimed

Two questions were flagged for review of `4bb03a069`. Both are
answered here rather than in a commit message, because both are
claims about coverage rather than about code.

### 9.1 The completion / quiescence changes, and the witnesses over them

`4bb03a069` changed three native behaviours: when the completion
owner releases the signalling budget, whether a late bootstrap
candidate is a refusal, and whether the C3 quiescence gate is
role-aware. These are the existing Stage 3/4 witnesses that run over
those paths, by name, all green at this head:

**The completion owner and the install fence** (`tests/rtc_install_race.rs`, 8):
`a_close_consumed_before_the_commit_refuses_the_dead_endpoint`,
`a_close_before_the_responder_commits_refuses_the_dead_endpoint`,
`an_absent_snapshot_cannot_overwrite_a_session_installed_during_the_handshake`,
`an_expected_present_snapshot_loses_to_a_newer_incarnation`,
`an_incumbent_that_becomes_busy_during_the_handshake_is_preserved`,
`a_quiet_incumbent_is_replaced_by_the_parked_exchange`,
`a_close_inside_the_commit_window_leaves_nothing_published`, and the
race harness's own `park_initiator_install` scaffolding. The fence
now takes `require_quiescent` from the snapshot rather than
re-deciding at the seam; `an_expected_present_snapshot_loses_to_a_
newer_incarnation` and `an_absent_snapshot_cannot_overwrite_…` are
the two that would fail if the snapshot and the commit disagreed.

**The quiescence gate** (`tests/rtc_repairs.rs`):
`a_busy_incumbent_survives_an_rtc_upgrade_attempt` — R3-E, unchanged
and still green, which is what pins that the **Initiator** still
defers — beside the new
`a_peer_that_superseded_its_own_channel_is_not_locked_out_by_it`,
which drives ONE busy incumbent so the role is the only
discriminator. Its stated limitation is in its doc comment: it
asserts the gate's decision (the Responder gets past and parks on
the handshake inbox; the Initiator is refused and its message names
`Initiator`), not a full install over a second real DataChannel —
two channels between the same pair of UDP sockets share a 5-tuple
and the in-process fixture signalling cannot demux them. The full
install over a real second channel is what the browser matrix
measures, and it is green there.

**The routed incumbent** (`tests/rtc_routed_restore.rs`, 4):
`routed_then_direct_then_loss_then_manually_restored_routed`,
`an_idle_routed_stream_is_replaced_cleanly_by_the_rtc_pair`,
`an_nrpc_call_round_trips_over_the_datachannel`,
`a_capability_fold_applies_a_remote_fact_received_over_rtc`. These
are what pin the OTHER half of the new role rule — a Responder
facing a **routed** incumbent still defers, so the routed session is
still replaced rather than displaced mid-flight.

**The budget's terminal paths** (`sdk/tests/rtc_bootstrap_listener.rs`,
24): Stage 4b's `an_attempt_that_ended_on_the_anchor_stops_authorizing_
its_token` and `ending_an_attempt_releases_the_identity_its_ingress_
was_charged_to` are unchanged and green — the release still happens
on every terminal path, it happens later — beside the new
`an_attempt_whose_channel_just_opened_is_not_over_yet`.

Totals at this head: 73 across the five RTC binaries named above, 24
listener tests, 19 browser witnesses.

### 9.2 Payload-size interoperability: exactly what is claimed, in BOTH directions

The bound both directions are measured against is `MAX_EVENT_SIZE` =
8 104 B — `MAX_PAYLOAD_SIZE` (8 108) minus the event frame's 4-byte
length prefix. It is defined once, in `net_wire::protocol`, and the
leaf's `MAX_FRAGMENT_PAYLOAD` is an alias of it: the native sender
refuses above exactly the bound the leaf fragments at, and two
derivations of the same number could drift apart and turn a refusal
back into a drop.

**LEAF → NATIVE.** *Claimed:* a leaf fragments an application payload
larger than `MAX_EVENT_SIZE` into up to 8 fragments and reassembles
inbound fragments with a bound (8 concurrent groups, 2 s TTL, every
refusal counted). Leaf ↔ leaf, an over-cap payload round-trips; this
is witnessed natively and in wasm, and the native RTC ingress
(`src/adapter/net/rtc/fragment.rs`) reassembles what a leaf sent to
it. *Not claimed:* no other NATIVE receive path reassembles, so a
leaf → native payload above 8 104 B arrives as N packets each
carrying a partial event and is not put back together. *Also not
claimed:* above 64 832 B nothing is attempted at all —
`fragment_offset` is a u16 BYTE offset, so the last fragment cannot
start past byte 65 535. The leaf send path returns a typed
`LeafError::Wire` naming streams as the way out — never a truncation
and never a drop.

**NATIVE → LEAF.** *Claimed, and this is the round-3 repair.* One
event must fit one packet, and nothing on the native side fragments:
`MeshNode::send_on_stream` refuses any event above `MAX_EVENT_SIZE`
with a typed `StreamError::EventTooLarge { size, limit }` that NAMES
the limit. The refusal happens before the peer is resolved, before a
sequence is consumed, and it is whole — the fitting prefix of a mixed
batch does not reach the wire either. `MAX_EVENT_SIZE` is re-exported
from `net::adapter::net` so a caller can check before sending, and the
bound is mirrored through every binding (`SdkError::EventTooLarge`,
`NET_ERR_MESH_EVENT_TOO_LARGE = -118`, a Node `Error` and a Python
`ValueError`, each carrying size and limit).

Round 3's ABI evidence lane measured this direction rather than
assuming it, and found the defect it was built to find: a 32 KiB
`send_on_stream` returned **`Ok`** and delivered **nothing**. The
batching loop splits a batch across packets but cannot split one
event, so the builder stamped a `payload_len` no receiver accepts —
every native receive path reads into a `MAX_PACKET_SIZE` buffer and
`NetHeader::validate` refuses an over-cap length — and the bytes went
nowhere. Under the cap the identical send arrived byte-exact, so the
limit was a caller-visible cliff with no signature, no counter and no
error.

**Why a typed refusal and not fragmentation.** `send_on_stream` is
transport-agnostic. Only two receivers interpret `frag_flags` — the
browser leaf and the native RTC ingress — and no plain native peer
reassembles anything. Fragmenting in the generic send would therefore
hand a native peer's application N partial events as if each were a
message: a silent corruption in place of a silent drop, on the much
more common native ↔ native path, with no per-peer "reassembles
fragments" capability to gate on. And `fragment_offset` being a u16
means any fragmentation scheme still ends at 64 832 B — a second
undiscoverable cliff. A refusal that names the limit is uniform
across every transport and discoverable on the first call.

**What the matrix therefore keeps under the cap.** Every browser
witness sends payloads below 8 104 B deliberately, because a witness
that quietly relied on native reassembly would be asserting a
property this stage does not have. The over-cap leg of
`stage5_large_messages_cross_the_public_api_in_both_directions` is
now GATED on the typed refusal *and* on the payload never arriving —
it was RECORDED for one round, while the native → leaf answer was
unwritten.

---

## 10. The repair round (Kyra's HOLD at `9f8bde0c7`, R1–R16)

Her 15 leaf probes landed VERBATIM first (`0162fce73`) as
`leaf/tests/kyra_review.rs` and reproduced exactly what she reported:
**12 failing, 3 controls passing**. Her JS probes landed beside them.
All 15 are green now, and both rosters are pinned by name in CI.

**One adaptation, in the JS probes only**: her absolute worktree path
`C:/.../kyra-stage5-9f8bde0c7` cannot resolve here, so the repo root
is derived from the file's own location. Every assertion is
untouched. Her leaf file gained a `//!` header and nothing else,
because the crate denies `missing_docs` on every target.

| Row | Change, or scoped disposition | Witness | Inverse reaching the original defect | Restored positive |
| --- | --- | --- | --- | --- |
| **R1** | `node_id_for_entity` derives the node id from the signing key; `verify_announcement` refuses a mismatch before store replacement or any authority use | `kyra_other_entity_cannot_replace_victim_and_authorize_signal` | remove the derivation check → the attacker's record replaces the victim's and its signals are accepted | the honest announcement path and the store tests |
| **R2** | `CallOwner {peer, incarnation, reply_route}` recorded at registration, matched on all three BEFORE consuming; no route discriminator ⇒ cannot end a call | `kyra_reply_from_another_session_cannot_complete_call`, `kyra_same_peer_wrong_reply_route_cannot_complete_call`, `a_reply_is_refused_unless_peer_incarnation_and_route_all_match` | restore match-by-call-id → both probes fail as at `0162fce73`; drop only `reply_route` → the wrong-route probe alone reds, so the fields are independently load-bearing | `kyra_correct_peer_and_route_reply_control`, the enrollment round trip |
| **R3** | reliable sends register retransmit descriptors (rebuilt with a fresh AEAD counter), receives return grants and ACKs, `tick` retransmits and NACKs, exhaustion is typed; credit admitted for the whole payload before any sequence is consumed | `kyra_reliable_packet_build_retains_retransmit_owner`, loss+reorder over the mock, `stage5_reliable_round_trip` (64 sequential round trips, ordered bodies) | strip the `on_send` registration → `has_unacked()` false again and the loss witness stalls | the small-reliable control, the fragment control |
| **R4** | fragments consume the sequences they carry; reassembly hands the reorder buffer the group's FIRST sequence; coverage validated; NATIVE reassembly at the RTC ingress (`src/adapter/net/rtc/fragment.rs`, feature-gated, bounded by the existing byte budget) | `kyra_reliable_fragmented_payload_is_delivered`, `kyra_incomplete_fragment_coverage_is_rejected`, 7 core fragment tests | hand the reorder buffer the LAST sequence again → the delivery probe hangs on sequences never offered | `kyra_fire_and_forget_fragment_control` |
| **R5** | receive, reorder, reassembly, stream and RPC state keyed by session incarnation; the old incarnation retired exactly once, typed | `kyra_session_replacement_resets_receive_sequence`, `kyra_session_replacement_fails_old_pending_call` | retain the predecessor's reorder state → the successor's sequence-zero traffic is suppressed again | successor traffic and partial-fragment retention |
| **R6** | classification by registered namespace (subscribed channel / opened stream), wire format unchanged | `kyra_channel_publication_is_not_misclassified_as_stream` | classify on bit 49 again → a channel whose hash carries that bit is delivered as stream data | stream traffic on real stream ids |
| **R7** | the reorder buffer holds `(seq, origin, channel, bytes)` records and delivers each with its own provenance; O(1) gap accounting with a max-gap refusal | `kyra_reordered_stream_events_keep_their_own_sequences` | buffer bytes only → arrival 2,0,1 emits labels 0,1,1 | mixed-metadata delivery |
| **R8** | replay key carries a BLAKE2s payload digest; hard cap 4096 that **refuses new admissions** (typed `SignalAdmission::AtCapacity`, counted `signal_capacity_refused`) rather than evicting — the eviction path is gone, so no code can remove an unexpired entry; freshness enforced at read time for discovery AND key authority, literal `issued + ttl` | `kyra_two_distinct_ice_candidates_in_one_dialog_survive_dedup`, `kyra_expired_announcement_is_not_discovery_or_signal_authority`, `kyra_unexpired_signal_replay_stays_refused_under_capacity_pressure` | restore the `(from, dialog, kind)` key → the second candidate is refused; skip the freshness filter → an expired peer is discoverable and still a signal authority | multi-candidate traffic, re-announce restoring discovery |
| **R9** | stand-down is ordered: revoke the generation lease, shut the backend down (cancel in-flight, fail pending once, typed `LeaderLost`), then release the lock; every outbound effect consults the lease; `IdentityVault::fence` gained its caller (interval revalidation against the store) | `standing_down_fences_then_retires_the_node_then_releases_the_lock` over a real node with a real Noise session, a real pending call and an operation parked before dispatch; `a_leader_revalidates_its_generation_against_the_store_and_stands_down` | delete the `revoke` → the fencing witness fails | the ordinary leader/follower lifecycle |
| **R10** | attaches queued until the backend exists and replayed on the first Leadership broadcast; `closed` checked at publish with requeue/abort; stream handles carry their opening generation and end their iterators on lifecycle loss; `announce()` updates restoration state; followers own a local deadline with an honest indeterminate outcome | one witness per schedule in `wasm_leader.rs` + the TS leader tests | drop the attach queue → subscriptions vanish across promotion | promotion, restoration and follower calls |
| **R11** | one contract: Rust emits its canonical event JSON and TS decodes it; numeric `channelHash`; `RTCIceServer` parsed with credentials; ids matched numerically; the fake WASM mirrors the real shape | 10 ABI probes against the REAL built package, direct and leader-proxied, plus Kyra's two TS probes | reintroduce the string/bytes mismatch → `typescript_real_rust_callback_shape` reds | `typescript_byte_callback_control` |
| **R12** | native `Stream` handles carry the session incarnation; send/close/credit refuse a mismatch with typed `SessionSuperseded`; the fenced operations are `close_stream_handle` / `close_stream_graceful_handle` / `try_acquire_tx_credit_for_lifetime`, added **beside** the restored id-addressed `close_stream` / `close_stream_graceful` / `try_acquire_tx_credit_matching_epoch` (see F/N §11, row N4 — this row's original wording described a replacement, which was the compatibility break Kyra held on); the error reaches the C ABI, both SDKs and both bindings, and is never retried | `a_displaced_sessions_handle_cannot_address_its_successor` (one identity in two processes, real displacement, successor's epoch walked to EXACTLY the stale handle's), `test_regression_equal_epochs_across_sessions_are_not_the_same_lifetime` | compare epoch only → the stale handle addresses the successor again | the initiator/routed refusal, the install CAS, 75 RTC tests |
| **R13** | the IndexedDB transaction's `complete` is awaited and `abort` propagated | abort-after-put, concurrent first creation, ciphertext tamper, wrapping-key export refusal | await only the put → an aborted transaction reads as a successful write | ordinary vault reads and writes |
| **R14** | **scoped disposition**: `AnchorControlPlane::signal` refuses with a typed error naming the peer, instead of serialising a frame the Stage 4b listener discards while reporting delivery. Carrying envelopes end to end needs a forwarding route with an acknowledgement — generic peer coordination, Stage 6's. D1 and the module doc corrected to what ships | the typed refusal; the anchorless mock carries the identical envelopes for real | send the `type: "signal"` frame again → the anchor discards it and the caller is told it was delivered | `wasm_anchorless`, unchanged |
| **R15** | two-tab PASS gates follower RPC and the leader-close/pending-work/restoration schedule; the busy-responder witness reaches Noise and install; the reconnect leg requires an extant busy incumbent; the sequential-RPC assertion replaced by the retransmission/reorder property (64 round trips, ordered); nft narrowed to address+port; new native names pinned | the strengthened witnesses themselves | each predicate's own before/after | no coverage reduced, no retry, no floor, no timeout inflated |
| **R16** | credited by the reviewer; kept green | Firefox 19/19 in CI | — | — |

### Why six rows share one commit

R2, R3, R4, R5, R6 and R7 all land in the leaf's receive path and the
types it hands to. Splitting them would have produced commits that do
not compile rather than commits that isolate a change; each row's
production change, witness and inverse is listed separately above.

### R3's tail: what the strengthened witness found

R15's 64-round-trip witness failed at 5 of 64 when it first ran, and
three defects were behind it, each hidden by the one in front:

1. **The leaf acknowledged nothing.** `maybe_grant` gated the whole
   `StreamWindow` frame on a 32 KiB volume threshold, but the frame
   carries `ack_seq` as well as `total_consumed`, and an nRPC body is
   ~150 wire bytes. Fixed: the ack leaves as soon as it advances;
   credit keeps the volume cadence.
2. **The leaf deferred inbound work to its 50 ms ticker**, against a
   sender whose initial RTO is 50 ms — measured at 62 ms between
   consecutive acks. The RTC sink delivers immediately now.
3. **The core never accounted for a reliable frame carrying a control
   subprotocol on a real stream id** — which is exactly the leaf's
   channel Subscribe — because every control arm of
   `process_local_packet` returned before the event-plane accounting.
   The leaf retransmitted to exhaustion and reset the stream, and the
   reset arm called `close_stream`, destroying the anchor's own SEND
   half for that id: `tx_seq` restarted at 0 on the stream it
   publishes replies on. One stream is one sequence space whatever
   subprotocol a frame carries; a reset drops only the receive half.

Both core defects are pinned natively in `rtc_repairs.rs` with their
inverses verified red (remove the accounting hoist → 21 retransmits;
restore `close_stream` → the anchor loses its own stream). This is
R3's native-window closure arriving through the witness that demanded
it rather than as a separate exercise.

### Two things the first repair push got wrong, and what they say

**The dispatched-subprotocol table was not feature-aware.** R3's
accounting hoist has to know which subprotocols this node dispatches,
and the first list named `traversal`, `sensing`, `redex`, `meshdb`,
`fold` and `blob` unconditionally — modules that exist only under
their own features. Default and `webrtc` builds were fine; the narrow
configurations (`--no-default-features --features net` and the six
FFI shims) failed to compile. Each gated entry now sits behind its
own `cfg`, and the 13-configuration matrix from `ci.yml` is part of
what gets run before a push rather than after.

**R2's witness set was incomplete — in the tests, not in
production.** Kyra's `kyra_same_peer_wrong_reply_route_cannot_complete
_call` passes because it constructs a reply carrying a route that is
genuinely wrong. The anchorless wasm witness then failed at the real
path, and the reason is worth stating: it echoed the REQUEST's route
back on the response, which no native responder does —
`mesh_rpc.rs` stamps the REPLY channel's canonical hash on every
server-to-caller frame, and R2 matches against exactly that. So the
production side was right and the mock was under-specified; R2 is
what surfaced it. The witness now stamps the reply route the way the
native responder does, and the discriminating case (a responder
stamping the request route completes nothing) is precisely Kyra's
wrong-route probe, which is why production needed no change.

### Secondary audit notes, adjudicated

- **Credential `Debug` in the leaf** — redacted with the same
  treatment `OfferRequest`/`OfferResponse` got in Stage 4b.
- **JS dialog safe-integer validation** — every u64 crosses the
  wasm-bindgen boundary as a decimal string, and R11 made the stream
  path match ids numerically via BigInt; a supplied number is now
  refused rather than silently dropped.
- **Packet-builder lease reset** — the fragment stamp is one-shot and
  a wire test asserts an unstamped builder emits the pre-Stage-5
  header byte for byte.
- **wasm32 unchecked length arithmetic**, **bootstrap URL prefix /
  override validation**, **probe setup failure vs negative network
  evidence**, **reduced-fold / canonical verifier parity** — stated,
  not fixed in this round: each is a real observation, none is
  reachable from a probe in the roster, and taking them here would
  mean shipping changes with no discriminating witness. They belong
  to the next scoped slice.

---

## 11. Second repair round: the follow-up counterexamples, with raw inverse receipts

Kyra's second HOLD kept every original probe green and raised seven
NEW executable counterexamples, seven source-established lifecycle
items, and five native ones. Her ten follow-up probes landed
verbatim as `leaf/tests/kyra_followup.rs` as the round's first
commit and reproduced **7 failures against 3 passing controls**; all
ten names are pinned in CI beside the fifteen originals. No
assertion in that file was edited.

The receipts below are raw. For each row: the bounded source diff
that undoes the repair, the **verbatim** failure text it produced,
and the restored positive. Kyra's standard, adopted: *mutation
descriptions are not raw inverse receipts.*

### 11.1 Leaf data path — F1 to F7, and N3

| row | defect | repair | inverse (source) | RED (verbatim) | restored |
|---|---|---|---|---|---|
| **F1** | membership consumed sequence 0, but only event-plane records entered the reorder buffer, so a publication at seq 1 was held against a hole nothing could fill | one sequence disposition: every non-feedback subprotocol on a real stream id advances the cursor when consumed; the exemption is exactly the four control-stream feedback messages the send side already excludes. `StreamRecord` carries its own subprotocol so deferred release still decodes correctly | `node.rs:1319` `!is_stream_control(subprotocol_id)` → `subprotocol_id != SUBPROTOCOL_EVENT_PLANE` | `kyra_followup.rs:183 ... membership sequence zero left the reliable publication blocked forever / left: [] / right: [[104, 101, 108, 108, 111]]` | 10/10; the other-channel control stayed green |
| **F3a** | reliable reorder overflow released past the head gap — silent loss on the one mode whose contract is no loss | typed terminal `ReorderOverflow`; the receive half fails once and stays failed (further arrivals drop before credit/ack accounting, so it cannot re-fail once per bound); no RESET is sent, because RESET means "my SEND half gave up" and would make the peer re-accept sequences already delivered | `stream.rs:271` — the typed return replaced by "advance the cursor to the lowest held sequence" | `kyra_followup.rs:281 ... silently delivered 65 records, first=Some(1), without terminal failure=true` — byte-for-byte Kyra's reported result | typed `StreamFailed`, nothing past the hole |
| **F3b** | admission counted bytes but not PACKETS, so tiny reliable messages outran the 128-descriptor retransmit window and evicted unacknowledged descriptors | a reliable send reserves descriptor ownership as well as byte credit before any sequence is consumed; new typed `ReliableWindowFull`, distinct from byte backpressure | `session.rs:447` — the 15-line reservation deleted | `node.rs:2779 ... admission must stop at the retransmit window's packet capacity / left: 736 / right: 128`, and a throwaway probe through the live receive→NACK→send path measured the consequence: inverse `admitted=200 recovered_after_nack=0`, repaired `admitted=128 recovered_after_nack=51` | both green |
| **F4** | a RESPONSE on an unrelated carrier channel completed the call if it stamped the expected reply route inside itself — the frame authorising its own delivery | completion binds four facts compared as one: peer, incarnation, inner route, and the AUTHENTICATED carrier stream the frame arrived on, derived locally from the subscribed reply channel and never read from a frame | `rpc.rs:222` — the four-fact comparison expanded back to peer+incarnation+route | `kyra_followup.rs:215 ... self-declared expected inner route completed call carried on wrong channel` | wrong carrier leaves the call pending; the proper channel then completes it |
| **F5** | a stale leaf stream handle sent through the successor session | handles carry their opening incarnation; send and close refuse typed on mismatch — the check lives where the handle is USED, because clearing internal maps cannot reach a value the caller holds | `node.rs:835` — `self.check_handle(handle)?;` deleted | `kyra_followup.rs:232 ... stale handle send accepted=true, queued=1` — byte-for-byte Kyra's result | typed Err, zero queued; reopen after replacement still delivers |
| **F2** | head delivered, its ACK and the tail lost; the sender's legitimate retransmission WIPED the retained partial group | a byte-identical piece at the same offset and sequence is a counted duplicate (ACK repeats, nothing else); conflicting bytes stay a typed refusal; the group deadline runs from last progress, so a sender whose RTO exceeds the TTL cannot have its group reaped between two pieces it is still resending | `frame.rs` — the 17-line duplicate arm deleted, so an exact duplicate falls into the overlap branch again | `kyra_followup.rs:86 ... a legitimate retransmission destroyed the partial group, while receive ACKs accepted its sequences / left: [] / right: [[90, 90, ... 9000 bytes]]` | file restored byte-identically (md5 `d20c21a4…`); 10/10 |
| **F6** | fragment groups accepted conflicting provenance | stream, origin and channel are fixed by the head and enforced on every fragment; sequence ownership is unique and CONTIGUOUS, so a min/max span cannot claim an intervening non-fragment record | see §11.4 | | |
| **F7** | the replay set evicted its oldest entry under capacity pressure — an attacker could make room for the replay it wanted | at capacity it refuses NEW admissions: typed `SignalAdmission::AtCapacity`, counted `signal_capacity_refused`. The `order: VecDeque` field is GONE — there is no code left that can remove an unexpired entry, which is the structural half of the claim | see §11.4 | | |
| **N3** | native partial groups were not retired with the session; expiry depended on a new group arriving | `retire_session` retires them explicitly; late tails cannot bypass the TTL; an admitted old packet cannot recreate a retired group | see §11.4 | | |

Two in-crate tests were **deleted rather than re-pinned**, because
each pinned exactly the behaviour a repair had to remove:
`a_reliable_stream_abandons_its_head_gap_at_the_buffer_bound` and
`a_duplicate_fragment_is_inconsistent_rather_than_double_counted`.
Re-pinning either to the new text would have preserved the shape of
a test whose subject no longer exists.

`DropReason` is now 18: `ReassemblyDuplicate` and
`SignalCapacityRefused` added, `ReorderBufferFull` removed because
nothing emits it after F3. The counters JSON follows.

**A sentence in §5 that was aspirational when written.** "…cannot be
replayed inside the window either" became TRUE only with F7: before
it, capacity pressure could evict the entry that refusal depended
on. It is recorded here rather than left to read as though it had
always held.

### 11.2 Leader and TypeScript lifecycle — L1 to L7

| row | defect | repair | inverse | RED (verbatim) | restored |
|---|---|---|---|---|---|
| **L1** | the granted lock and lease were held inside a suspended factory stack: a close during promotion could neither cancel nor release until the factory completed | the bootstrap future is owned by `Shared` and cancellable; close drops the wake; the woken frame releases the lock AFTER retiring what it installed. Cancellation is checked before the inner poll, so a factory completing in the same turn as the close cannot install | close no longer drops the cancel sender | `wasm_leader.rs:1720 — close must cancel the bootstrap, not wait for it: the factory future has to be dropped while parked / left: 0 / right: 1` | 24/24 |
| **L2** | TS stream iterators ended only on an explicit loss notification, so an abrupt leader disappearance stranded every awaiting consumer | end on an observed generation transition too (read off the session, so a duplicate notification for the generation in force ends nothing); fence an `openStream` result that crossed the change before registration | the transition observation deleted; separately the `openedUnder` comparison dropped | `× ends a waiting stream iterator when the leader vanished without announcing it (5009ms — the iterator never settles)`; `× ends an openStream result that crossed the leadership change; AssertionError: expected false to be true` | 161/161 |
| **L3** | `announce(C)` replaced the union with C; an empty union returned early, so the last capability-declaring follower's tag was never withdrawn | a leader's announce records its own intent and publishes the UNION; an empty union is published as an actual announcement; reconciliation's union subscriptions no longer leak into leader-local intent | announce publishes its argument; the `wanted.is_empty()` early return restored | `a leader's announce() is its own intent, and the document is the union / left: Some(["cap:leader-2"]) / right: Some(["cap:follower", "cap:leader-2"])`; `the departed follower's capability must be withdrawn by an actual announcement / left: Some(["cap:only"]) / right: Some([])` | 24/24 |
| **L4** | a `None` follower timeout waited forever | the leaf's own default deadline is armed locally, keeping Indeterminate and never-retried | `timeout_ms.unwrap_or(DEFAULT)` → `Some(ms)` only | the call NEVER settles: the test hangs and the runner kills the driver at 126.2 s, exit 1 — which IS "a None follower timeout stays unbounded" | asserts `Indeterminate { deadline_ms: 30_000 }` and elapsed in 30_000..40_000 |
| **L5** | a failed generation transaction propagated above the recovery path and stranded the tab | both failure branches share one fallback: release the lock, fail old pendings typed, surface `promotion_failed` (generation "0" when none was allocated — exact, the counter starts at 1), re-attach as a follower, re-queue | the fallback call deleted, error propagated | `wasm_leader.rs:1822 — the failure must be surfaced to the page, naming generation zero because none was allocated: ["{\"type\":\"leader_lost\",\"generation\":\"1\",\"failed\":\"0\"}"]` | 24/24. The break is a REAL storage failure — a database at the vault's own version whose `leader` store is missing, created through raw IndexedDB — not an injected one |
| **L6** | direct callbacks were invoked while `Inner`'s mutable borrow was held, so a synchronous `send`/`close` from `onMessage` panicked the RefCell | events are collected under a short borrow, the borrow is released, then listeners are called; a callback's own send is picked up next turn. `LeafStream::close` was a no-op behind a real API and now calls the fenced close | `collect` restored to emitting inside the borrow | `RTCB FAIL stage5_direct_event_callback_may_reenter_the_node — counters() re-borrow returned 0 counters (read=false); openStream never reached (None); trapped=Some("counters")` — the RefCell panic, on the real browser path; the other 20 witnesses stayed green | 21/21, exit 0. The listener does two re-borrows of different kinds: `counters()` reads, `openStream()` mutates |
| **L7** | the deferred `ProxyStream.close` checked the generation at spawn, not at dispatch | re-checked at dispatch; send's three dispositions documented — admission, transport refusal at the flush boundary, no ACK anywhere | — | — | see the honest gap below |

### 11.3 Native seams — N1, N2, N4, N5, and the expiry boundary

| row | defect | repair | inverse | RED (verbatim) | restored |
|---|---|---|---|---|---|
| **N1** | the R3 control-accounting hoist charged receive-consumed bytes for control packets while the native send path debited nothing: UDP byte conservation broken | `next_tx_seq_charged` allocates the sequence AND debits under one map lookup; every native `build_subprotocol` producer routes through one send-side mirror of the receive decision; the receive side skips the charge for exactly the four stream-control subprotocols the leaf already excludes. `StreamStats` exposes `tx_bytes_sent` and `max_consumed_seen`, so `remaining + (sent − consumed) == window` is observable | `note_tx_bytes_sent` returns early unconditionally (the pre-fix "allocate a sequence, debit nothing" producer) | `the control frame's 152 wire bytes — header, AEAD tag, event frame and payload, the same total the receiver charges — must be debited from the stream's send ledger`; `the sender's committed total must exceed the receiver's reported total by exactly the withheld packet's 89 wire bytes: sent = 182, consumed = 182, gap = 0 (grants received 2)` | both PASS. The RED numbers ARE the mechanism: uncharged, the receiver's surplus pushes its total above the sender's, the grant clamps to 182, and the withheld 89 bytes are refunded in full |
| **N2** | the stream lookup was dropped before an unconditional removal, so a concurrent same-id reopen could be removed by the old handle | comparison and removal under ONE entry guard keyed by session id AND epoch; a vacant slot removes nothing; the graceful path returns immediately on mismatch rather than waiting out a successor's retransmit window; no guard held across the wait | the pre-fix two-step shape restored inside the conditional close | `a lifetime-conditional close removed a stream it does not own in 2118 of 20000 races / left: 2118 / right: 0`, and `left: Closed / right: Absent` for the absent-then-open case | 54/54 wire session tests |
| **N4** | R12 REPLACED public signatures instead of adding to them — a silent compatibility break for every id-addressed consumer | `close_stream`, `close_stream_graceful` and `try_acquire_tx_credit_matching_epoch` restored verbatim under their original names and contracts, documented as unfenced; the fenced operations live beside them as `*_handle` / `*_for_lifetime`. The fence was NOT weakened — only the names moved. R12's `close_stream_id` is removed rather than left as a second convention beside the restored name | — | — | file-by-file disposition in §11.5 |
| **N5** | Go mapped −117 to a freshly allocated "mesh unknown error (code −117)" that no caller can match, and `Close()` discarded the status | exported `ErrSessionSuperseded` sentinel carrying the same stable string the N-API surface emits, mapped in `meshErrorFromCode`, deliberately outside the backpressure retry loop; `CloseErr()` reports the status while `Close()` keeps its exact signature | the `case -117:` arm deleted | `stream_close_test.go:89: meshErrorFromCode(-117) = mesh unknown error (code -117), want ErrSessionSuperseded --- FAIL` | PASS. The parity test reads `NET_ERR_MESH_SESSION_SUPERSEDED` out of the header cgo compiles against, so constant and sentinel cannot drift |
| **expiry** | the boundary was correct and undocumented, which is how it becomes incorrect later | rustdoc states the three load-bearing facts: second granularity by truncation, INCLUSIVE at `issued + ttl` and exclusive one second later, and `ttl_secs == 0` as a literal zero-second lifetime with no "forever" escape. `get_at`/`query_at` take the reading as a parameter — the same seam shape as `clock::Deadline::expired_at`, so one scan's answer cannot depend on a second ticking mid-iteration | — | — | the witness runs build → verify → ingest → production lookup with the stamp at `issued.999`, so truncation cannot shift what it proves |

**Go execution, contrary to the previous round's report.** cgo does
work on this host: the earlier failure was a `:`-separated `PATH` on
Windows, not a broken gcc. With `;` separators plus
`cargo build --release -p net-ffi` and `CGO_LDFLAGS`,
`go test -run "TestSessionSuperseded|TestMeshStream_" -count=1`
passes in 0.611 s. The previous round's "unproven locally" note is
withdrawn.

### 11.4 What ships without an executable witness, and why

Stated here rather than discovered by the next reviewer.

- **L3's subscription-ownership split** has no reachable public
  schedule at this head: the polluted set is re-read only by
  `reconcile` (which filters on `subscribed`) and by
  `attach_as_follower`, and a tab cannot be promoted twice. Source
  fix plus documentation, which is what the note asked for.
- **L7's dispatch-time check** is bounded by the microtask boundary:
  `spawn_local` resolves on a microtask while every path that moves
  the generation is at least a macrotask, so the interleaving is
  unreachable in one page without inventing a seam. The check is one
  comparison; the row's other half — the documented dispositions —
  is delivered.
- **N5 end-to-end from Go**: producing −117 needs a session
  displacement, and the Go surface exposes no session-replacement
  entrypoint. The sentinel is proven at the mapping and
  header-parity boundary; the fence that produces −117 is proven in
  Rust and through the C ABI.
- **F6, F7, N3 inverse receipts** are recorded in the lane report
  rather than the table above; each was executed the same way (diff,
  verbatim RED, restored positive).

### 11.5 N4 consumer diff, file by file

| file | disposition |
|---|---|
| `src/adapter/net/mesh.rs` | RESTORED `close_stream(peer, id)` and `close_stream_graceful(peer, id, timeout)` — base signatures, `()` returns, base bodies, documented as unfenced by contract. ADDED `close_stream_handle` / `close_stream_graceful_handle`. REMOVED `close_stream_id`. Net: base surface restored, two names added |
| `wire/src/session.rs` | RESTORED `try_acquire_tx_credit_matching_epoch` — base signature, delegating exactly as base did. ADDED `note_tx_bytes_sent`, `next_tx_seq_charged`, `close_stream_for_lifetime`, `drain_state_for_lifetime`, `StreamCloseOutcome`, `StreamDrainState` |
| `wire/src/stream.rs` | `StreamStats` gained two public fields. ADDITIVE with one caveat, flagged not hidden: a downstream consumer constructing the literal needs them. No in-repo consumer does. `StreamError::SessionSuperseded` remains a new variant on an enum that is not `#[non_exhaustive]` — unavoidable for a distinct terminal error, and marking it non-exhaustive now would be the same break |
| `src/adapter/net/mod.rs` | re-export list extended. Additive |
| `sdk/src/mesh.rs` | RESTORED `close_stream(peer, id)`; ADDED `close_stream_handle`; REMOVED `close_stream_id` |
| `sdk/src/error.rs` | unchanged; the `SessionSuperseded` mapping is correct and retained |
| `src/ffi/mesh.rs` | `net_mesh_close_stream` retargeted to the fenced call. No C ABI change — same symbol, signature and −117 disposition. Export baseline unaffected |
| `dataforts/blob/transfer.rs` | receive side (no handle) → restored `close_stream`; serve side (owns a handle) → `close_stream_graceful_handle`. Behaviour preserved |
| `bindings/node`, `bindings/python` | retargeted to the restored name; docs now state that the id-addressed close is deliberately NOT lifetime-fenced, because no opaque core handle crosses those boundaries |
| `go/mesh.go`, `go/net.h` | no signature change; `CloseErr()` added. Additive |
| `tests/rtc_repairs.rs`, `tests/three_node_integration.rs` | five handle-close callsites renamed. No assertion touched; Kyra's witness names preserved |

### 11.6 The hedge failure: diagnosed, and NOT attributed to N1

Kyra reported `mesh_rpc_hedge::hedge_loser_handler_observes_
cancellation` failing 3/3 with the file unchanged. It passes here 5/5
under both the plain and the full CI feature list, and 3× more after
N1 landed. The honest result:

- **The CANCEL is credit-gated.** `spawn_cancel_publish` publishes it
  on the REQUEST channel's publish stream through `publish_to_peer`,
  which charges wire bytes and admits through the credit guard;
  `WindowFull` becomes `SendFailed`, the spawn discards the result
  and never retries. **A single admission refusal silently drops the
  CANCEL**, and the only backstop is keep-alive expiry. That is a
  real, load-sensitive fragility of exactly the observed shape.
- **N1 is not the cause, and we will not claim it is.** N1's error
  direction is OVER-crediting the sender: the receiver's total runs
  ahead, the grant clamps to `tx_bytes_sent`, outstanding becomes 0
  and credit returns to the full window — visible in N1's own
  inverse receipt (`sent = 182, consumed = 182, gap = 0`). A sender
  cannot starve from being handed too much credit. The CANCEL also
  rides subprotocol 0, whose charge is unconditional before and
  after the change.
- **The one coupling we can substantiate is weak**: the R3 hoist
  makes recognized control frames create receive-side state and
  enqueue grants, adding work to the single grant drainer under
  load. That delays grants without ever lowering credit below its
  honest value. A plausible latency contributor to a 3 s deadline on
  a loaded runner; not a diagnosis.

Disposition: the mechanism is named, the item stays OPEN against CI
rather than being closed by attribution, and the single-shot
credit-gated CANCEL publish is recorded as the thing to fix if it
recurs.

### 11.7 Residues named rather than silently carried

- **`wasm_anchorless.rs:647`** and its RefCell panic at `:253` are
  now CLOSED, and neither was what either of us guessed. Production
  is unchanged; both were harness defects. The single
  `duplicate_sequence` drop was traced to the exact packet — event
  plane, stream "pingpong", sequence 0, arriving at `next_expected`
  1, i.e. B's retransmit of the pong landing beside the original —
  because the fixture's inbound sink only QUEUED and delivered on
  the next 50 ms tick while `DEFAULT_RTO` is also 50 ms, so every
  reliable packet was retransmitted once. `wasm.rs` documents that
  arithmetic and delivers on arrival for precisely this reason: the
  fixture had invented a slower wire than the one that ships, and
  the drop, its reason and its accounting were all correct. The
  RefCell panic was a cascade: `assert_eq!(a.node.borrow()…)` keeps
  the guard alive as a statement temporary, a wasm-bindgen panic is
  `panic=abort` and runs no destructor, so the borrow count was
  never decremented and the earlier test's unbounded ticker then hit
  `borrow_mut()` on a permanently-read cell — charging one failure
  to a second test. The drops assertion was STRENGTHENED to cover
  both ends of the session, which is how a second retransmit cause
  had stayed invisible.
- **Event-plane batch senders** (`send_to_peer_node` / `send_routed`,
  subprotocol 0) put bytes on the wire without a debit. That
  asymmetry predates Stage 5 and was not raised; charging it would
  change credit behaviour for the primary event-plane API. Flagged
  as a known out-of-scope residue rather than silently fixed or
  silently omitted.
- **One harness flake observed**: on one of four local runs,
  `mitm_anchor_fails_the_handshake_and_installs_nothing` failed on
  the impostor-timing leg ("timeout: noise msg2") and passed on the
  next run with identical source. Reported rather than buried: 3/4
  green, in a Stage 4b witness untouched this round.

### 11.8 The three CI failures at `72c8abd6b`, each diagnosed at its cause

52 jobs green, three red, three different causes — and in all three
the production diff is empty. That is the finding, not an excuse:
two witnesses were asserting interleavings rather than invariants,
and one was modelling a transport the leaf does not ship.

**Evidence status of this section.** Kyra's round-2 review (E4) was
right that the causality claims below originally rested on traces
that had been captured and then not kept. Each paragraph now says
which of its sentences is a re-captured receipt with a path, which
is a cited source fact, and which is inference. Nothing here stands
on evidence that no longer exists.

**(1) `rtc_repairs::a_control_frame_shares_the_sequence_space_of_the
_stream_it_rides`** — `retransmit_packets_sent == 1` after a 1 s
settle on a loss-free link. NOT the control frame: it is built with
`PacketFlags::NONE` and `register_retransmit` returns immediately
for non-reliable flags (**cited source fact**), so it can never be
in a retransmit window. The resent packet was an application
payload, and its ACK was always SENT — the peer steps over the
control frame's sequence and charges its bytes, which is N1
working. `flush_stream_batch` awaits the socket and only then
registers the descriptor, while the peer's grant drainer answers on
a 1 ms cadence, so under load the covering ACK can be applied
BEFORE the sender registers the packet it covers; the packet misses
its own prune, times out once, and the duplicate's grant prunes it.
That ordering predates this round.

**[INFERENCE], and only these three numbers:** the "17/20
reproductions by pinning to one logical CPU against 24 burners",
"0/12 unloaded" and the "66 µs" measurement. Those traces were not
kept, reproducing them needs CPU pinning against burners, and this
round did not re-run them. The *mechanism* they illustrate —
register-after-ack — is not inference: it is readable in
`flush_stream_batch`'s ordering and is now named in the witness's
own doc comment and explicitly permitted by its disposition
assertion.

**What IS re-captured, at this head.** The oracle itself changed
this round, because "empty pending ∧ `max_consumed_seen ==
tx_bytes_sent`" does not distinguish acknowledgement from give-up
(Kyra, evidence row 2). It is replaced by: the receiver's
cumulative ACK cursor observed to have advanced ACROSS the control
frame's sequence; the sender's ACK frontier covering every
registered descriptor; the byte ledger retained; and an explicit
same-lifetime disposition. The flaky zero-retransmit and zero-reset
assertions stay retired. Two inverses were executed and restored,
with diffs, commands, exits and logs, at
`spikes/S5_R3_NATIVE_RECEIPTS/README.md`:

- **Inverse A**, the whole `account_inbound_stream_packet` block
  deleted — exit 100, reporting `tx_bytes_sent=1247
  max_consumed_seen=1136 gap=111 tx_seq=9`. This is a
  **byte-accounting** receipt and is cited for nothing else.
- **Inverse B**, the load-bearing one: the control frame keeps its
  byte consumption and only `on_receive(sequence)` is omitted. The
  new oracle fails (exit 100, `frontier Some(1)`) while the RETIRED
  oracle *passes* under the same mutation (exit 0,
  `tx_bytes_sent=1247 max_consumed_seen=1247 gap=0 tx_seq=9`) —
  empty window, byte ledger exactly closed, nine sequences issued,
  cumulative ack never past 1. That is the old oracle's blind spot,
  executed rather than argued, and it is why the sequence-only
  sensitivity claim cites B and not A.

**(2) `wasm_anchorless.rs:647`** — closed; see §11.7. A fixture
modelling a slower wire than production.

**RE-CAPTURED**, because the original trace was not kept:
`spikes/S5_R3_EVIDENCE/anchorless-fixture-causality.log` carries the
bounded inverse diff (remove the inbound sink's
`deliver_on_arrival()` call, putting the fixture back on the
tick-delivery wire it had invented), the exact command, and both
runs. Inverse: **exit 1**, `duplicate_sequence` A=1 B=3, `left: (1,
3)  right: (0, 0)` — that drop counter and no other. Restored via
`git checkout --` (empty `git status --porcelain` for the path,
md5 unchanged): **exit 0, 2 passed, 0 failed**. The mechanism
reproduces on demand. The figure differs from the single drop
described in §11.7 only because the assertion was strengthened from
A-only to `(A, B) == (0, 0)`, which is what exposed B's three.

**(3) Chromium `mdns_on_pair_formed`** — neither of the two
hypotheses. Not a peer-reflexive race: Chromium reached `connected`
1.1 ms after receiving the anchor's host candidate. Not a missing
reflexive candidate: the obfuscated `.local` host candidate paired,
and the anchor-stun variant that DID produce an srflx failed the
same way. The pair formed, the DataChannel opened, and the attempt
died at the Noise handshake — then the probe discarded the pair it
had and reported the anchor as lacking mDNS support.

**Cited source facts, not inference:** `page/app.js` opened the
channel `{ordered: false, maxRetransmits: 0}` while the leaf opens
it ordered and reliable (`leaf/src/rtc.rs:145–155`, unchanged
production), because the AEAD replay window refuses packet-level
reorder; and Noise `msg1`/`msg2` are `build_handshake` packets
outside the reliable-stream machinery, so `reliability.rs` cannot
retransmit them and SCTP is their only recovery — which the harness
had switched off. One lost datagram was therefore terminal.

**[INFERENCE], and only this:** the specific A/B figures reported
last round — "old config `NO PAIR — timeout: noise msg2…
disconnected@12824ms`", "new config, identical drop, PAIR FORMED in
25 ms". Those traces were not kept and are not re-captured here.

What this round did instead of leaving that unfalsifiable: the
repository had **no injector that could produce that A/B at all**.
`RtcTestHooks::set_ingress_drop_one_in` discards an
`Event::ChannelData` — SCTP has already delivered it, so the loss is
ABOVE SCTP and is terminal under both negotiations; the page's
`RTCDataChannel.prototype.send` hook is above SCTP for the same
reason; and `UdpProfile` removes all UDP rather than one datagram.
An instrument that cannot distinguish the two configurations is not
evidence about the difference between them. So
`RtcTestHooks::set_raw_egress_drop_at` was added beside the existing
hook (`src/adapter/net/rtc/driver.rs`, same
`cfg(any(test, feature = "fixtures"))` gating, unreachable in a
production build): it drops the **Nth** — deterministic, never
probabilistic — outbound datagram carrying DTLS `application_data`,
at the socket, before the peer's SCTP sees the chunk. A reliable
channel retransmits it; a `maxRetransmits: 0` channel does not. The
claim is now re-checkable by anyone, which the numbers above were
not.

R7's gate is untouched — mDNS-on remains its own verdict requiring a
session, with no fallback, no retry and no widened deadline. What
changed is that a failure now carries its own evidence: candidate
types per side with the `.local` flag, the ICE timeline, DataChannel
and msg1 timings, the anchor's pair sampled while the attempt runs,
and the browser's selected pair snapshotted BEFORE the connection
closes — closing it empties `getStats`, which is why a pair that
formed used to leave no trace at all. The verdict no longer asserts
that an anchor-side mDNS client is required, which was false for
exactly this failure.

`BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md` §3 specified the
unordered, zero-retransmit channel and is corrected in place: it
described the transport the harness was still modelling, and the
leaf had already chosen otherwise, for a reason its code states.

**[INFERENCE]** why the loss happened on that leg in that run: the
routable-interface probe is the only one whose datagrams traverse
the runner's real NIC stack, so it is the only one exposed to queue
drop under load. The loss itself is environmental and not fixable in
a harness. What was fixable — and is fixed — is that a single loss
was unrecoverable, and was then reported as a missing anchor
capability.

---

## 12. Third repair round

Kyra's HOLD at `b66752643` widened the credit substantially: all 25
earlier probes green, F1/F4/F7, L2/L4/L5/L6/L7 and N2/N4/N5 credited,
`wasm_leaf` 15/15, anchorless 2/2, leader 24/24, and the browser
matrix 21/21 on **both** gate engines. What remained was three new
executable counterexamples, eleven source-established items, and two
questions that are the owner's to answer rather than ours to decide
quietly.

Her five probes landed verbatim as `leaf/tests/kyra_round2_review.rs`
and reproduced **3 failures against 2 controls** — her exact split.

### 12.0 The roster defect that produced the one CI red

The red job at `b66752643` was not a regression: `wasm_leader` ran
24/24 green and the job failed anyway, because eighteen of the
twenty-four names I had pinned did not exist. A pin that names
nothing can never be satisfied, and when it fails it reads exactly
like a witness that regressed.

`.github/scripts/check-roster.py` makes that class impossible: every
roster is checked against its SOURCE before the suite runs (`fn
<name>(` for Rust, a quoted literal for harness witnesses), and the
by-name log check still runs afterwards. They are complements —
*pinned but absent from source* is caught up front, *present but did
not run* is caught after. All four rosters are wired: leaf review
probes, `wasm_leaf` replays, `wasm_leader` witnesses, browser matrix.

Stale floors went to the real counts in the same commit — leaf native
150 → 209, `wasm_leaf` 14 → 15, browser 19 → 21 — and the browser
roster gained the two Stage 5 witnesses that had been running
ungated. Firefox's log is now uploaded as an artifact: it is a gate,
so its evidence should not require scrolling a job log.

### 12.1 Owner question: announcement expiry, leaf vs native

**These two rules disagree, and the disagreement is load-bearing.**

| | rule | TTL-zero |
|---|---|---|
| leaf `announce.rs:144` | `now_secs <= floor(ts_ns / 1e9) + ttl` | authoritative for the remainder of the issuing second |
| native `capability.rs:2841` | `age_secs >= ttl` ⇒ expired | expired at age zero |

They are not two spellings of one policy. A TTL-zero announcement is
an authority on the leaf and already dead natively; at the exact
boundary second the leaf says fresh and the native side says expired.
The native rule is deliberate and documented — it matches
`PermissionToken::is_valid`, so the effective lifetime is exactly
`ttl_secs`.

Options:

1. **Match native exactly** — nanosecond precision, `age >= ttl`.
   The reviewer's stated default, and the answer that leaves one
   rule in the system. Cost: it changes leaf behaviour at the
   boundary and kills the TTL-zero-within-the-issuing-second case
   that two leaf tests currently pin, so those move with it.
2. **Keep the leaf's inclusive rule and document the divergence** as
   deliberate: the leaf reads a second-granular clock (`clock::
   now_unix_secs`) where the native side reads nanoseconds, and
   rounding down an issue stamp then expiring inclusively is the
   conservative direction under truncation.

We are **not** flipping a comparison operator to make this go away.
Recommendation: option 1, on the grounds that two expiry rules for
one announcement type is a defect regardless of which is better.
Awaiting the owner.

### 12.2 N4's residual source breaks — decided by the owner, and taken

Round 2 restored the id-addressed API beside the fenced one, which
closed the *behavioural* break. Two **source**-compatibility breaks
for downstream Rust remained, and calling them additive would have
been wrong:

- `StreamStats` gained public fields (`tx_bytes_sent`,
  `max_consumed_seen`), so any downstream struct literal stops
  compiling. No in-repo consumer constructs one — the Node and
  Python bindings build their own from ours — but that is our tree,
  not theirs.
- `StreamError::SessionSuperseded` is a new variant on an enum that
  was not `#[non_exhaustive]`, so any downstream exhaustive `match`
  stops compiling.

**Decision (owner): accept the bounded source break now and
establish extensibility properly**, rather than paying the same cost
again at every diagnostic counter and every meaningful new error.
Both types are now `#[non_exhaustive]`:

- **`StreamError`** — `SessionSuperseded` stays a DISTINCT error;
  it is not folded into `NotConnected`. The rustdoc states the
  consumer contract the owner set: handle known cases explicitly and
  **propagate** unknown ones. A wildcard arm is not permission to
  retry — `SessionSuperseded` can never succeed on the same handle,
  so treating an unrecognised variant as retryable turns a terminal
  condition into a loop.
- **`StreamStats`** — fields stay public and readable; only
  outside-crate literal construction closes. It is an evolving
  diagnostic snapshot, and adding a counter should not repeatedly
  break downstream construction or exhaustive destructuring.
- **A stable entry point, deliberately not an all-counters
  constructor.** `StreamStats::empty()` returns an all-zero snapshot
  to assign fields on. A constructor taking every counter would
  merely relocate the next break to the next counter, which is the
  problem the attribute exists to solve. Its doc says plainly that
  synthetic snapshots are for tests, fixtures and adapters, and that
  live statistics come from the stream.

Two corrections the owner made to this section's earlier draft, kept
here because they were right and the draft was not:

1. **"One break now, none later" was too strong.**
   `#[non_exhaustive]` prevents future breaks from ADDING fields or
   variants. It does nothing about removing a field, changing a
   type, or changing a semantic — all of which remain breaking.
2. **This does not discharge the release obligation.** Marking an
   existing public type `#[non_exhaustive]` is itself a breaking
   change and must be versioned and documented as one. The version
   bump and release note are the owner's, deliberately not taken
   here; the API-extension policy and the migration-relevant
   contract (propagate unknown errors; use `empty()` to construct)
   are what this commit lands.

### 12.3 The executed counterexamples — P1, P2, P3

Every row below cites a receipt containing the bounded diff, the
exact command, its exit code and the restored run's output. The
receipts are in the tree; paths are given rather than summarised,
because Kyra's standard for this round was explicit: *mutation
descriptions are not complete attributable diffs, commands, exits and
restored logs.*

| row | defect | repair | receipt |
|---|---|---|---|
| **P1** | a head fragment received and ACKed — so the sender retired its descriptor and will never resend — then the group expired in silence: the tail arrived, was ACKed into an orphan, the caller got nothing, no `StreamFailed` | an acknowledged group is OWNED. Typed terminal disposition, not retention: eight acknowledged-but-incomplete groups would otherwise pin every reassembly slot for the session, so backpressure never clears. Every non-completion destruction of a sequenced group names the stream it owned; the TTL sweep runs before the open-group shortcut AND before the acknowledgement decision, so a tail can never be ACKed into a group that has lost its head | `spikes/s5r3_inverse/P1{a,b,c}-*.log` (three inverses, because it is three production changes) |
| **P2** | close deleted the consumer cursor while the wire's receive state and the peer's TX sequence stayed live: reopen waited for sequence 0 while the peer sent 1, permanently stranded | close RETAINS the receive cursor for the stream's lifetime within the session; a reopen resumes at the peer's next sequence. The alternative — refusing reopen — was rejected because `open_stream` returns one bidirectional handle, so refusing it would kill the send half too. Records for a closed stream still advance the cursor (the credit and ack loop stays live) and are counted `stream_closed` | `spikes/s5r3_inverse/P2-close-retains-receive-cursor.log` |
| **P3** | a 30-byte unconditional control debit floored credit at zero with 100 application bytes outstanding, and the next 30-byte grant refunded application credit that was still owed | the shortfall is DEBT, not forgiveness. `StreamState` carries an `overdraft`; the debit still floors at zero, because feedback must never sit behind the window it refills; an authoritative grant retires the debt with its newly-consumed delta BEFORE any of it reopens application credit. The identity becomes `remaining + (sent − max_consumed) == window + overdraft` | in the P3 row's lane report; mutation `shortfall = 0`, exit 101, `left: 30 right: 0`, restored exit 0 |

### 12.4 The source-established rows — X1 to X11

| row | what was wrong | what ships |
|---|---|---|
| **X1** | RESET left the old receive lifetime's partial groups behind | a reset retires that stream's fragments |
| **X2** | `send_subprotocol` bypassed the `send_failed` terminal guard, so every non-handle producer could still send on a stream that had given up | the fence moves to the shared admission path, before packets are built. Stream-control subprotocols stay exempt: the RESET that reports the failure, and the feedback the credit loop runs on, must not be refused by the condition they report |
| **X3** | `PieceMeta` had no plane or mode provenance; completion took them from whichever packet finished the group | bound group-wide, exactly as F6 bound stream, origin and channel |
| **X4** | the 1 024-entry retransmit stamp cap could evict admitted ownership across streams | admission reserves the stamp, so a new stream is refused rather than an owned one silently evicted |
| **X5** | cancelling the bootstrap released the lock without closing the RTC resources it had built; the offering transport held a strong cycle that kept the connection alive regardless | the cycle is broken with weak references, the close is intrinsic to ownership (`PeerLink` closes on drop rather than relying on a caller to remember), and a drop guard runs `close_all` synchronously before the lock moves. The accepted dialog is recorded before `accept_answer`'s await, so an attempt abandoned mid-`setRemoteDescription` is still handed back |
| **X6** | a follower's unchanged announcement was performed raw and overwrote the union, silently dropping every other follower's capabilities | follower declarations route through the authoritative union publisher only, settled against the same snapshot the union was computed from; a retired server settles every parked reply typed rather than dropping promises |
| **X7** | a proxied synchronous send held `Shared.server` while the pump could emit another stream's terminal event that reborrowed it | the broadcast is queued and drained after the synchronous frame returns, followers before local listeners. Nothing is suppressed — suppressing the terminal event would have been the easy fix and the wrong one, since that event is how a consumer learns its stream ended |
| **X8** | the pre-send control debit had no refund for work the transport never admitted | lifetime-scoped refund across the helper and sixteen producers. Transport-owned LOSS is never refunded: a packet that reached the wire and vanished is spent, and refunding it would reopen a window the peer never credited |
| **X9** | native group retirement was not atomic with ingress — two maps, with an interval between check and insert | one aggregate per session, one entry guard for the whole decision; `retire_session` publishes the marker first and releases the groups second under that same guard. Both of Kyra's schedules are structurally unreachable rather than defensively handled: there is no interval left to interleave with. Tombstones bounded, expired markers evicted before live ones |
| **X10** | the replacement installer, the dead-peer sweep and the routed responder rotation each ended a lifetime without retiring its reassembly state | retirement is wired to every lifetime end. The rotation arm mattered most: no close notification and no endpoint removal follow it, so nothing else would ever have cleaned up behind it |
| **X11** | native fragments could not bind stream, channel, origin, sequence or reliability, and native ACK-before-capacity/expiry had no complete-or-terminal disposition | the leaf's F6 and P1 repairs mirrored natively, so one model governs both sides |

Leadership receipts: `spikes/s5r3-leadership-inverse.md` (five rows).
Native receipts: `spikes/S5_R3_NATIVE_RECEIPTS/`. Leaf receipts:
`spikes/s5r3_inverse/`.

### 12.5 Evidence items

- **Rosters and floors** — §12.0. Every roster is now checked against
  its source before its suite runs; floors raised to the real counts.
- **Firefox log** uploaded as an artifact alongside Chromium's and
  WebKit's. Firefox is a gate; its evidence should not require
  scrolling a job log.
- **The native control ACK oracle** was "empty pending ∧
  `max_consumed_seen == tx_bytes_sent`", which cannot tell an
  acknowledgement from a give-up — both drain the window. It now
  observes the real ACK frontier and the terminal/reset disposition.
  The flaky zero-retransmit assertion stays retired.
- **Retirement evidence** no longer runs against a custom backend
  (which proved the harness retires, not the leaf): the witness drives
  the production `RtcLeafTransport` with a real `RTCPeerConnection`
  and the production event sink, asserts the engine's own observed
  `connectionState` after retirement, and compares generations by
  EQUALITY rather than substring.
- **ABI** — the Node probes hand-drove inner stream objects while the
  output claimed "no test doubles". Node has no `RTCPeerConnection`,
  so a wasm-owned `LeafStream` cannot exist in that process; the file
  now states exactly what is real and names the browser witnesses
  that carry the real exercises. Five of those are new, against the
  built package over real WebRTC: direct stream to callback AND async
  iterator with forwarded options agreeing, sustained native traffic
  at 8× the credit window with the grant round trip counted and the
  conservation identity asserted, and a leader-proxied stream both
  ways — gated on the proxied surface returning a PROMISE, so that leg
  cannot silently retest the direct one.
- **R14/D1** — the report claimed the adapter carries envelopes. It
  does not, and §D1 is corrected in place rather than left to age.
  The production refusal is pinned by
  `the_anchor_control_plane_refuses_to_carry_a_signalling_envelope`,
  and `browser-ts/README.md`'s example now shows the refusal instead
  of implying success.
- **Uncharged event-batch producers** predate Stage 5 (Kyra verified
  this independently). The byte-conservation claim is scoped
  explicitly to the admitted and control-debited producers; it is not
  attributed to N1, and it is not quietly widened either.
- **§11.8 causality** — re-captured rather than asserted. The
  anchorless fixture receipt reproduces on demand
  (`spikes/S5_R3_EVIDENCE/anchorless-fixture-causality.log`: inverse
  diff, command, exit 1, `left: (1, 3) right: (0, 0)`, restore, exit
  0). The mDNS A/B needed a drop BELOW SCTP, which no existing hook
  could do — the pre-SCTP injector now exists, so that claim is
  re-checkable instead of historical. Sentences that remain inference
  are marked as such, one by one, rather than the section carrying a
  blanket disclaimer.

### 12.6 Three defects the round's own new witnesses found

The ABI evidence work was supposed to be evidence. It found two live
defects, and repairing the first surfaced a third. That is the
argument for making evidence real rather than shaped to pass.

**P4 — a reliable stream silently running on fire-and-forget
machinery.** 40 nRPC requests on one reliable stream through the
built package, every 5th datagram elided above SCTP and every 3rd
reordered: the handler saw 32 of 40, the 8 missing bodies were
exactly the 8 elided ones, and `stream_failed` was 0. No recovery AND
no terminal disposition — P1's family, on the mode whose contract is
that there is no loss.

One root cause, three faces. `open_stream_full` was first-open-wins
for the RELIABILITY MODE, and a channel's publish stream id is
derived from the channel, so the harness sink's fire-and-forget leg
and its reliable leg shared one id. The log says it verbatim:
`ignoring conflicting config; first open wins … existing_reliable=
false new_reliable=true`. `FireAndForget::on_send` retains nothing,
so there was no descriptor to rebuild from; `build_nack` is `None`,
so the receiver never NACKed; and `stream_failed: 0` was an
unreachable give-up path rather than a missing one — with no
descriptor there is no RTO to exhaust. An intermediate build with
descriptors registered and the receiver still stalled reported
`stream_failed: 2`, which is how we know the path fires when it can.

RELIABLE is now authoritative and monotonic, on the wire and in the
leaf's `RxStream`, and applies to existing traffic rather than only
at first touch. The boundary hole a fire-and-forget prefix leaves is
NACKed first and conceded only after eight later sequences arrive
without it: conceding at once discards a recoverable packet, never
conceding stalls the stream on a hole no retransmit can fill.

**A third defect on the way through.** The native core had no
in-order delivery for reliable streams at all — `process_local_packet`
pushed arrivals in wire order, its own comment saying so while
`TRANSPORT.md` promised the receive side reorders into sequence. The
receive path now holds out-of-order arrivals per reliable stream in a
window-bounded buffer, drops duplicates and releases in sequence
order, with room reserved before the sequence is accepted so nothing
is acknowledged that cannot be delivered. `RELIABLE_STREAM_
HARDENING_PLAN.md`'s H-8 bullet deferred exactly this buffer
"because no consumer needs it"; the browser leaf is that consumer,
and the bullet now says so rather than quietly contradicting the
transport.

**In-order handler entry.** With the transport delivering strictly in
sequence (release order measured as ascending 40..79, each
retransmitted sequence becoming the head), one flag stayed false:
the unary nRPC fold spawned a task per call, handing ENTRY order to
the scheduler. This is a guarantee Net makes — `TRANSPORT.md`'s
ordering contract is FIFO within the stream with "no ordering across
streams" as the single named exception, one service's requests from
one caller ride one stream, and the serve bridge is a single task
draining one receiver, so delivery order survived intact right up to
the fold. The fold now chains handler entry per source, polling each
handler future exactly once to its first await before passing the
baton. Signalling *before* invoking the handler — the obvious shape —
races: the successor can enter first by a few instructions on a
multi-worker runtime. Only the synchronous prefix is serialized; 128
parked handlers were all inside their bodies simultaneously in the
unit witness, and a handler that parks passes its baton at its first
await, so it cannot wedge its successors.

**Over-cap stream events.** A 32 KiB native → leaf send returned `Ok`
and delivered nothing; 7 800 bytes arrived byte-exact. Refused typed
at the sender rather than fragmented, because `send_on_stream` is
transport-agnostic and only two receivers in the tree reassemble —
fragmenting generically would hand a native peer's application N
partial events as if each were a message, replacing a silent drop on
the rare path with silent corruption on the common one. `MAX_EVENT_
SIZE` is defined once, re-exported for pre-checking, and reaches
every consumer as its own error rather than a generic transport
failure. The harness leg that merely RECORDED this outcome is now a
gate that `Ok`-plus-silence, a truncation, a late delivery and an
untyped error each fail.

**One honest note on the matrix runs.** Two runs during this round
each failed a different witness — once a Stage 5 stream row, once a
Stage 4b handshake row with no stream involvement — while other work
was running on the same host. Neither reproduced. Serialized, with
nothing else running, the matrix is 26/26 exit 0 twice consecutively.
Recorded rather than omitted: an intermittent failure under host
contention is worth knowing about even when it is not a defect in the
tree.

### 12.7 What CI found that this workstation could not

Three rounds of local checklists were green before each push, and CI
still found four things. Each is worth naming, because the pattern is
the interesting part: none of them was reachable from a clean,
unloaded, single-engine Windows run.

**A roster of twenty-four pins, eighteen of which named nothing.**
The suite ran 24/24 green and the job failed anyway. A pin that names
nothing can never be satisfied and reads exactly like a regression.
`.github/scripts/check-roster.py` now checks every roster against its
source BEFORE the suite runs, so the two failure modes are
distinguishable: *pinned but absent* up front, *present but did not
run* afterwards. Committing that script 100644 while ci.yml runs it
by path then cost one more round — the guard against silent gate
failure failing silently, which is at least an honest lesson.

**`#[non_exhaustive]` binds crates, not workspaces.** The attribute
was added having checked `net-mesh-wire`, forgetting that `net-mesh`,
the SDK and both bindings are downstream of it and bind exactly like
any external consumer. Four crates, three separate CI jobs. The
owner's ruling — handle known cases, PROPAGATE unknown ones, and a
wildcard is not permission to retry — is now applied at each of
those boundaries, and neither binding maps an unknown variant onto
something a caller would retry or reconnect on.

**A healthy reliable stream reset after 200 ms of receiver silence.**
`retries` counted retransmit ATTEMPTS rather than evidence of loss,
and the RTO never backed off, so a browser tab starved for a second
on a loaded runner cost its sender the stream — with every byte in
fact delivered and acknowledged. Reproduced by blocking the tab's
main thread for 1.2 s: `duplicate_sequence` 36, an RTO storm on
packets that were never lost, and a `StreamReset` for a stream that
was working. Fixed with RFC 6298 backoff and a budget spanning about
5.15 s, and `give_up_horizon` now publishes the ladder so dependents
stop hardcoding a constant.

**The event plane decided what a payload WAS by reading its first
byte.** `handle_event_plane` tried to decode an nRPC reply first and
treated the bytes as application data only if that failed;
`DISPATCH_RPC_DEADLINE_EXCEEDED` is a single `0x13` with no body, so
any payload of 24+ bytes starting with `0x13` was consumed as a reply
that matched no call. One message in 256 on a subscribed channel,
swallowed with no event at all, and 4.7% of harness runs losing one
of twelve stream payloads — the arithmetic of consecutive seeds, not
luck. The plane is now decided by the CARRIER: a reply is honoured
only when the route it declares hashes to the stream it arrived on,
which turns away nothing the call table could have accepted.

The last one is the one to dwell on. It was found by the ANCHOR
LEDGER the previous round added to a failing witness — `ack_frontier=
12 pending=false` against a consumer holding 11, with every drop
counter zero. Without that line it is a flake; with it, it is a
one-hypothesis diagnosis. Evidence that only prints on success tells
you nothing you did not already believe.

**And the second half of that repair matters as much as the first:**
no post-acknowledgement discard on the event plane is silent any
more. A record could be acknowledged on the wire and then dropped
inside the leaf with nothing but a counter nobody polls, which is
precisely why this survived a round of review. Both remaining
refusals now raise a typed `Dropped` event beside their counter.

One finding is reported and NOT fixed here: `unparsable` on a healthy
session is the anchor's 72-byte discovery beacons, which the leaf has
no discriminator for. The drop is correct — a leaf must not act on
mesh proximity gossip — but filing well-formed frames of another
protocol under "malformed" gives `unparsable` a rate-dependent
baseline that would hide a real malformed-packet problem underneath
it. It is not data loss, it is wider than this stage, and it belongs
to whoever owns the leaf ingress discriminator.
