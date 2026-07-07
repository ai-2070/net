//! The `solana` chain checker: JSON-RPC `getSignatureStatuses` +
//! `getTransaction` token-balance deltas against a participant-configured
//! RPC endpoint.
//!
//! Tier mapping (the adapter's one job): Solana's commitment ladder maps
//! directly — `processed` claims nothing ([`ChainVerdict::Pending`]),
//! `confirmed` is `Confirmed(n)` (n from the reported confirmation
//! count), and `finalized` is `Final`. Finality here is **deterministic**
//! (a rooted slot cannot revert), so unlike `eip155` there is no
//! depth-arithmetic posture to configure: the `final_depth` config knob
//! is deliberately unused by this adapter.
//!
//! Delivered-amount cross-check: SPL token-balance deltas from the
//! transaction meta (`postTokenBalances − preTokenBalances`), summed for
//! `(mint == token, owner == payTo)` — the amount **delivered**, straight
//! from the chain, robust through CPI (balances net out however the
//! transfer was routed). The `owner` field is the wallet, not the
//! associated token account, so multi-ATA recipients sum correctly.
//!
//! Payer binding: token balances are transaction-level facts (there is no
//! per-log `from` topic as on eip155), so the bind is transaction-level —
//! delivery counts only when the queried payer's own balance for the same
//! mint *decreased* in this transaction. A qualifying transfer funded by
//! a stranger sums to an honest zero, which the engine turns into an
//! amount-mismatch invalidation.

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{ChainChecker, ChainVerdict, CheckerError, TransferQuery};
use crate::core::verification::{VerificationTier, VerifierRef};

/// JSON-RPC responses (a parsed transaction with balance meta) are
/// bounded but can be large; cap so a malicious/compromised RPC cannot
/// stream a giant body within the timeout and exhaust memory.
const MAX_RPC_BODY: usize = 16 * 1024 * 1024;

/// The CAIP-2 `solana` reference is the genesis hash truncated to 32
/// base58 characters.
const CAIP2_SOLANA_REF_LEN: usize = 32;

/// The JSON-RPC checker for one `solana` network.
pub struct SvmChecker {
    rpc_endpoint: String,
    network: String,
    http: reqwest::Client,
    /// Set once the RPC's `getGenesisHash` has been confirmed to match
    /// `network`'s CAIP-2 reference — a swapped/typo'd endpoint (a devnet
    /// URL paired with the mainnet CAIP-2) must never validate a
    /// worthless tx as a real settlement. The `eip155` checker's
    /// `eth_chainId` twin.
    genesis_verified: std::sync::atomic::AtomicBool,
}

