# NAT and Traversal

Most production deployments have nodes behind NATs — cloud VPCs with private subnets, residential connections, mobile networks, restricted corporate environments. Net's NAT-traversal layer makes the mesh work in those environments without the operator having to think about it: nodes probe their own connectivity, classify what kind of NAT they're sitting behind, and use the right combination of reflex, rendezvous, and (optionally) port mapping to reach peers.

For the common cases — symmetric NATs, full-cone NATs, restricted-cone NATs — traversal is fully automatic. For the awkward cases — symmetric NATs talking to symmetric NATs, double NATs, ISPs with carrier-grade NAT — you might end up using a relay; the runtime makes the choice based on what it can probe and what it knows about each peer.

## What's running

Three components do the work:

**Reflex.** A small probe protocol that asks a peer "what address do you see me at?" The peer's reply tells the local node its external endpoint, which is the thing it needs to advertise to other peers that want to reach it. Reflex runs on every node by default; the protocol is a few bytes and runs once per peer at session setup.

**Classification.** Based on what reflex sees, the local node decides what kind of NAT it's behind. Full-cone, restricted-cone, port-restricted, symmetric, or "direct" (no NAT). The classification drives the strategy: symmetric NATs need more work to traverse than full-cone NATs, and full-cone NATs can sometimes be reached directly once an external endpoint is known.

**Rendezvous.** When two peers need to establish a connection and neither can be reached directly, they coordinate through a rendezvous peer — usually another node on the mesh that both can already reach. The rendezvous helps them simultaneously punch through their respective NATs; once the punch lands, the connection is direct and the rendezvous drops out.

All three are part of the `nat-traversal` feature, which is on by default. You don't enable it; you don't configure it; it just works.

## When it doesn't work

NAT traversal isn't magic. Some combinations don't work:

- **Symmetric NAT on both sides.** A symmetric NAT picks a different external port for every destination, so the punch from peer A doesn't open the port peer B is trying to reach. The runtime detects this case and falls back to a relay path through a third node on the mesh.
- **Hostile firewalls.** Networks that drop UDP entirely (rare but real, especially in some corporate environments) won't talk to Net at all. The fix is either a different network or a tunnel that converts to TCP.
- **Carrier-grade NAT with port exhaustion.** Some mobile networks throttle or close UDP ports aggressively. The runtime's failure detector picks up on this and reports it as a peer health issue.

In the relay-fallback case, the path is encrypted end-to-end — the relay forwards the encrypted packets but can't decrypt them, since the session keys are negotiated between the two endpoints directly. Performance is worse than a direct path (it adds a hop), but correctness is unaffected.

## Port mapping (optional)

For nodes that have a router supporting UPnP-IGD or NAT-PMP / PCP, opportunistic port mapping can open the inbound port automatically. It's not on by default — port mapping modifies state on the user's router, which some environments forbid — but it's a one-flag opt-in:

```toml
[dependencies]
net-mesh = { version = "0.20", features = ["port-mapping"] }
```

When enabled, the runtime probes for UPnP-IGD on the local router, requests a port mapping, and renews the lease before it expires. The mapping is for the duration of the node's lifetime and is released cleanly on shutdown. If the router doesn't respond, the runtime falls back to whatever NAT traversal the network's geometry supports.

The decision to enable port mapping is environmental. Use it in single-tenant residential or office environments where modifying the router is expected. Skip it in cloud environments (where the router doesn't speak UPnP) and in multi-tenant networks (where modifying the router has implications for other users).

## Failure detection

Independent of NAT traversal, every peer-to-peer session is monitored by a failure detector. The detector watches for missed heartbeats, runaway latency, and outright session closures, and it transitions peers through three states:

- **Healthy.** Normal operation; packets flowing.
- **Suspect.** Recent missed heartbeats; the runtime starts trying alternative paths.
- **Failed.** Sustained loss; peer is removed from active routing.

Failed peers stay reachable through the mesh's other paths if any exist. The failure detector is per-direct-peer; if a node has two routes to a destination (direct + via a relay), losing one doesn't lose the destination. The routing layer fails over automatically.

The detector is conservative on purpose. It takes a few seconds of sustained loss before marking a peer as failed, because flapping failures cause more disruption than they prevent. If you have a workload that needs faster failure detection — sub-second recovery from a node going away — that's what standby groups and replica groups are for; they observe the failure independently and act on it.

## What you'll see in practice

Operators interacting with NAT traversal mostly see it through three surfaces:

- **The peer table.** Each peer has a classification (`FullCone`, `Symmetric`, `Direct`, etc.) and a current path (`Direct`, `Relayed(via_node)`). The classification helps debug connectivity issues; the current path tells you whether the mesh is doing what you expected.
- **The reflex metrics.** Reflex packet counts, classification results, and the distribution of NAT types across the mesh. Useful for understanding what kind of environment your deployment is sitting in.
- **The rendezvous logs.** When a rendezvous happens, the runtime logs which peers were involved and which mediator was used. Frequent rendezvous through the same mediator can be a signal that the mediator is doing too much work — a hint to expand mesh capacity in a strategic place.

Application code typically doesn't see any of this. You ingest events, you publish, you consume — the mesh's job is to make those operations work regardless of network geometry. NAT traversal is the part of the runtime that earns its keep in the background.

## What it doesn't replace

Two things the NAT-traversal layer is deliberately not:

**It is not a VPN.** Net doesn't tunnel arbitrary IP traffic between nodes. It carries Net's own protocol, end-to-end encrypted, and that's it. If you need a tunnel for a service that doesn't speak Net, run a VPN underneath; Net will work fine over it.

**It is not a substitute for network design.** A deployment that puts all its critical nodes behind symmetric NATs with no public connectivity will hit relay paths a lot, and relays add latency. For high-throughput, low-latency workloads, give at least some of the nodes public IPs or stable port mappings; the traversal layer is there for the realistic cases, not for an adversarial topology.

Used the way it's meant to be used — in a mesh where most nodes are reachable directly and some require traversal help — the layer is invisible and the runtime just works. That's the goal.
