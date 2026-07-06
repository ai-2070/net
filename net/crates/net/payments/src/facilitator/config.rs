//! `net.payment.facilitator_config@1` — the versioned config object
//! that enables a network. This is the "config, not code" artifact:
//! enabling Base Sepolia, Base, or Solana is one of these plus registry
//! entries plus a conformance run. Auditable, exportable, diffable.
//!
//! Auth carries a **secret ref only** (forwarding doctrine): the host's
//! secret handling resolves the ref to a value at construction time;
//! neither this object nor any log ever holds credential material.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::core::verification::VerificationTier;
use crate::core::versioning::{ensure_tag, VersionError};
use crate::x402::X402_VERSION;

/// The facilitator-config object tag.
pub const TAG_FACILITATOR_CONFIG: &str = "net.payment.facilitator_config@1";

/// One `(scheme, network)` pair a facilitator supports (`GET
/// /supported` → `kinds[]`, spec-pinned shape).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupportedKind {
    #[serde(rename = "x402Version")]
    pub x402_version: u64,
    pub scheme: String,
    pub network: String,
}

/// The `GET /supported` response shape (spec-pinned).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupportedResponse {
    pub kinds: Vec<SupportedKind>,
    #[serde(default)]
    pub extensions: Vec<String>,
    #[serde(default)]
    pub signers: BTreeMap<String, Vec<String>>,
}

/// How to authenticate against the facilitator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuthConfig {
    /// Open facilitator (the x402.org testnet, self-hosted deployments).
    None,
    /// Bearer token named by `secret_ref` — the host resolves the ref
    /// through its own secret handling; the value never appears here.
    Bearer { secret_ref: String },
}

/// One enabled `(scheme, network)` pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemePair {
    pub scheme: String,
    pub network: String,
}

/// The facilitator configuration for a set of networks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FacilitatorConfig {
    /// Always [`TAG_FACILITATOR_CONFIG`].
    pub object: String,
    /// Facilitator base URL.
    pub endpoint: String,
    /// Authentication (secret refs only).
    pub auth: AuthConfig,
    /// The `(scheme, network)` pairs this config enables. Every pair
    /// must be offered by the facilitator (`validate_against`).
    pub pairs: Vec<SchemePair>,
    /// Chain RPC endpoints per CAIP-2 network, for the independent
    /// verification checker (`confirmed(n)`/`final` — the facilitator
    /// is never in the trust root above `observed`).
    #[serde(default)]
    pub rpc_endpoints: BTreeMap<String, String>,
    /// Required verification tier before serving, per CAIP-2 network.
    /// Absent = `observed` (receipt-trust; pick deliberately).
    #[serde(default)]
    pub required_tier: BTreeMap<String, VerificationTier>,
}

impl FacilitatorConfig {
    /// Decode + tag-check a config object.
    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self, ConfigError> {
        let config: Self =
            serde_json::from_slice(bytes).map_err(|e| ConfigError::Malformed(e.to_string()))?;
        ensure_tag(TAG_FACILITATOR_CONFIG, &config.object)?;
        if config.pairs.is_empty() {
            return Err(ConfigError::Invalid(
                "a facilitator config enabling zero (scheme, network) pairs enables nothing"
                    .to_string(),
            ));
        }
        Ok(config)
    }

    /// Offline validation against a facilitator's `GET /supported`
    /// answer: every enabled pair must be offered at x402Version 2.
    /// (The `http-facilitator` client fetches and applies this at load.)
    pub fn validate_against(&self, supported: &SupportedResponse) -> Result<(), ConfigError> {
        for pair in &self.pairs {
            let offered = supported.kinds.iter().any(|k| {
                k.x402_version == X402_VERSION
                    && k.scheme == pair.scheme
                    && k.network == pair.network
            });
            if !offered {
                return Err(ConfigError::Unsupported {
                    scheme: pair.scheme.clone(),
                    network: pair.network.clone(),
                    endpoint: self.endpoint.clone(),
                });
            }
        }
        Ok(())
    }

