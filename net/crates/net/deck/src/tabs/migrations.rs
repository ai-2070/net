//! MIGRATIONS tab — projects `snapshot.in_flight_migrations`,
//! the daemon migrations the local `MigrationOrchestrator`
//! currently has in progress. Cursor (`j`/`k`) selects a row;
//! `[K]` opens the ICE kill-migration confirmation modal
//! targeting the cursored daemon.

use net_sdk::deck::{MeshOsSnapshot, MigrationPhaseSnapshot};
use ratatui::{
    layout::{Alignment, Constraint, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Row, Table},
    Frame,
};

use crate::{theme, widgets};

pub fn render(frame: &mut Frame<'_>, area: Rect, snapshot: Option<&MeshOsSnapshot>, cursor: usize) {
    let has_records = snapshot
        .map(|s| !s.in_flight_migrations.is_empty())
        .unwrap_or(false);
    if has_records {
        render_table(frame, area, snapshot.unwrap(), cursor);
    } else {
        render_empty(frame, area);
    }
}

fn render_empty(frame: &mut Frame<'_>, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
            Span::styled("MIGRATIONS", theme::green_hi()),
            Span::styled("    0 in flight", theme::chrome()),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    widgets::empty::render(
        frame,
        inner,
        "no daemon migrations in flight",
        "wire a MigrationSnapshotSource (production: OrchestratorMigrationSnapshotSource)",
    );
}

fn render_table(frame: &mut Frame<'_>, area: Rect, snapshot: &MeshOsSnapshot, cursor: usize) {
    let total = snapshot.in_flight_migrations.len();
    let pos = cursor.min(total.saturating_sub(1)) + 1;
    let header_line = Line::from(vec![
        Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
        Span::styled("MIGRATIONS", theme::green_hi()),
        Span::styled(format!("    {total} in flight"), theme::chrome()),
        Span::styled(format!("    {pos}/{total}"), theme::dim()),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(header_line)
        .title_alignment(Alignment::Left);

    let header = Row::new(vec![
        cell_dim(" "),
        cell_dim("DAEMON"),
        cell_dim("PHASE"),
        cell_dim("ELAPSED"),
    ])
    .height(1);

    let mut rows: Vec<Row> = Vec::with_capacity(total);
    for (i, m) in snapshot.in_flight_migrations.iter().enumerate() {
        let is_cursor = i == cursor;
        let marker = if is_cursor { "▶" } else { " " };
        let daemon_text = format!("daemon.0x{:x}", m.daemon_origin);
        let daemon_style = if is_cursor {
            theme::green_hi()
        } else {
            theme::text()
        };
        let (phase_style, phase_text) = phase_repr(&m.phase);
        rows.push(Row::new(vec![
            Cell::from(Span::styled(marker, theme::green_hi())),
            Cell::from(Span::styled(daemon_text, daemon_style)),
            Cell::from(Span::styled(phase_text, phase_style)),
            Cell::from(Span::styled(format_age(m.elapsed_ms), theme::dim())),
        ]));
    }

    let table = Table::new(
        rows,
        [
            Constraint::Length(2),  // cursor
            Constraint::Length(18), // daemon id
            Constraint::Length(12), // phase
            Constraint::Min(0),     // elapsed
        ],
    )
    .header(header)
    .block(block)
    .column_spacing(2);
    frame.render_widget(table, area);
}

fn phase_repr(p: &MigrationPhaseSnapshot) -> (ratatui::style::Style, &'static str) {
    // Earlier phases dim, later phases brighter, Complete green.
    match p {
        MigrationPhaseSnapshot::Snapshot => (theme::dim(), "Snapshot"),
        MigrationPhaseSnapshot::Transfer => (theme::cyan(), "Transfer"),
        MigrationPhaseSnapshot::Restore => (theme::cyan(), "Restore"),
        MigrationPhaseSnapshot::Replay => (theme::cyan(), "Replay"),
        MigrationPhaseSnapshot::Cutover => (theme::amber(), "Cutover"),
        MigrationPhaseSnapshot::Complete => (theme::green(), "Complete"),
        _ => (theme::chrome(), "?"),
    }
}

fn cell_dim(s: &'static str) -> Cell<'static> {
    Cell::from(Span::styled(s, theme::chrome()))
}

use super::format_age_ms as format_age;
