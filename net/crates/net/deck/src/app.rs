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
}

impl Tab {
    pub fn all() -> [Tab; 5] {
        [
            Tab::NetMap,
            Tab::List,
            Tab::Dataforts,
            Tab::Daemon,
            Tab::Logs,
        ]
    }

    pub fn label(self) -> &'static str {
        match self {
            Tab::NetMap => "NET.MAP",
            Tab::List => "LIST",
            Tab::Dataforts => "DATAFORTS",
            Tab::Daemon => "DAEMON",
            Tab::Logs => "LOGS",
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
    /// Active modal overlay (confirmation prompt, future
    /// signature collector, future help screen). When `Some`,
    /// the modal absorbs key input until dismissed.
    pub modal: Option<Modal>,
}

#[derive(Clone, Debug)]
pub enum Modal {
    Confirm(crate::widgets::confirm::ConfirmAction),
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
            _ => {}
        }
    }

    fn on_modal_key(&mut self, code: KeyCode, _mods: KeyModifiers) {
        match code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.modal = None;
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                let modal = self.modal.take();
                if let Some(Modal::Confirm(action)) = modal {
                    self.dispatch_confirm(action);
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
            Tab::List => tabs::list_view::render(frame, chunks[3], Some(&self.snapshot)),
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
        }
        widgets::footer::render(frame, chunks[4]);

        // Modal overlay (renders last so it sits visually on
        // top of the body).
        if let Some(Modal::Confirm(action)) = &self.modal {
            widgets::confirm::render(frame, area, action);
        }
    }
}
