---
title: The Agentic Mesh
description: "How applications discover and use capabilities across machines while providers retain their credentials and local authority."
---

# The Agentic Mesh

An agent rarely owns everything it needs. The useful tools may live on another
machine, behind another runtime, inside an organization, or next to a device that
cannot hand its credentials to the caller.

Net gives applications a common way to use that distributed supply. Providers
announce typed capabilities from the machines that own them. Callers discover the
providers currently visible to them, invoke one under explicit authority, and
follow the resulting streams, state, and artifacts.

We call this an **agentic mesh**.

```text title="The application loop"
request a capability
→ discover visible providers
→ evaluate availability and authority
→ invoke one provider
→ observe typed execution state
→ collect streams, state, and artifacts
→ verify the required outcome when the application has suitable evidence
```

## Capabilities instead of configured endpoints

A capability is a typed operation a participant offers: run a tool, query a
service, process an artifact, operate a device, provide compute, or perform work
for another application.

The provider announces the capability with its schema and relevant properties.
Other nodes fold those announcements into a local index and query by what they
need rather than by a hostname someone configured earlier.

```sh
net-mesh cap query --tag hardware.gpu --tag hardware.gpu.vram_gb=24
net-mesh cap nodes
```

Discovery can span several hops. The result may be a laptop, a rack server, or an
edge device; the application receives provider identities and capability details,
not a promise that every match is admissible or healthy forever.

## Discovery, invocation, and outcome are separate

Finding a provider does not authorize a call. A capability may be visible but
owner-only, restricted to an organization, or granted to a specific audience.
The provider enforces invocation policy where the work and its consequences live.

An invocation result is also not automatically proof that the requested real-world
outcome holds. Net distinguishes transport acceptance, provider execution, and
verified outcome. The capability contract can specify the evidence the application
requires; without that evidence, the caller keeps the narrower typed result.

This separation matters for retries. A caller can select another provider after a
known pre-execution refusal or clearly unexecuted request. An ambiguous external
effect needs reconciliation or an idempotency contract before another attempt.

## Credentials stay with the provider

The node that owns a credential performs the operation. The caller receives the
result and any permitted artifacts, not the provider's API key, device secret, or
local account.

That lets a personal agent use a capability on another trusted device, or an
application invoke a partner-operated service, without turning the caller into a
central vault for every credential in the system.

## Applications remain applications

Net does not replace workspaces, dashboards, fleet products, or agent runtimes.
Those systems keep their own conversations, workflows, approvals, and interfaces.
They use Net when work must cross a machine or authority boundary.

A workspace might resolve a capability through Net, invoke a qualified provider,
track the resulting task, attach returned artifacts to its own record, and show the
outcome in its existing interface. Net supplies the distributed capability model;
the application supplies the product experience.

## The substrate underneath

The same identity and routing model supports typed RPC, streams, durable logs,
folded state, content-addressed artifacts, and long-running work. These are
separate layers over one encrypted mesh rather than requirements every application
must adopt at once.

Start with the smallest useful loop: announce one capability, discover it from
another node, and invoke it under an explicit authority rule. Add streams,
artifacts, durable state, or scheduling when the work requires them.

Next:

- [Discover and invoke](/docs/guides/discover-and-invoke)
- [Tool federation](/docs/concepts/tool-federation)
- [Private capabilities](/docs/guides/private-capabilities)
- [Submitted is not completed](/docs/guides/submitted-is-not-completed)
