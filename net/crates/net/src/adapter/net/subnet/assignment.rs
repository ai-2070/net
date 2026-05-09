//! Label-based subnet assignment.
//!
//! Nodes belong to subnets by their capability tags, not static configuration.
//! A `SubnetPolicy` maps tag patterns to hierarchy levels, deriving a `SubnetId`
//! from a node's `CapabilitySet`.

use std::collections::HashMap;

use super::error::SubnetError;
use super::id::SubnetId;
use crate::adapter::net::behavior::capability::CapabilitySet;

/// Policy for assigning nodes to subnets based on capability tags.
///
/// Rules are evaluated in order. Each rule maps a tag prefix to a hierarchy
/// level and provides a value map for the tag's value.
///
/// Example: a node with tags `["region:us-west", "fleet:alpha"]` and rules:
/// - `SubnetRule { tag_prefix: "region:", level: 0, values: {"us-west": 1} }`
/// - `SubnetRule { tag_prefix: "fleet:", level: 1, values: {"alpha": 2} }`
///
/// Would get `SubnetId::new(&[1, 2])`.
///
/// # Semantics (rule precedence and matching contract)
///
/// Pinned by unit tests in this module — changes here are
/// behavioral breaks for operators configuring subnets:
///
/// 1. **Rule order is declaration order.** `assign()` walks
///    `rules` in the order passed to `add_rule()`. Two rules
///    targeting the *same* `level` with overlapping values
///    resolve as *later-rule-wins*: the earlier rule may write
///    the level byte first, but a subsequent match at the same
///    level overwrites it.
/// 2. **First tag wins per rule.** Inside one rule, the first
///    capability tag whose stripped suffix is present in `values`
///    wins — subsequent tags matching the same rule are ignored.
/// 3. **No partial-prefix match on values.** `tag_prefix` is
///    stripped by [`str::strip_prefix`]; the remaining value is
///    then looked up by *exact* string equality against `values`.
///    A rule `region:` matching on `"us"` will **not** match the
///    tag `region:us:extra` (the stripped suffix `"us:extra"` is
///    not in the values map).
/// 4. **Unmatched levels stay zero.** Levels with no rule (or a
///    rule that failed to match) remain `0`, which [`SubnetId`]
///    interprets as "no restriction at this level".
#[derive(Debug, Clone)]
pub struct SubnetPolicy {
    rules: Vec<SubnetRule>,
}

/// A single rule mapping a tag pattern to a hierarchy level.
#[derive(Debug, Clone)]
pub struct SubnetRule {
    /// Tag prefix to match (e.g., "region:").
    pub tag_prefix: String,
    /// Which hierarchy level this tag fills (0-3).
    pub level: u8,
    /// Map from tag value to level value (e.g., "us-west" -> 1).
    pub values: HashMap<String, u8>,
}

impl SubnetPolicy {
    /// Create an empty policy (all nodes get SubnetId::GLOBAL).
    pub fn new() -> Self {
        Self { rules: Vec::new() }
    }

    /// Add a rule to the policy.
    ///
    /// # Panics
    /// Panics if the rule's level is >= 4. For untrusted input
    /// (config files, FFI, JSON) prefer [`Self::try_add_rule`],
    /// which returns a [`SubnetError`] instead of panicking.
    pub fn add_rule(self, rule: SubnetRule) -> Self {
        self.try_add_rule(rule)
            .expect("SubnetPolicy::add_rule: invalid rule (use try_add_rule for fallible)")
    }

    /// Fallible variant of [`Self::add_rule`].
    ///
    /// Pre-existing `add_rule` panics on `rule.level >= 4`.
    /// Subnet policies typically come from config / FFI / JSON and
    /// a malformed entry should surface as a recoverable error
    /// rather than crashing the daemon loader.
    pub fn try_add_rule(mut self, rule: SubnetRule) -> Result<Self, SubnetError> {
        if rule.level >= 4 {
            return Err(SubnetError::LevelOutOfRange { got: rule.level });
        }
        self.rules.push(rule);
        Ok(self)
    }

