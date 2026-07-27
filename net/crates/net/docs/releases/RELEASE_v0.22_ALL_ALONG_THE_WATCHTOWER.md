# Net v0.22 — "All Along the Watchtower"

*Named after Bob Dylan's 1967 cut from John Wesley Harding — the one Jimi Hendrix took six months later and turned into his own song so completely that Dylan started covering Hendrix's arrangement instead of his own ("he found things in the song that I didn't realize were there"). Twelve lines, three verses, no chorus: the joker tells the thief there must be some way out of here, the thief replies that the hour is getting late, and the camera pulls back to a watchtower where princes watch the women come and go and two riders are approaching from outside the frame. The whole song is built around the vantage point — somebody up on the parapet, looking down at the territory below, naming what they see. v0.22 puts that vantage point in the substrate. An aggregator daemon sits one tier up from a source subnet, subscribes to that subnet's detail channels through the existing gateway, summarizes what it sees, and publishes the summary to channels visible at the parent or peer tier. The mesh already had four-level hierarchical `SubnetId` on every packet, a gateway that enforced visibility at subnet boundaries by reading header fields only, label-based subnet assignment, and replica groups for any daemon role. v0.21 was about shrinking the gap between call and arrival on the hot path; v0.22 is about giving every tier a watchtower without inventing a parallel scoping mechanism. One generic state-aggregation framework replaces three would-be-separate fold implementations. One aggregator daemon role, deployed via the existing replica-group infrastructure, bridges tiers — N watchers per subnet, all publishing independently, subscribers picking the freshest by generation. One RPC surface lets operators spawn, scale, and query those watchers from any node, any language, any process. And one Deck retab puts the whole topology on the cyberdeck so the operator has the same vantage point the substrate just built.*

## One framework, three folds, watchers in front of all of it

The v0.22 release is the result of four planning passes that converged on the same insight: the substrate already had the primitives for million-node scale, but three layers above it were duplicating work to consume them. The capability index was a bespoke per-class store with its own subscription model and its own eviction; the routing table was a pingwave-driven sorted-by-metric thing with its own staleness sweeper; reservations were going to be a third bespoke layer doing the same shape of work for a third domain. The fold framework lands one generic runtime parameterized by a typed `FoldKind` trait — apply, expire, query, snapshot, recovery, audit emission, metrics — and three concrete instantiations on top of it. The legacy `CapabilityIndex` and the pingwave-driven routing table delete in the same diff that lands their fold replacements. No bridges, no dual-publish, no transition window.

Above the folds, the aggregator role lands as a normal daemon — but a daemon with a lifecycle. The substrate gets a new `LifecycleDaemon` async sibling trait to `MeshDaemon` (the existing sync/WASM-friendly trait), a `LifecycleHandle` RAII wrapper that owns the daemon's tokio loop, and a generic `LifecycleGroup<L>` that manages N replicas of any `LifecycleDaemon` as a unit. Aggregators are the first application; future tier services (market matchers, settlement bridges, reputation oracles) reuse the primitive. Placement spreads across failure domains via the scheduler; per-replica health drives auto-replacement via a background `HealthMonitor` with exponential backoff; a `register_with_monitor` constructor wires registry + monitor together so the operator never has to thread them by hand.

On top of the lifecycle layer, the `aggregator.registry` RPC service lets any node enumerate, spawn, scale, and unregister aggregator groups on any other node. The new turnkey `net-aggregator-daemon` binary boots from a TOML config, registers templates the operator can instantiate by name, defaults to auto-respawn-on-failure, and prints a single JSON bootstrap line on stdout for tools that need its bound address and pubkey. Five language bindings (Rust, TypeScript, Python, Go, C) get the same `RegistryClient` + `FoldQueryClient` surface with the same typed error kinds and the same wire contract locked in a single table. The CLI grows remote-attach: `net aggregator query / spawn / scale / ls --remote` against `--node-addr <ip:port> --node-pubkey <hex> --node-id <n>` round-trips through a live daemon. The Deck grows three new tabs and a focus page so operators can see subnet hierarchy, gateway state, and aggregator health without leaving the cyberdeck.

Below: the wins, grouped by where they fire.

---

## Multi-fold framework: one runtime, three typed instantiations

The `Fold<K>` runtime is the new spine of the substrate's typed-state layer. One implementation handles every fold; concrete folds are trait impls.

