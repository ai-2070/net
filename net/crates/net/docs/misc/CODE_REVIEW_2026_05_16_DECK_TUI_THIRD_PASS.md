# Deck TUI branch — independent third-pass review — 2026-05-16

Branch: `tui`. Two prior reviews (see
`CODE_REVIEW_2026_05_16_DECK_TUI.md` 48 items and
`CODE_REVIEW_2026_05_16_DECK_TUI_SECOND_PASS.md` 17 items)
each closed every item they opened. This is an independent
re-read across the whole branch plus the seven commits that
landed after the second-pass closeout (`14e075d7`):

- `8807ced0` — `l` pivots to LOGS prefiltered for cursored / focused entity.
- `16f07e72` — free `l` (and `h`) from the vim-style tab-cycle binding.
- `051c6e11` — cursor-driven tables render with `TableState` so the window auto-scrolls.
- `b5b245b6` — Esc on LOGS pops the back-target stashed by the `l` pivot.
- `82fe51eb` — status-bar peer summary counts the local node.
- `c0ff2604` — blob detail modal surfaces size / replicas SDK fields.
- `42c23b56` — NRPC `p` freezes the call ring (mirrors LOGS pause).

## Status

**Closed.** 24 items identified: **3 High / 8 Medium / 11 Low / 2 Nit.**
22/24 landed across the same number of commits on `tui`; L3 and
L11 were verified non-issues and tracked as "no change" below.
Per the "no review-tracking IDs in code or commit messages"
feedback rule, labels (H1-H3, M1-M8, L1-L11, N1-N2) are for
this doc only — code and commit messages stay
self-explanatory.

Substrate / SDK items (H3, M7) shipped with regression tests.

## Verification notes

Two claims surfaced during exploration but ruled out:

