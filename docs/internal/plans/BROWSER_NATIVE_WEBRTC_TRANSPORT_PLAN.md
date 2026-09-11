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

**Stage 0 executed and complete (2026-09-11)** — five outputs, each
reproduced by the reviewer before acceptance: S0a `4a95691d4`, S0b
`4b4d9875f`, S0c `80fbe57bf`, S0d + S0e `1a0504131`. Evidence is folded
into §Context, §1, §2, §3, §4, §6, §7, §8, §12, Stages 2–4, §Rough
estimates and §Dependencies, each marked "What S0x established" or
"(S0x)". The four pre-Stage-1 decisions (A–D below) now rest on measured
evidence; decision D's dependency claim was withdrawn by S0b and reopened
as a Stage 3 choice. **Stage 1 remains unauthorized**; the Stage 1
authorization review has everything it asked for.

**Revision 3 (2026-09-11) — Fable's source-checked review of revision 2,
dispositions by Kyra.** Four repairs were accepted as *pre-Stage-1
decisions*; none overturns the architecture and none widens the
authorization. Before Stage 1 may be authorized the plan must carry:

| # | Decision | Section |
|---|---|---|
| A | Send-path seam: UDP keeps its async/deadline/batching semantics; RTC uses bounded non-blocking admission; Stage 0 inventories the send and ingress paths and proposes the contract | §1, §2, Stage 0 (S0d), Stage 1 |
| B | Pre-enrollment privileges — **CLOSED 2026-09-11: enrollment-gated.** Provisional sessions, action-level bootstrap allow-list, no pre-enrollment transit, session-bound promotion, provider authority unchanged; the PSK stated precisely, not as "public" | §5 Layer 0, §12, Open question 7 |
| C | The routing-envelope codec is part of the portable wire inventory; S0a proves a routed round-trip | §7, Stage 0 (S0a) |
| D | `str0m` pinned with `default-features = false, features = ["rust-crypto"]`; S0b proved both roles on it — **but it still compiles `aws-lc-sys` C via `dimpl`'s `rcgen` feature**; Stage 3 picks (a) accept, (b) per-platform provider, or (c) upstream fix before adding the dependency | §2, §Dependencies, Stage 0 (S0b) |

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

**What S0d established (`docs/internal/spikes/S0D_SEND_INGRESS_INVENTORY.md`,
commit `1a0504131`, read at `4b52454a1`):**

- **49 production call sites, not ~30/36.** The list above is stale in
  both directions at that head: it misses the three `try_send_to` shed
  sites (`:24568`, `:24751`, `:25057`), `:33706`, `:33726`, `:33958`,
  `:33992`, `:34054`, `:40747`, the five `send_datagram` funnel callers
  (`:28302`, `:28323`, `:33389`, `:33443`, `:35820`), and
  `router.rs:895` (`route_packet`'s `enqueue` — where a
  `RouteEntry::next_hop` becomes a `QueuedPacket::dest`). Plus 7
  primitive fns. The inventory's table, not this paragraph, is Stage 1's
  checklist.
