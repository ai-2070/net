//! The outbound HTTP-402 client (`PAYMENTS_LANGUAGE_SDKS_PLAN` WS-P2): a
//! Python agent paying an **external x402 HTTP API** through the same spend
//! policy, signers, and status vocabulary as the mesh gateway — the demand
//! surface for `net-payments`' [`X402HttpFlow::fetch_paid`], no mesh required.
//!
//! Doctrine #1 holds (no logic in bindings): the probe → 402 → spend policy →
//! sign → retry lifecycle is decided in Rust; this module builds the flow from
//! the payment kwargs (the same ones [`crate::capability_gateway`] takes) and
//! projects [`X402HttpOutcome`] to the gateway's status-JSON plus the raw body
//! bytes. Feature-gated with `payments-http` (which pulls
//! `net-payments/http-facilitator`, i.e. `reqwest`), so it is a build-time
//! opt-in rather than a default-wheel dependency.

#![cfg(feature = "payments-http")]

use std::sync::Arc;

use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyTuple};
use serde_json::json;

use net::adapter::net::identity::EntityKeypair;
use net_payments::core::registry::default_registry_v1;
use net_payments::flow::http402::{X402HttpFlow, X402HttpOutcome};
use net_payments::flow::SystemClock;
use net_payments::policy::spend::{SpendPolicyEngine, SpendProfile};

use crate::capability_gateway::{python_external_signer, unbind_signer, PaymentConfig};

/// A paid-fetch result crossing the boundary as a `(status_json, body)`
/// tuple: the status-JSON string plus the raw HTTP body as Python `bytes`
/// (empty for outcomes with no body — approval-held, denied, transport
/// error). Building `PyBytes` in the `IntoPyObject` step keeps the body a
/// single copy and lets the async dual resolve to the same tuple.
struct HttpFetchResult {
    json: String,
    body: Vec<u8>,
}

impl<'py> pyo3::IntoPyObject<'py> for HttpFetchResult {
    type Target = PyTuple;
    type Output = Bound<'py, Self::Target>;
    type Error = PyErr;
    fn into_pyobject(self, py: Python<'py>) -> Result<Self::Output, Self::Error> {
        (self.json, PyBytes::new(py, &self.body)).into_pyobject(py)
    }
}

/// Project a [`X402HttpOutcome`] to `(status_json, body)`. The status
/// vocabulary is the outbound-HTTP projection named in the plan:
/// `fetched` / `paid` / `requires_payment_approval` / `denied` /
/// `provider_refused` / `transport_error`. The body rides beside the JSON as
/// raw bytes (never base64 into the JSON) — an HTTP body is arbitrary bytes.
fn outcome_to_result(outcome: X402HttpOutcome) -> HttpFetchResult {
    let (json, body) = match outcome {
        X402HttpOutcome::Ok { status, body } => (
            json!({ "status": "fetched", "http_status": status }).to_string(),
            body,
        ),
        X402HttpOutcome::Paid {
            status,
            body,
            settlement,
        } => {
            // The server's PAYMENT-RESPONSE, byte-preserved for audit, as
            // base64 (or null) — the body itself rides raw beside the JSON.
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
    };
    HttpFetchResult { json, body }
}

/// Build the outbound flow from the payment kwargs — the same validation the
/// gateway uses (`PaymentConfig`), except a policy path is **required** here:
/// the caller's own spend engine is the entire gate on this path, so no
/// spend policy means no way to pay (fail-closed).
fn build_flow(
    identity: Option<&crate::identity::Identity>,
    config: PaymentConfig,
) -> PyResult<X402HttpFlow> {
    let profile = match config.profile.as_str() {
        "production" => SpendProfile::Production,
        "dev_test" | "dev-test" | "devtest" => SpendProfile::DevTest,
        other => {
            return Err(PyValueError::new_err(format!(
                "unknown payment_profile {other:?} (expected \"production\" or \"dev_test\")"
            )))
        }
    };

    // The payer identity. There is no provider identity on this path and no
    // signed quote (policy runs on a local pseudo-quote), so an ephemeral
    // keypair is sound — spend is tracked by (network, asset, day), not by
    // caller. Pass a stable `identity` to correlate audit across calls.
    let caller: Arc<EntityKeypair> = match identity {
        Some(id) => id.keypair.clone(),
        None => Arc::new(EntityKeypair::generate()),
    };
    let registry = default_registry_v1(caller.entity_id().clone());
    let spend = SpendPolicyEngine::new(&config.policy_path, profile)
        .with_unsafe_mock_auto_allow(config.unsafe_mock_auto_allow);

    let mut flow = X402HttpFlow::new(caller, spend, registry, Arc::new(SystemClock))
        .map_err(|e| PyRuntimeError::new_err(format!("http client: {e}")))?;
    if let Some((address, callable)) = config.signer {
        flow = flow.with_signer("eip155", python_external_signer(address, callable));
    }
    Ok(flow)
}

/// Collect the payment kwargs, requiring a policy path (the spend gate).
fn collect_required(
    payment_policy_path: Option<String>,
    payment_profile: Option<String>,
    payment_unsafe_mock_auto_allow: bool,
    payment_signer_address: Option<String>,
    payment_signer: Option<Bound<'_, PyAny>>,
) -> PyResult<PaymentConfig> {
    PaymentConfig::collect(
        payment_policy_path,
        payment_profile,
        payment_unsafe_mock_auto_allow,
        payment_signer_address,
        unbind_signer(payment_signer)?,
        // The outbound HTTP client wires only the eip155 signer in v1;
        // svm/xrpl on this path are deferred (matrix WS-P3 is gateway-only).
        None,
        None,
        None,
        None,
    )?
    .ok_or_else(|| {
        PyValueError::new_err(
            "payment_policy_path is required for PaymentHttpClient — the caller's spend policy \
             is the entire gate for outbound 402 payments (there is no provider quote to trust)",
        )
    })
}

/// Aborts the client-runtime task if the owning awaitable is dropped
/// (asyncio cancel), matching the gateway's cancellation semantics.
struct AbortOnDrop(tokio::task::AbortHandle);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The client's shared state: the flow behind an `Arc` (fetch_paid takes
/// `&self`) and its own tokio runtime (no mesh, so the client owns the
/// reactor reqwest runs on).
struct HttpClientState {
    flow: Arc<X402HttpFlow>,
    runtime: Arc<tokio::runtime::Runtime>,
}

impl HttpClientState {
    fn new(identity: Option<&crate::identity::Identity>, config: PaymentConfig) -> PyResult<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|e| PyRuntimeError::new_err(format!("http client runtime: {e}")))?;
        // Build the flow inside the runtime context so reqwest's client finds
        // a reactor at construction.
        let flow = {
            let _guard = runtime.enter();
            build_flow(identity, config)?
        };
        Ok(Self {
            flow: Arc::new(flow),
            runtime: Arc::new(runtime),
        })
    }
}

