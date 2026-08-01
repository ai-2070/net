//! x402 v2 `PaymentPayload` — the client-signed payment authorization.
//!
//! Shape per the pinned v2 spec:
//!
//! ```json
//! {
//!   "x402Version": 2,
//!   "resource": { "url": "..." },
//!   "accepted": { /* the PaymentRequirements the client accepted */ },
//!   "payload": { /* scheme-specific, e.g. EIP-3009 signature+authorization */ },
//!   "extensions": { }
//! }
//! ```
//!
//! There is **no separate Net intent object** — this payload travels in
//! the invocation envelope, byte-preserved. Binding of payload to
//! requirements is x402-internal (scheme-level), and that's the point:
//! Net's quote binds to the requirements; the scheme binds the payment to
//! them.

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use super::requirements::PaymentRequirements;
use super::{X402Carry, X402Error, X402View, X402_VERSION};

/// Parsed view over an x402 v2 `PaymentPayload`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentPayload {
    /// Protocol version — must be 2.
    pub x402_version: u64,
    /// Optional echo of the resource being paid for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<serde_json::Value>,
    /// The `PaymentRequirements` entry the client accepted, echoed back.
    pub accepted: PaymentRequirements,
    /// Scheme-specific payment authorization (opaque to Net; the scheme
    /// binds it to the accepted requirements).
    pub payload: serde_json::Value,
    /// x402 extensions map (consumed for interop only — never a substitute
    /// for Net identity, consent, or billing semantics).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<serde_json::Value>,
}

impl X402View for PaymentPayload {
    const KIND: &'static str = "PaymentPayload";

    fn validate(&self) -> Result<(), X402Error> {
        if self.x402_version != X402_VERSION {
            return Err(X402Error::UnsupportedX402Version {
                got: self.x402_version,
                expected: X402_VERSION,
            });
        }
        self.accepted.validate()?;
        if !self.payload.is_object() {
            return Err(X402Error::Invalid(
                "PaymentPayload.payload must be a scheme-specific object".into(),
            ));
        }
        Ok(())
    }
}

