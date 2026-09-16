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
from `net::adapter::net` so a caller can check before sending.

**The bindings, stated per language, because "mirrored through every
binding" was not true when it was written.** Rust keeps
`SdkError::EventTooLarge { size, limit }`. Node rejects with a plain
`Error` — deliberately not one of the prefix-sniffable classes,
which route to a retry or a reconnect — whose message names size and
limit. Python raises `ValueError`, not a transport error, with the
same two numbers. C returns `NET_ERR_MESH_EVENT_TOO_LARGE = -118`.

**Go was the hole, and it is now closed.** `meshErrorFromCode` went
from `-117` straight to `-130`: every oversize send surfaced as
`mesh unknown error (code -118)`, untypeable and indistinguishable
from a variant the binding predates. The header also promised a
"detail string" that no send function has ever returned, and the C
ABI's `int` return discarded the error's `size` and `limit`
outright. Closure: a `net.ErrEventTooLarge` sentinel, an
`*EventTooLargeError{Size, Limit}` that wraps it so `errors.Is` and
`errors.As` both work, and a new export `net_mesh_max_event_size()`
— because `size` is an element of the caller's own `lens` array but
`limit` is a build constant of the linked cdylib that a caller could
otherwise discover only by being refused. The Go arm reads
`C.NET_ERR_MESH_EVENT_TOO_LARGE` from the header cgo compiles rather
than a transcribed literal, so a renumbered enum is a compile error
rather than another silent unknown. The false detail-string promise
is deleted from both headers and replaced by the accessor. Nothing
about the preflight moved: it stays inside `send_on_stream`, and the
nRPC and channel-publication paths that bypass that function are
untouched and separately scoped.

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
| **R8** | replay key carries a BLAKE2s payload digest; hard cap 4096 that **refuses new admissions** (typed `SignalAdmission::AtCapacity`, counted `signal_capacity_refused`) rather than evicting — the eviction path is gone, so no code can remove an unexpired entry; freshness enforced at read time for discovery AND key authority, on the half-open interval `[stamp, stamp + ttl)` at nanosecond precision (owner ruling 2026-09-16, §14.1; this row read `literal issued + ttl` while the leaf still truncated to seconds) | `kyra_two_distinct_ice_candidates_in_one_dialog_survive_dedup`, `kyra_expired_announcement_is_not_discovery_or_signal_authority`, `kyra_unexpired_signal_replay_stays_refused_under_capacity_pressure` | restore the `(from, dialog, kind)` key → the second candidate is refused; skip the freshness filter → an expired peer is discoverable and still a signal authority | multi-candidate traffic, re-announce restoring discovery |
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
| **expiry** | the boundary was correct and undocumented, which is how it becomes incorrect later | rustdoc states the three load-bearing facts, AS REVISED by the owner's 2026-09-16 ruling (§14.1): NANOSECOND precision, EXCLUSIVE at `age == ttl` — the interval is `[stamp, stamp + ttl)` — and `ttl_secs == 0` as a lifetime that is over at the stamp itself, with no "forever" escape. The row's original wording (second granularity, inclusive at `issued + ttl`) described the pre-ruling leaf rule and is superseded, not deleted, because the divergence it documented is what the ruling settled. `get_at_nanos`/`query_at_nanos` take the reading as a parameter — the same seam shape as `clock::Deadline::expired_at`, so one scan's answer cannot depend on a second ticking mid-iteration | — | — | the witness runs build → verify → ingest → production lookup with the stamp at `issued.999`, so truncation cannot shift what it proves |

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

`.github/scripts/check-roster.py` catches that class, and it is
worth being exact about how far it reaches, because the first
write-up of it was not.

It is a **lexical preflight**. It searches a source file's raw text
for `fn <name>(` (Rust) or the name in quotes (harness witnesses).
It does not parse either language, does not evaluate `cfg`, and
cannot tell a live test from a commented-out one, an ordinary
helper, a declaration behind an inactive feature, or a witness
literal nothing emits. It also does not deduplicate a roster or
assert set EQUALITY between source and pins, so a witness that
exists and is *not* pinned is invisible to it — that is what the
cardinality floors are for. The by-name log checks are therefore
not redundancy: they are the only step that establishes a pinned
name belongs to a test that RAN, and they carry the weight.

**"Before the suite runs" is true of four rosters, not all of
them.** The leaf review probes and the `wasm_leaf` / `wasm_leader`
rosters are checked before their `cargo test` invocations, and the
browser-package ABI roster before `node abi_real_package.mjs`. The
browser-matrix roster is checked in the post-run inventory step:
before the engine LOGS are read, but after both engines have
already executed — it saves the inventory from a bad pin and
nothing more. The native RTC rosters have no preflight of this kind
at all; they go through the existing JUnit checker. An earlier
revision of this section claimed every roster in the workflow was
checked before its suite ran, which is false.

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
**RULED 2026-09-16: option 1, match native.** Implemented in §14.1.
The leaf's predicate is now the native one to the nanosecond, so the
table at the top of this section describes a disagreement that no
longer exists — it is kept because the disagreement is the reason the
ruling was needed, and because one of the two rules in it had to lose
for a reason, not by preference.

### 12.2 N4's residual source breaks — owner-confirmed 2026-09-16; versioning deferred to release

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

**Attribution correction.** An earlier draft of this section, and
the commit message of `477bc345e`, presented what follows as a
decision made by the owner. **No owner ruling was given.** The owner
was paused at the time; the brief asked for these options to be
WRITTEN UP in §12, not resolved. The policy was chosen by the
implementer, and the reviewer subsequently endorsed it on its merits
("accepted in principle … release/version work owner-managed"), so
the CODE STANDS — but the attribution did not, and it is corrected
here rather than quietly left to age. A decision must never be
attributed to an owner who did not make it: it launders a judgement
call into an instruction and removes exactly the scrutiny the call
deserves. **Status: owner-confirmed 2026-09-16; version bump and
release note deferred to the release process.** The policy below was
the implementer's choice and the reviewer endorsed it on its merits;
the owner has now confirmed it as policy. Marking an existing public
type `#[non_exhaustive]` is itself a breaking change — the release
owner handles the version bump and the release note when the branch
ships, and this round deliberately does not take that work.

**The choice: accept the bounded source break now and establish
extensibility properly**, rather than paying the same cost again at
every diagnostic counter and every meaningful new error. Both types
are now `#[non_exhaustive]`:

- **`StreamError`** — `SessionSuperseded` stays a DISTINCT error;
  it is not folded into `NotConnected`. The rustdoc states the
  consumer contract: handle known cases explicitly and
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

Two corrections to this section's earlier draft, kept here because
they were right and the draft was not:

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
   are what shipped.

**Still to confirm with the owner**, and listed again in §13: the
`#[non_exhaustive]` + `empty()` policy itself, and how the version
bump and release note are handled.

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

- **Rosters and floors** — §12.0. Four rosters gained a LEXICAL
  source preflight (leaf review probes, `wasm_leaf`, `wasm_leader`,
  browser-package ABI); the browser-matrix roster is checked after
  its engines run and before their logs are read; the native RTC
  rosters have no such preflight. Floors were raised, but not all
  to the real counts — leaf native is still 209 against 234 actual.
  The exact-name log checks are what establish that a pinned
  witness ran.
