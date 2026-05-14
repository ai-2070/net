## Deck — implementation plan

> The operator cyberdeck. A `ratatui` + `crossterm` terminal binary that turns [`DECK_FEATURES.md`](DECK_FEATURES.md)'s thirteen feature blocks into a single composable surface, composing every view against the live Deck SDK ([`DECK_SDK_PLAN.md`](DECK_SDK_PLAN.md)) — `snapshots()` for state, `subscribe_logs()` / `subscribe_failures()` / `audit().since(seq).stream()` for tails, `admin()` for signed commits, `ice()` for break-glass with `simulate()` → `commit(signatures)`. Companion to [`MESHOS_PLAN.md`](MESHOS_PLAN.md) (the substrate the binary commands against) and [`DECK_SDK_PLAN.md`](DECK_SDK_PLAN.md) (the surface the binary imports). **Atomic Playboys release** per [`RELEASE_ROADMAP.md`](RELEASE_ROADMAP.md); follows the Deck SDK.

## Status

Design only. Substrate + SDK prereqs all in code as of v0.17 + the post-v0.17 ICE + chain-seam slices:

- **MeshOS pipeline** — `MESHOS_PLAN.md` Phases A–G + executor + scheduler + snapshot reader + chain integration. The behavior snapshot Deck renders is the same `MeshOsSnapshot` MeshOS publishes on every tick.
- **Deck SDK** — `DECK_SDK_PLAN.md` Phase 1 (snapshot subscription + admin commits + audit queries + log + failure stream) and Phase 3 (ICE — `IceCommands`, `IceProposal::simulate` / `commit`, multi-operator signing). All re-exported from `net_sdk::deck::*`.
- **MeshDB federated executor** — `MESHDB_PLAN.md`. Powers the MeshDB Console (Feature 8).
- **Production chain seams** — `RedexAdminAuditAppender` / `RedexLogAppender` / `RedexFailureAppender` (in `net_sdk::meshos`) and the `OrchestratorMigrationAborter` + `OrchestratorMigrationSnapshotSource` dispatcher/source seams. Wired through `MeshOsRuntime::start_with_full_extensions`. Operator deployments wire all five so audit / log / failure history and `KillMigration` dispatch all work end-to-end.

Activation gate: an SRE workload that wants to operate a running cluster from a terminal — drain a node, watch the migration drain progress, force-evict a wedged replica during incident triage, scroll the audit ring after the dust settles. The features doc is the product brief; this plan is the binary's first shippable arc.

**Substrate gaps this plan introduces:**

- **No public Dataforts read surface for Deck.** Feature 6 (Blob & Artifact Explorer) needs `chain metadata`, `blob movement history`, `heat level`, `access frequency`, `anti-entropy cycles`, `artifact ancestry`. The Deck SDK doesn't surface those today — they live behind the Dataforts adapter. Two paths: (a) extend the Deck SDK with a `dataforts()` accessor that re-exports the existing types, (b) compose against the substrate-internal Dataforts types directly from the binary. This plan pins (a) as the slice that unblocks Feature 6.
- **No per-node cluster inventory surface.** Feature 11 (Node Inventory) needs CPU / mem / disk / saturation trend / capability set / fork-of ancestry / software version. The snapshot's `peers: BTreeMap<NodeId, PeerSnapshot>` has health + locality but not the resource axes. This plan extends the substrate-side `PeerSnapshot` (a strict addition, default-able) when Feature 11 lands.
- **No persistent cluster bookmark store.** Feature 12 (Multi-Cluster Switcher) needs disk-backed "known meshes" with optional pin-per-tab semantics. Out of scope for the SDK — the binary owns the bookmark store on disk.

## Frame

The substrate is the cluster's nervous system. The SDK is the operator's typed surface against it. The binary is the *interaction layer* — the shape an operator's hands and eyes actually touch. The features doc lists thirteen views; this plan turns them into the smallest set of shippable phases that lets a real operator run a cluster from a real terminal.

**Why a binary at all,** when the SDK exists: a cluster operator under incident triage cannot afford "open a web console, log in, navigate to the topology page, click the node, click drain, confirm." The terminal is the lowest-friction surface — `ssh maintenance-node`, `deck`, the live view fills the terminal in under a second, every action is one keystroke + one signed confirmation. Latency to first useful pixel is the load-bearing metric; every architectural choice in this plan trades implementation effort against that latency.

