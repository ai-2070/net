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
    /// `None` in fixture mode (binary built without
    /// `feature = "demo"` and not yet connected to a real
    /// cluster). `Some` when wired to a running runtime.
    pub deck: Option<Arc<DeckClient>>,
    /// Latest snapshot refreshed on each tick. Tabs that
    /// render live data read this; tabs still on fixtures
    /// ignore it. Wrapped in `Arc` so cloning into per-tab
    /// scope is one atomic-refcount op.
    pub snapshot: Option<Arc<MeshOsSnapshot>>,
}

impl App {
    pub fn new(deck: Option<Arc<DeckClient>>) -> Self {
        let snapshot = deck.as_ref().map(|d| Arc::new(d.status()));
        Self {
            current: Tab::NetMap,
            should_quit: false,
            started: Instant::now(),
            tick: 0,
            deck,
            snapshot,
        }
    }

    /// True iff the binary is connected to a live runtime —
    /// status bar uses this to switch between "DEMO" / "LIVE"
    /// / "FIXTURE" mode indicators.
    pub fn is_connected(&self) -> bool {
        self.deck.is_some()
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
        if let Some(deck) = &self.deck {
            self.snapshot = Some(Arc::new(deck.status()));
        }
    }

    fn on_key(&mut self, code: KeyCode, mods: KeyModifiers) {
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
            _ => {}
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
            Tab::NetMap => tabs::net_map::render(frame, chunks[3], self.tick),
            Tab::List => tabs::list_view::render(frame, chunks[3]),
            Tab::Dataforts => tabs::dataforts::render(frame, chunks[3], self.tick),
            Tab::Daemon => tabs::daemon::render(frame, chunks[3]),
            Tab::Logs => {
                tabs::logs::render(frame, chunks[3], self.tick, self.snapshot.as_deref())
            }
        }
        widgets::footer::render(frame, chunks[4]);
    }
}
