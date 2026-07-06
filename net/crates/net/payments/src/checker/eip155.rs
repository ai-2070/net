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

use super::{ChainChecker, ChainVerdict, CheckerError, TransferQuery};
use crate::core::verification::{VerificationTier, VerifierRef};

/// keccak256("Transfer(address,address,uint256)").
const TRANSFER_TOPIC: &str = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";

/// The JSON-RPC checker for one eip155 network.
pub struct Eip155Checker {
    rpc_endpoint: String,
    network: String,
    final_depth: u64,
    http: reqwest::Client,
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
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| CheckerError::terminal(format!("http client: {e}")))?;
        Ok(Self {
            rpc_endpoint: rpc_endpoint.into(),
            network,
            final_depth: 12,
            http,
        })
    }

    /// Depth at which `Confirmed(n)` becomes `Final` (default 12; pick
    /// per network posture in the config pack).
    pub fn with_final_depth(mut self, final_depth: u64) -> Self {
        self.final_depth = final_depth.max(1);
        self
    }

    async fn rpc(&self, method: &str, params: Value) -> Result<Value, CheckerError> {
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
        let response = self
            .http
            .post(&self.rpc_endpoint)
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() || e.is_connect() {
                    CheckerError::retryable(e.to_string())
                } else {
                    CheckerError::terminal(e.to_string())
                }
            })?;
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|e| CheckerError::retryable(e.to_string()))?;
        if !status.is_success() {
            return Err(if status.is_server_error() {
                CheckerError::retryable(format!("{method} -> {status}"))
            } else {
                CheckerError::terminal(format!("{method} -> {status}"))
            });
        }
        let envelope: Value = serde_json::from_slice(&bytes)
            .map_err(|e| CheckerError::terminal(format!("{method} decode: {e}")))?;
        if let Some(error) = envelope.get("error") {
            return Err(CheckerError::terminal(format!(
                "{method} rpc error: {error}"
            )));
        }
        Ok(envelope.get("result").cloned().unwrap_or(Value::Null))
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
            endpoint: format!("independent-chain-check:{}", self.rpc_endpoint),
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

        let receipt = self
            .rpc("eth_getTransactionReceipt", json!([transaction]))
            .await?;
        if receipt.is_null() {
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
                let mut total: u128 = 0;
                for log in receipt["logs"].as_array().into_iter().flatten() {
                    let emitter = log["address"].as_str().unwrap_or_default();
                    let topics: Vec<&str> = log["topics"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .collect();
                    if emitter.eq_ignore_ascii_case(&q.token)
                        && topics.len() >= 3
                        && topics[0].eq_ignore_ascii_case(TRANSFER_TOPIC)
                        && topic_is_address(topics[2], &q.to)
                    {
                        let value =
                            parse_hex_u128(log["data"].as_str().unwrap_or("0x0"), "log.data")?;
                        total = total.saturating_add(value);
                    }
                }
                Some(total.to_string())
            }
            None => None,
        };

        Ok(ChainVerdict::Included { tier, delivered })
    }
}