**The architectural posture.** One binary, one operator identity per session, one MeshOS runtime per connected cluster, one `DeckClient` per runtime. Multi-cluster (Feature 12) is multiple `DeckClient`s switched between via tabs. No multi-user, no RBAC at the binary level — the operator identity is the trust boundary, the substrate enforces M-of-N at the chain-commit layer. Read views fire-and-forget on cold cache; signed actions go through the SDK's commit handle + the substrate's verifier; ICE actions go through `simulate()` → confirmation prompt → `commit(signatures)`.

## Why this exists

Three reasons this needs a written plan rather than "we'll ship the SDK and someone builds a TUI":

1. **The view set is interdependent — designing it as thirteen separate views produces drift.** Cluster Topology Map (Feature 1), Replica Inspector (Feature 2), and Daemon Panel (Feature 3) all read from the same `MeshOsSnapshot`. Maintenance Control (Feature 4) and ICE (Feature 13) share the confirmation-prompt + signed-commit pipeline. Log Matrix (Feature 9) and Audit Trail (Feature 10) share the tail-with-seq-watermark + filter-bar pattern. Designing the shared components first (snapshot cache, confirmation prompt, signed-commit wiring, tail-watermark widget) lets every view compose against them; designing the views first produces thirteen reimplementations of the same patterns.

2. **The interaction model is operationally critical.** A keyboard shortcut that drains a node by accident is a paged-out SRE at 3am. A drain confirmation that doesn't show the blast-radius preview is incident retrospectives complaining about "operator error." The plan pins the safety patterns — confirmation prompts, blast-radius rendering, ICE lockout windows, multi-operator signature collection — as substrate-level rather than per-view discretion, so every dangerous action goes through the same gate.

3. **The features list compounds — the order it ships in is load-bearing.** Shipping ICE before the topology map renders is a binary that can break the cluster but can't show what it broke. Shipping Audit Trail before signed commits is a binary that can read history but can't add to it. The phases in this plan are dependency-ordered: each phase has read views before write views, observability before control, and confirms operator UX with a real workload before the next phase opens up more action surface.

## What ships

Six interlocking phases, each landing a vertical slice of usable features:

1. **Skeleton + shared widgets.** `ratatui` app frame, multi-tab layout, snapshot cache with poll-via-SDK refresh, status bar (operator id + cluster id + last-tick age + commit count), confirmation prompt component, tail-watermark widget, log filter bar.
2. **Read-only observability (Features 1, 2, 3, 5, 11).** Cluster Topology Map, Replica & Placement Inspector, Daemon Supervision Panel (view-only — no controls), Behavior Timeline, Node Inventory. Every view a thin render layer over `DeckClient::snapshots()`; no admin-chain writes yet.
3. **Signed admin surface (Features 4, 7, 10).** Admin Surface (drain / cordon / uncordon / drop-replicas / invalidate-placement / restart-all / clear-avoid-list), Maintenance Node Control (the full state machine UI), Operator Identity + Audit Trail (key loading + `audit()` query + audit-event tail). First write phase — the confirmation prompt + signed-commit pipeline lands here.
4. **Streaming surfaces (Feature 9).** Log Matrix with per-node / per-daemon / per-level filters and follow-mode; failure tail; audit tail. Composes `subscribe_logs()` / `subscribe_failures()` / `audit().since(seq).stream()` into a unified tail widget.
5. **ICE break-glass (Feature 13).** ICE panel — Force-drain, Force-evict, Force-restart, Force-cutover, Kill-migration, Freeze / Thaw, Flush-avoid-lists — each with the mandatory `simulate()` → blast-radius render → multi-operator signature collection → `commit(signatures)`. Lockout-timer + cluster-freeze-warning banners.
6. **MeshDB Console + Blob Explorer + Multi-Cluster (Features 6, 8, 12).** Interactive MeshDB REPL (composes against the MeshDB SDK), Blob & Artifact Explorer (composes against the new `net_sdk::dataforts` surface this plan introduces), and Multi-Cluster Switcher (disk-backed bookmark store + per-tab `DeckClient`).

