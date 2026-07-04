//! Duplicate grouping + collapse (`MCP_BRIDGE_PLAN.md` Phase 4).
//!
//! When the same capability is wrapped on several providers, the demand side
//! collapses the interchangeable ones into a single logical capability, so the
//! model sees one tool with multiple providers behind it and invoke can fail
//! over between them. Collapse is deliberately conservative — it happens ONLY
//! when the operator opted in AND the providers are provably interchangeable
//! AND there is no cross-account risk:
//!
//! - `substitutability == provider_equivalent` — the `net wrap --substitutable`
//!   opt-in. A provider-local tool (filesystem-class) never collapses.
//! - identical **descriptor fingerprint** (tool id + compat tier + input/output
//!   schema) — a different contract is a different capability, even under the
//!   same name.
//! - **credential-compatible**: v0 collapses only `credential_status == none`
//!   (stateless — no account that could differ between providers), so
//!   *cross-account collapse is impossible by construction*. Collapsing
//!   same-account credentialed tools needs the privacy-safe `credential_context`
//!   equivalence class (a later refinement); until then a credentialed tool
//!   stays provider-local.
//!
//! Everything here is pure — the mesh-facing gateway feeds it the discovered
//! `(provider, descriptor)` pairs and consumes the resulting groups.

use std::collections::BTreeMap;

use crate::bridge::BridgedToolInfo;

/// One logical capability after grouping: a single provider (any non-collapsible
/// tool, the common case) or several interchangeable providers.
#[derive(Debug, Clone, PartialEq)]
pub struct CapabilityGroup {
    /// The capability (tool) id.
    pub capability: String,
    /// Provider node ids serving this logical capability, sorted ascending —
    /// the first is the primary (deterministic) choice. Never empty.
    pub providers: Vec<u64>,
    /// A representative descriptor. Within a group the providers are
    /// interchangeable, so any one describes the whole group.
    pub info: BridgedToolInfo,
}

impl CapabilityGroup {
    /// The primary (preferred) provider — the lowest node id, for determinism.
    pub fn primary(&self) -> u64 {
        // `providers` is non-empty by construction (a group is created from at
        // least one discovered provider) and sorted.
        self.providers.first().copied().unwrap_or_default()
    }

    /// Providers other than `exclude`, in order — the failover candidates.
    pub fn others(&self, exclude: u64) -> Vec<u64> {
        self.providers
            .iter()
            .copied()
            .filter(|&n| n != exclude)
            .collect()
    }
}

/// Is a discovered tool eligible to collapse across providers? Conservative:
/// operator-declared substitutable AND uncredentialed (no account to differ).
pub fn is_collapsible(info: &BridgedToolInfo) -> bool {
    info.substitutability == "provider_equivalent" && info.credential_status == "none"
}

/// A stable equivalence key for a tool's observable contract. Two providers'
/// tools collapse only when these fingerprints match. Excludes the provider and
/// the server version — two providers on slightly different builds with the
/// same contract are still interchangeable — and includes what a caller
/// observably depends on (id, tier, schemas). The schemas serialize
/// deterministically (serde_json orders object keys), so the fingerprint is
/// stable across providers announcing the same tool.
pub fn descriptor_fingerprint(info: &BridgedToolInfo) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    info.tool_id.hash(&mut hasher);
    info.compat_tier.hash(&mut hasher);
    info.input_schema.to_string().hash(&mut hasher);
    info.output_schema
        .as_ref()
        .map(|v| v.to_string())
        .hash(&mut hasher);
    hasher.finish()
}

/// The grouping key: collapsible tools key on `(capability, fingerprint)` so
/// equivalent providers merge; anything else keys uniquely on
/// `(capability, provider)` so it stays provider-local.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum GroupKey {
    Equivalent {
        capability: String,
        fingerprint: u64,
    },
    ProviderLocal {
        capability: String,
        provider: u64,
    },
}

