---
title: Zenoh vs Net
description: "Zenoh is the closest conceptual comparison to Net — both reject the idea that distributed state belongs behind a central endpoint. The split is data versus authorized capability."
---
# Zenoh vs Net

Zenoh is the closest **conceptual** comparison to Net. Both reject the assumption
that useful distributed state belongs behind a conventional central service
endpoint; both care about edge-to-cloud continuity, locality, and resources that
can move or exist in several places at once. If you have looked at Zenoh and are
now looking at Net, you are asking the right question.

The distinction in one line:

> **Zenoh routes data. Net routes authorized capabilities.**

## The root abstraction

| | Zenoh | Net |
|---|---|---|
| **Root abstraction** | Key expression | Capability under authority |
| **You address** | a resource key, wherever it lives | a thing that can be done, and who may do it |
| **The question it answers** | *read, write, subscribe to, or query this resource* | *find an admissible provider of this capability, invoke it under explicit authority, and keep its streams, state and artifacts coherent as the topology changes* |

Zenoh describes itself as "a pub/sub/query protocol that unifies data in motion,
data at rest and computations," and the ambition behind it is "one protocol from
bare-metal microcontroller to data-center." The unification is real: publish and
subscribe, query geo-distributed storage, and expose computation as something
queryable — all through one key space, with "location transparency for data at
rest, allowing queries to be expressed without any concerns on the actual location
of data."

That is a genuinely strong idea, and it occupies most of the territory people
imagine when they hear "brokerless pub/sub with discovery and storage."

## What Zenoh is genuinely better at

- **Constrained and embedded environments.** Down to microcontrollers, deliberately
  and from the start.
- **Robotics legitimacy.** Adopted in robotics, autonomous vehicles, gaming and
  telecommunications; the most visible integration is as a ROS 2 middleware, which
  is an ecosystem Net has no presence in.
- **Transport and topology breadth.** Peer, client and router modes; multicast
  scouting on `224.0.0.224:7446`, plus gossip scouting where peers share what they
  have discovered — so discovery works with or without multicast.
- **A coherent data plane.** Data in motion, data at rest and computation in one
  model, which is a harder thing to get right than it sounds.
- **Maturity.** Shipped, operated, and documented. Net is younger and should be
  evaluated as such.

**If what you need is an edge or robotics data fabric with a distributed query
space, use Zenoh.**

## Where the models actually diverge

Zenoh remains fundamentally **data-centric**. Key expressions name resources, and
those names determine where publications, subscriptions and queries flow. Even
computation enters the model as something *queryable* through the resource space.

Net is **capability- and authority-centric**. The addressed object is not a
resource name but a declared operation with an owner, and the questions the
protocol treats as first-class are:

```text title="What the capability model has to answer"
Who provides this capability?
Under what organization or root does that node act?
Who is allowed to know it exists?
Who is allowed to invoke it?
Which delegation authorizes this exact call?
Where should the work execute?
Which artifacts and durable state belong to the operation?
What happens when the provider, route, or authority changes?
```

Two of those have no natural place in a data-centric model:

**Existence as a permission.** In Net an organisation-scoped capability is
**invisible** to those outside its audience rather than refused. A key space
answers "does this key exist" as a routing question; Net answers it as an authority
question. See [Private capabilities](/docs/guides/private-capabilities).

**Authority that travels with the work.** The identity governing whether you may
discover a capability is the same identity governing whether you may invoke it,
which streams you may open, which artifacts move with the call, and what happens
when the provider goes away. That is not a feature Zenoh lacks so much as a
different thing to be organised around.

Both systems propagate announcements past direct peers — Zenoh through routers and
gossip, Net by forwarding capability announcements up to a depth of 16 hops.

## Three differences that hold up

Feature-by-feature the two look adjacent. These three survive scrutiny.

**1. What propagates when a node joins.** Zenoh exchanges *routing state* — which
key expressions have subscribers, which queryables exist, which liveliness tokens
are declared. A joining peer learns the shape of the key space. It does not learn
what any node *is*. Net propagates signed capability announcements: attributed to
an identity, carrying a TTL and a hop count, verifiable by any receiver. Zenoh's
liveliness tokens are the closest analogue, and they assert that *a key expression
is alive* — a subscriber is notified when the token appears or disappears, and gets
a delete if connectivity to its creator is lost. That is an assertion about a key,
not a description of a provider.

