//! The asset registry: **signed policy over CAIP-19 ids, not an identity
//! authority.**
//!
//! The registry answers "which assets does policy allow, what are their
//! decimals for cross-checking and display, and which ids does this
//! participant treat as equivalent" — it never mints or authorizes asset
//! identity (CAIP-19 does that). The SDK ships a signed default;
//! participants pin or override. Envelopes bind `asset_registry
//! {version, hash}`; verification uses the revision the quote was issued
//! under — never "whatever the latest registry says today."
//!
//! Nonstandard assets (fee-on-transfer, rebasing, transfer-hook/blacklist
//! tokens) are unsupported: the registry is an allowlist, and absence is a
//! hard reject on the money path.

use serde::{Deserialize, Serialize};

use super::canonical::{canonical_bytes, ExtraFields, SignatureHex, SignedEnvelope};
use crate::x402::caip::{AssetId, ChainId};
use crate::x402::requirements::PaymentRequirements;
use net::adapter::net::identity::EntityId;

/// Errors from registry policy checks.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegistryError {
    #[error("asset `{asset}` on `{network}` is not in registry {registry_version} — unregistered assets are hard-rejected on the money path")]
    UnknownAsset {
        network: String,
        asset: String,
        registry_version: String,
    },
    #[error("decimals mismatch for `{asset_id}`: declared {declared}, registry {registry_version} says {expected} — present-and-mismatched hard-rejects")]
    DecimalsMismatch {
        asset_id: String,
        declared: u8,
        expected: u8,
        registry_version: String,
    },
    #[error("registry canonicalization failed: {0}")]
    Encoding(String),
}

/// One allowed asset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssetEntry {
    /// The CAIP-19 identity of the asset — a specific issued asset on a
    /// specific chain. Native vs bridged vs wrapped are distinct ids.
    pub id: AssetId,
    /// The asset locator exactly as it appears in x402
    /// `PaymentRequirements.asset` on this network (comparison is exact —
    /// registry authors record the on-wire spelling).
    pub x402_asset: String,
    /// Minor-unit decimals, the cross-check source of truth.
    pub decimals: u8,
    /// Display symbol (UX metadata only).
    pub symbol: String,
    /// Optional display name (UX metadata only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Participants sharing an equivalence class label treat these ids as
    /// interchangeable *by their own policy declaration*. Absent = this
    /// asset is equivalent to nothing but itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equivalence_class: Option<String>,
}

/// The signed asset-policy registry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssetRegistry {
    /// Registry revision label (e.g. `net-default-0`).
    pub version: String,
    /// Allowed assets. Anything absent is denied on the money path.
    pub assets: Vec<AssetEntry>,
    /// The identity accountable for this registry revision.
    pub signer: EntityId,
    /// Signature over the canonical bytes (sans `signature`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<SignatureHex>,
    #[serde(flatten)]
    pub extra: ExtraFields,
}

impl SignedEnvelope for AssetRegistry {
    const OBJECT_TAG: &'static str = "net.payment.asset_registry@1";
    fn signer(&self) -> &EntityId {
        &self.signer
    }
    fn signature(&self) -> Option<&SignatureHex> {
        self.signature.as_ref()
    }
    fn set_signature(&mut self, sig: SignatureHex) {
        self.signature = Some(sig);
    }
}

impl AssetRegistry {
    /// The `{version, hash}` reference envelopes bind. The hash covers the
    /// canonical bytes *including* the signature, so a re-signed registry
    /// is a different revision.
    pub fn reference(&self) -> Result<RegistryRef, RegistryError> {
        let bytes = canonical_bytes(self).map_err(|e| RegistryError::Encoding(e.to_string()))?;
        Ok(RegistryRef {
            version: self.version.clone(),
            hash: hex::encode(blake3::hash(&bytes).as_bytes()),
        })
    }

    /// Look up the entry for an x402 `(network, asset)` pair. Exact match;
    /// absence is a policy denial, not a lookup miss.
    pub fn lookup(
        &self,
        network: &ChainId,
        x402_asset: &str,
    ) -> Result<&AssetEntry, RegistryError> {
        self.assets
            .iter()
            .find(|e| e.id.chain() == network && e.x402_asset == x402_asset)
            .ok_or_else(|| RegistryError::UnknownAsset {
                network: network.as_str().to_string(),
                asset: x402_asset.to_string(),
                registry_version: self.version.clone(),
            })
    }

    /// The full money-path check for a requirements view: asset must be
    /// registered, and if the requirements' `extra` declares decimals they
    /// must match the registry (present-and-mismatched hard-rejects,
    /// pre-sign on the provider side and pre-verify on the caller side).
    pub fn check_requirements(
        &self,
        requirements: &PaymentRequirements,
    ) -> Result<&AssetEntry, RegistryError> {
        let network = requirements
            .chain()
            .map_err(|e| RegistryError::Encoding(e.to_string()))?;
        let entry = self.lookup(&network, &requirements.asset)?;
        let declared = requirements
            .extra
            .as_ref()
            .and_then(|e| e.get("decimals"))
            .and_then(|d| d.as_u64());
        if let Some(declared) = declared {
            if declared != u64::from(entry.decimals) {
                return Err(RegistryError::DecimalsMismatch {
                    asset_id: entry.id.as_str().to_string(),
                    declared: declared.min(u64::from(u8::MAX)) as u8,
                    expected: entry.decimals,
                    registry_version: self.version.clone(),
                });
            }
        }
        Ok(entry)
    }
}

/// The `{version, hash}` pair envelopes bind to pin the registry revision
/// a quote was issued under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryRef {
    pub version: String,
    /// Hex blake3 of the registry's canonical bytes.
    pub hash: String,
}