- **Firefox log** uploaded as an artifact alongside Chromium's and
  WebKit's. Firefox is a gate; its evidence should not require
  scrolling a job log.
- **The native control ACK oracle** was "empty pending ∧
  `max_consumed_seen == tx_bytes_sent`", which cannot tell an
  acknowledgement from a give-up — both drain the window. It now
  observes the real ACK frontier and the terminal/reset disposition.
  The flaky zero-retransmit assertion stays retired.
- **Retirement evidence** drives the production `RtcLeafTransport`
  with a real `RTCPeerConnection` and the production event sink, and
  asserts the engine's own observed `connectionState` after
  retirement. Scope, stated plainly because an earlier revision was
  categorical: the fixture is still an instrumented composition of
  production components, not the production installed-node shutdown
  path, and it does not drive `wasm::LeafNode::connect`'s
  `ConnectGuard` cancellation — deleting that guard's arming line
  alone is not caught by these suites. Generations are now compared
  by EQUALITY everywhere the pending-failure leg reads one: the
  two-tab witness compares the predecessor's generation exactly, and
  the pending call's `leader-lost` failure is matched against its own
  STRUCTURED `generation` field rather than a `contains` on the
  Display text.
- **ABI** — the Node probes hand-drove inner stream objects while the
  output claimed "no test doubles". Node has no `RTCPeerConnection`,
  so a wasm-owned `LeafStream` cannot exist in that process; the file
  now states exactly what is real and names the browser witnesses
  that carry the real exercises. Five of those are new, against the
  built package over real WebRTC: direct stream to callback AND async
  iterator, with the forwarded options observed AT THE INNER CALL —
  the page records the object the package hands
  `LeafNode.open_stream` from inside that call. The parser reading
  the page also takes is not evidence of forwarding, and the label
  is carried nowhere downstream — sustained native traffic
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

**In-order handler entry — WITHDRAWN this round.** Round 3 took one
step past the transport repair: the unary nRPC fold chained handler
*entry* per source, polling each handler future exactly once to its
first await before passing a baton to the next call from that
source. The reviewer's analysis of that step is correct and it is
**removed**. A ready `.await` does not yield, so one long first poll
held every successor from that source; the wait on the predecessor
raced neither cancellation, nor the deadline, nor shutdown, so a
request CANCELled while queued still entered its handler when the
predecessor finally released; the entry map's sweep constant bounded
the *map*, not the queued tasks holding request bytes behind a
stalled predecessor; and the Go and synchronous-Python adapters
enqueue blocking work before the first Rust poll returns, so ordered
Rust polls never implied ordered foreign handler bodies. Net's
ordering contract is on **delivery**, and serializing application
handlers is a policy an owner asks for — with its own bounded queued
ownership and cancellation disposition — not a side effect of making
an oracle pass. Nothing is retained, so there is no owner decision
outstanding here.

**Where the ordering is measured now.** Three places, none of them
handler entry: the transport releases a reliable stream in sequence
order (wire reliability plus the native in-order hold added this
stage); the serve bridge drains its inbound receiver from a single
task; and the fold disposes of each delivered frame — deadline
refusal, duplicate refusal, in-flight registration, dispatch —
before it looks at the next one. The last of those is the piece that
lives in `cortex/rpc.rs` and it is pinned by
`server_fold_disposes_of_one_sources_requests_in_delivery_order`,
which measures at the DISPATCH boundary: every REQUEST in the burst
carries an already-elapsed deadline, so each is answered `Timeout`
from inside `apply_inbound` before it returns, with no handler and
no task in the picture — the emitted sequence per source therefore
*is* the order `apply_frame` processed that source's frames, and
interleaving a second source cannot reorder either. Its counterpart,
`server_fold_runs_one_sources_handlers_concurrently`, keeps the
other half honest: 128 parked handlers are inside their bodies
simultaneously, which is the statement that a handler which never
completes cannot wedge later calls from its source. `TRANSPORT.md`'s
ordering bullet was rewritten to say exactly this and no more.

**Over-cap stream events.** A 32 KiB native → leaf send returned `Ok`
and delivered nothing; 7 800 bytes arrived byte-exact. Refused typed
at the sender rather than fragmented, because `send_on_stream` is
transport-agnostic and only two receivers in the tree reassemble —
fragmenting generically would hand a native peer's application N
partial events as if each were a message, replacing a silent drop on
the rare path with silent corruption on the common one. `MAX_EVENT_
SIZE` is defined once and re-exported for pre-checking. It now
reaches every consumer as its own error rather than a generic
transport failure — Go was the exception until this round, where
`-118` fell through to `mesh unknown error (code -118)`; it has a
sentinel and a typed `{Size, Limit}` error now, and
`net_mesh_max_event_size()` makes the limit readable across the C
ABI that used to discard it. The harness leg that merely RECORDED
this outcome is now a
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
`.github/scripts/check-roster.py` adds a LEXICAL source preflight to
four of the rosters, so the two failure modes become distinguishable
there: *pinned but absent* up front, *present but did not run*
afterwards. Not "every roster", and not always "before the suite" —
the browser matrix is checked after its engines run and the native
RTC rosters are not checked by this script at all (§12.0 states the
reach exactly). Committing that script 100644 while ci.yml runs it
by path then cost one more round — the guard against silent gate
failure failing silently, which is at least an honest lesson.

**`#[non_exhaustive]` binds crates, not workspaces.** The attribute
was added having checked `net-mesh-wire`, forgetting that `net-mesh`,
the SDK and both bindings are downstream of it and bind exactly like
any external consumer. Four crates, three separate CI jobs. The
policy (see §12.2 — implementer's choice, reviewer-endorsed, not an
owner ruling) — handle known cases, PROPAGATE unknown ones, and a
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
was working. Fixed with RFC 6298 backoff and a budget spanning
**7.15 s**, and `give_up_horizon` now publishes the ladder so
dependents stop hardcoding a constant.

That number was misreported as 5.15 s for two rounds, and the error
is instructive: the ladder was written out by hand as
`50 + 100 + 200 + 400 + 800 + 1600 + 2000` ms and summed, which is
seven terms. `give_up_horizon` sums `retransmit_timeout` over
`0..=max_retries` — **eight** terms for `DEFAULT_MAX_RETRIES = 7`,
because the budget is the seven attempts *plus the final timeout
that gives up*, and the eighth doubling is capped by `MAX_RTO` to
another 2 000 ms. So the ladder is
`50 + 100 + 200 + 400 + 800 + 1600 + 2000 + 2000 = 7 150` ms. Every
statement of the figure now DERIVES it — a unit assertion pins
`give_up_horizon(DEFAULT_RTO, DEFAULT_MAX_RETRIES)` at 7 150 ms, so
prose and pacing cannot part company again — rather than restating a
constant, which is exactly how a hand-summed ladder went two rounds
without anyone re-adding it.

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

---

## 13. Fourth repair round

Kyra's HOLD at `f50454106` credited most of the stage: exact-head CI
55/55, Chromium **and** Firefox 26/26, hosted native 5 805 units plus
119 RTC, all 30 prior probes preserved and green, P1–P3 and X1–X10
substantially closed. Twelve new probes landed verbatim as
`leaf/tests/kyra_round3_review.rs` and reproduced **8 failures
against 4 controls** — her exact split.

