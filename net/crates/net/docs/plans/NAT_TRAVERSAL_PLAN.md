# NAT Traversal Plan

Give nodes behind NAT a path to direct peer connectivity — reflexive-address discovery, NAT-type classification, hole-punch rendezvous, opportunistic port mapping — while keeping the existing mesh-relay fallback as the safety net for the cases direct connectivity can't solve. Mesh-native throughout: no external STUN / TURN servers, no WebRTC ICE, no third-party signalling.

> **Framing.** NAT traversal in this codebase is a **latency / throughput optimization**, not a correctness requirement. Connectivity between two NATed peers already works today via routed handshakes + relay forwarding — every message reaches its destination regardless of NAT type. What this plan adds is a shorter path for the cases where a direct punch is feasible, reducing the per-packet relay tax and the load concentrated on topological relays. Everything below is safe to ship incrementally because the fallback path never goes away. Docstrings and READMEs written as part of the rollout must make this framing load-bearing: nothing in this design should read as "needed to talk to NATed peers" — it's "needed to talk to NATed peers *faster*."

## Context

The mesh already has two load-bearing pieces that a NAT-traversal design can lean on:

- **Routed handshakes.** A node with no direct UDP path to its peer completes a full Noise NKpsk0 handshake through already-connected relays (`mesh.rs::handle_routed_handshake`). This is the equivalent of TURN-through-the-mesh for the unreachable case, and it already works multi-hop. We never need to "fall back to TURN" because we're always implicitly on it.
- **Relay forwarding without decryption.** Forwarders route encrypted bytes end-to-end. Adding more relay-assisted primitives (rendezvous, reflex echo) doesn't erode the security posture — relays still see only routing headers.

The mesh also has dormant infrastructure for NAT awareness:

- `adapter::net::behavior::metadata::NatType` — five-way enum (`None | FullCone | RestrictedCone | PortRestrictedCone | Symmetric | Unknown`) with a `difficulty()` score and a `can_connect_direct()` helper. Exists; is not currently populated.
- `NodeMetadata.nat_type` field — storage exists; no writer ever updates it.

Nothing in the current codebase actively *detects* NAT type, discovers reflexive addresses, or attempts hole punching. Every connection between NATed peers today rides the routed-handshake fallback for its entire lifetime. That works, but:

1. Every packet traverses a relay, adding 1+ RTTs to every request.
2. Relays pay the forwarding cost for traffic that, with a punched path, wouldn't touch them.
3. Bandwidth-heavy daemons (inference streams, file transfer) concentrate load on whichever relay is topologically between the endpoints.

Direct paths when they're available would cut both latency and relay load, with relayed fallback staying as the catch-all.

## Goals

- Discover each node's own reflexive (public) address and classify its NAT type on first startup + periodically thereafter.
- Announce NAT type so peers can decide whether to attempt direct connection vs. skip to routed-handshake fallback.
- Hole-punch between two NATed nodes via a mutually-connected relay acting as rendezvous coordinator.
- Opportunistically request port mappings via UPnP-IGD / NAT-PMP / PCP when the gateway supports it, lifting the node to `NatType::None`.
- Route preference for relay-capable peers when no direct path exists, without mandating that any given node relay (autonomy stays intact).
- Parity across all four bindings (core, Node, Python, Go) at the SDK-visible layer (observable `nat_type`, explicit `attempt_direct_connect(peer)` API, stats for punch attempts / successes).

## Non-goals

- **External STUN / TURN servers.** No third-party dependencies for address discovery or relay fallback. The mesh is the STUN server and the TURN server.
- **WebRTC ICE.** No Candidate Pair tables, no priority algebra, no SDP. The proximity graph already answers "which path works"; we just need to *discover* that a direct path exists, and the graph does the rest.
- **Mandatory relaying.** Every node decides whether to serve as a relay through the same autonomy envelope that gates rate limits. A `relay-capable` capability tag is opt-in.
- **IPv6-only networks as a special case.** IPv6 without NAT is already `NatType::None`; the plan applies as-is. Dual-stack nodes probe both families.
- **Punching symmetric × symmetric.** Two symmetric NATs can't hole-punch reliably. These pairs stay on the routed-handshake path forever — we don't waste coordination budget trying.
- **Mobile carrier NAT churn compensation.** CGNAT + mobile networks rebind ports aggressively. The plan doesn't attempt to track rebinds mid-flow; a re-probe after connection loss is sufficient.

---

## Design decisions

### 1. Mesh-native reflex discovery, not STUN

The classic STUN protocol is a simple request/response that echoes the observer's source address. We implement the same semantics as a mesh subprotocol — any peer can answer a reflex probe with "I saw you at `ip:port`." Two+ probes to different peers are enough to detect symmetric NAT (the observed source port differs per destination).

**Decision:** `SUBPROTOCOL_REFLEX` (pick a free id in the 0x0D00 block — see *Subprotocol ID assignment* below). Request: empty body (source comes from the UDP header). Response: 18 bytes (`family: u8` + `addr: [u8; 16]` (IPv4 zero-padded into the low bytes) + `port: u16`).

**Alternative considered:** piggyback reflex on the existing heartbeat / pingwave. Rejected — heartbeats are peer-to-peer on an already-established session, so the source address they'd report is the session's cached one, not a fresh observation from another vantage point. Reflex needs unsolicited probes from the node's perspective.

