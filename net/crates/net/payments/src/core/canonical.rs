//! The payments canonical-encoding regime.
//!
//! Every Net payment envelope has exactly one canonical byte encoding,
//! byte-identical across languages and pinned by the
//! `tests/cross_lang_payments/` golden vectors:
//!
//! - one JSON object, **all keys sorted bytewise** (known and unknown
//!   fields alike — so a reader's schema knowledge never changes the byte
//!   layout, which is what makes additive-within-version signatures
//!   survive old readers);
//! - compact separators (`,` and `:`), UTF-8, no trailing newline;
//! - strings escaped exactly as serde_json does (minimal escaping, raw
//!   UTF-8 — verifiers in Python use `ensure_ascii=False`);
//! - numbers are integers or booleans only — **floats are rejected**, the
//!   money path has none;
//! - x402 documents appear only as base64 strings of their preserved
//!   bytes ([`crate::x402::X402Carry`]), never as nested JSON;
//! - signatures are hex strings ([`SignatureHex`]) covering the canonical
//!   bytes of the envelope **with the `signature` key absent**.
//!
//! Unknown-field preservation is structural for envelope fields (captured,
//! re-emitted deterministically, covered by the signature) and byte-exact
//! for x402 carries.

use std::collections::BTreeMap;

use net::adapter::net::identity::{EntityId, EntityKeypair};
use serde::{Deserialize, Serialize};

/// Errors from canonical encoding, signing, and verification.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum EnvelopeError {
    #[error("envelope does not canonicalize: {0}")]
    Encoding(String),
    #[error("floats are not representable in payment envelopes (got {0})")]
    Float(f64),
    #[error("envelope is unsigned")]
    Unsigned,
    #[error("envelope signature does not verify against the signer identity")]
    BadSignature,
    #[error("signing failed: {0}")]
    Signing(String),
    #[error("envelope object tag mismatch: {0}")]
    Tag(#[from] super::versioning::VersionError),
    #[error("envelope field invalid: {0}")]
    Field(String),
}

/// A detached ed25519 signature carried in an envelope, hex on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignatureHex(pub [u8; 64]);

impl Serialize for SignatureHex {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&hex::encode(self.0))
    }
}

impl<'de> Deserialize<'de> for SignatureHex {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        let bytes = hex::decode(&s).map_err(serde::de::Error::custom)?;
        let arr: [u8; 64] = bytes
            .try_into()
            .map_err(|_| serde::de::Error::custom("signature must be 64 bytes"))?;
        Ok(Self(arr))
    }
}

/// Unknown envelope fields, preserved across decode/encode. `BTreeMap`
/// (not `serde_json::Map`) so key order is sorted regardless of feature
/// unification elsewhere in a build graph (`preserve_order`).
pub type ExtraFields = BTreeMap<String, serde_json::Value>;

/// Canonical bytes of any serializable envelope value.
///
/// Serializes to a `serde_json::Value`, then emits it with the canonical
/// writer (sorted keys, compact, floats rejected).
pub fn canonical_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, EnvelopeError> {
    let value = serde_json::to_value(value).map_err(|e| EnvelopeError::Encoding(e.to_string()))?;
    let mut out = Vec::with_capacity(256);
    write_canonical(&value, &mut out)?;
    Ok(out)
}

/// Canonical bytes with the top-level `signature` key removed — the byte
/// string envelope signatures cover.
pub fn signed_payload_bytes<T: Serialize>(envelope: &T) -> Result<Vec<u8>, EnvelopeError> {
    let mut value =
        serde_json::to_value(envelope).map_err(|e| EnvelopeError::Encoding(e.to_string()))?;
    match value.as_object_mut() {
        Some(map) => {
            map.remove("signature");
        }
        None => {
            return Err(EnvelopeError::Encoding(
                "envelope must serialize to a JSON object".into(),
            ))
        }
    }
    let mut out = Vec::with_capacity(256);
    write_canonical(&value, &mut out)?;
    Ok(out)
}

fn write_canonical(value: &serde_json::Value, out: &mut Vec<u8>) -> Result<(), EnvelopeError> {
    use serde_json::Value;
    match value {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(true) => out.extend_from_slice(b"true"),
        Value::Bool(false) => out.extend_from_slice(b"false"),
        Value::Number(n) => {
            if let Some(u) = n.as_u64() {
                out.extend_from_slice(u.to_string().as_bytes());
            } else if let Some(i) = n.as_i64() {
                out.extend_from_slice(i.to_string().as_bytes());
            } else {
                // The money path has no floats; a float here is a schema
                // bug upstream, not something to encode "as best we can".
                return Err(EnvelopeError::Float(n.as_f64().unwrap_or(f64::NAN)));
            }
        }
        Value::String(s) => {
            let escaped =
                serde_json::to_vec(s).map_err(|e| EnvelopeError::Encoding(e.to_string()))?;
            out.extend_from_slice(&escaped);
        }
        Value::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_canonical(item, out)?;
            }
            out.push(b']');
        }
        Value::Object(map) => {
            // Sort bytewise regardless of the Map backing (BTreeMap today,
            // but insertion-ordered if `preserve_order` ever unifies in).
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
            out.push(b'{');
            for (i, (key, item)) in entries.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                let escaped =
                    serde_json::to_vec(key).map_err(|e| EnvelopeError::Encoding(e.to_string()))?;
                out.extend_from_slice(&escaped);
                out.push(b':');
                write_canonical(item, out)?;
            }
            out.push(b'}');
        }
    }
    Ok(())
}