### 13.0 A record correction, taken first

`477bc345e` and §12.2 attributed the N4 `#[non_exhaustive]` decision
to the owner. **No owner ruling was given.** The owner was paused and
the brief asked for a write-up of options, not a resolution. The
policy was the implementer's choice; the reviewer endorsed it
afterwards on its merits, so the code stands and the attribution does
not. §12.2 now reads "implementer's choice, reviewer-endorsed,
pending owner confirmation".

This is worth more than a correction line. Attributing a judgement
call to an owner who did not make it launders it into an instruction
and removes exactly the scrutiny it needed — nobody re-opens a
decision that looks already approved. §12.1's expiry question was and
remains genuinely open, and was stated correctly; only the N4
attribution was wrong.


### 13.1 The mode boundary — R3-1 to R3-4, one design

Round 3's promotion made a stream reliable when a reliable handle
opened on it, and left three owners disagreeing about what that
meant: the wire's ACK accounting, the consumer's reorder cursor, and
the still-open fire-and-forget handles. Four probes, one design.

The boundary is **signalled, never inferred**. `PacketFlags::
MODE_BOUNDARY` is stamped on the first reliable packet a sender puts
on a stream; that packet's sequence IS the boundary, and the flag
rides the retransmit descriptor so the signal cannot be permanently
lost. One `StreamMode` is defined in the wire and read by all three
owners, and `skippable_below()` is the whole gap rule: below the
boundary a gap is conceded and counted, at or above it the record is
held. The first stated boundary wins, so a peer cannot re-signal
upward to make a receiver concede reliable sequences it already
holds.

What that DELETES is the defect. `RESUME_CONCESSION_ARRIVALS` and the
arrival-count concession are gone: `next_expected` is `rx_ack_seq`,
so conceding a hole because eight later packets arrived emitted a
cumulative ACK for a sequence nobody received and retired the
sender's only copy.

| row | receipt (mutation → verbatim RED) |
|---|---|
| R3-1 | `rx_ack_seq` → highest received range end ⇒ ONLY `kyra_promotion_wire_ack_does_not_claim_missing_sequence` red, `ack=10` |
| R3-1 (loss) | immediate gap-NACK emission removed ⇒ ONLY `..._lost_reliable_boundary_delivers_or_fails` red, `delivered=[], events=[]` |
| R3-2 | `open_packet`'s `mode_boundary` → `None`, i.e. the signal deleted at source ⇒ ONLY `..._past_lost_faf_boundary_settles_consumer` red, `delivered=[], expected=[2..9]` |
| R3-2 (admission) | admission-time consumer promotion disabled ⇒ same probe red — so the "obligation at ADMISSION" hunk is what carries the consumer half |
| R3-3 | `skippable_below` → always `u64::MAX` ⇒ three red: the fragment row with Kyra's exact `events=[StreamData{seq:3}]`, and BOTH loss rows now silently losing sequence 1 |
| R3-4 | the reliable guard in `dispose_abandoned_groups` removed ⇒ ONLY `kyra_faf_fragment_loss_does_not_kill_following_faf_messages` red, `StreamFailed{ReassemblyAbandoned}` |

The R3-3 receipt is the one to read first: removing the boundary makes
both loss rows deliver `[2..9]` and **silently drop sequence 1** —
precisely the "do not copy the cumulative ACK into the consumer
cursor" failure the review forbade.

**Stated, not implied:** producer inheritance — a fire-and-forget
send on a promoted stream going out reliable — is NOT independently
witnessed. Removing it alone leaves 12/12 green, because
admission-time promotion already covers R3-3's schedule. Two kept
tests were added for the mechanisms the probes do not pin on their
own.

### 13.2 Credit ownership and classification — R3-5, R3-6, R3-7, L5

| row | defect | repair | receipt |
|---|---|---|---|
| **R3-5** | implicit streams reused the epoch-0 sentinel, so every implicit lifetime of a stream id shared one identity and a closed predecessor's uncommitted debit guard passed its own equality check against its SUCCESSOR — refunding committed bytes and reclaiming a sequence | implicit creation allocates from the same monotonic counter explicit opens use; the guard already compared that id, so no guard logic changed. No map guard across an await | mutation: `.or_insert_with(\|\| StreamState::new(..))` restored ⇒ exit 101, `left: (0, 65536, 0) / right: (30, 65506, 1)` — Kyra's exact collapse, with the explicit-epoch control green in the same run |
| **R3-6** | debt repaid from a delta already narrowed to `u32`, so a debt above that ceiling stranded permanently — the full consumed total had advanced the watermark, so no later grant could re-present those bytes | settle from the full `u64`, narrow only the remainder | mutation: the pre-fix `grant_add`-first form ⇒ exit 101, `left: 0 / right: 100`. **No pre-existing wire unit test covered this** — under the mutation `net-mesh-wire --lib` still reported 268 passed, so the pinned probe is the only oracle |
| **R3-7** | round 3's carrier check was an improvement and still not enough: application bytes beginning `0x13` carrying the channel's own route were still eaten as RPC | the leaf keeps a registry of the carriers the nRPC plane OWNS, consulted before the payload is touched; a frame on an unowned carrier is application bytes whatever it looks like. Reply-route and carrier fencing unchanged for frames that are RPC | mutation: the ownership predicate swapped back for round 3's payload-shape predicate ⇒ exit 101, `events=[Dropped { reason: UnknownCall }]`, with the other-route control green |
| **L5** | the stamp cap is deliberately non-evicting, and the only releases were a cumulative ack (which an exhausted stream will never get) and destroying the session — so ended streams held their slots forever and eventually refused fresh ids with `ReliableWindowFull`, defeating the recovery that error exists to offer | the terminal path retires that stream's stamps with the exact owner; the cap is unchanged | mutation: the one retirement line deleted ⇒ exit 101, `ReliableWindowFull { needed: 1, remaining: 0 }` after the real RTO ladder exhausts 8 owner streams holding all 1024 stamps |

One preserved witness could not stay byte-identical, and the change is
declared rather than buried: `a_reply_that_matches_no_call_is_
surfaced_as_well_as_counted` previously reached the call table only
because its PAYLOAD named its own carrier — exactly the residual R3-7
removes. Name, assertions and failure message are untouched; the
setup now establishes real plane ownership and a genuine late reply,
which makes it strictly stronger.

### 13.3 Native lifetimes and the nRPC disposition

**NR2** `take_abandoned()` had no production consumer: abandonment
was diagnostic only, so a first-piece capacity refusal created
neither a record nor a fence and later tails formed headless groups.
Terminals now ride a second bounded queue — separate from the
diagnostic ring, so a diagnostic reader cannot swallow a production
terminal — drained into a receive-half reset and a `StreamReset`.
Idle expiry moved onto the heartbeat tick, so a quiet session's
groups are reaped on a timer rather than waiting for traffic that may
never arrive.

**NR3** retirement markers could expire or be churn-evicted while a
captured session was still admitted, which made retirement a lookup
race. A frame is now refused AT DISPATCH when its session is not
active — before decrypt, and again immediately before the only write
that can recreate retired state — so a marker's lifetime stops being
load-bearing. All seven retirement paths share one entry point; the
replacement installer had not been deactivating the displaced session
at all.