### 2. NAT type classification via two-probe comparison

Classic STUN-style NAT classification (the NAT Behavior Discovery RFC 5780 version) needs two public IP addresses on the server side to detect each cone type. We don't have that — all our peers have single addresses from our perspective. What we *can* cheaply determine is symmetric vs. non-symmetric, which is the classification that actually matters for punching decisions:

- Probe peer A → observe reflexive address R_A.
- Probe peer B → observe reflexive address R_B.
- If R_A.port == R_B.port → non-symmetric (some cone variant, punching is likely).
- If R_A.port != R_B.port → symmetric (punching is unlikely; skip to relay).

**Decision:** collapse the classification to `{Open, Cone, Symmetric, Unknown}` at the wire level. `NatType`'s five-way enum stays as-is internally for richer reasoning later, but wire announcement uses the collapsed form.

**Alternative considered:** full RFC-5780 behavior-discovery with a dedicated test server. Rejected — the classification cost (multiple peers, multiple probes) isn't justified by the extra granularity for our use case.

### 3. Piggyback NAT type on the capability broadcast

`CapabilitySet` already has a tag set (`add_tag("gpu")`, `add_tag("prod")`, etc.). NAT type fits naturally as reserved tags: `nat:open`, `nat:cone`, `nat:symmetric`, `nat:unknown`. Placement and subnet derivation don't need NAT type; peer selection for hole-punch initiation does.

**Decision:** reserved tags on `CapabilitySet`. Nodes set exactly one `nat:*` tag; the tag is overwritten on re-classification. Tag syntax avoids carving out a dedicated subprotocol for a single 2-bit value.

**Alternative considered:** dedicated `SUBPROTOCOL_NAT_META`. Rejected — the propagation path is identical to capability broadcast (same TTL, same dedup, same signing), so duplicating the subprotocol infrastructure just to avoid tag reservation adds surface area for no gain.

**Tradeoff:** hitches NAT-type propagation to the capability broadcast cadence (default 10 s min-interval). For a NAT type that changes (mobile network reassignment), peers see the new tag on the next cap announcement. Acceptable because hole-punch decisions cache NAT type for longer than 10 s anyway.

### 4. Rendezvous-coordinated simultaneous open, not cold punching

Hole punching requires both endpoints to send a packet outbound "at the same time" so each NAT's connection-tracking table has an entry for the peer's address before the peer's packet arrives. A coordinator (any mutually-connected peer) exchanges addresses and nominates a target wall-clock instant.

**Decision:** `SUBPROTOCOL_RENDEZVOUS`. Three-message dance:

1. A → R: `PunchRequest { target: B_node_id }`.
2. R → B: `PunchIntroduce { peer: A_node_id, peer_reflex: A_reflex, fire_at: ts }`.
   R → A: `PunchIntroduce { peer: B_node_id, peer_reflex: B_reflex, fire_at: ts }`.
3. At `fire_at`, A and B send 3 keep-alive packets to each other's reflexive address (spaced 100 / 250 / 500 ms to cover clock skew).
4. Whichever side sees inbound first sends back a `PunchAck`; on ack, the Noise handshake continues over the punched path.
5. If no `PunchAck` within `fire_at + 5 s`, both sides give up and fall back to routed-handshake.

Clock skew tolerance: `fire_at` is ~500 ms in the future; NTP-level clock accuracy is sufficient. The 3-packet keep-alive train covers up to ~1 s of skew.

**Alternative considered:** Nostr-style relay-initiated open (R sends both sides the address and lets them each reach out at their own pace). Rejected — without synchronized firing, the first packet typically hits before the other side has opened a hole, and each side's retry loop has to discover the right retransmission cadence. The synchronized approach converges in one RTT if it converges at all.

### 5. Port mapping upcalls are best-effort side quests

UPnP-IGD, NAT-PMP, and PCP let a host ask its gateway to install a port forward. When it works, the host effectively becomes `NatType::None` for the duration of the mapping. When it doesn't (disabled on the router, no gateway on the path, firewall in the way), we fall back to stage 1–3 behavior.

**Decision:** opt-in at mesh-build time via `MeshBuilder::try_port_mapping(true)` (default off). On startup the node probes NAT-PMP first (short timeout, 1 s), then UPnP-IGD (2 s), skipping if the first succeeds. Mapping TTL renewal runs as a background task on a 60 s interval.

