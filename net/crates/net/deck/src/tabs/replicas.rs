//! CHAINS tab — projects `snapshot.replicas` as a chain-by-
//! chain table. Each row shows the chain id, current holders
//! as `id.label` chips, desired count, elected leader, and a
//! health column derived from holder count vs desired.
//!
//! Cursor (`j`/`k`) selects a chain row; future ICE bindings
//! (`E` force-evict-replica, `O` force-cutover) target the
//! cursored chain.

use net_sdk::deck::MeshOsSnapshot;
use ratatui::{
    layout::{Alignment, Constraint, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Row, Table, TableState},
    Frame,
};

use crate::{nodes, theme, widgets};

pub fn render(frame: &mut Frame<'_>, area: Rect, snapshot: Option<&MeshOsSnapshot>, cursor: usize) {
    let has_replicas = snapshot.map(|s| !s.replicas.is_empty()).unwrap_or(false);
    if has_replicas {
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
            Span::styled("CHAINS", theme::green_hi()),
            Span::styled("    0 chains", theme::chrome()),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    widgets::empty::render(
        frame,
        inner,
        "no replicas reported yet",
        "publish a ReplicaUpdate or wait for a real cluster source",
    );
}

fn render_table(frame: &mut Frame<'_>, area: Rect, snapshot: &MeshOsSnapshot, cursor: usize) {
    let total = snapshot.replicas.len();
    let mut under = 0;
    let mut over = 0;
    let mut ok = 0;
    let mut leaderless = 0;
    for r in snapshot.replicas.values() {
        if r.leader.is_none() {
            leaderless += 1;
        }
        match r.desired_count {
            Some(d) => {
                let held = r.holders.len() as u32;
                match held.cmp(&d) {
                    std::cmp::Ordering::Less => under += 1,
                    std::cmp::Ordering::Greater => over += 1,
                    std::cmp::Ordering::Equal => ok += 1,
                }
            }
            None => ok += 1,
        }
    }

    let pos = cursor.min(total.saturating_sub(1)) + 1;
    let body_h = (area.height as usize).saturating_sub(2).saturating_sub(1);
    let (start, end, hidden_above, hidden_below) = super::scroll_window(total, body_h, cursor);
    let mut title_spans = vec![
        Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
        Span::styled("CHAINS", theme::green_hi()),
        Span::styled(
            format!(
                "    {total} chains · {ok} ok · {under} under · {over} over · {leaderless} leaderless"
            ),
            theme::chrome(),
        ),
        Span::styled(format!("    {pos}/{total}"), theme::dim()),
    ];
    if hidden_above > 0 {
        title_spans.push(Span::styled(
            format!("    ▲ {hidden_above} more"),
            theme::dim(),
        ));
    }
    if hidden_below > 0 {
        title_spans.push(Span::styled(
            format!("    ▼ {hidden_below} more"),
            theme::dim(),
        ));
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(title_spans))
        .title_alignment(Alignment::Left);

    let header = Row::new(vec![
        cell_dim(" "),
        cell_dim("CHAIN"),
        cell_dim("HELD"),
        cell_dim("DESIRED"),
        cell_dim("STATUS"),
        cell_dim("LEADER"),
        cell_dim("HOLDERS"),
    ])
    .height(1);

    let mut rows: Vec<Row> = Vec::with_capacity(end.saturating_sub(start));
    for (offset, (chain, r)) in snapshot
        .replicas
        .iter()
        .skip(start)
        .take(end - start)
        .enumerate()
    {
        let i = start + offset;
        let is_cursor = i == cursor;
        let marker = if is_cursor { "▶" } else { " " };
        let chain_text = format!("chain.0x{chain:x}");
        let chain_style = if is_cursor {
            theme::green_hi()
        } else {
            theme::text()
        };

        let held = r.holders.len();
        let (status_style, status_text) = match r.desired_count {
            Some(d) if (held as u32) == d => (theme::green(), "ok".to_string()),
            Some(d) if (held as u32) < d => (theme::amber(), format!("under -{}", d - held as u32)),
            Some(d) => (theme::amber(), format!("over +{}", held as u32 - d)),
            None => (theme::dim(), "—".to_string()),
        };

        let desired_text = match r.desired_count {
            Some(d) => format!("{d}"),
            None => "—".to_string(),
        };

        // Leader cell as id.label, or — when leaderless.
        let leader_cell = match r.leader {
            Some(leader) => Cell::from(Line::from(nodes::id_spans(&format!("0x{leader:x}")))),
            None => Cell::from(Span::styled("—", theme::red())),
        };

        // Holders — comma-separated `0xNN.label` spans, truncated
        // to the first 4 so the column fits. Extra count shown
        // as `+N more`.
        let mut holder_spans: Vec<Span> = Vec::new();
        let show = 4;
        for (j, h) in r.holders.iter().take(show).enumerate() {
            if j > 0 {
                holder_spans.push(Span::styled(", ", theme::chrome()));
            }
            // Bold the leader if it shows up.
            let id_style = if Some(*h) == r.leader {
                theme::green_hi()
            } else {
                theme::text()
            };
            holder_spans.extend(nodes::id_spans_styled(&format!("0x{h:x}"), id_style));
        }
        if r.holders.len() > show {
            holder_spans.push(Span::styled(
                format!("  +{} more", r.holders.len() - show),
                theme::dim(),
            ));
        }
        if r.holders.is_empty() {
            holder_spans.push(Span::styled("—", theme::red()));
        }

        rows.push(Row::new(vec![
            Cell::from(Span::styled(marker, theme::green_hi())),
            Cell::from(Span::styled(chain_text, chain_style)),
            Cell::from(Span::styled(format!("{held:>4}"), theme::text())),
            Cell::from(Span::styled(format!("{desired_text:>7}"), theme::text())),
            Cell::from(Span::styled(status_text, status_style)),
            leader_cell,
            Cell::from(Line::from(holder_spans)),
        ]));
    }

    let table = Table::new(
        rows,
        [
            Constraint::Length(2),  // cursor
            Constraint::Length(16), // chain.0xN
            Constraint::Length(4),  // held
            Constraint::Length(7),  // desired
            Constraint::Length(10), // status
            Constraint::Length(18), // leader id.label
            Constraint::Min(0),     // holders
        ],
    )
    .header(header)
    .block(block)
    .column_spacing(2);
    let selected = cursor.checked_sub(start).filter(|s| start + *s < end);
    let mut state = TableState::default().with_selected(selected);
    frame.render_stateful_widget(table, area, &mut state);
}

fn cell_dim(s: &'static str) -> Cell<'static> {
    Cell::from(Span::styled(s, theme::chrome()))
}
