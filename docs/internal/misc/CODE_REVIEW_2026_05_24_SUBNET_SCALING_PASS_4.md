# Code review — `subnet-scaling` branch, pass 4 (2026-05-24)

Branch base: `master`.
Scope: the 22 commits ahead of master AFTER passes 1–3 (substrate
aggregator + lifecycle/registry + SDK/FFI/bindings work all merged via
prior PRs). This pass covers:

1. **CLI remote-attach** — `net aggregator query|spawn|scale|ls --remote`
   against live daemons, plus `cli/tests/aggregator_remote.rs`
   integration test.
2. **In-place Scale RPC** — `LifecycleGroup::add_replica` /
   `remove_last` primitives, `AggregatorRegistry::scale_group`,
   `RegistryRequest::Scale` + `ScaleRejected` / `ScaleNotSupported`
   errors, RegistryClient + FFI / Node / Python wiring.
3. **`MeshNode::connect_via`** — client-side routed-attach path so the
   CLI handshake completes against an already-started daemon.
4. **Massive deck UI iteration** — tab strip horizontal scroll + letter
   shortcuts, cursor + scroll on SUBNETS/GATEWAYS/AGGREGATORS, new
   SUBNET focus page, GATEWAYS gains CHANNEL/VIS/REACH columns,
   SUBNETS gains PARENT/HEALTH/AGG columns, demo fixtures for
   SUBNETS/GATEWAYS/AGGREGATORS.
5. **`source_subnet` + `fold_kinds`** lifted onto `RegistryGroupSummary`
   so remote `ls`/`spawn`/`scale` views show them.
6. **aggregator-daemon** — boot+query integration test, template
   index improvements.

~4,100 LOC added / 430 removed across 37 files.

Three review agents (reuse / quality / efficiency) were dispatched in
parallel. Findings below are organised by severity. File paths are
relative to repo root; line numbers reflect the branch tip and may
drift.

---

## CRITICAL — build is broken

### P1 — `cargo check --tests --lib` fails with 11 errors

Two breakage classes from recent commits:

- **Commit `2a237a79`** added a `scaler: Option<&ScaleFn>` parameter to
  `registry_service::answer(...)` at position 3. Eight in-file test
  call sites still use the old 3-arg form:
  `registry_service.rs:625, 657, 670, 688, 705, 776, 812, 845`.
- **Commit `285a39c9`** added `source_subnet: SubnetId` + `fold_kinds:
  Vec<u16>` to `RegistryGroupSummary`. Two literal sites still build
  the struct without them: `registry_service.rs:890` and
  `ffi/aggregator.rs:806`.

Verified locally with `cargo check --tests --lib`:
```
error[E0063]: missing fields `fold_kinds` and `source_subnet` in
              initializer of `registry_service::RegistryGroupSummary`
   --> src\ffi\aggregator.rs:806:17
error: could not compile `net-mesh` (lib test) due to 11 previous errors
```