**NR4** in-order holds reacquired by stream id WITHOUT the epoch, so
a close and reopen between receipt and insertion let an old frame
into the replacement's buffer. The accepting stream's lifetime
(incarnation plus epoch) is captured under the same guard that
accepted the sequence and carried to insertion. Evicting a hold whose
sequences were already ACKNOWLEDGED is now a typed terminal — the
receiver had told the sender those bytes arrived.

**NR6** contiguous sequence-to-offset provenance, a second piece on a
held sequence treated as a contradiction rather than a duplicate, and
`StreamReset` retiring that receive lifetime's native groups. The
leaf had all three.

**nRPC handler-entry serialization: REMOVED.** The transport reorder
repair stays; the per-source first-poll serialization was a separate
semantic expansion shipped as a side effect of making an oracle
green. The review's analysis is correct on every point — one long
first poll holds every successor, `prev.await` does not race
cancellation or deadline or shutdown so a request cancelled while
queued can still enter its handler, the sweep constant does not bound
task-owned queued requests, and the Go and Python adapters enqueue
before Pending so it proves nothing cross-language. Ordering is
measured at the transport and dispatch boundary, which is where the
guarantee lives. Application-level serialization, if wanted, needs
its own bounded-ownership and cancellation design and an owner's
decision.

### 13.4 Error parity, and the evidence corrections

**Go's `-118`.** `meshErrorFromCode` went from -117 straight to -130,
so every oversize send arrived as "mesh unknown error (code -118)" —
untypeable, and indistinguishable from a variant the binding
predates. `ErrEventTooLarge` plus `*EventTooLargeError{Size, Limit}`
unwrapping to it, keyed to the constant read from the header cgo
compiles rather than transcribed, so a renumbered enum is a compile
error and not another silent unknown. The limit crosses as
`net_mesh_max_event_size()`, a pure accessor; the preflight did not
move and the bypassing paths stay out of scope. Both headers' false
"detail string" promise is deleted rather than implemented. Receipt:
deleting the arm ⇒ `meshErrorFromCode(-118) = mesh unknown error
(code -118), want ErrEventTooLarge`, with the limit accessor green as
the control.

Corrections, each executed rather than asserted:

- **The leaf oversize witness** now requires the exact refusal — kind
  `wire`, the fragmentation-ceiling message, and a parsed byte count
  ≥ the payload — instead of any nonempty error kind. A **spec
  discrepancy is reported**: the brief asked for the exact
  `EventTooLarge` kind, but that is the NATIVE sender's variant; the
  leaf leg is a 96 KiB call refused by the leaf's own fragmentation
  ceiling, so requiring that literal would assert a kind production
  never emits. Leg 3b already asserts `EventTooLarge` by value.
- **The ABI label check** observes the forwarded label AT THE INNER
  CALL. The page previously called `effective_stream_options` itself,
  with `BrowserNode.openStream` nowhere in between, so a wrapper that
  dropped only `label` left the assertion intact — and nothing
  downstream carries the label.
- **Browser pending-error generation** compares by EQUALITY against
  the structured `RpcError.failure.generation`; `contains` was
  satisfied by 1 inside 31.
- **The below-SCTP hook now has a caller.** Two attempts failed
  first, and both are worth recording. Attempt one armed immediately
  after the handshake and hit a leftover SCTP acknowledgement: 16
  qualifying datagrams counted, every sequence delivered, ZERO
  retransmits — *the instrument fired and hit nothing*, which is the
  failure mode that makes any injector receipt worthless. A quiesce
  before arming fixes it and is commented as load-bearing. Attempt
  three **confirms §11.8 rather than correcting it**, against the
  implementer's own written prediction: one lost `msg1` IS terminal,
  and not for want of retransmission — the initiator does resend
  byte-identically, but `accept_rtc` is one-shot, spends its deadline
  on the copy that was dropped, and is no longer listening when the
  resend lands. Below SCTP, recovery needs something that RE-ARMS the
  far side, and only `reliability.rs` does.
- **The backoff ladder sums to 7.15 s, not 5.15 s**, and the bug was
  a DROPPED TERM rather than a bad sum: `give_up_horizon` covers
  `0..=max_retries`, i.e. eight steps, and the eighth is a second
  `MAX_RTO` cap. Anyone re-deriving from the old seven-term list gets
  5.15 s again and concludes the code is wrong. A new test derives
  the ladder from `retransmit_timeout` and asserts its SHAPE — step
  count, each value, that the cap is reached — before the total, so a
  change that shortened one step and lengthened another fails.
- **The roster checker says what it is**: a LEXICAL PREFLIGHT that
  parses neither language, evaluates no cfg, and is satisfied by a
  commented-out declaration. It also does not run before every suite
  — the browser roster runs after both engines execute, and native
  RTC has no preflight of this kind. The earlier claim that every
  roster is checked before its suite runs was false and is corrected
  in §12.0, §12.5 and §12.7.
- **Six native witnesses were running unpinned**, plus two in
  `rtc_routed_restore` whose floor was 4 against 5 actual tests.
  All pinned; floors raised to the real counts — `rtc_repairs` 42,
  `rtc_routed_restore` 5, and the leaf's native floor 209 → 249,
  which had been 40 tests of slack, enough to lose five whole
  binaries and still pass.

### 13.5 Two regressions this round caused, and its own evidence caught

Both were found by the browser matrix after every lane reported
green, and both were diagnosed from evidence earlier rounds had added
to the witnesses rather than by re-running until something looked
different.

**The mode boundary was stamped by the leaf and read by nobody
else.** R3-1's `PacketFlags::MODE_BOUNDARY` had exactly one reader in
the tree — the leaf — because `ensure_reliable_at` had exactly one
caller. The native receive path still used the conservative ASSUMED
boundary of "rx high-water + 1", and the same commit deleted
`RESUME_CONCESSION_ARRIVALS` from the SHARED wire path, which is what
had been covering the native receiver until then. Neither mechanism
was left.

The consequence is exact, not probabilistic: the fire-and-forget
witness rides the same stream id and always elides its LAST datagram,
so the anchor's assumed boundary named a fire-and-forget sequence
whose sender retained no descriptor. Nothing could ever produce it,
every reliable arrival above it was held, the cumulative ack never
advanced, and the ladder exhausted into a typed failure — on a link
where every reliable byte was recoverable. The counters said it
plainly: `stream_failed: 2` with every gap and duplicate counter at
zero.

The boundary now lives on the one call every receive path makes, so a
third path cannot forget it, and the leaf's separate call is deleted
rather than left as a second way to do the same thing. A leaf-to-leaf
reproduction of the browser's exact shape recovers 40/40 — both ends
of a leaf pair stamp AND read the signal, which is why no leaf probe
could ever have seen this. The cheap witness is pinned.

**An oracle outlived the mechanism it measured.** Round 3 added the
per-source handler-entry chain and a witness leg for it in the same
commit. Round 4 removed the chain — deliberately, on the reviewer's
analysis, with two tests pinned for the opposite — and left the leg
measuring handler ENTRY order, which the fold no longer provides and
Net does not promise. Measured with a throwaway probe: 40 requests
handed to `apply_inbound` in order, fully synchronous handler, enter
as 1, 0, 2, 4, 3, 5, 14, 21, 6, with no transport involved at all.

