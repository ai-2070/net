use net_sdk::deck::{
    DaemonHealthSnapshot, DaemonLifecycleSnapshot, MaintenanceStateSnapshot, MeshOsSnapshot,
    PeerHealthSnapshot, PeerSnapshot,
};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::{app::App, theme};

pub fn render(frame: &mut Frame<'_>, area: Rect, app: &App) {
    // Right column carries codename + version + help. The
    // codename ("ATOMIC PLAYBOYS" today, ~25 cols including
    // the label) pushes the budget past the prior 28-col
    // reservation; 44 fits the row without squeezing the
    // left chips at 80-col widths.
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(44)])
        .split(area);

    let uptime = app.started.elapsed().as_secs();
    let mode_text = if cfg!(feature = "samples") {
        "LIVE +samples"
    } else {
        "LIVE"
    };

    // Pass the synthesized local PeerSnapshot so peer_summary
    // reads its actual `health` field instead of hardcoding
    // "local is Healthy". The synth stamps Healthy today, but
    // a future contract change that lets the local node self-
    // report Degraded / Unreachable propagates here without
    // silently disagreeing with NODES / NET.MAP.
    let local_peer = app.local_peer_snapshot();
    let peers = peer_summary(&app.snapshot, &local_peer);
    let daemons = daemon_summary(&app.snapshot);
    // Substrate's `recently_emitted` is a ring of the last N
    // reconcile-emitted actions (capped by
    // `action_queue_capacity`); the executor doesn't signal
    // completion, so this is NOT a live "in flight" count.
    // Label the chip honestly so operators read it as "recent
    // reconcile work," not "currently pending tasks."
    let recent = app.snapshot.recently_emitted.len();
    let (maint_style, maint_text) = local_maint_summary(&app.snapshot);

    // Single-character separator (` · `) compresses spacing vs
    // the prior triple-space gutters so the whole row fits at
    // 100 cols even with the cluster chip + version on the
    // right. Each chip stays self-describing via its label.
    let sep = || Span::styled(" · ", theme::chrome());

    let mut left = vec![
        Span::styled("● ", theme::green()),
        Span::styled(mode_text, theme::green_hi()),
        sep(),
    ];
    // Remote bookmarks get their cluster name in the header so
    // an operator pivoting between clusters always sees which
    // one they're on; the default `"local"` is implicit and
    // gets omitted to keep the chrome quiet.
    if app.active_cluster != "local" {
        left.push(Span::styled(
            app.active_cluster.clone(),
            theme::amber(),
        ));
        left.push(sep());
    }

    // peers: 17p 14H/2D/0U
    left.push(Span::styled(format!("{}p ", peers.total), theme::text()));
    left.push(Span::styled(format!("{}H", peers.healthy), theme::green()));
    left.push(Span::styled("/", theme::chrome()));
    left.push(Span::styled(format!("{}D", peers.degraded), theme::amber()));
    left.push(Span::styled("/", theme::chrome()));
    left.push(Span::styled(
        format!("{}U", peers.unreachable),
        theme::red(),
    ));
    left.push(sep());

    // daemons: 11d 11R/0B/0X
    left.push(Span::styled(format!("{}d ", daemons.total), theme::text()));
    left.push(Span::styled(
        format!("{}R", daemons.running),
        theme::green(),
    ));
    left.push(Span::styled("/", theme::chrome()));
    left.push(Span::styled(
        format!("{}B", daemons.backing_off),
        theme::amber(),
    ));
    left.push(Span::styled("/", theme::chrome()));
    left.push(Span::styled(
        format!("{}X", daemons.crash_looping),
        theme::red(),
    ));
    left.push(sep());

    // recently emitted: 0 recent — kept dim regardless of
    // count. The substrate's `recently_emitted` ring is a
    // diagnostic counter (last N emitted; executor doesn't
    // signal completion back), not a live-pressure signal
    // worth amber-pulsing in the operator's peripheral view.
    left.push(Span::styled(format!("{recent} recent"), theme::dim()));
    left.push(sep());

    // local maintenance state
    left.push(Span::styled(maint_text, maint_style));
    left.push(sep());

    left.push(Span::styled(format!("{uptime}s"), theme::dim()));

    // Version comes from Cargo at compile time and is always
    // accurate. Codename is the substrate's release milestone
    // (v0.17 → "Atomic Playboys"); kept inline rather than
    // env-injected because the deck ships from the same
    // workspace and tracks the same release cadence.
    let version = concat!("v", env!("CARGO_PKG_VERSION"));
    let codename = "ATOMIC PLAYBOYS";
    let right = Line::from(vec![
        Span::styled("CODENAME: ", theme::chrome()),
        Span::styled(codename, theme::text()),
        Span::styled("   ", theme::chrome()),
        Span::styled(format!("{version}   "), theme::chrome()),
        Span::styled("?", theme::green_hi()),
        Span::styled(" help", theme::dim()),
    ]);

    frame.render_widget(Paragraph::new(Line::from(left)), cols[0]);
    frame.render_widget(Paragraph::new(right).alignment(Alignment::Right), cols[1]);
}

