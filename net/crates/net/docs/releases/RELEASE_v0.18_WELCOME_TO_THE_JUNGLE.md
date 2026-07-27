# Net v0.18 — "Welcome to the Jungle"

*Named after Guns N' Roses's 1987 Appetite for Destruction opener. v0.15 stood up the Dataforts data plane. v0.16 stacked the MeshDB query plane on top. v0.17 stacked the MeshOS behavior plane on both. v0.18 is the operator plane — the TUI cyberdeck, the command-line surface, and the daemon-author / operator SDKs across five languages — that turns the substrate the prior releases built into something a human can see, command, and break-glass.*

## The operator plane

For three releases the substrate has been growing in capabilities that nothing outside the cluster could observe. v0.15 made replicas place themselves and blobs move under operator-defined policies; v0.16 made every chain federally queryable; v0.17 turned every node into a single reconciling event loop with admission control and an admin-event ledger. By the end of v0.17 the cluster was a living distributed operating system — and the only way to interact with it was to write Rust against the substrate crate. v0.18 closes that gap.

Three surfaces land in this release. **Deck** is the operator TUI — a real-time terminal cyberdeck rendering everything MeshOS, MeshDB, RedEX, and Dataforts are doing, with signed admin actions, ICE break-glass overrides, and a blast-radius preview that simulates every dangerous action before it commits. **Net CLI** is the command-line operator surface — the same admin verbs Deck exposes (drain / cordon / maintenance / drop-replicas / restart-all / invalidate-placement / clear-avoid-list), the same ICE break-glass actions, the same audit-chain reads, the same NetDB local-store + MeshDB query surfaces, every command JSON-output-able for scripting, every operation gated by the same operator identity. **MeshOS SDK** and **Deck SDK** ship daemon-author and operator-tooling surfaces in Rust (canonical), Python (pyo3), TypeScript (napi-rs), Go (cgo), and C (raw FFI) — five-language parity behind one common substrate-side wire contract.

There is no separate observability service to provision. There is no admin RPC to harden. The TUI, the CLI, and every binding compose against the same `MeshOsRuntime` + `DeckClient` + admin-chain primitives the substrate already ships. Operators see the cluster move, and they can move with it, in whatever language they already write.

---

v0.18 lands **the full Deck TUI** (cluster topology map, replica + placement inspector, daemon supervision panel, maintenance node control, behavior timeline, admin surface with signed ops, MeshDB console, log matrix, operator identity + audit trail, node inventory, ICE break-glass with blast-radius preview — see `DECK_FEATURES.md` for the operator-facing tour), **the Net CLI Phase 1 + 2 + 3** (read-only inspection, mesh + nRPC client + capability surface, admin verbs + ICE preview + audit + identity store + NetDB + MeshDB query — see `NET_CLI_PLAN.md`), **the MeshOS daemon-author SDK** in all five languages (Rust canonical, Python pyo3 wrapper with Protocol class + async control-event iterator, TypeScript napi-rs with full TSFN-bridged daemon trait + AsyncIterable control events, Go cgo with `MeshOsDaemon` interface + cgo trampolines for snapshot/restore/onControl/health/saturation, C raw FFI with vtable + last-error pair — see `MESHOS_SDK_PLAN.md`), and **the Deck operator SDK** in all five languages covering `DeckClient` lifecycle, all 9 `AdminCommands` verbs, ICE break-glass simulate → commit typestate, snapshot + status-summary + log + failure + audit streams (see `DECK_SDK_PLAN.md`).

Every binding ships behind one common wire contract. The Python wheel, the npm package, the Go binding's `bindings/go/net` tree, and the C `libnet_deck` / `libnet_meshos` cdylibs all serialize the same `MeshOsSnapshot` + `ChainCommit` + `StatusSummary` shapes the substrate emits, with one operator-id + signature envelope across every admin commit. A Python operator's blast-radius dry-run produces the same `affected_nodes` / `affected_replicas` / `affected_daemons` count a Rust operator would see; a TypeScript admin verb commits the same `ActionChainRecord` a CLI invocation would. Cross-language SDK consumers see one cluster.

The hardening posture from the Black Diamond → Rebel Yell → Eye of the Tiger → Atomic Playboys line continues. **Five coordinated code-review passes** landed before the v0.18 branch cut, covering the Deck SDK design + implementation (`CODE_REVIEW_2026_05_14_DECK_SDK.md` — 34 items closed across 22 commits), the Deck TUI render layer (`CODE_REVIEW_2026_05_16_DECK_TUI.md` + second pass — 48 + 17 items closed across two passes), the cross-language MeshOS SDKs (`CODE_REVIEW_2026_05_17_MESHOS_SDKS.md` + second pass — 8 Criticals / 23 Importants / 16 Nits and 14 follow-up items across two passes), and the Net CLI Phase 1 surface (`CODE_REVIEW_2026_05_17_NET_CLI_PHASE1.md` + second pass). Every numbered item closed in-tree with regression coverage where the shape made one possible. The list is real work: the signing-payload envelope now carries a domain tag + nonce + expiry so an ICE simulation can't be replayed as a different action variant; the substrate enforces simulate-before-commit through a `SimulatedIceProposal` typestate that consumes itself on commit (no more "commit re-runs simulate from scratch" path the first review caught in all three FFIs); the operator registry deduplicates by operator id before threshold check so M-of-N can't be satisfied by the same operator signing twice; per-node ICE cooldown lands as a 5-minute gate before the same node accepts another break-glass; the `IceProposal: Send` regression that broke `tokio::spawn` is fixed at the substrate layer; seq counters survive restart via RedEX chain replay; the `dispatch_kill_migration_if_applicable` ordering bug that ran before `actual.apply` is corrected so kills observe the freshest reconcile state. **3115+ lib tests + 14 deck-pipeline integration tests + 11 net-cli phase 1 / 2 / 3 exit-code tests + 86 net-cli help-text snapshot tests** all pass. `cargo clippy --features meshos,deck --all-targets -- -D warnings` clean across substrate + every binding crate + the deck demo.

