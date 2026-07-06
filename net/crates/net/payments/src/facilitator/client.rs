//! The real-facilitator HTTP client (P1) — `POST /verify`,
//! `POST /settle`, `GET /supported` per the pinned x402 v2 spec.
//!
//! Implements the P0 [`Facilitator`] trait **unchanged** — that was the
//! acceptance test of the P0 design, and it comes due here. Everything
//! network-specific stays in configuration; this client is
//! network-agnostic.
//!
//! Byte-preservation discipline crosses the HTTP boundary in both
//! directions: request bodies embed the payload/requirements **carry
//! bytes as raw JSON** (composed via `serde_json::value::RawValue`,
//! never re-serialized through Net types), and response bodies land in
//! [`X402Carry`] with their original bytes preserved — the same bytes a
//! later independent check or audit will hash.
//!
//! Trust posture: a facilitator receipt can only ever justify tier
//! [`VerificationTier::Observed`] — the v2 spec gives facilitators no
//! way to report finality, and this client refuses to invent one.
//! `confirmed(n)` / `final` come from the independent chain checker,
//! which keeps the facilitator out of the trust root for anything above
//! "someone saw it".
//!
//! Failure posture (P0 contract): transport-level failures are
//! structured, retryability-tagged [`FacilitatorError`]s; spec-level
//! rejections (`isValid: false`, `success: false`, and the spec's error
//! vocabulary in `invalidReason`/`errorReason`) ride **inside** the
//! carried responses for the engine to judge — the client never
//! collapses a facilitator's answer into a transport error.

#![cfg(feature = "http-facilitator")]

use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;

use super::traits::{Facilitator, FacilitatorError, SettleOutcome, VerifyOutcome};
use crate::core::verification::{VerificationTier, VerifierRef};
use crate::x402::payload::PaymentPayload;
use crate::x402::requirements::PaymentRequirements;
use crate::x402::settlement::{SettlementResponse, VerifyResponse};
use crate::x402::{X402Carry, X402_VERSION};

/// Request-auth header source. The spec leaves auth unspecified; CDP
/// mainnet wants API keys, testnet and self-hosted facilitators are
/// open. Header **values** are resolved by the host's secret handling
/// and held in memory only — never in config objects, never logged
/// (forwarding doctrine).
#[async_trait]
pub trait AuthProvider: Send + Sync {
    /// Headers to attach to every facilitator request.
    async fn headers(&self) -> Result<Vec<(String, String)>, FacilitatorError>;
}

/// No authentication — the x402.org testnet facilitator and open
/// self-hosted deployments.
pub struct NoAuth;

#[async_trait]
impl AuthProvider for NoAuth {
    async fn headers(&self) -> Result<Vec<(String, String)>, FacilitatorError> {
        Ok(Vec::new())
    }
}

/// A static bearer token, resolved by the host through its own secret
/// handling before construction. Held in memory for the client's
/// lifetime; deliberately not `Debug`.
pub struct BearerAuth {
    token: String,
}

impl BearerAuth {
    pub fn new(token: impl Into<String>) -> Self {
        Self { token: token.into() }
    }
}

#[async_trait]
impl AuthProvider for BearerAuth {
    async fn headers(&self) -> Result<Vec<(String, String)>, FacilitatorError> {
        Ok(vec![("authorization".to_string(), format!("Bearer {}", self.token))])
    }
}

/// One `(scheme, network)` pair a facilitator supports (`GET
/// /supported` → `kinds[]`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SupportedKind {
    #[serde(rename = "x402Version")]
    pub x402_version: u64,
    pub scheme: String,
    pub network: String,
}

/// The `GET /supported` response shape (spec-pinned).
#[derive(Debug, Clone, Deserialize)]
pub struct SupportedResponse {
    pub kinds: Vec<SupportedKind>,
    #[serde(default)]
    pub extensions: Vec<String>,
    #[serde(default)]
    pub signers: std::collections::BTreeMap<String, Vec<String>>,
}

/// The HTTP facilitator client.
pub struct HttpFacilitator {
    endpoint: String,
    http: reqwest::Client,
    auth: std::sync::Arc<dyn AuthProvider>,
}

