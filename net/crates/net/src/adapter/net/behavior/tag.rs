//! Typed-taxonomy capability tags — Phase A foundations of the
//! Capability System Plan.
//!
//! See `docs/plans/CAPABILITY_SYSTEM_PLAN.md` §§1–2 for the design.
//! This module ships the load-bearing primitives the rest of Phase A
//! builds on:
//!
//! - [`TaxonomyAxis`] — the four-axis ontology
//!   (`hardware` / `software` / `devices` / `dataforts`).
//! - [`Tag`] — parsed-tag value covering axis-prefixed shapes,
//!   reserved cross-axis prefixes (`causal:` / `heat:` / `fork-of:` /
//!   `scope:`), and pre-Warriors legacy untyped tags during the
//!   deprecation window.
//! - [`TagKey`] — the (axis, key) half of an axis tag, used by the
//!   forthcoming `Predicate` variants that match on key alone.
//! - [`CapabilityTagError`] — typed parse / construction errors.
//!
//! Encoding (per the substrate plan):
//!
//! ```text
//! <axis>.<key>                  boolean axis tag        e.g. hardware.gpu
//! <axis>.<key>=<value>          keyed axis tag (=)      e.g. hardware.gpu.vram_gb=80
//! <axis>.<key>:<value>          keyed axis tag (:)      e.g. dataforts.has_chain:abc...
//! <reserved-prefix><body>       reserved cross-axis     e.g. causal:abc..., scope:prod
//! <anything else>               legacy untyped (warn)   e.g. myteam-tag
//! ```
//!
//! The parser is permissive — it accepts every shape including
//! reserved-prefix tags, because the wire format must round-trip
//! whatever the substrate itself emits. Reserved-prefix
//! **enforcement** for user code lives at the application-facing
//! builder ([`Tag::parse_user`] vs. [`Tag::parse`]); user code that
//! tries to emit a tag with a reserved prefix gets
//! [`CapabilityTagError::ReservedPrefix`] before the call hits the
//! wire. Internal callers and deserialization use [`Tag::parse`]
//! which accepts everything.

use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;

// =============================================================================
// TaxonomyAxis
// =============================================================================

/// The four axes of the typed capability taxonomy.
///
/// Per `CAPABILITY_SYSTEM_PLAN.md` §1:
///
/// | Axis | Meaning |
/// |---|---|
/// | `hardware`  | What the node *can do* compute-wise. Objective, measurable. |
/// | `software`  | What the node *currently runs*. Configurable. |
/// | `devices`   | Custom semantic role tags. World-facing roles. |
/// | `dataforts` | Storage capacity + hosted causal chains. |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaxonomyAxis {
    /// Compute capabilities of the node — CPU / RAM / GPU / accelerators.
    Hardware,
    /// Software stack — OS / runtimes / loaded models / available tools.
    Software,
    /// World-facing semantic role tags (printer, sensor, actuator).
    Devices,
    /// Storage capacity + hosted causal chains (Rebel Yell axis).
    Dataforts,
}

impl TaxonomyAxis {
    /// Lowercase prefix string used in tag encoding
    /// (`hardware`, `software`, `devices`, `dataforts`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hardware => "hardware",
            Self::Software => "software",
            Self::Devices => "devices",
            Self::Dataforts => "dataforts",
        }
    }

    /// Parse an axis prefix from its canonical string form. Returns
    /// `None` for unknown axes (caller decides whether to treat as
    /// legacy or reject).
    pub fn from_prefix(s: &str) -> Option<Self> {
        match s {
            "hardware" => Some(Self::Hardware),
            "software" => Some(Self::Software),
            "devices" => Some(Self::Devices),
            "dataforts" => Some(Self::Dataforts),
            _ => None,
        }
    }

    /// All four axes in declaration order. Useful for iteration
    /// (e.g. enumerate-and-match against an unknown prefix).
    pub const fn all() -> [Self; 4] {
        [
            Self::Hardware,
            Self::Software,
            Self::Devices,
            Self::Dataforts,
        ]
    }
}

