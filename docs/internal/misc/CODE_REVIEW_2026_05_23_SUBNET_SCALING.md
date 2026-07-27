# Code review — `subnet-scaling` branch (2026-05-23)

Branch base: `master`.
Branch tip: `5f7eebf8` ("Create AGGREGATOR_LIFECYCLE_DEFERRED_2026_05_23.md").
Scope: ~5,400 LOC across 23 commits. New `adapter/net/behavior/aggregator/` module (8 files), new `adapter/net/behavior/deck.rs`, three new deck TUI tabs (`aggregators`, `gateways`, `subnets`), four new CLI subcommand modules (`aggregator`, `channel`, `gateway`, `subnet`), modifications to `mesh.rs` / `subnet/gateway.rs` / `subnet/id.rs` / `channel/config.rs`, and a new integration test `aggregator_fold_query.rs`.

Three review agents (reuse / quality / efficiency) were dispatched in parallel. Findings below are organised by severity, then category. File paths are relative to repo root; line numbers reflect the branch tip and may drift.

---

## HIGH — correctness / maintainability risks

### H1 — `AggregatorDaemon::on_start` duplicates the entire summarize loop

`net/crates/net/src/adapter/net/behavior/aggregator/daemon.rs:404-535`.

The `LifecycleDaemon::on_start` impl reimplements the spawn loop, `produce_summaries`, `publish_summary`, `summary_channel_name`, and `append_to_latest` as the free function `run_one_tick` (lines 476-535). The author left an 11-line apologetic comment (lines 407-417) explaining the workaround: they couldn't recover an `Arc<Self>` from `&self` in the trait method.

Result: two parallel summarize-and-publish paths with byte-identical logic. The `if *kind == CapabilityFold::KIND_ID { ... } else if *kind == ReservationFold::KIND_ID { ... }` dispatch (`daemon.rs:245-286`) is duplicated verbatim at `:485-513`. Any fix to one path silently skips the other.

Fix options:
- Change `LifecycleDaemon::on_start` to take `self: Arc<Self>`. The trait impl wrapping then becomes a thin forward to a private `start_arc(self: Arc<Self>)` method.
- Or: store a `Weak<Self>` on construction; `on_start` upgrades and calls `tick_and_publish` on the resulting `Arc`.

Either path deletes `run_one_tick` (~60 lines), the apologetic comment, and the duplicated dispatch.

### H2 — `AggregatorDaemon` re-publishes byte-identical summaries every tick

`net/crates/net/src/adapter/net/behavior/aggregator/daemon.rs:229-238` and `:476-526`.

Every `summary_interval`, the loop calls `produce_summaries()` → `publish_summary()` unconditionally, even when the resulting buckets are byte-identical to the previous batch. With `LATEST_SUMMARIES_CAP=32` and folds (capability, reservation) that change rarely, this is repeated postcard-encoding + per-fold-kind channel publishes for no reason.

Fix: before `publish_summary`, compare new summary to the prior `latest` entry by `(fold_kind, buckets)` and skip both `publish_summary` and the latest-buffer push when identical. `generation` bump becomes optional or guarded.

### H3 — `FoldQueryClient` cache is unbounded and never TTL-evicts

`net/crates/net/src/adapter/net/behavior/aggregator/query_client.rs:84-167`.

Cache key is `(target_node_id, service, kind)`. Entries are inserted on every miss; nothing scans for expired entries. The "TTL" only controls hit/miss freshness, not eviction. A long-running deck that queries many targets accumulates dead entries forever.

Additionally, `CacheKey::service: String` (`:68-73`) allocates a fresh `String` per call via `service.to_string()` (`:145`), so the cache key allocates even on hits. The default `FOLD_QUERY_SERVICE` is `&'static str`; key on `Cow<'static, str>` or drop `service` from the key (use a per-service client instead).

Fix: opportunistic eviction during `query_with_service` (drop any expired entry encountered en route), or a periodic sweep.

---

## MEDIUM — quality / hygiene

### M1 — Cheating test asserts nothing

`net/crates/net/cli/src/commands/aggregator.rs:251-256`.

```rust
#[test]
fn _summary_interval_seconds_round_trips_zero() {
    let _ = Duration::ZERO.as_secs_f64();
}
```

Delete.

### M2 — CLI hex/decimal parsers duplicated four ways

`net/crates/net/cli/src/commands/aggregator.rs:147-167` and `gateway.rs:157-177`.

