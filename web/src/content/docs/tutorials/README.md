# Tutorials

The pages in this section are end-to-end builds. Each one starts from an empty project, walks through the design decisions, and ends with something that works — code you could lift, adapt, and ship.

Tutorials are longer than guides because they cover the whole arc. You'll read about why each piece is shaped the way it is, not just what it is. If you're new to Net, picking one tutorial that's close to what you're building and working through it end-to-end is the fastest way to internalize the patterns.

The three tutorials here cover different parts of the surface:

- **[Fleet telemetry](./fleet-telemetry)** — edge devices publishing to a hierarchical channel namespace, gateways scoping by subnet, a fold materializing aggregate metrics for an operator dashboard. Hits channels, subnets, capabilities, folds.
- **[Distributed daemon with failover](./distributed-daemon)** — a stateful daemon running across a standby group, surviving a node failure by promoting a passive replica. Hits MeshDaemon, placement, standby groups, continuity.
- **[Event-sourced service](./event-sourced-service)** — building a small service backed by RedEX, with a CortEX fold materializing the queryable view and snapshot/restore handling restarts. Hits the storage stack, queries, and snapshots.
