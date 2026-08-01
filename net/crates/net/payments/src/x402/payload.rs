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

/// Canonical form of a hex value whose `0x` prefix and letter case carry
/// no meaning: lowercased, prefix stripped.
///
/// Used for the eip155 authorizer, nonce and contract address. All three
/// are treated as prefix-optional and case-insensitive elsewhere in the
/// stack (`is_eip3009_nonce` strips the prefix; the checker compares
/// addresses with `eq_ignore_ascii_case`), so two spellings of one
/// authorization must not become two replay identities.
fn hex_key(value: &str) -> String {
    let lowered = value.to_ascii_lowercase();
    lowered
        .strip_prefix("0x")
        .map(str::to_owned)
        .unwrap_or(lowered)
}

/// Canonicalize the `(network, asset)` scope a replay identity is keyed
/// under, so two spellings of one on-chain scope collapse to one key.
///
/// Only `eip155` is normalized, and only where a difference in spelling
/// provably is not a difference in meaning:
///
/// - the CAIP-2 reference is a decimal chain id, so `eip155:08453`
///   re-renders as `eip155:8453`;
/// - the asset is a hex contract address, so its `0x` prefix and EIP-55
///   checksum casing are stripped ([`hex_key`]).
///
/// Everything else passes through untouched. Solana mints are base58,
/// where case *is* significant and lowercasing would merge distinct
/// mints; XRPL's asset is a currency code. Normalizing those would trade
/// a missed replay for a false one, which on a payment path is the worse
/// failure.
fn canonical_eip_scope(network: &str, asset: &str) -> (String, String) {
    let Some(reference) = network.strip_prefix("eip155:") else {
        return (network.to_string(), asset.to_string());
    };
    // A non-numeric reference is not an eip155 chain id this build
    // understands; leave it alone rather than guess at a canonical form.
    let network = match reference.parse::<u64>() {
        Ok(chain_id) => format!("eip155:{chain_id}"),
        Err(_) => network.to_string(),
    };
    (network, hex_key(asset))
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
                // Normalize both spellings that carry no meaning: the
                // `0x` prefix is optional (`is_eip3009_nonce` and the
                // settlement signer both accept a bare-hex nonce), and
                // EIP-55 checksum casing varies. Without stripping the
                // prefix, `0xabc…` and `abc…` are the same authorization
                // with two replay identities — the same bypass re-casing
                // would give.
                vec![hex_key(from).into_bytes(), hex_key(nonce).into_bytes()]
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
                let bytes =
                    crate::core::canonical::canonical_bytes(&view.payload).map_err(|e| {
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

        // The namespace, canonicalized. Two spellings of the *same* scope
        // must not produce two identities, or one authorization satisfies
        // one quote per spelling.
        //
        // This is normalization toward a single on-chain meaning, not a
        // relaxation of CAIP's exact-comparison rule: `eip155:08453` and
        // `eip155:8453` are the same chain, and two EIP-55 casings of one
        // address are the same contract. The registry treats CAIP ids as
        // case-sensitive and that stays true — equivalence between
        // genuinely different assets is still registry policy. What is
        // handled here is only the spelling of one asset.
        let (scope_network, scope_asset) = canonical_eip_scope(&accepted.network, &accepted.asset);

        let mut hasher = blake3::Hasher::new();
        hasher.update(b"net.payments.replay_identity@2");
        // Namespace first: scheme, network, asset. Length-prefixed so no
        // concatenation of parts can be confused for another.
        let mut parts: Vec<&[u8]> = vec![
            scheme.as_bytes(),
            scope_network.as_bytes(),
            scope_asset.as_bytes(),
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
        let other_chain = FIXTURE.replace(
            "\"network\": \"eip155:84532\"",
            "\"network\": \"eip155:8453\"",
        );
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

    /// Two spellings of the same on-chain scope must be one replay
    /// identity, or one authorization satisfies one quote per spelling.
    ///
    /// EIP-55 checksum casing carries no on-chain meaning, and a CAIP-2
    /// reference is a decimal chain id, so `eip155:08453` and
    /// `eip155:8453` are the same chain. Neither difference is a
    /// difference in what is being paid.
    #[test]
    fn one_scope_spelled_two_ways_is_one_replay_identity() {
        let base: X402Carry<PaymentPayload> =
            X402Carry::from_bytes(FIXTURE.as_bytes().to_vec()).unwrap();

        // Same contract, different EIP-55 casing.
        let recased = FIXTURE.replace(
            "0x036CbD53842c5426634e7929541eC2318f3dCF7e",
            "0x036cbd53842c5426634e7929541ec2318f3dcf7e",
        );
        let recased: X402Carry<PaymentPayload> =
            X402Carry::from_bytes(recased.into_bytes()).unwrap();

        // Same chain, zero-padded reference.
        let padded = FIXTURE.replace("eip155:84532", "eip155:084532");
        let padded: X402Carry<PaymentPayload> = X402Carry::from_bytes(padded.into_bytes()).unwrap();

        let key = base.replay_key().unwrap();
        assert_eq!(
            key,
            recased.replay_key().unwrap(),
            "EIP-55 casing must not mint a second identity for one contract"
        );
        assert_eq!(
            key,
            padded.replay_key().unwrap(),
            "a zero-padded chain id must not mint a second identity"
        );
    }

    /// The `0x` prefix is optional everywhere else in the stack, so it
    /// must not be a way to mint a second replay identity for one
    /// authorization.
    ///
    /// `is_eip3009_nonce` accepts a bare-hex nonce and the settlement
    /// signer's `decode_bytes32` does too — so `0xabc…` and `abc…` are
    /// the same nonce to everything that matters. If the replay key
    /// disagreed, re-spelling the prefix would satisfy a second quote
    /// with one authorization.
    #[test]
    fn the_0x_prefix_is_not_a_second_replay_identity() {
        let base: X402Carry<PaymentPayload> =
            X402Carry::from_bytes(FIXTURE.as_bytes().to_vec()).unwrap();

        // Same authorization, nonce and payer written without `0x`.
        let bare = FIXTURE
            .replace(
                "\"nonce\": \"0xf3746613c2d920b5fdabc0856f2aeb2d4f88ee6037b8cc5d04a71a4462f13480\"",
                "\"nonce\": \"f3746613c2d920b5fdabc0856f2aeb2d4f88ee6037b8cc5d04a71a4462f13480\"",
            )
            .replace("\"from\": \"0xPayer\"", "\"from\": \"Payer\"");
        let bare: X402Carry<PaymentPayload> = X402Carry::from_bytes(bare.into_bytes()).unwrap();

        assert_ne!(
            base.content_hash(),
            bare.content_hash(),
            "the preserved bytes really do differ"
        );
        assert_eq!(
            base.replay_key().unwrap(),
            bare.replay_key().unwrap(),
            "an optional prefix must not mint a second identity"
        );

        // The asset spelling likewise.
        let bare_asset = FIXTURE.replace(
            "\"asset\": \"0x036CbD53842c5426634e7929541eC2318f3dCF7e\"",
            "\"asset\": \"036cbd53842c5426634e7929541ec2318f3dcf7e\"",
        );
        let bare_asset: X402Carry<PaymentPayload> =
            X402Carry::from_bytes(bare_asset.into_bytes()).unwrap();
        assert_eq!(
            base.replay_key().unwrap(),
            bare_asset.replay_key().unwrap(),
            "the contract address prefix must not mint a second identity"
        );
    }

    /// The normalization is scoped to eip155 and must not merge scopes
    /// that only *look* similar. Solana mints are base58, where case is
    /// significant — lowercasing them would turn a missed replay into a
    /// false one.
    #[test]
    fn non_eip155_scopes_are_not_case_folded() {
        let (net, asset) = canonical_eip_scope(
            "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp",
            "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
        );
        assert_eq!(net, "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp");
        assert_eq!(
            asset, "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
            "a base58 mint must survive untouched"
        );

        // eip155 with a non-numeric reference is left alone rather than
        // guessed at.
        let (net, _) = canonical_eip_scope("eip155:not-a-number", "0xAbC");
        assert_eq!(net, "eip155:not-a-number");

        // And the eip155 case really does normalize — chain id numerically,
        // contract address by case AND `0x` prefix.
        let (net, asset) = canonical_eip_scope("eip155:08453", "0xAbCdEf");
        assert_eq!(net, "eip155:8453");
        assert_eq!(asset, "abcdef");
        assert_eq!(canonical_eip_scope("eip155:1", "AbCdEf").1, "abcdef");
    }

    /// A scheme with no defined identity fails closed rather than falling
    /// back to hashing the wrapper.
    #[test]
    fn an_unknown_scheme_has_no_replay_identity() {
        let unknown = FIXTURE
            .replace("\"scheme\": \"exact\"", "\"scheme\": \"future-scheme\"")
            .replace("\"network\": \"eip155:84532\"", "\"network\": \"future:1\"");
        let carry: X402Carry<PaymentPayload> = X402Carry::from_bytes(unknown.into_bytes()).unwrap();
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
