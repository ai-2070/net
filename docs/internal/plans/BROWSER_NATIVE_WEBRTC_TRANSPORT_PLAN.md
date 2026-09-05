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

**Draft, revision 2 — not started.** First draft 2026-09-05 against
`net-mesh` 0.36.0 (`master` at `079894c76`); revised the same day after
Kyra's source-checked review (see [Review log](#review-log)). Line references
below will drift.

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

Verified facts about the substrate as of the commit above.

**The transport is UDP by type, not by abstraction.**

- `adapter/net/transport.rs` wraps a tokio `UdpSocket` behind `NetSocket`,
  `PacketSender`, `PacketReceiver` and the Linux `BatchedPacketReceiver`.
  There is no transport trait. `MeshNode::spawn_receive_loop`
  (`mesh.rs:17815`) funnels the per-packet and batched paths through a local
  `IngressReceiver` enum.
- Peer transport state **is already typed**. `PeerTransport`
  (`mesh.rs:2800–2872`) separates *where a packet goes* from *who owns that
  endpoint*:

  ```rust
  enum PeerTransport {
      Direct { owned_addr: SocketAddr },
      Routed { relay_addr: SocketAddr, adjacent_relay_identity: Option<u64> },
  }
  ```

  with `send_addr()` / `owned_addr()` / `is_direct()` accessors and 19 match
  sites. Installation, direct↔routed migration, teardown and stale-session
  protection all hang off it. This is the state to *generalize*, not replace.
- Around it, a peer endpoint is a `SocketAddr` at every keyed site: the
  identity binding map `addr_to_node: DashMap<SocketAddr, u64>`
  (`mesh.rs:1281`), `RouteEntry::next_hop` (`route.rs:416`),
  `NetSession::peer_addr` (`session.rs:67`), `NodeInfo::addr`
  (`swarm.rs:343`), `pending_direct_initiators`, the proxy forwarder, the
  failure detector and reroute. Occurrence counts, production code only:

  | File | `SocketAddr` mentions |
  |---|---|
  | `mesh.rs` | 206 |
  | `route.rs` | 70 |
  | `reroute.rs` | 44 |
  | `router.rs` | 33 |
  | `behavior/proximity.rs` | 29 |
  | `swarm.rs`, `session.rs`, `proxy.rs`, `failure.rs` | 14–20 each |
  | `traversal/*` | ~60 total (stays UDP-only, see §6) |

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
| Protected route-hop | `ROUTE_HOP_MAGIC = 0x5248` (`subnet/route_hop.rs:53`, dispatched `mesh.rs:18214`) | `48 52` |
| Punch keep-alive | `KEEPALIVE_MAGIC = 0x4850` (`traversal/rendezvous.rs:191`) | `50 48` |
| Headerless pingwave | none — 72-byte fixed size, recognised by length and *not* starting with `MAGIC` (`mesh.rs:18014–18022`) | arbitrary (origin id) |

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
| `session.rs` | 3527 | 2 | 7 | `crate::event::StoredEvent`, `subnet::route_hop::SharedHopReplayWindow`, `pool`, `reliability`, `stream` |

But `Cargo.toml:239` pulls tokio unconditionally with `rt-multi-thread`,
`net` and `time`, and `lib.rs` exposes `bus`, `consumer`, `shard` and `ffi`
unconditionally (`bus.rs` alone has 97 tokio call sites). tokio refuses to
build `net` / `rt-multi-thread` on `wasm32-unknown-unknown`. A "wire feature"
on the core is therefore not a clock shim; it is a crate split (§7, Stage 2).

Crypto is Noise NKpsk0 via `snow` (default pure-Rust resolver), ChaCha20-
Poly1305 via `ring` 0.17 (builds for wasm32), Ed25519 / X25519 via the dalek
3.x crates, BLAKE3, `postcard`. `getrandom` 0.4 needs `wasm_js` on wasm.
None of this needs replacing.

