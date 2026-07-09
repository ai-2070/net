//! Provider-side payment surface: pricing a capability + charging for it. The
//! supply-side counterpart to `capability_gateway.rs` — what a Python node
//! needs to *be* a paid provider: author `net.pricing.terms@1`
//! ([`build_pricing_terms`]) and stand up a [`PyPaymentProvider`] that runs one
//! shared `PaymentEngine` behind the quote/pay wire and gates its priced tools
//! (the MCP wrap `payment_admission` path). Doctrine #1 holds: the engine +
//! settlement logic is `net-payments`; this marshals config in.

#![cfg(feature = "payments")]

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

use net::adapter::net::identity::EntityId;
use net_payments::core::canonical::canonical_bytes;
use net_payments::core::registry::default_registry_v1;
use net_payments::core::terms::PricingTerms;
use net_payments::x402::requirements::PaymentRequirements;
use net_payments::x402::X402Carry;

/// Author the canonical `net.pricing.terms@1` JSON for a capability from a
/// provider entity id + a JSON array of x402 `PaymentRequirements`. Pure —
/// the pyfunction below is a thin wrapper.
fn author_pricing_terms(
    provider_entity_id: [u8; 32],
    capability: &str,
    requirements_json: &str,
) -> Result<String, String> {
    let reqs: Vec<PaymentRequirements> = serde_json::from_str(requirements_json).map_err(|e| {
        format!("requirements_json must be a JSON array of x402 PaymentRequirements objects: {e}")
    })?;
    if reqs.is_empty() {
        return Err(
            "at least one payment requirement is required — an empty accepts[] prices nothing"
                .to_string(),
        );
    }
    // Locally-originated x402: `author` is the sanctioned serialization point
    // (the templates originate here, so these bytes become the preserved
    // originals — no byte-preservation violation).
    let accepts = reqs
        .iter()
        .map(X402Carry::author)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("author payment requirement: {e}"))?;
    let provider = EntityId::from_bytes(provider_entity_id);
    // The v1 default registry (mock + survey networks). Its `reference()` is
    // signer-independent (it hashes the asset content), so it matches any
    // caller authoring quotes under the same default registry.
    let registry = default_registry_v1(provider.clone());
    let reference = registry
        .reference()
        .map_err(|e| format!("registry reference: {e}"))?;
    let terms = PricingTerms::new(provider, capability, accepts, reference);
    let bytes = canonical_bytes(&terms).map_err(|e| format!("canonicalize terms: {e}"))?;
    String::from_utf8(bytes).map_err(|e| format!("terms are not UTF-8: {e}"))
}

/// Author the canonical `net.pricing.terms@1` JSON string that prices a
/// capability — to hand to the priced publish path or announce at discovery.
///
/// `provider_entity_id` is the node's 32-byte mesh entity id (``mesh.entity_id``)
/// — the identity that will issue quotes for these terms. Only the public id
/// crosses; keys never do. `requirements_json` is a JSON array of x402
/// ``PaymentRequirements`` objects (``scheme``, ``network``, ``amount``,
/// ``asset``, ``payTo``, ``maxTimeoutSeconds``, optional ``extra`` — the x402
/// camelCase wire names); one entry per acceptable ``(scheme, network,
/// asset)``. Returns the canonical, byte-preserved terms string, opaque
/// downstream and echoed verbatim at discovery. Raises ``ValueError`` on a bad
/// entity id, malformed JSON, or an empty list.
#[pyfunction]
pub fn build_pricing_terms(
    provider_entity_id: Vec<u8>,
    capability: &str,
    requirements_json: &str,
) -> PyResult<String> {
    let id: [u8; 32] = provider_entity_id.as_slice().try_into().map_err(|_| {
        PyValueError::new_err(format!(
            "provider_entity_id must be 32 bytes (got {})",
            provider_entity_id.len()
        ))
    })?;
    author_pricing_terms(id, capability, requirements_json).map_err(PyValueError::new_err)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOCK_REQS: &str = r#"[{"scheme":"mock","network":"mock:net","amount":"2500","asset":"musd","payTo":"mock-provider-settle-addr","maxTimeoutSeconds":60}]"#;

    #[test]
    fn authors_canonical_decodable_pricing_terms() {
        let terms = author_pricing_terms([7u8; 32], "prov/echo", MOCK_REQS).expect("author");

        // The typed decoder accepts it (tag + non-empty accepts[]).
        let parsed = PricingTerms::from_json_bytes(terms.as_bytes()).expect("decode");
        assert_eq!(parsed.object, "net.pricing.terms@1");
        assert_eq!(parsed.capability, "prov/echo");
        assert_eq!(parsed.accepts.len(), 1);
        assert_eq!(parsed.provider, EntityId::from_bytes([7u8; 32]));

        // Canonical emission is a fixed point.
        let reparse: serde_json::Value = serde_json::from_str(&terms).unwrap();
        let re = String::from_utf8(canonical_bytes(&reparse).unwrap()).unwrap();
        assert_eq!(re, terms, "authored terms are already canonical");
    }

    #[test]
    fn multiple_accepts_are_preserved() {
        let two = r#"[
            {"scheme":"mock","network":"mock:net","amount":"2500","asset":"musd","payTo":"a","maxTimeoutSeconds":60},
            {"scheme":"mock","network":"mock:net","amount":"5000","asset":"musd","payTo":"a","maxTimeoutSeconds":60}
        ]"#;
        let terms = author_pricing_terms([7u8; 32], "prov/echo", two).expect("author");
        assert_eq!(
            PricingTerms::from_json_bytes(terms.as_bytes())
                .unwrap()
                .accepts
                .len(),
            2
        );
    }

    #[test]
    fn empty_and_malformed_are_rejected() {
        assert!(author_pricing_terms([1u8; 32], "prov/echo", "[]").is_err());
        assert!(author_pricing_terms([1u8; 32], "prov/echo", "not json").is_err());
        // A requirement missing a required field (payTo) is a decode error.
        let bad = r#"[{"scheme":"mock","network":"mock:net","amount":"1","asset":"musd","maxTimeoutSeconds":60}]"#;
        assert!(author_pricing_terms([1u8; 32], "prov/echo", bad).is_err());
    }
}

