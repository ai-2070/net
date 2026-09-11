# Browser-Native Net over WebRTC Plan

Make a browser tab a first-class Net node: its own entity identity, its own
Noise sessions, its own streams, channels, nRPC calls and fold participation —
with WebRTC DataChannels as the **primary** transport so browser ↔ browser and
browser ↔ native traffic never transits a server on the data path. Native
"anchor" nodes exist to bootstrap signalling, answer STUN, and forward for the
minority of pairs ICE cannot connect. They are control plane and fallback, not
the path.

> **Framing.** This plan inverts the [`NAT_TRAVERSAL_PLAN.md`](NAT_TRAVERSAL_PLAN.md)
> framing on purpose. There, a direct path is an *optimization* and the relay
> is the correctness guarantee. Here, for the products that motivate the work
> (in-browser three.js multiplayer, collaborative editing, synchronized scene
> state for audiovisual worlds), a direct path is a **product requirement** —
> a server hop on every position update is the thing we are refusing to ship.
> The relay stays as the correctness fallback exactly as before, but the plan
> is measured on the fraction of sessions that land direct, not on whether the
> relay works. Docstrings and READMEs written under this plan must carry that
> framing: "anchors are how browsers *find* each other, not how they *talk*
> to each other."

## Status

**Draft, revision 3 — not started.** First draft 2026-09-05 against
`net-mesh` 0.36.0 (`master` at `079894c76`); revised the same day after
Kyra's source-checked review; revised again 2026-09-11 after Fable's
review of revision 2 and Kyra's dispositions (see [Review log](#review-log)).

**Re-baselined 2026-09-11 against `master`
`132dbdcff251973e9eaf24e5c08eca7078d3b6f2`.** Every `path:line` and every
count in §Context, §1 and §Critical files was re-derived at that commit; the
numbers below are current, not carried over. What moved and why: the
organization exact-sensing / sensing-SDK lane (PR #943 `a2efc950a`, PR #949
`525b88ac0`) landed 62 commits on `mesh.rs` between the two heads and grew it
**43,114 → 52,943 lines**, so every `mesh.rs` citation shifted by 6–7k lines.
Nothing structural changed: `PeerTransport` has the same two variants and the
same 19 match sites, the five outer packet formats are unchanged, the seven
wire modules are byte-identical, and `str0m` is still at 0.23.1. Three of the
original counts were also wrong when written, and are corrected in place
(marked *corrected*). Line references will drift again — `mesh.rs` is the
repository's most-edited file.

**Authorization (Kyra, 2026-09-11): Stage 0 and its prerequisite evidence
ONLY.** The architecture is accepted — WebRTC primary, browser leaf,
dedicated RTC socket, preserved direct/routed ownership, Noise endpoint
identity, the explicit UDP-blocked limitation, real-browser spikes before the
broad refactor. Stages 1–6 are **not** implementation-ready and are not
authorized by this document. Neither a resolved question list nor a clean
exit gate is implementation proof.

**Revision 3 (2026-09-11) — Fable's source-checked review of revision 2,
dispositions by Kyra.** Four repairs were accepted as *pre-Stage-1
decisions*; none overturns the architecture and none widens the
authorization. Before Stage 1 may be authorized the plan must carry:

| # | Decision | Section |
|---|---|---|
| A | Send-path seam: UDP keeps its async/deadline/batching semantics; RTC uses bounded non-blocking admission; Stage 0 inventories the send and ingress paths and proposes the contract | §1, §2, Stage 0 (S0d), Stage 1 |
| B | Pre-enrollment privileges — **CLOSED 2026-09-11: enrollment-gated.** Provisional sessions, action-level bootstrap allow-list, no pre-enrollment transit, session-bound promotion, provider authority unchanged; the PSK stated precisely, not as "public" | §5 Layer 0, §12, Open question 7 |
| C | The routing-envelope codec is part of the portable wire inventory; S0a proves a routed round-trip | §7, Stage 0 (S0a) |
| D | `str0m` pinned with `default-features = false, features = ["rust-crypto"]`; S0b proves it on supported native targets in both roles | §2, §Dependencies, Stage 0 (S0b) |

Estimates are withdrawn, not doubled: Stage 0's outputs re-derive them
(§Rough estimates).

