//! The `exact` scheme on `eip155` networks: EIP-3009
//! `transferWithAuthorization`, signed as EIP-712 typed data.
//!
//! This module builds **documents**, nothing else:
//!
//! - the standard `eth_signTypedData_v4` typed-data JSON a signer
//!   (KMS / wallet / MPC / the dev signer) computes the digest of and
//!   signs — handing over the *structured* authorization, never a raw
//!   digest, so a policy-bearing signer can inspect the amount and
//!   recipient it is authorizing ("no raw signing of arbitrary bytes");
//! - the x402 exact-EVM `payload` object (`{signature, authorization}`)
//!   that travels inside the `PaymentPayload`.
//!
//! The EIP-712 domain comes from the requirements themselves: `name` /
//! `version` from `requirements.extra` (spec-carried token metadata),
//! `chainId` from the CAIP-2 network, `verifyingContract` from the
//! asset field. Present-and-wrong domain metadata produces signatures
//! the token contract rejects — the registry cross-check (WS4 packs)
//! catches mismatches before signing.

use serde_json::{json, Value};

use crate::x402::requirements::PaymentRequirements;
use crate::x402::X402Error;

/// The EIP-3009 authorization tuple. String-typed exactly as the x402
/// exact-EVM payload carries it (decimal strings for the uint256s,
/// 0x-hex for addresses and the 32-byte nonce).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExactEvmAuthorization {
    /// Payer address (`0x…`, the signer's).
    pub from: String,
    /// Recipient — must equal `requirements.payTo`.
    pub to: String,
    /// Atomic amount — must equal `requirements.amount`.
    pub value: String,
    /// Unix seconds from which the authorization is valid.
    pub valid_after: u64,
    /// Unix seconds at which it expires.
    pub valid_before: u64,
    /// 32-byte replay-prevention nonce, `0x…` hex.
    pub nonce: String,
}

/// The `eip155` chain id from a CAIP-2 network string.
pub fn chain_id(network: &str) -> Result<u64, X402Error> {
    let (namespace, reference) = network
        .split_once(':')
        .ok_or_else(|| X402Error::Invalid(format!("network `{network}` is not CAIP-2")))?;
    if namespace != "eip155" {
        return Err(X402Error::Invalid(format!(
            "exact-EVM authoring needs an eip155 network, got `{network}`"
        )));
    }
    reference.parse().map_err(|_| {
        X402Error::Invalid(format!("eip155 reference `{reference}` is not a chain id"))
    })
}

/// Build the `eth_signTypedData_v4` document for this authorization
/// against `requirements`. The signer receives THIS — domain, types,
/// and the full message — never a bare digest.
pub fn typed_data(
    requirements: &PaymentRequirements,
    auth: &ExactEvmAuthorization,
) -> Result<Value, X402Error> {
    if requirements.scheme != "exact" {
        return Err(X402Error::Invalid(format!(
            "exact-EVM authoring got scheme `{}`",
            requirements.scheme
        )));
    }
    let chain = chain_id(&requirements.network)?;
    let extra = requirements.extra.as_ref().ok_or_else(|| {
        X402Error::Invalid(
            "exact-EVM requirements carry no `extra` — the EIP-712 domain needs \
             the token's {name, version}"
                .to_string(),
        )
    })?;
    let name = extra.get("name").and_then(Value::as_str).ok_or_else(|| {
        X402Error::Invalid("requirements.extra.name (EIP-712 domain name) missing".to_string())
    })?;
    let version = extra
        .get("version")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            X402Error::Invalid(
                "requirements.extra.version (EIP-712 domain version) missing".to_string(),
            )
        })?;
    if auth.to != requirements.pay_to {
        return Err(X402Error::Invalid(
            "authorization recipient differs from requirements.payTo".to_string(),
        ));
    }
    if auth.value != requirements.amount {
        return Err(X402Error::Invalid(
            "authorization value differs from requirements.amount".to_string(),
        ));
    }

    Ok(json!({
        "types": {
            "EIP712Domain": [
                { "name": "name", "type": "string" },
                { "name": "version", "type": "string" },
                { "name": "chainId", "type": "uint256" },
                { "name": "verifyingContract", "type": "address" },
            ],
            "TransferWithAuthorization": [
                { "name": "from", "type": "address" },
                { "name": "to", "type": "address" },
                { "name": "value", "type": "uint256" },
                { "name": "validAfter", "type": "uint256" },
                { "name": "validBefore", "type": "uint256" },
                { "name": "nonce", "type": "bytes32" },
            ],
        },
        "primaryType": "TransferWithAuthorization",
        "domain": {
            "name": name,
            "version": version,
            "chainId": chain,
            "verifyingContract": requirements.asset,
        },
        "message": {
            "from": auth.from,
            "to": auth.to,
            "value": auth.value,
            "validAfter": auth.valid_after.to_string(),
            "validBefore": auth.valid_before.to_string(),
            "nonce": auth.nonce,
        },
    }))
}

