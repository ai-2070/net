//! The two-way door, outbound: a Net agent paying an **external x402
//! HTTP API** — same objects, same spend policy, same signers, zero
//! translation.
//!
//! Wire mechanics per the pinned v2 HTTP transport (header-only; bodies
//! are the server's business):
//!
//! - `402` + `PAYMENT-REQUIRED` header: base64 `PaymentRequired` JSON;
//! - retry with `PAYMENT-SIGNATURE` header: base64 of *our* payload's
//!   preserved bytes;
//! - success carries `PAYMENT-RESPONSE`: base64 `SettlementResponse`,
//!   landed byte-preserved for audit.
//!
//! Policy runs on a **local pseudo-quote** over the chosen accepts
//! entry: there is no provider identity and no signed quote on this
//! path — the external server's demand is the commercial fact, and the
//! caller's own spend engine (caps, network enablement, approvals) is
//! the entire gate. The pseudo-quote's capability key is
//! `x402-http/<host>`, so per-capability overrides and approval
//! redemption work per external host.
//!
//! Honesty note on retries: HTTP has no provider-side idempotency here.
//! A lost response after a settled payment is a dispute with the
//! external server, not something this client can dedupe — one
//! `fetch_paid` call authors one payment attempt.

#![cfg(feature = "http-facilitator")]

use std::sync::Arc;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;

use super::signer::SchemeSigner;
use super::{exact_evm_authorization_for_quote, Clock};
use crate::core::quote::PaymentQuote;
use crate::core::registry::AssetRegistry;
use crate::policy::spend::{SpendDecision, SpendPolicyEngine};
use crate::x402::payload::PaymentPayload;
use crate::x402::payment_required::PaymentRequired;
use crate::x402::requirements::PaymentRequirements;
use crate::x402::settlement::SettlementResponse;
use crate::x402::{X402Carry, X402_VERSION};
use net::adapter::net::identity::EntityKeypair;

/// Client → server payment payload header (v2 HTTP transport).
pub const HDR_PAYMENT_SIGNATURE: &str = "payment-signature";
/// Server → client payment demand header on 402.
pub const HDR_PAYMENT_REQUIRED: &str = "payment-required";
/// Server → client settlement response header on success.
pub const HDR_PAYMENT_RESPONSE: &str = "payment-response";

/// The structured outcome of a paid HTTP fetch.
#[derive(Debug)]
pub enum X402HttpOutcome {
    /// The resource needed no payment (or the server answered without a
    /// 402): status + body, passed through.
    Ok { status: u16, body: Vec<u8> },
    /// Paid and served. `settlement` is the server's `PAYMENT-RESPONSE`
    /// when present, byte-preserved for audit.
    Paid {
        status: u16,
        body: Vec<u8>,
        settlement: Option<X402Carry<SettlementResponse>>,
    },
    /// Spend policy wants a human — same contract as everywhere else;
    /// the request was NOT retried and nothing was signed or sent.
    RequiresPaymentApproval {
        quote_id: String,
        policy_reason: String,
        approve_hint: String,
    },
    /// Spend policy denies (unenabled network, unknown asset, …).
    Denied { policy_reason: String },
    /// The server refused the payment (a second 402 / 400 after
    /// paying): terminal for this attempt; the reservation was
    /// released (per the transport, non-2xx means not settled).
    PaymentRejected { status: u16, message: String },
    /// Transport-level failure.
    Failed { message: String, retryable: bool },
}

/// The outbound paid-HTTP client.
pub struct X402HttpFlow {
    caller: Arc<EntityKeypair>,
    spend: SpendPolicyEngine,
    registry: AssetRegistry,
    signers: std::collections::BTreeMap<String, Arc<dyn SchemeSigner>>,
    clock: Arc<dyn Clock>,
    http: reqwest::Client,
}

