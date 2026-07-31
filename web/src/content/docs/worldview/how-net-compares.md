---
title: How Net relates to other systems
description: "MCP, NATS, Zenoh, HTTP, and Net are organized around different addressable objects and can coexist in one architecture."
---

# How Net relates to other systems

Net overlaps with tool protocols, messaging systems, and data fabrics, but it is
organized around a different object. This page compares those models so you can
choose the smallest one that fits and combine them where their boundaries meet.

## The addressable object

| System          | What the application addresses                    |
| --------------- | ------------------------------------------------- |
| **HTTP / REST** | an endpoint or resource                           |
| **MCP**         | a tool exposed by a configured server             |
| **NATS**        | a subject                                         |
| **Zenoh**       | data or computation through a key expression      |
| **Net**         | a capability offered under identity and authority |

That choice determines what each system knows. A subject routes messages without
needing to describe the publisher. A key expression gives distributed data a
location-transparent name. An MCP server gives a host a callable tool schema. A
Net capability joins a typed operation to its provider, visibility, invocation
authority, availability, and associated execution state.

## Side by side

|                      | MCP                         | NATS                                                                         | Zenoh                                               | Net                                                                 |
| -------------------- | --------------------------- | ---------------------------------------------------------------------------- | --------------------------------------------------- | ------------------------------------------------------------------- |
| Normal job           | call a configured tool      | route messages and services by subject                                       | publish, query, and subscribe to distributed data   | discover and invoke provider-held capabilities                      |
| Topology             | host to configured server   | server fabric, including clusters and leaf nodes                             | peer, client, and router modes                      | peers with routed fallback; no broker                               |
| Discovery            | server configuration        | connect to a server URL; subjects emerge through subscriptions               | multicast scouting, gossip, or configured endpoints | signed capability announcements folded into a local index           |
| Authority boundary   | host and server policy      | accounts, credentials, and subject permissions enforced by the server fabric | deployment ACLs and TLS/mTLS configuration          | provider identity plus separate visibility and invocation authority |
| Durable state        | server-specific             | JetStream                                                                    | storage/query model                                 | RedEX logs and CortEX folds                                         |
| Artifacts            | server-specific             | application-defined                                                          | data is native to the key space                     | content-addressed blobs and directories through Dataforts           |
| Operational maturity | young, broad tool ecosystem | mature CNCF project                                                          | mature Eclipse project with robotics deployments    | young                                                               |

## How they compose

### MCP and Net

MCP remains the tool interface. Net publishes those tools as capabilities so other
nodes can discover and invoke them without centralizing credentials. See
[Net and MCP](/docs/worldview/mcp-vs-net).

### HTTP and Net

An adapter can expose an HTTP operation as a capability while keeping the URL,
credentials, and vendor behavior on the provider side. See
[Connecting HTTP systems](/docs/worldview/rest-vs-net).

### NATS and Net

NATS can carry messaging and service traffic inside one operational domain while
Net connects selected capabilities across device, runtime, or organization
boundaries. See [Net and NATS](/docs/worldview/nats-vs-net).

### Zenoh and Net

Zenoh fits systems organized around distributed data and queries. Net fits systems
organized around provider-held work and authority. See
[Net and Zenoh](/docs/worldview/zenoh-vs-net).

## Mechanical differences that affect deployment

Net's mesh transport is UDP-only. A network that permits TCP but blocks UDP will
not establish a Net session. Reliability is opt-in and does not imply in-order
delivery: reliable streams preserve gap-free eventual delivery while consumers
that need strict order reassemble by sequence.

Net uses Noise `NKpsk0` sessions rather than TLS. A link requires local bind and
peer addresses, a shared 32-byte PSK, and the responder's static public key. That
is a different key-management model, not a universal simplification: deployments
must distribute and rotate PSKs, while organizations with an established PKI may
prefer systems that use it directly.

Capability properties used for selection are provider assertions unless an
external attestation backs them. Visibility and invocation authority are separate:
organization-scoped descriptors are encrypted for their audience, while provider
policy still decides whether a call may execute.

## Choosing

Use the system whose organizing object matches the application:

- fixed web endpoint: HTTP;
- configured agent tool: MCP;
- messages and services by subject: NATS;
- distributed data by key: Zenoh;
- provider-held work under changing availability and authority: Net.

These are architectural roles, not rankings. A single product may use several of
them at once.
