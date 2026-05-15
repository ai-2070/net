//! Confirmation-prompt overlay. Every signed admin action
//! flows through this widget — the operator gets a centered
//! box describing what's about to commit, with `[Enter]` and
//! `[Esc]` bindings.
//!
//! Rendered by `App::draw` after the tab content, so it
//! visually sits on top.

use net_sdk::deck::BlastRadius;
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::theme;

/// Typed description of a pending operator action. Carries
/// the human-readable details for rendering; the dispatch
/// side decodes the variant to know which SDK call to fire.
#[derive(Clone, Debug)]
pub enum ConfirmAction {
    /// Restart every daemon on the given node. Reads from
    /// `admin().restart_all_daemons(node)`.
    RestartAllDaemons {
        node: u64,
        /// Pre-formatted `id.label` for the display string.
        node_display: String,
        /// Optional context — how many daemons live on this
        /// node — so the operator sees the blast radius.
        daemon_count: usize,
    },
    /// Mark the node as not accepting new placements. Reads
    /// from `admin().cordon(node)`. Reversible via `Uncordon`.
    Cordon { node: u64, node_display: String },
    /// Reverse a prior cordon. Reads from
    /// `admin().uncordon(node)`.
    Uncordon { node: u64, node_display: String },
    /// Drain the node's workload by the configured deadline.
    /// Kicks the maintenance state machine: Active →
    /// EnteringMaintenance → Maintenance. Reads from
    /// `admin().drain(node, drain_for)`.
    Drain {
        node: u64,
        node_display: String,
        drain_for: std::time::Duration,
    },
    /// Begin a maintenance window on the node. Unlike drain,
    /// no auto-exit — requires an explicit `ExitMaintenance`.
    /// `drain_for: None` defers to the cluster's configured
    /// default deadline. Reads from
    /// `admin().enter_maintenance(node, drain_for)`.
    EnterMaintenance {
        node: u64,
        node_display: String,
        drain_for: Option<std::time::Duration>,
    },
    /// End a maintenance window. Reads from
    /// `admin().exit_maintenance(node)`.
    ExitMaintenance { node: u64, node_display: String },
    /// Clear the node's local avoid list. Reads from
    /// `admin().clear_avoid_list(node)`.
    ClearAvoidList { node: u64, node_display: String },
    /// Force a placement recompute for the node. Reads from
    /// `admin().invalidate_placement(node)`.
    InvalidatePlacement { node: u64, node_display: String },
    /// ICE break-glass: freeze cluster-wide action emission
    /// for `ttl`. Carries the simulator's blast-radius preview
    /// computed at modal-open time.
    IceFreezeCluster {
        ttl: std::time::Duration,
        blast: BlastRadius,
    },
    /// ICE break-glass: cancel an in-effect freeze.
    IceThawCluster { blast: BlastRadius },
    /// ICE break-glass: force-restart a daemon, bypassing the
    /// supervisor's crash-loop backoff gate. Reads from
    /// `ice().force_restart_daemon(daemon)`.
    IceForceRestartDaemon {
        daemon_id: u64,
        daemon_name: String,
        blast: BlastRadius,
    },
    /// Drop every chain the node currently holds. Reads from
    /// `admin().drop_replicas(node, chains)`. `chains_count`
    /// is captured for the modal's blast-radius summary.
    DropReplicas {
        node: u64,
        node_display: String,
        chains: Vec<u64>,
    },
    /// ICE break-glass: flush avoid lists cluster-wide
    /// (`AvoidScope::Global`). Reads from
    /// `ice().flush_avoid_lists(scope)`.
    IceFlushAvoidLists { blast: BlastRadius },
    /// ICE break-glass: abort an in-flight migration on the
    /// node that hosts it. Reads from
    /// `ice().kill_migration(migration)`.
    IceKillMigration {
        migration: u64,
        blast: BlastRadius,
    },
    /// ICE break-glass: force-evict a replica holder bypassing
    /// the scheduler's rebalance cooldown. Reads from
    /// `ice().force_evict_replica(chain, victim)`.
    IceForceEvictReplica {
        chain: u64,
        victim: u64,
        victim_display: String,
        blast: BlastRadius,
    },
    /// ICE break-glass: pin the chain's elected leader to
    /// `target` on the next reconcile pass. Reads from
    /// `ice().force_cutover(chain, target)`.
    IceForceCutover {
        chain: u64,
        target: u64,
        target_display: String,
        blast: BlastRadius,
    },
}

