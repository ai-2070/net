//! AUDIT tab — renders the streaming audit tail (Phase 4),
//! the ring of every admin commit the runtime has observed.
//! Newest first. Each row carries the seq, wall-clock ts,
//! command kind + target, operator ids, and the verifier's
//! outcome.

use net_sdk::deck::{AdminAuditRecord, AdminEvent, VerificationOutcome};
use ratatui::{
    layout::{Alignment, Constraint, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Row, Table},
    Frame,
};

use crate::{nodes, theme, widgets};

pub fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    records: &[AdminAuditRecord],
    force_only: bool,
    limit: Option<usize>,
    search: &str,
    search_editing: bool,
) {
    if records.is_empty() {
        render_empty(frame, area, force_only, limit, search, search_editing);
    } else {
        render_table(
            frame,
            area,
            records,
            force_only,
            limit,
            search,
            search_editing,
        );
    }
}

fn render_empty(
    frame: &mut Frame<'_>,
    area: Rect,
    force_only: bool,
    limit: Option<usize>,
    search: &str,
    search_editing: bool,
) {
    let mut title_spans = vec![
        Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
        Span::styled("AUDIT", theme::green_hi()),
        Span::styled("    0 commits", theme::chrome()),
    ];
    append_filter_chips(&mut title_spans, force_only, limit, search, search_editing);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(title_spans));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    widgets::empty::render(
        frame,
        inner,
        "no admin commits yet",
        "cordon a node ([c] on NODES) or restart a daemon ([r] on GROUPS) to populate",
    );
}

fn render_table(
    frame: &mut Frame<'_>,
    area: Rect,
    records: &[AdminAuditRecord],
    force_only: bool,
    limit: Option<usize>,
    search: &str,
    search_editing: bool,
) {
    // One pass over the records to count outcomes — the prior
    // three separate `.filter().count()` passes scanned a
    // potentially-large ring three times per frame.
    let total = records.len();
    let mut accepted = 0usize;
    let mut unverified = 0usize;
    for r in records {
        match r.outcome {
            VerificationOutcome::Accepted => accepted += 1,
            VerificationOutcome::Unverified => unverified += 1,
            _ => {}
        }
    }
    let rejected = total.saturating_sub(accepted + unverified);

    let mut title_spans = vec![
        Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
        Span::styled("AUDIT", theme::green_hi()),
        Span::styled(
            format!(
                "    {total} commits · {accepted} accepted · {unverified} unverified · {rejected} rejected"
            ),
            theme::chrome(),
        ),
    ];
    append_filter_chips(&mut title_spans, force_only, limit, search, search_editing);
    let header_line = Line::from(title_spans);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(header_line)
        .title_alignment(Alignment::Left);

    let header = Row::new(vec![
        cell_dim("SEQ"),
        cell_dim("WHEN"),
        cell_dim("OUTCOME"),
        cell_dim("OPERATOR"),
        cell_dim("COMMAND"),
        cell_dim("TARGET"),
    ])
    .height(1);

    // Newest first, with optional ICE-only filter + substring
    // search + row cap.
    let now_ms = unix_now_ms();
    let cap = limit.unwrap_or(usize::MAX);
    let needle = search.to_ascii_lowercase();
    let mut rows: Vec<Row> = Vec::with_capacity(total.min(cap));
    for rec in records
        .iter()
        .rev()
        .filter(|r| !force_only || r.event.is_ice())
        .filter(|r| needle.is_empty() || record_matches(r, &needle))
        .take(cap)
    {
        let (outcome_style, outcome_text) = outcome_repr(&rec.outcome);
        let (cmd, cmd_style) = command_repr(&rec.event);
        let target_spans = target_spans(&rec.event);
        let when = format_relative(rec.committed_at_ms, now_ms);
        let op_text = if rec.operator_ids.is_empty() {
            "—".to_string()
        } else {
            rec.operator_ids
                .iter()
                .map(|id| format!("0x{id:x}"))
                .collect::<Vec<_>>()
                .join(",")
        };

        rows.push(Row::new(vec![
            Cell::from(Span::styled(format!("{:>5}", rec.seq), theme::dim())),
            Cell::from(Span::styled(when, theme::text())),
            Cell::from(Span::styled(outcome_text, outcome_style)),
            Cell::from(Span::styled(op_text, theme::dim())),
            Cell::from(Span::styled(cmd, cmd_style)),
            Cell::from(Line::from(target_spans)),
        ]));
    }

    let table = Table::new(
        rows,
        [
            Constraint::Length(5),  // SEQ
            Constraint::Length(9),  // WHEN (Ns ago)
            Constraint::Length(11), // OUTCOME
            Constraint::Length(11), // OPERATOR
            Constraint::Length(20), // COMMAND
            Constraint::Min(0),     // TARGET (id.label or chain hex)
        ],
    )
    .header(header)
    .block(block)
    .column_spacing(2);
    frame.render_widget(table, area);
}