**The rest of the mesh is not portable and must not be ported.** `tokio::time`
appears in 39 files under `adapter/net`, `Instant::now` in 88, `std::thread`
spawns in ~20 production files, and `mesh.rs` alone is 43k lines. A browser
node is a smaller node profile (§7), not the core compiled to wasm.

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
`mesh.rs:33114` requires `dest_pubkey`), then calls the enrollment nRPC
service. The Noise static key and the entity key are **different keypairs**;
`NoiseHandshake::initiator_with_prologue` (`crypto.rs:206`) needs the
responder's static key and the PSK before it can build msg1. Nothing in the
substrate distributes Noise static keys on the wire today: capability
announcements carry `node_id`, `entity_id`, capabilities, `reflex_addr` and a
signature, but no Noise key.

**Reachability learning already exists.** Forwarded capability announcements
install a route toward their origin through the sender
(`mesh.rs:26655–26715`: `hop_count > 0` → `add_route_with_metric(origin,
next_hop = sender, hop_count + 2)`), preserving authenticated next-hop
identity when direct adjacency is confirmed. A leaf does not need to
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
- 123 `cfg(target_os = …)` sites under `adapter/net` — platform gating is
  routine here.
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

**Test policy for the refactor.** Changing a Rust parameter from
`SocketAddr` to `PeerAddr` legitimately requires mechanical test edits. The
gate is that every existing assertion and its coverage is preserved; no
witness may be weakened or deleted, and diffs to witness tests are reviewed
line by line.

**Alternative considered:** synthesising fake loopback tuples for RTC peers.
Rejected — it lies to every `is_loopback` / partition / reflex check and
would leak into `reflex_addr`.

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
  payloads that come out are Net packets tagged `PeerAddr::Rtc(id)` and are
  pushed to the mesh dispatch context exactly as UDP packets are.
- **Outbound:** the mesh calls `Transport::try_send(packet, PeerAddr::Rtc(id))`,
  which is a **non-blocking** push onto a bounded per-peer queue
  (`RtcConfig::send_queue_packets`, default 256). A full queue returns
  `WouldBlock`, which the stream layer maps to
  `StreamError::Backpressure` — the same signal a slow receiver produces
  today, so daemon policy (drop / retry / app buffer) applies unchanged.
- **SCTP buffering:** receiver credits bound Net's window, not the browser's
  or str0m's SCTP send buffer, and fire-and-forget streams have no credit at
  all. The driver therefore checks the channel's buffered amount before each
  write and refuses above `RtcConfig::max_buffered_bytes` (default 256 KiB),
  reporting `Backpressure`. Loss happens at the source, never as unbounded
  SCTP growth.
- **Timers:** the driver runs `Rtc::poll_output` to completion after every
  input and arms one timer per instance from the returned `Timeout`.
- **Close / stale work:** closing a channel bumps its `generation`; queued
  packets and timers carrying an old generation are discarded, and the mesh
  is told via the existing peer-removal path so `addr_to_node` and
  `PeerTransport` are torn down in the normal order.

The trait shape is fixed only after Stage 0's spike exercises this loop:

```rust
pub trait Transport: Send + Sync {
    fn try_send(&self, packet: &[u8], to: PeerAddr) -> io::Result<()>; // WouldBlock == Backpressure
    fn kind(&self) -> TransportKind;
}
```

### 3. One Net packet per DataChannel message; unordered, zero-retransmit; Net's reliability stays authoritative

Channels open with `ordered: false, maxRetransmits: 0` on both sides.

- `Reliability::Reliable` streams get NACK-driven retransmission from
  `reliability.rs`, identical to UDP. No retransmission stacking, no
  SCTP head-of-line blocking *from retransmits*.
