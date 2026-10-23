//! The daemon's subnet policy: `[[subnet_rule]]` blocks in the config.
//!
//! Slice Q0 of `docs/internal/plans/AGGREGATOR_MULTI_HOST_PLAN.md` (D4).
//!
//! The policy classifies peers into subnets from their signed capability
//! tags. The mesh records a direct peer's subnet only when the node has a
//! policy installed, so summary visibility (Q1) and the `fold.query`
//! access check (Q2) both depend on it.
//!
//! ```toml
//! [[subnet_rule]]
//! tag_prefix = "region:"
//! level = 0
//! values = { "eu" = 1, "us" = 2 }
//! ```
//!
//! # Rule order is part of the policy
//!
//! `SubnetPolicy` evaluates rules in declaration order, and when two rules
//! write the same level the later one wins. Reordering rules can change the
//! subnet a node is assigned, so the fingerprint hashes the rules in
//! declaration order. Only each rule's `values` map, which has no order, is
//! canonicalized (by key).

use std::collections::BTreeMap;

use serde::Deserialize;

use net::adapter::net::{SubnetPolicy, SubnetRule};

/// One `[[subnet_rule]]` block, as written in the config.
///
/// Kept separately from the built [`SubnetPolicy`], which does not expose
/// its rules, so the daemon can fingerprint exactly what the operator
/// configured.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct SubnetRuleConfig {
    /// Tag prefix the rule matches, e.g. `"region:"`.
    pub tag_prefix: String,
    /// Hierarchy level the rule writes (0–3).
    pub level: u8,
    /// Tag value (the part after the prefix) → the byte written at
    /// `level`. Zero is reserved and refused.
    pub values: BTreeMap<String, u8>,
}

/// Why a `[[subnet_rule]]` list was refused at boot.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SubnetRuleError {
    #[error("subnet_rule[{index}] ({prefix:?}): {error}")]
    Invalid {
        index: usize,
        prefix: String,
        error: String,
    },
    #[error("subnet_rule[{index}] ({prefix:?}) has no `values`; it can never match")]
    EmptyValues { index: usize, prefix: String },
    #[error(
        "the subnet rules can never assign a non-global subnet, so every peer \
         would classify as GLOBAL; check the rules' `values`"
    )]
    NeverAssigns,
}

/// Build the policy the daemon installs on its `MeshNode`.
///
/// Returns `Ok(None)` for an empty rule list, so a config without
/// `[[subnet_rule]]` boots exactly as before (no policy installed).
pub(crate) fn build_policy(
    rules: &[SubnetRuleConfig],
) -> Result<Option<SubnetPolicy>, SubnetRuleError> {
    if rules.is_empty() {
        return Ok(None);
    }
    let mut policy = SubnetPolicy::new();
    for (index, rule) in rules.iter().enumerate() {
        let invalid = |error: String| SubnetRuleError::Invalid {
            index,
            prefix: rule.tag_prefix.clone(),
            error,
        };
        if rule.values.is_empty() {
            return Err(SubnetRuleError::EmptyValues {
                index,
                prefix: rule.tag_prefix.clone(),
            });
        }
        let mut built = SubnetRule::new(rule.tag_prefix.clone(), rule.level);
        for (value, level_value) in &rule.values {
            built = built
                .try_map(value.clone(), *level_value)
                .map_err(|e| invalid(format!("value {value:?}: {e}")))?;
        }
        policy = policy
            .try_add_rule(built)
            .map_err(|e| invalid(e.to_string()))?;
    }
    if !policy.can_assign_non_global() {
        return Err(SubnetRuleError::NeverAssigns);
    }
    Ok(Some(policy))
}

/// Domain separation for the fingerprint, so it can never collide with
/// another BLAKE3 use of the same bytes. Bump the version if the encoding
/// below changes.
const FINGERPRINT_CONTEXT: &str = "net-aggregator-daemon 2026-10-23 subnet-policy fingerprint v1";

/// Fingerprint of a rule list, in declaration order.
///
/// Two daemons that classify differently must not share a fingerprint, so
/// rule order is hashed as written (later rule wins on a shared level).
/// Each rule's `values` map is hashed sorted by key, because its order has
/// no meaning. Every variable-length field is length-prefixed, so no two
/// rule lists encode to the same bytes.
pub(crate) fn policy_fingerprint(rules: &[SubnetRuleConfig]) -> [u8; 32] {
    let mut h = blake3::Hasher::new_derive_key(FINGERPRINT_CONTEXT);
    let put_str = |h: &mut blake3::Hasher, s: &str| {
        h.update(&(s.len() as u64).to_le_bytes());
        h.update(s.as_bytes());
    };
    h.update(&(rules.len() as u64).to_le_bytes());
    for rule in rules {
        put_str(&mut h, &rule.tag_prefix);
        h.update(&[rule.level]);
        h.update(&(rule.values.len() as u64).to_le_bytes());
        // `BTreeMap` iterates in key order: the canonical form.
        for (value, level_value) in &rule.values {
            put_str(&mut h, value);
            h.update(&[*level_value]);
        }
    }
    *h.finalize().as_bytes()
}

