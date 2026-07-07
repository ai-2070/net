//! Well-known network config packs — the "config, not code" artifacts
//! for the P1 survey networks, shipped as constructors so enabling a
//! network starts from a reviewed, pinned baseline instead of a
//! hand-typed endpoint.
//!
//! A pack is **data**: a [`FacilitatorConfig`] naming the facilitator
//! endpoint, the enabled `(scheme, network)` pairs, the checker RPC
//! endpoints, and the per-network serve tier. Nothing here branches on
//! a network — that is the WS4 review invariant, and these constructors
//! are deliberately boring so a diff that isn't boring is a finding.
//!
//! Two operational truths every pack inherits:
//!
//! 1. **Pinned facts go stale.** Endpoints and supported pairs were
//!    survey-verified 2026-07-06 (see `PAYMENTS_P1_IMPLEMENTATION_PLAN.md`);
//!    [`HttpFacilitator::from_config`] re-verifies against the live
//!    `GET /supported` at every load, so a stale pack fails loudly at
//!    startup, never at first payment.
//! 2. **A pack enables nothing by itself.** Spending on a real network
//!    additionally requires the network in the spend policy's
//!    `allowed_networks` (the operator's explicit production consent), a
//!    settlement signer for the namespace, and — above `observed` — a
//!    chain checker. The pack is the map, not the permission.
//!
//! [`HttpFacilitator::from_config`]: super::client::HttpFacilitator::from_config

use std::collections::BTreeMap;

use super::config::{AuthConfig, FacilitatorConfig, SchemePair, TAG_FACILITATOR_CONFIG};
use crate::core::verification::VerificationTier;

/// The x402.org testnet facilitator (unauthenticated, Base Sepolia).
pub const X402_ORG_FACILITATOR: &str = "https://x402.org/facilitator";
/// The Coinbase CDP x402 facilitator (API-key auth, mainnet networks).
pub const CDP_FACILITATOR: &str = "https://api.cdp.coinbase.com/platform/v2/x402";

/// Base Sepolia (CAIP-2) — the conformance target.
pub const NETWORK_BASE_SEPOLIA: &str = "eip155:84532";
/// Base mainnet (CAIP-2) — the first real-money target.
pub const NETWORK_BASE: &str = "eip155:8453";
/// Solana mainnet (CAIP-2, genesis-hash reference).
pub const NETWORK_SOLANA: &str = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp";

/// Public Base Sepolia JSON-RPC, for the independent chain checker.
pub const RPC_BASE_SEPOLIA: &str = "https://sepolia.base.org";
/// Public Base mainnet JSON-RPC, for the independent chain checker.
pub const RPC_BASE: &str = "https://mainnet.base.org";
/// Public Solana mainnet JSON-RPC, for the independent chain checker.
/// Heavily rate-limited — fine for conformance shapes; production
/// operators supply their own endpoint in the pack.
pub const RPC_SOLANA: &str = "https://api.mainnet-beta.solana.com";

/// `final`-tier confirmation depth for Base (an OP-stack L2). A dozen L2
/// blocks (~24s) is *not* L1-backed finality — L2 blocks stay reversible
/// until their batch finalizes on L1 (minutes). ~1800 L2 blocks (≈1h at
/// 2s/block) is a conservative L1-finalization posture; operators tune it
/// per deployment. Chosen deliberately over the checker's L1-scale default.
pub const FINAL_DEPTH_BASE: u64 = 1800;

/// Rung 1 — Base Sepolia via the x402.org facilitator: open auth, test
/// USDC, and the full production *posture* (serve at `confirmed(1)`, so
/// a conformance run exercises the checker path, not just receipt
/// trust). This is the pack the live conformance suite loads.
pub fn x402_org_base_sepolia() -> FacilitatorConfig {
    FacilitatorConfig {
        object: TAG_FACILITATOR_CONFIG.to_string(),
        endpoint: X402_ORG_FACILITATOR.to_string(),
        auth: AuthConfig::None,
        pairs: vec![SchemePair {
            scheme: "exact".to_string(),
            network: NETWORK_BASE_SEPOLIA.to_string(),
        }],
        rpc_endpoints: BTreeMap::from([(
            NETWORK_BASE_SEPOLIA.to_string(),
            RPC_BASE_SEPOLIA.to_string(),
        )]),
        required_tier: BTreeMap::from([(
            NETWORK_BASE_SEPOLIA.to_string(),
            VerificationTier::Confirmed(1),
        )]),
        final_depth: BTreeMap::from([(NETWORK_BASE_SEPOLIA.to_string(), FINAL_DEPTH_BASE)]),
    }
}

/// Rung 2 — Base mainnet via the CDP facilitator. `secret_ref` names
/// the CDP API credential in the **host's** secret store (forwarding
/// doctrine: the ref travels, the value never does — the host resolves
/// it into an [`AuthProvider`] at construction). Serve tier defaults to
/// `confirmed(1)`; raise per deployment for high-value capabilities.
///
/// [`AuthProvider`]: super::client::AuthProvider
pub fn cdp_base_mainnet(secret_ref: impl Into<String>) -> FacilitatorConfig {
    FacilitatorConfig {
        object: TAG_FACILITATOR_CONFIG.to_string(),
        endpoint: CDP_FACILITATOR.to_string(),
        auth: AuthConfig::Bearer {
            secret_ref: secret_ref.into(),
        },
        pairs: vec![SchemePair {
            scheme: "exact".to_string(),
            network: NETWORK_BASE.to_string(),
        }],
        rpc_endpoints: BTreeMap::from([(NETWORK_BASE.to_string(), RPC_BASE.to_string())]),
        required_tier: BTreeMap::from([(NETWORK_BASE.to_string(), VerificationTier::Confirmed(1))]),
        final_depth: BTreeMap::from([(NETWORK_BASE.to_string(), FINAL_DEPTH_BASE)]),
    }
}