/// Assemble the x402 exact-EVM `payload` object from the authorization
/// and its 65-byte signature (`0x…` hex, r‖s‖v).
pub fn payload_object(auth: &ExactEvmAuthorization, signature: &str) -> Value {
    json!({
        "signature": signature,
        "authorization": {
            "from": auth.from,
            "to": auth.to,
            "value": auth.value,
            "validAfter": auth.valid_after.to_string(),
            "validBefore": auth.valid_before.to_string(),
            "nonce": auth.nonce,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn requirements() -> PaymentRequirements {
        PaymentRequirements {
            scheme: "exact".into(),
            network: "eip155:84532".into(),
            amount: "10000".into(),
            asset: "0x036CbD53842c5426634e7929541eC2318f3dCF7e".into(),
            pay_to: "0x209693Bc6afc0C5328bA36FaF03C514EF312287C".into(),
            max_timeout_seconds: 60,
            extra: Some(serde_json::json!({ "name": "USDC", "version": "2" })),
        }
    }

    fn auth() -> ExactEvmAuthorization {
        ExactEvmAuthorization {
            from: "0x857b06519E91e3A54538791bDbb0E22373e36b66".into(),
            to: "0x209693Bc6afc0C5328bA36FaF03C514EF312287C".into(),
            value: "10000".into(),
            valid_after: 1_740_672_089,
            valid_before: 1_740_672_154,
            nonce: format!("0x{}", "11".repeat(32)),
        }
    }

    #[test]
    fn typed_data_builds_the_v4_document() {
        let doc = typed_data(&requirements(), &auth()).unwrap();
        assert_eq!(doc["primaryType"], "TransferWithAuthorization");
        assert_eq!(doc["domain"]["name"], "USDC");
        assert_eq!(doc["domain"]["version"], "2");
        assert_eq!(doc["domain"]["chainId"], 84532);
        assert_eq!(
            doc["domain"]["verifyingContract"],
            "0x036CbD53842c5426634e7929541eC2318f3dCF7e"
        );
        assert_eq!(doc["message"]["value"], "10000");
        assert_eq!(
            doc["types"]["TransferWithAuthorization"][5]["type"],
            "bytes32"
        );
    }

    #[test]
    fn missing_domain_metadata_and_mismatches_refuse_to_author() {
        let mut no_extra = requirements();
        no_extra.extra = None;
        assert!(typed_data(&no_extra, &auth()).is_err());

        let mut wrong_recipient = auth();
        wrong_recipient.to = "0xAttacker".into();
        assert!(typed_data(&requirements(), &wrong_recipient).is_err());

        let mut wrong_value = auth();
        wrong_value.value = "999999".into();
        assert!(typed_data(&requirements(), &wrong_value).is_err());

        let mut wrong_chain = requirements();
        wrong_chain.network = "solana:mainnet".into();
        assert!(typed_data(&wrong_chain, &auth()).is_err());
    }

    #[test]
    fn payload_object_matches_the_spec_shape() {
        let p = payload_object(&auth(), "0xsig");
        assert_eq!(p["signature"], "0xsig");
        assert_eq!(p["authorization"]["validBefore"], "1740672154");
        assert_eq!(
            p["authorization"]["nonce"],
            format!("0x{}", "11".repeat(32))
        );
    }
}
