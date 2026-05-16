pub mod audit;
pub mod blobs;
pub mod daemon_page;
pub mod daemons;
pub mod dataforts;
pub mod failures;
pub mod groups;
pub mod logs;
pub mod migrations;
pub mod net_map;
pub mod node_page;
pub mod nodes;
pub mod replicas;

/// Tiered h/m/s renderer for an age expressed as elapsed
/// milliseconds. Shared across every tab that surfaces a
/// "X ago" column so the format stays consistent: `Xh YYm`
/// over 1h, `Xm YYs` over 1m, `Xs` under 1m (including 0).
/// Sub-second resolution is intentionally dropped — operators
/// reading the FAILURES / MIGRATIONS / DAEMONS columns don't
/// triage by milliseconds.
pub fn format_age_ms(ms: u64) -> String {
    let s = ms / 1_000;
    let m = s / 60;
    let h = m / 60;
    if h > 0 {
        format!("{h}h {:02}m", m % 60)
    } else if m > 0 {
        format!("{m}m {:02}s", s % 60)
    } else {
        format!("{s}s")
    }
}

/// Canonical short-id form for daemons / chains / migrations
/// across the deck. The leading nibbles of the full 64-bit id
/// printed with `{:016x}` so low-numbered ids render as
/// `0x000007` rather than `0x7` — a stable width keeps the
/// LIST / DAEMON / GROUPS columns aligned. Six hex chars
/// gives ~16M distinct prefixes, plenty for human disambig at
/// the tab-level density we render.
pub fn short_id(id: u64) -> String {
    let s = format!("{id:016x}");
    format!("0x{}", &s[..6])
}