**`FoldKind` trait + `Fold<K>` runtime.** `FoldKind` is parameterized by `Key`, `Payload`, `Query`, `Result`, and `Index` associated types; the fold author supplies `key_for`, `merge`, `build_index`, `query`, and optional `audit_event`. The runtime owns `FoldState<K>` (primary `HashMap<K::Key, FoldEntry<K>>` + a reverse `HashMap<NodeId, HashSet<K::Key>>` for O(1) node eviction), the expiry task, the audit sink, and the metrics handle. Snapshot/restore round-trips identical state — restored entries are naturally superseded by live announcements with higher generation numbers, so warm starts don't need any "I'm catching up" coordination.

**`SignedAnnouncement<P>` wire format.** One ed25519-signed envelope carries `kind` (the u16 `KIND_ID` that names the fold), `class` (the fold-specific sub-bucket — capability class hash, routing tier, reservation pool), `node_id`, `generation`, `announced_at` micros, `ttl_secs`, `flags`, and the typed `payload`. Subnet scope is **not** in the envelope — every packet already carries `NetHeader.subnet_id`, and the substrate's existing `ChannelConfig::visibility` (`SubnetLocal` / `ParentVisible` / `Exported` / `Global`) plus `SubnetGateway` handle scoping at the wire layer. Folds reuse this; they don't invent a parallel scoping model. The dispatch layer reads `kind` from the announcement header, looks up the registered fold instance, verifies the signature, and calls `Fold::apply`. Wire encoding is postcard; the format is versioned via the `KIND_ID` namespace.

**`CapabilityFold` — replaces the legacy `CapabilityIndex` outright.** Each (capability class, node) is one entry; subscribers learn which nodes are in which classes. The legacy `CapabilityIndex` and every caller (`MeshNode::capability_index`, `Scheduler::place_*`, `ReplicaGroup` / `ForkGroup` / `StandbyGroup` placement paths, the FFI surface, the Deck capability panel) was rewired in one diff. The inverted indices (`by_tag`, `by_region`, `by_state`) are part of the fold's `Index` type; tag-inverted lookup runs in the same shape it did pre-cutover, but now under the generic runtime with snapshot/restore and audit emission for free.

**`RoutingFold` — replaces the pingwave-driven `RoutingTable` outright.** Destination is the key; multiple announcements per destination from different routers compete via metric-based `merge`. The `Router::lookup` and `MeshNode::dispatch_packet` call sites rewired in the same diff. Pingwave packets become `SignedAnnouncement<RouteAnnouncement>` publishes on the `fold:route:` channel — same wire RTT measurement, new envelope. The route-staleness sweeper goes away; TTL expiry on the fold runtime handles it.

**`ReservationFold` — new typed fold for single-holder resources.** Each resource has at most one active reservation; `ReservationState::Free | Reserved { holder, until } | Active { holder, job_id }`; merge enforces a state machine (legal transitions accepted, illegal rejected with audit event). The same owner can transition through states; a different owner can only claim when the current state is `Free`. The fold's per-state summarizer derives stable bucket labels from a fixed-label match (not from `format!("{state:?}")`), so summary cardinality stays bounded regardless of how many distinct holders pass through.

**Subscription dispatch + `FoldRegistry`.** One `FoldRegistry` per node owns the `HashMap<u16, Arc<dyn FoldDispatch>>`. Channel-layer messages route by `kind` ID; signature verification happens at dispatch time using existing identity machinery. Replay protection is generation comparison; reorder protection is the same.

**Audit + snapshot + metrics integration.** Per-fold metrics: `fold_entries_total`, `fold_applies_total{outcome}`, `fold_expiries_total`, `fold_queries_total`, `fold_query_duration`, `fold_apply_duration`, `fold_subscription_lag`. Audit events: `FoldEntryCreated / Replaced / Expired / Evicted / Rejected` flow through the existing audit chain. Snapshots serialize at configurable cadence (default 5 min) and on graceful shutdown.

---

## Aggregator daemons: a lifecycle layer above `MeshDaemon`

The aggregator role is inherently async (`tokio::interval` + `mesh.publish().await`); the existing `MeshDaemon` is documented sync-only / WASM-compatible (`process(&CausalEvent) -> Vec<Bytes>`). v0.22 introduces an async sibling and builds the aggregator on top.