fn outcome_repr(o: &VerificationOutcome) -> (ratatui::style::Style, &'static str) {
    match o {
        VerificationOutcome::Accepted => (theme::green(), "Accepted"),
        VerificationOutcome::Unverified => (theme::amber(), "Unverified"),
        VerificationOutcome::Rejected { .. } => (theme::red(), "Rejected"),
        _ => (theme::chrome(), "?"),
    }
}

fn command_repr(e: &AdminEvent) -> (&'static str, ratatui::style::Style) {
    use AdminEvent::*;
    // ICE force-* variants in amber so they stand out from
    // routine admin commands.
    match e {
        EnterMaintenance { .. } => ("enter_maintenance", theme::cyan()),
        ExitMaintenance { .. } => ("exit_maintenance", theme::cyan()),
        Drain { .. } => ("drain", theme::cyan()),
        Cordon { .. } => ("cordon", theme::green_hi()),
        Uncordon { .. } => ("uncordon", theme::green_hi()),
        RestartAllDaemons { .. } => ("restart_all_daemons", theme::green_hi()),
        ClearAvoidList { .. } => ("clear_avoid_list", theme::green_hi()),
        DropReplicas { .. } => ("drop_replicas", theme::green_hi()),
        InvalidatePlacement { .. } => ("invalidate_placement", theme::green_hi()),
        FreezeCluster { .. } => ("freeze_cluster", theme::amber()),
        ThawCluster => ("thaw_cluster", theme::amber()),
        FlushAvoidLists { .. } => ("flush_avoid_lists", theme::amber()),
        ForceEvictReplica { .. } => ("force_evict_replica", theme::amber()),
        ForceRestartDaemon { .. } => ("force_restart_daemon", theme::amber()),
        ForceCutover { .. } => ("force_cutover", theme::amber()),
        KillMigration { .. } => ("kill_migration", theme::amber()),
        _ => ("unknown", theme::chrome()),
    }
}

fn target_spans(e: &AdminEvent) -> Vec<Span<'static>> {
    use AdminEvent::*;
    match e {
        EnterMaintenance { node, .. }
        | ExitMaintenance { node }
        | Drain { node, .. }
        | Cordon { node }
        | Uncordon { node }
        | RestartAllDaemons { node }
        | ClearAvoidList { node }
        | InvalidatePlacement { node } => nodes::id_spans(&format!("0x{node:x}")),
        DropReplicas { node, chains } => {
            let mut s = nodes::id_spans(&format!("0x{node:x}"));
            s.push(Span::styled(
                format!("  · {} chain(s)", chains.len()),
                theme::dim(),
            ));
            s
        }
        FreezeCluster { ttl } => vec![Span::styled(
            format!("ttl {}s", ttl.as_secs()),
            theme::text(),
        )],
        ThawCluster => vec![Span::styled("cluster", theme::text())],
        FlushAvoidLists { .. } => vec![Span::styled("avoid lists", theme::text())],
        ForceEvictReplica { chain, victim } => {
            let mut s = vec![Span::styled(format!("chain.0x{chain:x} · "), theme::text())];
            s.extend(nodes::id_spans(&format!("0x{victim:x}")));
            s
        }
        ForceRestartDaemon { daemon } => vec![Span::styled(
            format!("daemon.0x{:x}", daemon.id),
            theme::cyan(),
        )],
        ForceCutover { chain, target } => {
            let mut s = vec![Span::styled(format!("chain.0x{chain:x} → "), theme::text())];
            s.extend(nodes::id_spans(&format!("0x{target:x}")));
            s
        }
        KillMigration { migration } => vec![Span::styled(
            format!("migration.0x{migration:x}"),
            theme::cyan(),
        )],
        _ => vec![Span::styled("—", theme::chrome())],
    }
}