- `Reliability::FireAndForget` streams are genuinely lossy.
- What remains underneath and is **not** removed: SCTP congestion control
  and send buffering. §2's buffered-amount bound is how the plan keeps those
  from becoming invisible latency.
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
application hands the browser an **invite**, the same artefact
`Mesh::join` devices consume today: `Rendezvous { addr, noise_pubkey,
node_id }` for the anchor, the PSK, and the delegation `root`. For a browser
`addr` is the anchor's bootstrap URL. The browser POSTs an SDP offer to
`https://<anchor>/rtc/offer`, trickles candidates over a short-lived
WebSocket on the same origin, gets an answer, and opens a DataChannel. Over
that channel it runs the Noise initiator against the `noise_pubkey` **from
the invite, not from the HTTP response** — a substituted anchor fails the
handshake. It then calls the existing enrollment nRPC service with a signed
`JoinRequest` and verifies the grant, exactly as a native device does.

*PSK provisioning path, named precisely:* the invite string. Org membership:
the enrollment grant's delegation chain. Subnet bearer secrets are a separate
mechanism and are not used for first contact.

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

**Direct path.** When ICE connects, the direct DataChannel gets its own Noise
handshake using the already-known key, mirroring the punched-path upgrade
(`connect_direct → connect_on_direct_path → connect_via` today). The routed
session is retained as the fallback until the direct one is healthy, then
handled by the existing punched-path retention policy. Whether a session
*migration* (routed→direct without a second handshake) is preferable is an
open question for Stage 3.

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
`RtcConfig::ice_deadline` (default 10 s) with a typed `RtcError::UdpBlocked`
that `@net-mesh/browser` surfaces to the application. ICE-TCP passive
candidates on anchors are the candidate future mechanism (str0m support to
be verified in Stage 0) and are listed under deferred work, not promised.
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
`pool`, `batch`, `stream`, `reliability`, `session` and the wire-level
subprotocol codecs into a tokio-free crate; the core depends on it and
re-exports under the existing paths so no `use net::adapter::net::…` site
changes. Two known couplings to cut in Stage 2: `session.rs` imports
`crate::event::StoredEvent` (move the type or the dependency) and
`subnet::route_hop::SharedHopReplayWindow` (move the window type into
`net-wire`). The `Instant` uses go behind a `Clock` trait (native
`std::time::Instant`; wasm `web_time::Instant`). CI adds
`cargo check -p net-wire --target wasm32-unknown-unknown`.

**`net-leaf` crate** (`crates/net/leaf/`, wasm32, `wasm-bindgen`):

- `RtcLeafTransport` over `web_sys::RtcPeerConnection` / `RtcDataChannel`,
  one channel per peer; same §2 bounded-send and buffered-amount rules.
- Dispatcher for: plain events, channel membership (`0x0A00`), stream
  window (`0x0B00`), capability announcement (`0x0C00`), fold (`0x1000`),
  nRPC wire types, RTC signal (`0x0D02`). Unknown subprotocols dropped with a
  counter.
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

### 9. Browser ↔ browser is the §5 sequence with the mesh as the signalling network

A wants B, discovered by capability query like any native pair:

1. A already holds B's `(node_id, noise_pubkey)` from B's signed
   announcement (§5 Layer 1).
2. A runs `connect_via(anchor, …)` → routed A↔B session (Layer 2).
3. A sends `Offer` on `0x0D02` over that session; `Answer` and `Candidate`s
   flow back the same way; anchors forward blind.
4. ICE connects using the anchors' STUN; a direct DataChannel opens; a Noise
   handshake runs over it; `PeerTransport` for B becomes
   `Direct { owned: PeerAddr::Rtc(id) }`. A leaf has no routing table, so
   this is a session-table update, not a route install.
5. If ICE fails within `ice_deadline`, the routed session stays,
   `ice_relayed += 1`, and ICE is retried only on a browser network-change
   event or the periodic reclassify tick — never per packet.

A native node with `webrtc` on follows the same steps when its peer is a
leaf.

### 10. Stats and the direct-path witness

`RtcStats` on `MeshNode` and on the leaf: `ice_attempted`, `ice_direct`,
`ice_relayed`, `ice_failed`, `udp_blocked`, `stun_served`,
`signal_forwarded`. `ice_direct / ice_attempted` is the deployment
telemetry; it is reported, not gated.