impl ConfirmAction {
    /// True iff this is an ICE break-glass action — modal
    /// renders with a red border + warnings prominent.
    pub fn is_ice(&self) -> bool {
        matches!(
            self,
            Self::IceFreezeCluster { .. }
                | Self::IceThawCluster { .. }
                | Self::IceForceRestartDaemon { .. }
                | Self::IceFlushAvoidLists { .. }
                | Self::IceKillMigration { .. }
                | Self::IceForceEvictReplica { .. }
                | Self::IceForceCutover { .. }
        )
    }

    /// Optional blast-radius preview attached to ICE actions.
    /// Routine commands return `None` — they don't run the
    /// simulator before commit.
    pub fn blast(&self) -> Option<&BlastRadius> {
        match self {
            Self::IceFreezeCluster { blast, .. }
            | Self::IceThawCluster { blast }
            | Self::IceForceRestartDaemon { blast, .. }
            | Self::IceFlushAvoidLists { blast }
            | Self::IceKillMigration { blast, .. }
            | Self::IceForceEvictReplica { blast, .. }
            | Self::IceForceCutover { blast, .. } => Some(blast),
            _ => None,
        }
    }
}

impl ConfirmAction {
    /// One-line headline shown bold at the top of the modal.
    pub fn headline(&self) -> String {
        match self {
            Self::RestartAllDaemons { node_display, .. } => {
                format!("restart all daemons on {node_display}")
            }
            Self::Cordon { node_display, .. } => format!("cordon node {node_display}"),
            Self::Uncordon { node_display, .. } => {
                format!("uncordon node {node_display}")
            }
            Self::Drain {
                node_display,
                drain_for,
                ..
            } => {
                format!(
                    "drain node {node_display}  ·  window {}s",
                    drain_for.as_secs()
                )
            }
            Self::EnterMaintenance {
                node_display,
                drain_for,
                ..
            } => match drain_for {
                Some(d) => format!(
                    "enter maintenance on {node_display}  ·  window {}s",
                    d.as_secs()
                ),
                None => format!("enter maintenance on {node_display}  ·  cluster default"),
            },
            Self::ExitMaintenance { node_display, .. } => {
                format!("exit maintenance on {node_display}")
            }
            Self::ClearAvoidList { node_display, .. } => {
                format!("clear avoid list on {node_display}")
            }
            Self::InvalidatePlacement { node_display, .. } => {
                format!("invalidate placement on {node_display}")
            }
            Self::IceFreezeCluster { ttl, .. } => {
                format!("ICE  freeze cluster  ·  ttl {}s", ttl.as_secs())
            }
            Self::IceThawCluster { .. } => "ICE  thaw cluster".to_string(),
            Self::IceForceRestartDaemon {
                daemon_id,
                daemon_name,
                ..
            } => format!(
                "ICE  force-restart daemon  ·  0x{daemon_id:x} · {daemon_name}"
            ),
            Self::DropReplicas {
                node_display,
                chains,
                ..
            } => format!(
                "drop {} replica(s) on {node_display}",
                chains.len()
            ),
            Self::IceFlushAvoidLists { .. } => {
                "ICE  flush avoid lists  ·  global scope".to_string()
            }
            Self::IceKillMigration { migration, .. } => {
                format!("ICE  kill migration  ·  0x{migration:x}")
            }
            Self::IceForceEvictReplica {
                chain,
                victim_display,
                ..
            } => format!(
                "ICE  force-evict replica  ·  chain.0x{chain:x} from {victim_display}"
            ),
            Self::IceForceCutover {
                chain,
                target_display,
                ..
            } => format!(
                "ICE  force-cutover  ·  chain.0x{chain:x} → {target_display}"
            ),
        }
    }