/// The provider-identity signing surface every signed payment envelope
/// implements. Signing covers [`signed_payload_bytes`]; verification uses
/// `verify_strict` semantics via [`EntityId::verify_bytes`].
pub trait SignedEnvelope: Serialize + Sized {
    /// The `net.….…@N` object tag this envelope carries.
    const OBJECT_TAG: &'static str;

    /// The identity that signs (and is accountable for) this envelope.
    fn signer(&self) -> &EntityId;
    /// The current signature, if signed.
    fn signature(&self) -> Option<&SignatureHex>;
    /// Install a signature.
    fn set_signature(&mut self, sig: SignatureHex);

    /// Sign with `keypair`, which must match [`Self::signer`].
    fn sign_with(&mut self, keypair: &EntityKeypair) -> Result<(), EnvelopeError> {
        if keypair.entity_id() != self.signer() {
            return Err(EnvelopeError::Signing(
                "keypair does not match the envelope's signer identity".into(),
            ));
        }
        let payload = signed_payload_bytes(self)?;
        let sig = keypair
            .try_sign(&payload)
            .map_err(|e| EnvelopeError::Signing(e.to_string()))?;
        self.set_signature(SignatureHex(sig.to_bytes()));
        Ok(())
    }

    /// Verify the signature against the signer identity. Fail-closed:
    /// unsigned is an error, not a pass.
    fn verify_signature(&self) -> Result<(), EnvelopeError> {
        let sig = self.signature().ok_or(EnvelopeError::Unsigned)?;
        let payload = signed_payload_bytes(self)?;
        self.signer()
            .verify_bytes(&payload, &sig.0)
            .map_err(|_| EnvelopeError::BadSignature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Serialize)]
    struct Sample {
        b: u64,
        a: &'static str,
        #[serde(flatten)]
        extra: ExtraFields,
        signature: Option<SignatureHex>,
    }

    #[test]
    fn canonical_bytes_sort_all_keys_and_stay_compact() {
        let mut extra = ExtraFields::new();
        extra.insert(
            "zz_unknown".into(),
            serde_json::json!({"y": 1, "x": [true, null]}),
        );
        let s = Sample {
            b: 2,
            a: "hé\"llo",
            extra,
            signature: None,
        };
        let bytes = canonical_bytes(&s).unwrap();
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            r#"{"a":"hé\"llo","b":2,"signature":null,"zz_unknown":{"x":[true,null],"y":1}}"#
        );
    }

    #[test]
    fn signed_payload_excludes_the_signature_key() {
        let s = Sample {
            b: 1,
            a: "x",
            extra: ExtraFields::new(),
            signature: Some(SignatureHex([7u8; 64])),
        };
        let payload = signed_payload_bytes(&s).unwrap();
        assert_eq!(String::from_utf8(payload).unwrap(), r#"{"a":"x","b":1}"#);
    }

    #[test]
    fn floats_are_rejected_not_encoded() {
        let v = serde_json::json!({"price": 1.5});
        let mut out = Vec::new();
        assert!(matches!(
            write_canonical(&v, &mut out),
            Err(EnvelopeError::Float(_))
        ));
    }

    #[test]
    fn sign_and_verify_round_trip_with_entity_identity() {
        use net::adapter::net::identity::EntityKeypair;

        #[derive(Serialize)]
        struct Signed {
            object: &'static str,
            signer: EntityId,
            note: String,
            signature: Option<SignatureHex>,
        }
        impl SignedEnvelope for Signed {
            const OBJECT_TAG: &'static str = "net.test.sample@1";
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

        let kp = EntityKeypair::generate();
        let mut env = Signed {
            object: "net.test.sample@1",
            signer: kp.entity_id().clone(),
            note: "hello".into(),
            signature: None,
        };
        assert_eq!(env.verify_signature(), Err(EnvelopeError::Unsigned));
        env.sign_with(&kp).unwrap();
        env.verify_signature().unwrap();

        // Tamper → BadSignature.
        env.note = "tampered".into();
        assert_eq!(env.verify_signature(), Err(EnvelopeError::BadSignature));

        // Wrong keypair refuses to sign.
        let other = EntityKeypair::generate();
        let mut env2 = Signed {
            object: "net.test.sample@1",
            signer: kp.entity_id().clone(),
            note: "hello".into(),
            signature: None,
        };
        assert!(matches!(
            env2.sign_with(&other),
            Err(EnvelopeError::Signing(_))
        ));
    }
}
