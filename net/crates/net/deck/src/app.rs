use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use net_sdk::deck::{DeckClient, MeshOsSnapshot};
use ratatui::{
    layout::{Constraint, Direction, Layout},
    DefaultTerminal, Frame,
};

use crate::{tabs, widgets};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    NetMap,
    List,
    Dataforts,
    Daemon,
    Logs,
    Audit,
}

impl Tab {
    pub fn all() -> [Tab; 6] {
        [
            Tab::NetMap,
            Tab::List,
            Tab::Dataforts,
            Tab::Daemon,
            Tab::Logs,
            Tab::Audit,
        ]
    }

    pub fn label(self) -> &'static str {
        match self {
            Tab::NetMap => "NET.MAP",
            Tab::List => "LIST",
            Tab::Dataforts => "DATAFORTS",
            Tab::Daemon => "DAEMON",
            Tab::Logs => "LOGS",
            Tab::Audit => "AUDIT",
        }
    }

    pub fn next(self) -> Tab {
        let all = Self::all();
        let i = all.iter().position(|t| *t == self).unwrap();
        all[(i + 1) % all.len()]
    }

    pub fn prev(self) -> Tab {
        let all = Self::all();
        let i = all.iter().position(|t| *t == self).unwrap();
        all[(i + all.len() - 1) % all.len()]
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
    /// Cursor on the DAEMON tab's lineage tree. Indices into
    /// the live group list — `j`/`k` move the cursor; the
    /// detail pane on the right reflects whichever member is
    /// pointed to.
    pub daemon_cursor: DaemonCursor,
    /// Cursor on the LIST tab's nodes table — index into the
    /// peers map's sorted key order. `j`/`k` moves it; the
    /// row gets a `▶` marker + brighter id styling. Action
    /// bindings (`c` cordon, `C` uncordon, future drain)
    /// target the cursored node.
    pub list_cursor: usize,
    /// Active modal overlay (confirmation prompt, future
    /// signature collector, future help screen). When `Some`,
    /// the modal absorbs key input until dismissed.
    pub modal: Option<Modal>,
}

