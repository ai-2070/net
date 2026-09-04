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
> (in-browser three.js multiplayer, collaborative editing), a direct path is a
> **product requirement** — a server hop on every position update is the thing
> we are refusing to ship. The relay stays as the correctness fallback exactly
> as before, but the plan is measured on the fraction of sessions that land
> direct, not on whether the relay works. Docstrings and READMEs written under
> this plan must carry that framing: "anchors are how browsers *find* each
> other, not how they *talk* to each other."

## Status

**Draft — not started.** Written 2026-09-05 against `net-mesh` 0.36.0
(`master` at `079894c76`). Line references below will drift.

Decision already taken by the product owner: WebRTC is the primary browser
transport. WebSocket-to-anchor and WebTransport were considered and rejected
as primaries because both are client-to-server by construction and cannot
remove the server hop. Neither is part of this plan.

## Context

Verified facts about the substrate as of the commit above.

**The transport is UDP by type, not by abstraction.**

- `adapter/net/transport.rs` wraps a tokio `UdpSocket` behind `NetSocket`,
  `PacketSender`, `PacketReceiver` and the Linux `BatchedPacketReceiver`.
  There is no transport trait. `MeshNode::spawn_receive_loop`
  (`mesh.rs:17815`) already funnels the per-packet and batched paths through a
  local `IngressReceiver` enum, which is the natural seam for a second
  backend.
- A peer *is* a `SocketAddr` everywhere that matters: the identity binding
  map `addr_to_node: DashMap<SocketAddr, u64>` (`mesh.rs:1281`),
  `RouteEntry::next_hop: SocketAddr` (`route.rs:416`),
  `NetSession::peer_addr` (`session.rs:67`), `NodeInfo::addr`
  (`swarm.rs:343`), the proxy forwarder, the failure detector and the reroute
  logic. Occurrence counts, production code only:

  | File | `SocketAddr` mentions |
  |---|---|
  | `mesh.rs` | 206 |
  | `route.rs` | 70 |
  | `reroute.rs` | 44 |
  | `router.rs` | 33 |
  | `behavior/proximity.rs` | 29 |
  | `swarm.rs`, `session.rs`, `proxy.rs`, `failure.rs` | 14–20 each |
  | `traversal/*` | ~60 total (stays UDP-only, see §6) |

- The wire never carries a `SocketAddr` except inside the traversal
  subprotocols (`ReflexMsg`, `RendezvousMsg`). Routing headers carry node
  ids. `Pingwave` (`swarm.rs:33`) carries origin id, seq, ttl and hop count
  only. This is what makes a peer-address refactor a *local* change rather
  than a wire change.

**The wire layer is already portable.** The modules a browser node needs
have no socket or runtime coupling:

| Module | Lines | tokio calls | `Instant` calls |
|---|---|---|---|
| `protocol.rs` | 955 | 0 | 0 |
| `crypto.rs` | 2092 | 0 | 0 |
| `pool.rs` | 1841 | 0 | 0 |
| `batch.rs` | 394 | 0 | 0 |
| `stream.rs` | 268 | 0 | 0 |
| `reliability.rs` | 2529 | 0 | 7 |
| `session.rs` | 3527 | 2 | 7 |

Crypto is Noise NKpsk0 via `snow` (default pure-Rust resolver), ChaCha20-
Poly1305 via `ring` 0.17 (compiles for `wasm32-unknown-unknown`), Ed25519 /
X25519 via the dalek 3.x crates, BLAKE3, `postcard`. `getrandom` 0.4 needs the
`wasm_js` feature on the wasm target. None of this needs replacing.

**The rest of the mesh is not portable and must not be ported.** `tokio::time`
appears in 39 files under `adapter/net`, `Instant::now` in 88, `std::thread`
spawns in ~20 production files, and `mesh.rs` alone is 43k lines. tokio's time
driver does not exist on `wasm32-unknown-unknown`. A browser node therefore
cannot be "the core compiled to wasm"; it has to be a smaller node profile
(§7).

**Packet geometry fits DataChannels.** `MAX_PACKET_SIZE = 8192`
(`protocol.rs:33`) sits under the 16 KiB DataChannel message size every
browser guarantees. Net's fragmentation, stream windows and NACK reliability
need no changes to ride a DataChannel.