impl X402HttpFlow {
    pub fn new(
        caller: Arc<EntityKeypair>,
        spend: SpendPolicyEngine,
        registry: AssetRegistry,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, String> {
        let tls = crate::tls_roots::tls_config().map_err(|e| format!("http tls config: {e}"))?;
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            // Never follow redirects: both the unpaid probe and the paid
            // retry carry (or are about to carry) a signed EIP-3009
            // authorization — a bearer instrument. Following a 3xx would
            // hand it to an origin we never scoped spend policy against.
            .redirect(reqwest::redirect::Policy::none())
            // Ring provider + bundled Mozilla roots, no OS store and no
            // process-global provider (see `crate::tls_roots`): the money
            // path must not trust a store that could carry a MITM root.
            .use_preconfigured_tls(tls)
            .build()
            .map_err(|e| format!("http client: {e}"))?;
        Ok(Self {
            caller,
            spend,
            registry,
            signers: std::collections::BTreeMap::new(),
            clock,
            http,
        })
    }

    /// Register a settlement signer for a CAIP-2 namespace (same seam
    /// as the mesh flow).
    pub fn with_signer(
        mut self,
        namespace: impl Into<String>,
        signer: Arc<dyn SchemeSigner>,
    ) -> Self {
        self.signers.insert(namespace.into(), signer);
        self
    }

    fn can_settle(&self, requirements: &PaymentRequirements) -> bool {
        if requirements.network.starts_with("mock:") {
            return true;
        }
        let namespace = requirements.network.split(':').next().unwrap_or_default();
        requirements.scheme == "exact"
            && (namespace == "eip155" || super::OPAQUE_BLOB_NAMESPACES.contains(&namespace))
            && self.signers.contains_key(namespace)
    }

    /// GET `url`, paying if the server demands it.
    pub async fn fetch_paid(&self, url: &str) -> X402HttpOutcome {
        // -- [1] the unpaid attempt.
        let response = match self.http.get(url).send().await {
            Ok(r) => r,
            Err(e) => {
                return X402HttpOutcome::Failed {
                    message: e.to_string(),
                    retryable: e.is_timeout() || e.is_connect(),
                }
            }
        };
        let status = response.status().as_u16();
        // A redirect is refused, not followed: the client is built with
        // `Policy::none()`, so a 3xx lands here as a real response. Treat
        // it as a hard failure — a moved paid resource must be re-fetched
        // explicitly at its true origin, never chased while a payment is
        // in flight to a host we scoped policy against.
        if (300..400).contains(&status) {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("<none>");
            return X402HttpOutcome::Failed {
                message: format!(
                    "refusing to follow a {status} redirect to `{location}` on a paid fetch"
                ),
                retryable: false,
            };
        }
        if status != 402 {
            let body = response
                .bytes()
                .await
                .map(|b| b.to_vec())
                .unwrap_or_default();
            return X402HttpOutcome::Ok { status, body };
        }

        // The 402 demand must originate from the host we intend to pay.
        // With redirects disabled this holds by construction, but re-check
        // so the capability key (`x402-http/<host>`) and the signed retry
        // can never be scoped to one origin while the demand — and the
        // pay_to/amount it dictates — was authored by another.
        let intended_host = reqwest::Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned));
        let demand_host = response.url().host_str().map(str::to_owned);
        if intended_host.is_some() && intended_host != demand_host {
            return X402HttpOutcome::Failed {
                message: format!(
                    "402 demand origin `{}` does not match the intended host `{}`",
                    demand_host.as_deref().unwrap_or("<none>"),
                    intended_host.as_deref().unwrap_or("<none>"),
                ),
                retryable: false,
            };
        }

        // The paid retry carries the signed PAYMENT-SIGNATURE (a bearer
        // instrument): refuse to author payment for a cleartext http URL to
        // a remote host. http to loopback stays allowed for local testing.
        if !is_payment_safe_url(url) {
            return X402HttpOutcome::Denied {
                policy_reason: format!(
                    "refusing to send a signed payment over cleartext to `{url}` — use https"
                ),
            };
        }

        // -- [2] the demand, from the PAYMENT-REQUIRED header.
        let Some(required_b64) = response
            .headers()
            .get(HDR_PAYMENT_REQUIRED)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
        else {
            return X402HttpOutcome::Failed {
                message: "402 without a PAYMENT-REQUIRED header is not x402 v2".to_string(),
                retryable: false,
            };
        };
        let required: X402Carry<PaymentRequired> = match BASE64
            .decode(required_b64.as_bytes())
            .map_err(|e| e.to_string())
            .and_then(|bytes| X402Carry::from_bytes(bytes).map_err(|e| e.to_string()))
        {
            Ok(c) => c,
            Err(e) => {
                return X402HttpOutcome::Failed {
                    message: format!("PAYMENT-REQUIRED header: {e}"),
                    retryable: false,
                }
            }
        };
        let Some(entry) = required.view().accepts.iter().find(|r| self.can_settle(r)) else {
            let offered: Vec<String> = required
                .view()
                .accepts
                .iter()
                .map(|r| format!("({}, {})", r.scheme, r.network))
                .collect();
            return X402HttpOutcome::Denied {
                policy_reason: format!(
                    "no settleable accepts[] entry: the server offers {offered:?}"
                ),
            };
        };

