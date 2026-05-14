use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table},
    Frame,
};

use crate::theme;

pub fn render(frame: &mut Frame<'_>, area: Rect) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(area);
    render_list(frame, cols[0]);
    render_detail(frame, cols[1]);
}

fn render_list(frame: &mut Frame<'_>, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
            Span::styled("DAEMONS", theme::green_hi()),
            Span::styled("   52 total", theme::chrome()),
        ]));

    let header = Row::new(vec![
        cell_dim(" "),
        cell_dim("DAEMON"),
        cell_dim("KIND"),
        cell_dim("HEALTH"),
    ])
    .height(1);

    let rows = [
        ("▶", "0x69",  "mikoshi",    "Healthy",   theme::green()),
        (" ", "0xc2",  "gravity",    "Healthy",   theme::green()),
        (" ", "0xf1",  "gravity",    "Healthy",   theme::green()),
        (" ", "0xaa1", "scheduler",  "Healthy",   theme::green()),
        (" ", "0xab3", "drift_corr", "Degraded",  theme::amber()),
        (" ", "0xae9", "anti_entr",  "Healthy",   theme::green()),
        (" ", "0xb02", "telemetry",  "Unhealthy", theme::red()),
        (" ", "0xc09", "blob_mover", "Healthy",   theme::green()),
        (" ", "0xd11", "replica_co", "Healthy",   theme::green()),
        (" ", "0xe7b", "fork_coord", "Unhealthy", theme::red()),
    ];

    let table_rows: Vec<Row> = rows
        .iter()
        .map(|(marker, id, kind, health, hs)| {
            let id_style = if *marker == "▶" { theme::green_hi() } else { theme::text() };
            Row::new(vec![
                Cell::from(Span::styled(*marker, theme::green_hi())),
                Cell::from(Span::styled(*id, id_style)),
                Cell::from(Span::styled(*kind, theme::cyan())),
                Cell::from(Span::styled(*health, *hs)),
            ])
        })
        .collect();

    let table = Table::new(
        table_rows,
        [
            Constraint::Length(2),
            Constraint::Length(6),
            Constraint::Length(12),
            Constraint::Length(10),
        ],
    )
    .header(header)
    .block(block)
    .column_spacing(2);
    frame.render_widget(table, area);
}

fn render_detail(frame: &mut Frame<'_>, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
            Span::styled("DAEMON ", theme::green_hi()),
            Span::styled("0x69 ", theme::text()),
            Span::styled("· mikoshi", theme::cyan()),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(8),  // facts
            Constraint::Length(7),  // saturation history
            Constraint::Min(0),     // log tail + controls
        ])
        .split(inner);

    render_facts(frame, rows[0]);
    render_saturation(frame, rows[1]);
    render_log_tail(frame, rows[2]);
}

fn render_facts(frame: &mut Frame<'_>, area: Rect) {
    let lines = vec![
        kv("identity   ", "ent.0x69d8af11 · ed25519:k7xRq…9pwn"),
        kv("origin     ", "0x69d8af11 · spawned 0xa96f → migrated 0xbf44"),
        kv("kind       ", "mikoshi · v1.2.0 · sig-verified"),
        kv("lifecycle  ", "Running · started 2026.05.14 11:23:48 · age 2h 14m"),
        kv("health     ", "Healthy · last probe 380ms ago"),
        kv("saturation ", "0.31 · drain budget 2.04ms/event"),
        kv("capability ", "[compute, gpu:gb300, region:ap-south1]"),
        kv("placement  ", "node.0xbf44 · score 0.91 · stable"),
    ];
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_saturation(frame: &mut Frame<'_>, area: Rect) {
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled("SATURATION  ", theme::chrome()),
            Span::styled("60s window · 1s buckets", theme::dim()),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // hand-pasted "spark" silhouette so it reads at a glance
    let row0 = "▁ ▁ ▂ ▁ ▂ ▃ ▂ ▃ ▄ ▃ ▄ ▅ ▄ ▆ ▅ ▆ ▇ ▆ ▅ ▄ ▃ ▂ ▃ ▂ ▁ ▂ ▁ ▂ ▁ ▂ ▁ ▁";
    let row1 = "0.00                              0.50                              1.00";
    let label = Line::from(vec![
        Span::styled("p50 ", theme::chrome()),
        Span::styled("0.31  ", theme::green_hi()),
        Span::styled("p99 ", theme::chrome()),
        Span::styled("0.72  ", theme::amber()),
        Span::styled("max ", theme::chrome()),
        Span::styled("0.84", theme::amber()),
    ]);
    let lines = vec![
        Line::styled(row0, theme::green()),
        Line::styled(row1, theme::chrome()),
        label,
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_log_tail(frame: &mut Frame<'_>, area: Rect) {
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled("LOG.TAIL  ", theme::chrome()),
            Span::styled("daemon.0x69 · INFO+", theme::dim()),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(2)])
        .split(inner);

    let lines = vec![
        log_line("11:24:01.882", "INFO", "started on node.0xbf44"),
        log_line("11:24:01.917", "INFO", "subscribed to gravity_pull channel"),
        log_line("11:24:02.044", "INFO", "warm cache 12.4 MB ready"),
        log_line("11:25:12.310", "INFO", "snapshot taken seq=4912"),
        log_line("12:48:55.001", "WARN", "drift_correct nudge: −2.1ms vs anchor"),
        log_line("13:37:21.500", "INFO", "migrated to 0xbf44 ← 0x6dfb (cutover 280ns)"),
        log_line("13:37:21.880", "INFO", "replay caught up · 18 events"),
    ];
    frame.render_widget(Paragraph::new(lines), rows[0]);

    let controls = Line::from(vec![
        Span::styled("[r] ", theme::green_hi()),
        Span::styled("restart   ", theme::dim()),
        Span::styled("[d] ", theme::green_hi()),
        Span::styled("drain    ", theme::dim()),
        Span::styled("[m] ", theme::green_hi()),
        Span::styled("migrate  ", theme::dim()),
        Span::styled("[i] ", theme::green_hi()),
        Span::styled("inspect  ", theme::dim()),
        Span::styled("[k] ", theme::red()),
        Span::styled("kill", theme::dim()),
    ]);
    frame.render_widget(Paragraph::new(controls), rows[1]);
}

fn kv(k: &'static str, v: &'static str) -> Line<'static> {
    Line::from(vec![
        Span::styled(k, theme::chrome()),
        Span::styled(v, theme::text()),
    ])
}

fn log_line(ts: &'static str, level: &'static str, msg: &'static str) -> Line<'static> {
    let level_style = match level {
        "INFO" => theme::dim(),
        "WARN" => theme::amber(),
        "ERR" | "ERROR" => theme::red(),
        _ => theme::text(),
    };
    Line::from(vec![
        Span::styled(ts, theme::chrome()),
        Span::styled("  ", theme::chrome()),
        Span::styled(format!("{level:<5}"), level_style),
        Span::styled("  ", theme::chrome()),
        Span::styled(msg, theme::text()),
    ])
}

fn cell_dim(s: &'static str) -> Cell<'static> {
    Cell::from(Span::styled(s, theme::chrome()))
}
