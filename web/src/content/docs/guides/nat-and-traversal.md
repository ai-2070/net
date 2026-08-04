---
title: NAT and Traversal
description: Most production deployments have nodes behind NATs — cloud VPCs with private subnets, residential connections, mobile networks, restricted corporate environments.
---

# NAT and Traversal

Most deployments include nodes behind NATs: cloud VPCs, residential connections,
mobile networks, or restricted corporate environments. Net probes observed
endpoints, classifies the local NAT, and can use rendezvous, hole punching, relay
paths, or optional port mapping to establish a route.

The available path depends on both endpoints and the network between them. Some
pairs establish a direct route after probing. Symmetric-to-symmetric, double-NAT,
carrier-grade NAT, and UDP-restricted environments may require a reachable relay
or may not establish a Net path at all.

## What's running

Three components do the work:

**Reflex.** A small probe protocol that asks a peer "what address do you see me at?" The peer's reply tells the local node its external endpoint, which is the thing it needs to advertise to other peers that want to reach it. Reflex runs on every node by default; the protocol is a few bytes and runs once per peer at session setup.

**Classification.** Based on what reflex sees, the local node decides what kind of NAT it's behind. Full-cone, restricted-cone, port-restricted, symmetric, or "direct" (no NAT). The classification drives the strategy: symmetric NATs need more work to traverse than full-cone NATs, and full-cone NATs can sometimes be reached directly once an external endpoint is known. Classification stays fresh three ways: a periodic background sweep, an explicit `reclassify_nat()` call for apps that know the network just changed, and an automatic re-check whenever the node notices — at capability re-announce time — that its observed external address no longer matches the one it last published (a gateway reboot looks exactly like this). The re-check runs before the announcement goes out, so peers always see a consistent (NAT class, external address) pair.

**Rendezvous.** When two peers need to establish a connection and neither can be reached directly, they coordinate through a rendezvous peer — usually another node on the mesh that both can already reach. The rendezvous helps them simultaneously punch through their respective NATs; once the punch lands, the connection is direct and the rendezvous drops out.

All three are part of the `nat-traversal` feature, which is enabled by default.
Deployments must still provide reachable bootstrap or rendezvous peers and allow
the required UDP traffic.

## When it doesn't work

NAT traversal isn't magic. Some combinations don't work:

- **Symmetric NAT on both sides.** A symmetric NAT picks a different external port for every destination, so the punch from peer A doesn't open the port peer B is trying to reach. The runtime detects this case and falls back to a relay path through a third node on the mesh.
- **Hostile firewalls.** Networks that drop UDP entirely (rare but real, especially in some corporate environments) won't talk to Net at all. The fix is either a different network or a tunnel that converts to TCP.
- **Carrier-grade NAT with port exhaustion.** Some mobile networks throttle or close UDP ports aggressively. The runtime's failure detector picks up on this and reports it as a peer health issue.

In the relay-fallback case, the relay forwards end-to-end encrypted packets. The
extra hop adds latency and consumes relay capacity; applications should still
apply their normal deadlines and failure handling.

## Port mapping (optional)

For nodes that have a router supporting UPnP-IGD or NAT-PMP / PCP, opportunistic port mapping can open the inbound port automatically. It's not on by default — port mapping modifies state on the user's router, which some environments forbid — but it's a one-flag opt-in:

```toml
[dependencies]
net-mesh = { version = "0.20", features = ["port-mapping"] }
```

When enabled, the runtime probes for UPnP-IGD on the local router, requests a port mapping, and renews the lease before it expires. The mapping is for the duration of the node's lifetime and is released cleanly on shutdown. If the router doesn't respond, the runtime falls back to whatever NAT traversal the network's geometry supports.