// ---------------------------------------------------------------------------
// PaymentProvider — a Python node that PRICES + CHARGES for its own tools.
// One shared PaymentEngine serves the quote/pay wire AND gates the priced
// tools (redeem against the same engine). Needs the `publish` feature (the
// tool-publish building blocks) alongside `payments`.
// ---------------------------------------------------------------------------

#[cfg(feature = "publish")]
mod provider {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    use pyo3::exceptions::{PyRuntimeError, PyValueError};
    use pyo3::prelude::*;
    use serde_json::Value;
    use tokio::runtime::Runtime;

    use net::adapter::net::MeshNode;
    use net_mcp::spec::{Implementation, Tool};
    use net_mcp::wrap::{
        CredentialStatus, LoweringContext, OwnerScope, ServerPublisher, Substitutability,
        ToolInvoker, WrapConfig,
    };
    use net_payments::billing::BillingLog;
    use net_payments::core::registry::default_registry_v1;
    use net_payments::engine::{AdmitAll, PaymentEngine};
    use net_payments::facilitator::mock::MockFacilitator;
    use net_payments::flow::mcp_gate::EnginePaymentAdmission;
    use net_payments::flow::mesh::{serve_payments, PaymentServeHandle};
    use net_payments::flow::{Clock, InProcessProvider, SystemClock};

    use crate::publish::{mesh_over, PyLocalPublicationHandle, PyToolInvoker};

    /// A paid-capability provider over an embedded `NetMesh` node — the supply
    /// side. Construction stands up one `PaymentEngine` behind the quote/pay
    /// wire; :meth:`publish_paid_tools` publishes priced tools gated by that
    /// same engine, so a quote paid over the wire is the quote the gate
    /// redeems. Hold the provider to keep the wire served.
    #[pyclass(name = "PaymentProvider", module = "net._net")]
    pub struct PyPaymentProvider {
        engine: Arc<PaymentEngine>,
        node: Arc<MeshNode>,
        runtime: Arc<Runtime>,
        provider_entity_id: Vec<u8>,
        /// Keeps the `net.payments.quote/pay` services registered on the node.
        _serve: PaymentServeHandle,
    }

    #[pymethods]
    impl PyPaymentProvider {
        /// Build a provider over a started ``mesh``. ``state_path`` is the
        /// settlement store file — it holds the replay/idempotency index and
        /// **must be durable + single-owner** (a temp path loses paid quotes
        /// across restarts). ``billing_log_path`` optionally records the
        /// immutable ``net.billing.event@1`` stream.
        #[new]
        #[pyo3(signature = (mesh, state_path, billing_log_path=None))]
        fn new(
            mesh: &crate::mesh_bindings::NetMesh,
            state_path: String,
            billing_log_path: Option<String>,
        ) -> PyResult<Self> {
            let node = mesh.node_arc_clone()?;
            let runtime = mesh.runtime_arc();
            // The provider payment identity IS the node's mesh identity: quotes
            // are signed by, and settlement tracked against, the same ed25519
            // identity peers see on the mesh (matches the pricing terms' provider
            // + the caller-side payment identity). Borrowed in-process — nothing
            // crosses the boundary.
            let sdk_mesh = mesh_over(node.clone());
            let provider = Arc::new(sdk_mesh.entity_keypair().clone());
            let entity_id = provider.entity_id().clone();
            let provider_entity_id = entity_id.as_bytes().to_vec();
            let registry = default_registry_v1(entity_id);
            // `AdmitAll` gates QUOTE issuance — correct for a paid tool (anyone
            // may quote; PAYMENT is the real gate on the serve).
            let mut engine = PaymentEngine::new(
                provider,
                Arc::new(MockFacilitator::new()),
                Arc::new(AdmitAll),
                registry,
                PathBuf::from(state_path),
            )
            .map_err(|e| PyRuntimeError::new_err(format!("payment engine: {e}")))?;
            if let Some(bp) = billing_log_path {
                engine = engine.with_billing_log(Arc::new(BillingLog::new(bp)));
            }
            let engine = Arc::new(engine);

            let clock: Arc<dyn Clock> = Arc::new(SystemClock);
            let in_process = Arc::new(InProcessProvider::new(engine.clone(), clock));
            let serve = serve_payments(&sdk_mesh, in_process)
                .map_err(|e| PyRuntimeError::new_err(format!("serve payments: {e}")))?;

            Ok(Self {
                engine,
                node,
                runtime,
                provider_entity_id,
                _serve: serve,
            })
        }

