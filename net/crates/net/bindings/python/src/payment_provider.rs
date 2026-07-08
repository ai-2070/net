//! Provider-side payment surface: pricing a capability (and, later, charging
//! for it). The supply-side counterpart to `capability_gateway.rs` — the
//! helpers a Python node needs to *be* a paid provider, mirroring the Rust
//! reference (`sdk/src/tool.rs` pricing + the MCP wrap `payment_admission`
//! gate). Doctrine #1 holds: authoring/settlement logic is `net-payments`;
//! this marshals config in and the canonical envelope out.

#![cfg(feature = "payments")]

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

use net::adapter::net::identity::EntityId;
use net_payments::core::canonical::canonical_bytes;
use net_payments::core::registry::default_registry_v1;
use net_payments::core::terms::PricingTerms;
use net_payments::x402::requirements::PaymentRequirements;
use net_payments::x402::X402Carry;

/// Author the canonical `net.pricing.terms@1` JSON for a capability from a
/// provider entity id + a JSON array of x402 `PaymentRequirements`. Pure —
/// the pyfunction below is a thin wrapper.
fn author_pricing_terms(
    provider_entity_id: [u8; 32],
    capability: &str,
    requirements_json: &str,
) -> Result<String, String> {
    let reqs: Vec<PaymentRequirements> = serde_json::from_str(requirements_json).map_err(|e| {
        format!("requirements_json must be a JSON array of x402 PaymentRequirements objects: {e}")
    })?;
    if reqs.is_empty() {
        return Err(
            "at least one payment requirement is required — an empty accepts[] prices nothing"
                .to_string(),
        );
    }
    // Locally-originated x402: `author` is the sanctioned serialization point
    // (the templates originate here, so these bytes become the preserved
    // originals — no byte-preservation violation).
    let accepts = reqs
        .iter()
        .map(X402Carry::author)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("author payment requirement: {e}"))?;
    let provider = EntityId::from_bytes(provider_entity_id);
    // The v1 default registry (mock + survey networks). Its `reference()` is
    // signer-independent (it hashes the asset content), so it matches any
    // caller authoring quotes under the same default registry.
    let registry = default_registry_v1(provider.clone());
    let reference = registry
        .reference()
        .map_err(|e| format!("registry reference: {e}"))?;
    let terms = PricingTerms::new(provider, capability, accepts, reference);
    let bytes = canonical_bytes(&terms).map_err(|e| format!("canonicalize terms: {e}"))?;
    String::from_utf8(bytes).map_err(|e| format!("terms are not UTF-8: {e}"))
}

/// Author the canonical `net.pricing.terms@1` JSON string that prices a
/// capability — to hand to the priced publish path or announce at discovery.
///
/// `provider_entity_id` is the node's 32-byte mesh entity id (``mesh.entity_id``)
/// — the identity that will issue quotes for these terms. Only the public id
/// crosses; keys never do. `requirements_json` is a JSON array of x402
/// ``PaymentRequirements`` objects (``scheme``, ``network``, ``amount``,
/// ``asset``, ``payTo``, ``maxTimeoutSeconds``, optional ``extra`` — the x402
/// camelCase wire names); one entry per acceptable ``(scheme, network,
/// asset)``. Returns the canonical, byte-preserved terms string, opaque
/// downstream and echoed verbatim at discovery. Raises ``ValueError`` on a bad
/// entity id, malformed JSON, or an empty list.
#[pyfunction]
pub fn build_pricing_terms(
    provider_entity_id: Vec<u8>,
    capability: &str,
    requirements_json: &str,
) -> PyResult<String> {
    let id: [u8; 32] = provider_entity_id.as_slice().try_into().map_err(|_| {
        PyValueError::new_err(format!(
            "provider_entity_id must be 32 bytes (got {})",
            provider_entity_id.len()
        ))
    })?;
    author_pricing_terms(id, capability, requirements_json).map_err(PyValueError::new_err)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOCK_REQS: &str = r#"[{"scheme":"mock","network":"mock:net","amount":"2500","asset":"musd","payTo":"mock-provider-settle-addr","maxTimeoutSeconds":60}]"#;

    #[test]
    fn authors_canonical_decodable_pricing_terms() {
        let terms = author_pricing_terms([7u8; 32], "prov/echo", MOCK_REQS).expect("author");

        // The typed decoder accepts it (tag + non-empty accepts[]).
        let parsed = PricingTerms::from_json_bytes(terms.as_bytes()).expect("decode");
        assert_eq!(parsed.object, "net.pricing.terms@1");
        assert_eq!(parsed.capability, "prov/echo");
        assert_eq!(parsed.accepts.len(), 1);
        assert_eq!(parsed.provider, EntityId::from_bytes([7u8; 32]));

        // Canonical emission is a fixed point.
        let reparse: serde_json::Value = serde_json::from_str(&terms).unwrap();
        let re = String::from_utf8(canonical_bytes(&reparse).unwrap()).unwrap();
        assert_eq!(re, terms, "authored terms are already canonical");
    }

    #[test]
    fn multiple_accepts_are_preserved() {
        let two = r#"[
            {"scheme":"mock","network":"mock:net","amount":"2500","asset":"musd","payTo":"a","maxTimeoutSeconds":60},
            {"scheme":"mock","network":"mock:net","amount":"5000","asset":"musd","payTo":"a","maxTimeoutSeconds":60}
        ]"#;
        let terms = author_pricing_terms([7u8; 32], "prov/echo", two).expect("author");
        assert_eq!(
            PricingTerms::from_json_bytes(terms.as_bytes())
                .unwrap()
                .accepts
                .len(),
            2
        );
    }

    #[test]
    fn empty_and_malformed_are_rejected() {
        assert!(author_pricing_terms([1u8; 32], "prov/echo", "[]").is_err());
        assert!(author_pricing_terms([1u8; 32], "prov/echo", "not json").is_err());
        // A requirement missing a required field (payTo) is a decode error.
        let bad = r#"[{"scheme":"mock","network":"mock:net","amount":"1","asset":"musd","maxTimeoutSeconds":60}]"#;
        assert!(author_pricing_terms([1u8; 32], "prov/echo", bad).is_err());
    }
}
