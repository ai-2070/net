//! The `xrpl` chain checker: rippled JSON-RPC `tx` lookup against a
//! participant-configured endpoint.
//!
//! Tier mapping (the adapter's one job): a **validated** XRPL ledger is
//! deterministically final, so the ladder is two-runged — not found /
//! unvalidated → [`ChainVerdict::Pending`]; validated `tesSUCCESS` →
//! `Final`. This adapter never emits `Confirmed(n)` and the
//! `final_depth` knob is meaningless here, exactly as on Solana. The
//! failure rule is **closed**: validated with `TransactionResult !=
//! "tesSUCCESS"` is [`ChainVerdict::Reverted`] with the code recorded in
//! the error path — `tec*` codes (fee burned, payment did not happen,
//! e.g. `tecNO_LINE` / `tecPATH_DRY`) are the expected family, but the
//! rule is the inequality, never a prefix match.
//!
//! Delivered-amount cross-check: **`meta.delivered_amount` and nothing
//! else** — `tx.Amount` is an upper bound under partial payments, the
//! classic XRPL integration exploit. Only the canonical field shape for
//! the pinned rippled API is accepted (a string of drops for XRP); a
//! `tesSUCCESS` Payment with the field missing delivers an honest zero.
//!
//! Satisfaction form (pinned): only a `TransactionType == "Payment"`
//! without `tfPartialPayment` counts — the flag is rejected even when
//! `delivered_amount` happens to equal the quote, because this checker
//! verifies settlements it did not author (facilitator/HTTP paths);
//! the authoring seam's unrepresentability covers only our own blobs.
//!
//! Binding: recipient `Destination` (+ `DestinationTag` when the quote
//! carries one), payer `Account == query.from` (H3 parity), and — the
//! strongest bind any rung has — the pinned **invoice binding**: when
//! the quote threads its `invoiceId`, the matched transaction must carry
//! `MemoData = HEX(UTF-8(invoiceId))` or `InvoiceID = SHA256(invoiceId)`.
//! A qualifying payment without this quote's binding sums to an honest
//! zero, which the engine turns into an amount-mismatch invalidation.
//!
//! Doctrine note: the *generic engine* never decodes XRPL data; this
//! adapter may inspect XRPL transaction JSON freely — chain-specific
//! machinery belongs here, not in `PaymentEngine`.

use async_trait::async_trait;
use serde_json::{json, Value};
use sha2::Digest as _;

use super::transport::RpcTransport;
use super::{ChainChecker, ChainVerdict, CheckerError, TransferQuery};
use crate::core::verification::{VerificationTier, VerifierRef};

/// XRPL `Flags` bit for a partial payment — not an accepted satisfaction
/// form for `exact`, whatever it delivered.
const TF_PARTIAL_PAYMENT: u64 = 0x0002_0000;

/// The JSON-RPC checker for one `xrpl` network.
pub struct XrplChecker {
    transport: RpcTransport,
    network: String,
    /// Set once the endpoint's `server_info.network_id` has been
    /// confirmed to match `network`'s CAIP-2 reference — a swapped
    /// testnet/devnet endpoint must never validate a worthless tx as a
    /// mainnet settlement. The `eth_chainId`/`getGenesisHash` twin.
    network_verified: std::sync::atomic::AtomicBool,
}

