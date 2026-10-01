# RedEX replica placement — `Standard` and `ColocationStrict`

## Status

Done 2026-10-01 (P1–P3, see each slice). Branch `LZL0/node-sdk` (PR #1138),
after C1/C2 in `NODE_SDK_GAPS_PLAN.md` made `Pinned` replication work end to
end.

## The gap

`REDEX_DISTRIBUTED_PLAN.md` §4 says replica selection picks the top N
candidates by `PlacementFilter::placement_score` for `Artifact::Replica`, and
re-selects on roster change. That was never built: `Redex::open_file` seeds the
replica set only for `Pinned` and leaves `Standard` (the default) and
`ColocationStrict` with an empty set. Those channels sit in `Idle` and
replicate nothing; the code calls it "Phase F", though the plan's Phase F is the
DST harness, which shipped.

`ColocationStrict` also refers to the channel's `metadata.colocate-with-strict`,
which `ReplicationConfig` has no way to carry.

## Design

**Agreement: deterministic and local** (decided 2026-10-01). Every node computes
the replica set itself from data every node sees the same way, so the same
capability view gives the same set with no coordination and no wire change.

- **Candidates.** A node that opens a `Standard` / `ColocationStrict` channel
  advertises `dataforts:replica-candidate:<channel-id hex>` (a substrate-owned
  body under the existing reserved `dataforts:` prefix, so no reserved-prefix
  mirrors change). The candidate pool is every node advertising it, self
  included. The runtime re-advertises if the tag goes missing (concurrent
  capability rewrites can drop it) and withdraws it on shutdown.
- **Scoring.** `StandardPlacement` over `Artifact::Replica`, with only the axes
  whose inputs are announced data: colocation (soft for `Standard`, strict for
  `ColocationStrict`), intent (strict, against `IntentRegistry::defaults()`,
  which is the same everywhere), storage resources. RTT proximity, leadership
  anti-affinity and node-local custom filters differ by viewer and are left out
  of selection; election still ranks by RTT among the chosen replicas. A score
  of `None` or `0.0` excludes the candidate.
- **Selection.** Top `factor` by score, ties to the lower NodeId.
- **Placement metadata.** `ReplicationConfig::placement_metadata` (string map:
  `intent`, `colocate-with`, `colocate-with-strict`) becomes the artifact's
  metadata. `ColocationStrict` requires `colocate-with-strict`; validation
  rejects it otherwise.
- **Runtime.** The replica set moves from a fixed `RuntimeInputs` field to
  runtime state. With a resolver installed, each tick re-resolves it. Selected
  while `Idle` → bootstrap to `Replica` (the C1 path, leader wait armed). No
  longer selected while participating → `Idle` via a new
  `PlacementDeselected` signal, which withdraws the chain advertisement.
  Inbound frames are gated on the current set.

## Slices

### P1 — core

Config field + validation, candidate tag on `MeshNode`, pure selector,
mesh-backed resolver, dynamic set in the runtime, manager wiring.

- **Proves it:** selector unit tests; e2e: three nodes, `Standard`, factor 2 →
  two replicas, one leader, data reaches the other replica, the third stays
  `Idle`; `ColocationStrict` picks only holders of the named chain; a selected
  node closing its channel lets the third node take its place.

- **Done.**
  - `ReplicationConfig::placement_metadata` + `with_placement_metadata`;
    `validate()` rejects `ColocationStrict` without `colocate-with-strict`
    (`ColocationStrictWithoutChain`). `gang::colocated_island_config` now
    takes the host chain, since its config would otherwise fail validation.
  - `MeshNode::{announce,withdraw}_replica_candidate`,
    `advertises_replica_candidate`, `find_replica_candidates`,
    `min_announce_interval`.
  - `redex::replication_placement`: `select_replica_set` (pure),
    `ReplicaSetResolver`, `MeshReplicaPlacement`.
  - Runtime: optional `PlacementState` (`None` for `Pinned` and the
    existing tests); every read of the set goes through
    `current_replica_set`; `resolve_placement` runs first each tick.
    `TransitionSignal::PlacementDeselected` (`Replica` / `Candidate` /
    `Leader` → `Idle`). A disk-pressure `Withdraw` also withdraws
    candidacy, so the node stays out and peers re-select.
  - **Found while testing, fixed:**
    - **Announce rate limit.** `min_announce_interval` (10 s default)
      coalesces a node's later capability changes into a trailing flush,
      so a candidate tag can reach peers a window late. A node then
      resolves a set of one, joins it and elects itself, beside another
      such leader: diverging writes. **Settle rule:** a node joins a set
      smaller than `factor` only once it has been unchanged for two
      announce windows; a full set joins at once. Runtime unit tests with
      a fixed resolver pin all three cases. **RED** (rule removed): the
      short-set test joins at 300 ms.
    - **`close_file` aborted the runtime.** Dropping a runtime handle
      aborts its task, so a closing node never withdrew its candidacy and
      peers never re-selected (nor withdrew the chain tag, a pre-existing
      leak). `close_file` / `close_and_unlink_file` / `disable_replication`
      now share `shut_down_runtimes`, which awaits a graceful `cancel()`
      on a spawned task when a runtime is available.
  - **Tests:** selector units (ranking, ties, vetoes, canonical order);
    config and state-machine units; three e2e tests on three nodes
    (`Standard` factor 2 picks the two lowest ids, replicates, leaves the
    third `Idle`; a member closing hands its place to the third, which
    catches up; `ColocationStrict` with factor 3 picks only the two chain
    holders). The e2e nodes use a 50 ms announce window. **RED**
    (resolver off): all three fail. **GREEN:** the replication and
    dataforts e2e binaries 18/18 three runs in a row; redex / gang /
    dataforts / placement units 1332.

### P2 — bindings

`placementMetadata` (Node, TS SDK via the native type), `replication_placement_metadata`
(Python + stub), `placement_metadata` (C ABI JSON, Go wrapper).

- **Proves it:** a two-node `Standard` channel replicates from the SDK and from
  Python.

- **Done.** Node `ReplicationConfigJs.placementMetadata` (the TS SDK's
  `ReplicationConfig` follows from the native type), Python
  `replication_placement_metadata` (+ stub), C ABI JSON
  `placement_metadata`, Go `PlacementMetadata`. SDK test: two nodes,
  `factor: 2`, no node list, `leaderPinned` naming who leads: the leader's
  writes arrive and the other node never led (it joined a full set);
  converged in about 1.2 s. Python: the same, plus `colocation_strict`
  rejected without a chain and accepted with one. sdk-ts 666, Python 263.

### P3 — docs

`CONFIG_REPLICATION.md` (placement section, quick start back to the default
placement), `REDEX_DISTRIBUTED_PLAN.md` §4 status.

- **Done.** `CONFIG_REPLICATION.md`: quick start and binding examples back
  on the default placement (with the announce-window caveat), a "How
  `Standard` and `ColocationStrict` choose" section, `placement_metadata`,
  the lifecycle's selection step, `leader_pinned` under placement.
  `REDEX_DISTRIBUTED_PLAN.md` §4 notes the built design.

## Second review, 2026-10-01

Three bugs in the replication core, each reproduced by a test that failed
before its fix:

- **Closing one channel withdrew every channel's advertisement.** All channels
  on a `Redex` advertise one origin (`causal:<principal>`). Once `close_file`
  shut runtimes down gracefully (this plan), a closing channel's
  `* → Idle` withdrew the shared tag while other channels still held it, and
  their coordinators never noticed. Each coordinator now gets a
  `ChannelChainSink` that records it as a holder of the origin and withdraws
  only when the last holder lets go; holders are keyed per runtime, so a
  channel closed and reopened at once can't lose its new claim to the old
  runtime's late withdraw. **RED:** `closing_one_channel_keeps_the_shared_origin_advertised`
  (e2e) failed with "closing A withdrew the tag B still holds".
- **A non-announcing transition cleared the pending-announce flag.**
  `transition_to` stored `result.is_err()` on every transition, and
  `Replica → Candidate` / `Candidate → Replica` never call the sink, so a
  failed bootstrap announce was forgotten after a lost election. The flag
  now changes only when the sink was called. **RED:**
  `non_announcing_transitions_keep_a_failed_announce_pending` (unit).
- **An aborted runtime leaked its replica-candidate tag.** Off a tokio
  runtime, a `Redex` dropped without `disable_replication`, or a full
  priority lane at `cancel()` all abort the task, skipping the graceful
  withdraw, and the tag stays in the baseline every announce re-sends.
  `MeshReplicaPlacement` now drops the tag from the baseline when it is
  dropped (it lives in the task, so an abort drops it too) and re-announces
  when a runtime is available. **RED:**
  `an_aborted_runtime_drops_its_replica_candidacy` (e2e).

**Follow-up (cubic on the fix):** the holder count was scoped too
narrowly. Both registries now live on `MeshNode`, where the tags are:
`chain_holders` (origin → runtimes, held across the mesh call) and
`replica_candidate_holders` (channel → resolvers). A per-`Redex` count let
two managers on one mesh, or two wiring generations across a disable /
re-enable, withdraw each other's live tag. **RED:**
`a_second_manager_on_the_mesh_keeps_the_shared_origin_advertised` (e2e).
The candidate tag is now counted per resolver too, with each resolver
registering its claim at construction (inside `open_file`), so a
close-and-reopen's old release can't remove the new claim; **RED** by
mutation (a release that ignores other claimants):
`candidate_tag_outlives_all_but_the_last_resolver` (unit). The sync drop
path reports an announce-lock timeout separately and falls back to an
async withdraw instead of treating it as already withdrawn.

**GREEN:** redex / gang / dataforts / placement / FFI / chain / heat units
1500; the replication, dataforts-blob, chain-discovery and gravity e2e
binaries 33/33 twice; SDK replication tests 6/6; Python `test_redex.py`
18/18; clippy (three configurations), rustdoc and fmt clean.

## Risks

- **Views converge, not agree instantly.** Until capability announcements
  propagate, nodes can briefly disagree on the set. A node that isn't in a
  peer's set has its frames dropped by that peer; once the views converge the
  sets match. Same window as any capability-driven decision.
- **A dead candidate stays selected until its announcement expires.** The
  set then shrinks below `factor` for that window; election already ignores
  unhealthy replicas.

## Not in scope

- RTT- or load-aware replica choice (needs the leader-assigned model).
- Rebalancing data when the set changes beyond normal catch-up: a newly
  selected replica catches up through the existing pull path.