#[derive(Clone, Debug)]
pub enum Modal {
    Confirm(crate::widgets::confirm::ConfirmAction),
    /// Help overlay — full binding reference. Dismissed with
    /// `?` (toggle), `Esc`, or `q`.
    Help,
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

/// Default drain window when the operator hits `[d]` without
/// specifying a deadline. Five minutes is the cluster's typical
/// `MaintenanceConfig::default_drain_deadline` order of
/// magnitude — long enough for replicas to evacuate, short
/// enough that an accidental drain auto-times out.
pub const DEFAULT_DRAIN_WINDOW: std::time::Duration = std::time::Duration::from_secs(300);

/// Default ICE cluster-freeze TTL when the operator hits `[F]`.
/// 60 seconds — long enough to investigate, short enough that an
/// accidental freeze auto-thaws before reconcile drifts.
pub const DEFAULT_ICE_FREEZE_TTL: std::time::Duration = std::time::Duration::from_secs(60);

/// ICE commit pipeline shared by every ICE variant: simulate
/// (binds `issued_at_ms` + `blast_hash`), sign with the
/// deck's operator identity, commit the signed bundle. Errors
/// surface in the audit ring as `Rejected` entries; on
/// success the audit row reads `Accepted`.
async fn dispatch_ice(deck: &Arc<DeckClient>, proposal: net_sdk::deck::IceProposal<'_>) {
    let simulated = match proposal.simulate().await {
        Ok(s) => s,
        Err(_) => return,
    };
    let sig = deck.identity().sign_proposal(
        simulated.action(),
        simulated.issued_at_ms(),
        &simulated.blast_hash(),
    );
    let _ = simulated.commit(&[sig]).await;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DaemonCursor {
    pub group: usize,
    pub member: usize,
}

impl App {
    pub fn new(deck: Arc<DeckClient>) -> Self {
        let snapshot = Arc::new(deck.status());
        Self {
            current: Tab::NetMap,
            should_quit: false,
            started: Instant::now(),
            tick: 0,
            deck,
            snapshot,
            daemon_cursor: DaemonCursor::default(),
            list_cursor: 0,
            modal: None,
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
                last_tick = Instant::now();
            }
        }
        Ok(())
    }

    fn refresh_snapshot(&mut self) {
        self.snapshot = Arc::new(self.deck.status());
    }

    fn on_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        // Modal absorbs all input until dismissed.
        if self.modal.is_some() {
            self.on_modal_key(code, mods);
            return;
        }
        match code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Char('c') if mods.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true
            }
            // Help overlay — works on every tab, no cursor
            // required.
            KeyCode::Char('?') => {
                self.modal = Some(Modal::Help);
            }
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                self.current = self.current.next()
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                self.current = self.current.prev()
            }
            KeyCode::Char('1') => self.current = Tab::NetMap,
            KeyCode::Char('2') => self.current = Tab::List,
            KeyCode::Char('3') => self.current = Tab::Dataforts,
            KeyCode::Char('4') => self.current = Tab::Daemon,
            KeyCode::Char('5') => self.current = Tab::Logs,
            KeyCode::Char('6') => self.current = Tab::Audit,
            // DAEMON tab navigation. `j`/`k` move within the
            // current group's members; `J`/`K` step to the
            // next / previous group. No-op on other tabs.
            KeyCode::Char('j') if self.current == Tab::Daemon => {
                self.daemon_cursor.member = self.daemon_cursor.member.saturating_add(1);
                self.clamp_daemon_cursor();
            }
            KeyCode::Char('k') if self.current == Tab::Daemon => {
                self.daemon_cursor.member = self.daemon_cursor.member.saturating_sub(1);
            }
            KeyCode::Char('J') if self.current == Tab::Daemon => {
                self.daemon_cursor.group = self.daemon_cursor.group.saturating_add(1);
                self.daemon_cursor.member = 0;
                self.clamp_daemon_cursor();
            }
            KeyCode::Char('K') if self.current == Tab::Daemon => {
                self.daemon_cursor.group = self.daemon_cursor.group.saturating_sub(1);
                self.daemon_cursor.member = 0;
            }
            // DAEMON tab actions: `r` proposes
            // restart-all-daemons on the cursored member's
            // host node. Pops a confirmation modal; Enter on
            // the modal fires the signed admin commit.
            KeyCode::Char('r') if self.current == Tab::Daemon => {
                self.propose_restart_all_daemons();
            }
            // ICE force-restart on DAEMON tab. Targets the
            // cursored daemon; bypasses crash-loop backoff.
            KeyCode::Char('R') if self.current == Tab::Daemon => {
                self.propose_ice_force_restart_daemon();
            }
            // LIST tab navigation: `j`/`k` move the cursor
            // through the nodes table (sorted by NodeId).
            KeyCode::Char('j') if self.current == Tab::List => {
                self.list_cursor = self.list_cursor.saturating_add(1);
                self.clamp_list_cursor();
            }
            KeyCode::Char('k') if self.current == Tab::List => {
                self.list_cursor = self.list_cursor.saturating_sub(1);
            }
            // LIST tab actions on the cursored node: `c` cordon,
            // `C` uncordon, `d` drain (5-minute default window).
            KeyCode::Char('c') if self.current == Tab::List => {
                self.propose_node_action(NodeActionKind::Cordon);
            }
            KeyCode::Char('C') if self.current == Tab::List => {
                self.propose_node_action(NodeActionKind::Uncordon);
            }
            KeyCode::Char('d') if self.current == Tab::List => {
                self.propose_node_action(NodeActionKind::Drain);
            }
            KeyCode::Char('m') if self.current == Tab::List => {
                self.propose_node_action(NodeActionKind::EnterMaintenance);
            }
            KeyCode::Char('M') if self.current == Tab::List => {
                self.propose_node_action(NodeActionKind::ExitMaintenance);
            }
            KeyCode::Char('a') if self.current == Tab::List => {
                self.propose_node_action(NodeActionKind::ClearAvoidList);
            }
            KeyCode::Char('i') if self.current == Tab::List => {
                self.propose_node_action(NodeActionKind::InvalidatePlacement);
            }
            KeyCode::Char('D') if self.current == Tab::List => {
                self.propose_drop_replicas();
            }
            // ICE break-glass on LIST tab: `F` freeze, `T` thaw,
            // `A` flush avoid lists (global scope). Cluster-wide
            // except where noted; capital letters distinguish
            // from routine commands.
            KeyCode::Char('F') if self.current == Tab::List => {
                self.propose_ice_freeze();
            }
            KeyCode::Char('T') if self.current == Tab::List => {
                self.propose_ice_thaw();
            }
            KeyCode::Char('A') if self.current == Tab::List => {
                self.propose_ice_flush_avoid_lists();
            }
            _ => {}
        }
    }

    fn propose_ice_freeze(&mut self) {
        use net_sdk::deck::{simulate_ice_proposal, IceActionProposal};
        let action = IceActionProposal::FreezeCluster {
            ttl: DEFAULT_ICE_FREEZE_TTL,
        };
        let blast = simulate_ice_proposal(&self.snapshot, &action);
        self.modal = Some(Modal::Confirm(
            crate::widgets::confirm::ConfirmAction::IceFreezeCluster {
                ttl: DEFAULT_ICE_FREEZE_TTL,
                blast,
            },
        ));
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
        let Some(node) = self.cursored_node() else { return };
        // Default: every chain this node currently holds. The
        // operator confirms before any commit fires.
        let chains: Vec<u64> = self
            .snapshot
            .replicas
            .iter()
            .filter(|(_, r)| r.holders.contains(&node))
            .map(|(chain, _)| *chain)
            .collect();
        let node_display = format!(
            "0x{:x}{}",
            node,
            crate::nodes::label_of(&format!("0x{node:x}"))
                .map(|l| format!(".{l}"))
                .unwrap_or_default(),
        );
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
        let groups = crate::lineage::group_daemons(&self.snapshot.daemons);
        let Some(group) = groups.get(self.daemon_cursor.group) else { return };
        let Some(member) = group.members.get(self.daemon_cursor.member) else { return };
        let daemon_id = member.id;
        let daemon_name = member.daemon.name.clone();
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

    fn clamp_list_cursor(&mut self) {
        let n = self.snapshot.peers.len();
        if n == 0 {
            self.list_cursor = 0;
        } else if self.list_cursor >= n {
            self.list_cursor = n - 1;
        }
    }

    /// Look up the NodeId at the LIST cursor position. Returns
    /// `None` if the peers map is empty.
    pub fn cursored_node(&self) -> Option<u64> {
        self.snapshot
            .peers
            .keys()
            .nth(self.list_cursor)
            .copied()
    }

    fn propose_node_action(&mut self, kind: NodeActionKind) {
        let Some(node) = self.cursored_node() else { return };
        let node_display = format!(
            "0x{:x}{}",
            node,
            crate::nodes::label_of(&format!("0x{node:x}"))
                .map(|l| format!(".{l}"))
                .unwrap_or_default(),
        );
        let action = match kind {
            NodeActionKind::Cordon => crate::widgets::confirm::ConfirmAction::Cordon {
                node,
                node_display,
            },
            NodeActionKind::Uncordon => crate::widgets::confirm::ConfirmAction::Uncordon {
                node,
                node_display,
            },
            NodeActionKind::Drain => crate::widgets::confirm::ConfirmAction::Drain {
                node,
                node_display,
                drain_for: DEFAULT_DRAIN_WINDOW,
            },
            NodeActionKind::EnterMaintenance => {
                crate::widgets::confirm::ConfirmAction::EnterMaintenance {
                    node,
                    node_display,
                    drain_for: None,
                }
            }
            NodeActionKind::ExitMaintenance => {
                crate::widgets::confirm::ConfirmAction::ExitMaintenance {
                    node,
                    node_display,
                }
            }
            NodeActionKind::ClearAvoidList => {
                crate::widgets::confirm::ConfirmAction::ClearAvoidList {
                    node,
                    node_display,
                }
            }
            NodeActionKind::InvalidatePlacement => {
                crate::widgets::confirm::ConfirmAction::InvalidatePlacement {
                    node,
                    node_display,
                }
            }
        };
        self.modal = Some(Modal::Confirm(action));
    }

    fn on_modal_key(&mut self, code: KeyCode, _mods: KeyModifiers) {
        match code {
            // Help overlay toggles off on `?` as well as the
            // standard dismiss keys.
            KeyCode::Char('?') if matches!(self.modal, Some(Modal::Help)) => {
                self.modal = None;
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                self.modal = None;
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                let modal = self.modal.take();
                match modal {
                    Some(Modal::Confirm(action)) => self.dispatch_confirm(action),
                    Some(Modal::Help) | None => {}
                }
            }
            _ => {}
        }
    }

    /// Build a restart-all-daemons confirmation for the
    /// cursored daemon's host node. No-op if no daemon is
    /// selected (empty snapshot, etc.).
    fn propose_restart_all_daemons(&mut self) {
        let groups = crate::lineage::group_daemons(&self.snapshot.daemons);
        let Some(group) = groups.get(self.daemon_cursor.group) else { return };
        let Some(member) = group.members.get(self.daemon_cursor.member) else { return };
        let node = member.daemon.placement;
        let node_display = format!(
            "0x{:x}{}",
            node,
            crate::nodes::label_of(&format!("0x{node:x}"))
                .map(|l| format!(".{l}"))
                .unwrap_or_default(),
        );
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
    /// to the confirmed action. Fire-and-forget — the result
    /// surfaces in the snapshot's audit ring on the next tick.
    fn dispatch_confirm(&self, action: crate::widgets::confirm::ConfirmAction) {
        let deck = Arc::clone(&self.deck);
        tokio::spawn(async move {
            use crate::widgets::confirm::ConfirmAction;
            match action {
                ConfirmAction::RestartAllDaemons { node, .. } => {
                    let _ = deck.admin().restart_all_daemons(node).await;
                }
                ConfirmAction::Cordon { node, .. } => {
                    let _ = deck.admin().cordon(node).await;
                }
                ConfirmAction::Uncordon { node, .. } => {
                    let _ = deck.admin().uncordon(node).await;
                }
                ConfirmAction::Drain {
                    node, drain_for, ..
                } => {
                    let _ = deck.admin().drain(node, drain_for).await;
                }
                ConfirmAction::EnterMaintenance {
                    node, drain_for, ..
                } => {
                    let _ = deck.admin().enter_maintenance(node, drain_for).await;
                }
                ConfirmAction::ExitMaintenance { node, .. } => {
                    let _ = deck.admin().exit_maintenance(node).await;
                }
                ConfirmAction::ClearAvoidList { node, .. } => {
                    let _ = deck.admin().clear_avoid_list(node).await;
                }
                ConfirmAction::InvalidatePlacement { node, .. } => {
                    let _ = deck.admin().invalidate_placement(node).await;
                }
                ConfirmAction::IceFreezeCluster { ttl, .. } => {
                    let proposal = deck.ice().freeze_cluster(ttl);
                    dispatch_ice(&deck, proposal).await;
                }
                ConfirmAction::IceThawCluster { .. } => {
                    let proposal = deck.ice().thaw_cluster();
                    dispatch_ice(&deck, proposal).await;
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
                    dispatch_ice(&deck, proposal).await;
                }
                ConfirmAction::DropReplicas { node, chains, .. } => {
                    let _ = deck.admin().drop_replicas(node, chains).await;
                }
                ConfirmAction::IceFlushAvoidLists { .. } => {
                    let proposal = deck.ice().flush_avoid_lists(
                        net_sdk::deck::AvoidScope::Global,
                    );
                    dispatch_ice(&deck, proposal).await;
                }
            }
        });
    }

    /// Clamp the daemon cursor against the current snapshot's
    /// live lineage groups. With no daemons in the snapshot
    /// the cursor is reset to (0, 0) — the fixture tab uses
    /// hardcoded constants in that case.
    fn clamp_daemon_cursor(&mut self) {
        let groups = crate::lineage::group_daemons(&self.snapshot.daemons);
        if groups.is_empty() {
            self.daemon_cursor = DaemonCursor::default();
            return;
        }
        if self.daemon_cursor.group >= groups.len() {
            self.daemon_cursor.group = groups.len() - 1;
        }
        let n_members = groups[self.daemon_cursor.group].members.len();
        if n_members == 0 {
            self.daemon_cursor.member = 0;
        } else if self.daemon_cursor.member >= n_members {
            self.daemon_cursor.member = n_members - 1;
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
        match self.current {
            Tab::NetMap => {
                tabs::net_map::render(frame, chunks[3], self.tick, Some(&self.snapshot))
            }
            Tab::List => tabs::list_view::render(
                frame,
                chunks[3],
                Some(&self.snapshot),
                self.list_cursor,
            ),
            Tab::Dataforts => tabs::dataforts::render(frame, chunks[3], self.tick),
            Tab::Daemon => tabs::daemon::render(
                frame,
                chunks[3],
                Some(&self.snapshot),
                self.daemon_cursor,
            ),
            Tab::Logs => {
                tabs::logs::render(frame, chunks[3], self.tick, Some(&self.snapshot))
            }
            Tab::Audit => tabs::audit::render(frame, chunks[3], Some(&self.snapshot)),
        }
        widgets::footer::render(frame, chunks[4]);

        // Modal overlay (renders last so it sits visually on
        // top of the body).
        match &self.modal {
            Some(Modal::Confirm(action)) => widgets::confirm::render(frame, area, action),
            Some(Modal::Help) => widgets::help::render(frame, area),
            None => {}
        }
    }
}
