//! Canonical node fixture — single source of truth for the
//! visual mock's `id ↔ label` mapping. Every tab references this
//! so a node renders identically wherever it appears:
//! the id in normal text, then `.` and the label in dim chrome.

use ratatui::{style::Style, text::Span};

use crate::theme;

pub struct NodeRef {
    pub id: &'static str,
    pub label: &'static str,
}

// Fixture keys MUST match the `format!("0x{:x}", id)` form the
// deck renders — no leading zeros. `0x0fc2` would mismatch
// against `0xfc2` and the row would render unlabeled; same goes
// for the local node `0x0001` → `0x1`. The `label_of` lookup is
// a strict string compare against this table.
pub const NODES: &[NodeRef] = &[
    // Local node — `this_node` for the samples runtime. Kept in
    // the fixture so the NODES + NET.MAP renderers resolve it
    // through the same path as remote peers.
    NodeRef {
        id: "0x1",
        label: "local",
    },
    // Live event production (5 nodes) — concert stage, audio
    // mix booths, lighting + special effects.
    NodeRef {
        id: "0xa96f",
        label: "main-stage",
    },
    NodeRef {
        id: "0xe9b8",
        label: "side-stage",
    },
    NodeRef {
        id: "0xe685",
        label: "concert-audio",
    },
    NodeRef {
        id: "0xd4ff",
        label: "monitor-mix",
    },
    NodeRef {
        id: "0x3599",
        label: "stage-lighting",
    },
    // Drone swarm + ground station (3 nodes).
    NodeRef {
        id: "0x372b",
        label: "ground-station",
    },
    NodeRef {
        id: "0xeba8",
        label: "scout-3",
    },
    NodeRef {
        id: "0x82ee",
        label: "follower-1",
    },
    // AI inference cluster (3 nodes) — GPU racks running the
    // vision-grasp + chat model harnesses + a KV-cache host.
    NodeRef {
        id: "0xbdda",
        label: "ai-gpu-1",
    },
    NodeRef {
        id: "0x6dfb",
        label: "ai-gpu-2",
    },
    NodeRef {
        id: "0x3c81",
        label: "ai-cache",
    },
    // Robotics cell (2 nodes) — 6-/7-axis arms on a shared
    // gantry, gripper + motion-planning workloads.
    NodeRef {
        id: "0xe068",
        label: "robot-arm",
    },
    NodeRef {
        id: "0xbf44",
        label: "assembly-line",
    },
    // Autonomous vehicle mesh (2 nodes) — chase truck + pit
    // lane support; CAN-FD / EtherCAT busses, ADAS fusion.
    NodeRef {
        id: "0xf206",
        label: "chase-truck",
    },
    NodeRef {
        id: "0x6808",
        label: "pit-lane",
    },
    // Edge drone + vision rig — depth cameras, on-board AI.
    NodeRef {
        id: "0xf83d",
        label: "edge-drone",
    },
    NodeRef {
        id: "0xfc2",
        label: "camera-system",
    },
];

pub fn label_of(id: &str) -> Option<&'static str> {
    NODES.iter().find(|n| n.id == id).map(|n| n.label)
}

/// Richer label resolution that falls back to the peer's
/// capability tags when no fixture entry matches. Order of
/// preference:
///
/// 1. Hardcoded fixture (`NODES`).
/// 2. Any `region:` / `host:` scoped cap, with the prefix
///    stripped — `region:eu-west-3` → `eu-west-3`. Matches
///    the scope-tag shape the substrate's
///    `dataforts::greedy::GreedyConfig` already uses.
/// 3. The first plain (un-scoped) cap as a last resort, so
///    a peer with `["compute.daemon", "meshos.health"]`
///    renders as `…compute.daemon` rather than bare hex.
///
/// `None` only when the id isn't in the fixture AND the cap
/// set is empty.
pub fn label_for(id: &str, caps: &std::collections::BTreeSet<String>) -> Option<String> {
    if let Some(fixture) = NODES.iter().find(|n| n.id == id) {
        return Some(fixture.label.to_string());
    }
    for cap in caps {
        if let Some(rest) = cap.strip_prefix("region:") {
            return Some(rest.to_string());
        }
        if let Some(rest) = cap.strip_prefix("host:") {
            return Some(rest.to_string());
        }
    }
    caps.iter().next().cloned()
}

/// Render `id.label` as a 3-span sequence: id (caller-supplied
/// style), `.` (chrome), label (dim). If the id is unknown to
/// the fixture, returns just the id span.
pub fn id_spans_styled(id: &str, id_style: Style) -> Vec<Span<'static>> {
    match label_of(id) {
        Some(label) => vec![
            Span::styled(id.to_string(), id_style),
            Span::styled(".", theme::chrome()),
            Span::styled(label.to_string(), theme::dim()),
        ],
        None => vec![Span::styled(id.to_string(), id_style)],
    }
}

/// Default styling: id in [`theme::text`] (white-ish), label in
/// [`theme::dim`] (gray).
pub fn id_spans(id: &str) -> Vec<Span<'static>> {
    id_spans_styled(id, theme::text())
}