**The NAT-traversal module already is the mesh-native STUN/TURN.** Reflex
probing (`0x0D00`), rendezvous punching (`0x0D01`), the `NatClass` /
`PairAction` matrix and relay fallback all exist and are measured. For native
↔ native pairs WebRTC adds nothing. Its value is exclusively browser reach,
and this plan keeps the two traversal mechanisms disjoint: ICE for any pair
with a browser in it, the existing punch for native ↔ native.

**Nothing browser-facing exists today.** `sdk-ts` is a wrapper over the
`@net-mesh/core` napi binding (Node ≥ 20). `web/` is a Next.js site, not a
client. Every `wasm-bindgen` / `web-sys` entry in `Cargo.lock` is transitive.

**Precedent to lean on.**

- `docs/SUBPROTOCOLS.md` — `0x0D02` is the next free id in the traversal
  block.
- `tests/cross_lang_*` — golden-vector discipline for wire types shared across
  language tiers. The leaf crate joins that matrix.
- 123 `cfg(target_os = …)` sites under `adapter/net` — platform gating is
  routine here; target-arch gating for wasm follows the same style.
- The MCP adapter doctrine ("adapters attach, nodes participate",
  `adapters/mcp/src/lib.rs`). A browser under this plan *participates*.

## Goals

- A browser tab runs a Net node with a stable, self-held identity
  (`EntityKeypair` + Noise static key) and its own node id, sessions, streams,
  channel subscriptions, nRPC client and signed capability announcements.
- Browser ↔ browser and browser ↔ native sessions ride a WebRTC DataChannel
  directly whenever ICE can connect the pair. Measured target: ≥ 90 % of
  sessions direct on the mixed residential-NAT test matrix (§Stage 5), with
  the remainder relayed through an anchor and *counted*.
- No third-party STUN, TURN or signalling infrastructure. Anchors answer STUN
  on the Net UDP socket; the mesh is the signalling network after the first
  session; the mesh relay is the TURN.
- Wire parity: a browser node speaks the exact Net wire — 68-byte header,
  Noise NKpsk0, ChaCha20-Poly1305, the same subprotocol ids — so native peers
  cannot tell a browser session from a UDP one above the transport layer.
- The native `webrtc` feature is off by default and the default cdylib is
  byte-identical without it, matching the `nat-traversal` / `port-mapping`
  precedent.
- Node, Python and Go bindings gain the anchor role behind the same feature
  flag; a new `@net-mesh/browser` package ships the wasm leaf.

## Non-goals

- **Porting `MeshNode` to wasm.** The browser node is a leaf profile (§7). It
  never forwards, never runs the failure detector, never originates
  pingwaves, never holds a routing table.
- **WebTransport, WebSocket data paths.** Both are client-to-server. A
  WebSocket appears in exactly one place — the anchor bootstrap endpoint
  (§5) — and carries at most the first SDP exchange, never a Net packet.
- **Media.** No SRTP, no audio/video tracks. DataChannels only.
- **A TURN protocol server.** The mesh relay is the fallback; we do not
  implement RFC 5766.
- **Native ↔ native WebRTC.** UDP + the existing punch remains the native
  path. The str0m backend accepts browsers; it is never chosen between two
  native nodes.
- **Dropping Noise in favour of DTLS.** See §4.
- **Mobile native (iOS / Android) clients.** Those are native nodes with UDP;
  nothing here applies.
- **Browser-side persistence beyond identity.** RedEX / Dataforts on
  IndexedDB is a separate plan.

---

## Design decisions

### 1. `PeerAddr` replaces `SocketAddr` as the peer key; identity binding stays address-independent

Introduce in `adapter/net/transport.rs`:

```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum PeerAddr {
    /// A UDP tuple — the only variant in default builds.
    Udp(SocketAddr),
    /// A DataChannel owned by the local `RtcTransport`. The id is local,
    /// never on the wire, unique for the life of the process.
    #[cfg(feature = "webrtc")]
    Rtc(RtcPeerId),
}
```

`addr_to_node`, `RouteEntry::next_hop`, `NetSession::peer_addr`,
`NodeInfo::addr`, `pending_direct_initiators`, the proxy forwarder and the
failure detector key on `PeerAddr`. `MeshNodeConfig::bind_addr` /
`peer_addr` stay `SocketAddr` — they describe the UDP socket, not a peer.

