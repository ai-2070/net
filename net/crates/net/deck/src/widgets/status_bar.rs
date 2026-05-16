use net_sdk::deck::{
    DaemonHealthSnapshot, DaemonLifecycleSnapshot, MaintenanceStateSnapshot, MeshOsSnapshot,
    PeerHealthSnapshot,
};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::{app::App, theme};

pub fn render(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(75), Constraint::Percentage(25)])
        .split(area);

    let uptime = app.started.elapsed().as_secs();
    let mode_text = if cfg!(feature = "samples") {
        "LIVE +samples"
    } else {
        "LIVE"
    };

    let peers = peer_summary(&app.snapshot);
    let daemons = daemon_summary(&app.snapshot);
    let pending = app.snapshot.pending.len();
    let (maint_style, maint_text) = local_maint_summary(&app.snapshot);

    let mut left = vec![
        Span::styled("● ", theme::green()),
        Span::styled(mode_text, theme::green_hi()),
        Span::styled("   ", theme::chrome()),
        Span::styled("cluster:", theme::chrome()),
        Span::styled(
            format!("{}   ", app.active_cluster),
            if app.active_cluster == "local" {
                theme::green_hi()
            } else {
                theme::amber()
            },
        ),
    ];

    // peers: 17p ·  14H/2D/0U
    left.push(Span::styled(format!("{}p ", peers.total), theme::text()));
    left.push(Span::styled(format!("{}H", peers.healthy), theme::green()));
    left.push(Span::styled("/", theme::chrome()));
    left.push(Span::styled(format!("{}D", peers.degraded), theme::amber()));
    left.push(Span::styled("/", theme::chrome()));
    left.push(Span::styled(format!("{}U", peers.unreachable), theme::red()));
    left.push(Span::styled("   ", theme::chrome()));

    // daemons: 11d ·  11R/0B/0X (running/backoff/crashloop)
    left.push(Span::styled(format!("{}d ", daemons.total), theme::text()));
    left.push(Span::styled(format!("{}R", daemons.running), theme::green()));
    left.push(Span::styled("/", theme::chrome()));
    left.push(Span::styled(format!("{}B", daemons.backing_off), theme::amber()));
    left.push(Span::styled("/", theme::chrome()));
    left.push(Span::styled(format!("{}X", daemons.crash_looping), theme::red()));
    left.push(Span::styled("   ", theme::chrome()));

    // pending: 0 pending
    let pending_style = if pending == 0 { theme::dim() } else { theme::amber() };
    left.push(Span::styled(format!("{pending} pending"), pending_style));
    left.push(Span::styled("   ", theme::chrome()));

    // local maintenance state
    left.push(Span::styled("MAINT: ", theme::chrome()));
    left.push(Span::styled(maint_text, maint_style));
    left.push(Span::styled("   ", theme::chrome()));

    left.push(Span::styled("UP: ", theme::chrome()));
    left.push(Span::styled(format!("{uptime}s"), theme::text()));

    // Version comes from Cargo at compile time and is always
    // accurate. The SHA is opt-in via `DECK_GIT_SHA` (set by a
    // CI build script when shipping); local dev builds show
    // "dev" so the chip never lies about being a release.
    let version = concat!("v", env!("CARGO_PKG_VERSION"));
    let sha = option_env!("DECK_GIT_SHA").unwrap_or("dev");
    let (sha_label_style, sha_style) = if sha == "dev" {
        (theme::chrome(), theme::dim())
    } else {
        (theme::chrome(), theme::text())
    };
    let right = Line::from(vec![
        Span::styled(format!("{version}   "), theme::chrome()),
        Span::styled("SHA: ", sha_label_style),
        Span::styled(format!("{sha}   "), sha_style),
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

fn peer_summary(snap: &MeshOsSnapshot) -> PeerSummary {
    let mut healthy = 0;
    let mut degraded = 0;
    let mut unreachable = 0;
    for p in snap.peers.values() {
        match p.health {
            Some(PeerHealthSnapshot::Healthy) => healthy += 1,
            Some(PeerHealthSnapshot::Degraded) => degraded += 1,
            Some(PeerHealthSnapshot::Unreachable) => unreachable += 1,
            _ => {}
        }
    }
    PeerSummary {
        total: snap.peers.len(),
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

fn local_maint_summary(
    snap: &MeshOsSnapshot,
) -> (ratatui::style::Style, &'static str) {
    match snap.local_maintenance {
        MaintenanceStateSnapshot::Active => (theme::green(), "active"),
        MaintenanceStateSnapshot::EnteringMaintenance { .. } => (theme::cyan(), "draining"),
        MaintenanceStateSnapshot::Maintenance { .. } => (theme::cyan(), "maint"),
        MaintenanceStateSnapshot::ExitingMaintenance { .. } => (theme::cyan(), "exiting"),
        MaintenanceStateSnapshot::DrainFailed { .. } => (theme::red(), "DRAIN-FAILED"),
        MaintenanceStateSnapshot::Recovery { .. } => (theme::cyan(), "recovery"),
        _ => (theme::chrome(), "?"),
    }
}