/// Compact byte-count: B / KB / MB / GB / TB with one decimal
/// past KB. Truncates to the largest unit where the value
/// reads as ≥1 so a 999-byte blob stays "999B" instead of
/// jumping to "0.9KB". Shared across BLOBS, MIGRATIONS, and
/// the DAEMON.PAGE migration sub-panel so every byte-count
/// column speaks the same magnitude.
pub fn format_bytes(n: u64) -> String {
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

/// Unix-ms wall-clock as `HH:MM:SS.mmm` for the MESH.EVENTS /
/// LOG.TAIL columns. Hours wrap mod 24 so a session crossing
/// midnight reads sensibly without the epoch's day count
/// leaking in.
pub fn fmt_ts_hms_ms(ts_ms: u64) -> String {
    let total_s = ts_ms / 1000;
    let ms = ts_ms % 1000;
    let s = total_s % 60;
    let m = (total_s / 60) % 60;
    let h = (total_s / 3600) % 24;
    format!("{h:02}:{m:02}:{s:02}.{ms:03}")
}

/// Wall-clock unix-ms (best-effort; pre-1970 clocks read 0).
/// Shared across the tabs that surface a "Xs ago" relative
/// time column so the now-anchor lives in one place.
pub fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Glyph + color for a log record, derived from level + a
/// coarse message-content classifier. Replaces the prior
/// dedicated LEVEL column on the LOGS / MESH.EVENTS surfaces
/// — the icon does double duty as a severity tag and an
/// at-a-glance event-category marker.
///
/// Categories (Info level only — Warn / Error / Debug short-
/// circuit to their own glyphs):
///
/// - `▶` announce / publish / commit / verified — broadcast
///   and admin-acknowledgement events.
/// - `↗` started / transfer / snapshot / register — outgoing
///   work the local node initiated.
/// - `↘` drained / fetch / acked / received / completed —
///   incoming or terminal events.
/// - `↻` retry / restart / rotation / reflow / freeze / thaw /
///   rebalance / swept — lifecycle cycling.
/// - `·` default catch-all.
pub fn event_icon(
    rec: &net_sdk::deck::LogRecord,
) -> (char, ratatui::style::Style) {
    use net_sdk::deck::LogLevel;
    // Every glyph here MUST render at one terminal cell —
    // mixing 1-cell and 2-cell characters jitters the source
    // column right by a cell per emoji row. `⚠` (U+26A0) gets
    // emoji presentation by default in Windows Terminal /
    // most modern fonts, so we use `▲` (U+25B2, BMP triangle)
    // instead. `✗` / `▶` / `↗` / `↘` / `↻` / `·` all stay
    // text-presented at one cell.
    match rec.level {
        LogLevel::Error => ('✗', crate::theme::red()),
        LogLevel::Warn => ('▲', crate::theme::amber()),
        LogLevel::Debug => ('·', crate::theme::dim()),
        _ => classify_info(&rec.message),
    }
}

fn classify_info(message: &str) -> (char, ratatui::style::Style) {
    // ASCII case-insensitive substring scan; the fixture
    // vocabulary is English / ASCII so a plain lowercase view
    // is fine. Order matters — first match wins, so the more
    // specific categories (cycle / pull / push) sit before the
    // catch-all dot.
    let lower = message.to_ascii_lowercase();
    let contains_any = |needles: &[&str]| needles.iter().any(|n| lower.contains(n));
    if contains_any(&[
        "announce",
        "advertise",
        "publish",
        "intent",
        "commit",
        "verified",
        "bundle",
    ]) {
        return ('▶', crate::theme::green());
    }
    if contains_any(&[
        "started",
        "transfer",
        "snapshot taken",
        "register",
        "store",
    ]) {
        return ('↗', crate::theme::green());
    }
    if contains_any(&[
        "drained",
        "fetch",
        "acked",
        "received",
        "completed",
        "cleared",
        "swept",
        "pull",
    ]) {
        return ('↘', crate::theme::cyan());
    }
    if contains_any(&[
        "retry",
        "restart",
        "rotation",
        "reflow",
        "freeze",
        "thaw",
        "rebalance",
        "cutover",
        "drain",
    ]) {
        return ('↻', crate::theme::cyan());
    }
    ('·', crate::theme::dim())
}

/// Compact source attribution string for a log record.
/// Mirrors the prior NET.MAP source rule:
/// - daemon id present → `daemon.0x<hex>`
/// - node id present, no daemon → `node.0x<hex>`
/// - neither → `substrate`
pub fn event_source(rec: &net_sdk::deck::LogRecord) -> String {
    match (rec.daemon_id, rec.node_id) {
        (Some(d), _) => format!("daemon.0x{d:x}"),
        (None, Some(n)) => format!("node.0x{n:x}"),
        (None, None) => "substrate".to_string(),
    }
}

/// Render a `LogRecord` as a single ratatui line in the
/// combined format the LOGS + MESH.EVENTS surfaces share:
///
///   `HH:MM:SS.mmm  ICON  source  message`
///
/// Icon + source + message are each styled distinctly so the
/// operator's eye lands on the event category first, the
/// origin second, and the body last.
pub fn render_event_line(
    rec: &net_sdk::deck::LogRecord,
) -> ratatui::text::Line<'static> {
    use ratatui::text::Span;
    let (icon, icon_style) = event_icon(rec);
    let source = event_source(rec);
    // Source pad sized to the widest realistic attribution:
    // `daemon.0x` (9) + up-to-10-hex daemon id = 19 chars.
    // Under-pad and the daemon rows would overflow with no
    // trailing space, jamming the message body up against
    // the source while `node.0x1` rows kept their breathing
    // room — the staggered look the prior `<14` produced.
    const SOURCE_PAD: usize = 19;
    // Source label tracks the icon color so the eye sees one
    // categorical band per row instead of cyan-everywhere vs.
    // category-colored-icon. Warn / error / cycle / announce
    // rows pick up their accent on the attribution column too.
    ratatui::text::Line::from(vec![
        Span::styled(format!("  {}  ", fmt_ts_hms_ms(rec.ts_ms)), crate::theme::chrome()),
        Span::styled(format!("{icon} "), icon_style),
        Span::styled(
            format!("{source:<width$}  ", source = source, width = SOURCE_PAD),
            icon_style,
        ),
        Span::styled(rec.message.clone(), crate::theme::text()),
    ])
}