The **witness** that a pair is off the anchor is per-pair: the anchor's
forwarded-packet counter keyed by `(src, dst)` for *application-data*
subprotocols stays flat while the pair exchanges traffic. A globally flat
anchor counter is not the witness — signalling, announcements and unrelated
pairs legitimately keep flowing.

### 11. Registry additions

| Item | Value |
|---|---|
| `SUBPROTOCOL_RTC_SIGNAL` | `0x0D02` |
| Capability tags | `rtc-anchor`, `transport:rtc`, `leaf` |
| Announcement fields (all optional, `reflex_addr` wire-compat pattern) | `noise_pubkey: Option<[u8; 32]>`, `rtc_bootstrap: Option<String>`, `rtc_addr: Option<SocketAddr>` |
| `PairAction` variant | `Ice` |
| Error | `RtcError::{UdpBlocked, IceTimeout, BootstrapRejected, …}` |

`docs/SUBPROTOCOLS.md` and `docs/CAPABILITIES_SCHEMA.md` are updated in the
stage that introduces each.

---

## Stage 0 — Spikes (before any wide refactor)

Three throwaway spikes, in parallel, none touching `mesh.rs`. Their purpose
is to retire the unknowns that the wide refactor would otherwise be built on
top of.

- **S0a — wire boundary.** Copy the seven wire modules into a scratch crate,
  cut the two `session.rs` couplings, shim `Instant`, and get
  `cargo check --target wasm32-unknown-unknown` green. Output: the exact list
  of types that must move for Stage 2, and the wasm size of crypto + wire.
- **S0b — RTC loop.** A scratch native binary owning a UDP socket and a
  str0m `Rtc`, plus a headless Chromium page: DataChannel up, then a Noise
  NKpsk0 handshake and one reliable-stream round-trip over it using the S0a
  crate on both ends. Output: the driver ownership shape (§2) validated, the
  buffered-amount hook confirmed, ICE-TCP support in str0m confirmed or
  denied, `RTCPeerConnection`-in-worker status confirmed.
- **S0c — double-AEAD cost.** In S0b, measure ChaCha20-Poly1305 over DTLS
  at 60 Hz × 1 KiB and at 1 MB/s bulk in the browser. Output: a number in
  `docs/internal/performance/` and a decision on whether the DTLS-exporter
  shortcut leaves "deferred".

### Exit criteria

- All three outputs written up; §2's trait and §7's type list finalized
  from them.
- No repository code outside `docs/` and a `spikes/` scratch directory
  changed.

## Stage 1 — `PeerAddr` endpoint generalization (UDP-only, behaviour-neutral)

Generalize `PeerTransport` and every peer-keyed site listed in §Context to
`PeerAddr`. Only the `Udp` variant exists; no feature flag yet. Traversal
modules keep `SocketAddr` at their edges.

### Exit criteria

- Zero wire change: `cross_lang_*` golden tests pass unmodified.
- Every existing integration and witness test passes; mechanical signature
  edits allowed, assertion and coverage preserved, witness diffs reviewed
  line by line.
- `cargo clippy --lib --bins -- -D warnings` clean.
- Default build: exported C-ABI symbol set unchanged (symbol diff in CI).

## Stage 2 — `net-wire` crate + `Clock`

Extract per §7 using S0a's type list; re-export under existing paths; add
the wasm32 check and a `cross_lang_wire` golden fixture set (header,
`EventFrame`, `NackPayload`, `StreamWindow`, announcement with and without
the new optional fields).

### Exit criteria

- `cargo check -p net-wire --target wasm32-unknown-unknown` in CI.
- `cargo test -p net-wire` runs the crypto, protocol, reliability and
  session unit tests natively.
- No behaviour change under default features; no `use` path in the core or
  any binding changes.

## Stage 3 — Native `webrtc` feature: driver, dedicated socket, STUN, loopback harness