impl fmt::Display for TaxonomyAxis {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// =============================================================================
// Reserved cross-axis prefixes
// =============================================================================

/// Reserved cross-axis tag prefixes per `CAPABILITY_SYSTEM_PLAN.md` §2.
/// These describe the *artifact* (chain, fork lineage, heat) rather
/// than the node, so they don't fit into one of the four taxonomy axes.
///
/// Application code attempting to emit a tag with any of these
/// prefixes via [`Tag::parse_user`] is rejected with
/// [`CapabilityTagError::ReservedPrefix`]. The substrate itself emits
/// these via privileged paths (e.g. `Mesh::announce_chain` for
/// `causal:`, the fork-coordination layer for `fork-of:`, the
/// existing scope helpers for `scope:`).
pub const RESERVED_PREFIXES: &[&str] = &["causal:", "dataforts:", "fork-of:", "heat:", "scope:"];

/// True if `s` starts with a reserved cross-axis prefix.
fn starts_with_reserved_prefix(s: &str) -> Option<&'static str> {
    RESERVED_PREFIXES
        .iter()
        .find(|p| s.starts_with(*p))
        .copied()
}

// =============================================================================
// TagKey
// =============================================================================

/// `(axis, key)` half of an axis-prefixed tag. Used by `Predicate`
/// variants that match on the key without the value (e.g.
/// `Predicate::Exists(TagKey)`).
///
/// Display form is the same as the axis-presence tag:
/// `<axis>.<key>` (e.g. `hardware.gpu` for
/// `TagKey { axis: Hardware, key: "gpu" }`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TagKey {
    /// Taxonomy axis the key belongs to.
    pub axis: TaxonomyAxis,
    /// Key portion after the `<axis>.` prefix (e.g. `gpu.vram_gb`).
    pub key: String,
}

impl TagKey {
    /// Build a `TagKey` from `(axis, key)`. The `key` is stored
    /// verbatim; callers are responsible for choosing a stable
    /// canonical form.
    pub fn new(axis: TaxonomyAxis, key: impl Into<String>) -> Self {
        Self {
            axis,
            key: key.into(),
        }
    }
}

impl fmt::Display for TagKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.axis.as_str(), self.key)
    }
}

// =============================================================================
// Tag
// =============================================================================

/// Parsed capability tag.
///
/// Internal representation; the wire format is the canonical string
/// returned by [`fmt::Display`]. Custom `Serialize` / `Deserialize`
/// impls below render Tag → string and parse string → Tag, so a
/// `HashSet<Tag>` rides over the wire as a JSON string array
/// (`["hardware.gpu", "scope:tenant:foo", ...]`). The internal
/// enum shape is an implementation detail callers don't see.
///
/// Parser is permissive — see module docs for the
/// `parse` (internal) vs. `parse_user` (application) split.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Tag {
    /// Axis-prefixed presence tag with no value.
    /// Wire form: `<axis>.<key>` (e.g. `hardware.gpu`).
    AxisPresent {
        /// Taxonomy axis (`hardware` / `software` / `devices` / `dataforts`).
        axis: TaxonomyAxis,
        /// Key portion after the `<axis>.` prefix (e.g. `gpu`).
        key: String,
    },
    /// Axis-prefixed keyed value. The separator is captured so the
    /// canonical Display round-trips byte-for-byte: some keys use `=`
    /// (e.g. `hardware.gpu.vram_gb=80`); the dataforts subset uses `:`
    /// (e.g. `dataforts.has_chain:<hex>`).
    AxisValue {
        /// Taxonomy axis.
        axis: TaxonomyAxis,
        /// Key portion (everything between `<axis>.` and the separator).
        key: String,
        /// Value portion (everything after the separator).
        value: String,
        /// `=` or `:` — captured so wire form round-trips byte-for-byte.
        separator: AxisSeparator,
    },
    /// Reserved cross-axis prefix (`causal:`, `fork-of:`, `heat:`,
    /// `scope:`). Stored as `(prefix, body)` so the parser can route
    /// per-prefix at probe time without re-scanning.
    Reserved {
        /// One of [`RESERVED_PREFIXES`] (e.g. `causal:`).
        prefix: String,
        /// Tag content after the reserved prefix.
        body: String,
    },
    /// Legacy pre-Warriors untyped tag. Parses with deprecation
    /// warning; one minor version of compatibility per
    /// `CAPABILITY_SYSTEM_PLAN.md` Locked decision 1.
    Legacy(String),
}

