---
title: How Net compares
description: "MCP, NATS, Zenoh and Net side by side — transport, addressing, delivery semantics and where the trust boundary sits."
---
# How Net compares

One table, four systems. It exists so the differences can be checked rather than
asserted, and so the two long-form comparisons —
[NATS vs Net](/docs/worldview/nats-vs-net) and
[Zenoh vs Net](/docs/worldview/zenoh-vs-net) — do not each carry their own drifting
copy of the same facts.

Read it as *what each system makes addressable*, not as a scoreboard. Three of
these four are more mature than Net and two of them are the right answer for a
large class of work.

## The one-line version

| | Addresses |
|---|---|
| **MCP** | a tool on a server the host was configured with |
| **NATS** | a subject |
| **Zenoh** | a key expression |
| **Net** | a capability under an owner identity |

> MCP makes tools callable. NATS routes subjects. Zenoh routes data.
> Net routes authorized capabilities.

## Side by side

| | MCP | NATS | Zenoh | Net |
|---|---|---|---|---|
| **Transport** | stdio or HTTP | TCP (TLS and WebSocket ride TCP) | TCP by default; also UDP, QUIC, TLS, serial, shared memory | **UDP only** |
| **Topology** | host ↔ configured server | server fabric required; clusters, superclusters, leaf nodes | peer / client / router — the mode is configured | peers, no broker; relay-capable nodes carry the routed fallback |
| **Discovery** | none — servers are wired into the host | client connects to a server URL | multicast scouting on-segment; gossip or configured endpoints beyond it | signed capability announcements propagate peer to peer, re-broadcast to a depth of 16 hops |
| **What is addressed** | tool name | subject string | key expression | capability, under an owner identity |
| **Typing** | JSON Schema per tool | none — opaque bytes | encoding hint only | typed at the SDK, JSON on the wire; raw bytes available |
| **Liveness** | process lifetime | no core primitive; server-side slow-consumer detection | liveliness tokens declared on a key expression | announcements carry a TTL and are refreshed by signed diffs |
| **Delivery** | request/response over the transport | core at-most-once; JetStream at-least-once or exactly-once | best-effort, or reliable over a TCP/QUIC session | fire-and-forget by default; reliability is opt-in |
| **Ordering** | n/a | per-subject within a stream | reliable delivery is ordered | **not implied by reliability** — see below |
| **Trust boundary** | host-local: the host decides what is wired in | server-enforced: NKeys, JWT, subject permissions | deployment-configured: ACL rules, mTLS | node-held: signed permission tokens with explicitly trusted roots |
| **Cross-organisation** | not a primitive | accounts with subject import/export, server-mediated | not a primitive | organisation-scoped capabilities — invisible rather than refused |
| **Maturity** | young; large and fast-moving ecosystem | CNCF project, many client languages, very large production base | Eclipse project, ROS 2 middleware, robotics and industrial deployments | **young, with no comparable production record** |

Two rows a previous draft of this table carried have been dropped rather than
guessed at: process footprint in megabytes, and client-language counts. Neither was
verified, and a comparison page is exactly the wrong place to round a number in our
own favour.

## The rows worth reading twice

### Transport — Net is UDP, and only UDP

The mesh speaks UDP datagrams. There is no TCP fallback, and this bites during
deployment more often than anything else in this table: **a security group or
firewall that only permits TCP will let the process start and then silently break
the handshake.** Open the bind port for inbound and outbound UDP. See
[NAT and traversal](/docs/guides/nat-and-traversal).

NATS is the opposite end: TCP throughout, including TLS and WebSocket, which makes
it trivially compatible with ordinary corporate networking. Zenoh spans both and
adds serial and shared memory, which is part of why it reaches microcontrollers.

### Reliability is opt-in, and it does not mean ordered

This is the row most likely to be misread, so it is stated plainly.

Net's default is fire-and-forget. Reliability is a choice — `None`, `Light` or
`Full` on the adapter, `FireAndForget` or `Reliable` per stream. Choosing
`Reliable` buys **gap-free eventual delivery**: nothing in flight is
unrecoverable.

It does **not** buy ordering. The substrate does not reorder for you. Accepted
packets — including out-of-order arrivals and retransmits — are delivered in
**arrival order**, each tagged with a monotonic sequence number, and a consumer
that needs strict in-order bytes reassembles them itself. Consumers that frame
their own ordering, like nRPC streaming keying on the call id, need do nothing.

> **Reliable here means "no loss", not "delivered in order."**

Fire-and-forget still puts monotonic sequence numbers on the wire, so a consumer
that only wants to *detect* gaps or reordering can do that without paying for
retransmission.

Compare: NATS core is at-most-once and JetStream gives you at-least-once or
exactly-once with per-subject ordering inside a stream; Zenoh offers best-effort or
reliable, where reliable rides a TCP or QUIC session and arrives ordered. Net is
the only one of the three that separates "no loss" from "in order" and hands the
second decision to you.

### Typing is a contract, not a wire format

Net's typed surface is real at the SDK boundary — `emit<T: Serialize>` takes your
type — and what goes on the wire is JSON. Raw bytes are available when you want to
carry your own encoding. So "typed" here means a compile-time contract in your
language and a schema you maintain, not a negotiated wire type.

The one genuinely typed, attributed object on the wire is the **capability
announcement** itself: signed, carrying a TTL and a hop count, verifiable by any
receiver. That is what makes discovery a statement about a provider rather than
about a name.

## Where each one wins

- **MCP** — you have a tool and you want an agent host to call it. Net
  [bridges to MCP](/docs/worldview/mcp-vs-net) rather than competing with it.
- **NATS** — messaging and conventional distributed services, ephemeral or durable,
  inside one operator's world.
- **Zenoh** — location-transparent data across edge and cloud, embedded targets,
  robotics and ROS 2.
- **Net** — authorized capabilities crossing machines, runtimes and organisations,
  where the same identity has to govern discovery, invocation, streams, artifacts
  and recovery.

If only one narrow row in this table is what you need, the mature system that owns
that row is the better choice. [Right and wrong use
cases](/docs/worldview/right-and-wrong-use-cases) is blunter still.
