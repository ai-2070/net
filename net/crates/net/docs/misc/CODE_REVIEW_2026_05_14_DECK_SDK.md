# Deck SDK branch code review — 2026-05-14

Branch: `meshos-deck-sdk` (~10.7K LOC; 39 commits ahead of `master`).
Phase 1 (Rust SDK: snapshots + admin commits + audit + logs) and
Phase 2/3 (ICE substrate + Rust ICE surface) per
[`DECK_SDK_PLAN.md`](../plans/DECK_SDK_PLAN.md).

Five parallel passes covered the SDK surface
(`behavior/deck.rs` + `sdk/`), ICE security + correctness
(`meshos/ice.rs`, `migration_aborter.rs`, `migration_snapshot_source.rs`),
chain / state / persistence (audit / log / failure rings +
RedEX appenders + state.rs), event-loop + reconcile + executor +
runtime, and the integration test surface
(`tests/deck_pipeline.rs` + `tests/meshos_pipeline.rs`).

## Status

**Open — partial closure.** 35 items identified:
**7 Critical / 13 Important / 15 Nit.** Per the "no
review-tracking IDs in code or commit messages" feedback
rule, labels (C1-C7, S8-S20, N21-N35) are for this doc only —
code and commit messages stay self-explanatory.

### Closed (10 items across 9 commits)

| ID  | Title                                                                  | Commit (short title)                                                                                                |
|-----|------------------------------------------------------------------------|--------------------------------------------------------------------------------------------------------------------|
| C1  | `verify_bundle` deduplicates by `operator_id`                          | `MeshOS: verify_bundle dedupes by operator_id.`                                                                    |
| C2  | Domain-separated, replay-protected signing envelopes                   | `MeshOS: domain-separated, replay-protected signing envelopes.`                                                    |
| C3  | Substrate-enforced simulate-before-commit (blast-radius hash binding)  | `MeshOS: substrate-enforced simulate-before-commit via blast-radius hash.`                                         |
| C5  | Per-target ICE cooldown                                                | `MeshOS: per-target ICE cooldown on the admin verifier.`                                                           |
| C6  | `IceProposal` / `SimulatedIceProposal` type-state split                | `Deck SDK: type-state split for IceProposal / SimulatedIceProposal.`                                               |
| S8  | `dispatch_kill_migration_if_applicable` runs after `actual.apply`      | (batched) `MeshOS + Deck SDK: tighten admin-event apply ordering and surface invariants.`                          |
| S14 | Unsigned `AdminCommands::freeze_cluster` / `thaw_cluster` removed      | (batched) same as above                                                                                            |
| S16 | Freeze gates ordinary admin commits; ICE bypasses                      | `MeshOS: freeze gates ordinary admin commits; ICE bypasses.`                                                       |
| S17 | `static_assert` `Send` on every public stream type                     | (batched) same as above                                                                                            |
| S18 | `emit_maintenance_transitions` anchors on `last_tick`, not wall-clock  | (batched) same as above                                                                                            |
| N24 | Removed redundant `cx.waker().wake_by_ref()` in stream poll bodies     | (batched) `Deck SDK: nit batch — DeckClient: Clone, drop redundant wake_by_ref, fix stale docstring.`              |
| N31 | `AuditQuery::force_only` docstring updated                             | (batched) same as above                                                                                            |
| N32 | `DeckClient: Clone`                                                    | (batched) same as above                                                                                            |

### Still open

**Critical:**

- **C4** — Simulator uses real `reconcile` arms. Substantial
  rewrite. Pragmatic interim landed via existing
  `simulate_*` arms; full convergence requires the snapshot-
  clone-then-reconcile design from the doc.
- **C7** — Persistent / epoch-tagged seq counters across
  restarts. Requires `runtime_epoch_id` on
  `MeshOsSnapshot` and matching SDK-side dedup contract.

**Important:**

- **S9** — Warn on partial extension wiring + emit
  `FailureRecord` when an ICE `KillMigration` lands but the
  installed aborter is the no-op.
- **S10** — Migration-abort failures route to the failure
  ring (currently `tracing::warn!`-and-swallow).
- **S11** — Snapshot avoids full-ring clones (carry rings
  as `Arc<[T]>` or windowed views).
