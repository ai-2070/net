# Release Roadmap

> Forward-looking release planning. Past releases live in `docs/releases/`; their post-mortems and changelogs are the artifacts. This doc is upcoming-direction-only — what's planned, what's parked, what's noted-but-not-committed.

## Naming convention

Cult-music + cult-cinema references from roughly the 1979–1990 Cyberpunk-adjacent era. Each release name is one of:

- A 1980s rock/punk album or song (Billy Idol, Steve Stevens, Echo & the Bunnymen, etc.)
- A late-70s / early-80s cult film (*The Warriors*, *First Blood*, etc.)

Era-consistency is load-bearing for brand identity. Don't deviate without a reason.

## Past releases (shipped)

| Version | Codename | Reference |
|---|---|---|
| v0.8 | KILLING_MOON | Echo & the Bunnymen, *The Killing Moon* (1984) |
| v0.9 | FIRST_BLOOD | First Blood (1982 film) |
| v0.10 | HEX | — |
| v0.11 | BLACK_DIAMOND | — |
| v0.12 | FIRESTARTER | Stephen King novel (1980) / The Prodigy (1996) |

See `docs/releases/RELEASE_v0.X_*.md` for each.

## Upcoming releases

### The Warriors (precursor to Dataforts)

Substrate foundations needed before Dataforts can integrate cleanly. Reference: 1979 cult film about a NYC gang traversing enemy territory to get home. Thematically: foundation work, traversal, getting prepared.

Scope (per `docs/misc/DATAFORTS_PLAN.md`):
1. Capability taxonomy reorganization (`hardware` / `software` / `devices`)
2. Capability-tag discovery primitive + metadata field
3. Federated query primitives
4. Generalized 5-axis `PlacementFilter` + Mikoshi integration
5. RedEX V2 — raw log-segment replication

Activation: when any phase inside has an activation gate firing. Ships as a coherent release.

### Rebel Yell (Dataforts)

The loud declaration. Reference: Billy Idol, *Rebel Yell* (1983), with iconic guitar work by Steve Stevens. Thematically: breakout, *more, more, more*, the visible product moment after Warriors prepares the ground.

Scope (per `docs/misc/DATAFORTS_PLAN.md`):
1. Greedy-LRU dataforts (5-axis filter; composes `PlacementFilter`)
2. Data gravity (heat-counter migration; emergent from greedy + heat)
3. BlobRef + BlobAdapter hook trait
4. Read-your-writes guarantees (optional)

Activation: phase-by-phase as workloads demand each piece.

### Atomic Playboys (post-Rebel-Yell, parked candidates)

Reference: Steve Stevens, *Atomic Playboys* (1989) — Stevens's solo album after his iconic Rebel Yell guitar work. Musical lineage is exact (same guitarist, next chapter); 1989 timing puts it inside the original Cyberpunk RPG era (*Cyberpunk 2013* in 1988, *Cyberpunk 2020* in 1990). Thematically: explosive, confident, post-breakthrough swagger.

Candidate items, no commitment yet — parked for review when Rebel Yell traction calls for the next release:

1. **Full MeshDB** — the deferred extension above the Warriors-shipped query primitives. Time-travel queries against historical chain ranges (`causal:X[start..end]`), full lineage-walk traversals via the `fork-of:` graph, cross-chain joins with bounded result streaming. Activates when a workload (incident-investigation tooling, replay debugging, fleet-wide aggregate analytics) genuinely needs distributed queries.

2. **Mikoshi v2** — battle-tested `PlacementFilter` behavior plus richer migration semantics learned from production. Probable additions: live migration without snapshot replay (delta-based), placement re-evaluation under capacity drift (rebalance running daemons, not just initial placement), application-pinned migrations for advanced operators.

3. **Federated mesh-wide scheduler** — builds on `PlacementFilter` for not just placement but ongoing rebalancing. Continuously evaluates whether existing daemon/replica placements still satisfy the 5-axis filter as capability tags shift; migrates artifacts toward higher-scoring nodes opportunistically. Effectively turns the substrate into a self-organizing scheduler without a central coordinator.
