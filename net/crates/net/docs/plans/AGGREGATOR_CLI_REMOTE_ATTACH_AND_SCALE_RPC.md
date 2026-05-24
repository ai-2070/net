# Aggregator CLI remote-attach + dedicated Scale RPC

Branch: `subnet-scaling`.
Predecessor: `docs/plans/AGGREGATOR_LIFECYCLE_DEFERRED_2026_05_23.md` — every AL-1..AL-8 item resolved, slices 7–9 + SDK stages 0–6 + tidy passes T1..T14 landed.
Scope: close the two surfaces called out in that doc's "Remaining gap" section (`AGGREGATOR_LIFECYCLE_DEFERRED_2026_05_23.md:163-164`):

1. **CLI remote-attach** — `net aggregator query / spawn / scale` are parse-only today (`net/crates/net/cli/src/commands/aggregator.rs:210-235`, `:264-294`, `:296-312`). The wire surface (`RegistryClient`, `FoldQueryClient`) is shipped and proven by `tests/aggregator_registry_rpc.rs` + `tests/aggregator_fold_query.rs` + `aggregator-daemon/tests/boot_and_query.rs`. The gap is `CliContext` — it owns an in-process `MeshOsDaemonSdk` + `DeckClient` but **no `MeshNode`** the typed clients can route through (`cli/src/context.rs:29-110`). NET_CLI_PLAN.md Phase 5 (line 542-544) tracked this generically as a Deck-RPC remote-attach gap; we close the aggregator slice of it now, leaving the broader Deck-RPC surface for that plan.
2. **Dedicated Scale RPC** — `Scale` is implementable today as `Unregister` + `Spawn(replica_count=N)` (`registry_service.rs:25-31`). The substrate doesn't yet have `LifecycleGroup::add_replica` / `remove_last`, so any in-place grow/shrink tears down + re-derives identity. We add the helpers, a `RegistryRequest::Scale` variant, server-side dispatch, client method, and the CLI flip.

Two independent gaps. **Recommended order: ship Gap 1 first** — it unblocks the CLI's three "preview" verbs which operators are already reading the help text for, and the Scale work is small and additive on top of a live CLI. Gap 2 can ship in parallel from the substrate side but its visible payoff is `net aggregator scale` flipping from "Unregister + Spawn churn" to "in-place add/remove," which only matters once the CLI verb is live.

Tagged `[A | B | C]`:

- A — CLI remote-attach (Gap 1)
- B — Dedicated Scale RPC (Gap 2)
- C — Closeout (smoke / docs / tidy)

---

## Status

| ID    | Pri | Area                  | Title                                                                                       |
|-------|-----|-----------------------|---------------------------------------------------------------------------------------------|
| A-1   | H   | CLI bootstrap         | `CliContext::with_mesh` — optional `Arc<MeshNode>` + `--node-addr` / `--node-pubkey` flags  |
| A-2   | H   | wire: query           | flip `net aggregator query` from parse-only to `FoldQueryClient` round-trip                 |
| A-3   | H   | wire: spawn           | flip `net aggregator spawn` to `RegistryClient::spawn(template_name, …)`                    |
| A-4   | H   | wire: scale (interim) | flip `net aggregator scale` to Unregister + Spawn against `RegistryClient`                  |
| A-5   | M   | wire: ls (remote)     | `net aggregator ls --remote` flips to `RegistryClient::list` against `--node-addr`          |
| A-6   | M   | tests                 | `crates/net/cli/tests/aggregator_remote.rs` — spawn daemon subprocess + assert CLI behaviour |
| B-1   | H   | substrate             | `LifecycleGroup::add_replica` + `remove_last` helpers                                       |
| B-2   | H   | wire enum             | `RegistryRequest::Scale { group_name, target_replica_count }` + reply variant               |
| B-3   | H   | server                | `RegistryHandler` dispatch + `AggregatorRegistry::scale_group` action                        |
| B-4   | H   | client                | `RegistryClient::scale(target, group_name, count)` + `BoundRegistryClient::scale`           |
| B-5   | H   | CLI flip              | `net aggregator scale` flips from interim Unregister+Spawn to dedicated `Scale` RPC          |
| B-6   | M   | tests                 | round-trip Scale integration test (`tests/aggregator_registry_rpc.rs` extension)            |
| C-1   | L   | docs                  | NET_CLI_PLAN.md + AGGREGATOR_LIFECYCLE_DEFERRED note: aggregator slice of Phase 5 closed     |
| C-2   | L   | tidy                  | drop "[preview]" markers + parse-only error copy from `cli/src/commands/aggregator.rs`       |

---

## Gap 1 — CLI remote-attach

### A-1 — `CliContext::with_mesh` + `--node-addr` / `--node-pubkey` / `--remote` flags

**Why this slice first.** Every other A-* slice needs `ctx.mesh_node()` to construct a typed client. Today `CliContext` (cli/src/context.rs:29-110) holds `_sdk: MeshOsDaemonSdk` and `deck: Arc<DeckClient>` — neither carries a live `MeshNode` the substrate's RPC primitives can route through. NET_CLI_PLAN.md Phase 5 frames the broader Deck-RPC remote-attach work; we land only the aggregator-shaped piece here.

