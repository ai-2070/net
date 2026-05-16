use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use net_sdk::dataforts::BlobAdapter;
use net_sdk::deck::{DeckClient, MeshOsSnapshot};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    DefaultTerminal, Frame,
};

use crate::{tabs, widgets};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    NetMap,
    Nodes,
    Daemons,
    Groups,
    Dataforts,
    Blobs,
    Logs,
    Audit,
    Replicas,
    Migrations,
    Failures,
}

impl Tab {
    /// Tab order rendered by the tab strip. FAILURES is hidden
    /// (its variant, state, render module, and key handlers all
    /// stay in the codebase so re-enabling is a single-line
    /// addition here). The strip carries 10 slots — keys
    /// `1`..`9` plus `0` — with LOGS pinned to `0` so it stays
    /// reachable from any tab without clobbering the alphabetic
    /// shortcuts.
    pub fn all() -> [Tab; 10] {
        [
            Tab::NetMap,
            Tab::Nodes,
            Tab::Daemons,
            Tab::Groups,
            Tab::Dataforts,
            Tab::Blobs,
            Tab::Migrations,
            Tab::Replicas,
            Tab::Audit,
            Tab::Logs,
        ]
    }

    pub fn label(self) -> &'static str {
        match self {
            Tab::NetMap => "NET.MAP",
            Tab::Nodes => "NODES",
            Tab::Daemons => "DAEMONS",
            Tab::Groups => "GROUPS",
            Tab::Dataforts => "DATAFORTS",
            Tab::Logs => "LOGS",
            Tab::Audit => "AUDIT",
            Tab::Replicas => "CHAINS",
            Tab::Migrations => "MIGRATIONS",
            Tab::Failures => "FAILURES",
            Tab::Blobs => "BLOBS",
        }
    }

    pub fn next(self) -> Tab {
        let all = Self::all();
        // `Tab` has variants beyond `Tab::all()` (e.g. focused-
        // page-only variants); fall back to the head of the
        // cycle instead of panicking when the current tab isn't
        // in the wheel.
        match all.iter().position(|t| *t == self) {
            Some(i) => all[(i + 1) % all.len()],
            None => all[0],
        }
    }

    pub fn prev(self) -> Tab {
        let all = Self::all();
        match all.iter().position(|t| *t == self) {
            Some(i) => all[(i + all.len() - 1) % all.len()],
            None => all[0],
        }
    }
}

pub struct App {
    pub current: Tab,
    pub should_quit: bool,
    pub started: Instant,
    pub tick: u64,
    /// The deck client. Always present — the binary spawns
    /// an in-process runtime at startup. Tabs read snapshot
    /// data through it.
    pub deck: Arc<DeckClient>,
    /// Latest snapshot refreshed on each tick. Wrapped in
    /// `Arc` so cloning into per-tab scope is one
    /// atomic-refcount op. Tabs check whether the snapshot's
    /// collections are empty to decide between live and
    /// fixture rendering paths.
    pub snapshot: Arc<MeshOsSnapshot>,
    /// Phase-4 streaming tail for LOGS. Fed by a background
    /// `subscribe_logs` task; the LOGS render path reads from
    /// this buffer instead of `snapshot.log_ring`, so the
    /// operator's session can outlive the substrate ring cap.
    pub logs_tail: crate::streams::LogsTail,
    /// Phase-4 streaming tail for AUDIT. Same shape as
    /// `logs_tail`; replaces the `snapshot.admin_audit`
    /// dependency on the AUDIT render path.
    pub audit_tail: crate::streams::AuditTail,
    /// Phase-4 streaming tail for FAILURES. Backs the FAILURES
    /// tab — executor rejections, drain failures, constraint
    /// drops.
    pub failures_tail: crate::streams::FailuresTail,
    /// Registered blob adapters. DATAFORTS lists them at the
    /// top of the tab; the cursored adapter drives the detail
    /// body. Empty when no adapter is wired (the tab shows
    /// its "no adapter wired" empty state).
    pub blob_adapters: Vec<Arc<net_sdk::dataforts::MeshBlobAdapter>>,
    /// Cursor on the DATAFORTS adapter list.
    pub dataforts_cursor: usize,
    /// BLOBS inventory tail — periodically refreshed from
    /// `MeshBlobAdapter::list(...)`. Empty when no adapter is
    /// wired; the BLOBS tab shows its empty state in that
    /// case.
    pub blobs_tail: crate::streams::BlobsTail,
    /// Cursor on the BLOBS tab — index into the visible
    /// (filtered) projection of `blobs_tail.snapshot()`.
    pub blobs_cursor: usize,
    /// BLOBS substring search. Matches hash prefix; empty =
    /// no filter.
    pub blobs_search: String,
    /// When `true`, keystrokes go into `blobs_search`.
    pub blobs_search_editing: bool,
    /// Cluster bookmark store — loaded from
    /// `$XDG_CONFIG_HOME/deck/bookmarks.toml` at startup.
    /// Surfaced through the cluster picker modal (`:` to open).
    pub bookmarks: crate::bookmarks::BookmarkStore,
    /// Active cluster identity. `"local"` for the in-process
    /// runtime the binary spawned at startup; future remote
    /// connections will set this to the bookmark name. Today
    /// switching to a non-`"local"` value is gated by the
    /// substrate RPC slice — the picker surfaces a toast
    /// rather than misleadingly succeeding.
    pub active_cluster: String,
    /// The substrate runtime's local node id, plumbed from the
    /// harness at startup. Used everywhere the UI synthesizes
    /// or attributes per-node state to "this node"
    /// (placement-based pivots, admin commits, local-datafort
    /// node card) so the deck never hardcodes a literal that
    /// can drift from the actual `MeshOsConfig::this_node`.
    pub this_node: net_sdk::meshos::NodeId,
    /// Cursor on the GROUPS tab's lineage tree. Indices into
    /// the live group list — `j`/`k` move the cursor; the
    /// detail pane on the right reflects whichever member is
    /// pointed to.
    pub groups_cursor: DaemonCursor,
    /// Cursor on the DAEMONS tab — flat index into the daemon
    /// list in lineage-group order (same order the table
    /// renders). `Enter` opens the NODE page for the cursored
    /// daemon's placement.
    pub daemons_cursor: usize,
    /// Cursor on the NET.MAP tab — index into the same
    /// peers-sorted-by-id order the LIST tab uses, so the
    /// cursor stays semantically aligned across the two
    /// node-centric tabs. `Enter` opens the node detail
    /// modal.
    pub netmap_cursor: usize,
    /// Cursor on the LIST tab's nodes table — index into the
    /// peers map's sorted key order. `j`/`k` moves it; the
    /// row gets a `▶` marker + brighter id styling. Action
    /// bindings (`c` cordon, `C` uncordon, future drain)
    /// target the cursored node.
    pub nodes_cursor: usize,
    /// Cursor on the CHAINS tab — index into the replicas
    /// map's sorted-by-chain order.
    pub replica_cursor: usize,
    /// Cursor on the MIGRATIONS tab — index into
    /// `snapshot.in_flight_migrations`.
    pub migration_cursor: usize,
    /// Cursor on the FAILURES tab — index into the failures
    /// tail. 0 = newest record (since the projection reverses
    /// the buffer for display).
    pub failures_cursor: usize,
    /// AUDIT tab filter: show only ICE force-* records when true.
    pub audit_force_only: bool,
    /// AUDIT tab filter: cap the visible rows. `None` shows
    /// the full ring; values cycle via `[n]` on the AUDIT tab.
    pub audit_limit: Option<usize>,
    /// LOGS tab filter: minimum log level to project. Cycled
    /// via `[f]` on the LOGS tab through Info → Warn → Error
    /// → Debug → Info.
    pub logs_min_level: net_sdk::deck::LogLevel,
    /// LOGS tab pause: when `Some`, the log grid renders this
    /// frozen Vec instead of the streaming tail. Toggled via
    /// `[p]` on the LOGS tab. Other tabs keep using the live
    /// snapshot — only the log tail is paused.
    pub logs_paused: Option<Vec<net_sdk::deck::LogRecord>>,
    /// LOGS tab substring filter applied to record messages.
    /// Empty = no filter. Edited via `[/]`; survives switching
    /// off the LOGS tab until explicitly cleared.
    pub logs_search: String,
    /// When `true`, keystrokes go into `logs_search` instead of
    /// the normal binding table. Toggled via `[/]` (enter) and
    /// `Enter`/`Esc` (exit; Esc also clears the buffer).
    pub logs_search_editing: bool,
    /// AUDIT tab substring search. Matches against command name,
    /// operator IDs, and the rendered target text. Edited via
    /// `[/]` on the AUDIT tab.
    pub audit_search: String,
    /// When `true`, keystrokes go into `audit_search` instead of
    /// the normal binding table.
    pub audit_search_editing: bool,
    /// FAILURES tab substring search. Matches against the source
    /// token and the reason string.
    pub failures_search: String,
    /// When `true`, keystrokes go into `failures_search` instead
    /// of the normal binding table.
    pub failures_search_editing: bool,
    /// Active modal overlay (confirmation prompt, future
    /// signature collector, future help screen). When `Some`,
    /// the modal absorbs key input until dismissed.
    pub modal: Option<Modal>,
    /// Focused node — when `Some`, the body of the active
    /// tab is replaced with a full-page node detail view of
    /// the peer with this id. Set by `[Enter]` on NODES,
    /// NET.MAP, DATAFORTS, or a Daemon-page placement row;
    /// cleared by `[Esc]`.
    pub node_focus: Option<crate::tabs::node_page::NodeFocusEntry>,
    /// Focused daemon — same shape as `node_focus` but for the
    /// Daemon page. Mutually exclusive with `node_focus`; each
    /// `focus_*` helper clears the other before setting.
    pub daemon_focus: Option<crate::tabs::daemon_page::DaemonFocusEntry>,
    /// Ephemeral "toast" message shown in the footer for
    /// ~3 seconds after an action. Used for confirming
    /// side-effects the operator can't see directly — e.g.
    /// `[w]` exports report "wrote N records to <path>" so
    /// the operator knows the file landed without leaving
    /// the TUI.
    pub toast: Option<(String, Instant)>,
    /// Sender side of the spawn-back toast channel. Cloned into
    /// every detached admin dispatch task so failed simulate /
    /// commit calls surface as a footer toast instead of being
    /// silently dropped.
    pub toast_tx: std::sync::mpsc::Sender<String>,
    /// Receiver side; drained by the tick loop into `toast`.
    pub toast_rx: std::sync::mpsc::Receiver<String>,
}

#[derive(Clone, Debug)]
pub enum Modal {
    Confirm(crate::widgets::confirm::ConfirmAction),
    /// Help overlay — full binding reference. Dismissed with
    /// `?` (toggle), `Esc`, or `q`.
    Help,
    /// Node picker — `j/k` to cursor through peers, `Enter`
    /// transitions to a `Confirm` modal with the cursored
    /// peer baked into the action.
    PickNode {
        purpose: crate::widgets::pick_node::PickNodePurpose,
        cursor: usize,
    },
    /// Duration-input prompt — operator types a value with
    /// `s`/`m`/`h` units. `Enter` parses and transitions to a
    /// `Confirm` modal; parse failures stash an `error` on the
    /// modal and the operator can keep editing.
    ParamInput {
        purpose: crate::widgets::param_input::ParamInputPurpose,
        buffer: String,
        error: Option<String>,
    },
    /// Cluster picker — lists `"local"` + the bookmark store's
    /// entries. `j`/`k` to cursor, `Enter` to select. Selecting
    /// `"local"` is a no-op (already active); selecting a
    /// bookmark today toasts a deferred-feature notice because
    /// the substrate RPC slice isn't landed yet.
    ClusterPicker {
        cursor: usize,
    },
    /// Blob detail — opened with `[Enter]` on the BLOBS tab.
    /// Snapshots the cursored entry into the modal so a
    /// subsequent inventory refresh under the cursor doesn't
    /// shift the body. Dismissed with the usual `Esc` / `q`.
    BlobDetail {
        entry: net_sdk::dataforts::BlobInventoryEntry,
        /// Node hosting this blob. Threaded in so the modal can
        /// label the holder and `[Enter]` can jump straight to
        /// the NODE page for it. Always the local datafort
        /// (`App::this_node`) today — BLOBS sources from local
        /// adapters; cross-node attribution lands with the
        /// remote inventory probe.
        host_id: u64,
        host_label: Option<String>,
    },
    /// Export confirmation — pops after `[e]` lands a file
    /// on disk so the operator sees the resolved path before
    /// returning to the tab. Carries the outcome (success
    /// with path + count, or failure with error string).
    ExportDone {
        outcome: crate::widgets::export_done::ExportOutcome,
    },
}

