# Deck remote-attach + Scale RPC

Branch (proposed): `deck-cli-tabs` (continuation).
Predecessor: `docs/plans/AGGREGATOR_CLI_REMOTE_ATTACH_AND_SCALE_RPC.md` — all A-1..A-6 + B-1..B-6 slices landed, plus task #102 (post-start direct handshake). The wire surface (`RegistryClient::list/spawn/unregister/scale`, `FoldQueryClient`) and the CLI's `CliContext::build_with_remote` pattern are live and proven by `cli/tests/aggregator_remote.rs`.
Scope: lift the operator's TUI from a local-only viewer into a remote-attached operator console. Specifically:

1. **Deck remote bootstrap** — Deck spawns a connected `Arc<MeshNode>` against a remote `aggregator-daemon` using the same `Mesh::connect_via` routed-handshake pattern the CLI proved. Today `runtime::spawn` is in-process only (`deck/src/runtime.rs:55-92`).
2. **Live AGGREGATORS panel** — currently reads `DeckClient::aggregator_snapshot()` (local AggregatorDaemon, almost always None in operator deployments). Flip to a `RegistryClient::list()` poller against the attached daemon, rendering per-group `source_subnet` / `fold_kinds` / per-replica health from the wire-side `RegistryGroupSummary` (the fields B-2 added).
3. **Write surface** — operator-driven `[s]pawn` / `[S]cale` / `[u]nregister` from the AGGREGATORS tab with the existing modal pattern (`Modal::ParamInput` + `Modal::Confirm`) extended for multi-field input.
4. **Cluster picker integration** — `bookmarks.rs` + the `:` picker today are scaffolding that toasts "substrate RPC slice required" when a remote is selected (`app.rs:1440-1448`). Wire the real attach path.

Tagged `[D | E]`:

- D — Deck remote-attach + write surface
- E — Closeout (smoke / docs / tidy)

---

## Status

| ID    | Pri | Area                         | Title                                                                                          |
|-------|-----|------------------------------|------------------------------------------------------------------------------------------------|
| D-1   | H   | bootstrap                    | `DeckBackend` abstraction: `Local` (in-process SDK) vs `Remote` (`Arc<MeshNode>` + targeted daemon) |
| D-2   | H   | bookmarks                    | Extend `Bookmark` schema with `psk_hex` / `node_pubkey` / `node_id` + version bump              |
| D-3   | H   | picker                       | `commit_cluster_pick` flips from toast to live attach; `--attach <NAME>` / `--endpoint` CLI flags |
| D-4   | H   | polling                      | `RegistrySnapshotTail` — background `RegistryClient::list` poller at ~1Hz cadence              |
| D-5   | H   | render                       | AGGREGATORS panel renders remote `RegistryGroupSummary` (groups + per-replica health)           |
| D-6   | M   | status                       | Header + footer chips showing attached / connecting / stale / detached states                  |
| D-7   | H   | write (spawn)                | `[s]` opens multi-field input modal → `RegistryClient::spawn` dispatch + toast                 |
| D-8   | H   | write (scale)                | `[S]` opens scale modal → `RegistryClient::scale` dispatch + toast                             |
| D-9   | H   | write (unregister)           | `[u]` → two-step confirm modal → `RegistryClient::unregister` dispatch + toast                 |
| D-10  | M   | subnets                      | SUBNETS `AGG` column folds in remote `RegistryGroupSummary.source_subnet`                       |
| D-11  | M   | failure                      | Reconnect strategy + stale-data indicator + error-toast throttle                                |
| D-12  | L   | feature gate                 | `--features remote-attach` gate for first ship; default-on once D-11 settles                   |
| E-1   | L   | docs + tidy                  | `DECK_PLAN.md` § Deferred → § Closed; help overlay copy; tab footer chips                       |

---

## Phase 1 — Bootstrap

### D-1 — `DeckBackend` abstraction + remote-attach plumbing

**Why this slice first.** Every D-* read/write slice needs a way to ask "is the deck attached to a remote daemon, and if so what's its `node_id` + which `Arc<MeshNode>` do I route through?" Today the deck has only `App::deck: Arc<DeckClient>` against an in-process SDK (`app.rs:169`, `runtime.rs:84`). The CLI solved this by adding `mesh_node: Option<Arc<MeshNode>>` to `CliContext` (`cli/src/context.rs:57`); the deck needs a richer shape because (a) the connection persists for the session lifetime (CLI is one-shot), (b) the operator can switch clusters mid-session via the picker, and (c) the local fallback still has to work for read-only viewing of the in-process SDK.

**Backend shape.**

```rust
// deck/src/backend.rs (new)
pub enum DeckBackend {
    /// Today's path — in-process MeshOsRuntime + DeckClient.
    /// No MeshNode plumbed. AGGREGATORS reads local
    /// DeckClient::aggregator_snapshot() (almost always None
    /// in operator deployments).
    Local {
        deck: Arc<DeckClient>,
        this_node: NodeId,
    },
    /// New path — attached to a remote aggregator-daemon via
    /// routed handshake. Local SDK still spun so existing
    /// MeshOsSnapshot fields keep returning (peers, daemons,
    /// etc. populated by the local probe; the remote daemon
    /// is the source of truth for AGGREGATORS / SUBNET AGG).
    Remote {
        deck: Arc<DeckClient>,           // local — for non-aggregator panels
        this_node: NodeId,
        mesh: Arc<Mesh>,                  // held to keep socket + dispatch loop alive
        mesh_node: Arc<MeshNode>,         // for RegistryClient / FoldQueryClient
        daemon_node_id: u64,              // remote target
        daemon_label: String,             // bookmark name or "ad-hoc"
    },
}

impl DeckBackend {
    pub fn deck(&self) -> &Arc<DeckClient> { ... }
    pub fn this_node(&self) -> NodeId { ... }
    pub fn mesh_node(&self) -> Option<&Arc<MeshNode>> { ... }
    pub fn daemon_node_id(&self) -> Option<u64> { ... }
    pub fn is_remote(&self) -> bool { ... }
    pub fn label(&self) -> &str { ... }   // "local" or bookmark name
}
```