> **Zenoh discovers endpoints. Net discovers providers.** Both find each other
> without configuration; the difference is what the mesh knows once they have.

**2. Admission is network-level versus identity-level.** Zenoh's access control is
configured — ACL rules and mTLS certificates set on nodes and routers — so trust is
a property of the deployment. In Net the joining node carries its own organisation
membership as a signed attestation, and visibility follows from that rather than
from what an operator wrote in a config file. The difference is between *the admin
allowed this machine onto the network* and *this machine proves which fleet it
belongs to*. It matters exactly where you would expect: contractors, dealers,
cross-vendor fleets, anywhere the operator of the network is not the owner of the
capability.

**3. Mode is a deployment decision versus a property.** Someone configures a Zenoh
node as peer, client or router, and getting that wrong is a real source of the
complaints you will find in ROS 2 threads. Net nodes are symmetric by construction
— a node publishes, subscribes, relays and persists as needed, and relay-capable is
a tag rather than a role. Modest, but it is a genuine operational simplification
and the one a field technician actually feels.

## Transport and delivery, concretely

Two mechanical facts decide whether Net fits your network at all.

**Zenoh is a mix; Net is UDP, and only UDP.** Zenoh runs over TCP by default and
also speaks UDP, QUIC, TLS, serial and shared memory — the breadth is deliberate
and is part of how it reaches microcontrollers and gets zero-copy on-host. The Net
mesh speaks UDP datagrams with no TCP fallback. **A security group that permits
only TCP will let a Net process start and then silently break the handshake.** Open
the bind port for inbound and outbound UDP; see
[NAT and traversal](/docs/guides/nat-and-traversal).

**Reliability is opt-in in both, but Net does not fold ordering into it.**

| | Zenoh | Net |
|---|---|---|
| Default | best-effort | fire-and-forget |
| Stronger | reliable, over a TCP or QUIC session | opt-in: `None` / `Light` / `Full` per adapter, `FireAndForget` / `Reliable` per stream |
| Ordering | reliable delivery arrives ordered | **not implied by reliability** |

Choosing `Reliable` in Net buys *gap-free eventual delivery* and nothing about
order: packets, including retransmits, are delivered in **arrival order** tagged
with a monotonic sequence number, and a consumer needing strict ordering
reassembles them itself. Fire-and-forget carries the same sequence numbers, so
detecting gaps costs nothing. If you want "reliable and therefore ordered" as one
switch, Zenoh gives you that and Net does not.

The full four-way table, including MCP and NATS, is on
[How Net compares](/docs/worldview/how-net-compares).

## The comparison as a decision

Neither of us should pretend this is a knockout. The honest version:

- **Location-transparent data across edge and cloud, embedded targets, ROS 2** —
  Zenoh.
- **A heterogeneous network that mostly moves telemetry** — Zenoh. Net's authority
  machinery is weight you would not be using.
- **Capabilities crossing runtime, device and *organisational* boundaries**, where
  discovery, invocation, streams, artifacts and recovery all have to answer to one
  identity — evaluate Net.
- **One narrow feature Net happens to include** — do not adopt Net for it yet.

The dividing line is not brokerlessness, latency, or edge operation; Zenoh has all
three and has had them longer. It is whether the unit you need to address is *data
at a key* or *work behind an authority boundary*. If treating the world as a key
space is sufficient, that is the simpler system and you should use it.

## An honest caveat about Net's own model

Capability **filters are advisory**. They match what a node advertises about
itself, so a node that lies matches. They are routing hints, not access control;
the real boundary is a signed permission token with explicitly trusted roots, or
organisation-scoped auth. Reading Net's capability predicates as a security
mechanism produces something unsafe — see [Channels](/docs/concepts/channels).

And on maturity: Zenoh has years of field deployment in robotics and industrial
settings. Net has deep semantics and a much shorter operational record. Semantics
are not a substitute for proof, and
[Right and wrong use cases](/docs/worldview/right-and-wrong-use-cases) is where we
say where Net is not the answer.