The identity-binding invariants the routing-plane witnesses in `AGENTS.md`
protect (a reused NAT tuple must never inherit a node's binding; the
withdrawing hop is the resolved sender, never a wire field) are **strictly
stronger** under `PeerAddr::Rtc`: an `RtcPeerId` is minted per DataChannel and
never reused, so the address-reuse race the UDP path defends against cannot
occur on the RTC path. The refactor must not weaken the UDP defence to get
there — Stage 0 lands with zero behaviour change.

**Alternative considered:** keep `SocketAddr` and synthesise fake loopback
tuples for RTC peers. Rejected — it lies to every `is_loopback` / partition /
reflex check, and the fake tuples would leak into `reflex_addr` on capability
announcements.

### 2. One transport trait, sans-IO backend, feature `webrtc`, crate `str0m`

```rust
pub trait Transport: Send + Sync {
    fn send_to(&self, packet: &[u8], to: PeerAddr) -> io::Result<()>;
    fn local_addr(&self) -> Option<SocketAddr>;   // UDP only
    fn kind(&self) -> TransportKind;              // Udp | Rtc
}
```

The UDP backend is the existing `NetSocket`. The RTC backend is `RtcTransport`
built on **`str0m`** (sans-IO, single-threaded, no tokio dependency, MIT licensed).
The mesh feeds it inbound UDP datagrams (§6) and drains its outbound datagrams
back through the same socket; DataChannel payloads it produces enter the
dispatch loop as Net packets tagged `PeerAddr::Rtc(id)`.

**Why not `webrtc-rs`.** It is tokio-native (own tasks, own locks per
connection), its dependency tree is large, and it would be the heaviest
crate in the build. The `Cargo.toml` commentary already refuses a 500 KiB
`regex` and a 1.4 MB `igd-next` by default; `webrtc-rs` is an order of
magnitude past that. str0m fits the loop-owned, zero-allocation doctrine the
transport already follows and keeps the socket under `NetSocket`'s control.

**Gating.** Cargo feature `webrtc = ["nat-traversal", "dep:str0m"]`. Depends
on `nat-traversal` because the anchor's reflex classification decides whether
it can serve STUN (§6) and because relay-fallback accounting reuses
`TraversalStats`. Off by default; `MeshNodeConfig::rtc: Option<RtcConfig>`
(present only under the feature) enables it at runtime.

### 3. One Net packet per DataChannel message; the channel is unordered and unreliable; Net's reliability stays authoritative

DataChannels are opened with `ordered: false, maxRetransmits: 0` on both
sides. Consequences:

- `Reliability::Reliable` streams get NACK-driven retransmission from
  `reliability.rs`, identical to UDP. No double-retransmit, no SCTP
  head-of-line blocking.
- `Reliability::FireAndForget` streams are genuinely lossy — which is the
  whole reason a game wants WebRTC.
- Stream-window backpressure (`0x0B00`) works unchanged. SCTP's own congestion
  control sits underneath and is not exposed; a browser stream that hits it
  sees the same `StreamError::Backpressure` signal because its credit stops
  being replenished.
- The 8192-byte packet cap stays. No fragmentation changes.

A single DataChannel per peer pair carries every stream, exactly as one UDP
5-tuple does today. Multiple DataChannels per pair are not used; stream
multiplexing is Net's job.

### 4. Noise stays on top of DTLS

A DataChannel is already DTLS-encrypted. We keep the Noise NKpsk0 handshake
and ChaCha20-Poly1305 payload encryption on top of it anyway, because:

- **Identity.** The mesh identity is the Noise static key bound to the
  `EntityKeypair`, not a DTLS certificate fingerprint. Every gate above the
  transport (`addr_to_node`, org admission, subnet auth, capability
  signatures) assumes it.
- **PSK.** NKpsk0's pre-shared key is the admission secret. DTLS has no slot
  for it.
- **Relay blindness.** A relayed session must stay opaque to the anchor
  forwarding it. DTLS terminates *at* the anchor; Noise terminates at the
  peer.
- **Wire parity.** A browser session decrypts with the same `PacketCipher` a
  UDP session does. One code path, one test matrix.

The cost is a second AEAD pass per packet in the browser (ChaCha20 in wasm
runs at hundreds of MB/s; a 60 Hz game sends kilobytes per second). Stage 4
measures it; the DTLS-exporter shortcut (derive Noise keys from the DTLS
session, skip the second AEAD) is explicitly **deferred** unless that
measurement says otherwise.

### 5. Signalling is a mesh subprotocol; the bootstrap endpoint carries one SDP exchange and nothing else

