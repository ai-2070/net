//! MIGRATIONS tab — projects `snapshot.in_flight_migrations`,
//! the daemon migrations the local `MigrationOrchestrator`
//! currently has in progress. Cursor (`j`/`k`) selects a row;
//! `[K]` opens the ICE kill-migration confirmation modal
//! targeting the cursored daemon.

use net_sdk::deck::{MeshOsSnapshot, MigrationPhaseSnapshot, NodeId};
use ratatui::{
    layout::{Alignment, Constraint, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Row, Table},
    Frame,
};

use crate::{theme, widgets};

pub fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    snapshot: Option<&MeshOsSnapshot>,
    cursor: usize,
    this_node: NodeId,
) {
    let has_records = snapshot
        .map(|s| !s.in_flight_migrations.is_empty())
        .unwrap_or(false);
    if has_records {
        render_table(frame, area, snapshot.unwrap(), cursor, this_node);
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

fn render_table(
    frame: &mut Frame<'_>,
    area: Rect,
    snapshot: &MeshOsSnapshot,
    cursor: usize,
    this_node: NodeId,
) {
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
        cell_dim("SOURCE"),
        cell_dim("TARGET"),
        cell_dim("ROLE"),
        cell_dim("SIZE"),
        cell_dim("PHASE"),
        cell_dim("PROG"),
        cell_dim("RETRY"),
        cell_dim("AGE/PHASE"),
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
        let source_text = node_label(m.source_node);
        let target_text = node_label(m.target_node);
        let (role_text, role_style) = role_for(m.source_node, m.target_node, this_node);
        let size_text = match m.snapshot_bytes {
            Some(n) => format_bytes(n),
            None => "—".to_string(),
        };
        let (phase_style, phase_text) = phase_repr(&m.phase);
        let prog_text = match m.progress_pct {
            Some(p) => format!("{p}%"),
            None => "—".to_string(),
        };
        let retry_text = format!("{}", m.retries);
        let retry_style = if m.retries == 0 {
            theme::dim()
        } else if m.retries < 3 {
            theme::amber()
        } else {
            theme::red()
        };
        rows.push(Row::new(vec![
            Cell::from(Span::styled(marker, theme::green_hi())),
            Cell::from(Span::styled(daemon_text, daemon_style)),
            Cell::from(Span::styled(source_text, theme::cyan())),
            Cell::from(Span::styled(target_text, theme::cyan())),
            Cell::from(Span::styled(role_text, role_style)),
            Cell::from(Span::styled(size_text, theme::text())),
            Cell::from(Span::styled(phase_text, phase_style)),
            Cell::from(Span::styled(prog_text, theme::text())),
            Cell::from(Span::styled(retry_text, retry_style)),
            Cell::from(Span::styled(format_age(m.age_in_phase_ms), theme::text())),
            Cell::from(Span::styled(format_age(m.elapsed_ms), theme::dim())),
        ]));
    }

    let table = Table::new(
        rows,
        [
            Constraint::Length(2),  // cursor
            Constraint::Length(18), // daemon id
            Constraint::Length(10), // source
            Constraint::Length(10), // target
            Constraint::Length(8),  // role
            Constraint::Length(8),  // size
            Constraint::Length(10), // phase
            Constraint::Length(5),  // progress %
            Constraint::Length(5),  // retries
            Constraint::Length(10), // age in phase
            Constraint::Min(0),     // elapsed
        ],
    )
    .header(header)
    .block(block)
    .column_spacing(2);
    frame.render_widget(table, area);
}

/// Compact node id label. Operator-grep-friendly hex form; the
/// migration page already shows fixture-derived labels through
/// `nodes::label_of` on neighboring tabs, but for the narrow
/// SOURCE / TARGET columns we keep just the hex prefix to stay
/// within the column budget.
fn node_label(id: u64) -> String {
    format!("0x{id:x}")
}

/// This-node's part in the migration: `target` (incoming),
/// `source` (outgoing), or `observer` (the operator is watching
/// from a third node — neither side hosts the daemon). Color
/// reflects the same axis as `pressure_style` elsewhere — green
/// for target (the migration is bringing work to us), cyan for
/// source (sending it elsewhere), dim for purely observational.
fn role_for(
    source: NodeId,
    target: NodeId,
    this_node: NodeId,
) -> (&'static str, ratatui::style::Style) {
    if this_node == target {
        ("target", theme::green())
    } else if this_node == source {
        ("source", theme::cyan())
    } else {
        ("observer", theme::dim())
    }
}

/// Compact byte-count: bytes / KB / MB / GB / TB. Mirrors the
/// BLOBS tab's `format_bytes` so both surfaces speak the same
/// magnitude.
fn format_bytes(n: u64) -> String {
    const KB: u64 = 1_024;
    const MB: u64 = 1_024 * KB;
    const GB: u64 = 1_024 * MB;
    const TB: u64 = 1_024 * GB;
    if n < KB {
        format!("{n}B")
    } else if n < MB {
        format!("{:.1}KB", n as f64 / KB as f64)
    } else if n < GB {
        format!("{:.1}MB", n as f64 / MB as f64)
    } else if n < TB {
        format!("{:.1}GB", n as f64 / GB as f64)
    } else {
        format!("{:.1}TB", n as f64 / TB as f64)
    }
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
