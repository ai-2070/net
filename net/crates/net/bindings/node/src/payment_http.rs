//! The outbound HTTP-402 client — a Node agent paying an external x402 HTTP
//! API through the same spend policy and status vocabulary as the mesh gateway,
//! over `net-payments`' [`X402HttpFlow::fetch_paid`]. The Node twin of the
//! Python `payment_http.rs`.
//!
//! Doctrine #1 holds: the probe → 402 → spend policy → sign → retry lifecycle
//! is `net-payments`; this projects [`X402HttpOutcome`] to a `[statusJson,
//! body]` pair. Behind the opt-in `payments-http` feature (pulls reqwest).
//!
//! **Real-network settlement needs a per-scheme signer** (the same TSFN signer
//! bridge deferred for the gateway); until then this client covers the
//! `fetched` / `denied` / `transport_error` / mock paths.

#![cfg(feature = "payments-http")]

use std::sync::Arc;

use napi::bindgen_prelude::Buffer;
use napi::{Error, Result};
use napi_derive::napi;
use parking_lot::Mutex;
use serde_json::json;

use net::adapter::net::identity::EntityKeypair;
use net_payments::core::registry::default_registry_v1;
use net_payments::flow::http402::{X402HttpFlow, X402HttpOutcome};
use net_payments::flow::SystemClock;
use net_payments::policy::spend::{SpendPolicyEngine, SpendProfile};

/// Project a [`X402HttpOutcome`] to `(statusJson, body)`. The status vocabulary
/// is the outbound-HTTP projection: `fetched` / `paid` /
/// `requires_payment_approval` / `denied` / `provider_refused` /
/// `transport_error`. The body rides beside the JSON as raw bytes.
fn outcome_to_result(outcome: X402HttpOutcome) -> (String, Vec<u8>) {
    match outcome {
        X402HttpOutcome::Ok { status, body } => (
            json!({ "status": "fetched", "http_status": status }).to_string(),
            body,
        ),
        X402HttpOutcome::Paid {
            status,
            body,
            settlement,
        } => {
            let settlement_b64 = settlement.map(|carry| {
                use base64::Engine as _;
                base64::engine::general_purpose::STANDARD.encode(carry.bytes())
            });
            (
                json!({ "status": "paid", "http_status": status, "settlement": settlement_b64 })
                    .to_string(),
                body,
            )
        }
        X402HttpOutcome::RequiresPaymentApproval {
            quote_id,
            policy_reason,
            approve_hint,
        } => (
            json!({
                "status": "requires_payment_approval",
                "quote_id": quote_id,
                "policy_reason": policy_reason,
                "approve_hint": approve_hint,
            })
            .to_string(),
            Vec::new(),
        ),
        X402HttpOutcome::Denied { policy_reason } => (
            json!({ "status": "denied", "policy_reason": policy_reason }).to_string(),
            Vec::new(),
        ),
        X402HttpOutcome::PaymentRejected { status, message } => (
            json!({ "status": "provider_refused", "http_status": status, "error": message })
                .to_string(),
            Vec::new(),
        ),
        X402HttpOutcome::Failed { message, retryable } => (
            json!({ "status": "transport_error", "error": message, "retryable": retryable })
                .to_string(),
            Vec::new(),
        ),
    }
}

fn parse_profile(profile: &str) -> Result<SpendProfile> {
    match profile {
        "production" => Ok(SpendProfile::Production),
        "dev_test" | "dev-test" | "devtest" => Ok(SpendProfile::DevTest),
        other => Err(Error::from_reason(format!(
            "payment-http: unknown paymentProfile {other:?} (expected \"production\" or \"dev_test\")"
        ))),
    }
}

struct HttpConfig {
    policy_path: String,
    profile: String,
    unsafe_mock_auto_allow: bool,
}

/// Build the outbound flow. The payer identity is ephemeral — there is no
/// provider identity on this path and no signed quote (policy runs on a local
/// pseudo-quote), so the caller id is bookkeeping; spend is tracked by
/// `(network, asset, day)`, not by caller.
fn build_flow(config: &HttpConfig) -> Result<X402HttpFlow> {
    let profile = parse_profile(&config.profile)?;
    let caller = Arc::new(EntityKeypair::generate());
    let registry = default_registry_v1(caller.entity_id().clone());
    let spend = SpendPolicyEngine::new(&config.policy_path, profile)
        .with_unsafe_mock_auto_allow(config.unsafe_mock_auto_allow);
    X402HttpFlow::new(caller, spend, registry, Arc::new(SystemClock))
        .map_err(|e| Error::from_reason(format!("payment-http: http client: {e}")))
}

