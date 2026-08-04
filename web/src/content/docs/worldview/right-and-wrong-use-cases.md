---
title: When to use Net
description: "Use Net when capability discovery, authority, live execution state, or artifacts must cross machines. Use a smaller system when they do not."
---
# When to use Net

Net earns its place when work crosses a machine or authority boundary and the
caller cannot reduce the operation to one fixed endpoint.

## Use Net when

- Providers appear, disappear, move, or change availability while the application
  is running.
- Work spans personal devices, edge nodes, services, agents, or organizations.
- The provider must keep its credentials and enforce policy locally.
- Discovery authority and invocation authority need different rules.
- The call produces streams, durable state, long-running task progress, or
  content-addressed artifacts.
- Several providers offer the same capability and selection depends on health,
  locality, resources, organization, or request policy.
- The application must distinguish transport acceptance, execution, and verified
  outcome.
- The same identity must remain attached to discovery, invocation, state, and
  artifacts as topology changes.

Typical examples include a personal agent using tools across trusted devices, a
fleet application resolving equipment capabilities, a company invoking a partner's
service without receiving its credentials, or a scheduler placing work near data
and compute.

## Use a smaller system when

- A single server and database already solve the problem.
- The caller knows one stable endpoint and request/response is the complete
  lifecycle.
- An MCP host has a fixed set of tools and no runtime discovery is needed.
- A normal queue or job runner already matches a fixed producer/consumer workflow.
- NATS or another messaging system already supplies the required subjects,
  durability, and operational model inside one domain.
- Zenoh or another data fabric already matches a system organized around keys,
  telemetry, and distributed queries.
- You only need one narrow feature that Net also happens to contain.

The dividing question is not whether the system is distributed. It is whether the
application needs to address **work under identity and authority**, rather than a
known endpoint, subject, or data key.

See [How Net relates to other systems](/docs/worldview/how-net-compares) for the
models side by side.
