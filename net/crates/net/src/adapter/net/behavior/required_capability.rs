//! `RequiredCapability` + the `require!` / `require_axis!` /
//! `require_axis_value!` macros — Phase A foundation for the
//! `IntentRegistry` shipped in `CAPABILITY_SYSTEM_PLAN.md` §7.
//!
//! Element type of the `IntentRegistry` value vector — each intent
//! maps to a `Vec<RequiredCapability>` describing what the
//! `metadata.intent`-tagged artifact needs from a candidate node.
//! All four `IntentRegistry::defaults()` examples in the substrate
//! plan land cleanly on the four variants below:
//!
//! | Substrate example | Variant produced |
//! |---|---|
//! | `require!("hardware.gpu")` | `Tag(Tag::AxisPresent { … })` |
//! | `require!("hardware.gpu.vram_gb >= 24")` | `Predicate(NumericAtLeast { … })` |
//! | `require!("software.daemon:postgres")` | `Tag(Tag::AxisValue { … })` |
//! | `require_axis!("devices")` | `AxisAny(Devices)` |
//! | `require_axis_value!("software", "model")` | `AxisKey(TagKey { … })` |
//!
//! Evaluation: [`RequiredCapability::evaluate`] returns `true` iff
//! the candidate's `(tags, metadata)` satisfies the requirement.
//! Pure function; reuses [`Predicate::evaluate`] for the predicate
//! variant and a tag-set scan for the others.

use crate::adapter::net::behavior::predicate::{EvalContext, Predicate};
use crate::adapter::net::behavior::tag::{CapabilityTagError, Tag, TagKey, TaxonomyAxis};

// =============================================================================
// RequiredCapability
// =============================================================================

/// One requirement an intent imposes on a candidate node.
///
/// Built from one of the three macros (`require!`, `require_axis!`,
/// `require_axis_value!`) or constructed directly via the variant
/// constructors. Cheap to clone; structural equality (no `Eq`
/// because the predicate variant carries `f64` thresholds).
#[derive(Debug, Clone, PartialEq)]
pub enum RequiredCapability {
    /// Specific tag must be present in the candidate's tag set.
    /// Built by `require!("<axis>.<key>")` (axis presence) or
    /// `require!("<axis>.<key>=<value>")` / `require!("<axis>.<key>:<value>")`
    /// (axis value).
    Tag(Tag),
    /// Predicate must evaluate to `true` against the candidate.
    /// Built by `require!("<axis>.<key> >= <n>")` and similar
    /// comparison forms.
    Predicate(Predicate),
    /// Any tag in this axis is sufficient. Built by
    /// `require_axis!("<axis>")` — useful for "any device" /
    /// "any loaded model" / etc.
    AxisAny(TaxonomyAxis),
    /// Any tag with this `(axis, key)` is sufficient (presence or
    /// value). Built by `require_axis_value!("<axis>", "<key>")`.
    AxisKey(TagKey),
}

impl RequiredCapability {
    /// Evaluate against a candidate's `(tags, metadata)`. Pure
    /// function; reuses [`Predicate::evaluate`] for the
    /// `Predicate` variant.
    pub fn evaluate(&self, ctx: &EvalContext<'_>) -> bool {
        match self {
            Self::Tag(required) => ctx.tags.iter().any(|t| t == required),
            Self::Predicate(p) => p.evaluate(ctx),
            Self::AxisAny(axis) => ctx.tags.iter().any(|t| t.axis() == Some(*axis)),
            Self::AxisKey(key) => ctx.tags.iter().any(|t| t.axis_key().as_ref() == Some(key)),
        }
    }
}

// =============================================================================
// Errors
// =============================================================================