/// Internal helper enum used by `propose_node_action` to
/// pick which `ConfirmAction` variant to build. Keeps the key
/// handler short.
enum NodeActionKind {
    Cordon,
    Uncordon,
    /// Drain with a fixed 5-minute window. Future UX: a
    /// `[D]` "drain with custom window" prompt that takes a
    /// numeric input.
    Drain,
    /// Indefinite maintenance window — no auto-exit. The
    /// modal passes `drain_for = None`, deferring to the
    /// cluster's configured default deadline.
    EnterMaintenance,
    ExitMaintenance,
    ClearAvoidList,
    InvalidatePlacement,
}

/// Map a lowercase action keypress to its NodeActionKind. Used
/// by both the NODES tab dispatcher and the NODE-page focus
/// handler so the bindings stay aligned between the list and
/// the dedicated page.
fn node_action_for(code: KeyCode) -> Option<NodeActionKind> {
    match code {
        KeyCode::Char('c') => Some(NodeActionKind::Cordon),
        KeyCode::Char('C') => Some(NodeActionKind::Uncordon),
        KeyCode::Char('d') => Some(NodeActionKind::Drain),
        KeyCode::Char('m') => Some(NodeActionKind::EnterMaintenance),
        KeyCode::Char('M') => Some(NodeActionKind::ExitMaintenance),
        KeyCode::Char('a') => Some(NodeActionKind::ClearAvoidList),
        KeyCode::Char('i') => Some(NodeActionKind::InvalidatePlacement),
        _ => None,
    }
}