- `adapter/net/rtc/{mod,driver,transport,stun,config}.rs`; `PeerAddr::Rtc`;
  `RtcConfig { bind_addr, public_addr, ice_deadline, max_peers,
  send_queue_packets, max_buffered_bytes, serve_stun, serve_bootstrap }`.
- **Harness:** two native nodes on loopback where B's session to A is forced
  onto a DataChannel via a test-only `connect_rtc_loopback`. Run the
  *existing* stream, reliability, backpressure, nRPC, fold and
  routing-plane witness files against it.

### Exit criteria

- Those files pass with the peer on `PeerAddr::Rtc`, including the
  stale-session and direct/routed migration witnesses.
- Full-queue and over-buffer conditions surface as `Backpressure`, never as
  blocking or unbounded growth (test with an injected slow DataChannel).
- External `stunclient` gets a correct XOR-MAPPED-ADDRESS from the RTC
  socket; the Net socket's ingress tests are byte-for-byte unchanged with
  `webrtc` on.
- Default build unchanged; `--features webrtc` clean under `-D warnings`.

## Stage 4 — Announcement fields, `0x0D02`, bootstrap, enrollment reuse

- `noise_pubkey` / `rtc_bootstrap` / `rtc_addr` on the announcement (emission
  off by default, OA-1 migration pattern); `PairAction::Ice`.
- `0x0D02` codec, dispatch, forwarding, per-sender budget.
- Bootstrap HTTPS/WebSocket listener (`axum` + the payments crate's pinned
  rustls), feature-gated; invite decoding for the RTC `Rendezvous` form;
  `Mesh::join` over a DataChannel session.
- `net-mesh anchor` CLI subcommand; Deck surfaces anchors and their
  `rtc_addr`.

### Exit criteria

