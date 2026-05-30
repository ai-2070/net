# RedEX replication — direct role-by-intent (remove the state-machine-abuse crutch)

**Status:** Planning. Follow-up to `DATAFORTS_BLOB_REPLICATION_AUTOROLE_PLAN.md` (A-1/A-2/A-3, landed). Removes the last crutch in the replication-mode blob transfer.
**Goal:** Let a node claim Leader (or Replica) for a channel **directly and honestly**, instead of faking its way there through the election state machine. Makes `become_chunk_leader` a single semantically-correct transition.

## The crutch, precisely

`MeshBlobAdapter::become_chunk_leader` drives the coordinator `Idle → Replica → Candidate → Leader`. The state machine (`replication_state.rs`) only permits Leader via `Candidate → Leader` on `ElectionWon`, and `Candidate` only via `Replica → Candidate` on `MissedHeartbeats`. So to make a content-addressed writer a leader we **lie to the matrix**:

- `MissedHeartbeats` — but no heartbeats were missed (there's no leader yet).
- `ElectionWon` — but no election ran; `elect()` was never called.

`Idle → Leader` is explicitly rejected (`replication_state.rs` `rejects_invalid_pair_idle_to_leader`). The reader side (`become_chunk_replica`: `Idle → Replica` via `CapabilitySelected`) is already a single honest transition — only the **leader** path is the hack.

## Why role-by-intent, not auto-election

For content-addressed blob chunks the role is **known by intent**, not decided by an election:

- The node that **wrote** the bytes is the authoritative source → **Leader**.
- A node that **wants** the bytes → **Replica**.

The general placement-driven auto-election (the original "Phase F") is a different, harder problem — `elect()` sets self-RTT = 0 (`replication_election.rs:123`), so a symmetric cold start has every node electing itself (dual-leader, resolved only by after-the-fact convergence). We do **not** need that for blobs and explicitly do **not** take it on here. Role-by-intent sidesteps the election consensus problem entirely.

## Plan

### R-1: add a direct `Idle → Leader` transition

**Where:** `replication_state.rs`.

**What:** add a new `TransitionSignal::ClaimLeadership` and permit `(Idle, Leader, ClaimLeadership)` in the matrix. Semantics: "this node holds the authoritative copy and claims leadership for the channel" — the content-addressed-writer case. Update `pair_is_valid_for_some_signal` and the matrix-coverage test (valid-pair count goes 8 → 9).

Optionally add `TransitionSignal::ClaimReplica` as an honest alias for the reader's `Idle → Replica` (today reusing `CapabilitySelected`, which is about placement-filter selection, not fetch intent). Low priority — `CapabilitySelected` already works; decide during implementation whether the clarity is worth the extra signal.

### R-2: expose a clean coordinator claim path

**Where:** `ReplicationCoordinator` + `MeshBlobAdapter::become_chunk_leader`.

**What:** `become_chunk_leader` becomes a single `coordinator.transition_to(Leader, ClaimLeadership)` (idempotent no-op if already Leader). Drop the three-step dance. `become_chunk_replica` stays a single transition. No other caller changes — A-1 (auto-leader on store) and A-2 (auto-replica on fetch) keep working, now over honest transitions.

### R-3: verify a directly-claimed Leader behaves identically

**Where:** review, not code (unless a gap surfaces).

**What:** confirm nothing downstream depends on *how* a node became Leader — a Leader heartbeats its replica set, serves `SyncRequest`s (`replication_catchup.rs`), and concedes via `PeerLeaderObserved` (`Leader → Replica`) under dual-leader convergence. None of that inspects the path to Leader. The "Leader only via election" property relaxes to "Leader via election OR explicit claim"; the safety net (dual-leader convergence: lower tail / higher node-id concedes) is unchanged and still resolves the two-writers-of-identical-content case.

### R-4: tests

- **Unit (`replication_state.rs`):** `(Idle, Leader, ClaimLeadership)` is valid; still rejected under any other signal; matrix coverage = 9 valid pairs; `Idle → Leader` under `ElectionWon` still rejected.
- **E2E:** the existing replication + blob tests stay green. `tests/cross_peer_blob.rs::replicated_directory_transfer_end_to_end` (60-file transparent transfer) is the acceptance test — must keep passing with `become_chunk_leader` now doing a single `ClaimLeadership` transition.
- **Regression:** `redex_replication_e2e.rs` (which manually drives `Replica → Candidate → Leader` for the failover-lifecycle test) is unaffected — that path stays valid; we only *add* a transition.

## Order

1. **R-1** — matrix + signal (smallest, fully unit-testable in isolation).
2. **R-2** — collapse `become_chunk_leader` to the single transition.
3. **R-3/R-4** — review + tests; run the full blob + replication suites.

## What this does NOT include

- **No general placement-driven auto-election.** `elect()`, the heartbeat/lag machinery, and `PlacementStrategy::Standard` recomputation are untouched. A node still becomes Leader by *intent* (a `store`), not by a cold-start election. The general election remains future work if non-blob channels ever need cold-start auto-assignment.
- **No change to the wire protocol** (`SyncRequest`/`SyncResponse`/heartbeat) or to durability/retention semantics.
- **No removal of the manual-driving public API.** `become_chunk_leader`/`become_chunk_replica` stay as the primitive; they just transition honestly now.

## Risks & open questions

- **Relaxing "Leader only via election."** Audit every consumer that might assume a Leader came through `Candidate` (metrics labels like `election_thrash_total`, any invariant in `replication_runtime.rs`/`replication_coordinator.rs`). `ClaimLeadership` should be its own metric label, distinct from `ElectionWon`, so dashboards don't read intent-claims as election thrash.
- **Two nodes claim leadership for the same content** (both `store` the same chunk). Both go `Idle → Leader` directly. Dual-leader convergence must still fire (`PeerLeaderObserved` on heartbeat) and converge to one — verify it does for two intent-claimed leaders, not just two election-won ones. Harmless either way (identical bytes), but it should converge, not thrash.
- **A claimed Leader with an empty file.** `become_chunk_leader` is only called post-`store` (file non-empty), so this shouldn't happen via the blob path; but the transition itself doesn't check file state. Decide whether to guard (reject claim on empty file) or document the caller contract.
