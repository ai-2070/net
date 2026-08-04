---
title: What is Net?
description: "Net is a capability substrate for applications that operate across machines, runtimes, and authority boundaries."
---
# What is Net?

Net is a capability substrate for applications that operate across machines,
runtimes, and authority boundaries.

A provider announces something it can do: run a tool, query a service, process an
artifact, operate a device, or provide compute. A caller discovers providers by
that capability, applies its availability and authority requirements, invokes one,
and follows the related execution state.

```text title="The core model"
application requests a capability
→ discover visible providers
→ select an available and admissible provider
→ invoke it under explicit authority
→ receive typed execution state and artifacts
→ verify the required outcome when a verifier exists
```

Net supplies the distributed machinery under this interaction: cryptographic
identity, capability discovery, encrypted routing, typed RPC, streams, durable
state, content-addressed artifacts, task lifecycle, and provider-local authority.
Applications retain their own workflows, interfaces, and approval model.

## The four objects to know

### Nodes

A node is a participant with a cryptographic identity. It may provide capabilities,
consume them, relay traffic, hold state, or do several of those at once. Identity
belongs to the participant rather than its current address.

### Capabilities

A capability is a typed operation a node offers. Its announcement can include a
schema and properties used for discovery and selection. Provider properties are
assertions unless backed by an external attestation; finding a match is not the
same as proving the provider's claim.

Visibility and invocation are separate. A caller may be allowed to discover a
capability without being allowed to invoke it, and organization-scoped capabilities
can be encrypted so callers outside the audience do not learn they exist.

### Channels and events

Channels carry events between participants. Events retain origin and causal
lineage, which lets applications reason about related state without imposing one
global clock across the mesh.

The event bus is the common substrate used by higher layers. You can also embed it
directly for pub/sub, durable logs, or adapter work.

### Artifacts and state

Work often produces more than a response. Net can attach content-addressed blobs
and directories, durable event history, folded state, streams, and task progress
to the same distributed identity model.

## Layers you can adopt independently

- **nRPC** provides typed request/response and streaming calls over the mesh.
- **RedEX** stores append-only event history for deterministic replay.
- **CortEX** folds logs into materialized state.
- **NetDB** queries folded state.
- **Dataforts** moves and caches content-addressed blobs and directories.
- **MeshOS** places long-running stateful daemons and preserves their continuity
  as hosts change.

These layers share transport and identity, but an application does not need all of
them. Start with the capability and invocation path, then add durable state,
artifacts, or scheduling when the work requires them.

## Authority stays with the provider

The machine holding a credential or physical resource performs the operation. The
caller receives the permitted result, not the provider's secret.

Net authenticates participants and carries authority material, but each provider
keeps the final admission decision. Transport membership, capability discovery,
and invocation permission are different facts.

## Accepted, executed, and verified

A successful network exchange does not necessarily mean the requested outcome
holds. Net models several layers separately:

```text
accepted by the transport
received by the provider
executed by the handler
external effect observed
postcondition independently verified
```

Applications can carry typed outcomes and attach verification evidence to the
result. The capability contract defines what counts as verified success. Calls
without that evidence retain the narrower result they can actually prove. This
also determines whether retry is safe when an external effect may already have
occurred.

## How Net relates to existing systems

Net does not replace the systems around it:

- MCP remains a useful tool interface; Net can publish MCP tools as discoverable
  capabilities.
- HTTP remains the boundary for web APIs and SaaS; adapters expose selected
  operations to the mesh.
- NATS remains a strong subject-oriented messaging system.
- Zenoh remains a strong data-centric edge and robotics fabric.

Net is different because the addressable object is **work offered by a provider
under identity and authority**. See [Where Net fits](/docs/worldview) for the
comparison.

## Which API should you start with?

Most applications should use `net-mesh-sdk` and its `Mesh` surface for capability
discovery and invocation.

Use the lower-level `net-mesh` event bus when you are embedding the bus, writing an
adapter, or tuning ingestion and consumption directly.

Next:

1. [Quickstart](/docs/start/quickstart)
2. [The Agentic Mesh](/docs/worldview/agentic-mesh)
3. [Discover and invoke](/docs/guides/discover-and-invoke)
4. [Architecture](/docs/concepts/architecture)