**`SUBPROTOCOL_RTC_SIGNAL = 0x0D02`.** Postcard-encoded, session-authenticated,
routed like any subprotocol packet:

```rust
pub enum RtcSignalMsg {
    Offer     { dialog: u64, target: u64, sdp: String, static_pubkey: [u8; 32] },
    Answer    { dialog: u64, target: u64, sdp: String, static_pubkey: [u8; 32] },
    Candidate { dialog: u64, target: u64, candidate: String, mid: String },
    Reject    { dialog: u64, target: u64, reason: RtcRejectReason },
}
```

`target` is the destination node id; the origin is the resolved session
sender, never a wire field (same rule as `RouteWithdrawal`). Anchors forward
these as ordinary routed packets — they are Noise-encrypted end to end, so an
anchor relaying signalling between two browsers cannot read the SDP. SDP is
carried as an opaque string; nothing in the mesh parses it. `dialog` correlates
the three-way exchange and is minted by the offerer.

`static_pubkey` rides in the offer/answer because NKpsk0 needs the responder's
static key before the first Noise message, and a browser that has never seen
its peer has no other source for it. The value is authenticated by the Noise
session the signalling rode on, so a forged key is only as good as a forged
session.

**Bootstrap.** A tab with zero sessions cannot use `0x0D02`. Anchors therefore
expose a minimal HTTPS endpoint (`POST /rtc/offer` → answer, plus the
anchor's node id and static pubkey; ICE candidates trickle over a short-lived
WebSocket on the same origin). It exists only to get the *first* DataChannel
up, to the anchor itself. From then on the browser is a session peer and
every further signal — including to other browsers — rides `0x0D02`. The
endpoint carries no Net packet, ever, and is rate-limited per source IP with
the same budget shape Finding 5 in
[`NAT_TRAVERSAL_V2_PLAN.md`](NAT_TRAVERSAL_V2_PLAN.md) prescribes for
rendezvous.

Anchors advertise themselves with the capability tag `rtc-anchor` and the
bootstrap URL in a `rtc_bootstrap` field on the capability announcement,
mirroring how `reflex_addr` rides today. Deck shows them.

### 6. Anchors answer STUN on the Net socket via RFC 7983 first-byte demux; the mesh relay is the TURN

Browsers need at least one STUN server to learn their reflexive address, and
the plan forbids third-party ones. RFC 5389 *binding request / response only*
is ~200 lines and needs no state. Anchors with `NatClass::Open` (natively or
via `reflex_override` from port mapping) serve it on the **same UDP socket**
as Net traffic.

Demux is the RFC 7983 rule, made exact by Net's magic byte:

| First byte | Protocol | Handler |
|---|---|---|
| `0x00..=0x03` | STUN | `RtcTransport` (str0m ICE) |
| `0x14..=0x3F` | DTLS | `RtcTransport` (str0m) |
| `0x4E` (`'N'`) | Net | existing dispatch |
| anything else | drop, count `demux_unknown` |

The check is one byte compare before the existing header parse, in the
receive loop, compiled only under `webrtc`. The Linux `recvmmsg` batched path
applies the same predicate per packet. Net's magic `0x4E45` sits in the range
RFC 7983 leaves unassigned, so the three cannot collide.

Browsers receive the anchor's `ip:port` as their single `iceServers` entry
via the bootstrap response. Native nodes on the RTC path do the same through
`0x0D02` when the browser peer asks.

**TURN.** Not implemented. When ICE fails for a pair (symmetric × symmetric,
UDP-blocked networks), the browser already holds a DataChannel to at least
one anchor, and the anchor forwards routed packets for it — the exact relay
path NATed native peers use today. The session is Noise end-to-end, the
anchor sees headers only, and `TraversalStats.relay_fallbacks` is bumped so
the direct ratio is observable. The pair-type matrix short-circuits to
`PairAction::Ice` whenever either side is `transport:rtc`; the existing punch
logic never runs for such pairs.

### 7. The browser node is a leaf profile: `net-leaf` crate, `wire` feature on the core

**`wire` feature on `net-mesh`.** Exposes `protocol`, `crypto`, `pool`,
`batch`, `stream`, `reliability`, `session`, `identity` and the subprotocol
codecs (capability announcement, stream window, fold, nRPC wire types) with
**no tokio, no socket2, no libc**. The two `tokio` uses in `session.rs` and
the `Instant` uses in `reliability.rs` / `session.rs` go behind a
`Clock` trait (native: `std::time::Instant`; wasm: `web_time::Instant`). CI
adds `cargo check --target wasm32-unknown-unknown --no-default-features
--features wire` so the boundary cannot silently regress.

