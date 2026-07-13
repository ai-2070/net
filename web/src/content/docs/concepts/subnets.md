# Subnets

A subnet is a way of grouping nodes that should share scope. Nodes in the same subnet see each other's local traffic by default; nodes in different subnets see each other only when a channel's visibility explicitly says they should. Gateways at subnet boundaries enforce this — without decrypting payloads, without per-flow state, without a central authority deciding what crosses.

Subnets are how Net handles the question "which nodes can see what" at any scale beyond a single LAN. They're how you keep telemetry from one fleet from cross-contaminating another, how you keep a multi-tenant deployment honest, and how a vehicle's internal channels stay internal even when the vehicle is on a public mesh.

## The hierarchy

A `SubnetId` packs four levels of hierarchy into a single 32-bit integer — eight bits per level, 256 children per level. The conventional read of the four levels is region, fleet, vehicle, subsystem, but nothing in Net imposes that interpretation; the hierarchy is structural and the labels are yours to assign.

```
subnet_id (u32):
  [ level 0: 8 bits ][ level 1: 8 bits ][ level 2: 8 bits ][ level 3: 8 bits ]
```

A `SubnetId` of `[3]` is a region-level subnet. `[3, 7]` is a fleet inside that region. `[3, 7, 1, 4]` is a fully qualified subsystem. The all-zero `SubnetId::GLOBAL` is the unrestricted root.

Parent, child, sibling, and distance relationships all resolve with bitwise operations at wire speed. A gateway can decide whether a packet should cross a boundary by reading the header's `subnet_id`, comparing it to its own, and looking up the channel's visibility — three integer operations and one map probe, with no decryption involved.

## Assignment

Nodes are assigned to subnets by policy, not by hand. A `SubnetPolicy` is a list of `SubnetRule`s; each rule maps a capability-tag *prefix* to a hierarchy *level* (0–3) and a set of tag-value→byte mappings, and rules combine across levels to fill the subnet id. Any level no rule fills stays `0` — i.e. unrestricted (`GLOBAL`); there is no separate "default subnet."

This means subnet membership is data-driven and changes as capabilities change. Add a `fleet=west-coast` tag to a node and it moves into the west-coast fleet's subnet automatically; the matching gateway picks it up; the matching channel scopes start applying. There's no separate config to push, and there's no opportunity for the node's claimed subnet to disagree with its capability set.

## Gateways

A subnet gateway is a node that sits between two subnets and applies channel visibility to packets crossing the boundary. The gateway reads only the header — `channel_hash` and `subnet_id` — and consults the channel's `Visibility`:

| Visibility       | Decision                                                             |
|------------------|----------------------------------------------------------------------|
| `SubnetLocal`    | Always dropped at the boundary.                                      |
| `ParentVisible`  | Forwarded only toward ancestor subnets.                              |
| `Exported`       | Forwarded only to subnets named in the channel's export table.       |
| `Global`         | Always forwarded.                                                    |

The gateway also enforces a TTL on every forwarded packet, so a loop in the mesh can't burn forever. Drop reasons are tracked with atomic counters, so you can see at a glance whether a gateway is rejecting traffic for visibility, for TTL, or for an unknown subnet (which usually means a configuration drift).

Importantly, **the gateway never decrypts**. The channel's visibility is policy that lives in the channel's configuration; the gateway has a copy of that configuration in its `ChannelConfigRegistry` and applies it directly to the header. A subnet boundary is a strong boundary by construction, not a polite request that the destination might choose to honor.

## What this lets you do

Subnets are how you compose the mesh into something larger than a single trust domain.

**Multi-tenancy.** Each tenant lives in its own subnet. Channels marked `SubnetLocal` stay inside the tenant. Channels marked `ParentVisible` or `Exported` are the explicit, audited interfaces between tenants. There's no "accidentally exposed an internal topic" failure mode — the gateway is the failure mode, and it can be inspected.

**Hierarchical fleets.** Regions contain fleets, fleets contain vehicles, vehicles contain subsystems. Telemetry from a subsystem stays local; aggregated metrics propagate upward via `ParentVisible`; cross-fleet commands ride on explicit `Exported` channels with a small allowed-destinations list.

**Edge deployments.** A vehicle's onboard mesh is its own subnet. It talks to the cloud over a gateway that exports specific channels (telemetry uplink, command downlink, OTA updates) and drops everything else. The vehicle's internal channels are guaranteed not to leak, because the gateway physically cannot forward them.

## What it doesn't do

Subnets are about scope, not encryption. Two nodes in the same subnet still use end-to-end encrypted sessions; two nodes in different subnets can still talk on a globally visible channel; the subnet doesn't grant or revoke key material, and it doesn't change what's on the wire beyond deciding which packets are allowed to cross which boundary.

Subnets are also not consensus groups. There's no leader election within a subnet, no quorum decisions, no shared state that the subnet maintains. A subnet is a labeling convention plus a gateway-enforced visibility model — the heavyweight semantics live elsewhere, on the channels and the entities and the causal links.

The right way to think about subnets is as the *spatial* dimension of the mesh, complementary to the *temporal* dimension that causal links provide. Subnets answer "who can see this"; causal links answer "what happened before this." Most operational questions about a Net deployment land somewhere in the intersection of those two.