/// Lowercase hex, for logs and operator output.
pub(crate) fn fingerprint_hex(fp: &[u8; 32]) -> String {
    fp.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use net::adapter::net::SubnetId;

    /// Parse `[[subnet_rule]]` blocks the way the daemon config does.
    fn parse(toml_src: &str) -> Vec<SubnetRuleConfig> {
        #[derive(Deserialize)]
        struct Wrapper {
            #[serde(default, rename = "subnet_rule")]
            rules: Vec<SubnetRuleConfig>,
        }
        toml::from_str::<Wrapper>(toml_src).expect("parse").rules
    }

    #[test]
    fn no_rules_installs_no_policy() {
        assert!(build_policy(&[]).expect("empty is fine").is_none());
    }

    #[test]
    fn rules_build_a_policy_that_classifies_from_tags() {
        let rules = parse(
            r#"
            [[subnet_rule]]
            tag_prefix = "region:"
            level = 0
            values = { "eu" = 1, "us" = 2 }

            [[subnet_rule]]
            tag_prefix = "rack:"
            level = 1
            values = { "a" = 7 }
            "#,
        );
        let policy = build_policy(&rules).expect("valid").expect("some");
        assert_eq!(
            policy.assign_from_rendered_tags(&["region:eu", "rack:a"]),
            SubnetId::new(&[1, 7])
        );
        assert_eq!(
            policy.assign_from_rendered_tags(&["region:us"]),
            SubnetId::new(&[2])
        );
        assert_eq!(
            policy.assign_from_rendered_tags(&["unrelated"]),
            SubnetId::GLOBAL
        );
    }

    #[test]
    fn value_map_order_does_not_change_the_fingerprint() {
        let a = parse(
            r#"
            [[subnet_rule]]
            tag_prefix = "region:"
            level = 0
            values = { "eu" = 1, "us" = 2, "ap" = 3 }
            "#,
        );
        let b = parse(
            r#"
            [[subnet_rule]]
            tag_prefix = "region:"
            level = 0
            values = { "ap" = 3, "us" = 2, "eu" = 1 }
            "#,
        );
        assert_eq!(policy_fingerprint(&a), policy_fingerprint(&b));
    }

    /// Review 2, finding 2: reversing two conflicting same-level rules
    /// changes BOTH what a node classifies as and the fingerprint. A
    /// fingerprint that sorted the rules would miss this.
    #[test]
    fn reversing_conflicting_same_level_rules_changes_classification_and_fingerprint() {
        let forward = parse(
            r#"
            [[subnet_rule]]
            tag_prefix = "region:"
            level = 0
            values = { "eu" = 1 }

            [[subnet_rule]]
            tag_prefix = "zone:"
            level = 0
            values = { "north" = 9 }
            "#,
        );
        let reversed: Vec<SubnetRuleConfig> = forward.iter().rev().cloned().collect();
        let tags = ["region:eu", "zone:north"];

        let f = build_policy(&forward).expect("valid").expect("some");
        let r = build_policy(&reversed).expect("valid").expect("some");
        // Later rule wins on the shared level 0.
        assert_eq!(f.assign_from_rendered_tags(&tags), SubnetId::new(&[9]));
        assert_eq!(r.assign_from_rendered_tags(&tags), SubnetId::new(&[1]));
        assert_ne!(policy_fingerprint(&forward), policy_fingerprint(&reversed));
    }

    #[test]
    fn distinct_policies_have_distinct_fingerprints() {
        let base = parse(
            r#"
            [[subnet_rule]]
            tag_prefix = "region:"
            level = 0
            values = { "eu" = 1 }
            "#,
        );
        let mut other_level = base.clone();
        other_level[0].level = 1;
        let mut other_value = base.clone();
        other_value[0].values.insert("eu".into(), 2);
        let mut other_prefix = base.clone();
        other_prefix[0].tag_prefix = "regio".into();
        other_prefix[0].values = BTreeMap::from([("n:eu".into(), 1)]);
        let fps = [
            policy_fingerprint(&base),
            policy_fingerprint(&other_level),
            policy_fingerprint(&other_value),
            policy_fingerprint(&other_prefix),
            policy_fingerprint(&[]),
        ];
        for i in 0..fps.len() {
            for j in (i + 1)..fps.len() {
                assert_ne!(fps[i], fps[j], "fingerprints {i} and {j} collide");
            }
        }
    }

    #[test]
    fn invalid_rules_are_refused_with_their_index() {
        let level_out_of_range = parse(
            r#"
            [[subnet_rule]]
            tag_prefix = "region:"
            level = 4
            values = { "eu" = 1 }
            "#,
        );
        assert!(matches!(
            build_policy(&level_out_of_range),
            Err(SubnetRuleError::Invalid { index: 0, .. })
        ));

        let reserved_zero = parse(
            r#"
            [[subnet_rule]]
            tag_prefix = "region:"
            level = 0
            values = { "eu" = 1 }

            [[subnet_rule]]
            tag_prefix = "rack:"
            level = 1
            values = { "a" = 0 }
            "#,
        );
        assert!(matches!(
            build_policy(&reserved_zero),
            Err(SubnetRuleError::Invalid { index: 1, .. })
        ));

        let empty = parse(
            r#"
            [[subnet_rule]]
            tag_prefix = "region:"
            level = 0
            values = {}
            "#,
        );
        assert!(matches!(
            build_policy(&empty),
            Err(SubnetRuleError::EmptyValues { index: 0, .. })
        ));
    }

    #[test]
    fn unknown_rule_keys_are_refused() {
        #[derive(Deserialize)]
        struct Wrapper {
            #[serde(rename = "subnet_rule")]
            _rules: Vec<SubnetRuleConfig>,
        }
        let typo = r#"
            [[subnet_rule]]
            tag_prefix = "region:"
            level = 0
            value = { "eu" = 1 }
        "#;
        assert!(toml::from_str::<Wrapper>(typo).is_err());
    }
}
