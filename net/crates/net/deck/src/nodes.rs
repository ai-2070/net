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