impl X402Carry<PaymentPayload> {
    /// The **scheme-semantic** replay identity of this payment.
    ///
    /// The engine's "one payload satisfies exactly one quote" guard keys
    /// on this. Getting it wrong is a bug in *both* directions, which is
    /// why the derivation is per-scheme rather than a hash of something
    /// convenient:
    ///
    /// - **too broad** — one authorization yields several identities, so
    ///   a genuine replay is missed. A security failure.
    /// - **too narrow** — two unrelated authorizations collide, so the
    ///   second is refused as a replay. A liveness failure, and on a
    ///   payment path a spurious refusal is a real harm.
    ///
    /// This used to hash the whole parsed `PaymentPayload` wrapper. That
    /// is too broad: `resource`, `extensions`, and any tolerated extra
    /// field inside `payload` are **not covered by the scheme's own
    /// signature**, so re-wrapping one signed authorization with a
    /// different `resource` produced a different key and slipped past the
    /// guard. Canonicalization fixes whitespace and key order; it cannot
    /// fix "these bytes were never signed in the first place".
    ///
    /// The identity is therefore built from the signed material only, and
    /// **fully namespaced**:
    ///
    /// ```text
    /// scheme + network + asset/contract + scheme-specific authorization identity
    /// ```
    ///
    /// The namespace is not padding. An EIP-3009 nonce is unique only
    /// within one token contract's `authorizationState[authorizer][nonce]`
    /// mapping, so the same wallet may legitimately reuse a nonce on a
    /// different token or a different chain — keying on `(from, nonce)`
    /// alone would let a USDC payment block an unrelated payment in
    /// another asset. The same reasoning applies to an XRPL account
    /// sequence, scoped per account *per network*.
    ///
    /// Fails closed: a scheme with no defined identity is an error the
    /// caller turns into a rejection, never a fallback to hashing the
    /// wrapper.
    pub fn replay_key(&self) -> Result<String, X402Error> {
        let view = self.view();
        let accepted = &view.accepted;
        let namespace = accepted.network.split(':').next().unwrap_or_default();
        let scheme = accepted.scheme.as_str();

        // The scheme-specific part: the material the authorization is
        // actually unique by, and which its own signature covers.
        let identity: Vec<Vec<u8>> = match (scheme, namespace) {
            ("exact", "eip155") => {
                // EIP-3009: the authorizer and the nonce, both inside the
                // signed typed-data message.
                let auth = view.payload.get("authorization").ok_or_else(|| {
                    X402Error::Invalid(
                        "exact-eip155 payload carries no `authorization` — no replay identity"
                            .into(),
                    )
                })?;
                let from = auth.get("from").and_then(|v| v.as_str()).ok_or_else(|| {
                    X402Error::Invalid("exact-eip155 authorization carries no `from`".into())
                })?;
                let nonce = auth.get("nonce").and_then(|v| v.as_str()).ok_or_else(|| {
                    X402Error::Invalid("exact-eip155 authorization carries no `nonce`".into())
                })?;
                // Addresses and hex nonces are case-insensitive on the wire
                // (EIP-55 checksums vary), so normalize — otherwise a
                // re-cased resubmission would mint a fresh identity.
                vec![
                    from.to_ascii_lowercase().into_bytes(),
                    nonce.to_ascii_lowercase().into_bytes(),
                ]
            }
            ("exact", "solana") => {
                // The partially-signed versioned transaction. Hash the
                // base64-DECODED bytes, not the string: those are the
                // transaction, and they are stable across any re-encoding
                // of the JSON around them.
                let tx = view
                    .payload
                    .get("transaction")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        X402Error::Invalid(
                            "exact-solana payload carries no `transaction` — no replay identity"
                                .into(),
                        )
                    })?;
                let raw = base64::engine::general_purpose::STANDARD
                    .decode(tx)
                    .map_err(|e| {
                        X402Error::Invalid(format!("exact-solana transaction is not base64: {e}"))
                    })?;
                vec![raw]
            }
            ("exact", "xrpl") => {
                // The presigned Payment blob, hex-decoded for the same
                // reason: the bytes are the transaction, the hex spelling
                // is not.
                let blob = view
                    .payload
                    .get("signedTxBlob")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        X402Error::Invalid(
                            "exact-xrpl payload carries no `signedTxBlob` — no replay identity"
                                .into(),
                        )
                    })?;
                let raw = hex::decode(blob).map_err(|e| {
                    X402Error::Invalid(format!("exact-xrpl signedTxBlob is not hex: {e}"))
                })?;
                vec![raw]
            }
            ("mock", "mock") => {
                // The chainless conformance scheme signs nothing, so its
                // whole scheme object is its authorization. Canonicalized
                // rather than raw, so the identity is still independent of
                // encoding — and taken from `payload` only, never the
                // wrapper, which is the point of this function.
                let bytes = crate::core::canonical::canonical_bytes(&view.payload)
                    .map_err(|e| {
                        X402Error::Invalid(format!("mock payload not canonicalizable: {e}"))
                    })?;
                vec![bytes]
            }
            _ => {
                return Err(X402Error::Invalid(format!(
                    "no replay identity defined for scheme `{scheme}` on `{}` — refusing to fall \
                     back to hashing the payload wrapper, which would let an unsigned field mint \
                     a fresh identity for one authorization",
                    accepted.network
                )))
            }
        };

        let mut hasher = blake3::Hasher::new();
        hasher.update(b"net.payments.replay_identity@2");
        // Namespace first: scheme, network, asset. Length-prefixed so no
        // concatenation of parts can be confused for another.
        let mut parts: Vec<&[u8]> = vec![
            scheme.as_bytes(),
            accepted.network.as_bytes(),
            accepted.asset.as_bytes(),
        ];
        for part in &identity {
            parts.push(part);
        }
        for part in parts {
            hasher.update(&(part.len() as u64).to_le_bytes());
            hasher.update(part);
        }
        Ok(hex::encode(hasher.finalize().as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::x402::X402Carry;

    const FIXTURE: &str = r#"{
  "x402Version": 2,
  "accepted": {
    "scheme": "exact",
    "network": "eip155:84532",
    "amount": "10000",
    "asset": "0x036CbD53842c5426634e7929541eC2318f3dCF7e",
    "payTo": "0x209693Bc6afc0C5328bA36FaF03C514EF312287C",
    "maxTimeoutSeconds": 60
  },
  "payload": {
    "signature": "0xdeadbeef",
    "authorization": {
      "from": "0xPayer", "to": "0xPayee", "value": "10000",
      "validAfter": "1740672089", "validBefore": "1740672154",
      "nonce": "0xf3746613c2d920b5fdabc0856f2aeb2d4f88ee6037b8cc5d04a71a4462f13480"
    }
  }
}"#;

    #[test]
    fn parses_and_validates_v2_payload() {
        let carry: X402Carry<PaymentPayload> =
            X402Carry::from_bytes(FIXTURE.as_bytes().to_vec()).unwrap();
        assert_eq!(carry.view().x402_version, 2);
        assert_eq!(carry.view().accepted.amount, "10000");
        assert_eq!(carry.bytes(), FIXTURE.as_bytes());
    }

    #[test]
    fn rejects_wrong_version() {
        let v1 = FIXTURE.replace("\"x402Version\": 2", "\"x402Version\": 1");
        let err = X402Carry::<PaymentPayload>::from_bytes(v1.into_bytes()).unwrap_err();
        assert_eq!(
            err,
            X402Error::UnsupportedX402Version {
                got: 1,
                expected: 2
            }
        );
    }

    #[test]
    fn rejects_non_object_scheme_payload() {
        let bad = FIXTURE.replace(
            "\"payload\": {\n    \"signature\": \"0xdeadbeef\",",
            "\"payload\": \"oops\", \"ignored\": {\"signature\": \"0xdeadbeef\",",
        );
        assert!(X402Carry::<PaymentPayload>::from_bytes(bad.into_bytes()).is_err());
    }

    /// M5: the replay identity is built from the scheme's **signed**
    /// material, so mutating an unsigned wrapper field cannot mint a
    /// fresh one.
    ///
    /// `resource` and `extensions` are not covered by the EIP-3009
    /// signature — only the `authorization` tuple is. Keying replay on
    /// the whole wrapper meant one signed authorization had unboundedly
    /// many identities: re-wrap it with a different `resource` and the
    /// engine's "one payload satisfies exactly one quote" guard saw
    /// something new.
    #[test]
    fn wrapper_fields_cannot_mint_a_fresh_replay_identity() {
        let base: X402Carry<PaymentPayload> =
            X402Carry::from_bytes(FIXTURE.as_bytes().to_vec()).unwrap();

        // Same authorization, different unsigned wrapper fields.
        let rewrapped = FIXTURE.replace(
            "\"x402Version\": 2,",
            "\"x402Version\": 2, \"resource\": {\"url\": \"https://elsewhere.example\"},",
        );
        let rewrapped: X402Carry<PaymentPayload> =
            X402Carry::from_bytes(rewrapped.into_bytes()).unwrap();

        assert_ne!(
            base.content_hash(),
            rewrapped.content_hash(),
            "the preserved bytes really do differ"
        );
        assert_eq!(
            base.replay_key().unwrap(),
            rewrapped.replay_key().unwrap(),
            "an unsigned wrapper field must not change the replay identity"
        );
    }

    /// The other direction: the identity is **namespaced**, so the same
    /// EIP-3009 nonce on a different token or a different chain is a
    /// different payment and must not collide.
    ///
    /// An EIP-3009 nonce is unique only within one contract's
    /// `authorizationState[authorizer][nonce]` mapping, so a wallet may
    /// legitimately reuse one elsewhere. Keying on `(from, nonce)` alone
    /// would let a USDC payment block an unrelated payment in another
    /// asset — a spurious refusal, which on a payment path is a real
    /// harm.
    #[test]
    fn the_same_nonce_on_another_asset_or_chain_is_a_different_identity() {
        let base: X402Carry<PaymentPayload> =
            X402Carry::from_bytes(FIXTURE.as_bytes().to_vec()).unwrap();

        // Same authorization, different token contract.
        let other_asset = FIXTURE.replace(
            "\"asset\": \"0x036CbD53842c5426634e7929541eC2318f3dCF7e\"",
            "\"asset\": \"0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913\"",
        );
        let other_asset: X402Carry<PaymentPayload> =
            X402Carry::from_bytes(other_asset.into_bytes()).unwrap();

        // Same authorization, different chain.
        let other_chain = FIXTURE.replace("\"network\": \"eip155:84532\"", "\"network\": \"eip155:8453\"");
        let other_chain: X402Carry<PaymentPayload> =
            X402Carry::from_bytes(other_chain.into_bytes()).unwrap();

        let base_key = base.replay_key().unwrap();
        assert_ne!(
            base_key,
            other_asset.replay_key().unwrap(),
            "the same nonce on another token is a different payment"
        );
        assert_ne!(
            base_key,
            other_chain.replay_key().unwrap(),
            "the same nonce on another chain is a different payment"
        );
    }

    /// A scheme with no defined identity fails closed rather than falling
    /// back to hashing the wrapper.
    #[test]
    fn an_unknown_scheme_has_no_replay_identity() {
        let unknown = FIXTURE
            .replace("\"scheme\": \"exact\"", "\"scheme\": \"future-scheme\"")
            .replace("\"network\": \"eip155:84532\"", "\"network\": \"future:1\"");
        let carry: X402Carry<PaymentPayload> =
            X402Carry::from_bytes(unknown.into_bytes()).unwrap();
        let err = carry.replay_key().unwrap_err();
        assert!(
            format!("{err}").contains("no replay identity"),
            "must fail closed, got {err}"
        );
    }

    /// M2: the replay key ignores serialization. Two byte-different
    /// encodings of the same authorization must collapse to one replay
    /// identity even though their preserved-byte content hashes differ.
    #[test]
    fn replay_key_is_encoding_agnostic() {
        // A re-encoding of FIXTURE: keys reordered inside `authorization`
        // and `accepted`, extra whitespace — identical logical content.
        const REENCODED: &str = r#"{
  "payload": {
    "authorization": {
      "nonce": "0xf3746613c2d920b5fdabc0856f2aeb2d4f88ee6037b8cc5d04a71a4462f13480",
      "validBefore": "1740672154", "validAfter": "1740672089",
      "value": "10000", "to": "0xPayee", "from": "0xPayer"
    },
    "signature": "0xdeadbeef"
  },
  "accepted": {
    "maxTimeoutSeconds": 60,
    "payTo": "0x209693Bc6afc0C5328bA36FaF03C514EF312287C",
    "asset": "0x036CbD53842c5426634e7929541eC2318f3dCF7e",
    "amount": "10000", "network": "eip155:84532", "scheme": "exact"
  },
  "x402Version": 2
}"#;

        let a: X402Carry<PaymentPayload> =
            X402Carry::from_bytes(FIXTURE.as_bytes().to_vec()).unwrap();
        let b: X402Carry<PaymentPayload> =
            X402Carry::from_bytes(REENCODED.as_bytes().to_vec()).unwrap();

        // Preserved bytes differ — so the old byte-keyed index would have
        // treated these as two distinct payloads.
        assert_ne!(a.content_hash(), b.content_hash());
        // But the canonical replay identity is one and the same.
        assert_eq!(a.replay_key().unwrap(), b.replay_key().unwrap());
    }
}
