# Net v0.32 — "Summer Madness"

*Named after Kool & the Gang's 1974 instrumental from Light of Worlds — a Moog-soaked slow burn with no vocal. All groove, no words: the track carries everything a lyric would.*

Four tracks land, all in the network layer:

- **NAT traversal** — reflex-address discovery, NAT-type classification, hole-punch rendezvous, and opportunistic port mapping give two NATed peers a *direct* path, with the routed-relay fallback kept as the correctness floor it always was.
- **The punch** — the direct path stops needing hand-orchestration: a background upgrade lifts a live relayed session onto a punched one when the pair allows, behind coordinator auto-selection, rendezvous rate-budgets, typed rejections, and a session-migration contract that can't corrupt in-flight work — plus a netns NAT-simulator harness that finally exercises the punch against real NAT behavior.
- **Real-time routing & discovery** — a capability change or a topology change propagates mesh-wide in one debounce plus one flood instead of waiting 90–150 s on a timer: change-driven announcements, event-triggered pingwaves, and origin-scoped route withdrawal. Push for latency, gossip for reliability.
- **Node/TS Hermes parity** — delegation chains, device enrollment, and agent-to-agent task handoff reach Node/TS, closing the one piece v0.31 left on the deferred list.

The organizing observation is the mirror of last cycle's. Where v0.31 was *an adapter over infrastructure that already existed*, v0.32 is **a fast path layered over a correctness path that never moves.** A punch that fails drops to the relay. A push that's dropped inside the rate-limit window is still delivered by the anti-entropy gossip. A session swap that can't land leaves the relayed session byte-for-byte intact. A binding that ships is a marshaling layer over a Rust lifecycle that already worked. Nothing below changes what the mesh *guarantees* — it changes how fast the mesh *reaches* the guarantee. The timers aren't deleted; they're demoted to the floor.

---

## NAT traversal — a direct path, never a new dependency

Connectivity between two NATed peers has always worked. A node with no direct UDP path completes its full Noise handshake through already-connected relays and rides that routed path for the session's life — the mesh is its own TURN, multi-hop, no external server ever involved. That path is not going anywhere. What it costs is latency and relay load: every packet takes an extra hop, and bandwidth-heavy daemons concentrate their traffic on whichever relay sits between the endpoints.

The NAT Traversal work adds the shorter path for the cases a direct punch is feasible. It is, load-bearingly, **an optimization and not a correctness requirement** — every docstring and guide added this cycle says so, and reviewers were told to treat "enables cross-NAT connectivity" as a language bug on par with a wrong API signature. A `nat_type` of `symmetric`, or a `punch-failed` error, is not a connectivity failure; it just means the traffic keeps riding the relay.