- **Exactly three blocking shapes**, so the UDP seam has three entry
  points and collapsing them would change behaviour: awaited
  (`send`, 42 rows incl. 25 fire-and-forget), awaited under a caller
  deadline (`send_bounded`, 3 rows — all `DATAGRAM_SEND_DEADLINE = 5 s`
  in production; `:6797`/`:6806` additionally accept a fixtures-only
  policy deadline, per Kyra's narrowing at `b6e522bb5`), and
  synchronous non-blocking shed (`try_send`, 3 rows whose comments
  reject queuing). 14 rows spawn a task around the send; the seam must
  preserve that structure, including the rollback guard that travels
  into the task at `:25462–25477`.
- **The `PeerAddr` leak surface is 10 rows**, not the 49 mechanical
  sites: 5 destinations derived from `RouteEntry::next_hop`
  (`:24699`, `:25213`, `:35497`, `router.rs:889`, the routed-forward
  lookup feeding the drain), 5 from a packet source (`:25217`, `:25902`,
  `:40747`, `mod.rs:1138`, `pending_direct_initiators` keyed at `:679`).
  Those pull `PeerAddr` into `route.rs`, and `reroute.rs` / `failure.rs`
  follow. Reflex/traversal (2 rows) and `config.peer_addr` (1 row) stay
  `SocketAddr`.
- **`proxy.rs` is a separate forwarder with its own route table**; the
  plan never said whether it participates in `PeerAddr`. Stage 1 decides
  explicitly: in scope, or frozen UDP-only.
- **Guard-across-await audit: expected zero, found zero** at that head —
  by reading, enforced by one witness
  (`org_routing_wiring_tests.rs:7807`) for two funnels only. No
  mechanical check prevents a new site from reintroducing it.
- `transport.rs`'s connected-socket `send()` variants (`:265`, `:705`)
  have no production caller and can be dropped rather than generalized.
- **Two rows resist a single class**: `handle_routed_handshake`'s msg2
  (destination is a route entry *or* the packet source, chosen at
  runtime, `:25213–25217`) and `try_publish_to_peer` (`:35497`), which is
  the transport for every nRPC frame, so its RTC disposition governs far
  more than channel fan-out.

Sequencing: Stage 0 (S0d) has produced the send-path and ingress-path
inventory and the proposed contract (`S0D_SEND_INGRESS_INVENTORY.md` §3:
`PeerSink::{send, try_send, send_bounded}` for UDP; `try_send(.., RtcPeerId)`
for RTC; a partitioned scheduler drain; a bounded third `IngressReceiver`
variant). Stage 1 implements only the UDP-preserving preparation; Stage 3
adds the RTC behaviour. That keeps the staging boundary real. **S0d's
Stage 1 exit criterion, stated as a property of its table:** after the
edit every row's blocking, error-mapping and batching column is
unchanged and no row's guard column becomes `yes` — `deliver_stream_packet`
still maps failure to `StreamError::Transport` and the scheduler enqueue is
still the only `Backpressure` producer.

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
  `RtcConfig::buffered_amount_advisory` is an **input to
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

**What S0b established (`docs/internal/spikes/S0B_RTC_LOOP.md`, commit
`4b4d9875f`; headless Chromium 149, str0m 0.23.1, both ICE roles, both
directions, through the S0a `NetSession` on both ends):**

- **str0m caps SCTP buffering at 128 KiB across all streams of one
  `Rtc`** (`MAX_BUFFERED_ACROSS_STREAMS = 128 * 1024`, `sctp/mod.rs:30`,
  not configurable). `Channel::write` returned `Ok(false)` at exactly
  129 424 B buffered. So the earlier 256 KiB advisory default could never
  fire; the advisory threshold must sit **below 128 KiB**, and several
  channels on one `Rtc` would share one budget (untested — the spike ran
  one channel per `Rtc`, which §3 already fixes as the design).
- **Retention policy is not optional.** With the browser's event loop
  blocked 3 s and 8 KB packets pushed: a drop-on-`Ok(false)` policy
  silently lost 19 476 packets while reporting one admission refusal;
  retain-and-retry turned the same workload into 2 343 admission refusals
  and **zero** in-flight loss. Stage 3's policy is **retain-and-retry**:
  a packet accepted at admission is retained until written or the channel
  closes (249 discarded at close in the probe, counted). Loss then has one
  named point — admission — plus terminal close.
- **Staleness is real and bounded by one drain**: the published
  `buffered_amount` lagged the true value by up to 8 089 B (mean 3 235 B),
  i.e. about one packet. The reserved-bytes bound held throughout.
- **The advisory is refreshed only when the driver writes to that peer.**
  `Channel::buffered_amount` needs `&mut Rtc`, so an idle peer keeps
  whatever it last published. Stage 3 specifies the refresh cadence (at
  every drain for every peer with a non-empty queue, at minimum), not just
  the factoring.
- **One choke-point drain.** Two contract violations were hit while
  writing the loop: returning from the pump after `Channel::write` without
  draining (transmits sit until the next iteration — a ~5 ms latency
  floor), and calling `Rtc::channel(cid)` twice around a
  `buffered_amount` read. The driver gets a single `drain()` that every
  mutation goes through.
- **Windows `WSAECONNRESET` on UDP `recv_from`.** After a peer vanishes,
  an ICMP port-unreachable makes the next `recv_from` on the shared RTC
  socket fail with os error 10054 about a *different* peer. Treated as
  fatal it kills every session on the socket. The driver swallows it and
  counts it — the same per-packet-path treatment `spawn_receive_loop`
  already gives `ConnectionReset` (`mesh.rs:24273–24304`). str0m's
  `http-post` example does not handle it.
- **Native-as-offerer worked**: no p2p asymmetry in str0m; the ~150 ms
  extra open time was one additional signalling round trip in the spike's
  HTTP shape, not the stack.

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
  cap stays; no fragmentation changes. **Mind `MAX_PAYLOAD_SIZE = 8108`**
  (`8192 − HEADER_SIZE 68 − TAG_SIZE 16`): S0c found that a payload of
  8 192 bytes fails `NetHeader::validate` (`protocol.rs:478`) on arrival
  and is dropped with **no log and no counter** — the channel looks dead.
  The leaf fragments at `MAX_PAYLOAD_SIZE`, and Stage 3 adds a counter for
  `validate()` rejections on the RTC ingress path so the black hole is
  visible.
- **Batch events per packet.** S0c: cost at 60 Hz is per-*packet*, not
  per-byte (1 KiB → ~4.5 µs, 8 kB → ~25 µs to build; one `dc.send` per
  packet is the dominant main-thread term). The leaf's event path
  coalesces a frame's updates into one packet by default.

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

The DTLS-exporter shortcut is **deferred, and S0c says it stays so on
performance grounds** (`docs/internal/performance/WEBRTC_DOUBLE_AEAD.md`,
commit `80fbe57bf`; headless Chromium 149, wasm `opt-level="z"`, scalar
ChaCha, three 30 s runs per cell): Net's AEAD on top of DTLS costs
+3.5 µs per 1 KiB packet hot (+0.21 ms of main-thread time per second at
60 Hz; +0.6–1.7 ms/s measured cold inline, of which most is `dc.send` and
the wasm/JS crossing the shortcut would still pay), +3–4 ms/MB browser CPU
sending and +4.5 ms/MB receiving at 1 MB/s bulk (≈0.3–0.45 % of one core),
and no measurable latency difference (≤0.06 ms in medians). The 1 MB/s
workload was sustained with zero admission refusals; the unpaced ceiling
was 6.3–7.4 MB/s. Cheaper levers if per-frame time ever binds: batch
events per packet (§3), then `+simd128`. Reconsidering the exporter is a
*security-model* change — it makes the anchor's DTLS the only
confidentiality boundary for what it relays — not a performance change.

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

**mDNS host candidates (S0b).** Chromium publishes host candidates as
`<uuid>.local` names by default (`WebRtcHideLocalIpsWithMdns`);
`Candidate::from_sdp_string` accepts them, but str0m has no mDNS resolver,
so no pair forms from host candidates alone. The spike disabled the feature
with a flag; production cannot. Stage 4 chooses among: the anchor accepting
the **peer-reflexive** candidate str0m learns from the browser's inbound
binding request, relying on the browser's **server-reflexive** candidates
(gathered against the anchor's own STUN, §STUN above), or an mDNS client on
the anchor. The first two need no new dependency and are the expected
answer; the harness must run *without* the flag before Stage 4 exits.

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
and are listed under deferred work, not promised. **S0b:** str0m 0.23.1
constructs and accepts a passive TCP host candidate
(`Candidate::builder().tcp().tcptype(TcpType::Passive)`;
`str0m::net::{Protocol, TcpType}` are public), but as a sans-IO crate it
leaves the TCP listener, RFC 4571 framing and connection lifecycle to the
caller — not exercised end to end.

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
by the existing address-independent identity-binding path. **S0b
confirmed:** `RTCPeerConnection` is `ReferenceError: not defined` in both a
dedicated `Worker` and a `SharedWorker` on Chromium 149 — the leaf's RTC
driver is main-thread; the leader-elected design stands.

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

**What S0e established (`docs/internal/spikes/S0E_BOOTSTRAP_FRAMES.md`,
commit `1a0504131`).** The frame table has 14 rows; rows 1–9 are required
(routed handshake msg1/msg2; reply-channel `0x0A00` Subscribe + Ack; nRPC
REQUEST / RESPONSE / CANCEL for `net.mesh.enroll`; `0x0B00` grants for the
two bootstrap streams; NACK/retransmit on them), rows 10–14 incidental
(heartbeat — permitted as maintenance; pingwave; periodic capability
re-announce; a corrective re-announce; fold/sensing/migration/
withdrawal/`0x0D02`). The allow-list Stage 4 enforces is S0e §2, verbatim;
the points that shape it:

- **The sharpest finding:** a rejected reply-channel Subscribe fires a
  **rate-limit-bypassing corrective capability re-announce**
  (`mesh_rpc.rs:5842–5870`, rationale `mesh.rs:29577–29581`). Left in
  place it hands an unenrolled browser a mesh-wide flood trigger, one per
  refused Subscribe. Removed from the provisional path.
- The reply-channel Subscribe is fire-and-forget UDP retried up to
  `membership_max_attempts` times **reusing one nonce**
  (`mesh.rs:29695–29703`); the anchor's dedupe (`:29791`) makes that safe.
  So the bound is "≤ N frames sharing one nonce", never "exactly one
  frame" — a naive bound breaks the flow on the first dropped datagram.
- The enrollment REQUEST is a generic `publish_to_peer` payload whose
  service name lives **inside the nRPC envelope**
  (`RpcRequestPayload.service`, `mesh_rpc.rs:5374–5380`) behind a
  channel-hash discriminator. §12 step 3 therefore means decoding the
  nRPC envelope under strict bounds *before* admission is decided — the
  check cannot be a header test. A real cost, now named.
- `0x0B00` is required-but-conditional: a single `JoinRequest` fits the
  default window (`open_stream_with`, `mesh.rs:35421`; credit charged at
  `:35429–35433`), but a larger payload or response makes a grant
  load-bearing, so the two bootstrap streams get `0x0B00` regardless.
- Heartbeat (`:27554`, permitted) and pingwave (`:27556`, denied) are
  emitted two lines apart in one loop body; the provisional check splits
  the statements, not the loop.
- The allow-list is about **what the anchor accepts**, never what the
  device sends: `Mesh::join` requires a started node, and `start_arc`
  spawns seven loops (`mesh.rs:22389–22410`, `:22524`), three of which
  emit traffic unrelated to enrollment.
- **The attack is one `if` away today.** `connect_via`'s `relay_addr`
  and `dest_node_id` are independent parameters (`mesh.rs:39514`); `join`
  happens to pass the same anchor for both, which is what makes
  local delivery true (`RoutingHeader::new(dest_node_id, …)` at `:39562`,
  `dest_id == local_node_id` branch at `:24614`). Nothing enforces it — a
  provisional peer calling the same path with a third-party `dest_node_id`
  reaches the F1 forwarding branch unchecked.
- **Seven forwarding sites need the adjacent-session admission check
  (F1–F7):** `dispatch_packet`'s non-local arm (`:24675–24757`, forward
  `:24751`); `relay_protected_hop` (`:24860`, `:25057`);
  `Router::route_packet` (`router.rs:769` → `:895`); the pingwave
  re-flood (`:24568`); `forward_scoped_announcement` /
  `forward_capability_announcement` (`:33389`, `:33443`);
  `forward_punch_ack` / rendezvous introduce (`:34104`, `:33706`,
  `:33726`); `proxy.rs:349`. None has an admission gate today. F1 keeps
  its `dest_id == local_node_id` test *above* the admission check so the
  enrollment envelope is delivered, not refused.
- Route installation from the routed handshake (`:25460–25477`,
  `registered_next_hop: source`) is the concrete site where "a provisional
  peer must not become a routing participant" lands: install the
  session, withhold routing/discovery until admitted.
- Renewal is `RENEWAL_SERVICE = "net.mesh.renew"`
  (`sdk/src/mesh_enroll.rs:42`), runs on an admitted session, and is not
  on the provisional list. `net.mesh.enroll` has no method dimension
  (`serve_rpc_typed(service, codec, handler)`, `:175`): `(service, unary)`
  is the whole identity; finer granularity is Stage 4's to add.
- Whole-session bounds proposed by S0e (Stage 4 may tighten, not loosen):
  provisional state expires 30 s after the handshake; ≤ 256 inbound
  frames; ≤ 256 KiB inbound; ≤ 2 streams; ≤ 1 channel membership;
  ≤ 1 in-flight enrollment call, ≤ 4 REQUEST frames, body ≤ 16 KiB; on
  breach close and reclaim.

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

### Exit criteria — **MET 2026-09-11**

- All five outputs written up: S0a `4a95691d4`
  (`docs/internal/spikes/S0A_WIRE_BOUNDARY.md`), S0b `4b4d9875f`
  (`S0B_RTC_LOOP.md`), S0c `80fbe57bf`
  (`docs/internal/performance/WEBRTC_DOUBLE_AEAD.md`), S0d + S0e
  `1a0504131` (`S0D_SEND_INGRESS_INVENTORY.md`, `S0E_BOOTSTRAP_FRAMES.md`).
  Each was reproduced by the reviewer before acceptance (S0a test +
  wasm check + size; S0b `run.ps1` + `cargo tree` + str0m source; S0c
  `run.ps1 -Bench` 27/27 cells; S0d/S0e citations spot-checked at
  `mesh.rs:39562`, `:24568`, `:29695–29703`, `:35421`, `router.rs:895`).
- §2's RTC submission contract, §1's send seam, §7's type list and §12's
  allow-list are finalized from them (folded into those sections).
- Sizing inputs for Stages 1–6 recorded (§Rough estimates); day figures
  are the Stage 1 authorization review's to set.
- No repository code outside `docs/` and `spikes/` changed
  (`git diff --stat 4b52454a1..1a0504131`).

**Stage 0 is complete. Stage 1 was authorized from `ad874ff43` (Kyra,
2026-09-11) — implementation only, not acceptance, merge, or Stage 2.**

## Stage 1 — `PeerAddr` endpoint generalization (UDP-only, behaviour-neutral)

**Authorized (Kyra, 2026-09-11) from `ad874ff433b89f20ce8813ecd8d6c93b2ec58f89`.**
Implementation authorization only: not Stage 1 acceptance, not merge, not
Stage 2. Kyra verified the branch head, that its commits touch only
`docs/` and `spikes/`, re-ran the S0a routed round-trip (2 passed) and
independently confirmed the `aws-lc-sys` pull; the browser benchmark was
not re-run for authorization.

**The three decisions, recorded with the first Stage 1 commit:**

1. **`NetProxy` is frozen UDP-only.** `proxy.rs` owns its own UDP socket
   and next-hop map; it is not the mesh's peer-session router. Its
   endpoint API, next-hop storage and send behaviour stay unchanged; it is
   not connected to RTC ingress or to provisional browser sessions; its
   S0d rows (F7, row 48) are **preservation checks**, not instructions to
   route it through `PeerSink`. Shared-type/import compatibility edits are
   allowed; no transport redesign. The same distinction applies to
   standalone UDP primitives (`transport.rs` `send()` variants, traversal
   sockets): the inventory names behaviour to preserve, not a requirement
   to generalize every socket API.
2. **Stage 2 follows *accepted* Stage 1.** No endpoint type parameters
   introduced to run the stages concurrently. Stage 1 establishes
   `PeerAddr` and the UDP-preserving submission boundary; Stage 2 then
   extracts the portable types, placing the endpoint types low enough that
   `net-wire` never depends on the native runtime. No concurrent
   production wire-crate extraction.
3. **The exported-symbol baseline job is the first Stage 1 commit**,
   established and exercised before any production endpoint type is
   edited: baseline pinned to `ad874ff43`; comparison on matching build
   target, profile and feature set; target is the actual exported C-ABI
   artifact — the repository's single `net-ffi` cdylib (`libnet`), not an
   arbitrary Rust library; the checker is demonstrated to reject both an
   added and a removed export. An unchanged symbol set *complements* the
   binding tests; it does not replace them.

**Scope.** Generalize `PeerTransport` and every peer-keyed site listed in
§Context to `PeerAddr`, and introduce the send seam of §1 in its
**UDP-preserving** form only: the applicable production call sites in
`S0D_SEND_INGRESS_INVENTORY.md` §1 submit through
`PeerSink::{send, try_send, send_bounded}` with their current async
behaviour, deadlines, error mapping, batching and synchronous shedding
intact; the 10 leak rows of S0d §3.5 carry `PeerAddr` into `route.rs` /
`reroute.rs` / `failure.rs`; required peer-keyed state, source typing and
scheduler plumbing move to `PeerAddr`, with the drain partitioned by
variant and only the `Udp` arm live. `PeerAddr::Udp` only; no feature
flag; no RTC variant or behaviour; no admission state or anything from
§12; no crypto/backend change; no Stage 2 extraction. Traversal modules
keep `SocketAddr` at their edges. Existing direct/routed ownership,
session lifecycle, deadlines, error mappings, batching and shedding
preserved; **no new guards held across awaits.**

**Stop condition.** A clean, committed candidate for review. No automatic
continuation to Stage 2.

**Candidate delivered: `10302333a` (2026-09-11), awaiting Kyra's
acceptance.** Three heads, kept distinct: the agent's **validated head**
`d65731727` (every S1_REPORT §5 command ran there; historical
attribution, preserved as such in the report), the **implementation
candidate** `10302333a` (validated head + report + one doc-comment word,
`git diff d65731727..10302333a -- net/` is a single non-code line), and
the **submitted head** — the branch tip at handoff, `8605f26ec` plus the
docs-only attribution fix, with `git diff 10302333a..HEAD -- net/` empty.
Nine commits from `ad874ff43`: the export baseline
(`3f73e04f4`), `PeerAddr` + `PeerSink` (`2db4406aa`), peer-keyed state /
ingress source typing / every send site (`a55b8c78b`), fmt + test seam
(`bb34926d0`), **witness and test edits in their own commits**
(`57654b685`, `d65731727`), the Linux batched-ingress argument fix
(`96dc9fdf4`), the report (`98cc22d2c`,
`docs/internal/spikes/S1_REPORT.md`), and an identifier sweep
(`10302333a`). Reviewer re-ran on the Windows host: `cargo test --lib`
with CI's `UNIT_FEATURES` (5975 passed), the six witness floors
(93/24/62/41/60/68, all 276 `REQUIRED` names present — the same 71
live outside `src/` at both ends), strict clippy (`--all-features` and
default, `--lib --bins -D warnings`), per-file `rustfmt --check` on all
32 changed `.rs` files, a fresh `net-ffi` release build with the export
checker (568/568 match, self-test green), diff scope (no `Rtc` /
`webrtc` / provisional tokens; `proxy.rs` and `traversal/` untouched),
the `org_routing_wiring_tests.rs` diff line by line (signature and field
renames only), and the org-egress deadline arms against the original
(identical). Three source-text pins followed a rename; each still fails
if its property is removed (S1_REPORT §4). Deviations accepted: rows
25/26 (punch train to `peer_reflex`) stay on the raw socket per decision
1; rows 4/5 call the `bound_datagram_send` wrapper directly because the
fixtures arm substitutes the future. **Not verifiable on this host, CI is
the arbiter:** the `cfg(target_os = "linux")` batched-ingress arm and
`sendmmsg` drain (no cross C toolchain; one such arm was broken for one
commit and caught by reading), `go test ./...` (no cgo toolchain), and
`cargo fmt --all` (Windows argument-length limit, os error 206).

### Exit criteria

- S0d's applicable rows preserve their blocking, error-mapping and
  batching columns; no row's guard column becomes `yes`
  (`deliver_stream_packet` still maps failure to `StreamError::Transport`;
  the scheduler enqueue is still the only `Backpressure` producer;
  `bound_datagram_send` deadlines and the three `try_send_to` sheds
  survive; the `sendmmsg` grouping survives; no UDP `WouldBlock` surfaces
  as `Backpressure`).
- Zero wire change: `cross_lang_*` golden fixtures pass unmodified; every
  existing assertion intact. Mechanical signature edits allowed; witness
  diffs reviewed line by line.
- Witness floors 93 / 24 / 62 / 41 / 60 / 67 remain enforced, including
  their named witnesses (`ci.yml`).
- The repository's full applicable pre-push matrix (`AGENTS.md`, "Pre-push
  checklist"): `cargo fmt --check`, `cargo check --workspace --all-targets`,
  the three strict `--lib --bins` clippy feature sets, the permissive
  `--all-targets` clippy, `RUSTDOCFLAGS="-D warnings" cargo doc`, plus the
  per-member clippy/doc for every touched member — and exact-head CI green.
- Export comparison green: the `libnet` cdylib's exported symbol set
  matches the `ad874ff43` baseline (decision 3).

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

**Authorized by the product owner (2026-09-11), stacked on the Stage 1
candidate `10302333a` before Kyra's acceptance** — a deliberate
deviation from decision 2's "follows *accepted* Stage 1", taken with
exact-head CI on `8605f26ec` running concurrently (its Unit, Linux
batched send/recv, Go and Format jobs were green; its FFI-clippy failure
for the meshdb/meshos/deck-ffi feature sets was a dead
`DispatchCtx.socket` field, fixed in `cea1def23` — punch train now reads
the socket via `PeerSink::udp_socket`, no behaviour change). Every
Stage 2 commit is additive so a further Stage 1 fix cannot conflict.

**Candidate delivered: `a9e7f223c` (2026-09-11), awaiting acceptance.**
Validated head `2a9a1d7c5`; candidate = validated head + report; the
submitted head adds this record and a one-line trailing-newline fix in
`ci.yml`. Crate `net-mesh-wire` (lib `net_wire`) at
`net/crates/net/wire/`: the eight named items, all of `route_hop.rs`,
`ParsedPacket`, `PeerAddr` (decision 2: the endpoint type sits in the
wire crate), the coarse clock, `StoredEvent` (+ its `serde_json` users
behind a `json` feature the core enables — an inherent impl cannot stay
in another crate), the AEAD seam (`ring` native / `chacha20poly1305`
wasm32) and the `Clock` seam over the 13 sites. Core re-exports keep
every path: consumer diff over `go/`, bindings, SDK, CLI, deck, MCP,
payments is empty; 19 `adapter::net::<module>` consumer sites unedited.
Three `pub(crate)` items widened to `pub` (`session_prefix_from_id`,
`PacketBuilder::new`, `Stream` fields) — the `PacketBuilder::new`
invariant is now enforced by the relocated drift check plus a **negative
witness** (`a_planted_production_caller_breaks_the_allowlist`). New CI:
`wasm-wire` (wasm32 check + clippy + **executed** `wasm-bindgen-test`
under Node with a per-test grep so a zero-test harness cannot pass) and
`wasm-wire-no-native-deps`; `net-mesh-wire` in the per-member lint/doc
lists; `cross_lang_wire` pinned; `release-crates.yml` publishes
`net-mesh-wire` before `net-mesh`. Reviewer re-ran on the Windows host:
`cargo test --lib` with CI's features (5781 passed — the 194 moved tests
now run as `cargo test -p net-mesh-wire --features json`, 196 passed),
the six floors (93/24/62/41/60/68), `cross_lang_wire` (7), the six
drift-check tests incl. the negative witness, `cargo check` +
**executed** wasm tests (3 passed under Node, `wasm-bindgen` 0.2.128),
`cargo tree` with zero tokio/mio/socket2/net-mesh edges, strict clippy,
export checker on a fresh cdylib (568/568), YAML + script-permission
checks, and the `mesh.rs` delta since `10302333a` (only `cea1def23`'s
three lines). **Not verifiable here:** CI's own runs of the new jobs and
the Linux/Go arms — CI at the submitted head is the arbiter.