impl SvmChecker {
    /// A checker for `network` (CAIP-2, `solana:…`) against
    /// `rpc_endpoint`.
    pub fn new(
        network: impl Into<String>,
        rpc_endpoint: impl Into<String>,
    ) -> Result<Self, CheckerError> {
        let network = network.into();
        let Some(reference) = network.strip_prefix("solana:") else {
            return Err(CheckerError::terminal(format!(
                "SvmChecker got non-solana network `{network}`"
            )));
        };
        if reference.len() != CAIP2_SOLANA_REF_LEN {
            return Err(CheckerError::terminal(format!(
                "solana CAIP-2 reference `{reference}` is not {CAIP2_SOLANA_REF_LEN} \
                 base58 characters"
            )));
        }
        let tls = crate::tls_roots::tls_config()
            .map_err(|e| CheckerError::terminal(format!("http tls config: {e}")))?;
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .use_preconfigured_tls(tls)
            .build()
            .map_err(|e| CheckerError::terminal(format!("http client: {e}")))?;
        Ok(Self {
            rpc_endpoint: rpc_endpoint.into(),
            network,
            http,
            genesis_verified: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Build a checker for `network` straight from a facilitator config
    /// pack. Errors if the pack enables no RPC endpoint for `network`.
    /// A configured `final_depth` for the network is ignored — Solana
    /// finality is deterministic (`finalized` commitment), so there is
    /// no depth posture to carry.
    pub fn from_config(
        config: &crate::facilitator::config::FacilitatorConfig,
        network: &str,
    ) -> Result<Self, CheckerError> {
        let rpc = config.rpc_endpoints.get(network).ok_or_else(|| {
            CheckerError::terminal(format!(
                "facilitator config has no rpc endpoint for `{network}` — cannot check it"
            ))
        })?;
        Self::new(network, rpc)
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
        // Bound the body: a hostile RPC could otherwise stream unbounded.
        let mut response = response;
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| CheckerError::retryable(e.to_string()))?
        {
            if bytes.len().saturating_add(chunk.len()) > MAX_RPC_BODY {
                return Err(CheckerError::terminal(format!(
                    "{method} response exceeded the {MAX_RPC_BODY}-byte cap"
                )));
            }
            bytes.extend_from_slice(&chunk);
        }
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

    /// Confirm, once, that the RPC endpoint actually serves the CAIP-2
    /// chain the checker is configured for: the endpoint's genesis hash
    /// must start with the network's 32-character reference.
    async fn ensure_genesis(&self) -> Result<(), CheckerError> {
        use std::sync::atomic::Ordering;
        if self.genesis_verified.load(Ordering::Relaxed) {
            return Ok(());
        }
        // Validated non-empty in `new`.
        let expected = self.network.strip_prefix("solana:").unwrap_or_default();
        let genesis = self.rpc("getGenesisHash", json!([])).await?;
        let genesis = genesis.as_str().ok_or_else(|| {
            CheckerError::terminal("getGenesisHash did not return a string".to_string())
        })?;
        if genesis.get(..CAIP2_SOLANA_REF_LEN) != Some(expected) {
            return Err(CheckerError::terminal(format!(
                "RPC at {} serves genesis `{genesis}`, but the checker is configured for \
                 `{}` — refusing to validate against the wrong chain",
                self.rpc_endpoint, self.network
            )));
        }
        self.genesis_verified.store(true, Ordering::Relaxed);
        Ok(())
    }
}

/// Parse a `uiTokenAmount.amount` decimal string (raw units, u64 domain
/// on SPL) into u128 for the delta arithmetic.
fn parse_amount(v: &Value, what: &str) -> Result<u128, CheckerError> {
    let s = v
        .as_str()
        .ok_or_else(|| CheckerError::terminal(format!("{what} is not a string amount")))?;
    s.parse::<u128>()
        .map_err(|e| CheckerError::terminal(format!("{what} `{s}`: {e}")))
}

/// One account's `(mint, owner, pre, post)` balances, keyed by
/// `accountIndex`. Accounts created (no pre entry) or closed (no post
/// entry) in this transaction default the missing side to zero.
#[derive(Default)]
struct BalanceRow {
    mint: String,
    owner: String,
    pre: u128,
    post: u128,
}

fn fold_balances(
    rows: &mut std::collections::BTreeMap<u64, BalanceRow>,
    entries: &Value,
    post: bool,
    what: &str,
) -> Result<(), CheckerError> {
    for entry in entries.as_array().into_iter().flatten() {
        let Some(index) = entry["accountIndex"].as_u64() else {
            return Err(CheckerError::terminal(format!(
                "{what} entry carries no accountIndex"
            )));
        };
        let amount = parse_amount(&entry["uiTokenAmount"]["amount"], what)?;
        let row = rows.entry(index).or_default();
        // base58 is case-sensitive: mint/owner compare exactly, never
        // case-folded (unlike eip155 hex).
        row.mint = entry["mint"].as_str().unwrap_or_default().to_string();
        row.owner = entry["owner"].as_str().unwrap_or_default().to_string();
        if post {
            row.post = amount;
        } else {
            row.pre = amount;
        }
    }
    Ok(())
}

#[async_trait]
impl ChainChecker for SvmChecker {
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
        // Confirm (once) the RPC serves the chain we think it does before
        // trusting any status from it.
        self.ensure_genesis().await?;

        let statuses = self
            .rpc(
                "getSignatureStatuses",
                json!([[transaction], { "searchTransactionHistory": true }]),
            )
            .await?;
        let status = &statuses["value"][0];
        if status.is_null() {
            // Unknown signature: not-yet-landed, transient RPC lag, or an
            // expired/never-landed transaction — no confidence claim
            // either way, same doctrine as the eip155 missing-receipt arm.
            return Ok(ChainVerdict::Pending);
        }
        if !status["err"].is_null() {
            return Ok(ChainVerdict::Reverted);
        }
        let tier = match status["confirmationStatus"].as_str() {
            // `processed` is a single validator's view — reversible,
            // claims nothing.
            Some("processed") => return Ok(ChainVerdict::Pending),
            Some("confirmed") => {
                let n = status["confirmations"].as_u64().unwrap_or(1).max(1);
                VerificationTier::Confirmed(u32::try_from(n).unwrap_or(u32::MAX))
            }
            // Rooted: deterministic finality, no depth arithmetic.
            Some("finalized") => VerificationTier::Final,
            other => {
                return Err(CheckerError::terminal(format!(
                    "unknown confirmationStatus {other:?} — refusing to map it to a tier"
                )))
            }
        };

        // Delivered amount: SPL token-balance deltas for the queried
        // (mint, recipient) — straight from the chain.
        let delivered = match query {
            Some(q) => {
                let tx = self
                    .rpc(
                        "getTransaction",
                        json!([transaction, {
                            "encoding": "jsonParsed",
                            "commitment": "confirmed",
                            "maxSupportedTransactionVersion": 0,
                        }]),
                    )
                    .await?;
                if tx.is_null() {
                    // The status said included but the fetch missed (RPC
                    // lag between calls): claim nothing rather than a
                    // tier with an unverifiable amount.
                    return Ok(ChainVerdict::Pending);
                }
                if !tx["meta"]["err"].is_null() {
                    return Ok(ChainVerdict::Reverted);
                }
                let mut rows = std::collections::BTreeMap::new();
                fold_balances(
                    &mut rows,
                    &tx["meta"]["preTokenBalances"],
                    false,
                    "preTokenBalances",
                )?;
                fold_balances(
                    &mut rows,
                    &tx["meta"]["postTokenBalances"],
                    true,
                    "postTokenBalances",
                )?;

                let mut total: u128 = 0;
                let mut payer_debited = false;
                for row in rows.values() {
                    if row.mint != q.token {
                        continue;
                    }
                    if row.owner == q.to {
                        total = total.saturating_add(row.post.saturating_sub(row.pre));
                    }
                    if let Some(from) = q.from.as_deref() {
                        if row.owner == from && row.pre > row.post {
                            payer_debited = true;
                        }
                    }
                }
                // Transaction-level payer bind (balances carry no per-log
                // `from`): the queried payer must have funded this
                // transaction's transfer, or nothing was delivered *by
                // this quote's payer* — a stranger's payment to the same
                // merchant is an honest zero.
                if q.from.is_some() && !payer_debited {
                    total = 0;
                }
                Some(total.to_string())
            }
            None => None,
        };

        Ok(ChainVerdict::Included { tier, delivered })
    }
}
