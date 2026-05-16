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