**HOLD (Kyra, 2026-09-11, reviewed at `b6e522bb5`).** Stage 1 UDP
preservation: no identified source blocker, all 56 inventory rows and
the real Linux batched-ingress / `sendmmsg` source reviewed, its
Linux/binding CI jobs green at the submitted head — credit preserved.
The combined candidate is held on five Stage 2 defects, in repair
(`spikes/S2_REPAIR_BRIEF.md`); Stage 3 is not authorized:

| # | Defect | Repair |
|---|---|---|
| R1 (P2) | `Stream` handle fields became `pub`; an application can set `handle.config.reliability = Reliable` on a `FireAndForget` stream and the packet ships `wire_reliable=true` with no retransmit entry (reviewer-executed two-node probe; E0616 at Stage 1) | `Stream` handle back in the core, private fields + accessors, no public constructor; compile-probe and live-send witnesses |
| R2 (P1) | the 196 moved native wire tests no longer gate CI (root unit job runs `net-mesh` only; clippy compiles, wasm runs three cases) | explicit `cargo test --locked -p net-mesh-wire --features json` + non-vacuous inventory/count guard; planted-failure demonstration |
| R3 (P1, CI red) | wasm target installed for `@stable`, commands resolve the pinned 1.98.1 → `E0463` | install the target for the selected toolchain; deps gate checks host **and** wasm graphs |
| R4 (P2, CI red) | `guards/fixtures_off_probe/Cargo.lock` not refreshed for `net-mesh-wire`; negative leg fails for the wrong reason | regenerate the probe lockfile; keep `--locked` and both legs |
| R5 (P2) | wasm test includes `../../tests/cross_lang_wire/aead_vector.json` (outside the package); drift guard reads `wire/src/session.rs` (absent for a registry dependency) | package-owned shared fixture; guard explicitly repository-only, never silently vacuous; verified on an unpacked `.crate` |