/// GET a URL, paying if the server demands it. Returns `[statusJson, body]`.
///
/// Construct with `paymentPolicyPath` (**required** — the caller's spend policy
/// is the entire gate for outbound 402), plus optional `paymentProfile` /
/// `paymentUnsafeMockAutoAllow`.
#[napi]
pub struct PaymentHttpClient {
    config: HttpConfig,
    /// The flow, built lazily on the first `fetchPaid` (inside the async fn, so
    /// reqwest finds napi's reactor at construction — the JS-thread constructor
    /// has no runtime).
    flow: Mutex<Option<Arc<X402HttpFlow>>>,
}

#[napi]
impl PaymentHttpClient {
    #[napi(constructor)]
    pub fn new(
        payment_policy_path: String,
        payment_profile: Option<String>,
        payment_unsafe_mock_auto_allow: Option<bool>,
    ) -> Result<Self> {
        let profile = payment_profile.unwrap_or_else(|| "production".to_string());
        // Validate the profile up front (a bad profile is a construction error,
        // not a first-fetch surprise).
        parse_profile(&profile)?;
        Ok(Self {
            config: HttpConfig {
                policy_path: payment_policy_path,
                profile,
                unsafe_mock_auto_allow: payment_unsafe_mock_auto_allow.unwrap_or(false),
            },
            flow: Mutex::new(None),
        })
    }

    /// GET `url`, paying if the server answers `402`. Resolves to
    /// `[statusJson, body]`: `statusJson` is
    /// `{"status": "fetched" | "paid" | "requires_payment_approval" | "denied"
    /// | "provider_refused" | "transport_error", ...}` and `body` is the raw
    /// response bytes (empty for the non-body outcomes). Never rejects for a
    /// payment outcome.
    #[napi]
    pub async fn fetch_paid(&self, url: String) -> Result<(String, Buffer)> {
        // Get-or-build the flow once, cloning the `Arc` out of the lock so no
        // guard / `&self` is held across the await; the build runs inside this
        // async fn (napi's runtime), so reqwest's client finds a reactor.
        let flow = {
            let mut guard = self.flow.lock();
            if guard.is_none() {
                *guard = Some(Arc::new(build_flow(&self.config)?));
            }
            guard.as_ref().expect("just initialized above").clone()
        };
        let (status_json, body) = outcome_to_result(flow.fetch_paid(&url).await);
        Ok((status_json, Buffer::from(body)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn each_outcome_projects_its_status_and_body() {
        let (json_s, body) = outcome_to_result(X402HttpOutcome::Ok {
            status: 200,
            body: b"hello".to_vec(),
        });
        let v: Value = serde_json::from_str(&json_s).unwrap();
        assert_eq!(v["status"], "fetched");
        assert_eq!(v["http_status"], 200);
        assert_eq!(body, b"hello");

        let (json_s, _) = outcome_to_result(X402HttpOutcome::Paid {
            status: 200,
            body: b"paid".to_vec(),
            settlement: None,
        });
        let v: Value = serde_json::from_str(&json_s).unwrap();
        assert_eq!(v["status"], "paid");
        assert!(v["settlement"].is_null());

        let (json_s, body) = outcome_to_result(X402HttpOutcome::Failed {
            message: "connection refused".into(),
            retryable: true,
        });
        let v: Value = serde_json::from_str(&json_s).unwrap();
        assert_eq!(v["status"], "transport_error");
        assert_eq!(v["retryable"], true);
        assert!(body.is_empty());

        let (json_s, _) = outcome_to_result(X402HttpOutcome::PaymentRejected {
            status: 402,
            message: "second 402".into(),
        });
        assert_eq!(
            serde_json::from_str::<Value>(&json_s).unwrap()["status"],
            "provider_refused"
        );
        let (json_s, _) = outcome_to_result(X402HttpOutcome::Denied {
            policy_reason: "network not enabled".into(),
        });
        assert_eq!(
            serde_json::from_str::<Value>(&json_s).unwrap()["status"],
            "denied"
        );
    }

    #[test]
    fn unknown_profile_is_rejected() {
        assert!(parse_profile("production").is_ok());
        assert!(parse_profile("yolo").is_err());
    }
}