The Rust crate now ships the same default feature stack as the Python wheel and the Node npm package (`net`, `nat-traversal`, `cortex`, `meshdb`, `meshos`, `dataforts`) — `cargo add ai2070-net` previously gave you nothing; v0.18 gives Rust consumers the full operator stack out of the box. The `redis` external-service dep stays opt-in. The Python and Node defaults are unchanged.

No new dependencies. No protocol changes. The crate version moves from `0.17.x` to `0.18.0`.

---

## Deck

The operator cyberdeck. Lives in the workspace member `deck/` (binary-only — `[[bin]]` with no `[lib]`).

```
deck/src/
├── app.rs              — main event loop + tab routing
├── tabs/
│   ├── net_map.rs      — cluster topology map (RTT edges, avoid-list, maintenance flags)
│   ├── replicas.rs     — replica + placement inspector
│   ├── daemons.rs      — daemon supervision panel
│   ├── daemon_page.rs  — per-daemon log tail + control surface
│   ├── nodes.rs        — node inventory + saturation trends
│   ├── node_page.rs    — per-node deep dive
│   ├── behavior.rs     — MeshOsSnapshot timeline
│   ├── blobs.rs        — blob explorer (replica locations, heat, ancestry)
│   ├── dataforts.rs    — local + remote adapter telemetry
│   ├── admin.rs        — signed admin surface
│   ├── ice.rs          — break-glass overrides with blast-radius
│   ├── meshdb.rs       — MeshDB query console
│   ├── logs.rs         — RED/HEAT/INFO log matrix
│   ├── audit.rs        — operator audit trail
│   ├── failures.rs     — recent-failure ring
│   └── groups.rs       — ReplicaGroup / ForkGroup / StandbyGroup roster
├── widgets/
│   ├── confirm.rs      — blast-radius confirm modal
│   ├── footer.rs       — toast + status footer
│   └── cursor.rs       — cursor + `/` filter primitives
├── streams.rs          — RedEX + Deck subscription routing
├── bookmarks.rs        — persistent cluster bookmarks
├── lineage.rs          — chain ancestry walks
└── demo/               — 9-node spawn harness for local development
```

The TUI composes against `DeckClient` (the operator SDK that ships in the same release — see below). Every tab is a `ratatui` widget that reads from a `MeshOsSnapshotReader` + a set of stream readers, renders at 60 fps when there's activity, idles at 1 Hz otherwise. The cursor + `/` filter primitives are tab-uniform; `g` / `G` jump to top / bottom on every cursor tab; `Enter` opens a detail page; `?` shows context-aware help. A blast-radius confirm modal pre-flights every admin commit — the modal prints "This action affects N nodes, M replicas, K daemons. Type YES to confirm" and refuses to dispatch if the operator types anything else. Warnings beyond the modal's 3-row cap surface as `… +N more (see AUDIT)`.

The ICE break-glass surface is the cyberpunk SRE panel — seven force-level operators (`force-drain`, `force-evict-replica`, `force-restart-daemon`, `force-cutover`, `kill-migration`, `flush-avoid-lists`, `freeze-cluster` / `thaw-cluster`) each gated through a `simulate()` → `commit(signatures)` typestate. The simulation runs the same reconcile arms the production loop runs and surfaces the projected blast radius (which replicas move, which daemons restart, which nodes become hot, expected drain delay, placement stability impact). The commit threshold defaults to 1-of-1 in development; production deployments raise it to 2-of-N via `DeckClientConfig::ice_signature_threshold` and the M-of-N gate enforces operator-id deduplication before counting signatures.

Behavior visibility folds back through the MeshDB query plane the prior releases shipped. The behavior timeline tab reads `MeshOsSnapshot` from a `MeshQuery::Latest` against the per-node snapshot chain — no Deck-specific RPC, no separate observability stack. The MeshDB console tab is a fully-interactive query editor: write a `QueryBuilder` chain in the operator's preferred shape, hit `Enter`, watch the federated executor route to a node holding the relevant fold, scroll the streaming result rows.