Retained from the review: 5781 / 196 / 7 / 3-wasm / 4-doc all green at
the candidate; six floors 93/24/62/41/60/68 with all 209 floor-roster
names executed; reviewer inverses (timeout arm → success fails the
stalled-egress witness; a native monotonic read on wasm fails 2/3; a
corrupted wasm AEAD nonce fails the golden vector; drift-guard source
mutations fail 2 and 1) all restored and hash-checked. Non-blocking
wording corrections: `PacketBuilder::new`'s widening is not a new crypto
vulnerability and the drift check is a lexical two-file guard, not
nonce/key-ownership enforcement; the wasm test graph is not JSON-free
(`serde_json` is a dev-dependency); ordinary org-egress uses
`DATAGRAM_SEND_DEADLINE` and the queue-selected deadline is fixture-only
(S0d/S1 wording narrowed); the wasm smoke meets Stage 2's minimum and is
not lifecycle coverage; the duration-below-`u128::MAX` assertion does not
prove monotonic ordering.

**Repair candidate delivered: `614b1c636` (2026-09-11), awaiting Kyra's
re-review.** One commit per repair (`512d808b2` R1, `3f22e5443` R2,
`a0d8f2158` R3, `64b0256c9` R4, `b9b5d0536` R5) plus the §9 report;
the submitted head adds this record and Kyra's non-blocking wording
corrections to `S2_REPORT.md` §3/§7 and to S0d/S1 (`ee3cfaf18`).
Reviewer re-ran on the Windows host at `614b1c636`:

- **R1** — `Stream` lives in `adapter/net/stream_handle.rs` with private
  fields, `pub(crate) fn new`, read-only accessors; three `compile_fail`
  doctests (config write, epoch write, struct-literal forge) pass as
  doctests (4 passed); two live-send witnesses over the real two-node
  connect/accept path pass (fire-and-forget: unreliable on the wire and
  nothing retained; reliable: `RELIABLE` on the wire and a descriptor
  retained). `ffi/mesh.rs` tests use the crate constructor. Inherited,
  not charged: the conflicting-config idempotent reopen.
- **R2** — unit job runs `cargo test --locked -p net-mesh-wire
  --features json` and a `MIN=196` count + exact-name roster guard (two
  `should_panic` cases and one test per moved module). Reviewer: 206
  passed (the ten `route.rs` codec tests moved with their code; core
  `--lib` 5781 → 5773 = −10 codec + 2 R1 witnesses), roster names
  present. Planted-failure demonstration recorded in §9 (exit 101,
  reverted inside the commit).
- **R3** — `rustup target add` runs from `net/crates/net` so the pinned
  1.98.1 receives the target, with an installed-target assertion; the
  deps gate checks host and wasm32 graphs. Reviewer: `rustup show
  active-toolchain` = 1.98.1 (overridden by `rust-toolchain.toml`),
  wasm32 installed, executed wasm tests 3 passed under Node, wasm-graph
  forbidden-edge count 0.
