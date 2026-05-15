# Deck — build plan

Working doc for replacing every fixture in `src/tabs/*` with a
real SDK projection. The strategic 6-phase doc lives at
[`../docs/plans/DECK_PLAN.md`](../docs/plans/DECK_PLAN.md); this
file is the tactical "which fixture becomes which SDK call, in
what order" engineering plan.

## Status

The skeleton ships:

- ratatui + crossterm + tokio (`#[tokio::main]`).
- Five tabs: NET.MAP / LIST / DATAFORTS / DAEMON / LOGS.
- Shared chrome: status bar, tab bar, footer.
- Canonical node fixture in `src/nodes.rs` with the `id.label`
  rendering helper used across every tab.
- `feature = "demo"` spawns an in-process `MeshOsRuntime` + 4
  demo daemons (`mikoshi`, `gravity`, `anti_entr`, `telemetry`)
  + a tokio seeder that publishes log lines every 600 ms and
  signed cordon/uncordon admin events every 4 cycles.
- `App` holds `Option<Arc<DeckClient>>` + a `snapshot:
  Option<Arc<MeshOsSnapshot>>` cache refreshed once per
  120 ms tick.
- LOGS tab reads `snapshot.log_ring` live; falls back to the
  fixture stream when no runtime is wired.
- Status bar's first slot reads `● DEMO` / `● LIVE` / `● FIXTURE`
  depending on connection state.

Everything else (NET.MAP, LIST, DATAFORTS, DAEMON) still
renders hardcoded constants.

## Goal

Every visible cell on every tab is a projection of a live
`MeshOsSnapshot` (or a live SDK stream). No hardcoded node
ids, daemon names, RTT values, or saturation numbers in the
default render path. Fixture mode (no `--features demo`,
unconnected) stays runnable but switches to a single "NOT
CONNECTED" splash per tab — it's a build-time mode, not a
data source.

---

## Per-tab plan

For each tab: the SDK call that projects the data, the
column-by-column mapping, and any substrate / SDK gaps that
block the projection.

### NET.MAP — `snapshot.peers` + derived edges

| Render element | Source | Notes |
|---|---|---|
| Node glyphs | `snapshot.peers.keys()` | NodeId u64; format as `0xNN`. |
| Node kind | `peer.kind` (NEW field — gap) | Today PeerSnapshot has health/locality, no `kind`. |
| Node health color | `peer.health` | Healthy/Degraded/Unreachable. |
| Edges | proximity graph (gap) | PeerSnapshot doesn't expose adjacency. |
| In-transit overlay | `snapshot.in_flight_migrations` | Already wired in the SDK. |
| Daemon overlays (`d.0xNN`) | `snapshot.daemons` grouped by placement node | Needs `DaemonSnapshot.placement: NodeId`. |
| Event tail | `snapshot.admin_audit` (newest 5) + `snapshot.recent_failures` | Already in snapshot. |

**Gaps to close:**

1. `PeerSnapshot.kind`: enum `{ Compute, Datafort, Region, Device }`. Operator-set via capability tag (e.g. `mesh.kind=datafort`) so MeshOS just reads from `peer.capabilities` and folds.
2. Topology edges: cheapest path is "two nodes share at least one chain replica → there's an edge." Use `snapshot.replicas` and union the holder lists. Doesn't require a substrate change.
3. `DaemonSnapshot.placement: NodeId`: which node this daemon runs on. Today the snapshot's `daemons` map is keyed by daemon id but doesn't carry the host. Add as a field.
4. Layout algorithm: peers have no `(x, y)`. Use a deterministic hash-based ring layout for the first slice (simple, jitter-free); upgrade to force-directed once the graph has real density.

### LIST — `snapshot.peers` + `snapshot.daemons`

**Nodes table:**

| Column | Source |
|---|---|
| NODE (`id.label`) | `peer_id` → fixture's `nodes::id_spans` (gap: labels) |
| KIND | `peer.kind` (gap as above) |
| HEALTH | `peer.health` |
| RTT.P50 | `peer.rtt_p50_us` (gap — exists? need to check) |
| SAT | `peer.saturation` (gap?) |
| DAEMONS | `count_of(snapshot.daemons where placement == peer_id)` |
| MAINT | `snapshot.maintenance.get(peer_id)` |

**Daemons table:**