The render pipeline carries the polish details a TUI needs to feel alive: `net_map`'s unreachable peers render as the hollow diamond the legend advertises (not a red filled diamond — the second-pass review caught this); `tabs::logs` reuses an `ascii_icontains` helper instead of lowercasing the haystack per record per render (the previous form reintroduced a per-frame `String` alloc); the BLOBS poll unions every wired adapter (not just `blob_adapters[0]`) and surfaces per-adapter errors as footer toasts; the `cursor_to_bottom` for DATAFORTS uses the visible row count (not `blob_adapters.len()`) so `G` lands on the last visible row when remote dataforts exist. `tabs::short_id` is canonical across `daemon_page` and `groups` (0xXXXXXX, 6-padded). The `fmt_ts_hms_ms` and `unix_now_ms` helpers are hoisted once so the three prior copies don't drift.

---

## Net CLI

The command-line operator surface. Lives in `cli/` (a `[[bin]]`-only workspace member; binary name `net`).

```
cli/src/
├── main.rs        — clap dispatch + global flags
├── context.rs     — identity / config / output-format plumbing
├── identity.rs    — operator key generate / load / rotate
├── node.rs        — node start / status / health
├── chain.rs       — RedEX chain inspect + tail
├── netdb.rs       — local NetDB CRUD (tasks / memories)
├── meshdb.rs      — federated MeshDB query
├── admin.rs       — 9 signed admin verbs
├── ice.rs         — 7 ICE break-glass verbs with simulate → commit
├── audit.rs       — admin + ICE audit reads
├── logs.rs        — log stream subscription + filter
├── daemon.rs      — daemon roster + supervision
├── rpc.rs         — typed nRPC client
└── version.rs     — semver + feature surface report
```

The CLI is the same admin surface Deck exposes, mapped onto `clap` subcommands. `net admin drain --node N --drain-for 30s` commits the same `ChainCommit` Deck's admin tab would; `net ice freeze-cluster --reason "incident X" --preview` runs the same simulate-before-commit pre-flight Deck's ICE tab runs and prints the blast-radius JSON to stdout for scripting; `net netdb tasks ls --json` reads the local NetDB store and emits JSON for piping into `jq`. Every command honors `--output {pretty,json}` for human vs. script output and exits with stable error codes — 0 success, 1 generic failure, 2 invalid arguments, 3 not found, 4 already exists, 5 permission denied (no operator key for an admin commit), 6 not connected, 7 timeout, 8 confirmation refused on a non-TTY ICE without `--yes`.

The ICE preview workflow is locked in. `net ice <verb> --preview` runs `IceProposal::simulate()` and prints the `BlastRadius` JSON without committing — the same dry-run shape that ships in every SDK. `net ice <verb> --yes` commits without the TTY confirmation gate; `net ice <verb>` alone (TTY) prints the simulation and prompts for `Type YES to confirm:` before reaching the substrate. The TTY-only `--yes` path is the same gate `net admin` uses for cordon / drain / drop-replicas — the simulation isn't the gate, the confirmation is.

The identity store at `~/.config/net/operator/<name>.key` is the canonical authentication source for every admin / ICE commit. `net identity generate <name>` creates a fresh ed25519 keypair, prints the public key for registry installation, and writes the private key to disk with 0600 mode. `net identity ls` enumerates installed identities; `net identity rotate <name>` rotates with audit-trail commitment. The CLI refuses to commit an admin or ICE event under an ephemeral keypair — admin writes require an explicit operator identity (the second-pass review caught the silent ephemeral-keypair fallback).

JSON output is stable. `OperatorIdentity`'s `operator_id` is a `u64` decimal; `ChainCommit`'s `event_kind` is a string enum (`drain` / `enter-maintenance` / `cordon` / etc., not the Rust `Debug` form — the first-pass review caught `TaskRow` and `MemoryRow` shoving debug-printed structs into named fields and corrected them); timestamps are ISO 8601 with `Z` suffix; durations honor humantime input but always emit milliseconds-as-integer.

86 help-text snapshot tests pin every subcommand's `--help` output so wording can't drift accidentally. 11 exit-code tests cover the documented exit-code contract end-to-end through a real substrate boot. The CLI is the operator surface; "scriptable + stable + signed" is the contract.

---

## MeshOS SDK

The daemon-author SDK. Mirrors the canonical Rust surface from v0.17 (`MeshOsDaemonSdk` + `MeshOsDaemonHandle`) in four more languages with one shared wire contract. Lives in:

- **Rust (canonical)**: `sdk/src/meshos/` — `MeshOsDaemonSdk::start(config)` / `register_daemon(daemon, identity)` returning `MeshOsDaemonHandle` with `next_control()` / `try_next_control()` / `publish_log()` / `publish_capabilities()` / `graceful_shutdown()`.
- **Python (pyo3)**: `bindings/python/python/net/` + `sdk-py/src/net_sdk/meshos.py` — `MeshOsDaemon` Protocol class for type-checker verification; `MeshOsDaemonSdk.start()` returns a handle with sync `next_control` + async `anext_control` + `async for ev in handle` iterator; context-manager dunders drive graceful shutdown on scope exit.
- **TypeScript (napi-rs)**: `bindings/node/` + `sdk-ts/src/meshos.ts` — `registerDaemon(daemon: DaemonObjectTsfns, identity)` accepts a full daemon object with TSFN-bridged `process` / `snapshot` / `restore` / `onControl` / `health` / `saturation`; the napi `MeshOsDaemonHandle` exposes async control-event iteration through an `AsyncIterable<DaemonControl>`.
- **Go (cgo)**: `bindings/go/net/meshos.go` — `MeshOsDaemon` interface with cgo trampolines (`goMeshOsProcessTrampoline`, `goMeshOsSnapshotTrampoline`, `goMeshOsRestoreTrampoline`, `goMeshOsOnControlTrampoline`, `goMeshOsHealthTrampoline`, `goMeshOsSaturationTrampoline`); `MeshOsDaemonSdk.RegisterDaemon` returns a handle with `ControlEvents() <-chan DaemonControl` (context.Context-cancellable) + `TryNextControl()`.
- **C (raw FFI)**: `include/net_meshos.h` + `libnet_meshos.{so,dylib,dll}` — `NetMeshOsDaemonVtable` carrying `name` / `process` / `health` / `saturation` / `on_control` / `snapshot` / `restore` function pointers; `net_meshos_register_daemon(sdk, &vtable, ctx, &identity)` returning an opaque handle; `net_meshos_next_control(handle, timeout_ms, out)` for blocking control reads; per-thread `net_meshos_last_error_message` / `net_meshos_last_error_kind` discriminator after every non-OK return.

Daemon-side only by lock. No placement APIs in any binding. No admin-event issuance. No MeshOS-control surfaces. The SDK is **the daemon contract** in five languages; operator tooling, federated interactions, and MeshDB queries belong to separate SDKs (the Deck SDK below, plus the existing MeshDB SDK that v0.16 shipped).

The cross-language hardening list is real work. The Go `MeshOsDaemonHandle.Free()` now blocks on the pump goroutine's in-flight `NextControl` before the C-free, so a concurrent shutdown can't race the cgo destructor (the second-pass review caught this); the Python `PyDeckClient.close()` path no longer swallows shutdown errors asymmetrically vs. Node; the Python standalone constructor gains `__enter__` / `__exit__` + `__del__` so dropping a client without an explicit close still tears down the supervisor; the TypeScript handle classes gain explicit `dispose()` / `[Symbol.dispose]()` for TC39 explicit resource management; the Go Deck streams pick up the same `Close`-vs-`Next` race fix the MeshOS streams shipped; operator seed bytes are zeroized at the binding boundary across Node / Python / Go standalone constructors.

Cargo features gate the feature surface uniformly: `meshos` activates the runtime symbols, `cortex` activates the snapshot fold layer the runtime composes against. Wheels / npm packages / Go cdylibs ship the full default set; `bindings/python/python/net/__init__.py` and the npm `@ai2070/net` package's lazy feature checks raise `ImportError` (Python) or surface a typed missing-feature error (Node) rather than `AttributeError` if the wheel was built without a feature.

The Python `_net.pyi` stub now carries full method signatures for every feature-gated class — MeshOS (`MeshOsDaemonSdk`, `MeshOsDaemonHandle`), MeshDB (`MeshQuery`, `MeshQueryRunner`, `QueryBuilder`, `Predicate`, the result-row family), and Deck (every class below). A `test_stub_drift.py` regression test parametrizes over every stub class and asserts the runtime symbol matches, so future PyO3 method renames trip CI rather than silently break IDE autocomplete.

---

## Deck SDK

The operator-tooling SDK. Mirrors the Rust `DeckClient` surface in four more languages with one shared substrate-side wire contract:

- **Rust (canonical)**: `sdk/src/deck/` — `DeckClient::new(operator_identity, config)` / `from_runtime(runtime, ...)` returning a client with `.admin()` / `.ice()` / `.audit()` / `.snapshots()` / `.status_summary_stream()` / `.subscribe_logs(filter)` / `.subscribe_failures(since_seq)`.
- **Python (pyo3)**: `bindings/python/python/net/` + `sdk-py/src/net_sdk/deck.py` — `DeckClient(operator_seed, config)` + `DeckClient.from_seed(seed, **config_kwargs)` wrapper; admin verbs return typed `ChainCommit` dataclasses; ICE break-glass typestate enforced through `IceProposal` → `SimulatedIceProposal` → `commit(signatures)`; context-manager dunders drive shutdown on scope exit.
- **TypeScript (napi-rs)**: `bindings/node/` + `sdk-ts/src/deck.ts` — `DeckClient` with `readonly admin` / `readonly ice` properties holding typed verb dispatchers; async-iterable `SnapshotStream` / `StatusSummaryStream` / `LogStream` / `FailureStream`; `DeckSdkError` carries the structured `kind` discriminator the substrate emits.
- **Go (cgo)**: `bindings/go/net/deck.go` + top-level `go/deck.go` companion — `DeckClient.Admin().Drain(node, drainForMs)` / `DeckClient.ICE().FreezeCluster(reason, ttl).Simulate()` / `.Commit(signatures)`; the stream surfaces use buffered `<-chan` with context-aware shutdown; `DeckError` wraps the substrate's `<<deck-sdk-kind:KIND>>MSG` envelope. The binding-tier `bindings/go/net/deck.go` covers all three slices (admin + ICE + logs + audit + failures); the top-level `go/deck.go` ships slice 1 (admin + status streams) — slices 2/3 callers use the binding tier directly.
- **C (raw FFI)**: `include/net_deck.h` + `libnet_deck.{so,dylib,dll}` — `net_deck_client_new(this_node, …, operator_seed, &out)` constructor; 9 `net_deck_admin_*` verbs; 7 `net_deck_ice_*` factories returning `NetIceProposal*` that consumes itself on `_simulate` (yielding `NetSimulatedIceProposal*` that consumes itself on `_commit`); snapshot / status-summary / log / failure / audit streams; per-thread `net_deck_last_error_kind` / `net_deck_last_error_message`.