- **R4** — probe lockfile regenerated; reviewer: negative leg fails with
  the intended `E0432 org_exact_sensing_bridge` (exit 101), positive
  leg compiles.
- **R5** — AEAD vector packaged inside the crate
  (`src/test_vectors/aead_vector.json`, `net_wire::test_vectors::AEAD_VECTOR`
  behind `test-vectors`); the core's `cross_lang_wire` reads the constant
  and byte-checks the repository copy; the callee drift guard skips with
  a printed reason only when no `.git` is found up the tree. Reviewer:
  `cargo package` → unpacked `.crate` (23 files) compiles natively with
  `json test-vectors` and for wasm32 `--all-targets --features
  test-vectors`.
- Sweep: `cargo test --locked --lib` 5773 passed; floors 93/24/62/41/60/68;
  `cross_lang_wire` 8 passed; strict clippy pass; YAML + whitespace
  clean; consumer diff since `10302333a` empty; fresh `net-ffi` release
  rebuild → export checker 568/568.

**Not verifiable here:** CI's own run of `wasm-wire`,
`wasm-wire-no-native-deps`, the probe step and the wire-suite gate at
the submitted head — CI is the arbiter.

**Second HOLD (Kyra, reviewed code head `614b1c636`, docs head
`80eb147d4`), repair credit retained.** Exact-code CI 48 green / 1 red.
R1 opacity, R2 execution, R3 target install, R4 lockfile accepted as
substantively repaired; three bounded closure items:
C1 — the wasm all-targets clippy step was featureless while the wasm
test needs `test-vectors` (`E0433`, the one red job); C2 — the drift
guard's "any `.git` ancestor" check misclassifies a package unpacked
under a consumer repo as the Net checkout; C3 — the R1 witnesses never
observed the emitted packet (Kyra's one-line production inverse,
`builder.build(.., PacketFlags::NONE)` at `mesh.rs:39292`, left both
passing).

**Closure candidate delivered: `bdcd47125` (2026-09-11), awaiting
re-review.** `513ea73d4` C1, `c14199bcd` C2, `fb31c198c` C3, plus §10 of
`S2_REPORT.md`. Reviewer re-ran at `bdcd47125`:

- **C1** — the previously wired featureless clippy command fails with
  `E0433` (2 hits); the new command with `--features test-vectors`
  passes; the featureless portable-library `cargo check` stays and
  passes; the three wasm witnesses pass under Node; both graph guards
  unchanged; an unpacked `.crate` passes featureless `check` and
  feature-enabled `--all-targets`. Comment corrected.
- **C2** — `is_net_workspace` requires `wire/Cargo.toml` **and** the
  workspace `members` entry **and** the `net-mesh-wire = { path = "wire" }`
  dependency; a `.git` ancestor is no longer a signal. New unit witness
  `net_workspace_detection_needs_every_layout_marker` covers Kyra's
  three placements (outside Git, under an unrelated repo at
  `target/package/…`, real checkout); the seven drift-check tests pass,
  missing callee source stays fatal in the real checkout.
- **C3** — both witnesses now `recv_from` the peer's raw socket, parse
  with `ParsedPacket::parse`, decrypt under the peer session's rx key,
  assert `stream_id` and the `RELIABLE` bit (set / clear), and the
  reliable witness NACKs `next_expected = header.sequence` and asserts
  **exactly one** retained descriptor with matching `stream_id`, `seq`
  and `RELIABLE`. Reviewer ran Kyra's inverse: the reliable witness
  fails on "a reliable stream's packet must carry RELIABLE on the wire";
  preimage restored and hash-checked (`4c24168709469794`); both pass.
  Prose narrowed in `stream_handle.rs` and §9/§10 to "the mutation and
  forgery paths Stage 2 newly exposed".
- Sweep: `--lib` 5774 (+1 C2 witness); floors 93/24/62/41/60/68; wire
  206; `cross_lang_wire` 8; strict clippy; forced `net-ffi` rebuild →
  exports 568/568; consumer diff since `10302333a` empty; whitespace +
  YAML clean.

**Not verifiable here:** CI's own run of the corrected `wasm-wire` job
at the submitted head.

**Closure review (Kyra, code head `bdcd47125`, run head `a1610df3f`):
C1 and C3 CLOSED; exact-head CI 49/49 green; one C2 test defect
remains.** The new classifier witness asserted its own
`CARGO_MANIFEST_DIR` is the Net workspace, so a packaged core's test
module still failed — at the classifier test instead of the callee test.