**`net-leaf` crate** (`crates/net/leaf/`, `wasm32-unknown-unknown`,
`wasm-bindgen`). Contains:

- `RtcLeafTransport` over `web_sys::RtcPeerConnection` /
  `RtcDataChannel`, one per peer.
- A dispatcher for the subprotocols a client needs: plain events, channel
  membership (`0x0A00`), stream window (`0x0B00`), capability announcement
  (`0x0C00`), fold (`0x1000`), nRPC wire types, RTC signal (`0x0D02`). Unknown
  subprotocols are dropped with a counter — a leaf never forwards.
- Session table keyed by node id; Noise handshakes; per-stream reliability.
- Identity storage (§8).

Contains **no** routing table, proxy forwarder, pingwave origination, failure
detector, swarm, traversal classifier or fair scheduler (local outbound
already bypasses the scheduler on native, per `TRANSPORT.md`).

**Reachability.** A leaf signs its own capability announcement with its
`EntityKeypair` and sends it to each anchor it holds a session with, tagged
`transport:rtc` and `leaf`. Anchors flood it exactly as they flood any peer's
announcement; receivers install a route with `next_hop = anchor` through the
existing pingwave-installed-route mechanism (metric `hop + 2`), so nothing new
is needed for "how do I reach a browser". A leaf sets TTL 0 on anything it
emits, so no forwarder ever expects it to relay.

**Alternative considered:** a TypeScript reimplementation of the wire layer
instead of wasm. Rejected — two Noise/ChaCha implementations means two
implementations to keep byte-identical forever; the crate already refuses
that for the transfer wire types (golden vectors). The wasm leaf *is* the core
code.

### 8. Browser identity is self-held, persisted in IndexedDB, never extractable in the clear

On first run the leaf generates an `EntityKeypair` (Ed25519) and a Noise
`StaticKeypair` (X25519) in wasm, using `getrandom` with `wasm_js`. Both are
stored in IndexedDB wrapped by a non-extractable WebCrypto AES-GCM key, so the
raw scalars never sit at rest in the clear and never cross into JS. The
node id is therefore stable across reloads and tabs on the same origin. A
host application may instead inject a keypair (custodial model) — same API
as `MeshNodeConfig::entity_keypair` on native.

The PSK is provisioned by the application through the existing admission
paths (org admission proofs, subnet bearer secrets, invite token exchanged
over the bootstrap HTTPS). The plan adds no new secret-distribution scheme.

### 9. Browser ↔ browser is the same machinery as browser ↔ anchor, with the mesh as the signalling network

A wants B (discovered via a capability query the same way native nodes
discover each other):

1. A sends `Offer` on `0x0D02` addressed to B. It rides A's session to an
   anchor, which routes it toward B like any packet (possibly via a second
   anchor).
2. B answers on `0x0D02`; candidates trickle both ways on `0x0D02`.
3. ICE connects the pair directly using the anchors' STUN. A new DataChannel
   opens; A runs the Noise initiator over it with B's `static_pubkey` from
   the answer; a session forms; `addr_to_node` binds `PeerAddr::Rtc(id) →
   B`.
4. Traffic to B now goes on the direct channel. The anchor is no longer on
   the path and installs nothing — routes for B on A's side are *local
   sessions*, not routing-table entries, because leaves have no routing
   table.
5. If ICE fails within `RtcConfig::ice_deadline` (default 10 s), A keeps
   addressing B through the anchor, `relay_fallbacks += 1`, and retries ICE
   on the next `reclassify`-style trigger (network change event in the
   browser, or a periodic tick), never per packet.

Nothing in steps 1–5 is browser-specific; a native node with `webrtc` on
follows the same sequence when its peer is a leaf.

### 10. Stats are decision / action / outcome, and the direct ratio is the headline number

`RtcStats` on `MeshNode` and on the leaf: `ice_attempted`, `ice_direct`,
`ice_relayed`, `ice_failed`, `stun_served`, `signal_forwarded`,
`demux_unknown`. `ice_direct / ice_attempted` is the metric this plan is
judged on. Deck gets a column; the CLI gets `net-mesh rtc stats`.

### 11. Subprotocol and tag registry