    /// Assign a subnet ID to a node based on its capability tags.
    ///
    /// Evaluates all rules against the node's tags. Unmatched levels
    /// remain zero (meaning "no restriction at that level").
    pub fn assign(&self, caps: &CapabilitySet) -> SubnetId {
        let mut levels = [0u8; 4];

        // Phase A.5.N.2: caps.tags is HashSet<Tag>; render each tag
        // to its wire-form string AND sort lexicographically so
        // the first-match-wins resolution is deterministic across
        // runs (HashSet iteration order is unspecified).
        let mut tag_strings: Vec<String> = caps.tags.iter().map(|t| t.to_string()).collect();
        tag_strings.sort();

        for rule in &self.rules {
            for s in &tag_strings {
                if let Some(value) = s.strip_prefix(&rule.tag_prefix) {
                    if let Some(&level_value) = rule.values.get(value) {
                        levels[rule.level as usize] = level_value;
                        break; // first match wins for this rule
                    }
                }
            }
        }

        SubnetId::new(&levels)
    }
}

impl Default for SubnetPolicy {
    fn default() -> Self {
        Self::new()
    }
}

impl SubnetRule {
    /// Create a new rule.
    pub fn new(tag_prefix: impl Into<String>, level: u8) -> Self {
        Self {
            tag_prefix: tag_prefix.into(),
            level,
            values: HashMap::new(),
        }
    }

    /// Map a tag value to a level value.
    ///
    /// # Panics
    /// Panics if `level_value` is 0 (reserved for "unmatched /
    /// no restriction"). For untrusted input prefer
    /// [`Self::try_map`].
    pub fn map(self, tag_value: impl Into<String>, level_value: u8) -> Self {
        self.try_map(tag_value, level_value)
            .expect("SubnetRule::map: level_value 0 is reserved (use try_map for fallible)")
    }

