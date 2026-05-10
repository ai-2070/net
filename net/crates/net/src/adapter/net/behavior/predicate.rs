//! Capability predicate AST — Phase A foundation for the federated
//! query primitives in `CAPABILITY_SYSTEM_PLAN.md` §6a.
//!
//! Ships the `Predicate` enum with all 17 variants the substrate plan
//! pins, an evaluator that takes a `(tags, metadata)` context, and
//! constructor helpers + the `pred!` macro that the cross-binding
//! SDK plan exposes language-idiomatic builders for.
//!
//! ## Variants
//!
//! Existence + equality (axis tags):
//! - [`Predicate::Exists`] — tag with this `(axis, key)` is present.
//! - [`Predicate::Equals`] — tag's value matches exactly.
//!
//! Numeric (axis tags whose value parses to `f64`):
//! - [`Predicate::NumericAtLeast`] / [`Predicate::NumericAtMost`] / [`Predicate::NumericInRange`]
//!
//! Semver (axis tags whose value parses to `MAJOR.MINOR.PATCH`):
//! - [`Predicate::SemverAtLeast`] / [`Predicate::SemverAtMost`]
//! - [`Predicate::SemverCompatible`] — same major-version family
//!   (or, for `0.x.y`, same minor) per the standard semver
//!   compatibility rules.
//!
//! String (axis tag values):
//! - [`Predicate::StringPrefix`] — value starts with the prefix.
//! - [`Predicate::StringMatches`] — value contains the substring.
//!   Phase E will swap this to regex behind the existing `regex`
//!   feature gate; semantics today are substring-only.
//!
//! Metadata (the `BTreeMap<String, String>` field added in Phase C):
//! - [`Predicate::MetadataExists`] / [`Predicate::MetadataEquals`]
//! - [`Predicate::MetadataMatches`] (substring; same Phase-E swap)
//! - [`Predicate::MetadataNumericAtLeast`]
//!
//! Boolean composition:
//! - [`Predicate::And`] / [`Predicate::Or`] / [`Predicate::Not`]
//!
//! ## Evaluation
//!
//! `Predicate::evaluate` is a pure function over [`EvalContext`]
//! (`(tags, metadata)`) — no I/O, no allocation outside what the
//! pattern variants explicitly need (regex compilation lands with
//! the Phase E swap). Numeric / semver parse failures evaluate to
//! `false` rather than panicking; cross-binding queries should not
//! fault on a malformed tag value.

use std::collections::BTreeMap;

use crate::adapter::net::behavior::tag::{Tag, TagKey};

// =============================================================================
// EvalContext
// =============================================================================

/// `(tags, metadata)` context passed to [`Predicate::evaluate`].
/// Decoupled from `CapabilitySet` so the predicate evaluator works
/// against the substrate's pre-Phase-A.5 capability shape AND the
/// post-migration shape (`tags: HashSet<Tag>`) without churn.
#[derive(Debug, Clone, Copy)]
pub struct EvalContext<'a> {
    /// Tag set against which axis predicates evaluate.
    pub tags: &'a [Tag],
    /// Key-value metadata against which metadata predicates evaluate.
    pub metadata: &'a BTreeMap<String, String>,
}

impl<'a> EvalContext<'a> {
    /// Build a context from explicit slices. The most common
    /// constructor for callers that hold a `Vec<Tag>` or `&[Tag]`.
    pub fn new(tags: &'a [Tag], metadata: &'a BTreeMap<String, String>) -> Self {
        Self { tags, metadata }
    }
}

// =============================================================================
// Predicate
// =============================================================================

/// AST for capability queries. Pure data — clones, equality, and
/// serde round-trip are the basis of cross-binding wire format.
///
/// See module docs for the variant taxonomy.
// `PartialEq` only because `f64` doesn't implement `Eq` (NaN
// asymmetry). Predicate equality is structural, not hashable —
// we never use it as a HashMap key.
//
// Serde derive intentionally OMITTED for Phase A. The recursive
// `Box<Predicate>` + `Vec<Predicate>` shape compounds with the
// existing `event::*` serializer monomorphization graph and
// pushes the test-build's recursion-limit / compile-time past
// the project's budget. Phase E (federated query primitives)
// adds cross-binding wire format with a flat-tree IR (or
// postcard, which handles recursion better than serde_json's
// derive expansion). For Phase A, the AST + evaluator are
// process-local — no need to serialize.
#[derive(Debug, Clone, PartialEq)]
pub enum Predicate {
    // ---- Axis tags: existence + equality --------------------------------
    /// Tag with this `(axis, key)` is present (regardless of value).
    Exists {
        /// Tag key to probe.
        key: TagKey,
    },
    /// Tag's value matches exactly. Presence-only tags don't match
    /// (use [`Predicate::Exists`] for that).
    Equals {
        /// Tag key.
        key: TagKey,
        /// Required value (string-equality).
        value: String,
    },

    // ---- Axis tags: numeric ---------------------------------------------
    /// Tag's value parses to `f64` and is `>= threshold`.
    NumericAtLeast {
        /// Tag key.
        key: TagKey,
        /// Inclusive lower bound.
        threshold: f64,
    },
    /// Tag's value parses to `f64` and is `<= threshold`.
    NumericAtMost {
        /// Tag key.
        key: TagKey,
        /// Inclusive upper bound.
        threshold: f64,
    },
    /// Tag's value parses to `f64` and lies in `[min, max]` inclusive.
    NumericInRange {
        /// Tag key.
        key: TagKey,
        /// Inclusive lower bound.
        min: f64,
        /// Inclusive upper bound.
        max: f64,
    },

    // ---- Axis tags: semver ----------------------------------------------
    /// Tag's value parses to `MAJOR.MINOR.PATCH` and is `>= version`.
    SemverAtLeast {
        /// Tag key.
        key: TagKey,
        /// Reference version.
        version: String,
    },
    /// Tag's value parses to `MAJOR.MINOR.PATCH` and is `<= version`.
    SemverAtMost {
        /// Tag key.
        key: TagKey,
        /// Reference version.
        version: String,
    },
    /// Tag's value parses to `MAJOR.MINOR.PATCH` and is in the same
    /// compatibility band: same major for `>= 1.0.0`, same minor for
    /// `0.x.y`. Mirrors the standard semver caret-compatibility rule.
    SemverCompatible {
        /// Tag key.
        key: TagKey,
        /// Reference version.
        version: String,
    },

    // ---- Axis tags: string ----------------------------------------------
    /// Tag's value starts with `prefix`.
    StringPrefix {
        /// Tag key.
        key: TagKey,
        /// Prefix to match.
        prefix: String,
    },
    /// Tag's value contains `pattern` as a substring. Phase E will
    /// upgrade to regex behind the `regex` feature gate; semantics
    /// today are substring-only.
    StringMatches {
        /// Tag key.
        key: TagKey,
        /// Substring pattern.
        pattern: String,
    },

    // ---- Metadata -------------------------------------------------------
    /// Metadata key is present.
    MetadataExists {
        /// Metadata key.
        key: String,
    },
    /// Metadata value matches exactly.
    MetadataEquals {
        /// Metadata key.
        key: String,
        /// Required value (string-equality).
        value: String,
    },
    /// Metadata value contains `pattern` as a substring (same
    /// substring-only semantics as [`Predicate::StringMatches`]).
    MetadataMatches {
        /// Metadata key.
        key: String,
        /// Substring pattern.
        pattern: String,
    },
    /// Metadata value parses to `f64` and is `>= threshold`.
    MetadataNumericAtLeast {
        /// Metadata key.
        key: String,
        /// Inclusive lower bound.
        threshold: f64,
    },

    // ---- Boolean composition --------------------------------------------
    /// Conjunction. Empty `Vec` evaluates to `true` (vacuous match —
    /// matches the standard math/logic convention; pin in tests).
    And(Vec<Predicate>),
    /// Disjunction. Empty `Vec` evaluates to `false` (vacuous miss).
    Or(Vec<Predicate>),
    /// Negation.
    Not(Box<Predicate>),
}

// =============================================================================
// Wire format — Phase 5 of CAPABILITY_ENHANCEMENTS_PLAN.md.
//
// The recursive `Box<Predicate>` + `Vec<Predicate>` shape compounds
// with the existing `event::*` serializer monomorphization graph
// and pushes test-build recursion-limit / compile-time past the
// project's budget (per the comment at the head of this module).
//
// The flat-tree IR below sidesteps that: nodes live in a single
// `Vec<PredicateNodeWire>`; And/Or/Not reference children via
// `u32` indices into that table. No variant of `PredicateNodeWire`
// transitively references `PredicateWire` itself, so serde derive
// expansion stays bounded.
//
// Round-trip:
//
//   Predicate::to_wire()        →  PredicateWire
//   PredicateWire::into_predicate() →  Result<Predicate, _>
//
// Pinned in `wire_round_trip_*` tests below.
// =============================================================================

/// One node in the flat predicate wire format. `And`/`Or`/`Not`
/// reference their children via `u32` indices into the parent
/// [`PredicateWire`]'s `nodes` table.
///
/// Node ordering invariant: children always appear at lower
/// indices than their parent (post-order serialization). The
/// rebuild path enforces this to catch malformed wire payloads
/// that attempt index cycles.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum PredicateNodeWire {
    /// Leaf: tag with this `(axis, key)` is present.
    Exists {
        /// Tag key.
        key: TagKey,
    },
    /// Leaf: tag's value matches exactly.
    Equals {
        /// Tag key.
        key: TagKey,
        /// Required value.
        value: String,
    },
    /// Leaf: tag's value parses to `f64` and is `>= threshold`.
    NumericAtLeast {
        /// Tag key.
        key: TagKey,
        /// Inclusive lower bound.
        threshold: f64,
    },
    /// Leaf: tag's value parses to `f64` and is `<= threshold`.
    NumericAtMost {
        /// Tag key.
        key: TagKey,
        /// Inclusive upper bound.
        threshold: f64,
    },
    /// Leaf: tag's value parses to `f64` and lies in `[min, max]`.
    NumericInRange {
        /// Tag key.
        key: TagKey,
        /// Inclusive lower bound.
        min: f64,
        /// Inclusive upper bound.
        max: f64,
    },
    /// Leaf: tag's value parses to a semver triple and is `>= version`.
    SemverAtLeast {
        /// Tag key.
        key: TagKey,
        /// Reference version.
        version: String,
    },
    /// Leaf: tag's value parses to a semver triple and is `<= version`.
    SemverAtMost {
        /// Tag key.
        key: TagKey,
        /// Reference version.
        version: String,
    },
    /// Leaf: tag's value parses to a semver triple and is in the
    /// same compatibility band as `version`.
    SemverCompatible {
        /// Tag key.
        key: TagKey,
        /// Reference version.
        version: String,
    },
    /// Leaf: tag's value starts with `prefix`.
    StringPrefix {
        /// Tag key.
        key: TagKey,
        /// Prefix to match.
        prefix: String,
    },
    /// Leaf: tag's value contains `pattern` as a substring.
    StringMatches {
        /// Tag key.
        key: TagKey,
        /// Substring pattern.
        pattern: String,
    },
    /// Leaf: metadata key is present.
    MetadataExists {
        /// Metadata key.
        key: String,
    },
    /// Leaf: metadata value matches exactly.
    MetadataEquals {
        /// Metadata key.
        key: String,
        /// Required value.
        value: String,
    },
    /// Leaf: metadata value contains `pattern` as a substring.
    MetadataMatches {
        /// Metadata key.
        key: String,
        /// Substring pattern.
        pattern: String,
    },
    /// Leaf: metadata value parses to `f64` and is `>= threshold`.
    MetadataNumericAtLeast {
        /// Metadata key.
        key: String,
        /// Inclusive lower bound.
        threshold: f64,
    },
    /// Composite: conjunction of children at the named indices.
    And {
        /// Child indices into the parent `PredicateWire::nodes`.
        children: Vec<u32>,
    },
    /// Composite: disjunction of children at the named indices.
    Or {
        /// Child indices into the parent `PredicateWire::nodes`.
        children: Vec<u32>,
    },
    /// Composite: negation of the child at the named index.
    Not {
        /// Child index into the parent `PredicateWire::nodes`.
        child: u32,
    },
}

/// Wire format for [`Predicate`]. Flat node table with index
/// references for `And`/`Or`/`Not` children.
///
/// Phase 5 of `CAPABILITY_ENHANCEMENTS_PLAN.md`. Crosses the
/// nRPC envelope as serde-encoded bytes (postcard for cross-binding,
/// JSON for debug fixtures); the substrate's capability
/// announcement path is unchanged.
///
/// Build via [`Predicate::to_wire`]; rebuild via
/// [`PredicateWire::into_predicate`].
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PredicateWire {
    /// Flat node table. Children always live at lower indices
    /// than their parents.
    pub nodes: Vec<PredicateNodeWire>,
    /// Index of the root node within `nodes`. Always
    /// `nodes.len() - 1` for a freshly-emitted `to_wire()` output;
    /// callers receiving an externally-built wire payload should
    /// not assume that.
    pub root_idx: u32,
}

/// Errors raised by [`PredicateWire::into_predicate`].
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PredicateWireError {
    /// Wire payload had an empty `nodes` table.
    #[error("predicate wire has empty nodes table")]
    Empty,
    /// `root_idx` was out of bounds for the `nodes` table.
    #[error("predicate wire root_idx {root_idx} >= nodes len {len}")]
    RootOutOfBounds {
        /// The provided `root_idx`.
        root_idx: u32,
        /// Length of the `nodes` table.
        len: usize,
    },
    /// A composite node referenced a child index that was out of
    /// bounds.
    #[error("predicate wire child index {child} out of bounds for nodes len {len}")]
    ChildOutOfBounds {
        /// The malformed child index.
        child: u32,
        /// Length of the `nodes` table.
        len: usize,
    },
    /// A composite node referenced a child index that was greater
    /// than or equal to its own. Catches index cycles introduced
    /// by malformed / malicious wire payloads.
    #[error("predicate wire child index {child} >= parent index {parent} (cycle)")]
    CycleDetected {
        /// Parent node index.
        parent: u32,
        /// Offending child index.
        child: u32,
    },
}

impl Predicate {
    /// Convert to the flat wire format. Post-order serialization:
    /// leaves land first, the root has the highest index.
    ///
    /// Output is byte-stable across calls — two `to_wire()`s on
    /// equal predicates produce identical `PredicateWire` values
    /// (and identical bytes through any serde encoder).
    pub fn to_wire(&self) -> PredicateWire {
        let mut nodes = Vec::new();
        let root_idx = self.append_to_wire(&mut nodes);
        PredicateWire { nodes, root_idx }
    }

    /// Recursive helper: append `self` (and any sub-tree) into
    /// `nodes`, returning the index of the root of the sub-tree.
    /// Post-order: children push first, then the parent referring
    /// to them by index.
    fn append_to_wire(&self, nodes: &mut Vec<PredicateNodeWire>) -> u32 {
        let node = match self {
            Self::Exists { key } => PredicateNodeWire::Exists { key: key.clone() },
            Self::Equals { key, value } => PredicateNodeWire::Equals {
                key: key.clone(),
                value: value.clone(),
            },
            Self::NumericAtLeast { key, threshold } => PredicateNodeWire::NumericAtLeast {
                key: key.clone(),
                threshold: *threshold,
            },
            Self::NumericAtMost { key, threshold } => PredicateNodeWire::NumericAtMost {
                key: key.clone(),
                threshold: *threshold,
            },
            Self::NumericInRange { key, min, max } => PredicateNodeWire::NumericInRange {
                key: key.clone(),
                min: *min,
                max: *max,
            },
            Self::SemverAtLeast { key, version } => PredicateNodeWire::SemverAtLeast {
                key: key.clone(),
                version: version.clone(),
            },
            Self::SemverAtMost { key, version } => PredicateNodeWire::SemverAtMost {
                key: key.clone(),
                version: version.clone(),
            },
            Self::SemverCompatible { key, version } => PredicateNodeWire::SemverCompatible {
                key: key.clone(),
                version: version.clone(),
            },
            Self::StringPrefix { key, prefix } => PredicateNodeWire::StringPrefix {
                key: key.clone(),
                prefix: prefix.clone(),
            },
            Self::StringMatches { key, pattern } => PredicateNodeWire::StringMatches {
                key: key.clone(),
                pattern: pattern.clone(),
            },
            Self::MetadataExists { key } => PredicateNodeWire::MetadataExists { key: key.clone() },
            Self::MetadataEquals { key, value } => PredicateNodeWire::MetadataEquals {
                key: key.clone(),
                value: value.clone(),
            },
            Self::MetadataMatches { key, pattern } => PredicateNodeWire::MetadataMatches {
                key: key.clone(),
                pattern: pattern.clone(),
            },
            Self::MetadataNumericAtLeast { key, threshold } => {
                PredicateNodeWire::MetadataNumericAtLeast {
                    key: key.clone(),
                    threshold: *threshold,
                }
            }
            Self::And(children) => {
                let child_idxs: Vec<u32> =
                    children.iter().map(|c| c.append_to_wire(nodes)).collect();
                PredicateNodeWire::And {
                    children: child_idxs,
                }
            }
            Self::Or(children) => {
                let child_idxs: Vec<u32> =
                    children.iter().map(|c| c.append_to_wire(nodes)).collect();
                PredicateNodeWire::Or {
                    children: child_idxs,
                }
            }
            Self::Not(inner) => {
                let child_idx = inner.append_to_wire(nodes);
                PredicateNodeWire::Not { child: child_idx }
            }
        };
        let idx = nodes.len() as u32;
        nodes.push(node);
        idx
    }
}

impl PredicateWire {
    /// Rebuild a [`Predicate`] AST from the flat wire format.
    ///
    /// Validates structural integrity: empty tables, out-of-bounds
    /// indices, and child-index cycles are surfaced as typed
    /// [`PredicateWireError`] rather than panicking. A successful
    /// rebuild is byte-equal to the input of the matching
    /// [`Predicate::to_wire`] call.
    pub fn into_predicate(self) -> Result<Predicate, PredicateWireError> {
        if self.nodes.is_empty() {
            return Err(PredicateWireError::Empty);
        }
        let len = self.nodes.len();
        if (self.root_idx as usize) >= len {
            return Err(PredicateWireError::RootOutOfBounds {
                root_idx: self.root_idx,
                len,
            });
        }
        rebuild_predicate(&self.nodes, self.root_idx)
    }
}