/// GET a URL, paying if the server demands it. Returns the status-JSON +
/// raw body bytes.
///
/// Construct with the same payment kwargs as :class:`CapabilityGateway`,
/// but `payment_policy_path` is required (the spend gate). `identity` is an
/// optional payer :class:`Identity` handle; omit it for an ephemeral one.
#[pyclass(name = "PaymentHttpClient", module = "net._net")]
pub struct PyPaymentHttpClient {
    state: HttpClientState,
}

#[pymethods]
impl PyPaymentHttpClient {
    #[new]
    #[pyo3(signature = (payment_policy_path, payment_profile=None, payment_unsafe_mock_auto_allow=false, payment_signer_address=None, payment_signer=None, identity=None))]
    fn new(
        payment_policy_path: String,
        payment_profile: Option<String>,
        payment_unsafe_mock_auto_allow: bool,
        payment_signer_address: Option<String>,
        payment_signer: Option<Bound<'_, PyAny>>,
        identity: Option<&crate::identity::Identity>,
    ) -> PyResult<Self> {
        let config = collect_required(
            Some(payment_policy_path),
            payment_profile,
            payment_unsafe_mock_auto_allow,
            payment_signer_address,
            payment_signer,
        )?;
        Ok(Self {
            state: HttpClientState::new(identity, config)?,
        })
    }

    /// GET `url`, paying if the server answers `402`. Returns
    /// `(status_json, body)` where `status_json` is
    /// `{"status": "fetched" | "paid" | "requires_payment_approval" |
    /// "denied" | "provider_refused" | "transport_error", ...}` and `body`
    /// is the raw response bytes (empty for the non-body outcomes). Never
    /// raises for a payment outcome; releases the GIL while in flight.
    fn fetch_paid(&self, py: Python<'_>, url: &str) -> HttpFetchResult {
        let flow = self.state.flow.clone();
        let runtime = self.state.runtime.clone();
        let url = url.to_string();
        py.detach(move || {
            runtime.block_on(async move { outcome_to_result(flow.fetch_paid(&url).await) })
        })
    }

    fn __repr__(&self) -> String {
        "PaymentHttpClient()".to_string()
    }
}

/// Awaitable dual of :class:`PaymentHttpClient` — `fetch_paid` as a
/// coroutine, resolving to the same `(status_json, body)` tuple.
#[pyclass(name = "AsyncPaymentHttpClient", module = "net._net")]
pub struct PyAsyncPaymentHttpClient {
    state: HttpClientState,
}

