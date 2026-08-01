//! Shared bounded-body JSON-RPC transport for the chain checkers.
//!
//! Every checker (`eip155`, `svm`, `xrpl`) POSTs a JSON body to a
//! participant-configured RPC endpoint, reads a size-bounded response, and
//! maps transport/HTTP failures to retryable/terminal
//! [`CheckerError`](super::CheckerError)s.
//!
//! The security-sensitive parts — scheme enforcement, destination policy,
//! the pinned-TLS client build, and the response cap — live in
//! [`crate::http_policy`], shared with the facilitator client and the
//! outbound HTTP-402 door so the three cannot drift. They did drift: this
//! transport was for a while the only money-path client that accepted a
//! cleartext `http://` endpoint to a remote host, which is the worst place
//! for that hole to be. This is the path that mints `confirmed(n)` and
//! `final` — the tiers that exist precisely so a facilitator need not be
//! trusted. Over cleartext an on-path attacker fabricates receipts, block
//! heights, and chain ids at will, and `ensure_chain_id` cannot help
//! because it reads its answer from the same unauthenticated channel.
//!
//! What the transport deliberately does **not** do is interpret the
//! response envelope: eip155/svm carry RPC errors in a top-level `error`
//! field while rippled rides them *inside* `result`, so each checker
//! extracts result/error itself from the decoded [`Value`].

use serde_json::Value;

use super::CheckerError;
use crate::http_policy::{self, DestinationPolicy, ReadError};

/// JSON-RPC responses (a receipt/transaction with many logs or balances)
/// are bounded but can be large; cap so a malicious/compromised RPC cannot
/// stream a giant body within the timeout and exhaust memory.
const MAX_RPC_BODY: usize = 16 * 1024 * 1024;

/// A pinned-TLS HTTP client bound to one RPC endpoint.
pub(super) struct RpcTransport {
    endpoint: String,
    http: reqwest::Client,
}

impl RpcTransport {
    /// Build a transport for `endpoint` with pinned TLS roots and a 15s
    /// timeout.
    ///
    /// **Refuses a cleartext endpoint to a non-loopback host** — the same
    /// policy the facilitator client applies, and for a stronger reason:
    /// a checker's answers are the independent leg of verification, so an
    /// endpoint an attacker can rewrite is worth less than no checker at
    /// all (it manufactures confidence rather than withholding it).
    ///
    /// The destination policy is [`DestinationPolicy::AllowPrivate`]: an
    /// RPC endpoint is operator configuration, and a node on a LAN or on
    /// loopback is an ordinary self-hosted deployment. What it still
    /// refuses is the set nobody configures on purpose — link-local
    /// (including the cloud metadata address), carrier-NAT, and reserved
    /// ranges — so a templated or partially-substituted endpoint fails
    /// closed instead of reaching instance metadata.
    pub(super) fn new(endpoint: impl Into<String>) -> Result<Self, CheckerError> {
        let endpoint = endpoint.into();
        http_policy::require_secure_endpoint(&endpoint)
            .map_err(|e| CheckerError::terminal(format!("rpc endpoint: {e}")))?;
        // An IP-literal endpoint is dialled without a DNS lookup, so the
        // resolver never sees it — check literals here, names at resolve
        // time. Together they cover every host form.
        if let Ok(url) = reqwest::Url::parse(&endpoint) {
            http_policy::check_url_destination(&url, DestinationPolicy::AllowPrivate)
                .map_err(|e| CheckerError::terminal(format!("rpc endpoint: {e}")))?;
        }
        let http = http_policy::client(
            DestinationPolicy::AllowPrivate,
            std::time::Duration::from_secs(15),
            std::time::Duration::from_secs(10),
            reqwest::redirect::Policy::none(),
        )
        .map_err(|e| CheckerError::terminal(e.to_string()))?;
        Ok(Self { endpoint, http })
    }

    /// The endpoint URL (for `reference()` and error messages).
    pub(super) fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// POST `body` as JSON and return the decoded response envelope,
    /// bounding the body at [`MAX_RPC_BODY`]. `what` labels the call in
    /// error messages (the RPC method name). Transport errors and
    /// 5xx map to retryable; other non-2xx, oversize, and decode failures
    /// map to terminal. The envelope is returned uninterpreted — the caller
    /// extracts result/error per its chain's convention.
    pub(super) async fn post(&self, what: &str, body: &Value) -> Result<Value, CheckerError> {
        let response = self
            .http
            .post(&self.endpoint)
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
        let bytes = http_policy::read_bounded(response, MAX_RPC_BODY)
            .await
            .map_err(|e| match e {
                // A body that overruns the cap is the peer misbehaving,
                // not a transient fault: terminal.
                ReadError::TooLarge { .. } => CheckerError::terminal(format!("{what}: {e}")),
                ReadError::Transport(_) => CheckerError::retryable(e.to_string()),
            })?;
        if !status.is_success() {
            return Err(if status.is_server_error() {
                CheckerError::retryable(format!("{what} -> {status}"))
            } else {
                CheckerError::terminal(format!("{what} -> {status}"))
            });
        }
        serde_json::from_slice(&bytes)
            .map_err(|e| CheckerError::terminal(format!("{what} decode: {e}")))
    }
}
