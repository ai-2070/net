use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    symbols::Marker,
    text::{Line, Span},
    widgets::{
        canvas::{Canvas, Context, Line as CLine, Points},
        Block, Borders, Paragraph,
    },
    Frame,
};

use crate::{nodes, theme};

struct Node {
    id: &'static str,
    x: f64,
    y: f64,
    kind: NodeKind,
}

#[derive(Clone, Copy)]
enum NodeKind {
    Compute,
    Dataforts,
    Region,
    Device,
}

impl NodeKind {
    fn glyph(self) -> char {
        match self {
            NodeKind::Compute => '◆',
            NodeKind::Dataforts => '■',
            NodeKind::Region => '●',
            NodeKind::Device => '◇',
        }
    }
}

const NODES: &[Node] = &[
    Node { id: "0xa96f", x: -60.0, y:  60.0, kind: NodeKind::Compute   },
    Node { id: "0xe9b8", x: -40.0, y:  55.0, kind: NodeKind::Compute   },
    Node { id: "0xe685", x: -55.0, y:  35.0, kind: NodeKind::Region    },
    Node { id: "0xd4ff", x: -42.0, y:  25.0, kind: NodeKind::Dataforts },
    Node { id: "0x3599", x: -22.0, y:  30.0, kind: NodeKind::Dataforts },
    Node { id: "0x372b", x:   2.0, y:  20.0, kind: NodeKind::Compute   },
    Node { id: "0xeba8", x:  18.0, y:  48.0, kind: NodeKind::Compute   },
    Node { id: "0x82ee", x:  60.0, y:  60.0, kind: NodeKind::Compute   },
    Node { id: "0xbdda", x:  35.0, y:  35.0, kind: NodeKind::Compute   },
    Node { id: "0x6dfb", x: -10.0, y: -10.0, kind: NodeKind::Region    },
    Node { id: "0x3c81", x:  -2.0, y: -32.0, kind: NodeKind::Compute   },
    Node { id: "0xe068", x:  48.0, y:  10.0, kind: NodeKind::Compute   },
    Node { id: "0xbf44", x:  35.0, y:   5.0, kind: NodeKind::Region    },
    Node { id: "0xf206", x:  25.0, y:  -2.0, kind: NodeKind::Dataforts },
    Node { id: "0xf83d", x:  60.0, y: -20.0, kind: NodeKind::Compute   },
    Node { id: "0x6808", x:  72.0, y: -32.0, kind: NodeKind::Region    },
    Node { id: "0x0fc2", x:  62.0, y: -38.0, kind: NodeKind::Device    },
];

const EDGES: &[(&str, &str)] = &[
    ("0xa96f", "0xe9b8"),
    ("0xe9b8", "0xe685"),
    ("0xe685", "0xd4ff"),
    ("0xd4ff", "0x3599"),
    ("0x3599", "0x372b"),
    ("0x372b", "0xeba8"),
    ("0xeba8", "0x82ee"),
    ("0x82ee", "0xbdda"),
    ("0xbdda", "0xe068"),
    ("0xe068", "0xbf44"),
    ("0xbf44", "0xf206"),
    ("0xf206", "0x372b"),
    ("0xbdda", "0xeba8"),
    ("0xf206", "0x6dfb"),
    ("0x6dfb", "0x3c81"),
    ("0xe068", "0xf83d"),
    ("0xf83d", "0x6808"),
    ("0x6808", "0x0fc2"),
    ("0xa96f", "0xe685"),
    ("0xd4ff", "0x6dfb"),
];

fn find(id: &str) -> Option<&'static Node> {
    NODES.iter().find(|n| n.id == id)
}

pub fn render(frame: &mut Frame<'_>, area: Rect, tick: u64) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(9), Constraint::Length(2)])
        .split(area);

    let header = Line::from(vec![
        Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
        Span::styled("MESH.PROXIMITY", theme::green_hi()),
        Span::styled("   17 nodes   20 edges   3 dataforts", theme::chrome()),
    ]);
    let title_block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(header)
        .title_alignment(Alignment::Left);

    let canvas = Canvas::default()
        .block(title_block)
        .marker(Marker::Braille)
        .x_bounds([-80.0, 80.0])
        .y_bounds([-45.0, 70.0])
        .paint(move |ctx| paint_graph(ctx, tick));
    frame.render_widget(canvas, rows[0]);

    render_event_tail(frame, rows[1], tick);
    render_legend(frame, rows[2]);
}

fn paint_graph(ctx: &mut Context<'_>, tick: u64) {
    // Edges first, so node glyphs paint on top.
    for (a, b) in EDGES {
        let (Some(na), Some(nb)) = (find(a), find(b)) else { continue };
        ctx.draw(&CLine {
            x1: na.x,
            y1: na.y,
            x2: nb.x,
            y2: nb.y,
            color: theme::RULE,
        });
    }

    // Highlight a wandering daemon edge (the "in transit" feel).
    let lit_idx = (tick / 8) as usize % EDGES.len();
    let (a, b) = EDGES[lit_idx];
    if let (Some(na), Some(nb)) = (find(a), find(b)) {
        ctx.draw(&CLine {
            x1: na.x,
            y1: na.y,
            x2: nb.x,
            y2: nb.y,
            color: theme::GREEN,
        });
        // Moving daemon dot along the edge.
        let phase = ((tick % 32) as f64) / 32.0;
        let dx = na.x + (nb.x - na.x) * phase;
        let dy = na.y + (nb.y - na.y) * phase;
        ctx.draw(&Points {
            coords: &[(dx, dy)],
            color: theme::CYAN,
        });
        ctx.print(dx + 2.0, dy + 1.0, Line::styled("d.0x69", theme::cyan()));
    }

    // Nodes.
    for n in NODES {
        let color = match n.kind {
            NodeKind::Compute => theme::TEXT,
            NodeKind::Dataforts => theme::GREEN_HI,
            NodeKind::Region => theme::GREEN,
            NodeKind::Device => theme::AMBER,
        };
        ctx.print(
            n.x,
            n.y,
            Line::styled(n.kind.glyph().to_string(), ratatui::style::Style::default().fg(color)),
        );
        // Graph keeps just the id — adding `.label` would crowd
        // neighbors. The label still appears wherever this node
        // is referenced in the event tail, list, or detail views.
        ctx.print(n.x + 2.5, n.y - 2.0, Line::styled(n.id, theme::dim()));
    }
}