- **S12** — Move `admin_audit` / `log_ring` off
  `MeshOsState` onto the loop.
- **S13** — Replace `start_with_*` stair-step with
  `MeshOsRuntimeBuilder`.
- **S19** — Split `behavior/deck.rs` into the
  `deck/{identity,error,admin,ice,audit,logs,failures,streams,client}.rs`
  module tree.
- **S20** — Promote ICE-discipline negatives + missing
  surfaces (audit pagination, log-filter, failure
  subscription, cooldown end-to-end) up into
  `tests/deck_pipeline.rs`.

**Nits:**

- N21, N22, N23, N25, N26, N27, N28, N29, N30, N33, N34, N35
  (per-item descriptions below).

### Rationale for the closure boundary

The closed items are the ICE security guarantees that the
plan's locked decisions hang on (multi-sig dedup, replay
defense, simulate-before-commit at the cryptographic layer,
cooldown, type-state simulate→commit) plus the small
correctness / ergonomics fixes (apply ordering, freeze
gating, last-tick anchoring, stream Send assertions, the
nits batch). The remaining items are larger structural
refactors (S11/S12/S13/S19) or substantive new features
(C4 simulator rewrite, C7 epoch tag) — better as their own
follow-up PRs than rolled into a single mega-commit. Each
open item has a target shape pinned in this doc so the next
contributor doesn't need to relitigate the design.

## A. Critical — security and correctness

### C1 — `verify_bundle` does not deduplicate operator id

**Where:** `src/adapter/net/behavior/meshos/ice.rs:270-286`.

`OperatorRegistry::verify_bundle` checks
`signatures.len() < threshold` and then verifies each signature
individually. Nothing rejects a bundle of `[sig_A, sig_A]`
(the same operator signing twice). With the plan-default
`threshold = 2`, a single operator's key satisfies the
M-of-N gate — the headline guarantee of the entire ICE
surface fails.

**Fix shape:** collect verified `operator_id`s into a
`BTreeSet<u64>` and check `set.len() >= threshold`. Add a
regression test that asserts `[sig_A, sig_A]` with
`threshold = 2` returns `InsufficientSignatures`.

### C2 — Signing payload has no domain tag, no nonce, no expiry

**Where:** `src/adapter/net/behavior/meshos/ice.rs:155-157`
(`ice_proposal_signing_payload`) and `:167-169`
(`admin_event_signing_payload`).

Both functions return raw
`postcard::to_allocvec(payload)`. Consequences:

- **No domain separation** between ICE proposal bytes and
  ordinary admin event bytes (or any future signed surface
  riding postcard). A capture from one context could be
  replayed against another if the encodings overlap.
- **No replay protection.** A captured `ForceCutover { chain,
  target: attacker_node }` signature bundle is permanently
  valid until the operator key rotates. The audit ring is
  advisory; the verifier itself doesn't check whether a
  `seq` has already fired.
- **No expiry.** Compounds the replay window with operator
  turnover.

**Fix shape:** prepend a context tag
(`b"net.meshos.ice.v1\0"` / `b"net.meshos.admin.v1\0"`) and
a freshness binding (proposal-issued-at + per-node
`chain_tip` hash, or a per-operator monotonic nonce the
verifier persists). The SDK encodes the same envelope on
sign; the verifier re-encodes deterministically and rejects
stale or out-of-domain bundles. Round-trip + cross-domain
regression tests.

### C3 — Substrate does not enforce simulate-before-commit

**Where:** `event_loop.rs:694-731` (apply path) +
`ice.rs:637` (free-function `simulate`) +
`behavior/deck.rs:987-1037` (`IceProposal::commit` runtime
`Cell<bool>` gate).

Plan locked decision #4: "Blast-radius simulation is
mandatory before ICE commit. Substrate-side contract."
Today the SDK keeps a `Cell<bool>` honor-system gate; the
substrate never demands proof of simulation. A malformed or
malicious client can sign a proposal and push
`SignedIceCommit` without ever invoking the simulator —
the loop verifies the signatures and folds.

**Fix shape:** include a `BlastRadius` hash in the signed
envelope. The SDK simulates, hashes the deterministic
`BlastRadius` encoding, and signs over
`proposal_payload || blast_hash`. The substrate re-simulates
against its own snapshot and rejects if the hash mismatches
or the simulation produces a non-trivial divergence (e.g.
victim no longer a holder). Also fold the
`IceProposal` runtime gate into a type-state split — see C6.