`parse_u64` / `parse_u16` reimplement the exact `0x`/decimal branching that `cli/src/parsers.rs::parse_u64_flexible` already provides with tests. Hoist a `parse_u16_flexible` one-liner into `parsers.rs` and delete all four local copies. Pass as clap `value_parser`s the way the rest of the CLI does, instead of hand-validating in run handlers.

### M3 — `parse_channel_hash` silently returns `Ok(0)` for non-numeric names

`net/crates/net/cli/src/commands/gateway.rs:157-177`.

Function contract is "parse a hash" but it returns `Ok(0)` ("sentinel for the write path") on parse failure. The test at line 280 asserts this misbehavior. Either fail with a clear "name lookup not supported in read-only CLI" error, or return `Result<ChannelHashSource, _>` with a `Name(String)` variant.

### M4 — Placeholder commands parse args then unconditionally error

`net/crates/net/cli/src/commands/aggregator.rs:120-143` (`run_query`) and `gateway.rs:130-151` (`run_export`).

Both are 100% placeholder errors that still accept and parse flags. Surprising for an operator who fills in a long command line. Either gate behind `#[cfg(feature = "write-attach")]` or move to a `_deferred/` shim until the write path lands.

### M5 — `profile_node_id` always returns `0`

`net/crates/net/cli/src/commands/subnet.rs:194-199`.

Returns a fake `0` with a TODO comment. Change signature to `Option<u64>` so callers can render `"—"` honestly. Otherwise CLI output asserts a node identity that doesn't exist.

### M6 — Dead `#[allow(dead_code)]` stub with prose comment

`net/crates/net/src/adapter/net/behavior/aggregator/query_client.rs:405-418`.

`_aggregator_with_summary` is dead, gated by `#[allow(dead_code)]`, with a 10-line essay about "lands when …". Delete; if the prose is load-bearing, move it to a `_deferred/` doc.

### M7 — Comment narration violates project rule

Project rule: comments should explain WHY, not WHAT.

- `daemon.rs:407-417` — 11-line apologetic block about Arc resolution (evaporates with H1 fix).
- `query_client.rs:391-401` — 10-line essay explaining why a test doesn't exist.
- `aggregator/mod.rs:1-37` — repeats the design doc; one-line + link to `SCALING_SUBNET_SPEC.md` is enough.
- Many `pub fn`s in `deck.rs:738-782` have rustdoc that just restates the function name (`aggregator_installed` → "`true` when a live AggregatorDaemon is installed…").

### M8 — Test brittleness: fixed sleeps as synchronization

Files: `aggregator_fold_query.rs:272`, `daemon.rs:674,691,912,921,945`, `group.rs:195,207`, `lifecycle.rs:220`.

Many tests use `tokio::time::sleep(Duration::from_millis(75|85|90|100))` then assert `generation() >= 1`. On a loaded CI runner the 20ms-interval loop can easily miss its first tick before the 75ms wake-up. `wire_publish_summary_reaches_subscriber_on_remote_node` (`aggregator_fold_query.rs:272`) uses a single 100ms sleep before assuming subscribe-membership propagation.

Prefer `tokio::time::pause()` + `advance()` for timer-driven cases, or poll-until-condition with a 2–3 s outer timeout.

### M9 — Inconsistent error handling

- `daemon.rs:313-314, 520-521` — postcard error stringified with `format!("{e:?}")` rather than `{e}`; debug-format is unstable across versions.
- `query_service.rs:175-188` — `encode_response` swallows encode failure to empty bytes (comment admits the client sees a decode error and retries). At minimum log a `tracing::warn!`.
- `lifecycle.rs:137-142` — `Drop::drop` silently skips shutdown when no tokio runtime is current; comment claims "the daemon's internal task cleans itself up via its shutdown flag on drop of its own Arc" but that contract isn't enforced anywhere.

---

## MEDIUM — efficiency

### E1 — `LATEST_SUMMARIES_CAP` eviction is O(n) per tick

`net/crates/net/src/adapter/net/behavior/aggregator/daemon.rs:290-298, :527-533`.

`latest.remove(0)` shifts every remaining element. For cap=32 it's tiny per call but the loop runs forever. Use `VecDeque::pop_front` / `push_back`.

### E2 — `AggregatorGroup::spawn` starts replicas sequentially

`net/crates/net/src/adapter/net/behavior/aggregator/group.rs:78-87`.