**C2 follow-up delivered: `01e4b0f20` (reviewer-authored, one file).**
The positive case is a controlled complete fixture (temp tree with all
three markers under a fake `.git`); a member-only manifest is a further
negative; the real checkout is asserted only when `CI` is set, with a
message naming the assumption — so the callee guard cannot skip on the
machine that enforces it, and a packaged build never trips it. Missing
callee in a valid layout stays fatal. Reproduced with the exact guard
source under controlled manifest locations: outside any repo 7/7;
beneath an unrelated consumer checkout at
`target/package/net-mesh-0.36.0` 7/7; real workspace 7/7 (`CI` unset
and set); complete layout with `wire/src/session.rs` missing 6/1 exit
101; packaged layout with `CI=true` 6/1 exit 101 naming the assumption.
Core `--lib` 5774; permissive all-targets clippy clean. Stage 3 not
started.

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
- **Loss injection.** With `maxRetransmits: 0`, `reliability.rs` is the
  only recovery mechanism, and S0b's round-trip never lost a packet so
  the NACK/retransmit path never ran over a DataChannel. The harness
  injects loss on the RTC path and asserts a `Reliability::Reliable`
  stream completes and a fire-and-forget stream reports the loss.
- **Retention policy is retain-and-retry** (§2, S0b): with a paused peer,
  zero packets accepted at admission are lost before channel close; the
  count discarded at close is reported. The advisory threshold is below
  str0m's 128 KiB cap and its refresh cadence is asserted for an idle peer
  with a non-empty queue.
- The RTC ingress path counts `NetHeader::validate` rejections (S0c's
  silent 8 192-byte black hole) and a test sends one oversize packet and
  asserts the counter, not a dead channel.
- The driver survives an injected `ConnectionReset` on the RTC socket
  with every other session intact.
- Every non-`Stream::send` outbound class (events, `0x0D02` signalling,
  forwarding, retransmission) has its stated pressure disposition asserted.
- The §5 delivery sequence — routed → authenticated direct → forced direct
  failure → restored routed — passes on the loopback harness.
- External `stunclient` gets a correct XOR-MAPPED-ADDRESS from the RTC
  socket; the Net socket's ingress tests are byte-for-byte unchanged with
  `webrtc` on.
- Default build unchanged; `--features webrtc` clean under `-D warnings`.

**Authorized by the product owner (2026-09-11), stacked on the Stage 2
closure head `01e4b0f20`** while Kyra's C2 follow-up re-review was
pending; additive and feature-gated by construction. Decisions fixed in
`spikes/S3_BRIEF.md`: `aws-lc-sys` **option (a)** (accepted behind the
off-by-default feature, host requirements in `CONTRIBUTING.md`, cost
recorded: 105 s clean `--lib` build on the workstation); advisory default
**96 KiB** (the 256 KiB above could never fire against str0m's 128 KiB
cap — corrected here); retain-and-retry with `discarded_at_close`
counted; bounded `IngressReceiver::Rtc` (1 024) dropping with a counter;
`WSAECONNRESET` swallowed and counted.

**Candidate delivered: `f1b13f5db` + reviewer fix `0fcff7a16`
(2026-09-11), awaiting Kyra's review.** Validated head `6e7ba2116`;
report `docs/internal/spikes/S3_REPORT.md`. The checkpoint head
`8eef41940` was pushed mid-stage and went broadly red in CI (Format,
Clippy, Unit, FFI members, bindings); `6e7ba2116` repaired the local
matrix, and the reviewer found one more member-feature-set defect the
report's sweep missed — `pair_action_for` referenced
`super::traversal::classify` and `nat_class()` without the
`nat-traversal` gate its three callers carry, so `go-meshdb-ffi` /
`go-meshos-ffi` / `go-deck-ffi` could not compile the core (same class
as `cea1def23` in Stage 1). Fixed in `0fcff7a16`; all seven FFI members,
the SDK, `--no-default-features` and `--features webrtc` pass strict
clippy. Reviewer re-ran at the fixed head: default `--lib` 5775 with the
six floors 93/24/62/41/60/68; `--lib` with `webrtc` 5791; `rtc_loopback`
6/6 and `rtc_backpressure` 8/8 (the agent soaked them ×10 / ×25);
`--all-features` and default strict clippy, permissive `--all-targets`,
`cargo doc --all-features`; export checker 568/568 on a fresh cdylib;
consumer diff since `01e4b0f20` empty.

**Exit-criterion status.** 17 of 18 rows in `S3_REPORT.md` §5 met. The
§5 delivery sequence (routed → authenticated direct → forced direct
failure → restored routed) is met **only in its direct half**: the
loopback harness has no third node, no relay and no `0x0D02`, so the
pre-direct routed leg and the "restored routed" leg cannot exist before
Stage 4. Accepted as a **carried criterion**: Stage 4's exit re-asserts
the whole sequence end to end. nRPC and fold over RTC are likewise
covered as a *class* (row 30's disposition) rather than end to end, for
the same reason; `stunclient` was not on the host, so the external-client
STUN leg is CI's. Two new unconditional accessors (`peer_endpoint`,
`peer_is_direct`) are additive; `peer_addr` still answers "which UDP
tuple" and returns `None` for an RTC peer.