### C4 — Blast-radius simulator does not share reconcile arms

**Where:** `meshos/ice.rs:637-848` (every `simulate_*` arm)
vs `reconcile.rs:266-319` (real `diff_forced_*`).

Plan: "the simulator must use the same reconcile arms as
real execution." Reality: every `simulate_*` is hand-written,
none invoke `reconcile()`. Concrete drift visible today:

- `simulate_force_evict_replica` reports
  `affected_nodes: vec![victim]` for every node — but real
  `diff_forced_evictions` (`reconcile.rs:266-286`) is a
  no-op on non-leaders.
- `placement_stability_delta: 0.15` is a magic constant with
  no counterpart in real reconcile.
- `simulate_force_cutover` reports only the target node; the
  real cutover displaces current holders as well.

**Fix shape:** the simulator clones the snapshot, applies
the proposal as if it landed (a synthetic
`AdminEvent::*Force*` fold), runs the same `reconcile()`
function, and diffs the resulting action list into the
`BlastRadius`. Eliminates the maintenance trap of two
parallel implementations.

### C5 — No 5-minute per-node ICE cooldown

**Where:** absent. Grep for `ice_cooldown|last_ice|per_node_cooldown`
across the repo returns zero hits. `AdminVerifier::verify_commit`
(`ice.rs:443-451`) accepts unlimited back-to-back ICE commits
against the same node target.

Plan: "After an ICE force-operation commits on a node, that
node enters a 5-minute ICE cooldown during which subsequent
ICE operations targeting the same node require an extra
signature. The cooldown rides chain metadata; every node
observes it identically."

**Fix shape:** track per-node-target last-ICE timestamps in
the verifier (or beside it on the loop). On a new
`SignedIceCommit`, look up the target node(s) from the
proposal, compare against the last-ICE timestamp, and
reject inside the cooldown window unless the bundle
carries `threshold + 1` signatures. Persist via the audit
ring so observer nodes converge on the same cooldown view.

### C6 — `IceProposal: !Send` breaks `tokio::spawn`

**Where:** `behavior/deck.rs:987-1037`. `IceProposal<'a>`
holds `std::cell::Cell<bool>` (line :990). `Cell<T>` is
`!Sync`, so `IceProposal: !Sync`. `simulate(&self)` /
`commit(self)` are `async fn` that hold the borrow across
`.await`, so the returned futures are `!Send`. Deck-the-
binary code that does `tokio::spawn(async move { p.simulate().await; p.commit(...).await })`
fails to compile.

**Fix shape:** type-state split. `IceProposal::simulate(self) -> Result<SimulatedIceProposal, IceError>`
returns a fresh type that owns the simulated `BlastRadius`
and exposes `commit(self, &[OperatorSignature])`. Eliminates
the `Cell<bool>`, eliminates the runtime
`simulation_required` error path (becomes a type error),
and dovetails with C3 (the `SimulatedIceProposal` is what
carries the `blast_hash` into the signing envelope).

### C7 — Seq counters reset on restart; rings silently truncate

**Where:** `event_loop.rs:417-418` (admin_audit_seq / log_seq
initialized to 0), `executor.rs:275` (failure_seq), plus
the FIFO truncate at `event_loop.rs:840-843` /
`event_loop.rs:875-878` / `executor.rs:524-528`.

Default `admin_audit_appender` is
`NoOpAdminAuditChainAppender` (`audit_chain.rs:130-132`).
Without operator wiring, FIFO eviction = **permanent loss
of ICE break-glass records** because the SDK reads from
snapshots, not RedEX files.

Across a runtime restart the seq resets to 0, but the
record header `LogRecord.seq` doc claims "strictly
increasing across the runtime's lifetime." A Deck SDK
client that did `since(seq=4096)` before restart will,
post-restart, see new records with `seq=1` (smaller than
its watermark — silently filtered out) and re-receive any
records the new run replays.

**Fix shape:** tag each ring's records with a `boot_id` so
the SDK detects the reset, OR persist the seq counter
across restart (initialize from `1 + chain.head_seq` if a
RedEX appender is wired). Expose a `chain_dropped` /
`ring_evicted` counter on each appender so operators can
observe divergence. Surface those counters in the
`StatusSummary` / runtime stats so a slow chain appender
isn't silently swallowed.