    /// Multi-line detail body. Each Vec entry is one rendered
    /// row.
    pub fn detail(&self) -> Vec<String> {
        match self {
            Self::RestartAllDaemons { daemon_count, .. } => vec![
                format!("affects {daemon_count} daemon(s) on the host node"),
                "each daemon is stopped and re-spawned by the supervisor".to_string(),
                "fires `admin().restart_all_daemons(node)` — signed,".to_string(),
                "lands on the admin chain with the operator's identity".to_string(),
            ],
            Self::Cordon { .. } => vec![
                "stops new placements from landing on this node".to_string(),
                "existing daemons + replicas stay; no eviction".to_string(),
                "reversible via `[C]` (uncordon) without further effect".to_string(),
                "fires `admin().cordon(node)` — signed, audit-logged".to_string(),
            ],
            Self::Uncordon { .. } => vec![
                "re-admits the node to the placement scorer".to_string(),
                "new replicas + daemons may land here on the next pass".to_string(),
                "no-op if the node was never cordoned".to_string(),
                "fires `admin().uncordon(node)` — signed, audit-logged".to_string(),
            ],
            Self::Drain { drain_for, .. } => vec![
                format!("drains the node within {}s", drain_for.as_secs()),
                "kicks the maintenance state machine: Active →".to_string(),
                "EnteringMaintenance → Maintenance → DrainFailed?".to_string(),
                "replicas evacuate; daemons receive Shutdown control event".to_string(),
                "fires `admin().drain(node, drain_for)` — signed, audit-logged".to_string(),
            ],
            Self::EnterMaintenance { .. } => vec![
                "begins an indefinite maintenance window".to_string(),
                "drain runs to the deadline; node stays Maintenance".to_string(),
                "no auto-exit — requires `[M]` to release".to_string(),
                "fires `admin().enter_maintenance(node, drain_for)`".to_string(),
            ],
            Self::ExitMaintenance { .. } => vec![
                "ends an active maintenance window".to_string(),
                "kicks Maintenance → ExitingMaintenance → Recovery".to_string(),
                "no-op if the node wasn't in maintenance".to_string(),
                "fires `admin().exit_maintenance(node)`".to_string(),
            ],
            Self::ClearAvoidList { .. } => vec![
                "wipes this node's local avoid list".to_string(),
                "previously-avoided peers become eligible immediately".to_string(),
                "reconcile may re-add entries next tick if RTT still bad".to_string(),
                "fires `admin().clear_avoid_list(node)`".to_string(),
            ],
            Self::InvalidatePlacement { .. } => vec![
                "forces a placement recompute on the next tick".to_string(),
                "useful after a capability / topology change".to_string(),
                "no replica moves until the scorer re-runs".to_string(),
                "fires `admin().invalidate_placement(node)`".to_string(),
            ],
            Self::IceFreezeCluster { ttl, .. } => vec![
                format!("freezes cluster-wide action emission for {}s", ttl.as_secs()),
                "reconcile + folds keep running; only outbound actions stop".to_string(),
                "auto-thaws at the deadline; `[T]` cancels early".to_string(),
                "ICE — multi-op signed; lands on the admin chain".to_string(),
            ],
            Self::IceThawCluster { .. } => vec![
                "cancels an in-effect freeze immediately".to_string(),
                "reconcile resumes action emission on the next tick".to_string(),
                "no-op if no freeze is in effect".to_string(),
                "ICE — multi-op signed; lands on the admin chain".to_string(),
            ],
            Self::IceForceRestartDaemon { .. } => vec![
                "force-restarts the daemon, bypassing crash-loop backoff".to_string(),
                "supervisor's BackingOff / CrashLooping gate is cleared".to_string(),
                "use after operator-side recovery — not a routine retry".to_string(),
                "ICE — multi-op signed; lands on the admin chain".to_string(),
            ],
            Self::DropReplicas { chains, .. } => vec![
                format!("evicts {} replica(s) from this node", chains.len()),
                "desired_local_replicas → Drop; reconcile fires".to_string(),
                "DropReplica actions; refill happens elsewhere".to_string(),
                "fires `admin().drop_replicas(node, chains)`".to_string(),
            ],
            Self::IceFlushAvoidLists { .. } => vec![
                "flushes avoid-list entries cluster-wide".to_string(),
                "every node clears its local avoid list".to_string(),
                "reconcile may re-add entries on the next tick".to_string(),
                "ICE — multi-op signed; lands on the admin chain".to_string(),
            ],
            Self::IceKillMigration { .. } => vec![
                "aborts the in-flight migration on its host node".to_string(),
                "MigrationOrchestrator drops the daemon's record".to_string(),
                "no-op on nodes that aren't hosting this migration".to_string(),
                "ICE — multi-op signed; lands on the admin chain".to_string(),
            ],
            Self::IceForceEvictReplica { .. } => vec![
                "evicts the replica holder, bypassing scheduler".to_string(),
                "cooldown + count-driven hysteresis".to_string(),
                "elected chain leader emits the RequestEviction".to_string(),
                "non-leaders fold the event but emit nothing".to_string(),
                "ICE — multi-op signed; lands on the admin chain".to_string(),
            ],
            Self::IceForceCutover { .. } => vec![
                "pins the chain's next placement to the target".to_string(),
                "bypasses the placement scorer for one pass".to_string(),
                "elected chain leader emits the RequestPlacement".to_string(),
                "no-op if target is already a holder".to_string(),
                "ICE — multi-op signed; lands on the admin chain".to_string(),
            ],
        }
    }
}