        // -- [3] the local pseudo-quote: the external demand as a
        //    commercial fact the spend engine can judge. Capability key
        //    is per external host, so overrides + approvals scope
        //    sensibly.
        let requirements = match X402Carry::author(entry) {
            Ok(c) => c,
            Err(e) => {
                return X402HttpOutcome::Failed {
                    message: e.to_string(),
                    retryable: false,
                }
            }
        };
        let now_ns = self.clock.now_ns();
        let ttl_ns = entry
            .max_timeout_seconds
            .max(1)
            .saturating_mul(1_000_000_000);
        let quote = PaymentQuote::new(
            self.caller.entity_id().clone(),
            self.caller.entity_id().clone(),
            format!("x402-http/{}", host_of(url)),
            None,
            requirements,
            match self.registry.reference() {
                Ok(r) => r,
                Err(e) => {
                    return X402HttpOutcome::Failed {
                        message: e.to_string(),
                        retryable: false,
                    }
                }
            },
            now_ns,
            now_ns.saturating_add(ttl_ns),
        );

        match self
            .spend
            .check_and_reserve(&quote, &self.registry, now_ns)
            .await
        {
            Ok(SpendDecision::Allowed) => {}
            Ok(SpendDecision::RequiresPaymentApproval {
                quote_id,
                policy_reason,
                approve_hint,
            }) => {
                return X402HttpOutcome::RequiresPaymentApproval {
                    quote_id,
                    policy_reason,
                    approve_hint,
                }
            }
            Ok(SpendDecision::Denied { policy_reason }) => {
                return X402HttpOutcome::Denied { policy_reason }
            }
            Err(e) => {
                return X402HttpOutcome::Failed {
                    message: e.to_string(),
                    retryable: false,
                }
            }
        }