## B. Important — plan gaps and structural issues

### S8 — `dispatch_kill_migration_if_applicable` runs before `actual.apply`

**Where:** `event_loop.rs:692-783` (signed and unsigned
arms).

Sequence: `record_admin_audit` → `desired.apply_admin` →
`dispatch_kill_migration_if_applicable` → `actual.apply` →
`emit_maintenance_transitions`. Today `state.apply_admin`
is a no-op for `KillMigration` (`state.rs:462`), so the
ordering is benign. If a future variant adds state, or
another dispatcher gets bolted in along the same path,
the dispatcher silently observes pre-state.

**Fix shape:** move dispatcher invocations to immediately
after `actual.apply` in both arms.

### S9 — `start_with_full_extensions(None, …)` silently no-ops

**Where:** `runtime.rs:173-228` + `event_loop.rs:431`.

`None` migration_aborter installs `NoOpMigrationAborter`.
An ICE `KillMigration` commit lands on the audit chain but
the migration runs to completion. No log, no warning, no
failure-ring record.

**Fix shape:** at runtime startup, if `admin_verifier` is
wired but `migration_aborter` is the no-op, emit a
`tracing::warn!` once. On every `KillMigration` dispatch
through the no-op aborter, push a `FailureRecord {
source: "meshos-migration-aborter", reason: "no-op aborter
installed; KillMigration is a no-op" }` so it surfaces in
`subscribe_failures`.

### S10 — Migration-abort failures are logged and swallowed

**Where:** `event_loop.rs:851-867`.

`dispatch_kill_migration_if_applicable` emits
`tracing::warn!` on aborter error and returns. Operators
watching `subscribe_failures` see nothing.

**Fix shape:** push a `FailureRecord` onto the executor's
ring on every aborter error.

### S11 — Snapshot clones full rings every publish tick

**Where:** `snapshot.rs:536-538` (`from_state` clones
`actual.admin_audit` + `log_ring` verbatim into fresh
`Vec`s).

At default caps (4096 audit + 16384 log), each snapshot tick
clones every record (including owned `String`s on
`LogRecord`). Will dominate snapshot cost as rings fill.

**Fix shape:** carry the rings as `Arc<[Record]>` in the
snapshot, or expose a `from_seq` parameter on the
snapshot-builder so a caller's watermark drives the slice
size. Combined with S12 the rings move off `MeshOsState`
and the snapshot builder receives a windowed view.

### S12 — `admin_audit` / `log_ring` don't belong on `MeshOsState`