Six contract defects were found in draft 2 and are repaired in place; each is
marked at its section and listed in the [Review log](#review-log):

| # | Defect | Repaired in |
|---|---|---|
| 1 | The existing invite carries no PSK — "enrollment reuse" was a new credential format | §5 Layer 0, Stage 4 |
| 2 | Device enrollment is not organization membership; `0x0C04` is dropped by the leaf | §5, §7; v1 scoped to the public-capability path |
| 3 | Queue admission and later SCTP refusal are different outcomes | §2, §3, Stage 3 |
| 4 | Session replacement retains no live routed backup | §5 direct path, §9, Stage 3 |
| 5 | New announcement fields must enter `SignedPayloadCanonical`, not just the derived `Serialize` | Stage 4 |
| 6 | The serverless hook's startup sequence is incompatible with a routed prerequisite | Stage 5, §Follow-on |

Four acceptance tests were also mis-specified and are corrected: the MITM
witness (substitute the responder, don't mutate an ignored field), failure
typing (an ICE timeout is not proof of UDP blocking), the direct-path witness
(positive receipt plus a forced-relay inverse, not a flat counter alone), and
relay visibility (`0x0D02` is classifiable in the clear header; only the SDP
is confidential).

Decision already taken by the product owner: WebRTC is the primary browser
transport. WebSocket-to-anchor and WebTransport were considered and rejected
as primaries because both are client-to-server by construction and cannot
remove the server hop. Neither is part of this plan.

Decisions this revision fixes relative to draft 1: no shared-socket demux
(§6), first-contact key discovery precedes signalling (§5), UDP-blocked
networks are explicitly unsupported in v1 (§6), leaf non-forwarding is a
role not a TTL (§7), the endpoint refactor generalizes the existing
`PeerTransport` state rather than replacing it (§1), and spikes precede the
wide refactor (§Stage 0).

## Context

Verified facts about the substrate, re-derived at `132dbdcff`.

**The transport is UDP by type, not by abstraction.**

- `adapter/net/transport.rs` wraps a tokio `UdpSocket` behind `NetSocket`,
  `PacketSender`, `PacketReceiver` and the Linux `BatchedPacketReceiver`.
  There is no transport trait. `MeshNode::spawn_receive_loop`
  (`mesh.rs:24203`) funnels the per-packet and batched paths through a local
  `IngressReceiver` enum.
- Peer transport state **is already typed**. `PeerTransport`
  (`mesh.rs:2990–3005`) separates *where a packet goes* from *who owns that
  endpoint*:

  ```rust
  enum PeerTransport {
      Direct { owned_addr: SocketAddr },
      Routed { relay_addr: SocketAddr, adjacent_relay_identity: Option<u64> },
  }
  ```

  with `send_addr()` (`:3011`), `owned_addr()` (`:3022`) and `is_direct()`
  (`:3031`) accessors and 19 match sites. Installation, direct↔routed
  migration, teardown and stale-session
  protection all hang off it. This is the state to *generalize*, not replace.
- Around it, a peer endpoint is a `SocketAddr` at every keyed site: the
  identity binding map `addr_to_node: DashMap<SocketAddr, u64>`
  (`mesh.rs:1424`), `RouteEntry::next_hop` (`route.rs:416`),
  `NetSession::peer_addr` (`session.rs:67`), `NodeInfo::addr`
  (`swarm.rs:343`), `pending_direct_initiators`, the proxy forwarder, the
  failure detector and reroute. Occurrence counts, production code only:

  | File | `SocketAddr` mentions |
  |---|---|
  | `mesh.rs` | 224 |
  | `route.rs` | 70 |
  | `reroute.rs` | 44 |
  | `router.rs` | 33 |
  | `behavior/proximity.rs` | 29 |
  | `swarm.rs` / `session.rs` / `proxy.rs` / `failure.rs` | 20 / 17 / 16 / 14 |
  | `traversal/*` | 46 total *(corrected — the "~60" was never accurate)*; stays UDP-only, see §6 |

**Wire-level address inventory.** Three places serialize a `SocketAddr`:

1. The traversal subprotocols (`ReflexMsg`, `RendezvousMsg`) — UDP by nature.
2. `CapabilityAnnouncement::reflex_addr: Option<SocketAddr>`
   (`behavior/capability.rs:2319`), `#[serde(default,
   skip_serializing_if)]` so `None` keeps the pre-field signed byte form.
3. Nothing else. Routing headers carry node ids; `Pingwave` (`swarm.rs:33`)
   carries origin id, seq, ttl, hop count.

Browsers omit (2). No process-local RTC handle is ever serialized anywhere.

**Outer packet formats on the UDP socket.** The receive loop distinguishes
five shapes by their leading bytes, and one of them has *no* discriminator:

| Format | Constant | Leading bytes on the wire (LE) |
|---|---|---|
| Net header | `MAGIC = 0x4E45` (`protocol.rs:9`, `to_bytes` at `:378`) | `45 4E` |
| Routing envelope | `ROUTING_MAGIC = 0x5452` (`route.rs:28`) | `52 54` |
| Protected route-hop | `ROUTE_HOP_MAGIC = 0x5248` (`subnet/route_hop.rs:53`, dispatched `mesh.rs:24602`) | `48 52` |
| Punch keep-alive | `KEEPALIVE_MAGIC = 0x4850` (`traversal/rendezvous.rs:191`) | `50 48` |
| Headerless pingwave | none — 72-byte fixed size, recognised by length and *not* starting with `MAGIC` (`mesh.rs:24402–24410`) | arbitrary (origin id) |

The pingwave's leading bytes are an origin id and can take any value,
including the STUN (`0x00–0x03`) and DTLS (`0x14–0x3F`) ranges RFC 7983
demuxes on. Draft 1's one-byte shared-socket demux was therefore wrong twice
over (it also keyed on `0x4E`, which is the *second* byte); see §6.

**The wire layer is portable in principle, but not yet in Cargo terms.** The
modules a browser node needs have no socket coupling:

| Module | Lines | tokio calls | `Instant` calls | Intra-crate imports |
|---|---|---|---|---|
| `protocol.rs` | 955 | 0 | 0 | none |
| `crypto.rs` | 2092 | 0 | 0 | `protocol` |
| `pool.rs` | 1841 | 0 | 0 | `crypto`, `protocol` |
| `batch.rs` | 394 | 0 | 0 | `protocol` |
| `stream.rs` | 268 | 0 | 0 | none |
| `reliability.rs` | 2529 | 0 | 7 | `protocol` |
| `session.rs` | 3527 | 0 *(corrected — the two `tokio::` hits are prose in comments at `:458` and `:3100`, not call sites)* | 7 | `crate::event::StoredEvent` (`:16`), `subnet::route_hop::SharedHopReplayWindow` (`:19`), `pool`, `reliability`, `stream` |

But `Cargo.toml:239` pulls tokio unconditionally with `rt-multi-thread`,
`net` and `time`, and `lib.rs` exposes `bus`, `consumer`, `shard` and `ffi`
unconditionally (`bus.rs` alone has 97 tokio call sites). tokio refuses to
build `net` / `rt-multi-thread` on `wasm32-unknown-unknown`. A "wire feature"
on the core is therefore not a clock shim; it is a crate split (§7, Stage 2).

Crypto is Noise NKpsk0 via `snow` (default pure-Rust resolver), ChaCha20-
Poly1305 via `ring` 0.17, Ed25519 / X25519 via the dalek 3.x crates, BLAKE3,
`postcard`. `getrandom` 0.4 needs `wasm_js` on wasm. None of this needs
replacing on the native side. *(S0a, 2026-09-11, corrects the earlier
"`ring` builds for wasm32": it does only with a wasm32-targeting `clang`
on the build host — `ring`'s `build.rs` drives `cc` — so the packet AEAD
needs a per-target backend seam; see §7 and Stage 2.)*

**The rest of the mesh is not portable and must not be ported.** `tokio::time`
appears in 40 files under `adapter/net`, `Instant::now` in 90, `thread::spawn`
in 36 files (test modules included), and `mesh.rs` alone is now **53k lines**.
A browser node is a smaller node profile (§7), not the core compiled to wasm.

**Packet geometry fits DataChannels.** `MAX_PACKET_SIZE = 8192`
(`protocol.rs:33`). RFC 8831 §6.6 *recommends* senders stay at or below
16 KiB per message to avoid monopolising the SCTP association when the peer
lacks interleaving; it is a recommendation, not a guarantee, and browser
limits above it vary. 8 KiB sits comfortably inside every implementation's
floor, so Net's fragmentation, stream windows and NACK reliability need no
change.

**First contact already has a mesh-native shape.** `sdk/src/mesh_enroll.rs`
defines `Rendezvous { addr, noise_pubkey, node_id }` — the transport
coordinates a device needs to dial an operator it has never met — encoded
into an invite string alongside the ed25519 `root` that anchors delegation.
The device dials with `MeshNode::connect_via` (the routed handshake;
`mesh.rs:39609` requires `dest_pubkey`), then calls the enrollment nRPC
service. The Noise static key and the entity key are **different keypairs**;
`NoiseHandshake::initiator_with_prologue` (`crypto.rs:206`) needs the
responder's static key and the PSK before it can build msg1. Nothing in the
substrate distributes Noise static keys on the wire today: capability
announcements carry `node_id`, `entity_id`, capabilities, `reflex_addr` and a
signature, but no Noise key.

**Reachability learning already exists.** Forwarded capability announcements
install a route toward their origin through the sender
(`mesh.rs:33150–33188`: `ann.hop_count > 0` (`:33160`) →
`add_route_with_metric(ann.node_id, sender_addr, hop_count + 2)` (`:33185`),
or `add_authenticated_route_with_metric` (`:33178`) when the adjacency is
direct-confirmed), preserving authenticated next-hop identity when direct
adjacency is confirmed. A leaf does not need to
originate pingwaves to be reachable.

**Non-forwarding is not a TTL.** `subnet/gateway.rs:329–342` treats
`hop_ttl == 0` as expired and drops the packet; routing-envelope TTL
(`route.rs:132`) bounds forwarding of the *emitter's* packet. Neither says
anything about whether the emitter forwards for others.

**The NAT-traversal module already is the mesh-native STUN/TURN for native
peers.** Reflex probing (`0x0D00`), rendezvous punching (`0x0D01`), the
`NatClass` / `PairAction` matrix and relay fallback exist and are measured.
For native ↔ native pairs WebRTC adds nothing; its value is browser reach.

**Nothing browser-facing exists today.** `sdk-ts` wraps the `@net-mesh/core`
napi binding (Node ≥ 20). `web/` is a Next.js site. Every `wasm-bindgen` /
`web-sys` entry in `Cargo.lock` is transitive.

**Precedent to lean on.**

- `docs/SUBPROTOCOLS.md` — `0x0D02` is the next free id in the traversal
  block.
- `tests/cross_lang_*` — golden-vector discipline for wire types shared
  across language tiers. The leaf joins that matrix.
- The `reflex_addr` optional-field pattern on announcements — the exact
  wire-compat recipe for the two fields §5 and §6 add.
- 16 `cfg(target_os = …)` sites and 123 platform `cfg(…)` sites overall
  (`target_os` / `unix` / `windows` / `target_family`) under `adapter/net`
  *(corrected — the original "123 `cfg(target_os …)`" conflated the two)*.
  Platform gating is routine here.
- The MCP adapter doctrine ("adapters attach, nodes participate",
  `adapters/mcp/src/lib.rs`). A browser under this plan *participates*.

## Goals

- A browser tab runs a Net node with a stable, self-held identity
  (`EntityKeypair` + Noise static key) and its own node id, sessions, streams,
  channel subscriptions, nRPC client and signed capability announcements.
- Browser ↔ browser and browser ↔ native sessions ride a WebRTC DataChannel
  directly whenever ICE can connect the pair. **Conformance:** every
  NAT-matrix row ICE is expected to solve lands direct in the deterministic
  harness. **Deployment:** the field direct ratio is reported as telemetry
  with its own denominator; no fixed percentage is a gate.
- No third-party STUN, TURN or signalling infrastructure. Anchors answer STUN
  on their RTC socket; the mesh is the signalling network after the first
  session; the mesh relay is the fallback for pairs whose anchors are
  reachable.
- Wire parity: a browser node speaks the exact Net wire — 68-byte header,
  Noise NKpsk0, ChaCha20-Poly1305, the same subprotocol ids — so native peers
  cannot tell a browser session from a UDP one above the transport layer.
- Compatibility guarantee for the native `webrtc` feature: **off by default;
  the default build's exported C-ABI symbol set and observable behaviour are
  unchanged.** (Not byte-identical binaries — the endpoint refactor changes
  code layout under default features and that is fine.)
- Node, Python and Go bindings gain the anchor role behind the same feature
  flag; a new `@net-mesh/browser` package ships the wasm leaf.

## Non-goals

- **Porting `MeshNode` to wasm.** The browser node is a leaf profile (§7).
- **WebTransport, WebSocket data paths.** Both are client-to-server. A
  WebSocket appears in exactly one place — the anchor bootstrap endpoint
  (§5) — and carries at most the first SDP exchange, never a Net packet.
- **Media.** No SRTP, no audio/video tracks. DataChannels only. Audiovisual
  worlds use this plan for scene state and model control; tracks are a
  separate, explicit integration.
- **A TURN protocol server, or any rescue for UDP-blocked browsers in v1.**
  See §6 — this is a declared limitation with a prompt typed failure, not a
  gap to be discovered in production.
- **Native ↔ native WebRTC.** UDP + the existing punch remains the native
  path.
- **Dropping Noise in favour of DTLS.** See §4.
- **Mobile native (iOS / Android) clients.** Native nodes with UDP.
- **Anchorless ("serverless-only") deployments.** Serverless runtimes
  (Cloudflare Workers, Vercel / Netlify functions, Deno Deploy, Lambda)
  expose no listening UDP socket and cannot hold an ICE agent, a DTLS
  session or a routed relay session alive between invocations, so none of
  them can host bootstrap, STUN, announcement flooding or the relay
  fallback. A browser cannot cover for that either — a tab has no raw UDP,
  so it can neither answer STUN nor serve a bootstrap URL. v1 therefore
  requires at least one always-on native anchor. The intended product
  answer is a **packaged anchor** (Stage 7): one small container or VM with
  one UDP port, off the data path, serving many browsers. A third-party
  STUN escape hatch is *not* offered in v1 (see §6, "Serverless"). The
  serverless substitute for each anchor function is sketched under
  §Follow-on.
- **Browser-side persistence beyond identity.** RedEX / Dataforts on
  IndexedDB is a separate plan.
- **A collaborative-text CRDT.** Net carries and persists updates; the CRDT
  is the application's.

---

## Design decisions

### 1. Generalize `PeerTransport` endpoints to `PeerAddr`; keep its ownership semantics intact

```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum PeerAddr {
    /// A UDP tuple — the only variant in default builds.
    Udp(SocketAddr),
    /// A DataChannel owned by the local `RtcDriver`. Process-local, never
    /// on the wire. `generation` is bumped on every (re)open so a callback
    /// captured against a closed channel can never address its successor.
    #[cfg(feature = "webrtc")]
    Rtc(RtcPeerId /* { slot: u32, generation: u32 } */),
}

enum PeerTransport {
    Direct { owned: PeerAddr },
    Routed { relay: PeerAddr, adjacent_relay_identity: Option<u64> },
}
```

`send_addr()` / `owned_addr()` / `is_direct()` keep their meaning over
`PeerAddr`. `addr_to_node`, `RouteEntry::next_hop`, `NetSession::peer_addr`,
`NodeInfo::addr`, `pending_direct_initiators`, the proxy forwarder and the
failure detector key on `PeerAddr`. `MeshNodeConfig::bind_addr` /
`peer_addr` stay `SocketAddr` — they describe the UDP socket.

**What must survive unchanged:** the direct/routed installation rules, the
routed→direct migration path, teardown ordering, the stale-session and
address-reuse protections the routing-plane witnesses in `AGENTS.md`
exercise, and the invariant that the withdrawing hop is the resolved sender.
A non-reused `RtcPeerId` makes the *tuple-reuse* race impossible on the RTC
path; it does **not** make stale callbacks or wrong direct/routed ownership
impossible, so every protection stays and is re-exercised with an RTC
endpoint in Stage 3's harness.

**The floors this refactor must clear, as of `132dbdcff`.** The routing-plane
witnesses are named CI gates with *minimum counts*, and they grew with the
organization exact-sensing lane. `ci.yml` currently asserts
`org_routing_wiring_tests >= 93` (`ci.yml:175`; `AGENTS.md`'s "currently 86"
is stale), `behavior::org_routing:: >= 24` (`:285`),
`behavior::org_routing_registry:: >= 62` / routing-state `>= 41`
(`:345–346`), and the org gate/mesh floors `60` / `67` (`:563–564`). Those
suites cover exactly the peer-keyed sites Stage 1 rewrites, so re-exercising
them on `PeerAddr` — not the mechanical signature edit — is where Stage 1's
cost actually sits. Re-read the floors before starting; they move.

**Test policy for the refactor.** Changing a Rust parameter from
`SocketAddr` to `PeerAddr` legitimately requires mechanical test edits. The
gate is that every existing assertion and its coverage is preserved; no
witness may be weakened or deleted, and diffs to witness tests are reviewed
line by line.

**Alternative considered:** synthesising fake loopback tuples for RTC peers.
Rejected — it lies to every `is_loopback` / partition / reflex check and
would leak into `reflex_addr`.

**The send path is part of the refactor, and its contract is fixed now
(Fable / Kyra, 2026-09-11).** Peer-keyed *state* is not the whole
surface: every outbound packet leaves through a raw
`socket.send_to(packet, addr).await` — ~30 sites in `mesh.rs` (`:515`,
`:4827`, `:5070`, `:6797`/`:6806` under `bound_datagram_send` with
`DATAGRAM_SEND_DEADLINE`, `:7815`, `:9317`, `:25479`, `:25741`, `:25792`,
`:25906`, `:26399`, `:27187`, `:27287`, `:27554`/`:27556`, `:28397`/`:28425`,
`:33536`, `:34104`, `:34860`, `:35503`, `:37543`, `:39387`, `:39566`,
`:40321`, `:40377`), five in `mod.rs`, `proxy.rs:357`, and the scheduler
drain in `router.rs` (`QueuedPacket::dest: SocketAddr` at `:198`, grouped
by destination for `sendmmsg` at `:945–1020`). None of them compiles once
the destination is a `PeerAddr`, so Stage 1 necessarily introduces the
send seam. Its contract is **transport-specific submission behind the
generalized endpoint** — option B, chosen over forcing UDP through the RTC
admission contract:

- UDP keeps its current async behaviour, the applicable deadlines, its
  error mapping (`deliver_stream_packet`'s unscheduled arm maps failure to
  `StreamError::Transport`, `mesh.rs:39386–39391`, never `Backpressure`)
  and its batching. A UDP `WouldBlock` is **not** converted into
  application backpressure to make one trait look uniform.
- RTC uses bounded, non-blocking queue admission (§2).
- The scheduler drain distinguishes UDP batches from RTC submissions.

Sequencing: Stage 0 (S0d) produces the send-path and ingress-path
inventory and the proposed contract; Stage 1 implements only the
UDP-preserving preparation; Stage 3 adds the RTC behaviour. That keeps the
staging boundary real.

### 2. `str0m` behind a single owning driver task with a bounded, non-blocking send contract

**Crate.** `str0m` 0.23.1 (checked 2026-09-05: MSRV 1.85.0 < toolchain
1.98.0; MIT OR Apache-2.0; sans-IO, tokio-free). Feature
`webrtc = ["nat-traversal", "dep:str0m"]`, off by default;
`MeshNodeConfig::rtc: Option<RtcConfig>` enables at runtime.

**Why not `webrtc-rs`.** Tokio-native (own tasks and locks per connection),
large dependency tree, an order of magnitude beyond what `Cargo.toml` already
refuses by default (`regex`, `igd-next`).

**Ownership.** str0m's contract is *mutate, then drain outputs*. One
`RtcDriver` task per node owns the RTC UDP socket (§6) and every `Rtc`
instance, single-threaded, no shared locks:

- **Inbound:** datagrams from the RTC socket → STUN-binding responder
  (unsolicited, no ICE credentials) or `Rtc::handle_input`; DataChannel
  payloads that come out are Net packets tagged `PeerAddr::Rtc(id)`.
  **One dispatch owner is preserved** (Kyra, 2026-09-11): `dispatch_packet`
  (`mesh.rs:24321`) is called only from the single receive loop today, and
  that stays true — RTC ingress is fed into a bounded input of the
  `IngressReceiver` orchestration (`mesh.rs:24218`), never dispatched
  concurrently from the driver. The UDP socket and its framing are
  unchanged; the ingress *orchestration* is not, and the earlier claim that
  the seam is "left untouched" is withdrawn.
- **Outbound — admission is the ONLY refusal boundary.** For an RTC
  endpoint the send seam (§1) submits via `try_send(packet, RtcPeerId)`, a
  **non-blocking** push onto a bounded per-peer queue
  (`RtcConfig::send_queue_packets`, default 256, with reserved bytes/slots
  as the hard bound — see below). The admission decision is synchronous
  and total: the packet is either
  refused *now* with `WouldBlock` — which the stream layer maps to
  `StreamError::Backpressure`, meaning **no packets were enqueued**
  (`stream.rs:168–170`) — or accepted, after which the call has returned and
  the caller has already committed credit, sequence and retransmission state
  (`mesh.rs:39273–39292`: `guard.commit()` then `register_retransmit`).

  **Nothing after acceptance may retroactively become whole-call
  backpressure** (Kyra, 2026-09-11). Draft 2's §2 promised exactly that: the
  buffered-amount check ran *after* `try_send` returned, and the trait had no
  way to hand that refusal back to a completed call. Any later refusal —
  including `str0m`'s `Channel::write` returning `Ok(false)` **after** a
  passing buffered-amount precheck — is a post-acceptance event and is handled
  by post-acceptance policy, never by rewriting the caller's result. Partial-
  send accounting (the rollback/`try_rollback_tx_seq` path at `mesh.rs:39284+`)
  is preserved exactly: it belongs to the synchronous boundary.
- **SCTP buffering is folded into admission, not bolted on after it.** The
  driver-published buffered-amount reading against
  `RtcConfig::buffered_amount_advisory` (default 256 KiB) is an **input to
  the admission decision** — consulted before `try_send` returns — so an
  over-buffered channel refuses at the same boundary a full queue does. It
  is advisory (see "Hard bound vs advisory reading" below); the hard bound
  is `send_queue_packets` / `send_queue_bytes`. After acceptance the driver owns the packet
  under a bounded retention policy that Stage 3 must specify explicitly:
  how long a queued packet is retained, whether it is retried or dropped on a
  later `Ok(false)`, and what terminal channel close does to the remainder.
  Loss still happens at a named point, never as unbounded SCTP growth — but
  the named point is now honest about *which side of the call* it is on.
- **Credit and reliability are independent** *(corrected — draft 2 said
  "fire-and-forget streams have no credit at all", which is wrong)*. An
  explicitly fire-and-forget stream can carry a receiver window. Stage 3
  therefore maps pressure behaviour for **every** outbound class, not just
  `Stream::send`: ordinary events, `0x0D02` signalling, forwarded packets on
  behalf of other peers, and retransmissions. None of those return
  `StreamError`, so each needs its own stated disposition (refuse at
  admission / drop with a counter / defer), and Stage 3's exit criteria assert
  them.
- **Timers:** the driver runs `Rtc::poll_output` to completion after every
  input and arms one timer per instance from the returned `Timeout`.
- **Close / stale work:** closing a channel bumps its `generation`; queued
  packets and timers carrying an old generation are discarded, and the mesh
  is told via the existing peer-removal path so `addr_to_node` and
  `PeerTransport` are torn down in the normal order.

The shape below is the **RTC submission contract**, not a uniform transport
trait: UDP keeps its own async submission behind the same `PeerAddr`
boundary (§1). It is fixed only after Stage 0's spike exercises this loop:

```rust
/// RTC half of the send seam. `WouldBlock` == `Backpressure`.
fn try_send(&self, packet: &[u8], to: RtcPeerId) -> io::Result<()>;
```

**Hard bound vs advisory reading** (Kyra, 2026-09-11).
`Channel::buffered_amount` takes `&mut self` on a handle obtained from
`&mut Rtc`, so the driver is the only reader; whatever it publishes to the
admission side is a snapshot that is stale by up to one drain cycle. A
snapshot is **advisory** — it cannot enforce a hard bound. The hard bound
is provided by **reserved queue bytes/slots** accounted at admission; the
driver then handles actual SCTP acceptance, including `Ok(false)` after a
passing advisory precheck, under the post-acceptance policy above. How the
snapshot is factored (atomic, message, otherwise) is Stage 3's choice, not
prescribed here.

**str0m's single-mutation invariant** (README, "The single-mutation
invariant"): every mutation of an `Rtc` — `handle_input`, `Channel::write`,
SDP/candidate changes — must be followed by a complete `poll_output` drain
to `Output::Timeout` before the next mutation. The driver therefore pops one
queued packet per `Channel::write` + drain; the per-peer queue is the only
cross-task structure and is consumed only by the driver.

### 3. One Net packet per DataChannel message; unordered, zero-retransmit; Net's reliability stays authoritative

Channels open with `ordered: false, maxRetransmits: 0` on both sides.

- `Reliability::Reliable` streams get NACK-driven retransmission from
  `reliability.rs`, identical to UDP. No retransmission stacking, no
  SCTP head-of-line blocking *from retransmits*.
- `Reliability::FireAndForget` streams are genuinely lossy in the
  *retransmission* sense only — dropping a packet is not an error. That is
  independent of flow control: such a stream may still carry a receiver
  window, so "fire-and-forget" never implies "uncredited" (§2).
- What remains underneath and is **not** removed: SCTP congestion control
  and send buffering. §2's buffered-amount bound — consulted *at admission* —
  is how the plan keeps those from becoming invisible latency.
- Stream-window backpressure (`0x0B00`) works unchanged. 8192-byte packet
  cap stays; no fragmentation changes.

One DataChannel per peer pair; stream multiplexing is Net's job.

### 4. Noise stays on top of DTLS

A DataChannel is DTLS-encrypted; Noise NKpsk0 + ChaCha20-Poly1305 stay on
top because:

- **Identity.** The mesh identity is the Noise static key bound to the
  `EntityKeypair`, not a DTLS fingerprint. Every gate above the transport
  assumes it.
- **PSK.** NKpsk0's pre-shared key is the admission secret; DTLS has no slot.
- **Relay blindness.** DTLS terminates *at* the anchor; Noise at the peer.
- **Wire parity.** One `PacketCipher` path, one test matrix.

The DTLS-exporter shortcut is **deferred** pending Stage 5's measurement.

### 5. First contact: authenticated key discovery, then a routed end-to-end session, then opaque signalling over it

Draft 1 had the order backwards — it carried the responder's Noise key in an
answer that could only be sent over the session that key was needed to
build. The corrected sequence has three layers, each authenticated by the
one below it.

**Layer 0 — bootstrap to an anchor (browser's first session).** The
application hands the browser a **browser bootstrap credential**. This is a
**new artefact**, and calling it "the existing invite, reused" was wrong
(Kyra, 2026-09-11): `InviteToken` carries `root`, `rendezvous`, `nonce` and
`expires_at` and **no PSK** (`sdk/src/enrollment.rs:216–228`), and
`Rendezvous`'s own doc states the mesh PSK "is an out-of-band build-time
property of both nodes, not carried here"
(`sdk/src/mesh_enroll.rs:47–54`). A browser has no build-time provisioning
step, so the secret has to come from somewhere named. The credential is
therefore defined as:

```text
browser bootstrap credential =
    InviteToken           (root, rendezvous, nonce, expires_at — unchanged)
  + anchor Noise static pubkey   (X25519; the key Layer 0 pins)
  + mesh PSK                     (the NKpsk0 admission secret)
  + anchor bootstrap URL
```

Its encoding, minting CLI and expiry rules are a **Stage 4 deliverable with
its own review**, not an inherited format. The PSK is a standing transport
secret while the invite `nonce` is single-use, so the two halves have
different lifetimes and the credential must state both.

**The PSK, stated precisely** (Fable / Kyra, 2026-09-11). `psk: [u8; 32]`
is "PSK shared across the mesh" (`mesh.rs:1570`). A secret shipped to
browser JavaScript is available to every recipient of that credential; it
becomes *public* exactly when credentials are handed out publicly. A
browser does not by itself imply anonymous provisioning. Two rules follow:

- **Never** distribute an existing private/native deployment's mesh PSK to
  arbitrary public application visitors.
- A public-browser deployment is a **deliberately separate transport trust
  domain** with its own PSK.

A separate PSK domain does **not** solve enforcement, because enrollment
(the `JoinRequest` nRPC call) runs *after* the Noise session and nothing on
the responder path today ties post-handshake behaviour to it: a
transport-authenticated peer can announce capabilities, open streams, obtain
routed forwarding via `connect_via` and emit `0x0D02` before enrolling.
**Decided (Kyra, 2026-09-11): the v1 browser-facing anchor is
enrollment-gated.** A successful handshake yields a *provisional* session
that may exercise only the bounded bootstrap exchange; everything else is
denied before effects until enrollment promotes that exact session. The
contract is §12; Open question 7 records the closure. Implementing it is a
new browser admission contract, landed *after* Stage 1 — never inside the
behaviour-neutral UDP refactor.

With that in hand: for a browser `rendezvous` is the anchor's bootstrap URL.
The browser POSTs an SDP offer to `https://<anchor>/rtc/offer`, trickles
candidates over a short-lived WebSocket on the same origin, gets an answer,
and opens a DataChannel. Over that channel it runs the Noise initiator
against the anchor static key **from the credential, not from the HTTP
response**. It then calls the existing enrollment nRPC service with a signed
`JoinRequest` and verifies the grant, exactly as a native device does.

**What enrollment gets you, and what it does not.** Enrollment yields a
`DelegationChain::derive_device` (`sdk/src/enrollment.rs:836–842`) — device
admission to the mesh. It is **not** organization membership. Protected
organization invocation consumes an `OrgMembershipCert`, an
`OrgDispatcherGrant` and an exact call-binding signature
(`OrgCallProof`, `behavior/org_call.rs:175–189`), and owner-private services
are announced only on the encrypted `0x0C04`
(`SUBPROTOCOL_SCOPED_CAPABILITY_ANN`, `behavior/broadcast.rs:35–44`) — which
the §7 leaf drops as an unknown subprotocol. **v1 browser scope is therefore
the PUBLIC capability path**: plaintext `0x0C00` discovery, ordinary nRPC,
no org-protected calls. Extending a leaf to organization capabilities is a
separate slice that must provision org credentials, decrypt scoped
discovery, and construct call proofs — and its gate is one browser
discovery → protected-invocation witness plus a refusal witness with no
dispatcher authority. No new authority scheme is needed or permitted; the
existing one is simply not free.

**Layer 1 — learning a peer's Noise key.** `CapabilityAnnouncement` gains
`noise_pubkey: Option<[u8; 32]>` with the `reflex_addr` wire-compat
treatment (`#[serde(default, skip_serializing_if)]`; `None` keeps the
pre-field signed bytes). The field enters the signed transcript, so it is
authenticated by the announcing entity's Ed25519 key and bound to its
`node_id` / `entity_id` by the existing announcement verifier. Native nodes
may emit it too (it retires the out-of-band pubkey handoff `connect()`
demands today) but emission is off until the fleet is upgraded, per the
OA-1 migration pattern. A browser B's announcement floods through its
anchors; browser A receives it and now holds `(B.node_id, B.noise_pubkey)`
authenticated.

**Layer 2 — routed end-to-end session.** A calls
`connect_via(anchor, B.noise_pubkey, B.node_id)`: the existing routed Noise
handshake through anchors, no wire change. Result: an A↔B session whose
keys no anchor holds, with `PeerTransport::Routed { relay: anchor }`.

**Layer 3 — signalling over that session.** `SUBPROTOCOL_RTC_SIGNAL =
0x0D02`, postcard, session-authenticated:

```rust
pub enum RtcSignalMsg {
    Offer     { dialog: u64, sdp: String },
    Answer    { dialog: u64, sdp: String },
    Candidate { dialog: u64, candidate: String, mid: String },
    Reject    { dialog: u64, reason: RtcRejectReason },
}
```

Origin and target are the session endpoints, never wire fields. SDP is
opaque. Anchors forward these as ordinary routed packets and cannot read
them. No key material rides here — it is already established.

**Direct path — replacement, not a dual session.** When ICE connects, the
direct DataChannel gets its own Noise handshake using the already-known key,
mirroring the punched-path upgrade
(`connect_direct → connect_on_direct_path → connect_via` today).

Draft 2 said "the routed session is retained as the fallback until the direct
one is healthy." **That is not what the substrate does** (Kyra,
2026-09-11). The installer replaces the *sole* peer session and removes the
displaced session's reverse index (`mesh.rs:22050–22103`); the existing
upgrade protects the incumbent only by *deferring* while streams or unacked
traffic remain (`mesh.rs:39886–39897`). So the incumbent survives a **failed**
upgrade — it does not survive a **successful** one. There are never two
simultaneously usable sessions today.

**The behaviour required of v1, settled now** (the mechanism is still Stage
3's to choose, per open question 4): direct-path loss must produce either
(a) explicit interruption followed by routed reconnection, or (b) continuity
through a migration mechanism designed and reviewed as such. Silent
half-working fallback is excluded. The default this plan assumes is (a) —
fresh-session replacement with quiescence, then routed reconnection on
direct loss — because it is what the installer already implements.

**Required delivery witness** (Stage 3, harness; Stage 6, browsers). Not ICE
state — actual traffic, in this order:

```text
routed delivery
→ authenticated direct delivery
→ forced direct failure
→ restored routed delivery
```

ICE reaching `connected` is not the health gate and must not be asserted as
one.

**Bootstrap endpoint budget.** Per-source-IP rate limit with the shape
Finding 5 in [`NAT_TRAVERSAL_V2_PLAN.md`](NAT_TRAVERSAL_V2_PLAN.md)
prescribes; rejections are typed and fast.

Anchors advertise `rtc-anchor` plus `rtc_bootstrap: Option<String>` (URL)
and `rtc_addr: Option<SocketAddr>` (their public RTC/STUN socket, UDP-only,
omitted by browsers) on the announcement, same wire-compat pattern.

### 6. A dedicated RTC UDP socket, STUN served there, no shared-socket demux; UDP-blocked browsers are unsupported in v1

**No demux.** The Net socket already carries five outer formats, one of
which (the headerless pingwave) has arbitrary leading bytes. A first-byte
demux against STUN/DTLS would misroute some pingwaves and, if done ahead of
the existing dispatch, break native ingress whenever `webrtc` is on. Giving
pingwaves a magic is a wire change with no other justification. (Draft 1
also mis-cited RFC 7983: first-byte values 64–79 are TURN ChannelData, not
unassigned.)

**Decision:** the `RtcDriver` owns a **second UDP socket**
(`RtcConfig::bind_addr`, default `bind_addr.ip():0`; operators pin a port to
publish it). All ICE, DTLS and STUN traffic lives there. The Net socket, its
receive loop, the batched-ingress path and every existing outer format are
untouched. Costs: one more port to expose per anchor; anchors behind NAT need
that port mapped or the address supplied via `RtcConfig::public_addr`
(analogue of `reflex_override`). Stage 4's exit requires an anchor to
publish a working `rtc_addr`.

**STUN.** A ~200-line RFC 5389 *binding request/response only* responder
answers unsolicited requests on the RTC socket; str0m handles the
credentialed ICE checks itself. Browsers get the anchor's `rtc_addr` as
their `iceServers` entry from the invite (bootstrap) or, for other anchors,
from announcements.

**Fallback and its limit.** When ICE fails for a pair whose *anchors are
reachable*, the routed session from §5 Layer 2 is simply kept: the anchor
forwards Noise-opaque packets, `TraversalStats.relay_fallbacks` and
`RtcStats.ice_relayed` increment, and the pair-type matrix returns
`PairAction::Ice` for any pair with a `transport:rtc` side so the punch
logic never runs.

This is **not** TURN. A browser on a UDP-blocked network cannot reach its
anchor either, because that DataChannel is also ICE-over-UDP. v1 therefore
declares such networks **unsupported**: bootstrap fails within
`RtcConfig::ice_deadline` (default 10 s) with a typed error that
`@net-mesh/browser` surfaces to the application. **That error is
`RtcError::IceTimeout`, not `UdpBlocked`, unless the narrower cause is
actually established** (Kyra, 2026-09-11): an unreachable or misconfigured
anchor produces an identical timeout, and reporting "your network blocks
UDP" for a broken anchor sends the operator to the wrong place. `UdpBlocked`
requires distinguishing evidence — e.g. STUN reachable but every candidate
pair failing, versus nothing at all reachable. If v1 cannot produce that
evidence, it reports the timeout and says so.

ICE-TCP passive candidates on anchors are the candidate future mechanism
(str0m support to be verified in Stage 0) and are listed under deferred
work, not promised.

"100 % of sessions established" is a Stage 6 exit criterion **only over
pairs whose anchors are reachable**.

**Serverless is not UDP-blocked; it is anchorless.** Hosting the page on a
static host or edge CDN says nothing about the browser's network path — a
tab on an ordinary connection has UDP regardless of where it loaded from.
What serverless hosting removes is any place to *run the anchor*. That is a
deployment constraint, not a connectivity class, and it is handled by the
packaged anchor (Stage 7), not by this section's fallback logic. The
tempting shortcut — a serverless HTTPS signalling relay plus a public
third-party STUN server — would reintroduce the external dependency the
goals exclude and would still leave no relay fallback and no announcement
flooding, so it is not offered.

### 7. The browser node is a leaf profile: a `net-wire` crate plus a `net-leaf` crate

**`net-wire` crate** (`crates/net/wire/`). Extract `protocol`, `crypto`,
`pool`, `batch`, `stream`, `reliability`, `session`, the wire-level
subprotocol codecs **and the routing-envelope codec** into a tokio-free
crate; the core depends on it and re-exports under the existing paths so no
`use net::adapter::net::…` site changes. The routing envelope was missing
from the seven-module list (Fable, 2026-09-11): every routed-session packet
is wrapped in a `RoutingHeader` (`send_routed`, `mesh.rs:28369–28427`;
`connect_via`, `:39565`), and Layer 2 is the leaf's only pre-direct path,
so a non-forwarding leaf still originates and receives routed packets.
`RoutingHeader`, its flags/constants and `to_bytes` / `from_bytes` /
`write_to` (`route.rs:182`, `:200`, `:215`, `:264`) move; the route *table*
(`RouteEntry`, metrics, `next_hop`) does not.

**What S0a established (`docs/internal/spikes/S0A_WIRE_BOUNDARY.md`,
commit `4a95691d4`):**

- The two `session.rs` couplings: `crate::event::StoredEvent` → **move the
  type** (session only queues it); `subnet::route_hop::SharedHopReplayWindow`
  → **move all of `subnet/route_hop.rs`** — `NetSession` also calls
  `seal`/`seal_into`/`open`/`sealed_len` and names `OpenedHop` /
  `RouteHopError`, so "move the window type" was insufficient. Adds
  `blake2` + `subtle` to `net-wire`.
- Also pulled in: `ParsedPacket` (split out of `transport.rs`),
  `current_timestamp` / `coarse_clock_advance` (out of `mod.rs`), and
  `tracing` as a real dependency (seven log sites survive, one a tripwire).
- The packet AEAD gets a **backend seam**: `ring` natively (byte-identical),
  the pure-Rust `chacha20poly1305` on wasm32 — same RFC 8439 bytes. Stage 2
  adds a cross-backend golden vector to `cross_lang_wire`.
- `snow` on wasm32 needs `default-features = false, features =
  ["default-resolver", "default-resolver-crypto"]` (its `std` feature
  force-activates `ring`); `getrandom` 0.2 **and** 0.3 both need their
  browser opt-in in addition to 0.4's.
- `Instant` / `Clock`: 13 sites across `reliability.rs`, `session.rs` and
  the coarse clock. On wasm32 `std::time::Instant::now()` **compiles and
  panics at runtime**, so a `cargo check` job alone cannot guard the seam —
  Stage 2 needs an executed wasm test.
- `NetSession::peer_addr` and `ParsedPacket::source` are `SocketAddr`:
  compiles on wasm32 but a leaf has no peer socket address, so `net-wire`
  cannot reach its final shape until Stage 1's `PeerAddr` exists (see
  §Rough estimates, sequencing).
- Every `#[cfg(test)]` module was left behind, including
  `session.rs`'s `heartbeat_api_drift_check`, which greps `mesh.rs` /
  `mod.rs` source text and cannot cross the crate boundary as-is. Stage 2
  relocates each test module explicitly; losing the drift check silently
  would remove the #97/#106 guard.
- `adapter/net/subprotocol/*` codecs are **not** a compile dependency of
  the wire modules; their move is driven by the leaf dispatcher's needs,
  not by S0a.
- Wasm size, `opt-level="z"` + lto + strip: 576 KiB raw / 161 KiB gzipped,
  of which the copied Net wire code is ~21 KiB and the crypto ~93 KiB;
  ~260 KiB is inert `wasm-bindgen` describe metadata pulled in by
  `getrandom`'s browser backend. `net-leaf` pays that anyway; a
  `net-wire`-only check job should not.

The `Instant` uses go behind a `Clock` trait (native `std::time::Instant`;
wasm `web_time::Instant`). CI adds
`cargo check -p net-wire --target wasm32-unknown-unknown` **and** an
executed wasm test.

**`net-leaf` crate** (`crates/net/leaf/`, wasm32, `wasm-bindgen`):

- `RtcLeafTransport` over `web_sys::RtcPeerConnection` / `RtcDataChannel`,
  one channel per peer; same §2 bounded-send and buffered-amount rules.
- Dispatcher for: routing envelopes addressed to itself (`ROUTING_MAGIC`,
  unwrapped; anything else dropped per the non-forwarding role below),
  plain events, channel membership (`0x0A00`), stream window (`0x0B00`),
  capability announcement (`0x0C00`), fold (`0x1000`), nRPC wire types, RTC
  signal (`0x0D02`). Unknown subprotocols dropped with a counter.
- Session table keyed by node id; Noise handshakes; per-stream reliability;
  identity storage (§8).

**Non-forwarding is a role, not a TTL.** A leaf:

- never originates pingwaves and never re-floods announcements (so no node
  ever learns "X via leaf" — the route-learning path installs routes toward
  an *origin* through the *sender*, and a leaf is only ever an origin);
- drops any routing-envelope or route-hop packet whose destination is not
  itself;
- tags its announcement `leaf` and `transport:rtc`, omits `reflex_addr`;
- sets **normal** TTLs on what it originates — a browser's packet may
  legitimately cross several native hops to reach a far peer, and
  `hop_ttl == 0` is *expired* at every gateway.

**Reachability** uses the existing capability-announcement route-learning
path: the leaf sends its signed announcement to each anchor it holds a
session with; anchors flood it; receivers install `route(leaf) = via sender`
with metric `hop_count + 2`. Nothing new.

**Alternative considered:** a TypeScript reimplementation of the wire layer.
Rejected — two Noise/ChaCha implementations to keep byte-identical forever;
the crate already refuses that for the transfer wire types.

### 8. Browser identity: self-held, encrypted at rest, same-origin trust boundary, one node per origin

On first run the leaf generates an `EntityKeypair` (Ed25519) and a Noise
`StaticKeypair` (X25519) in wasm via `getrandom`/`wasm_js`, and stores them
in IndexedDB encrypted under a non-extractable WebCrypto AES-GCM key.

**What that protects, stated exactly:** the scalars are not readable from
IndexedDB in the clear, and the wrapping key cannot be exported. It does
**not** make the scalars non-extractable: WebCrypto decryption returns
plaintext to the calling context, and wasm is not a security boundary
against same-origin JavaScript. The trust boundary is the origin; XSS on the
origin owns the identity. A host application that needs stronger custody
injects a keypair (custodial model, same API as
`MeshNodeConfig::entity_keypair`) or accepts short-lived identities under
the org revocation floors (open question 5).

**Tabs.** One identity per origin means several tabs would otherwise present
the same node id from independent sessions and evict each other under the
identity-rebind rules. Decision: **one node per origin, leader-elected**.
Tabs contend for a Web Lock; the holder runs the node, others attach to it
over `BroadcastChannel` / `MessagePort` and see the same API. On leader
loss a new leader re-bootstraps with the same identity — a rebind, handled
by the existing address-independent identity-binding path. (Running the
node in a `SharedWorker` would be cleaner, but `RTCPeerConnection` is not
available in worker contexts today; Stage 0 verifies current browser
status.)

**Leader lifecycle, to be specified in Stage 5** (Kyra, 2026-09-11). The
dead leader owned every DataChannel; peers learn of it by ICE disconnect
(browser defaults on the order of 5–30 s) or Net failure detection. The
specification must cover: the interruption budget; disposition of pending
nRPC calls and in-flight stream sends; restoration of streams and channel
subscriptions by the new leader; and stale-leader fencing so a suspended
tab that resumes cannot present the identity alongside its successor. The
follower-tab proxy (streams, nRPC futures, events over
`BroadcastChannel` / `MessagePort`) is a real SDK surface, not glue. Tests
must cover tab suspension/resumption as well as closing the leader.

### 9. Browser ↔ browser is the §5 sequence with the mesh as the signalling network

A wants B, discovered by capability query like any native pair:

1. A already holds B's `(node_id, noise_pubkey)` from B's signed
   announcement (§5 Layer 1).
2. A runs `connect_via(anchor, …)` → routed A↔B session (Layer 2).
3. A sends `Offer` on `0x0D02` over that session; `Answer` and `Candidate`s
   flow back the same way; anchors forward blind.
4. ICE connects using the anchors' STUN; a direct DataChannel opens; a Noise
   handshake runs over it; `PeerTransport` for B becomes
   `Direct { owned: PeerAddr::Rtc(id) }`, **replacing** the routed session
   rather than joining it (§5, "replacement, not a dual session"). A leaf
   has no routing table, so this is a session-table update, not a route
   install.
5. On later direct loss, B is re-reached by routed reconnection through an
   anchor — an explicit interruption, not a silent failover.
6. If ICE never connects within `ice_deadline`, the routed session is simply
   never replaced, `ice_relayed += 1`, and ICE is retried only on a browser
   network-change event or the periodic reclassify tick — never per packet.

A native node with `webrtc` on follows the same steps when its peer is a
leaf.

### 10. Stats and the direct-path witness

`RtcStats` on `MeshNode` and on the leaf: `ice_attempted`, `ice_direct`,
`ice_relayed`, `ice_failed`, `udp_blocked`, `stun_served`,
`signal_forwarded`. `ice_direct / ice_attempted` is the deployment
telemetry; it is reported, not gated.

The **witness** that a pair is off the anchor has three parts, because a flat
counter alone is also what dropped traffic looks like (Kyra, 2026-09-11):

1. **Positive receipt over the selected direct connection** — the payload
   arrives, and the receiver attributes it to the direct endpoint.
2. **A flat per-pair anchor counter** — the anchor's forwarded-packet
   counter keyed by `(src, dst)` for *application-data* subprotocols stays
   flat while that traffic flows. A globally flat anchor counter is not the
   witness: signalling, announcements and unrelated pairs legitimately keep
   flowing.
3. **An inverse forced-relay phase** — force the pair back onto the anchor
   and assert the same counter *increments*. Without this leg, part 2 cannot
   distinguish "direct" from "broken".

Anchor-side classification is viable as designed: `subprotocol_id` is a
cleartext, AAD-authenticated header field (`protocol.rs:171`, folded into the
AAD at `:357`), so an anchor can count `0x0D02` separately from
application data **without** decoding the SDP it carries.

### 11. Registry additions

| Item | Value |
|---|---|
| `SUBPROTOCOL_RTC_SIGNAL` | `0x0D02` |
| Capability tags | `rtc-anchor`, `transport:rtc`, `leaf` |
| Announcement fields (all optional, `reflex_addr` wire-compat pattern) | `noise_pubkey: Option<[u8; 32]>`, `rtc_bootstrap: Option<String>`, `rtc_addr: Option<SocketAddr>` |
| `PairAction` variant | `Ice` |
| Error | `RtcError::{UdpBlocked, IceTimeout, BootstrapRejected, …}` |
| Peer admission state (distinct from `PeerTransport`) | `Provisional` → `Admitted`, bound to the exact live session incarnation (§12) |

`docs/SUBPROTOCOLS.md` and `docs/CAPABILITIES_SCHEMA.md` are updated in the
stage that introduces each.

### 12. Browser admission contract: provisional sessions, enrollment-gated

**Decision (Kyra, 2026-09-11, checked at `79ad91283`): enrollment-gated
participation for the v1 browser-facing anchor. No intentionally open mode
in this plan.** The gate protects the anchor's *participation and
forwarding services*. It does not turn enrollment into organization
membership, channel authority or permission to invoke providers.

The bounded bootstrap target already exists: the enrollment service is
`net.mesh.enroll` (`sdk/src/mesh_enroll.rs:38`), and the current flow is
explicitly direct-addressed nRPC to the operator — `Mesh::join` dials with
`connect_via` (`:290`) and then `call`s the service with a signed
`JoinRequest`.

**Before enrollment: a restricted session, not an ordinary participating
peer.** A successful Noise handshake establishes a *provisional* session.
Until admission succeeds, permit only:

- Session establishment and teardown: the exact handshake addressed to the
  anchor, bounded handshake retries, and necessary session maintenance.
- Enrollment nRPC: a bounded unary `JoinRequest` to `net.mesh.enroll` *on
  that anchor*, and its corresponding response / error / cancellation.
- Channel membership (`0x0A00`): only the exact membership operations
  required for that enrollment request and the authenticated caller's
  reply channel. No wildcard subscriptions, arbitrary reply destinations or
  unrelated channels.
- Stream-window / reliability control: only for the bounded bootstrap
  exchange's tracked streams and packets. No arbitrary stream creation or
  caller-selected state allocation.
- Any mandatory protocol negotiation: bounded and local to the anchor; it
  must not trigger discovery, forwarding or application dispatch.

Everything else is **denied before effects**, including: capability
publication or discovery subscriptions; general event publication and
channel membership; ordinary nRPC calls; fold publication, replication,
sensing and migration; RTC peer signalling through `0x0D02`; forwarding or
initiating a routed session to another destination.

This is an **action-level allow-list, not "allow the nRPC subprotocol"**.
Allowing a whole carrier would also admit unrelated operations carried
inside it. Stage 0 (S0e) identifies the exact frames the existing
enrollment exchange needs; if that exchange currently performs broader
incidental work, the work is removed or constrained rather than the
provisional privilege set widened.

**Enforcement: installation plus checks before dispatch effects.** It
cannot live only in `accept` — enrollment happens afterward, and routed
forwarding can happen before application dispatch. The contract:

1. Install new browser-facing RTC sessions as provisional.
2. Check provisional/admitted status before route installation,
   forwarding, subscription mutation, announcement ingestion or application
   delivery.
3. For locally addressed bootstrap traffic, decode within strict bounds and
   validate the exact permitted action before its handler runs.
4. On successful enrollment, promote the **exact live session incarnation
   and authenticated identity** — not merely whichever session currently
   occupies that `NodeId`.
5. On rejection, expiry or resource-budget exhaustion, close and reclaim the
   provisional state.

Admission state is kept **distinct from `PeerTransport::{Direct, Routed}`**.
Transport ownership answers *where this session goes*; admission answers
*what this session may exercise*. They are coherently associated, never
collapsed into one concept. A provisional peer must not become a normal
discovery/routing participant merely because the handshake installer
(`mesh.rs:22050–22103`) populated a peer entry.

This is a new browser admission contract, implemented after Stage 1.
Native UDP behaviour remains unchanged by the endpoint refactor.

**No third-party relay forwarding before enrollment. No exceptions in
v1.** An unenrolled session may address the anchor's own bootstrap
service. It may not ask that anchor to forward handshake, signalling or
application packets to another peer. Crucially, the existing enrollment
flow uses `connect_via` even when the operator is the immediate
destination: a routing envelope addressed to the anchor *itself* is local
delivery, not permission to relay onward. Admission of the adjacent
browser session is checked **before** forwarding, so an attacker cannot
bypass it by putting another origin inside an opaque envelope.
Consequently the enrollment authority must be available **locally at each
bootstrap-serving anchor**, or supplied through an explicit
operator-controlled backend; a remote enrollment deployment is never solved
by giving provisional browsers arbitrary transit.

**Enrollment enables eligibility, not unrestricted authority.** After
enrollment the session becomes eligible for the anchor's configured,
bounded services — announcement handling, discovery, signalling,
forwarding. Existing channel, subnet, resource and provider checks still
apply; enrollment bypasses none of them. In particular:

> Device delegation ≠ `OrgMembershipCert` ≠ dispatcher grant ≠ provider
> admission.

Additional anchors and fresh direct sessions must verify an existing
enrollment credential with **proof bound to the new session**; they cannot
trust an "already enrolled" flag or inherit admission solely from a reused
`NodeId`. The credential-presentation mechanism is named in the Stage 4
implementation design.

**Required witnesses** (Stage 4, native anchor with a scripted browser
client; re-run in Stage 5 from the real leaf):

1. A PSK holder can complete the permitted enrollment exchange.
2. The same provisional session cannot publish an announcement, join an
   unrelated channel, invoke another service or forward to another node.
3. A local bootstrap routing envelope succeeds; the same envelope
   redirected elsewhere is denied.
4. Successful enrollment promotes only its exact live session; delayed
   completion cannot promote a replacement.
5. Provisional connections and bootstrap allocations remain globally
   bounded.
6. An enrolled peer without the required provider authority is still
   denied.

This closes the policy choice. It does not authorize implementation beyond
the already approved Stage 0.

---

## Stage 0 — Spikes (before any wide refactor)

Three throwaway spikes in parallel plus two desk inventories (S0d, S0e), none
touching `mesh.rs`. Their purpose is to retire the unknowns that the wide
refactor would otherwise be built on top of, and to return the evidence
Stage 1's authorization and the re-estimate depend on.

- **S0a — wire boundary.** Copy the seven wire modules **plus the
  routing-envelope codec** (§7) into a scratch crate, cut the two
  `session.rs` couplings, shim `Instant`, and get
  `cargo check --target wasm32-unknown-unknown` green. Minimum proof is a
  **routed handshake/envelope round-trip** through the scratch crate, not
  compilation alone — compilation can pass while omitting the leaf's
  required pre-direct path. Output: the exact list of types that must move
  for Stage 2, and the wasm size of crypto + wire.
- **S0b — RTC loop.** A scratch native binary owning a UDP socket and a
  str0m `Rtc` built with the pinned configuration (§Dependencies:
  `default-features = false, features = ["rust-crypto"]`), plus a headless
  Chromium page: DataChannel up, then a Noise NKpsk0 handshake and one
  reliable-stream round-trip over it using the S0a crate on both ends.
  Exercise **both roles** — native responder ↔ browser *and* native
  initiator ↔ browser (str0m is developed as an SFU; its README says
  peer-to-peer "has received less testing") — and observe the
  mutate → drain-to-`Timeout` invariant between mutations. Output: the
  driver ownership shape (§2) validated, the buffered-amount reading
  confirmed as advisory and the reserved-bytes bound exercised, the pinned
  dependency configuration built on every supported native target, ICE-TCP
  support in str0m confirmed or denied, `RTCPeerConnection`-in-worker
  status confirmed, and the offer → DataChannel-open setup latency with
  and without trickle (Stage 4 decides the WebSocket on it).
- **S0c — double-AEAD cost.** In S0b, measure ChaCha20-Poly1305 over DTLS
  at 60 Hz × 1 KiB and at 1 MB/s bulk in the browser. Output: a number in
  `docs/internal/performance/` and a decision on whether the DTLS-exporter
  shortcut leaves "deferred".
- **S0d — send/ingress inventory.** Not a spike: enumerate every outbound
  submission site (§1's list is the starting point, re-derived at the
  current head) and every ingress entry into `dispatch_packet`, classify
  each by deadline / error-mapping / batching behaviour, and propose the
  UDP-preserving seam contract (§1) and the bounded RTC ingress input (§2).
  Output: the inventory and the proposed contract, reviewed before Stage 1.
- **S0e — bootstrap frame inventory.** Not a spike: trace the existing
  `Mesh::join` exchange (`connect_via` handshake, `net.mesh.enroll` unary
  call, reply-channel membership, stream-window / reliability control) and
  list the exact frames, by subprotocol and action, the exchange needs.
  Flag any incidental work outside that set (§12: it is removed or
  constrained, not allowed). Output: the action-level allow-list Stage 4
  enforces.

### Exit criteria

- All five outputs written up; §2's RTC submission contract, §1's send
  seam, §7's type list and §12's allow-list finalized from them.
- Estimates for Stages 1–6 re-derived from the outputs (§Rough estimates).
- No repository code outside `docs/` and a `spikes/` scratch directory
  changed.

## Stage 1 — `PeerAddr` endpoint generalization (UDP-only, behaviour-neutral)

Generalize `PeerTransport` and every peer-keyed site listed in §Context to
`PeerAddr`, and introduce the send seam of §1 in its **UDP-preserving**
form only: every raw `socket.send_to` site and the scheduler drain submit
through the generalized endpoint with their current async behaviour,
deadlines, error mapping and batching intact. Only the `Udp` variant
exists; no feature flag yet; no RTC behaviour. Traversal modules keep
`SocketAddr` at their edges. Nothing from Open question 7 (pre-enrollment
admission) lands here — this stage is behaviour-neutral by definition.

### Exit criteria

- Zero wire change: `cross_lang_*` golden tests pass unmodified.
- Every existing integration and witness test passes; mechanical signature
  edits allowed, assertion and coverage preserved, witness diffs reviewed
  line by line.
- The repository's full applicable pre-push matrix (`AGENTS.md`, "Pre-push
  checklist"): `cargo fmt --check`, `cargo check --workspace --all-targets`,
  the three strict `--lib --bins` clippy feature sets, the permissive
  `--all-targets` clippy, and `RUSTDOCFLAGS="-D warnings" cargo doc` — not
  one clippy command.
- UDP send semantics unchanged: `deliver_stream_packet`'s unscheduled arm
  still maps failure to `StreamError::Transport`; `bound_datagram_send`
  deadlines and the `sendmmsg` drain grouping survive; no UDP `WouldBlock`
  surfaces as `Backpressure`.
- Default build: exported C-ABI symbol set unchanged. **Stage 1 owns this
  job**: establish the symbol baseline and the comparison *before* the
  endpoint refactor lands, so the criterion can actually fail. It is a stage
  deliverable, not unowned infrastructure.

## Stage 2 — `net-wire` crate + `Clock`

Extract per §7 using S0a's type list (eight named items plus `route_hop.rs`,
`ParsedPacket`, the coarse clock, `StoredEvent`); the AEAD backend seam;
re-export under existing paths; add the wasm32 check and a
`cross_lang_wire` golden fixture set (header, `RoutingHeader` envelope,
`EventFrame`, `NackPayload`, `StreamWindow`, a **cross-backend AEAD
vector** — same key/nonce/AAD/plaintext ⇒ identical ciphertext on `ring`
and `chacha20poly1305` — and announcement with and without the new
optional fields). Decide where each left-behind `#[cfg(test)]` module
lands, including the `heartbeat_api_drift_check` tripwire.

### Exit criteria

- `cargo check -p net-wire --target wasm32-unknown-unknown` in CI **plus an
  executed wasm test** (wasm-bindgen-test or a wasmtime-hosted probe) that
  runs the S0a routed round-trip on the wasm build — a check job cannot
  catch `Instant::now()`'s runtime panic. **Stage 2 owns those jobs and the
  target installation** — a stage deliverable, not unowned infrastructure.
- `cargo test -p net-wire` runs the crypto, protocol, reliability and
  session unit tests natively; the `heartbeat_api_drift_check` guard still
  exists somewhere and still fails on drift.
- No behaviour change under default features; no `use` path in the core or
  any binding changes.

## Stage 3 — Native `webrtc` feature: driver, dedicated socket, STUN, loopback harness

- `adapter/net/rtc/{mod,driver,transport,stun,config}.rs`; `PeerAddr::Rtc`;
  the RTC arm of the §1 send seam and of the scheduler drain; the bounded
  RTC input on `IngressReceiver` (§2, one dispatch owner);
  `RtcConfig { bind_addr, public_addr, ice_deadline, max_peers,
  send_queue_packets, send_queue_bytes, buffered_amount_advisory,
  serve_stun, serve_bootstrap }` — `send_queue_packets` / `send_queue_bytes`
  are the reserved slots/bytes that form the hard admission bound;
  `buffered_amount_advisory` is the driver-published SCTP reading that may
  refuse earlier but never defines the bound (§2).
- **Harness:** two native nodes on loopback where B's session to A is forced
  onto a DataChannel via a test-only `connect_rtc_loopback`. Run the
  *existing* stream, reliability, backpressure, nRPC, fold and
  routing-plane witness files against it.

### Exit criteria

- Those files pass with the peer on `PeerAddr::Rtc`, including the
  stale-session and direct/routed migration witnesses.
- Admission refuses — reserved slots or reserved bytes exhausted, or the
  advisory buffered-amount reading over threshold — surface as
  `Backpressure` *from `try_send` itself*, with no packets enqueued and no
  credit committed; nothing refuses after acceptance (§2). Test with an
  injected slow DataChannel, and separately with a `Channel::write` stub
  returning `Ok(false)` after a passing advisory precheck: that path must
  exercise the post-acceptance retention policy and must NOT surface as
  whole-call backpressure. A witness must show the reserved-bytes bound
  holding while the advisory reading is stale (driver paused mid-drain).
- RTC ingress reaches `dispatch_packet` only through the receive loop's
  single owner: a witness asserts no second caller and preserved per-source
  ordering across the UDP and RTC inputs.
- UDP behaviour with `webrtc` on is byte-for-byte the Stage 1 behaviour:
  `deliver_stream_packet`'s UDP arm, `bound_datagram_send` deadlines and
  the `sendmmsg` grouping are unchanged.
- Every non-`Stream::send` outbound class (events, `0x0D02` signalling,
  forwarding, retransmission) has its stated pressure disposition asserted.
- The §5 delivery sequence — routed → authenticated direct → forced direct
  failure → restored routed — passes on the loopback harness.
- External `stunclient` gets a correct XOR-MAPPED-ADDRESS from the RTC
  socket; the Net socket's ingress tests are byte-for-byte unchanged with
  `webrtc` on.
- Default build unchanged; `--features webrtc` clean under `-D warnings`.

## Stage 4 — Announcement fields, `0x0D02`, bootstrap credential + listener

- `noise_pubkey` / `rtc_bootstrap` / `rtc_addr` on the announcement, added to
  the **hand-maintained canonical signer** `SignedPayloadCanonical`
  (`behavior/capability.rs:2402–2463`; signed via `serde_json::to_vec` at
  `:2634`) — **not** merely to the derived `Serialize`. That serializer is
  where the signature preimage is actually produced, and its own doc warns
  that field order, names and `skip_serializing_if` must match the struct
  declaration exactly; adding a field to one and not the other silently
  breaks signing or wire compat. Emission off by default (OA-1 migration
  pattern); `PairAction::Ice`.
- `0x0D02` codec, dispatch, forwarding, per-sender budget.
- Bootstrap HTTPS/WebSocket listener (`axum` + the payments crate's pinned
  rustls), feature-gated. **Browser-trusted TLS is in scope** (Fable /
  Kyra, 2026-09-11): browsers refuse a self-signed `https://<anchor>`, the
  app origin differs from the anchor origin, and trickle rides `wss`. Stage
  4 therefore delivers a browser-trusted certificate path (ACME or
  operator-supplied), an explicit cross-origin HTTP policy, and WebSocket
  `Origin` validation if trickling remains. CI must not hide deployment
  failures behind certificate-ignore flags. A gather-complete single POST
  (no WebSocket) is a valid simplification, decided on S0b's setup-latency
  evidence, not by preference. **The browser bootstrap credential** of §5
  Layer 0 — its encoding, minting path and expiry rules, reviewed as a new
  credential format, not as invite reuse; `Mesh::join` over a DataChannel
  session.
- `net-mesh anchor` CLI subcommand; Deck surfaces anchors and their
  `rtc_addr`.
- **Browser admission contract (§12).** Provisional installation for
  browser-facing RTC sessions; admission checks before route installation,
  forwarding, subscription mutation, announcement ingestion and application
  delivery; strict-bounds decode + exact-action validation for local
  bootstrap traffic; session-bound promotion on enrollment; reclaim on
  rejection / expiry / budget exhaustion; global bounds on provisional
  connections and bootstrap allocations. The credential-presentation
  mechanism for additional anchors and fresh direct sessions is named in
  this stage's design. Enrollment authority local to each bootstrap-serving
  anchor or an explicit operator-controlled backend.

### Exit criteria

- A native node with `webrtc` on completes bootstrap-credential →
  offer/answer → DataChannel → Noise (against the credential's pinned anchor
  key) → enrollment grant with a scripted Chromium client.
- **MITM witness, correctly shaped.** Mutating a key in the HTTP response
  proves nothing — that field is ignored by construction. The test must
  substitute the actual responder with one that does **not** hold the
  credential-pinned static private key, and assert the Noise handshake fails.
- **Canonical-signer witnesses:** tampering with each of the three new fields
  invalidates the signature; an announcement with all three absent produces
  byte-identical signed bytes to the pre-field form; announcement encoding
  remains JSON.
- Signalling between two nodes sharing no direct session is delivered via an
  intermediate anchor; a test asserts the anchor never reads the **SDP**. It
  may legitimately *classify* the packet as `0x0D02` — `subprotocol_id` is a
  cleartext AAD-authenticated header field (`protocol.rs:171`, `:357`) — so
  the assertion is about payload confidentiality, not id obscurity.
- Bootstrap budget rejections are typed and fast.
- An anchor behind a simulated NAT publishes a working `rtc_addr` via
  `public_addr` or port mapping.
- The six §12 witnesses pass: permitted enrollment exchange completes;
  provisional session denied announcement / unrelated channel / other
  service / forwarding; local bootstrap envelope accepted, redirected
  envelope denied; promotion binds the exact live session and a delayed
  completion cannot promote a replacement; provisional connections and
  bootstrap allocations globally bounded; enrolled peer without provider
  authority still denied.

## Stage 5 — `net-leaf` + `@net-mesh/browser`

- Leaf crate and TypeScript wrapper; identity storage and leader election
  (§8); dispatcher; channels; nRPC client; fold announce; capability query.
- **`ControlPlane` trait from day one.** Everything the leaf needs from an
  anchor that is *not* a Net packet on the data path — bootstrap
  offer/answer, candidate trickle, announcement publish/subscribe,
  signalling dialog transport for peers it has no session with yet — goes
  behind one trait. `AnchorControlPlane` (DataChannel to a native anchor,
  §5) is the only v1 implementation. The trait exists so the serverless
  follow-on (§Follow-on) is a second implementation, not a refactor; it
  must not leak `PeerAddr::Rtc` or any anchor-specific type. **The trait
  alone does not reconcile the two startup sequences** (Kyra, 2026-09-11):
  the native §5 path requires a routed A↔B Noise session *before* signalling,
  while serverless Tier A promises signalling and discovery with no relay,
  and therefore cannot establish that prerequisite. Stage 5 must specify the
  **session-independent signalling path** that makes an anchorless start
  possible, or the follow-on is a refactor after all.
- `cross_lang_wire` replay inside the wasm test runner.
- Playwright CI: Chromium + Firefox against a native anchor — handshake,
  reliable round-trip, fire-and-forget loss under injected DataChannel loss,
  nRPC to a native service, a native peer's `find_best_node` returning the
  browser node, two tabs sharing one identity without evicting each other,
  and a prompt **typed** failure under a UDP-blocked profile. **Stage 5 owns
  the browser runner and matrix** — a stage deliverable, not unowned
  infrastructure.
- **Failure typing, corrected.** An ICE timeout does not prove UDP blocking:
  an unreachable, misconfigured or overloaded anchor produces the same
  symptom. The surfaced result stays a typed timeout/unreachable
  (`RtcError::IceTimeout`) unless the narrower cause is actually established;
  `RtcError::UdpBlocked` is reserved for evidence that distinguishes it.

### Exit criteria

- All scenarios green on Chromium and Firefox; Safari best-effort, recorded.
- Bundle and wasm sizes recorded; wasm ≤ 1.5 MB gzipped target.
- The anchorless mock `ControlPlane` (in-memory, no anchor) drives the leaf
  through handshake and one direct browser ↔ browser session in the wasm test
  runner. To count, it must be **genuinely anchorless**: no forwarding of
  pre-direct Net packets behind the mock, and every input it needs —
  identity, keys, admission — explicitly provisioned by the test. A mock that
  quietly relays is a re-implementation of the anchor and proves nothing
  about the trait boundary.

## Stage 6 — Browser ↔ browser direct, NAT conformance, telemetry, demo

- §9 end to end; `RtcStats`; browser network-change retry trigger; per-pair
  witness counter on anchors.
- **Deterministic conformance:** extend the NAT simulator harness from
  [`NAT_TRAVERSAL_V2_PLAN.md`](NAT_TRAVERSAL_V2_PLAN.md) Stage 4 with two
  headless browsers behind simulated cone / port-restricted / symmetric NATs
  and one anchor. **Prerequisite, checked 2026-09-11:** that harness is
  recorded as *"landed, pending first CI run"* — authored blind on a macOS box,
  with only its loopback halves verified locally and the netns halves never
  executed. `natsim.yml` exists. **Check for a green run at the relevant
  revision first; if there is none, dispatch the existing workflow before
  authoring the browser extension.** Execution is cheap, but a failed
  environment or a broken simulator is repair work with its own cost — do not
  treat this as free.
- **Field telemetry:** `ice_direct / ice_attempted` exported through the
  existing stats surface and Deck; documented as a deployment metric with
  its own denominator.

### Exit criteria

- Every matrix row ICE is expected to solve lands direct; symmetric ×
  symmetric lands relayed; `ice_direct + ice_relayed + ice_failed +
  udp_blocked == ice_attempted`; 100 % of sessions established **over rows
  where both anchors are reachable**.
- A 60 Hz three.js position-update demo between two tabs shows the pair's
  application-data forwarding counter on the anchor flat once direct, while
  signalling and announcements continue.

## Stage 7 — Surface completion and deferred items (**DEFERRED**)

- Node / Python / Go anchor-role parity for `RtcConfig` + `RtcStats`.
- `sdk-ts` / `@net-mesh/browser` shared generated types.
- **Packaged anchor.** A single-binary / container distribution of a
  `webrtc`-enabled node preconfigured as an anchor (`serve_bootstrap`,
  `serve_stun`, a pinned `rtc_addr`, invite minting), so that "deploy a
  browser-native Net app" means static assets plus one small always-on
  process. This is the answer to serverless-only hosting (§Non-goals).
- ICE-TCP passive candidates on anchors (if S0b confirms str0m support).
- DTLS-exporter shortcut (if S0c demands it).
- Browser-side RedEX on IndexedDB (separate plan).

---

## Critical files

### Stages 1–2 (endpoint generalization, wire crate)

- `adapter/net/mesh.rs` — `PeerTransport` (`:2990`), `addr_to_node` (`:1424`),
  `pending_direct_initiators`, dispatch context, `spawn_receive_loop`
  (`:24203`), `connect*` (`connect_via` at `:39609`), reroute call sites,
  announcement route-learning (`:33150`).
- `adapter/net/transport.rs` — `PeerAddr`, the send seam (UDP submission in
  Stage 1; the RTC half arrives in Stage 3).
- `adapter/net/route.rs`, `reroute.rs`, `router.rs`, `proxy.rs`,
  `failure.rs`, `session.rs`, `swarm.rs`, `behavior/proximity.rs`,
  `behavior/fold/{routing,capability}.rs`.
- New `crates/net/wire/`; `Cargo.toml` workspace + tokio gating; `lib.rs`
  re-exports; `.github/workflows/ci.yml` wasm32 check + symbol diff.

### Stages 3–4 (native backend, signalling)

- `adapter/net/rtc/` — new module.
- `adapter/net/traversal/classify.rs` — `PairAction::Ice`.
- `adapter/net/behavior/capability.rs`, `docs/CAPABILITIES_SCHEMA.md` —
  three optional fields + tags.
- `adapter/net/subprotocol/mod.rs`, `docs/SUBPROTOCOLS.md` — `0x0D02`.
- `sdk/src/mesh_enroll.rs` — RTC `Rendezvous` form.
- `cli/` — `anchor`, `rtc stats`; `deck/` — anchors, leaves, `rtc_addr`.
- `adapter/net/mesh.rs` — provisional/admitted state alongside `PeerInfo`,
  checks at the forwarding, route-install, subscription, announcement and
  delivery boundaries (§12); `sdk/src/mesh_enroll.rs` — local enrollment
  authority at bootstrap-serving anchors.

### Stages 5–6 (leaf, P2P)

- `crates/net/leaf/`, `crates/net/sdk-browser/`.
- `tests/cross_lang_wire/`, `tests/rtc_*.rs`, Playwright suite under
  `crates/net/leaf/e2e/`, NAT simulator extension.

---

## Open questions — dispositions (Kyra, 2026-09-11)

Five are **closed** as recorded policy; two stay at their evidence gates.
Question 7 was added and closed on 2026-09-11 (§12). Closing
these does **not** discharge the implementation defects in the
[Review log](#review-log) — that is a separate repair list.

1. **Anchor placement — CLOSED: explicitly operator-selected.** Compiling
   `webrtc`, or enabling browser connectivity, does **not** make a node a
   public anchor. `serve_bootstrap` is opt-in. Publishing an `rtc_addr` as a
   STUN endpoint requires its responder to be enabled and reachable. An
   ordinary WebRTC-capable native provider need not accept bootstrap
   traffic. Browser connectivity and volunteering public infrastructure stay
   separate decisions.
2. **Leaf announcement TTL — CLOSED: 60 s TTL, 20 s re-announce, both
   configurable.** One wording correction to the proposal: 3× gives more
   missed-refresh margin than the native 2× (`DEFAULT_TTL_SECS` 300 /
   `capability_reannounce_interval` 150) — it is *not* a stricter invariant.
   Cost, sized not certified: the refresh-frequency increase is 7.5×, so at
   1,000 leaves that is ~50 distinct refresh announcements per second, ~150
   submissions per second if each leaf sends every refresh to three anchors,
   plus mesh forwarding that depends on topology and duplicate suppression.
   Enough to pick a default; not enough to certify deployment cost — measure
   serialized bytes, verification work and forwarded copies in a
   representative deployment. **And the bound is on stale discovery only:**
   announcement TTL is not authority and not live-session failure detection,
   and must never become permission to use a dead or revoked peer for
   another minute.
   **Leaf-side ingress is also unsized** (Kyra, 2026-09-11): each leaf
   receives the flood its anchors forward, so measure leaf receive bytes
   and signature-verification work as well — a per-second figure derived
   from refresh frequency alone assumes an announcement size and a
   deduplication behaviour it has not established. Filtering what anchors
   forward to `leaf`-tagged peers would be a separate discovery-contract
   decision, not a tuning knob.
3. **Anchors per leaf — CLOSED: `min(3, n)` is a target, not a startup
   requirement.** Bootstrap through one; replenish toward the target as
   eligible anchors become known; never delay a usable direct connection
   waiting for three. Prefer independent failure domains where that is
   knowable — three processes behind one failed uplink are not redundancy.
   No placement subsystem. This belongs in the `ControlPlane` lifecycle
   **before the trait freezes** in Stage 5.
4. **Direct-path mechanism — OPEN at Stage 3, but the required behaviour is
   settled now.** Direct-path loss must produce either explicit interruption
   followed by routed reconnection, or continuity through a supported
   migration mechanism. The harness may choose how; it may not decide
   afterwards what "fallback" was supposed to mean. Single-session
   replacement must never be described as retaining a simultaneously usable
   routed backup (§5).
5. **Browser identity lifetime — CLOSED: keep the persistent browser
   identity; no blanket short-lived-identity policy.** Use the existing
   membership lifecycle and revocation with bounded, capability-specific
   dispatch authority. Shortening every membership certificate is not an XSS
   remedy: hostile same-origin code can use live authority and may also reach
   the renewal machinery, so short grants help only where renewal and
   issuance enforce a separate boundary. Managed browser nodes use the
   existing managed membership policy; ephemeral guest identities are an
   explicit application option. **No browser-specific identity-rotation
   subsystem in this plan.**
6. **Safari — OPEN, evidence-gated.** Record exact tested versions and
   behaviours from the real leaf; decide support status from that, not from
   transport folklore.
7. **Pre-enrollment privileges — CLOSED (Kyra, 2026-09-11): enrollment-gated
   anchors, narrowly scoped bootstrap, no pre-enrollment transit,
   session-bound promotion, provider authority unchanged.** No intentionally
   open mode in this plan. Full contract, enforcement points and the six
   required witnesses in §12. The gate protects the anchor's participation
   and forwarding services; it is not organization membership, channel
   authority or provider admission. Implemented after Stage 1, never inside
   the behaviour-neutral refactor; does not authorize implementation beyond
   Stage 0.

---

## Rough estimates

**Withdrawn 2026-09-11.** The revision-2 figures (~33–42 days serial) did
not include the send-path seam and scheduler work now in Stage 1, the
routing-envelope extraction in Stage 2, or the follower-tab proxy and
leader lifecycle in Stage 5; S0b's own budget did not cover a wasm build
of S0a plus JS glue plus a scratch signalling server. Doubling them would
not be evidence either. Stage 0 returns the actual extraction list, the
send/ingress inventory, the dependency build results and the browser
harness evidence; Stages 1–6 are re-estimated from those outputs, and the
re-derived table replaces this section.

Sequencing that survives the withdrawal: Stage 0 gates the trait and type
decisions; Stage 5 can start against the Stage 3 harness before Stage 4
completes. **Corrected by S0a:** Stages 1 and 2 are *not* independent —
`NetSession::peer_addr` and `ParsedPacket::source` are `SocketAddr`, so
`net-wire`'s final shape needs `PeerAddr`. Either Stage 2 follows Stage 1,
or Stage 2 makes the peer endpoint a type parameter of the wire types and
Stage 1 instantiates it; the choice is made when Stage 1 is authorized.

---

## Dependencies

- `str0m` 0.23.1 — sans-IO WebRTC. Native, feature `webrtc` only. MSRV
  1.85.0 (toolchain is 1.98.0), MIT OR Apache-2.0. **Pinned configuration**
  (Kyra, 2026-09-11) — the crate's default features enable `aws-lc-rs` and
  `examples`, which pull cmake/C (NASM on Windows) into the build:

  ```toml
  str0m = { version = "0.23.1", optional = true, default-features = false, features = ["rust-crypto"] }
  ```

  S0b proves this configuration on every supported native target;
  dependency defaults do not get to make the decision. ICE-TCP support to
  be confirmed in S0b. *(Still the current release: 0.23.1, 2026-08-21, per
  docs.rs at 2026-09-11. A web search claiming 0.21.0 is the latest is
  stale — trust the registry.)*
- `web-time` — `Instant` on wasm, `net-wire` on wasm32 only.
- `getrandom` `wasm_js` — wasm32 only.
- `wasm-bindgen`, `web-sys` (`RtcPeerConnection`, `RtcDataChannel`,
  `IndexedDb`, `SubtleCrypto`, Web Locks, `BroadcastChannel`),
  `wasm-bindgen-futures` — leaf only.
- `axum` + the payments crate's pinned `rustls` — bootstrap endpoint,
  feature `webrtc` only.
- Playwright — CI dev dependency.

No new dependency reaches the default build.

**CI capability this plan assumes and the repository does not have yet
(checked 2026-09-11).** Three exit criteria are currently unfalsifiable
because the jobs they name do not exist:

- **No `wasm32` anywhere in CI** — zero matches for `wasm32` across
  `.github/workflows/*.yml`. Stage 2's
  `cargo check -p net-wire --target wasm32-unknown-unknown` needs the target
  installed and a job written.
- **No Playwright** — zero matches. Stage 5's browser matrix has no runner.
- **No exported-symbol diff** — Stage 1's exit criterion ("exported C-ABI
  symbol set unchanged (symbol diff in CI)") has nothing to compare against.
  That job must exist *before* Stage 1, or the criterion cannot be met or
  failed.

Also absent, as expected at "not started": `crates/net/wire/`,
`crates/net/leaf/`, and any `str0m` / `web-time` / `wasm-bindgen` entry in
`net/crates/net/Cargo.toml` (every `wasm-bindgen` / `web-sys` line in
`Cargo.lock` is still transitive). `getrandom` 0.4.3, `ring` 0.17.14 and
`snow` 0.10.0 are present and are the versions §Context assumes.

---

## Out of scope (for this plan)

- WebTransport; WebSocket data paths.
- A TURN protocol server; any UDP-blocked rescue in v1.
- Audio / video / SRTP.
- Browser forwarding for other browsers.
- Browser-side durable storage.
- A collaborative-text CRDT (Yjs / yrs, Automerge). A separate integration
  note should cover the fire-and-forget + state-vector-sync pattern once
  Stage 5 gives it a transport.
- Native ↔ native WebRTC.

## Follow-on: serverless control plane (not in this plan)

Recorded here so the v1 design keeps the door open; scoped and staged in a
separate plan once Stage 6 lands.

**What "serverless support" means.** Hosting a browser-native Net app with
no always-on native anchor — static assets plus a serverless runtime
(Cloudflare Workers / Durable Objects, Deno Deploy, Vercel or Netlify
functions, Lambda). §Non-goals explains why v1 cannot: those runtimes cannot
bind a listening UDP socket or hold ICE / DTLS / relay state alive. The
follow-on replaces each anchor *function* with a serverless-compatible
substitute. The data path is untouched: sessions still ride direct
DataChannels, Noise end to end, the same wire.

| Anchor function | Serverless substitute | Marginal effort after v1 |
|---|---|---|
| Bootstrap + ongoing signalling (§5 Layer 0, Layer 3) | One durable object per room holding a WebSocket to each tab; SDP and candidates are opaque payloads | small–medium |
| Discovery / announcement flooding (§7) | The same object stores signed announcements; tabs subscribe. Announcements are self-authenticating (entity signature), so the store is dumb and the leaf verifies on receipt | small–medium |
| STUN (§6) | A third-party STUN server, configured as `iceServers`. Serverless cannot answer UDP. **Policy relaxation, one config line** | trivial |
| Relay fallback for ICE failures (§6) | Net packets over the room object's WebSocket, Noise-opaque to it; or a managed TURN. The only place a Net packet touches a WebSocket, and only for the failure case | medium |
| Enrollment / admission (§5 Layer 0) | The enrollment handler is **SDK code, not wire code** — porting it to the worker runtime is its own scoped work, not a consequence of `net-wire` targeting wasm; the delegation root lives in the platform secret store | medium–large |

**Why it is additive.** `net-wire` targets wasm, so it runs inside Workers as
well as browsers. Signed announcements need no trusted store. `PeerAddr`
gains a `Ws` variant used only by the leaf's control plane and the relay
fallback. The framing "anchors are how browsers *find* each other" holds
exactly — only the finder moves.

**Two corrections to that claim** (Kyra, 2026-09-11). First, compiling
`net-wire` to wasm does **not** hand the worker an enrollment handler: that
service lives in the SDK, not the wire layer, so its portability is
follow-on work to be scoped, not a free consequence of the crate split.
Second, "the invite / enrollment flow is unchanged" no longer holds — v1
introduces a browser bootstrap credential (§5 Layer 0), and the follow-on
inherits that artefact, including the question of where its PSK lives when
there is no always-on anchor to hold one.

**What it costs, honestly.**

- The "no third-party infrastructure" goal is relaxed for STUN. Product
  decision, not engineering.
- The relay fallback is a deliberate exception to the "no WebSocket data
  path" non-goal, confined to pairs ICE cannot connect. It stays
  Noise-opaque, so the security model survives.
- Running enrollment in a worker moves the delegation root into a platform
  secret. Custody decision.
- Platform limits need measuring before commitment: idle-WebSocket
  hibernation, per-object fan-out ceilings, egress pricing on the relay
  path, CPU budget per invocation for Noise handshakes.

**Tiers and rough sizing (after Stage 6):**

| Tier | Scope | Estimate |
|---|---|---|
| A | Serverless signalling + discovery + third-party STUN; ICE failures simply fail | ~2 weeks |
| B | A + WebSocket relay fallback through the room object | ~2 weeks more |
| C | B + in-worker enrollment and admission | ~2–4 weeks more |

**The one v1 design hook that makes this true:** the leaf's `ControlPlane`
trait (Stage 5). With it, every tier above is a second implementation of an
existing boundary; without it, Tier A alone is a leaf refactor.

## Related plans

- [`NAT_TRAVERSAL_PLAN.md`](NAT_TRAVERSAL_PLAN.md) /
  [`NAT_TRAVERSAL_V2_PLAN.md`](NAT_TRAVERSAL_V2_PLAN.md) — mesh-native
  STUN/TURN for native peers; the relay fallback and punched-path upgrade
  this plan mirrors; the NAT simulator Stage 6 extends. Both list "browser /
  WebRTC bridge" as out of scope; this is that plan.
- [`HERMES_INTEGRATION_PLAN_V2.md`](HERMES_INTEGRATION_PLAN_V2.md) — the
  invite / `Rendezvous` / enrollment flow §5 reuses for first contact.
- [`NRPC_RECV_LOOP_BATCHING_PLAN.md`](NRPC_RECV_LOOP_BATCHING_PLAN.md) — the
  `IngressReceiver` seam. The UDP socket and its framing stay unchanged; the
  ingress *orchestration* gains a bounded RTC input so dispatch keeps one
  owner (§2). "Left untouched" was withdrawn 2026-09-11.
- [`FAIRSCHEDULER_TRANSPORT_PLAN.md`](FAIRSCHEDULER_TRANSPORT_PLAN.md) —
  subprotocol-id and stream-allocation conventions.
- [`MCP_BRIDGE_PLAN.md`](MCP_BRIDGE_PLAN.md) — attach vs participate;
  browsers participate.
- [`CAPABILITY_AUTH_PLAN.md`](CAPABILITY_AUTH_PLAN.md) /
  `ORG_CAPABILITY_AUTH_PLAN.md` — the optional-field migration pattern and
  the admission path the leaf reuses.

---

## Review log

**2026-09-05 — Kyra, source-checked review of draft 1.** Verdict: direction
right, not implementation-ready. All findings verified against the tree and
applied in this revision:

| # | Finding | Applied as |
|---|---|---|
| 1 | Shared-socket first-byte demux would drop native traffic (magic is LE `45 4E`; routing, route-hop, keep-alive and headerless pingwave formats exist; RFC 7983 64–79 is TURN ChannelData) | §6: dedicated RTC socket, no demux; outer-format inventory in §Context |
| 2 | First-contact key dependency was circular; PSK path unnamed | §5: invite-carried anchor key → signed `noise_pubkey` on announcements → routed session → opaque signalling; PSK = invite, org = enrollment grant |
| 3 | Fallback cannot rescue a UDP-blocked browser | §6: declared unsupported in v1, typed `UdpBlocked`; "100 %" scoped to reachable-anchor pairs |
| 4 | TTL 0 does not express "I do not forward" and is dropped as expired | §7: non-forwarding is a role; leaves set normal TTLs |
| 5 | `PeerTransport::{Direct, Routed}` already separates destination from ownership | §1: generalize it, preserve its protections, re-exercise on RTC |
| 6 | Sans-IO owner, bounded send, SCTP buffering, 16 KiB claim | §2 driver contract; §3 and §Context corrected |
| 7 | Key-storage guarantee overstated; multi-tab undefined | §8: same-origin boundary stated; leader-elected single node per origin |
| — | Spikes before the wide refactor; wire split is a crate split; conformance vs deployment denominators; per-pair witness; compatibility guarantee precision; allow mechanical test edits | Stage 0; §7 / Stage 2; Goals + Stage 6; §10; Goals; Stage 1 |
| — | Name the announcement route-learning path; `reflex_addr` carries a `SocketAddr` on the wire | §Context, §7; wire-address inventory |
| — | 2026-09-05, product owner: serverless-only hosting is anchorless, not UDP-blocked | §Non-goals, §6 "Serverless", Stage 7 packaged anchor |
| — | 2026-09-05, product owner: document the serverless follow-on and keep v1 open to it | §Follow-on; `ControlPlane` trait + mock-driven exit criterion in Stage 5 |

**2026-09-11 — re-baseline against `master` `132dbdcff`.** No design change;
citations only. Every `path:line` and count in §Context, §1 and §Critical
files re-derived at that head. What moved: the organization exact-sensing /
sensing-SDK lane (PR #943, PR #949) put 62 commits on `mesh.rs` and grew it
43,114 → 52,943 lines, shifting every `mesh.rs` reference by 6–7k lines
(`PeerTransport` 2800 → 2990, `spawn_receive_loop` 17815 → 24203,
`addr_to_node` 1281 → 1424, route-hop dispatch 18214 → 24602, headerless
pingwave 18014 → 24402, `connect_via` 33114 → 39609, announcement
route-learning 26655 → 33150). What did not move: the `PeerTransport` shape
and its 19 match sites, all five outer packet formats, the seven wire modules
(byte-identical), `MAX_PACKET_SIZE = 8192`, `route.rs:416` / `session.rs:67` /
`swarm.rs:343` / `capability.rs:2319` / `crypto.rs:206` /
`protocol.rs:9`+`:378` / `route.rs:28`+`:132` / `route_hop.rs:53` /
`rendezvous.rs:191` / `gateway.rs:329–342` / `Cargo.toml:239`, the free
`0x0D02` id, and `str0m` 0.23.1.

Four claims were wrong when written and are corrected in place: `traversal/*`
carries **46** `SocketAddr` mentions, not ~60; `session.rs` has **zero** tokio
call sites (its two `tokio::` hits are prose in comments); the "123
`cfg(target_os …)` sites" figure is **16** `target_os` sites out of 123
platform `cfg(…)` sites overall; and `thread::spawn` appears in 36 files with
test modules counted, not ~20 production files.

Two prerequisites were added rather than discovered later: the routing-plane
witness floors Stage 1 must clear (§1 — `MIN=93`, up from the 86 `AGENTS.md`
still cited, plus four sibling floors; `AGENTS.md` corrected in the same
commit), and the fact that the NAT simulator Stage 6 extends has never had a
green CI run (Stage 6). §Dependencies now also records the three CI
capabilities the plan's exit criteria assume and the repository lacks: wasm32,
Playwright, and an exported-symbol diff.

**2026-09-11 — Kyra, source-checked review of revision 2 (read at
`18e73e4dbf0a35cbfe5ea06de7c1edb7a9d64d62`).** Verdict: architecture right,
not implementation-ready; **authorize the bounded Stage 0 spikes only**. Kept
without change: WebRTC primary, browser leaf, dedicated RTC socket, preserved
direct/routed ownership, Noise endpoint identity, the explicit UDP-blocked
limitation, and real-browser spikes before the broad refactor. Every citation
below was re-verified against this tree before applying.

| # | Finding | Verified at | Applied as |
|---|---|---|---|
| 1 | The existing invite does **not** contain the PSK — `InviteToken` is `root`/`rendezvous`/`nonce`/`expires_at`, and `Rendezvous`'s own doc calls the PSK an out-of-band build-time property | `sdk/src/enrollment.rs:216–228`, `sdk/src/mesh_enroll.rs:47–54` | §5 Layer 0 defines an explicit **browser bootstrap credential** (invite + anchor static key + PSK + bootstrap URL) as a NEW format with its own Stage 4 review; the "unchanged enrollment reuse" claim is withdrawn |
| 2 | Device enrollment ≠ organization membership: enrollment yields `DelegationChain::derive_device`, while protected invocation needs `OrgMembershipCert` + `OrgDispatcherGrant` + call-binding signature; owner-private services announce only on `0x0C04`, which the leaf drops | `sdk/src/enrollment.rs:836–842`, `behavior/org_call.rs:175–189`, `behavior/broadcast.rs:35–44` | §5 scopes v1 to the **public-capability path**; org capabilities become a separate slice gated on one discovery→invocation witness plus a no-dispatcher-authority refusal witness |
| 3 | Queue admission and a later SCTP refusal are different outcomes; `StreamError::Backpressure` means *nothing was enqueued*, while a returned `try_send` has already committed credit and retransmission state — and `Channel::write` can return `Ok(false)` after a passing precheck | `stream.rs:168–170`, `mesh.rs:39273–39292` | §2 makes bounded admission the single synchronous refusal boundary, folds the buffered-amount check into it, and assigns post-acceptance retention/retry/drop/close to Stage 3; partial-send accounting preserved. §3's "fire-and-forget has no credit" corrected — credit and reliability are independent |
| 4 | Session replacement does not retain a live routed backup: the installer replaces the sole session and drops the displaced reverse index; the upgrade protects the incumbent only by *deferring* while streams/unacked remain | `mesh.rs:22050–22103`, `mesh.rs:39886–39897` | §5 and §9 state replacement, not dual-session; the required behaviour (interruption + routed reconnection, or a designed migration) is settled now, with the delivery-sequence witness routed → direct → forced failure → restored routed |
| 5 | The signature preimage comes from a hand-maintained `SignedPayloadCanonical`, not the derived `Serialize` | `behavior/capability.rs:2402–2463`, signed at `:2634` via `serde_json::to_vec` | Stage 4 names that serializer in the implementation contract and requires the three witnesses: per-field tamper invalidation, all-absent byte identity, JSON encoding retained |
| 6 | The serverless hook is startup-incompatible: the native path needs a routed A↔B session before signalling, which Tier A cannot establish; and `net-wire`-on-wasm does not supply the enrollment handler (it is SDK code) | plan §5 vs §Follow-on | Stage 5 must specify the session-independent signalling path and prove it with a **genuinely anchorless** mock (no pre-direct packet forwarding, explicit identity/key/admission provisioning); §Follow-on's portability claim corrected |
| — | Acceptance-test corrections: MITM must substitute the responder, not mutate an ignored field; an ICE timeout is not proof of UDP blocking; a flat anchor counter alone can mean dropped traffic; `0x0D02` *is* classifiable in the clear header, only the SDP is confidential | `protocol.rs:171`, `:357` | Stage 4 exit criteria; §6 typed failure; §10 three-part witness incl. forced-relay inverse |

**Dispositions and sequencing recorded the same day.** Open questions 1, 2, 3
and 5 closed as policy (see §Open questions — dispositions); 4 and 6 stay at
their evidence gates, with 4's *required behaviour* fixed now. Missing CI
jobs are assigned to their consuming stages — Stage 1 owns the exported-symbol
baseline, Stage 2 the wasm target, Stage 5 the browser runner — rather than
forming an unowned infrastructure lane. `mesh.rs` sequencing: Stage 0 →
bounded UDP-only Stage 1 → leader implementation, justified by avoiding
overlapping structural edits, **not** by Stage 1 being harmless; "browser
first" must not expand into Stages 2–6 before returning to sensing/OLB.

**2026-09-11 — Fable, source-checked review of revision 2 (read at
`9b1ce093330bae34ea5e192867f14e3df7466ead`); dispositions by Kyra the same
day.** Verdict: the six revision-2 repairs verified; architecture holds;
four gaps sit under Stage 1 and are repaired as pre-Stage-1 decisions.
Accepted with three qualifications: the PSK is not necessarily publicly
distributed (stated precisely in §5), a buffered-amount snapshot cannot
enforce a hard bound (reserved queue bytes/slots do, §2), and doubling
estimates is not evidence (withdrawn, §Rough estimates).

| # | Finding | Verified at | Applied as |
|---|---|---|---|
| 1 | The send path is raw `socket.send_to(..).await` at ~30 `mesh.rs` sites plus `mod.rs`, `proxy.rs` and the `router.rs` `sendmmsg` drain; none compiles under `PeerAddr`, so Stage 1 introduces the seam, and §2's sync `try_send` contract conflicts with UDP's async / deadline / `Transport`-error semantics | `mesh.rs:39386–39391`, `:6797` / `:9317` (`bound_datagram_send`), `router.rs:198`, `:945–1020` | §1 send-path contract: option B, transport-specific submission behind the endpoint; UDP semantics preserved; §2 trait re-scoped to the RTC half; S0d inventory; Stage 1 scope and exit criteria |
| 2 | The mesh PSK is one shared secret; enrollment runs after the Noise session with no responder-side enforcement, so a PSK holder has full session privileges before enrolling | `mesh.rs:1570`; no `enrolled` / `is_admitted` gate on the accept/dispatch path | §5 Layer 0 states the PSK precisely (separate trust domain for public-browser deployments; never ship a private deployment's PSK); Open question 7 (pre-enrollment privileges) added as a pre-Stage-1 decision |
| 3 | `net-wire` omitted the routing-envelope codec, which every routed-session packet uses and the leaf's only pre-direct path needs | `mesh.rs:28369–28427`, `:39565`; `route.rs:182–268` | §7 adds the codec (not the table); §7 leaf dispatcher unwraps envelopes addressed to itself; S0a's minimum proof is a routed round-trip |
| 4 | `str0m` defaults to `aws-lc-rs` + `examples`; provider unchosen; `buffered_amount` is `&mut`-only so any admission-side reading is a stale snapshot; str0m's own p2p caveat | docs.rs 0.23.1 feature and `Channel` docs; README single-mutation invariant | §Dependencies pins `default-features = false, features = ["rust-crypto"]`; §2 reserved-bytes hard bound + advisory snapshot + single-mutation invariant; S0b both roles on all native targets |
| 5 | Ingress: "pushed to dispatch exactly as UDP" vs "seam left untouched" contradict; `dispatch_packet` has one caller | `mesh.rs:24218`, `:24321` | §2 one dispatch owner, bounded RTC input; Related plans corrected |
| 6 | Bootstrap endpoint: browser-trusted TLS, cross-origin policy, WebSocket `Origin` unnamed | — | Stage 4 scope; no cert-ignore flags in CI; gather-complete POST decided on S0b latency evidence |
| 7 | Leader-tab loss: interruption budget, pending-call disposition, restoration, stale-leader fencing unspecified; follower proxy is a real SDK surface | — | §8 leader lifecycle; Stage 5 tests suspension/resumption |
| 8 | Leaf-side announcement ingress unsized | — | Open question 2 extended; filtering is a separate discovery-contract decision |
| 9 | Stage 1 exit below the repository's pre-push bar | `AGENTS.md` | Stage 1 exit references the full matrix |
| 10 | Estimates omit the above | — | Withdrawn; re-derived from Stage 0 outputs |

**Operative boundary unchanged: Stage 0 only.** Before Stage 1: settle
pre-enrollment semantics (OQ 7 — closed later the same day, see below),
approve the UDP-preserving send/ingress contract (S0d), include the routed
codecs (S0a), and pin the RTC dependency configuration (S0b). A bounded
repair to the plan — not another architecture program, and not
authorization to start the production refactor.

**2026-09-11 — Kyra, closure of Open question 7 (checked at
`79ad91283`).** Enrollment-gated participation for the v1 browser-facing
anchor; no intentionally open mode. Verified in this tree:
`ENROLLMENT_SERVICE = "net.mesh.enroll"` (`sdk/src/mesh_enroll.rs:38`);
`Mesh::join` dials the operator with `connect_via` (`:290`) before the
unary `JoinRequest`, so a routing envelope addressed to the anchor itself
is local delivery, not transit. Applied as §12 (provisional sessions,
action-level allow-list, five enforcement points, admission state distinct
from `PeerTransport`, no pre-enrollment relay, session-bound promotion,
eligibility-not-authority, six witnesses), §11 registry row, S0e bootstrap
frame inventory, Stage 4 scope + exit criteria, Critical files. Boundary
unchanged: Stage 0 only.