| Column | Source |
|---|---|
| DAEMON | `daemon_id` → format `0xNN` |
| KIND | `daemon.name` (== kind) |
| LINEAGE | `daemon.lineage` (gap — group membership) |
| NODE (`id.label`) | `daemon.placement` (gap as above) |
| LIFE | `daemon.lifecycle` |
| HEALTH | `daemon.health` |
| SAT | `daemon.saturation` |
| RST | `daemon.restart_state` → restart_count |
| AGE | `now - daemon.started_at_ms` (gap — `started_at_ms` field) |

**Gaps to close:**

1. **Labels** — `peer.label` field, or operator-side mapping `NodeId → String` that deck loads from a config file (`~/.config/deck/labels.toml`). Either works; the config-file path is cleaner for first slice (no substrate change) and matches how operators tag their nodes today.
2. `DaemonSnapshot.placement` (as above).
3. `DaemonSnapshot.lineage: Option<LineageRef>` — `{ kind: Replica/Fork/Standby, group_seed: [u8;32], index: u8 }`. Substrate-side: the `behavior::groups` registry knows this; the snapshot fold pulls it.
4. `DaemonSnapshot.started_at_ms` — daemon birth timestamp. Likely already on `MeshOsState.daemons[id].started_at`; just need to project it.

### DATAFORTS — needs new SDK surface

The Deck SDK doesn't expose a Dataforts read API today. Three
data shapes the tab needs:

- Storage pool: per-node `{ used_bytes, total_bytes, status }`.
- Recent events: cool / absorb / pull stream (blob lifecycle).
- Pressure summary: cluster-wide watermarks + steady/draining tag.

**Gap:** `net_sdk::dataforts` module that re-exports
`DataforterSnapshot` / `DataforterEvent` / `DataforterPressure`.
Substrate has the data inside the Dataforts adapter; the SDK
re-export is the missing seam.

Deferred — pick this up when the Dataforts SDK lands.

### DAEMON — `snapshot.daemons` with lineage grouping

Left pane (lineage tree):

- Iterate `snapshot.daemons`, group by `daemon.lineage`.
- Standalone group at top (no lineage).
- One group per `(kind, group_seed)` for Replica / Fork / Standby.
- Within each group, members ordered by index.
- Cursor: `(group_idx, member_idx)` tuple on `App` state.

Right pane (detail):

- Selected daemon's full `DaemonSnapshot`.
- Lineage panel: same group's other members.
- Saturation sparkline: per-daemon ring of last N saturation
  samples (gap — needs a saturation history on the snapshot).
- Log tail: filter `subscribe_logs` by `daemon_id`.
- Controls (later phase): admin commands restart/drain/migrate.

**Gaps to close:** same lineage + placement gaps as LIST; plus
`DaemonSnapshot.saturation_history: VecDeque<f32>` for the
sparkline (or operator-side maintains the history by sampling
`saturation` per tick).

### LOGS — wired, polish remaining

Done: live `snapshot.log_ring` projection with timestamp +
level color + `0xnode.label/0xdaemon` source + message.

Remaining:

- **Stream-based tail** instead of poll-the-snapshot. Use
  `DeckClient::subscribe_logs(LogFilter)` so newer lines
  arrive without depending on the 120 ms render tick.
- **Filter bar interaction** — `[f]` opens a modal, set
  `level / node / daemon / kind`, rebuilds the LogFilter,
  re-subscribes.
- **Search** — `[/]` enters search mode; filters lines whose
  message contains the substring.
- **Pause** — `[p]` toggles; paused streams hold their tail
  position so the operator can read.
- **Scrollback** — `PgUp/PgDn` scrolls; auto-resume on
  reaching the bottom.

---

## Substrate gaps consolidated

Net changes the substrate / SDK needs before all tabs can read
live data. Ordered by how many tabs they unblock:

| Gap | Unblocks | Effort | Plan |
|---|---|---|---|
| `DaemonSnapshot.placement: NodeId` | LIST + DAEMON + NET.MAP | small (add field, fold from `MeshOsState.daemons[id].host_node`) | Phase A. |
| `DaemonSnapshot.lineage: Option<LineageRef>` | LIST + DAEMON + future ICE preview | medium (groups registry → snapshot fold) | Phase A. |
| `DaemonSnapshot.started_at_ms: u64` | LIST + DAEMON | small | Phase A. |
| Per-node `label` (operator-defined tag) | every tab | trivial (config file path on the binary side) | Phase A — load from `~/.config/deck/labels.toml`. |
| `PeerSnapshot.kind` | NET.MAP + LIST | small (derived from capability tags) | Phase B. |
| `PeerSnapshot.rtt_p50_us` + `PeerSnapshot.saturation` | LIST | small (probably already there — verify) | Phase B (verify in slice). |
| Topology edges in snapshot | NET.MAP | already derivable from `snapshot.replicas` | no substrate change, decode in deck. |
| `DaemonSnapshot.saturation_history` | DAEMON sparkline | optional — operator-side ring works | Phase D, deck-side ring keyed by daemon_id. |
| `net_sdk::dataforts` | DATAFORTS | large — needs the Dataforts SDK plan | deferred. |

---

## App-state evolution

`App` today: `current_tab`, `should_quit`, `tick`,
`deck: Option<Arc<DeckClient>>`, `snapshot: Option<Arc<MeshOsSnapshot>>`.

Needed:

- **Per-tab selection state.** Currently DAEMON's `▶ cursor` is
  hardcoded `(group=1, member=0)`. Becomes `daemon_cursor:
  (usize, usize)` on App, mutated by `j/k`.
- **LogStream subscription.** A tokio task driving
  `deck.subscribe_logs(filter).next().await` → pushes lines into
  a bounded `parking_lot::Mutex<VecDeque<LogRecord>>` shared with
  the render path. Tick refresh continues to read `snapshot.log_ring`
  as a cold start; the stream fills the gap between ticks.
- **Filter / search state** (LOGS): `level`, `node_id`, `daemon_id`
  filters; `search_query` string; `follow_mode: bool`;
  `scroll_offset: usize`.
- **Modal state.** `ConfirmationPrompt` (admin commits) and
  `SignatureCollector` (ICE) are mutually-exclusive overlays —
  one enum `Modal::{ None, Confirm(ConfirmAction), Signature(IceProposal), Help, Search }`.
- **Bookmark store** (Phase F — multi-cluster). Disk-backed
  `Vec<ClusterBookmark>`; current tab references one. Out of
  scope for the first wired build.

The render path stays sync — every tab reads `App` immutably
and projects it. The async surfaces (stream pumps, key
handling, admin commits) live in `App::run`'s tokio
`select!` (which replaces the current `event::poll`-only
loop).

---

## Phases

Each phase ships one or two tabs end-to-end, plus the
substrate-side work it needs. Activation gates between
phases are real-operator outcomes, not feature counts.

### Phase A — daemons live (LIST.daemons + DAEMON)

- Substrate: add `placement`, `lineage`, `started_at_ms` to
  `DaemonSnapshot`.