    /// The tier policy for a network (`observed` unless configured).
    pub fn required_tier(&self, network: &str) -> VerificationTier {
        self.required_tier
            .get(network)
            .copied()
            .unwrap_or(VerificationTier::Observed)
    }

    /// The CAIP-2 networks this config enables.
    pub fn networks(&self) -> Vec<String> {
        let mut networks: Vec<String> = self.pairs.iter().map(|p| p.network.clone()).collect();
        networks.dedup();
        networks
    }
}

/// Config decode/validation failures — always loud, always at load.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("facilitator config malformed: {0}")]
    Malformed(String),
    #[error("facilitator config invalid: {0}")]
    Invalid(String),
    #[error(transparent)]
    Tag(#[from] VersionError),
    #[error("facilitator at {endpoint} does not offer ({scheme}, {network}) at x402Version 2 — refusing the configuration")]
    Unsupported {
        scheme: String,
        network: String,
        endpoint: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> FacilitatorConfig {
        FacilitatorConfig {
            object: TAG_FACILITATOR_CONFIG.to_string(),
            endpoint: "https://x402.org/facilitator".to_string(),
            auth: AuthConfig::None,
            pairs: vec![SchemePair {
                scheme: "exact".to_string(),
                network: "eip155:84532".to_string(),
            }],
            rpc_endpoints: BTreeMap::from([(
                "eip155:84532".to_string(),
                "https://sepolia.base.org".to_string(),
            )]),
            required_tier: BTreeMap::from([(
                "eip155:84532".to_string(),
                VerificationTier::Confirmed(1),
            )]),
        }
    }

    fn supported(kinds: &[(&str, &str)]) -> SupportedResponse {
        SupportedResponse {
            kinds: kinds
                .iter()
                .map(|(scheme, network)| SupportedKind {
                    x402_version: 2,
                    scheme: scheme.to_string(),
                    network: network.to_string(),
                })
                .collect(),
            extensions: Vec::new(),
            signers: BTreeMap::new(),
        }
    }

    #[test]
    fn round_trips_with_tag_check_and_refuses_empty_pairs() {
        let bytes = serde_json::to_vec(&config()).unwrap();
        let back = FacilitatorConfig::from_json_bytes(&bytes).unwrap();
        assert_eq!(back, config());
        assert_eq!(back.required_tier("eip155:84532"), VerificationTier::Confirmed(1));
        assert_eq!(back.required_tier("eip155:8453"), VerificationTier::Observed);

        let mut wrong_tag = config();
        wrong_tag.object = "net.payment.quote@1".to_string();
        let bytes = serde_json::to_vec(&wrong_tag).unwrap();
        assert!(FacilitatorConfig::from_json_bytes(&bytes).is_err());

        let mut empty = config();
        empty.pairs.clear();
        let bytes = serde_json::to_vec(&empty).unwrap();
        assert!(FacilitatorConfig::from_json_bytes(&bytes).is_err());
    }

    #[test]
    fn validation_requires_every_pair_offered_at_v2() {
        let c = config();
        c.validate_against(&supported(&[("exact", "eip155:84532")])).unwrap();

        let err = c
            .validate_against(&supported(&[("exact", "eip155:8453")]))
            .unwrap_err();
        assert!(matches!(err, ConfigError::Unsupported { .. }));

        // Offered only at a different protocol version = not offered.
        let mut v1_only = supported(&[("exact", "eip155:84532")]);
        v1_only.kinds[0].x402_version = 1;
        assert!(c.validate_against(&v1_only).is_err());
    }

    #[test]
    fn auth_carries_refs_never_values() {
        let c = FacilitatorConfig {
            auth: AuthConfig::Bearer { secret_ref: "cdp-api-key".to_string() },
            ..config()
        };
        let json = serde_json::to_string(&c).unwrap();
        assert!(json.contains("cdp-api-key"), "the REF appears");
        assert!(json.contains("\"kind\":\"bearer\""));
    }
}