The break-glass typestate is enforced across every language. `IceProposal` carries an `issued_at_ms`-tagged signing payload with a substrate-side nonce, domain-separation prefix, and one-minute commit-window expiry; `simulate()` builds a `SimulatedIceProposal` and freezes the substrate's reconcile arms against the proposal's effect projection; `commit(signatures)` verifies signatures against the operator registry's M-of-N threshold (deduplicating by operator id before counting — same operator can't sign twice), then commits to the admin chain and clears the per-node ICE cooldown. The substrate enforces simulate-before-commit; commit without a fresh simulation returns `consumed` rather than re-running simulate from scratch. The simulate path consumes itself on success — a second `simulate()` on the same proposal returns `consumed`, not a fresh blast-radius (the first-pass review caught the consumed-state sentinel reading back as a valid timestamp; the typestate flip closes both regressions).

```rust
impl DeckClient {
    pub fn new(identity: OperatorIdentity, config: DeckClientConfig) -> Self;
    pub fn from_runtime(runtime: &MeshOsRuntime, identity: OperatorIdentity, ...) -> Self;
    pub fn with_operator_registry(self, registry: OperatorRegistry) -> Self;

    pub fn snapshots(&self) -> SnapshotStream;
    pub fn status(&self) -> Result<MeshOsSnapshot, DeckError>;
    pub fn status_summary(&self) -> Result<StatusSummary, DeckError>;
    pub fn status_summary_stream(&self) -> StatusSummaryStream;
    pub fn subscribe_logs(&self, filter: LogFilter) -> LogStream;
    pub fn subscribe_failures(&self, since_seq: Option<u64>) -> FailureStream;
    pub fn audit(&self) -> AuditQuery;

    pub fn admin(&self) -> AdminCommands<'_>;
    pub fn ice(&self) -> IceCommands<'_>;
}

pub struct DeckClientConfig {
    pub snapshot_poll_interval: Duration,
    pub ice_signature_threshold: usize,  // M-of-N for ICE commits
}
```

`AdminCommands` exposes the 9 substrate admin verbs (drain / enter-maintenance / exit-maintenance / cordon / uncordon / drop-replicas / invalidate-placement / restart-all-daemons / clear-avoid-list). `IceCommands` exposes the 7 break-glass operators (freeze-cluster / thaw-cluster / flush-avoid-lists / force-evict-replica / force-restart-daemon / force-cutover / kill-migration). The `AuditQuery` builder is fluent — `.recent(n)` / `.by_operator(id)` / `.between(start, end)` / `.force_only()` / `.since(seq)` / `.collect()` / `.stream()` — and surfaces the same admin-event ledger across every binding.

The operator registry is the M-of-N gate. `OperatorRegistry::insert(id, public_key)` registers operators; `verify(payload, signatures)` returns `Ok(())` when the signature set meets the threshold and rejects duplicates by id. The registry survives RedEX chain replay so registrations made on one node propagate to every other node; bundle-verify (`verify_bundle`) covers the multi-signature ICE-commit path with the same dedup-by-id rule.

---

## Default feature parity

`net/crates/net/Cargo.toml` now ships defaults matching the Python wheel and Node npm package, minus `redis`:

```toml
[features]
default = [
    "net",
    "nat-traversal",
    "cortex",      # → redex, redex-disk transitively
    "meshdb",
    "meshos",
    "dataforts",
]
redis = ["dep:redis"]   # opt-in: external service dep
```

`cargo add ai2070-net` now gives Rust consumers the same operator stack Python and Node consumers already get. Existing Rust consumers who want the prior minimal surface can opt out with `--no-default-features`.

The Cargo features section at the bottom of every binding README (`bindings/python/README.md`, `bindings/node/README.md`, `bindings/go/net/README.md`, `sdk-py/README.md`, `sdk-ts/README.md`) documents the five relevant feature flags (`cortex`, `redex-disk`, `netdb`, `meshdb`, `meshos`), what each enables, and the build invocation for each binding so consumers building from source know what to pass.

---

## C ABI consolidation

