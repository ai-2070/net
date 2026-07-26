# CODE REVIEW 2026-07-26 — Org Capability Load Balancing, pass 2 (`load-balancing`)

**Scope:** the full branch diff `master..f472edca4` (merge base `0b9afc7a3`) —
86 commits, ~21,300 insertions across 34 files. This pass covers the whole
branch, so it SUPERSEDES nothing in
[`CODE_REVIEW_2026_07_23_ORG_LOAD_BALANCING.md`](CODE_REVIEW_2026_07_23_ORG_LOAD_BALANCING.md)
(base `e7fce993e`) but extends past it: everything after that base is the
OLB-1 / OLB-2A / OLB-2B line — the indexed scoped-discovery substrate and
private-discovery change streams (`org_scoped_store.rs`), the exclusive drain
lease + mint, the supervised node-owned routing actor (`org_routing.rs`, new),
the bounded routing registry (`org_routing_registry.rs`, new), the production
`SlotSource` / `RoutingAuthority` / `ScopedCommitPin` wiring in `mesh.rs`, the
revocation-store poison gate and publication-generation exhaustion latch
(`org_revocation.rs`), and the SDK candidate factoring (`sdk/src/org/call.rs`).

**Method:** direct read of the production surface, adversarially, with the
concurrency claims checked by tracing every lock acquisition order rather than
by trusting the module docs. Files read end-to-end (production sections):
`org_routing.rs`, `org_routing_registry.rs`, `org_scoped_store.rs` (drain /
mint / index / ingest), `org_revocation.rs` (`StoreCore`, poison, publish),
`mesh.rs` §§ routing source + authority + supervisor lifecycle + read seam,
`sensing/org_gate.rs`, `sensing/lease.rs`, `sensing/table.rs`,
`org_admission_gate.rs`, `org_scoped_ingest.rs`, `sdk/src/org/call.rs`,
`.github/workflows/ci.yml`.

**Coverage limits — stated so the verdict is not read wider than it is.**
`sensing/rendezvous.rs` (+622) and the sensing *dispatch* hunks inside
`mesh.rs` were NOT re-read in this pass; they were covered by the 2026-07-23
review and its §2 hold is closed (`9c506b0c7`, `f2c82e467`). The ~11k lines of
new test code and the ~1.8k lines of plan/design docs were read only where a
finding depended on them. No test battery was run for this pass; the branch
was verified to type-check (`cargo check --lib --features cortex`, clean).

