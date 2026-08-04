---
title: Tutorials
description: "End-to-end builds that connect Net's APIs into complete systems."
---
# Tutorials

Tutorials start from an empty project and finish with a working system. They
explain the design choices that connect the individual APIs.

- **[Fleet telemetry](/docs/tutorials/fleet-telemetry)** builds hierarchical
  channels, subnet scopes, capability announcements, and a folded operator view.
- **[Distributed daemon with failover](/docs/tutorials/distributed-daemon)** runs a
  stateful daemon with a standby and promotes it after host failure.
- **[Event-sourced service](/docs/tutorials/event-sourced-service)** stores events
  in RedEX, materializes a query view with CortEX, and restores from a snapshot.

The tutorials use Rust. TypeScript, Python, Go, and C expose the same major SDK
surfaces with language-specific call shapes; use the [SDK section](/docs/sdk) while
translating an example.