**Where:** `state.rs` (rings as fields) + `state.rs:241-260`
(dead arms for `SignedIceCommit` / `SignedAdminCommit` /
`LogLine` precisely because they don't fold).

The rings are append-only output buffers, not fold state.
They're already on the loop side via the seq counters; the
state side just clones them on snapshot publish.

**Fix shape:** move the rings onto `MeshOsLoop` (alongside
`admin_audit_seq` / `log_seq`). Pass a borrowed slice into
`from_state` the same way `recent_failures` already is
(via the executor handle, `snapshot.rs:387-393`). Combined
with S11 this also closes the full-clone path.

### S13 — Constructor stair-step in `MeshOsRuntime`

**Where:** `runtime.rs:140-228`. Six chained constructors
(`start_with_dispatcher` → `start_with_options` →
`start_with_all` → `start_with_audit_chain` →
`start_with_chains` → `start_with_all_chains` →
`start_with_full_extensions`), each adding one
`Option<Arc<dyn …>>`.

**Fix shape:** introduce `MeshOsRuntimeBuilder` with
`.with_admin_audit_appender(...)`,
`.with_log_chain_appender(...)`,
`.with_failure_chain_appender(...)`,
`.with_admin_verifier(...)`,
`.with_migration_aborter(...)`,
`.with_migration_snapshot_source(...)`,
`.with_supervision_policy(...)`,
`.build_and_start(node, dispatcher)`. Forward the existing
`start_with_*` entry points to the builder for backward
compatibility with v0.17 callers.

### S14 — `AdminCommands::freeze_cluster` / `thaw_cluster` are an unsigned backdoor

**Where:** `behavior/deck.rs:849-861` (admin) vs `:913-915`
(ICE).

`AdminCommands::freeze_cluster` / `thaw_cluster` route
through the unsigned `MeshOsEvent::AdminEvent` channel — no
simulate, no multi-op, no signature. Duplicates the ICE
surface around the plan's locked-decision #4 ceremony.

**Fix shape:** remove the duplicate `AdminCommands` methods.
Freeze and thaw stay on `IceCommands` only.

### S16 — Freeze does not gate ordinary admin commits

**Where:** `reconcile.rs:67-69` (`is_frozen` suppresses
reconcile output, not commits). `apply_admin` runs
unconditionally — `EnterMaintenance` / `Cordon` / etc.
during a freeze still update desired state.

The plan reads "freeze suppresses reconcile-driven
actions"; today's behavior matches that strictly, but the
operator-facing intent is "the cluster is paused." A
queued `Cordon` that lands during a freeze takes effect
the moment the freeze expires, which is surprising.

**Fix shape:** during freeze, route ordinary admin commits
into the audit ring as `Rejected { kind: "freeze_in_effect" }`.
ICE force-ops continue to bypass by design (break-glass).
Add an SDK error kind `freeze_in_effect`.

### S17 — Streams don't statically assert `Send`

**Where:** `behavior/deck.rs`. No
`static_assertions::assert_impl_all!(SnapshotStream: Send)`
on any public stream type. One internal field swap to a
`!Send` type silently breaks every downstream `tokio::spawn`
consumer.

**Fix shape:** add `static_assertions::assert_impl_all!`
for `Send + Sync + 'static` next to each public stream
type definition. Apply to `SnapshotStream`,
`StatusSummaryStream`, `AuditStream`, `LogStream`,
`FailureStream`.

### S18 — `emit_maintenance_transitions` reads wall-clock

**Where:** `event_loop.rs:923`. Computes
`now = Instant::now()` then `now + default_drain_deadline`,
while the fold side anchors on `self.actual.last_tick`
precisely for replay convergence.

**Fix shape:** use `self.actual.last_tick.unwrap_or_else(Instant::now)`.

### S19 — `behavior/deck.rs` is 3214 lines in one file

**Where:** `src/adapter/net/behavior/deck.rs`. ~1500 lines
of `#[cfg(test)]` at the bottom. 94 `pub` items.

**Fix shape:** split into a module tree at
`src/adapter/net/behavior/deck/`:

- `deck/mod.rs` — `DeckClient`, `DeckClientConfig`, top
  re-exports.
- `deck/identity.rs` — `OperatorIdentity` + signing helpers.
- `deck/error.rs` — `DeckError`, `AdminError`, `IceError`,
  `verify_error_to_ice`.
- `deck/admin.rs` — `AdminCommands`.
- `deck/ice.rs` — `IceCommands`, `IceProposal`,
  `SimulatedIceProposal` (per C6).
- `deck/audit.rs` — `AuditQuery`, `AuditFilter`,
  `AuditStream`.
- `deck/logs.rs` — `LogFilter`, `LogStream`.
- `deck/failures.rs` — `FailureStream`.
- `deck/streams.rs` — `SnapshotStream`,
  `StatusSummaryStream`, `StatusSummary`.

Move each type's unit tests to the new file. Keep the
public surface (`pub use deck::*` at the previous file's
path) identical.

### S20 — Integration test coverage gaps in `deck_pipeline.rs`

**Where:** `tests/deck_pipeline.rs`. ~50 tests live in
`behavior/deck.rs`'s unit block instead.

Missing from the integration surface:

- `insufficient_signatures` end-to-end (only unit).
- Multi-operator bundle accepted through the loop verifier
  (only unit; no verifier installed there).
- Audit `since(seq)` pagination across snapshot polls.
- `LogStream` with daemon/level filter through real
  `LogLine` publish.
- `subscribe_failures` over a real dispatcher rejection.
- ICE cooldown / lockout (depends on C5).
- Log ring overflow.
- `force_cutover` / `force_evict_replica` against
  populated snapshots.

**Fix shape:** promote the ICE-discipline negatives
(`insufficient_signatures`, cooldown-locked commit,
multi-op verifier round-trip, audit pagination) up into
`deck_pipeline.rs`. Add the missing surfaces.

## C. Nits

### N21 — `AuditQuery::recent(0)` silently returns empty

**Where:** `behavior/deck.rs:1183-1189`. Fluent-builder
footgun. Take `NonZeroUsize` or reject with
`invalid_argument`.

### N22 — `since(seq)` beyond head returns empty silently

**Where:** `behavior/deck.rs:1216-1224`. Expose a
`current_head_seq()` accessor for sanity-checking.

### N23 — `since(0)` semantics inconsistent across surfaces

**Where:** `behavior/deck.rs` (audit, logs, failures).
Pick one ("from beginning of ring" / "from now") and
document the boundary. The `FailureStream` docstring
contradicts its implementation.

### N24 — Redundant `wake_by_ref` after `Interval::poll_tick`

**Where:** `behavior/deck.rs:1392, 1539, 1610, 1694`.
`Interval::poll_tick(cx)` already arms the waker when it
returns `Pending`. Drop the `cx.waker().wake_by_ref();
Poll::Pending` tail and just return `Poll::Pending` after
draining.

### N25 — Test sleeps in `deck_pipeline.rs`

**Where:** `tests/deck_pipeline.rs:241, 286, 335, 384, 446`
and dozens in the unit-test block. Use
`watch_timeout(predicate, …)` (the SDK already exposes it)
instead of `tokio::time::sleep`. Same intent, no flakiness.

### N26 — `MigrationPhaseSnapshot::default = Snapshot` misleads

**Where:** `snapshot.rs:127`. Postcard errors on unknown
variants; the `Default` impl doesn't drive wire fallback.
Drop the `Default` or document its in-memory-only intent.

### N27 — `postcard::to_allocvec(...).expect("infallible")`

**Where:** `ice.rs:156, 168`. True today for the
present-day enums, fragile in the face of variant
additions. Replace with documented `unwrap_or_default()`
+ `tracing::error!`.

### N28 — `OperatorSignature::sign` panics on read-only keypairs

**Where:** `ice.rs:179-187`. Wrap in `Result` so confused
callers don't crash a UI thread.

### N29 — `BufferingMigrationAborter` silently evicts on overflow

**Where:** `migration_aborter.rs:117-127`. Expose
`dropped_count` and have tests assert against it (or
make the test buffer cap fatal-on-overflow).

### N30 — `MeshOsHandleError → DeckError` mapping is implicit

**Where:** `behavior/deck.rs:156-166`. Add
`#[non_exhaustive]` on `MeshOsHandleError` or document the
stability promise of `loop_closed` / `queue_full`.

### N31 — `AuditQuery::force_only` docstring is stale

**Where:** `behavior/deck.rs:1207-1214` (doc says "no-op
Phase 1") vs `:1252-1254` (filter does fire). Drop the
"no-op" line.

### N32 — `DeckClient` is not `Clone`

**Where:** `behavior/deck.rs:372-405`. Every field is
already cheaply cloneable. Implement `Clone`, or document
the `Arc<DeckClient>` recommendation.

### N33 — Two parallel commit paths in `IceProposal::commit`

**Where:** `behavior/deck.rs:1049` (registry branch) vs
`:1081` (no-registry branch). Duplicate discriminator
table at `:1069-1077`. Extract `IceActionProposal::kind()`.

### N34 — `OperatorIdentity::keypair()` exposes raw key

**Where:** `behavior/deck.rs:128`. Tighten to `pub(crate)`
and add `sign_bytes(&self, payload: &[u8]) -> OperatorSignature`
for legitimate external uses.

### N35 — Confirm `EntityKeypair` zeroizes secret on drop

**Where:** `OperatorIdentity` (`behavior/deck.rs:96-131`).
`Arc<EntityKeypair>` is cloned freely across SDK code; the
secret half must zeroize on drop. Audit the upstream
keypair type and document the guarantee at the SDK seam.

---

## Process

Fixes land one per commit. Critical items get regression
tests in the same commit as the fix. Important items get
regression tests where the behavior is observable through
the public surface. Nits batch into 2–3 commits at the
end. This doc updates as items close; the per-item lock
is "fix landed + regression test asserting the closure"
on the same commit.