struct PeerSummary {
    total: usize,
    healthy: usize,
    degraded: usize,
    unreachable: usize,
}

fn peer_summary(snap: &MeshOsSnapshot, local: &PeerSnapshot) -> PeerSummary {
    // Local node counts too: NODES + NET.MAP render it
    // alongside remote peers, so the status-bar chip needs
    // to match (17 peers + self = 18). The local PeerSnapshot
    // is synthesized by the App (the substrate fold doesn't
    // insert self into `snap.peers`), and its `health` field
    // is what NODES + NET.MAP read — so this summary stays in
    // lockstep with them.
    let mut healthy = 0;
    let mut degraded = 0;
    let mut unreachable = 0;
    // Pre-pass: classify the local peer.
    match local.health {
        Some(PeerHealthSnapshot::Healthy) => healthy += 1,
        Some(PeerHealthSnapshot::Degraded) => degraded += 1,
        Some(PeerHealthSnapshot::Unreachable) => unreachable += 1,
        _ => {}
    }
    for p in snap.peers.values() {
        match p.health {
            Some(PeerHealthSnapshot::Healthy) => healthy += 1,
            Some(PeerHealthSnapshot::Degraded) => degraded += 1,
            Some(PeerHealthSnapshot::Unreachable) => unreachable += 1,
            _ => {}
        }
    }
    PeerSummary {
        total: snap.peers.len() + 1,
        healthy,
        degraded,
        unreachable,
    }
}

struct DaemonSummary {
    total: usize,
    running: usize,
    backing_off: usize,
    crash_looping: usize,
}

fn daemon_summary(snap: &MeshOsSnapshot) -> DaemonSummary {
    use net_sdk::deck::RestartStateSnapshot;
    let mut running = 0;
    let mut backing_off = 0;
    let mut crash_looping = 0;
    for d in snap.daemons.values() {
        // Restart-state buckets dominate so crash-loop /
        // backoff aren't double-counted as "running" even
        // when the lifecycle still reads Running.
        match d.restart_state {
            RestartStateSnapshot::CrashLooping { .. } => crash_looping += 1,
            RestartStateSnapshot::BackingOff { .. } => backing_off += 1,
            _ => {
                let healthy = matches!(d.health, Some(DaemonHealthSnapshot::Healthy));
                let alive = matches!(d.lifecycle, DaemonLifecycleSnapshot::Running);
                if healthy && alive {
                    running += 1;
                }
            }
        }
    }
    DaemonSummary {
        total: snap.daemons.len(),
        running,
        backing_off,
        crash_looping,
    }
}

fn local_maint_summary(snap: &MeshOsSnapshot) -> (ratatui::style::Style, &'static str) {
    // `MaintenanceStateSnapshot` is `#[non_exhaustive]` so a
    // wildcard is unavoidable — but every *shipped* variant
    // must have an explicit arm above. A future variant
    // surfacing here as the `unknown-maint` fallback is the
    // signal to extend the match; the explicit fallback label
    // beats the prior `"?"` for operator legibility and trips
    // a debug-build assertion so CI catches the gap.
    match snap.local_maintenance {
        MaintenanceStateSnapshot::Active => (theme::green(), "active"),
        MaintenanceStateSnapshot::EnteringMaintenance { .. } => (theme::cyan(), "draining"),
        MaintenanceStateSnapshot::Maintenance { .. } => (theme::cyan(), "maint"),
        MaintenanceStateSnapshot::ExitingMaintenance { .. } => (theme::cyan(), "exiting"),
        MaintenanceStateSnapshot::DrainFailed { .. } => (theme::red(), "DRAIN-FAILED"),
        MaintenanceStateSnapshot::Recovery { .. } => (theme::cyan(), "recovery"),
        ref other => {
            debug_assert!(
                false,
                "local_maint_summary: unrecognised MaintenanceStateSnapshot variant {other:?} — add an explicit arm",
            );
            (theme::amber(), "unknown-maint")
        }
    }
}