/// Recursive rebuild helper. Walks the flat node table from `idx`,
/// validating child indices and cycles as it goes.
fn rebuild_predicate(
    nodes: &[PredicateNodeWire],
    idx: u32,
) -> Result<Predicate, PredicateWireError> {
    let len = nodes.len();
    let node = nodes
        .get(idx as usize)
        .ok_or(PredicateWireError::ChildOutOfBounds { child: idx, len })?;
    let result = match node {
        PredicateNodeWire::Exists { key } => Predicate::Exists { key: key.clone() },
        PredicateNodeWire::Equals { key, value } => Predicate::Equals {
            key: key.clone(),
            value: value.clone(),
        },
        PredicateNodeWire::NumericAtLeast { key, threshold } => Predicate::NumericAtLeast {
            key: key.clone(),
            threshold: *threshold,
        },
        PredicateNodeWire::NumericAtMost { key, threshold } => Predicate::NumericAtMost {
            key: key.clone(),
            threshold: *threshold,
        },
        PredicateNodeWire::NumericInRange { key, min, max } => Predicate::NumericInRange {
            key: key.clone(),
            min: *min,
            max: *max,
        },
        PredicateNodeWire::SemverAtLeast { key, version } => Predicate::SemverAtLeast {
            key: key.clone(),
            version: version.clone(),
        },
        PredicateNodeWire::SemverAtMost { key, version } => Predicate::SemverAtMost {
            key: key.clone(),
            version: version.clone(),
        },
        PredicateNodeWire::SemverCompatible { key, version } => Predicate::SemverCompatible {
            key: key.clone(),
            version: version.clone(),
        },
        PredicateNodeWire::StringPrefix { key, prefix } => Predicate::StringPrefix {
            key: key.clone(),
            prefix: prefix.clone(),
        },
        PredicateNodeWire::StringMatches { key, pattern } => Predicate::StringMatches {
            key: key.clone(),
            pattern: pattern.clone(),
        },
        PredicateNodeWire::MetadataExists { key } => Predicate::MetadataExists { key: key.clone() },
        PredicateNodeWire::MetadataEquals { key, value } => Predicate::MetadataEquals {
            key: key.clone(),
            value: value.clone(),
        },
        PredicateNodeWire::MetadataMatches { key, pattern } => Predicate::MetadataMatches {
            key: key.clone(),
            pattern: pattern.clone(),
        },
        PredicateNodeWire::MetadataNumericAtLeast { key, threshold } => {
            Predicate::MetadataNumericAtLeast {
                key: key.clone(),
                threshold: *threshold,
            }
        }
        PredicateNodeWire::And { children } => {
            check_children_below(children, idx)?;
            let kids: Result<Vec<_>, _> = children
                .iter()
                .map(|&c| rebuild_predicate(nodes, c))
                .collect();
            Predicate::And(kids?)
        }
        PredicateNodeWire::Or { children } => {
            check_children_below(children, idx)?;
            let kids: Result<Vec<_>, _> = children
                .iter()
                .map(|&c| rebuild_predicate(nodes, c))
                .collect();
            Predicate::Or(kids?)
        }
        PredicateNodeWire::Not { child } => {
            if *child >= idx {
                return Err(PredicateWireError::CycleDetected {
                    parent: idx,
                    child: *child,
                });
            }
            Predicate::Not(Box::new(rebuild_predicate(nodes, *child)?))
        }
    };
    Ok(result)
}

/// Validate that every child index in `children` is strictly less
/// than `parent`. Catches cycles introduced by malformed wire
/// payloads.
fn check_children_below(children: &[u32], parent: u32) -> Result<(), PredicateWireError> {
    for &child in children {
        if child >= parent {
            return Err(PredicateWireError::CycleDetected { parent, child });
        }
    }
    Ok(())
}

// =============================================================================
// nRPC envelope integration — Phase 5.B of CAPABILITY_ENHANCEMENTS_PLAN.md.
//
// The cleanest place to attach a `where:` filter to an nRPC call
// is the existing request-headers slot. Headers already carry
// out-of-band metadata (trace context, idempotency keys,
// content-type) and are typed as `(String, Vec<u8>)` — binary-safe,
// per-header capped at `MAX_RPC_HEADER_VALUE_LEN` (4 KB), passed
// through opaquely by the substrate.
//
// Predicate-handling code uses two helpers:
//
//   `predicate_to_rpc_header(&pred)` — JSON-encodes a `PredicateWire`
//                                      into the canonical
//                                      `cyberdeck-where` header.
//   `predicate_from_rpc_headers(headers)` — locates the header in
//                                           a request's headers,
//                                           decodes back to
//                                           `Predicate`.
//
// Service handlers that opt in look for the header; services that
// don't ignore it. The substrate (cortex/rpc) itself never
// inspects the header — `eternal-rule §4: no semantic growth at
// the substrate`. Per-binding API exposure lives in the SDK layer
// (Phase 9b of `CAPABILITY_SYSTEM_SDK_PLAN.md`).
//
// JSON wire format (vs. postcard) trades ~2-3× size for human
// readability + diff-able cross-binding fixtures. Predicates that
// fit a typical service filter are ~200-500 bytes JSON, well
// under the header cap.
// =============================================================================

/// Canonical header name for a predicate-pushdown filter on an
/// nRPC request. Lowercase per HTTP-style convention; the substrate
/// `cortex/rpc` codec passes header names through unchanged, but
/// this constant is the one downstream callers must agree on.
pub const RPC_WHERE_HEADER: &str = "cyberdeck-where";

/// Maximum size of the JSON-encoded `PredicateWire` header value.
/// Mirrors `cortex::rpc::MAX_RPC_HEADER_VALUE_LEN`; redeclared here
/// so the predicate helper can reject oversize encodings without
/// pulling in the `cortex` feature gate.
pub const MAX_PREDICATE_RPC_HEADER_VALUE_LEN: usize = 4096;