- A native node with `webrtc` on completes invite → offer/answer →
  DataChannel → Noise (against the invite's key) → enrollment grant with a
  scripted Chromium client.
- A wrong `noise_pubkey` in the HTTP answer (MITM simulation) fails the
  handshake; the invite's key is the only one honoured.
- Signalling between two nodes sharing no direct session is delivered via
  an intermediate anchor; a test asserts the anchor never decodes `0x0D02`.
- Bootstrap budget rejections are typed and fast.
- An anchor behind a simulated NAT publishes a working `rtc_addr` via
  `public_addr` or port mapping.

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
  must not leak `PeerAddr::Rtc` or any anchor-specific type.
- `cross_lang_wire` replay inside the wasm test runner.
- Playwright CI: Chromium + Firefox against a native anchor — handshake,
  reliable round-trip, fire-and-forget loss under injected DataChannel loss,
  nRPC to a native service, a native peer's `find_best_node` returning the
  browser node, two tabs sharing one identity without evicting each other,
  `RtcError::UdpBlocked` surfaced promptly under a UDP-blocked profile.

### Exit criteria

- All scenarios green on Chromium and Firefox; Safari best-effort, recorded.
- Bundle and wasm sizes recorded; wasm ≤ 1.5 MB gzipped target.
- A mock `ControlPlane` (in-memory, no anchor) drives the leaf through
  handshake and one direct browser ↔ browser session in the wasm test
  runner, proving the trait boundary is complete.

## Stage 6 — Browser ↔ browser direct, NAT conformance, telemetry, demo

- §9 end to end; `RtcStats`; browser network-change retry trigger; per-pair
  witness counter on anchors.
- **Deterministic conformance:** extend the NAT simulator harness from
  [`NAT_TRAVERSAL_V2_PLAN.md`](NAT_TRAVERSAL_V2_PLAN.md) Stage 4 with two
  headless browsers behind simulated cone / port-restricted / symmetric NATs
  and one anchor.
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

- `adapter/net/mesh.rs` — `PeerTransport` (`:2800`), `addr_to_node`,
  `pending_direct_initiators`, dispatch context, `spawn_receive_loop`,
  `connect*`, reroute call sites, announcement route-learning (`:26655`).
- `adapter/net/transport.rs` — `PeerAddr`, `Transport`.
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

### Stages 5–6 (leaf, P2P)

- `crates/net/leaf/`, `crates/net/sdk-browser/`.
- `tests/cross_lang_wire/`, `tests/rtc_*.rs`, Playwright suite under
  `crates/net/leaf/e2e/`, NAT simulator extension.

---

## Open questions

1. **Anchor placement policy.** Every `webrtc`-enabled native node, or only
   operator-marked ones? Proposal: `serve_bootstrap` opt-in; STUN automatic
   for any node with a published `rtc_addr`. Decide before Stage 4.
2. **Leaf announcement TTL.** A closed tab cannot withdraw. Proposal: 60 s
   TTL with 20 s re-announce for `leaf`-tagged entries; check against the
   announce rate limit.
3. **Anchors per leaf.** One anchor is a single point of failure for
   signalling and fallback. Proposal: bootstrap to one, learn others from
   `rtc-anchor` announcements, hold `min(3, n)` sessions. Affects Stage 5.
4. **Direct-path session: fresh handshake or migration?** §5 mirrors the
   punched-path upgrade (fresh handshake). A routed→direct *migration* of
   the same session would save a round-trip and a key; decide in Stage 3
   once the harness exists.
5. **Identity lifetime for browsers.** Same-origin XSS owns the identity
   (§8). Whether leaf identities need shorter certificate lifetimes under
   the org revocation floors is an org-auth question, tracked against OA-4.
6. **Safari.** `maxRetransmits: 0` semantics and ICE restart differ; Stage 5
   records behaviour and Stage 6 decides whether it gates.

---

## Rough estimates

| Stage | Surface | Complexity | Estimate |
|---|---|---|---|
| 0 | Three spikes | Small each, parallel | ~4–5 days |
| 1 | `PeerAddr` generalization | Large, mechanical, security-sensitive | ~5–7 days |
| 2 | `net-wire` crate + `Clock` | Medium (crate split, two couplings) | ~3–4 days |
| 3 | Driver, socket, STUN, harness | Medium–large | ~5–6 days |
| 4 | Fields, `0x0D02`, bootstrap, enrollment | Medium | ~4–5 days |
| 5 | Leaf + browser SDK + Playwright | Large | ~7–9 days |
| 6 | P2P, conformance matrix, telemetry, demo | Medium–large | ~5–6 days |

Total: ~33–42 days serial. Stage 0 gates the trait and type decisions;
Stages 1 and 2 can run in parallel after it; Stage 5 can start against the
Stage 3 harness before Stage 4 completes.

---

## Dependencies

- `str0m` 0.23.1 — sans-IO WebRTC. Native, feature `webrtc` only. MSRV
  1.85.0 (toolchain is 1.98.0), MIT OR Apache-2.0. ICE-TCP support to be
  confirmed in S0b.
- `web-time` — `Instant` on wasm, `net-wire` on wasm32 only.
- `getrandom` `wasm_js` — wasm32 only.
- `wasm-bindgen`, `web-sys` (`RtcPeerConnection`, `RtcDataChannel`,
  `IndexedDb`, `SubtleCrypto`, Web Locks, `BroadcastChannel`),
  `wasm-bindgen-futures` — leaf only.
- `axum` + the payments crate's pinned `rustls` — bootstrap endpoint,
  feature `webrtc` only.
- Playwright — CI dev dependency.

No new dependency reaches the default build.

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
| Enrollment / admission (§5 Layer 0) | `net-wire` compiled to wasm inside the worker runs the enrollment handler; the delegation root lives in the platform secret store | medium–large |

**Why it is additive.** `net-wire` already targets wasm, so it runs inside
Workers as well as browsers. Signed announcements need no trusted store.
`PeerAddr` gains a `Ws` variant used only by the leaf's control plane and
the relay fallback. The invite / enrollment flow is unchanged. The framing
"anchors are how browsers *find* each other" holds exactly — only the
finder moves.

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
  `IngressReceiver` seam, which this plan now leaves untouched.
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