**`App` field swap.** `App::deck: Arc<DeckClient>` becomes `App::backend: DeckBackend`. Every read site that uses `self.deck.foo()` becomes `self.backend.deck().foo()` — mechanical refactor. `this_node` and the per-snapshot field stay on `App` (the snapshot is still local-SDK-sourced).

**`Mesh`'s lifetime.** The CLI drops its `Mesh` at end-of-subcommand (`context.rs:61`). The Deck session is long-lived, so `DeckBackend::Remote` owns the `Arc<Mesh>` for the session and the receive loop runs the whole time. A picker switch (D-3) builds a new `Mesh`, points the backend at it, and drops the old one — which closes the prior socket + dispatch loop deterministically.

**Bootstrap entry points.**

- `runtime::spawn_local()` — today's `runtime::spawn` (rename for clarity). Returns `DeckBackend::Local`.
- `runtime::spawn_remote(attach: RemoteAttach) -> Result<DeckBackend, RemoteAttachError>` — new. Lift `cli/src/context.rs::build_remote_mesh` (`:211-230`) into a shared helper in `crates/net/sdk/src/aggregator.rs` (or a new `crates/net/sdk/src/remote_attach.rs`) so both CLI and Deck consume it. The helper takes `RemoteAttach { addr, public_key, node_id, psk }` and returns `Arc<Mesh>` + `Arc<MeshNode>`. CLI's `RemoteAttach` type also lifts into the SDK (currently `cli`-private).

**`RemoteAttach` lifted to SDK.** New `net_sdk::remote_attach::RemoteAttach` (re-export the existing CLI struct). Single source of truth for the field set (addr / public_key / node_id / psk).

**Files touched (D-1).**
- New `crates/net/sdk/src/remote_attach.rs` — `RemoteAttach` struct + `connect_remote(...)` helper. Lift the body of `cli/src/context.rs::build_remote_mesh`.
- `crates/net/cli/src/context.rs` — re-export `RemoteAttach` from SDK; delete local copy.
- New `crates/net/deck/src/backend.rs` — `DeckBackend` enum + accessors.
- `crates/net/deck/src/runtime.rs` — rename `spawn` → `spawn_local`; add `spawn_remote(attach)`.
- `crates/net/deck/src/app.rs` — replace `App::deck` with `App::backend: DeckBackend`; mechanical accessor refactor through ~40 call sites (rg shows ~38 `self.deck.` references).
- `crates/net/deck/src/main.rs` — branch on a new `DeckArgs` (clap-derived) that supports `--endpoint mesh://...` / `--attach <bookmark-name>` / `--local`.

**Test plan (D-1).**
- `deck/tests/backend_local_construction.rs` — `DeckBackend::Local` constructs without a remote target; `mesh_node()` returns `None`; `deck()` returns the in-process `DeckClient`.
- `deck/tests/backend_remote_construction.rs` — spin an `aggregator-daemon` in-process (mirror `cli/tests/aggregator_remote.rs:71-148`); call `runtime::spawn_remote(attach)`; assert `mesh_node().is_some()` + `daemon_node_id() == Some(daemon_id)`.
- Unit-test the App-side mechanical refactor by running the existing Deck test suite — render harness tests in `deck/src/tabs/*` that fixture a backend prove no regression.

### D-2 — Bookmark schema extension + version bump

Today `Bookmark` carries `name` / `endpoint` / `default_identity` / `pinned` (`bookmarks.rs:44-61`). The `endpoint` field is opaque (`mesh://0xa96f@10.0.0.7:9001`) — never parsed. To actually attach, we need PSK, public key, and the daemon's node_id as separate fields (parsing them out of a single string is fragile and locks the format).

**Proposed extension.**

```rust
pub struct Bookmark {
    pub name: String,
    /// Display-only; the structured fields below are what we
    /// actually connect with. Kept for back-compat with TOMLs
    /// written before the version bump — when the structured
    /// fields are absent we try to parse them from this string
    /// (best-effort, surfaces a load warning).
    pub endpoint: String,
    pub default_identity: Option<String>,
    pub pinned: bool,
    /// NEW (v2). When all four are set, the bookmark is
    /// remote-attach-ready.
    pub addr: Option<String>,            // "10.0.0.7:9001"
    pub node_pubkey_hex: Option<String>, // 64-char lowercase hex
    pub node_id: Option<String>,         // "0x42" or "66"
    pub psk_hex: Option<String>,         // 64-char lowercase hex
}
```

**Version bump.** Bump `CURRENT_VERSION` from 1 → 2. v1 files load with a migration step: leave the new fields `None` so old bookmarks render in the picker but show "(incomplete — edit bookmarks.toml)" when selected. Operator can either edit the file or run a new `net deck bookmark add` CLI subcommand (out of scope here; surface in E-1 as deferred).