The leg moved to the boundary where the contract lives, via a
fixtures-gated observer firing INLINE in the serve bridge's own task
immediately before `apply_inbound` — the hand-off itself, so an
observation cannot reorder relative to the dispatch it observes.
Every other assertion is unchanged, and the two pinned fold tests
were not touched. This is a re-scope and the witness text says so:
restoring entry order instead would have reinstated, through the back
door, the semantics this round was told to withdraw.

**And a process finding worth as much as either.** `leaf/pkg` and
`browser-ts/dist` on disk predated this round's leaf commits by two
hours, so part of a failing matrix run was measuring a stale bundle.
A browser result is only about the tree if the bundle was rebuilt
from it; the rebuild is now documented as part of running the matrix:

```
cd net/crates/net/leaf && cargo build --release --target wasm32-unknown-unknown \
  && wasm-bindgen --target web --out-dir pkg target/wasm32-unknown-unknown/release/net_leaf.wasm
cd net/crates/net/browser-ts && npm run build
```

### 13.6 The day the disk filled

Recorded because it shaped the round's evidence and because the
detection technique is worth keeping. C: reached zero bytes free
twice while five lanes were editing. ENOSPC on a write TRUNCATES the
file rather than rolling back, and `git status` reports the result as
an ordinary ` M` — indistinguishable from a normal edit. Four source
files were truncated to zero or one line across the two events
(`go/mesh.go`, `leaf/src/session.rs`, `wire/src/protocol.rs`,
`tests/rtc_repairs.rs`).

Nothing shipped truncated, for one reason: every lane compared line
counts against `git show HEAD:<path>` and then re-verified by
CONSTRUCT — grepping for each symbol it had added — rather than
trusting a plausible-looking file. One lane also restored a shared
file from a `cp` backup and correctly flagged that it could have
rolled back a sibling's landed hunk; it had not, and the technique
was dropped in favour of targeted edits for the rest of the round.

The lesson that belongs in the repo rather than in a person's memory:
after any ENOSPC, line count is the cheap detector and construct
grep is the real one, and a restore that rewrites a whole shared file
is never safe while siblings are editing it.

### 13.7 Owner questions — stated, not decided

Four, none of them ours to settle. Each is stated with what we would
recommend and what it costs, so a decision is cheap to make and
nothing is pre-empted by our silence.

**1. Announcement expiry: leaf inclusive vs native `age >= ttl`.
RULED — match native. Implemented in §14.1.** The leaf held
`now <= floor(issued) + ttl` at second granularity; the native side
expires at `age_secs >= ttl` at nanosecond precision, and documents
that it matches `PermissionToken::is_valid`. A TTL-zero announcement
was authoritative on the leaf and already dead natively. The leaf now
uses the native predicate.

**A wording correction, because the error is the kind that survives
into an implementation.** This item previously called the native rule
"inclusive expiry". It is the opposite: `age >= ttl` means the
instant `stamp + ttl` is EXPIRED, so the live interval is the
half-open `[stamp, stamp + ttl)` and the rule is exclusive at its
upper end. The leaf's old rule was the inclusive one. Getting this
backwards in a sentence is free; getting it backwards in a comparison
operator ships a peer that is an authority for one second after it
said it would not be. Cost, as predicted: two leaf tests that pinned
the TTL-zero-within-its-second case moved to the opposite assertion.

**2. The N4 policy itself. RULED — owner-confirmed 2026-09-16;
version bump and release note deferred to the release process.**
`#[non_exhaustive]` on `StreamError` and `StreamStats` plus
`StreamStats::empty()` as the construction seam stand as policy. The
code does not move: it was already shipped and reviewer-endorsed, and
the confirmation changes its STATUS, not its content — so this ruling
is recorded in §12.2 and here and produces no code change, which is
the honest outcome rather than a cosmetic edit to look busy. Marking
an existing public type non-exhaustive remains a breaking change and
must be versioned as one; the release owner handles the bump and the
note when the branch ships. We deliberately did not take the release
work and still have not.

**3. Direct `BrowserNode.close` and iterator lifetime. RULED — end
iterators on parent close. Implemented in §14.3.** Pre-existing and
not introduced by this stage: a direct wrapper's stream iterators
were not ended when the parent node closed, so a consumer awaiting
one hung past close — the wasm side stops calling `on_message` and
nothing else could settle the queue. The leader-proxied path has
always ended them on a generation change or a leadership loss (L2),
so the direct surface was the outlier and it is the direct surface
that moved. The behaviour change is stated in the package's README
and CHANGELOG as a behaviour change, in the words a consumer would
search for, together with which of the two paths moved.

**4. Is above-one-event fragmentation a Stage 5 contract, or an
explicit bound? RULED — supply the interoperability leg. Implemented
in §14.4.** The recommendation in this item was the opposite: write
the bound down and scope reassembly separately. The owner ruled for
the work, and the work is done — the native stream receive path
reassembles leaf fragment groups (which, as §14.4 records, already
worked and is now pinned), and the native sender fragments for a peer
advertising `net.stream.fragment_reassembly@1`.

**The overstatement this item names is now retired at its source.**
The large-message witness proved near-ceiling delivery and a typed
refusal, and the report should have stopped being read as though it
proved multi-fragment interoperability. It no longer needs the
caveat: leg 3b of that witness has changed sides and now requires a
32 KiB native → leaf payload to ARRIVE as one byte-identical event,
counting arrivals so that a group delivered as five pieces fails. Both
directions are witnessed, and the bound that remains — 64 832 B, one
number now shared by both ends instead of two that differed by 32
bytes — is a refusal, not a silence.


## 14. Fifth round — the four owner rulings

Round 4 closed every item the third review had established and stopped
at four questions only the owner could settle (§13.7). All four are now
ruled, and this round implements the rulings and nothing else.

**Two framing facts a reviewer should have before reading the rows.**

First, **ruling 4 is core work inside a Stage 5 round, by owner
decision.** Native fragment reassembly and gated native-side
fragmentation are changes to the core transport, not to the browser
spike, and they were taken here because the owner decided the
interoperability leg should be supplied now rather than scoped away.
It should be reviewed as a core change — the same bar as any other
change to the stream path — and not given spike latitude because of
the section it is written in. Saying so is the point: the riskiest
edit in this round is the one most easily waved through as "part of
the browser work".

Second, **this round stacks additively on round 4 (`e342007a0`), which
has not been reviewed.** Nothing here rewrites a round-4 decision, so a
verdict on round 4 cannot be contradicted by anything below.

One commit per ruling. Each row states the ruling, the change, the
witness, and the raw inverse receipt.

### 14.1 Ruling 1 — announcement expiry matches native

**Ruled:** match native. Nanosecond precision, expire at `age >= ttl`,
including the TTL-zero and fractional cases. One expiry rule for one
announcement type.

**The change.** The leaf's freshness predicate is now the native one:

```
is_fresh_at_nanos(now) = now.saturating_sub(timestamp_ns)
                       < u64::from(ttl_secs) * 1_000_000_000
```

This is not a nanosecond *approximation* of the native rule, it is the
same rule: native computes `age_secs = (now_ns - ts_ns) / 1e9` then
tests `age_secs >= ttl`, and for an integer `ttl`,
`floor(age_ns / 1e9) >= ttl` holds exactly when `age_ns >= ttl * 1e9`.
No precision is lost and none is invented.

