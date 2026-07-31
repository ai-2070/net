---
title: NATS vs Net
description: "NATS is the closest operational comparison to Net, and for most messaging work it is the better answer."
---
# NATS vs Net

NATS is the closest **operational** comparison to Net, and for a large class of
work it is the better answer. This page exists because a technical buyer will
reasonably ask *why not just use NATS?* — and that question deserves a straight
answer rather than a feature table tilted to flatter us.

The distinction in one line:

> **NATS routes subjects. Net routes authorized capabilities.**

## The root abstraction

Everything else follows from what each system makes addressable.

| | NATS | Net |
|---|---|---|
| **Root abstraction** | Subject | Capability under authority |
| **You address** | a name messages flow through | a thing that can be done, and who may do it |
| **The question it answers** | *publish or request on this subject* | *find an admissible provider of this capability, invoke it under explicit authority, and keep its streams, state and artifacts coherent as the topology changes* |

A subject is a string with dot-separated tokens and wildcards — `orders.retail.*`
matches one token, `sensor.>` matches the rest of the tree. Publishers and
subscribers find each other through that name and nothing else; neither needs to
know where the other is. It is a genuinely excellent primitive, and its smallness
is the point: subjects, subscriptions, servers, streams, consumers. You can
explain the whole model in a few minutes and ship something useful the same day.

Net addresses a capability instead — a declared thing a node can do, announced
under an identity, discoverable only by those allowed to see it, invocable only by
those allowed to call it. That is a heavier object, and it is worth carrying only
when the extra weight is doing something.

## What NATS is genuinely better at

Not a courtesy list. These are the reasons to pick NATS:

- **Production maturity.** Years of operation at scale, in organisations that
  depend on it. Net does not have that record and cannot claim it.
- **A small mental model.** Fewer concepts, faster onboarding, easier hiring.
- **Ecosystem breadth.** Mature clients across many languages, tooling, monitoring,
  managed offerings, people who already know it.
- **Durable messaging.** JetStream gives persistence, replay, and stronger delivery
  semantics as a first-class, well-understood subsystem.
- **Service ergonomics.** Request/reply and queue groups make load-balanced
  services almost trivial.
- **Operational familiarity.** Deployment, administration and failure modes are
  documented and widely understood.

**If what you need is reliable messaging and conventional microservices, use
NATS.** Net should not pitch itself as a more ambitious NATS for those workloads,
and a team that adopts Net *for messaging alone* has taken on a larger system for
no gain.

## The caricature to avoid

NATS is not "just a centralised broker." Leaf nodes open an **outbound** connection
from an edge network to a hub and bridge subject interest across it, which means
edge sites work behind firewalls with no public address and no inbound rules. Local
clients on a leaf never appear as connections on the hub. Bound to an account with
a credentials file, both ends share one isolated subject space.

So NATS builds real adaptive edge topologies, keeps working locally when the link
drops, and reconciles afterwards. The NATS server fabric is the substrate in an
architectural sense — but "one central broker" is the wrong picture.

## Where Net actually differs

The differences are not about speed, brokerlessness, or edge operation. Those are
table stakes or credible elsewhere. They are about what the protocol treats as a
first-class object.

**Nodes are participants, not clients of a messaging service.** A Net node holds a
cryptographic identity in the protocol itself. It is not a client authenticating
into a fabric; it is a peer that announces what it can do.

**Discovery authority and invocation authority are separate.** In Net, *who may
know a capability exists* and *who may call it* are different questions with
different answers. Organisation-scoped capabilities are **invisible** to
outsiders rather than refused — a distinction that matters when the existence of a
service is itself sensitive. See
[Private capabilities](/docs/guides/private-capabilities).

**Selection is capability-relative, not subject-relative.** A queue group
distributes to whoever is subscribed. Net's discovery matches a predicate against
what nodes advertise — tags, resources, locality — so the choice of provider can
depend on the request. Announcements propagate beyond direct peers, with a
forwarding depth capped at 16 hops.

**Work, artifacts and state share one model.** Typed RPC, ordered streams, durable
logs, folded state, content-addressed [artifact transfer](/docs/guides/dataforts),
and [daemon placement](/docs/guides/daemons-and-placement) sit under the same
identity and routing. Assembling that from a broker plus an object store plus a
workflow engine plus an IAM system is possible; keeping their identity, authority
and failure models agreeing with each other is the part that is hard.

## Transport and delivery, concretely

The abstraction argument above is the important one, but two mechanical facts
decide whether Net can be deployed on your network at all.

**NATS is TCP; Net is UDP, and only UDP.** TLS and WebSocket connections to a NATS
server are still TCP underneath, which is why NATS drops into ordinary corporate
networking without an argument. The Net mesh speaks UDP datagrams and has no TCP
fallback. **A security group that permits only TCP will let a Net process start and
then silently break the handshake** — the failure looks like a peer that never
finishes connecting, not like a blocked port. Open the bind port for inbound and
outbound UDP.

