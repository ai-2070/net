use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Row, Table},
    Frame,
};

use crate::theme;

pub fn render(frame: &mut Frame<'_>, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(area);
    render_nodes_table(frame, rows[0]);
    render_daemons_table(frame, rows[1]);
}

fn render_nodes_table(frame: &mut Frame<'_>, area: Rect) {
    let header_line = Line::from(vec![
        Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
        Span::styled("NODES", theme::green_hi()),
        Span::styled("    17 total   14 healthy   2 degraded   1 maintenance", theme::chrome()),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(header_line);

    let header = Row::new(vec![
        cell_dim("NODE"),
        cell_dim("KIND"),
        cell_dim("REGION"),
        cell_dim("HEALTH"),
        cell_dim("RTT.P50"),
        cell_dim("SAT"),
        cell_dim("DAEMONS"),
        cell_dim("MAINT"),
    ])
    .height(1);

    let rows_data = [
        ("0xa96f", "compute",   "eu-west-3", "Healthy",  "  41µs", "0.42", "  3", "—"),
        ("0xe9b8", "compute",   "eu-west-3", "Healthy",  "  39µs", "0.51", "  4", "—"),
        ("0xe685", "region",    "eu-west-3", "Healthy",  "  12µs", "0.18", "  1", "—"),
        ("0xd4ff", "datafort",  "eu-west-3", "Healthy",  "  44µs", "0.66", "  2", "—"),
        ("0x3599", "datafort",  "eu-west-3", "Healthy",  "  47µs", "0.71", "  2", "—"),
        ("0x372b", "compute",   "us-east-1", "Healthy",  "  88µs", "0.33", "  6", "—"),
        ("0xeba8", "compute",   "us-east-1", "Degraded", " 244µs", "0.91", "  9", "—"),
        ("0x82ee", "compute",   "us-east-1", "Healthy",  "  92µs", "0.40", "  3", "—"),
        ("0xbdda", "compute",   "us-east-1", "Healthy",  "  85µs", "0.55", "  5", "—"),
        ("0x6dfb", "region",    "us-east-1", "Healthy",  "  31µs", "0.22", "  2", "—"),
        ("0x3c81", "compute",   "us-east-1", "Maint.",   "  —   ", "0.00", "  0", "drain"),
        ("0xe068", "compute",   "ap-south1", "Healthy",  " 162µs", "0.48", "  4", "—"),
        ("0xbf44", "region",    "ap-south1", "Healthy",  "  29µs", "0.20", "  1", "—"),
        ("0xf206", "datafort",  "ap-south1", "Healthy",  " 167µs", "0.62", "  2", "—"),
        ("0xf83d", "compute",   "ap-south1", "Healthy",  " 159µs", "0.39", "  3", "—"),
        ("0x6808", "region",    "ap-south1", "Degraded", " 451µs", "0.88", "  2", "—"),
        ("0x0fc2", "device",    "ap-south1", "Healthy",  "  —   ", "—   ", "  0", "—"),
    ];

    let table_rows: Vec<Row> = rows_data
        .iter()
        .map(|(id, kind, region, health, rtt, sat, daemons, maint)| {
            let health_style = match *health {
                "Healthy" => theme::green(),
                "Degraded" => theme::amber(),
                "Maint." => theme::cyan(),
                _ => theme::red(),
            };
            let maint_style = if *maint == "—" { theme::chrome() } else { theme::cyan() };
            Row::new(vec![
                Cell::from(Span::styled(*id, theme::text())),
                Cell::from(Span::styled(*kind, theme::dim())),
                Cell::from(Span::styled(*region, theme::dim())),
                Cell::from(Span::styled(*health, health_style)),
                Cell::from(Span::styled(*rtt, theme::text())),
                Cell::from(Span::styled(*sat, theme::text())),
                Cell::from(Span::styled(*daemons, theme::text())),
                Cell::from(Span::styled(*maint, maint_style)),
            ])
        })
        .collect();

    let table = Table::new(
        table_rows,
        [
            Constraint::Length(8),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(8),
            Constraint::Length(6),
            Constraint::Length(7),
            Constraint::Length(8),
        ],
    )
    .header(header)
    .block(block)
    .column_spacing(2);
    frame.render_widget(table, area);
}

fn render_daemons_table(frame: &mut Frame<'_>, area: Rect) {
    let header_line = Line::from(vec![
        Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
        Span::styled("DAEMONS", theme::green_hi()),
        Span::styled("    52 total   48 running   3 backoff   1 crash-loop", theme::chrome()),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(header_line)
        .title_alignment(Alignment::Left);

    let header = Row::new(vec![
        cell_dim("DAEMON"),
        cell_dim("KIND"),
        cell_dim("NODE"),
        cell_dim("LIFE"),
        cell_dim("HEALTH"),
        cell_dim("SAT"),
        cell_dim("RESTARTS"),
        cell_dim("AGE"),
    ])
    .height(1);

    let rows_data = [
        ("0x69",   "mikoshi",     "0xbf44", "Running",  "Healthy",  "0.31", "0", "2h 14m"),
        ("0xc2",   "gravity",     "0x6dfb", "Running",  "Healthy",  "0.42", "0", "5h 03m"),
        ("0xf1",   "gravity",     "0x372b", "Running",  "Healthy",  "0.38", "0", "5h 03m"),
        ("0xaa1",  "scheduler",   "0xa96f", "Running",  "Healthy",  "0.12", "0", "11h 27m"),
        ("0xab3",  "drift_corr",  "0xeba8", "Running",  "Degraded", "0.91", "2", "37m"),
        ("0xae9",  "anti_entr",   "0xd4ff", "Running",  "Healthy",  "0.55", "0", "2d 04h"),
        ("0xb02",  "telemetry",   "0xeba8", "Backoff",  "Unhealthy","0.00", "5", "—"),
        ("0xc09",  "blob_mover",  "0x3599", "Running",  "Healthy",  "0.66", "0", "8h 21m"),
        ("0xd11",  "replica_co",  "0x82ee", "Running",  "Healthy",  "0.23", "0", "9h 47m"),
        ("0xe7b",  "fork_coord",  "0xbdda", "Crash-loop","Unhealthy","0.00", "12", "—"),
    ];

    let table_rows: Vec<Row> = rows_data
        .iter()
        .map(|(d, kind, node, life, health, sat, restarts, age)| {
            let life_style = match *life {
                "Running" => theme::green(),
                "Backoff" => theme::amber(),
                "Crash-loop" => theme::red(),
                _ => theme::dim(),
            };
            let health_style = match *health {
                "Healthy" => theme::green(),
                "Degraded" => theme::amber(),
                "Unhealthy" => theme::red(),
                _ => theme::dim(),
            };
            let restart_style = if *restarts == "0" { theme::dim() } else { theme::amber() };
            Row::new(vec![
                Cell::from(Span::styled(*d, theme::text())),
                Cell::from(Span::styled(*kind, theme::cyan())),
                Cell::from(Span::styled(*node, theme::dim())),
                Cell::from(Span::styled(*life, life_style)),
                Cell::from(Span::styled(*health, health_style)),
                Cell::from(Span::styled(*sat, theme::text())),
                Cell::from(Span::styled(*restarts, restart_style)),
                Cell::from(Span::styled(*age, theme::dim())),
            ])
        })
        .collect();

    let table = Table::new(
        table_rows,
        [
            Constraint::Length(7),
            Constraint::Length(12),
            Constraint::Length(8),
            Constraint::Length(11),
            Constraint::Length(10),
            Constraint::Length(6),
            Constraint::Length(8),
            Constraint::Length(9),
        ],
    )
    .header(header)
    .block(block)
    .column_spacing(2);
    frame.render_widget(table, area);
}

fn cell_dim(s: &'static str) -> Cell<'static> {
    Cell::from(Span::styled(s, theme::chrome()))
}