What this doc does NOT ship:

🚫 **Not a metrics platform.** Deck reads the snapshot's `recent_failures` ring and the per-daemon `saturation` field; it does not aggregate time-series, alert, or replace Prometheus. Operators who need long-window dashboards pipe MeshOS's chain into their existing tooling.

🚫 **Not a CMDB.** No asset registry, no SLA tracking, no on-call schedule. Deck shows the cluster *right now*; the historical record is the admin chain.

🚫 **Not a deploy / CI tool.** Pushing new daemon code, rolling builds, key rotation across operators — those flow through whatever pipeline the cluster uses. Deck triggers admin events (`restart_all_daemons`, `enter_maintenance`); it doesn't ship binaries.

🚫 **Not a chat / notes / runbook surface.** Incident notes belong somewhere durable (the on-call runbook, a postmortem doc); Deck is the surgery kit, not the case file.

🚫 **No multi-user / RBAC at the binary level.** One operator identity per session. The substrate's M-of-N signing is the multi-operator boundary; Deck collects signatures from co-operators (via paste / file / hardware key) rather than running as a multi-tenant service.

🚫 **No web UI, no GUI, no mobile.** Terminal only. Other surfaces compose against the SDK; the binary is specifically the terminal cyberdeck.

🚫 **No alerting, paging, or escalation routing.** A failure shows up on the Behavior Timeline + the failure tail; routing it to PagerDuty is your incident pipeline's job.

---

## Design

### 1. Architecture

```text
┌─────────────────────────────────────────────────────────────┐
│  deck binary                                                │
│ ┌──────────────────────────────────────────────────────┐    │
│ │ app loop (tokio)                                     │    │
│ │  ┌────────────────┐   ┌────────────────────────────┐ │    │
│ │  │ terminal input │   │ SDK subscription pumps     │ │    │
│ │  │ (crossterm)    │   │  - snapshots()             │ │    │
│ │  │                │   │  - subscribe_logs()        │ │    │
│ │  │                │   │  - subscribe_failures()    │ │    │
│ │  │                │   │  - audit().stream()        │ │    │
│ │  └────────┬───────┘   └────────────────┬───────────┘ │    │
│ │           │                            │             │    │
│ │           ▼                            ▼             │    │
│ │  ┌────────────────────────────────────────────────┐  │    │
│ │  │ app state (single owner)                       │  │    │
│ │  │  - current_tab                                 │  │    │
│ │  │  - snapshot_cache: Arc<MeshOsSnapshot>         │  │    │
│ │  │  - log_buffer / failure_buffer / audit_buffer  │  │    │
│ │  │  - confirmation_prompt: Option<…>              │  │    │
│ │  │  - active_ice_proposal: Option<IceProposal>    │  │    │
│ │  └────────────────────┬───────────────────────────┘  │    │
│ │                       ▼                              │    │
│ │  ┌────────────────────────────────────────────────┐  │    │
│ │  │ renderer (ratatui)                             │  │    │
│ │  │  - tab strip + status bar                      │  │    │
│ │  │  - per-tab view: topology / replicas / …       │  │    │
│ │  └────────────────────────────────────────────────┘  │    │
│ └──────────────────────────────────────────────────────┘    │
│                              │                              │
│                              ▼ (signed commits)             │
│ ┌──────────────────────────────────────────────────────┐    │
│ │ net_sdk::deck::DeckClient                            │    │
│ │  - admin().drain / cordon / …                        │    │
│ │  - ice().force_drain / freeze_cluster / …            │    │
│ └──────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────┘
```

The app loop is a single `tokio::select!` over: terminal events (`crossterm::event::EventStream`), each SDK subscription's `next()`, and a timer for the snapshot poll. Every wake-up reduces to "fold the event into app state, redraw."

### 2. Shared widgets

These ship in Phase 1 and every later phase consumes them:

- **`SnapshotPanel`** — generic `Arc<MeshOsSnapshot>` reader with last-tick-age indicator. Every observability view embeds this; the topology map, replica inspector, daemon panel, behavior timeline, and node inventory are all `SnapshotPanel<T>` projections.
- **`TailWidget<T>`** — bounded scrollback + seq-watermark + follow-mode toggle. Powers the Log Matrix, Failure tail, and Audit tail. Generic over the record type; the SDK's `subscribe_*` streams feed it.
- **`ConfirmationPrompt`** — modal overlay: action description + blast-radius rendering + cancel / confirm bindings. Every admin commit + every ICE action passes through this. Embeds the same `ConfirmationPrompt` regardless of whether the underlying action is `drain` (ordinary) or `force_drain` (ICE); the difference is which fields it shows.
- **`SignatureCollector`** — modal that accepts operator signatures via paste / file / future hardware-key plugin. Used by ICE for the M-of-N bundle. Single-signature ordinary admin commits skip this and use `deck.identity().sign_admin_event(...)` directly.
- **`StatusBar`** — operator id + connected cluster bookmark + last-tick age + outstanding-commit count + freeze-banner + lockout-timer countdown.

### 3. Feature mapping

Per the features doc, here's the SDK call that backs each view:

| Feature | View | SDK call | Substrate read/write |
|---|---|---|---|
| 1 — Cluster Topology Map | topology | `deck.snapshots()` → `peers`, `replicas`, `avoid_list`, `local_maintenance` | Read |
| 2 — Replica & Placement Inspector | replicas | `deck.snapshots()` → `replicas`, `in_flight_migrations`; future: scoring axes | Read |
| 3 — Daemon Supervision (view) | daemons | `deck.snapshots()` → `daemons` | Read |
| 3 — Daemon Supervision (control) | daemons | `deck.admin().restart_all_daemons(node)` | Signed commit |
| 4 — Maintenance Control | maintenance | `deck.admin().enter_maintenance(...)` / `exit_maintenance(...)`; tail `local_maintenance` discriminant | Signed commit + Read |
| 5 — Behavior Timeline | timeline | `deck.snapshots()` → `pending`; `subscribe_failures(seq)`; `audit().recent(N).collect()` | Read |
| 6 — Blob & Artifact Explorer | blobs | `net_sdk::dataforts::*` (new surface this plan introduces) | Read |
| 7 — Admin Surface | admin | `deck.admin().{drain, cordon, uncordon, drop_replicas, invalidate_placement, restart_all_daemons, clear_avoid_list}` | Signed commit |
| 8 — MeshDB Console | meshdb | `net_sdk::meshdb` query API | Read |
| 9 — Log Matrix | logs | `deck.subscribe_logs(LogFilter::default().with_*)` | Read tail |
| 10 — Operator Identity + Audit Trail | audit | `deck.audit().*` filter chain + `.stream()` for tail | Read |
| 11 — Node Inventory | inventory | `deck.snapshots()` → `peers`; needs `PeerSnapshot` extension for resource axes | Read |
| 12 — Multi-Cluster Switcher | (root) | Multiple `DeckClient`s; disk-backed bookmark store | n/a |
| 13 — ICE | ice | `deck.ice().*` → `simulate()` → `commit(signatures)` | Signed commit (multi-op) |

The keyboard mappings (tab cycle, action shortcuts) land per-phase and stay configurable via a `~/.config/deck/keymap.toml` parsed at startup. Default bindings mirror tmux + vim conventions where they conflict (`Ctrl-b` for tab navigation, `hjkl` for motion, `:` for command bar, `?` for help overlay).

### 4. Locked decisions

Pin these so phase implementations don't relitigate:

1. **`ratatui` + `crossterm`.** ratatui is the modern Rust TUI standard with active maintenance, immediate-mode rendering (fits the per-tick redraw pattern), and a mature widget ecosystem. crossterm is the cross-platform terminal backend. No `tui-rs` (unmaintained); no `cursive` (heavier widget model, doesn't match the data-bound flow).
2. **Single operator identity per session.** Loaded at startup from `~/.config/deck/identity.toml` (or `$DECK_IDENTITY` for CI / tooling). No in-binary key generation — that's `KEY_MIGRATION_PLAN.md`'s territory.
3. **Every admin commit goes through `ConfirmationPrompt`.** No "skip confirmation" flag, even for cordon / uncordon. The friction is the feature.
4. **Every ICE action requires both `simulate()` and a multi-signature bundle.** Single-operator clusters configure the threshold to 1 in operator-policy; the SDK enforces, the binary defers.
5. **Each tab is a `DeckClient`.** Multi-cluster (Feature 12) is N tabs each holding a client; they do not share state. The bookmark store maps cluster id → connection config.
6. **All snapshot-derived rendering uses `Arc<MeshOsSnapshot>` clones.** Reading the snapshot is one atomic `ArcSwap::load` — no locks in the render path. Tabs that need a stable view across a render pass clone the Arc once at frame start and consume the same projection until the next tick.
7. **Subscription pumps run as separate `tokio` tasks; their output funnels into a bounded `mpsc::Sender<UiEvent>` the app loop drains.** A stalled view (operator paused on the log tab) never wedges the snapshot poll; the bounded channel drops oldest log lines (counter increments) and `StatusBar` surfaces the drop count.
8. **No async in the render path.** Every render is sync over the current `app_state`. The subscription pumps + commit / sign tasks live elsewhere; the renderer reads the resulting `app_state` and projects it.
9. **`deck` is a single binary; the workspace member lives at `net/crates/deck/`.** It depends on `ai2070-net-sdk` (with `features = ["meshos", "deck", "meshdb"]`). No "deck-core" library split until a second consumer (e.g. a Python TUI binding) exists.
10. **The disk footprint is bounded.** Bookmarks + per-cluster recent-commit log + scrollback caches: under 10 MiB per cluster total. No SQLite, no embedded LMDB — just JSON / TOML files under `$XDG_CONFIG_HOME/deck/` and `$XDG_CACHE_HOME/deck/`. Anything bigger is a metrics-platform problem.

---

## Phases

Activation order, dependency-driven:

- **Phase 1 — skeleton + shared widgets.** `ratatui` app frame, tab strip, status bar, `SnapshotPanel<T>` over `Arc<MeshOsSnapshot>`, `TailWidget<T>`, `ConfirmationPrompt`, `SignatureCollector`. One placeholder tab. Smoke test: launch the binary against a `MeshOsRuntime` test fixture; see the status bar tick and the tab strip render.
- **Phase 2 — read-only observability.** Cluster Topology Map, Replica Inspector, Daemon Panel (view), Behavior Timeline, Node Inventory. Each tab a `SnapshotPanel<T>` projection. No writes yet; the binary is observability-only. Activation gate for Phase 3: an operator can hold a long session open without UI tear.
- **Phase 3 — signed admin surface.** Admin tab + Maintenance tab + Audit tab. Confirmation-prompt + signed-commit pipeline lands here. The Daemon Panel grows its control row in this phase (the "restart daemon" / "drain daemon" actions). Activation gate for Phase 4: an operator can drain a node end-to-end (initiate, watch the maintenance state machine progress, see the avoid-list clear, exit maintenance).
- **Phase 4 — streaming.** Log Matrix + Failure tail + Audit tail. `TailWidget<T>` instances per stream, each backed by a `subscribe_*` SDK call. Filter bar, follow-mode, scrollback. Activation gate for Phase 5: a stuck cluster's log surface fills the Log Matrix within 100 ms.
- **Phase 5 — ICE.** ICE tab. `simulate()` → blast-radius modal → `SignatureCollector` → `commit(signatures)`. Freeze-banner + lockout-timer in the status bar. Activation gate for Phase 6: an operator can ICE-force-evict a wedged replica during a real incident triage.
- **Phase 6 — long tail.** MeshDB Console, Blob & Artifact Explorer, Multi-Cluster Switcher. Each independent of the others; ship as separate sub-slices. Bookmark store lands with the Multi-Cluster slice.

Phases 4–6 land independently of each other; Phases 1–3 are a hard prereq chain. Each phase can ship partial scope (e.g. Phase 4 ships Log Matrix first, then Failure tail, then Audit tail) as long as the phase converges before declaring activation-gate passed.

---

## Non-goals

Per the scope brief, the binary is not:

- A metrics / observability platform (use Prometheus + Grafana for time-series; Deck reads the live snapshot).
- A CMDB / asset registry (Deck shows runtime state, not inventory).
- A deploy / CI tool (Deck triggers admin events; binaries flow through other pipelines).
- A chat / notes / runbook surface.
- A multi-user / RBAC service.
- A web UI / GUI / mobile surface.
- An alerting / paging engine.
- An operator-key generation tool.

Tenant-side workflows that want richer semantics build them on top of the SDK; we don't extend the binary to cover them.

---

## Interaction surfaces

The binary interacts with five external surfaces:

- **Net SDK (`net_sdk::deck` + `net_sdk::meshos` + `net_sdk::meshdb`)** — every cluster-facing call. The binary imports the SDK; it never reaches into substrate internals.
- **Operator identity store** — `~/.config/deck/identity.toml` (or `$DECK_IDENTITY` override). Loaded once at startup; the binary never writes back.
- **Cluster bookmark store** — `$XDG_CONFIG_HOME/deck/bookmarks.toml`. Read at startup; written when the operator adds / removes / pins a cluster.
- **Per-cluster scrollback cache** — `$XDG_CACHE_HOME/deck/<cluster-id>/`. Bounded ring of recent log / audit / failure entries so scrollback survives across reconnects. Eviction is age-driven.
- **Terminal (`crossterm`)** — input events + output rendering. The binary never assumes a specific terminal emulator; `ratatui` handles the lowest-common-denominator subset.

The binary explicitly does NOT interact with:

- **MeshOS internals.** Every cluster read goes through `MeshOsSnapshot`; every cluster write goes through a signed admin commit.
- **RedEX directly.** Logs / audit / failures arrive through the SDK's tail surfaces, not raw RedEX reads.
- **PagerDuty / OpsGenie / Slack.** Operators paste the URL of the current commit into whatever incident channel they already use; the binary doesn't ship outbound integrations.

---

## Test surface

Following the SDK plans' precedent:

- **Per-view snapshot tests.** Each `SnapshotPanel<T>` projection has a unit test that pins a fixture `MeshOsSnapshot` and asserts the rendered cells match a recorded golden. Rendering changes are easy to review; widget changes don't silently drift the rendered shape.
- **Confirmation-prompt + signed-commit integration tests.** Mock SDK that records every admin / ICE commit; drive the binary through `Enter → confirm → commit` and assert the chain commit landed with the expected `AdminEvent` variant + operator signature.
- **ICE discipline tests.** `simulate()` is mandatory before `commit()`; sub-threshold signature bundles refuse with `IceError::InsufficientSignatures`; lockout-window violations refuse with `IceError::LockedOut`. Mirrors the SDK-level checks but at the binary's interaction layer.
- **Tail-widget regression tests.** Bounded scrollback, seq-watermark dedup, follow-mode toggle, filter-bar narrowing. Each behavior pinned against a synthetic stream.
- **Multi-cluster bookmark tests.** Bookmark add / remove / pin / switch survives a restart. The disk format is part of the public surface (operators edit it directly when scripting).

---

## Open questions

- **Keymap conflicts.** tmux + vim share `Ctrl-b` for different semantics; the default `Ctrl-b` for tab navigation will surprise tmux power-users. Likely resolution: surface a `--no-tmux` mode that swaps `Ctrl-b` for `Ctrl-w`-style navigation. Decision deferred to Phase 1's UX validation.
- **Wide-terminal rendering of the topology map.** ratatui's canvas widget renders well on terminals ≥ 120 columns; narrower terminals collapse the map to a list view. The exact breakpoint (and the list-view shape) ships with Phase 2.
- **Operator-identity multiplexing.** Default is one identity per session; the disk format already supports a `[identities.<name>]` table for future "switch identity mid-session" workflows (e.g. paging-in a co-operator without restarting the binary). Phase 5 (ICE) is where this becomes load-bearing; the slice that lands the multi-signature collection determines whether the binary holds multiple identities live or shells out to a co-operator's instance.

---

*Atomic Playboys (post-`DECK_SDK_PLAN.md`) release candidate. Gates on a real cluster operator workload — drain a node, observe the migration progress, ICE-force-evict a wedged replica, scroll the audit ring. The substrate + SDK are in code; this plan turns the features list into the smallest sequence of phases that lets an operator actually run a cluster from a terminal.*