/// Errors raised by [`predicate_to_rpc_header`].
#[derive(Debug, thiserror::Error)]
pub enum PredicateRpcEncodeError {
    /// `serde_json::to_vec` failed on the wire-form predicate.
    #[error("predicate wire encode failed: {0}")]
    Encode(#[from] serde_json::Error),
    /// The encoded payload exceeds the header-value cap.
    #[error("predicate wire encoding {actual} bytes exceeds header cap {limit}")]
    TooLarge {
        /// Encoded byte length.
        actual: usize,
        /// Maximum permitted (`MAX_PREDICATE_RPC_HEADER_VALUE_LEN`).
        limit: usize,
    },
}

/// Errors raised by [`predicate_from_rpc_headers`].
#[derive(Debug, thiserror::Error)]
pub enum PredicateRpcDecodeError {
    /// JSON parse failed on the header value.
    #[error("predicate wire decode failed: {0}")]
    Json(#[from] serde_json::Error),
    /// Wire payload was structurally malformed (cycle, OOB index,
    /// empty table).
    #[error("predicate wire malformed: {0}")]
    Wire(#[from] PredicateWireError),
}

/// Encode a [`Predicate`] for transport in an nRPC request header.
///
/// Returns the canonical header tuple `(name, json_bytes)`. The
/// service handler reading the request looks up the header by
/// name (`RPC_WHERE_HEADER`) and decodes via
/// [`predicate_from_rpc_headers`].
///
/// Phase 5.B of `CAPABILITY_ENHANCEMENTS_PLAN.md`. Round-trip
/// pinned in `predicate_rpc_header_round_trip_*` tests.
pub fn predicate_to_rpc_header(
    pred: &Predicate,
) -> Result<(String, Vec<u8>), PredicateRpcEncodeError> {
    let wire = pred.to_wire();
    let bytes = serde_json::to_vec(&wire)?;
    if bytes.len() > MAX_PREDICATE_RPC_HEADER_VALUE_LEN {
        return Err(PredicateRpcEncodeError::TooLarge {
            actual: bytes.len(),
            limit: MAX_PREDICATE_RPC_HEADER_VALUE_LEN,
        });
    }
    Ok((RPC_WHERE_HEADER.to_string(), bytes))
}

/// Extract and decode a [`Predicate`] from a request's headers,
/// if a `cyberdeck-where` header is present.
///
/// Returns:
///
/// - `None` — no `cyberdeck-where` header. Service should default
///   to "no filter" (return all rows).
/// - `Some(Ok(pred))` — header present, decoded cleanly. Service
///   filters its result stream against `pred`.
/// - `Some(Err(_))` — header present but malformed JSON or
///   structurally invalid wire payload. Service should reject the
///   request with a typed error rather than silently ignoring;
///   silent skip would let a misencoded filter return more rows
///   than the caller expected, which is a confidentiality concern
///   in some workloads.
///
/// The first matching header wins — duplicate headers under the
/// same name are not coalesced.
///
/// Phase 5.B of `CAPABILITY_ENHANCEMENTS_PLAN.md`.
pub fn predicate_from_rpc_headers<H>(
    headers: &[H],
) -> Option<Result<Predicate, PredicateRpcDecodeError>>
where
    H: AsRpcHeader,
{
    let value = headers
        .iter()
        .find(|h| h.name() == RPC_WHERE_HEADER)?
        .value();
    let result = serde_json::from_slice::<PredicateWire>(value)
        .map_err(PredicateRpcDecodeError::Json)
        .and_then(|wire| wire.into_predicate().map_err(PredicateRpcDecodeError::Wire));
    Some(result)
}

/// Adapter trait letting [`predicate_from_rpc_headers`] consume any
/// shape that exposes a `(name, value)` view. Generic over both
/// `(String, Vec<u8>)` (the substrate's `RpcHeader` alias) and
/// any binding-side wrapper that exposes name + value accessors.
pub trait AsRpcHeader {
    /// Header name (case-sensitive match against `RPC_WHERE_HEADER`).
    fn name(&self) -> &str;
    /// Header value bytes.
    fn value(&self) -> &[u8];
}

impl AsRpcHeader for (String, Vec<u8>) {
    fn name(&self) -> &str {
        &self.0
    }
    fn value(&self) -> &[u8] {
        &self.1
    }
}

impl AsRpcHeader for &(String, Vec<u8>) {
    fn name(&self) -> &str {
        &self.0
    }
    fn value(&self) -> &[u8] {
        &self.1
    }
}

// =============================================================================
// Service-side row filter ergonomics — Phase 5.B follow-on of
// CAPABILITY_ENHANCEMENTS_PLAN.md.
//
// The Phase 5.B helpers (`predicate_to_rpc_header` /
// `predicate_from_rpc_headers`) move predicates across the wire,
// but service handlers still have to manually construct an
// `EvalContext` per row and dispatch through `Predicate::evaluate`.
// These helpers close that gap:
//
//   - `Predicate::matches_capability_set(caps)` — single-row match
//     against a `CapabilitySet`.
//   - `RpcPredicateContext` trait — application rows expose tags +
//     metadata for predicate evaluation.
//   - `filter_by_predicate(rows, pred)` — iterator combinator that
//     skips rows the predicate filters out.
//
// All three handle the `Option<&Predicate>` shape returned by
// `predicate_from_rpc_headers` ergonomically — `None` means "no
// filter, all rows match".
// =============================================================================

impl Predicate {
    /// True if this predicate evaluates to true against the
    /// given [`super::capability::CapabilitySet`]'s tags + metadata.
    ///
    /// Materializes `caps.tags` (a `HashSet<Tag>`) as a `Vec<Tag>`
    /// for the slice-based `EvalContext`. The cost is a single
    /// allocation per call; for hot loops over many capability
    /// sets, callers may prefer to materialize tags once and
    /// invoke [`Self::evaluate`] directly.
    pub fn matches_capability_set(
        &self,
        caps: &crate::adapter::net::behavior::CapabilitySet,
    ) -> bool {
        let tags: Vec<Tag> = caps.tags.iter().cloned().collect();
        let ctx = EvalContext::new(&tags, &caps.metadata);
        self.evaluate(&ctx)
    }
}

/// Application-row adapter for predicate evaluation.
///
/// Service handlers that filter custom row types (training jobs,
/// documents, sensor readings, …) implement this trait on their
/// row to expose tags + metadata to the predicate. The
/// [`filter_by_predicate`] helper then provides a one-line
/// filter pattern over any iterator of `RpcPredicateContext` rows.
///
/// Phase 5.B follow-on of `CAPABILITY_ENHANCEMENTS_PLAN.md`.
///
/// ```ignore
/// struct TrainingJob {
///     tags: Vec<Tag>,
///     metadata: BTreeMap<String, String>,
///     payload: ...,
/// }
///
/// impl RpcPredicateContext for TrainingJob {
///     fn rpc_predicate_tags(&self) -> &[Tag] { &self.tags }
///     fn rpc_predicate_metadata(&self) -> &BTreeMap<String, String> {
///         &self.metadata
///     }
/// }
/// ```
pub trait RpcPredicateContext {
    /// Tags the predicate's axis-keyed clauses match against.
    fn rpc_predicate_tags(&self) -> &[Tag];
    /// Metadata the predicate's metadata-keyed clauses match against.
    fn rpc_predicate_metadata(&self) -> &BTreeMap<String, String>;
}

/// Filter `rows` by an optional predicate.
///
/// `pred = None` returns all rows (the no-filter case — the
/// caller's request didn't include a `cyberdeck-where` header).
/// `pred = Some(p)` returns only rows where `p` evaluates to true
/// against the row's tags + metadata.
///
/// Service handler usage:
///
/// ```ignore
/// let pred_opt = predicate_from_rpc_headers(&request.headers).transpose()?;
/// let matched: Vec<TrainingJob> =
///     filter_by_predicate(jobs, pred_opt.as_ref()).collect();
/// ```
///
/// Phase 5.B follow-on of `CAPABILITY_ENHANCEMENTS_PLAN.md`.
pub fn filter_by_predicate<R, I>(rows: I, pred: Option<&Predicate>) -> impl Iterator<Item = R> + '_
where
    R: RpcPredicateContext,
    I: IntoIterator<Item = R>,
    I::IntoIter: 'static,
{
    rows.into_iter().filter(move |row| match pred {
        None => true,
        Some(p) => {
            let ctx = EvalContext::new(row.rpc_predicate_tags(), row.rpc_predicate_metadata());
            p.evaluate(&ctx)
        }
    })
}

// =============================================================================
// Constructors
// =============================================================================

impl Predicate {
    /// Build [`Predicate::Exists`] from a [`TagKey`].
    pub fn exists(key: TagKey) -> Self {
        Self::Exists { key }
    }

    /// Build [`Predicate::Equals`] from a key + value.
    pub fn equals(key: TagKey, value: impl Into<String>) -> Self {
        Self::Equals {
            key,
            value: value.into(),
        }
    }

    /// Build [`Predicate::NumericAtLeast`].
    pub fn numeric_at_least(key: TagKey, threshold: f64) -> Self {
        Self::NumericAtLeast { key, threshold }
    }

    /// Build [`Predicate::NumericAtMost`].
    pub fn numeric_at_most(key: TagKey, threshold: f64) -> Self {
        Self::NumericAtMost { key, threshold }
    }

    /// Build [`Predicate::NumericInRange`].
    pub fn numeric_in_range(key: TagKey, min: f64, max: f64) -> Self {
        Self::NumericInRange { key, min, max }
    }

    /// Build [`Predicate::SemverAtLeast`].
    pub fn semver_at_least(key: TagKey, version: impl Into<String>) -> Self {
        Self::SemverAtLeast {
            key,
            version: version.into(),
        }
    }

    /// Build [`Predicate::SemverAtMost`].
    pub fn semver_at_most(key: TagKey, version: impl Into<String>) -> Self {
        Self::SemverAtMost {
            key,
            version: version.into(),
        }
    }

    /// Build [`Predicate::SemverCompatible`].
    pub fn semver_compatible(key: TagKey, version: impl Into<String>) -> Self {
        Self::SemverCompatible {
            key,
            version: version.into(),
        }
    }

    /// Build [`Predicate::StringPrefix`].
    pub fn string_prefix(key: TagKey, prefix: impl Into<String>) -> Self {
        Self::StringPrefix {
            key,
            prefix: prefix.into(),
        }
    }

    /// Build [`Predicate::StringMatches`].
    pub fn string_matches(key: TagKey, pattern: impl Into<String>) -> Self {
        Self::StringMatches {
            key,
            pattern: pattern.into(),
        }
    }

    /// Build [`Predicate::MetadataExists`].
    pub fn metadata_exists(key: impl Into<String>) -> Self {
        Self::MetadataExists { key: key.into() }
    }

    /// Build [`Predicate::MetadataEquals`].
    pub fn metadata_equals(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self::MetadataEquals {
            key: key.into(),
            value: value.into(),
        }
    }

    /// Build [`Predicate::MetadataMatches`].
    pub fn metadata_matches(key: impl Into<String>, pattern: impl Into<String>) -> Self {
        Self::MetadataMatches {
            key: key.into(),
            pattern: pattern.into(),
        }
    }

    /// Build [`Predicate::MetadataNumericAtLeast`].
    pub fn metadata_numeric_at_least(key: impl Into<String>, threshold: f64) -> Self {
        Self::MetadataNumericAtLeast {
            key: key.into(),
            threshold,
        }
    }

    /// Build [`Predicate::And`] from a `Vec` of clauses.
    pub fn and(clauses: Vec<Predicate>) -> Self {
        Self::And(clauses)
    }

    /// Build [`Predicate::Or`] from a `Vec` of clauses.
    pub fn or(clauses: Vec<Predicate>) -> Self {
        Self::Or(clauses)
    }

    /// Build [`Predicate::Not`] wrapping a single clause.
    ///
    /// Named `not` to match `and` / `or` as a constructor —
    /// not an `Op<Output = Predicate>` impl. Implementing
    /// `std::ops::Not` would force callers to depend on
    /// `Predicate: Not` for the `!` operator, which requires
    /// `Predicate: Sized + Not<Output = ?>` boilerplate without
    /// any expressivity gain over the explicit constructor.
    #[allow(clippy::should_implement_trait)]
    pub fn not(inner: Predicate) -> Self {
        Self::Not(Box::new(inner))
    }
}

// =============================================================================
// Evaluation
// =============================================================================

impl Predicate {
    /// Evaluate against `(tags, metadata)`. Pure function.
    ///
    /// Phase 4 of `CAPABILITY_ENHANCEMENTS_PLAN.md`: at every
    /// `And` / `Or` node, children are evaluated in cost-ascending
    /// order so cheap+selective clauses short-circuit first. The
    /// reordering is a pure local optimization — semantics are
    /// identical to [`Self::evaluate_unplanned`]. Pinned by the
    /// `planned_evaluate_matches_unplanned_*` property tests.
    ///
    /// Numeric / semver parse failures yield `false` (a malformed
    /// tag value shouldn't fault a federated query).
    pub fn evaluate(&self, ctx: &EvalContext<'_>) -> bool {
        match self {
            Self::And(children) => Self::eval_all_in_cost_order(children, ctx),
            Self::Or(children) => Self::eval_any_in_cost_order(children, ctx),
            Self::Not(inner) => !inner.evaluate(ctx),
            other => other.evaluate_leaf(ctx),
        }
    }

    /// Evaluate without the planner — children of `And` / `Or` run
    /// in declaration order.
    ///
    /// Phase 4 escape hatch for benchmarking and the planner-
    /// equivalence property tests. Production callers should use
    /// [`Self::evaluate`]; this is a diagnostic surface only.
    pub fn evaluate_unplanned(&self, ctx: &EvalContext<'_>) -> bool {
        match self {
            Self::And(children) => children.iter().all(|c| c.evaluate_unplanned(ctx)),
            Self::Or(children) => children.iter().any(|c| c.evaluate_unplanned(ctx)),
            Self::Not(inner) => !inner.evaluate_unplanned(ctx),
            other => other.evaluate_leaf(ctx),
        }
    }

    /// Evaluate a leaf predicate (anything except `And` / `Or` /
    /// `Not`). Shared between [`Self::evaluate`] and
    /// [`Self::evaluate_unplanned`] so the leaf logic lives in one
    /// place and the two entry points only differ in their
    /// composite handling.
    fn evaluate_leaf(&self, ctx: &EvalContext<'_>) -> bool {
        match self {
            // Presence check: matches both `AxisPresent` and
            // `AxisValue` for `key`. Cannot route through
            // `match_axis_tag` because that helper now skips
            // `AxisPresent` (presence-only tags carry no value;
            // value predicates would otherwise see `""`).
            Self::Exists { key } => ctx
                .tags
                .iter()
                .any(|t| t.axis_key().as_ref() == Some(key)),
            Self::Equals { key, value } => match_axis_tag(ctx.tags, key, |v| v == value.as_str()),
            Self::NumericAtLeast { key, threshold } => match_axis_tag(ctx.tags, key, |v| {
                v.parse::<f64>().is_ok_and(|n| n >= *threshold)
            }),
            Self::NumericAtMost { key, threshold } => match_axis_tag(ctx.tags, key, |v| {
                v.parse::<f64>().is_ok_and(|n| n <= *threshold)
            }),
            Self::NumericInRange { key, min, max } => match_axis_tag(ctx.tags, key, |v| {
                v.parse::<f64>().is_ok_and(|n| n >= *min && n <= *max)
            }),
            Self::SemverAtLeast { key, version } => {
                let Some(rhs) = parse_semver(version) else {
                    return false;
                };
                match_axis_tag(ctx.tags, key, |v| {
                    parse_semver(v).is_some_and(|lhs| lhs >= rhs)
                })
            }
            Self::SemverAtMost { key, version } => {
                let Some(rhs) = parse_semver(version) else {
                    return false;
                };
                match_axis_tag(ctx.tags, key, |v| {
                    parse_semver(v).is_some_and(|lhs| lhs <= rhs)
                })
            }
            Self::SemverCompatible { key, version } => {
                let Some(rhs) = parse_semver(version) else {
                    return false;
                };
                match_axis_tag(ctx.tags, key, |v| {
                    parse_semver(v).is_some_and(|lhs| semver_compatible(lhs, rhs))
                })
            }
            Self::StringPrefix { key, prefix } => {
                match_axis_tag(ctx.tags, key, |v| v.starts_with(prefix.as_str()))
            }
            Self::StringMatches { key, pattern } => {
                match_axis_tag(ctx.tags, key, |v| v.contains(pattern.as_str()))
            }
            Self::MetadataExists { key } => ctx.metadata.contains_key(key),
            Self::MetadataEquals { key, value } => {
                ctx.metadata.get(key).is_some_and(|v| v == value)
            }
            Self::MetadataMatches { key, pattern } => ctx
                .metadata
                .get(key)
                .is_some_and(|v| v.contains(pattern.as_str())),
            Self::MetadataNumericAtLeast { key, threshold } => ctx
                .metadata
                .get(key)
                .and_then(|v| v.parse::<f64>().ok())
                .is_some_and(|n| n >= *threshold),
            // Composite variants are routed through `evaluate` /
            // `evaluate_unplanned`, never reach this leaf-only path.
            Self::And(_) | Self::Or(_) | Self::Not(_) => unreachable!(
                "evaluate_leaf called with a composite Predicate; \
                 routing bug in evaluate / evaluate_unplanned"
            ),
        }
    }

    /// `And` short-circuit evaluation in cost-ascending child order.
    fn eval_all_in_cost_order(children: &[Predicate], ctx: &EvalContext<'_>) -> bool {
        let mut order: Vec<usize> = (0..children.len()).collect();
        order.sort_by_key(|&i| children[i].static_cost());
        order.into_iter().all(|i| children[i].evaluate(ctx))
    }

    /// `Or` short-circuit evaluation in cost-ascending child order.
    fn eval_any_in_cost_order(children: &[Predicate], ctx: &EvalContext<'_>) -> bool {
        let mut order: Vec<usize> = (0..children.len()).collect();
        order.sort_by_key(|&i| children[i].static_cost());
        order.into_iter().any(|i| children[i].evaluate(ctx))
    }

    /// Static cost estimate for the planner. Lower = cheaper to
    /// evaluate; planner sorts children ascending.
    ///
    /// Phase 4 first cut uses fixed-per-variant costs (no index
    /// integration). The ordering reflects empirical evaluation
    /// cost: hashmap lookups < tag-set scans with simple parses
    /// < substring scans < semver parses.
    ///
    /// Composite costs sum the children's costs, so a deeply
    /// nested branch is heavier than a shallow one with the same
    /// leaf shape.
    fn static_cost(&self) -> u32 {
        match self {
            // Tier 1: O(1) hashmap lookup.
            Self::MetadataExists { .. } => 10,
            Self::MetadataEquals { .. } => 11,
            // Tier 2: O(N) tag-set scan with cheap value handling.
            Self::Exists { .. } => 20,
            Self::Equals { .. } => 21,
            Self::MetadataNumericAtLeast { .. } => 25,
            // Tier 3: O(N) tag-set scan + numeric parse per match.
            Self::NumericAtLeast { .. }
            | Self::NumericAtMost { .. }
            | Self::NumericInRange { .. } => 30,
            // Tier 4: O(N) scan + substring / prefix scan.
            Self::StringPrefix { .. } => 40,
            Self::MetadataMatches { .. } => 45,
            Self::StringMatches { .. } => 50,
            // Tier 5: semver triple parse (heaviest leaf).
            Self::SemverAtLeast { .. }
            | Self::SemverAtMost { .. }
            | Self::SemverCompatible { .. } => 60,
            // Composites: sum of children. Caps avoid u32 overflow
            // by saturating at u32::MAX (a predicate this big
            // would have a different problem already).
            Self::And(c) | Self::Or(c) => c
                .iter()
                .map(|c| c.static_cost())
                .fold(0u32, |a, b| a.saturating_add(b)),
            Self::Not(inner) => inner.static_cost(),
        }
    }

    /// Cardinality-aware cost estimate. Refines [`Self::static_cost`]
    /// with per-key distinct-value counts from a `CapabilityIndex`.
    ///
    /// Phase 4 follow-on of `CAPABILITY_ENHANCEMENTS_PLAN.md`. A
    /// leaf clause keyed on a high-cardinality tag (many distinct
    /// values across nodes) is more selective than one keyed on
    /// a low-cardinality tag — running it first short-circuits
    /// faster on the common-mismatch case.
    ///
    /// The intuition: an `Equals(key, v)` clause has roughly
    /// `1 / cardinality` chance of matching a uniformly-random
    /// node, so expected work is `static_cost / cardinality`.
    ///
    /// Behavior:
    ///
    /// - Tag-keyed leaves (Exists / Equals / Numeric* / Semver* /
    ///   String*): `static_cost / max(1, cardinality)`. A
    ///   cardinality of zero (key not yet indexed) falls back to
    ///   raw `static_cost` — conservative.
    /// - Metadata leaves: `static_cost` unchanged. The
    ///   `CapabilityIndex` doesn't track metadata cardinality
    ///   (Phase D / E may add a metadata index; lands then).
    /// - Composites: sum of child dynamic costs (saturating).
    /// - `Not`: passes through inner cost.
    fn dynamic_cost<P: crate::adapter::net::behavior::CardinalityProvider>(
        &self,
        index: &P,
    ) -> u32 {
        match self {
            // Tag-keyed leaves: static_cost / cardinality.
            Self::Exists { key }
            | Self::Equals { key, .. }
            | Self::NumericAtLeast { key, .. }
            | Self::NumericAtMost { key, .. }
            | Self::NumericInRange { key, .. }
            | Self::SemverAtLeast { key, .. }
            | Self::SemverAtMost { key, .. }
            | Self::SemverCompatible { key, .. }
            | Self::StringPrefix { key, .. }
            | Self::StringMatches { key, .. } => {
                let static_c = self.static_cost();
                let cardinality = index.axis_cardinality(key);
                if cardinality > 0 {
                    static_c.saturating_div((cardinality as u32).max(1))
                } else {
                    // Key absent from the index — could be a brand-new
                    // tag the substrate hasn't observed yet. Conservatively
                    // keep static_cost so we don't underestimate work.
                    static_c
                }
            }
            // Metadata leaves: refine via the index's metadata
            // cardinality tracking (mirrors the axis-tag side).
            Self::MetadataExists { key }
            | Self::MetadataEquals { key, .. }
            | Self::MetadataMatches { key, .. }
            | Self::MetadataNumericAtLeast { key, .. } => {
                let static_c = self.static_cost();
                let cardinality = index.metadata_value_cardinality(key);
                if cardinality > 0 {
                    static_c.saturating_div((cardinality as u32).max(1))
                } else {
                    // Key absent from index → fall back to static cost.
                    static_c
                }
            }
            // Composites: sum of children's dynamic costs.
            Self::And(c) | Self::Or(c) => c
                .iter()
                .map(|c| c.dynamic_cost(index))
                .fold(0u32, |a, b| a.saturating_add(b)),
            Self::Not(inner) => inner.dynamic_cost(index),
        }
    }

    /// Evaluate against `ctx`, using `index`'s per-key cardinality
    /// data to refine the planner's clause ordering at every
    /// `And` / `Or` node.
    ///
    /// Phase 4 follow-on of `CAPABILITY_ENHANCEMENTS_PLAN.md`.
    /// Produces the same boolean result as
    /// [`Self::evaluate_unplanned`] for any `(ast, ctx)`; the index
    /// only changes execution order, not semantics. Pinned in the
    /// `index_planner_evaluate_matches_unplanned_*` property tests.
    ///
    /// When the index is available, prefer this entry point over
    /// [`Self::evaluate`] (static-cost planner) — cardinality data
    /// catches selective clauses the static planner misses (e.g.,
    /// a `MetadataEquals` happens to be the cheapest leaf
    /// statically, but a high-cardinality `Equals` on an axis tag
    /// is even more selective in this index's data).
    ///
    /// When the index is unavailable or unhelpful (zero-cardinality
    /// for every key — empty index), this falls back to behavior
    /// equivalent to [`Self::evaluate`].
    pub fn evaluate_with_index<P: crate::adapter::net::behavior::CardinalityProvider>(
        &self,
        ctx: &EvalContext<'_>,
        index: &P,
    ) -> bool {
        match self {
            Self::And(children) => Self::eval_all_with_index(children, ctx, index),
            Self::Or(children) => Self::eval_any_with_index(children, ctx, index),
            Self::Not(inner) => !inner.evaluate_with_index(ctx, index),
            other => other.evaluate_leaf(ctx),
        }
    }

    /// `And` short-circuit evaluation in dynamic-cost-ascending
    /// child order.
    fn eval_all_with_index<P: crate::adapter::net::behavior::CardinalityProvider>(
        children: &[Predicate],
        ctx: &EvalContext<'_>,
        index: &P,
    ) -> bool {
        let mut order: Vec<usize> = (0..children.len()).collect();
        order.sort_by_key(|&i| children[i].dynamic_cost(index));
        order
            .into_iter()
            .all(|i| children[i].evaluate_with_index(ctx, index))
    }

    /// `Or` short-circuit evaluation in Or-mode-cost-ascending
    /// child order.
    ///
    /// Phase 4 final close of `CAPABILITY_ENHANCEMENTS_PLAN.md`.
    /// Uses [`Self::dynamic_cost_or`] (the inverted formula
    /// favoring low-cardinality "often-true" clauses) instead of
    /// the And-mode [`Self::dynamic_cost`]. The asymmetry matches
    /// short-circuit semantics: And short-circuits on first false
    /// (run rare-true clauses first), Or short-circuits on first
    /// true (run often-true clauses first).
    fn eval_any_with_index<P: crate::adapter::net::behavior::CardinalityProvider>(
        children: &[Predicate],
        ctx: &EvalContext<'_>,
        index: &P,
    ) -> bool {
        let mut order: Vec<usize> = (0..children.len()).collect();
        order.sort_by_key(|&i| children[i].dynamic_cost_or(index));
        order
            .into_iter()
            .any(|i| children[i].evaluate_with_index(ctx, index))
    }

    /// Or-mode dynamic cost. Inverts the cardinality direction
    /// from [`Self::dynamic_cost`] so low-cardinality clauses
    /// (likely to match many candidates → often-true) sort first.
    ///
    /// Phase 4 final close of `CAPABILITY_ENHANCEMENTS_PLAN.md`.
    ///
    /// Behavior at leaves:
    ///
    /// - Tag-keyed leaves: `static_cost × max(1, cardinality)`.
    ///   High cardinality → many distinct values → each rare → high
    ///   Or-cost (run later). Low cardinality → matches concentrated
    ///   on few values → each common → low Or-cost (run first).
    /// - Metadata leaves: same shape against
    ///   `metadata_value_cardinality`.
    /// - Cardinality-0 (key absent from index) → fall back to
    ///   `static_cost`, conservative.
    ///
    /// Behavior at composites:
    ///
    /// - `And(children)` recurses with And-mode `dynamic_cost`
    ///   (the And's own internal ordering).
    /// - `Or(children)` recurses with Or-mode `dynamic_cost_or`.
    /// - `Not(inner)` passes through the same Or-mode recursion.
    ///
    /// Note: this is a leaf-level asymmetry. A rigorous treatment
    /// would also penalize And-as-Or-child with a "rare-true"
    /// score (since an And is true only when all children are
    /// true), but doing that requires modeling per-clause
    /// truthiness probability (a separate piece of work). For
    /// typical predicate shapes (mostly leaf-or-mixed, not
    /// deeply-nested And-of-Or-of-And), the leaf-level
    /// asymmetry catches the load-bearing case.
    fn dynamic_cost_or<P: crate::adapter::net::behavior::CardinalityProvider>(
        &self,
        index: &P,
    ) -> u32 {
        match self {
            Self::Exists { key }
            | Self::Equals { key, .. }
            | Self::NumericAtLeast { key, .. }
            | Self::NumericAtMost { key, .. }
            | Self::NumericInRange { key, .. }
            | Self::SemverAtLeast { key, .. }
            | Self::SemverAtMost { key, .. }
            | Self::SemverCompatible { key, .. }
            | Self::StringPrefix { key, .. }
            | Self::StringMatches { key, .. } => {
                let static_c = self.static_cost();
                let cardinality = index.axis_cardinality(key);
                if cardinality == 0 {
                    return static_c;
                }
                static_c.saturating_mul((cardinality as u32).max(1))
            }
            Self::MetadataExists { key }
            | Self::MetadataEquals { key, .. }
            | Self::MetadataMatches { key, .. }
            | Self::MetadataNumericAtLeast { key, .. } => {
                let static_c = self.static_cost();
                let cardinality = index.metadata_value_cardinality(key);
                if cardinality == 0 {
                    return static_c;
                }
                static_c.saturating_mul((cardinality as u32).max(1))
            }
            // Composites: recurse with mode appropriate to the
            // composite's own type. This is a leaf-level asymmetry —
            // the cost reflects the composite's own internal
            // expected work, not its truthiness probability.
            Self::And(c) => c
                .iter()
                .map(|c| c.dynamic_cost(index))
                .fold(0u32, |a, b| a.saturating_add(b)),
            Self::Or(c) => c
                .iter()
                .map(|c| c.dynamic_cost_or(index))
                .fold(0u32, |a, b| a.saturating_add(b)),
            Self::Not(inner) => inner.dynamic_cost_or(index),
        }
    }
}

// =============================================================================
// Debug session — Phase 6 of CAPABILITY_ENHANCEMENTS_PLAN.md.
//
// `Predicate::evaluate_with_trace` instruments a single evaluation,
// producing a tree of clause traces showing which clauses ran and
// what they returned. `PredicateDebugReport` aggregates traces over
// a candidate corpus into per-clause hit/miss stats plus a printable
// summary.
//
// Opt-in only — production hot paths use `evaluate()`, never this
// path. The instrumentation overhead is dominated by the per-clause
// label allocation (`format!`); production-grade ~5% overhead is
// achievable but the current implementation favors simplicity.
// =============================================================================

/// Tree-shaped trace from one debug evaluation against a single
/// `EvalContext`. Mirrors the AST of the predicate that was
/// evaluated, except `And` / `Or` short-circuits drop unevaluated
/// siblings — the trace only carries clauses that actually ran.
///
/// Phase 6 of `CAPABILITY_ENHANCEMENTS_PLAN.md`. Returned by
/// [`Predicate::evaluate_with_trace`].
#[derive(Debug, Clone, PartialEq)]
pub struct ClauseTrace {
    /// One-line summary of the clause (`"Exists(hardware.gpu)"`,
    /// `"And(3 clauses)"`, `"MetadataEquals(intent=ml-training)"`).
    /// Aggregated stats merge by label, so two structurally-equal
    /// leaf clauses share one entry in the report.
    pub label: String,
    /// Final result of evaluating this clause.
    pub result: bool,
    /// Children traces in evaluation order. For `And` / `Or` this is
    /// the planner-ordered (cost-ascending) sequence of children
    /// that actually ran (short-circuited siblings are absent).
    /// `Not` has exactly one child. Leaves have an empty children
    /// list.
    pub children: Vec<ClauseTrace>,
}

impl Predicate {
    /// Evaluate against `ctx`, also producing a tree of per-clause
    /// traces.
    ///
    /// The result equals `self.evaluate(ctx)`; this entry point adds
    /// the [`ClauseTrace`] tree as a side channel for debug
    /// inspection. Composite clauses retain the planner's
    /// short-circuit behavior — descendants that didn't run aren't
    /// in the trace.
    ///
    /// Phase 6 of `CAPABILITY_ENHANCEMENTS_PLAN.md`. Opt-in only;
    /// production callers use [`Predicate::evaluate`].
    pub fn evaluate_with_trace(&self, ctx: &EvalContext<'_>) -> (bool, ClauseTrace) {
        let label = self.debug_label();
        match self {
            Self::And(children) => {
                let mut order: Vec<usize> = (0..children.len()).collect();
                order.sort_by_key(|&i| children[i].static_cost());
                let mut traces = Vec::with_capacity(order.len());
                let mut result = true;
                for i in order {
                    let (r, t) = children[i].evaluate_with_trace(ctx);
                    traces.push(t);
                    if !r {
                        result = false;
                        break;
                    }
                }
                (
                    result,
                    ClauseTrace {
                        label,
                        result,
                        children: traces,
                    },
                )
            }
            Self::Or(children) => {
                let mut order: Vec<usize> = (0..children.len()).collect();
                order.sort_by_key(|&i| children[i].static_cost());
                let mut traces = Vec::with_capacity(order.len());
                let mut result = false;
                for i in order {
                    let (r, t) = children[i].evaluate_with_trace(ctx);
                    traces.push(t);
                    if r {
                        result = true;
                        break;
                    }
                }
                (
                    result,
                    ClauseTrace {
                        label,
                        result,
                        children: traces,
                    },
                )
            }
            Self::Not(inner) => {
                let (r, t) = inner.evaluate_with_trace(ctx);
                (
                    !r,
                    ClauseTrace {
                        label,
                        result: !r,
                        children: vec![t],
                    },
                )
            }
            leaf => {
                let result = leaf.evaluate_leaf(ctx);
                (
                    result,
                    ClauseTrace {
                        label,
                        result,
                        children: Vec::new(),
                    },
                )
            }
        }
    }

    /// One-line debug label for this clause. Used by
    /// [`ClauseTrace`] and [`PredicateDebugReport`] to identify
    /// clauses in human-readable output.
    fn debug_label(&self) -> String {
        match self {
            Self::Exists { key } => format!("Exists({key})"),
            Self::Equals { key, value } => format!("Equals({key}={value})"),
            Self::NumericAtLeast { key, threshold } => {
                format!("NumericAtLeast({key} >= {threshold})")
            }
            Self::NumericAtMost { key, threshold } => {
                format!("NumericAtMost({key} <= {threshold})")
            }
            Self::NumericInRange { key, min, max } => {
                format!("NumericInRange({key} in [{min}, {max}])")
            }
            Self::SemverAtLeast { key, version } => {
                format!("SemverAtLeast({key} >= {version})")
            }
            Self::SemverAtMost { key, version } => {
                format!("SemverAtMost({key} <= {version})")
            }
            Self::SemverCompatible { key, version } => {
                format!("SemverCompatible({key} ~= {version})")
            }
            Self::StringPrefix { key, prefix } => {
                format!("StringPrefix({key} starts with {prefix:?})")
            }
            Self::StringMatches { key, pattern } => {
                format!("StringMatches({key} contains {pattern:?})")
            }
            Self::MetadataExists { key } => format!("MetadataExists({key})"),
            Self::MetadataEquals { key, value } => {
                format!("MetadataEquals({key}={value})")
            }
            Self::MetadataMatches { key, pattern } => {
                format!("MetadataMatches({key} contains {pattern:?})")
            }
            Self::MetadataNumericAtLeast { key, threshold } => {
                format!("MetadataNumericAtLeast({key} >= {threshold})")
            }
            Self::And(c) => format!("And({} clauses)", c.len()),
            Self::Or(c) => format!("Or({} clauses)", c.len()),
            Self::Not(_) => "Not".to_string(),
        }
    }
}

/// Per-clause aggregated stats across a candidate corpus.
///
/// Merged by `label`: two structurally-equal clauses (same variant,
/// same key, same value) share one [`ClauseStats`] entry. This is
/// generally what an operator wants — "how often does
/// `MetadataEquals(intent=ml-training)` succeed?" doesn't depend on
/// where in the AST that clause sits.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClauseStats {
    /// Clause label (matches the `label` field on [`ClauseTrace`]).
    pub label: String,
    /// Number of candidates whose evaluation reached this clause
    /// (i.e. wasn't short-circuited away by an earlier sibling).
    pub evaluated: usize,
    /// Number of those evaluations that returned `true`.
    pub matched: usize,
}

/// Aggregate report from running a [`Predicate`] across a corpus
/// of candidate evaluation contexts.
///
/// Phase 6 of `CAPABILITY_ENHANCEMENTS_PLAN.md`. Built by
/// [`PredicateDebugReport::from_evaluations`].
///
/// The report answers: "given this predicate and these candidates,
/// how many matched, and how often did each clause filter?". A
/// clause with 1042 evaluations and 12 matches has 1.2% positive
/// selectivity — operators use that to spot mismatches between
/// their mental model of the data and the actual data.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PredicateDebugReport {
    /// Number of candidates the predicate was evaluated against.
    pub total_candidates: usize,
    /// Number of candidates the predicate matched (returned `true`).
    pub matched: usize,
    /// Per-clause aggregated stats, keyed by the clause's debug
    /// label. `BTreeMap` for deterministic iteration order in
    /// printed output.
    pub clause_stats: std::collections::BTreeMap<String, ClauseStats>,
}

impl PredicateDebugReport {
    /// Run `pred` against each context in `contexts`, accumulating
    /// per-clause hit / miss stats.
    ///
    /// Each context contributes one trace; the trace tree is walked
    /// post-order to update the per-label `ClauseStats`. Composite
    /// clauses (And / Or / Not) get their own labels too, so an
    /// operator can see "the And short-circuited 730/1042 times" at
    /// a glance.
    pub fn from_evaluations<'a, I>(pred: &Predicate, contexts: I) -> Self
    where
        I: IntoIterator<Item = EvalContext<'a>>,
    {
        let mut report = Self::default();
        for ctx in contexts {
            report.total_candidates += 1;
            let (matched, trace) = pred.evaluate_with_trace(&ctx);
            if matched {
                report.matched += 1;
            }
            accumulate_trace(&trace, &mut report.clause_stats);
        }
        report
    }