`for index in 0..replica_count { LifecycleHandle::start(...).await? }`. Each `on_start` awaits tokio spawn registration. For N replicas this is N round-trips. Use `futures::future::try_join_all` over the start futures.

### E3 — Deck tab `render` rebuilds groupings every frame

Files: `deck/src/tabs/subnets.rs:62-66, 96-120`, `tabs/aggregators.rs:107`, `tabs/gateways.rs:100-105`.

At ~8 fps each tab calls accessor → groups into `BTreeMap<u32, BTreeSet<u64>>` (or sorts a Vec) → builds rows. For a small mesh this is fine; at hundreds of peers it's wasted work since the data rarely changes.

Cache the grouping keyed by `known_subnets().len()` and the highest node_id seen; rebuild only when those change.

### E4 — `DeckClient::aggregator_*` clones five times per frame

`net/crates/net/src/adapter/net/behavior/deck.rs:746-782`.

The aggregators tab calls `aggregator_source_subnet()`, `aggregator_fold_kinds()` (Vec clone), `aggregator_generation()`, `aggregator_summary_interval()`, `aggregator_summaries()` (Vec clone of up to 32 with internal `Vec<(String,u64)>` buckets). Five hops + two Vec clones per frame.

Collapse to a single `aggregator_snapshot()` returning one struct, computed under one borrow.

### E5 — `latest_summaries()` is a deep clone on every read

`net/crates/net/src/adapter/net/behavior/aggregator/daemon.rs:373-375`.

Clones `Vec<SummaryAnnouncement>` including every `Vec<(String,u64)>` bucket and every `String` key. The TUI calls this every frame even when the buffer is unchanged. Pair with E4: emit `Arc<Vec<SummaryAnnouncement>>` and return `Arc::clone` (cheap).

### E6 — `query_service::answer` SummarizeNow pulls the full buffer

`net/crates/net/src/adapter/net/behavior/aggregator/query_service.rs:155-172`.

`SummarizeNow` calls `tick_once()` (write), then `latest_summaries()` (full clone of up to 32), then filters. Have `tick_once` return the just-produced batch directly so the filter walks 1–2 entries.

### E7 — `daemon.rs::on_start` clones every field manually

`net/crates/net/src/adapter/net/behavior/aggregator/daemon.rs:404-456`.

The H1 workaround clones `shutdown`, `generation`, `latest`, `summarizers` HashMap (cloning each `Arc`), `fold_kinds` Vec, `mesh` Arc, etc. on every start. Fix evaporates once H1 lands.

---

## LOW — reuse / nits

### L1 — `FoldQueryClient::issue_call` reproduces `Mesh::call_typed`

`net/crates/net/src/adapter/net/behavior/aggregator/query_client.rs:202-224`.

Encode/`mesh.call`/decode is exactly what `sdk/src/mesh_rpc.rs::Mesh::call_typed` (`:372-394`) does. The blocker is JSON-only `Codec`. Add `Codec::Postcard` (single match arm in `encode`/`decode`) and have `FoldQueryClient` use `call_typed::<FoldQueryRequest, FoldQueryResponse>` with `Codec::Postcard`. Removes postcard error-mapping boilerplate.

### L2 — Bucket-by-subnet logic duplicated across CLI and deck

`net/crates/net/cli/src/commands/subnet.rs:107-126` and `deck/src/tabs/subnets.rs:62-65, 96-104`.

Both walk `known_subnets()` into a `BTreeMap<u32, BTreeSet<u64>>`. Should live as a method on `DeckClient` (e.g. `subnets_with_members() -> Vec<(SubnetId, Vec<u64>)>`) so both surfaces consume the same rollup. Matches the pattern already established by `gateway_stats()` / `gateway_exports()`.

### L3 — `AggregatorGroup` parallels `compute/replica_group.rs` but ignores `GroupCoordinator`

`net/crates/net/src/adapter/net/behavior/aggregator/group.rs:51-126`.

Correctly reuses `derive_replica_keypair`, but the per-replica `LifecycleHandle` collection-and-stop pattern mirrors what `compute/replica_group.rs` + `compute/group_coord.rs` already do for sync daemons. If `LifecycleDaemon` is the long-term async sibling, `GroupCoordinator` should grow async variants rather than `AggregatorGroup` re-inventing member tracking. Acceptable for one slice; flag for follow-up consolidation.

### L4 — `SubnetGateway::peer_subnets()` re-sorts on every call

`net/crates/net/src/adapter/net/subnet/gateway.rs:105-109`.