The C SDK header story is cleaned up. `net.h` is the bus + shared error enum; `net_cortex.h` is the RedEX + CortEX + NetDb surface (new in this release — split out of the prior `net.go.h` catch-all); `net_rpc.h` is the RPC surface; `net_meshdb.h` is the MeshDB query layer; `net_meshos.h` is the MeshOS daemon-author surface; `net_deck.h` is the Deck operator surface. The convenience `net.go.h` `#include`s `net_cortex.h` and inlines RPC + Deck declarations for callers who want everything in one place.

The NetDb FFI lands in this release as well — `net_netdb_open` / `net_netdb_open_from_snapshot` / `net_netdb_snapshot` / `net_netdb_tasks` / `net_netdb_memories` / `net_netdb_close` / `net_netdb_free` plus `net_netdb_free_bundle` for the snapshot bytes. Adapter accessors hand out independent Arc-cloned `net_tasks_adapter_t*` / `net_memories_adapter_t*` handles — freeing them does NOT close the underlying adapter, and the NetDb itself can be freed before the adapter clones. The Go binding consumes this through `bindings/go/net/netdb.go`; Python and Node already consumed the Rust core directly.

The MeshDB and MeshOS cdylibs are unchanged (still separate libraries — `libnet_meshdb.{so,dylib,dll}`, `libnet_meshos.{so,dylib,dll}`). The new `libnet_deck.{so,dylib,dll}` cdylib ships alongside, built from the `net-deck-ffi` workspace member at `bindings/go/deck-ffi`. C consumers link the libs they want.

---

## Substrate hardening — pre-watcher pass

Alongside the SDK / TUI / CLI work, a three-pass bug audit closed **42 substrate + CLI items across 42 commits** before the v0.18 branch cut (`BUG_AUDIT_2026_05_17_NET_CLI.md`). Pass 1 covered the Net CLI command surface (17 items). Pass 2 extended outward into `adapter/net/**`, `sdk/src/**`, and the Python / Node / Go bindings (11 items). Pass 3 was specifically scoped to the layers a future `netdb-watcher` subscriber will sit on top of — the cortex adapter's fold loop, `RedexFile::tail` / `append`, and the `NetDb` façade (9 items). These are not the kind of bug that surfaces in unit tests at low load — they manifest under burst-write contention, lagged subscribers, or after the first restart. The Criticals would have made a watcher look broken; closing them ahead of any consumer means the watcher writes against substrate that already behaves.

Three Criticals closed in the cortex fold loop. The fold task used to be `tokio::spawn`-ed *after* `file.tail(start_seq)` already registered the live watcher — between registration and the spawned task being polled, concurrent appends could call `notify_watchers`, evict the watcher with `try_send(Err(Lagged))`, and the fold task's first `stream.next()` would yield `Some(Err(Lagged))` and break out of the loop before processing a single event. Moving the `tail` call to be the first statement inside the spawned task gives registration and consumption a deterministic ordering. The second Critical was the live `Lagged` match arm permanently killing the adapter — any subscriber falling behind `tail_buffer_size` once now re-subscribes at `folded_through_seq + 1` instead of silently halting the fold task forever. The third was `wait_for_seq` returning `Ok(())` when `running == false`, which couldn't distinguish "your seq is folded" from "the fold task crashed before reaching your seq"; it now returns `Err(folded_through_seq())` mirroring the sibling `wait_for_applied_seq`'s contract, so a watcher polling for a seq the substrate will never reach gets a typed signal instead of false success.

Three Highs closed across the RedEX and watermark layers. `notify_watchers` previously fired *before* fsync, so a crash after the broadcast but before the kernel flushed the page cache could lose an event from disk that subscribers had already acted on; the durability contract is now explicit and watchers reconcile from `last_persisted_seq + 1` on restart by `(channel, seq)` key. The `next_seq.fetch_add(1)` allocator could be visible to a concurrent `LiveOnly` opener across a failed-write rollback — a `LiveOnly` adapter opening during the rollback window would start its tail at the inflated seq, then silently filter out the real append at the lower seq. The `applied_through_seq` strict-prefix advance used `wrapping_add(1)`, which collided with the `u64::MAX` "nothing applied yet" sentinel when `seq` reached the boundary — the snapshot would persist `None`, restore would re-replay everything, and every pending `wait_for_applied_seq` would block forever. All three close before the watcher tier writes.

Three Mediums closed in the `NetDb` façade and the cortex `changes_tx` ordering. `NetDbBuilder::build()` with both `want_tasks=false` and `want_memories=false` used to silently return a no-op `NetDb` whose accessors panicked on first use — combined with the CLI's `--with-tasks` / `--with-memories` flag work, a misconfigured profile or test fixture would have turned a config error into a process panic. The builder now returns `NetDbError::NoModelsEnabled` explicitly. `NetDb::snapshot()`'s sequential per-adapter capture documents its lack of a cross-model barrier so watchers snapshotting between event deliveries know to coordinate ordering. The `changes_tx.send(seq)` edge-trigger broadcast moved inside the state write-lock block so subscribers treating the seq value as authoritative ordering can't observe out-of-order seqs under contention.