Fix: thread `None`/`Some(&scaler)` into every `answer(...)` call (tests
that don't exercise Scale can pass `None`) and add `source_subnet:
SubnetId::GLOBAL, fold_kinds: Vec::new()` to both struct literals.
This is a merge blocker if CI runs `cargo test --lib`.

---

## HIGH — correctness / concurrency

### P2 — `LifecycleGroup` scale holds the entry's async mutex across N sequential `add_replica` awaits

`net/crates/net/src/adapter/net/behavior/aggregator/registry.rs:356-385`.

`AggregatorRegistry::scale_group` acquires the entry's
`AsyncMutex<Option<LifecycleGroup>>` once, then loops
`for _ in 0..(target - current) { group.add_replica(...).await }`.
Each `add_replica` starts a `LifecycleHandle` and runs `on_start`,
which for `AggregatorDaemon` spawns a tick task and registers
RPC services. For a 1→32 grow that's 31 sequential `on_start`s with
the mutex held end-to-end.

While the mutex is held:
- `List` RPC reads via `snapshot()` block (covered by registry_service
  `snapshot_group` path).
- `replica_count()` / `health()` / `entries()` block.
- The `HealthMonitor` tick (covered by `register_with_monitor`) using
  the same `group_arc` stalls.

Fix: parallelise grow with `try_join_all` over a Vec of `add_replica`
futures — the underlying `LifecycleHandle::start` futures are
independent per replica (same pattern that `start_replicas` already
uses at `lifecycle/group.rs:528`). Shrink stays sequential — `stop()`
genuinely is.

### P3 — `MeshNode::connect_via` has no handshake retries

`net/crates/net/src/adapter/net/mesh.rs:9356-9428`.

Sends msg1 once at line 9408, awaits with `handshake_timeout` (5 s by
SDK default at `sdk/src/mesh.rs:276`). The non-routed
`handshake_initiator` at `:9494` retries `handshake_retries` times
(default 3). `connect_via` is the asymmetric outlier.

Every CLI `--remote` verb (`ls`, `query`, `spawn`, `scale`) routes
through `cli/src/context.rs::build_remote_mesh` → `connect_via`. A
single UDP packet loss on msg1 or msg2 → 5 s wait → typed error →
CLI exits without retry.

Fix: wrap the send + wait in the same `attempt < handshake_retries`
loop `handshake_initiator` uses, or expose `connect_via_with_retries`
honoring `MeshNodeConfig::handshake_retries`. Both paths should respect
the same config knob.

---

## MEDIUM — quality / hygiene

### P4 — CLI remote-attach `mesh_node` extraction copy-pasted across 4 verbs

`net/crates/net/cli/src/commands/aggregator.rs:286-289, 390-393,
438-441, 482-485`.

The same 4-line block appears in `run_query`, `run_ls_remote`,
`run_spawn`, `run_scale`:
```rust
let mesh = ctx.mesh_node().ok_or_else(|| {
    sdk("internal: remote-attach context returned no mesh_node — should be unreachable")
})?;
```

The error message is character-identical in all four sites. Since
`build_with_remote` always populates `mesh_node`, this is defensive
boilerplate.

Fix: have `build_with_remote` return `(CliContext, Arc<MeshNode>)`, or
expose `ctx.require_mesh_node()`. Drops ~40 LOC + pins the
"unreachable" diagnostic to one spot.

### P5 — `CliContext` carries both `mesh_node: Option<Arc<MeshNode>>` AND `_mesh: Option<Mesh>`

`net/crates/net/cli/src/context.rs:53-62`.

`Mesh::node_arc()` already returns the same `Arc`. The `mesh_node`
field is redundant — `_mesh.as_ref().map(|m| m.node_arc())` covers
the accessor. The duplicated state masks the real lifetime constraint
(Mesh owns the socket and the receive-loop task).

Fix: drop the `mesh_node` field; have the accessor derive it from
`_mesh`. Rename `_mesh` to `mesh` once it's the canonical owner.

### P6 — Scale RPC no-op (`target == current`) still pays full snapshot+validation cost

`net/crates/net/aggregator-daemon/src/lib.rs:699-718` (`make_scaler`).

`make_scaler` calls `existing_entry.snapshot().await` BEFORE the
delta check, which drops the entry lock and runs `join_all` over
per-replica `health()` futures. Then `scale_group` checks `target
== current` and short-circuits — but the snapshot was already
allocated.

Fix: check `if target_replica_count == entry.replica_count().await
{ return existing_snapshot }` before the snapshot+validation block in
`make_scaler`. Low operator-driven impact, but free win.

### P7 — `MeshNode::connect_via` re-implements `connect`'s post-handshake peer registration

`net/crates/net/src/adapter/net/mesh.rs:9444-9467` vs `:2351-2387`.

Both paths build the `NetSession`, call `router.add_route`, insert
into `peers`, populate `peer_addrs`. `connect_via` deliberately omits
`proximity_graph.on_pingwave` + `failure_detector.heartbeat` +
`push_local_announcement`, and uses `addr_to_node.entry().or_insert(...)`
rather than `.insert(...)` (per the `:9434-9443` comment about
multi-hop). Diverging silently risks "why doesn't routed-attach get a
failure-detector entry" mysteries later.

Fix: extract `fn install_peer(node_id, addr, keys, addr_mode: enum) ->
()` for the shared steps; leave `connect`'s extra proximity /
failure-detector / announcement wiring at the call site. Or document
the omissions inline at `connect_via` with the rationale.

### P8 — `aggregator-daemon::make_spawner` / `make_scaler` template-index duplicated

`net/crates/net/aggregator-daemon/src/lib.rs:612-649` (`make_spawner`)
vs `:660-744` (`make_scaler`).

Both build `HashMap<String, TemplateConfig>::from_iter` + `Arc::new` +
`templates.into_iter().map(|t| (t.name.clone(), t)).collect()`, then
both run `by_name.get(&req.template_name).cloned().ok_or_else(...
UnknownTemplate ...)`.

Fix: `fn build_template_index(Vec<TemplateConfig>) -> Arc<HashMap<...>>`
and a `fn resolve_template(&Arc<...>, &str) -> Result<TemplateConfig,
RegistryRpcError>` helper used by both closures.

### P9 — `connect_via` naming + docstring is leaky

`net/crates/net/sdk/src/mesh.rs:441-452` and
`net/crates/net/src/adapter/net/mesh.rs:9349-9356`.

Name implies "connect via an intermediate" but the CLI uses it with
`relay_addr == final_dest`. Docstring describes implementation
(`pending_handshakes`, "Case 1 of `handle_routed_handshake`") rather
than contract. The `relay_addr == final_dest` "degenerate one-hop"
case is the actual load-bearing one for CLI remote-attach.

Fix: rename to `connect_routed` or `connect_after_start`. Lead the doc
with the operator-visible contract: "Use when the responder is already
`start()`ed and hasn't pre-`accept()`'d this initiator's node_id."

### P10 — `RegistryRpcError` mixes "not supported" and "rejected" shapes inconsistently

`net/crates/net/src/adapter/net/behavior/aggregator/registry_service.rs:160-191`.

`SpawnNotSupported` / `ScaleNotSupported` carry no payload while
`SpawnRejected(String)` / `ScaleRejected(String)` carry a diagnostic.
Unregister returns `Unregistered { existed: false }` instead of a
typed `UnknownGroup` — divergent shape for the same "no group by
that name" condition that `Spawn` reports via `UnknownTemplate`.

Fix: either align Unregister to also use a typed `UnknownGroup`
variant, or document the asymmetry in the enum doc-comment. The
comment at `:414-416` hints at the rationale but the wire shape
divergence is real.

---

## LOW — efficiency / cosmetic

### P11 — Deck SUBNETS render path re-derives rollups per frame at 8 fps

`net/crates/net/deck/src/app.rs:3374-3375, 1352-1364, 1369-1388`.

Each frame on the SUBNETS tab: `aggregator_source_subnets()` builds a
fresh `HashSet`, `subnets_with_members(None)` walks peers. The new
SUBNET focus page also calls `subnet_rollups()` indirectly via
`open_subnet_focus` — only on Enter, so not hot.

Fix: cache `(snapshot_rev, rollups)` on `App`; invalidate when
`refresh_snapshot` swaps the Arc.

### P12 — SUBNET focus page allocates fresh `Vec<(u64, &PeerSnapshot)>` per frame + per key event

`net/crates/net/deck/src/tabs/subnet_page.rs:161-189, 230-238,
249-254`.

`render_members`, `cursored_member_id`, and `visible_member_count`
each walk the same member list independently and allocate a fresh
Vec. At 8 fps with 100s of members in production, this is the
biggest per-frame allocator pressure in the new code.

Fix: compute `visible_members: Vec<u64>` once at the top of
`render_members`, pass it down; optionally cache on
`SubnetFocusEntry` keyed by snapshot Arc identity.

### P13 — Demo fixtures rebuild per render

`net/crates/net/deck/src/demo/fixtures.rs`, called from
`deck/src/tabs/aggregators.rs:33`, `gateways.rs:60`, plus
`app.rs:1361` inside `aggregator_source_subnets`.

Each call rebuilds `Vec<SummaryAnnouncement>` / `GatewayStats` from
deterministic literals. At 8 fps on the AGGREGATORS / GATEWAYS /
SUBNETS tabs, the fixtures are reallocated every frame.

Fix: wrap each fixture in `std::sync::OnceLock` so the first render
computes and subsequent renders read the cached `&'static`.

### P14 — Bootstrap-triple fields are `Option<String>` at the clap layer

`net/crates/net/cli/src/commands/aggregator.rs:51-69`.

All four (`node_addr`, `node_pubkey`, `remote_node_id`, `psk_hex`)
are typed as raw strings; validation happens later in
`resolve_remote_attach`. Clap supports custom parsers via
`#[arg(value_parser = ...)]`.

Fix: typed parsers for `SocketAddr` / `[u8;32]` hex / `u64` hex-or-
decimal. Catches typos at parse time and removes the duplicate
"invalid args" wrapping in `resolve_remote_attach`.

### P15 — Tab letter-shortcut mapping hard-coded in two places

`net/crates/net/deck/src/widgets/tab_bar.rs:37-46` and the app's
key-handler each carry the same `Subnets→'H', Gateways→'V',
Aggregators→'B', Audit→'U'` table.

Fix: add `Tab::letter_shortcut(self) -> Option<char>` so the mapping
lives next to `Tab::label`.

### P16 — Heavy WHAT-narration on new deck cursor fields

`net/crates/net/deck/src/app.rs:14-22, 207-216, 304-336, 393-394`.

Multi-line comments narrating "Cursor on the GATEWAYS tab — index
into the resolved export-rule rows. Persists across tab switches so
the operator's selection survives a quick pivot away" repeat for each
cursor field. The first establishes the pattern; subsequent fields
can be one-liners.

### P17 — `widgets/tab_bar.rs::scroll_window_horizontal` parallels `tabs::scroll_window`

`deck/src/widgets/tab_bar.rs:121-184` (variable-width, horizontal,
2-side chips) vs `deck/src/tabs/mod.rs:28-63` (fixed-row, vertical,
1-cell chips).

Same 2-pass-reservation shape; the comment at `tab_bar.rs:92-96`
explicitly says "Same shape as `tabs::scroll_window`." Signatures
differ enough (`widths: &[usize]` vs uniform rows; `LEFT_CHIP=4 /
RIGHT_CHIP=3` vs `1/1`) that a generic abstraction would be lossy.

Recommendation: leave as-is, add a `// see also` doc-link so future
changes to one keep the other in mind.

### P18 — Test coverage gap: 4 of 6 remote-attach tests `#[ignore]`

`net/crates/net/cli/tests/aggregator_remote.rs:27-29`.

Positive-path tests (`ls --remote`, `spawn`, `scale`, `query`) all
`#[ignore]`d on "substrate direct-handshake responder gap; see task
#102." Only the 2 negative-path tests (bad pubkey / missing flag) run
in CI. Track #102; without it the e2e suite tests only the error
paths of the 4 documented verbs.

---

## False positives noted during the pass

- **`LifecycleGroup::scale_to`**: doesn't exist. Reviewer's brief
  asked about it; the actual scale loop lives in
  `AggregatorRegistry::scale_group` + `aggregator-daemon::make_scaler`.
  `group.rs` only ships `add_replica` / `remove_last` primitives.
- **`subnet_page.rs` duplicates `tabs/nodes.rs` rendering**: false.
  It properly delegates to `nodes::render_nodes_view` and
  `subnets::health_rollup`.
- **`aggregator-daemon` static `[[group]]` boot vs Spawn RPC
  duplication**: false. Already shares `spawn_and_register` (from
  pass 2's S8 fix).
- **CLI `context.rs` hex-decode reinvention**: false. Cleanly reuses
  `parse_u64_flexible` and `hex_decode_32` from
  `cli/src/parsers.rs`.
- **Deck cursor `saturating_sub(1)` arm proliferation**: pre-existing
  pattern; new code adopts it rather than introducing fresh
  duplication.

---

## Clean areas

- **`LifecycleGroup::add_replica` / `remove_last`** — genuine new
  primitives, not overlapping with `replace`.
- **`Sdk Mesh::connect_via`** (sdk/src/mesh.rs:441-452) — thin
  parallel to `connect` (parse + delegate); correct mirror.
- **`demo/fixtures.rs`** — no overlap with
  `aggregator-daemon/tests/boot_and_query.rs` (different domains).
- **`RegistryRequest::Scale` arm shape** — mirrors `Spawn`'s
  spawner-Optional + duplicate-check pattern with enough unique
  error semantics that a shared helper would obscure intent.
- **Locks (`parking_lot::RwLock` / `DashMap`)** added in this slice
  are read briefly; no held-across-await issues introduced.

---

## Suggested fix order

1. **P1** — un-break the test build. Trivial, ~10 LOC across 3 files.
   Merge blocker.
2. **P2 + P3** — real concurrency/UX gaps. P2 is a single
   `try_join_all` substitution in `scale_group`; P3 is a retry loop
   mirroring `handshake_initiator`.
3. **P4 + P5** — small CLI cleanup once P1 is in.
4. **P6 + P8** — daemon-side polish.
5. **P7 + P9** — `connect_via` consolidation + rename.
6. **P10** — error-shape decision (align vs document).
7. **P11–P18** — deck UI polish, cosmetic, deferrable.