        // -- [4] author the payload (same scheme dispatch as the mesh
        //    flow) and retry with PAYMENT-SIGNATURE.
        let payload = match self.author_payload(&quote).await {
            Ok(p) => p,
            Err(message) => {
                self.release(&quote, now_ns).await;
                return X402HttpOutcome::Failed {
                    message,
                    retryable: false,
                };
            }
        };
        let paid_response = match self
            .http
            .get(url)
            .header(HDR_PAYMENT_SIGNATURE, BASE64.encode(payload.bytes()))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                // Transport ambiguity after sending a payment: the
                // reservation stands (fail-closed accounting).
                return X402HttpOutcome::Failed {
                    message: e.to_string(),
                    retryable: e.is_timeout() || e.is_connect(),
                };
            }
        };

        let status = paid_response.status().as_u16();
        let settlement = paid_response
            .headers()
            .get(HDR_PAYMENT_RESPONSE)
            .and_then(|v| v.to_str().ok())
            .and_then(|b64| BASE64.decode(b64.as_bytes()).ok())
            .and_then(|bytes| X402Carry::<SettlementResponse>::from_bytes(bytes).ok());
        let body = paid_response
            .bytes()
            .await
            .map(|b| b.to_vec())
            .unwrap_or_default();

        if (200..300).contains(&status) {
            X402HttpOutcome::Paid {
                status,
                body,
                settlement,
            }
        } else {
            // The v2 transport says a non-2xx answer to a paid request
            // means it did not settle — but the server already holds our
            // signed EIP-3009 authorization, a bearer instrument it could
            // submit on-chain regardless of the status it returns. Release
            // the reservation only for the chainless mock scheme; for a
            // real bearer authorization the reservation must stand
            // (fail-closed accounting), mirroring the mesh flow (M1).
            if super::reject_releases_reservation(&quote) {
                self.release(&quote, now_ns).await;
            }
            X402HttpOutcome::PaymentRejected {
                status,
                message: String::from_utf8_lossy(&body[..body.len().min(256)]).into_owned(),
            }
        }
    }

    async fn author_payload(
        &self,
        quote: &PaymentQuote,
    ) -> Result<X402Carry<PaymentPayload>, String> {
        let requirements = quote.requirements.view();
        let payload_object = if requirements.network.starts_with("mock:") {
            serde_json::json!({
                "mock_authorization": hex::encode(self.caller.entity_id().as_bytes()),
                "nonce": quote.quote_id,
            })
        } else if self.can_settle(requirements) && requirements.network.starts_with("eip155:") {
            let signer = self
                .signers
                .get("eip155")
                .ok_or_else(|| "no eip155 signer configured".to_string())?;
            let auth = exact_evm_authorization_for_quote(quote, &signer.address());
            let typed = crate::x402::schemes::exact_evm::typed_data(requirements, &auth)
                .map_err(|e| e.to_string())?;
            let signature = signer
                .sign_typed_data(&typed)
                .await
                .map_err(|e| e.to_string())?;
            crate::x402::schemes::exact_evm::payload_object(&auth, &signature)
        } else if self.can_settle(requirements)
            && super::OPAQUE_BLOB_NAMESPACES
                .contains(&requirements.network.split(':').next().unwrap_or_default())
        {
            // exact / solana | xrpl: the wallet authors the opaque blob
            // from the demanded requirements, via the shared
            // `author_opaque_blob_payload` (identical dispatch to the mesh
            // flow — the two paths cannot drift). Retry honesty on this
            // path: HTTP has no provider-side idempotency (one `fetch_paid`
            // = one attempt), so a re-fetch that re-signs (fresh SPL
            // blockhash / a re-quoted XRPL blob after an expired
            // LastLedgerSequence) is simply the next attempt.
            let namespace = requirements.network.split(':').next().unwrap_or_default();
            let signer = self
                .signers
                .get(namespace)
                .ok_or_else(|| format!("no {namespace} signer configured"))?;
            super::author_opaque_blob_payload(namespace, requirements, signer).await?
        } else {
            return Err(format!(
                "no payload author for scheme `{}` on `{}`",
                requirements.scheme, requirements.network
            ));
        };
        X402Carry::author(&PaymentPayload {
            x402_version: X402_VERSION,
            resource: None,
            accepted: requirements.clone(),
            payload: payload_object,
            extensions: None,
        })
        .map_err(|e| e.to_string())
    }

    async fn release(&self, quote: &PaymentQuote, now_ns: u64) {
        if let Err(e) = self.spend.release_reservation(quote, now_ns).await {
            tracing::warn!(quote = %quote.quote_id, error = %e, "spend reservation release failed");
        }
    }
}

/// Whether a signed payment may be sent to `url`: https anywhere, or http
/// only to a loopback host (local/self-hosted testing). Anything else
/// would put the PAYMENT-SIGNATURE bearer instrument on the wire in the
/// clear.
fn is_payment_safe_url(url: &str) -> bool {
    match reqwest::Url::parse(url) {
        Ok(u) if u.scheme() == "https" => true,
        Ok(u) if u.scheme() == "http" => {
            let host = u.host_str().unwrap_or_default();
            let bare = host.trim_start_matches('[').trim_end_matches(']');
            host == "localhost"
                || bare
                    .parse::<std::net::IpAddr>()
                    .map(|ip| ip.is_loopback())
                    .unwrap_or(false)
        }
        _ => false,
    }
}

/// The host segment of a URL, for the per-host capability key.
fn host_of(url: &str) -> String {
    url.split("://")
        .nth(1)
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or("unknown-host")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::is_payment_safe_url;

    #[test]
    fn payment_requires_https_except_loopback() {
        assert!(is_payment_safe_url("https://api.example.com/x"));
        assert!(is_payment_safe_url("http://127.0.0.1:8080/x"));
        assert!(is_payment_safe_url("http://[::1]/x"));
        assert!(is_payment_safe_url("http://localhost/x"));
        // Cleartext to a remote host, or a non-web scheme: refused.
        assert!(!is_payment_safe_url("http://api.example.com/x"));
        assert!(!is_payment_safe_url("ftp://api.example.com/x"));
    }
}