**Alternative considered:** on by default. Rejected — port mapping is a noticeable side effect (modifies external state on the user's router), and some environments actively forbid it (corporate LAN, public Wi-Fi). Opt-in matches the "every node enforces its own rules" invariant.

### 6. Relay-preference routing, not relay-forced routing

When A can't reach B directly and the existing `RoutingTable::lookup` returns multiple candidates, prefer ones that advertise `nat:open` or the `relay-capable` tag. This is a soft preference — if no relay-capable path exists, routing falls through to whatever the proximity graph says.

**Decision:** add an optional `prefer_relay_tag: Option<&str>` to `RoutingTable::lookup` paths that matter for unreachable pairs. Default behavior unchanged; hole-punch-failure handlers opt in.

**Alternative considered:** hard routing rule that forces all unreachable-pair traffic through declared relays. Rejected — violates node autonomy (a relay can be overloaded, withdraw its `relay-capable` tag, or simply refuse to forward) and would require mesh-wide coordination on which relays to use.

### 7. Piggyback the reflex address on capability announcements

Once a node has observed its own reflexive address via stage-1 probes, it publishes that address so peers can skip the round-trip on first contact. The two natural places are (a) a dedicated subprotocol broadcast, (b) an extension of the capability announcement envelope. Capability announcements already fan out multi-hop, already ride the origin's ed25519 signature, and already carry arbitrary tag data — adding one signed field costs no new subprotocol.

**Decision:** extend `CapabilityAnnouncement` with `reflex_addr: Option<SocketAddr>`. Included in the signed envelope so peers cannot forge a target's reflexive address to redirect punch traffic. Absent / `None` on nodes that haven't run classification yet, or where classification yielded `Unknown`. Rate-limited + dedup'd on the existing capability broadcast path.

**Alternative considered:** peer-to-peer lazy discovery (A probes B directly only when A decides to punch to B). Rejected — adds 1 RTT to every new punch target and needlessly hits random peers with reflex traffic when the information is cheaply propagatable. Piggybacking on an already-signed broadcast turns address discovery from O(N peers × probes) into O(1 announcement per node) on the network side.

**Wire cost:** 18 bytes per announcement (`family | addr | port`). Negligible against the envelope's existing signature overhead.

**Trust model:** the reflex address is advisory, not authoritative. The punch step still waits for a real keep-alive exchange on the advertised address before handing off to the Noise handshake. A lying node can only cause its own incoming punches to fail silently; it can't redirect traffic to a third party because the Noise prologue binds the target's `node_id` to the destination of every packet in the session.

### 8. Single punch attempt for symmetric × cone

Symmetric × cone is the partial-success case: the cone side's outbound port mapping is deterministic (same external port per local source port), so if the symmetric side happens to hash the peer's address into the same outbound port it used for the coordinator, the punch works. Retrying doesn't improve odds — the symmetric side's port mapping is deterministic per destination, not randomized per attempt — so a retry just delays the inevitable fallback to routed-handshake.

**Decision:** attempt the punch exactly once for symmetric × cone pairs. On failure (no `PunchAck` within the 5 s window), fall back to routed-handshake without retry. The SDK records the outcome in `traversal_stats` so operators can see the symmetric-NAT population directly.

**Alternative considered:** two attempts (first with the coordinator's observed reflex address, second with a fresh probe in case the symmetric side rebound). Rejected — rebinds only happen on timing events the client can't observe, so a naive retry at a short interval has the same success rate as the first attempt. Worth revisiting if real-world data shows a meaningful second-attempt hit rate.

**Cone × cone stays unchanged** (single attempt, high success rate). **Symmetric × symmetric skips punch entirely** (decision 4 — no point in coordinating a shot that can't land).

### 9. Rendezvous relay selection — relay-capable preferred, graceful degradation

A caller that wants to punch to B needs to pick a coordinator R. Always using "any mutually-connected peer" concentrates coordination load on whichever peer happens to bridge the most pairs. Always requiring `relay-capable` is too strict — early-mesh deployments might have no nodes advertising the tag, and refusing to even attempt a punch would violate the "optimization, not correctness" framing.

**Decision:** three-tier policy, graceful degradation:

1. **Preferred.** Among peers mutually connected to both A and B, pick one that advertises the `relay-capable` tag. If more than one, use random-two-choices (pick two at random, forward to the one with lower current coordination load from A's perspective). Avoids hot-spotting on a single relay-capable node.
2. **Fallback.** If no mutually-connected peer has `relay-capable`, pick any mutually-connected peer. Worse than a dedicated relay but still better than failing the optimization.
3. **Skip.** If no peer is mutually connected to both A and B, skip rendezvous entirely and fall through to routed-handshake. `connect()` still resolves — the punch just didn't get attempted.

**Never fail the connection** because rendezvous isn't available. Direct connect is an optimization; the routed-handshake path is the correctness guarantee (see the top-of-doc framing).

**Alternative considered:** strictly require `relay-capable`, failing with `rendezvous-no-relay` otherwise. Rejected — would make the optimization non-functional in any mesh without explicit relay-tagged nodes, which is the common early-deployment case.

### 10. Punch packet-loss resilience — single attempt, caller-driven retry

If all three keep-alive packets in the punch train drop, the punch fails. Options for handling: (a) internal retry loop with longer backoff, (b) single shot with caller-driven retry.

**Decision:** single attempt per `connect_direct()` call. Three keep-alives (100 / 250 / 500 ms) as already specified. If the train doesn't land a `PunchAck` within the 5 s window, mark the punch failed and immediately fall back to routed-handshake. If the caller wants a fresh attempt, they call `connect_direct` again — which runs a fresh rendezvous + a fresh punch, not a retry of the same failed window.

**Rationale:** an internal retry loop spends time + network budget chasing links whose lossiness is likely about to cause the session to fail anyway. The higher-level connect logic is in a better position to decide whether retrying at all is the right move (maybe the caller wants to back off to routed-handshake permanently, maybe it wants to re-probe reflex first). Keeping the primitive single-shot keeps the cost predictable.

**Alternative considered:** one internal retry after 2 s backoff. Rejected — doubles the worst-case punch-establishment latency with marginal success-rate improvement. If data shows retries actually help, a follow-up can add an opt-in `connect_direct_with_retry` without changing the base primitive.

### 11. IPv6 is not special — same code path, explicit test coverage

IPv6 usually doesn't need punching (most IPv6 stacks are `NatType::Open`), but CGNAT / NAT64 / 464XLAT deployments do apply v6-to-v4 rewrites that make punching as relevant as on IPv4.

**Decision:** no IPv6-specific code paths. Reflex probes, classification, rendezvous, and port mapping all handle `SocketAddr::V6` transparently — the `family: u8` byte in the wire format already discriminates. Stage 3 exit criteria add two explicit tests: (a) dual-stack peers with both on IPv6-open → direct connect, no punch attempted; (b) simulated NAT64/464XLAT pair → classification + punch behave identically to the IPv4 case.

**Rationale:** the temptation to write "v6 needs no NAT logic" special cases into the SDK or docs is the thing that breaks later when someone deploys behind CGNAT-v6. The explicit tests catch the "I assumed v6 is always open" regression.

### 12. Port-mapping lease on crash — accept the leak

UPnP / NAT-PMP / PCP mappings outlive the process if it crashes without sending `DeletePortMapping`. The mapping stays on the router's forwarding table until its TTL expires.

**Decision:** TTL 3600 s, renewal every 30 min, no aggressive TTL-shortening to compensate for crashes. A crashed process leaves one extra entry on the local router for up to an hour; every typical UPnP-using application does the same.

**Rationale:** the alternative is either (a) a tiny TTL with high renewal traffic (chewing router CPU for zero benefit in the common no-crash case) or (b) a supervisor that survives the crash to clean up (wildly out of scope). Leaking one entry for ≤ 1 hour is not an operational concern until someone produces evidence of a router whose NAT table overflows at that rate.

Revisit only if field data shows real router-table pressure.

### 13. `relay-capable` covers both forwarding and coordination

A node that advertises `relay-capable` is offering two distinct services: (a) data-plane forwarding for encrypted packets, (b) control-plane rendezvous coordination. They could be split into separate tags — but in practice a node that's willing to forward traffic is also willing to run a three-message rendezvous dance.

**Decision:** one tag (`relay-capable`) covers both. Internally the node keeps two rate-limit budgets so heavy data-forwarding pressure doesn't starve rendezvous coordination and vice versa — a loaded data relay can still introduce new punches cheaply, and a rendezvous-busy node can still carry its existing data forwarding.

**Future-friendly escape hatch:** if an operator shows up wanting "coordination only, no data forwarding," split into `relay-capable` + `rendezvous-capable` at that point. The single-tag default is fine until the split has a concrete use case.

**Alternative considered:** separate tags from the start. Rejected — extra configuration surface, extra capability-filter composition, for a distinction users don't currently need to make.

### 14. Classification re-run: explicit triggers only, never per-failure

A node's NAT type can legitimately change (mobile network transitions, VPN turning on, WiFi → cellular, interface flap). Re-classifying on every handshake failure sounds responsive but in practice turns transient per-peer issues into mesh-wide NAT-type flapping that the capability broadcast then re-announces at 10 s cadence. That's observable as NAT-type thrash in operator dashboards and triggers cascading re-evaluation across peers.

**Decision:** three triggers only, none of them per-failure:

1. **Startup.** Classify once on `MeshNode::start()`.
2. **Capability re-announce cadence.** If a scheduled capability re-announce is about to fire and the cached reflex from any anchor peer differs from the last observation, re-classify first.
3. **Explicit upcalls.** `mesh.reclassify_nat()` for tests + mobile-aware apps; the mesh also queues a re-classify internally on observable network-stack events (interface down/up, OS `ConnectivityManager` transitions where the platform exposes them) — coarse-grained and rare, not per-failure.

Handshake failures stay on the existing mesh-healing path (failure detector → reroute → routed handshake). Normal mesh resilience handles transient per-peer issues; NAT reclassification is for genuine topology shifts.

**Alternative considered:** auto-reclassify on any handshake failure. Rejected — converts per-peer flakiness into mesh-wide NAT flapping, and the signal is noisy enough that the classifier would re-run on every lossy link without actually changing its output.

---

## Stage 0 — Scaffolding

New module: `adapter::net::traversal`. Single parent for all NAT-traversal surface so future growth (IPv6-specific heuristics, WebRTC-DataChannel bridge, etc.) has a clear home.

```
adapter/net/traversal/
├── mod.rs          — pub use; SUBPROTOCOL_* constants
├── reflex.rs       — reflex probe sub-protocol handler
├── classify.rs     — NatType classification state machine
├── rendezvous.rs   — hole-punch coordinator
├── portmap.rs      — UPnP / NAT-PMP / PCP client (behind feature)
└── config.rs       — TraversalConfig (probe cadence, timeouts, ...)
```

Subprotocol ID assignment (next free block is `0x0D00`):

| ID       | Name                      |
|----------|---------------------------|
| `0x0D00` | `SUBPROTOCOL_REFLEX`      |
| `0x0D01` | `SUBPROTOCOL_RENDEZVOUS`  |
| `0x0D02` | `SUBPROTOCOL_PORTMAP_META` *(optional, stage 4 only if we decide to announce mapping TTL over the wire)* |

Feature flags added to `net/crates/net/Cargo.toml`:

```toml
[features]
nat-traversal = ["net"]           # reflex + classify + rendezvous (stages 1–3)
port-mapping = ["nat-traversal", "dep:igd-next", "dep:rust-natpmp"]  # stage 4
```

Keeps the core `net` feature minimal — consumers who don't need NAT traversal (LAN-only testbeds, fully-public nodes) don't pay compile / link cost.

---

## Stage 1 — Reflex probe subprotocol

### Wire format

```rust
// SUBPROTOCOL_REFLEX = 0x0D00
// Request body: empty (the source address is the echo target).
// Response body: 18 bytes.

#[repr(C, packed)]
pub struct ReflexResponse {
    pub family: u8,      // 4 or 6
    pub addr: [u8; 16],  // IPv4 zero-padded into the low 4 bytes
    pub port: u16,       // network-byte-order
}
```

Requests ride as regular mesh packets; the response is unicast back to the requester using the source address of the request (not the requester's claimed `node_id` → address mapping, which would defeat the purpose).

### Handler

`ReflexHandler::process_inbound(src_sockaddr, payload)` — one-liner: marshal the src socket address into a `ReflexResponse` and send it back. Stateless.

### Client

`MeshNode::probe_reflex(peer_node_id: u64) -> impl Future<Output = Result<SocketAddr, TraversalError>>` — sends one reflex request to the peer, awaits the response (timeout 3 s), returns the observed address.

### Exit criteria

- `probe_reflex(peer)` returns the actual public `ip:port` of the requesting node when run through a real NAT, and `ip:port` == bind address when no NAT is present.
- Two-node test: node A runs behind a deterministic NAT simulator (or on a different interface), probes node B, gets back its NAT-rewritten address.
- The new subprotocol is idle in steady state — only fires on explicit `probe_reflex` calls.

---

## Stage 2 — NAT type classification

### State machine

```rust
pub enum NatType {
    Open,       // reflexive == bind (no NAT) OR port-mapping installed
    Cone,       // reflexive.port consistent across different destinations
    Symmetric,  // reflexive.port varies per destination
    Unknown,    // classification inconclusive / not run
}

pub struct ClassifyFsm {
    probes: Vec<(NodeId, SocketAddr)>,  // at most 3 probes held
    result: Option<NatType>,
}

impl ClassifyFsm {
    fn observe(&mut self, peer: NodeId, reflex: SocketAddr);
    fn classify(&self) -> NatType;
}
```

Classification rule:

- If `bind_addr == reflex_addr` for any probe → `Open`.
- Else if all observed ports match → `Cone`.
- Else → `Symmetric`.
- If fewer than 2 probes → `Unknown`.

### Trigger

Classification runs on `MeshNode::start()`:

1. Pick 2 random peers from the handshake table (skip if < 2).
2. Fire `probe_reflex` to each in parallel; wait up to 5 s total.
3. Feed results into `ClassifyFsm`, write result to `NodeMetadata.nat_type`.
4. Add one of `nat:open` / `nat:cone` / `nat:symmetric` / `nat:unknown` to the local `CapabilitySet`.
5. Store the observed reflex address on the local `CapabilityAnnouncement` builder (see decision 7 — the `reflex_addr` field).
6. Trigger a fresh `announce_capabilities` so peers see both the new tag and the new reflex address in one round-trip.

Re-classification triggers (locked by decision 14 — no per-handshake-failure path):

- **Capability re-announce cadence.** Default every 10 s min-interval; if the cached reflex address from any anchor peer differs from the last observation at re-announce time, re-classify first so the new tag + reflex ship together.
- **Explicit upcall.** `mesh.reclassify_nat()` — called by mobile-aware apps and integration tests.
- **Coarse network-stack events.** Interface down/up or platform `ConnectivityManager` transitions (Android, iOS, Win32 NLM) queue one re-classification. Never per-peer handshake failure — that path stays on the existing mesh-healing reroute logic to avoid NAT-type flapping under transient per-link loss.

### Reflex address on the wire

`CapabilityAnnouncement` grows one optional field:

```rust
pub struct CapabilityAnnouncement {
    // ... existing fields ...
    pub reflex_addr: Option<SocketAddr>,
}
```

Encoding: 1 byte presence flag + 18 bytes address when present (`family: u8 | addr: [u8; 16] | port: u16`) = 19 bytes in the signed envelope when populated, 1 byte when absent. Absence means "node hasn't run classification yet" or "classification returned Unknown." Peers treat absence as "probe lazily on first punch target," present as "use this as the initial target for stage-3 rendezvous, no per-target probe needed."

### Exit criteria

- `mesh.node_metadata().nat_type` populates within 5 s of `start()` when ≥ 2 peers are handshaken.
- Capability announcements after start carry exactly one `nat:*` tag and, when classification succeeded, the `reflex_addr` field.
- `find_nodes(CapabilityFilter::new().require_tag("nat:open"))` returns relay-capable candidates.
- A fresh joiner observing its first capability announcement from a classified peer can initiate a rendezvous punch without emitting its own `probe_reflex` to that peer first.

---

## Stage 3 — Hole-punch rendezvous

### Wire format

```rust
// SUBPROTOCOL_RENDEZVOUS = 0x0D01
// Request types (discriminator byte + body):

#[repr(u8)]
pub enum RendezvousMsg {
    PunchRequest(PunchRequest)     = 0x01,
    PunchIntroduce(PunchIntroduce) = 0x02,
    PunchAck(PunchAck)             = 0x03,
}

pub struct PunchRequest {
    pub target: u64,          // peer the requester wants to punch to
    pub self_reflex: SocketAddr,  // requester's currently-believed reflexive address
}

pub struct PunchIntroduce {
    pub peer: u64,
    pub peer_reflex: SocketAddr,
    pub fire_at_unix_millis: u64,
}

pub struct PunchAck {
    pub peer: u64,
    pub punch_id: u32,  // request correlation; echoed from `PunchRequest`
}
```

### Coordinator (R's role)

On `PunchRequest { target, self_reflex }` from A:

1. Look up target's reflexive address. Preferred source: the `reflex_addr` field on the latest signed `CapabilityAnnouncement` from `target` in R's local cache (decision 7). Secondary: R's own past `PunchIntroduce` to this pair. If neither available, reject with a typed error and the caller falls back to routed-handshake.
2. Pick `fire_at = now() + 500 ms`.
3. Send `PunchIntroduce` to both A and B with the other's reflex and the shared `fire_at`.

### Endpoints (A's and B's role)

On receipt of `PunchIntroduce { peer, peer_reflex, fire_at }`:

1. Schedule 3 keep-alive sends to `peer_reflex` at `fire_at`, `fire_at + 100 ms`, `fire_at + 250 ms`.
2. Arm a 5 s timer: if no inbound packet from `peer_reflex` arrives, declare punch failed.
3. On first inbound packet from `peer_reflex`, send `PunchAck` via routed path (not the punched one — we don't know yet if it's reliable) and begin Noise handshake on the punched path.

### Connect-time pair-type matrix

`connect(peer)` uses the cached NAT classifications from capability announcements to pick the path:

| Local → | Remote → `Open`     | Remote → `Cone`        | Remote → `Symmetric` |
|---------|---------------------|------------------------|----------------------|
| `Open`  | Direct, no punch    | Direct, no punch       | Single-shot punch (decision 8) |
| `Cone`  | Direct, no punch    | Single-shot punch      | Single-shot punch (decision 8) |
| `Symmetric` | Single-shot punch (decision 8) | Single-shot punch (decision 8) | Skip punch, routed-handshake only |
| `Unknown` | Direct attempt, fall back on first-packet failure | Single-shot punch | Skip punch, routed-handshake |

"Single-shot" means one rendezvous round — no retry on punch failure. Record the outcome in `traversal_stats`; the caller's upper-layer retry loop (if any) handles reconnection.

### SDK surface

```rust
impl MeshNode {
    /// Attempt a direct connection via rendezvous punch. Falls back
    /// to routed-handshake on failure — connectivity is always
    /// available via the relay path, the punch is a latency
    /// optimization.
    pub async fn connect_direct(&self, peer: NodeId) -> Result<(), MeshError>;
}
```

Default behavior for an ordinary `connect()`: consult the pair-type matrix above. Under the hood `connect()` always yields a working session — via punch if feasible, via routed-handshake if not.

### Exit criteria

- Three-node integration test: A behind NAT1, B behind NAT2, R reachable by both. `A.connect_direct(B)` completes. Inspect the resulting session: `peer_addr()` is the punched socket, not the relay's.
- Symmetric × symmetric test: `connect_direct` short-circuits to routed-handshake without attempting a punch (`traversal_stats.punches_attempted` stays 0, `relay_fallbacks` increments).
- Symmetric × cone test: exactly one punch attempt. `traversal_stats.punches_attempted == 1` regardless of outcome; on failure `relay_fallbacks == 1`.
- Punch-failure test (dropped keep-alives, cone × cone): 5 s timer fires, routed-handshake takes over, `connect()` still resolves.
- Pre-announced reflex test: joiner C receives A's capability announcement carrying `reflex_addr`, then calls `connect_direct(A)` — the rendezvous succeeds without C ever having emitted a `probe_reflex` to A first.

---

## Stage 4 — Port mapping (UPnP / NAT-PMP / PCP)

Gated behind the `port-mapping` cargo feature. Adds two dependencies:

- `igd-next` — UPnP-IGD control point.
- `rust-natpmp` — NAT-PMP + PCP (they share a wire format).

### Behavior

`PortMapper` is a tokio task spawned by `MeshNode::start()` when `MeshBuilder::try_port_mapping(true)` is set:

1. Read the default gateway address from the routing table.
2. Fire NAT-PMP probe (1 s timeout). On success: request a mapping for the mesh's bind port, TTL 3600 s.
3. On NAT-PMP failure: fire UPnP SSDP probe (2 s timeout). On success: issue `AddPortMapping` for the mesh's bind port.
4. On success: record the external `ip:port`; call `mesh.set_reflex_override(external)` which forces `NatType::Open` and caches the mapping for reflex responses.
5. Background renewal task: every 30 min, re-issue the mapping (both protocols renew on re-request).
6. On mesh shutdown: `DeletePortMapping` if the mapping is still alive.

### Config

```rust
impl MeshBuilder {
    /// Enable UPnP-IGD / NAT-PMP port mapping at startup. Off by
    /// default because it modifies external state (the user's
    /// router) and some environments forbid it.
    pub fn try_port_mapping(self, enabled: bool) -> Self;

    /// Optional hard override — skip auto-probe, use this mapping.
    /// Useful for port-forwarded servers where the mapping is
    /// manually configured.
    pub fn reflex_override(self, external: SocketAddr) -> Self;
}
```

### Exit criteria

- On a LAN with a UPnP-enabled router, startup completes with `NatType::Open` and the mesh's `reflex_addr()` matches the router's external IP.
- On a LAN with UPnP disabled, startup continues (doesn't hang) and falls through to stage 2 classification.
- Graceful shutdown removes the mapping from the router's table.

---

## Stage 5 — SDK + binding surface

Symmetric across Rust, TS, Python, Go. Each binding exposes:

- `mesh.nat_type() -> "open" | "cone" | "symmetric" | "unknown"` (getter).
- `mesh.reflex_addr() -> Option<String>` (the observed / mapped public address).
- `mesh.probe_reflex(peer_node_id) -> Promise<string>` (test/debugging).
- `mesh.connect_direct(peer_node_id)` (non-default; ordinary `connect()` picks punch vs. routed automatically per the pair-type matrix).
- `mesh.traversal_stats() -> { punches_attempted, punches_succeeded, punches_failed, relay_fallbacks, port_mapping_active }`.

### Docstring framing (load-bearing, per the top-of-doc note)

Every user-visible docstring added as part of this stage — `nat_type`, `reflex_addr`, `connect_direct`, `traversal_stats`, the `TraversalError` class, each binding's README section — must position NAT traversal as **optimization, not correctness**. A sample phrasing to reuse:

> Nodes behind NAT can always talk to each other through the mesh's routed-handshake path. These APIs let the mesh upgrade to a **direct** path when the underlying NATs allow it, cutting relay hops out of the data plane. A `nat_type` of `symmetric` or a `punch-failed` error is not a connectivity failure — it just means traffic keeps riding the relay.

Anti-phrasings to avoid in docs:

- "Required for NATed peers to communicate."
- "Enables cross-NAT connectivity."
- "Fixes NAT issues."

Each of these implies the mesh otherwise can't reach NATed peers, which is false. Reviewers should treat these as language bugs on the same severity as an API-signature mistake.

### Error surface

New typed error variants on the mesh-error family:

```rust
pub enum TraversalError {
    ReflexTimeout,
    RendezvousNoRelay,
    RendezvousRejected,
    PunchFailed { reason: PunchFailureReason },
    PortMapUnavailable,
    Unsupported,  // peer doesn't advertise nat-traversal capability
}
```

Kind vocabulary for cross-binding parity (TS / Python / Go map to classes with a `kind` discriminator, same pattern as `MigrationError` and `GroupError`):

| Kind                     | Meaning                                                      |
|--------------------------|--------------------------------------------------------------|
| `reflex-timeout`         | reflex probe didn't complete in time                         |
| `rendezvous-no-relay`    | no mutually-connected relay found                            |
| `rendezvous-rejected`    | relay refused to coordinate (rate-limit / unknown target)    |
| `punch-failed`           | keep-alive train didn't establish a path                     |
| `port-map-unavailable`   | UPnP/NAT-PMP/PCP all failed                                  |
| `unsupported`            | peer doesn't advertise traversal capability                  |

### Exit criteria

- `nat_type()` returns a stable classification on all four bindings within 5 s of a handshaken mesh.
- `connect_direct(peer)` resolves the punched path where possible and falls back cleanly otherwise.
- Stats reflect real attempts — a test that forces a cone↔cone punch sees `punches_succeeded > 0`; a test behind symmetric NAT sees `relay_fallbacks > 0` and `punches_attempted == 0`.

---

## Critical files

### Stages 1–3 (core Rust)

- `adapter/net/traversal/{reflex,classify,rendezvous,config}.rs` — new module.
- `adapter/net/subprotocol/mod.rs` — register `SUBPROTOCOL_REFLEX` / `SUBPROTOCOL_RENDEZVOUS` dispatchers.
- `adapter/net/mesh.rs` — plumb classification trigger into `start()`; extend `connect()` to consider direct-punch first.
- `adapter/net/behavior/capability.rs` — reserve the `nat:*` tag namespace; document it.
- `adapter/net/behavior/metadata.rs` — wire up the `NatType` writer path (currently an orphaned field).

### Stage 4 (port mapping)

- `adapter/net/traversal/portmap.rs` — UPnP / NAT-PMP / PCP client.
- `Cargo.toml` — `port-mapping` feature + deps (`igd-next`, `rust-natpmp`).
- `adapter/net/mesh.rs` — `MeshBuilder::try_port_mapping`, `MeshBuilder::reflex_override`, spawn `PortMapper` task.

### Stage 5 (SDK + bindings)

- `sdk/src/mesh.rs` — `Mesh::nat_type()` / `reflex_addr()` / `connect_direct()` / `traversal_stats()` / `probe_reflex()`.
- `sdk/src/error.rs` — `TraversalError` variants.
- `bindings/node/src/lib.rs` — NAPI exports + typed `TraversalError` class.
- `bindings/python/src/lib.rs` — PyO3 exports + `TraversalError` exception class.
- `bindings/go/net/traversal.go` + `bindings/go/compute-ffi` or a new `bindings/go/traversal-ffi` crate — extern "C" surface + Go wrappers.
- `sdk-ts/src/mesh.ts` / `sdk-py/` wrappers as appropriate.

---

## Implementation status

Current state against the staging plan:

| Stage | Scope                                       | Status       |
|-------|---------------------------------------------|--------------|
| 0     | Module scaffolding, feature gates           | **done**     |
| 1     | Reflex probe subprotocol (`SUBPROTOCOL_REFLEX = 0x0D00`) | **done** |
| 2     | NAT classification FSM, `reflex_addr` on capability announcements, background classifier loop | **done** |
| 3a    | Rendezvous wire format (`PunchRequest / PunchIntroduce / PunchAck`) | **done** |
| 3b    | Coordinator fan-out — resolves peer reflex, sends `PunchIntroduce` to both endpoints | **done** |
| 3c    | Pair-type matrix, `connect_direct`, `TraversalStats` | **done** |
| 3d    | Keep-alive train + observer + `PunchAck` round-trip (routed via coordinator) | **done** |
| 4a    | `reflex_override` — config field + runtime `set_reflex_override` / `clear_reflex_override` across all 4 surfaces | **done** |
| 4b    | UPnP-IGD / NAT-PMP client + renewal task | **done** |
| 5     | SDK + NAPI + PyO3 + Go binding surface (nat_type / reflex_addr / probe_reflex / connect_direct / traversal_stats / reflex_override) | **done** |

**Stage 4b landing notes.** Implemented per [`PORT_MAPPING_PLAN.md`](PORT_MAPPING_PLAN.md): `PortMapperClient` trait, `NatPmpMapper` (inlined RFC 6886 wire codec with `UdpSocket::connect` source-address filter), `UpnpMapper` (`igd-next`-backed), `SequentialMapper` composer, and a `PortMapperTask` lifecycle state machine driving probe → install → renew → revoke. Install calls `set_reflex_override(external)`; renewal failures clear it via `clear_reflex_override()`. Surface-wide binding parity for the `try_port_mapping` flag across SDK / NAPI / PyO3 / Go. PCP is explicitly out of scope (§PORT_MAPPING_PLAN non-goals — different wire format from NAT-PMP despite the shared port).

Testing coverage summary (as of stage 5 / 4a):

- **33 NAT-traversal integration tests** across 7 files in `crates/net/tests/`: reflex probe, classification, rendezvous coordinator, rendezvous ack round-trip, connect_direct orchestration, keep-alive observer, reflex override. Plus 7 SDK-level integration tests in `sdk/tests/mesh_nat_traversal.rs`.
- **45 new unit tests** across the traversal module: classify FSM + pair-action matrix (19), rendezvous wire codec (13), reflex probe codec (7), TraversalConfig defaults (1), capability-index reflex storage (2), capability `reflex_addr` serde round-trip (3).
- All 987 lib tests pass; no regressions from any stage in this rollout.
- Clean compile across no-default-features, `net`, `net,nat-traversal`, `port-mapping` feature combos.

---

## Open questions

None — the initial design review resolved all of them. Decisions captured in the numbered design decisions section above (specifically §9–§14 correspond to the formerly-open rendezvous / retry / IPv6 / port-mapping-lease / relay-tag / reclassification questions).

Future open questions land here as implementation reveals them.

---

## Rough estimates

| Stage | Surface        | Complexity     | Estimate |
|-------|----------------|----------------|----------|
| 1     | Reflex probe   | Small          | ~1 day   |
| 2     | Classification | Small          | ~1 day   |
| 3     | Rendezvous + punch | Medium–large (state machine, integration test harness) | ~3–4 days |
| 4     | Port mapping   | Medium (two external protocols + renewal task) | ~2 days  |
| 5     | SDK + 4 bindings | Medium (same typed-error pattern across bindings) | ~2–3 days |

Total: ~9–11 days serial. Parallelizable across people for stages 3/4/5.

---

## Dependencies

- `igd-next` (≈150 KB, MIT licensed, actively maintained) — UPnP-IGD.
- `rust-natpmp` (≈50 KB, MIT, sparse maintenance but stable wire format) — NAT-PMP + PCP.

Both feature-gated under `port-mapping`. Stages 1–3 have no new deps.

---

## Out of scope (for this plan)

- TURN-over-TLS / TURN-over-TCP fallback for DPI'd networks (the mesh's own UDP transport is assumed).
- Browser-compatible WebRTC bridge (needs separate ICE / DTLS / SRTP plumbing — different problem).
- Persistent hole-punch state across mesh restarts. The node re-probes and re-classifies on every startup; no disk state.
- Reflex-address signing. The reflex response carries the observer's signature on nothing today; an attacker in the forwarding path could rewrite it. A future extension can add a signed observation if we find a threat model that demands it — for now, the mesh's own identity authentication means a lie only lasts until the punch fails.
