---
title: Tutorials
description: "End-to-end builds that connect Net's APIs into complete systems."
---

# Tutorials

These tutorials are worked architecture examples. They connect the main APIs and
show the expected control flow, but the snippets are illustrative rather than
complete repositories with deployment manifests.

- **[Fleet telemetry](/docs/tutorials/fleet-telemetry)** builds hierarchical
  channels, subnet scopes, capability announcements, and a folded operator view.
- **[Distributed daemon with failover](/docs/tutorials/distributed-daemon)** runs a
  stateful daemon with a standby and promotes it after host failure.
- **[Event-sourced service](/docs/tutorials/event-sourced-service)** stores events
  in RedEX, materializes a query view with CortEX, and restores from a snapshot.

The tutorials use Rust. TypeScript, Python, Go, and C expose the same major SDK
surfaces with language-specific call shapes; use the [SDK section](/docs/sdk) while
translating an example.