- "AUDIT / FAILURES tabs are unreachable through any UX path" —
  intentional per the `Tab::all()` docstring at
  `deck/src/app.rs:31-37` ("their variants, state, render
  modules, and key handlers stay in the codebase so re-enabling
  is a one-line addition here"). Not a bug.
- "`phase_progress_pct(Snapshot) = 10` is falsely-earned
  progress" — explicitly documented at
  `behavior/meshos/migration_snapshot_source.rs:86-92` as a
  coarse phase-ordinal projection with the same caveat. Not a
  bug.

## H. High — fix before merge

| ID  | Area      | Title                                                                                  | Location                                  |
|-----|-----------|----------------------------------------------------------------------------------------|-------------------------------------------|
| H1  | deck      | `widgets/node_card` renders the title id as `0x{:04x}` while every other tab uses `0x{:x}` — the same node renders as `0x0001` in the NODE card but `0x1` elsewhere on the same screen (GROUPS local-node panel, DAEMON page). | `deck/src/widgets/node_card.rs:37-39` |
| H2  | deck      | `Ctrl-A` fires `ClearAvoidList` because the `Char('A')` arm doesn't check `mods.contains(CONTROL)` — only `Ctrl-C` checks. An operator's terminal-native `^A` (readline cursor-home, screen prefix, etc.) silently spawns an admin proposal. | `deck/src/app.rs:1340-1360, 1698-1706` |
| H3  | substrate | `MigrationListItem.buffered_events` truncates `usize → u32` unchecked at `record.state.buffered_event_count() as u32`. The whole point of surfacing `buffered_events` is to flag the stuck-in-Replay failure mode that buffers without bound; a wrap to small numbers reports forward progress when reality is the opposite. | `adapter/net/compute/orchestrator.rs:1669` |

## M. Medium

| ID  | Area      | Title                                                                                  | Location                                  |
|-----|-----------|----------------------------------------------------------------------------------------|-------------------------------------------|
| M1  | deck      | `pop_logs_back` clears `logs_search` but doesn't restore the pre-pivot `logs_min_level` or `logs_paused`. `filter_logs_for_id` forces level to `Debug` and pause to `None`; after Esc-back, both stay clobbered. An operator who paused LOGS at `Warn`, pressed `l` on a daemon, and Esc'd back finds their pause gone and the level filter at Debug on the next visit. | `deck/src/app.rs:820-845` |
| M2  | deck      | NRPC table sums to ≥136 cells of fixed-width columns; rightmost (STATUS / RESP / REQ) silently clip below that. Every column except METHOD is `Constraint::Length(N)`; STATUS at `Length(20)` truncates `"Err: kinematic singularity"` to `"Err: kinematic sin"` on an 80-col tmux pane with no visual signal. | `deck/src/tabs/nrpc.rs:112-128` |
| M3  | deck      | MIGRATION cell progress bar uses fixed `WIDTH: usize = 16` inside a `Percentage(50)` cell of the daemon page. Label (≤13) + bar (16) + " 50%" needs ~34 inner cells; a 60-col daemon page yields a ~26-cell inner — the bar wraps or overflows. No narrow-cell fallback. | `deck/src/tabs/daemon_page.rs:318-336` |
| M4  | deck      | `widgets/node_card` redefines `HEALTH_GATE_CLEAR = 0.85` / `HEALTH_GATE_EMIT = 0.95` locally. The second-pass L4 wired NODES + NODE.PAGE to the shared `dataforts::HEALTH_GATE_*_THRESHOLD` constants but missed this widget — same drift risk lives on. | `deck/src/widgets/node_card.rs:18-19` |
| M5  | deck      | `dispatch_confirm` drops the spawned task's `JoinHandle`. On `q` / Ctrl-C the harness drops and the tokio runtime tears down — in-flight admin tasks get cancelled at their next `.await` with no operator-visible outcome. A toast posted from the spawned task is also lost because the receiver is gone. | `deck/src/app.rs:2435-2535` |
| M6  | deck      | Export writes untrusted record fields verbatim. A daemon-published `message` containing `\n` breaks the one-record-per-line export; a `\x1b]…` payload survives to disk and to any pager an operator runs over the file. Sanitise on write. | `deck/src/widgets/export.rs:42-58, 67-99, 126-143` |
| M7  | substrate | `MeshOsRuntime::add_inventory_probe` is append-only — no remove / clear API. Long-running runtimes (hot-reload, test harness probe swaps) accumulate dead probes that keep firing every Tick; last-writer-wins per peer means a stale probe can stomp the live one. | `behavior/meshos/runtime.rs:622-629`, `event_loop.rs:294-305` |
| M8  | deck      | `nrpc_tail.snapshot()` clones the entire 5000-entry ring (each owns `String` method + optional `String` error) every render. Same per-frame alloc pattern the first-pass M7 closed for audit / failures. With samples-logs at 6 cps × 8 Hz redraw this is steady-state churn; the NRPC pause snapshot also clones the ring on toggle but that's keystroke-driven (acceptable). | `deck/src/streams.rs:80-83`, `app.rs:2682-2683, 2828-2842`, `tabs/nrpc.rs:78-79` |

## L. Low

| ID  | Area      | Title                                                                                  | Location                                  |
|-----|-----------|----------------------------------------------------------------------------------------|-------------------------------------------|
| L1  | deck      | `peer_summary` initialises `healthy = 1` and `total = peers.len() + 1` unconditionally, assuming the local node is always Healthy. The synthesized local PeerSnapshot stamps `Some(Healthy)` today, but the assumption isn't asserted; a future change that lets the local node self-report Degraded / Unreachable silently disagrees with NODES (which renders the real state). | `deck/src/widgets/status_bar.rs:135-160` |
| L2  | deck      | `pop_logs_back` clears `logs_search` to `String::new()` — but if the operator edited the search post-pivot (refined the hex query to a custom substring), that intentional edit is also discarded on Esc-back. | `deck/src/app.rs:847-873` |
| L3  | deck      | `cursor-driven TableState` refactor (`051c6e11`) covers 7 tabs but the `audit` tab keeps `frame.render_widget(table, area)`. AUDIT has its own audit-cursor key path in the codebase even though it's hidden from `Tab::all()`; re-enabling AUDIT later (a documented one-line change) brings back the original clipping behaviour the refactor fixed elsewhere. | `deck/src/tabs/audit.rs:158-172` vs `tabs/{blobs,daemons,dataforts,failures,migrations,nodes,replicas}.rs` |
| L4  | deck      | NRPC seeder cycles 16 methods × 16 caller/callee pairs in lockstep — both indexed `i % 16`. After 16 ticks the (caller, callee, method) tuple repeats identically. Fixture shows 16 distinct triples instead of 256; reads as far more uniform traffic than intended. | `deck/src/runtime.rs:910-928, 934-973` |
| L5  | deck      | NRPC status column at `Length(20)` truncates several seeded error reasons (`"Err: kinematic singularity"` → `"Err: kinematic sin"`). Either widen to `Length(28)` or move STATUS to a flex column. | `deck/src/tabs/nrpc.rs:90, 123` |
| L6  | deck      | NRPC latency colour ladder tops out at "≥100ms = red"; 240ms and 6000ms render identically. Once latencies escalate to seconds, the operator can't distinguish slow-but-OK from catastrophic. Add a `>= 1000ms` tier. | `deck/src/tabs/nrpc.rs:139-152` |
| L7  | deck      | MIGRATION cell continues to render the `Complete` phase with a green 100% bar indefinitely. Fixture never emits `Complete` to `in_flight_migrations`, but the snapshot contract doesn't bar it; once a real source ships, completed entries pin the daemon's right cell. Auto-hide on Complete or filter on the producer. | `deck/src/tabs/daemon_page.rs:273` |
| L8  | deck      | Cluster picker recomputes `BookmarkStore::sorted()` on every `j`/`k` keystroke — each call allocates a `Vec<&Bookmark>` and re-sorts. Cache once on picker open. | `deck/src/bookmarks.rs:176-180`, `app.rs:2150, 2803-2805` |
| L9  | deck      | `bookmarks::save` uses a fixed sibling tmp name (`bookmarks.toml.tmp`); two concurrent deck instances pointing at the same config dir race on the rename. Unique tmp suffix (pid + ms) would resolve. | `deck/src/bookmarks.rs:248-260` |
| L10 | deck      | `tab_bar` truncates silently on narrow terminals — no `+N` indicator. Keys still work (numeric jumps), but the visual cue vanishes; on an 80-col tmux pane the last 2–3 tabs disappear. | `deck/src/widgets/tab_bar.rs:34-49` |
| L11 | substrate | Inventory map not scrubbed on `PeerDeparted` events; only on the next probe tick. Between the departure event and the next probe pass, the published snapshot shows a peer with no rtt / health but populated `cpu_load_1m` — operator confusion. | `behavior/meshos/state.rs:50-55`, `event_loop.rs:778-823` |

## N. Nits

| ID  | Title                                                                                  | Location                                  |
|-----|----------------------------------------------------------------------------------------|-------------------------------------------|
| N1  | `blob_detail::replicas_text` uses `(o as u32) < t` and `o as u32 == t` — `replicas_observed` and `replica_target` are both `Option<u32>` (per `adapter.rs:343-353`), so the casts are no-ops. Dead expression. | `deck/src/widgets/blob_detail.rs:126, 128, 140` |
| N2  | NRPC empty-state hint mentions `--features samples-logs`, leaking the build feature to an operator-facing surface. Use operator-vocabulary phrasing matching the FAILURES / AUDIT empty-states. | `deck/src/tabs/nrpc.rs:45` |

## Notes on the new commits

Net read of the 7 post-second-pass commits:

- **`l` pivot + `LogsBackTarget`** (`8807ced0` + `b5b245b6` + `16f07e72`):
  The Esc absorber ordering (search-editing first, back-pop
  second) is correct. The two issues are M1 (level / pause not
  restored) and L2 (operator's custom search overwritten). The
  decoupled `h`/`l` freed both directions from the tab-cycle —
  fine, but the commit message only mentions `l`; `h` is now an
  un-bound key with no replacement and no commit-message
  acknowledgement.

- **`TableState` refactor** (`051c6e11`):
  Pattern is correct (rebuild state per frame; selected drives
  offset). Coverage gap noted at L3 (audit). The per-frame
  `.with_selected(Some(cursor.min(total.saturating_sub(1))))`
  pattern handles `total == 0` cleanly.

- **NRPC pause** (`42c23b56`):
  Mirrors the LOGS pattern correctly. The `owned` variable
  lifetime hack at `app.rs:2828-2842` is awkward but works; not
  worth a finding on its own. Footer chip update is consistent.

- **Blob detail size / replicas** (`c0ff2604`):
  Modal height bumped 21 → 23. On a terminal shorter than ~25
  rows the modal centring extends past the visible area;
  ratatui clips, so the bindings row at the bottom disappears
  first. Not raised — the same height assumption was already
  present at 21 and is bounded.

- **Status-bar peer count** (`82fe51eb`):
  L1 notes the unconditional `healthy = 1` assumption. The
  comment in the code calls out the local-Healthy contract
  explicitly, so this is a contract pin rather than a bug.

---

## Closed

### Substrate / SDK (regression tests included)

| ID  | Commit (short title)                                                                                  |
|-----|-------------------------------------------------------------------------------------------------------|
| H3  | `MeshOS: list_migrations saturates buffered_events at u32::MAX instead of wrapping.`                  |
| M7  | `MeshOS: ProbeRegistry gains clear_locality/health/inventory probes APIs.`                            |

### Deck (no regression tests — TUI render code)

| ID  | Commit (short title)                                                                                  |
|-----|-------------------------------------------------------------------------------------------------------|
| H1  | (batched) `Deck: node_card uses canonical 0x{:x} id + shared HEALTH_GATE_* constants.`                |
| H2  | `Deck: drop Ctrl/Alt/Super-modified alphabetic keys at the on_key entry.`                             |
| M1  | (batched) `Deck: pop_logs_back restores the pre-pivot search, level, and pause state.`                |
| M2  | (batched) `Deck: NRPC table moves STATUS to flex + adds a >=1s catastrophic latency tier.`            |
| M3  | (batched) `Deck: MIGRATION sub-panel adapts the progress bar to cell width + hides on Complete.`      |
| M4  | (batched in H1) `Deck: node_card uses canonical 0x{:x} id + shared HEALTH_GATE_* constants.`          |
| M5  | `Deck: stash dispatch_confirm JoinHandles + await them on shutdown.`                                  |
| M6  | `Deck: sanitize daemon-supplied strings before writing to export files.`                              |
| M8  | `Deck: NRPC render path clones only the visible tail, not the whole ring.`                            |
| L1  | `Deck: peer_summary reads the synthesized local PeerSnapshot's health field.`                         |
| L2  | (batched in M1) `Deck: pop_logs_back restores the pre-pivot search, level, and pause state.`          |
| L4  | `Deck: NRPC seeder decouples method index from call-pair index.`                                      |
| L5  | (batched in M2) `Deck: NRPC table moves STATUS to flex + adds a >=1s catastrophic latency tier.`      |
| L6  | (batched in M2)                                                                                       |
| L7  | (batched in M3) `Deck: MIGRATION sub-panel adapts the progress bar to cell width + hides on Complete.`|
| L8  | `Deck: cluster picker caches the sorted bookmark snapshot at modal open.`                             |
| L9  | `Deck: bookmarks save uses pid+ms tmp suffix so concurrent deck instances don't race.`                |
| L10 | `Deck: tab_bar shows '+N' indicator when the tab strip overflows.`                                    |
| N1  | (batched) `Deck: drop dead u32 casts in blob_detail + soften NRPC empty-state hint.`                  |
| N2  | (batched in N1)                                                                                       |

### Verified no-change

| ID  | Title                                                                                  | Why                                                                                                                                                                                                                                                                            |
|-----|----------------------------------------------------------------------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| L3  | `TableState on the audit tab`                                                          | Verified by reading `tabs/audit.rs` + the AUDIT key paths: the tab has filter state (`audit_force_only`, `audit_limit`, `audit_search`) but no row cursor. The `j`/`k` arms don't apply to AUDIT, so the `TableState` refactor doesn't apply either. Was a misread.            |
| L11 | `Inventory map not scrubbed on PeerDeparted events`                                    | Verified by reading `behavior/meshos/event.rs`: `MeshOsEvent` has no `PeerDeparted` variant. `rtt` / `node_health` / `inventory` are all poll-and-overwrite — there's no event-driven departure semantic for any of them. The probe-driven GC at `event_loop.rs:837-841` already correctly retains-by-probes. Was a misread. |
