//! Shared bounded-body JSON-RPC transport for the chain checkers.
//!
//! Every checker (`eip155`, `svm`, `xrpl`) POSTs a JSON body to a
//! participant-configured RPC endpoint, reads a size-bounded response, and
//! maps transport/HTTP failures to retryable/terminal
//! [`CheckerError`](super::CheckerError)s. That machinery — the pinned-TLS
//! client build, the [`MAX_RPC_BODY`] streaming cap (a hostile RPC must not
//! be able to exhaust memory within the timeout), and the status→error
//! classification — is security-sensitive and identical across chains, so
//! it lives here once instead of three copies that must be kept in sync.
//!
//! What the transport deliberately does **not** do is interpret the
//! response envelope: eip155/svm carry RPC errors in a top-level `error`
//! field while rippled rides them *inside* `result`, so each checker
//! extracts result/error itself from the decoded [`Value`].

use serde_json::Value;

use super::CheckerError;

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
    /// timeout. Errors terminally if the TLS config or client build fails.
    pub(super) fn new(endpoint: impl Into<String>) -> Result<Self, CheckerError> {
        let tls = crate::tls_roots::tls_config()
            .map_err(|e| CheckerError::terminal(format!("http tls config: {e}")))?;
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .use_preconfigured_tls(tls)
            .build()
            .map_err(|e| CheckerError::terminal(format!("http client: {e}")))?;
        Ok(Self {
            endpoint: endpoint.into(),
            http,
        })
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
                    "{what} response exceeded the {MAX_RPC_BODY}-byte cap"
                )));
            }
            bytes.extend_from_slice(&chunk);
        }
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