impl HttpFacilitator {
    /// Build a client for the facilitator at `endpoint` (scheme + host
    /// + optional base path, no trailing slash needed).
    pub fn new(
        endpoint: impl Into<String>,
        auth: std::sync::Arc<dyn AuthProvider>,
    ) -> Result<Self, FacilitatorError> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| FacilitatorError::protocol(format!("http client build: {e}")))?;
        let endpoint = endpoint.into();
        let endpoint = endpoint.trim_end_matches('/').to_string();
        Ok(Self { endpoint, http, auth })
    }

    /// Override request timeouts (per call).
    pub fn with_timeout(mut self, timeout: Duration) -> Result<Self, FacilitatorError> {
        self.http = reqwest::Client::builder()
            .timeout(timeout)
            .connect_timeout(timeout.min(Duration::from_secs(10)))
            .build()
            .map_err(|e| FacilitatorError::protocol(format!("http client build: {e}")))?;
        Ok(self)
    }

    /// `GET /supported`, for config-time validation and signer
    /// recording. A facilitator that stops offering a configured pair
    /// fails loudly at load, not at first payment.
    pub async fn supported(&self) -> Result<SupportedResponse, FacilitatorError> {
        let url = format!("{}/supported", self.endpoint);
        let mut req = self.http.get(&url);
        for (name, value) in self.auth.headers().await? {
            req = req.header(name, value);
        }
        let response = req.send().await.map_err(map_send_error)?;
        let status = response.status();
        let body = response.bytes().await.map_err(map_send_error)?;
        if !status.is_success() {
            return Err(http_status_error("/supported", status, &body));
        }
        serde_json::from_slice(&body)
            .map_err(|e| FacilitatorError::protocol(format!("/supported decode: {e}")))
    }

    /// Assert every configured `(scheme, network)` pair is offered.
    pub async fn validate_pairs(
        &self,
        pairs: &[(String, String)],
    ) -> Result<SupportedResponse, FacilitatorError> {
        let supported = self.supported().await?;
        for (scheme, network) in pairs {
            let offered = supported.kinds.iter().any(|k| {
                k.x402_version == X402_VERSION && k.scheme == *scheme && k.network == *network
            });
            if !offered {
                return Err(FacilitatorError::rejected(format!(
                    "facilitator at {} does not support ({scheme}, {network}) at \
                     x402Version {X402_VERSION} — refusing the configuration",
                    self.endpoint
                )));
            }
        }
        Ok(supported)
    }

    /// POST the spec's facilitator request shape, embedding the carry
    /// bytes verbatim.
    async fn post_payment_op(
        &self,
        path: &str,
        payload: &X402Carry<PaymentPayload>,
        requirements: &X402Carry<PaymentRequirements>,
    ) -> Result<Vec<u8>, FacilitatorError> {
        // Compose around the preserved bytes: the payload/requirements
        // enter the request exactly as they were signed/quoted.
        let payload_raw: &serde_json::value::RawValue =
            serde_json::from_str(payload.as_json_str())
                .map_err(|e| FacilitatorError::protocol(format!("payload carry: {e}")))?;
        let requirements_raw: &serde_json::value::RawValue =
            serde_json::from_str(requirements.as_json_str())
                .map_err(|e| FacilitatorError::protocol(format!("requirements carry: {e}")))?;

        #[derive(serde::Serialize)]
        struct FacilitatorRequest<'a> {
            #[serde(rename = "x402Version")]
            x402_version: u64,
            #[serde(rename = "paymentPayload")]
            payment_payload: &'a serde_json::value::RawValue,
            #[serde(rename = "paymentRequirements")]
            payment_requirements: &'a serde_json::value::RawValue,
        }
        let body = serde_json::to_vec(&FacilitatorRequest {
            x402_version: X402_VERSION,
            payment_payload: payload_raw,
            payment_requirements: requirements_raw,
        })
        .map_err(|e| FacilitatorError::protocol(format!("request encode: {e}")))?;

        let url = format!("{}{path}", self.endpoint);
        let mut req = self
            .http
            .post(&url)
            .header("content-type", "application/json")
            .body(body);
        for (name, value) in self.auth.headers().await? {
            req = req.header(name, value);
        }
        let response = req.send().await.map_err(map_send_error)?;
        let status = response.status();
        let body = response.bytes().await.map_err(map_send_error)?;
        if !status.is_success() {
            return Err(http_status_error(path, status, &body));
        }
        Ok(body.to_vec())
    }
}

/// Transport-level error mapping. Timeouts and connect failures are the
/// facilitator being unreachable (retryable, policy decides); anything
/// else at this layer is a protocol fault (fail-closed).
fn map_send_error(e: reqwest::Error) -> FacilitatorError {
    if e.is_timeout() {
        FacilitatorError::timeout(e.to_string())
    } else if e.is_connect() || e.is_request() {
        FacilitatorError::unavailable(e.to_string())
    } else {
        FacilitatorError::protocol(e.to_string())
    }
}

/// Non-2xx mapping: 5xx is the facilitator degraded (retryable); 4xx is
/// a terminal answer about *this* request (never retried — replaying a
/// rejected payment op is exactly the mistake the money path must not
/// make).
fn http_status_error(path: &str, status: reqwest::StatusCode, body: &[u8]) -> FacilitatorError {
    let snippet = String::from_utf8_lossy(&body[..body.len().min(256)]).into_owned();
    if status.is_server_error() {
        FacilitatorError::unavailable(format!("{path} -> {status}: {snippet}"))
    } else {
        FacilitatorError::rejected(format!("{path} -> {status}: {snippet}"))
    }
}

#[async_trait]
impl Facilitator for HttpFacilitator {
    fn reference(&self) -> VerifierRef {
        VerifierRef { identity: None, endpoint: self.endpoint.clone() }
    }

    async fn verify(
        &self,
        payload: &X402Carry<PaymentPayload>,
        requirements: &X402Carry<PaymentRequirements>,
    ) -> Result<VerifyOutcome, FacilitatorError> {
        let body = self.post_payment_op("/verify", payload, requirements).await?;
        let response: X402Carry<VerifyResponse> = X402Carry::from_bytes(body)
            .map_err(|e| FacilitatorError::protocol(format!("/verify response: {e}")))?;
        Ok(VerifyOutcome { response, tier: VerificationTier::Observed })
    }

    async fn settle(
        &self,
        payload: &X402Carry<PaymentPayload>,
        requirements: &X402Carry<PaymentRequirements>,
    ) -> Result<SettleOutcome, FacilitatorError> {
        let body = self.post_payment_op("/settle", payload, requirements).await?;
        let response: X402Carry<SettlementResponse> = X402Carry::from_bytes(body)
            .map_err(|e| FacilitatorError::protocol(format!("/settle response: {e}")))?;
        // A receipt is a receipt: `observed`, never more (the spec
        // reports no finality; the chain checker owns everything above).
        Ok(SettleOutcome { response, tier: VerificationTier::Observed })
    }
}