#[pymethods]
impl PyAsyncPaymentHttpClient {
    #[new]
    #[pyo3(signature = (payment_policy_path, payment_profile=None, payment_unsafe_mock_auto_allow=false, payment_signer_address=None, payment_signer=None, identity=None))]
    fn new(
        payment_policy_path: String,
        payment_profile: Option<String>,
        payment_unsafe_mock_auto_allow: bool,
        payment_signer_address: Option<String>,
        payment_signer: Option<Bound<'_, PyAny>>,
        identity: Option<&crate::identity::Identity>,
    ) -> PyResult<Self> {
        let config = collect_required(
            Some(payment_policy_path),
            payment_profile,
            payment_unsafe_mock_auto_allow,
            payment_signer_address,
            payment_signer,
        )?;
        Ok(Self {
            state: HttpClientState::new(identity, config)?,
        })
    }

    /// Awaitable :meth:`PaymentHttpClient.fetch_paid`.
    fn fetch_paid<'py>(&self, py: Python<'py>, url: &str) -> PyResult<Bound<'py, PyAny>> {
        let flow = self.state.flow.clone();
        let url = url.to_string();
        let join = self
            .state
            .runtime
            .spawn(async move { outcome_to_result(flow.fetch_paid(&url).await) });
        let abort = AbortOnDrop(join.abort_handle());
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let out = join
                .await
                .map_err(|e| PyRuntimeError::new_err(format!("http fetch task failed: {e}")))?;
            drop(abort);
            Ok(out)
        })
    }

    fn __repr__(&self) -> String {
        "AsyncPaymentHttpClient()".to_string()
    }
}

// ---------------------------------------------------------------------------
// Contract tests — the outcome->status-JSON projection is the marshaling this
// module owns, so its shape is pinned here (like the gateway's
// `outcome_to_json` tests). A real fetch against an unreachable URL exercises
// the whole flow -> transport_error path.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn each_outcome_projects_its_status_and_body() {
        let r = outcome_to_result(X402HttpOutcome::Ok {
            status: 200,
            body: b"hello".to_vec(),
        });
        let v: Value = serde_json::from_str(&r.json).unwrap();
        assert_eq!(v["status"], "fetched");
        assert_eq!(v["http_status"], 200);
        assert_eq!(r.body, b"hello");

        let r = outcome_to_result(X402HttpOutcome::Paid {
            status: 200,
            body: b"paid-body".to_vec(),
            settlement: None,
        });
        let v: Value = serde_json::from_str(&r.json).unwrap();
        assert_eq!(v["status"], "paid");
        assert!(v["settlement"].is_null());
        assert_eq!(r.body, b"paid-body");

        let r = outcome_to_result(X402HttpOutcome::RequiresPaymentApproval {
            quote_id: "q-1".into(),
            policy_reason: "over cap".into(),
            approve_hint: "approve q-1".into(),
        });
        let v: Value = serde_json::from_str(&r.json).unwrap();
        assert_eq!(v["status"], "requires_payment_approval");
        assert_eq!(v["quote_id"], "q-1");
        assert!(r.body.is_empty());

        let r = outcome_to_result(X402HttpOutcome::Denied {
            policy_reason: "network not enabled".into(),
        });
        assert_eq!(
            serde_json::from_str::<Value>(&r.json).unwrap()["status"],
            "denied"
        );

        let r = outcome_to_result(X402HttpOutcome::PaymentRejected {
            status: 402,
            message: "second 402".into(),
        });
        let v: Value = serde_json::from_str(&r.json).unwrap();
        assert_eq!(v["status"], "provider_refused");
        assert_eq!(v["http_status"], 402);

        let r = outcome_to_result(X402HttpOutcome::Failed {
            message: "connection refused".into(),
            retryable: true,
        });
        let v: Value = serde_json::from_str(&r.json).unwrap();
        assert_eq!(v["status"], "transport_error");
        assert_eq!(v["retryable"], true);
    }

    #[tokio::test]
    async fn a_fetch_to_an_unreachable_url_projects_transport_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let caller = Arc::new(EntityKeypair::generate());
        let registry = default_registry_v1(caller.entity_id().clone());
        let spend = SpendPolicyEngine::new(dir.path().join("spend.json"), SpendProfile::DevTest);
        let flow =
            X402HttpFlow::new(caller, spend, registry, Arc::new(SystemClock)).expect("build flow");
        // Port 1 is unreachable — the unpaid probe fails at the transport.
        let r = outcome_to_result(flow.fetch_paid("http://127.0.0.1:1/nope").await);
        let v: Value = serde_json::from_str(&r.json).expect("json");
        assert_eq!(v["status"], "transport_error");
        assert!(r.body.is_empty());
    }
}