/// Separator between an axis-tag's key and value — `=` is the
/// general convention; `:` is the dataforts pre-typed convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AxisSeparator {
    /// `=` separator — `hardware.gpu.vram_gb=80`.
    Eq,
    /// `:` separator — `dataforts.has_chain:<hex>`. Used only by the
    /// dataforts axis at the substrate level; available for any
    /// axis if a pre-typed convention emerges later.
    Colon,
}

impl AxisSeparator {
    /// Single-character form of the separator (`=` or `:`).
    pub const fn as_char(self) -> char {
        match self {
            Self::Eq => '=',
            Self::Colon => ':',
        }
    }
}

impl Tag {
    /// Permissive parser: accepts every wire-form shape including
    /// reserved-prefix tags. Used by deserialization and by substrate
    /// code that has authority to emit reserved-prefix tags. Returns
    /// [`CapabilityTagError::Empty`] only on the empty string;
    /// otherwise produces some valid `Tag` variant (legacy if no
    /// shape recognized).
    pub fn parse(s: &str) -> Result<Self, CapabilityTagError> {
        if s.is_empty() {
            return Err(CapabilityTagError::Empty);
        }

        // 1. Reserved cross-axis prefix?
        if let Some(prefix) = starts_with_reserved_prefix(s) {
            return Ok(Self::Reserved {
                prefix: prefix.to_string(),
                body: s[prefix.len()..].to_string(),
            });
        }

        // 2. Axis-prefixed?
        if let Some((axis_prefix, rest)) = s.split_once('.') {
            if let Some(axis) = TaxonomyAxis::from_prefix(axis_prefix) {
                return Ok(parse_axis_body(axis, rest));
            }
        }

        // 3. Legacy untyped tag (deprecation window).
        Ok(Self::Legacy(s.to_string()))
    }

    /// Application-facing parser: rejects reserved-prefix tags with
    /// [`CapabilityTagError::ReservedPrefix`]. Use this in user-code
    /// builders (e.g. `CapabilitySet::add_tag`) so reserved-prefix
    /// emission is caught at the source rather than corrupting the
    /// substrate's view of who's authoritative for `causal:` etc.
    pub fn parse_user(s: &str) -> Result<Self, CapabilityTagError> {
        if let Some(prefix) = starts_with_reserved_prefix(s) {
            return Err(CapabilityTagError::ReservedPrefix {
                prefix: prefix.to_string(),
                tag: s.to_string(),
            });
        }
        Self::parse(s)
    }

    /// `(axis, key)` extraction for `Predicate` evaluation. Returns
    /// `None` for `Reserved` and `Legacy` tags (which don't fit the
    /// axis taxonomy by construction).
    pub fn axis_key(&self) -> Option<TagKey> {
        match self {
            Self::AxisPresent { axis, key } | Self::AxisValue { axis, key, .. } => {
                Some(TagKey::new(*axis, key.clone()))
            }
            Self::Reserved { .. } | Self::Legacy(_) => None,
        }
    }

    /// Value half of a keyed axis tag. Returns `None` for presence
    /// tags, reserved tags, and legacy tags. (Reserved tags have a
    /// `body` accessible via [`Tag::reserved_body`]; semantically
    /// distinct from "axis value.")
    pub fn value(&self) -> Option<&str> {
        match self {
            Self::AxisValue { value, .. } => Some(value),
            _ => None,
        }
    }

    /// Body of a reserved-prefix tag (e.g. `<hex>` from
    /// `causal:<hex>`). `None` for non-reserved variants.
    pub fn reserved_body(&self) -> Option<&str> {
        match self {
            Self::Reserved { body, .. } => Some(body),
            _ => None,
        }
    }

    /// Reserved-prefix string (`causal:`, `scope:`, etc.) for
    /// reserved-variant tags. `None` otherwise.
    pub fn reserved_prefix(&self) -> Option<&str> {
        match self {
            Self::Reserved { prefix, .. } => Some(prefix),
            _ => None,
        }
    }

    /// `true` if this is a legacy untyped tag — useful for emitting
    /// the per-process deprecation log when the tag set is built.
    pub fn is_legacy(&self) -> bool {
        matches!(self, Self::Legacy(_))
    }