/// Errors raised by the `require!` family of macros at parse time.
/// All are programmer errors — the macros panic with these
/// messages so misuse fails loudly at first run.
#[derive(Debug, thiserror::Error)]
pub enum RequireParseError {
    /// Empty input passed to a `require*!` macro.
    #[error("require! input must be non-empty")]
    Empty,
    /// Wrapped [`CapabilityTagError`] from tag parsing
    /// (e.g. user attempted to emit a reserved-prefix tag).
    #[error("require! could not parse tag: {0}")]
    Tag(#[from] CapabilityTagError),
    /// Numeric comparison's right-hand side did not parse to
    /// `f64`. Carries the original lhs / rhs spelling for
    /// diagnostics.
    #[error("require! numeric value {value:?} for key {key:?} did not parse as f64")]
    NumericParse {
        /// Tag-key portion of the failed comparison (`hardware.gpu.vram_gb`).
        key: String,
        /// Right-hand-side spelling that failed to parse (`twenty-four`).
        value: String,
    },
    /// Tag key (`<axis>.<key>`) couldn't be parsed (missing dot or
    /// unknown axis prefix).
    #[error("require! tag key {key:?} must be `<axis>.<key>` with a known axis")]
    InvalidKey {
        /// Offending key spelling.
        key: String,
    },
    /// Unknown axis literal passed to `require_axis!`.
    #[error("require_axis! axis {axis:?} is not one of: hardware, software, devices, dataforts")]
    InvalidAxis {
        /// Offending axis spelling.
        axis: String,
    },
}

// =============================================================================
// Runtime parsers (used by the macros)
// =============================================================================

/// Parse the string passed to `require!` into a [`RequiredCapability`].
/// Three shapes recognized, in priority order:
///
/// 1. `<key> >= <number>` / `<key> <= <number>` / `<key> == <value>`
///    → [`RequiredCapability::Predicate`]
/// 2. `<axis>.<key>=<value>` / `<axis>.<key>:<value>` (no spaces
///    around the separator) → [`RequiredCapability::Tag`] holding
///    a [`Tag::AxisValue`]
/// 3. `<axis>.<key>` (no operator, no separator) →
///    [`RequiredCapability::Tag`] holding a [`Tag::AxisPresent`]
///
/// User code shouldn't call this directly — the [`require!`] macro
/// does. It's `pub(crate)` because the macro expands across crate
/// boundaries to call into here.
#[doc(hidden)]
pub fn __require_parse(s: &str) -> Result<RequiredCapability, RequireParseError> {
    let s = s.trim();
    if s.is_empty() {
        return Err(RequireParseError::Empty);
    }

    // 1. Comparison operators (longer-first to avoid `=` matching `>=`).
    for (op, build) in [
        (
            ">=",
            (|key: TagKey, n: f64| Predicate::numeric_at_least(key, n))
                as fn(TagKey, f64) -> Predicate,
        ),
        (
            "<=",
            (|key: TagKey, n: f64| Predicate::numeric_at_most(key, n))
                as fn(TagKey, f64) -> Predicate,
        ),
    ] {
        if let Some((lhs, rhs)) = s.split_once(op) {
            let lhs = lhs.trim();
            let rhs = rhs.trim();
            let key = parse_tag_key(lhs)?;
            let n: f64 = rhs.parse().map_err(|_| RequireParseError::NumericParse {
                key: lhs.to_string(),
                value: rhs.to_string(),
            })?;
            return Ok(RequiredCapability::Predicate(build(key, n)));
        }
    }
    if let Some((lhs, rhs)) = s.split_once("==") {
        let lhs = lhs.trim();
        let rhs = rhs.trim().trim_matches('"');
        let key = parse_tag_key(lhs)?;
        return Ok(RequiredCapability::Predicate(Predicate::equals(
            key,
            rhs.to_string(),
        )));
    }

    // 2 + 3. Plain tag (presence or value form).
    let tag = Tag::parse_user(s)?;
    Ok(RequiredCapability::Tag(tag))
}

/// Parse `"<axis>"` into a [`TaxonomyAxis`] for the
/// [`require_axis!`] macro. Errors on unknown axis spelling.
#[doc(hidden)]
pub fn __require_axis_parse(s: &str) -> Result<TaxonomyAxis, RequireParseError> {
    TaxonomyAxis::from_prefix(s.trim()).ok_or_else(|| RequireParseError::InvalidAxis {
        axis: s.to_string(),
    })
}

/// Parse `"<axis>"` + `"<key>"` into a [`TagKey`] for the
/// [`require_axis_value!`] macro.
#[doc(hidden)]
pub fn __require_axis_value_parse(axis: &str, key: &str) -> Result<TagKey, RequireParseError> {
    let axis =
        TaxonomyAxis::from_prefix(axis.trim()).ok_or_else(|| RequireParseError::InvalidAxis {
            axis: axis.to_string(),
        })?;
    let key = key.trim();
    if key.is_empty() {
        return Err(RequireParseError::InvalidKey { key: String::new() });
    }
    Ok(TagKey::new(axis, key))
}

/// Parse `"<axis>.<key>"` into a [`TagKey`]. Used by the comparison-
/// operator branches of [`__require_parse`].
fn parse_tag_key(s: &str) -> Result<TagKey, RequireParseError> {
    let (axis_str, key) = s
        .split_once('.')
        .ok_or_else(|| RequireParseError::InvalidKey { key: s.to_string() })?;
    let axis = TaxonomyAxis::from_prefix(axis_str)
        .ok_or_else(|| RequireParseError::InvalidKey { key: s.to_string() })?;
    if key.is_empty() {
        return Err(RequireParseError::InvalidKey { key: s.to_string() });
    }
    Ok(TagKey::new(axis, key.to_string()))
}

// =============================================================================
// Macros
// =============================================================================

/// `require!(<spec>)` — build a [`RequiredCapability`] from a
/// string-literal spec. Panics at construction on malformed input
/// (matches the substrate plan's "validates shapes at parse time"
/// contract for the macro family).
///
/// ## Forms
///
/// ```ignore
/// require!("hardware.gpu");                  // axis presence
/// require!("software.daemon:postgres");      // axis value
/// require!("hardware.gpu.vram_gb >= 24");    // numeric ≥
/// require!("hardware.cpu_cores <= 64");      // numeric ≤
/// require!("software.runtime == \"cuda-12.4\"");  // string equality
/// ```
///
/// Reserved-prefix tags (`causal:`, `scope:`, etc.) are rejected —
/// `require!("scope:prod")` panics with `CapabilityTagError::ReservedPrefix`.
/// Use `require_axis_value!` if a reserved-prefix concept needs to
/// land in an intent registry (it shouldn't — those prefixes are
/// substrate-private).
#[macro_export]
macro_rules! require {
    ($spec:literal) => {
        $crate::adapter::net::behavior::required_capability::__require_parse($spec)
            .unwrap_or_else(|e| panic!("require!({:?}) failed at parse time: {}", $spec, e))
    };
}

/// `require_axis!(<axis>)` — build a [`RequiredCapability::AxisAny`]
/// matching any tag in the named axis. Useful for "any device" /
/// "any loaded model" intents where the application doesn't need a
/// specific tag, just *something* in the axis.
///
/// ```ignore
/// require_axis!("devices");   // any tag with axis = Devices
/// require_axis!("software");  // any tag with axis = Software
/// ```
///
/// Panics on unknown axis spelling.
#[macro_export]
macro_rules! require_axis {
    ($axis:literal) => {
        $crate::adapter::net::behavior::required_capability::RequiredCapability::AxisAny(
            $crate::adapter::net::behavior::required_capability::__require_axis_parse($axis)
                .unwrap_or_else(|e| panic!("require_axis!({:?}) failed: {}", $axis, e)),
        )
    };
}

/// `require_axis_value!(<axis>, <key>)` — build a
/// [`RequiredCapability::AxisKey`] matching any tag with the given
/// `(axis, key)` pair (presence OR value). Useful for "any version
/// of this thing" intents — e.g. `require_axis_value!("software",
/// "model")` matches `software.model`, `software.model:llama-7b`,
/// or `software.model=mistral-large` interchangeably.
///
/// ```ignore
/// require_axis_value!("software", "model");
/// require_axis_value!("hardware", "gpu");
/// ```
///
/// Panics on unknown axis spelling or empty key.
#[macro_export]
macro_rules! require_axis_value {
    ($axis:literal, $key:literal) => {
        $crate::adapter::net::behavior::required_capability::RequiredCapability::AxisKey(
            $crate::adapter::net::behavior::required_capability::__require_axis_value_parse(
                $axis, $key,
            )
            .unwrap_or_else(|e| {
                panic!("require_axis_value!({:?}, {:?}) failed: {}", $axis, $key, e)
            }),
        )
    };
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::adapter::net::behavior::tag::{AxisSeparator, Tag, TaxonomyAxis};

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
            separator: AxisSeparator::Eq,
        }
    }

    fn axis_colon(axis: TaxonomyAxis, key: &str, value: &str) -> Tag {
        Tag::AxisValue {
            axis,
            key: key.into(),
            value: value.into(),
            separator: AxisSeparator::Colon,
        }
    }

    fn meta() -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    // ---- require! parsing ---------------------------------------------------

    #[test]
    fn require_axis_presence() {
        let r = require!("hardware.gpu");
        assert_eq!(
            r,
            RequiredCapability::Tag(Tag::AxisPresent {
                axis: TaxonomyAxis::Hardware,
                key: "gpu".into(),
            })
        );
    }

    #[test]
    fn require_axis_value_eq() {
        let r = require!("hardware.gpu.vram_gb=80");
        assert_eq!(
            r,
            RequiredCapability::Tag(Tag::AxisValue {
                axis: TaxonomyAxis::Hardware,
                key: "gpu.vram_gb".into(),
                value: "80".into(),
                separator: AxisSeparator::Eq,
            })
        );
    }

    #[test]
    fn require_dataforts_pre_typed_colon() {
        let r = require!("software.daemon:postgres");
        match r {
            RequiredCapability::Tag(Tag::AxisValue {
                axis,
                key,
                value,
                separator,
            }) => {
                assert_eq!(axis, TaxonomyAxis::Software);
                assert_eq!(key, "daemon");
                assert_eq!(value, "postgres");
                assert_eq!(separator, AxisSeparator::Colon);
            }
            other => panic!("expected AxisValue with `:` separator, got {other:?}"),
        }
    }

    #[test]
    fn require_numeric_at_least() {
        let r = require!("hardware.gpu.vram_gb >= 24");
        match r {
            RequiredCapability::Predicate(Predicate::NumericAtLeast { key, threshold }) => {
                assert_eq!(key.axis, TaxonomyAxis::Hardware);
                assert_eq!(key.key, "gpu.vram_gb");
                assert!((threshold - 24.0).abs() < f64::EPSILON);
            }
            other => panic!("expected NumericAtLeast, got {other:?}"),
        }
    }

    #[test]
    fn require_numeric_at_most() {
        let r = require!("hardware.cpu_cores <= 64");
        match r {
            RequiredCapability::Predicate(Predicate::NumericAtMost { key, threshold }) => {
                assert_eq!(key.key, "cpu_cores");
                assert!((threshold - 64.0).abs() < f64::EPSILON);
            }
            other => panic!("expected NumericAtMost, got {other:?}"),
        }
    }

    #[test]
    fn require_numeric_threshold_can_be_float() {
        // Pinned: thresholds that aren't integers (e.g. RTT
        // budgets in milliseconds) parse as f64.
        let r = require!("hardware.cpu_cores >= 1.5");
        match r {
            RequiredCapability::Predicate(Predicate::NumericAtLeast { threshold, .. }) => {
                assert!((threshold - 1.5).abs() < f64::EPSILON);
            }
            other => panic!("expected NumericAtLeast, got {other:?}"),
        }
    }

    #[test]
    fn require_string_equality() {
        let r = require!("software.runtime == \"cuda-12.4\"");
        match r {
            RequiredCapability::Predicate(Predicate::Equals { key, value }) => {
                assert_eq!(key.axis, TaxonomyAxis::Software);
                assert_eq!(key.key, "runtime");
                assert_eq!(value, "cuda-12.4");
            }
            other => panic!("expected Equals, got {other:?}"),
        }
    }

    // ---- require_axis! ------------------------------------------------------

    #[test]
    fn require_axis_each_taxonomy() {
        for axis in TaxonomyAxis::all() {
            let r = match axis {
                TaxonomyAxis::Hardware => require_axis!("hardware"),
                TaxonomyAxis::Software => require_axis!("software"),
                TaxonomyAxis::Devices => require_axis!("devices"),
                TaxonomyAxis::Dataforts => require_axis!("dataforts"),
            };
            assert_eq!(r, RequiredCapability::AxisAny(axis));
        }
    }

    // ---- require_axis_value! ------------------------------------------------

    #[test]
    fn require_axis_value_basic() {
        let r = require_axis_value!("software", "model");
        assert_eq!(
            r,
            RequiredCapability::AxisKey(TagKey::new(TaxonomyAxis::Software, "model"))
        );
    }

    // ---- evaluation ---------------------------------------------------------

    #[test]
    fn tag_variant_matches_exact_tag() {
        let tags = [axis_present(TaxonomyAxis::Hardware, "gpu")];
        let m = meta();
        let ctx = EvalContext::new(&tags, &m);
        let r = require!("hardware.gpu");
        assert!(r.evaluate(&ctx));
        // Different key — no match.
        let r = require!("hardware.tpu");
        assert!(!r.evaluate(&ctx));
    }

    #[test]
    fn tag_variant_value_matches_exactly() {
        let tags = [axis_eq(TaxonomyAxis::Hardware, "gpu.vram_gb", "80")];
        let m = meta();
        let ctx = EvalContext::new(&tags, &m);
        let r = require!("hardware.gpu.vram_gb=80");
        assert!(r.evaluate(&ctx));
        // Different value — no match (Tag variant is exact).
        let r = require!("hardware.gpu.vram_gb=24");
        assert!(!r.evaluate(&ctx));
    }

    #[test]
    fn predicate_variant_evaluates_via_predicate() {
        let tags = [axis_eq(TaxonomyAxis::Hardware, "gpu.vram_gb", "80")];
        let m = meta();
        let ctx = EvalContext::new(&tags, &m);
        // Numeric ≥ 24 against value "80" → true.
        let r = require!("hardware.gpu.vram_gb >= 24");
        assert!(r.evaluate(&ctx));
        let r = require!("hardware.gpu.vram_gb >= 96");
        assert!(!r.evaluate(&ctx));
    }

    #[test]
    fn axis_any_matches_any_tag_in_axis() {
        let tags = [axis_present(TaxonomyAxis::Devices, "lidar")];
        let m = meta();
        let ctx = EvalContext::new(&tags, &m);
        let r = require_axis!("devices");
        assert!(r.evaluate(&ctx));
        // No devices tag — no match.
        let tags = [axis_present(TaxonomyAxis::Hardware, "gpu")];
        let ctx = EvalContext::new(&tags, &m);
        assert!(!r.evaluate(&ctx));
    }

    #[test]
    fn axis_key_matches_presence_or_value() {
        // Presence form matches.
        let tags = [axis_present(TaxonomyAxis::Software, "model")];
        let m = meta();
        let ctx = EvalContext::new(&tags, &m);
        let r = require_axis_value!("software", "model");
        assert!(r.evaluate(&ctx));
        // Value form matches.
        let tags = [axis_colon(TaxonomyAxis::Software, "model", "llama-7b")];
        let ctx = EvalContext::new(&tags, &m);
        assert!(r.evaluate(&ctx));
        // Different key — no match.
        let tags = [axis_present(TaxonomyAxis::Software, "runtime")];
        let ctx = EvalContext::new(&tags, &m);
        assert!(!r.evaluate(&ctx));
    }

    // ---- error paths --------------------------------------------------------

    #[test]
    fn require_unknown_axis_falls_through_to_legacy_tag() {
        // `bogus.foo` isn't one of the four known axes, so the
        // parser falls through to `Tag::Legacy("bogus.foo")`. This
        // is intentional — the deprecation window for untyped tags
        // (Locked decision 1) keeps such forms parseable. Pin the
        // behavior here so a future "reject legacy in require!"
        // change is loud.
        let r = __require_parse("bogus.foo").unwrap();
        match r {
            RequiredCapability::Tag(Tag::Legacy(s)) => assert_eq!(s, "bogus.foo"),
            other => panic!("expected Tag(Legacy(...)), got {other:?}"),
        }
    }

    #[test]
    fn require_parses_unparseable_threshold_as_error() {
        match __require_parse("hardware.cpu_cores >= many") {
            Err(RequireParseError::NumericParse { key, value }) => {
                assert_eq!(key, "hardware.cpu_cores");
                assert_eq!(value, "many");
            }
            other => panic!("expected NumericParse error, got {other:?}"),
        }
    }

    #[test]
    fn require_rejects_reserved_prefix() {
        match __require_parse("scope:prod") {
            Err(RequireParseError::Tag(CapabilityTagError::ReservedPrefix { prefix, .. })) => {
                assert_eq!(prefix, "scope:");
            }
            other => panic!("expected ReservedPrefix, got {other:?}"),
        }
    }

    #[test]
    fn require_rejects_empty() {
        match __require_parse("") {
            Err(RequireParseError::Empty) => {}
            other => panic!("expected Empty, got {other:?}"),
        }
        match __require_parse("   ") {
            Err(RequireParseError::Empty) => {}
            other => panic!("expected Empty, got {other:?}"),
        }
    }

    #[test]
    fn require_axis_rejects_unknown_axis() {
        match __require_axis_parse("bogus") {
            Err(RequireParseError::InvalidAxis { axis }) => {
                assert_eq!(axis, "bogus");
            }
            other => panic!("expected InvalidAxis, got {other:?}"),
        }
    }

    #[test]
    fn require_axis_value_rejects_empty_key() {
        match __require_axis_value_parse("software", "") {
            Err(RequireParseError::InvalidKey { .. }) => {}
            other => panic!("expected InvalidKey, got {other:?}"),
        }
    }

    // ---- intent-registry-style usage (substrate plan §7 worked example) ----

    #[test]
    fn intent_registry_defaults_examples_compile_and_evaluate() {
        // Mirror the substrate plan's IntentRegistry::defaults() entry
        // for "ml-training": [hardware.gpu, hardware.gpu.vram_gb >= 24].
        // A node with both tags satisfies; one tag alone does not.
        let reqs = [
            require!("hardware.gpu"),
            require!("hardware.gpu.vram_gb >= 24"),
        ];

        // Both required tags present + adequate VRAM → all reqs match.
        let tags = [
            axis_present(TaxonomyAxis::Hardware, "gpu"),
            axis_eq(TaxonomyAxis::Hardware, "gpu.vram_gb", "80"),
        ];
        let m = meta();
        let ctx = EvalContext::new(&tags, &m);
        assert!(reqs.iter().all(|r| r.evaluate(&ctx)));

        // GPU present but VRAM only 16 → numeric req fails.
        let tags = [
            axis_present(TaxonomyAxis::Hardware, "gpu"),
            axis_eq(TaxonomyAxis::Hardware, "gpu.vram_gb", "16"),
        ];
        let ctx = EvalContext::new(&tags, &m);
        assert!(!reqs.iter().all(|r| r.evaluate(&ctx)));

        // No GPU tag at all → both reqs fail.
        let tags: Vec<Tag> = vec![];
        let ctx = EvalContext::new(&tags, &m);
        assert!(!reqs.iter().any(|r| r.evaluate(&ctx)));
    }
}