    /// Format a human-readable summary suitable for terminal output.
    ///
    /// Returned as a `String` rather than printed directly so tests
    /// can pin the format and callers can route to their own logger.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let pct = |num: usize, denom: usize| -> f64 {
            if denom == 0 {
                0.0
            } else {
                100.0 * (num as f64) / (denom as f64)
            }
        };
        out.push_str("Predicate evaluation report\n");
        out.push_str("─────────────────────────────────────────\n");
        out.push_str(&format!(
            "Total candidates: {}\nMatched:          {} ({:.1}%)\n\n",
            self.total_candidates,
            self.matched,
            pct(self.matched, self.total_candidates),
        ));
        out.push_str("Per-clause stats (alphabetical):\n");
        for stats in self.clause_stats.values() {
            out.push_str(&format!(
                "  {:<60} evaluated {:>5}, matched {:>5} ({:>5.1}%)\n",
                stats.label,
                stats.evaluated,
                stats.matched,
                pct(stats.matched, stats.evaluated),
            ));
        }
        out
    }
}

/// Walk a [`ClauseTrace`] tree post-order, updating per-label
/// stats in `acc`.
fn accumulate_trace(
    trace: &ClauseTrace,
    acc: &mut std::collections::BTreeMap<String, ClauseStats>,
) {
    let entry = acc
        .entry(trace.label.clone())
        .or_insert_with(|| ClauseStats {
            label: trace.label.clone(),
            evaluated: 0,
            matched: 0,
        });
    entry.evaluated += 1;
    if trace.result {
        entry.matched += 1;
    }
    for child in &trace.children {
        accumulate_trace(child, acc);
    }
}

/// Find any value-bearing tag in `tags` matching `key` and run
/// `value_pred` against its value. [`Tag::AxisPresent`] tags carry
/// no value and are skipped — feeding `""` through `value_pred`
/// would let an empty-string `Equals` / `StringPrefix` /
/// `StringMatches` predicate spuriously match a presence-only tag.
/// Use [`Predicate::Exists`] (which goes through a separate
/// presence-aware path in `evaluate_leaf`) when key-presence
/// without a value is the intended check.
fn match_axis_tag(tags: &[Tag], key: &TagKey, value_pred: impl Fn(&str) -> bool) -> bool {
    tags.iter().any(|t| {
        t.axis_key().as_ref() == Some(key)
            && match t {
                Tag::AxisValue { value, .. } => value_pred(value),
                _ => false,
            }
    })
}

// =============================================================================
// Semver — minimal inline parser
// =============================================================================

/// Semver triple `(major, minor, patch)`. Pre-release / build
/// metadata is stripped at parse time; comparing only the triple is
/// enough for this plan's `NumericAtLeast` / `Compatible` semantics.
type SemverTriple = (u64, u64, u64);

/// Parse a `MAJOR.MINOR.PATCH[-prerelease][+build]` string. Returns
/// `None` on any malformed input. Lenient on missing components: `1`
/// → `(1, 0, 0)`, `1.2` → `(1, 2, 0)` — matches caller expectation
/// when applications emit truncated version strings.
fn parse_semver(s: &str) -> Option<SemverTriple> {
    // Drop pre-release / build suffix.
    let core = s
        .split_once('-')
        .map(|(c, _)| c)
        .unwrap_or(s)
        .split_once('+')
        .map(|(c, _)| c)
        .unwrap_or_else(|| s.split_once('-').map(|(c, _)| c).unwrap_or(s));
    let mut parts = core.split('.').map(str::trim);
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().map(|p| p.parse().ok()).unwrap_or(Some(0))?;
    let patch = parts.next().map(|p| p.parse().ok()).unwrap_or(Some(0))?;
    if parts.next().is_some() {
        return None; // 4+ components is not semver
    }
    Some((major, minor, patch))
}

/// `lhs` is caret-compatible with `rhs` per the standard semver
/// rule: same major (or same minor for `0.x.y`, exact for `0.0.x`),
/// and `lhs >= rhs`. Mirrors cargo's `^` operator semantics.
fn semver_compatible(lhs: SemverTriple, rhs: SemverTriple) -> bool {
    if lhs < rhs {
        return false;
    }
    if rhs.0 == 0 {
        if rhs.1 == 0 {
            // 0.0.x — patch is the compatibility band; anything
            // other than the exact tuple is a breaking change.
            // Combined with the `lhs >= rhs` guard above this
            // collapses to lhs == rhs.
            lhs == rhs
        } else {
            // 0.x.y — minor is the compatibility band.
            rhs.1 == lhs.1
        }
    } else {
        rhs.0 == lhs.0
    }
}

// =============================================================================
// pred! macro
// =============================================================================

/// Lightweight macro sugar over [`Predicate`] constructors. Mirrors
/// the substrate plan's macro-style examples in §6a; lowers to plain
/// constructor calls so the AST stays the single source of truth.
///
/// ## Forms
///
/// ```ignore
/// pred!(exists "hardware.gpu");
/// pred!(equals "software.runtime", "cuda-12.4");
/// pred!(num_at_least "hardware.gpu.vram_gb", 24.0);
/// pred!(num_at_most "hardware.gpu.vram_gb", 80.0);
/// pred!(num_in_range "hardware.cpu_cores", 8.0, 64.0);
/// pred!(semver_at_least "software.runtime", "12.0");
/// pred!(semver_compatible "software.runtime", "12.0");
/// pred!(prefix "software.tool", "ffmpeg");
/// pred!(matches "software.daemon", "postgres");
/// pred!(metadata_exists "intent");
/// pred!(metadata_equals "intent", "ml-training");
/// pred!(and [a, b, c]);
/// pred!(or  [a, b, c]);
/// pred!(not a);
/// ```
///
/// The string forms are `<axis>.<key>` literals; the macro splits
/// them into `(axis, key)` via [`crate::adapter::net::behavior::tag::Tag::parse`]
/// and panics at construction time on invalid axis prefixes —
/// matching the substrate plan's "validates shapes at parse time"
/// contract for the macro.
#[macro_export]
macro_rules! pred {
    (exists $key:literal) => {
        $crate::adapter::net::behavior::predicate::Predicate::exists(
            $crate::adapter::net::behavior::predicate::__tag_key_from_str($key),
        )
    };
    (equals $key:literal, $value:expr) => {
        $crate::adapter::net::behavior::predicate::Predicate::equals(
            $crate::adapter::net::behavior::predicate::__tag_key_from_str($key),
            $value,
        )
    };
    (num_at_least $key:literal, $t:expr) => {
        $crate::adapter::net::behavior::predicate::Predicate::numeric_at_least(
            $crate::adapter::net::behavior::predicate::__tag_key_from_str($key),
            $t,
        )
    };
    (num_at_most $key:literal, $t:expr) => {
        $crate::adapter::net::behavior::predicate::Predicate::numeric_at_most(
            $crate::adapter::net::behavior::predicate::__tag_key_from_str($key),
            $t,
        )
    };
    (num_in_range $key:literal, $min:expr, $max:expr) => {
        $crate::adapter::net::behavior::predicate::Predicate::numeric_in_range(
            $crate::adapter::net::behavior::predicate::__tag_key_from_str($key),
            $min,
            $max,
        )
    };
    (semver_at_least $key:literal, $v:expr) => {
        $crate::adapter::net::behavior::predicate::Predicate::semver_at_least(
            $crate::adapter::net::behavior::predicate::__tag_key_from_str($key),
            $v,
        )
    };
    (semver_at_most $key:literal, $v:expr) => {
        $crate::adapter::net::behavior::predicate::Predicate::semver_at_most(
            $crate::adapter::net::behavior::predicate::__tag_key_from_str($key),
            $v,
        )
    };
    (semver_compatible $key:literal, $v:expr) => {
        $crate::adapter::net::behavior::predicate::Predicate::semver_compatible(
            $crate::adapter::net::behavior::predicate::__tag_key_from_str($key),
            $v,
        )
    };
    (prefix $key:literal, $p:expr) => {
        $crate::adapter::net::behavior::predicate::Predicate::string_prefix(
            $crate::adapter::net::behavior::predicate::__tag_key_from_str($key),
            $p,
        )
    };
    (matches $key:literal, $p:expr) => {
        $crate::adapter::net::behavior::predicate::Predicate::string_matches(
            $crate::adapter::net::behavior::predicate::__tag_key_from_str($key),
            $p,
        )
    };
    (metadata_exists $key:expr) => {
        $crate::adapter::net::behavior::predicate::Predicate::metadata_exists($key)
    };
    (metadata_equals $key:expr, $v:expr) => {
        $crate::adapter::net::behavior::predicate::Predicate::metadata_equals($key, $v)
    };
    (metadata_matches $key:expr, $p:expr) => {
        $crate::adapter::net::behavior::predicate::Predicate::metadata_matches($key, $p)
    };
    (metadata_num_at_least $key:expr, $t:expr) => {
        $crate::adapter::net::behavior::predicate::Predicate::metadata_numeric_at_least(
            $key, $t,
        )
    };
    (and [ $($clause:expr),* $(,)? ]) => {
        $crate::adapter::net::behavior::predicate::Predicate::and(vec![$($clause),*])
    };
    (or [ $($clause:expr),* $(,)? ]) => {
        $crate::adapter::net::behavior::predicate::Predicate::or(vec![$($clause),*])
    };
    (not $clause:expr) => {
        $crate::adapter::net::behavior::predicate::Predicate::not($clause)
    };
}