fn render_event_tail(frame: &mut Frame<'_>, area: Rect, tick: u64) {
    let header = Line::from(vec![
        Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
        Span::styled("MESH.EVENTS", theme::green_hi()),
        Span::styled("                                                       tail -f autoform.log", theme::chrome()),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(header);

    let base = tick / 2;

    // Helper: build a line line-by-line so we can splice in
    // `id.label` spans for every node reference.
    let mut lines: Vec<Line> = Vec::new();

    // cap.announce 0x3c81 …
    let mut row1 = vec![
        Span::styled(fmt_ts(base + 0), theme::chrome()),
        Span::styled("  ▶ ", theme::green()),
        Span::styled("cap.announce ", theme::cyan()),
    ];
    row1.extend(nodes::id_spans("0x3c81"));
    row1.push(Span::styled(" ", theme::chrome()));
    row1.push(Span::styled("[device] ", theme::amber()));
    row1.push(Span::styled("lat:<200µs sensor:ph ", theme::text()));
    row1.push(Span::styled("capb ann 24ns", theme::chrome()));
    lines.push(Line::from(row1));

    // mikoshi daemon.0x69 0x6dfb → 0xbf44 [daemon]
    let mut row2 = vec![
        Span::styled(fmt_ts(base + 4), theme::chrome()),
        Span::styled("  ↗ ", theme::green()),
        Span::styled("mikoshi   ", theme::green_hi()),
        Span::styled("daemon.0x69 ", theme::text()),
    ];
    row2.extend(nodes::id_spans("0x6dfb"));
    row2.push(Span::styled(" → ", theme::chrome()));
    row2.extend(nodes::id_spans("0xbf44"));
    row2.push(Span::styled(" [daemon]", theme::cyan()));
    lines.push(Line::from(row2));

    // cap.announce 0xf206 …
    let mut row3 = vec![
        Span::styled(fmt_ts(base + 9), theme::chrome()),
        Span::styled("  ▶ ", theme::green()),
        Span::styled("cap.announce ", theme::cyan()),
    ];
    row3.extend(nodes::id_spans("0xf206"));
    row3.push(Span::styled(" ", theme::chrome()));
    row3.push(Span::styled("[region] ", theme::green()));
    row3.push(Span::styled("gpu:gb300 sensor:ph ", theme::text()));
    row3.push(Span::styled("capb ann 14ns", theme::chrome()));
    lines.push(Line::from(row3));

    // gravity_pull daemon.0xc2 0x285e → 0x6dfb [datafort]
    let mut row4 = vec![
        Span::styled(fmt_ts(base + 13), theme::chrome()),
        Span::styled("  ↘ ", theme::green()),
        Span::styled("gravity_pull ", theme::green_hi()),
        Span::styled("daemon.0xc2 ", theme::text()),
        Span::styled("blob.0x285e → ", theme::chrome()),
    ];
    row4.extend(nodes::id_spans("0x6dfb"));
    row4.push(Span::styled(" [datafort]", theme::cyan()));
    lines.push(Line::from(row4));

    // drift_correct nodes(3) 0x6dfb 0x3c81 0x0fc2 reflow
    let mut row5 = vec![
        Span::styled(fmt_ts(base + 18), theme::chrome()),
        Span::styled("  ↻ ", theme::amber()),
        Span::styled("drift_correct ", theme::amber()),
        Span::styled("nodes(3) ", theme::text()),
    ];
    for (i, id) in ["0x6dfb", "0x3c81", "0x0fc2"].iter().enumerate() {
        if i > 0 {
            row5.push(Span::styled(" ", theme::chrome()));
        }
        row5.extend(nodes::id_spans_styled(id, theme::dim()));
    }
    row5.push(Span::styled(" reflow", theme::cyan()));
    lines.push(Line::from(row5));

    let area_inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(lines), area_inner);
}

fn render_legend(frame: &mut Frame<'_>, area: Rect) {
    let legend = Line::from(vec![
        Span::styled("◆ ", theme::text()),
        Span::styled("COMPUTE   ", theme::dim()),
        Span::styled("■ ", theme::green_hi()),
        Span::styled("DATAFORTS   ", theme::dim()),
        Span::styled("● ", theme::green()),
        Span::styled("REGION   ", theme::dim()),
        Span::styled("◇ ", theme::amber()),
        Span::styled("DEVICE   ", theme::dim()),
        Span::styled("• ", theme::cyan()),
        Span::styled("MIKOSHI · IN TRANSIT", theme::cyan()),
    ]);
    frame.render_widget(Paragraph::new(legend).alignment(Alignment::Right), area);
}

fn fmt_ts(t: u64) -> String {
    let mm = (t / 60) % 60;
    let ss = t % 60;
    let ms = (t.wrapping_mul(37)) % 1000;
    format!("{mm:02}:{ss:02}.{ms:03}")
}