The decision to enable port mapping is environmental. Use it in single-tenant residential or office environments where modifying the router is expected. Skip it in cloud environments (where the router doesn't speak UPnP) and in multi-tenant networks (where modifying the router has implications for other users).

## Background direct-path upgrade

When two peers first reach each other through a relay, that relayed session works — but every packet pays the extra hop. The runtime notices relay-routed sessions in the background and opportunistically re-handshakes over a direct path (the peer's advertised external address), migrating the session once the direct path lands. Traffic rides the relay until the swap; if the direct attempt fails, nothing changes.

The swap is guarded so it never disrupts in-flight work: only one side initiates (the lower node id, so re-handshakes can't cross), the install is compare-and-swapped against any racing handshake, and a session with open streams or unacked data defers the upgrade until it goes quiescent. Failed attempts back off exponentially per peer.

On by default as of v0.34 (it was opt-in in v0.32–v0.33), validated against the netns + nftables real-NAT harness in CI. To pin traffic to the relay path, disable it explicitly: `auto_direct_upgrade(false)` on the Rust SDK builder, `autoDirectUpgrade: false` in the Node options, `auto_direct_upgrade=False` on the Python constructor, `AutoDirectUpgrade: &false` in the Go config (the field is a `*bool`, so leaving it `nil` inherits the default). There is also a one-shot form — `connect_direct_auto(peer, pubkey)` (`connectDirectAuto` / `ConnectDirectAuto`) — which picks the rendezvous coordinator automatically and establishes the best available path for a single peer on demand.

## Failure detection

Independent of NAT traversal, every peer-to-peer session is monitored by a failure detector. The detector watches for missed heartbeats, runaway latency, and outright session closures, and it transitions peers through three states:

- **Healthy.** Normal operation; packets flowing.
- **Suspect.** Recent missed heartbeats; the runtime starts trying alternative paths.
- **Failed.** Sustained loss; peer is removed from active routing.

Another route may keep a destination reachable when one direct peer fails. If no
eligible route remains, the destination becomes unavailable until topology
recovers.

The detector waits for sustained loss before marking a peer failed to avoid
flapping. Workloads with a shorter recovery objective need separately configured
health checks, standby or replica state, and a measured promotion path; a group by
itself does not make network failure detection instantaneous.

## What you'll see in practice

Operators interacting with NAT traversal mostly see it through four surfaces:

- **The peer table.** Each peer has a classification (`FullCone`, `Symmetric`, `Direct`, etc.) and a current path (`Direct`, `Relayed(via_node)`). The classification helps debug connectivity issues; the current path tells you whether the mesh is doing what you expected.
- **The reflex metrics.** Reflex packet counts, classification results, and the distribution of NAT types across the mesh. Useful for understanding what kind of environment your deployment is sitting in.
- **The rendezvous logs.** When a rendezvous happens, the runtime logs which peers were involved and which mediator was used. Frequent rendezvous through the same mediator can be a signal that the mediator is doing too much work — a hint to expand mesh capacity in a strategic place.
- **The traversal stats.** Every binding exposes the same cumulative snapshot — `traversal_stats()` in Rust and Python, `traversalStats()` in Node, `TraversalStats()` in Go — with thirteen fields in three groups:
  - _Punch outcomes_: `punches_attempted` (coordinator mediated an introduction), `punches_succeeded` (a direct session landed), `punches_failed` (derived: attempted − succeeded), and `relay_fallbacks` (resolutions that stayed on the routed path).
  - _Failure causes_: `punch_timeouts` (a punch wait hit its deadline), `punch_rejections` (a coordinator refused with a typed reason — rate limit, unknown target, anti-reflection), and `rendezvous_no_relay` (no coordinator candidate existed). These count causes, including failures before a punch was ever mediated, so they aren't a partition of `punches_failed`.
  - _Background activity_: `upgrades_attempted` / `upgrades_succeeded` / `upgrades_deferred_busy` for the direct-path upgrade, and `port_mapping_active` / `port_mapping_external` / `port_mapping_renewals` for port mapping.

  Base counters are monotonic and never reset; compute deltas between snapshots for rates. Two fields are exempt from delta math: `punches_failed` is derived at snapshot time (`attempted − succeeded`) and can _decrease_ when an in-flight punch lands, and `port_mapping_renewals` resets to zero on each fresh mapping install — difference only the base counters. A high `punch_rejections` count points at coordinator-side policy (often rate limits); a high `punch_timeouts` count points at network geometry (symmetric NATs, dropped UDP); a growing `upgrades_deferred_busy` means long-lived busy sessions are staying on their relays by design.

Application code usually sees path changes as latency, availability, or typed
transport failures. Operators use these surfaces to determine whether traffic is
direct, relayed, or unable to establish a path.

## What it doesn't replace

Two things the NAT-traversal layer is deliberately not:

**It is not a VPN.** Net doesn't tunnel arbitrary IP traffic between nodes. It carries Net's own protocol, end-to-end encrypted, and that's it. If you need a tunnel for a service that doesn't speak Net, run a VPN underneath; Net will work fine over it.

**It is not a substitute for network design.** A deployment that puts all its critical nodes behind symmetric NATs with no public connectivity will hit relay paths a lot, and relays add latency. For high-throughput, low-latency workloads, give at least some of the nodes public IPs or stable port mappings; the traversal layer is there for the realistic cases, not for an adversarial topology.

For latency-sensitive deployments, provide enough publicly reachable or stably
mapped nodes that relay paths remain a fallback rather than the normal topology.