/// Internal helper used by the [`pred!`] macro to lift an
/// `<axis>.<key>` string literal into a [`TagKey`]. Panics on
/// unknown axis or empty key — the macro contract is "parse-time
/// validation," and violating it at the call site is a programmer
/// error caught at the first run (matches the substrate plan's
/// macro-validation guarantee).
#[doc(hidden)]
pub fn __tag_key_from_str(s: &'static str) -> TagKey {
    let (axis_str, key) = s
        .split_once('.')
        .unwrap_or_else(|| panic!("pred! tag key {s:?} must be `<axis>.<key>`"));
    let axis = crate::adapter::net::behavior::tag::TaxonomyAxis::from_prefix(axis_str)
        .unwrap_or_else(|| {
            panic!(
                "pred! tag key {s:?} has unknown axis prefix {axis_str:?}; \
                 valid axes: hardware, software, devices, dataforts"
            )
        });
    TagKey::new(axis, key.to_string())
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::tag::{Tag, TaxonomyAxis};

    fn ctx<'a>(tags: &'a [Tag], metadata: &'a BTreeMap<String, String>) -> EvalContext<'a> {
        EvalContext::new(tags, metadata)
    }

    fn empty_meta() -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    fn axis_present(axis: TaxonomyAxis, key: &str) -> Tag {
        Tag::AxisPresent {
            axis,
            key: key.into(),
        }
    }

    fn axis_eq(axis: TaxonomyAxis, key: &str, value: &str) -> Tag {
        Tag::AxisValue {
            axis,
            key: key.into(),
            value: value.into(),
            separator: crate::adapter::net::behavior::tag::AxisSeparator::Eq,
        }
    }

    // ---- existence + equality ------------------------------------------

    #[test]
    fn exists_matches_axis_present_tag() {
        let tags = [axis_present(TaxonomyAxis::Hardware, "gpu")];
        let meta = empty_meta();
        let p = pred!(exists "hardware.gpu");
        assert!(p.evaluate(&ctx(&tags, &meta)));
    }

    #[test]
    fn exists_matches_axis_value_tag() {
        let tags = [axis_eq(TaxonomyAxis::Hardware, "gpu.vram_gb", "80")];
        let meta = empty_meta();
        let p = pred!(exists "hardware.gpu.vram_gb");
        assert!(p.evaluate(&ctx(&tags, &meta)));
    }

    #[test]
    fn exists_misses_when_axis_differs() {
        let tags = [axis_present(TaxonomyAxis::Software, "gpu")];
        let meta = empty_meta();
        let p = pred!(exists "hardware.gpu");
        assert!(!p.evaluate(&ctx(&tags, &meta)));
    }

    #[test]
    fn equals_matches_value_exactly() {
        let tags = [axis_eq(TaxonomyAxis::Software, "runtime", "cuda-12.4")];
        let meta = empty_meta();
        assert!(pred!(equals "software.runtime", "cuda-12.4").evaluate(&ctx(&tags, &meta)));
        assert!(!pred!(equals "software.runtime", "cuda-11").evaluate(&ctx(&tags, &meta)));
    }

    // ---- numeric --------------------------------------------------------

    #[test]
    fn numeric_at_least_compares_value() {
        let tags = [axis_eq(TaxonomyAxis::Hardware, "gpu.vram_gb", "80")];
        let meta = empty_meta();
        assert!(pred!(num_at_least "hardware.gpu.vram_gb", 24.0).evaluate(&ctx(&tags, &meta)));
        assert!(pred!(num_at_least "hardware.gpu.vram_gb", 80.0).evaluate(&ctx(&tags, &meta)));
        assert!(!pred!(num_at_least "hardware.gpu.vram_gb", 96.0).evaluate(&ctx(&tags, &meta)));
    }

    #[test]
    fn numeric_at_most_and_in_range() {
        let tags = [axis_eq(TaxonomyAxis::Hardware, "cpu_cores", "16")];
        let meta = empty_meta();
        assert!(pred!(num_at_most "hardware.cpu_cores", 32.0).evaluate(&ctx(&tags, &meta)));
        assert!(!pred!(num_at_most "hardware.cpu_cores", 8.0).evaluate(&ctx(&tags, &meta)));
        assert!(pred!(num_in_range "hardware.cpu_cores", 8.0, 32.0).evaluate(&ctx(&tags, &meta)));
        assert!(!pred!(num_in_range "hardware.cpu_cores", 32.0, 64.0).evaluate(&ctx(&tags, &meta)));
    }

    #[test]
    fn numeric_unparseable_value_evaluates_to_false() {
        // Pinned: a tag whose value is not numeric must NOT panic
        // and must NOT match a numeric predicate. Federated queries
        // rely on this — a malformed tag from a peer's binding
        // shouldn't fault our query.
        let tags = [axis_eq(TaxonomyAxis::Hardware, "cpu_cores", "many")];
        let meta = empty_meta();
        assert!(!pred!(num_at_least "hardware.cpu_cores", 1.0).evaluate(&ctx(&tags, &meta)));
    }

    // ---- semver ---------------------------------------------------------

    #[test]
    fn semver_at_least_basic() {
        let tags = [axis_eq(TaxonomyAxis::Software, "runtime", "12.4.1")];
        let meta = empty_meta();
        assert!(pred!(semver_at_least "software.runtime", "12.0.0").evaluate(&ctx(&tags, &meta)));
        assert!(pred!(semver_at_least "software.runtime", "12.4.0").evaluate(&ctx(&tags, &meta)));
        assert!(!pred!(semver_at_least "software.runtime", "13.0.0").evaluate(&ctx(&tags, &meta)));
    }

    #[test]
    fn semver_compatible_caret_rule() {
        // 1.x.y compatibility: same major.
        let tags = [axis_eq(TaxonomyAxis::Software, "runtime", "1.5.2")];
        let meta = empty_meta();
        assert!(pred!(semver_compatible "software.runtime", "1.0.0").evaluate(&ctx(&tags, &meta)));
        assert!(pred!(semver_compatible "software.runtime", "1.4.0").evaluate(&ctx(&tags, &meta)));
        assert!(!pred!(semver_compatible "software.runtime", "0.9.0").evaluate(&ctx(&tags, &meta)));
        assert!(!pred!(semver_compatible "software.runtime", "2.0.0").evaluate(&ctx(&tags, &meta)));

        // 0.x.y compatibility: same minor.
        let tags = [axis_eq(TaxonomyAxis::Software, "runtime", "0.5.7")];
        assert!(pred!(semver_compatible "software.runtime", "0.5.0").evaluate(&ctx(&tags, &meta)));
        assert!(!pred!(semver_compatible "software.runtime", "0.4.0").evaluate(&ctx(&tags, &meta)));
    }

    /// Regression: `0.0.x` is exact-only under cargo's caret rule.
    /// The pre-fix `rhs.0 == 0 → rhs.1 == lhs.1` branch ignored the
    /// patch component and admitted any `0.0.y >= 0.0.x` as
    /// compatible — concretely, `^0.0.1` would match a peer running
    /// `0.0.2`, which is a breaking-change boundary.
    #[test]
    fn semver_compatible_zero_zero_patch_is_exact_only() {
        let meta = empty_meta();

        // Exact match passes.
        let tags = [axis_eq(TaxonomyAxis::Software, "runtime", "0.0.3")];
        assert!(
            pred!(semver_compatible "software.runtime", "0.0.3").evaluate(&ctx(&tags, &meta))
        );

        // Higher patch must NOT match (was admitted pre-fix).
        let tags = [axis_eq(TaxonomyAxis::Software, "runtime", "0.0.4")];
        assert!(
            !pred!(semver_compatible "software.runtime", "0.0.3").evaluate(&ctx(&tags, &meta))
        );

        // Lower patch fails (already covered by the lhs >= rhs guard).
        let tags = [axis_eq(TaxonomyAxis::Software, "runtime", "0.0.2")];
        assert!(
            !pred!(semver_compatible "software.runtime", "0.0.3").evaluate(&ctx(&tags, &meta))
        );

        // Cross-band (different minor) still fails.
        let tags = [axis_eq(TaxonomyAxis::Software, "runtime", "0.1.0")];
        assert!(
            !pred!(semver_compatible "software.runtime", "0.0.3").evaluate(&ctx(&tags, &meta))
        );
    }

    /// Regression: presence-only tags (`Tag::AxisPresent`) must not
    /// match value-bearing predicates. Pre-fix, `match_axis_tag` fed
    /// `""` through `value_pred`, which let `Equals(_, "")` /
    /// `StringPrefix(_, "")` / `StringMatches(_, "")` accept any
    /// presence tag. Use `Exists` for key-presence checks.
    #[test]
    fn axis_present_does_not_satisfy_value_predicates() {
        let tags = [axis_present(TaxonomyAxis::Hardware, "gpu")];
        let meta = empty_meta();

        // Equality with empty string was the worst offender — every
        // presence tag matched it pre-fix.
        assert!(!pred!(equals "hardware.gpu", "").evaluate(&ctx(&tags, &meta)));
        // Equality with any non-empty value also doesn't match a
        // presence tag (no value to compare against).
        assert!(!pred!(equals "hardware.gpu", "nvidia").evaluate(&ctx(&tags, &meta)));
        // String predicates anchored at the empty string used to
        // permissively accept presence tags.
        assert!(!pred!(prefix "hardware.gpu", "").evaluate(&ctx(&tags, &meta)));
        assert!(!pred!(matches "hardware.gpu", "").evaluate(&ctx(&tags, &meta)));

        // `Exists` is the correct check for key presence — it still
        // matches both `AxisPresent` and `AxisValue` shapes.
        assert!(pred!(exists "hardware.gpu").evaluate(&ctx(&tags, &meta)));
        let tags = [axis_eq(TaxonomyAxis::Hardware, "gpu", "nvidia")];
        assert!(pred!(exists "hardware.gpu").evaluate(&ctx(&tags, &meta)));
    }

    #[test]
    fn semver_lenient_parser() {
        // Pinned: the inline parser accepts truncated versions
        // (`1` → `(1, 0, 0)`, `1.2` → `(1, 2, 0)`). Applications in
        // the wild emit these; the parser shouldn't reject them.
        assert_eq!(parse_semver("1"), Some((1, 0, 0)));
        assert_eq!(parse_semver("1.2"), Some((1, 2, 0)));
        assert_eq!(parse_semver("1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_semver("1.2.3-beta"), Some((1, 2, 3)));
        assert_eq!(parse_semver("1.2.3+build.42"), Some((1, 2, 3)));
        // Invalid: 4+ components, non-numeric.
        assert_eq!(parse_semver("1.2.3.4"), None);
        assert_eq!(parse_semver("a.b.c"), None);
        assert_eq!(parse_semver(""), None);
    }

    // ---- string ---------------------------------------------------------

    #[test]
    fn string_prefix_and_matches() {
        let tags = [axis_eq(TaxonomyAxis::Software, "tool", "ffmpeg-7.0")];
        let meta = empty_meta();
        assert!(pred!(prefix "software.tool", "ffmpeg").evaluate(&ctx(&tags, &meta)));
        assert!(!pred!(prefix "software.tool", "imagemagick").evaluate(&ctx(&tags, &meta)));
        assert!(pred!(matches "software.tool", "7.0").evaluate(&ctx(&tags, &meta)));
        assert!(!pred!(matches "software.tool", "8.0").evaluate(&ctx(&tags, &meta)));
    }

    // ---- metadata -------------------------------------------------------

    #[test]
    fn metadata_predicates() {
        let tags: Vec<Tag> = vec![];
        let mut meta = BTreeMap::new();
        meta.insert("intent".into(), "ml-training".into());
        meta.insert("priority".into(), "5".into());

        assert!(pred!(metadata_exists "intent").evaluate(&ctx(&tags, &meta)));
        assert!(!pred!(metadata_exists "missing").evaluate(&ctx(&tags, &meta)));
        assert!(pred!(metadata_equals "intent", "ml-training").evaluate(&ctx(&tags, &meta)));
        assert!(!pred!(metadata_equals "intent", "billing").evaluate(&ctx(&tags, &meta)));
        assert!(pred!(metadata_matches "intent", "training").evaluate(&ctx(&tags, &meta)));
        assert!(pred!(metadata_num_at_least "priority", 3.0).evaluate(&ctx(&tags, &meta)));
        assert!(!pred!(metadata_num_at_least "priority", 10.0).evaluate(&ctx(&tags, &meta)));
    }

    // ---- boolean composition --------------------------------------------

    #[test]
    fn and_or_not_composition() {
        let tags = [
            axis_present(TaxonomyAxis::Hardware, "gpu"),
            axis_eq(TaxonomyAxis::Hardware, "gpu.vram_gb", "80"),
        ];
        let meta = empty_meta();

        // AND: both clauses match.
        let p = pred!(and [
            pred!(exists "hardware.gpu"),
            pred!(num_at_least "hardware.gpu.vram_gb", 24.0),
        ]);
        assert!(p.evaluate(&ctx(&tags, &meta)));

        // AND: one fails.
        let p = pred!(and [
            pred!(exists "hardware.gpu"),
            pred!(num_at_least "hardware.gpu.vram_gb", 96.0),
        ]);
        assert!(!p.evaluate(&ctx(&tags, &meta)));

        // OR: at least one matches.
        let p = pred!(or [
            pred!(exists "hardware.tpu"),
            pred!(exists "hardware.gpu"),
        ]);
        assert!(p.evaluate(&ctx(&tags, &meta)));

        // NOT: inverts.
        let p = pred!(not pred!(exists "hardware.tpu"));
        assert!(p.evaluate(&ctx(&tags, &meta)));
        let p = pred!(not pred!(exists "hardware.gpu"));
        assert!(!p.evaluate(&ctx(&tags, &meta)));
    }

    #[test]
    fn empty_and_is_vacuously_true() {
        // Standard math/logic convention: `forall` over empty set
        // is `true`. Pinned because alternatives surprise readers.
        let tags: Vec<Tag> = vec![];
        let meta = empty_meta();
        assert!(Predicate::and(vec![]).evaluate(&ctx(&tags, &meta)));
    }

    #[test]
    fn empty_or_is_vacuously_false() {
        // Dual convention: `exists` over empty set is `false`.
        let tags: Vec<Tag> = vec![];
        let meta = empty_meta();
        assert!(!Predicate::or(vec![]).evaluate(&ctx(&tags, &meta)));
    }

    // ---- not predicate over unparseable value ---------------------------

    #[test]
    fn not_does_not_flip_unparseable_to_true() {
        // Pinned by the substrate plan's "Predicate::Not(NumericAtLeast)
        // against an unparseable value yields `false`, NOT `true`"
        // contract. The inner numeric predicate fails (returns
        // false); Not(false) = true. But the spec explicitly says
        // "predicate failure is a hard miss, not a logical inversion":
        // the inner check fails to find any matching tag at all, so
        // the inner predicate evaluates to `false`, and `Not(false)`
        // evaluates to `true`. This test pins the documented
        // behavior so a future change is intentional.
        let tags = [axis_eq(TaxonomyAxis::Hardware, "cpu_cores", "many")];
        let meta = empty_meta();
        // Inner: NumericAtLeast against "many" → false (parse fails).
        // Outer: Not(false) → true.
        let p = pred!(not pred!(num_at_least "hardware.cpu_cores", 1.0));
        assert!(p.evaluate(&ctx(&tags, &meta)));
    }

    // ---- structural equality ------------------------------------------
    //
    // Serde wire format is deferred to Phase E (federated query
    // primitives) — see the comment on the `Predicate` declaration.
    // Phase A pins structural-equality round-trip via Clone + PartialEq
    // so a future serde drop-in has a reference behavior to match.

    #[test]
    fn clone_and_eq_preserve_ast() {
        let p = pred!(and [
            pred!(exists "hardware.gpu"),
            pred!(num_at_least "hardware.gpu.vram_gb", 24.0),
            pred!(or [
                pred!(equals "software.runtime", "cuda-12.4"),
                pred!(semver_compatible "software.runtime", "13.0"),
            ]),
            pred!(not pred!(metadata_exists "decommissioning")),
        ]);
        let p2 = p.clone();
        assert_eq!(p, p2);
    }

    // ---- macro ----------------------------------------------------------

    #[test]
    #[should_panic(expected = "unknown axis prefix")]
    fn pred_macro_panics_on_unknown_axis() {
        let _ = pred!(exists "bogus.foo");
    }

    #[test]
    #[should_panic(expected = "must be `<axis>.<key>`")]
    fn pred_macro_panics_on_missing_dot() {
        let _ = pred!(exists "hardware");
    }

    // ========================================================================
    // Query planner — Phase 4 of CAPABILITY_ENHANCEMENTS_PLAN.md.
    // ========================================================================

    fn meta_with(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    /// Worst-case AST: high-selectivity metadata-equals clause buried
    /// LAST among 5 children. Unplanned eval pays for the four
    /// preceding clauses on every false case; planned eval runs the
    /// metadata-equals first and short-circuits.
    fn worst_case_and() -> Predicate {
        Predicate::And(vec![
            Predicate::SemverCompatible {
                key: TagKey::new(TaxonomyAxis::Software, "runtime.python"),
                version: "3.11".into(),
            },
            Predicate::StringMatches {
                key: TagKey::new(TaxonomyAxis::Software, "os"),
                pattern: "linux".into(),
            },
            Predicate::NumericAtLeast {
                key: TagKey::new(TaxonomyAxis::Hardware, "memory_mb"),
                threshold: 65536.0,
            },
            Predicate::Exists {
                key: TagKey::new(TaxonomyAxis::Hardware, "gpu"),
            },
            Predicate::MetadataEquals {
                key: "intent".into(),
                value: "ml-training".into(),
            },
        ])
    }

    #[test]
    fn planner_reorders_and_children_cheap_first() {
        // Pin the planner's ordering on the worst-case AST.
        // The cheapest leaf (`MetadataEquals`, cost=11) must run
        // before the heaviest (`SemverCompatible`, cost=60).
        let ast = worst_case_and();
        if let Predicate::And(children) = &ast {
            // Verify costs as expected from the static_cost table.
            let costs: Vec<u32> = children.iter().map(|c| c.static_cost()).collect();
            assert_eq!(costs, vec![60, 50, 30, 20, 11]);
        } else {
            panic!("worst_case_and produced non-And");
        }
    }

    #[test]
    fn planner_preserves_semantics_on_short_circuit_false() {
        // Pin: planner-vs-unplanned equivalence on a clearly-false
        // input. Both must return false; planner short-circuits
        // earlier but the result is identical.
        let tags: Vec<Tag> = vec![axis_eq(TaxonomyAxis::Hardware, "memory_mb", "32768")];
        let meta = empty_meta();
        let cx = ctx(&tags, &meta);
        let ast = worst_case_and();
        // Memory is 32768 < 65536, so the AND fails. Both paths
        // agree.
        assert!(!ast.evaluate(&cx));
        assert!(!ast.evaluate_unplanned(&cx));
    }

    #[test]
    fn planner_preserves_semantics_on_full_match() {
        let tags: Vec<Tag> = vec![
            axis_eq(TaxonomyAxis::Hardware, "memory_mb", "131072"),
            axis_present(TaxonomyAxis::Hardware, "gpu"),
            axis_eq(TaxonomyAxis::Software, "os", "linux"),
            axis_eq(TaxonomyAxis::Software, "runtime.python", "3.11.5"),
        ];
        let meta = meta_with(&[("intent", "ml-training")]);
        let cx = ctx(&tags, &meta);
        let ast = worst_case_and();
        assert!(ast.evaluate(&cx));
        assert!(ast.evaluate_unplanned(&cx));
    }

    #[test]
    fn planner_preserves_or_short_circuit_semantics() {
        // Or with mixed costs: cheap clause that's true should win
        // either way (planner runs it first; unplanned still finds
        // it eventually).
        let ast = Predicate::Or(vec![
            Predicate::SemverCompatible {
                key: TagKey::new(TaxonomyAxis::Software, "runtime.python"),
                version: "9.9".into(),
            },
            Predicate::MetadataEquals {
                key: "intent".into(),
                value: "ml-training".into(),
            },
        ]);
        let meta = meta_with(&[("intent", "ml-training")]);
        let cx = ctx(&[], &meta);
        assert!(ast.evaluate(&cx));
        assert!(ast.evaluate_unplanned(&cx));
    }

    #[test]
    fn planner_static_cost_compositees_sum_children() {
        // And/Or cost = sum of children. Used to prefer shallow
        // branches over deep ones when ordering nested compositions.
        let cheap = Predicate::MetadataExists { key: "k".into() };
        let expensive = Predicate::SemverCompatible {
            key: TagKey::new(TaxonomyAxis::Software, "x"),
            version: "1.0".into(),
        };
        let nested = Predicate::And(vec![cheap.clone(), expensive.clone()]);
        let leaf_cost = cheap.static_cost() + expensive.static_cost();
        assert_eq!(nested.static_cost(), leaf_cost);

        // Not(inner) keeps inner's cost (no overhead for negation).
        let negated = Predicate::Not(Box::new(expensive.clone()));
        assert_eq!(negated.static_cost(), expensive.static_cost());
    }

    #[test]
    fn planner_handles_empty_and_or_correctly() {
        // Empty And is vacuous true; empty Or is vacuous false.
        // Planner reordering on empty children is a no-op, but
        // pin the contract so a future "ordered eval requires
        // children" assertion doesn't slip in.
        let meta = BTreeMap::new();
        let cx = ctx(&[], &meta);
        assert!(Predicate::And(vec![]).evaluate(&cx));
        assert!(!Predicate::Or(vec![]).evaluate(&cx));
        assert!(Predicate::And(vec![]).evaluate_unplanned(&cx));
        assert!(!Predicate::Or(vec![]).evaluate_unplanned(&cx));
    }

    /// Exhaustive small-input parity: enumerate a handful of small
    /// `(ast, ctx)` combinations and assert planned = unplanned.
    /// Phase 4 doesn't ship full property-based fuzzing
    /// (no proptest dep yet); this hand-rolled equivalence test
    /// covers the load-bearing cases.
    #[test]
    fn planner_evaluate_matches_unplanned_across_canonical_inputs() {
        // Build a corpus of N predicates × M contexts and assert
        // planned == unplanned for every combination.
        let predicates: Vec<Predicate> = vec![
            // Simple leaves
            Predicate::Exists {
                key: TagKey::new(TaxonomyAxis::Hardware, "gpu"),
            },
            Predicate::MetadataEquals {
                key: "intent".into(),
                value: "ml-training".into(),
            },
            Predicate::NumericAtLeast {
                key: TagKey::new(TaxonomyAxis::Hardware, "memory_mb"),
                threshold: 65536.0,
            },
            // Composites
            worst_case_and(),
            Predicate::Or(vec![
                Predicate::Exists {
                    key: TagKey::new(TaxonomyAxis::Hardware, "gpu"),
                },
                Predicate::MetadataEquals {
                    key: "intent".into(),
                    value: "ml-training".into(),
                },
            ]),
            // Nested And-of-Or-of-And
            Predicate::And(vec![
                Predicate::Or(vec![
                    Predicate::Exists {
                        key: TagKey::new(TaxonomyAxis::Hardware, "gpu"),
                    },
                    Predicate::And(vec![
                        Predicate::NumericAtLeast {
                            key: TagKey::new(TaxonomyAxis::Hardware, "memory_mb"),
                            threshold: 65536.0,
                        },
                        Predicate::MetadataExists {
                            key: "intent".into(),
                        },
                    ]),
                ]),
                Predicate::Not(Box::new(Predicate::MetadataEquals {
                    key: "decommissioning".into(),
                    value: "true".into(),
                })),
            ]),
        ];

        let contexts: Vec<(Vec<Tag>, BTreeMap<String, String>)> = vec![
            // Empty
            (vec![], BTreeMap::new()),
            // Hardware match only
            (
                vec![
                    axis_present(TaxonomyAxis::Hardware, "gpu"),
                    axis_eq(TaxonomyAxis::Hardware, "memory_mb", "131072"),
                ],
                BTreeMap::new(),
            ),
            // Metadata match only
            (vec![], meta_with(&[("intent", "ml-training")])),
            // Full match
            (
                vec![
                    axis_present(TaxonomyAxis::Hardware, "gpu"),
                    axis_eq(TaxonomyAxis::Hardware, "memory_mb", "131072"),
                    axis_eq(TaxonomyAxis::Software, "os", "linux"),
                    axis_eq(TaxonomyAxis::Software, "runtime.python", "3.11.5"),
                ],
                meta_with(&[("intent", "ml-training")]),
            ),
            // Full match + decommissioning marker (should fail the
            // last nested predicate's `Not` clause).
            (
                vec![
                    axis_present(TaxonomyAxis::Hardware, "gpu"),
                    axis_eq(TaxonomyAxis::Hardware, "memory_mb", "131072"),
                ],
                meta_with(&[("intent", "ml-training"), ("decommissioning", "true")]),
            ),
        ];

        for (i, ast) in predicates.iter().enumerate() {
            for (j, (tags, meta)) in contexts.iter().enumerate() {
                let cx = ctx(tags, meta);
                let planned = ast.evaluate(&cx);
                let unplanned = ast.evaluate_unplanned(&cx);
                assert_eq!(
                    planned, unplanned,
                    "predicate[{i}] ctx[{j}]: planned={planned} != unplanned={unplanned}"
                );
            }
        }
    }

    // ========================================================================
    // Predicate debug session — Phase 6 of CAPABILITY_ENHANCEMENTS_PLAN.md.
    // ========================================================================

    #[test]
    fn evaluate_with_trace_returns_same_result_as_evaluate() {
        // Pin: the trace-instrumented evaluation produces the
        // same boolean result as `evaluate()`. Trace is a side
        // channel; the predicate semantic is unchanged.
        let ast = worst_case_and();
        let tags: Vec<Tag> = vec![axis_eq(TaxonomyAxis::Hardware, "memory_mb", "32768")];
        let meta = empty_meta();
        let cx = ctx(&tags, &meta);
        let plain_result = ast.evaluate(&cx);
        let (traced_result, _trace) = ast.evaluate_with_trace(&cx);
        assert_eq!(plain_result, traced_result);
    }

    #[test]
    fn evaluate_with_trace_short_circuits_drop_unevaluated_siblings() {
        // Pin: when an `And` short-circuits on a false child, the
        // trace for the And node only carries the children that
        // actually ran. Lets operators see "the metadata clause
        // failed; we never got to the GPU check."
        let ast = Predicate::And(vec![
            // Cheap leaf, false → short-circuit
            Predicate::MetadataEquals {
                key: "intent".into(),
                value: "ml-training".into(),
            },
            // Heavier leaf — should not be evaluated
            Predicate::SemverCompatible {
                key: TagKey::new(TaxonomyAxis::Software, "runtime.python"),
                version: "3.11".into(),
            },
        ]);
        let meta = empty_meta();
        let cx = ctx(&[], &meta); // no metadata → first clause false
        let (result, trace) = ast.evaluate_with_trace(&cx);
        assert!(!result);
        // And's children: only one entry (the metadata clause that
        // returned false and short-circuited the rest).
        assert_eq!(
            trace.children.len(),
            1,
            "And trace should drop unevaluated siblings; got {trace:?}"
        );
        assert!(trace.children[0].label.starts_with("MetadataEquals"));
        assert!(!trace.children[0].result);
    }

    #[test]
    fn evaluate_with_trace_captures_full_evaluation_when_no_short_circuit() {
        // Pin: when no clause short-circuits (all true in an And,
        // all false in an Or), the trace covers every child.
        let ast = Predicate::And(vec![
            Predicate::MetadataExists {
                key: "intent".into(),
            },
            Predicate::Exists {
                key: TagKey::new(TaxonomyAxis::Hardware, "gpu"),
            },
        ]);
        let tags: Vec<Tag> = vec![axis_present(TaxonomyAxis::Hardware, "gpu")];
        let meta = meta_with(&[("intent", "ml-training")]);
        let cx = ctx(&tags, &meta);
        let (result, trace) = ast.evaluate_with_trace(&cx);
        assert!(result);
        assert_eq!(trace.children.len(), 2);
        for child in &trace.children {
            assert!(child.result, "all children must have matched: {child:?}");
        }
    }

    #[test]
    fn evaluate_with_trace_records_not_inversion() {
        // Pin: Not's trace child carries the inner result (pre-
        // negation); the Not node carries the post-negation result.
        let ast = Predicate::Not(Box::new(Predicate::Exists {
            key: TagKey::new(TaxonomyAxis::Hardware, "gpu"),
        }));
        let meta = empty_meta();
        let cx = ctx(&[], &meta); // gpu absent → inner false → Not true
        let (result, trace) = ast.evaluate_with_trace(&cx);
        assert!(result, "Not(absent) should be true");
        assert_eq!(trace.label, "Not");
        assert!(trace.result);
        assert_eq!(trace.children.len(), 1);
        assert!(!trace.children[0].result, "inner Exists should be false");
    }

    #[test]
    fn debug_report_aggregates_match_counts() {
        // 3 candidates, 1 matches.
        let pred = Predicate::Exists {
            key: TagKey::new(TaxonomyAxis::Hardware, "gpu"),
        };
        let no_gpu_tags: Vec<Tag> = vec![];
        let gpu_tags: Vec<Tag> = vec![axis_present(TaxonomyAxis::Hardware, "gpu")];
        let meta = empty_meta();

        let contexts = vec![
            ctx(&no_gpu_tags, &meta),
            ctx(&gpu_tags, &meta),
            ctx(&no_gpu_tags, &meta),
        ];
        let report = PredicateDebugReport::from_evaluations(&pred, contexts);
        assert_eq!(report.total_candidates, 3);
        assert_eq!(report.matched, 1);
        // One leaf clause.
        assert_eq!(report.clause_stats.len(), 1);
        let stats = report.clause_stats.values().next().unwrap();
        assert_eq!(stats.evaluated, 3);
        assert_eq!(stats.matched, 1);
    }

    #[test]
    fn debug_report_separates_per_clause_stats_in_composite() {
        // For an And of two clauses, the report should carry stats
        // for the And node + each leaf. Short-circuited clauses
        // get fewer evaluations.
        let pred = Predicate::And(vec![
            Predicate::MetadataEquals {
                key: "intent".into(),
                value: "ml-training".into(),
            }, // cheap, often false
            Predicate::Exists {
                key: TagKey::new(TaxonomyAxis::Hardware, "gpu"),
            }, // moderate
        ]);

        // 4 candidates: only one has the right intent + GPU.
        let no_meta = empty_meta();
        let intent_meta = meta_with(&[("intent", "ml-training")]);
        let no_gpu: Vec<Tag> = vec![];
        let gpu: Vec<Tag> = vec![axis_present(TaxonomyAxis::Hardware, "gpu")];

        let contexts = vec![
            ctx(&no_gpu, &no_meta),     // both fail; short-circuit on metadata
            ctx(&gpu, &no_meta),        // both fail; short-circuit on metadata
            ctx(&no_gpu, &intent_meta), // metadata true, gpu fail
            ctx(&gpu, &intent_meta),    // both true → match
        ];
        let report = PredicateDebugReport::from_evaluations(&pred, contexts);

        assert_eq!(report.total_candidates, 4);
        assert_eq!(report.matched, 1);

        // 3 entries: And node + MetadataEquals leaf + Exists leaf.
        assert_eq!(report.clause_stats.len(), 3);

        let metadata_stats = report
            .clause_stats
            .values()
            .find(|s| s.label.starts_with("MetadataEquals"))
            .expect("MetadataEquals stats present");
        assert_eq!(
            metadata_stats.evaluated, 4,
            "metadata clause runs every time"
        );
        assert_eq!(metadata_stats.matched, 2, "intent matches in 2 of 4");

        let exists_stats = report
            .clause_stats
            .values()
            .find(|s| s.label.starts_with("Exists"))
            .expect("Exists stats present");
        // Only the 2 candidates with intent_meta got past the
        // short-circuit; gpu check ran twice.
        assert_eq!(
            exists_stats.evaluated, 2,
            "gpu clause only runs after metadata passes"
        );
        assert_eq!(exists_stats.matched, 1);
    }

    #[test]
    fn debug_report_render_includes_summary_and_clauses() {
        let pred = Predicate::Exists {
            key: TagKey::new(TaxonomyAxis::Hardware, "gpu"),
        };
        let report = PredicateDebugReport::from_evaluations(&pred, vec![ctx(&[], &empty_meta())]);
        let rendered = report.render();
        // Pin the load-bearing parts of the format. Operators read
        // the report by these markers; CI fails loudly if they drift.
        assert!(rendered.contains("Predicate evaluation report"));
        assert!(rendered.contains("Total candidates: 1"));
        assert!(rendered.contains("Matched:          0"));
        assert!(rendered.contains("Per-clause stats"));
        assert!(rendered.contains("Exists(hardware.gpu)"));
    }

    #[test]
    fn debug_report_handles_empty_corpus() {
        let pred = Predicate::Exists {
            key: TagKey::new(TaxonomyAxis::Hardware, "gpu"),
        };
        let report = PredicateDebugReport::from_evaluations(&pred, Vec::<EvalContext>::new());
        assert_eq!(report.total_candidates, 0);
        assert_eq!(report.matched, 0);
        assert!(report.clause_stats.is_empty());
        // Render must not panic on empty.
        let rendered = report.render();
        assert!(rendered.contains("Total candidates: 0"));
    }

    // ========================================================================
    // PredicateWire (flat-tree IR) — Phase 5 of CAPABILITY_ENHANCEMENTS_PLAN.md.
    // ========================================================================

    fn sample_complex_predicate() -> Predicate {
        // And-of-Or-of-And + Not — exercises every composite variant
        // and a sampling of leaf variants.
        Predicate::And(vec![
            Predicate::Or(vec![
                Predicate::Exists {
                    key: TagKey::new(TaxonomyAxis::Hardware, "gpu"),
                },
                Predicate::And(vec![
                    Predicate::NumericAtLeast {
                        key: TagKey::new(TaxonomyAxis::Hardware, "memory_mb"),
                        threshold: 65536.0,
                    },
                    Predicate::MetadataExists {
                        key: "intent".into(),
                    },
                ]),
            ]),
            Predicate::Not(Box::new(Predicate::MetadataEquals {
                key: "decommissioning".into(),
                value: "true".into(),
            })),
            Predicate::SemverCompatible {
                key: TagKey::new(TaxonomyAxis::Software, "runtime.python"),
                version: "3.11".into(),
            },
        ])
    }

    #[test]
    fn wire_round_trip_preserves_complex_predicate() {
        // Pin: `Predicate → PredicateWire → Predicate` is identity.
        let original = sample_complex_predicate();
        let wire = original.to_wire();
        let rebuilt = wire.into_predicate().expect("rebuild");
        assert_eq!(original, rebuilt);
    }

    #[test]
    fn wire_round_trip_through_serde_json() {
        // Pin: the wire format serializes through serde_json
        // cleanly (no recursion-limit blowup like raw Predicate).
        let original = sample_complex_predicate();
        let wire = original.to_wire();
        let json = serde_json::to_string(&wire).expect("serialize wire");
        let parsed: PredicateWire = serde_json::from_str(&json).expect("deserialize wire");
        let rebuilt = parsed.into_predicate().expect("rebuild");
        assert_eq!(original, rebuilt);
    }

    #[test]
    fn wire_root_is_at_highest_index_in_post_order_emission() {
        // Pin: `to_wire` emits children before parents, so the
        // root always sits at `nodes.len() - 1` for a freshly-
        // emitted wire payload. The substrate's invariant
        // (children at lower indices) leans on this.
        let pred = sample_complex_predicate();
        let wire = pred.to_wire();
        assert_eq!(wire.root_idx as usize, wire.nodes.len() - 1);
    }

    #[test]
    fn wire_round_trip_byte_stable_across_calls() {
        // Pin: two `to_wire()` calls on equal predicates produce
        // identical wire bytes. Required for cross-binding fixture
        // pinning.
        let pred = sample_complex_predicate();
        let wire_a = pred.to_wire();
        let wire_b = pred.to_wire();
        assert_eq!(wire_a, wire_b);
        let json_a = serde_json::to_string(&wire_a).unwrap();
        let json_b = serde_json::to_string(&wire_b).unwrap();
        assert_eq!(json_a, json_b);
    }

    #[test]
    fn wire_round_trip_preserves_evaluation_semantics() {
        // Pin: a rebuilt predicate produces identical evaluation
        // results to the original on a fixed corpus. The serde
        // round-trip is semantically transparent.
        let original = sample_complex_predicate();
        let wire = original.to_wire();
        let rebuilt = wire.into_predicate().unwrap();

        let no_meta = empty_meta();
        let intent_meta = meta_with(&[("intent", "ml-training")]);
        let decommission_meta =
            meta_with(&[("intent", "ml-training"), ("decommissioning", "true")]);
        let no_gpu: Vec<Tag> = vec![];
        let gpu: Vec<Tag> = vec![axis_present(TaxonomyAxis::Hardware, "gpu")];
        let gpu_with_runtime: Vec<Tag> = vec![
            axis_present(TaxonomyAxis::Hardware, "gpu"),
            axis_eq(TaxonomyAxis::Software, "runtime.python", "3.11.5"),
        ];

        let cases: Vec<(&[Tag], &BTreeMap<String, String>)> = vec![
            (&no_gpu, &no_meta),
            (&gpu, &no_meta),
            (&gpu, &intent_meta),
            (&gpu_with_runtime, &intent_meta),
            (&gpu_with_runtime, &decommission_meta),
        ];

        for (i, (tags, meta)) in cases.iter().enumerate() {
            let cx = ctx(tags, meta);
            assert_eq!(
                original.evaluate(&cx),
                rebuilt.evaluate(&cx),
                "case {i}: original vs rebuilt diverged on evaluation",
            );
        }
    }

    #[test]
    fn wire_from_empty_nodes_table_errors_gracefully() {
        let wire = PredicateWire {
            nodes: Vec::new(),
            root_idx: 0,
        };
        assert_eq!(wire.into_predicate(), Err(PredicateWireError::Empty));
    }

    #[test]
    fn wire_from_out_of_bounds_root_errors_gracefully() {
        let wire = PredicateWire {
            nodes: vec![PredicateNodeWire::Exists {
                key: TagKey::new(TaxonomyAxis::Hardware, "gpu"),
            }],
            root_idx: 5,
        };
        assert_eq!(
            wire.into_predicate(),
            Err(PredicateWireError::RootOutOfBounds {
                root_idx: 5,
                len: 1,
            }),
        );
    }

    #[test]
    fn wire_from_cycle_in_and_children_errors_gracefully() {
        // Malformed: the `And` at index 0 references child index
        // 1, which doesn't exist yet (post-order requires
        // child < parent). Catches index cycles.
        let wire = PredicateWire {
            nodes: vec![PredicateNodeWire::And { children: vec![1] }],
            root_idx: 0,
        };
        let err = wire.into_predicate().unwrap_err();
        assert!(
            matches!(
                err,
                PredicateWireError::CycleDetected {
                    parent: 0,
                    child: 1
                }
            ),
            "expected CycleDetected; got {err:?}",
        );
    }

    #[test]
    fn wire_from_self_referencing_not_errors_gracefully() {
        // `Not` referencing its own index is the simplest cycle.
        let wire = PredicateWire {
            nodes: vec![PredicateNodeWire::Not { child: 0 }],
            root_idx: 0,
        };
        let err = wire.into_predicate().unwrap_err();
        assert!(
            matches!(
                err,
                PredicateWireError::CycleDetected {
                    parent: 0,
                    child: 0
                }
            ),
            "expected CycleDetected; got {err:?}",
        );
    }

    #[test]
    fn wire_simple_leaf_round_trips() {
        // Smallest case: a single leaf predicate. nodes has one
        // entry; root_idx is 0.
        let pred = Predicate::Exists {
            key: TagKey::new(TaxonomyAxis::Hardware, "gpu"),
        };
        let wire = pred.to_wire();
        assert_eq!(wire.nodes.len(), 1);
        assert_eq!(wire.root_idx, 0);
        assert_eq!(wire.into_predicate().unwrap(), pred);
    }

    #[test]
    fn wire_rebuilt_predicate_matches_planner_evaluation() {
        // Pin: planner-aware evaluation continues to work after
        // round-trip. The flat IR doesn't lose the AST shape;
        // `evaluate()` still finds And/Or to reorder.
        let original = sample_complex_predicate();
        let wire = original.to_wire();
        let rebuilt = wire.into_predicate().unwrap();

        let tags: Vec<Tag> = vec![
            axis_present(TaxonomyAxis::Hardware, "gpu"),
            axis_eq(TaxonomyAxis::Software, "runtime.python", "3.11.5"),
        ];
        let meta = meta_with(&[("intent", "ml-training")]);
        let cx = ctx(&tags, &meta);

        // Both planned and unplanned must agree, AND match between
        // original and rebuilt.
        let orig_planned = original.evaluate(&cx);
        let orig_unplanned = original.evaluate_unplanned(&cx);
        let rebuilt_planned = rebuilt.evaluate(&cx);
        let rebuilt_unplanned = rebuilt.evaluate_unplanned(&cx);

        assert_eq!(orig_planned, orig_unplanned);
        assert_eq!(rebuilt_planned, rebuilt_unplanned);
        assert_eq!(orig_planned, rebuilt_planned);
    }

    // ========================================================================
    // nRPC envelope helpers — Phase 5.B of CAPABILITY_ENHANCEMENTS_PLAN.md.
    // ========================================================================

    #[test]
    fn rpc_header_round_trip_preserves_predicate() {
        // Pin the canonical happy path: predicate → header → headers
        // table on the server side → decoded predicate. Service
        // handlers will use exactly this flow.
        let original = sample_complex_predicate();
        let header = predicate_to_rpc_header(&original).expect("encode");
        assert_eq!(header.0, RPC_WHERE_HEADER);

        // Receiver: a Vec<RpcHeader>-shaped surface, with our
        // `where:` header alongside others (trace context, etc.).
        let headers = vec![
            ("trace-id".to_string(), b"abc123".to_vec()),
            header,
            ("idempotency-key".to_string(), b"def456".to_vec()),
        ];
        let decoded = predicate_from_rpc_headers(&headers)
            .expect("header present")
            .expect("decode succeeds");
        assert_eq!(decoded, original);
    }

    #[test]
    fn rpc_header_missing_returns_none() {
        // Service that doesn't see a `cyberdeck-where` header
        // should treat the request as unfiltered. `None` is the
        // signal; service defaults to "match all".
        let headers = vec![
            ("trace-id".to_string(), b"abc123".to_vec()),
            ("idempotency-key".to_string(), b"def456".to_vec()),
        ];
        assert!(predicate_from_rpc_headers(&headers).is_none());
    }

    #[test]
    fn rpc_header_empty_returns_none() {
        let headers: Vec<(String, Vec<u8>)> = Vec::new();
        assert!(predicate_from_rpc_headers(&headers).is_none());
    }

    #[test]
    fn rpc_header_malformed_json_returns_decode_error() {
        // Service receiving a `cyberdeck-where` header with garbage
        // bytes should reject the request, not silently default to
        // unfiltered. Silent fallback would let an attacker / bug
        // return more rows than the caller intended.
        let headers = vec![(RPC_WHERE_HEADER.to_string(), b"not-json".to_vec())];
        let result = predicate_from_rpc_headers(&headers).unwrap();
        assert!(
            matches!(result, Err(PredicateRpcDecodeError::Json(_))),
            "expected JSON decode error; got {result:?}",
        );
    }

    #[test]
    fn rpc_header_cycle_in_payload_returns_decode_error() {
        // Defensive: a wire payload with a child-index cycle
        // (legal JSON but structurally invalid) is rejected.
        let bad_wire = PredicateWire {
            nodes: vec![PredicateNodeWire::Not { child: 0 }],
            root_idx: 0,
        };
        let bad_bytes = serde_json::to_vec(&bad_wire).unwrap();
        let headers = vec![(RPC_WHERE_HEADER.to_string(), bad_bytes)];
        let result = predicate_from_rpc_headers(&headers).unwrap();
        assert!(
            matches!(
                result,
                Err(PredicateRpcDecodeError::Wire(
                    PredicateWireError::CycleDetected { .. }
                ))
            ),
            "expected wire cycle error; got {result:?}",
        );
    }

    #[test]
    fn rpc_header_first_match_wins_on_duplicate_headers() {
        // Per the helper's documented contract: duplicate headers
        // under `cyberdeck-where` are not coalesced; the first
        // match wins. Pin so a future "merge duplicates" change
        // is loud.
        let pred_a = Predicate::Exists {
            key: TagKey::new(TaxonomyAxis::Hardware, "gpu"),
        };
        let pred_b = Predicate::MetadataEquals {
            key: "intent".into(),
            value: "ml-training".into(),
        };
        let header_a = predicate_to_rpc_header(&pred_a).unwrap();
        let header_b = predicate_to_rpc_header(&pred_b).unwrap();
        let headers = vec![header_a, header_b];
        let decoded = predicate_from_rpc_headers(&headers).unwrap().unwrap();
        assert_eq!(decoded, pred_a);
    }

    #[test]
    fn rpc_header_oversize_predicate_rejected_at_encode() {
        // A predicate that would exceed the header-value cap is
        // rejected by `predicate_to_rpc_header` rather than being
        // truncated / silently dropped. Caller decides how to
        // surface this (split the predicate, simplify, or fail).
        // Build a many-clause Or that overflows the 4 KB cap.
        let mut clauses = Vec::new();
        // ~30 chars of metadata key per clause; 200 clauses ≈ 6 KB JSON.
        for i in 0..200 {
            clauses.push(Predicate::MetadataEquals {
                key: format!("very-long-metadata-key-{i:04}"),
                value: format!("very-long-metadata-value-{i:04}"),
            });
        }
        let huge = Predicate::Or(clauses);
        let result = predicate_to_rpc_header(&huge);
        assert!(
            matches!(result, Err(PredicateRpcEncodeError::TooLarge { actual, limit })
                if actual > limit && limit == MAX_PREDICATE_RPC_HEADER_VALUE_LEN),
            "expected TooLarge; got {result:?}",
        );
    }

    #[test]
    fn rpc_header_typical_predicate_fits_well_under_cap() {
        // Sanity bound: a representative predicate (5 leaves +
        // some boolean composition) should encode well under
        // the 4 KB cap. This is the load-bearing case for
        // production use.
        let pred = sample_complex_predicate();
        let header = predicate_to_rpc_header(&pred).expect("encode");
        // Should be well under the cap. Loose upper bound: 1 KB.
        assert!(
            header.1.len() < 1024,
            "encoded predicate is {} bytes, expected < 1024",
            header.1.len(),
        );
    }

    #[test]
    fn rpc_header_can_be_decoded_via_borrow_or_owned_tuple() {
        // Pin: the `AsRpcHeader` trait accepts both `&(String, Vec<u8>)`
        // and `(String, Vec<u8>)` so service handlers can iterate
        // either an owned vec or a borrowed slice.
        let pred = Predicate::Exists {
            key: TagKey::new(TaxonomyAxis::Hardware, "gpu"),
        };
        let header = predicate_to_rpc_header(&pred).unwrap();
        let headers = vec![header];

        // Owned slice.
        let decoded_owned = predicate_from_rpc_headers(&headers).unwrap().unwrap();
        assert_eq!(decoded_owned, pred);

        // Borrow-collected slice.
        let by_ref: Vec<&(String, Vec<u8>)> = headers.iter().collect();
        let decoded_borrow = predicate_from_rpc_headers(&by_ref).unwrap().unwrap();
        assert_eq!(decoded_borrow, pred);
    }

    #[test]
    fn rpc_header_json_format_is_human_readable() {
        // Pin the wire format as JSON (not postcard) so cross-
        // binding fixtures and tcpdump captures are diff-able.
        // Phase 9b of CAPABILITY_SYSTEM_SDK_PLAN.md uses this same
        // shape for the `predicate_nrpc_envelope.json` fixture.
        let pred = Predicate::MetadataEquals {
            key: "intent".into(),
            value: "ml-training".into(),
        };
        let header = predicate_to_rpc_header(&pred).unwrap();
        let json = std::str::from_utf8(&header.1).expect("JSON is UTF-8");
        assert!(
            json.contains("\"kind\":\"metadata_equals\""),
            "unexpected JSON shape: {json}",
        );
        assert!(json.contains("\"key\":\"intent\""), "missing key: {json}");
        assert!(
            json.contains("\"value\":\"ml-training\""),
            "missing value: {json}",
        );
    }

    // ========================================================================
    // Cardinality-aware planner — Phase 4 follow-on of
    // CAPABILITY_ENHANCEMENTS_PLAN.md.
    // ========================================================================

    use crate::adapter::net::behavior::{
        CapabilityAnnouncement, CapabilityIndex, CapabilitySet, GpuInfo, GpuVendor,
        HardwareCapabilities,
    };
    use crate::adapter::net::identity::EntityId;

    fn entity() -> EntityId {
        EntityId::from_bytes([0u8; 32])
    }

    /// Build a CapabilityIndex with `n` distinct memory_mb values.
    /// Used to give `axis_cardinality(hardware.memory_mb)` a known
    /// target value.
    fn index_with_distinct_memory_values(n: u32) -> CapabilityIndex {
        let index = CapabilityIndex::new();
        for i in 0..n {
            let caps = CapabilitySet::new()
                .with_hardware(HardwareCapabilities::new().with_memory(1024 + i));
            let ann = CapabilityAnnouncement::new(i as u64, entity(), 1, caps);
            index.index(ann);
        }
        index
    }

    /// Build a CapabilityIndex with low-cardinality gpu.vendor
    /// (only 2 distinct values across many nodes).
    fn index_with_low_card_gpu_vendor() -> CapabilityIndex {
        let index = CapabilityIndex::new();
        let vendors = [GpuVendor::Nvidia, GpuVendor::Amd];
        for i in 0..20u64 {
            let caps = CapabilitySet::new().with_hardware(
                HardwareCapabilities::new()
                    .with_memory(1024 + i as u32) // unique memory_mb
                    .with_gpu(GpuInfo::new(vendors[i as usize % 2], "x", 1024)),
            );
            let ann = CapabilityAnnouncement::new(i, entity(), 1, caps);
            index.index(ann);
        }
        index
    }

    #[test]
    fn dynamic_cost_lowers_for_high_cardinality_keys() {
        // Pin the planner's intuition: a key with high cardinality
        // (memory_mb across 100 distinct values) gets a much
        // lower dynamic cost than a key with low cardinality
        // (gpu.vendor with 2 values).
        let index = index_with_low_card_gpu_vendor();

        let high_card_clause = Predicate::Equals {
            key: TagKey::new(TaxonomyAxis::Hardware, "memory_mb"),
            value: "1029".into(),
        };
        let low_card_clause = Predicate::Equals {
            key: TagKey::new(TaxonomyAxis::Hardware, "gpu.vendor"),
            value: "nvidia".into(),
        };

        let high_dynamic = high_card_clause.dynamic_cost(&index);
        let low_dynamic = low_card_clause.dynamic_cost(&index);
        // Both have the same static_cost (Equals → tier-2 cost),
        // but high-cardinality divides by 20 while low-cardinality
        // divides by 2 — high-cardinality clause should run first.
        assert!(
            high_dynamic < low_dynamic,
            "expected high-card < low-card; got high={high_dynamic} low={low_dynamic}",
        );
    }

    #[test]
    fn dynamic_cost_falls_back_to_static_for_unknown_keys() {
        let index = index_with_distinct_memory_values(10);
        let unknown_clause = Predicate::Equals {
            key: TagKey::new(TaxonomyAxis::Devices, "qpu"),
            value: "rigetti-aspen".into(),
        };
        // Devices.qpu doesn't exist in the index → cardinality 0
        // → fallback to static_cost. Equals is tier-2 cost = 21.
        assert_eq!(
            unknown_clause.dynamic_cost(&index),
            unknown_clause.static_cost()
        );
    }

    #[test]
    fn dynamic_cost_falls_back_to_static_on_empty_index() {
        let index = CapabilityIndex::new();
        let pred = sample_complex_predicate();
        // Every leaf hits cardinality=0 (empty index), so dynamic
        // cost equals static cost recursively.
        assert_eq!(pred.dynamic_cost(&index), pred.static_cost());
    }

    #[test]
    fn dynamic_cost_for_metadata_leaves_uses_static_cost_when_key_absent() {
        // Metadata key not present in the index → planner falls
        // back to static_cost. This is the path that fires when
        // the index hasn't yet seen any node carrying that
        // metadata key, e.g. on a fresh-spun mesh.
        let index = index_with_distinct_memory_values(100);
        let pred = Predicate::MetadataEquals {
            key: "intent".into(),
            value: "ml-training".into(),
        };
        assert_eq!(pred.dynamic_cost(&index), pred.static_cost());
    }

    /// Build an index with N distinct nodes carrying a metadata
    /// key + 2 distinct metadata values for that key. Used to
    /// pin metadata-cardinality-refined cost ordering.
    fn index_with_metadata_intents() -> crate::adapter::net::behavior::CapabilityIndex {
        let index = crate::adapter::net::behavior::CapabilityIndex::new();
        let intents = ["ml-training", "embedding-cache"];
        for i in 0..20u64 {
            let caps = crate::adapter::net::behavior::CapabilitySet::new()
                .with_metadata("intent", intents[i as usize % 2])
                .with_metadata("owner", format!("alice-{}", i)); // 20 distinct owners
            let ann =
                crate::adapter::net::behavior::CapabilityAnnouncement::new(i, entity(), 1, caps);
            index.index(ann);
        }
        index
    }

    #[test]
    fn dynamic_cost_for_metadata_leaves_lowers_for_high_cardinality() {
        // Index has 20 distinct `owner` values and 2 distinct
        // `intent` values. A `MetadataEquals(owner, ...)` clause
        // is more selective (rare-true) than `MetadataEquals(intent, ...)`,
        // so its dynamic cost should be lower.
        let index = index_with_metadata_intents();
        assert_eq!(
            index.metadata_value_cardinality("intent"),
            2,
            "fixture sanity"
        );
        assert_eq!(
            index.metadata_value_cardinality("owner"),
            20,
            "fixture sanity"
        );

        let intent_clause = Predicate::MetadataEquals {
            key: "intent".into(),
            value: "ml-training".into(),
        };
        let owner_clause = Predicate::MetadataEquals {
            key: "owner".into(),
            value: "alice-5".into(),
        };

        let intent_cost = intent_clause.dynamic_cost(&index);
        let owner_cost = owner_clause.dynamic_cost(&index);
        assert!(
            owner_cost < intent_cost,
            "expected high-card owner < low-card intent; got owner={owner_cost}, intent={intent_cost}",
        );
    }

    #[test]
    fn dynamic_cost_metadata_exists_uses_cardinality_refinement() {
        // `MetadataExists` also benefits from the metadata-cardinality
        // refinement — same key, same cardinality, lower dynamic
        // cost than static_cost when cardinality > 1.
        let index = index_with_metadata_intents();
        let pred = Predicate::MetadataExists {
            key: "owner".into(),
        };
        let dynamic = pred.dynamic_cost(&index);
        let static_c = pred.static_cost();
        assert!(
            dynamic < static_c,
            "expected dynamic < static when cardinality > 1; got dynamic={dynamic}, static={static_c}",
        );
    }

    // ========================================================================
    // Or-vs-And ordering asymmetry — Phase 4 final close of
    // CAPABILITY_ENHANCEMENTS_PLAN.md.
    // ========================================================================

    #[test]
    fn dynamic_cost_or_inverts_cardinality_direction_for_axis_leaves() {
        // Pin: a clause with low cardinality (often-true) gets a
        // LOW Or-cost; high cardinality (rare-true) gets a HIGH
        // Or-cost. Inverse of And-mode `dynamic_cost`.
        let index = index_with_low_card_gpu_vendor();

        // gpu.vendor has 2 distinct values, memory_mb has 20.
        let high_card_clause = Predicate::Equals {
            key: TagKey::new(TaxonomyAxis::Hardware, "memory_mb"),
            value: "1029".into(),
        };
        let low_card_clause = Predicate::Equals {
            key: TagKey::new(TaxonomyAxis::Hardware, "gpu.vendor"),
            value: "nvidia".into(),
        };

        // And-mode (existing): high-card runs first (rare-true).
        assert!(high_card_clause.dynamic_cost(&index) < low_card_clause.dynamic_cost(&index));

        // Or-mode (new): low-card runs first (often-true).
        assert!(
            low_card_clause.dynamic_cost_or(&index) < high_card_clause.dynamic_cost_or(&index),
            "expected low-card < high-card in Or-mode; got low={}, high={}",
            low_card_clause.dynamic_cost_or(&index),
            high_card_clause.dynamic_cost_or(&index),
        );
    }

    #[test]
    fn dynamic_cost_or_inverts_cardinality_direction_for_metadata_leaves() {
        // Same property, metadata side.
        let index = index_with_metadata_intents();

        let intent_clause = Predicate::MetadataEquals {
            key: "intent".into(), // low-card (2 values)
            value: "ml-training".into(),
        };
        let owner_clause = Predicate::MetadataEquals {
            key: "owner".into(), // high-card (20 values)
            value: "alice-5".into(),
        };

        // And-mode: high-card (owner) sorts first.
        assert!(owner_clause.dynamic_cost(&index) < intent_clause.dynamic_cost(&index));

        // Or-mode: low-card (intent) sorts first.
        assert!(
            intent_clause.dynamic_cost_or(&index) < owner_clause.dynamic_cost_or(&index),
            "expected low-card < high-card in Or-mode; got intent={}, owner={}",
            intent_clause.dynamic_cost_or(&index),
            owner_clause.dynamic_cost_or(&index),
        );
    }

    #[test]
    fn dynamic_cost_or_falls_back_to_static_for_unknown_keys() {
        let index = index_with_distinct_memory_values(10);
        let unknown_clause = Predicate::Equals {
            key: TagKey::new(TaxonomyAxis::Devices, "qpu"),
            value: "rigetti-aspen".into(),
        };
        // Devices.qpu cardinality 0 → falls back to static_cost.
        assert_eq!(
            unknown_clause.dynamic_cost_or(&index),
            unknown_clause.static_cost()
        );
    }

    #[test]
    fn dynamic_cost_or_falls_back_to_static_on_empty_index() {
        let index = CapabilityIndex::new();
        let pred = Predicate::Equals {
            key: TagKey::new(TaxonomyAxis::Hardware, "memory_mb"),
            value: "65536".into(),
        };
        assert_eq!(pred.dynamic_cost_or(&index), pred.static_cost());
    }

    #[test]
    fn evaluate_with_index_or_short_circuits_on_often_true_clause_first() {
        // Build a context where one Or-child is true (the
        // low-cardinality intent metadata clause) and the other
        // would also be true on its own. The result is `true`
        // either way. Pin: result is correct regardless of the
        // Or planner's child order.
        let index = index_with_metadata_intents();

        let pred = Predicate::Or(vec![
            // High-cost, high-cardinality clause; would normally
            // sort first under And-mode planner.
            Predicate::Equals {
                key: TagKey::new(TaxonomyAxis::Hardware, "memory_mb"),
                value: "999999".into(), // doesn't match anything
            },
            // Low-cost, low-cardinality clause; should sort first
            // under Or-mode planner.
            Predicate::MetadataEquals {
                key: "intent".into(),
                value: "ml-training".into(),
            },
        ]);

        let tags: Vec<Tag> = vec![]; // no memory_mb=999999
        let meta = meta_with(&[("intent", "ml-training")]);
        let cx = ctx(&tags, &meta);

        // Result should be true (intent matches).
        assert!(pred.evaluate_with_index(&cx, &index));
        // Equivalence vs unplanned holds.
        assert_eq!(
            pred.evaluate_with_index(&cx, &index),
            pred.evaluate_unplanned(&cx),
        );
    }

    #[test]
    fn evaluate_with_index_or_planner_equivalence_on_canonical_inputs() {
        // Pin: Or-mode planner produces the same result as
        // unplanned eval for a corpus of (predicate, context)
        // combinations. The reordering is a pure local
        // optimization.
        let index = index_with_low_card_gpu_vendor();

        let predicates: Vec<Predicate> = vec![
            // Pure Or with mixed cardinalities
            Predicate::Or(vec![
                Predicate::Equals {
                    key: TagKey::new(TaxonomyAxis::Hardware, "gpu.vendor"),
                    value: "nvidia".into(),
                },
                Predicate::Equals {
                    key: TagKey::new(TaxonomyAxis::Hardware, "memory_mb"),
                    value: "1029".into(),
                },
            ]),
            // Or wrapped in And (planner re-enters Or-mode at the
            // inner Or)
            Predicate::And(vec![
                Predicate::Or(vec![
                    Predicate::Exists {
                        key: TagKey::new(TaxonomyAxis::Hardware, "gpu"),
                    },
                    Predicate::MetadataEquals {
                        key: "intent".into(),
                        value: "ml-training".into(),
                    },
                ]),
                Predicate::Not(Box::new(Predicate::MetadataEquals {
                    key: "decommissioning".into(),
                    value: "true".into(),
                })),
            ]),
        ];

        let contexts: Vec<(Vec<Tag>, BTreeMap<String, String>)> = vec![
            (vec![], BTreeMap::new()),
            (
                vec![axis_eq(TaxonomyAxis::Hardware, "gpu.vendor", "nvidia")],
                BTreeMap::new(),
            ),
            (
                vec![axis_present(TaxonomyAxis::Hardware, "gpu")],
                meta_with(&[("intent", "ml-training")]),
            ),
            (
                vec![axis_eq(TaxonomyAxis::Hardware, "memory_mb", "1029")],
                meta_with(&[("decommissioning", "true")]),
            ),
        ];

        for (i, pred) in predicates.iter().enumerate() {
            for (j, (tags, meta)) in contexts.iter().enumerate() {
                let cx = ctx(tags, meta);
                let with_index = pred.evaluate_with_index(&cx, &index);
                let unplanned = pred.evaluate_unplanned(&cx);
                assert_eq!(
                    with_index, unplanned,
                    "pred[{i}] ctx[{j}]: with_index={with_index} != unplanned={unplanned}",
                );
            }
        }
    }

    #[test]
    fn metadata_cardinality_index_tracks_distinct_values() {
        // Direct test of `CapabilityIndex::metadata_value_cardinality`.
        let index = crate::adapter::net::behavior::CapabilityIndex::new();
        // 5 nodes, 3 distinct intent values.
        let intents = [
            "ml-training",
            "ml-training",
            "embedding-cache",
            "ml-training",
            "scratchpad",
        ];
        for (i, intent) in intents.iter().enumerate() {
            let caps = crate::adapter::net::behavior::CapabilitySet::new()
                .with_metadata("intent", *intent);
            let ann = crate::adapter::net::behavior::CapabilityAnnouncement::new(
                i as u64,
                entity(),
                1,
                caps,
            );
            index.index(ann);
        }
        assert_eq!(index.metadata_value_cardinality("intent"), 3);
        // Unknown key → 0.
        assert_eq!(index.metadata_value_cardinality("nonexistent"), 0);
    }

    #[test]
    fn evaluate_with_index_matches_evaluate_unplanned_canonical_inputs() {
        // Pin: cardinality-aware planner produces identical
        // boolean results as unplanned eval. Reordering is a pure
        // local optimization.
        let index = index_with_low_card_gpu_vendor();

        let predicates: Vec<Predicate> = vec![
            // Single leaf
            Predicate::Exists {
                key: TagKey::new(TaxonomyAxis::Hardware, "gpu"),
            },
            // And of axis tag clauses with mixed cardinalities
            Predicate::And(vec![
                Predicate::Equals {
                    key: TagKey::new(TaxonomyAxis::Hardware, "gpu.vendor"),
                    value: "nvidia".into(),
                },
                Predicate::NumericAtLeast {
                    key: TagKey::new(TaxonomyAxis::Hardware, "memory_mb"),
                    threshold: 1024.0,
                },
            ]),
            // Nested with Not
            Predicate::And(vec![
                Predicate::Or(vec![
                    Predicate::Exists {
                        key: TagKey::new(TaxonomyAxis::Hardware, "gpu"),
                    },
                    Predicate::MetadataEquals {
                        key: "intent".into(),
                        value: "ml-training".into(),
                    },
                ]),
                Predicate::Not(Box::new(Predicate::Equals {
                    key: TagKey::new(TaxonomyAxis::Hardware, "gpu.vendor"),
                    value: "intel".into(),
                })),
            ]),
        ];

        let contexts: Vec<(Vec<Tag>, BTreeMap<String, String>)> = vec![
            (vec![], BTreeMap::new()),
            (
                vec![
                    axis_present(TaxonomyAxis::Hardware, "gpu"),
                    axis_eq(TaxonomyAxis::Hardware, "gpu.vendor", "nvidia"),
                    axis_eq(TaxonomyAxis::Hardware, "memory_mb", "65536"),
                ],
                meta_with(&[("intent", "ml-training")]),
            ),
            (
                vec![axis_eq(TaxonomyAxis::Hardware, "gpu.vendor", "amd")],
                BTreeMap::new(),
            ),
        ];

        for (i, pred) in predicates.iter().enumerate() {
            for (j, (tags, meta)) in contexts.iter().enumerate() {
                let cx = ctx(tags, meta);
                let with_index = pred.evaluate_with_index(&cx, &index);
                let unplanned = pred.evaluate_unplanned(&cx);
                assert_eq!(
                    with_index, unplanned,
                    "pred[{i}] ctx[{j}]: with_index={with_index} != unplanned={unplanned}",
                );
            }
        }
    }

    #[test]
    fn evaluate_with_index_empty_index_matches_static_planner() {
        // Empty index → cardinality 0 for every key → fallback to
        // static cost throughout. Result equals plain evaluate().
        let index = CapabilityIndex::new();
        let pred = sample_complex_predicate();

        let tags: Vec<Tag> = vec![
            axis_present(TaxonomyAxis::Hardware, "gpu"),
            axis_eq(TaxonomyAxis::Hardware, "memory_mb", "131072"),
            axis_eq(TaxonomyAxis::Software, "runtime.python", "3.11.5"),
        ];
        let meta = meta_with(&[("intent", "ml-training")]);
        let cx = ctx(&tags, &meta);

        assert_eq!(pred.evaluate_with_index(&cx, &index), pred.evaluate(&cx));
    }

    // ========================================================================
    // Service-side row filter ergonomics — Phase 5.B follow-on of
    // CAPABILITY_ENHANCEMENTS_PLAN.md.
    // ========================================================================

    #[test]
    fn matches_capability_set_evaluates_against_caps_tags_and_metadata() {
        // Pin: `Predicate::matches_capability_set` is a one-line
        // entry point for "does this CapabilitySet match this
        // predicate?". Internally materializes caps.tags as a Vec
        // for the slice-based EvalContext.
        let pred = Predicate::And(vec![
            Predicate::Exists {
                key: TagKey::new(TaxonomyAxis::Hardware, "gpu"),
            },
            Predicate::MetadataEquals {
                key: "intent".into(),
                value: "ml-training".into(),
            },
        ]);

        // Match: caps has both tag and metadata.
        let caps_match = CapabilitySet::new()
            .with_hardware(HardwareCapabilities::new().with_gpu(GpuInfo::new(
                GpuVendor::Nvidia,
                "h100",
                81920,
            )))
            .with_metadata("intent", "ml-training");
        assert!(pred.matches_capability_set(&caps_match));

        // Miss on the metadata side.
        let caps_miss_meta = CapabilitySet::new().with_hardware(
            HardwareCapabilities::new().with_gpu(GpuInfo::new(GpuVendor::Nvidia, "h100", 81920)),
        );
        assert!(!pred.matches_capability_set(&caps_miss_meta));

        // Miss on the tag side.
        let caps_miss_tag = CapabilitySet::new().with_metadata("intent", "ml-training");
        assert!(!pred.matches_capability_set(&caps_miss_tag));

        // Empty caps don't match.
        assert!(!pred.matches_capability_set(&CapabilitySet::default()));
    }

    /// Application row type used to exercise `RpcPredicateContext`
    /// and `filter_by_predicate`. Mirrors what a service
    /// handler's row would look like.
    struct TestJob {
        id: u64,
        tags: Vec<Tag>,
        metadata: BTreeMap<String, String>,
    }

    impl RpcPredicateContext for TestJob {
        fn rpc_predicate_tags(&self) -> &[Tag] {
            &self.tags
        }
        fn rpc_predicate_metadata(&self) -> &BTreeMap<String, String> {
            &self.metadata
        }
    }

    #[test]
    fn filter_by_predicate_returns_all_rows_when_predicate_is_none() {
        // Pin: `pred = None` is the no-filter case (request didn't
        // include `cyberdeck-where`). Every row passes through.
        let jobs = vec![
            TestJob {
                id: 1,
                tags: vec![],
                metadata: BTreeMap::new(),
            },
            TestJob {
                id: 2,
                tags: vec![axis_present(TaxonomyAxis::Hardware, "gpu")],
                metadata: BTreeMap::new(),
            },
        ];
        let filtered: Vec<u64> = filter_by_predicate(jobs, None).map(|j| j.id).collect();
        assert_eq!(filtered, vec![1, 2]);
    }

    #[test]
    fn filter_by_predicate_keeps_only_matching_rows() {
        // Pin: with a predicate set, only rows whose tags +
        // metadata satisfy it survive the filter.
        let pred = Predicate::Exists {
            key: TagKey::new(TaxonomyAxis::Hardware, "gpu"),
        };
        let jobs = vec![
            TestJob {
                id: 1,
                tags: vec![],
                metadata: BTreeMap::new(),
            },
            TestJob {
                id: 2,
                tags: vec![axis_present(TaxonomyAxis::Hardware, "gpu")],
                metadata: BTreeMap::new(),
            },
            TestJob {
                id: 3,
                tags: vec![axis_eq(TaxonomyAxis::Hardware, "gpu.vendor", "nvidia")],
                metadata: BTreeMap::new(),
            },
            TestJob {
                id: 4,
                tags: vec![
                    axis_present(TaxonomyAxis::Hardware, "gpu"),
                    axis_eq(TaxonomyAxis::Hardware, "memory_mb", "65536"),
                ],
                metadata: BTreeMap::new(),
            },
        ];
        let filtered: Vec<u64> = filter_by_predicate(jobs, Some(&pred))
            .map(|j| j.id)
            .collect();
        // Only ids 2 and 4 have the gpu presence tag.
        assert_eq!(filtered, vec![2, 4]);
    }

    #[test]
    fn filter_by_predicate_combined_axis_and_metadata_clauses() {
        // Pin: predicates with both axis-tag AND metadata clauses
        // work end-to-end through the filter helper. Mirrors the
        // canonical "where: gpu AND intent = ml-training" use case.
        let pred = Predicate::And(vec![
            Predicate::Exists {
                key: TagKey::new(TaxonomyAxis::Hardware, "gpu"),
            },
            Predicate::MetadataEquals {
                key: "intent".into(),
                value: "ml-training".into(),
            },
        ]);
        let jobs = vec![
            TestJob {
                id: 1,
                tags: vec![axis_present(TaxonomyAxis::Hardware, "gpu")],
                metadata: meta_with(&[("intent", "embedding-cache")]),
            },
            TestJob {
                id: 2,
                tags: vec![axis_present(TaxonomyAxis::Hardware, "gpu")],
                metadata: meta_with(&[("intent", "ml-training")]),
            },
            TestJob {
                id: 3,
                tags: vec![],
                metadata: meta_with(&[("intent", "ml-training")]),
            },
        ];
        let filtered: Vec<u64> = filter_by_predicate(jobs, Some(&pred))
            .map(|j| j.id)
            .collect();
        // Only id 2 has both gpu AND intent=ml-training.
        assert_eq!(filtered, vec![2]);
    }

    #[test]
    fn filter_by_predicate_empty_input_yields_empty_iterator() {
        let pred = Predicate::Exists {
            key: TagKey::new(TaxonomyAxis::Hardware, "gpu"),
        };
        let jobs: Vec<TestJob> = Vec::new();
        let filtered: Vec<u64> = filter_by_predicate(jobs, Some(&pred))
            .map(|j| j.id)
            .collect();
        assert!(filtered.is_empty());
    }

    #[test]
    fn end_to_end_predicate_pushdown_flow() {
        // Pin the canonical Phase 5.B usage: client builds a
        // predicate, encodes to an RPC header, server decodes and
        // filters its row stream. This is the load-bearing
        // workflow Phase 5.B exists for.

        // Client side: build predicate, encode.
        let pred = Predicate::And(vec![
            Predicate::Exists {
                key: TagKey::new(TaxonomyAxis::Hardware, "gpu"),
            },
            Predicate::NumericAtLeast {
                key: TagKey::new(TaxonomyAxis::Hardware, "memory_mb"),
                threshold: 32768.0,
            },
        ]);
        let encoded = predicate_to_rpc_header(&pred).expect("encode");

        // Server side: receive request with this header alongside
        // standard tracing/idempotency keys. Decode the predicate.
        let request_headers = vec![
            ("trace-id".to_string(), b"abc123".to_vec()),
            encoded,
            ("idempotency-key".to_string(), b"def456".to_vec()),
        ];
        let decoded_pred = predicate_from_rpc_headers(&request_headers)
            .expect("header present")
            .expect("decode");

        // Server side: filter the row stream.
        let jobs = vec![
            TestJob {
                id: 1, // No GPU.
                tags: vec![axis_eq(TaxonomyAxis::Hardware, "memory_mb", "65536")],
                metadata: BTreeMap::new(),
            },
            TestJob {
                id: 2, // GPU + 32 GB → matches.
                tags: vec![
                    axis_present(TaxonomyAxis::Hardware, "gpu"),
                    axis_eq(TaxonomyAxis::Hardware, "memory_mb", "32768"),
                ],
                metadata: BTreeMap::new(),
            },
            TestJob {
                id: 3, // GPU + 16 GB → too little memory.
                tags: vec![
                    axis_present(TaxonomyAxis::Hardware, "gpu"),
                    axis_eq(TaxonomyAxis::Hardware, "memory_mb", "16384"),
                ],
                metadata: BTreeMap::new(),
            },
            TestJob {
                id: 4, // GPU + 65 GB → matches.
                tags: vec![
                    axis_present(TaxonomyAxis::Hardware, "gpu"),
                    axis_eq(TaxonomyAxis::Hardware, "memory_mb", "65536"),
                ],
                metadata: BTreeMap::new(),
            },
        ];
        let matched: Vec<u64> = filter_by_predicate(jobs, Some(&decoded_pred))
            .map(|j| j.id)
            .collect();
        assert_eq!(matched, vec![2, 4]);
    }

    #[test]
    fn evaluate_with_index_handles_deeply_nested_correctly() {
        // 3-level nest with cardinality data. Pin: result matches
        // unplanned eval; the planner doesn't get confused by
        // depth.
        let index = index_with_low_card_gpu_vendor();
        let pred = Predicate::And(vec![
            Predicate::Or(vec![
                Predicate::And(vec![
                    Predicate::Equals {
                        key: TagKey::new(TaxonomyAxis::Hardware, "gpu.vendor"),
                        value: "nvidia".into(),
                    },
                    Predicate::NumericAtLeast {
                        key: TagKey::new(TaxonomyAxis::Hardware, "memory_mb"),
                        threshold: 1024.0,
                    },
                ]),
                Predicate::Not(Box::new(Predicate::Exists {
                    key: TagKey::new(TaxonomyAxis::Hardware, "gpu"),
                })),
            ]),
            Predicate::MetadataExists {
                key: "intent".into(),
            },
        ]);

        let tags: Vec<Tag> = vec![
            axis_eq(TaxonomyAxis::Hardware, "gpu.vendor", "nvidia"),
            axis_eq(TaxonomyAxis::Hardware, "memory_mb", "2048"),
        ];
        let meta = meta_with(&[("intent", "ml-training")]);
        let cx = ctx(&tags, &meta);

        let with_index = pred.evaluate_with_index(&cx, &index);
        let unplanned = pred.evaluate_unplanned(&cx);
        assert_eq!(with_index, unplanned);
    }
}