Three arithmetic details, each of which is a defect if taken casually:

- **A clock that went backwards cannot expire the store.** `age` uses
  `saturating_sub`, so a stamp in the future — a peer's clock ahead of
  ours, or our own clock stepped back between stamp and read — yields
  age zero, the youngest possible, instead of wrapping `u64` into
  roughly 584 years and expiring every record at once. A clock
  disagreement can only grant unearned lifetime, never fabricate
  expiry. Native saturates in the same place.
- **The TTL bound cannot wrap.** `ttl_secs` is `u32`, so the widened
  product tops out at `4.295e18`, comfortably below `u64::MAX`
  (`1.845e19`). The widening is the proof; it is documented on the
  predicate rather than left to be re-derived.
- **The store's readers take the reading as a parameter.**
  `get_at_nanos` / `query_at_nanos` / `resolve_routing_id_at_nanos`,
  with the un-suffixed callers reading `clock::now_unix_nanos()` once,
  so a single scan's answer cannot change because a second ticked
  mid-iteration.

**The two moved tests.** Both pinned the TTL-zero-within-its-issuing-
second case, which the ruling deletes. They now assert the opposite,
correct outcome, and each carries the ruling's date and its reason in
its comment:

| was | is | the flip |
|---|---|---|
| `the_authority_lookup_expires_one_second_after_issue_plus_ttl` | `the_authority_lookup_expires_exactly_ttl_nanoseconds_after_the_stamp` | `stamp + ttl` was asserted `Some`; it is asserted `None`. Gained a `Some` at `stamp + ttl - 1ns` and a `Some` at `(issued + ttl) * 1e9`, so the stamp's sub-second remainder is proven no longer truncated away |
| `a_zero_ttl_announcement_is_authoritative_only_within_its_issuing_second` | `a_zero_ttl_announcement_is_expired_from_the_instant_it_was_stamped` | `get_at(node, issued).is_some()` and a one-row `query_at` became `is_none()` and empty. Gained a read one nanosecond BEFORE the stamp that still gets `None`, pinning the saturating age |

Neither is weakened: the boundary moved from a second to a nanosecond
and each test gained an assertion it did not have.