| Item | Value |
|---|---|
| `SUBPROTOCOL_RTC_SIGNAL` | `0x0D02` |
| Capability tags | `rtc-anchor`, `transport:rtc`, `leaf` |
| Announcement field | `rtc_bootstrap: Option<String>` (URL) |
| `PairAction` variant | `Ice` |

`docs/SUBPROTOCOLS.md` and `docs/CAPABILITIES_SCHEMA.md` are updated in the
stage that introduces each.

---

## Stage 0 — `PeerAddr` refactor (UDP-only, behaviour-neutral)

Replace `SocketAddr` with `PeerAddr` at every peer-keyed site listed in
§Context. Only the `Udp` variant exists; no feature flag yet. The
`traversal/*` modules keep `SocketAddr` at their edges and convert at the
call boundary (they are UDP by nature).

Wide, mechanical, and the riskiest stage because it touches the identity-
binding and reroute paths the routing-plane witnesses protect.

### Exit criteria

- Zero wire change: `cross_lang_*` golden tests and every existing
  integration test pass unmodified.
- `cargo clippy --lib --bins -- -D warnings` clean.
- The default-feature cdylib has the same exported symbol set as before
  (symbol diff in CI, matching the `nat-traversal` "identical cdylib" rule).
- Every routing-plane witness test in `AGENTS.md` passes; no witness is
  edited.

## Stage 1 — `wire` feature + wasm32 boundary

Add the `wire` feature, the `Clock` trait, and the cfg gating in
`session.rs` / `reliability.rs`. Add the wasm32 `cargo check` to CI. Extend
the golden-vector suite with a `cross_lang_wire` fixture set (header,
`EventFrame`, `NackPayload`, `StreamWindow`, capability announcement) that the
leaf will replay in Stage 4.

### Exit criteria

- `cargo check --target wasm32-unknown-unknown --no-default-features
  --features wire` passes in CI.
- `cargo test --no-default-features --features wire` runs the crypto,
  protocol, reliability and session unit tests on native.
- No behaviour change under default features.

## Stage 2 — Native `webrtc` feature: str0m backend, demux, STUN

- `adapter/net/rtc/{mod,transport,stun,demux,config}.rs`.
- `RtcTransport` over str0m; `PeerAddr::Rtc`; the first-byte demux in
  `spawn_receive_loop` and the batched path.
- RFC 5389 binding responder.
- `MeshNodeConfig::rtc` + `RtcConfig { ice_deadline, max_peers,
  serve_stun, bootstrap_listen }`.
- **Test harness:** two native nodes on loopback where node B's session to A
  is forced onto a DataChannel (a test-only `connect_rtc_loopback`). This
  proves the transport under the *existing* stream / reliability / backpressure
  / nRPC integration tests before a single browser line exists.

### Exit criteria

- The full stream, reliability, backpressure, nRPC and fold test files pass
  with the peer forced onto `PeerAddr::Rtc`.
- A STUN binding request from an external client (e.g. `stunclient`) gets a
  correct XOR-MAPPED-ADDRESS from the Net socket while Net traffic continues
  on it.
- Default build unchanged; `--features webrtc` clean under
  `-D warnings`.

## Stage 3 — Signalling subprotocol + anchor bootstrap

- `SUBPROTOCOL_RTC_SIGNAL = 0x0D02` codec + dispatch, forwarding rules,
  per-sender budget.
- Bootstrap HTTPS/WebSocket listener on the anchor (`axum` or `hyper`,
  feature-gated behind `webrtc`; the payments crate's rustls stack is the
  TLS precedent).
- `rtc-anchor` tag + `rtc_bootstrap` announcement field.
- `net-mesh anchor` CLI subcommand; Deck surfaces anchors.

### Exit criteria

- A native node with `webrtc` on can complete offer → answer → candidates →
  DataChannel → Noise session with a scripted external WebRTC client
  (Stage 2 harness extended with a headless Chromium via Playwright).