/// The P1 default registry: the mock asset plus the survey-verified real
/// networks — Base Sepolia (the conformance target), Base (the first
/// real-money target), Solana SPL-USDC, and XRP (XRPL rung, Mode A). Network enablement is
/// registry entries + facilitator config, never code; participants pin
/// or override this default.
pub fn default_registry_v1(signer: EntityId) -> AssetRegistry {
    let mut registry = default_mock_registry(signer);
    registry.version = "net-default-1".to_string();
    registry.assets.extend([
        AssetEntry {
            id: AssetId::parse(
                "eip155:84532/erc20:0x036CbD53842c5426634e7929541eC2318f3dCF7e",
            )
            .unwrap_or_else(|_| unreachable!("static base-sepolia USDC id is valid CAIP-19")),
            x402_asset: "0x036CbD53842c5426634e7929541eC2318f3dCF7e".to_string(),
            decimals: 6,
            symbol: "USDC".to_string(),
            display_name: Some("USDC (Base Sepolia testnet)".to_string()),
            equivalence_class: None,
        },
        AssetEntry {
            id: AssetId::parse(
                "eip155:8453/erc20:0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
            )
            .unwrap_or_else(|_| unreachable!("static base USDC id is valid CAIP-19")),
            x402_asset: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".to_string(),
            decimals: 6,
            symbol: "USDC".to_string(),
            display_name: Some("USDC (Base)".to_string()),
            equivalence_class: None,
        },
        AssetEntry {
            id: AssetId::parse(
                "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp/token:EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
            )
            .unwrap_or_else(|_| unreachable!("static solana USDC id is valid CAIP-19")),
            x402_asset: "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v".to_string(),
            decimals: 6,
            symbol: "USDC".to_string(),
            display_name: Some("USDC (Solana SPL)".to_string()),
            equivalence_class: None,
        },
        // XRPL rung, Mode A (XRP-only — RLUSD waits on the IOU
        // amount-domain review; see PAYMENTS_XRPL_ENABLEMENT_PLAN.md).
        // The CAIP-2 `xrpl:0` reference is the pinned-doc convention
        // (unratified upstream), bound through this signed registry
        // revision. Amounts are drops: 6 decimals, integer grammar.
        AssetEntry {
            id: AssetId::parse("xrpl:0/slip44:144")
                .unwrap_or_else(|_| unreachable!("static xrpl XRP id is valid CAIP-19")),
            x402_asset: "XRP".to_string(),
            decimals: 6,
            symbol: "XRP".to_string(),
            display_name: Some("XRP (XRP Ledger)".to_string()),
            equivalence_class: None,
        },
    ]);
    registry
}

/// The P0 default registry: the mock network's asset only. Real networks
/// enter in P1 as registry entries + facilitator config — config, not code.
pub fn default_mock_registry(signer: EntityId) -> AssetRegistry {
    AssetRegistry {
        version: "net-default-0".to_string(),
        assets: vec![AssetEntry {
            // Deliberately parallel to a real entry: `mock:net` is a valid
            // CAIP-2 network, `musd` a 6-decimal USD-shaped test asset.
            id: AssetId::parse("mock:net/token:musd").unwrap_or_else(|_| {
                // The literal above is a compile-time constant shape; this
                // arm is unreachable but keeps the lint policy honest.
                unreachable!("static mock asset id is valid CAIP-19")
            }),
            x402_asset: "musd".to_string(),
            decimals: 6,
            symbol: "MUSD".to_string(),
            display_name: Some("Mock USD (test asset, no value)".to_string()),
            equivalence_class: None,
        }],
        signer,
        signature: None,
        extra: ExtraFields::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use net::adapter::net::identity::EntityKeypair;

    fn registry() -> AssetRegistry {
        default_mock_registry(EntityKeypair::generate().entity_id().clone())
    }

    fn mock_requirements(asset: &str, extra: Option<serde_json::Value>) -> PaymentRequirements {
        PaymentRequirements {
            scheme: "mock".into(),
            network: "mock:net".into(),
            amount: "10000".into(),
            asset: asset.into(),
            pay_to: "mock-payee".into(),
            max_timeout_seconds: 60,
            extra,
        }
    }

    #[test]
    fn known_asset_passes_unknown_hard_rejects() {
        let reg = registry();
        assert!(reg
            .check_requirements(&mock_requirements("musd", None))
            .is_ok());
        let err = reg
            .check_requirements(&mock_requirements("evil-musd", None))
            .unwrap_err();
        assert!(matches!(err, RegistryError::UnknownAsset { .. }));
    }

    #[test]
    fn decimals_present_and_mismatched_hard_rejects() {
        let reg = registry();
        let ok = mock_requirements("musd", Some(serde_json::json!({"decimals": 6})));
        assert!(reg.check_requirements(&ok).is_ok());
        let bad = mock_requirements("musd", Some(serde_json::json!({"decimals": 18})));
        assert!(matches!(
            reg.check_requirements(&bad).unwrap_err(),
            RegistryError::DecimalsMismatch {
                declared: 18,
                expected: 6,
                ..
            }
        ));
    }

    #[test]
    fn reference_pins_content_and_signature() {
        let kp = EntityKeypair::generate();
        let mut reg = default_mock_registry(kp.entity_id().clone());
        let unsigned_ref = reg.reference().unwrap();
        reg.sign_with(&kp).unwrap();
        reg.verify_signature().unwrap();
        let signed_ref = reg.reference().unwrap();
        assert_eq!(unsigned_ref.version, signed_ref.version);
        assert_ne!(
            unsigned_ref.hash, signed_ref.hash,
            "a re-signed registry is a different pinned revision"
        );
    }
}