**The production authority-lookup witness** Kyra asked for, and it is
the real path, not a helper:
`node::tests::an_expired_announcement_is_not_discoverable_and_not_a_signal_authority`
drives `LeafNode::on_datagram` with real `0x0C00` capability-
announcement frames over a real anchor session, through
`ingest_announcement` → `verify_announcement` → `AnnouncementStore::
ingest`, and then reads back through **both** production readers: the
discovery one (`LeafNode::query` → `query_json` → `query()`, which is
what the wasm surface's `query` resolves to) and the authority one
(`LeafNode::accept_signal` → `self.announcements.get(envelope.from)`,
`node.rs:2401` — the accessor the signal verifier takes the peer's
Ed25519 key from), reached by a real `0x0D02` signal frame.

It asserts three negatives — absent from `query`, `announcement_for`
is `None`, the real signed envelope is refused with
`DropReason::SignalRejected` advancing by exactly one — and a fresh
control peer ingested and signalled identically that is discoverable,
held, and accepted. Without the control, all three negatives would
pass on a path that was simply broken.

**Raw inverse receipt.** Restore the pre-ruling leaf rule, one line out,
two in, in `is_fresh_at_nanos`:

```
-        self.age_nanos(now_unix_nanos) < u64::from(self.ttl_secs) * NANOS_PER_SEC
+        let issued = self.timestamp_ns / NANOS_PER_SEC;
+        now_unix_nanos / NANOS_PER_SEC <= issued.saturating_add(u64::from(self.ttl_secs))
```

`cargo test -p net-mesh-leaf --features mock-control-plane --lib -- --exact`
on the three names, exit **101**:

```
test announce::tests::the_authority_lookup_expires_exactly_ttl_nanoseconds_after_the_stamp ... FAILED
test announce::tests::a_zero_ttl_announcement_is_expired_from_the_instant_it_was_stamped ... FAILED
test node::tests::an_expired_announcement_is_not_discoverable_and_not_a_signal_authority ... FAILED

---- the_authority_lookup_expires_exactly_ttl_nanoseconds_after_the_stamp ----
panicked at src\announce.rs:922:9:
at age == ttl the peer has no authority left: the interval a ttl of 300
covers is [stamp, stamp + 300), half-open. The old inclusive rule called
this instant fresh because it fell inside second 1700000300

---- a_zero_ttl_announcement_is_expired_from_the_instant_it_was_stamped ----
panicked at src\announce.rs:984:9:
read at its own stamp, a zero-TTL record has already spent every
nanosecond it declared

---- an_expired_announcement_is_not_discoverable_and_not_a_signal_authority ----
panicked at src\node.rs:3333:9:
a peer whose announcement is 300s old with a 300s ttl is past its declared
lifetime and must not answer a capability query — got [{"node_id":"787399…
"capabilities":["expiry.witness","leaf","transport:rtc"],…}, {…}]

test result: FAILED. 0 passed; 3 failed; 207 filtered out
```

The `node.rs` red is itself the proof the witness rides the production
lookup: the failure message carries the JSON that `node.query(CAP)`
actually returned, with both peers in it. Restored: `3 passed`, exit 0.

**Counts and cross-checks.** Leaf 269 → **270** (+1 witness; the two
moved tests were renamed, not added). `cargo check --target
wasm32-unknown-unknown` clean. The `cross_lang_wire` fixtures that
carry an announcement with a TTL still decode on both sides: **13
passed, unchanged** — the ruling changes when a record is *believed*,
not how it is *encoded*, and that distinction is why no fixture moved.

### 14.2 Ruling 2 — the N4 policy, confirmed

**Ruled:** owner-confirmed 2026-09-16; the version bump and release
note are deferred to the release process.

**No code.** `#[non_exhaustive]` on `StreamError` and `StreamStats`
plus `StreamStats::empty()` were already shipped and
reviewer-endorsed; the ruling changes their STATUS, not their
content. The only change is §12.2's heading and status line and
§13.7 item 2. A cosmetic code edit to make the ruling look
implemented would have been worse than nothing.

**What is deliberately not done.** Marking an existing public type
non-exhaustive is itself a breaking change and must be versioned as
one. The release owner handles the bump and the note when the branch
ships.

**The attribution correction stands unchanged.** §12.2 and §13.5
record that an earlier draft attributed this decision to an owner who
had not made it, that the policy was the implementer's choice, and
that the reviewer endorsed it afterwards. That history is not rewritten
now that the owner has agreed — the correction's point was that a
judgement call must not be laundered into an instruction, and a
later confirmation does not make the earlier attribution true.

### 14.3 Ruling 3 — a direct node's close ends its iterators

**The ruling.** `BrowserNode.close()` ends every stream iterator the
node handed out — the same terminal the leader-proxied path emits on
a generation change (L2) — so a consumer parked in `for await`
completes rather than hangs, and a subsequent `openStream` on a
closed node is a typed refusal. A behaviour change; said so in the
package docs.

**The change.** No new terminal was invented: the one L2 already
uses is `LeafStream.close()`, which ends each registered
`AsyncQueue` (so an iterator resolves `{ value: undefined, done:
true }` — the normal end of iteration, not a rejection) and clears
the listeners. `BrowserNode` now retains the streams it hands out
and drains that set in `close()`, exactly as `MeshSession.endStreams`
does, **before** `inner.close()`: the leaf retires a stream handle
*through* the node, so a handle closed after the node is never
retired at all. `LeafStream` gained an `@internal onClosed`
callback — the shape `AsyncQueue` already uses for a departing
consumer — so a page that opens and closes a stream per frame does
not grow the retained set for the node's lifetime.

The typed refusal needed no new code and, deliberately, no second
fence in TypeScript: `Inner::admit` in `leaf/src/wasm.rs` already
refuses every outbound operation on a closed node with
`session: the node is closed: it no longer holds this origin's identity`,
and `openStream` re-types it as `SessionError` (`kind: 'session'`)
through the same `fromWasmError` path every other boundary failure
takes. What was missing was that this is a *contract*: it is now
declared on `LeafWasmNode.open_stream` in `src/wasm.ts`, spoken by
the unit-suite double (`test/leaf-abi.ts`'s `NODE_CLOSED_REFUSAL`),
and — so the double cannot drift from Rust the way the Stage 5 stream
callback did — extracted from `leaf/src/wasm.rs`'s source by the
real-package probe and compared against both. `Inner::admit`'s
doc comment names that coupling from the Rust side; that is the only
change to `wasm.rs` and it is documentation.

Files: `browser-ts/src/node.ts`, `src/stream.ts`, `src/wasm.ts`,
`test/node.test.ts`, `test/fake-wasm.ts`, `test/leaf-abi.ts`,
`tests/abi_real_package.mjs`, `README.md`, `CHANGELOG.md`;
`leaf/src/wasm.rs` (doc only).

**The witnesses.** Through the real built package,
`tests/abi_real_package.mjs`, 12 → 14 probes (the 12 are unchanged):

- `real_package_ends_a_parked_iterator_when_the_direct_node_closes` —
  the built `connect()` → `BrowserNode.openStream` → `LeafStream`
  chain: park an iterator with nothing buffered, `node.close()`, the
  iterator completes with the terminal; the handles are retired
  before the node; a further `openStream` refuses with
  `kind: 'session'` and the verbatim Rust `Display`.
- `real_package_re_types_the_leafs_closed_node_fence` — the fence
  text read out of `leaf/src/wasm.rs`, re-typed by the built package
  as `SessionError`, and the unit-suite double asserted to speak the
  same sentence.

What is real and what is not, stated in the file and in its evidence
label: Node has no `RTCPeerConnection`, so no wasm-owned node or
stream can exist in the process. Everything the ruling touched is
compiled `dist/` code; the stand-in is the transport, and it is never
*driven* — it emits no bytes and answers no call, because the
property under test is precisely what a consumer sees when nothing
arrives again. The fully-real direct and proxied stream exercises
remain the Stage 5 browser witnesses.

And in the unit suite, `npm test` 172 → 175:

- `ends an iterator parked in for-await when the node closes, so the
  consumer completes`
- `refuses a stream on a closed node with the leaf's typed session
  refusal`
- `retires every stream handle through a live node, and each one only
  once`

**The raw inverse receipt.** The diff that undoes the behaviour —
the three lines of `BrowserNode.close` that drain the retained set:

```diff
--- a/net/crates/net/browser-ts/src/node.ts
+++ b/net/crates/net/browser-ts/src/node.ts
@@ close(): void {
     if (this.closed) return;
     this.closed = true;
-    const open = [...this.streams];
-    this.streams.clear();
-    for (const stream of open) stream.close();
     this.hub.close();
     this.inner.close();
   }
```

`cd net/crates/net/browser-ts && npm test` — exit **1**:

```
⎯⎯⎯⎯⎯⎯⎯ Failed Tests 2 ⎯⎯⎯⎯⎯⎯⎯

 FAIL  test/node.test.ts > BrowserNode > ends an iterator parked in for-await when the node closes, so the consumer completes
AssertionError: promise rejected "Error: the iterator parked before node.cl…" instead of resolving
 ❯ test/node.test.ts:352:6
    350|     await expect(
    351|       withinDeadline(parked, 250, 'the iterator parked before node.clo…
    352|     ).resolves.toEqual({ value: undefined, done: true });
       |      ^
    353|     expect(inner.streams[0]?.closed).toBe(true);
    354|   });

Caused by: Error: the iterator parked before node.close() was still pending after 250ms
 ❯ Timeout.<anonymous> test/node.test.ts:510:37

⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯[1/2]⎯

 FAIL  test/node.test.ts > BrowserNode > retires every stream handle through a live node, and each one only once
AssertionError: expected [ 'stream', 'node' ] to deeply equal [ 'stream', 'stream', 'node' ]

- Expected
+ Received

  [
    "stream",
-   "stream",
    "node",
  ]

 ❯ test/node.test.ts:394:28
    392|     // stream the page had already closed, which the leaf would refuse
    393|     // as a handle it no longer holds.
    394|     expect(inner.teardown).toEqual(['stream', 'stream', 'node']);
       |                            ^
    395|   });

⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯[2/2]⎯

 Test Files  1 failed | 6 passed (7)
      Tests  2 failed | 173 passed (175)
```

`npm run build && node tests/abi_real_package.mjs` — exit **1**:

```json
{
  "name": "real_package_ends_a_parked_iterator_when_the_direct_node_closes",
  "pass": false,
  "error": "the parked iterator was still pending 250ms after node.close()"
}
```

Note that the typed-refusal assertion stays **green** under this
inverse: the fence is Rust's and is witnessed independently of the
terminal, which is the point of not adding a second fence in
TypeScript.

Restored: `npm test` → `Tests 175 passed (175)`, `Test Files 7 passed
(7)`; `node tests/abi_real_package.mjs` → exit 0, 14 probes, 0
failed.

The deadline is a **bounded race**, not a widened timeout: "never
settles" is not observable by waiting longer, so `Promise.race`
against a 250 ms timer that *fails the test* is the assertion. It
costs no wall clock when green — the race resolves on an
already-settled promise and the timer is cleared in `finally` (suite
duration 353 ms green vs 596 ms with the inverse applied).

### 14.4 Ruling 4 — native reassembly, and gated native fragmentation

**Ruled:** supply native reassembly now. The bound-as-contract option
is rejected. **This is core work inside a Stage 5 round by owner
decision** — see the framing note at the head of §14.

#### What was already true, established before anything was built

The brief's witness (a) — leaf → native, 40 000 B, one event,
byte-identical — was written FIRST and run against unmodified code.
**It passed** (`a_leaf_fragmented_payload_reaches_a_native_peer_as_one_event
... ok`, 0.29 s).

The receive leg already worked. `reassemble_rtc_fragments` (rounds
3–4) is wired into event-plane dispatch for every RTC source, and a
native stream receive IS that path. Reporting that honestly matters
more than appearing to have built it: the ruling asked for a leg that
was, for the receive direction, already standing, and the witness that
proves it is now pinned so it cannot quietly stop being true.

All genuinely new work is therefore the **send** half, plus one
ceiling correction.

#### The send half

`send_on_stream` now decides a size disposition before any piece
exists: at or under `MAX_EVENT_SIZE`, unchanged; above it, fragment
for a peer that reassembles, refuse typed for one that does not.

The gate is **two-factor and neither factor is optional**
(`peer_reassembles_fragments`):

1. the peer advertises `FRAGMENT_REASSEMBLY_TAG` in its folded
   capability set, and
2. the resolved peer address is `PeerAddr::Rtc`.

Factor 2 is not belt-and-braces. `reassemble_rtc_fragments` returns
early for any non-RTC source by deliberate design, so fragmenting
toward a UDP peer that advertised the tag would hand its application N
partial events. Reassembly is not widened to UDP in this round, so the
sender must not pretend otherwise.

The tag is `net.stream.fragment_reassembly@1`, a plain signed tag
beside `RELAY_CAPABLE_TAG` — not a new canonical field, as the brief
required. It confers no authority and obligates nothing: a receiver
reassembles whether or not it says so, and the tag exists for the
**sender's** benefit.

The browser leaf advertises it unconditionally, and that is a
deliberate choice rather than a shortcut: `frame::Reassembler` is
always on the leaf's receive path, so the tag can never be a claim the
leaf fails to honour. A conditional tag would be a capability
depending on state the peer cannot see.

#### The ceiling correction — a real divergence, found by building

The native reassembler's ceiling was `MAX_PAYLOAD_SIZE * 8 = 64 864`;
the leaf's producer ceiling is `MAX_EVENT_SIZE * 8 = 64 832`. **The
two ends were carrying two different numbers**, 32 bytes apart, and
the larger one was the receiver's — so the receiver would have
accepted a group no conformant producer can emit. `wire/src/protocol.rs`
now defines `MAX_FRAGMENTS_PER_GROUP` and `MAX_FRAGMENTED_EVENT_SIZE`
once, with a compile-time assertion that the last piece's start offset
fits the header's `u16`, and the RTC reassembler reads it. This is the
brief's "a constant genuinely requires it" exemption, and it is the
kind of defect that only surfaces when someone writes the other side.

#### One reassembler, and where the leaf deliberately differs

No second reassembler was written. New seams, named as the brief
requires: `MeshNode::flush_stream_fragment_group`,
`MeshNode::next_fragment_id`, `peer_reassembles_fragments`, and the
fixtures-only `RtcTestHooks::set_ingress_drop_at` / `ingress_counted`.

`RetransmitDescriptor` gained `fragment: Option<FragmentStamp>` so a
retransmitted piece restamps its fragment header — witness (c) is what
forced it; without it the receipt reads `Got 8104 bytes of 40000`.
`PacketBuilder::set_fragment` remains the single stamping seam, with
exactly three call sites in core (the fragmenting send and the two
retransmit rebuilds).

**The leaf sets `fragment: None` and keeps its own stamp table**, and
the reason is worth recording because the tidy-looking alternative
would have deleted two shipped repairs. The leaf's `stamps` table is
not a duplicate stamp carrier, it is an **admission reservation**:
`build_packets` refuses a whole message with `ReliableWindowFull`
against `MAX_RETRANSMIT_STAMPS` before consuming a sequence (F3b), the
cap is non-evicting since L5 repaired an eviction that stole another
stream's ownership, and `rebuild` skips a descriptor whose reservation
is gone. Folding the stamp onto the descriptor leaves that reservation
with nothing to count, and `a_full_stamp_table_refuses_a_new_stream_rather_than_evicting_an_owned_one`
along with the F3b and L5 receipts would stop meaning anything. The
native path has no equivalent table — its admission is byte credit plus
the reliability window — so the field is the native carrier and the
leaf's table stays the leaf's gate. One fact per mechanism; they are
different facts.

#### `MAX_EVENT_SIZE` did not change meaning

It is still the largest single event one packet carries, so **no
Go/Node/Python typed error or parity test moves**. What becomes newly
reachable is a second *value* in `EventTooLarge.limit` — 64 832, the
fragmentation ceiling — and only for a peer that advertises the tag. A
binding parity test that sends over-cap to a peer without the tag still
observes limit 8 104, unchanged.

#### The five witnesses, and what each is for

In `tests/rtc_repairs.rs`, **44 → 49** (the brief said 43; 44 was the
measured count at this head, and the discrepancy is reported rather
than silently absorbed). All five are pinned by name in `ci.yml` with
the floor raised 43 → 49.

| witness | what it proves |
|---|---|
| `a_leaf_fragmented_payload_reaches_a_native_peer_as_one_event` | (a) leaf → native, 40 000 B, one byte-identical event. Passed before the change; pinned so it stays true |
| `a_native_sender_fragments_for_a_peer_that_advertises_reassembly` | (b) the **sender** emits a conformant group: cut at `MAX_EVENT_SIZE`, contiguous sequences in offset order, reassembled byte-identically |
| `a_lost_middle_fragment_is_retransmitted_and_the_payload_arrives_once` | (c) a lost middle piece is recovered by the existing reliability machinery and the payload arrives ONCE — the witness that forced the descriptor's stamp |
| `a_group_over_the_ceiling_is_refused_at_the_first_piece_on_both_sides` | (d) typed refusal at the FIRST piece, never a partial |
| `a_peer_without_the_reassembly_tag_still_gets_event_too_large` | (e) native ↔ native unchanged: no tag ⇒ typed `EventTooLarge` at 8 104 |

Six raw inverse receipts accompany them (reassembly disabled, the gate
removed, the ceiling check removed, and three narrower ones).

#### Two claims this round does NOT make

**(b) is not proof that a real browser leaf accepts those bytes.**
There is no browser leaf inside `tests/rtc_repairs.rs`; (b)'s "leaf" is
an RTC peer advertising the tag. It proves the sender emits a
conformant group. The only witness anywhere that a **real** leaf
reassembles what the native sender cut is the browser matrix's leg 3b,
described below. Both are needed; neither is the other.

**(e) is not proved twice.** Leg 3b used to assert the typed refusal
native → leaf. After this ruling it asserts delivery, so it has changed
sides, not become a second copy of (e). The "no tag ⇒ `EventTooLarge`
at 8 104" invariant is now witnessed in exactly ONE place, (e), and
that is stated here rather than left for a reviewer to discover by
counting.

#### The browser witness changed sides

`stage5_large_messages_cross_the_public_api_in_both_directions` leg 3b
sends 32 KiB native → leaf. It required `StreamError::EventTooLarge`;
it now requires the payload to ARRIVE as one byte-identical event, and
its gate counts arrivals: **a group that was never reassembled arrives
as five pieces, not as nothing**, and every weaker check — "the bytes
arrived", "a matching mark arrived" — would call that a pass.

The round-4 comment said fragmentation "was rejected on the merits …
fragmenting here would hand a native peer's application N partial
events". That reasoning was never wrong, and it is preserved rather
than deleted: it was an argument against fragmenting **blindly**, and
it is now the gate's justification. The comment, the constant's
rustdoc, `send_on_stream`'s own doc, and the verdict string all say so
in those terms, because a comment that contradicts the code it sits
above is worse than no comment.

The leaf's pinned announcement fixture moved with the new tag — both
copies and the core-side pin, in the same commit. The core-side check
now compares against the core's own `FRAGMENT_REASSEMBLY_TAG` constant
instead of a repeated literal: a leaf spelling the tag differently from
the core would not produce a refusal, it would produce a silent
fallback to the 8 104-byte cap that nothing else would notice.