**`LifecycleDaemon` async sibling trait + `LifecycleHandle` RAII wrapper.** `LifecycleDaemon` is the trait async daemons implement (`async fn on_start`, `async fn on_stop`, etc.); `LifecycleHandle::start(daemon)` owns the tokio loop and stops it cleanly on drop. The shutdown-aware tick loop checks the shutdown flag between publishes, so a long-running `publish().await` doesn't get its task dropped mid-flight by the backstop timeout. The backstop itself bumped from "summary interval + 100 ms" to a value that absorbs realistic publish latencies under load.

**`LifecycleGroup<L>` — N-replica HA generic over the lifecycle daemon.** Hoisted out of an aggregator-specific group type so any future tier service uses the same primitive. Deterministic per-replica keypairs via `derive_replica_keypair(group_seed, index)`; `spawn_with_placement` consults the scheduler to spread replicas across failure domains within the source subnet; `requirements()` on the trait flows through to placement constraints. In-place grow/shrink via new `add_replica` (takes a factory closure, returns the new index) and `remove_last` (returns the stopped replica's Arc); deterministic-last-as-victim keeps the lowest-indexed replicas across resizes so identity continuity is preserved.

**`ReplicaHealth` + `LifecycleGroup::replace`.** Per-replica liveness is derived from `start_instant + generation` (no `last_tick_at` field needed — generation already advances on every successful summary). Unhealthy replicas can be swapped via `replace(index, new_daemon)`; group-level health is "≥1 healthy replica."

**`HealthMonitor` — background auto-respawn driver.** Periodic per-replica health checks; failed replicas get re-spawned via a cached factory with exponential backoff so a persistently-broken daemon doesn't spin in a respawn loop. Configurable; `register_with_monitor` is the one-call constructor that wires registry + monitor together as the operator-facing entry point.

**`AggregatorDaemon` as a `LifecycleDaemon`.** `AggregatorConfig { source_subnet, summary_visibility, summary_targets, fold_kinds, summary_interval, custom_summarizers }`. On start, subscribes to source-subnet detail channels for each configured fold kind; on tick, walks each fold's `Summarizer` to produce `SummaryAnnouncement`s and publishes them at the configured visibility. Validates at boot via a dry-run `AggregatorDaemon::new` so a misconfigured template is rejected on the operator's terminal, not on the first tick.

**All replicas publish independently.** No election machinery; subscribers see N summary announcements per cycle and the fold's `merge` picks the latest by generation. Operator can `scale_to(1)` to reduce summary traffic when availability isn't the constraint. State across re-placements rebuilds from incoming channel announcements + TTL refreshes within one TTL cycle (~30-60s); other replicas in the group publish full summaries during rebuild.

**Built-in summarizers per fold.** `CapabilityFoldSummarizer` (count by class + state, aggregate hardware capacity, distribution across sub-subnets) and `ReservationFoldSummarizer` (count by resource class + state, fixed-label state buckets). Routing is intentionally not summarized — routing wants full detail or none. Custom summarizers are Rust-only; bindings get the two built-ins via the daemon's template registry.

**`AggregatorRegistry` on `MeshNode`.** First-class registry surface — `net aggregator inspect` reads it; the RPC service publishes from it; `MeshOS` inspection surfaces include aggregator groups alongside `DaemonRegistry`'s mesh-daemon entries. Holds `LifecycleGroup` directly (rather than wrapping it), so registry entries carry enough state to fill `RegistryGroupSummary` fields without a second indirection.

---

## Turnkey `net-aggregator-daemon` binary

For operators who don't want to embed the substrate in their own process, the new `net-aggregator-daemon` crate ships a turnkey binary + library that boots from a single TOML file.

**Templates + groups.** `[[template]]` blocks declare named aggregator specs (`source_subnet`, `summary_visibility`, `fold_kinds`, `summary_interval`); `[[group]]` blocks instantiate templates at boot. Templates are validated up-front via a dry-run `AggregatorDaemon::new` — if a template's config is broken, the daemon fails on start with a copy that points at the bad field, not silently at the first tick.

**Spawn / Unregister / List / Scale via RPC.** Once running, the daemon serves the `aggregator.registry` RPC service. Operators ship a config with zero `[[group]]` blocks and spawn dynamically via the wire, or pre-declare groups in TOML and let the wire surface only handle scale + lifecycle. The `Spawn { template_name, group_name, replica_count }` op resolves the template, `derive_seed_from_name` (blake3, deterministic) computes the group seed from the operator-supplied name, and a factory closure constructs each replica with the resolved spec.

**Auto-respawn by default.** `HealthMonitor` is installed by default; operators who want bare-metal control opt out. Replica failures trigger respawn via the cached factory with exponential backoff on persistent failures.

**`--print-bootstrap` flag.** Emits a single JSON line to stdout before entering the wait loop: `{"node_id": N, "bound_addr": "127.0.0.1:54321", "public_key_hex": "abcd…"}`. Binding test fixtures and CLI subprocesses read the first stdout line, parse it, and use the triple to drive their handshake — no more parsing tracing output.

**Parallel group spawn at boot.** Groups declared in TOML start their replicas in parallel via `try_join_all` (replica `on_start` is independent within a group; group-level startup is independent across groups). Boot time on a 4-group × 3-replica config drops from sequential `4 × 3 × on_start_time` to `max(on_start_time)`.

**`AggregatorSpec` unification.** `GroupConfig` and `TemplateConfig` unify behind one `AggregatorSpec`; the spawn-and-register path is shared between boot-time groups and RPC-spawned groups.

---

## Cross-subnet detail-on-demand RPC

When a subscriber sees a summary (via `ParentVisible` / `Global` summary channels) and wants detail from the source subnet, it RPCs the aggregator. The wire and client surface are first-class.

**`FoldQueryService` (`fold.query`).** Aggregator daemons install the service automatically. Query shape: `(kind: u16, class: u64, query: Bytes)` → `Bytes` (fold-specific, postcard-encoded). The aggregator answers from its local fold state; the gateway forwards the RPC based on `subnet_id` + channel visibility per the substrate's normal routing. No new wire protocol, no special-case routing.

**`FoldQueryClient` with cache semantics.** `query_latest(target, kind)` consults a per-target LRU keyed on `(target_node_id, kind)` with `DEFAULT_QUERY_CACHE_TTL` (5s) before going to the wire; `query_summarize_now(target, kind)` forces a fresh tick on the host and bypasses the cache. `with_ttl` / `with_deadline` builders override defaults; `invalidate_cache` / `invalidate_target` give explicit eviction. Same cache semantics across every language binding.

**Discovery via the registry.** Replicas are tagged with `role:aggregator`; the source subnet is in their identity. `route_event` on the underlying group picks the closest healthy replica.

**Copy-on-write latest buffer.** The aggregator's latest-summaries buffer is `Arc<Vec<SummaryAnnouncement>>` — `tick_once` returns its novel batch directly, `SummarizeNow` reads it without re-copying, and the buffer evicts oldest-first via `VecDeque`.

---

## `aggregator.registry` RPC + `RegistryClient`

A single RPC service drives every aggregator operation across the wire.

**Wire surface (`RegistryRequest` / `RegistryResponse`).**

- `List` → `Groups(Vec<RegistryGroupSummary>)` — every registered group with per-replica health.
- `Spawn { template_name, group_name, replica_count }` → `Spawned(RegistryGroupSummary)` — daemon resolves the template, derives the seed from the group name, spawns N replicas via the lifecycle group.
- `Unregister { group_name }` → `Unregistered { existed }` — stops the group cleanly.
- `Scale { group_name, template_name, target_replica_count }` → `Scaled(RegistryGroupSummary)` — dedicated in-place grow/shrink; held replicas keep their tokio loops and their identities across the resize (no Unregister + Spawn churn). The server re-resolves the template and compares against the group's stored `source_subnet` + `fold_kinds` so a `--template` typo is caught before any state change.

**Server split.** The registry service splits into `RegistryReadHandler` (List-only — installable without a spawner) and `RegistryHandler` (full read + write). Read-only deployments don't pull in spawn-side dependencies.

**`RegistryClient` + `BoundRegistryClient`.** `RegistryClient::new(mesh).with_deadline(d).list(target_node_id) / spawn(...) / unregister(...) / scale(...)`. `BoundRegistryClient::for_node(mesh, target_node_id)` binds the target once so subsequent calls don't repeat it.

**Typed error discrimination.** `RegistryClientError { Transport, Codec, UnknownTemplate, DuplicateGroupName, SpawnRejected, SpawnNotSupported, ScaleRejected, UnknownGroup, Server(detail) }` — kind discrimination flows through to every language binding's native error type.

**Wire metadata on `RegistryGroupSummary`.** Carries `name` + `group_seed` (32 raw bytes → 64-char lowercase hex in language SDKs) + `source_subnet` + `fold_kinds` + `replicas: [{generation, healthy, diagnostic, placement_node_id}]`. The `source_subnet` + `fold_kinds` fields land in the same wire bump as `Scale` so `net aggregator ls --remote` renders the full spec without a separate `Describe` op.

**Parallel scale grow.** Scale-up grows via a bulk `add_replicas` helper that runs each replica's `on_start` in parallel via `try_join_all`. Same shape as boot-time parallel group spawn.

**`typed_call` helper.** A thin `MeshRpc::typed_call` wrapper carries the postcard codec + deadline plumbing for both `FoldQueryClient` and `RegistryClient`; client code stops re-implementing the marshal-call-unmarshal-translate-error chain per surface.

---

## CLI remote-attach

The CLI grows the ability to drive a remote daemon, not just inspect the local one.

**`CliContext::with_mesh` + remote-attach flags.** `CliContext` now optionally carries an `Arc<MeshNode>` constructed at build time when `--node-addr <ip:port> --node-pubkey <hex> --node-id <n>` are passed (or the `--remote <NAME>` shortcut pulls all three from a named profile). The local mesh boots on `127.0.0.1:0` with a PSK from the profile, connects to the remote daemon via the routed handshake path (see below), and starts the dispatch loop before any verb runs.

**`net aggregator query / spawn / scale / ls` against live daemons.** Each verb consumes `ctx.mesh_node()` and dispatches via the typed client.

- `query --kind <hex>` → `FoldQueryClient::query_latest` (or `--fresh` → `query_summarize_now`). Output renders the `SummaryAnnouncement`s as JSON via the existing `SummaryRow` shape.
- `spawn --template <NAME> --name <NAME> --replica-count <N>` → `RegistryClient::spawn`. `--source-subnet` is gone from `spawn` — the template owns it, and the daemon resolves it on the wire side.
- `scale --template <NAME> --name <NAME> --replica-count <N>` → `RegistryClient::scale`. Atomic in-place resize; held replicas keep their generations across the call.
- `ls --remote` → `RegistryClient::list`. Renders `source_subnet` + `fold_kinds` + per-replica rows from the wire shape.

**Routed handshake via `connect_via`.** The substrate's dispatch loop drops direct handshake msg1 packets from peers it hasn't pre-`accept`ed — only the initiator-side has a post-start registry; the responder side was explicitly deferred. CLI subprocesses generate fresh ephemeral identities, so the daemon can't pre-`accept` them. The CLI now connects via `connect_via` (routed) — `handle_routed_handshake` Case 2 already accepts msg1 from fresh initiators against a running dispatch loop. Substrate change: `connect_via` populates `addr_to_node[relay_addr]` via `entry().or_insert(...)` (preserves true multi-hop semantics when relay ≠ final dest), and honors the same `handshake_retries` knob as direct `connect`.

**Profile + flag precedence.** `[default].psk_hex` + `[default].node_addr / node_pubkey / node_id` in the CLI config serve as bootstrap shortcuts when always pointing at the same daemon; flags override. Bad pubkey / wrong PSK / wrong node_id all map to typed `CliError::RemoteAttach` with exit code 6 ("connection / handshake failure").

**CLI integration test.** `cli/tests/aggregator_remote.rs` boots `net-aggregator-daemon` (in-process via the library helper, not as a subprocess — same trick the bootstrap pin test uses), reads `node_id` / `bound_addr` / `public_key` from the booted handle, then invokes each verb via `assert_cmd::Command::cargo_bin("net")` and asserts exit codes + JSON shapes. Positive paths plus bad pubkey (exit 6), wrong PSK (exit 6), unknown template (exit 3).

---

## SDK surface across five languages

Every operator-facing aggregator surface ships in Rust, TypeScript, Python, Go, and C — same wire types, same error kinds, same factory-callback infrastructure where applicable.

**`net_sdk::aggregator` module (Rust).** Re-exports the client-only types (`RegistryClient`, `FoldQueryClient`, error types, default constants) plus the daemon-author surface (`AggregatorConfig`, `AggregatorDaemon`, `AggregatorRegistry`, `LifecycleGroup`, `HealthMonitor`, `Summarizer`, the two built-in summarizers, the registry service installers). `BoundRegistryClient::for_node` binds a target id once so subsequent calls don't repeat it. `install_default_service` is the one-call read-only registry installer. Aggregator is promoted to a first-class default SDK feature flag — operators don't opt in via `--features aggregator`; it's on by default everywhere the SDK is consumed.

**TypeScript (NAPI + `@net-mesh/sdk`).** `import { RegistryClient, FoldQueryClient } from '@net-mesh/sdk'`. Constructors take a `MeshNode`; `withDeadline(ms)` / `withTtl(ms)` are builders; ops return promises. Group seeds are 64-char lowercase hex strings (BigInt is awkward at 32 bytes); u64 fields are `BigInt`. Errors are JS `class RegistryClientError extends Error` with `kind: string` + optional `serverDetail`.

**Python (PyO3).** `from net_mesh.aggregator import RegistryClient, FoldQueryClient`. asyncio futures via `pyo3-asyncio` (already in the binding's dep set). Returned shapes are dicts; errors are typed subclasses of `RegistryClientError` (`UnknownTemplate`, `DuplicateGroupName`, `SpawnRejected`, `SpawnNotSupported`, etc.).

**Go (CGO).** `net_mesh.NewRegistryClient(mesh).List(ctx, targetNodeID)` / `.Spawn(...)` / `.Scale(...)` / `.Unregister(...)`. `context.Context` carries the deadline (honors `ctx.Deadline()` if set, falls back to the client's configured default). `RegistryClientError { Kind, Detail }` implements `error` + matches via `errors.Is`. A consumer-side Go wrapper around the cgo cdylib keeps the test surface idiomatic.

**C ABI (in the main `net-mesh` cdylib).** `net_registry_client_new / free / with_deadline / list / spawn / unregister`. Errors as `net_registry_error_kind_t` discriminant + `net_registry_last_error_detail` accessor. Also: `net_visibility_t` (`GLOBAL` / `PARENT_VISIBLE` / `SUBNET_LOCAL`) + `net_register_channel(mesh, name, visibility)` — C consumers can now configure channels with the visibility tier, not just read snapshots.

**Wire contract locked across languages.** One table fixes `group_seed` encoding (32 raw bytes → 64-char hex), u64 marshaling per language, deadline carriers (TS: number ms; Py: float seconds; Go: `time.Duration`), and the canonical error-kind string set (`transport`, `codec`, `unknown-template`, `duplicate-group-name`, `spawn-rejected`, `spawn-not-supported`, `unknown-group`, `scale-rejected`). Bindings that diverge fail their compatibility test.

---

## Deck: subnets, gateways, aggregators as first-class tabs

The cyberdeck grows three new tabs in the tab strip plus a focus page that drills into a single subnet.

**Tab strip overflow + horizontal scrolling.** The strip scrolls horizontally to keep the current tab visible; trailing letter-key hints render on overflowed entries so operators can still hit them by key. `SUBNETS`, `GATEWAYS`, `AGGREGATORS`, and `AUDIT` join the strip alongside the existing tabs.

**`SUBNETS` tab.** Cursor-navigable table of subnets with `PARENT` / `HEALTH` / `AGG` columns. Local subnet renders a `LOCAL: yes/—` marker (the prior name-highlighting was visually noisy and dropped). Pressing Enter drills into a per-subnet focus page.

**`SUBNETS` focus page.** Per-subnet health rollup at the top; members render via the shared `NODES` table widget so cursor + filter behavior matches the main `NODES` tab. The focus title pops off the redundant id row.

**`GATEWAYS` tab.** Cursor-navigable per-channel rows with `CHANNEL` / `VIS` / `REACH` columns — operator sees, at a glance, which channels have which visibility and which subnets they're reaching. Forwarded / dropped counters render in the existing rollup section.

**`AGGREGATORS` tab.** Cursor + scrolling over the registry snapshot. Live read of `AggregatorRegistry::snapshot` (which bundles every field a renderer needs in a single lock — no fan-out reads per row).

**Demo fixtures.** `deck::demo::fixtures` ships canned `SUBNETS` / `GATEWAYS` / `AGGREGATORS` shapes so the demo mode renders the new tabs against believable data.

**Widget refactors.** A `section_title` helper de-duplicates panel-title boilerplate across widgets; `subnets_with_members` is shared between the CLI and the `SUBNETS` tab so the two views render the same shape.

---

## Test hygiene

- **Lib suite at 4050+ tests** (was 3950+ at v0.21 release). 100+ net new tests across the fold framework (property tests for apply-then-query consistency, TTL expiry determinism, snapshot-restore round-trips, 100K applies/sec stress, concurrent apply + query), the lifecycle layer (`add_replica_grows_in_place_preserving_existing_replicas`, `remove_last_stops_only_the_last_replica`, `remove_last_refuses_to_drop_below_one`, parallel-Vec invariants under `spawn_with_placement`), the registry service (`scale_grows_existing_group_via_template`, `scale_shrinks_existing_group_to_target`, `scale_rejects_unknown_group`, `scale_rejects_template_mismatch`, `scale_to_same_count_is_noop_and_returns_current_snapshot`, `scale_to_zero_is_rejected`), and the CLI remote-attach surface (`cli/tests/aggregator_remote.rs` — every verb against an in-process daemon).
- **Cross-language wire round-trip pinning.** Every binding has a test that pins the canonical error-kind string set + the `group_seed`-as-hex encoding against the locked wire table. A binding that drifts fails its compatibility test.
- **`cargo clippy --features meshos,deck,aggregator --all-features --all-targets -- -D warnings` clean.** The strict floor from v0.20.2 (`unwrap_used`, `expect_used`, `undocumented_unsafe_blocks`, `multiple_unsafe_ops_per_block`) stays armed; aggregator-side hits caught (`manual_is_multiple_of` in the `HealthMonitor` backoff retry-gate).
- **`cargo doc --features meshos,deck,aggregator --no-deps` clean under `RUSTDOCFLAGS="-D warnings"`.** Doc-comment hygiene includes rustdoc intra-doc links surfaced under the new warning floor.
- **Codecov coverage** sits at ~90% on the substrate feature set, informational on the CI status — same posture as v0.21.
- **CI pipeline additions.** Aggregator-daemon + registry-RPC test job; consumer-side Go wrapper exercised against the cdylib; bindings CI enables the `aggregator` feature so the surface isn't gated out of the published bindings.

---

## Breaking changes

### `CapabilityIndex` removed; callers migrate to `Fold<CapabilityFold>`

The legacy `CapabilityIndex` module is deleted. Callers (`MeshNode::capability_index`, `Scheduler::place_*`, replica/fork/standby placement, the FFI surface, the Deck capability panel) all moved in the same diff. Consumers reaching into the legacy type directly need to switch to `Fold<CapabilityFold>` query / snapshot; the inverted-index tag/region/state lookups are part of the fold's `Index` and surface via `CapabilityQuery`.

### `RoutingTable` removed; callers migrate to `Fold<RoutingFold>`

The pingwave-driven `RoutingTable` is deleted. `Router::lookup` and `MeshNode::dispatch_packet` consult the fold instead. Pingwave packets are repurposed as `SignedAnnouncement<RouteAnnouncement>` on the `fold:route:` channel — same wire RTT measurement, new envelope.

### `MeshDaemon` is no longer the trait an aggregator-shaped daemon implements

`MeshDaemon` stays sync-only / WASM-compatible for compute daemons. Async tier services (aggregators today, market matchers / settlement bridges / reputation oracles later) implement the new `LifecycleDaemon` trait and are deployed via `LifecycleGroup<L>`. Existing `MeshDaemon` implementations are unaffected; this is a sibling trait, not a replacement.

### `net aggregator spawn --source-subnet` is gone; `--template` is required

`net aggregator spawn` takes `--template <NAME>` (required) — the template owns the source subnet, summary visibility, fold kinds, and summary interval. `--source-subnet` was parse-only in v0.21 (the verb errored out before doing anything); no scripted CLI consumer broke, but the help text changed and the flag is removed.

### `net aggregator scale` takes `--template <NAME>`

Scale needs the operator to re-supply the template name so the server can sanity-check against the group's stored spec (template mismatch → `ScaleRejected("template mismatch")` before any state change). Same shape as spawn — operators copy the spawn invocation, swap the verb, change the replica count.

### `RegistryGroupSummary` gains `source_subnet` + `fold_kinds`

Wire shape additive — postcard appends; existing readers tolerate the additional fields. SDK consumers that rendered the old shape see the new fields populated; constructors that built the struct by hand grow two parameters.

### `RegistryRequest` / `RegistryResponse` grow a `Scale` variant

Operators ship CLI + daemon together; no backwards-compatibility constraint. Variant added at the tail; existing match arms compile unchanged.

### `AggregatorDaemon::on_stop` no longer drops mid-publish work

The shutdown-aware tick loop checks the shutdown flag between publishes, and the backstop deadline bumped to absorb realistic publish latencies. Behavior change: an in-flight `mesh.publish().await` at shutdown now completes (up to the new backstop) instead of being aborted. Consumers that relied on the abort timing (none in the substrate) need to revisit.

### `AggregatorGroupEntry` lives behind `AggregatorRegistry::snapshot`

Registry inspection goes through `AggregatorRegistry::snapshot()`, which bundles every field a renderer needs in one lock. Direct field access on `AggregatorGroupEntry` is no longer the supported path.

### `aggregator` is a default SDK feature

`net-sdk` ships with the aggregator surface on by default. Consumers who explicitly disabled default features and want the aggregator client get it via `--features aggregator`; consumers on default features see the new module unconditionally.

---

## How to upgrade

1. **Rust consumers — update the dependency to `0.22`.** Most consumers see only the additions. Direct consumers of the legacy `CapabilityIndex` / `RoutingTable` need to switch to the fold APIs (the compiler points at every site).

2. **Daemon authors with an async daemon — implement `LifecycleDaemon`, not `MeshDaemon`.** If your daemon does `tokio::interval` work or `await`-blocking publish/subscribe, it's a `LifecycleDaemon`. Deploy via `LifecycleGroup<L>::spawn` or `spawn_with_placement`; auto-respawn via `register_with_monitor`. Existing `MeshDaemon` impls are unchanged.

3. **Operators running aggregators — switch to `net-aggregator-daemon`.** Drop the binary in `/usr/local/bin` (or your platform's equivalent), write a TOML config with `[[template]]` blocks (and optionally `[[group]]` blocks for boot-time instantiation), run `net-aggregator-daemon --config foo.toml`. Spawn additional groups dynamically via `net aggregator spawn --template … --name … --replica-count N --node-addr … --node-pubkey … --node-id …`. The bootstrap triple comes from `net-aggregator-daemon --config foo.toml --print-bootstrap` (first stdout line, JSON).

4. **CLI users — adopt `--node-addr / --node-pubkey / --node-id` or a `--remote <NAME>` profile.** `net aggregator query / spawn / scale / ls --remote` now round-trips against any daemon you can reach. Set `[default].psk_hex` + `[default].node_addr / node_pubkey / node_id` in your CLI config for a one-flag `--remote default` shortcut.

5. **SDK consumers (TypeScript / Python / Go / C) — `RegistryClient` + `FoldQueryClient` are first-class.** Pull the surface from `@net-mesh/sdk` / `net_mesh.aggregator` / `net_mesh` Go package / the C cdylib's `net_registry_client_*` symbols. Same wire contract across every language — error kinds, `group_seed` as 64-char hex, u64 marshaling per the locked table.

6. **`net aggregator spawn` callers — switch to `--template`.** The `--source-subnet` flag is gone. Templates live in the daemon's TOML; operators reference them by name. `--name` + `--replica-count` are unchanged.

7. **No CI config change required.** The strict clippy floor stays armed; rustdoc warnings stay denied; the test-side allow-list is unchanged. CI adds an aggregator-daemon + registry-RPC test job and enables the `aggregator` feature on bindings — repo CI picks both up automatically.

8. **Operators — bump the binary.** Pre-built `net-mesh`, `net-deck`, and `net-aggregator-daemon` binaries land in the release archive for every supported target (Linux x86_64 / aarch64, macOS x86_64 / aarch64, Windows x86_64). Drop in `/usr/local/bin` and restart. Wire format is additive from v0.21; mixed-version fleets handshake cleanly, though the new fold envelopes and `Scale` RPC obviously won't reach pre-v0.22 peers.

9. **Deck users — three new tabs.** `SUBNETS`, `GATEWAYS`, and `AGGREGATORS` appear in the tab strip; the strip scrolls horizontally on overflow. Press Enter on a subnet row to drill into its focus page; cursor + filter on every new table.

---

Released 2026-05-24.

## License

See [LICENSE](../../LICENSE-APACHE).
