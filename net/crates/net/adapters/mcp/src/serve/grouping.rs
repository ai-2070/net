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
    /// Lowercased searchable text (name + description) from *every* provider.
    /// The fingerprint intentionally ignores name/description, so equivalent
    /// providers can carry divergent text; searching against all of it means a
    /// query that matches a non-primary provider is not dropped after collapse.
    search_terms: Vec<String>,
}

impl CapabilityGroup {
    /// The primary (preferred) provider — the lowest node id, for determinism.
    pub fn primary(&self) -> u64 {
        match self.providers.first() {
            Some(&node) => node,
            // Unreachable: `group_capabilities` only ever creates a group from
            // at least one provider. Fail loud rather than return a bogus `0`
            // (a valid node id) that would silently misroute.
            None => panic!("CapabilityGroup has no providers (invariant violated)"),
        }
    }

    /// Providers other than `exclude`, in order — the failover candidates.
    pub fn others(&self, exclude: u64) -> Vec<u64> {
        self.providers
            .iter()
            .copied()
            .filter(|&n| n != exclude)
            .collect()
    }

    /// Does `query_lower` (already lowercased) match this capability? Matches
    /// the tool id (shared by every provider) or any provider's name/description.
    pub fn matches_query(&self, query_lower: &str) -> bool {
        self.capability.to_lowercase().contains(query_lower)
            || self.search_terms.iter().any(|t| t.contains(query_lower))
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
///
/// It also folds in the two collapse-gating fields [`is_collapsible`] checks —
/// `substitutability` and `credential_status`. Grouping is unaffected (every
/// collapsible tool shares the same values, `provider_equivalent` + `none`, so
/// they still fingerprint together), but it closes a failover hole: the
/// demand-side gateway remembers the *primary's* fingerprint unconditionally
/// and fails over to any collapsible candidate whose fingerprint matches. If
/// the fingerprint ignored these fields, a credentialed or provider-local
/// primary that merely shares schemas with a collapsible candidate would match
/// — routing a call the operator never declared interchangeable. Including them
/// makes the fingerprint carry the *full* equivalence contract, so a
/// cross-class match is impossible by construction.
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
    info.substitutability.hash(&mut hasher);
    info.credential_status.hash(&mut hasher);
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
///
/// `collapse` gates cross-provider merging. When `false` (the demand side's
/// default) every `(provider, tool)` stays provider-local, so an equivalent
/// capability offered by several nodes is shown — and pinned/invoked — per
/// node, with its real provider id visible. Collapsing is opt-in because
/// equivalence is proven only from *wire-declared* attributes a peer controls
/// (`substitutability`, `credential_status`, schema): on a multi-identity mesh
/// a hostile co-tenant could forge a matching fingerprint and, as the lowest
/// node id, become a group's representative — the capability the operator sees
/// and pins. Merging is safe only when the operator asserts the mesh's peers
/// are trustworthy-equivalent (see `MeshGateway::trust_equivalent_providers`).
///
/// The representative `info` and the `(node, tool_id)` dedup keep the FIRST
/// descriptor seen for a key. Upstream (`mesh_gateway::search`) flattens each
/// provider's describe catalog, and a wrap catalog has exactly one entry per
/// `tool_id`, so a given `(node, tool_id)` appears at most once — the dedup
/// therefore never discards a genuinely-different descriptor. Every provider's
/// searchable text is still accumulated so the query filter sees all of it.
pub fn group_capabilities(
    discovered: Vec<(u64, BridgedToolInfo)>,
    collapse: bool,
) -> Vec<CapabilityGroup> {
    let mut groups: BTreeMap<GroupKey, CapabilityGroup> = BTreeMap::new();
    for (node, info) in discovered {
        let key = if collapse && is_collapsible(&info) {
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
        // Capture this provider's searchable text before `info` moves into the
        // group's representative descriptor on first insert.
        let terms = [
            info.name.to_lowercase(),
            info.description.clone().unwrap_or_default().to_lowercase(),
        ];
        let entry = groups.entry(key).or_insert_with(|| CapabilityGroup {
            capability: info.tool_id.clone(),
            providers: Vec::new(),
            info,
            search_terms: Vec::new(),
        });
        for term in terms {
            if !term.is_empty() && !entry.search_terms.contains(&term) {
                entry.search_terms.push(term);
            }
        }
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
        let groups = group_capabilities(discovered, true);
        assert_eq!(groups.len(), 1, "one logical capability: {groups:?}");
        assert_eq!(groups[0].capability, "echo");
        assert_eq!(groups[0].providers, vec![10, 20]);
        assert_eq!(groups[0].primary(), 10);
        assert_eq!(groups[0].others(10), vec![20]);
    }

    #[test]
    fn collapse_disabled_keeps_equivalent_providers_separate() {
        // F2: with collapse OFF (the demand side's default) even provably-
        // equivalent providers stay provider-local, so a peer that forged a
        // matching fingerprint cannot become the representative of the
        // operator's capability — each provider is shown with its own node id.
        let discovered = vec![
            (10, info("echo", true, "none", echo_schema())),
            (20, info("echo", true, "none", echo_schema())),
        ];
        let groups = group_capabilities(discovered, false);
        assert_eq!(
            groups.len(),
            2,
            "no cross-provider merge when collapse is off"
        );
        assert!(groups.iter().all(|g| g.providers.len() == 1));
        assert_eq!(groups[0].providers, vec![10]);
        assert_eq!(groups[1].providers, vec![20]);
    }

    #[test]
    fn provider_local_tools_never_collapse() {
        // Not substitutable → two separate provider-local entries even under the
        // same name (filesystem-class tools stay provider-local forever).
        let discovered = vec![
            (10, info("fs.read", false, "none", echo_schema())),
            (20, info("fs.read", false, "none", echo_schema())),
        ];
        let groups = group_capabilities(discovered, true);
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
        let groups = group_capabilities(discovered, true);
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
        let groups = group_capabilities(vec![(10, a), (20, b)], true);
        assert_eq!(
            groups.len(),
            2,
            "mismatched schemas are distinct capabilities"
        );
    }

    #[test]
    fn fingerprint_separates_collapse_classes_even_with_identical_schemas() {
        // The failover invariant: the demand side remembers the primary's
        // fingerprint unconditionally and matches collapsible candidates by it.
        // A credentialed or provider-local primary that merely shares schemas
        // with a collapsible candidate must NOT fingerprint-match it, or it
        // would fail over to a provider it was never grouped with.
        let collapsible = info("echo", true, "none", echo_schema());
        let provider_local = info("echo", false, "none", echo_schema());
        let credentialed = info("echo", true, "credentialed", echo_schema());

        assert_ne!(
            descriptor_fingerprint(&collapsible),
            descriptor_fingerprint(&provider_local),
            "a provider-local tool must not match a collapsible one",
        );
        assert_ne!(
            descriptor_fingerprint(&collapsible),
            descriptor_fingerprint(&credentialed),
            "a credentialed tool must not match an uncredentialed one",
        );
        assert_ne!(
            descriptor_fingerprint(&provider_local),
            descriptor_fingerprint(&credentialed),
            "the two non-collapsible classes are themselves distinct",
        );
    }

    #[test]
    fn fingerprint_still_matches_across_equivalent_providers() {
        // The other half of the invariant: folding the collapse-gating fields
        // into the fingerprint must NOT stop genuinely-equivalent providers from
        // matching — two collapsible tools with the same contract still collapse.
        let a = info("echo", true, "none", echo_schema());
        let b = info("echo", true, "none", echo_schema());
        assert_eq!(
            descriptor_fingerprint(&a),
            descriptor_fingerprint(&b),
            "equivalent collapsible providers keep one fingerprint",
        );
        // And end-to-end: they still merge into a single group.
        let groups = group_capabilities(vec![(10, a), (20, b)], true);
        assert_eq!(groups.len(), 1, "grouping behavior is unchanged");
        assert_eq!(groups[0].providers, vec![10, 20]);
    }

    #[test]
    fn a_provider_only_appears_once_per_group() {
        // Defensive: the same node discovered twice for one capability dedups.
        let discovered = vec![
            (10, info("echo", true, "none", echo_schema())),
            (10, info("echo", true, "none", echo_schema())),
        ];
        let groups = group_capabilities(discovered, true);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].providers, vec![10]);
    }

    fn info_desc(tool: &str, desc: &str) -> BridgedToolInfo {
        let mut i = info(tool, true, "none", echo_schema());
        i.description = Some(desc.to_string());
        i
    }

    #[test]
    fn query_matches_a_non_primary_providers_description_after_collapse() {
        // Two equivalent providers (same schema → they collapse) with DIFFERENT
        // descriptions. A query matching only the non-primary's description must
        // still match the collapsed group — the fingerprint ignores description,
        // so searchable text is accumulated from every provider.
        let discovered = vec![
            (10, info_desc("echo", "the primary blurb")),
            (20, info_desc("echo", "handles zebra requests")),
        ];
        let groups = group_capabilities(discovered, true);
        assert_eq!(groups.len(), 1, "still collapses despite divergent text");
        assert_eq!(groups[0].providers, vec![10, 20]);
        assert!(
            groups[0].matches_query("zebra"),
            "matches non-primary description"
        );
        assert!(
            groups[0].matches_query("primary"),
            "matches primary description"
        );
        assert!(
            groups[0].matches_query("echo"),
            "matches the shared tool id"
        );
        assert!(!groups[0].matches_query("giraffe"), "no false positive");
    }

    #[test]
    #[should_panic(expected = "no providers")]
    fn primary_fails_loud_on_an_empty_group() {
        // The non-empty invariant is enforced at construction by
        // `group_capabilities`; if it is ever violated, `primary` must fail
        // loud rather than return a bogus provider `0`.
        let group = CapabilityGroup {
            capability: "x".to_string(),
            providers: Vec::new(),
            info: info("x", true, "none", echo_schema()),
            search_terms: Vec::new(),
        };
        let _ = group.primary();
    }
}