    /// Fallible variant of [`Self::map`].
    ///
    /// Pre-existing `map` panics on `level_value == 0`.
    /// Returns [`SubnetError::LevelValueReserved`] instead.
    pub fn try_map(
        mut self,
        tag_value: impl Into<String>,
        level_value: u8,
    ) -> Result<Self, SubnetError> {
        if level_value == 0 {
            return Err(SubnetError::LevelValueReserved);
        }
        self.values.insert(tag_value.into(), level_value);
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::capability::CapabilitySet;

    fn caps_with_tags(tags: &[&str]) -> CapabilitySet {
        let mut caps = CapabilitySet::new();
        for tag in tags {
            caps = caps.add_tag(*tag);
        }
        caps
    }

    #[test]
    fn test_empty_policy() {
        let policy = SubnetPolicy::new();
        let caps = caps_with_tags(&["region:us-west"]);
        assert_eq!(policy.assign(&caps), SubnetId::GLOBAL);
    }

    #[test]
    fn test_single_level() {
        let policy = SubnetPolicy::new().add_rule(
            SubnetRule::new("region:", 0)
                .map("us-west", 1)
                .map("eu-central", 2),
        );

        let caps = caps_with_tags(&["region:us-west"]);
        assert_eq!(policy.assign(&caps), SubnetId::new(&[1]));

        let caps = caps_with_tags(&["region:eu-central"]);
        assert_eq!(policy.assign(&caps), SubnetId::new(&[2]));
    }

    #[test]
    fn test_multi_level() {
        let policy = SubnetPolicy::new()
            .add_rule(
                SubnetRule::new("region:", 0)
                    .map("us-west", 1)
                    .map("eu-central", 2),
            )
            .add_rule(SubnetRule::new("fleet:", 1).map("alpha", 1).map("beta", 2));

        let caps = caps_with_tags(&["region:us-west", "fleet:beta"]);
        assert_eq!(policy.assign(&caps), SubnetId::new(&[1, 2]));
    }

    #[test]
    fn test_unmatched_tag() {
        let policy = SubnetPolicy::new().add_rule(SubnetRule::new("region:", 0).map("us-west", 1));

        // Tag value not in the map
        let caps = caps_with_tags(&["region:unknown"]);
        assert_eq!(policy.assign(&caps), SubnetId::GLOBAL);

        // No matching tag prefix
        let caps = caps_with_tags(&["fleet:alpha"]);
        assert_eq!(policy.assign(&caps), SubnetId::GLOBAL);
    }

    #[test]
    fn test_partial_match() {
        let policy = SubnetPolicy::new()
            .add_rule(SubnetRule::new("region:", 0).map("us-west", 3))
            .add_rule(SubnetRule::new("fleet:", 1).map("alpha", 7));

        // Only region tag, no fleet
        let caps = caps_with_tags(&["region:us-west"]);
        assert_eq!(policy.assign(&caps), SubnetId::new(&[3]));
    }

    #[test]
    fn test_four_levels() {
        let policy = SubnetPolicy::new()
            .add_rule(SubnetRule::new("region:", 0).map("us", 1))
            .add_rule(SubnetRule::new("fleet:", 1).map("f1", 2))
            .add_rule(SubnetRule::new("vehicle:", 2).map("v42", 3))
            .add_rule(SubnetRule::new("subsystem:", 3).map("lidar", 4));

        let caps = caps_with_tags(&["region:us", "fleet:f1", "vehicle:v42", "subsystem:lidar"]);
        assert_eq!(policy.assign(&caps), SubnetId::new(&[1, 2, 3, 4]));
    }

    // ========================================================================
    // Tie-breaking / ambiguity semantics (TEST_COVERAGE_PLAN §P3-17)
    //
    // Pins the three ambiguity cases the doc contract on
    // `SubnetPolicy` calls out: same-prefix duplicate rules,
    // rule-order dependency for the same level, and the no-partial-
    // match contract on values. If any of these assertions flips,
    // either the doc contract is wrong or a silent behavior change
    // snuck in — the PR touching `assign()` needs to decide which.
    // ========================================================================

    /// Duplicate `tag_prefix` rules both writing the same level:
    /// the later rule wins (last write). An earlier rule's mapping
    /// is overwritten if a later rule matches the same tag input.
    #[test]
    fn duplicate_prefix_same_level_later_rule_wins() {
        let policy = SubnetPolicy::new()
            // First rule writes level 0 = 1
            .add_rule(SubnetRule::new("region:", 0).map("us", 1))
            // Second rule at the same level remaps "us" to 9
            .add_rule(SubnetRule::new("region:", 0).map("us", 9));

        let caps = caps_with_tags(&["region:us"]);
        assert_eq!(
            policy.assign(&caps),
            SubnetId::new(&[9]),
            "a later rule with the same prefix + level must overwrite \
             the earlier rule's value — pinned as last-write-wins",
        );
    }

    /// Duplicate `tag_prefix` rules writing *different* levels
    /// coexist: both writes land on their respective level slots.
    /// Exercises the "rules evaluated in declaration order, each
    /// writes its own level independently" part of the contract.
    #[test]
    fn duplicate_prefix_different_levels_both_apply() {
        let policy = SubnetPolicy::new()
            .add_rule(SubnetRule::new("region:", 0).map("us", 1))
            // Same prefix, different level — coexists with the first
            .add_rule(SubnetRule::new("region:", 2).map("us", 5));

        let caps = caps_with_tags(&["region:us"]);
        assert_eq!(
            policy.assign(&caps),
            SubnetId::new(&[1, 0, 5, 0]),
            "two rules sharing a prefix but targeting different \
             levels must both fire; level 1 + 3 remain unset",
        );
    }

    /// Rule-order dependency: when two rules both claim the same
    /// level but match *different* tags, the later rule's match
    /// still overwrites the earlier rule's match if both tags are
    /// present on the node. Pins "later rule wins" even across
    /// different tag prefixes targeting the same level.
    #[test]
    fn rule_order_dependency_later_rule_overwrites_earlier_level_write() {
        let policy = SubnetPolicy::new()
            // Earlier: region:* writes level 0
            .add_rule(SubnetRule::new("region:", 0).map("us", 1))
            // Later: zone:* ALSO writes level 0 — this rule comes
            // after the first, so it wins when both tags match
            .add_rule(SubnetRule::new("zone:", 0).map("west", 4));

        let caps = caps_with_tags(&["region:us", "zone:west"]);
        assert_eq!(
            policy.assign(&caps),
            SubnetId::new(&[4]),
            "later rule targeting the same level must overwrite earlier one",
        );

        // And: if only the earlier rule's tag is present, level 0
        // still ends up with the earlier rule's value (the later
        // rule does not match any tag).
        let caps = caps_with_tags(&["region:us"]);
        assert_eq!(
            policy.assign(&caps),
            SubnetId::new(&[1]),
            "later rule does not clobber when it has no matching tag",
        );
    }

    /// No partial-match on the stripped value: `values` is an
    /// exact-string lookup table, not a prefix matcher. A tag
    /// carrying extra suffix after the prefix does not hit a rule
    /// keyed on the bare inner token.
    #[test]
    fn partial_prefix_on_value_does_not_match() {
        let policy = SubnetPolicy::new().add_rule(SubnetRule::new("region:", 0).map("us", 1));

        // `region:us` → stripped "us" → hits values map.
        let caps = caps_with_tags(&["region:us"]);
        assert_eq!(policy.assign(&caps), SubnetId::new(&[1]));

        // `region:us:extra` → stripped "us:extra" → NOT in map,
        // so rule doesn't fire and level stays at zero.
        let caps = caps_with_tags(&["region:us:extra"]);
        assert_eq!(
            policy.assign(&caps),
            SubnetId::GLOBAL,
            "values map is exact-match; suffixes after the matching \
             inner token must not partial-match against the map key",
        );

        // Cousin case: tag where the stripped value is a *prefix*
        // of a values-map entry doesn't match either.
        let policy = SubnetPolicy::new().add_rule(SubnetRule::new("region:", 0).map("us-west", 1));
        let caps = caps_with_tags(&["region:us"]);
        assert_eq!(
            policy.assign(&caps),
            SubnetId::GLOBAL,
            "stripped value \"us\" is a prefix of \"us-west\" but \
             must not partial-match the values map key",
        );
    }

    /// First matching tag wins *within* a single rule — a second
    /// tag for the same rule is ignored (the `break` in `assign`).
    /// Phase A.5.N.2: tags are now `HashSet<Tag>` (unordered);
    /// `assign()` sorts tag strings lexicographically before the
    /// first-match scan, so the result is deterministic regardless
    /// of insertion order.
    #[test]
    fn first_tag_wins_within_a_single_rule() {
        let policy =
            SubnetPolicy::new().add_rule(SubnetRule::new("region:", 0).map("us", 1).map("eu", 2));

        // Both insertions converge on the same answer — the
        // lexicographically-first matching tag wins. `region:eu`
        // sorts before `region:us`, so 2 (eu's level value) wins
        // regardless of which tag was inserted first.
        let caps = caps_with_tags(&["region:us", "region:eu"]);
        assert_eq!(
            policy.assign(&caps),
            SubnetId::new(&[2]),
            "lexicographically-first matching tag wins (`region:eu` < `region:us`)",
        );

        let caps = caps_with_tags(&["region:eu", "region:us"]);
        assert_eq!(
            policy.assign(&caps),
            SubnetId::new(&[2]),
            "insertion order is irrelevant — the same tag still wins",
        );
    }

    /// Out-of-range `level` must surface as `Err(...)`,
    /// not panic. Subnet policies typically come from config /
    /// FFI / JSON; a malformed entry must not crash the daemon
    /// loader.
    #[test]
    fn try_add_rule_rejects_level_out_of_range() {
        let policy = SubnetPolicy::new();
        let err = policy
            .try_add_rule(SubnetRule::new("region:", 4).map("us", 1))
            .unwrap_err();
        assert!(
            matches!(err, SubnetError::LevelOutOfRange { got: 4 }),
            "expected LevelOutOfRange{{got: 4}}, got {:?}",
            err
        );
    }

    #[test]
    fn try_add_rule_accepts_max_level() {
        let policy = SubnetPolicy::new();
        // Level 3 is the highest valid level (0..=3).
        policy
            .try_add_rule(SubnetRule::new("level3:", 3).map("x", 1))
            .expect("level=3 must be accepted (boundary)");
    }

    /// Zero `level_value` must surface as `Err(...)`,
    /// not panic.
    #[test]
    fn try_map_rejects_reserved_zero() {
        let rule = SubnetRule::new("region:", 0);
        let err = rule.try_map("us", 0).unwrap_err();
        assert!(
            matches!(err, SubnetError::LevelValueReserved),
            "expected LevelValueReserved, got {:?}",
            err
        );
    }

    #[test]
    fn try_map_accepts_one() {
        // 1 is the lowest non-reserved level value.
        SubnetRule::new("region:", 0)
            .try_map("us", 1)
            .expect("level_value=1 must be accepted (boundary)");
    }
}
