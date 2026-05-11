//! Greedy-LRU admission decision — a pure function the runtime
//! consults per inbound event. Decoupled from the async runtime,
//! the proximity graph, and the per-channel cache state so it
//! can be exhaustively unit-tested.
//!
//! The runtime layer above this is responsible for the impure
//! axes (proximity RTT lookup, storage budget, colocation
//! target-held check); this function covers the pure axes
//! (scope match, intent match, colocation gate per policy) and
//! returns a typed verdict the runtime maps to a cache-write
//! decision.
//!
//! Locked semantics:
//!
//! - **Scope match.** Empty `cfg.scopes` admits regardless. Non-
//!   empty `cfg.scopes` requires the chain to advertise at least
//!   one `scope:<label>` tag whose label matches an entry in
//!   `cfg.scopes`.
//! - **Intent match.** Mirrors `StandardPlacement`'s
//!   [`IntentMatchPolicy`] semantics:
//!   - `Disabled` — axis disabled; always admits.
//!   - `Strict` — the chain's `metadata.intent` value names an
//!     intent in the registry; ALL of that intent's required
//!     capabilities must evaluate against the local capability
//!     set. Unknown intent or no declared intent → admit
//!     (forward-compat).
//!   - `AnyOfLocalCapabilities` — at least one registered intent's
//!     full requirement-list passes against the local capability
//!     set ("I'm generally useful for some workload"). Empty
//!     registry → admit (per `score_intent_axis`'s CR-22 fix).
//! - **Colocation gate.** Reads the chain's `metadata.colocate-with`
//!   / `metadata.colocate-with-strict` values:
//!   - `Ignore` policy — colocation hints don't gate admission.
//!   - `SoftPreference` policy — `colocate-with` is non-blocking
//!     (no admission impact; affects scoring in the runtime, not
//!     here). `colocate-with-strict` STILL hard-rejects when the
//!     target chain isn't held.
//!   - `StrictRequired` policy — both keys hard-reject when the
//!     target chain isn't held.

use crate::adapter::net::behavior::capability::CapabilitySet;
use crate::adapter::net::behavior::placement::{
    ColocationPolicy, IntentMatchPolicy, IntentRegistry, PlacementMetadataKeys,
};
use crate::adapter::net::behavior::predicate::EvalContext;
use crate::adapter::net::behavior::tag::Tag;

use super::config::GreedyConfig;

/// Outcome of an admission decision. Reject variants name the
/// axis that triggered the rejection so the runtime can route
/// the right metric bump (`dataforts_greedy_admit_rejected_total{reason}`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmissionVerdict {
    /// All gates passed — runtime may proceed with the cache
    /// write (subject to the impure axes the runtime owns:
    /// proximity, storage budget).
    Admit,
    /// Chain's scope tags didn't intersect with any of
    /// `cfg.scopes`.
    RejectScope,
    /// Intent axis rejected. `cfg.intent_match == Strict`: the
    /// chain's declared intent's required-caps list didn't
    /// evaluate against local. `AnyOfLocalCapabilities`: no
    /// registered intent's required-caps list evaluated against
    /// local.
    RejectIntent,
    /// Colocation gate rejected. The chain carried a strict
    /// colocation hint (or `cfg.colocation_policy ==
    /// StrictRequired` with a soft hint) and the local node
    /// doesn't hold the target chain.
    RejectColocation,
}

/// Inputs to [`should_admit`]. Grouped into a struct so future
/// axes can be added without breaking call sites; the pure-
/// function contract is preserved (all inputs are explicit).
#[derive(Debug, Clone, Copy)]
pub struct AdmissionInputs<'a> {
    /// Capability set the chain advertises (its tags + metadata).
    pub chain_caps: &'a CapabilitySet,
    /// Local node's advertised capability set — what we can do.
    pub local_caps: &'a CapabilitySet,
    /// Greedy configuration (locked at `Mesh::enable_greedy_dataforts`).
    pub config: &'a GreedyConfig,
    /// Intent registry for `Strict` / `AnyOfLocalCapabilities` lookup.
    /// Typically `IntentRegistry::defaults()` augmented with
    /// application-registered intents; passed in explicitly so
    /// admission stays pure / testable.
    pub intent_registry: &'a IntentRegistry,
    /// Metadata key names; `Default` uses the substrate's
    /// `"intent"` / `"colocate-with"` / `"colocate-with-strict"`.
    /// Applications with legacy conventions override.
    pub metadata_keys: &'a PlacementMetadataKeys,
    /// Whether the local node currently holds the chain named by
    /// the chain's `colocate-with` / `colocate-with-strict`
    /// metadata. `None` means the chain has no colocation hint OR
    /// the runtime hasn't resolved the target yet (treat as
    /// "target not held" for the gate decision so a missing
    /// resolution doesn't accidentally relax `StrictRequired`).
    pub colocation_target_held: Option<bool>,
}