**Not verifiable here:** the Linux `webrtc-feature` job and its
`aws-lc-sys` build time; CI at the fixed head is the arbiter.

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
  failures behind certificate-ignore flags. **Trickle stays** (S0b:
  offer-created → DataChannel-open, five runs each, one interface, no
  STUN: trickle 20.8 / 22.5 / 22.7 ms min/median/max vs gather-complete
  146.9 / 150.2 / 177.2 ms — 6.6× at the floor, worse once STUN gathering
  is in the path). The WebSocket, or an equivalent trickle transport,
  is a Stage 4 deliverable; a gather-complete POST is not offered. Stage 4
  also answers the mDNS host-candidate question (§6) and its harness
  runs Chromium without `--disable-features=WebRtcHideLocalIpsWithMdns`.
  **The browser bootstrap credential** of §5
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
- ICE-TCP passive candidates on anchors (S0b: candidate constructible in
  str0m; listener/framing/lifecycle are the caller's — still deferred).
- DTLS-exporter shortcut — **stays deferred** (S0c, §4): not a performance
  lever; only reconsidered as a security-model decision.
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

**Sizing inputs from Stage 0 (2026-09-11), for the Stage 1 authorization
review to turn into figures:**

| Stage | Inputs now known |
|---|---|
| 1 | 49 send sites + 7 primitives across 3 blocking shapes; 10 `PeerAddr` leak rows into `route.rs`/`reroute.rs`/`failure.rs`; scheduler drain partition; `proxy.rs` decision; witness floors 93/24/62/41/60/67 re-exercised; exported-symbol baseline job |
| 2 | 8 named items + `route_hop.rs` (1 005 lines) + `ParsedPacket` + coarse clock + `StoredEvent`; AEAD backend seam; 13 `Clock` sites; ≥ 7 `#[cfg(test)]` modules to relocate incl. the drift-check tripwire; wasm check + executed wasm test jobs; cross-backend AEAD vector; the Stage 1 dependency |
| 3 | driver with single `drain()`; retain-and-retry retention; advisory < 128 KiB with a refresh cadence; `ConnectionReset` swallow; `validate()` counter; loss injection; the `aws-lc-sys` decision (a/b/c); STUN responder; loopback harness re-running witness files |
| 4 | three canonical-signer fields; `0x0D02`; bootstrap listener with browser-trusted TLS + CORS + WS `Origin` + trickle; mDNS answer; the credential format; **§12 admission contract at F1–F7 plus the allow-list with nRPC-envelope decode before admission**; six §12 witnesses |
| 5 | leaf crate + wrapper; main-thread RTC driver (no worker); leader lifecycle + follower proxy; batch-per-packet event path; fragment at `MAX_PAYLOAD_SIZE`; Playwright runner; `ControlPlane` trait + anchorless mock |
| 6 | §9 end to end; NAT simulator extension (green run first); telemetry; demo |

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
  1.85.0 (toolchain is 1.98.0), MIT OR Apache-2.0. **The pinned
  configuration**

  ```toml
  str0m = { version = "0.23.1", optional = true, default-features = false, features = ["rust-crypto"] }
  ```

  **does not exclude the C toolchain — the claim that it does is withdrawn
  (S0b, 2026-09-11).** `str0m-rust-crypto` 0.6.0 → `dimpl` with features
  `["rust-crypto", "rcgen"]`, and `dimpl`'s `rcgen` feature is
  `["dep:rcgen", "aws-lc-rs"]` → `aws-lc-rs` 1.18.1 / `aws-lc-sys` 0.45.0
  (`cargo tree -e features -i aws-lc-sys` in `spikes/s0b-rtc/native`). On
  the Windows host that meant ~11 700 lines of `aws-lc-sys` build-script
  output, C compiled by MSVC `cl.exe` (~40 s), and NASM absent — the build
  survived only via `prebuilt-nasm`. The DTLS certificate generator
  (`rcgen`) is the path that drags it in. **Stage 3 must pick one before
  adding the dependency:** (a) accept `aws-lc-sys` as a build-time C
  dependency behind the `webrtc` feature and document the host
  requirements (cmake/MSVC or clang; NASM or prebuilt); (b) use a
  per-platform provider (`wincrypto` / `apple-crypto` / `openssl`) so no
  bundled C is compiled; (c) upstream a `dimpl` feature that generates the
  self-signed DTLS certificate without `aws-lc-rs`. The `webrtc` feature is
  off by default either way, so the default build stays C-free.
  Everything else in S0b (DataChannel, both roles, DTLS, SCTP, ICE) ran on
  this configuration. ICE-TCP: see §6. *(Still the current release: 0.23.1,
  2026-08-21, per docs.rs at 2026-09-11. A web search claiming 0.21.0 is
  the latest is stale — trust the registry.)*
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

**2026-09-11 — Stage 0 executed (Opus 5 implementing agent; Fable
reviewing).** Four slices, each briefed in `spikes/S0*_BRIEF.md`, each
reviewed by reproduction before acceptance:

| Slice | Commit | Reproduced | Plan corrections it forced |
|---|---|---|---|
| S0a wire boundary | `4a95691d4` | `cargo test` (2 pass), `cargo check --target wasm32`, wasm 576 461 B / 160 694 B gz | `ring` needs a wasm32 clang → AEAD backend seam; all of `route_hop.rs` moves; `ParsedPacket`, coarse clock, `StoredEvent`, `tracing` come along; `Instant::now()` panics at runtime on wasm32 → executed wasm test; `NetSession::peer_addr` is `SocketAddr` → Stages 1 and 2 not independent; `heartbeat_api_drift_check` must be relocated |
| S0b RTC loop | `4b4d9875f` | `run.ps1` → `[run] OK`, four verdict lines both roles/directions; `cargo tree -i aws-lc-sys`; str0m `sctp/mod.rs:30` | `rust-crypto` pin still compiles `aws-lc-sys` via `dimpl`/`rcgen` (claim withdrawn; Stage 3 picks a/b/c); 128 KiB hard SCTP cap → advisory below it; retain-and-retry (drop policy silently lost 19 476 packets); advisory refresh cadence; single `drain()`; Windows `WSAECONNRESET` swallow; mDNS host candidates need a Stage 4 answer; trickle stays (22 ms vs 150 ms); no `RTCPeerConnection` in workers; loss-injection and reset-survival exit criteria |
| S0c double-AEAD | `80fbe57bf` | `run.ps1 -Bench` 27/27 cells, same deltas | exporter stays deferred (+3.5 µs/1 KiB pkt hot, +0.21 ms/s at 60 Hz, +3–4.5 ms/MB bulk, no latency delta); `MAX_PAYLOAD_SIZE = 8108` silent drop → fragment there + `validate()` counter; batch events per packet |
| S0d + S0e inventories | `1a0504131` | citations spot-checked (`mesh.rs:39562`, `:24568`, `:29695–29703`, `:35421`, `router.rs:895`) | 49 send sites / 3 blocking shapes / 10 `PeerAddr` leak rows / `proxy.rs` decision; `PeerSink::{send, try_send, send_bounded}`; bounded third `IngressReceiver` variant; §12 allow-list A–E with whole-session bounds; rate-limit-bypassing corrective re-announce removed from the provisional path; nRPC-envelope decode precedes admission; F1–F7 forwarding sites gated; `connect_via`'s independent `relay_addr`/`dest_node_id` is the attack one `if` away; `RENEWAL_SERVICE = "net.mesh.renew"` |

Stage 0 exit criteria met; sizing inputs recorded in §Rough estimates.
Boundary unchanged: **Stage 1 requires its own authorization.**
