---
title: Connecting HTTP systems
description: "HTTP remains the natural interface for web APIs and SaaS. Net adapters expose selected operations as capabilities at the mesh boundary."
---

# Connecting HTTP systems

HTTP remains the natural interface for web APIs, SaaS products, browser
applications, and many existing services. Net does not replace those interfaces.
It gives distributed applications another way to address the work behind them.

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

## Keep the boundary explicit

Use HTTP directly when the endpoint is stable, request/response is the whole job,
and the caller can own the credentials and retry policy.

Use a Net adapter when the operation needs live provider discovery, provider-held
credentials, explicit cross-organization authority, streams or artifacts, or a
typed distinction between accepted, executed, and verified outcomes.

Net's native model is built from capabilities, events, identity, and causal state,
not HTTP resources and verbs. The adapter should therefore stay thin: translate
the external API into one or more capabilities, preserve the external system's
actual outcomes, and avoid turning the mesh into an API gateway.

There is no first-party general REST adapter today. Build an edge adapter for the
specific service you need, or use the shipped MCP, Redis, and JetStream adapters
where they already match the system.
