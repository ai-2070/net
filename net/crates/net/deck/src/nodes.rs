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

pub const NODES: &[NodeRef] = &[
    NodeRef { id: "0xa96f", label: "eu-west-3" },
    NodeRef { id: "0xe9b8", label: "eu-west-3" },
    NodeRef { id: "0xe685", label: "eu-west-3" },
    NodeRef { id: "0xd4ff", label: "eu-west-3" },
    NodeRef { id: "0x3599", label: "eu-west-3" },
    NodeRef { id: "0x372b", label: "us-east-1" },
    NodeRef { id: "0xeba8", label: "us-east-1" },
    NodeRef { id: "0x82ee", label: "us-east-1" },
    NodeRef { id: "0xbdda", label: "gpu-rig"   },
    NodeRef { id: "0x6dfb", label: "us-east-1" },
    NodeRef { id: "0x3c81", label: "us-east-1" },
    NodeRef { id: "0xe068", label: "ap-south1" },
    NodeRef { id: "0xbf44", label: "ap-south1" },
    NodeRef { id: "0xf206", label: "ap-south1" },
    NodeRef { id: "0xf83d", label: "edge"      },
    NodeRef { id: "0x6808", label: "ap-south1" },
    NodeRef { id: "0x0fc2", label: "lab-bench" },
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