/// ICE commit pipeline shared by every ICE variant: simulate
/// (binds `issued_at_ms` + `blast_hash`), sign with the
/// deck's operator identity, commit the signed bundle. Errors
/// surface in the audit ring as `Rejected` entries; on
/// success the audit row reads `Accepted`. Failures at the
/// simulate / commit boundary also flow into `toast_tx` so
/// the operator sees the rejection in the footer immediately
/// instead of waiting for the audit ring to update.
async fn dispatch_ice(
    deck: &Arc<DeckClient>,
    proposal: net_sdk::deck::IceProposal<'_>,
    kind: &str,
    toast_tx: std::sync::mpsc::Sender<String>,
) {
    let simulated = match proposal.simulate().await {
        Ok(s) => s,
        Err(err) => {
            let _ = toast_tx.send(format!("ICE {kind} rejected: simulate failed — {err}"));
            return;
        }
    };
    let sig = deck.identity().sign_proposal(
        simulated.action(),
        simulated.issued_at_ms(),
        &simulated.blast_hash(),
    );
    if let Err(err) = simulated.commit(&[sig]).await {
        let _ = toast_tx.send(format!("ICE {kind} rejected: commit failed — {err}"));
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DaemonCursor {
    pub group: usize,
    pub member: usize,
}

/// Synthetic per-node greedy config used while the substrate has
/// no remote-greedy probe. Values are deterministically derived
/// from the node id so the deck shows varied (but stable) configs
/// across peers in samples mode. Locked defaults match
/// `GreedyConfig::default()` from the dataforts module.
fn synthetic_greedy_view(node_id: u64, label: Option<&'static str>) -> tabs::dataforts::GreedyView {
    // Cheap hash off the id so each peer gets a stable but
    // distinct config slot.
    let h = (node_id ^ (node_id >> 17)) as usize;
    let proximity_max_rtt_ms: u64 = match h % 4 {
        0 => 100,
        1 => 200, // matches GreedyConfig default
        2 => 350,
        _ => 500,
    };
    let total_cap_bytes: u64 = match h % 3 {
        0 => 4 * (1u64 << 30),  // 4 GiB
        1 => 10 * (1u64 << 30), // GreedyConfig default (10 GiB)
        _ => 32 * (1u64 << 30), // 32 GiB
    };
    let per_channel_cap_bytes: u64 = 100 * (1u64 << 20); // 100 MiB default
    let bandwidth_budget_fraction: f32 = match h % 4 {
        0 => 0.15,
        1 => 0.25, // default
        2 => 0.40,
        _ => 0.60,
    };
    let nic_peak_bytes_per_s: u64 = 125_000_000; // 1 Gbps default
                                                 // Scopes derive from the label (region suffix) when present
                                                 // so the demo reads "scopes: region:ap-south1" etc.
    let scopes: Vec<String> = match label {
        Some(l) if l.starts_with("eu-") || l.starts_with("us-") || l.starts_with("ap-") => {
            vec![format!("region:{l}")]
        }
        Some("gpu-rig") => vec!["intent:compute".to_string(), "region:any".to_string()],
        Some("edge") | Some("lab-bench") => vec!["intent:sensor".to_string()],
        _ => Vec::new(),
    };
    let (colocation, intent_match) = if h.is_multiple_of(2) {
        ("SoftPreference", "AnyOfLocalCapabilities")
    } else {
        ("Strict", "Strict")
    };
    tabs::dataforts::GreedyView {
        proximity_max_rtt_ms,
        per_channel_cap_bytes,
        total_cap_bytes,
        bandwidth_budget_fraction,
        nic_peak_bytes_per_s,
        scopes,
        colocation,
        intent_match,
        observer_inflight_cap: 1024,
    }
}

impl App {
    pub fn new(
        deck: Arc<DeckClient>,
        logs_tail: crate::streams::LogsTail,
        audit_tail: crate::streams::AuditTail,
        failures_tail: crate::streams::FailuresTail,
        blob_adapters: Vec<Arc<net_sdk::dataforts::MeshBlobAdapter>>,
        blobs_tail: crate::streams::BlobsTail,
        bookmarks: crate::bookmarks::BookmarkStore,
        this_node: net_sdk::meshos::NodeId,
    ) -> Self {
        let snapshot = Arc::new(deck.status());
        let (toast_tx, toast_rx) = std::sync::mpsc::channel();
        Self {
            current: Tab::NetMap,
            logs_tail,
            audit_tail,
            failures_tail,
            blob_adapters,
            dataforts_cursor: 0,
            blobs_tail,
            blobs_cursor: 0,
            blobs_search: String::new(),
            blobs_search_editing: false,
            bookmarks,
            active_cluster: "local".to_string(),
            this_node,
            should_quit: false,
            started: Instant::now(),
            tick: 0,
            deck,
            snapshot,
            groups_cursor: DaemonCursor::default(),
            daemons_cursor: 0,
            netmap_cursor: 0,
            nodes_cursor: 0,
            replica_cursor: 0,
            migration_cursor: 0,
            failures_cursor: 0,
            audit_force_only: false,
            audit_limit: None,
            logs_min_level: net_sdk::deck::LogLevel::Info,
            logs_paused: None,
            logs_search: String::new(),
            logs_search_editing: false,
            audit_search: String::new(),
            audit_search_editing: false,
            failures_search: String::new(),
            failures_search_editing: false,
            modal: None,
            node_focus: None,
            daemon_focus: None,
            toast: None,
            toast_tx,
            toast_rx,
        }
    }

    pub fn run(mut self, mut terminal: DefaultTerminal) -> color_eyre::Result<()> {
        let tick_rate = Duration::from_millis(120);
        let mut last_tick = Instant::now();
        while !self.should_quit {
            terminal.draw(|f| self.draw(f))?;
            let timeout = tick_rate.saturating_sub(last_tick.elapsed());
            if event::poll(timeout)? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press {
                        self.on_key(key.code, key.modifiers);
                    }
                }
            }
            if last_tick.elapsed() >= tick_rate {
                self.tick = self.tick.wrapping_add(1);
                self.refresh_snapshot();
                self.drain_toast_channel();
                self.expire_toast();
                last_tick = Instant::now();
            }
        }
        Ok(())
    }

    fn refresh_snapshot(&mut self) {
        self.snapshot = Arc::new(self.deck.status());
    }

    /// Drain any toasts queued by detached dispatch tasks; the
    /// most recent message wins (matches `set_toast`'s "latest
    /// action overwrites" rule).
    fn drain_toast_channel(&mut self) {
        let mut latest: Option<String> = None;
        while let Ok(msg) = self.toast_rx.try_recv() {
            latest = Some(msg);
        }
        if let Some(msg) = latest {
            self.set_toast(msg);
        }
    }

    /// 3-second decay on toast messages so a confirmation
    /// doesn't sit on screen forever; new actions overwrite
    /// stale toasts immediately via [`Self::set_toast`].
    fn expire_toast(&mut self) {
        if let Some((_, t)) = self.toast.as_ref() {
            if t.elapsed() >= Duration::from_secs(3) {
                self.toast = None;
            }
        }
    }

    /// Set the footer's ephemeral message. Replaces any prior
    /// toast — the latest action's confirmation always wins.
    pub fn set_toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    /// Resolve a node id to its human label using the chained
    /// lookup in `crate::nodes::label_for`: fixture first, then
    /// scoped caps, then first plain cap. Falls back to the
    /// fixture-only `label_of` when the peer isn't in the
    /// snapshot (e.g. the local node `App::this_node`).
    pub fn node_label(&self, id: u64) -> Option<String> {
        let id_hex = format!("0x{id:x}");
        if let Some(peer) = self.snapshot.peers.get(&id) {
            crate::nodes::label_for(&id_hex, &peer.capability_set)
        } else {
            crate::nodes::label_of(&id_hex).map(|s| s.to_string())
        }
    }

    /// `0x{id:x}.{label}` if the chain returns a label, bare
    /// hex otherwise. Used by the confirm-modal proposals to
    /// stamp the action target in a way the operator recognises.
    pub fn node_display(&self, id: u64) -> String {
        let suffix = self
            .node_label(id)
            .map(|l| format!(".{l}"))
            .unwrap_or_default();
        format!("0x{id:x}{suffix}")
    }

    /// Focus the node at `peer_index` in the snapshot's
    /// peers-by-id order. Peers iterate the BTreeMap order, so
    /// this matches both LIST and NET.MAP cursor semantics —
    /// the index is whichever cursor the caller passes.
    /// Snapshots the `PeerSnapshot` so the upper page body
    /// stays stable across a subsequent tick under the focused
    /// id.
    fn focus_node(&mut self, peer_index: usize) {
        let pair = self
            .snapshot
            .peers
            .iter()
            .nth(peer_index)
            .map(|(id, p)| (*id, p.clone()));
        if let Some((id, peer)) = pair {
            let label = crate::nodes::label_for(&format!("0x{id:x}"), &peer.capability_set);
            self.daemon_focus = None;
            self.node_focus = Some(crate::tabs::node_page::NodeFocusEntry {
                id,
                label,
                peer,
                placement_cursor: 0,
            });
        }
    }

    /// Open the Daemon page focused on the cursored daemon in
    /// the GROUPS tab. Resolves the daemon via its grouped
    /// position in the GROUPS list (same indexing the tab
    /// renders).
    fn focus_groups_cursored_daemon(&mut self) {
        let groups = crate::lineage::group_daemons(&self.snapshot.daemons);
        let Some(group) = groups.get(self.groups_cursor.group) else {
            return;
        };
        let Some(member) = group.members.get(self.groups_cursor.member) else {
            return;
        };
        self.focus_daemon(member.id, member.daemon.clone());
    }

    /// Open the Daemon page on `id`, snapshotting the daemon at
    /// focus time so the facts pane stays stable across ticks.
    /// Clears `node_focus` so the page replaces, not stacks on,
    /// any previously-focused node view.
    fn focus_daemon(&mut self, id: u64, snapshot: net_sdk::deck::DaemonSnapshot) {
        self.node_focus = None;
        self.daemon_focus = Some(crate::tabs::daemon_page::DaemonFocusEntry {
            id,
            snapshot,
            cursor: 0,
        });
    }

    /// Resolve the cursored daemon in the flat DAEMONS tab and
    /// open its Daemon page. Mirrors `focus_groups_cursored_daemon`
    /// for the GROUPS lineage view.
    fn focus_daemons_cursored(&mut self) {
        let mut idx = 0usize;
        let groups = crate::lineage::group_daemons(&self.snapshot.daemons);
        for g in &groups {
            for m in &g.members {
                if idx == self.daemons_cursor {
                    self.focus_daemon(m.id, m.daemon.clone());
                    return;
                }
                idx += 1;
            }
        }
    }

    /// Walk the Daemon page's group-row cursor up / down.
    /// Cursor 0 = placement node, 1..=N = sibling at index N-1.
    fn step_daemon_focus_cursor(&mut self, delta: i32) {
        let Some(focus) = self.daemon_focus.as_ref() else {
            return;
        };
        let rows = crate::tabs::daemon_page::group_rows(focus, &self.snapshot);
        if rows.is_empty() {
            return;
        }
        let cur = focus.cursor as i64 + delta as i64;
        let last = rows.len().saturating_sub(1) as i64;
        let next = cur.clamp(0, last) as usize;
        if let Some(f) = self.daemon_focus.as_mut() {
            f.cursor = next;
        }
    }

    /// Dispatch `[Enter]` on the Daemon page: placement-node row
    /// opens the Node page; sibling daemon row swaps focus to
    /// that daemon's page.
    fn dispatch_daemon_focus_enter(&mut self) {
        let Some(focus) = self.daemon_focus.as_ref() else {
            return;
        };
        let rows = crate::tabs::daemon_page::group_rows(focus, &self.snapshot);
        let cursor = focus.cursor.min(rows.len().saturating_sub(1));
        let Some(row) = rows.get(cursor) else { return };
        match row {
            crate::tabs::daemon_page::GroupRow::PlacementNode { id } => {
                let id = *id;
                let label = self.node_label(id);
                self.focus_host(id, label);
            }
            crate::tabs::daemon_page::GroupRow::Sibling { id } => {
                let id = *id;
                if let Some(d) = self.snapshot.daemons.get(&id) {
                    self.focus_daemon(id, d.clone());
                }
            }
        }
    }

    /// Walk the Node page's placement cursor through the daemons
    /// running on the focused node.
    fn step_node_placement_cursor(&mut self, delta: i32) {
        let Some(focus) = self.node_focus.as_ref() else {
            return;
        };
        let daemons = crate::tabs::node_page::daemons_on(&self.snapshot, focus.id);
        if daemons.is_empty() {
            return;
        }
        let cur = focus.placement_cursor as i64 + delta as i64;
        let last = daemons.len().saturating_sub(1) as i64;
        let next = cur.clamp(0, last) as usize;
        if let Some(f) = self.node_focus.as_mut() {
            f.placement_cursor = next;
        }
    }

    /// Open the cursored placement daemon's Daemon page from the
    /// Node page focus.
    fn open_cursored_node_placement(&mut self) {
        let Some(focus) = self.node_focus.as_ref() else {
            return;
        };
        let daemons = crate::tabs::node_page::daemons_on(&self.snapshot, focus.id);
        if daemons.is_empty() {
            return;
        }
        let cursor = focus.placement_cursor.min(daemons.len() - 1);
        let (id, d) = daemons[cursor];
        let d = d.clone();
        self.focus_daemon(id, d);
    }

    /// Open the NODE page on `host_id`. Used by the blob-detail
    /// modal's `[Enter]` so an operator inspecting a chunk can
    /// jump straight to the host node. Mirrors `focus_node` but
    /// takes an explicit id rather than a peer-index — the host
    /// is often the local node, which doesn't live in
    /// `snapshot.peers`.
    fn focus_host(&mut self, host_id: u64, host_label: Option<String>) {
        // Mirror `focus_daemon`'s mutual-exclusion: opening the
        // Node page drops any Daemon-page focus so the render
        // dispatcher (which checks daemon_focus first) doesn't
        // keep showing the old page over the new one.
        self.daemon_focus = None;
        if host_id == self.this_node {
            // Synthesize a PeerSnapshot for the local node (same
            // as `focus_datafort` for the local datafort).
            let local = self.local_datafort();
            let mut caps = std::collections::BTreeSet::new();
            for c in &local.capabilities {
                caps.insert(c.clone());
            }
            let peer = net_sdk::deck::PeerSnapshot {
                health: Some(net_sdk::deck::PeerHealthSnapshot::Healthy),
                cpu_load_1m: local.cpu_load_1m,
                mem_used_bytes: local.mem_used_bytes,
                mem_total_bytes: local.mem_total_bytes,
                disk_used_bytes: local.disk_used_bytes,
                disk_total_bytes: local.disk_total_bytes,
                capability_set: caps,
                software_version: Some("0.17.0".to_string()),
                ..Default::default()
            };
            self.node_focus = Some(crate::tabs::node_page::NodeFocusEntry {
                id: host_id,
                label: host_label,
                peer,
                placement_cursor: 0,
            });
        } else if let Some((id, peer)) =
            self.snapshot.peers.iter().find(|(pid, _)| **pid == host_id)
        {
            let label = host_label
                .or_else(|| crate::nodes::label_for(&format!("0x{:x}", *id), &peer.capability_set));
            self.node_focus = Some(crate::tabs::node_page::NodeFocusEntry {
                id: *id,
                label,
                peer: peer.clone(),
                placement_cursor: 0,
            });
        }
    }

    /// Snapshot of the deck's local node as a `NodeCardView`.
    /// Used by the DAEMONS detail panel when the cursored
    /// daemon's placement is the local node — that id isn't in
    /// `snapshot.peers`, so we synthesize it here from the same
    /// data the DATAFORTS local row reads.
    fn local_node_card(&self) -> crate::widgets::node_card::NodeCardView {
        let local = self.local_datafort();
        crate::widgets::node_card::NodeCardView {
            id: self.this_node,
            label: Some("local".to_string()),
            is_local: true,
            health: Some("Healthy"),
            cpu_load_1m: local.cpu_load_1m,
            mem_used_bytes: local.mem_used_bytes,
            mem_total_bytes: local.mem_total_bytes,
            disk_used_bytes: local.disk_used_bytes,
            disk_total_bytes: local.disk_total_bytes,
            capabilities: local.capabilities.clone(),
        }
    }

    /// Build the DATAFORT view rendered on the NODE page for the
    /// focused peer. Local datafort gets the full adapter list;
    /// remote dataforts surface only the aggregate disk + the
    /// `dataforts.*` cap tags (no remote-adapter probe today).
    fn datafort_view_for(&self, node_id: u64) -> tabs::node_page::DatafortView {
        if node_id == self.this_node {
            let adapters: Vec<tabs::node_page::DatafortAdapterRow> = self
                .blob_adapters
                .iter()
                .map(|a| {
                    let m = a.metrics().snapshot();
                    tabs::node_page::DatafortAdapterRow {
                        id: a.adapter_id().to_string(),
                        disk_used_bytes: m.disk_used_bytes,
                        disk_capacity_bytes: m.disk_capacity_bytes,
                        overflow_enabled: a.overflow_enabled(),
                        overflow_active: m.overflow.active,
                    }
                })
                .collect();
            let (disk_used, disk_total) = adapters.iter().fold((0u64, 0u64), |(u, t), a| {
                (u + a.disk_used_bytes, t + a.disk_capacity_bytes)
            });
            let overflow_enabled = adapters.iter().any(|a| a.overflow_enabled);
            let overflow_active = adapters.iter().any(|a| a.overflow_active);
            tabs::node_page::DatafortView {
                is_local: true,
                disk_used_bytes: Some(disk_used),
                disk_total_bytes: Some(disk_total),
                overflow_enabled,
                overflow_active,
                adapters,
                greedy: None,
            }
        } else if let Some((_, peer)) = self.snapshot.peers.iter().find(|(id, _)| **id == node_id) {
            let has_greedy = peer
                .capability_set
                .iter()
                .any(|c| c == "greedy.cache" || c == "dataforts.greedy.cache");
            // Fixture-only here — synthetic_greedy_view derives
            // a scope set keyed off `region:` / `gpu-rig` etc.
            // and uses the static fixture's vocabulary.
            let label = crate::nodes::label_of(&format!("0x{node_id:x}"));
            let greedy = if has_greedy {
                Some(synthetic_greedy_view(node_id, label))
            } else {
                None
            };
            tabs::node_page::DatafortView {
                is_local: false,
                disk_used_bytes: peer.disk_used_bytes,
                disk_total_bytes: peer.disk_total_bytes,
                overflow_enabled: peer
                    .capability_set
                    .iter()
                    .any(|c| c == "dataforts.blob.overflow"),
                overflow_active: false,
                adapters: Vec::new(),
                greedy,
            }
        } else {
            tabs::node_page::DatafortView::default()
        }
    }

    /// Focus the NODE page on the datafort at `idx` in the
    /// dataforts list. For the local datafort the deck
    /// synthesizes a `PeerSnapshot` from the same view the
    /// DATAFORTS tab renders (the local node isn't in
    /// `snapshot.peers`); for a remote datafort we look the
    /// peer up by id.
    fn focus_datafort(&mut self, idx: usize) {
        let entries = self.collect_dataforts();
        let Some(entry) = entries.get(idx) else {
            return;
        };
        self.daemon_focus = None;
        if entry.is_local {
            let mut caps = std::collections::BTreeSet::new();
            for c in &entry.capabilities {
                caps.insert(c.clone());
            }
            let peer = net_sdk::deck::PeerSnapshot {
                health: Some(net_sdk::deck::PeerHealthSnapshot::Healthy),
                cpu_load_1m: entry.cpu_load_1m,
                mem_used_bytes: entry.mem_used_bytes,
                mem_total_bytes: entry.mem_total_bytes,
                disk_used_bytes: entry.disk_used_bytes,
                disk_total_bytes: entry.disk_total_bytes,
                capability_set: caps,
                software_version: Some("0.17.0".to_string()),
                ..Default::default()
            };
            self.node_focus = Some(crate::tabs::node_page::NodeFocusEntry {
                id: entry.id,
                label: entry.label.clone(),
                peer,
                placement_cursor: 0,
            });
        } else if let Some((id, peer)) = self
            .snapshot
            .peers
            .iter()
            .find(|(pid, _)| **pid == entry.id)
        {
            let label = crate::nodes::label_for(&format!("0x{:x}", *id), &peer.capability_set);
            self.node_focus = Some(crate::tabs::node_page::NodeFocusEntry {
                id: *id,
                label,
                peer: peer.clone(),
                placement_cursor: 0,
            });
        }
    }

    /// Build the DATAFORTS list for the current frame. Always
    /// starts with the local datafort (the deck's host node +
    /// its wired adapters), then appends every peer that
    /// advertises a dataforts capability — blob storage or
    /// greedy cache — as a remote datafort.
    fn collect_dataforts(&self) -> Vec<tabs::dataforts::DatafortEntry> {
        let mut out: Vec<tabs::dataforts::DatafortEntry> = Vec::new();
        out.push(self.local_datafort());
        for (id, p) in self.snapshot.peers.iter() {
            let has_blob = p
                .capability_set
                .iter()
                .any(|c| c == "dataforts.blob.storage");
            let has_greedy = p
                .capability_set
                .iter()
                .any(|c| c == "greedy.cache" || c == "dataforts.greedy.cache");
            if !has_blob && !has_greedy {
                continue;
            }
            let label = crate::nodes::label_for(&format!("0x{:x}", *id), &p.capability_set);
            let health = match p.health {
                Some(net_sdk::deck::PeerHealthSnapshot::Healthy) => Some("Healthy"),
                Some(net_sdk::deck::PeerHealthSnapshot::Degraded) => Some("Degraded"),
                Some(net_sdk::deck::PeerHealthSnapshot::Unreachable) => Some("Unreachable"),
                _ => None,
            };
            // synthetic_greedy_view takes the static fixture
            // vocabulary; pass the bare fixture label, not the
            // chained one, so scope tags stay coherent.
            let greedy_fixture_label = crate::nodes::label_of(&format!("0x{:x}", *id));
            let greedy = if has_greedy {
                Some(synthetic_greedy_view(*id, greedy_fixture_label))
            } else {
                None
            };
            out.push(tabs::dataforts::DatafortEntry {
                id: *id,
                label,
                is_local: false,
                health,
                cpu_load_1m: p.cpu_load_1m,
                mem_used_bytes: p.mem_used_bytes,
                mem_total_bytes: p.mem_total_bytes,
                disk_used_bytes: p.disk_used_bytes,
                disk_total_bytes: p.disk_total_bytes,
                capabilities: p.capability_set.iter().cloned().collect(),
                adapters: Vec::new(),
                greedy,
            });
        }
        out
    }

    /// The local datafort: synthetic node stats + the actual
    /// per-adapter snapshots from `self.blob_adapters`. Disk
    /// aggregates across every wired adapter so the node-level
    /// gauge reflects the host's total blob footprint.
    fn local_datafort(&self) -> tabs::dataforts::DatafortEntry {
        let adapters: Vec<tabs::dataforts::AdapterEntry> = self
            .blob_adapters
            .iter()
            .map(|a| {
                let metrics = a.metrics().snapshot();
                let overflow_enabled = a.overflow_enabled();
                tabs::dataforts::AdapterEntry {
                    id: a.adapter_id().to_string(),
                    metrics,
                    overflow_enabled,
                }
            })
            .collect();
        let (disk_used, disk_total) = adapters.iter().fold((0u64, 0u64), |(u, t), a| {
            (
                u + a.metrics.disk_used_bytes,
                t + a.metrics.disk_capacity_bytes,
            )
        });
        let any_overflow = adapters.iter().any(|a| a.overflow_enabled);
        let mut capabilities = vec![
            "compute.daemon".to_string(),
            "meshos.health".to_string(),
            "dataforts.blob.storage".to_string(),
        ];
        if any_overflow {
            capabilities.push("dataforts.blob.overflow".to_string());
        }
        tabs::dataforts::DatafortEntry {
            id: self.this_node,
            label: Some("local".to_string()),
            is_local: true,
            health: Some("Healthy"),
            cpu_load_1m: Some(0.42),
            mem_used_bytes: Some(28u64 << 30),
            mem_total_bytes: Some(64u64 << 30),
            disk_used_bytes: Some(disk_used),
            disk_total_bytes: Some(disk_total),
            capabilities,
            adapters,
            // Local datafort is blob-only today; greedy isn't
            // wired into the deck runtime.
            greedy: None,
        }
    }

    /// Snapshot the cursored BLOBS entry into a detail modal.
    /// The modal owns its copy of the entry so a subsequent
    /// inventory refresh (~500 ms tick) under the cursor
    /// doesn't shift the body the operator is reading.
    fn open_blob_detail(&mut self) {
        let entries = self.blobs_tail.snapshot();
        if entries.is_empty() {
            return;
        }
        let needle = self.blobs_search.to_ascii_lowercase();
        // Apply the same filter the render path uses so the
        // cursor + modal stay coherent with the visible rows.
        let visible: Vec<_> = entries
            .iter()
            .filter(|e| tabs::blobs::record_matches(e, &needle))
            .cloned()
            .collect();
        let idx = self.blobs_cursor.min(visible.len().saturating_sub(1));
        if let Some(entry) = visible.get(idx) {
            // BLOBS sources from the local adapters today, so
            // every entry's host is `this_node`. When remote
            // adapter probes land, populate this from the entry.
            self.modal = Some(Modal::BlobDetail {
                entry: entry.clone(),
                host_id: self.this_node,
                host_label: Some("local".to_string()),
            });
        }
    }

    /// Cluster-picker selection: index 0 is the always-present
    /// `"local"` entry; subsequent indices map to the sorted
    /// bookmark list. Selecting `local` is a no-op (already
    /// active); selecting a remote bookmark surfaces a toast
    /// noting the substrate RPC slice is required — the
    /// connection itself can't dial until the wire layer lands.
    fn commit_cluster_pick(&mut self, cursor: usize) {
        if cursor == 0 {
            // Already on local — no-op feedback.
            if self.active_cluster != "local" {
                self.active_cluster = "local".to_string();
                self.set_toast("switched to local cluster");
            }
            return;
        }
        let sorted: Vec<crate::bookmarks::Bookmark> =
            self.bookmarks.sorted().into_iter().cloned().collect();
        let Some(bm) = sorted.get(cursor - 1) else {
            return;
        };
        // Real switch requires the substrate's deck-RPC slice
        // (DECK_PLAN.md § Deferred work § Multi-Cluster
        // Switcher). The picker UX exists today so operators
        // can manage bookmarks; the dial happens when the
        // substrate slot lands.
        self.set_toast(format!(
            "remote cluster '{}' — substrate RPC slice required",
            bm.name
        ));
    }

    /// Export the LOGS view to a file. Applies the same filter
    /// stack the render path uses (level threshold + substring
    /// search) so the export reflects what the operator sees;
    /// pause state determines live-vs-frozen source. Confirms
    /// success or failure in the footer toast.
    /// Wrap an export result into the `ExportDone` modal so
    /// the operator sees the resolved path immediately — toasts
    /// are too easy to miss in a busy session, and the path is
    /// the actionable bit (operator copies it into the incident
    /// write-up).
    fn open_export_modal(
        &mut self,
        tab: &str,
        result: Result<crate::widgets::export::ExportResult, crate::widgets::export::ExportError>,
    ) {
        use crate::widgets::export_done::ExportOutcome;
        let outcome = match result {
            Ok(out) => ExportOutcome::Ok {
                tab: tab.to_string(),
                path: out.path,
                count: out.count,
            },
            Err(message) => ExportOutcome::Err {
                tab: tab.to_string(),
                message,
            },
        };
        self.modal = Some(Modal::ExportDone { outcome });
    }

    fn export_logs(&mut self) {
        let records: Vec<net_sdk::deck::LogRecord> = match &self.logs_paused {
            Some(frozen) => frozen.clone(),
            None => self.logs_tail.snapshot(),
        };
        let min_rank = tabs::logs::level_rank(self.logs_min_level);
        let needle = self.logs_search.to_ascii_lowercase();
        let filtered: Vec<_> = records
            .into_iter()
            .filter(|r| tabs::logs::level_rank(r.level) >= min_rank)
            .filter(|r| tabs::logs::record_matches(r, &needle))
            .collect();
        let result = crate::widgets::export::write_logs(&filtered);
        self.open_export_modal("LOGS", result);
    }

    fn export_audit(&mut self) {
        let records = self.audit_tail.snapshot();
        let needle = self.audit_search.to_ascii_lowercase();
        let limit = self.audit_limit.unwrap_or(usize::MAX);
        // Match render-time projection: newest-first, force-only
        // + search applied, capped to limit. Then re-reverse so
        // the file reads chronologically (oldest-first) while
        // the on-screen view is newest-first.
        let filtered: Vec<_> = records
            .iter()
            .rev()
            .filter(|r| !self.audit_force_only || r.event.is_ice())
            .filter(|r| tabs::audit::record_matches(r, &needle))
            .take(limit)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let result = crate::widgets::export::write_audit(&filtered);
        self.open_export_modal("AUDIT", result);
    }

    fn export_failures(&mut self) {
        let records = self.failures_tail.snapshot();
        let needle = self.failures_search.to_ascii_lowercase();
        let filtered: Vec<_> = records
            .iter()
            .filter(|r| tabs::failures::record_matches(r, &needle))
            .cloned()
            .collect();
        let result = crate::widgets::export::write_failures(&filtered);
        self.open_export_modal("FAILURES", result);
    }

    fn export_blobs(&mut self) {
        let entries = self.blobs_tail.snapshot();
        let needle = self.blobs_search.to_ascii_lowercase();
        let filtered: Vec<_> = entries
            .iter()
            .filter(|e| tabs::blobs::record_matches(e, &needle))
            .cloned()
            .collect();
        let result = crate::widgets::export::write_blobs(&filtered);
        self.open_export_modal("BLOBS", result);
    }

    fn on_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        // Modal absorbs all input until dismissed.
        if self.modal.is_some() {
            self.on_modal_key(code, mods);
            return;
        }
        // Focused node page: Esc returns to the underlying
        // tab; cursor + tab-switch keys still work so the
        // operator can navigate back without explicitly
        // un-focusing first.
        // Tab-switch keys exit any focus mode cleanly. Defined
        // once so both branches below check the same set. `0`
        // is in here because it now jumps to LOGS.
        let is_tab_switch = matches!(
            code,
            KeyCode::Char('0'..='9')
                | KeyCode::Char('h')
                | KeyCode::Char('l')
                | KeyCode::Tab
                | KeyCode::BackTab
                | KeyCode::Left
                | KeyCode::Right
        );

        // Daemon page focus has priority — when both are set
        // (shouldn't happen, but defensive), drop only this
        // one on Esc / tab-switch.
        if self.daemon_focus.is_some() {
            if matches!(code, KeyCode::Esc) {
                self.daemon_focus = None;
                return;
            }
            if is_tab_switch {
                self.daemon_focus = None;
            } else if matches!(code, KeyCode::Down | KeyCode::Char('j' | 's')) {
                self.step_daemon_focus_cursor(1);
                return;
            } else if matches!(code, KeyCode::Up | KeyCode::Char('k' | 'w')) {
                self.step_daemon_focus_cursor(-1);
                return;
            } else if matches!(code, KeyCode::Enter) {
                self.dispatch_daemon_focus_enter();
                return;
            } else if matches!(code, KeyCode::Char('r')) {
                self.propose_restart_all_daemons();
                return;
            } else if matches!(code, KeyCode::Char('R')) {
                self.propose_ice_force_restart_daemon();
                return;
            } else if matches!(code, KeyCode::Char('?')) {
                self.modal = Some(Modal::Help);
                return;
            } else {
                return;
            }
        }
        if self.node_focus.is_some() {
            if matches!(code, KeyCode::Esc) {
                self.node_focus = None;
                return;
            }
            if is_tab_switch {
                self.node_focus = None;
                // fall through to the normal handler
            } else if matches!(code, KeyCode::Down | KeyCode::Char('j' | 's')) {
                self.step_node_placement_cursor(1);
                return;
            } else if matches!(code, KeyCode::Up | KeyCode::Char('k' | 'w')) {
                self.step_node_placement_cursor(-1);
                return;
            } else if matches!(code, KeyCode::Enter) {
                self.open_cursored_node_placement();
                return;
            } else if let Some(kind) = node_action_for(code) {
                // Routine admin actions on the focused node —
                // mirror NODES tab bindings so the operator can
                // act without Esc-ing back to the list.
                self.propose_node_action(kind);
                return;
            } else if matches!(code, KeyCode::Char('D')) {
                self.propose_drop_replicas();
                return;
            } else if matches!(code, KeyCode::Char('F')) {
                self.propose_ice_freeze();
                return;
            } else if matches!(code, KeyCode::Char('T')) {
                self.propose_ice_thaw();
                return;
            } else if matches!(code, KeyCode::Char('A')) {
                self.propose_ice_flush_avoid_lists();
                return;
            } else if matches!(code, KeyCode::Char('?')) {
                self.modal = Some(Modal::Help);
                return;
            } else {
                return;
            }
        }
        // Search prompts are the second-tier absorber: while a
        // tab's `_editing` flag is set, keystrokes go into that
        // tab's query buffer rather than the normal bindings.
        if self.logs_search_editing
            || self.audit_search_editing
            || self.failures_search_editing
            || self.blobs_search_editing
        {
            self.on_search_key(code);
            return;
        }
        match code {
            // Top-level Esc is a no-op. The modal absorber + focus
            // absorbers above handle Esc-to-dismiss in their own
            // arms; the outer fall-through used to quit the app,
            // which made Esc a session-ending key in any context
            // where the operator pressed it to "cancel" without
            // a modal open. Quit stays on `q` and Ctrl-C.
            KeyCode::Esc => {}
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('c') if mods.contains(KeyModifiers::CONTROL) => self.should_quit = true,
            // Help overlay — works on every tab, no cursor
            // required.
            KeyCode::Char('?') => {
                self.modal = Some(Modal::Help);
            }
            // Cluster picker — global, opens from any tab.
            KeyCode::Char(':') => {
                self.modal = Some(Modal::ClusterPicker { cursor: 0 });
            }
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                self.current = self.current.next()
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                self.current = self.current.prev()
            }
            // Numeric tab jumps follow `Tab::all()` order. `0`
            // is the 10th slot (LOGS) — pinned there so the
            // alphabetic shortcuts stay free.
            KeyCode::Char('1') => self.current = Tab::NetMap,
            KeyCode::Char('2') => self.current = Tab::Nodes,
            KeyCode::Char('3') => self.current = Tab::Daemons,
            KeyCode::Char('4') => self.current = Tab::Groups,
            KeyCode::Char('5') => self.current = Tab::Dataforts,
            KeyCode::Char('6') => self.current = Tab::Blobs,
            KeyCode::Char('7') => self.current = Tab::Migrations,
            KeyCode::Char('8') => self.current = Tab::Replicas,
            KeyCode::Char('9') => self.current = Tab::Audit,
            KeyCode::Char('0') => self.current = Tab::Logs,
            // DAEMON tab navigation. Lowercase letters walk the
            // member axis (cursor inside the focused group);
            // uppercase letters + arrows walk the group axis.
            // Arrows match the group axis because operators
            // typically think of the daemon list as "groups
            // first, members within"; the member axis is the
            // tighter sub-cursor reached via j/k/w/s.
            KeyCode::Char('j' | 's') if self.current == Tab::Groups => {
                self.groups_cursor.member = self.groups_cursor.member.saturating_add(1);
                self.clamp_groups_cursor();
            }
            KeyCode::Char('k' | 'w') if self.current == Tab::Groups => {
                self.groups_cursor.member = self.groups_cursor.member.saturating_sub(1);
            }
            KeyCode::Char('J' | 'S') | KeyCode::Down if self.current == Tab::Groups => {
                self.groups_cursor.group = self.groups_cursor.group.saturating_add(1);
                self.groups_cursor.member = 0;
                self.clamp_groups_cursor();
            }
            KeyCode::Char('K' | 'W') | KeyCode::Up if self.current == Tab::Groups => {
                self.groups_cursor.group = self.groups_cursor.group.saturating_sub(1);
                self.groups_cursor.member = 0;
            }
            // DAEMON tab actions: `r` proposes
            // restart-all-daemons on the cursored member's
            // host node. Pops a confirmation modal; Enter on
            // the modal fires the signed admin commit.
            KeyCode::Char('r') if self.current == Tab::Groups => {
                self.propose_restart_all_daemons();
            }
            // ICE force-restart on GROUPS tab. Targets the
            // cursored daemon; bypasses crash-loop backoff.
            KeyCode::Char('R') if self.current == Tab::Groups => {
                self.propose_ice_force_restart_daemon();
            }
            // NODES tab navigation: `j`/`k`/`s`/`w` + arrows move
            // the cursor through the nodes table (sorted by
            // NodeId). `s`/`w` are the WASD alias for `j`/`k`.
            KeyCode::Char('j' | 's') | KeyCode::Down if self.current == Tab::Nodes => {
                self.nodes_cursor = self.nodes_cursor.saturating_add(1);
                self.clamp_nodes_cursor();
            }
            KeyCode::Char('k' | 'w') | KeyCode::Up if self.current == Tab::Nodes => {
                self.nodes_cursor = self.nodes_cursor.saturating_sub(1);
            }
            // DAEMONS flat-table cursor — single axis over the
            // lineage-group flattened order. Enter opens the
            // placement node's NODE page.
            KeyCode::Char('j' | 's') | KeyCode::Down if self.current == Tab::Daemons => {
                self.daemons_cursor = self.daemons_cursor.saturating_add(1);
                self.clamp_daemons_cursor();
            }
            KeyCode::Char('k' | 'w') | KeyCode::Up if self.current == Tab::Daemons => {
                self.daemons_cursor = self.daemons_cursor.saturating_sub(1);
            }
            // NET.MAP shares the peers-by-id order with LIST.
            KeyCode::Char('j' | 's') | KeyCode::Down if self.current == Tab::NetMap => {
                self.netmap_cursor = self.netmap_cursor.saturating_add(1);
                self.clamp_netmap_cursor();
            }
            KeyCode::Char('k' | 'w') | KeyCode::Up if self.current == Tab::NetMap => {
                self.netmap_cursor = self.netmap_cursor.saturating_sub(1);
            }
            // DATAFORTS adapter list cursor.
            KeyCode::Char('j' | 's') | KeyCode::Down if self.current == Tab::Dataforts => {
                self.dataforts_cursor = self.dataforts_cursor.saturating_add(1);
                self.clamp_dataforts_cursor();
            }
            KeyCode::Char('k' | 'w') | KeyCode::Up if self.current == Tab::Dataforts => {
                self.dataforts_cursor = self.dataforts_cursor.saturating_sub(1);
            }
            KeyCode::Char('j' | 's') | KeyCode::Down if self.current == Tab::Replicas => {
                self.replica_cursor = self.replica_cursor.saturating_add(1);
                self.clamp_replica_cursor();
            }
            KeyCode::Char('k' | 'w') | KeyCode::Up if self.current == Tab::Replicas => {
                self.replica_cursor = self.replica_cursor.saturating_sub(1);
            }
            // ICE force-evict-replica on the cursored chain.
            // Opens a holder picker scoped to the chain's
            // current holders; the chosen holder drops its
            // replica on commit.
            KeyCode::Char('E') if self.current == Tab::Replicas => {
                self.propose_ice_force_evict_replica();
            }
            // ICE force-cutover: opens the node picker for the
            // cursored chain. Operator picks a target peer →
            // Confirm modal transitions in with the chain +
            // target baked in.
            KeyCode::Char('O') if self.current == Tab::Replicas => {
                self.propose_ice_force_cutover();
            }
            KeyCode::Char('j' | 's') | KeyCode::Down if self.current == Tab::Migrations => {
                self.migration_cursor = self.migration_cursor.saturating_add(1);
                self.clamp_migration_cursor();
            }
            KeyCode::Char('k' | 'w') | KeyCode::Up if self.current == Tab::Migrations => {
                self.migration_cursor = self.migration_cursor.saturating_sub(1);
            }
            KeyCode::Char('j' | 's') | KeyCode::Down if self.current == Tab::Failures => {
                self.failures_cursor = self.failures_cursor.saturating_add(1);
                self.clamp_failures_cursor();
            }
            KeyCode::Char('k' | 'w') | KeyCode::Up if self.current == Tab::Failures => {
                self.failures_cursor = self.failures_cursor.saturating_sub(1);
            }
            KeyCode::Char('j' | 's') | KeyCode::Down if self.current == Tab::Blobs => {
                self.blobs_cursor = self.blobs_cursor.saturating_add(1);
                self.clamp_blobs_cursor();
            }
            KeyCode::Char('k' | 'w') | KeyCode::Up if self.current == Tab::Blobs => {
                self.blobs_cursor = self.blobs_cursor.saturating_sub(1);
            }
            // Vim-style top/bottom on every cursor-driven tab.
            // `g` jumps to the first row / group / member; `G`
            // jumps to the last. No-op on tabs without a list.
            KeyCode::Char('g') => self.cursor_to_top(),
            KeyCode::Char('G') => self.cursor_to_bottom(),
            // ICE kill-migration on the cursored migration row.
            KeyCode::Char('K') if self.current == Tab::Migrations => {
                self.propose_ice_kill_migration();
            }
            // AUDIT filters. `f` toggles ICE-only; `n` cycles
            // the row limit none → 25 → 100 → none.
            KeyCode::Char('f') if self.current == Tab::Audit => {
                self.audit_force_only = !self.audit_force_only;
            }
            // LOGS: cycle the minimum-level threshold.
            // Info → Warn → Error → Debug → Info.
            KeyCode::Char('f') if self.current == Tab::Logs => {
                use net_sdk::deck::LogLevel;
                self.logs_min_level = match self.logs_min_level {
                    LogLevel::Info => LogLevel::Warn,
                    LogLevel::Warn => LogLevel::Error,
                    LogLevel::Error => LogLevel::Debug,
                    _ => LogLevel::Info,
                };
            }
            // LOGS: pause the tail. `None` follows live; toggling
            // captures the current ring so the operator can read
            // without the tail scrolling out from under them.
            // Other tabs keep using the live snapshot.
            KeyCode::Char('p') if self.current == Tab::Logs => {
                self.logs_paused = match self.logs_paused.take() {
                    Some(_) => None,
                    None => Some(self.logs_tail.snapshot()),
                };
            }
            // LOGS: open the substring search prompt. The
            // existing buffer is preserved so the operator can
            // refine instead of retyping.
            KeyCode::Char('/') if self.current == Tab::Logs => {
                self.logs_search_editing = true;
            }
            // AUDIT: same prompt pattern, scoped to the audit
            // ring. Matches against command name, operator IDs,
            // and rendered target text.
            KeyCode::Char('/') if self.current == Tab::Audit => {
                self.audit_search_editing = true;
            }
            // FAILURES: substring search across source + reason.
            KeyCode::Char('/') if self.current == Tab::Failures => {
                self.failures_search_editing = true;
            }
            // BLOBS: substring search against the hash hex.
            KeyCode::Char('/') if self.current == Tab::Blobs => {
                self.blobs_search_editing = true;
            }
            // Export the current filtered view to a timestamped
            // file in the cwd. Captures only what would render —
            // operator's filter chips dictate what lands in the
            // file. Surfaces success/failure in the footer toast.
            KeyCode::Char('e') if self.current == Tab::Logs => self.export_logs(),
            KeyCode::Char('e') if self.current == Tab::Audit => self.export_audit(),
            KeyCode::Char('e') if self.current == Tab::Failures => self.export_failures(),
            KeyCode::Char('e') if self.current == Tab::Blobs => self.export_blobs(),
            // BLOBS: open the detail modal for the cursored
            // entry. Snapshots the entry so a subsequent
            // inventory refresh doesn't shift the body.
            KeyCode::Enter if self.current == Tab::Blobs => self.open_blob_detail(),
            // Open the dedicated node detail page for the
            // cursored peer on NODES (`nodes_cursor`) or NET.MAP
            // (`netmap_cursor`). Both tabs share the
            // peers-by-id order, so the right cursor is
            // dispatched per source tab. Esc returns to the
            // originating tab.
            KeyCode::Enter if self.current == Tab::NetMap => {
                self.focus_node(self.netmap_cursor);
            }
            KeyCode::Enter if self.current == Tab::Nodes => {
                self.focus_node(self.nodes_cursor);
            }
            // DATAFORTS: Enter opens the NODE page for the
            // cursored datafort. Local goes via a synthesized
            // PeerSnapshot (the local node isn't in
            // `snapshot.peers`); remote dataforts find the
            // matching peer by id.
            KeyCode::Enter if self.current == Tab::Dataforts => {
                self.focus_datafort(self.dataforts_cursor);
            }
            // DAEMONS / GROUPS: Enter opens the Daemon page for
            // the cursored daemon. From there the operator can
            // drill into the placement Node or jump to a sibling.
            KeyCode::Enter if self.current == Tab::Daemons => {
                self.focus_daemons_cursored();
            }
            KeyCode::Enter if self.current == Tab::Groups => {
                self.focus_groups_cursored_daemon();
            }
            // DATAFORTS: cross-link to BLOBS. Operators reading
            // aggregate metrics jump straight to per-chunk
            // inventory of the same adapter.
            KeyCode::Char('b') if self.current == Tab::Dataforts => {
                self.current = Tab::Blobs;
            }
            KeyCode::Char('n') if self.current == Tab::Audit => {
                self.audit_limit = match self.audit_limit {
                    None => Some(25),
                    Some(25) => Some(100),
                    _ => None,
                };
            }
            // LIST tab actions on the cursored node: `c` cordon,
            // `C` uncordon, `d` drain (5-minute default window).
            KeyCode::Char('c') if self.current == Tab::Nodes => {
                self.propose_node_action(NodeActionKind::Cordon);
            }
            KeyCode::Char('C') if self.current == Tab::Nodes => {
                self.propose_node_action(NodeActionKind::Uncordon);
            }
            KeyCode::Char('d') if self.current == Tab::Nodes => {
                self.propose_node_action(NodeActionKind::Drain);
            }
            KeyCode::Char('m') if self.current == Tab::Nodes => {
                self.propose_node_action(NodeActionKind::EnterMaintenance);
            }
            KeyCode::Char('M') if self.current == Tab::Nodes => {
                self.propose_node_action(NodeActionKind::ExitMaintenance);
            }
            KeyCode::Char('a') if self.current == Tab::Nodes => {
                self.propose_node_action(NodeActionKind::ClearAvoidList);
            }
            KeyCode::Char('i') if self.current == Tab::Nodes => {
                self.propose_node_action(NodeActionKind::InvalidatePlacement);
            }
            KeyCode::Char('D') if self.current == Tab::Nodes => {
                self.propose_drop_replicas();
            }
            // ICE break-glass on NODES tab: `F` freeze, `T` thaw,
            // `A` flush avoid lists (global scope). Cluster-wide
            // except where noted; capital letters distinguish
            // from routine commands.
            KeyCode::Char('F') if self.current == Tab::Nodes => {
                self.propose_ice_freeze();
            }
            KeyCode::Char('T') if self.current == Tab::Nodes => {
                self.propose_ice_thaw();
            }
            KeyCode::Char('A') if self.current == Tab::Nodes => {
                self.propose_ice_flush_avoid_lists();
            }
            _ => {}
        }
    }

    fn propose_ice_freeze(&mut self) {
        use crate::widgets::param_input::ParamInputPurpose;
        let purpose = ParamInputPurpose::IceFreezeTtl;
        let buffer = purpose.default_buffer().to_string();
        self.modal = Some(Modal::ParamInput {
            purpose,
            buffer,
            error: None,
        });
    }

    fn propose_ice_thaw(&mut self) {
        use net_sdk::deck::{simulate_ice_proposal, IceActionProposal};
        let action = IceActionProposal::ThawCluster;
        let blast = simulate_ice_proposal(&self.snapshot, &action);
        self.modal = Some(Modal::Confirm(
            crate::widgets::confirm::ConfirmAction::IceThawCluster { blast },
        ));
    }

    fn propose_drop_replicas(&mut self) {
        let Some(node) = self.cursored_node() else {
            return;
        };
        // Default: every chain this node currently holds. The
        // operator confirms before any commit fires.
        let chains: Vec<u64> = self
            .snapshot
            .replicas
            .iter()
            .filter(|(_, r)| r.holders.contains(&node))
            .map(|(chain, _)| *chain)
            .collect();
        let node_display = self.node_display(node);
        self.modal = Some(Modal::Confirm(
            crate::widgets::confirm::ConfirmAction::DropReplicas {
                node,
                node_display,
                chains,
            },
        ));
    }

    fn propose_ice_flush_avoid_lists(&mut self) {
        use net_sdk::deck::{simulate_ice_proposal, AvoidScope, IceActionProposal};
        let action = IceActionProposal::FlushAvoidLists {
            scope: AvoidScope::Global,
        };
        let blast = simulate_ice_proposal(&self.snapshot, &action);
        self.modal = Some(Modal::Confirm(
            crate::widgets::confirm::ConfirmAction::IceFlushAvoidLists { blast },
        ));
    }

    fn propose_ice_force_restart_daemon(&mut self) {
        use net_sdk::deck::{simulate_ice_proposal, DaemonRef, IceActionProposal};
        // Daemon focus targets the focused daemon; falls back to
        // the GROUPS tab cursor when no Daemon page is open.
        let (daemon_id, daemon_name) = if let Some(focus) = self.daemon_focus.as_ref() {
            (focus.id, focus.snapshot.name.clone())
        } else {
            let groups = crate::lineage::group_daemons(&self.snapshot.daemons);
            let Some(group) = groups.get(self.groups_cursor.group) else {
                return;
            };
            let Some(member) = group.members.get(self.groups_cursor.member) else {
                return;
            };
            (member.id, member.daemon.name.clone())
        };
        let action = IceActionProposal::ForceRestartDaemon {
            daemon: DaemonRef {
                id: daemon_id,
                name: daemon_name.clone(),
            },
        };
        let blast = simulate_ice_proposal(&self.snapshot, &action);
        self.modal = Some(Modal::Confirm(
            crate::widgets::confirm::ConfirmAction::IceForceRestartDaemon {
                daemon_id,
                daemon_name,
                blast,
            },
        ));
    }

    fn clamp_nodes_cursor(&mut self) {
        let n = self.snapshot.peers.len();
        if n == 0 {
            self.nodes_cursor = 0;
        } else if self.nodes_cursor >= n {
            self.nodes_cursor = n - 1;
        }
    }

    fn clamp_netmap_cursor(&mut self) {
        let n = self.snapshot.peers.len();
        if n == 0 {
            self.netmap_cursor = 0;
        } else if self.netmap_cursor >= n {
            self.netmap_cursor = n - 1;
        }
    }

    fn clamp_dataforts_cursor(&mut self) {
        // DATAFORTS lists one row per datafort node (local +
        // every peer carrying a dataforts cap), not per blob
        // adapter. Pinning to `blob_adapters.len()` stuck the
        // cursor at 3 even when 7 dataforts were rendered.
        let n = self.collect_dataforts().len();
        if n == 0 {
            self.dataforts_cursor = 0;
        } else if self.dataforts_cursor >= n {
            self.dataforts_cursor = n - 1;
        }
    }

    fn clamp_replica_cursor(&mut self) {
        let n = self.snapshot.replicas.len();
        if n == 0 {
            self.replica_cursor = 0;
        } else if self.replica_cursor >= n {
            self.replica_cursor = n - 1;
        }
    }

    fn clamp_migration_cursor(&mut self) {
        let n = self.snapshot.in_flight_migrations.len();
        if n == 0 {
            self.migration_cursor = 0;
        } else if self.migration_cursor >= n {
            self.migration_cursor = n - 1;
        }
    }

    fn clamp_failures_cursor(&mut self) {
        let n = self.failures_tail.records.lock().len();
        if n == 0 {
            self.failures_cursor = 0;
        } else if self.failures_cursor >= n {
            self.failures_cursor = n - 1;
        }
    }

    fn clamp_blobs_cursor(&mut self) {
        let n = self.blobs_tail.records.lock().len();
        if n == 0 {
            self.blobs_cursor = 0;
        } else if self.blobs_cursor >= n {
            self.blobs_cursor = n - 1;
        }
    }

    /// Absorb a single keypress into the active tab's search
    /// buffer. `Enter` commits (filter stays active), `Esc`
    /// cancels and clears, `Backspace` pops, any printable char
    /// appends. Non-handled keys are dropped so they don't leak
    /// to the normal binding table.
    fn on_search_key(&mut self, code: KeyCode) {
        let (buffer, editing) = if self.logs_search_editing {
            (&mut self.logs_search, &mut self.logs_search_editing)
        } else if self.audit_search_editing {
            (&mut self.audit_search, &mut self.audit_search_editing)
        } else if self.failures_search_editing {
            (&mut self.failures_search, &mut self.failures_search_editing)
        } else if self.blobs_search_editing {
            (&mut self.blobs_search, &mut self.blobs_search_editing)
        } else {
            return;
        };
        match code {
            KeyCode::Enter => *editing = false,
            KeyCode::Esc => {
                *editing = false;
                buffer.clear();
            }
            KeyCode::Backspace => {
                buffer.pop();
            }
            KeyCode::Char(c) => buffer.push(c),
            _ => {}
        }
    }

    fn cursor_to_top(&mut self) {
        match self.current {
            Tab::NetMap => self.netmap_cursor = 0,
            Tab::Dataforts => self.dataforts_cursor = 0,
            Tab::Nodes => self.nodes_cursor = 0,
            Tab::Daemons => self.daemons_cursor = 0,
            Tab::Replicas => self.replica_cursor = 0,
            Tab::Migrations => self.migration_cursor = 0,
            Tab::Failures => self.failures_cursor = 0,
            Tab::Blobs => self.blobs_cursor = 0,
            Tab::Groups => self.groups_cursor = DaemonCursor::default(),
            _ => {}
        }
    }

    fn cursor_to_bottom(&mut self) {
        match self.current {
            Tab::NetMap => {
                let n = self.snapshot.peers.len();
                self.netmap_cursor = n.saturating_sub(1);
            }
            Tab::Dataforts => {
                let n = self.blob_adapters.len();
                self.dataforts_cursor = n.saturating_sub(1);
            }
            Tab::Nodes => {
                let n = self.snapshot.peers.len();
                self.nodes_cursor = n.saturating_sub(1);
            }
            Tab::Daemons => {
                let n = tabs::daemons::total_daemons(&self.snapshot);
                self.daemons_cursor = n.saturating_sub(1);
            }
            Tab::Replicas => {
                let n = self.snapshot.replicas.len();
                self.replica_cursor = n.saturating_sub(1);
            }
            Tab::Migrations => {
                let n = self.snapshot.in_flight_migrations.len();
                self.migration_cursor = n.saturating_sub(1);
            }
            Tab::Failures => {
                let n = self.failures_tail.records.lock().len();
                self.failures_cursor = n.saturating_sub(1);
            }
            Tab::Blobs => {
                let n = self.blobs_tail.records.lock().len();
                self.blobs_cursor = n.saturating_sub(1);
            }
            Tab::Groups => {
                let groups = crate::lineage::group_daemons(&self.snapshot.daemons);
                if let Some(last) = groups.len().checked_sub(1) {
                    self.groups_cursor.group = last;
                    self.groups_cursor.member = groups[last].members.len().saturating_sub(1);
                }
            }
            _ => {}
        }
    }

    fn propose_ice_force_cutover(&mut self) {
        let Some((chain, _)) = self.snapshot.replicas.iter().nth(self.replica_cursor) else {
            return;
        };
        self.modal = Some(Modal::PickNode {
            purpose: crate::widgets::pick_node::PickNodePurpose::ForceCutoverTarget {
                chain: *chain,
            },
            cursor: 0,
        });
    }

    fn propose_ice_force_evict_replica(&mut self) {
        // Open a picker over the cursored chain's holders. The
        // Enter handler transitions into the Confirm modal once
        // the operator chooses which holder to evict.
        let Some((chain, replica)) = self.snapshot.replicas.iter().nth(self.replica_cursor) else {
            return;
        };
        if replica.holders.is_empty() {
            return; // chain has no holders; nothing to evict
        }
        self.modal = Some(Modal::PickNode {
            purpose: crate::widgets::pick_node::PickNodePurpose::ForceEvictHolder { chain: *chain },
            cursor: 0,
        });
    }

    fn propose_ice_kill_migration(&mut self) {
        use net_sdk::deck::{simulate_ice_proposal, IceActionProposal};
        let Some(m) = self
            .snapshot
            .in_flight_migrations
            .get(self.migration_cursor)
        else {
            return;
        };
        let migration = m.daemon_origin;
        let action = IceActionProposal::KillMigration { migration };
        let blast = simulate_ice_proposal(&self.snapshot, &action);
        self.modal = Some(Modal::Confirm(
            crate::widgets::confirm::ConfirmAction::IceKillMigration { migration, blast },
        ));
    }

    /// Resolve the NodeId the active admin action should target.
    /// When the NODE page is open, that's the focused node;
    /// otherwise the cursored peer on NODES. Returns `None`
    /// only when nothing is focused and the peers map is empty.
    pub fn cursored_node(&self) -> Option<u64> {
        if let Some(focus) = self.node_focus.as_ref() {
            return Some(focus.id);
        }
        self.snapshot.peers.keys().nth(self.nodes_cursor).copied()
    }

    fn propose_node_action(&mut self, kind: NodeActionKind) {
        let Some(node) = self.cursored_node() else {
            return;
        };
        let node_display = self.node_display(node);
        // Drain takes an operator-typed window, so it routes
        // through ParamInput rather than building a Confirm
        // directly with the hardcoded default.
        if matches!(kind, NodeActionKind::Drain) {
            use crate::widgets::param_input::ParamInputPurpose;
            let purpose = ParamInputPurpose::DrainWindow { node, node_display };
            let buffer = purpose.default_buffer().to_string();
            self.modal = Some(Modal::ParamInput {
                purpose,
                buffer,
                error: None,
            });
            return;
        }
        let action = match kind {
            NodeActionKind::Cordon => {
                crate::widgets::confirm::ConfirmAction::Cordon { node, node_display }
            }
            NodeActionKind::Uncordon => {
                crate::widgets::confirm::ConfirmAction::Uncordon { node, node_display }
            }
            NodeActionKind::Drain => unreachable!("Drain handled above"),
            NodeActionKind::EnterMaintenance => {
                crate::widgets::confirm::ConfirmAction::EnterMaintenance {
                    node,
                    node_display,
                    drain_for: None,
                }
            }
            NodeActionKind::ExitMaintenance => {
                crate::widgets::confirm::ConfirmAction::ExitMaintenance { node, node_display }
            }
            NodeActionKind::ClearAvoidList => {
                crate::widgets::confirm::ConfirmAction::ClearAvoidList { node, node_display }
            }
            NodeActionKind::InvalidatePlacement => {
                crate::widgets::confirm::ConfirmAction::InvalidatePlacement { node, node_display }
            }
        };
        self.modal = Some(Modal::Confirm(action));
    }

    fn on_modal_key(&mut self, code: KeyCode, _mods: KeyModifiers) {
        // ParamInput is a typing surface — every Char (including
        // `q`) goes into the buffer, so it must be dispatched
        // before the normal `q`/Esc dismiss logic.
        if matches!(self.modal, Some(Modal::ParamInput { .. })) {
            self.on_param_input_key(code);
            return;
        }
        match code {
            // Help overlay toggles off on `?` as well as the
            // standard dismiss keys.
            KeyCode::Char('?') if matches!(self.modal, Some(Modal::Help)) => {
                self.modal = None;
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                self.modal = None;
            }
            // Cursor navigation inside the PickNode modal.
            KeyCode::Char('j' | 's') | KeyCode::Down
                if matches!(self.modal, Some(Modal::PickNode { .. })) =>
            {
                if let Some(Modal::PickNode { cursor, .. }) = self.modal.as_mut() {
                    *cursor = cursor.saturating_add(1);
                }
                self.clamp_pick_cursor();
            }
            KeyCode::Char('k' | 'w') | KeyCode::Up
                if matches!(self.modal, Some(Modal::PickNode { .. })) =>
            {
                if let Some(Modal::PickNode { cursor, .. }) = self.modal.as_mut() {
                    *cursor = cursor.saturating_sub(1);
                }
            }
            // Cluster picker cursor.
            KeyCode::Char('j' | 's') | KeyCode::Down
                if matches!(self.modal, Some(Modal::ClusterPicker { .. })) =>
            {
                let n = 1 + self.bookmarks.sorted().len();
                if let Some(Modal::ClusterPicker { cursor }) = self.modal.as_mut() {
                    *cursor = (*cursor + 1).min(n.saturating_sub(1));
                }
            }
            KeyCode::Char('k' | 'w') | KeyCode::Up
                if matches!(self.modal, Some(Modal::ClusterPicker { .. })) =>
            {
                if let Some(Modal::ClusterPicker { cursor }) = self.modal.as_mut() {
                    *cursor = cursor.saturating_sub(1);
                }
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                let modal = self.modal.take();
                match modal {
                    Some(Modal::Confirm(action)) => self.dispatch_confirm(action),
                    Some(Modal::PickNode { purpose, cursor }) => {
                        self.commit_pick(purpose, cursor);
                    }
                    Some(Modal::ClusterPicker { cursor }) => {
                        self.commit_cluster_pick(cursor);
                    }
                    // BlobDetail: Enter opens the NODE page for
                    // the host (the modal already shows the
                    // host id.label — Enter is the jump). Esc
                    // / q close without navigating.
                    Some(Modal::BlobDetail {
                        host_id,
                        host_label,
                        ..
                    }) => {
                        self.focus_host(host_id, host_label);
                    }
                    // ^ host_label is now Option<String>; moved
                    // by `take()` above so this is the owning
                    // copy.
                    // ExportDone is informational — Enter closes.
                    Some(Modal::ExportDone { .. }) => {}
                    // ParamInput is intercepted earlier in this
                    // function; reaching here would be a bug.
                    Some(Modal::ParamInput { .. }) => {}
                    Some(Modal::Help) | None => {}
                }
            }
            _ => {}
        }
    }

    /// Absorb a single keypress into the ParamInput modal's
    /// buffer. Backspace pops, Enter parses+commits (or stashes
    /// an error), Esc cancels the whole modal, and any printable
    /// char extends the buffer.
    fn on_param_input_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => self.modal = None,
            KeyCode::Enter => self.commit_param_input(),
            KeyCode::Backspace => {
                if let Some(Modal::ParamInput { buffer, error, .. }) = self.modal.as_mut() {
                    buffer.pop();
                    *error = None;
                }
            }
            KeyCode::Char(c) => {
                if let Some(Modal::ParamInput { buffer, error, .. }) = self.modal.as_mut() {
                    if buffer.chars().count() < crate::widgets::param_input::MAX_BUFFER_LEN {
                        buffer.push(c);
                        *error = None;
                    }
                }
            }
            _ => {}
        }
    }

    /// Try to parse the current ParamInput buffer; on success,
    /// transition to a `Confirm` modal carrying the parsed
    /// value. On failure, restash the modal with an error
    /// string so the operator can keep editing.
    fn commit_param_input(&mut self) {
        use crate::widgets::param_input::{parse_duration, ParamInputPurpose};
        let modal = self.modal.take();
        let Some(Modal::ParamInput {
            purpose, buffer, ..
        }) = modal
        else {
            return;
        };
        let parsed = match parse_duration(&buffer) {
            Ok(d) => d,
            Err(err) => {
                self.modal = Some(Modal::ParamInput {
                    purpose,
                    buffer,
                    error: Some(err),
                });
                return;
            }
        };
        let (min, max) = purpose.range();
        if parsed < min || parsed > max {
            self.modal = Some(Modal::ParamInput {
                purpose,
                buffer,
                error: Some(format!(
                    "out of range ({}..={})",
                    crate::widgets::param_input::fmt_duration(min),
                    crate::widgets::param_input::fmt_duration(max),
                )),
            });
            return;
        }
        match purpose {
            ParamInputPurpose::DrainWindow { node, node_display } => {
                self.modal = Some(Modal::Confirm(
                    crate::widgets::confirm::ConfirmAction::Drain {
                        node,
                        node_display,
                        drain_for: parsed,
                    },
                ));
            }
            ParamInputPurpose::IceFreezeTtl => {
                use net_sdk::deck::{simulate_ice_proposal, IceActionProposal};
                let action = IceActionProposal::FreezeCluster { ttl: parsed };
                let blast = simulate_ice_proposal(&self.snapshot, &action);
                self.modal = Some(Modal::Confirm(
                    crate::widgets::confirm::ConfirmAction::IceFreezeCluster { ttl: parsed, blast },
                ));
            }
        }
    }

    /// Clamp the picker cursor against the candidate set the
    /// current `PickNodePurpose` would offer.
    fn clamp_pick_cursor(&mut self) {
        let n = match self.modal.as_ref() {
            Some(Modal::PickNode { purpose, .. }) => {
                purpose.candidates(&self.snapshot, self.this_node).len()
            }
            _ => return,
        };
        if let Some(Modal::PickNode { cursor, .. }) = self.modal.as_mut() {
            if n == 0 {
                *cursor = 0;
            } else if *cursor >= n {
                *cursor = n - 1;
            }
        }
    }

    /// Transition from `PickNode` to `Confirm` once the
    /// operator presses Enter — bake the cursored candidate
    /// into the appropriate ICE action variant.
    fn commit_pick(&mut self, purpose: crate::widgets::pick_node::PickNodePurpose, cursor: usize) {
        use net_sdk::deck::{simulate_ice_proposal, IceActionProposal};
        let candidates = purpose.candidates(&self.snapshot, self.this_node);
        let Some(picked) = candidates.get(cursor).copied() else {
            return;
        };
        let picked_display = self.node_display(picked);
        match purpose {
            crate::widgets::pick_node::PickNodePurpose::ForceCutoverTarget { chain } => {
                let action = IceActionProposal::ForceCutover {
                    chain,
                    target: picked,
                };
                let blast = simulate_ice_proposal(&self.snapshot, &action);
                self.modal = Some(Modal::Confirm(
                    crate::widgets::confirm::ConfirmAction::IceForceCutover {
                        chain,
                        target: picked,
                        target_display: picked_display,
                        blast,
                    },
                ));
            }
            crate::widgets::pick_node::PickNodePurpose::ForceEvictHolder { chain } => {
                let action = IceActionProposal::ForceEvictReplica {
                    chain,
                    victim: picked,
                };
                let blast = simulate_ice_proposal(&self.snapshot, &action);
                self.modal = Some(Modal::Confirm(
                    crate::widgets::confirm::ConfirmAction::IceForceEvictReplica {
                        chain,
                        victim: picked,
                        victim_display: picked_display,
                        blast,
                    },
                ));
            }
        }
    }

    /// Build a restart-all-daemons confirmation for the
    /// cursored daemon's host node. No-op if no daemon is
    /// selected (empty snapshot, etc.).
    fn propose_restart_all_daemons(&mut self) {
        // Daemon focus targets its own placement; falls back to
        // the GROUPS tab cursor when no Daemon page is open.
        let placement = if let Some(focus) = self.daemon_focus.as_ref() {
            Some(focus.snapshot.placement)
        } else {
            let groups = crate::lineage::group_daemons(&self.snapshot.daemons);
            groups
                .get(self.groups_cursor.group)
                .and_then(|g| g.members.get(self.groups_cursor.member))
                .map(|m| m.daemon.placement)
        };
        let Some(node) = placement else { return };
        let node_display = self.node_display(node);
        let daemon_count = self
            .snapshot
            .daemons
            .values()
            .filter(|d| d.placement == node)
            .count();
        self.modal = Some(Modal::Confirm(
            crate::widgets::confirm::ConfirmAction::RestartAllDaemons {
                node,
                node_display,
                daemon_count,
            },
        ));
    }

    /// Spawn a tokio task that fires the SDK call corresponding
    /// to the confirmed action. Routine admin failures surface
    /// as a footer toast; ICE failures additionally surface
    /// through the audit ring via `dispatch_ice`.
    fn dispatch_confirm(&self, action: crate::widgets::confirm::ConfirmAction) {
        let deck = Arc::clone(&self.deck);
        let toast_tx = self.toast_tx.clone();
        tokio::spawn(async move {
            use crate::widgets::confirm::ConfirmAction;
            let report_routine = |kind: &str, res: Result<_, _>| {
                if let Err(err) = res {
                    let _ = toast_tx.send(format!("{kind} failed — {err}"));
                }
            };
            match action {
                ConfirmAction::RestartAllDaemons { node, .. } => {
                    report_routine(
                        "restart_all_daemons",
                        deck.admin().restart_all_daemons(node).await,
                    );
                }
                ConfirmAction::Cordon { node, .. } => {
                    report_routine("cordon", deck.admin().cordon(node).await);
                }
                ConfirmAction::Uncordon { node, .. } => {
                    report_routine("uncordon", deck.admin().uncordon(node).await);
                }
                ConfirmAction::Drain {
                    node, drain_for, ..
                } => {
                    report_routine("drain", deck.admin().drain(node, drain_for).await);
                }
                ConfirmAction::EnterMaintenance {
                    node, drain_for, ..
                } => {
                    report_routine(
                        "enter_maintenance",
                        deck.admin().enter_maintenance(node, drain_for).await,
                    );
                }
                ConfirmAction::ExitMaintenance { node, .. } => {
                    report_routine(
                        "exit_maintenance",
                        deck.admin().exit_maintenance(node).await,
                    );
                }
                ConfirmAction::ClearAvoidList { node, .. } => {
                    report_routine(
                        "clear_avoid_list",
                        deck.admin().clear_avoid_list(node).await,
                    );
                }
                ConfirmAction::InvalidatePlacement { node, .. } => {
                    report_routine(
                        "invalidate_placement",
                        deck.admin().invalidate_placement(node).await,
                    );
                }
                ConfirmAction::IceFreezeCluster { ttl, .. } => {
                    let proposal = deck.ice().freeze_cluster(ttl);
                    dispatch_ice(&deck, proposal, "freeze_cluster", toast_tx.clone()).await;
                }
                ConfirmAction::IceThawCluster { .. } => {
                    let proposal = deck.ice().thaw_cluster();
                    dispatch_ice(&deck, proposal, "thaw_cluster", toast_tx.clone()).await;
                }
                ConfirmAction::IceForceRestartDaemon {
                    daemon_id,
                    daemon_name,
                    ..
                } => {
                    let daemon_ref = net_sdk::deck::DaemonRef {
                        id: daemon_id,
                        name: daemon_name,
                    };
                    let proposal = deck.ice().force_restart_daemon(daemon_ref);
                    dispatch_ice(&deck, proposal, "force_restart_daemon", toast_tx.clone()).await;
                }
                ConfirmAction::DropReplicas { node, chains, .. } => {
                    report_routine(
                        "drop_replicas",
                        deck.admin().drop_replicas(node, chains).await,
                    );
                }
                ConfirmAction::IceFlushAvoidLists { .. } => {
                    let proposal = deck
                        .ice()
                        .flush_avoid_lists(net_sdk::deck::AvoidScope::Global);
                    dispatch_ice(&deck, proposal, "flush_avoid_lists", toast_tx.clone()).await;
                }
                ConfirmAction::IceKillMigration { migration, .. } => {
                    let proposal = deck.ice().kill_migration(migration);
                    dispatch_ice(&deck, proposal, "kill_migration", toast_tx.clone()).await;
                }
                ConfirmAction::IceForceEvictReplica { chain, victim, .. } => {
                    let proposal = deck.ice().force_evict_replica(chain, victim);
                    dispatch_ice(&deck, proposal, "force_evict_replica", toast_tx.clone()).await;
                }
                ConfirmAction::IceForceCutover { chain, target, .. } => {
                    let proposal = deck.ice().force_cutover(chain, target);
                    dispatch_ice(&deck, proposal, "force_cutover", toast_tx.clone()).await;
                }
            }
        });
    }

    /// Clamp the daemon cursor against the current snapshot's
    /// live lineage groups. With no daemons in the snapshot
    /// the cursor is reset to (0, 0) — the fixture tab uses
    /// hardcoded constants in that case.
    fn clamp_groups_cursor(&mut self) {
        let groups = crate::lineage::group_daemons(&self.snapshot.daemons);
        if groups.is_empty() {
            self.groups_cursor = DaemonCursor::default();
            return;
        }
        if self.groups_cursor.group >= groups.len() {
            self.groups_cursor.group = groups.len() - 1;
        }
        let n_members = groups[self.groups_cursor.group].members.len();
        if n_members == 0 {
            self.groups_cursor.member = 0;
        } else if self.groups_cursor.member >= n_members {
            self.groups_cursor.member = n_members - 1;
        }
    }

    /// Clamp the DAEMONS flat-table cursor.
    fn clamp_daemons_cursor(&mut self) {
        let n = tabs::daemons::total_daemons(&self.snapshot);
        if n == 0 {
            self.daemons_cursor = 0;
        } else if self.daemons_cursor >= n {
            self.daemons_cursor = n - 1;
        }
    }

    fn draw(&self, frame: &mut Frame<'_>) {
        let area = frame.area();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // top status line
                Constraint::Length(1), // tab bar
                Constraint::Length(1), // rule
                Constraint::Min(0),    // body
                Constraint::Length(1), // footer
            ])
            .split(area);
        widgets::status_bar::render(frame, chunks[0], self);
        widgets::tab_bar::render(frame, chunks[1], self.current);
        widgets::rule::render(frame, chunks[2]);
        // Daemon focus pre-empts the tab's normal body. The
        // operator drilled into a daemon (from DAEMONS, GROUPS,
        // or a NODE page's placement row); the Daemon page owns
        // the body until they Esc out.
        if let Some(focus) = self.daemon_focus.as_ref() {
            let logs = self.logs_tail.snapshot();
            tabs::daemon_page::render(frame, chunks[3], focus, &self.snapshot, &logs);
            widgets::footer::render(
                frame,
                chunks[4],
                self.current,
                widgets::footer::FocusKind::Daemon,
                self.toast.as_ref().map(|(s, _)| s.as_str()),
            );
            self.render_modal_overlay(frame, area);
            return;
        }
        // Node focus pre-empts the tab's normal body — the
        // operator drilled into a peer; the page owns the
        // body until they Esc out.
        if let Some(focus) = self.node_focus.as_ref() {
            let has_blob = focus
                .peer
                .capability_set
                .iter()
                .any(|c| c == "dataforts.blob.storage");
            let has_greedy = focus
                .peer
                .capability_set
                .iter()
                .any(|c| c == "greedy.cache" || c == "dataforts.greedy.cache");
            let datafort = if has_blob || has_greedy {
                Some(self.datafort_view_for(focus.id))
            } else {
                None
            };
            tabs::node_page::render(frame, chunks[3], focus, &self.snapshot, datafort.as_ref());
            widgets::footer::render(
                frame,
                chunks[4],
                self.current,
                widgets::footer::FocusKind::Node,
                self.toast.as_ref().map(|(s, _)| s.as_str()),
            );
            // Modal overlay still renders on top in case one
            // is open (rare in focus mode but possible).
            self.render_modal_overlay(frame, area);
            return;
        }
        match self.current {
            Tab::NetMap => {
                let logs = self.logs_tail.snapshot();
                tabs::net_map::render(
                    frame,
                    chunks[3],
                    self.tick,
                    Some(&self.snapshot),
                    self.netmap_cursor,
                    &logs,
                )
            }
            Tab::Nodes => {
                tabs::nodes::render(frame, chunks[3], Some(&self.snapshot), self.nodes_cursor)
            }
            Tab::Daemons => {
                tabs::daemons::render(frame, chunks[3], Some(&self.snapshot), self.daemons_cursor)
            }
            Tab::Dataforts => {
                let entries = self.collect_dataforts();
                tabs::dataforts::render(frame, chunks[3], &entries, self.dataforts_cursor);
            }
            Tab::Groups => {
                let logs = self.logs_tail.snapshot();
                let local_node = self.local_node_card();
                tabs::groups::render(
                    frame,
                    chunks[3],
                    Some(&self.snapshot),
                    self.groups_cursor,
                    &local_node,
                    &logs,
                );
            }
            Tab::Logs => {
                // Live records come from the streaming tail
                // (Phase 4); a paused snapshot is a frozen Vec
                // captured at `[p]`-toggle time.
                let live;
                let records: &[net_sdk::deck::LogRecord] = match &self.logs_paused {
                    Some(frozen) => frozen,
                    None => {
                        live = self.logs_tail.snapshot();
                        &live
                    }
                };
                tabs::logs::render(
                    frame,
                    chunks[3],
                    self.tick,
                    records,
                    tabs::logs::LogsView {
                        min_level: self.logs_min_level,
                        paused: self.logs_paused.is_some(),
                        search: &self.logs_search,
                        search_editing: self.logs_search_editing,
                    },
                );
            }
            Tab::Audit => {
                let records = self.audit_tail.snapshot();
                tabs::audit::render(
                    frame,
                    chunks[3],
                    &records,
                    self.audit_force_only,
                    self.audit_limit,
                    &self.audit_search,
                    self.audit_search_editing,
                );
            }
            Tab::Replicas => {
                tabs::replicas::render(frame, chunks[3], Some(&self.snapshot), self.replica_cursor)
            }
            Tab::Migrations => tabs::migrations::render(
                frame,
                chunks[3],
                Some(&self.snapshot),
                self.migration_cursor,
            ),
            Tab::Failures => {
                let records = self.failures_tail.snapshot();
                tabs::failures::render(
                    frame,
                    chunks[3],
                    &records,
                    self.failures_cursor,
                    &self.failures_search,
                    self.failures_search_editing,
                );
            }
            Tab::Blobs => {
                let entries = self.blobs_tail.snapshot();
                tabs::blobs::render(
                    frame,
                    chunks[3],
                    &entries,
                    self.blobs_cursor,
                    &self.blobs_search,
                    self.blobs_search_editing,
                );
            }
        }
        widgets::footer::render(
            frame,
            chunks[4],
            self.current,
            widgets::footer::FocusKind::None,
            self.toast.as_ref().map(|(s, _)| s.as_str()),
        );

        self.render_modal_overlay(frame, area);
    }

    /// Render the active modal (if any) over the body. Hoisted
    /// out of `draw` so the focused-node early-return path can
    /// still surface modals (rare: a confirm-modal opened from
    /// the page itself once we add page-level actions).
    fn render_modal_overlay(&self, frame: &mut Frame<'_>, area: Rect) {
        match &self.modal {
            Some(Modal::Confirm(action)) => widgets::confirm::render(frame, area, action),
            Some(Modal::Help) => widgets::help::render(frame, area),
            Some(Modal::PickNode { purpose, cursor }) => {
                widgets::pick_node::render(
                    frame,
                    area,
                    purpose,
                    &self.snapshot,
                    self.this_node,
                    *cursor,
                );
            }
            Some(Modal::ParamInput {
                purpose,
                buffer,
                error,
            }) => {
                widgets::param_input::render(frame, area, purpose, buffer, error.as_deref());
            }
            Some(Modal::ClusterPicker { cursor }) => {
                let sorted: Vec<crate::bookmarks::Bookmark> =
                    self.bookmarks.sorted().into_iter().cloned().collect();
                widgets::cluster_picker::render(
                    frame,
                    area,
                    &sorted,
                    &self.active_cluster,
                    *cursor,
                );
            }
            Some(Modal::BlobDetail {
                entry,
                host_id,
                host_label,
            }) => {
                widgets::blob_detail::render(frame, area, entry, *host_id, host_label.as_deref());
            }
            Some(Modal::ExportDone { outcome }) => {
                widgets::export_done::render(frame, area, outcome);
            }
            None => {}
        }
    }
}