Stored as `RwLock<Vec<SubnetId>>` unsorted, sorted on every snapshot. `add_peer` is rare and uses `contains` (O(n)); keep the Vec sorted on insert via `binary_search + insert`, so the read path is a single `clone()`. Same pattern applies to `gateway.rs:120-128` (`exports`) — DashMap iteration into Vec + sort on every render.

### L5 — `ReservationFoldSummarizer` allocates a `String` per entry

`net/crates/net/src/adapter/net/behavior/aggregator/summarizer.rs:166-174`.

`format!("{:?}").to_lowercase()` once per reservation per tick. Match on the variant directly to a static `&'static str` for the common cases; fall back to format only for `Reserved { ... }`.

### L6 — `ChannelConfigRegistry::snapshot()` clones every `ChannelConfig`

`net/crates/net/src/adapter/net/channel/config.rs:377-385`.

Right shape for a CLI command but expensive if pulled into a render loop. `channel_visibility(name)` already exists for single lookups.

### L7 — `parse_subnet` is the first of many

`net/crates/net/cli/src/commands/gateway.rs:181-204`.

No existing `FromStr for SubnetId`. Not a duplicate today, but the same parser will be needed by every future `net …` command that takes a subnet arg. Promote to `SubnetId::from_str` (or `parsers::parse_subnet`) before the second caller appears.

### L8 — CLI handler parameter sprawl

Every `cli/src/commands/*.rs` `run_*` fn takes `(args, output, config_path, profile_name)` — 4 args, 3 of which are pure plumbing re-passed unchanged from the top-level dispatcher. Wrap the three plumbing args in a `CliEnv { output, config_path, profile_name }` carrier.

### L9 — Mesh accessor sprawl / deck pass-throughs

`mesh.rs` exposes `local_subnet`, `known_subnets`, `gateway`, `channel_configs`, `set_channel_configs`, `capability_fold`, `reservation_fold` (`:2213, :2234, :5299-5326, :8915, :8926`). `DeckClient` wraps every one of them in a thin `Option<…>`-returning method (`deck.rs:644-782`) — pure pass-through.

Consider a single `MeshObserver` trait that the deck holds, with the seven accessors as methods; mock-friendly, keeps the substrate `MeshNode` from leaking internals.

### L10 — Tab title boilerplate

`tabs/aggregators.rs` / `gateways.rs` / `subnets.rs` repeat the empty-state title block and section-prefix construction. Matches the rest of `deck/src/tabs/` so the convention is established, but a `widgets::section_title(name, subtitle)` helper would collapse ~10 lines per tab.

---

## False positives noted during the pass

- **Reservation `class` mismatch.** None — the per-fold-kind channel name in `aggregator/daemon.rs:516` uses `KIND_ID`, which is the intended sharding axis. (Mirrors the same false positive that was flagged in the multifold pass.)

---

## Clean areas

- **Nested conditionals:** No 3+ level deep nesting found. Good use of `let-else` and early returns throughout.
- **`SubnetGateway`:** clean separation of `should_forward` from counter recording; regression tests cite the BUG_AUDIT issue.
- **`ChannelConfigRegistry`** snapshot accessors are minimal and stable; collision-safety is well-tested.
- **`SubnetId`:** clean bit-twiddling, fallible `try_new`, good regression coverage.
- **`LifecycleHandle`:** the type-state split (`IceProposal` → `simulate()` → `SimulatedIceProposal::commit`) is exemplary.
- **Locks:** `parking_lot::RwLock` / `DashMap` reads are brief, no held-across-await issues found.
- **Atomic counters** on gateway are `Relaxed` — correct for monotonic counters.
- **`AggregatorGroup` replica buffers** are bounded by `replica_count: u8`.

---

## Suggested fix order

1. **H1** (Arc<Self> refactor) — unblocks E7 and removes the duplicated dispatch.
2. **H2** (change-detection guard) — biggest steady-state efficiency win.
3. **H3** (TTL eviction + key allocation) — only unbounded-growth path in the diff.
4. **M1, M2, M5, M6** (dead test, parser hoist, profile_node_id, dead stub) — trivial.
5. **E1, E2, L5** (VecDeque, try_join_all, &'static str) — trivial.
6. **M3, M4, M8, M9** (silent-failure parser, placeholder commands, sleep tests, error handling).
7. **E3, E4, E5, E6** (snapshot consolidation in deck/query paths).
8. **L1–L10** as time permits or in a follow-up pass.