/// Collapse the discovered `(provider, descriptor)` pairs into logical
/// capabilities. The result is deterministic (sorted keys + sorted providers).
pub fn group_capabilities(discovered: Vec<(u64, BridgedToolInfo)>) -> Vec<CapabilityGroup> {
    let mut groups: BTreeMap<GroupKey, CapabilityGroup> = BTreeMap::new();
    for (node, info) in discovered {
        let key = if is_collapsible(&info) {
            GroupKey::Equivalent {
                capability: info.tool_id.clone(),
                fingerprint: descriptor_fingerprint(&info),
            }
        } else {
            GroupKey::ProviderLocal {
                capability: info.tool_id.clone(),
                provider: node,
            }
        };
        let entry = groups.entry(key).or_insert_with(|| CapabilityGroup {
            capability: info.tool_id.clone(),
            providers: Vec::new(),
            info,
        });
        if !entry.providers.contains(&node) {
            entry.providers.push(node);
        }
    }
    let mut out: Vec<CapabilityGroup> = groups.into_values().collect();
    for group in &mut out {
        group.providers.sort_unstable();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn info(
        tool: &str,
        substitutable: bool,
        cred: &str,
        schema: serde_json::Value,
    ) -> BridgedToolInfo {
        BridgedToolInfo {
            tool_id: tool.to_string(),
            name: tool.to_string(),
            description: None,
            input_schema: schema,
            output_schema: None,
            version: "1".to_string(),
            compat_tier: "mcp_bridge".to_string(),
            credential_status: cred.to_string(),
            substitutability: if substitutable {
                "provider_equivalent".to_string()
            } else {
                "provider_local".to_string()
            },
            visibility: "owner_only".to_string(),
            invocation_scope: "same_root_identity".to_string(),
        }
    }

    fn echo_schema() -> serde_json::Value {
        json!({ "type": "object", "properties": { "message": { "type": "string" } } })
    }

    #[test]
    fn equivalent_uncredentialed_providers_collapse_into_one_group() {
        let discovered = vec![
            (10, info("echo", true, "none", echo_schema())),
            (20, info("echo", true, "none", echo_schema())),
        ];
        let groups = group_capabilities(discovered);
        assert_eq!(groups.len(), 1, "one logical capability: {groups:?}");
        assert_eq!(groups[0].capability, "echo");
        assert_eq!(groups[0].providers, vec![10, 20]);
        assert_eq!(groups[0].primary(), 10);
        assert_eq!(groups[0].others(10), vec![20]);
    }

    #[test]
    fn provider_local_tools_never_collapse() {
        // Not substitutable → two separate provider-local entries even under the
        // same name (filesystem-class tools stay provider-local forever).
        let discovered = vec![
            (10, info("fs.read", false, "none", echo_schema())),
            (20, info("fs.read", false, "none", echo_schema())),
        ];
        let groups = group_capabilities(discovered);
        assert_eq!(groups.len(), 2);
        assert!(groups.iter().all(|g| g.providers.len() == 1));
    }

    #[test]
    fn credentialed_tools_never_collapse_cross_account() {
        // The load-bearing safety property: two providers of the same
        // substitutable tool but with credentials (possibly different accounts)
        // must NOT collapse — cross-account collapse is impossible here.
        let discovered = vec![
            (
                10,
                info("github.create_issue", true, "credentialed", echo_schema()),
            ),
            (
                20,
                info("github.create_issue", true, "credentialed", echo_schema()),
            ),
        ];
        let groups = group_capabilities(discovered);
        assert_eq!(groups.len(), 2, "credentialed tools stay provider-local");
        assert!(groups.iter().all(|g| g.providers.len() == 1));
    }

    #[test]
    fn different_contracts_do_not_collapse_even_under_one_name() {
        let a = info(
            "echo",
            true,
            "none",
            json!({ "type": "object", "properties": { "a": {} } }),
        );
        let b = info(
            "echo",
            true,
            "none",
            json!({ "type": "object", "properties": { "b": {} } }),
        );
        assert_ne!(descriptor_fingerprint(&a), descriptor_fingerprint(&b));
        let groups = group_capabilities(vec![(10, a), (20, b)]);
        assert_eq!(
            groups.len(),
            2,
            "mismatched schemas are distinct capabilities"
        );
    }

    #[test]
    fn a_provider_only_appears_once_per_group() {
        // Defensive: the same node discovered twice for one capability dedups.
        let discovered = vec![
            (10, info("echo", true, "none", echo_schema())),
            (10, info("echo", true, "none", echo_schema())),
        ];
        let groups = group_capabilities(discovered);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].providers, vec![10]);
    }
}
