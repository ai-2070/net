use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
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
}

impl App {
    pub fn new() -> Self {
        Self {
            current: Tab::NetMap,
            should_quit: false,
            started: Instant::now(),
            tick: 0,
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
                last_tick = Instant::now();
            }
        }
        Ok(())
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
            Tab::Logs => tabs::logs::render(frame, chunks[3], self.tick),
        }
        widgets::footer::render(frame, chunks[4]);
    }
}