**Verdict: no P1s.** The routing-plane concurrency design holds up under
adversarial reading — see [Checked clean](#checked-clean), which is the
substantive half of this review. Two P2s: one is a shipped-in-release test
seam with a real availability impact (§1), the other is a liveness/utility
starvation on a path that has no production consumer yet but acquires one the
moment OLB-3 lands (§2). The P3s are correctness-adjacent hygiene.

**Prior-pass closures confirmed at this head** (verified in code, not assumed):
2026-07-23 §1 closed by the distinct `DownstreamId::LeasedLocal` slot
(`table.rs:62`, wired through `mesh.rs:8237-8260`); §2 closed by
`9c506b0c7` / `f2c82e467`; §3 closed by
`tests/sensing_lease.rs:360 stale_ticket_release_cannot_remove_a_live_successor`;
§4 closed by the per-reason `org_*` counters (`evaluator.rs:215-232`) fired
from `org_gate.rs:221-238`; §7 closed structurally by the private
`GateProof(())` field (`org_gate.rs:90`); §8's non-first-holder rollback gap
closed by `tests/sensing_lease.rs:412
refused_non_first_holder_tighten_relaxes_back_to_survivor`.

---

## P2 findings

### §1 — State-mutating test seams are `pub` and ungated, so they ship in release builds

- **Where:** `mesh.rs:8900` (`install_sensing_cached_floor_for_test`),
  `mesh.rs:8913` (`clear_sensing_cached_floor_for_test`),
  `mesh.rs:8945` (`run_sensing_consumer_cell_sweep_for_test`), backed by
  `sensing/table.rs:494` (`set_cached_floor_for_test`) and
  `sensing/table.rs:509` (`clear_cached_floor_for_test`).
- **Defect:** all five are `#[doc(hidden)] pub` with **no** `#[cfg]` gate.
  `behavior` → `sensing` → `table` is a public module chain (the SDK already
  names `net::adapter::net::behavior::org::OrgId` through it), and
  `install_sensing_cached_floor_for_test` takes `&self` on a live `MeshNode`,
  so any code linked against `net-mesh` can write an arbitrary
  `refused_minimum` into the node's live `sensing_interest_table`. Genuine
  registrations for that `(interest, provider)` then return
  `RegisterOutcome::RefusedByCachedFloor` indefinitely — the sensing plane for
  that branch is switched off from outside, with no authority check and
  nothing in the logs.
- **Why this is a finding and not a nit:** it is the exact class the crate has
  already closed once. `org_revocation.rs:546` records §19 verbatim — the
  publish-pause hook "was plain `pub` (only `#[doc(hidden)]`) … Compiled out
  of consumer builds entirely." And every *other* seam this branch adds IS
  gated: `BarrieredGeneration::from_raw_for_test`,
  `saturate_generation_for_test`, `republish_for_test`,
  `register_sensing_interest_paused_for_test`,
  `run_sensing_consumer_cell_sweep_paused_for_test`,
  `set_sensing_projection_contention_hook_for_test` all carry
  `#[cfg(any(test, feature = "fixtures"))]` or `#[cfg(feature = "fixtures")]`.
  The sensing-lease/floor group is the only one that was missed.
- **Note:** the 2026-07-23 review's §8 flagged `set_cached_floor_for_test` for
  *fidelity* (it fabricates a floor-only entry production cannot reach) but did
  not catch that it is ungated. Both observations stand; this is the
  load-bearing one.
- **Fix direction:** `#[cfg(any(test, feature = "fixtures"))]` on all five.
  Zero test churn: the only consumers are `tests/sensing_lease.rs` and
  `tests/sensing_lease_wire.rs`, which the CI diff added to the
  `--features "cortex tool fixtures"` nextest group. The read-only siblings
  (`SensingInterestLeases::{len, is_empty, entry_for_test}` at
  `sensing/lease.rs:237-254`, `MeshNode::sensing_lease_entry_for_test`,
  `sensing_consumer_cell_interval_for_test`) are lower priority but should be
  gated in the same commit for consistency.

### §2 — Routing recapture can starve into permanent `Rebuilding`, with a yield-paced retry loop

- **Where:** `org_routing_registry.rs:1111` (`settle`), `:1117-1133` (the
  coherence re-queue), `:947-969` / `:979-1001` / `:1049-1067` (the three
  requeue-and-mark paths); `org_routing.rs:404-418` (health never advances on
  `Progress` / `Superseded`); `mesh.rs:5604` (`pin_if_current`).
- **Defect:** while `recapture_open`, `settle` re-queues every retained slot
  not coherent with the commit's `SourceEpoch` — and `Current` is the only
  outcome that lets the supervisor publish `Healthy`. So a complete recapture
  requires the whole `snapshot → reconstruct → pin_if_current → install →
  settle` sequence to land at ONE unchanging epoch. Two independent ways that
  fails to converge:
  1. `pin_if_current` refuses whenever the scoped revision, floor generation,
     poison bit or authority epoch moved during reconstruction. A sustained
     private-discovery mutation rate faster than that window means the pin
     never succeeds. Slot-count-independent.
  2. With more than `APPLY_QUANTUM` (64) retained slots, the recapture needs
     ⌈N/64⌉ *consecutive* quanta at one epoch; a single ingest, expiry sweep
     or floor raise between quanta re-queues the slots already built.
     `MAX_NODE_SLOTS` is 256, so 4 clean quanta in a row.
- **Failure scenario:** a node with ≥65 demanded routing slots sits behind a
  busy org whose providers re-announce (or whose floors move) faster than one
  quantum. `owed_recapture` never clears, health stays
  `RoutingHealth::Rebuilding`, and every `org_routing_base_facts` read
  (`mesh.rs:12210`, via `RoutingHealth::allows`) returns `None`. The routing
  cache is permanently cold on exactly the nodes busy enough to need it.
- **Severity qualifier — this fails SAFE.** A cold read sends the caller to
  the current-authority path; no stale route is ever served. This is utility
  and CPU, not correctness.
- **Compounding half:** the three requeue paths call `self.work.mark()`
  unconditionally, which makes the next `select!` in `run_incarnation`
  immediately ready. That contradicts the stated model at
  `org_routing.rs:441-443` — "Parking makes the retry rate the rate of actual
  source movement, which is exactly what `Superseded` reports." In practice
  the actor re-runs as fast as `tokio::task::yield_now()` permits, doing a
  full snapshot + rebuild of up to 64 slots per iteration. The mark is
  *necessary* (a requeued identity is only unioned into `named` when
  `request.registry_work` is set — `:860-862`), so the fix is not to remove
  it.
- **Latency qualifier:** no in-crate production path holds a `DemandHandle`
  today — `RoutingFamily` / `DemandHandle` / `demand` / `release` are all
  `#[allow(dead_code)]` pending the warmed-call consumer (`org_routing_registry.rs:450-461`,
  `mesh.rs:12126-12141`). With zero retained slots the settle-only path always
  reports `Current`, so nothing is broken right now. It becomes live with
  OLB-3.
- **Fix directions:** (a) bound consecutive `Superseded` retries with a
  backoff before re-marking, so a hot source degrades to a slower reconcile
  rather than a spin; (b) let a recapture epoch settle against a MONOTONE
  epoch floor rather than exact equality, so quanta built at ≥ the epoch that
  opened the recapture stay coherent; or (c) accept it explicitly and add a
  metric + witness for "recapture exceeded N restarts", so the starvation is
  observable rather than silent. `recaptures_restarted` already counts the
  displacement — nothing reads it as a health signal.

---

## P3 findings

### §3 — A doc comment was spliced onto the wrong function

- **Where:** `mesh.rs:12015-12026`.
- **Defect:** the block documenting `join_org_routing_supervisor` — "Signal-and-join
  the routing supervisor. / Awaiting the task is the proof the caller needs…"
  through "The shutdown flag and notification must already have been issued." —
  is attached to `#[cfg(test)] fn routing_join_blocked_for_test` (`:12028`),
  whose own one-line doc ("Test-only: whether a joiner is currently BLOCKED on
  the routing-task slot.") was appended into the middle of it. The real
  `join_org_routing_supervisor` (`:12033`) is left undocumented.
- **Why it matters beyond tidiness:** the sentence that went missing is a
  *precondition* ("the shutdown flag and notification must already have been
  issued"). It is now attached to a boolean accessor that has no
  preconditions, so `cargo doc` and any reader of the join path lose it.

### §4 — `SensingAuthorityStamp::is_current` lacks the exhaustion guard its sibling gained

- **Where:** `sensing/org_gate.rs:592`; contrast `org_admission_gate.rs:137`.
- **Defect:** `AdmissionStamp::is_current` was changed on this branch to
  require `store_generation.is_some()` on BOTH sides, precisely because a
  frozen generation "never compares current, in either position."
  `SensingAuthorityStamp::is_current` is still `self == current &&
  !current.poisoned`, so two `store_generation: None` stamps compare
  equal-and-current.
- **Unreachable today:** `capture_sensing_authority_snapshot`
  (`org_gate.rs:665`) returns `SensingAuthorityUnavailable::GenerationExhausted`
  before it ever builds a stamp, so the *captured* side is always `Some`, and
  a live `None` from `capture_current_sensing_stamp` (`:705`) fails the `==`
  and refuses. Fail-closed by construction.
- **Why fix it anyway:** the invariant that makes it safe lives in a different
  function from the check that depends on it, and the sibling type in the same
  branch chose to make it structural. A future caller that builds a captured
  stamp by any other route inherits a silent hole. One `&&` closes it, plus
  the mirror of the three-assert witness already written at
  `org_admission_gate.rs:775-791`.

### §5 — `dedup_by` on the routing snapshot keeps the OLDEST generation

- **Where:** `mesh.rs:5441-5446` (`ScopedSourceSnapshot::providers`).
- **Defect:** the rows are sorted by `(provider, generation)` **ascending**,
  then `providers.dedup_by(|a, b| a.provider == b.provider)`. `Vec::dedup_by`
  passes elements in reverse slice order and removes `a`, so the FIRST of each
  run survives — i.e. the lowest generation.
- **Unreachable today:** `find_scope_exact_private_providers`
  (`org_scoped_store.rs:1153`) reads keys `(scope, provider)` under an exact
  scope equality filter, so one row per provider by construction; the dedup is
  a no-op.
- **Fix direction:** either sort generation descending
  (`b.generation.cmp(&a.generation)`) so the defensive dedup keeps the newest,
  or delete the dedup and let the key uniqueness carry it. A silently-stale
  tiebreak is the worse of the two failure modes to leave armed.

### §6 — `SensingInterestLeases` is the one new registry with no cardinality bound

- **Where:** `sensing/lease.rs:138-141` (`entries`, and the per-key
  `registrations` map at `:130`).
- **Defect:** unbounded in both dimensions. Every other structure added across
  this branch carries an explicit cap and a fail-closed refusal:
  `MAX_NODE_SLOTS` / `MAX_HANDLES_PER_FAMILY` (`org_routing_registry.rs:41-43`),
  `MAX_ENTRIES` / `MAX_ENTRIES_PER_SCOPE` (`org_scoped_store.rs:283`, `:305`),
  `MAX_DIRTY_CAPABILITIES` (`:142`), `MAX_PROVIDER/CONSUMER_GRANT_AUDIENCES`.
- **Exposure qualifier:** holders are local SDK/binding callers, not remote
  peers, so this is a house-style consistency gap rather than a remote DoS.
  Recorded because the 2026-07-21 review's pattern (b) — "bounds correct in
  isolation that do not compose" — is exactly the reason every sibling got one.
- **Non-finding, checked:** the "second acquirer supplies a different `spec`
  under an occupied key" worry from the 2026-07-23 §7 does not bite.
  `InterestSpec::interest_digest` (`sensing/identity.rs:763-779`) hashes every
  field of the spec including `audience`, and the key carries that digest, so
  an equal key implies an equal spec.

### §7 — `plan` now probes every candidate's session instead of short-circuiting

- **Where:** `sdk/src/org/call.rs`, `authorized_candidates` phase 3.
- **Observation, not a defect:** the pre-factoring loop returned at the first
  directly-reachable provider; phase 3 now annotates `direct` for the whole
  sorted vector before `plan` selects. The determinism argument in the comment
  is correct — sampling reachability in discovery order could, under session
  churn, select a different provider than sampling in sorted order. But it
  turns an O(1)-in-the-common-case `peer_entity_id` lookup into O(candidates)
  per call. Cheap now; worth remembering when the sensed selector lands above
  this layer in OLB-3 and the candidate set stops being small.

---

## Checked clean

Adversarially examined and found sound. This is the load-bearing half of the
review: the OLB-2B routing plane introduces four new synchronization objects
across three modules, and the interesting question was whether they compose.

- **Lock order is globally consistent and acyclic.** The frozen order is
  `authority gate → publication gate → registry lock → poison_gate →
  live.read`. Traced every acquisition site:
  - `pin_if_current` (`mesh.rs:5617-5626`) takes authority then publication,
    then the scoped state lock briefly, and holds the first two through the
    conditional install beneath the registry lock;
  - `settle_if_current` (`mesh.rs:5504`) adds `store.pin_publication()`
    (`poison_gate` → `live.read()`) *under* the registry lock, and `settle`
    itself takes no new lock — it receives `&mut RegistryInner` already held;
  - `move_routing_authority` (`mesh.rs:5206`) holds the authority gate across
    `advance()` + `publish()` and calls `registry.invalidate_authority_older_than`
    strictly OUTSIDE it;
  - `invalidate_authority_older_than` (`org_routing_registry.rs:666`) and
    `invalidate_if_stale` (`:704`) release the registry lock before
    `work.mark()`, so no holder of the registry lock ever waits on the
    publication gate;
  - the floor-raise subscriber (`mesh.rs:9519-9580`) runs
    `move_routing_authority` (authority gate, released) and *then*
    `gated_commit` (publication gate) — never the reverse, and never while
    holding the registry lock;
  - `StoreCore::publish` (`org_revocation.rs:574`) takes only `live.write()`,
    never `poison_gate`, so the `poison_gate → live` order in the pin cannot
    invert against it;
  - `notify` (`:664`) and `notify_authority_changed` (`:692`) are invoked with
    no file lock, reload guard, `poison_gate` or `live` guard held, as their
    contract at `:689-691` requires — verified at the `apply_bundle` and
    recovery call sites, not assumed.

  The one inversion that would matter — a subscriber holding the registry lock
  while a routing commit holds the publication gate — is structurally excluded
  by the release-before-mark discipline above.

- **Drain exclusivity is a capability, not a convention.**
  `PrivateDiscoveryDrain` (`org_scoped_store.rs:1237`) has private fields, no
  public constructor, is `!Clone`, and releases its lease on `Drop`;
  `take_global_change_batch` / `take_owner_change_batch` are module-private
  with one production caller each. `PrivateDiscoveryDrains::new` (`:1334`)
  takes the lease words FROM the state rather than minting fresh ones, so a
  second façade over the same state cannot hand out a second live drain —
  exclusivity survives someone constructing a second mint. `LeaseRollback`
  (`:1269`) correctly disarms by value, so `Drop` still runs with
  `armed == false`. The `mem::forget` strand is the safe direction and the
  supervisor observes it as a refused mint and fences (`org_routing.rs:593-602`).

- **The predecessor-consumed-a-delta hole is closed at the right place.**
  `mint` commits `mark_rebuild_all` under the state lock BEFORE the handle
  exists (`org_scoped_store.rs:1362-1379`), so a successor's first drain is
  unconditionally a complete recapture. A fresh incarnation therefore always
  publishes `Rebuilding → Healthy` rather than sitting at `Fenced` — the
  degenerate "first batch is `Clean`, health never advances" path is
  unrepresentable.

- **Wake protocol is correct under coalescing and under early arrival.**
  `RegistryWork.pending` is authoritative and the `Notify` is a hint
  (`org_routing.rs:130-148`); both the shutdown signal and the work signal are
  constructed AND `enable()`d before their respective flag loads
  (`:325-338`), which is the `Notified`-epoch discipline the exact-expiry
  timer already uses. `borrow_and_update` runs before the drain (`:343`), so a
  mutation landing during drain-or-apply leaves `changed()` ready for the
  trailing pass.

- **`owed_recapture` cannot strand.** `batch.dirty` is forced to `RebuildAll`
  BEFORE `quiet` is computed (`org_routing.rs:358-366`), so an owed recapture
  can never be skipped as a quiet pass — the failure mode where a `Caps` wake
  reports `Current` and leaves the recapture owed is closed. `Progress` implies
  `pending` is non-empty by construction (`org_routing_registry.rs:1135-1144`),
  so "Progress with no owed work and no wake" is unrepresentable rather than
  merely avoided.

- **Health never moves forward after shutdown.** The `Current` arm re-checks
  the shutdown flag before publishing `Healthy` (`org_routing.rs:392-394`),
  which closes the window where a long synchronous `apply` straddles a
  shutdown. `IncarnationFence::drop` (`:224`) fences health and deactivates
  the incarnation on every exit path, synchronously, in the actor's own stack.
  Running the incarnation INLINE rather than spawned (`:625`) makes "the
  supervisor future resolving proves the drain is released" structural.

- **Identity spaces fail closed at exhaustion rather than wrapping.**
  `RoutingMetrics::next_incarnation` (`org_routing.rs:486`),
  `RegistryInner::allocate_id` (`org_routing_registry.rs:342`),
  `RoutingAuthority::advance` (`mesh.rs:5322`) and `StoreCore::publish`'s
  generation (`org_revocation.rs:636-647`) all use `checked_add` and latch or
  refuse. Consumers of the frozen generation fail closed too:
  `AdmissionStamp::is_current` (`org_admission_gate.rs:137`),
  `verify_provider_authority`'s `snapshot_with_generation` (`:243`),
  `capture_sensing_authority_snapshot` (`org_gate.rs:665`),
  `capture_live_org_relay_membership_seamed` (`:894`, `:910`),
  `revocation_view_of` (`mesh.rs:5381-5384`, folding exhaustion into the same
  unusable-authority flag as poison), and `pin_if_current` (`mesh.rs:5627`).
  §4 above is the one place the guard is implicit rather than explicit.

- **The epoch/publish ordering is the conservative direction.**
  `move_routing_authority` advances the epoch BEFORE publishing the new store
  (`mesh.rs:5220-5222`), so a lock-free reader observes either `(A, R)` or
  `(B, R+1)` and never `(B, R)` — facts stamped `R` stop matching the instant
  the epoch moves. The alternative ordering would let a reader serve A-derived
  facts as B-authoritative.

- **Poison is carried in the epoch and COMPARED, not read live.**
  `SourceEpoch.poisoned` (`org_routing_registry.rs:176`) plus the comparison at
  `mesh.rs:12183`. Both transitions invalidate (a mark invalidates facts built
  clean; a clear invalidates the `Unserved` reconstruction poison produced),
  and neither steady state churns — which a live-poison predicate would.
  `notify_authority_changed` (`org_revocation.rs:692`) supplies the proactive
  wake for a recovery clear, since a recovery raises no floor and `notify`
  short-circuits on an empty raise set.

- **`Unserved` is distinct from proven-empty.** `SourceFacts::Unserved`
  (`org_routing_registry.rs:210`) is produced for any scope the source cannot
  speak for — the grant plane, or any scope under poison/exhaustion
  (`mesh.rs:5565-5577`) — and reads COLD at the seam (`mesh.rs:12194-12199`)
  rather than as a fresh negative. The unserved count is a metric
  (`mesh.rs:12121`), not a silent zero.

- **The read seam is expiry-safe on its own.** `earliest_expiry` is carried on
  the facts (`org_routing_registry.rs:135`) and enforced against the wall clock
  at read (`mesh.rs:12207`), so the asynchronous exact-expiry timer governs
  promptness rather than correctness — matching the uncached scoped reads.
  The authority revalidation runs BEFORE the `Unserved` cold return, which is
  what lets a poison recovery retire the obsolete reconstruction instead of
  leaving the registry reconciled to it forever.

- **Conditional, not unconditional, invalidation.**
  `invalidate_authority_older_than` filters on `epoch.authority < live`
  (monotone, so it cannot match a successor) and `invalidate_if_stale` uses
  `Arc::ptr_eq` against the exact artifact the reader observed. Both avoid the
  classic bug where a delayed observer deletes a valid replacement and
  re-queues work that was already done.

- **Registry work authority is bound to the actor LIFECYCLE, not a
  high-water mark.** `live_actor: Option<u64>` set in `activate_incarnation`
  and cleared from the fence guard (`org_routing_registry.rs:772-796`).
  `apply` revalidates it at THREE points — phase 1 under the selection lock
  (`:810`), the empty-selection settle path after the off-lock snapshot
  (`:902`), and phase 5 beneath the commit pin (`:979`) — and the stale branch
  consumes NO pending identity, which is the specific way authoritative work
  gets lost while the caller is told it succeeded.

- **`AdmissionStamp` no-store semantics.** Changing `store_generation` to
  `Option` means a stamp captured with no store installed can never compare
  current. Verified safe: `verify_provider_authority` refuses before producing
  a stamp when no store is installed, so the only captured stamps carry
  `Some`, and a store disappearing between capture and recheck now denies —
  which is the fail-closed direction.

- **SDK candidate factoring is behaviour-preserving.**
  `authorized_candidates` builds authority in DISCOVERY order (so
  `AmbiguousCapabilityGrant` still surfaces on the same candidate), sorts by
  provider bytes, and only THEN samples reachability — the exact order the
  pre-factoring loop queried sessions in. `provider_owner_org` is still
  derived as the proof needs it (acting org for same-org, grant issuer for
  granted), never the raw record's owner org. `direct` is annotated, never a
  filter, so no transport state can make an unauthorized provider eligible.

- **`PreparedScopedCapability` closes the decode/recheck ordering.** Only
  `org_scoped_ingest` can construct one (`org_scoped_ingest.rs`), and the
  descriptor decode happens BEFORE the ingest path's final security recheck
  rather than between the recheck and the insert — so a stored row cannot
  diverge from the index buckets built from its declarations, and there is no
  API that accepts a record plus an independently-supplied declaration set.

- **Store cardinality still fails closed.** `ingest` (`org_scoped_store.rs:321`)
  reclaims only fully-forgotten (tombstone-horizon-passed) keys before
  admitting a new one, never evicts an unexpired high-water, and does not
  capacity-gate updates to known keys. Reviving a tombstone leaves
  `scope_counts` alone, correctly, because tombstones are counted.

- **The CI gate is genuinely drift-detecting.** The `MIN=31` count floor
  closes "a filtered run that matches nothing exits 0", and the named-witness
  list catches which one vanished. The `#[cfg(test)]`-not-`fixtures` note is
  right: `$UNIT_FEATURES` excludes `fixtures`, so a fixtures gate on
  `org_routing_wiring_tests` would compile the module to a silent 0-test
  no-op in the job that gates in-source units. Minor: with Actions' default
  `bash -e`, a cargo failure aborts the step before the friendly `::error::`
  message prints — harmless, cargo's own failure is loud.

**Accepted-by-design, restated:** the `Superseded` contract
(`org_routing.rs:98-105`) asserts that the movement invalidating an attempt
itself advanced a watch. That holds for source movement and, via the explicit
`work.mark()`, for registry movement — which is what §2's compounding half is
about. The contract is met; the retry *rate* is the open question.

---

## Disposition

Triaged with Kyra 2026-07-26 and worked in that order. Every finding has a
resolution here; the two that were NOT changed say why, so neither reads as
overlooked.

| # | Sev | Disposition | Where |
|---|-----|-------------|-------|
| §1 | P2 | **Fixed** — five mutating seams + the read-only siblings gated `#[cfg(any(test, feature = "fixtures"))]`; two now-unreachable internals gated with them | `e3a5da509` |
| §2 | P2 | **Deferred by decision** — recorded as an OLB-3 activation prerequisite (below), NOT general debt | this document |
| §3 | P3 | **Fixed** — doc block restored to `join_org_routing_supervisor`, precondition restated as one | `e3a5da509` |
| §4 | P3 | **Fixed** — exhaustion guard made structural in `SensingAuthorityStamp::is_current` + mirror witness | `e3a5da509` |
| §5 | P3 | **Fixed** — generation tiebreak sorted descending so the defensive dedup keeps the newest; witness (30) exercises the branch | `e3a5da509` |
| §6 | P3 | **Fixed** — `MAX_LEASED_INTERESTS` / `MAX_HOLDERS_PER_INTEREST` + total fail-closed refusal + counters + two witnesses | `f384b6664` |
| §7 | P3 | **No change** — an observation, not a defect (below) | — |

### §2 — recorded as an OLB-3 activation prerequisite

Not fixed now, deliberately. No in-crate production path holds a `DemandHandle`
today, so with zero retained slots the settle-only path always reports
`Current` and the starvation is dormant; and the "monotone epoch floor" fix
direction risks weakening exact-authority reconciliation, which is not
something to improvise during an authority-protocol closure.

**Before OLB-3 makes `DemandHandle` live, the following must land together:**

1. bounded exponential/jittered backoff after consecutive `Superseded`, so a
   hot source degrades to a slower reconcile rather than a yield-paced spin.
   The `work.mark()` itself must stay — a requeued identity is only unioned
   into `named` when `request.registry_work` is set;
2. a restart-streak metric promoted to a health signal. `recaptures_restarted`
   already counts the displacement; nothing reads it;
3. explicit degraded/cold behaviour rather than an unbounded rebuild loop;
4. a production witness with MORE than `APPLY_QUANTUM` (64) retained slots
   under sustained source movement.

Multi-quantum epoch semantics (fix direction (b)) are to be reconsidered ONLY
if that witness shows backoff alone is insufficient. Exact epochs are the
current guarantee and are not to be relaxed speculatively.

### §7 — no corrective work

Candidate sets are bounded and small at this layer, and the phase-3 ordering is
what makes selection deterministic under session churn — which is the property
`4dccb7767` was written to restore. Re-evaluate when OLB-3 introduces sensed
selection above this layer and the candidate set stops being small.