        /// The node's 32-byte mesh entity id — the provider identity these tools
        /// price + quote under. Pass it to :func:`build_pricing_terms`.
        #[getter]
        fn provider_entity_id(&self) -> Vec<u8> {
            self.provider_entity_id.clone()
        }

        /// Publish priced tools, gated by this provider's payment engine. Each
        /// ``tools`` entry is ``(name, description|None, input_schema_json)``;
        /// ``callback`` is the async invoker; ``pricing`` maps a tool name to
        /// its ``net.pricing.terms@1`` JSON (from :func:`build_pricing_terms`).
        /// A priced tool serves only **after** its quote is paid + redeemed
        /// (at-most-once). Fail-closed: an empty ``pricing`` map is a
        /// ``ValueError`` (use ``NetMesh.publish_tools`` for free tools); a
        /// pricing key naming no published tool is a publish error. ``version`` /
        /// ``owner_origin`` / ``allow_any_caller`` are as on
        /// ``NetMesh.publish_tools``. Hold the returned handle to keep serving.
        #[pyo3(signature = (tools, callback, pricing, version=String::new(), owner_origin=None, allow_any_caller=false))]
        #[allow(clippy::too_many_arguments)]
        fn publish_paid_tools(
            &self,
            py: Python<'_>,
            tools: Vec<(String, Option<String>, String)>,
            callback: Py<PyAny>,
            pricing: HashMap<String, String>,
            version: String,
            owner_origin: Option<u64>,
            allow_any_caller: bool,
        ) -> PyResult<PyLocalPublicationHandle> {
            if pricing.is_empty() {
                return Err(PyValueError::new_err(
                    "publish_paid_tools requires a non-empty pricing map \
                     (tool name -> net.pricing.terms@1 JSON from build_pricing_terms); \
                     use NetMesh.publish_tools for free tools",
                ));
            }
            let mut sdk_tools = Vec::with_capacity(tools.len());
            for (name, description, schema_json) in &tools {
                let input_schema: Value = serde_json::from_str(schema_json).map_err(|e| {
                    PyValueError::new_err(format!(
                        "tool `{name}`: input_schema is not valid JSON: {e}"
                    ))
                })?;
                sdk_tools.push(Tool {
                    name: name.clone(),
                    title: None,
                    description: description.clone(),
                    input_schema,
                    output_schema: None,
                });
            }
            let ctx = LoweringContext {
                server_version: if version.is_empty() {
                    "0".to_string()
                } else {
                    version
                },
                credential_status: CredentialStatus::None,
                substitutability: Substitutability::ProviderLocal,
                pricing: Default::default(), // pricing rides `config.pricing`
            };
            let mesh = Arc::new(mesh_over(self.node.clone()));
            let owner = owner_origin.unwrap_or_else(|| mesh.origin_hash());
            let mut config = WrapConfig::owner_only(
                Implementation {
                    name: "net-publish".to_string(),
                    version: "0".to_string(),
                },
                owner,
            );
            if allow_any_caller {
                config.scope = OwnerScope::any();
            }
            config.pricing = pricing.into_iter().collect();
            // The gate: redeem quotes against THIS provider's engine — the same
            // engine the quote/pay wire serves — so a paid tool serves once,
            // after payment. A priced tool with no gate is a wrap error; here
            // the gate is always set.
            config.payment_admission =
                Some(Arc::new(EnginePaymentAdmission::new(self.engine.clone())));

            let publisher = ServerPublisher::new(mesh);
            let invoker: Arc<dyn ToolInvoker> = Arc::new(PyToolInvoker { callback });
            let rt = Arc::clone(&self.runtime);
            let handle = py
                .detach(move || {
                    rt.block_on(publisher.publish_tools(&sdk_tools, invoker, ctx, config))
                })
                .map_err(|e| PyRuntimeError::new_err(format!("publish_paid_tools failed: {e}")))?;
            Ok(PyLocalPublicationHandle::wrap(handle, self.runtime.clone()))
        }
    }
}

#[cfg(feature = "publish")]
pub use provider::PyPaymentProvider;
