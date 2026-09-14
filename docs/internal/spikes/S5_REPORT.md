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
