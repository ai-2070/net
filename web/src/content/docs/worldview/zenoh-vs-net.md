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