/// Run the admission decision against the supplied inputs.
///
/// Pure function — no I/O, no async, no global state lookups.
/// Returns the first axis that rejects in evaluation order
/// (scope → intent → colocation); the runtime maps the verdict
/// to a cache-write decision plus the corresponding rejection
/// metric.
pub fn should_admit(inputs: &AdmissionInputs<'_>) -> AdmissionVerdict {
    if !passes_scope_gate(inputs.chain_caps, &inputs.config.scopes) {
        return AdmissionVerdict::RejectScope;
    }
    if !passes_intent_gate(
        inputs.chain_caps,
        inputs.local_caps,
        inputs.config.intent_match.clone(),
        inputs.intent_registry,
        inputs.metadata_keys,
    ) {
        return AdmissionVerdict::RejectIntent;
    }
    if !passes_colocation_gate(
        inputs.chain_caps,
        inputs.config.colocation_policy,
        inputs.metadata_keys,
        inputs.colocation_target_held,
    ) {
        return AdmissionVerdict::RejectColocation;
    }
    AdmissionVerdict::Admit
}

/// Scope gate. Empty `configured_scopes` admits regardless;
/// non-empty requires the chain to advertise at least one
/// `scope:<label>` reserved-prefix tag with a matching label.
fn passes_scope_gate(chain_caps: &CapabilitySet, configured_scopes: &[super::ScopeLabel]) -> bool {
    if configured_scopes.is_empty() {
        return true;
    }
    // RESERVED_PREFIXES carries the trailing colon (`"scope:"`).
    // The reserved tag's `prefix` field stores it verbatim, so the
    // canonical match is on `"scope:"` not `"scope"`. The tag's
    // body is the bare label.
    chain_caps.tags.iter().any(|tag| match tag {
        Tag::Reserved { prefix, body } if prefix == "scope:" => {
            configured_scopes.iter().any(|s| s.as_str() == body)
        }
        _ => false,
    })
}

/// Intent gate. Mirrors `StandardPlacement::score_intent_axis`
/// semantics, returning a bool instead of an [0.0, 1.0] score.
fn passes_intent_gate(
    chain_caps: &CapabilitySet,
    local_caps: &CapabilitySet,
    policy: IntentMatchPolicy,
    registry: &IntentRegistry,
    metadata_keys: &PlacementMetadataKeys,
) -> bool {
    let local_tags: Vec<Tag> = local_caps.tags.iter().cloned().collect();
    let local_ctx = EvalContext::new(&local_tags, &local_caps.metadata);
    match policy {
        IntentMatchPolicy::Disabled => true,
        IntentMatchPolicy::Strict => {
            let Some(intent) = chain_caps.metadata.get(&metadata_keys.intent) else {
                return true;
            };
            let Some(reqs) = registry.lookup(intent) else {
                return true; // forward-compat — unknown intent passes
            };
            reqs.iter().all(|req| req.evaluate(&local_ctx))
        }
        IntentMatchPolicy::AnyOfLocalCapabilities => {
            if registry.is_empty() {
                return true;
            }
            registry
                .iter()
                .any(|(_, reqs)| reqs.iter().all(|req| req.evaluate(&local_ctx)))
        }
    }
}