The CLI-side fixes touch every operator-facing path. The `restore` safety gate no longer treats I/O errors as "store is empty" and lets `restore` overwrite a populated dir without `--force`. The `--origin` flag now requires an explicit value (or `--allow-origin-zero`) so a stray missing flag can't silently fold against the wrong chain. Snapshot writes are atomic (`tmp.<pid>` → `fsync` → `rename` → `fsync` parent) so a crash mid-write doesn't truncate the operator's previous snapshot. Operator seed files are created with `OpenOptions::mode(0o600).create_new(true)` so the seed never hits disk world-readable. The Windows strict-permissions path warns unconditionally on reads without `--insecure-permissions` instead of silently no-oping the gate the module-header doc promises. `--force restore` is now scoped to the *replace* semantic the verb name implies, not the silent-merge it was doing before. The ICE confirm prompt uses `tokio::io::stdin` so a blocking read doesn't park a Tokio worker thread for as long as the operator stares at the gate. The `netdb` subcommand finally honors `--config` / `--profile` (it was the only top-level that ignored both, so an operator with `netdb = "/srv/netdb"` in their profile would silently land in the default XDG path and write mutations into the wrong store).

The pass-2 substrate items hit the SDK and bindings: a thundering-herd retry jitter source that was contributing ~0 ns of entropy now seeds from a process-epoch `Instant`; three `u32 → u8` truncation bugs in the Go compute-ffi spawn paths (replica count 300 was silently becoming 44) now mirror the `scale_to` validation that already existed alongside; the tasks/memories adapter fetch-add-then-ingest path either re-instates the rollback or rewrites the contract docs to acknowledge the gap; the `set_local_capabilities` lost-update race between `fetch_add` and the subsequent `store(version)` is corrected so the capability version monotonically advances; the loadbalance `connections.fetch_sub(1)` underflow that silently removed endpoints from rotation forever now saturates.

A handful of items reached "obsolete" rather than "fixed" — re-reading the code showed the audit had misread the contract (the `has_more` cursor advances correctly via `last_seen_seq`, `PyNetDb::open`'s `make_runtime()` is already a process-wide `OnceLock<Arc<Runtime>>` singleton so multiple adapters share one runtime, `next_seq()` already takes the state lock per the existing docstring, `changes_tx.send` runs sequentially from the spawn task's loop so the ordering already matches the watermark ordering). The audit doc records them under **Obsolete** so a future reader knows the agents looked at those call sites and the contract was already correct.

The watcher work itself follows this release. The substrate beneath it is now clean.

---

## Test hygiene

- **Lib suite at 3115+ tests** (was 2715+ at v0.17 release). 400+ net new tests across the Deck TUI snapshot suite + the cross-language SDK surfaces + the Net CLI exit-code / help-text regression suite + the substrate-side ICE / operator-registry / blast-radius simulation tests + the action-chain `MeshOsSnapshot` postcard / JSON forward-compat regression layer.
- **`cargo clippy --features meshos,deck --all-targets -- -D warnings` clean** across substrate + every binding crate + the deck demo + the deck TUI + the net CLI.
- **`cargo doc --features meshos,deck --no-deps` clean under `RUSTDOCFLAGS="-D warnings"`** — every public item in the v0.18 surface carries a doc comment; intra-doc links resolve through the public re-exports.
- **CI matrix expanded.** The Go CI step builds `net-deck-ffi` alongside the existing `net-compute-ffi`, `net-meshdb-ffi`, `net-meshos-ffi` cdylibs so the new `go/deck.go` cgo block links. The Python CI step builds with `meshdb` enabled so `test_meshdb.py` and the new `test_stub_drift.py` MeshDB class coverage exercise on every run.
- **86 help-text snapshot tests** pin every Net CLI subcommand's `--help` output. **14 deck-pipeline integration tests** cover the substrate-end-to-end behavior the TUI relies on (`MeshOsSnapshot` publish → `MeshOsSnapshotFold` consume → MeshDB `Latest` query → TUI render). **11 exit-code tests** cover the documented Net CLI exit-code contract through a real substrate boot.

---

## Breaking changes

### Crate-level default features

`ai2070-net`'s `default = [...]` moves from `[]` to `["net", "nat-traversal", "cortex", "meshdb", "meshos", "dataforts"]`. Existing Rust consumers who add the crate without a `--no-default-features` flag will compile a larger feature surface and pull additional transitive deps (chacha20poly1305, snow, ed25519-dalek, x25519-dalek, blake3, postcard, tokio-stream, async-trait). No code breakage — every default-activated feature is stable; consumers paying for the previous minimal surface should `--no-default-features` and re-add what they need.

### Workspace — new members

`cli/` (Net CLI binary) and `bindings/go/deck-ffi/` (Deck cdylib) are new workspace members. Existing build invocations are unaffected; consumers who `cargo build` without `-p <name>` will see the new members compiled by default. `cargo build --workspace --release` now produces five cdylibs (`libnet`, `libnet_compute`, `libnet_meshdb`, `libnet_meshos`, `libnet_deck`) and one new bin (`net`).

### `MeshOsSnapshot` wire format