/// Rung 3 — Solana mainnet via the CDP facilitator. Settleable through
/// the exact-SVM seam (`SchemeSigner::sign_svm_transfer` /
/// [`ExternalSvmSigner`](crate::flow::signer::ExternalSvmSigner) — the
/// wallet builds and partially signs; without one, accepts[] entries on
/// this network are honestly refused at selection). Independently
/// checkable via [`SvmChecker`](crate::checker::svm::SvmChecker), so the
/// pack serves at `confirmed(1)` like the eip155 rungs. `final_depth` is
/// deliberately absent: Solana's `finalized` commitment is deterministic
/// finality — there is no depth posture to configure and the SVM checker
/// ignores the knob.
pub fn cdp_solana_mainnet(secret_ref: impl Into<String>) -> FacilitatorConfig {
    FacilitatorConfig {
        object: TAG_FACILITATOR_CONFIG.to_string(),
        endpoint: CDP_FACILITATOR.to_string(),
        auth: AuthConfig::Bearer {
            secret_ref: secret_ref.into(),
        },
        pairs: vec![SchemePair {
            scheme: "exact".to_string(),
            network: NETWORK_SOLANA.to_string(),
        }],
        rpc_endpoints: BTreeMap::from([(NETWORK_SOLANA.to_string(), RPC_SOLANA.to_string())]),
        required_tier: BTreeMap::from([(
            NETWORK_SOLANA.to_string(),
            VerificationTier::Confirmed(1),
        )]),
        final_depth: BTreeMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::registry::default_registry_v1;
    use crate::facilitator::config::{SupportedKind, SupportedResponse};
    use net::adapter::net::identity::EntityKeypair;

    fn all_packs() -> Vec<FacilitatorConfig> {
        vec![
            x402_org_base_sepolia(),
            cdp_base_mainnet("cdp-api-key"),
            cdp_solana_mainnet("cdp-api-key"),
        ]
    }

    fn supported_for(pack: &FacilitatorConfig) -> SupportedResponse {
        SupportedResponse {
            kinds: pack
                .pairs
                .iter()
                .map(|p| SupportedKind {
                    x402_version: 2,
                    scheme: p.scheme.clone(),
                    network: p.network.clone(),
                })
                .collect(),
            extensions: Vec::new(),
            signers: std::collections::BTreeMap::new(),
        }
    }

    #[test]
    fn every_pack_round_trips_and_validates() {
        for pack in all_packs() {
            let bytes = serde_json::to_vec(&pack).unwrap();
            let back = FacilitatorConfig::from_json_bytes(&bytes).unwrap();
            assert_eq!(back, pack);
            back.validate_against(&supported_for(&pack)).unwrap();
        }
    }

    #[test]
    fn every_pack_network_is_in_the_default_registry() {
        // The pack and the registry must tell the same story: a network
        // a pack enables has a registered asset to spend on it.
        let registry = default_registry_v1(EntityKeypair::generate().entity_id().clone());
        for pack in all_packs() {
            for network in pack.networks() {
                assert!(
                    registry
                        .assets
                        .iter()
                        .any(|a| a.id.chain().as_str() == network),
                    "pack network `{network}` has no asset in registry {}",
                    registry.version
                );
            }
        }
    }

    #[test]
    fn mainnet_packs_carry_refs_and_the_conformance_pack_is_open() {
        assert_eq!(x402_org_base_sepolia().auth, AuthConfig::None);
        for pack in [cdp_base_mainnet("k"), cdp_solana_mainnet("k")] {
            assert!(matches!(pack.auth, AuthConfig::Bearer { .. }));
            // The serialized pack holds the REF, never a value shape.
            let json = serde_json::to_string(&pack).unwrap();
            assert!(json.contains("secret_ref"));
        }
    }

    #[test]
    fn tier_posture_matches_checker_availability() {
        // Every pack serves above receipt trust and says where to check —
        // eip155 via the depth-arithmetic checker, solana via the
        // commitment-level checker.
        let sepolia = x402_org_base_sepolia();
        assert_eq!(
            sepolia.required_tier(NETWORK_BASE_SEPOLIA),
            VerificationTier::Confirmed(1)
        );
        assert!(sepolia.rpc_endpoints.contains_key(NETWORK_BASE_SEPOLIA));

        // L2 packs carry a per-network final_depth well above the L1 default.
        assert_eq!(
            sepolia.final_depth(NETWORK_BASE_SEPOLIA),
            Some(FINAL_DEPTH_BASE)
        );

        let base = cdp_base_mainnet("k");
        assert_eq!(
            base.required_tier(NETWORK_BASE),
            VerificationTier::Confirmed(1)
        );
        assert!(base.rpc_endpoints.contains_key(NETWORK_BASE));
        assert_eq!(base.final_depth(NETWORK_BASE), Some(FINAL_DEPTH_BASE));

        let solana = cdp_solana_mainnet("k");
        assert_eq!(
            solana.required_tier(NETWORK_SOLANA),
            VerificationTier::Confirmed(1)
        );
        assert!(solana.rpc_endpoints.contains_key(NETWORK_SOLANA));
        // Deterministic finality: the depth knob is deliberately absent
        // (the SVM checker ignores it either way).
        assert_eq!(solana.final_depth(NETWORK_SOLANA), None);
    }
}