/// Colocation gate. Per policy:
///
/// - `Ignore` — pass regardless.
/// - `SoftPreference` — only `colocate-with-strict` hard-rejects
///   when target isn't held. `colocate-with` is non-blocking
///   here (the runtime applies a scoring boost elsewhere).
/// - `StrictRequired` — both keys hard-reject when target isn't
///   held.
fn passes_colocation_gate(
    chain_caps: &CapabilitySet,
    policy: ColocationPolicy,
    metadata_keys: &PlacementMetadataKeys,
    target_held: Option<bool>,
) -> bool {
    if matches!(policy, ColocationPolicy::Ignore) {
        return true;
    }
    let has_strict = chain_caps
        .metadata
        .contains_key(&metadata_keys.colocate_with_strict);
    let has_soft = chain_caps
        .metadata
        .contains_key(&metadata_keys.colocate_with);

    // Strict colocate hint blocks regardless of policy (so long as
    // the colocation axis isn't fully disabled, handled above).
    if has_strict && !target_held.unwrap_or(false) {
        return false;
    }
    // Soft colocate hint blocks only when policy escalates it.
    if has_soft
        && matches!(policy, ColocationPolicy::StrictRequired)
        && !target_held.unwrap_or(false)
    {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::adapter::net::behavior::capability::CapabilitySet;
    use crate::adapter::net::behavior::tag::{Tag, TaxonomyAxis};

    fn caps_with_scope(scope: &str) -> CapabilitySet {
        let mut caps = CapabilitySet::default();
        caps.tags.insert(Tag::Reserved {
            prefix: "scope:".to_string(),
            body: scope.to_string(),
        });
        caps
    }

    fn caps_with_intent(intent: &str) -> CapabilitySet {
        let mut caps = CapabilitySet::default();
        caps.metadata
            .insert("intent".to_string(), intent.to_string());
        caps
    }

    fn caps_with_gpu_24gb() -> CapabilitySet {
        let mut caps = CapabilitySet::default();
        caps.tags.insert(Tag::AxisPresent {
            axis: TaxonomyAxis::Hardware,
            key: "gpu".to_string(),
        });
        caps.tags.insert(Tag::AxisValue {
            axis: TaxonomyAxis::Hardware,
            key: "gpu.vram_gb".to_string(),
            value: "24".to_string(),
            separator: crate::adapter::net::behavior::tag::AxisSeparator::Eq,
        });
        caps
    }

    fn caps_with_cpu_only() -> CapabilitySet {
        let mut caps = CapabilitySet::default();
        caps.tags.insert(Tag::AxisValue {
            axis: TaxonomyAxis::Hardware,
            key: "cpu_cores".to_string(),
            value: "16".to_string(),
            separator: crate::adapter::net::behavior::tag::AxisSeparator::Eq,
        });
        caps
    }

    fn metadata_keys() -> PlacementMetadataKeys {
        PlacementMetadataKeys::default()
    }

    // ---- scope gate ----

    #[test]
    fn empty_scope_set_admits_regardless() {
        let chain = caps_with_scope("industrial");
        let local = CapabilitySet::default();
        let registry = IntentRegistry::new();
        let keys = metadata_keys();
        let cfg = GreedyConfig::default().with_intent_match(IntentMatchPolicy::Disabled);
        let inputs = AdmissionInputs {
            chain_caps: &chain,
            local_caps: &local,
            config: &cfg,
            intent_registry: &registry,
            metadata_keys: &keys,
            colocation_target_held: None,
        };
        assert_eq!(should_admit(&inputs), AdmissionVerdict::Admit);
    }

    #[test]
    fn scope_match_admits() {
        let chain = caps_with_scope("industrial");
        let local = CapabilitySet::default();
        let registry = IntentRegistry::new();
        let keys = metadata_keys();
        let cfg = GreedyConfig::default()
            .with_scopes(vec![super::super::ScopeLabel::new("industrial")])
            .with_intent_match(IntentMatchPolicy::Disabled);
        let inputs = AdmissionInputs {
            chain_caps: &chain,
            local_caps: &local,
            config: &cfg,
            intent_registry: &registry,
            metadata_keys: &keys,
            colocation_target_held: None,
        };
        assert_eq!(should_admit(&inputs), AdmissionVerdict::Admit);
    }

    #[test]
    fn scope_miss_rejects() {
        let chain = caps_with_scope("webcam-streams");
        let local = CapabilitySet::default();
        let registry = IntentRegistry::new();
        let keys = metadata_keys();
        let cfg = GreedyConfig::default()
            .with_scopes(vec![super::super::ScopeLabel::new("industrial")])
            .with_intent_match(IntentMatchPolicy::Disabled);
        let inputs = AdmissionInputs {
            chain_caps: &chain,
            local_caps: &local,
            config: &cfg,
            intent_registry: &registry,
            metadata_keys: &keys,
            colocation_target_held: None,
        };
        assert_eq!(should_admit(&inputs), AdmissionVerdict::RejectScope);
    }

    #[test]
    fn chain_with_no_scope_tag_rejects_under_non_empty_scope_set() {
        let chain = CapabilitySet::default();
        let local = CapabilitySet::default();
        let registry = IntentRegistry::new();
        let keys = metadata_keys();
        let cfg = GreedyConfig::default()
            .with_scopes(vec![super::super::ScopeLabel::new("industrial")])
            .with_intent_match(IntentMatchPolicy::Disabled);
        let inputs = AdmissionInputs {
            chain_caps: &chain,
            local_caps: &local,
            config: &cfg,
            intent_registry: &registry,
            metadata_keys: &keys,
            colocation_target_held: None,
        };
        assert_eq!(should_admit(&inputs), AdmissionVerdict::RejectScope);
    }

    // ---- intent gate ----

    #[test]
    fn intent_disabled_admits_regardless_of_local_caps() {
        let chain = caps_with_intent("ml-training");
        let local = caps_with_cpu_only();
        let registry = IntentRegistry::defaults();
        let keys = metadata_keys();
        let cfg = GreedyConfig::default().with_intent_match(IntentMatchPolicy::Disabled);
        let inputs = AdmissionInputs {
            chain_caps: &chain,
            local_caps: &local,
            config: &cfg,
            intent_registry: &registry,
            metadata_keys: &keys,
            colocation_target_held: None,
        };
        assert_eq!(should_admit(&inputs), AdmissionVerdict::Admit);
    }

    #[test]
    fn intent_strict_admits_when_local_satisfies() {
        let chain = caps_with_intent("ml-training");
        let local = caps_with_gpu_24gb();
        let registry = IntentRegistry::defaults();
        let keys = metadata_keys();
        let cfg = GreedyConfig::default().with_intent_match(IntentMatchPolicy::Strict);
        let inputs = AdmissionInputs {
            chain_caps: &chain,
            local_caps: &local,
            config: &cfg,
            intent_registry: &registry,
            metadata_keys: &keys,
            colocation_target_held: None,
        };
        assert_eq!(should_admit(&inputs), AdmissionVerdict::Admit);
    }

    #[test]
    fn intent_strict_rejects_when_local_lacks_required() {
        let chain = caps_with_intent("ml-training");
        let local = caps_with_cpu_only();
        let registry = IntentRegistry::defaults();
        let keys = metadata_keys();
        let cfg = GreedyConfig::default().with_intent_match(IntentMatchPolicy::Strict);
        let inputs = AdmissionInputs {
            chain_caps: &chain,
            local_caps: &local,
            config: &cfg,
            intent_registry: &registry,
            metadata_keys: &keys,
            colocation_target_held: None,
        };
        assert_eq!(should_admit(&inputs), AdmissionVerdict::RejectIntent);
    }

    #[test]
    fn intent_strict_with_no_declared_intent_admits() {
        // Chain with no metadata.intent — Strict treats this as
        // "no constraint" (forward-compat).
        let chain = CapabilitySet::default();
        let local = caps_with_cpu_only();
        let registry = IntentRegistry::defaults();
        let keys = metadata_keys();
        let cfg = GreedyConfig::default().with_intent_match(IntentMatchPolicy::Strict);
        let inputs = AdmissionInputs {
            chain_caps: &chain,
            local_caps: &local,
            config: &cfg,
            intent_registry: &registry,
            metadata_keys: &keys,
            colocation_target_held: None,
        };
        assert_eq!(should_admit(&inputs), AdmissionVerdict::Admit);
    }

    #[test]
    fn intent_strict_with_unknown_intent_admits() {
        // Intent not in registry — Strict passes through.
        let chain = caps_with_intent("custom-not-registered");
        let local = caps_with_cpu_only();
        let registry = IntentRegistry::defaults();
        let keys = metadata_keys();
        let cfg = GreedyConfig::default().with_intent_match(IntentMatchPolicy::Strict);
        let inputs = AdmissionInputs {
            chain_caps: &chain,
            local_caps: &local,
            config: &cfg,
            intent_registry: &registry,
            metadata_keys: &keys,
            colocation_target_held: None,
        };
        assert_eq!(should_admit(&inputs), AdmissionVerdict::Admit);
    }

    #[test]
    fn intent_any_of_admits_when_local_satisfies_some_registered_intent() {
        // Chain declares ml-training but local only has CPU.
        // AnyOfLocalCapabilities only asks "are you capable of
        // SOMETHING in the registry?" — local satisfies cpu-bound
        // (≥4 cores), so admit.
        let chain = caps_with_intent("ml-training");
        let local = caps_with_cpu_only();
        let registry = IntentRegistry::defaults();
        let keys = metadata_keys();
        let cfg =
            GreedyConfig::default().with_intent_match(IntentMatchPolicy::AnyOfLocalCapabilities);
        let inputs = AdmissionInputs {
            chain_caps: &chain,
            local_caps: &local,
            config: &cfg,
            intent_registry: &registry,
            metadata_keys: &keys,
            colocation_target_held: None,
        };
        assert_eq!(should_admit(&inputs), AdmissionVerdict::Admit);
    }

    #[test]
    fn intent_any_of_rejects_when_local_satisfies_no_registered_intent() {
        let chain = caps_with_intent("ml-training");
        let local = CapabilitySet::default(); // nothing
        let registry = IntentRegistry::defaults();
        let keys = metadata_keys();
        let cfg =
            GreedyConfig::default().with_intent_match(IntentMatchPolicy::AnyOfLocalCapabilities);
        let inputs = AdmissionInputs {
            chain_caps: &chain,
            local_caps: &local,
            config: &cfg,
            intent_registry: &registry,
            metadata_keys: &keys,
            colocation_target_held: None,
        };
        assert_eq!(should_admit(&inputs), AdmissionVerdict::RejectIntent);
    }

    #[test]
    fn intent_any_of_admits_when_registry_empty() {
        // CR-22 parity: empty registry → axis-disabled (1.0 in
        // placement; Admit here).
        let chain = caps_with_intent("ml-training");
        let local = CapabilitySet::default();
        let registry = IntentRegistry::new(); // empty
        let keys = metadata_keys();
        let cfg =
            GreedyConfig::default().with_intent_match(IntentMatchPolicy::AnyOfLocalCapabilities);
        let inputs = AdmissionInputs {
            chain_caps: &chain,
            local_caps: &local,
            config: &cfg,
            intent_registry: &registry,
            metadata_keys: &keys,
            colocation_target_held: None,
        };
        assert_eq!(should_admit(&inputs), AdmissionVerdict::Admit);
    }

    // ---- colocation gate ----

    fn caps_with_metadata(pairs: &[(&str, &str)]) -> CapabilitySet {
        let mut caps = CapabilitySet::default();
        for (k, v) in pairs {
            caps.metadata.insert((*k).to_string(), (*v).to_string());
        }
        caps
    }

    fn ignored_intent_cfg() -> GreedyConfig {
        GreedyConfig::default().with_intent_match(IntentMatchPolicy::Disabled)
    }

    #[test]
    fn colocation_ignore_admits_even_with_unheld_strict_target() {
        let chain = caps_with_metadata(&[("colocate-with-strict", "deadbeef")]);
        let local = CapabilitySet::default();
        let registry = IntentRegistry::new();
        let keys = metadata_keys();
        let cfg = ignored_intent_cfg().with_colocation_policy(ColocationPolicy::Ignore);
        let inputs = AdmissionInputs {
            chain_caps: &chain,
            local_caps: &local,
            config: &cfg,
            intent_registry: &registry,
            metadata_keys: &keys,
            colocation_target_held: Some(false),
        };
        assert_eq!(should_admit(&inputs), AdmissionVerdict::Admit);
    }

    #[test]
    fn colocation_strict_hint_blocks_under_soft_preference_when_target_absent() {
        let chain = caps_with_metadata(&[("colocate-with-strict", "deadbeef")]);
        let local = CapabilitySet::default();
        let registry = IntentRegistry::new();
        let keys = metadata_keys();
        let cfg = ignored_intent_cfg().with_colocation_policy(ColocationPolicy::SoftPreference);
        let inputs = AdmissionInputs {
            chain_caps: &chain,
            local_caps: &local,
            config: &cfg,
            intent_registry: &registry,
            metadata_keys: &keys,
            colocation_target_held: Some(false),
        };
        assert_eq!(should_admit(&inputs), AdmissionVerdict::RejectColocation);
    }

    #[test]
    fn colocation_strict_hint_admits_when_target_held() {
        let chain = caps_with_metadata(&[("colocate-with-strict", "deadbeef")]);
        let local = CapabilitySet::default();
        let registry = IntentRegistry::new();
        let keys = metadata_keys();
        let cfg = ignored_intent_cfg().with_colocation_policy(ColocationPolicy::SoftPreference);
        let inputs = AdmissionInputs {
            chain_caps: &chain,
            local_caps: &local,
            config: &cfg,
            intent_registry: &registry,
            metadata_keys: &keys,
            colocation_target_held: Some(true),
        };
        assert_eq!(should_admit(&inputs), AdmissionVerdict::Admit);
    }

    #[test]
    fn colocation_soft_hint_does_not_block_under_soft_preference() {
        // Soft hint + SoftPreference policy + target not held →
        // admit (the soft hint affects scoring, not admission).
        let chain = caps_with_metadata(&[("colocate-with", "deadbeef")]);
        let local = CapabilitySet::default();
        let registry = IntentRegistry::new();
        let keys = metadata_keys();
        let cfg = ignored_intent_cfg().with_colocation_policy(ColocationPolicy::SoftPreference);
        let inputs = AdmissionInputs {
            chain_caps: &chain,
            local_caps: &local,
            config: &cfg,
            intent_registry: &registry,
            metadata_keys: &keys,
            colocation_target_held: Some(false),
        };
        assert_eq!(should_admit(&inputs), AdmissionVerdict::Admit);
    }

    #[test]
    fn colocation_soft_hint_blocks_under_strict_required_when_target_absent() {
        let chain = caps_with_metadata(&[("colocate-with", "deadbeef")]);
        let local = CapabilitySet::default();
        let registry = IntentRegistry::new();
        let keys = metadata_keys();
        let cfg = ignored_intent_cfg().with_colocation_policy(ColocationPolicy::StrictRequired);
        let inputs = AdmissionInputs {
            chain_caps: &chain,
            local_caps: &local,
            config: &cfg,
            intent_registry: &registry,
            metadata_keys: &keys,
            colocation_target_held: Some(false),
        };
        assert_eq!(should_admit(&inputs), AdmissionVerdict::RejectColocation);
    }

    #[test]
    fn colocation_none_resolution_treated_as_target_not_held() {
        // The runtime is allowed to pass `None` if it hasn't
        // resolved the target yet; for the gate decision, "not
        // held" is the conservative interpretation so a missing
        // resolution can't accidentally relax a strict hint.
        let chain = caps_with_metadata(&[("colocate-with-strict", "deadbeef")]);
        let local = CapabilitySet::default();
        let registry = IntentRegistry::new();
        let keys = metadata_keys();
        let cfg = ignored_intent_cfg().with_colocation_policy(ColocationPolicy::SoftPreference);
        let inputs = AdmissionInputs {
            chain_caps: &chain,
            local_caps: &local,
            config: &cfg,
            intent_registry: &registry,
            metadata_keys: &keys,
            colocation_target_held: None,
        };
        assert_eq!(should_admit(&inputs), AdmissionVerdict::RejectColocation);
    }

    // ---- composition ----

    #[test]
    fn first_failing_axis_returns_specific_reject_variant() {
        // Chain fails scope AND intent — verdict names scope
        // (the first axis evaluated).
        let mut chain = caps_with_scope("webcam-streams");
        chain
            .metadata
            .insert("intent".to_string(), "ml-training".to_string());
        let local = CapabilitySet::default();
        let registry = IntentRegistry::defaults();
        let keys = metadata_keys();
        let cfg = GreedyConfig::default()
            .with_scopes(vec![super::super::ScopeLabel::new("industrial")])
            .with_intent_match(IntentMatchPolicy::Strict);
        let inputs = AdmissionInputs {
            chain_caps: &chain,
            local_caps: &local,
            config: &cfg,
            intent_registry: &registry,
            metadata_keys: &keys,
            colocation_target_held: None,
        };
        assert_eq!(should_admit(&inputs), AdmissionVerdict::RejectScope);
    }
}