- Signalling between two nodes that share no direct session is delivered
  through an intermediate anchor, Noise-opaque to it (test asserts the
  anchor's dispatch never decodes `0x0D02`).
- Bootstrap budget rejections are typed and fast (no 10 s silent timeout).

## Stage 4 — `net-leaf` crate + `@net-mesh/browser`

- `crates/net/leaf/` (wasm-bindgen), `crates/net/sdk-browser/` (TypeScript
  wrapper published as `@net-mesh/browser`).
- Identity storage (§8), `RtcLeafTransport`, dispatcher, session table,
  channel publish/subscribe, nRPC client (`call_typed`), fold announce,
  capability query.
- Golden-vector replay of `cross_lang_wire` inside the wasm test runner.
- Playwright CI job: Chromium + Firefox tab against a native anchor —
  handshake, reliable stream round-trip, fire-and-forget loss under an
  injected-loss DataChannel, nRPC call to a native service, capability
  query that finds the browser node from a native peer.
- Measure §4's double-AEAD cost at 60 Hz × 1 KiB and at 1 MB/s bulk; record
  in `docs/internal/performance/`.

### Exit criteria

- All Playwright scenarios green on Chromium and Firefox; Safari best-effort,
  recorded.
- `@net-mesh/browser` bundle size and wasm size recorded; wasm ≤ 1.5 MB
  gzipped is the target (crypto + postcard + wire, no mesh).
- A native node's `find_best_node` returns the browser node for a capability
  only the browser announced.

## Stage 5 — Browser ↔ browser direct + relay fallback + stats

- The §9 sequence end to end; `PairAction::Ice`; `RtcStats`; the ICE retry
  trigger on browser network-change events.
- **NAT matrix test:** extend the simulator harness from
  [`NAT_TRAVERSAL_V2_PLAN.md`](NAT_TRAVERSAL_V2_PLAN.md) Stage 4 with two
  headless browsers behind simulated cone / port-restricted / symmetric NATs
  and one anchor. Assert direct for every pair the matrix says ICE can
  connect, relayed for symmetric × symmetric, and that `ice_direct +
  ice_relayed + ice_failed == ice_attempted`.
- Deck column, `net-mesh rtc stats`.

### Exit criteria

- ≥ 90 % direct on the matrix rows ICE is expected to solve; 100 % of
  sessions established (direct or relayed).
- A 60 Hz position-update demo (three.js, two tabs) shows no anchor traffic
  on the data path once direct — asserted by the anchor's forwarded-packet
  counter staying flat.

## Stage 6 — Surface completion (**DEFERRED**)

- Node / Python / Go anchor-role parity for `RtcConfig` + `RtcStats`.
- `sdk-ts` type sharing with `@net-mesh/browser` (one generated type set).
- DTLS-exporter shortcut if Stage 4's measurement demands it.
- Browser-side RedEX log on IndexedDB (separate plan).

---

## Critical files

### Stage 0 (refactor)

- `adapter/net/transport.rs` — `PeerAddr`, `Transport` trait.
- `adapter/net/mesh.rs` — `addr_to_node`, `pending_direct_initiators`,
  dispatch context, `spawn_receive_loop`, `connect*`, reroute call sites.
- `adapter/net/route.rs`, `reroute.rs`, `router.rs`, `proxy.rs`,
  `failure.rs`, `session.rs`, `swarm.rs`, `behavior/proximity.rs`,
  `behavior/fold/{routing,capability}.rs`.

### Stages 1–2 (wire feature, native backend)

- `Cargo.toml` — `wire`, `webrtc` features; `str0m`, `web-time`,
  `getrandom/wasm_js` (target-gated).
- `adapter/net/{session,reliability}.rs` — `Clock` trait.
- `adapter/net/rtc/` — new module.
- `adapter/net/traversal/classify.rs` — `PairAction::Ice`.
- `.github/workflows/ci.yml` — wasm32 check, `webrtc` clippy, symbol diff.

### Stages 3–5 (signalling, leaf, P2P)

- `adapter/net/subprotocol/mod.rs`, `docs/SUBPROTOCOLS.md` — `0x0D02`.
- `adapter/net/behavior/capability.rs`, `docs/CAPABILITIES_SCHEMA.md` —
  tags + `rtc_bootstrap`.
- `crates/net/leaf/` — new crate.
- `crates/net/sdk-browser/` — new package.
- `cli/` — `anchor`, `rtc stats`.
- `deck/` — anchor + leaf rendering.
- `tests/cross_lang_wire/`, `tests/rtc_*.rs`, Playwright suite under
  `crates/net/leaf/e2e/`.

---

## Open questions

1. **Anchor placement policy.** Should every native node with `webrtc` on be
   an anchor, or only nodes an operator marks? Proposal: opt-in via
   `RtcConfig::serve_bootstrap`, STUN service automatic for any `Open`
   node with the feature. Needs a decision before Stage 3.
2. **Leaf capability TTL.** A closed tab cannot withdraw its announcement.
   `FoldKind::DEFAULT_TTL` handles expiry; do we want a shorter default for
   `leaf`-tagged entries (e.g. 60 s with a 20 s re-announce), and does that
   interact with the announce rate limit already noted for tests?
3. **Multiple anchors per leaf.** One anchor is a single point of failure for
   signalling and fallback. Proposal: a leaf bootstraps to one and learns
   others from `rtc-anchor` announcements, holding sessions to `min(3, n)`.
   Affects Stage 4 scope.
4. **Safari.** DataChannel `maxRetransmits: 0` semantics and
   `RTCPeerConnection` restart behaviour differ. Treated as best-effort in
   Stage 4; decide whether it gates Stage 5.
5. **Identity revocation for browsers.** A wrapped key in IndexedDB can be
   exfiltrated by XSS on the origin. Whether leaf identities need a shorter
   certificate lifetime under the org revocation floors is an org-auth
   question, tracked against OA-4.

---

## Rough estimates

| Stage | Surface | Complexity | Estimate |
|---|---|---|---|
| 0 | `PeerAddr` refactor | Large, mechanical, security-sensitive | ~5–7 days |
| 1 | `wire` feature + wasm32 boundary | Small–medium | ~2 days |
| 2 | str0m backend, demux, STUN, loopback harness | Medium–large | ~5–6 days |
| 3 | Signalling + bootstrap + anchor CLI | Medium | ~3–4 days |
| 4 | Leaf crate + browser SDK + Playwright | Large | ~7–9 days |
| 5 | P2P, NAT matrix, stats, demo | Medium–large | ~4–5 days |

Total: ~26–33 days serial. Stage 0 blocks everything; Stages 1 and 2 can run
in parallel once 0 lands; Stage 4 can start against the Stage 2 harness
before Stage 3 is complete.

---

## Dependencies

- `str0m` — sans-IO WebRTC (ICE, DTLS, SCTP, DataChannel). Native, feature
  `webrtc` only. Verify MSRV against `rust-toolchain.toml` (1.98.0) at
  Stage 2.
- `web-time` — `Instant` on wasm, feature `wire` on wasm32 only.
- `getrandom` `wasm_js` feature — wasm32 only.
- `wasm-bindgen`, `web-sys` (`RtcPeerConnection`, `RtcDataChannel`,
  `IndexedDb`, `SubtleCrypto`), `wasm-bindgen-futures` — leaf crate only.
- An HTTP/TLS stack for the bootstrap endpoint (`axum` + `rustls`, reusing
  the payments crate's pinned rustls) — feature `webrtc` only.
- Playwright — CI dev dependency.

No new dependency reaches the default build.

---

## Out of scope (for this plan)

- WebTransport as an alternative unreliable client-to-server transport.
- A TURN protocol server.
- Audio / video / SRTP.
- Browser forwarding for other browsers (leaf topology only).
- Browser-side durable storage (RedEX / Dataforts on IndexedDB).
- A collaborative-text CRDT. Net carries and persists updates; the CRDT
  (Yjs / yrs, Automerge) is the application's. A separate integration note
  should cover the fire-and-forget + state-vector-sync pattern once Stage 4
  gives it a transport.
- Native ↔ native WebRTC.

## Related plans

- [`NAT_TRAVERSAL_PLAN.md`](NAT_TRAVERSAL_PLAN.md) /
  [`NAT_TRAVERSAL_V2_PLAN.md`](NAT_TRAVERSAL_V2_PLAN.md) — mesh-native
  STUN/TURN for native peers; the relay fallback this plan reuses; the NAT
  simulator harness Stage 5 extends. Both list "browser / WebRTC bridge" as
  out of scope; this is that plan.
- [`NRPC_RECV_LOOP_BATCHING_PLAN.md`](NRPC_RECV_LOOP_BATCHING_PLAN.md) — the
  `IngressReceiver` seam the demux lands in.
- [`FAIRSCHEDULER_TRANSPORT_PLAN.md`](FAIRSCHEDULER_TRANSPORT_PLAN.md) —
  subprotocol-id and stream-allocation conventions.
- [`MCP_BRIDGE_PLAN.md`](MCP_BRIDGE_PLAN.md) — the attach-vs-participate
  doctrine; browsers participate.
- [`CAPABILITY_AUTH_PLAN.md`](CAPABILITY_AUTH_PLAN.md) — admission and PSK
  provisioning the leaf reuses unchanged.