    /// Axis of an axis tag (`AxisPresent` / `AxisValue`). `None` for
    /// reserved + legacy tags.
    pub fn axis(&self) -> Option<TaxonomyAxis> {
        match self {
            Self::AxisPresent { axis, .. } | Self::AxisValue { axis, .. } => Some(*axis),
            _ => None,
        }
    }

    /// Canonical wire string. Mirrors [`fmt::Display`].
    pub fn to_wire(&self) -> String {
        self.to_string()
    }

    /// Semantic equality — like `PartialEq` but ignores the
    /// `=` vs `:` separator on `AxisValue`. Two tags that only
    /// differ in their wire-form separator describe the same
    /// `(axis, key, value)` and should compare equal for
    /// membership / require / diff purposes.
    ///
    /// `PartialEq` itself stays separator-aware so the wire
    /// form round-trips byte-for-byte; callers comparing for
    /// *meaning* (rather than for *bytes*) should prefer this
    /// method. See `CODE_REVIEW_2026_05_10_CAPABILITY_SYSTEM_2.md`
    /// CR-1..CR-3 for the bug class this guards against.
    pub fn semantic_eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::AxisPresent { axis: a1, key: k1 }, Self::AxisPresent { axis: a2, key: k2 }) => {
                a1 == a2 && k1 == k2
            }
            (
                Self::AxisValue {
                    axis: a1,
                    key: k1,
                    value: v1,
                    ..
                },
                Self::AxisValue {
                    axis: a2,
                    key: k2,
                    value: v2,
                    ..
                },
            ) => a1 == a2 && k1 == k2 && v1 == v2,
            (
                Self::Reserved {
                    prefix: p1,
                    body: b1,
                },
                Self::Reserved {
                    prefix: p2,
                    body: b2,
                },
            ) => p1 == p2 && b1 == b2,
            (Self::Legacy(a), Self::Legacy(b)) => a == b,
            _ => false,
        }
    }
}

/// Parse the body of an axis-prefixed tag (everything after the
/// `<axis>.` prefix). Decides between presence (`gpu`), `=`-keyed
/// (`gpu.vram_gb=80`), and `:`-keyed (`has_chain:<hex>`) shapes
/// based on which separator (if any) appears first.
fn parse_axis_body(axis: TaxonomyAxis, body: &str) -> Tag {
    let eq_idx = body.find('=');
    let colon_idx = body.find(':');
    let (separator, sep_idx) = match (eq_idx, colon_idx) {
        (Some(e), Some(c)) if e < c => (Some(AxisSeparator::Eq), Some(e)),
        (Some(_), Some(c)) => (Some(AxisSeparator::Colon), Some(c)),
        (Some(e), None) => (Some(AxisSeparator::Eq), Some(e)),
        (None, Some(c)) => (Some(AxisSeparator::Colon), Some(c)),
        (None, None) => (None, None),
    };
    match (separator, sep_idx) {
        (Some(separator), Some(idx)) => Tag::AxisValue {
            axis,
            key: body[..idx].to_string(),
            value: body[idx + 1..].to_string(),
            separator,
        },
        _ => Tag::AxisPresent {
            axis,
            key: body.to_string(),
        },
    }
}

impl fmt::Display for Tag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AxisPresent { axis, key } => write!(f, "{}.{}", axis.as_str(), key),
            Self::AxisValue {
                axis,
                key,
                value,
                separator,
            } => write!(
                f,
                "{}.{}{}{}",
                axis.as_str(),
                key,
                separator.as_char(),
                value
            ),
            Self::Reserved { prefix, body } => write!(f, "{prefix}{body}"),
            Self::Legacy(s) => f.write_str(s),
        }
    }
}

// =============================================================================
// Serde — wire format is the canonical Display string.
//
// Phase A.5.N.2: a `HashSet<Tag>` rides over the wire as a JSON
// string array (`["hardware.gpu", "scope:tenant:foo", "myteam-tag"]`).
// The internal enum shape is an implementation detail; the wire
// shape is the same byte-for-byte string the substrate's tag wire
// format already pins.
//
// `serde(tag = "kind")` (the previous derive form) couldn't handle
// `Tag::Legacy(String)` because internally-tagged enums require
// every variant to be a struct or unit variant. The canonical-string
// representation sidesteps that and matches the wire form callers
// already expect via `Tag::to_string()` / `Tag::parse()`.
// =============================================================================