/// Render the modal centered over `area`. The Clear widget
/// wipes the underlying cells so the modal isn't transparent.
/// ICE actions render with a red border + an extra
/// blast-radius section above the bindings row.
pub fn render(frame: &mut Frame<'_>, area: Rect, action: &ConfirmAction) {
    let is_ice = action.is_ice();
    let modal_height: u16 = if is_ice { 18 } else { 12 };
    let modal_area = center(area, 72, modal_height);
    frame.render_widget(Clear, modal_area);

    let (border_style, title_glyph, title_text, title_color) = if is_ice {
        (
            theme::red(),
            " ❄ ",
            "ICE  BREAK-GLASS",
            theme::RED,
        )
    } else {
        (theme::amber(), " ⚠ ", "CONFIRM", theme::AMBER)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Line::from(vec![
            Span::styled(title_glyph, border_style),
            Span::styled(
                title_text,
                Style::default()
                    .fg(title_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
        ]))
        .title_alignment(Alignment::Left);
    let inner = block.inner(modal_area);
    frame.render_widget(block, modal_area);

    let constraints: Vec<Constraint> = if is_ice {
        vec![
            Constraint::Length(1), // headline
            Constraint::Length(1), // spacer
            Constraint::Length(5), // detail (4 lines + spacer)
            Constraint::Min(0),    // blast radius
            Constraint::Length(1), // bindings
        ]
    } else {
        vec![
            Constraint::Length(1), // headline
            Constraint::Length(1), // spacer
            Constraint::Min(0),    // detail
            Constraint::Length(1), // bindings
        ]
    };
    let bindings_idx = constraints.len() - 1;
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);

    let headline_style = if is_ice {
        Style::default().fg(theme::RED).add_modifier(Modifier::BOLD)
    } else {
        theme::green_hi()
    };
    let headline = Line::from(vec![Span::styled(action.headline(), headline_style)]);
    frame.render_widget(
        Paragraph::new(headline).alignment(Alignment::Center),
        rows[0],
    );

    let detail_lines: Vec<Line> = action
        .detail()
        .into_iter()
        .map(|s| Line::from(Span::styled(s, theme::text())))
        .collect();
    frame.render_widget(
        Paragraph::new(detail_lines).alignment(Alignment::Center),
        rows[2],
    );

    if is_ice {
        if let Some(blast) = action.blast() {
            render_blast_radius(frame, rows[3], blast);
        }
    }

    let bindings = Line::from(vec![
        Span::styled("[Enter]", if is_ice { theme::red() } else { theme::green_hi() }),
        Span::styled(" confirm    ", theme::dim()),
        Span::styled("[Esc]", theme::dim()),
        Span::styled(" cancel", theme::dim()),
    ]);
    frame.render_widget(
        Paragraph::new(bindings).alignment(Alignment::Center),
        rows[bindings_idx],
    );
}

fn render_blast_radius(frame: &mut Frame<'_>, area: Rect, blast: &BlastRadius) {
    let mut lines = Vec::new();
    lines.push(Line::from(vec![Span::styled(
        "── BLAST RADIUS ──",
        theme::chrome(),
    )]));
    lines.push(Line::from(vec![
        Span::styled("affects  ", theme::chrome()),
        Span::styled(
            format!(
                "{} node(s) · {} replica(s) · {} daemon(s)",
                blast.affected_nodes.len(),
                blast.affected_replicas.len(),
                blast.affected_daemons.len()
            ),
            theme::text(),
        ),
    ]));
    if let Some(delay) = blast.estimated_drain_delay {
        lines.push(Line::from(vec![
            Span::styled("delay    ", theme::chrome()),
            Span::styled(format!("~{}s drain", delay.as_secs()), theme::text()),
        ]));
    }
    if blast.placement_stability_delta.abs() > 1e-3 {
        lines.push(Line::from(vec![
            Span::styled("stab Δ   ", theme::chrome()),
            Span::styled(
                format!("{:+.2}", blast.placement_stability_delta),
                theme::amber(),
            ),
        ]));
    }
    for w in blast.warnings.iter().take(3) {
        lines.push(Line::from(vec![
            Span::styled("⚠  ", theme::amber()),
            Span::styled(format!("{w:?}"), theme::amber()),
        ]));
    }
    frame.render_widget(
        Paragraph::new(lines).alignment(Alignment::Center),
        area,
    );
}

fn center(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width.saturating_sub(2));
    let h = height.min(area.height.saturating_sub(2));
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect {
        x,
        y,
        width: w,
        height: h,
    }
}
