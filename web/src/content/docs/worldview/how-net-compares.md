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

These systems can occupy different positions in the same deployment. The useful
distinction is the boundary each one owns, not which one has the longest feature
list.

| System          | Position in a combined architecture         | What callers address                           | Boundary it owns                                                                                                                                                   | How it composes with Net                                                                                 |
| --------------- | ------------------------------------------- | ---------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------- |
| **HTTP / REST** | application or provider interface           | an endpoint or resource                        | request and response semantics for a web-facing operation                                                                                                          | a provider-side adapter translates a capability invocation into an HTTP request                          |
| **MCP**         | agent-host tool interface                   | a tool exposed by a configured server          | tool schemas and calls between a host and its configured servers                                                                                                   | selected tools can be published as Net capabilities, or Net can be exposed to an MCP host                |
| **NATS**        | provider-side messaging infrastructure      | a subject                                      | publish-subscribe and request-reply messaging inside the provider or deployment                                                                                    | a provider handles a Net invocation through internal NATS subjects and returns the result                |
| **Zenoh**       | provider-side distributed data plane        | data or computation through a key expression   | publication, subscription, queries, and storage integration across the provider's data plane                                                                       | a provider operates on Zenoh data and publishes selected operations through Net                          |
| **Net**         | capability and authority plane between them | a capability offered by an identified provider | provider identity, capability publication and discovery, visibility, invocation authority, provider selection, invocation, and the streams or artifacts it returns | applications address provider-held work without depending on the provider's internal interfaces or stack |

## How they compose

Every one of them can sit _under_ Net at a different layer. The systems below are
the provider's internals; what crosses the mesh boundary is a capability.

```text
Applications, agents, Hermes, OpenClaw
                    │
                    ▼
        Net capability/authority plane
                    │
                    ▼
     Provider implementations and adapters
                    │
   ┌────────┬───────┼────────┐
   ▼        ▼       ▼        ▼
 Zenoh    NATS     Redis     vLLM
  data  messaging  state   inference
```

### MCP and Net

MCP gives agent hosts a standard way to describe and call tools. Net can carry
those tools beyond one configured host by publishing them as capabilities on a
trusted mesh.

> **MCP makes tools callable. Net makes capabilities discoverable.**

|                              | MCP                               | Net                                                 |
| ---------------------------- | --------------------------------- | --------------------------------------------------- |
| Organizing object            | tool exposed by a server          | capability offered by a provider                    |
| Normal scope                 | a host and its configured servers | nodes across machines, runtimes, and organizations  |
| Discovery                    | server configuration              | live capability announcements                       |
| Authority                    | host and server policy            | visibility and invocation authority at the provider |
| Work beyond request/response | server-specific                   | streams, durable state, tasks, and artifacts        |

Use MCP directly when a host already knows which local or remote servers it should
call. Add Net when providers change at runtime, capabilities span several machines,
or credentials and policy must remain with the provider.

**Publish an MCP server on Net.** `net-mesh wrap` starts an existing stdio MCP
server and announces its tools as Net capabilities:

```sh
net-mesh wrap github -- npx -y @modelcontextprotocol/server-github
```

Wrapped tools are owner-only by default. The wrapping node keeps the server's
credentials and executes the tool locally; callers receive only the permitted
result. A wrapped tool carries `compat_tier: "mcp_bridge"` and retains MCP's
request/response shape, so it does not gain native Net streams, migration, or
artifact semantics merely by crossing the bridge.

**Expose Net to an MCP host.** The bridge runs in the other direction too:

```sh
net-mesh mcp serve
```

This presents the mesh to an MCP host through a small set of meta-tools for search,
description, and invocation, so the host can discover a capability without learning
the Net API. Credentialed or unknown capabilities remain search/describe-only until
approved through the pin and consent flow.

Guides: [Wrap an MCP server](/docs/guides/wrap-mcp-server) ·
[Expose Net as MCP](/docs/guides/expose-net-as-mcp) ·
[MCP bridge reference](/docs/reference/mcp-bridge).

### HTTP and Net

HTTP remains the natural interface for web APIs, SaaS products, browser
applications, and many existing services. Net does not replace those interfaces; it
gives distributed applications another way to address the work behind them.

An HTTP client normally calls a known endpoint. A Net caller asks for a capability
and resolves a visible, available, and admissible provider at runtime. When the
provider is an HTTP-only system, an adapter translates between the two models.

```text title="HTTP at the boundary"
Net capability call
→ adapter on the provider side
→ HTTP API or webhook
→ typed result and execution evidence returned to Net
```

The HTTP URL, credentials, and vendor-specific behavior stay with the adapter. The
caller sees the capability schema and Net authority model rather than depending on
the provider's internal endpoint.

**Keep the boundary explicit.** Use HTTP directly when the endpoint is stable,
request/response is the whole job, and the caller can own the credentials and retry
policy. Use a Net adapter when the operation needs live provider discovery,
provider-held credentials, explicit cross-organization authority, streams or
artifacts, or a typed distinction between accepted, executed, and verified outcomes.

Net's native model is built from capabilities, events, identity, and causal state,
not HTTP resources and verbs. The adapter should therefore stay thin: translate the
external API into one or more capabilities, preserve the external system's actual
outcomes, and avoid turning the mesh into an API gateway. There is no first-party
general REST adapter today — build an edge adapter for the specific service you
need, or use the shipped MCP, Redis, and JetStream adapters where they already
match the system.

### NATS and Net

A service can use NATS subjects to exchange messages within its own system, then
publish selected operations through Net as capabilities. Net callers do not need the
subject names or the NATS topology; they address the capability and the provider
offering it.

**A concrete composition.** Suppose several inference workers already accept
requests on a NATS subject and send their results through request-reply. A Net
provider can:

1. publish an `embed` or `generate` capability;
2. receive an authorized invocation from another Net node;
3. translate the invocation into a NATS request on the internal subject;
4. return the reply through the Net invocation, stream, or artifact associated with
   the work.

NATS continues to carry the internal messages. Net connects the provider-held
operation to callers across machines, runtimes, or authority boundaries. Where the
boundary sits: NATS remains responsible for publish-subscribe and request-reply
messaging over subjects; Net remains responsible for capability discovery, provider
identity, visibility, invocation authority, selection, and the streams or artifacts
attached to an invocation.

### Zenoh and Net

Zenoh gives a provider a distributed data space built around key expressions: the
provider can publish, subscribe to, store, or query data through that space. Net
makes the provider's operations available to the rest of the logical machine under a
stable identity and explicit authority.

**A concrete composition.** Consider a robotics deployment that uses Zenoh to
distribute camera frames, telemetry, and local world-model updates. A service in
that deployment can:

1. read and query the required data through Zenoh;
2. implement an operation that inspects a region or returns a local model;
3. publish that operation through Net as a capability;
4. return the result through the Net invocation, stream, or artifact associated with
   the work.

Callers address the capability and its provider. The provider remains free to use
Zenoh internally, change its key layout, or move work between edge and cloud without
exposing those details as the public invocation contract. Where the boundary sits:
Zenoh keeps the provider's data plane — key expressions, publication, subscription,
queries, storage integration; Net keeps discovery, identity, visibility, invocation
authority, selection, and the streams or artifacts attached to an invocation.

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
them at once. Continue with [What is Net?](/docs/start/what-is-net) for the
implementation model, or [When to use Net](/docs/worldview/right-and-wrong-use-cases)
for the fit boundary.