fn cell_dim(s: &'static str) -> Cell<'static> {
    Cell::from(Span::styled(s, theme::chrome()))
}

/// Substring match across the searchable surface of an audit
/// record: command kind, operator IDs (hex), and a flattened
/// rendition of the target spans. `needle_lower` must already
/// be lowercased; matches case-insensitively in one pass over
/// each candidate string without allocating a lowercased copy.
pub(crate) fn record_matches(rec: &AdminAuditRecord, needle_lower: &str) -> bool {
    if needle_lower.is_empty() {
        return true;
    }
    let (cmd, _) = command_repr(&rec.event);
    if ascii_icontains(cmd, needle_lower) {
        return true;
    }
    use std::fmt::Write;
    let mut buf = String::with_capacity(18);
    for id in &rec.operator_ids {
        buf.clear();
        let _ = write!(&mut buf, "0x{id:x}");
        if ascii_icontains(&buf, needle_lower) {
            return true;
        }
    }
    target_spans(&rec.event)
        .iter()
        .any(|s| ascii_icontains(s.content.as_ref(), needle_lower))
}

/// ASCII case-insensitive substring search that allocates
/// nothing — the haystack is scanned by byte position, with
/// `eq_ignore_ascii_case` over slices of the needle's length.
/// Both inputs are assumed ASCII (the call sites are hex IDs
/// and English command words).
pub(crate) fn ascii_icontains(haystack: &str, needle_lower: &str) -> bool {
    if needle_lower.is_empty() {
        return true;
    }
    let n = needle_lower.len();
    if haystack.len() < n {
        return false;
    }
    haystack
        .as_bytes()
        .windows(n)
        .any(|w| w.eq_ignore_ascii_case(needle_lower.as_bytes()))
}

/// Append active-filter chips to the title row so an operator
/// can tell at a glance that they're not looking at the whole
/// ring. ICE-only is amber (matches the ICE accent); the row
/// cap is dim. When the search prompt is open, the editing
/// state hijacks the chip row so the operator's typing is
/// front-and-center.
fn append_filter_chips(
    spans: &mut Vec<Span<'static>>,
    force_only: bool,
    limit: Option<usize>,
    search: &str,
    search_editing: bool,
) {
    if search_editing {
        spans.push(Span::styled("    / ", theme::amber()));
        spans.push(Span::styled(search.to_string(), theme::green_hi()));
        spans.push(Span::styled("_", theme::amber()));
        spans.push(Span::styled(
            "    [Enter] commit  [Esc] cancel",
            theme::dim(),
        ));
        return;
    }
    if force_only {
        spans.push(Span::styled("    [ICE only]", theme::amber()));
    }
    if !search.is_empty() {
        spans.push(Span::styled(
            format!("    [match /{search}/]"),
            theme::amber(),
        ));
    }
    if let Some(n) = limit {
        spans.push(Span::styled(format!("    [limit {n}]"), theme::dim()));
    }
}

fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Format a `committed_at_ms` (Unix epoch ms) as
/// relative-to-now: "Xs ago" / "Xm ago" / "Xh ago".
fn format_relative(committed_at_ms: u64, now_ms: u64) -> String {
    let delta = now_ms.saturating_sub(committed_at_ms) / 1_000;
    if delta < 60 {
        format!("{delta}s ago")
    } else if delta < 3_600 {
        format!("{}m ago", delta / 60)
    } else {
        format!("{}h ago", delta / 3_600)
    }
}
