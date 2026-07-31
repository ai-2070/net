---
title: Claiming a Contended Resource
description: "Some resources can't be shared. A GPU NVLink domain, an accelerator slot, a licensed seat — one holder at a time, and two nodes that both believe they hold it is a corruption bug,"
---

# Claiming a Contended Resource

Some resources can't be shared. A GPU NVLink domain, an accelerator slot, a
licensed seat — one holder at a time, and two nodes that both believe they hold
it is a corruption bug, not a performance problem.

The gang-claim scheduler is the surface for that: publish what you have, match
what you need, and claim it atomically under contention. It is deliberately
_not_ a job scheduler — it decides **who holds a resource**, and stops there.
What runs once you hold it is the [task lifecycle](/docs/guides/task-lifecycle).

## Islands and units

An **island** is a co-located pool of exclusive **units**. The motivating case
is a GPU NVLink domain — eight H100s that are only useful together — but nothing
in the model is GPU-specific. GPU-ness rides on ordinary capability tags
(`gpu:h100`, `model:<hex>`), the same tags [capability
discovery](/docs/concepts/capabilities) already uses.

The exclusivity boundary is the island, not the unit. You claim the whole
island or you don't claim it.

## Publish what you have

A node advertises its islands into the island fold:

```rust
use net_sdk::gang::IslandRecord;

let published = mesh.publish_island_topology(record).await?;
```

Publishing is how a resource becomes claimable by anyone. A node that never
publishes is invisible to matching, including its own.

## Match what you need

Matching is read-only — it tells you what _could_ be claimed, in preference
order, and takes no lock:

```rust
use net_sdk::gang::{
    CapabilityFilter, CapabilityQuery, MatchCriteria, NumericFilter, SelectionPolicy,
};

let criteria = MatchCriteria {
    capability: CapabilityQuery::Composite(CapabilityFilter {
        tags_all: vec!["gpu:h100".into()],
        ..Default::default()
    }),
    numeric: NumericFilter {
        min_units: 8,                      // the whole NVLink domain
        max_load: Some(0.7),               // live load, 0.0..=1.0
        max_p50_latency_us: Some(2_000),
        ..Default::default()
    },
    selection: SelectionPolicy::LeastLoaded,
    prefer_capability: None,
};

let candidates = mesh.match_islands(&criteria);
```

The numeric axes are **live**, not static inventory: `max_load` and
`max_p50_latency_us` filter on what the island is doing right now, and
`tags_all` on the island side can require _resident_ capabilities — a warm
`model:<hex>` that lets you skip a cold load.

### Selection policy is a scheduling decision

`SelectionPolicy` is the knob that decides what your cluster looks like under
load, and the four options make different trade-offs:

- **`LeastLoaded`** (default) — spread. Lowest-load island first, island id
  ascending as a deterministic tie-break.
- **`Pack`** — consolidate. Most-loaded-but-still-passing island first, so whole
  islands stay idle and claimable by a future large gang. Pick this when big
  gangs matter more than tail latency.
- **`LoadBand(f32)`** — aim for a load level. Avoids both stone-cold islands
  (cold-start cost) and near-saturated ones (tail-latency cliff). Note it is a
  **tuple** variant in Rust — `SelectionPolicy::LoadBand(0.6)`, not a struct
  variant with a named `target` field. Python and Go expose the target as a
  separate flat field instead.
- **`LowestId`** — deterministic. Lowest island id that passes, ignoring load
  entirely. Useful when you want reproducible placement across runs rather than
  balanced placement.

## Claim it

`claim_island` runs the match and reserves the first available result in one
call. This is the operation that has to be atomic:

```rust
let held = mesh.claim_island(&criteria, until_unix_us).await?;

match held {
    Some(island) => { /* we hold it until `until_unix_us` */ }
    None => { /* nothing matched, or every match was contended */ }
}
```

The reservation is time-bounded on purpose. A holder that crashes does not
strand the island forever — the claim expires. Renew before `until_unix_us` if
you need it longer, and release explicitly when you're done:

```rust
use net_sdk::gang::ClaimOutcome;

match mesh.release_island(island).await? {
    ClaimOutcome::Won => { /* released */ }
    ClaimOutcome::Lost => { /* we weren't the holder — someone else took it */ }
}
```

`ClaimOutcome::Lost` is not an error. It's the compare-and-swap telling you your
view was stale: someone else holds the island, and the correct response is to
re-run the match pipeline and try elsewhere, not to retry the same island.

## What this does and does not guarantee

**Atomic, not consensus.** The claim is a compare-and-swap against the
reservation fold. Under a partition, two sides can each believe they hold an
island that the other also claims — the fold converges when the partition heals,
and one side learns it lost. If double-booking across a partition is
unacceptable for your resource, you need a quorum, and a bare gang claim is the
wrong primitive.

**Local view, converging.** `match_islands` and `claim_island` read _this
node's_ folds. A node sees its own announced capabilities and published islands
immediately; peer-hosted islands appear only once their announcements converge
over the mesh. On an isolated node, only self-hosted islands match — which is
also why an empty match result on a freshly-started node usually means "not
converged yet", not "nothing exists".

**Advisory discovery, binding claim.** Matching tells you who _can_. Only the
claim is exclusive. Nothing stops a peer that ignores the scheduler entirely
from using its own hardware.

## Where to go next

- [Task lifecycle](/docs/guides/task-lifecycle) — what runs on a held island,
  and how its state survives a restart.
- [Capabilities](/docs/concepts/capabilities) — the tag and filter model
  matching is built on.
- [Daemons and placement](/docs/guides/daemons-and-placement) — for work that
  should be _placed_ rather than claimed.
