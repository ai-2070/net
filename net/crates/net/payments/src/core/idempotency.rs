//! Structural idempotency.
//!
//! Every stage of the paid lifecycle carries an idempotency key scoped
//! `{caller, provider, capability, quote}`. Same-key retry never
//! double-charges or double-serves: one settle, one serve, one billing
//! event id. Agents retry on timeouts constantly — this is the difference
//! between a hiccup and a duplicate charge.

use net::adapter::net::identity::EntityId;
use serde::{Deserialize, Serialize};

/// The idempotency scope. The derived key is a blake3 hash over a
/// domain-separated, length-prefixed transcript of the four coordinates —
/// no delimiter-injection ambiguity between e.g. `("a/b", "c")` and
/// `("a", "b/c")`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdempotencyScope {
    pub caller: EntityId,
    pub provider: EntityId,
    /// Capability id in its display form (`provider/capability`).
    pub capability: String,
    pub quote_id: String,
}

const DOMAIN: &[u8] = b"net.payments.idempotency@1";

impl IdempotencyScope {
    /// Derive the idempotency key, hex-encoded.
    pub fn key(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(DOMAIN);
        for part in [
            self.caller.as_bytes().as_slice(),
            self.provider.as_bytes().as_slice(),
            self.capability.as_bytes(),
            self.quote_id.as_bytes(),
        ] {
            hasher.update(&(part.len() as u64).to_le_bytes());
            hasher.update(part);
        }
        hex::encode(hasher.finalize().as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use net::adapter::net::identity::EntityKeypair;

    fn scope(capability: &str, quote_id: &str) -> IdempotencyScope {
        // Deterministic identities so the test pins real key stability.
        let caller = EntityKeypair::from_bytes([1u8; 32]).entity_id().clone();
        let provider = EntityKeypair::from_bytes([2u8; 32]).entity_id().clone();
        IdempotencyScope {
            caller,
            provider,
            capability: capability.to_string(),
            quote_id: quote_id.to_string(),
        }
    }

    #[test]
    fn same_scope_same_key_different_scope_different_key() {
        let a = scope("prov/tool", "q1");
        assert_eq!(a.key(), scope("prov/tool", "q1").key());
        assert_ne!(a.key(), scope("prov/tool", "q2").key());
        assert_ne!(a.key(), scope("prov/other", "q1").key());
    }

    #[test]
    fn length_prefixing_prevents_boundary_confusion() {
        assert_ne!(scope("ab", "c").key(), scope("a", "bc").key());
    }
}