impl Serialize for Tag {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Tag {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Self::parse(&s).map_err(serde::de::Error::custom)
    }
}

// =============================================================================
// Errors
// =============================================================================

/// Errors raised by tag construction / parsing.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CapabilityTagError {
    /// Empty string passed to a parser. Distinct from "valid empty
    /// tag" because we don't model emptiness as a tag.
    #[error("capability tag must be non-empty")]
    Empty,

    /// Application code tried to emit a tag with a reserved
    /// cross-axis prefix via [`Tag::parse_user`]. Substrate code that
    /// legitimately needs to emit such a tag uses [`Tag::parse`]
    /// instead.
    #[error("tag {tag:?} starts with reserved prefix {prefix:?}; user code cannot emit reserved-prefix tags")]
    ReservedPrefix {
        /// The reserved prefix the offending tag started with.
        prefix: String,
        /// The full offending tag string, echoed back for diagnostics.
        tag: String,
    },
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taxonomy_axis_string_round_trip() {
        for axis in TaxonomyAxis::all() {
            assert_eq!(TaxonomyAxis::from_prefix(axis.as_str()), Some(axis));
        }
        assert_eq!(TaxonomyAxis::from_prefix("unknown"), None);
        assert_eq!(TaxonomyAxis::from_prefix(""), None);
    }

    #[test]
    fn parse_axis_presence_tag() {
        let t = Tag::parse("hardware.gpu").unwrap();
        assert_eq!(
            t,
            Tag::AxisPresent {
                axis: TaxonomyAxis::Hardware,
                key: "gpu".into()
            }
        );
        assert_eq!(t.axis(), Some(TaxonomyAxis::Hardware));
        assert_eq!(
            t.axis_key(),
            Some(TagKey::new(TaxonomyAxis::Hardware, "gpu"))
        );
        assert_eq!(t.value(), None);
        assert_eq!(t.to_string(), "hardware.gpu");
    }

    #[test]
    fn parse_axis_value_tag_eq_separator() {
        let t = Tag::parse("hardware.gpu.vram_gb=80").unwrap();
        assert_eq!(
            t,
            Tag::AxisValue {
                axis: TaxonomyAxis::Hardware,
                key: "gpu.vram_gb".into(),
                value: "80".into(),
                separator: AxisSeparator::Eq,
            }
        );
        assert_eq!(t.value(), Some("80"));
        assert_eq!(t.to_string(), "hardware.gpu.vram_gb=80");
    }

    #[test]
    fn parse_axis_value_tag_colon_separator() {
        let t = Tag::parse("dataforts.has_chain:abc123").unwrap();
        assert_eq!(
            t,
            Tag::AxisValue {
                axis: TaxonomyAxis::Dataforts,
                key: "has_chain".into(),
                value: "abc123".into(),
                separator: AxisSeparator::Colon,
            }
        );
        assert_eq!(t.to_string(), "dataforts.has_chain:abc123");
    }

    #[test]
    fn parse_reserved_prefix_tags() {
        for (s, expected_prefix, expected_body) in [
            ("causal:abc123", "causal:", "abc123"),
            ("scope:prod", "scope:", "prod"),
            ("scope:tenant:foo", "scope:", "tenant:foo"),
            ("fork-of:0xdead", "fork-of:", "0xdead"),
            ("heat:abc=42", "heat:", "abc=42"),
        ] {
            let t = Tag::parse(s).unwrap();
            assert_eq!(t.reserved_prefix(), Some(expected_prefix), "tag={s}");
            assert_eq!(t.reserved_body(), Some(expected_body), "tag={s}");
            // Reserved tags have no axis / axis_key (they're cross-axis).
            assert_eq!(t.axis(), None);
            assert_eq!(t.axis_key(), None);
            // Round-trips via Display.
            assert_eq!(t.to_string(), s);
        }
    }

    #[test]
    fn parse_legacy_untyped_tag() {
        let t = Tag::parse("myteam-tag").unwrap();
        assert_eq!(t, Tag::Legacy("myteam-tag".into()));
        assert!(t.is_legacy());
        assert_eq!(t.axis(), None);
        assert_eq!(t.to_string(), "myteam-tag");
    }

    #[test]
    fn parse_unknown_axis_falls_through_to_legacy() {
        // `bogus.foo` is NOT one of the four axes, so it lands in legacy.
        // Pinned because a future axis addition shouldn't silently
        // reclassify legacy tags — adding a new axis is a deliberate
        // schema change.
        let t = Tag::parse("bogus.foo").unwrap();
        assert!(t.is_legacy());
        assert_eq!(t.to_string(), "bogus.foo");
    }

    #[test]
    fn parse_empty_returns_error() {
        assert_eq!(Tag::parse("").unwrap_err(), CapabilityTagError::Empty);
    }

    #[test]
    fn parse_user_rejects_reserved_prefix() {
        for s in ["causal:abc", "scope:prod", "fork-of:x", "heat:abc=1"] {
            let err = Tag::parse_user(s).unwrap_err();
            match err {
                CapabilityTagError::ReservedPrefix { tag, .. } => {
                    assert_eq!(tag, s, "tag={s}");
                }
                other => panic!("expected ReservedPrefix, got {other:?} for {s}"),
            }
        }
    }

    #[test]
    fn parse_user_accepts_axis_and_legacy() {
        Tag::parse_user("hardware.gpu").unwrap();
        Tag::parse_user("software.runtime=cuda-12.4").unwrap();
        Tag::parse_user("myteam-tag").unwrap();
    }

    #[test]
    fn parse_user_rejects_empty_same_as_internal() {
        assert_eq!(Tag::parse_user("").unwrap_err(), CapabilityTagError::Empty);
    }

    #[test]
    fn first_separator_wins_when_both_present() {
        // `=` first → Eq separator. Pinned because the `:` later in
        // the value should NOT cause the parser to re-split.
        let t = Tag::parse("hardware.gpu.driver=nvidia:535.86.10").unwrap();
        assert!(matches!(
            t,
            Tag::AxisValue {
                separator: AxisSeparator::Eq,
                ..
            }
        ));
        assert_eq!(t.value(), Some("nvidia:535.86.10"));
    }

    #[test]
    fn first_separator_wins_when_colon_first() {
        let t = Tag::parse("dataforts.has_chain:abc=def").unwrap();
        assert!(matches!(
            t,
            Tag::AxisValue {
                separator: AxisSeparator::Colon,
                ..
            }
        ));
        assert_eq!(t.value(), Some("abc=def"));
    }

    #[test]
    fn tag_key_display() {
        let k = TagKey::new(TaxonomyAxis::Hardware, "gpu");
        assert_eq!(k.to_string(), "hardware.gpu");
    }

    #[test]
    fn serde_round_trip_via_string() {
        // The Tag's enum-tagged JSON is internal; the canonical
        // wire form is the string returned by Display. Pin the
        // round-trip through parse → Display so a future serde
        // change to the enum doesn't silently drift.
        for s in [
            "hardware.gpu",
            "hardware.gpu.vram_gb=80",
            "dataforts.has_chain:abc123",
            "causal:abc123",
            "scope:tenant:foo",
            "myteam-tag",
        ] {
            let t = Tag::parse(s).unwrap();
            assert_eq!(t.to_string(), s, "round-trip drift for {s}");
        }
    }

    #[test]
    fn reserved_prefixes_constant_is_complete() {
        // Pin: every reserved prefix listed in the substrate plan
        // §2 is in RESERVED_PREFIXES. Adding a new prefix is a
        // schema-level decision; the test failing on a new prefix
        // forces the constant to be updated explicitly.
        //
        // `dataforts:` was promoted to a reserved prefix as part
        // of Phase 3b: the BLOB_STORAGE_UNHEALTHY_TAG
        // (`dataforts:blob-storage-unhealthy`) is documented as a
        // "cross-axis reserved tag" but was previously only
        // honored when callers manually constructed
        // `Tag::Reserved`. Promoting it makes `Tag::parse` round-
        // trip the canonical string form back into the Reserved
        // variant, which the fold-side synthesis path relies on.
        let expected: &[&str] = &["causal:", "dataforts:", "fork-of:", "heat:", "scope:"];
        assert_eq!(RESERVED_PREFIXES, expected);
    }
}