- **Reflex discovery, mesh-native.** A new `SUBPROTOCOL_REFLEX` echoes a probing node its own public `ip:port` — the same semantics as STUN, implemented as a mesh subprotocol against the peers the node already talks to. No third-party STUN or TURN, no WebRTC ICE, no SDP. The mesh is the STUN server.
- **Classification from two probes.** Two probes to different peers are enough to separate the case that matters: same observed port across destinations means a punchable cone, differing ports mean symmetric. The wire form collapses to `{ open, cone, symmetric, unknown }`, rides the capability broadcast as a reserved `nat:*` tag, and the observed reflex address piggybacks on the signed announcement so a fresh peer can target a punch without a round-trip of its own — forgeable reflexes are ruled out because the address travels inside the origin's existing signature.
- **Hole-punch rendezvous.** `SUBPROTOCOL_RENDEZVOUS` runs a three-message dance — a punch request to a mutually-connected coordinator, a synchronized introduce to both endpoints naming a shared `fire_at`, and a keep-alive train timed to open both NATs' mappings before the peer's packet arrives. Whichever side sees inbound first acks, and the Noise handshake continues over the punched socket. Symmetric × symmetric never attempts a punch (it can't land); the punchable pairs get a single deterministic shot and fall back to the routed handshake on miss.
- **Opportunistic port mapping.** Behind the `port-mapping` feature, a node asks its gateway for a forward over NAT-PMP or UPnP-IGD; on success it becomes `open` for the mapping's life, with background renewal and best-effort teardown. Off by default — it changes state on the user's router, and some networks forbid it. (PCP stays out of scope: same UDP port, different wire format.)
- **Parity across the native bindings.** `nat_type`, `reflex_addr`, `probe_reflex`, `connect_direct`, `traversal_stats`, and `reflex_override` land on Rust, Node, Python, and Go, gated behind an opt-in `nat-traversal` build feature so LAN-only and fully-public nodes pay no compile or link cost.

---

## The punch — and the review that made it safe

The first NAT Traversal cut shipped the machinery but left the punch dormant: it only fired when an application hand-supplied a coordinator and called `connect_direct` itself. Traffic between NATed peers stayed on relays unless something orchestrated a punch by hand. The **NAT Traversal V2** work closes that gap and hardens the control plane a code review flagged as sitting at exactly the seam an attacker wants.

- **The control plane, hardened.** An unsolicited introduce is now validated against the peer's announced reflex before any keep-alive train fires — an authenticated session peer can no longer steer a node's packets at a third-party address for reflection. Two independent rendezvous budgets (per-requester on coordination, per-source plus a global concurrent cap on responder trains) bound the abuse a Sybil set of session peers can extract, and a new `PunchReject` message turns what used to be a silent 5-second timeout into an immediate typed rejection — the `RendezvousRejected` / `RendezvousNoRelay` error kinds, long defined and never constructed, now come alive across all four bindings.
- **Background direct-path upgrade.** When a session is established over a relay and the pair-type matrix says a punch is worth trying, the mesh schedules a background upgrade: auto-select a coordinator (the relay currently forwarding is the highest-probability choice, then a `relay-capable` peer, then any mutual peer, then skip), run the punch, and migrate the live session onto the punched socket on success. The data plane never waits on a punch — first-byte latency over the relay is identical to before, and a successful upgrade drops the relay tax mid-session. It ships **off by default** (`auto_direct_upgrade`), opt-in per deployment pending validation against the real-NAT harness; the automatic upgrade currently covers directly-reachable pairs, with the coordinated-punch upgrade a tracked follow-up.
- **A session-migration contract, not a detail.** Swapping a live session's keys mid-flight is a contract, and V2 pins it against verified behavior. Only the lower-node-id end of a pair initiates, killing the crossing-handshake race at the source; the install is compare-and-swap-guarded so a racing handshake wins cleanly instead of being clobbered; a two-sided busy gate defers the swap while streams are open or reliable bytes are in flight; and failure atomicity is a pinned regression test — a punch whose handshake fails leaves the relayed session byte-for-byte intact. Pending unary nRPC calls survive a swap by design; only in-flight stream bytes gate it.
- **Tested against real NAT.** A network-namespace NAT-simulator harness (nftables masquerade, cone via persistent mapping and symmetric via fully-random) drives the punch across two distinct public IPs on a Linux CI job — the first time the headline capability is exercised against actual NAT behavior rather than a loopback simulation. The loopback matrix (symmetric × cone exactly-once, the pre-announced-reflex path) is verified; the netns half is authored and wired to CI, awaiting its first run.
- **Observability parity.** The full traversal-stats snapshot — punch outcomes, failure-cause counters, upgrade counters, and port-mapping state — is now identical across Rust, Node, Python, and Go over a versioned FFI struct that keeps the old ABI stable, and a reflex-diff check re-classifies at re-announce time when a node's observed address drifts. One parity fix rode along: the Node and Python `start()` now enters the Arc-based startup the FFI and Rust SDK already used, without which the re-announce and upgrade loops never ran for those bindings.

---

## Real-time routing & discovery — push for latency, gossip for reliability

The local watch surfaces already push — a node's `watch_tools` fires within microseconds of its local fold changing. The staleness that remained lived entirely in cross-node propagation, which was still timer-paced: a capability change could take up to 150 s to reach peers, a new session's routes waited on the 5 s heartbeat tick, and a dead peer's indirect routes aged out only after 90 s. The **Real-Time Routing & Capability Discovery** work makes propagation event-driven, on primitives that already existed — the fold change signal, the pingwave's spare capability fields, the signed capability flood — inventing nothing new. **No timer is deleted; every timer stops being the primary path and becomes an anti-entropy backstop.**

- **Change-driven announcements.** A dedicated local-origin change signal — bumped only by local mutations, never by applying a peer's inbound announcement, so there's no echo storm — drives a debounced announcer that coalesces a burst (a service registering twenty tools at startup) into one broadcast. The announce rate limiter's trailing edge is fixed at the same time: a change landing inside the window is now deferred to the window's end rather than silently dropped until the next keep-alive. The 150 s re-announce loop stays, demoted to pure TTL keep-alive.
- **Event-triggered pingwaves.** Topology now propagates at flood speed: a pingwave is emitted immediately on session open, on failure-detector recovery, and on a local capability-version bump, debounced by a per-node minimum gap so churn coalesces. Same 72-byte wire format, same receive path — just extra emission sites, with the 5 s heartbeat tick left as the anti-entropy floor.
- **Route withdrawal, scoped and safe.** A new `SUBPROTOCOL_ROUTE_WITHDRAW` turns the 90 s dead-route hole into a sub-second one. When a node's failure detector transitions a direct peer to `Failed`, it floods a poison-reverse withdrawal — "that destination is unreachable *via me*" — and receivers drop exactly the routes whose next hop is the sender, then run the existing reroute policy to promote an alternate instead of waiting for traffic to fail. The scoping is what keeps it safe without new crypto: a node is authoritative only about its own forwarding, and the emission is gated to handshaked peers, so a malicious peer can only poison routes that already went through it. It rides a *new* subprotocol rather than the pingwave's spare `health` field precisely so a mixed-version mesh degrades to today's age-out instead of degrading to wrong — old nodes drop the message and keep the 90 s behavior. Withdrawal fires only on `Failed`, never on `Suspected`, and a false positive heals in one recovery flood.

The remote-watch tail — an nRPC server-streaming `watch_tools` subscription and the Go RPC binding's cutover off its 1 s poll — is scoped here but stays deferred; with the tracks above landed, any node's local fold is already near-real-time mesh-wide, which makes remote streaming a thin-client convenience rather than a correctness need.

---

## Node/TS Hermes parity — closing the v0.31 deferral

v0.31 shipped Hermes-native identity — device enrollment, delegation chains, and agent-to-agent task handoff — in Rust and Python, and left exactly one item on its deferred list: the Node/TS surface for the same three subsystems. The **Node/TS delegation + A2A** work delivers it. Node/TS now gains delegated agent identity (`DelegationChain`, `RevocationRegistry`, child-identity derivation), the invite → join → approve device-enrollment handshake, and serve / submit / status / cancel A2A task handoff — the last Hermes gap versus Python, closed.

It is a port, not a new mechanism. Every subsystem is decided in the one Rust SDK both bindings wrap; each new Node surface is a napi marshaling layer that decides nothing and holds no key material. The non-custodial line holds at the Node edge: child-identity derivation returns an `Identity` handle, never key bytes, and enrollment exchanges invite and join tokens, not keys. Two honest divergences are documented rather than papered over — several methods that are synchronous in Python became async in Node (napi's synchronous calls have no tokio runtime context, and store IO belongs off the JS thread), and A2A cancellation is one-sided (a JS Promise can't be aborted from outside, so a cancel discards the handler's eventual result and records `Cancelled`), with a handler timeout guarding a wedged event loop from stranding an accepted task forever. The full Node suite passes green. Go and C stay out of scope — there is no Hermes surface there, consistent with the payments matrix.

---

## The docs

The NAT-and-traversal guide is updated for the shipped traversal-stats shape and the background-upgrade behavior, and every user-visible traversal docstring is written to the "optimization, not correctness" framing the plan makes load-bearing — a symmetric NAT or a failed punch reads as "traffic keeps riding the relay," never as a broken connection. The discovery-guide rewrite that replaces the recommended poll-until-appears loop with a `watchTools` subscription rides with the deferred remote-watch streaming tail.

---

## What's deferred (honestly)

- **The automatic direct-path upgrade ships off by default.** `auto_direct_upgrade` is opt-in per deployment; flipping the default waits on the real-NAT harness proving it out in CI. The automatic upgrade covers directly-reachable pairs today — the coordinated-punch (through-NAT) upgrade reuses the same install machinery and is a tracked follow-up.
- **The netns NAT harness awaits its first CI run.** It was authored on a macOS box against a Linux-only netns topology; the loopback halves are verified locally, the namespace halves await the CI job. The IPv6 and NAT64/464XLAT scenarios are documented but not yet wired (the translator needs tooling in the runner image).
- **`sdk-ts` / `sdk-py` NAT wrappers, and the CLI NAT/port verbs.** The native bindings carry the full traversal surface; the higher-level TS and Python SDK wrappers expose zero NAT APIs, and the `peer nat/reflex/*` and `port` CLI verbs remain design stubs. Punch-id correlation and platform interface-change re-classification triggers are deferred with them.
- **Remote-watch streaming.** The nRPC server-streaming `watch_tools` subscription and the Go RPC binding's cutover off its 1 s poll are scoped but unscheduled; the Go remote `WatchTools` still polls until it lands.
- **Symmetric-NAT punching.** Symmetric × symmetric never attempts a punch by design, and birthday-paradox / port-prediction punching is out of scope pending telemetry showing a symmetric population with real punch demand.
- **Signed reflex observations.** A reflex response carries no signature; a forwarding-path attacker's lie only lasts until the punch fails, and the mesh's own identity authentication binds the session regardless. A signed observation waits on a threat model that demands it.

---

## Breaking changes

v0.32 is **additive on the wire and on every existing transport, fold, reliability, and SDK path** — none of them changed shape. A downstream feels new surface and a version bump, not a behavior change to code it already ships.

- **New subprotocols, all backward-compatible:** `SUBPROTOCOL_REFLEX` and `SUBPROTOCOL_RENDEZVOUS` for traversal, and `SUBPROTOCOL_ROUTE_WITHDRAW` for routing. Every one is dropped by an un-upgraded peer, which degrades to today's behavior (relay fallback; 90 s route age-out) rather than to anything wrong — a deliberate choice, which is why route withdrawal rides a new subprotocol instead of the pingwave's spare `health` field.
- **New signed-announcement field:** an optional `reflex_addr` on the capability announcement, and reserved `nat:*` tags. Absent on nodes that haven't classified; old peers ignore both.
- **New build features:** `nat-traversal` (reflex + classify + rendezvous) and `port-mapping` (UPnP-IGD / NAT-PMP, additionally gating two feature-scoped dependencies). Both off unless enabled — a node that doesn't compile them is untouched. On the Node binding, `delegation` and `a2a` are added and enabled in `default`, so `@net-mesh/core` and `@net-mesh/sdk` ship the Hermes surface automatically.
- **New public binding surface:** `nat_type` / `reflex_addr` / `probe_reflex` / `connect_direct` (and its auto-coordinator overload) / `traversal_stats` / `reflex_override` / `try_port_mapping` across the four native bindings; the versioned FFI traversal-stats call is additive alongside the stable v1 ABI. The Node Hermes modules (delegation, enrollment, A2A) are new napi surface.
- **New config knobs, all defaulted to preserve today's behavior:** the announce debounce and event-pingwave gap, the route-withdrawal enable flag, the rendezvous budgets, and the auto-upgrade / backoff / quiescence knobs. A mesh that touches none of them behaves exactly as v0.31.

---

## How to upgrade

1. **Pull the release** — nothing changes unless you compile in the new features or opt into the new paths. Existing bus, stream, nRPC, payments, and persistence code behaves exactly as before, and the periodic timers keep their current cadence as anti-entropy floors.
2. **To get direct paths between NATed peers**, build with the `nat-traversal` feature; sessions still establish over the relay exactly as today and upgrade in the background where the NATs allow. Enable the automatic upgrade with `auto_direct_upgrade` once you've validated it for your deployment — it is off by default this cycle. Add `port-mapping` to let a node lift itself to `open` via UPnP / NAT-PMP where the gateway cooperates.
3. **To get real-time propagation**, no action is required — change-driven announcements, event-triggered pingwaves, and route withdrawal are on by default and degrade cleanly against un-upgraded peers. Tune the debounce and budget knobs only if a workload needs it.
4. **To use Hermes from Node/TS**, the delegation, enrollment, and A2A surfaces ship in the default build — derive child identities, run the enrollment handshake, and serve or submit A2A tasks the same way the Python and Rust SDKs already do.
5. **Everyone else** gets the new surfaces with no behavior change to existing paths.

---

## Dependency updates

The crate version bumps `0.31.0 → 0.32.0`, propagated across the CLI, deck, SDK, payments, and language-binding manifests. Unlike v0.31's crypto-major cycle, v0.32 is a routine dependency refresh — no first-party crypto or HTTP-client majors moved:

- **Rust crates:** `napi` 3.10.4 (the Node binding's FFI layer), `apple-native-keyring-store` 1.0.1, `regex` 1.13.0, `lru` 0.18.1 — lockfile-level, no API impact on downstreams.
- **Traversal dependencies** are feature-scoped: `igd-next` (UPnP-IGD) and `rust-natpmp` (NAT-PMP) pull in only under `port-mapping`; a node that doesn't enable it links neither. Stages of traversal below port mapping add no third-party weight.
- **Docs / web (Next.js under `web/`), tooling and lockfile only, no runtime path:** `marked` 18.0.6, `prettier` 3.9.5, `eslint` 10.7.0, the Sentry JavaScript monorepo to 10.65.0, and the routine `posthog-js` / `posthog-node` refreshes.

---

Released 2026-07-13.

## License

See [LICENSE](../../LICENSE).