**Delivery semantics are opt-in on both sides, but they divide differently.**

| | NATS | Net |
|---|---|---|
| Default | core: at-most-once | fire-and-forget |
| Stronger | JetStream: at-least-once or exactly-once | opt-in reliability: `None` / `Light` / `Full` per adapter, `FireAndForget` / `Reliable` per stream |
| Ordering | per-subject within a JetStream stream | **not implied by reliability** |

That last cell is the one to read twice. Choosing `Reliable` in Net buys *gap-free
eventual delivery* — nothing in flight is unrecoverable — and buys nothing about
order. Packets, including retransmits, arrive in **arrival order** tagged with a
monotonic sequence number, and a consumer that needs strict ordering reassembles
them itself. Reliable means "no loss", not "delivered in order".

Even fire-and-forget carries those sequence numbers, so a consumer can detect gaps
without paying for retransmission. If you want JetStream's "durable, ordered,
replayable" as a single setting, that is a NATS strength and not a Net one.

The full four-way table, including MCP and Zenoh, is on
[How Net compares](/docs/worldview/how-net-compares).

## Setup cost, and what there is to run

The section above weighed Net's transport honestly. This one weighs what each side
asks of an operator, which an earlier draft of this page skipped.

**There is no Net server.** The mesh is a **4.3 MB** library linked into your
process — measured, `libnet.dylib`, release build, default features. NATS is a
server: you deploy it, cluster it, monitor it, upgrade it, and decide where leaf
nodes sit. That is a mature and well-understood operational story, and it is also a
piece of infrastructure that has to exist before your first message moves.

**And Net uses Noise, not TLS** — `Noise_NKpsk0_25519_ChaChaPoly_BLAKE2s`. An
authenticated, encrypted link is configured with four values:

```text title="Everything a Net link is configured with"
bind_addr            where this node listens
peer_addr            where the peer listens
psk                  32 bytes, shared out of band
peer_static_pubkey   32 bytes, the peer's identity
```

NATS's equivalent is lighter than a full PKI but is still a model to learn and
maintain: server configuration, an operator/account/user hierarchy, NKeys or JWTs,
a `.creds` file per client, and subject permissions per account. Cross-account
sharing means import/export rules, mediated by the server.

Which is better depends on what you already have. **If you run a platform team, the
NATS model is an asset**: central policy, one place to audit, revocation by
reissuing credentials. **If your nodes are field devices, contractor machines or
another organisation's hardware**, the same centralisation is the problem — every
participant needs a credential issued by your infrastructure before it can do
anything.

**The honest cost on our side.** A PSK is symmetric and has to be distributed;
anyone holding it can attempt a handshake with any node that accepts it. There is
no credential expiry forcing rotation for you, and no server-side kill switch —
revocation lives in delegation chains, higher up the stack.

## Two questions, two different answers

A caveat about capability filters belongs here, but an earlier version of this page
ran two questions together and made Net sound weaker than it is. They have
different answers.

**"Is what this provider claims about itself true?" — no guarantee, ever.**
Capability predicates match self-advertised attributes. A node that advertises
`gpu` and has none still matches `require_gpu`, inside an organisation or outside
it. Predicates **select**; they do not attest. Read them as routing hints and build
accordingly — see [Channels](/docs/concepts/channels).

**"Who may see and call this capability?" — cryptographic, once scoped.** This is
the part the old wording obscured. An organisation-scoped announcement is not
broadcast and then filtered by well-behaved receivers: the capability descriptor is
**sealed** with XChaCha20-Poly1305 under a per-audience discovery key, and the
cleartext framing that routes the envelope is bound in as associated data. A
non-member cannot open it, and a forwarder can neither read the descriptor nor
transplant it onto different framing. The ciphertext is padded so its length does
not leak the descriptor's size either. That is what "invisible rather than refused"
means literally, and there are unit tests for each property — wrong key fails,
transplanted AAD fails, length does not leak.

So the honest summary is not "capability filters are advisory, therefore weak
access control". It is: **plaintext announcements and their predicates are
advisory; visibility and invocation are cryptographic once org-scoped**, and the
attributes a provider asserts about itself are self-asserted in both cases.

## Choosing

- **Messaging, ephemeral or durable, and conventional services** — NATS.
- **Everything is inside one trust domain and one operator** — NATS. The authority
  machinery Net adds is overhead you are not using.
- **Authorized work crossing machines, runtimes, organisations and failure
  boundaries** — where the same identity has to govern discovery, invocation,
  streams, artifacts and recovery — evaluate Net.

The two are not mutually exclusive. Net's event bus ships adapters for other
transports, and a mesh that federates capabilities across boundaries can sit
alongside a NATS deployment that carries traffic inside one.