impl XrplChecker {
    /// A checker for `network` (CAIP-2, `xrpl:…` — `0` mainnet, `1`
    /// testnet, `2` devnet per the pinned-doc convention) against a
    /// rippled JSON-RPC `rpc_endpoint`.
    pub fn new(
        network: impl Into<String>,
        rpc_endpoint: impl Into<String>,
    ) -> Result<Self, CheckerError> {
        let network = network.into();
        let Some(reference) = network.strip_prefix("xrpl:") else {
            return Err(CheckerError::terminal(format!(
                "XrplChecker got non-xrpl network `{network}`"
            )));
        };
        if reference.parse::<u32>().is_err() {
            return Err(CheckerError::terminal(format!(
                "xrpl CAIP-2 reference `{reference}` is not a numeric network id"
            )));
        }
        Ok(Self {
            transport: RpcTransport::new(rpc_endpoint)?,
            network,
            network_verified: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Build a checker for `network` straight from a facilitator config
    /// pack. Errors if the pack enables no RPC endpoint for `network`.
    /// A configured `final_depth` is ignored — a validated XRPL ledger
    /// is deterministic finality; there is no depth posture to carry.
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

    /// One rippled JSON-RPC call. rippled's envelope differs from
    /// Ethereum's: errors ride *inside* `result`
    /// (`result.status == "error"`, `result.error = "txnNotFound"`), so
    /// this returns the raw `result` and the caller maps rippled error
    /// codes — only transport/HTTP/shape failures error here.
    async fn rpc(&self, method: &str, params: Value) -> Result<Value, CheckerError> {
        // rippled wraps params in a single-element array and rides errors
        // *inside* `result`, so return the raw `result` for the caller to
        // map — the transport only surfaces transport/HTTP/shape failures.
        let envelope = self
            .transport
            .post(method, &json!({ "method": method, "params": [params] }))
            .await?;
        Ok(envelope.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Confirm, once, that the endpoint serves the CAIP-2 network the
    /// checker is configured for: `server_info.info.network_id` must
    /// equal the reference. Legacy mainnet servers may omit the field —
    /// tolerated only when the expected id is 0 (mainnet); a testnet or
    /// devnet checker requires the explicit id.
    async fn ensure_network(&self) -> Result<(), CheckerError> {
        use std::sync::atomic::Ordering;
        if self.network_verified.load(Ordering::Relaxed) {
            return Ok(());
        }
        // Validated numeric in `new`.
        let expected: u64 = self
            .network
            .strip_prefix("xrpl:")
            .unwrap_or_default()
            .parse()
            .unwrap_or(u64::MAX);
        let info = self.rpc("server_info", json!({})).await?;
        if info["status"].as_str() == Some("error") {
            return Err(CheckerError::terminal(format!(
                "server_info error: {}",
                info["error"].as_str().unwrap_or("unspecified")
            )));
        }
        match info["info"]["network_id"].as_u64() {
            Some(reported) if reported == expected => {}
            None if expected == 0 => {
                // Legacy mainnet rippled omits network_id; mainnet-only
                // tolerance, documented above.
            }
            Some(reported) => {
                return Err(CheckerError::terminal(format!(
                    "RPC at {} serves network_id {reported}, but the checker is configured \
                     for `{}` — refusing to validate against the wrong chain",
                    self.transport.endpoint(),
                    self.network
                )));
            }
            None => {
                return Err(CheckerError::terminal(format!(
                    "RPC at {} reports no network_id and the checker expects `{}` — \
                     refusing a non-mainnet check without an explicit id",
                    self.transport.endpoint(),
                    self.network
                )));
            }
        }
        self.network_verified.store(true, Ordering::Relaxed);
        Ok(())
    }
}

/// The object carrying the transaction-level fields (`Account`,
/// `Destination`, `TransactionType`, `Flags`, `Memos`, `InvoiceID`, …).
/// api_version 1 places them at the top of `result`; api_version 2 and
/// Clio nest them under `tx_json`. We pin api_version 1 on the request, but
/// resolve `tx_json` when present so a server that returns the v2 shape
/// (ignoring the pin) still verifies correctly rather than reading every
/// field as absent. `meta` and `validated` are `result`-level in both.
fn tx_fields(tx: &Value) -> &Value {
    match tx.get("tx_json") {
        Some(inner) if inner.is_object() => inner,
        _ => tx,
    }
}

/// Does the validated transaction carry this quote's invoice binding —
/// `MemoData = HEX(UTF-8(invoiceId))` (method A) or
/// `InvoiceID = SHA256(invoiceId)` (method B), per the pinned doc? `tx` is
/// the transaction-fields object (see [`tx_fields`]).
fn invoice_bound(tx: &Value, invoice_id: &str) -> bool {
    let memo_hex = hex::encode(invoice_id.as_bytes());
    let memo_match = tx["Memos"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["Memo"]["MemoData"].as_str())
        .any(|data| data.eq_ignore_ascii_case(&memo_hex));
    if memo_match {
        return true;
    }
    let digest = sha2::Sha256::digest(invoice_id.as_bytes());
    let invoice_hex = hex::encode(digest);
    tx["InvoiceID"]
        .as_str()
        .is_some_and(|id| id.eq_ignore_ascii_case(&invoice_hex))
}

#[async_trait]
impl ChainChecker for XrplChecker {
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
        // Confirm (once) the endpoint serves the network we think it
        // does before trusting any answer from it.
        self.ensure_network().await?;

        // Pin `api_version: 1`: rippled api_version 1 returns the
        // transaction fields (Account, Destination, TransactionType, …) at
        // the top of `result`, which is the shape this adapter reads. Under
        // api_version 2 (and Clio's default) those fields nest under
        // `tx_json`; pinning keeps the shape stable across endpoints, and
        // `tx_fields` below tolerates the v2 shape anyway for a server that
        // ignores the pin. (`meta`/`validated` stay at `result` level in
        // both versions.)
        let tx = self
            .rpc(
                "tx",
                json!({ "transaction": transaction, "binary": false, "api_version": 1 }),
            )
            .await?;
        if tx["status"].as_str() == Some("error") {
            return match tx["error"].as_str() {
                // Unknown signature: not-yet-landed, RPC lag, or an
                // expired blob that never landed — no confidence claim
                // either way. (XRPL could prove never-included via
                // LastLedgerSequence, but ChainVerdict has no vocabulary
                // for it; the conservative mapping stays Pending and the
                // engine's in-flight TTL unsticks the flow.)
                Some("txnNotFound") => Ok(ChainVerdict::Pending),
                other => Err(CheckerError::terminal(format!(
                    "tx error: {}",
                    other.unwrap_or("unspecified")
                ))),
            };
        }
        // Candidate ledgers revert: only a validated result claims
        // anything.
        if tx["validated"].as_bool() != Some(true) {
            return Ok(ChainVerdict::Pending);
        }
        // Closed failure rule: anything other than tesSUCCESS in a
        // validated ledger is a first-class invalidation (tec* — fee
        // burned, payment did not happen — is the expected family, but
        // the rule is the inequality).
        match tx["meta"]["TransactionResult"].as_str() {
            Some("tesSUCCESS") => {}
            Some(_code) => return Ok(ChainVerdict::Reverted),
            None => {
                return Err(CheckerError::terminal(
                    "validated tx carries no meta.TransactionResult".to_string(),
                ))
            }
        }

        // A validated XRPL ledger is deterministically final.
        let tier = VerificationTier::Final;

        let delivered = match query {
            Some(q) => {
                // Transaction-level fields live at `result` top level under
                // api_version 1 (pinned) or under `tx_json` under v2/Clio.
                let f = tx_fields(&tx);
                // Satisfaction form: a direct full Payment. Anything
                // else — another transaction type, or a partial payment
                // (rejected even when it delivered the full amount; this
                // checker verifies settlements it did not author) —
                // contributes an honest zero.
                let is_payment = f["TransactionType"].as_str() == Some("Payment");
                let partial = f["Flags"].as_u64().unwrap_or(0) & TF_PARTIAL_PAYMENT != 0;
                // Binds: recipient (+ tag when the quote carries one),
                // payer (H3), and the pinned invoice binding when the
                // quote threads its reference.
                let to_ok = f["Destination"].as_str() == Some(q.to.as_str());
                let tag_ok = match q.to_tag {
                    Some(expected) => f["DestinationTag"].as_u64() == Some(u64::from(expected)),
                    // Defense in depth (M3): a quote that authorized no
                    // sub-account tag must not be satisfied by a payment
                    // carrying one. On a shared/tag-routed custodial
                    // address a stray `DestinationTag` routes the funds to
                    // a *different* sub-account, so an untagged quote only
                    // matches an untagged payment.
                    None => f["DestinationTag"].is_null(),
                };
                let from_ok = match q.from.as_deref() {
                    Some(from) => f["Account"].as_str() == Some(from),
                    None => true,
                };
                let invoice_ok = match q.reference.as_deref() {
                    Some(invoice_id) => invoice_bound(f, invoice_id),
                    None => true,
                };
                // Delivered: canonical `meta.delivered_amount` only —
                // never tx.Amount (an upper bound under partial
                // payments). For XRP the canonical shape is a string of
                // drops; an IOU object is a token mismatch for the
                // XRP-only rung; a missing field on a tesSUCCESS Payment
                // is an honest zero.
                let amount_drops = tx["meta"]["delivered_amount"]
                    .as_str()
                    .and_then(|s| s.parse::<u128>().ok());
                let counts = is_payment
                    && !partial
                    && q.token == "XRP"
                    && to_ok
                    && tag_ok
                    && from_ok
                    && invoice_ok;
                let total = if counts { amount_drops.unwrap_or(0) } else { 0 };
                Some(total.to_string())
            }
            None => None,
        };

        Ok(ChainVerdict::Included { tier, delivered })
    }
}
