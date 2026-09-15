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

**What v1 implements.** `AnchorControlPlane` carries envelopes for
the bootstrap dialog it already owns; the leaf verifies every inbound
envelope through the same path a serverless carrier would use. The
trait admits the specified path because `ControlPlane::signal` takes
a peer id and an envelope, *not* a session — that is the shape
decision the follow-on depends on. The native side's `0x0D02`
in-session frames are untouched: this is an additional, weaker-coupled
path, not a replacement, and Stage 5 does not change any native
signalling behaviour.

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
4. **No native node reassembles fragments.** `leaf/src/frame.rs` is
   the first and only reader of `frag_flags` in the tree, so an
   over-cap payload works leaf ↔ leaf but a native peer sees N
   partial events. Nothing regresses (the prior behaviour was S0c's
   silent drop) and the harness keeps payloads under 8 104 B.
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

### 9.2 Payload-size interoperability: exactly what is claimed

**Claimed.** A leaf fragments an application payload larger than
`MAX_PAYLOAD_SIZE` (8 104 B) into up to 8 fragments and reassembles
inbound fragments with a bound (8 concurrent groups, 2 s TTL, every
refusal counted). Leaf ↔ leaf, an over-cap payload round-trips; this
is witnessed natively and in wasm.

**Not claimed.** No NATIVE node reassembles fragments.
`leaf/src/frame.rs` is the first and only reader of `frag_flags` in
the tree, so a leaf → native payload above 8 104 B arrives as N
packets each carrying a partial event, and the native side does not
put them back together. Nothing regresses — the pre-Stage-5
behaviour for such a payload was S0c's silent drop — but "Stage 5
made over-cap payloads work" is true only between leaves.

**Also not claimed.** Above 64 832 B nothing is attempted at all:
`fragment_offset` is a u16 BYTE offset, so the last fragment cannot
start past byte 65 535. The send path returns a typed
`LeafError::Wire` naming streams as the way out — never a truncation
and never a drop.

**What the matrix therefore keeps under the cap.** Every browser
witness sends payloads below 8 104 B deliberately, because a witness
that quietly relied on native reassembly would be asserting a
property this stage does not have. A native-side reassembly arm is
the prerequisite for leaf → native over-cap payloads, and it is
named in §8.

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
| **R8** | replay key carries a BLAKE2s payload digest; hard cap 4096 with oldest-first eviction; freshness enforced at read time for discovery AND key authority, literal `issued + ttl` | `kyra_two_distinct_ice_candidates_in_one_dialog_survive_dedup`, `kyra_expired_announcement_is_not_discovery_or_signal_authority` | restore the `(from, dialog, kind)` key → the second candidate is refused; skip the freshness filter → an expired peer is discoverable and still a signal authority | multi-candidate traffic, re-announce restoring discovery |
| **R9** | stand-down is ordered: revoke the generation lease, shut the backend down (cancel in-flight, fail pending once, typed `LeaderLost`), then release the lock; every outbound effect consults the lease; `IdentityVault::fence` gained its caller (interval revalidation against the store) | `standing_down_fences_then_retires_the_node_then_releases_the_lock` over a real node with a real Noise session, a real pending call and an operation parked before dispatch; `a_leader_revalidates_its_generation_against_the_store_and_stands_down` | delete the `revoke` → the fencing witness fails | the ordinary leader/follower lifecycle |
| **R10** | attaches queued until the backend exists and replayed on the first Leadership broadcast; `closed` checked at publish with requeue/abort; stream handles carry their opening generation and end their iterators on lifecycle loss; `announce()` updates restoration state; followers own a local deadline with an honest indeterminate outcome | one witness per schedule in `wasm_leader.rs` + the TS leader tests | drop the attach queue → subscriptions vanish across promotion | promotion, restoration and follower calls |
| **R11** | one contract: Rust emits its canonical event JSON and TS decodes it; numeric `channelHash`; `RTCIceServer` parsed with credentials; ids matched numerically; the fake WASM mirrors the real shape | 10 ABI probes against the REAL built package, direct and leader-proxied, plus Kyra's two TS probes | reintroduce the string/bytes mismatch → `typescript_real_rust_callback_shape` reds | `typescript_byte_callback_control` |
| **R12** | native `Stream` handles carry the session incarnation; send/close/credit refuse a mismatch with typed `SessionSuperseded`; `close_stream` is handle-addressed and fenced, `close_stream_id` is the explicit unfenced form; the error reaches the C ABI, both SDKs and both bindings, and is never retried | `a_displaced_sessions_handle_cannot_address_its_successor` (one identity in two processes, real displacement, successor's epoch walked to EXACTLY the stale handle's), `test_regression_equal_epochs_across_sessions_are_not_the_same_lifetime` | compare epoch only → the stale handle addresses the successor again | the initiator/routed refusal, the install CAS, 75 RTC tests |
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