**Bootstrap shape.** Two construction modes, gated by flags:

- **In-process** (default; preserves today's behaviour for `inspect` / `ls`). `CliContext::build` continues to construct a `MeshOsDaemonSdk` for `deck` access; **additionally** boots a lightweight `Arc<MeshNode>` against an ephemeral `127.0.0.1:0` socket via the same primitives `aggregator-daemon` uses (`MeshNode::new` + `MeshNodeConfig::new` — see `aggregator-daemon/src/lib.rs:308-316`). Used when the operator only needs read-only views of the **local** registry / daemon.
- **Remote-attach** (new; activates A-2 / A-3 / A-4 / A-5). When `--node-addr <IP:PORT> --node-pubkey <HEX>` are passed, `CliContext::build` mirrors the round-trip test bootstrap (`tests/aggregator_registry_rpc.rs:55-71`, `aggregator-daemon/tests/boot_and_query.rs:107-124`):
  1. Build a local `MeshNode` on `127.0.0.1:0` with a PSK from the profile (config addition: `[default].psk_hex`).
  2. `node.connect(remote_addr, &remote_pubkey, remote_node_id).await` — the same call path the round-trip tests use.
  3. `node.start()`.
  4. Cache the `Arc<MeshNode>` on `CliContext::mesh_node: Option<Arc<MeshNode>>`.

The daemon's `--print-bootstrap` flag (`aggregator-daemon/src/lib.rs:88-94, 248-260`) emits `{"node_id":N,"bound_addr":"IP:PORT","public_key_hex":"<64 hex>"}` — operators copy these fields into the CLI flags or into the config file. Phase 5 of NET_CLI_PLAN.md describes the broader `--endpoint` shape; we adopt the same pattern but scoped to `--node-addr` / `--node-pubkey` / `--node-id` for clarity (`--endpoint mesh://...` is the Deck-RPC framing; the aggregator RPC is a substrate-level surface).

**Files touched (A-1).**
- `cli/src/context.rs` — add `mesh_node: Option<Arc<MeshNode>>` to `CliContext`; add `CliContext::mesh_node()` accessor; extend `build()` to accept a `RemoteAttach` option (see below); reuse `MeshNode::new` + `MeshNodeConfig::new`.
- `cli/src/context.rs` — new `RemoteAttach { addr: SocketAddr, pubkey: [u8;32], node_id: u64, psk: [u8;32] }` builder helper. Parses `0x`-prefixed hex via `cli/src/parsers.rs::parse_u64_flexible` (already exists; used in `commands/aggregator.rs:218-221`). 32-byte hex decode mirrors the existing `hex_decode_32` (`commands/aggregator.rs:318-323`) — pull it up into `parsers.rs` to dedupe.
- `cli/src/config.rs` — add `[default].psk_hex` (optional `String`, parsed at build time, fail-fast on bad length); add `[default].node_addr` / `node_pubkey` / `node_id` as bootstrap shortcuts when always pointing at the same daemon. Profile precedence already exists; flags override.
- `cli/src/commands/aggregator.rs` — each of `QueryArgs` / `SpawnArgs` / `ScaleArgs` / `LsArgs` grows `--node-addr`, `--node-pubkey`, `--node-id` (or `--remote <NAME>` shortcut that pulls all three from the profile). `--node` (the existing supervisor-id flag) stays for in-process semantics.
- `cli/src/error.rs` — new variant `CliError::RemoteAttach(String)` mapped to exit code 6 ("connection / handshake failure" per NET_CLI_PLAN.md exit table line 219).

**Test plan (A-1).**
- `cli/tests/context_remote.rs` — `CliContext::build` with `RemoteAttach { … }` against a `MeshNode` host the test spins up. Confirms (a) handshake lands, (b) `mesh_node()` returns `Some`, (c) bad pubkey / wrong PSK / wrong node_id all map to typed errors with exit code 6.
- Reuse the round-trip pattern from `tests/aggregator_registry_rpc.rs::handshake` — same shape, but invoked from the CLI test rather than the substrate test.

### A-2 — `net aggregator query` round-trip

Today `run_query` (`commands/aggregator.rs:210-235`) parses `target` + `kind` then bails. Flip to:

1. Require `--node-addr` / `--node-pubkey` / `--node-id` (or `--remote <NAME>`). Without any of them, error with a copy that points at A-1's flags (`CliError::invalid_args`).
2. `ctx.mesh_node()` → unwrap or error with same message.
3. `FoldQueryClient::new(mesh).with_deadline(...)` — substrate type already exposed at `net::adapter::net::behavior::aggregator::FoldQueryClient` (`registry_client.rs:67-89` shape; same `with_deadline` builder).
4. `args.fresh ? client.query_summarize_now(target, kind) : client.query_latest(target, kind)` — branches match the substrate's wire surface (`summarize_now_forces_fresh_tick_on_host` test at `tests/aggregator_fold_query.rs:179-195` pins the cache-bypass path).
5. Render the `Vec<SummaryAnnouncement>` as JSON via the existing `SummaryRow` shape (`commands/aggregator.rs:415-443`) — same fields, no new view type needed.

`--node` (the existing in-process supervisor flag) becomes ignored on `query` when `--node-addr` is set; the supervisor node_id is meaningless once we're talking to a remote daemon. Document the precedence in the help text.

**Files touched (A-2).**
- `cli/src/commands/aggregator.rs` — rewrite `run_query`; reuse `SummaryRow` for output.
- Drop the "preview" doc-comment block on `AggregatorCommand::Query`.

**Test plan (A-2).**
- Extend `cli/tests/aggregator_remote.rs` (new in A-6 — see below): spin an `aggregator-daemon` subprocess with a `[[group]]` config, parse its `--print-bootstrap` line, invoke `net aggregator query` against it, assert the JSON output decodes back to a `SummaryAnnouncement`-shaped object with the expected `source_subnet` / `fold_kind`.

### A-3 — `net aggregator spawn` round-trip

`run_spawn` (`commands/aggregator.rs:264-294`) today validates `replica_count > 0` + parses `source_subnet` + optional `group_seed` hex. The wire surface needs:

- **Template name**, not raw `source_subnet`. The daemon's `Spawn` RPC takes `template_name` and resolves it against operator-config templates (`registry_service.rs:60-69`, `aggregator-daemon/src/lib.rs:142-159, 612-649`). The operator must register a `[[template]]` block in the daemon config first.

Current CLI args are wrong for the wire shape. Two paths:

1. **Add `--template <NAME>` flag**, deprecate `--source-subnet`. Cleaner; matches the wire surface 1:1.
2. **Auto-derive: pick a template whose `source_subnet` matches the operator-supplied `--source-subnet`.** Requires a new `RegistryRequest::ListTemplates` op or an extension to `RegistryResponse::Groups`. More wire churn; reject this path.

**Recommend path 1.** Add `--template <NAME>` (required). Keep `--source-subnet` as a parse-validated **assertion** — the CLI parses the flag if present, and after `Spawn` returns, asserts the `RegistryGroupSummary.source_subnet` (which the wire reply doesn't carry today — see below) matches. Or simpler: drop `--source-subnet` from `SpawnArgs` entirely, since the template owns it.

**Wire-protocol bump consideration.** `RegistryGroupSummary` carries only `name` + `group_seed` + `replicas` (`registry_service.rs:100-108`). It does **not** carry `source_subnet` or `fold_kinds`. The CLI's output today renders those from the **local** snapshot (`LsGroupRow`, `LsReplicaRow`); over the wire it has neither. For the spawn output, that's tolerable — the operator named the template; they know its source-subnet. For `ls --remote`, this is a real gap — covered in A-5.

**Files touched (A-3).**
- `cli/src/commands/aggregator.rs::SpawnArgs` — drop `source_subnet` + `group_seed`; add `template: String`. Keep `name` + `replica_count`. (Note: `group_seed` was always derived from `name` daemon-side anyway via `derive_seed_from_name`, `aggregator-daemon/src/lib.rs:724-734` — the CLI's flag was always ignored once wire dispatch landed.)
- `cli/src/commands/aggregator.rs::run_spawn` — call `RegistryClient::spawn(target_node_id, template_name, group_name, replica_count)` via `ctx.mesh_node()`.
- Update help text on the verb to point at template-driven deployment.
- `--source-subnet` removal is a breaking CLI change. Mention it explicitly in C-1 docs; no scripted-CI consumer exists today (the verb has always been parse-only error).

**Test plan (A-3).**
- `cli/tests/aggregator_remote.rs::spawn_round_trip` — daemon config with `[[template]]` block; CLI `net aggregator spawn --template primary --name dynamic --replica-count 2 --node-addr … --node-pubkey … --node-id …`; assert exit 0, JSON shape on stdout, then `net aggregator ls --remote` shows the new group.

### A-4 — `net aggregator scale` interim (Unregister + Spawn)

Per Gap 2's framing, Scale is implementable today as Unregister + Spawn. Ship A-4 against that contract; B-5 later flips the implementation to the dedicated `Scale` op without changing the CLI shape.

`run_scale` (`commands/aggregator.rs:296-312`) currently validates `replica_count > 0` then bails. Flip to:

1. `client.list(target)` → find group by `args.name`; if absent, exit code 10 ("subcommand-specific" — A-4 owns 13 for "group not found").
2. **Template lookup**: the current group's template is **not** carried in the wire snapshot. Operator must supply `--template <NAME>` to scale — same flag A-3 added. This is the interim's worst seam; B-5 + B-3 close it by letting the daemon look up the in-memory aggregator's spec by group name.
3. `client.unregister(target, name)`; if `existed == false`, exit code 13 with a diagnostic.
4. `client.spawn(target, template, name, replica_count)`.

**Documented limitation:** the interim churns the entire group (every replica gets a fresh keypair via `derive_seed_from_name(name)` — same name, same derivation, **same** identity — so identity continuity *is* preserved between the old and new group, but every replica's tokio loop restarts and rebuilds fold state). Acceptable as an interim; B-5 removes the churn.

**Files touched (A-4).**
- `cli/src/commands/aggregator.rs::ScaleArgs` — add `template: String`.
- `cli/src/commands/aggregator.rs::run_scale` — implement the two-step.
- Exit-code table addition (cli/src/error.rs): code 13 = "aggregator group not found on target."

**Test plan (A-4).**
- `cli/tests/aggregator_remote.rs::scale_grows_then_shrinks` — start daemon with template, spawn at 2, scale to 4, assert `list` shows 4 replicas; scale back to 1, assert `list` shows 1.

### A-5 — `net aggregator ls --remote`

Today `run_ls` reads from the local `DeckClient::aggregator_registry_snapshot()` (`commands/aggregator.rs:237-262`). Add a `--remote` flag (or auto-trigger when `--node-addr` is set) that uses `RegistryClient::list(target)` instead and renders via a new `RemoteLsGroupRow` shape.

**The wire shape's missing fields** (`RegistryGroupSummary` lacks `source_subnet` + `fold_kinds`) is a real gap for `ls --remote`. Three options:

1. **Show "(template metadata unavailable)" placeholder.** Cheap; loses information operators want.
2. **Extend `RegistryGroupSummary`** with `source_subnet: SubnetId` + `fold_kinds: Vec<u16>`. Append to the struct; no back-compat constraint.
3. **Add `RegistryRequest::Describe { group_name }`** returning the full spec. Adds a wire op.

**Recommend option 2.** The daemon already owns the spec (`AggregatorSpec` in `aggregator-daemon/src/lib.rs:390-465`); piping `source_subnet` + `fold_kinds` into `snapshot_group` (`registry_service.rs:304-312`) is a local refactor.

Defer this wire change to **B-2's scope** — combine it with the `Scale` variant addition so the wire surface lands in one step.

**A-5 in the meantime** can ship without the wire fields by using option 1 (placeholder) — operator who needs the full spec runs `query` against a replica's metadata. Land this slice without blocking on B-2; flip the placeholder to live data once B-2 is in.

**Files touched (A-5).**
- `cli/src/commands/aggregator.rs::LsArgs` — gate `--remote` (off by default; on when `--node-addr` is present).
- `cli/src/commands/aggregator.rs::run_ls` — branch on remote vs local; new `RemoteLsView` Serialize struct.

**Test plan (A-5).**
- `cli/tests/aggregator_remote.rs::ls_remote_lists_dynamic_groups` — spawn two groups via the daemon's `[[group]]` config, run `net aggregator ls --remote`, assert JSON contains both group names.

### A-6 — `cli/tests/aggregator_remote.rs` integration test

Mirrors `aggregator-daemon/tests/boot_and_query.rs:71-148` pattern but invokes the **CLI binary** rather than the library helpers. Uses `assert_cmd` (already a CLI dev-dep per NET_CLI_PLAN.md `cli/Cargo.toml:290-293`).

**Test shape.**
1. Boot the daemon via `boot()` directly (avoids subprocess overhead; same trick the bootstrap pin test uses for the binary path — see `aggregator-daemon/tests/boot_and_query.rs:96-105`).
2. Read `booted.mesh.node_id()`, `booted.bound_addr`, `booted.public_key`.
3. Use `assert_cmd::Command::cargo_bin("net")` to invoke each verb:
   - `net aggregator query --kind 0x0001 --node-addr <addr> --node-pubkey <hex> --node-id <n> 0` — assert exit 0 + JSON shape.
   - `net aggregator spawn --template primary --name newgrp --replica-count 2 …` — assert exit 0.
   - `net aggregator scale --template primary --name newgrp --replica-count 4 …` — assert exit 0.
   - `net aggregator ls --remote …` — assert JSON enumerates registered groups.
4. Negative cases: bad pubkey (exit 6), wrong PSK (exit 6), no template (server-side `UnknownTemplate` → exit 3).

**Files touched (A-6).**
- New `crates/net/cli/tests/aggregator_remote.rs`.
- `crates/net/cli/Cargo.toml` — add `tempfile`, `net-aggregator-daemon` to `[dev-dependencies]` (or feature-gate behind the same `cli` cargo feature the workspace uses).

---

## Gap 2 — Dedicated `Scale` RPC op

### B-1 — `LifecycleGroup::add_replica` / `remove_last`

Today `LifecycleGroup` (`adapter/net/behavior/lifecycle/group.rs:123-404`) has no in-place grow/shrink — only `replace` (single-slot swap, `:326-361`) and `stop` (consumes the whole group, `:373-377`). Add:

**`add_replica`** (mirror `start_replicas` for one slot):
- Signature: `pub async fn add_replica<F>(&mut self, factory: F) -> Result<u8, LifecycleGroupError> where F: FnOnce(u8) -> Arc<L>`. Returns the new index (= old `replica_count`).
- Validation: refuse when `replica_count == u8::MAX` (the substrate's u8-bound on group size — see `LifecycleGroup::spawn`'s use of `u8` and the `RegistryReplicaSummary` shape).
- Construction: `let new_idx = self.replicas.len() as u8; let daemon = factory(new_idx); let handle = LifecycleHandle::start(daemon.clone()).await?;` — matches the per-replica pattern in `start_replicas` (`group.rs:412-446`). Pushes onto both `self.replicas` and `self.handles`. Placement Vec stays empty under the placement-free path (matching `spawn`'s contract that placements are only recorded under `spawn_with_placement`).

**`remove_last`**:
- Signature: `pub async fn remove_last(&mut self) -> Result<Arc<L>, LifecycleGroupError>`. Returns the stopped replica's Arc so callers can inspect post-stop state.
- Validation: refuse when `replica_count == 1` (caller can `stop()` the whole group instead) — return `LifecycleGroupError::InvalidConfig("cannot remove last replica; stop the group instead")`. Or accept it; daemons designed around N≥1 invariant should refuse at the registry layer. Recommend refusing at the LifecycleGroup layer for safety; AggregatorRegistry can layer a friendlier error on top.
- Construction: pop from `self.handles`, await its `stop()`, pop from `self.replicas`. If the group used `spawn_with_placement`, also pop from `self.placements` to keep the parallel-Vec invariant.

**Why deterministic-last as the victim.** The plan's locked decision: scaling down preserves the lowest-indexed replicas. Symmetric with `add_replica` always taking the next-highest index. The alternative — picking the unhealthiest replica — couples scale to the health snapshot; that's `replace`'s job. Scale is pure resize.

**Files touched (B-1).**
- `crates/net/src/adapter/net/behavior/lifecycle/group.rs` — two new methods on `impl<L: LifecycleDaemon> LifecycleGroup<L>`.
- Existing tests in `group.rs` already cover the parallel-Vec invariant (e.g. `spawn_path_leaves_placements_empty`, `:778-790`). Mirror that style.

**Test plan (B-1).**
- `group.rs::add_replica_grows_in_place_preserving_existing_replicas` — spawn 2, `add_replica`, assert (a) `replica_count == 3`, (b) original replicas' `starts` counter still equals 1 (NOT 2 — no restart), (c) new replica's `starts == 1`.
- `group.rs::remove_last_stops_only_the_last_replica` — spawn 3, `remove_last`, assert (a) `replica_count == 2`, (b) returned Arc is the original index-2 replica, (c) replicas 0+1's `stops` counter is still 0.
- `group.rs::remove_last_refuses_to_drop_below_one` — spawn 1, `remove_last`, assert `Err(InvalidConfig)`.
- `group.rs::add_replica_at_u8_max_returns_error` — synthetic / mocked u8::MAX check. Doc test only; not worth the runtime cost.
- `group.rs::add_replica_under_placement_path` — `spawn_with_placement` with 2 replicas, `add_replica` extends the group but **doesn't** record a placement decision (the new replica runs on the local node; the placement-spread invariant only applies at `spawn_with_placement` time). Document this in the method's docstring as a known limitation; placement-aware scale is out-of-scope for this slice.

### B-2 — Wire-protocol additions: `RegistryRequest::Scale` + reply variant

No backwards-compatibility constraint — operators ship CLI + daemon together. Just append the variants.

**Variant addition (`registry_service.rs:49-77`).**

```rust
pub enum RegistryRequest {
    List,
    Spawn { template_name: String, group_name: String, replica_count: u8 },
    Unregister { group_name: String },
    Scale { group_name: String, target_replica_count: u8 },  // NEW
}
```

**Response variant (`registry_service.rs:80-97`).**

```rust
pub enum RegistryResponse {
    Groups(Vec<RegistryGroupSummary>),
    Spawned(RegistryGroupSummary),
    Unregistered { existed: bool },
    Scaled(RegistryGroupSummary),  // NEW — return snapshot post-resize
    Error(RegistryRpcError),
}
```

**Error variants (`registry_service.rs:125-144`).** Add:
- `RegistryRpcError::ScaleRejected(String)` — generic shape failure (replica count out of range, factory failed during add).
- `RegistryRpcError::UnknownGroup(String)` — scale-against-missing. Existing variants don't cover this; `Unregister` returns `Unregistered { existed: false }` instead of an error, but Scale is a write op against a presumed-extant group, so an error is more appropriate.

**Combined scope.** Per A-5, also add `source_subnet: SubnetId` + `fold_kinds: Vec<u16>` to `RegistryGroupSummary` (`registry_service.rs:100-108`). One wire change, two payoffs.

**Files touched (B-2).**
- `crates/net/src/adapter/net/behavior/aggregator/registry_service.rs` — three variant additions + the `RegistryGroupSummary` field additions.
- `crates/net/src/adapter/net/behavior/aggregator/registry.rs` — `AggregatorGroupEntry` already carries enough state to fill the new summary fields, but they aren't currently piped through. Find where the entry is constructed (`registry.rs:205-224, 238-278`) and ensure `source_subnet` + `fold_kinds` land on the entry at register time. The aggregator daemon's `AggregatorSpec` (`aggregator-daemon/src/lib.rs:390-476`) has both; piping requires either adding fields to `AggregatorGroupEntry` or sourcing them from the first replica's `AggregatorDaemon::config().source_subnet` / `fold_kinds` accessor (which already exists per `commands/aggregator.rs:179-184`).

**Test plan (B-2).**
- Extend `registry_service.rs::tests::registry_request_response_round_trip_through_postcard` (`:687-732`) to cover every new variant.

### B-3 — Server dispatch + `AggregatorRegistry::scale_group`

**`AggregatorRegistry::scale_group` action.**

```rust
pub async fn scale_group(
    &self,
    name: &str,
    target_replica_count: u8,
    factory: impl FnMut(u8) -> Arc<AggregatorDaemon> + Send + 'static,
) -> Result<Arc<AggregatorGroupEntry>, AggregatorRegistryError>;
```

Behaviour: lock the entry's `group: Arc<AsyncMutex<Option<LifecycleGroup<…>>>>` (current shape, `registry.rs:216-224`); compute delta = target - current.replica_count; call `add_replica(factory)` delta times for positive delta, `remove_last()` |delta| times for negative delta. The `factory` is invoked once per **added** replica (none on shrink); the daemon's `make_spawner` (`aggregator-daemon/src/lib.rs:612-649`) provides this from the same `AggregatorSpec` cached for the group.

**`RegistryHandler` dispatch** (`registry_service.rs:218-247`). Add `Scale` arm to `answer` (`:251-298`):

```rust
RegistryRequest::Scale { group_name, target_replica_count } => {
    let Some(spawner) = spawner else {
        return RegistryResponse::Error(RegistryRpcError::ScaleRejected("daemon read-only".into()));
    };
    // Look up the group by name; surface UnknownGroup if absent.
    // Invoke a registry-level `scale_group` action; require a stored
    // factory per group (see below).
    ...
}
```

**The cached factory problem.** `register` and `register_with_monitor` (`registry.rs:205-278`) don't store the factory closure. `register_with_monitor` does store one via the `HealthMonitor`'s `factory` parameter — that's the same Arc<dyn LifecycleDaemon> -building closure we want for scale-up. Two paths:

1. **Cache the factory on `AggregatorGroupEntry`.** New field `factory: Mutex<Option<BoxedReplicaFactory>>`. Set in `register_with_monitor` (already passed); set in `register` via a new optional parameter (or extend `register` to require it; existing callers are limited — `aggregator-daemon` uses `register_with_monitor`).
2. **Require operators to re-pass the template at `Scale` RPC time.** Wire `Scale { group_name, target_replica_count, template_name }`. The daemon re-derives the spec just like `Spawn` does. Less state in the registry; slightly more wire bytes. **No correctness issue** because the operator already knows the template — they spawned it.

Path 2 is cleaner. Wire shape becomes:

```rust
Scale {
    group_name: String,
    template_name: String,  // re-supplied per call
    target_replica_count: u8,
}
```

The server still must check that the group exists (UnknownGroup error) and that the **current** spec uses the named template (mismatch → `ScaleRejected("template mismatch")`). The spec-comparison check costs O(1) — `AggregatorGroupEntry.source_subnet` + `fold_kinds` (added in B-2) vs the resolved template's fields. This also gives the operator a sanity-check signal: a typo on `--template` doesn't silently grow a group with the wrong fold kinds.

**Files touched (B-3).**
- `crates/net/src/adapter/net/behavior/aggregator/registry.rs` — add `scale_group` method on `AggregatorRegistry`.
- `crates/net/src/adapter/net/behavior/aggregator/registry_service.rs::answer` — handle `Scale` variant.
- `crates/net/aggregator-daemon/src/lib.rs` — extend `make_spawner` (which is currently `Box::new(move |req| ...)`) to also export a `ScaleFn` closure with the same template-resolution shape; alternatively, fold both into one boxed enum dispatcher. Simpler: add `make_scaler` alongside `make_spawner`, install both via a new `install_registry_service_with_handlers` constructor.

**Test plan (B-3).**
- `registry_service.rs::tests::scale_grows_existing_group_via_template` — pre-spawn a group of 2; `Scale { group_name, template_name, target_replica_count: 4 }`; assert reply is `Scaled(summary)` with 4 replicas; assert the original 2 replicas' `starts` counter is still 1 (no restart).
- `registry_service.rs::tests::scale_shrinks_existing_group_to_target` — pre-spawn a group of 4; `Scale → 2`; assert 2 replicas remain; assert the dropped replicas' `stops` counter == 1.
- `registry_service.rs::tests::scale_rejects_unknown_group` — `Scale` against a name not in the registry; assert `RegistryRpcError::UnknownGroup(name)`.
- `registry_service.rs::tests::scale_rejects_template_mismatch` — pre-spawn group with template A; `Scale` with `template_name: "B"`; assert `RegistryRpcError::ScaleRejected("template mismatch")`.
- `registry_service.rs::tests::scale_to_same_count_is_noop_and_returns_current_snapshot` — `Scale` from 2 → 2; assert reply has 2 replicas; assert no `add_replica` / `remove_last` calls (verified via a per-replica `starts` / `stops` count check).
- `registry_service.rs::tests::scale_to_zero_is_rejected` — assert `RegistryRpcError::ScaleRejected("target_replica_count must be > 0")`.

### B-4 — `RegistryClient::scale` + `BoundRegistryClient::scale`

Mirror the `spawn` shape (`registry_client.rs:136-175`):

```rust
pub async fn scale(
    &self,
    target_node_id: u64,
    group_name: impl Into<String>,
    template_name: impl Into<String>,
    target_replica_count: u8,
) -> Result<RegistryGroupSummary, RegistryClientError>;

pub async fn scale_with_service(
    &self,
    target_node_id: u64,
    service: &str,
    group_name: impl Into<String>,
    template_name: impl Into<String>,
    target_replica_count: u8,
) -> Result<RegistryGroupSummary, RegistryClientError>;
```

`BoundRegistryClient::scale` (sdk/src/aggregator.rs:199-258) gets the bound-target convenience wrapper.

**Files touched (B-4).**
- `crates/net/src/adapter/net/behavior/aggregator/registry_client.rs` — new methods.
- `crates/net/sdk/src/aggregator.rs::BoundRegistryClient` — new method.

**Test plan (B-4).**
- `registry_client.rs::tests` only currently asserts deadline plumbing (`:228-258`); add a unit test that round-trips a `Scale` request via `postcard::to_allocvec` + `postcard::from_bytes` — same shape as the existing round-trip test.
- Full wire round-trip lives in B-6.

### B-5 — `net aggregator scale` flips to dedicated op

Replace A-4's interim Unregister + Spawn with a single `RegistryClient::scale` call:

```rust
let summary = client.scale(target, args.name, args.template, args.replica_count).await?;
```

The CLI's external shape is unchanged from A-4 (`--name`, `--template`, `--replica-count`). Only the wire op changes. Operators get:
- No identity churn for held replicas.
- Atomic resize (no observable "registry empty" window between Unregister and Spawn).
- Earlier failure detection (Scale rejects pre-resize on template mismatch; Unregister+Spawn could leave the registry empty between calls if Spawn fails).

**Files touched (B-5).**
- `crates/net/cli/src/commands/aggregator.rs::run_scale` — collapse to one call.

**Test plan (B-5).**
- Extend `cli/tests/aggregator_remote.rs::scale_grows_then_shrinks` with assertions on `EntrySnapshot.replicas[0].generation()` — confirm the original replica's generation persists across the resize (proving no identity churn). The substrate already exposes generation via `RegistryReplicaSummary.generation` (`registry_service.rs:113-114`).

### B-6 — Round-trip Scale integration test

Extend `tests/aggregator_registry_rpc.rs` (`:113-160` is the current pattern). Add:

- `scale_grows_existing_group_via_wire` — host + querier mesh nodes; daemon-flavoured spawn (with template) via the `Spawn` wire op; then `Scale` via the wire op; assert resulting `Vec<RegistryGroupSummary>` has the target count + the original replicas' generations preserved.
- `scale_shrinks_existing_group_via_wire` — same setup; scale from 4 → 2; assert post-scale list returns 2 replicas in indices 0 and 1.
- `scale_unknown_group_returns_server_error_via_wire` — assert `RegistryClientError::Server(RegistryRpcError::UnknownGroup(_))`.

Mirrors the existing `list_round_trips_two_registered_groups_across_handshake` shape (`:113-141`).

**Files touched (B-6).**
- `crates/net/tests/aggregator_registry_rpc.rs` — three new `#[tokio::test]` functions.

---

## Closeout (C)

### C-1 — Docs alignment

- `docs/plans/AGGREGATOR_LIFECYCLE_DEFERRED_2026_05_23.md:159-164` — append a "Closed in `AGGREGATOR_CLI_REMOTE_ATTACH_AND_SCALE_RPC.md`" note pointing at this plan's resulting commit range.
- `docs/plans/NET_CLI_PLAN.md:540-544` — Phase 5 stays open (broader Deck-RPC remote-attach is still parked); add a sub-bullet "Aggregator-shaped slice closed via `AGGREGATOR_CLI_REMOTE_ATTACH_AND_SCALE_RPC.md`."
- `docs/plans/SCALING_SUBNET_SPEC.md:401-409` — the spec's CLI sketch listed `net aggregator spawn --source-subnet --replicas …`. Updated shape (post-A-3) uses `--template` instead. Note the shape change.

### C-2 — Tidy

- Remove "[preview]" markers + parse-only error copy from `cli/src/commands/aggregator.rs:53-64, 222-235, 286-294, 305-312` once the corresponding verbs flip live.
- Remove `hex_decode_32` duplicate after A-1 lifts it into `cli/src/parsers.rs`.

---

## Phasing + ordering

**Recommended order:**

1. **A-1** (CLI bootstrap) — unblocks the rest of A and surfaces `mesh_node()` for the SDK aggregator helpers if they ever need it.
2. **A-2** (`query`) — smallest payoff slice; lets operators verify remote-attach against an existing daemon before riskier write paths.
3. **A-3** (`spawn`) — exercises a write path; introduces the `--template` decision that A-4 + B-5 inherit.
4. **A-4** (`scale` interim) — ships the user-visible verb with an Unregister+Spawn implementation. Operators stop seeing "parse-only" errors.
5. **A-5** (`ls --remote`) — placeholder for source_subnet/fold_kinds; flips to live data after B-2.
6. **A-6** (CLI integration test) — pins all four verbs against a real daemon subprocess.
7. **B-1** (lifecycle helpers) — substrate-only; can land in parallel with A-* but doesn't unblock anything user-visible until B-3.
8. **B-2** (wire bump) — combine Scale variant + RegistryGroupSummary fields. Ships the wire-version step.
9. **B-3** (server) — full Scale dispatch.
10. **B-4** (client) — `RegistryClient::scale` + bound wrapper.
11. **B-5** (CLI flip) — `net aggregator scale` now uses Scale instead of Unregister+Spawn. CLI shape unchanged from A-4.
12. **B-6** (round-trip test).
13. **C-1 + C-2** — docs + tidy.

**Parallelism.** Gap 1 and Gap 2 can ship side-by-side (B-* work is substrate + wire; A-* is CLI). The only sequencing constraint is **A-4 must land before B-5** (B-5 collapses A-4's logic); **A-5 lands at placeholder fidelity before B-2 ships full fields**.

**Estimated slice count.** 12 mergeable slices + 2 doc/tidy. Each slice is 1–3 days of work; the substrate + wire slices (B-1, B-2, B-3) are the slowest (~2 days each including tests). Total: ~3 weeks of focused work.

---

## Substrate gap — discovered + closed (task #102)

A-6 integration testing surfaced that the substrate's dispatch loop drops direct handshake msg1 packets from peers it hasn't pre-`accept()`ed (`mesh.rs:2409-2417` + `mesh.rs:3247-3250`). Only the initiator-side has a post-start registry; the responder side is "explicitly deferred." Every CLI subprocess invocation generates a fresh ephemeral identity, so the daemon can't pre-`accept` it.

**Closed by routing CLI handshakes through `connect_via` (routed) instead of `connect` (direct).** `handle_routed_handshake` Case 2 already accepts msg1 from fresh initiators against a running dispatch loop. The routed packet carries `src_id` (the initiator's routing-id) in the routing header (cleartext) so the responder can compute the prologue without pre-`accept`; the initiator's full u64 node_id lives in the AEAD-authenticated Noise payload.

Substrate change: `connect_via` now populates `addr_to_node[relay_addr]` via `entry().or_insert(...)` (mesh.rs:9442-9455). Without this, address-keyed paths like `send_subprotocol` (used by SUBSCRIBE / membership / RPC reply-channel setup) couldn't resolve the destination's node_id when relay == final dest (the CLI single-hop case). `or_insert` preserves the true multi-hop semantics — when `relay_addr` already maps to the relay's own node_id, the existing mapping is kept.

CLI change: `Mesh::connect_via` exposed on the SDK; `CliContext::build_with_remote` switched from `mesh.connect(...)` to `mesh.start(); mesh.connect_via(...)` (the routed path needs the dispatch loop running before sending msg1).

All four A-6 positive subprocess tests now pass. The `query` test stays `#[ignore]`'d for a different reason — fold.query handler is keyed on each replica's id, and replica id discovery from `BootedDaemon` isn't exposed today.

## Risks for the user to weigh in on

1. **`--source-subnet` removal from `net aggregator spawn`.** A-3 makes `--template` required and removes `--source-subnet`. The flag exists today as parse-only (no live use), so no consumer scripts break — but the help-text contract under NET_CLI_PLAN.md's "subcommand layout is a contract" lock (Locked decision 1, NET_CLI_PLAN.md:31-34) treats parse-only verbs the same as live ones. If the user wants to preserve `--source-subnet` for some future template-free path, flag this before A-3 lands.
2. **PSK transport.** A-1 requires the operator to supply the daemon's PSK to the CLI (via `--psk-hex` flag or config `psk_hex`). This is the same trust model the round-trip tests use, but it puts a 32-byte symmetric key into operator-facing CLI config. If the user wants a different trust model (e.g. operator-identity-based auth instead of mesh-PSK), the bootstrap shape needs revisiting — likely a Deck-RPC frame (NET_CLI_PLAN.md Phase 5 proper) rather than a raw MeshNode attach.
3. **Placement under scale-up.** B-1's `add_replica` doesn't engage the scheduler. Adding the 3rd replica to a group originally spawned via `spawn_with_placement` lands locally rather than on a third failure domain. Acceptable in the single-process / single-host daemon deployment we ship today; not acceptable for the multi-host deployment SCALING_SUBNET_SPEC.md sketches. If multi-host deployment is on the near roadmap, B-1 needs a `add_replica_with_placement` sibling — flag this if relevant.
