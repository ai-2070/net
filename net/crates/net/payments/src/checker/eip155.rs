//! The `eip155` chain checker: JSON-RPC `eth_getTransactionReceipt` +
//! head-depth arithmetic against a participant-configured RPC endpoint.
//!
//! Tier mapping (the adapter's one job): a successful receipt at depth
//! `n` is `Confirmed(n)`; `n >= final_depth` (config, default 12) is
//! `Final`. A reverted receipt is [`ChainVerdict::Reverted`]; a missing
//! receipt is [`ChainVerdict::Pending`].
//!
//! Delivered-amount cross-check: ERC-20 `Transfer(address,address,
//! uint256)` logs emitted by the queried token contract to the queried
//! recipient, summed — the amount **delivered**, straight from the
//! chain, independent of what anyone reported.

use async_trait::async_trait;
use serde_json::{json, Value};

use super::transport::RpcTransport;
use super::{ChainChecker, ChainVerdict, CheckerError, TransferQuery};
use crate::core::verification::{VerificationTier, VerifierRef};

/// keccak256("Transfer(address,address,uint256)").
const TRANSFER_TOPIC: &str = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";

/// The JSON-RPC checker for one eip155 network.
pub struct Eip155Checker {
    transport: RpcTransport,
    network: String,
    final_depth: u64,
    /// Set once the RPC's `eth_chainId` has been confirmed to match
    /// `network`'s CAIP-2 reference — a swapped/typo'd endpoint (a
    /// testnet URL paired with a mainnet CAIP-2) must never validate a
    /// worthless tx as a real settlement.
    chain_id_verified: std::sync::atomic::AtomicBool,
}