**`is_remote_attachable(&self) -> bool`** helper — true when all four new fields are `Some`. Picker uses this to grey out incomplete entries.

**Validation.** `BookmarkStore::upsert` validates the structured fields when present: pubkey + psk are 32-byte hex, `addr` parses as `SocketAddr`, `node_id` parses via `parse_u64_flexible`. Returns `BookmarkError::InvalidField` early so a malformed entry doesn't reach attach time.

**Files touched (D-2).**
- `crates/net/deck/src/bookmarks.rs` — schema extension; bump to v2; migration on v1 load (`load_from` accepts both); validation in `upsert`.
- Tests inline in the same file (existing test pattern).

**Test plan (D-2).**
- `bookmarks::tests::v1_loads_with_empty_attach_fields` — write a v1 file (current shape); load via `load_from`; assert `addr`/`node_pubkey_hex`/`node_id`/`psk_hex` are all `None`.
- `bookmarks::tests::v2_round_trip_with_attach_fields` — upsert a complete v2 bookmark; save; reload; assert all fields round-trip.
- `bookmarks::tests::upsert_rejects_bad_pubkey_hex` — operator-edited TOML with a 30-char pubkey rejected.
- `bookmarks::tests::is_remote_attachable_requires_all_four` — only when every structured field is `Some`.

### D-3 — Picker commits real attach; CLI flags

`commit_cluster_pick` (`app.rs:1426-1449`) today toasts "remote cluster '...' — substrate RPC slice required" instead of attaching. Replace with the live path.

**New `commit_cluster_pick` behavior.**
- Cursor 0 (`local`): if currently remote, tear down by rebuilding `DeckBackend::Local`. Toast `"switched to local cluster"`.
- Cursor > 0 (bookmark): resolve to a `Bookmark`. If `!is_remote_attachable()`, toast `"bookmark '{name}' missing attach fields — edit bookmarks.toml"`. Otherwise, spawn a tokio task that calls `runtime::spawn_remote(attach)`; on success, swap `App::backend` to the new `DeckBackend::Remote`, set `App::active_cluster = bm.name`, toast `"attached to '{name}' (daemon 0x{id:x})"`. On failure (handshake / PSK / addr unreachable), toast `"attach failed: {err}"` and leave the active backend untouched.

**Cross-thread mutation.** The picker's commit runs on the App's main thread; the `spawn_remote` call is async. Use the existing `toast_tx`/`pending_admin` pattern: detach a tokio task that builds the backend, then push the result back through a `mpsc::Sender<DeckBackend>` the tick loop drains alongside `toast_rx` (mirror `drain_toast_channel`, `app.rs:659-667`). New `App::backend_rx: mpsc::Receiver<BackendUpdate>` where `BackendUpdate` is `Result<DeckBackend, RemoteAttachError>`. The drain step swaps the backend in on success; toast on failure.

**Picker UI tweaks.** Cluster picker (`widgets/cluster_picker.rs`) gets a per-row indicator: `◀ active` for the currently-bound cluster, `(incomplete)` for non-attachable bookmarks. Lights up "(connecting...)" while a swap is in flight (gated on a new `App::picker_in_flight: bool`).

**CLI flags on `net-deck`.**

```
net-deck [--endpoint <ENDPOINT> | --attach <BOOKMARK_NAME> | --local]
         [--node-addr <IP:PORT> --node-pubkey <HEX> --node-id <N> --psk-hex <HEX>]
```

- `--local` (default): in-process backend, today's path.
- `--attach <name>`: load bookmark, attach via its structured fields. Errors at startup if not attachable.
- `--node-addr / --node-pubkey / --node-id / --psk-hex`: ad-hoc attach without persisting a bookmark. Useful for `--print-bootstrap` paste-in.
- `--endpoint <s>`: future deep-link parser; deferred (toast "endpoint parsing deferred — use --attach or the four-flag form").

**Files touched (D-3).**
- `crates/net/deck/src/app.rs` — `commit_cluster_pick` rewrite; `backend_rx` field + `drain_backend_channel`; render-side `picker_in_flight` hookup.
- `crates/net/deck/src/widgets/cluster_picker.rs` — per-row state styling.
- `crates/net/deck/src/main.rs` — `DeckArgs` (clap); branch the bootstrap accordingly.
- `crates/net/deck/Cargo.toml` — add `clap` (already a transitive dep — verify before adding).

**Test plan (D-3).**
- `deck/tests/picker_attach_round_trip.rs` — spin a daemon, write a bookmarks.toml with its bootstrap fields, launch the App harness, simulate `:` + cursor-down + Enter, assert backend swaps to `Remote` and toast confirms.
- `deck/tests/picker_attach_failure_toasts.rs` — bookmark points at a closed port; attach fails; assert toast text + backend stays `Local`.
- `deck/tests/cli_args_attach_at_startup.rs` — invoke `net-deck --attach prod` with a populated bookmarks file; assert App boots into `Remote`.

---

## Phase 2 — Live read

### D-4 — `RegistrySnapshotTail` background poller

Today the snapshot tick (`App::refresh_snapshot`, `app.rs:652-654`) runs every 120ms (`tick_rate`, line 629) and reads the local `DeckClient::status()`. For remote registry data we need a **separate** background poll because:

- `RegistryClient::list()` is a wire RPC with a deadline (default 2s per `RegistryClient` defaults). 120ms is too aggressive — every tick would queue a fresh RPC against the daemon's dispatcher.
- The data is lower-velocity (groups don't spin up/down at frame rate).
- A blocking poll on the render thread would freeze the UI on RPC stalls.

**Polling cadence.** 1Hz default. Operators triage on the order of seconds, not frames. Configurable via `--registry-poll-ms <N>` (clamped 250..30_000).

**Refresh on key press.** When the operator opens the AGGREGATORS tab (`B` keystroke) or refocuses to it via Tab cycling, a `force_refresh()` call kicks an immediate poll regardless of cadence — mirrors the operator's intuition that "switching tabs shows fresh data".

**Tail shape.**

```rust
// deck/src/streams.rs additions (alongside LogsTail, AuditTail, …)
pub struct RegistrySnapshotTail {
    inner: Arc<Mutex<RegistryTailState>>,
}

struct RegistryTailState {
    /// Latest successful snapshot. `None` until the first poll
    /// returns; render path shows "polling..." in that window.
    latest: Option<Vec<RegistryGroupSummary>>,
    /// Wall-clock of the latest successful poll. Drives the
    /// stale-data indicator (D-11).
    fetched_at: Option<Instant>,
    /// Latest error (if any). Cleared on next success.
    last_error: Option<String>,
    /// Set by force_refresh() — background task wakes early.
    force_kick: bool,
}

impl RegistrySnapshotTail {
    pub fn new() -> Self { ... }
    pub fn snapshot(&self) -> Option<Vec<RegistryGroupSummary>> { ... }
    pub fn fetched_at(&self) -> Option<Instant> { ... }
    pub fn last_error(&self) -> Option<String> { ... }
    pub fn force_refresh(&self);
}

pub fn spawn_registry_poll(
    mesh: Arc<MeshNode>,
    daemon_node_id: u64,
    tail: RegistrySnapshotTail,
    cadence: Duration,
) -> tokio::task::JoinHandle<()>;
```

The spawn function takes `Arc<MeshNode>` (not the whole backend) so a backend swap drops the task (the `Arc<MeshNode>` it captured is no longer the active one, but the task only outputs to the captured `RegistrySnapshotTail`, which gets re-created on swap). Backend swaps (D-3) cancel the old `JoinHandle` and start a fresh one.

**Coupling to backend.** The poller is alive only when the backend is `Remote`. Local mode skips spawning it; `RegistrySnapshotTail::snapshot()` returns `None` and the AGGREGATORS panel falls back to the local `DeckClient::aggregator_snapshot()` (today's behavior).

**Files touched (D-4).**
- `crates/net/deck/src/streams.rs` — `RegistrySnapshotTail` + `spawn_registry_poll`.
- `crates/net/deck/src/app.rs` — `App::registry_tail: RegistrySnapshotTail`; spawn the poll task in `runtime::spawn_remote`'s caller side; cancel on backend swap.
- `crates/net/deck/src/main.rs` — wire the cadence flag.

**Test plan (D-4).**
- `deck/tests/registry_tail_polls_at_cadence.rs` — spin daemon with 2 groups; spawn poll at 100ms; assert tail snapshot populates within 200ms; pulse `force_refresh()`; assert observation count increases.
- `deck/tests/registry_tail_surfaces_rpc_errors.rs` — point the poller at a closed daemon; assert `last_error()` populates within one cadence.
- `deck/tests/registry_tail_recovers_after_transient_error.rs` — restart the daemon mid-poll; assert `last_error` clears + a fresh `snapshot` lands.

### D-5 — AGGREGATORS panel renders remote `RegistryGroupSummary`

Today the panel reads `DeckClient::aggregator_snapshot()` → `AggregatorSnapshot { fold_kinds, source_subnet, summaries, ... }` (`tabs/aggregators.rs:29-37`). That shape is the **summarizer's output**, not the registry's view of groups + replicas. The two are different abstractions:

- `AggregatorSnapshot`: what a *single* aggregator has summarized — one fold_kind per row, source_subnet, buckets.
- `RegistryGroupSummary`: what groups are registered with the daemon — one row per group, with per-replica health + generation + placement.

Operators on a remote-attached deck want the registry view: "what groups exist, are they healthy, who are the replicas, what subnet/folds do they cover."

**New panel shape (remote mode).**

| Column | Source | Note |
|--------|--------|------|
| `▶`  | cursor | |
| NAME | `summary.name` | group key |
| SUBNET | `summary.source_subnet` | dotted form |
| FOLDS | `summary.fold_kinds` | comma-joined `0xNNNN` |
| REPLICAS | `summary.replicas.len()` | live count |
| HEALTH | `replicas.iter().filter(|r| r.healthy).count()` / total | e.g. `2/3` (green when ==, amber when partial, red when 0) |
| MAX_GEN | `replicas.iter().map(|r| r.generation).max()` | newest-replica tick |
| PLACEMENT | summarized `placement_node_id`s | hex list; `—` for unplaced |

**Detail pane** when a group is cursored: vertical layout showing each replica's `generation`, `healthy` bool, `diagnostic` text, `placement_node_id`. Mirrors the per-row detail pattern from the DAEMONS tab.

**Local-mode fallback.** When `App::backend.is_remote() == false`, render today's `AggregatorSnapshot` view via the existing path. Branch in `tabs/aggregators.rs::render`.

**Newest-first.** Registry returns groups alphabetically (current shape); preserve that for stable navigation. No newest-first reversal (operators bookmark groups by name; a stable order matters more than recency).

**Empty states.**
- Remote, poller has no data yet: "querying daemon..." with spinner glyph.
- Remote, poller returned empty list: "no groups registered on daemon 0x{id:x}".
- Remote, last poll errored: "registry poll failed: {err}" in red.
- Local, no AggregatorDaemon wired: today's empty state (`render_empty`).

**Files touched (D-5).**
- `crates/net/deck/src/tabs/aggregators.rs` — rewrite `render`; branch on `backend.is_remote()`; introduce `render_remote_table` + keep `render_local_table` as the existing path (rename today's `render_table`).
- `crates/net/deck/src/app.rs` — the dispatch site (`:3389-3390`) passes the backend + tail in addition to the deck.

**Test plan (D-5).**
- `tabs/aggregators::tests::renders_remote_groups_with_per_replica_health` — fixture a `RegistrySnapshotTail` with 2 groups (one 2/3 healthy, one 3/3), assert ratatui buffer contains `2/3` in amber + `3/3` in green.
- `tabs/aggregators::tests::renders_local_snapshot_when_backend_is_local` — backend = Local, AggregatorSnapshot fixtured; assert today's columns render.
- `tabs/aggregators::tests::renders_polling_state_before_first_snapshot` — backend = Remote, tail empty; assert "querying daemon..." present.

### D-6 — Status indicator (header / footer chips)

Operators need to know **at a glance** whether the deck is in local or remote mode, which cluster it's attached to, and whether data is stale. Today the active cluster is tracked in `App::active_cluster: String` (default `"local"`) but the visual indication is only inside the `:` picker modal.

**Header chip.** Add a chip to the right of the tab strip (top bar) — same row as the tab labels. Format:

- Local: `[ LOCAL ]` in dim grey.
- Remote attached: `[ ▶ prod-east  0xa96f ]` in green-hi (active cluster name + daemon id short form).
- Connecting: `[ ⟳ prod-east ... ]` in amber.
- Stale (> 5× poll cadence since last successful poll): `[ ▶ prod-east  STALE ]` in amber.
- Errored (last poll failed + no recovery): `[ ✗ prod-east  ERR ]` in red.

**Footer reminder.** Footer (per-tab chips, `widgets/footer.rs:222`) gets a "remote" or "local" mode suffix on the AGGREGATORS tab specifically — that's the tab where the difference matters most.

**Files touched (D-6).**
- `crates/net/deck/src/widgets/tab_bar.rs` — add a right-aligned status chip in `render`.
- `crates/net/deck/src/widgets/footer.rs` — append mode chip for AGGREGATORS.
- `crates/net/deck/src/app.rs` — `App::backend_status()` accessor returning one of `Local | Connecting | Attached | Stale | Errored` for the chip to map.

**Test plan (D-6).**
- Ratatui snapshot tests on `widgets/tab_bar` — one buffer assertion per status state.
- Manual smoke (deferred to E-1).

---

## Phase 3 — Write surface

### D-7 — `[s]pawn` group on AGGREGATORS tab

Today AGGREGATORS is read-only (footer comment: `// Read-only today. ... aggregators a summary detail card.`, `widgets/footer.rs:222-225`). Add the spawn write path.

**Modal flow.** Existing single-field `Modal::ParamInput` (used for durations like drain windows / freeze TTLs) doesn't fit — spawn needs three fields (template, name, replica_count). Two options:

1. **Sequential single-field modals.** First prompt: "template?" → on Enter, transition to "name?" modal → on Enter, transition to "replica count?" modal → on Enter, build a `Confirm` action. Three modal transitions, but reuses the existing `ParamInput` machinery verbatim.
2. **New multi-field input modal.** Single screen with three labeled inputs + `Tab` to walk between fields. More UX-friendly; net-new widget.

**Recommend option 2** — operators muscle-memory tab/enter through forms; sequential single-fields feel sluggish. New widget `widgets/multi_input.rs`:

```rust
#[derive(Clone, Debug)]
pub struct MultiInputModal {
    pub purpose: MultiInputPurpose,
    pub fields: Vec<MultiInputField>,
    pub cursor: usize,            // which field is active
    pub error: Option<String>,    // last validation failure
}

pub struct MultiInputField {
    pub label: &'static str,
    pub hint: &'static str,
    pub buffer: String,
    pub validator: ValidatorKind,  // Hex32, U8Positive, NonEmptyAscii, ...
}

pub enum MultiInputPurpose {
    AggregatorSpawn,
    AggregatorScale { group_name: String, current_replicas: u8 },
}
```

Bindings inside the modal: `Tab`/`Shift+Tab` next/prev field, characters append to active field, `Enter` validates all + transitions to `Modal::Confirm`, `Esc` dismisses.

**`ConfirmAction` variants.**

```rust
ConfirmAction::AggregatorSpawn {
    daemon_id: u64,
    daemon_label: String,
    template: String,
    name: String,
    replica_count: u8,
}
```

Headline: `"spawn aggregator group '{name}' on {daemon_label} · template {template} · {replica_count} replicas"`.

**Dispatch.** On `Confirm` → detach a tokio task (matches `dispatch_ice` shape, `app.rs:463-484`):

```rust
async fn dispatch_aggregator_spawn(
    mesh: Arc<MeshNode>,
    daemon_id: u64,
    template: String,
    name: String,
    replica_count: u8,
    toast_tx: mpsc::Sender<String>,
    registry_tail: RegistrySnapshotTail,
) {
    let client = RegistryClient::new(mesh);
    match client.spawn(daemon_id, name.clone(), template, replica_count).await {
        Ok(summary) => {
            let _ = toast_tx.send(format!(
                "spawned '{}' · {} replicas", summary.name, summary.replicas.len()
            ));
            registry_tail.force_refresh();
        }
        Err(err) => {
            let _ = toast_tx.send(format!("spawn failed: {err}"));
        }
    }
}
```

`registry_tail.force_refresh()` kicks an immediate re-poll so the new group appears in the panel without waiting a full cadence.

**Key binding.** `[s]` on AGGREGATORS (lowercase — matches existing per-tab action conventions). Only active when backend is `Remote` (toast a hint otherwise).

**Files touched (D-7).**
- New `crates/net/deck/src/widgets/multi_input.rs`.
- `crates/net/deck/src/widgets/confirm.rs` — `AggregatorSpawn` variant + headline / lines / styling.
- `crates/net/deck/src/app.rs` — `Modal::MultiInput` variant; keyboard absorber; dispatch function; `[s]` keypress handler under `Tab::Aggregators`; `on_modal_key` extension for multi-input.

**Test plan (D-7).**
- `deck/tests/aggregator_spawn_round_trip.rs` — spin daemon with a `[[template]]` block; launch App in Remote backend; simulate `[s]` + fill three fields + Enter on confirm; assert (a) toast text indicates success, (b) `registry_tail.snapshot()` reflects the new group within 200ms (via `force_refresh`).
- `deck/tests/aggregator_spawn_failure_toasts.rs` — use a non-existent template name; assert toast contains `UnknownTemplate`.
- `widgets/multi_input::tests::tab_walks_field_cursor` — pure widget test, no network.

### D-8 — `[S]cale` group

Same modal pattern as spawn but pre-populated from the cursored group's `RegistryGroupSummary` (template field doesn't change at scale — the cursored summary doesn't carry the template name because the wire shape only stores `source_subnet` + `fold_kinds`).

**Template re-supply problem.** From `AGGREGATOR_CLI_REMOTE_ATTACH_AND_SCALE_RPC.md:282-289` — the wire `Scale { group_name, template_name, target_replica_count }` requires the operator to re-supply the template. The deck doesn't know it from the snapshot. Two paths:

1. **Operator types it.** Field labeled "template" in the modal; prefilled empty. Annoying — operator just picked a group; why re-type its template? But honest to the wire.
2. **Deck caches template-name in a per-attach side index.** When the operator spawned a group via D-7, store `name → template` locally. Scale reads from cache. New groups (registered by daemon config, not Deck-spawned) miss the cache → fall back to (1).

**Recommend (1) initially** (cleanest). If muscle-memory pain emerges, add (2) in a follow-up.

**Modal fields:**
- `name` — prefilled with cursored group's name, read-only.
- `template` — empty buffer; operator types.
- `target_replicas` — prefilled with the cursored group's current `replicas.len()` as the default; operator edits.

**`ConfirmAction::AggregatorScale { daemon_id, daemon_label, group_name, template, current_replicas, target_replicas }`.** Headline shows the delta: `"scale '{name}' on {daemon_label} · {current} → {target}"`.

**Dispatch.** `RegistryClient::scale(daemon_id, group_name, template, target_replicas)`. On success: toast `"scaled '{name}' · {old}→{new} replicas"` + `force_refresh()`. On error (`UnknownGroup` / `ScaleRejected("template mismatch")`): toast the typed error.

**Files touched (D-8).**
- `crates/net/deck/src/widgets/multi_input.rs` — extend `MultiInputPurpose::AggregatorScale`; read-only field rendering.
- `crates/net/deck/src/widgets/confirm.rs` — `AggregatorScale` variant.
- `crates/net/deck/src/app.rs` — `[S]` handler under `Tab::Aggregators`; dispatch task.

**Test plan (D-8).**
- `deck/tests/aggregator_scale_grow_then_shrink.rs` — spawn group of 2; press `[S]`, type template + `4`; assert daemon-side `list` shows 4 replicas; press `[S]` again, change to `1`; assert 1 replica.
- `deck/tests/aggregator_scale_template_mismatch.rs` — type a wrong template; assert toast contains `template mismatch`.
- `deck/tests/aggregator_scale_zero_rejected.rs` — type `0`; assert validator rejects pre-RPC (no wire call).

### D-9 — `[u]nregister` group (destructive)

Different shape — destructive action needs an extra confirmation gate (same ceremony as ICE break-glass, but routine-action-styled).

**Modal flow.**
1. `[u]` on AGGREGATORS with a cursor → `Modal::Confirm` (existing widget) with `ConfirmAction::AggregatorUnregister { daemon_id, daemon_label, group_name, replica_count }`. Headline: `"unregister '{name}' from {daemon_label} · stops {N} replicas"`.
2. Confirm modal styled with the routine-action chrome (amber, not red — unregister is reversible by re-spawning; it's not a break-glass).
3. Enter dispatches `RegistryClient::unregister`; toast on result; `force_refresh()`.

**Edge case: cursored group disappears mid-dispatch.** If `Unregistered { existed: false }` returns (the group was already removed via another deck / CLI), toast `"group '{name}' was not registered (already removed?)"` rather than treating as a hard error.

**Files touched (D-9).**
- `crates/net/deck/src/widgets/confirm.rs` — `AggregatorUnregister` variant.
- `crates/net/deck/src/app.rs` — `[u]` handler; dispatch task.

**Test plan (D-9).**
- `deck/tests/aggregator_unregister_round_trip.rs` — spawn group; `[u]` + confirm; assert daemon-side `list` no longer contains it.
- `deck/tests/aggregator_unregister_idempotent_double_press.rs` — `[u]` + confirm twice in a row; second call surfaces the `existed: false` toast cleanly.

---

## Phase 4 — Polish

### D-10 — SUBNETS panel `AGG` column folds in remote groups

Today `aggregator_source_subnets` (`app.rs:1352-1364`) reads `DeckClient::aggregator_snapshot()` only — one entry (the local AggregatorDaemon, if any). Almost always empty in operator deployments. The SUBNETS `AGG` column shows `—` for every row.

**Remote fan-in.** When `App::backend.is_remote()`, include every `RegistryGroupSummary.source_subnet` from the registry tail.

```rust
fn aggregator_source_subnets(&self) -> HashSet<SubnetId> {
    let mut out = HashSet::new();
    if let Some(snap) = self.backend.deck().aggregator_snapshot() {
        out.insert(snap.source_subnet);  // local AggregatorDaemon if any
    }
    if let Some(groups) = self.registry_tail.snapshot() {
        for g in groups {
            out.insert(g.source_subnet);
        }
    }
    // Demo fixture path unchanged.
    out
}
```

The SUBNETS render path consumes this set unchanged (`tabs/subnets.rs:42` — `aggregator_subnets: &HashSet<SubnetId>`).

**Files touched (D-10).**
- `crates/net/deck/src/app.rs` — `aggregator_source_subnets` extension.

**Test plan (D-10).**
- `deck/tests/subnets_agg_column_reflects_remote_groups.rs` — fixture `RegistrySnapshotTail` with 2 groups across different subnets; assert SUBNETS `AGG` column reads `yes` for both subnets.

### D-11 — Failure handling: reconnect, stale, error-toast throttle

Three failure modes to handle:

1. **Transient RPC failure** (one poll fails, next succeeds). Already handled by D-4's `last_error` field. Render path shows red error chip in header (D-6); panel keeps showing the last successful snapshot stamped with `fetched_at`.
2. **Daemon disconnect / handshake invalidation** (UDP socket dies, daemon restarts under same addr but with new ephemeral keys, PSK rotation, NAT pinhole expires). The `RegistryClient` calls all time out after deadline; multiple consecutive failures should drive the `Stale` → `Errored` chip state and possibly kick a reconnect.
3. **Network unreachable** (laptop closes lid, VPN drops). Same as (2) from the deck's POV.

**Reconnect strategy.**
- After 3 consecutive poll failures, mark the backend `Errored` and stop the poll task.
- Background reconnect: try to re-establish the `Mesh::connect_via` handshake every 5s for the first minute, every 30s after. Each attempt builds a fresh `Mesh` (the old socket may be in a bad state) and on success, replaces the backend's mesh + mesh_node atomically.
- Operator can manually trigger reconnect with `[r]` on the AGGREGATORS tab when the chip shows `Errored`.

**Error-toast throttle.** Without throttling, a daemon outage produces one toast per poll = ~1 per second (or ~once per modal action) — drowns out everything. Throttle: at most one error toast per 30s per error-class. Sliding window inside `RegistrySnapshotTail` tracks `last_toasted_error: Option<(String, Instant)>`. Only toast when the error class changes or 30s elapsed.

**Stale-data indicator.** When `fetched_at.elapsed() > 5 * cadence`, the AGGREGATORS panel header gets a `(stale Xs ago)` suffix in amber. The data still renders (last known is better than nothing) but the operator sees the freshness explicitly.

**Files touched (D-11).**
- `crates/net/deck/src/streams.rs` — `RegistrySnapshotTail::record_error` with throttle; consecutive-failure counter; reconnect-state field.
- New `crates/net/deck/src/reconnect.rs` — backoff loop.
- `crates/net/deck/src/app.rs` — `[r]` keypress handler on AGGREGATORS when status == `Errored`.
- `crates/net/deck/src/tabs/aggregators.rs` — stale suffix.

**Test plan (D-11).**
- `deck/tests/registry_tail_throttles_error_toasts.rs` — point poller at a closed port; assert at most one toast in a 10s window despite many retries.
- `deck/tests/registry_tail_marks_stale_after_threshold.rs` — kill daemon mid-session; advance test clock; assert panel renders `(stale ...)`.
- `deck/tests/reconnect_swaps_in_fresh_mesh.rs` — daemon outage → restart on same addr → assert poller resumes from a new `Mesh` within backoff window.

### D-12 — Feature gate / phased rollout

Two reasonable shapes:

1. **`--features remote-attach`** on `crates/net/deck/Cargo.toml`. Compile-time gate. Without the feature, `DeckBackend::Remote` doesn't exist; CLI args reject `--attach`/`--endpoint`. Cleaner; smaller binary in pre-rollout builds.
2. **Default-on with runtime kill switch.** Ship default-on; `--no-remote-attach` flag forces Local mode if operators want to opt out for a while.

**Recommend (1) initially**, then flip to default-on once D-11 has soaked. The cargo-feature shape keeps the v2 bookmark schema, multi-input widget, and dispatch tasks out of binaries the operator hasn't opted into. Two `cfg(feature = "remote-attach")` blocks gate the `DeckBackend::Remote` variant + the CLI arg parsing.

**Rollout phases.**
- **Phase A** (feature gated): land D-1 through D-11 behind `remote-attach`. Internal canary deploys flip the feature on.
- **Phase B** (default-on, opt-out): flip the cargo feature default. Add `--no-remote-attach` for one release as escape hatch.
- **Phase C** (gate removed): delete the cargo feature; bake the path into the default.

**Files touched (D-12).**
- `crates/net/deck/Cargo.toml` — `remote-attach` feature, default off; add to CI matrix.
- `crates/net/deck/src/backend.rs` — `cfg`-gate `Remote` variant.
- `crates/net/deck/src/main.rs` — `cfg`-gate `--attach` / `--endpoint` / `--node-*` flags; reject at parse time when feature off.
- `crates/net/deck/src/bookmarks.rs` — schema extension stays unconditional (bookmark TOML is forward-compatible; v1 still loads).

**Test plan (D-12).**
- CI matrix runs deck tests both with and without `--features remote-attach`.
- `deck/tests/feature_off_rejects_attach_flag.rs` — when feature off, `--attach prod` exits with a clear error directing to enable the feature.

---

## Closeout (E)

### E-1 — Docs alignment + help overlay + tidy

- `docs/plans/DECK_PLAN.md` § Deferred work § Multi-Cluster Switcher → "Closed in `DECK_REMOTE_ATTACH_AND_SCALE_RPC.md`".
- `docs/plans/AGGREGATOR_CLI_REMOTE_ATTACH_AND_SCALE_RPC.md` — append a forward-pointer.
- Help overlay (`Modal::Help`) — add a section "AGGREGATORS (remote)" listing `[s]`, `[S]`, `[u]`, `[r]` bindings.
- Footer chip copy (`widgets/footer.rs:222-225`) — replace the read-only comment with live binding chips on AGGREGATORS.
- Remove the `aggregator_registry_snapshot()` accessor on `DeckClient` if no remaining consumer (the comment at `app.rs:1350` mentions it but only `aggregator_snapshot` is actually used today — verify before removing).

---

## Phasing + ordering recommendation

1. **D-1** — `DeckBackend` abstraction. Mechanical but invasive (every `self.deck.` site). Land first; everything else is additive.
2. **D-2** — bookmark schema bump. Independent; can land in parallel with D-1.
3. **D-3** — picker commits real attach + CLI flags. Depends on D-1 + D-2.
4. **D-4** — `RegistrySnapshotTail` poller. Depends on D-1.
5. **D-5** — AGGREGATORS panel render. Depends on D-4.
6. **D-6** — status chips. Depends on D-1; cosmetic so can slot anywhere after.
7. **D-7** — `[s]pawn`. Depends on D-1, D-4 (for force_refresh).
8. **D-8** — `[S]cale`. Depends on D-7's modal infrastructure.
9. **D-9** — `[u]nregister`. Same.
10. **D-10** — SUBNETS AGG fan-in. Depends on D-4. Independent of write path.
11. **D-11** — reconnect + stale + throttle. Best after the write surface so error paths have real consumers.
12. **D-12** — feature gate. Wraps the whole slice; can be added at any point — better as the final structural commit so the cargo-feature surface settles after the code stabilizes.
13. **E-1** — docs + tidy.

**Parallelism.** D-2 + D-6 + D-10 are independent and can land in any order once D-1 is in. D-7/D-8/D-9 share the multi-input widget — D-7 lands first (introduces the widget); D-8/D-9 ride on it. D-11 is the longest test cycle (failure simulation).

**Estimated slice count.** 13 mergeable slices. Per-slice cost: D-1 is 2–3 days (mechanical refactor + lift to SDK); D-3 is 2 days; D-4 + D-5 + D-7 are 1–2 days each; the rest are 0.5–1 day. Total: ~3–4 weeks of focused work.

---

## Risks for the user to weigh in on before any slice lands

1. **Connection lifetime model.** The CLI's one-shot `Mesh::connect_via` works because the CLI is a single command. The Deck session is long-lived — minutes to hours. Holding one persistent `Arc<Mesh>` for the session is what D-1 sketches, but the substrate's UDP socket / PSK / handshake state may not be designed for that lifetime. Concretely: does the dispatch loop survive ~hours of idle time? Are there keep-alive packets? What happens to in-flight reply channels when the daemon's responder restarts under the same addr? This is the single biggest unknown. Recommend a soak test against a real daemon (let it run 24h with no traffic, then send a `List`) before committing to the persistent-mesh model in D-1.

2. **Bookmark schema migration.** D-2 bumps the bookmark TOML version to v2. Operators with existing v1 bookmarks files keep them working but the new fields are `None` (not attachable). Two sub-questions: (a) do we surface a one-time migration prompt at startup, or just silently load and let the picker grey out incomplete entries? (b) Do we add a `net deck bookmark add --from-bootstrap` CLI that takes the daemon's `--print-bootstrap` JSON and writes a complete v2 bookmark? Without (b), the operator has to hand-edit TOML to use the picker. With (b), the UX is "paste-in once and pick forever." I'd push for (b) but it's a separate (small) plan.

3. **Operator-identity signing on write Registry requests.** Today `RegistryClient::spawn/scale/unregister` are NOT operator-identity-signed — the substrate's PSK gates the connection (anyone with the right PSK can spawn). For a multi-operator deployment this is too permissive: any deck with PSK access can unregister anyone's groups. The ICE break-glass path solves this for cluster-level operations (`AdminVerifier` with M-of-N operator signatures). Should Registry writes route through the same gate? If yes, that's a wire change (Registry requests grow a signature payload + a verifier on the daemon) — substantial, and should be its own plan. If no, the Deck is fine as designed but the threat model needs documenting. Recommend flagging this as a Phase-D parallel work item and shipping D-7/D-8/D-9 without identity signing for now (matching the CLI's behavior), with a documented gap.