The snapshot's postcard wire format gains a `wire_version: u8` prefix that the `MeshOsSnapshotFold`'s decoder checks before postcard dispatch. Existing consumers reading raw postcard bytes need to strip the version byte before decode; consumers using `MeshOsSnapshotReader::read()` are unaffected (the reader returns the decoded struct). Regression tests pin a captured legacy byte string so accidental field reorders trip CI.

### Deck SDK signing payload

ICE-commit signing payloads now carry a domain-separation prefix (`b"net-deck-ice:"`), a substrate-side nonce, and a one-minute commit-window expiry. Operators who had cached a signed payload from a prior version must re-sign — the substrate verifies the prefix + expiry before accepting the signature. The substrate-side bump is invisible to operators using the SDK's `simulate()` → `commit(signatures)` typestate; only consumers who hand-rolled the signing-payload bytes need to update.

### Python `MeshOsDaemonSdk.start` signature

The MeshOS SDK plan documented a `callback_timeout_ms` parameter on `MeshOsDaemonSdk.start`; the runtime never accepted it (the original stub was wrong — see the cross-language review). The stub now matches the runtime: `start(config: Optional[dict] = None, *, control_capacity: Optional[int] = None)`. Python consumers passing `callback_timeout_ms` to `start()` will see a `TypeError`; remove the argument.

---

## How to upgrade

1. **Bump your `Cargo.toml` / `package.json` / `requirements.txt` / `go.mod` to the v0.18 line.** Rust consumers who want the prior minimal default-feature set add `default-features = false` to their `ai2070-net` dependency and re-list the features they need; everyone else gets the full operator stack out of the box.
2. **Operators.** Install the Net CLI via `cargo install --path cli` (workspace-relative) or your distro's release artifact. Generate an operator identity with `net identity generate <name>`, install the public key into the cluster's operator registry, and start running admin / ICE / audit commands. Run `net --help` for the full subcommand map.
3. **Deck.** Build with `cargo build --release -p deck` (binary at `target/release/deck`). Configure the deck connection target via `~/.config/net/deck.toml` or the `--cluster` flag. Run `deck` from the operator workstation — the TUI auto-discovers reachable maintenance nodes and renders the cluster live. Press `?` in any tab for context-aware help.
4. **Daemon authors.** Pick your language:
   - **Rust**: `MeshOsDaemonSdk::start(...)` returning `MeshOsDaemonHandle` — see `sdk/src/meshos/`.
   - **Python**: `pip install ai2070-net-sdk` (or build from source with `--features meshos`); implement the `MeshOsDaemon` Protocol and register via `MeshOsDaemonSdk.start().register_daemon(daemon, identity)`.
   - **TypeScript**: `npm install @ai2070/net-sdk`; implement the daemon object shape (`name`, `process`, optional `snapshot`/`restore`/`onControl`/`health`/`saturation`) and pass it to `registerDaemon`.
   - **Go**: `import "github.com/ai-2070/net/bindings/go/net"`; implement the `MeshOsDaemon` interface and call `meshos.RegisterDaemon(daemon, identity)`.
   - **C**: `#include <net_meshos.h>`; populate a `NetMeshOsDaemonVtable` and call `net_meshos_register_daemon(...)`. Link against `libnet_meshos`.
5. **Operator-tooling authors.** Same per-language path with the Deck SDK: `DeckClient::new(operator_identity, config)` (Rust) / `DeckClient.from_seed(seed)` (Python) / `new DeckClient({operatorSeed, ...})` (Node) / `net.NewDeckClient(seed, config)` (Go) / `net_deck_client_new(...)` (C). Drive `.admin()` for signed commits, `.ice()` for break-glass, `.audit()` for the admin-event ledger.
6. **ICE workflow.** Every break-glass operator runs through `simulate()` → `commit(signatures)`. Build a proposal with `client.ice().freeze_cluster(reason, ttl)`; call `proposal.simulate().await` to get a `SimulatedIceProposal` carrying the blast-radius projection; collect operator signatures over `simulated.signing_payload()`; call `simulated.commit(signatures).await`. The substrate enforces simulate-before-commit; commit without a fresh simulation returns `consumed`.
7. **MeshOS daemon trait additions.** If you implement `MeshDaemon` and want supervision participation, override `health()` / `saturation()` / `on_control(DaemonControl)`. Defaults preserve compatibility from v0.17.
8. **NetDB from Go.** Go consumers who previously opened `OpenTasksAdapter` + `OpenMemoriesAdapter` separately can now use `OpenNetDb(redex, NetDbConfig{...})` for the cross-adapter façade + snapshot bundle that round-trips across every binding. See `bindings/go/net/netdb.go`.
9. **C ABI consumers.** Migrate `#include "net.go.h"` callsites to the per-surface headers (`net_cortex.h` for RedEX/CortEX/NetDb, `net_rpc.h` for RPC, `net_meshdb.h` for MeshDB, `net_meshos.h` for MeshOS, `net_deck.h` for Deck). The convenience `net.go.h` `#include`s `net_cortex.h` automatically; existing consumers compile unchanged.

---

Released 2026-05-17.

## License

See [LICENSE](../../LICENSE-APACHE).