impl Eip155Checker {
    /// A checker for `network` (CAIP-2, `eip155:…`) against
    /// `rpc_endpoint`.
    pub fn new(
        network: impl Into<String>,
        rpc_endpoint: impl Into<String>,
    ) -> Result<Self, CheckerError> {
        let network = network.into();
        if !network.starts_with("eip155:") {
            return Err(CheckerError::terminal(format!(
                "Eip155Checker got non-eip155 network `{network}`"
            )));
        }
        Ok(Self {
            transport: RpcTransport::new(rpc_endpoint)?,
            network,
            final_depth: 12,
            chain_id_verified: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Depth at which `Confirmed(n)` becomes `Final` (default 12; pick
    /// per network posture in the config pack).
    pub fn with_final_depth(mut self, final_depth: u64) -> Self {
        self.final_depth = final_depth.max(1);
        self
    }

    /// Build a checker for `network` straight from a facilitator config
    /// pack: RPC endpoint and (crucially) the network's configured
    /// `final_depth`, so an L2's L1-finalization posture actually reaches
    /// the checker instead of the L1-scale default. Errors if the pack
    /// enables no RPC endpoint for `network`.
    pub fn from_config(
        config: &crate::facilitator::config::FacilitatorConfig,
        network: &str,
    ) -> Result<Self, CheckerError> {
        let rpc = config.rpc_endpoints.get(network).ok_or_else(|| {
            CheckerError::terminal(format!(
                "facilitator config has no rpc endpoint for `{network}` — cannot check it"
            ))
        })?;
        let mut checker = Self::new(network, rpc)?;
        if let Some(depth) = config.final_depth(network) {
            checker = checker.with_final_depth(depth);
        }
        Ok(checker)
    }

    async fn rpc(&self, method: &str, params: Value) -> Result<Value, CheckerError> {
        // JSON-RPC 2.0 envelope: errors ride in a top-level `error` field.
        let envelope = self
            .transport
            .post(
                method,
                &json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }),
            )
            .await?;
        if let Some(error) = envelope.get("error") {
            return Err(CheckerError::terminal(format!(
                "{method} rpc error: {error}"
            )));
        }
        Ok(envelope.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Confirm, once, that the RPC endpoint actually serves the CAIP-2
    /// chain the checker is configured for. A swapped/typo'd RPC (e.g. a
    /// Base-Sepolia URL paired with `eip155:8453`) would otherwise
    /// validate a worthless testnet tx as a mainnet settlement.
    async fn ensure_chain_id(&self) -> Result<(), CheckerError> {
        use std::sync::atomic::Ordering;
        if self.chain_id_verified.load(Ordering::Relaxed) {
            return Ok(());
        }
        let expected: u64 = self
            .network
            .strip_prefix("eip155:")
            .and_then(|r| r.parse().ok())
            .ok_or_else(|| {
                CheckerError::terminal(format!(
                    "network `{}` carries no numeric eip155 chain id",
                    self.network
                ))
            })?;
        let reported = parse_hex_u64(&self.rpc("eth_chainId", json!([])).await?, "eth_chainId")?;
        if reported != expected {
            return Err(CheckerError::terminal(format!(
                "RPC at {} serves chain id {reported}, but the checker is configured for `{}` \
                 (chain {expected}) — refusing to validate against the wrong chain",
                self.transport.endpoint(),
                self.network
            )));
        }
        self.chain_id_verified.store(true, Ordering::Relaxed);
        Ok(())
    }
}

fn parse_hex_u64(v: &Value, what: &str) -> Result<u64, CheckerError> {
    let s = v
        .as_str()
        .ok_or_else(|| CheckerError::terminal(format!("{what} is not a hex string")))?;
    u64::from_str_radix(s.trim_start_matches("0x"), 16)
        .map_err(|e| CheckerError::terminal(format!("{what} `{s}`: {e}")))
}

fn parse_hex_u128(s: &str, what: &str) -> Result<u128, CheckerError> {
    let trimmed = s.trim_start_matches("0x").trim_start_matches('0');
    if trimmed.is_empty() {
        return Ok(0);
    }
    u128::from_str_radix(trimmed, 16)
        .map_err(|e| CheckerError::terminal(format!("{what} `{s}`: {e}")))
}

/// The `AuthorizationUsed(address indexed authorizer, bytes32 indexed
/// nonce)` event topic (EIP-3009 requires the token emit it on
/// `transferWithAuthorization`). Computed at runtime from the signature
/// — never a memorized constant on the money path.
fn authorization_used_topic() -> &'static str {
    use sha3::Digest as _;
    static TOPIC: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    TOPIC.get_or_init(|| {
        let digest = sha3::Keccak256::digest(b"AuthorizationUsed(address,bytes32)");
        format!("0x{}", hex::encode(digest))
    })
}

/// Is `s` a 32-byte hex word — the eip155 adapter's reference vocabulary
/// (an EIP-3009 nonce)? The `0x` prefix is optional: the settlement
/// signer's own `decode_bytes32` accepts a nonce with or without it, so
/// the checker must too — treating a bare-hex nonce as "not a nonce"
/// would silently skip the `AuthorizationUsed` bind (fail-open) on a
/// perfectly valid authorization.
fn is_nonce_hex(s: &str) -> bool {
    let h = s.strip_prefix("0x").unwrap_or(s);
    h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A 32-byte topic equals the 32-byte word `word` (both 0x-hex)?
fn topic_is_word(topic: &str, word: &str) -> bool {
    let topic = topic.trim_start_matches("0x");
    let word = word.trim_start_matches("0x");
    topic.len() == 64 && word.len() == 64 && topic.eq_ignore_ascii_case(word)
}

/// A 32-byte topic holding a left-padded address equals `addr`?
fn topic_is_address(topic: &str, addr: &str) -> bool {
    let topic = topic.trim_start_matches("0x");
    let addr = addr.trim_start_matches("0x");
    topic.len() == 64
        && addr.len() == 40
        && topic[24..].eq_ignore_ascii_case(addr)
        && topic[..24].chars().all(|c| c == '0')
}

#[async_trait]
impl ChainChecker for Eip155Checker {
    fn reference(&self) -> VerifierRef {
        VerifierRef {
            identity: None,
            endpoint: format!("independent-chain-check:{}", self.transport.endpoint()),
        }
    }

    async fn check(
        &self,
        network: &str,
        transaction: &str,
        query: Option<&TransferQuery>,
    ) -> Result<ChainVerdict, CheckerError> {
        if network != self.network {
            return Err(CheckerError::terminal(format!(
                "checker is configured for `{}`, asked about `{network}`",
                self.network
            )));
        }
        // Confirm (once) the RPC serves the chain we think it does before
        // trusting any receipt from it.
        self.ensure_chain_id().await?;

        let receipt = self
            .rpc("eth_getTransactionReceipt", json!([transaction]))
            .await?;
        if receipt.is_null() {
            // A missing receipt is ambiguous — not-yet-mined, transient RPC
            // lag, or reorged out after a prior confirmation — so it maps to
            // Pending (no answer), never an invalidation. Known limitation:
            // a settlement that was previously confirmed and then reorged
            // out degrades to Pending here rather than being flagged; the
            // engine keeps the last tier. Distinguishing reorg-out from lag
            // needs the checker to remember prior inclusion (stateful), out
            // of scope for this receipt-only check. An on-chain *revert*
            // (status 0) IS caught below.
            return Ok(ChainVerdict::Pending);
        }
        let status = parse_hex_u64(&receipt["status"], "receipt.status")?;
        if status == 0 {
            return Ok(ChainVerdict::Reverted);
        }
        let block = parse_hex_u64(&receipt["blockNumber"], "receipt.blockNumber")?;
        let head = parse_hex_u64(
            &self.rpc("eth_blockNumber", json!([])).await?,
            "blockNumber",
        )?;
        let confirmations = head.saturating_sub(block).saturating_add(1);
        let tier = if confirmations >= self.final_depth {
            VerificationTier::Final
        } else {
            VerificationTier::Confirmed(u32::try_from(confirmations).unwrap_or(u32::MAX))
        };

        // Delivered amount: sum the token's Transfer logs to the quoted
        // recipient — straight from the chain.
        let delivered = match query {
            Some(q) => {
                // Nonce binding (the H3 fix's recorded stronger follow-up):
                // when the quote threads its caller-signed EIP-3009 nonce as
                // the reference, the token contract must have emitted the
                // matching `AuthorizationUsed(authorizer, nonce)` in THIS
                // transaction — binding the settlement to this exact
                // authorization, not merely to (token, payer, recipient).
                // The eip155 adapter's reference vocabulary is a 32-byte hex
                // nonce (`0x` optional); any other reference shape belongs to
                // another adapter and is ignored here (the `to_tag`
                // convention).
                //
                // The emitter must be `q.token` itself. This is exactly right
                // for conforming EIP-3009 tokens, PROXIES INCLUDED: a proxy
                // (USDC) emits under the proxy address during the delegatecall
                // into its implementation, and that proxy address *is* the
                // quoted asset. A token that emits `AuthorizationUsed` from a
                // *different* contract than the quoted asset, or a
                // non-standard token that omits/renames the event, fails this
                // bind and zeroes out — intentional fail-closed: relaxing the
                // emitter to "any address" would let an unrelated contract's
                // event satisfy the bind. Widen the asset registry, don't
                // widen this check.
                let nonce_bound = match q.reference.as_deref().filter(|r| is_nonce_hex(r)) {
                    Some(nonce) => receipt["logs"].as_array().into_iter().flatten().any(|log| {
                        let topics: Vec<&str> = log["topics"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(Value::as_str)
                            .collect();
                        log["address"]
                            .as_str()
                            .unwrap_or_default()
                            .eq_ignore_ascii_case(&q.token)
                            && topics.len() >= 3
                            && topics[0].eq_ignore_ascii_case(authorization_used_topic())
                            && q.from
                                .as_deref()
                                .map(|from| topic_is_address(topics[1], from))
                                .unwrap_or(true)
                            && topic_is_word(topics[2], nonce)
                    }),
                    None => true,
                };
                let mut total: u128 = 0;
                for log in receipt["logs"].as_array().into_iter().flatten() {
                    let emitter = log["address"].as_str().unwrap_or_default();
                    let topics: Vec<&str> = log["topics"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .collect();
                    // Bind the transfer to (token, from-payer, to-recipient).
                    // topics[1] is the indexed `from`, topics[2] the indexed
                    // `to`. Without the `from` bind, a facilitator could point
                    // at *any* qualifying transfer to the merchant (e.g. a
                    // different customer's) and pass the delivered-amount
                    // check. When `q.from` is set, only transfers authorized by
                    // this quote's payer count.
                    let from_ok = match q.from.as_deref() {
                        Some(from) => topics.get(1).is_some_and(|t| topic_is_address(t, from)),
                        None => true,
                    };
                    if emitter.eq_ignore_ascii_case(&q.token)
                        && topics.len() >= 3
                        && topics[0].eq_ignore_ascii_case(TRANSFER_TOPIC)
                        && from_ok
                        && topic_is_address(topics[2], &q.to)
                    {
                        let value =
                            parse_hex_u128(log["data"].as_str().unwrap_or("0x0"), "log.data")?;
                        total = total.saturating_add(value);
                    }
                }
                // A settlement that never consumed THIS quote's
                // authorization delivered nothing for this quote — an
                // honest zero the engine turns into an amount-mismatch
                // invalidation.
                if !nonce_bound {
                    total = 0;
                }
                Some(total.to_string())
            }
            None => None,
        };

        Ok(ChainVerdict::Included { tier, delivered })
    }
}