- SDK: re-export the extended `DaemonSnapshot` (no change if
  it's a new field — additive).
- Deck side:
  - `tabs::list_view::render_daemons_table` reads `snapshot.daemons`.
  - `tabs::daemon::render_list` projects daemons → lineage tree.
  - Cursor moves with `j/k`; jumping daemons updates detail pane.
- Demo seeder side:
  - Register one ReplicaGroup of 3 + one StandbyGroup of 3 so
    the lineage tree has non-trivial groups.
- Activation gate: operator can scroll the daemon list, see
  lineage, see real saturation / health, see real ages.

### Phase B — peers live (LIST.nodes + NET.MAP)

- Substrate: add `PeerSnapshot.kind` (derived from capability
  tags) + verify `rtt_p50_us` / `saturation` already exist;
  add them if not.
- Deck side:
  - Labels loaded from `~/.config/deck/labels.toml` on startup;
    `nodes::label_of` reads from that map.
  - `tabs::list_view::render_nodes_table` reads `snapshot.peers`.
  - `tabs::net_map::paint_graph` reads `snapshot.peers` for
    node positions (deterministic hash-ring layout) + derives
    edges from `snapshot.replicas` holder unions.
- Demo seeder side:
  - Publish `ReplicaUpdate::Added { chain, holder }` events
    for a handful of synthetic chains so the topology has
    edges.
- Activation gate: operator sees the actual cluster topology
  with their own node labels, not the canned fixture.

### Phase C — admin actions wired (DAEMON controls + Maintenance tab)

- Confirmation-prompt modal (`ConfirmationPrompt` widget).
- Daemon controls: `[r]` restart, `[d]` drain → emit signed
  admin commits via `deck.admin().*`.
- New Maintenance subview within DAEMON or as a 6th tab — the
  feature spec calls for both maintenance and admin surfaces.
- Substrate: nothing new — the admin SDK is shipped.
- Activation gate: operator can drain a node end-to-end from
  the binary.

### Phase D — streaming (audit + failure + log subscriptions)

- Replace poll-the-snapshot with `subscribe_logs` /
  `subscribe_failures` / `audit().since(seq).stream()`.
- Per-tab tail widgets backed by bounded `VecDeque` mutated
  from pump tasks.
- Pause + scrollback + search + filter modal interactions.
- Activation gate: a stuck cluster's log surface fills the
  LOGS tab within 100 ms of an event landing.

### Phase E — ICE break-glass

- `IceCommands` UI: pick proposal, `[s]` simulates → modal
  shows `BlastRadius`, `[c]` collects signatures →
  `SignatureCollector` modal, `[Enter]` commits.
- Status bar surfaces freeze banner + lockout-timer.
- Activation gate: operator can ICE-force-evict a wedged
  replica during a real incident.

### Phase F — DATAFORTS + multi-cluster + node inventory

- Depends on `net_sdk::dataforts` (out of scope for deck —
  needs its own SDK plan).
- Multi-cluster: bookmark store + per-tab `DeckClient`.
- Node Inventory tab: requires extended `PeerSnapshot` fields
  for CPU / mem / disk / software-version.
- Activation gate: operator can swap between 2+ clusters and
  drill into per-node inventory.

---

## Demo seeder evolution

The seeder is the integration test surface — every phase
should leave the seeder publishing enough events to exercise
the new live wiring. Today's seeder publishes 4 daemons +
log lines + cordon/uncordon admin events. Per phase:

- **Phase A**: register `MeshOsDaemonSdk` daemons in groups
  (`ReplicaGroupConfig` of 3 mikoshis, `StandbyGroupConfig`
  of 3 anti_entrs). Each group exercises one lineage type so
  the DAEMON tab has all three group flavors visible.
- **Phase B**: publish `ReplicaUpdate::Added { chain, holder }`
  events so the snapshot's `replicas` populates → NET.MAP
  edge derivation has input. Optional: synthesize fake peers
  by publishing `PeerHealthUpdate` events.
- **Phase C**: nothing new — admin commits already exercised.
- **Phase D**: synthesize a burst of LogLine events on a
  sub-millisecond cadence to stress the tail widget.
- **Phase E**: bootstrap an `AdminVerifier` with a
  registered operator key so ICE commits actually verify.

The seeder lives in `src/demo.rs` and stays gated behind
`feature = "demo"` throughout.

---

## Open questions

- **Label store format.** TOML map `node_id → label` is
  simplest. Alternatively the substrate exposes
  `peer.capabilities[mesh.label]` and deck reads that — no
  client-side config. The latter is cleaner long-term;
  the former unblocks Phase B today.
- **Topology edge weight semantics.** "Two nodes hold the
  same chain" is a binary relation; thicker edge =
  more shared chains? Worth keeping simple at first (binary).
- **Sparkline window**: 60 s × 1 s buckets vs 5 min ×
  5 s. The DAEMON detail sparkline is just one example;
  pick a default that matches the substrate's actual
  sampling cadence so the ring isn't aliased.
- **Multi-operator signatures in the demo.** ICE requires
  M-of-N. The demo needs either: (a) a pre-baked test
  signature bundle, (b) a `--ice-threshold=1` operator
  policy override, (c) skip ICE in demo until real operator
  identities exist. Probably (b) for ergonomics.

---

## Out of scope for this build plan

- **GUI / web / mobile.** Terminal only.
- **Multi-user / RBAC.** One operator identity per session.
- **Cross-language bindings.** Other-language Deck bindings
  follow the SDK plan, not this binary.
- **Cluster bootstrap / installation.** Deck assumes a running
  cluster (or the demo harness in this binary).

---

*Tactical companion to [`../docs/plans/DECK_PLAN.md`](../docs/plans/DECK_PLAN.md).
Update as substrate gaps close + new tabs land.*
