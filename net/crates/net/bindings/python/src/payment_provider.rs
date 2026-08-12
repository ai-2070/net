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

/// Decode the caller's JSON array into byte-preserved carries.
///
/// Shared so the authoring path and the settlement-route check read the
/// same requirements from the same string — two parses of one input can
/// disagree, and the whole point of the check is that what gets
/// announced is what got validated.
///
/// Locally-originated x402: `author` is the sanctioned serialization
/// point (the templates originate here, so these bytes become the
/// preserved originals — no byte-preservation violation).
fn parse_requirements(
    requirements_json: &str,
) -> Result<Vec<X402Carry<PaymentRequirements>>, String> {
    let reqs: Vec<PaymentRequirements> = serde_json::from_str(requirements_json).map_err(|e| {
        format!("requirements_json must be a JSON array of x402 PaymentRequirements objects: {e}")
    })?;
    if reqs.is_empty() {
        return Err(
            "at least one payment requirement is required — an empty accepts[] prices nothing"
                .to_string(),
        );
    }
    reqs.iter()
        .map(X402Carry::author)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("author payment requirement: {e}"))
}

/// Author the canonical `net.pricing.terms@1` JSON for a capability from a
/// provider entity id + a JSON array of x402 `PaymentRequirements`. Pure —
/// the pyfunction below is a thin wrapper.
fn author_pricing_terms(
    provider_entity_id: [u8; 32],
    capability: &str,
    requirements_json: &str,
    production_registry: bool,
) -> Result<String, String> {
    let accepts = parse_requirements(requirements_json)?;
    let provider = EntityId::from_bytes(provider_entity_id);
    // The registry revision these terms are announced under. It must match
    // the one the provider's engine issues quotes under, or discovery
    // metadata names a different revision than the backend actually
    // serving — `PaymentProvider` picks `production_registry_v1` whenever a
    // real facilitator is configured, so this has to be able to follow.
    //
    // `reference()` hashes the whole registry, `signer` included — so it is
    // signer-*dependent*, and `provider_entity_id` has to be the same
    // identity the engine issues quotes under. A different id here produces
    // a different reference for the same asset list, and the announced
    // terms then name a registry revision no counterparty can match.
    let registry = if production_registry {
        net_payments::core::registry::production_registry_v1(provider.clone())
    } else {
        default_registry_v1(provider.clone())
    };
    // Every announced requirement must be one this registry actually
    // carries. Without the check, a production provider can advertise an
    // asset it will never quote — the caller picks that entry, asks for a
    // quote, and gets refused with no other entry to fall back to.
    for requirement in &accepts {
        registry
            .check_requirements(requirement.view())
            .map_err(|e| format!("payment requirement is not in the selected registry: {e}"))?;
    }
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
///
/// ``production_registry`` must match the provider that will quote these
/// terms: a :class:`PaymentProvider` built with a real ``facilitator_url``
/// issues quotes under the production registry revision (which drops the
/// mock asset and the testnets), so its announced terms must be authored
/// with ``True``. Announcing one revision while quoting under another
/// leaves discovery metadata naming a registry the backend does not use,
/// and a caller that picks an entry from it gets refused with nothing to
/// fall back to.
///
/// **Prefer :meth:`PaymentProvider.pricing_terms` when you have a
/// provider.** It takes the registry from the engine that will actually
/// issue the quotes, so the two cannot disagree. This free function is for
/// authoring terms without standing a provider up — a discovery tool, a
/// fixture — and its default of ``False`` is a real footgun for anyone who
/// has a real facilitator and does not pass the flag.
#[pyfunction]
#[pyo3(signature = (provider_entity_id, capability, requirements_json, production_registry=false))]
pub fn build_pricing_terms(
    provider_entity_id: Vec<u8>,
    capability: &str,
    requirements_json: &str,
    production_registry: bool,
) -> PyResult<String> {
    let id: [u8; 32] = provider_entity_id.as_slice().try_into().map_err(|_| {
        PyValueError::new_err(format!(
            "provider_entity_id must be 32 bytes (got {})",
            provider_entity_id.len()
        ))
    })?;
    author_pricing_terms(id, capability, requirements_json, production_registry)
        .map_err(PyValueError::new_err)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOCK_REQS: &str = r#"[{"scheme":"mock","network":"mock:net","amount":"2500","asset":"musd","payTo":"mock-provider-settle-addr","maxTimeoutSeconds":60}]"#;

    #[test]
    fn authors_canonical_decodable_pricing_terms() {
        let terms = author_pricing_terms([7u8; 32], "prov/echo", MOCK_REQS, false).expect("author");

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
        let terms = author_pricing_terms([7u8; 32], "prov/echo", two, false).expect("author");
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
        assert!(author_pricing_terms([1u8; 32], "prov/echo", "[]", false).is_err());
        assert!(author_pricing_terms([1u8; 32], "prov/echo", "not json", false).is_err());
        // A requirement missing a required field (payTo) is a decode error.
        let bad = r#"[{"scheme":"mock","network":"mock:net","amount":"1","asset":"musd","maxTimeoutSeconds":60}]"#;
        assert!(author_pricing_terms([1u8; 32], "prov/echo", bad, false).is_err());
    }

    /// The `production_registry` flag has to *do* something, and this is
    /// the cheapest statement of what.
    ///
    /// `mock:net` is the sharp edge: it exists in the dev registry so the
    /// conformance suite can drive the whole lifecycle without a chain,
    /// and a provider settling real money has no reason to allowlist an
    /// asset whose settlements move nothing.
    ///
    /// The Node twin of this test exists because the flag went untested
    /// there and its whole test module drifted out of compiling.
    #[test]
    fn the_production_registry_refuses_the_valueless_mock_asset() {
        assert!(
            author_pricing_terms([7u8; 32], "prov/echo", MOCK_REQS, false).is_ok(),
            "the dev registry carries mock:net"
        );
        let err = author_pricing_terms([7u8; 32], "prov/echo", MOCK_REQS, true)
            .expect_err("the production registry must not price a valueless asset");
        assert!(
            err.contains("not in the selected registry"),
            "the refusal must name the registry check: {err}"
        );
    }

    /// Same terms, different registry revision — the two must not agree.
    ///
    /// `reference()` hashes the whole registry, so announcing under one
    /// revision while quoting under another leaves discovery metadata
    /// naming a registry the backend does not use. This pins that the flag
    /// actually reaches the reference rather than being decorative.
    #[test]
    fn the_registry_revision_rides_the_authored_terms() {
        // A real-money asset, so both registries carry it.
        let base_usdc = r#"[{"scheme":"exact","network":"eip155:8453","asset":"0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913","amount":"2500","payTo":"0xmerchant","maxTimeoutSeconds":60,"extra":{"name":"USDC","version":"2"}}]"#;
        let dev = author_pricing_terms([7u8; 32], "prov/echo", base_usdc, false).expect("dev");
        let prod = author_pricing_terms([7u8; 32], "prov/echo", base_usdc, true).expect("prod");
        assert_ne!(
            dev, prod,
            "the announced terms must carry the registry revision they were authored under"
        );
    }
}

// ---------------------------------------------------------------------------
// PaymentProvider — a Python node that PRICES + CHARGES for its own tools.
// One shared PaymentEngine serves the quote/pay wire AND gates the priced
// tools (redeem against the same engine). Needs the `publish` feature (the
// tool-publish building blocks) alongside `payments`.
// ---------------------------------------------------------------------------

/// The registry revision a real settlement backend puts the engine on.
/// Named once so the provider's authoring path and its ``registry_version``
/// property cannot drift apart.
const PRODUCTION_REGISTRY_VERSION: &str = "net-production-1";

#[cfg(feature = "publish")]
mod provider {
    use super::PRODUCTION_REGISTRY_VERSION;
    use crate::runtime_guard::GuardedRuntime;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    use pyo3::exceptions::{PyRuntimeError, PyValueError};
    use pyo3::prelude::*;

    use net::adapter::net::MeshNode;
    use net_mcp::serve::payment::PaymentAdmission;
    use net_payments::billing::BillingLog;
    use net_payments::core::registry::default_registry_v1;
    use net_payments::engine::{AdmitAll, PaymentEngine};
    use net_payments::facilitator::mock::MockFacilitator;
    use net_payments::flow::mcp_gate::EnginePaymentAdmission;
    use net_payments::flow::mesh::{serve_payments, PaymentServeHandle};
    use net_payments::flow::{Clock, InProcessProvider, SystemClock};

    // `mesh_over` (constructor) + the publication handle + the shared publish
    // scaffolding the paid path delegates to.
    use crate::publish::{mesh_over, mesh_publish_tools_configured, PyLocalPublicationHandle};

    /// Choose the settlement backend, or fail closed.
    ///
    /// Returns the facilitator plus the asset registry that goes with it:
    /// a real backend gets `production_registry_v1` (no `mock:net` entry —
    /// a provider settling real money has no reason to allowlist an asset
    /// whose settlements move nothing), the mock gets the dev registry
    /// that includes it.
    ///
    /// There is deliberately no default. A provider that does not say how
    /// it settles is a provider whose operator has not decided, and
    /// guessing "mock" for them is how a simulator ends up in front of
    /// real customers.
    fn resolve_facilitator(
        entity_id: net::adapter::net::identity::EntityId,
        facilitator_url: Option<String>,
        facilitator_auth_token: Option<String>,
        unsafe_dev_mock: bool,
    ) -> PyResult<(
        Arc<dyn net_payments::facilitator::Facilitator>,
        net_payments::core::registry::AssetRegistry,
    )> {
        match (facilitator_url, unsafe_dev_mock) {
            (Some(_), true) => Err(PyValueError::new_err(
                "PaymentProvider: pass either facilitator_url or \
                 unsafe_dev_mock_facilitator=True, not both — the mock settles \
                 nothing, so pairing it with a real facilitator URL is \
                 ambiguous about which one you meant",
            )),
            (None, false) => Err(PyValueError::new_err(
                "PaymentProvider: no settlement backend configured. Pass \
                 facilitator_url=\"https://...\" to settle for real (build with \
                 --features payments-http), or unsafe_dev_mock_facilitator=True \
                 to settle against the in-process mock, which moves no value. \
                 There is no default: a provider that publishes priced tools \
                 without a real facilitator serves for free.",
            )),
            (None, true) => {
                // stderr, not `tracing`: a Python embedder usually has no
                // subscriber installed, and a warning nobody sees is the
                // same as no warning. This one has to land.
                eprintln!(
                    "WARNING: PaymentProvider is using the MOCK facilitator. Quotes are \
                     signed, billing events are emitted, and tools are served — but NO \
                     VALUE MOVES. Development and conformance only."
                );
                Ok((
                    Arc::new(MockFacilitator::new()),
                    default_registry_v1(entity_id),
                ))
            }
            #[cfg(feature = "payments-http")]
            (Some(url), false) => {
                use net_payments::facilitator::client::{
                    AuthProvider, BearerAuth, HttpFacilitator, NoAuth,
                };
                let auth: Arc<dyn AuthProvider> = match facilitator_auth_token {
                    Some(token) => Arc::new(BearerAuth::new(token)),
                    None => Arc::new(NoAuth),
                };
                let facilitator = HttpFacilitator::new(&url, auth).map_err(|e| {
                    PyValueError::new_err(format!("PaymentProvider facilitator: {e}"))
                })?;
                Ok((
                    Arc::new(facilitator),
                    net_payments::core::registry::production_registry_v1(entity_id),
                ))
            }
            #[cfg(not(feature = "payments-http"))]
            (Some(_), false) => {
                let _ = facilitator_auth_token;
                Err(PyValueError::new_err(
                    "PaymentProvider: facilitator_url needs the `payments-http` \
                     build feature (it pulls reqwest + rustls). Rebuild with \
                     --features payments-http, or pass \
                     unsafe_dev_mock_facilitator=True for a mock backend. \
                     Refusing to silently downgrade a real facilitator URL to \
                     the mock.",
                ))
            }
        }
    }

    /// A paid-capability provider over an embedded `NetMesh` node — the supply
    /// side. Construction stands up one `PaymentEngine` behind the quote/pay
    /// wire; :meth:`publish_paid_tools` publishes priced tools gated by that
    /// same engine, so a quote paid over the wire is the quote the gate
    /// redeems. Hold the provider to keep the wire served.
    #[pyclass(name = "PaymentProvider", module = "net._net")]
    pub struct PyPaymentProvider {
        engine: Arc<PaymentEngine>,
        node: Arc<MeshNode>,
        runtime: Arc<GuardedRuntime>,
        provider_entity_id: Vec<u8>,
        /// The asset registry revision the engine issues quotes under, which
        /// follows from the settlement backend that was chosen.
        registry_version: String,
        /// The billing stream, when a `billing_log_path` was supplied — for the
        /// read-only `read_billing` surface.
        billing: Option<Arc<BillingLog>>,
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
        ///
        /// **A settlement backend must be chosen explicitly.** Pass
        /// ``facilitator_url`` (plus ``facilitator_auth_token`` where the
        /// facilitator requires one) to settle for real, or
        /// ``unsafe_dev_mock_facilitator=True`` to settle against the
        /// in-process mock, which moves no value. Supplying neither is an
        /// error, and supplying both is an error.
        ///
        /// This constructor used to build a ``MockFacilitator``
        /// unconditionally, with no way to reach a real one: a provider
        /// could publish priced tools, sign quotes with its real mesh
        /// identity, emit signed billing events, and serve — while
        /// settlement moved nothing. Choosing is now mandatory, and the
        /// unsafe option says so in its name.
        ///
        /// ``facilitator_url`` requires the ``payments-http`` build
        /// feature (it pulls reqwest + rustls); without it, the only
        /// available backend is the mock, and asking for a real one is a
        /// build error rather than a silent downgrade.
        ///
        /// ``require_invocation_binding`` **defaults to True**: a paid
        /// invocation is refused unless the caller presents the paying
        /// identity's signature over the invocation-binding transcript.
        ///
        /// Without it the quote id alone redeems, and the quote id is not
        /// a secret — it rides a request header on every paid invoke and
        /// is carried on the billing event, so anything that learns one
        /// can spend it. Defaulting off would mean every provider stayed
        /// exposed unless its operator found the flag, which is not a
        /// posture a payments surface should ship.
        ///
        /// Pass ``False`` only for a deployment whose callers predate the
        /// binding. Anything built on ``CapabilityGateway`` already signs
        /// one whenever its identity can sign.
        ///
        /// Scope: this closes **off-path** leakage of the quote id (logs,
        /// billing records, proofs). It does not stop an intermediary
        /// that observes the paid invocation itself and copies both
        /// headers — that needs channel binding, not a bigger
        /// signature.
        #[new]
        #[pyo3(signature = (
            mesh,
            state_path,
            billing_log_path=None,
            facilitator_url=None,
            facilitator_auth_token=None,
            unsafe_dev_mock_facilitator=false,
            require_invocation_binding=true,
        ))]
        #[allow(clippy::too_many_arguments)]
        fn new(
            mesh: &crate::mesh_bindings::NetMesh,
            state_path: String,
            billing_log_path: Option<String>,
            facilitator_url: Option<String>,
            facilitator_auth_token: Option<String>,
            unsafe_dev_mock_facilitator: bool,
            require_invocation_binding: bool,
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
            let (facilitator, registry) = resolve_facilitator(
                entity_id,
                facilitator_url,
                facilitator_auth_token,
                unsafe_dev_mock_facilitator,
            )?;
            let registry_version = registry.version.clone();
            // `AdmitAll` gates QUOTE issuance — correct for a paid tool (anyone
            // may quote; PAYMENT is the real gate on the serve).
            let billing = billing_log_path.map(|bp| Arc::new(BillingLog::new(bp)));
            let mut engine = PaymentEngine::new(
                provider,
                facilitator,
                Arc::new(AdmitAll),
                registry,
                PathBuf::from(state_path),
            )
            .map_err(|e| PyRuntimeError::new_err(format!("payment engine: {e}")))?
            .with_require_invocation_binding(require_invocation_binding);
            if let Some(b) = &billing {
                engine = engine.with_billing_log(b.clone());
            }
            let engine = Arc::new(engine);

            let clock: Arc<dyn Clock> = Arc::new(SystemClock);
            let in_process = Arc::new(InProcessProvider::new(engine.clone(), clock));
            // serve_payments registers the quote/pay RPC handlers, which spawn
            // tasks — so it must run inside the runtime context. The caller's
            // main thread (e.g. pytest) has no reactor, so enter the mesh's
            // runtime explicitly (same reason payment_http enters it for reqwest;
            // else `tokio::spawn` panics "no reactor running").
            let serve = {
                let _guard = runtime.enter();
                serve_payments(&sdk_mesh, in_process)
            }
            .map_err(|e| PyRuntimeError::new_err(format!("serve payments: {e}")))?;

            Ok(Self {
                engine,
                node,
                runtime,
                provider_entity_id,
                registry_version,
                billing,
                _serve: serve,
            })
        }

        /// The node's 32-byte mesh entity id — the provider identity these tools
        /// price + quote under. Pass it to :func:`build_pricing_terms`.
        #[getter]
        fn provider_entity_id(&self) -> Vec<u8> {
            self.provider_entity_id.clone()
        }

        /// Author this provider's ``net.pricing.terms@1`` for
        /// ``capability``, against **the registry its own engine quotes
        /// under**.
        ///
        /// The same authoring as the free :func:`build_pricing_terms`,
        /// minus the two ways to get it wrong. That function takes the
        /// provider id and a ``production_registry`` flag as separate
        /// arguments, either of which can disagree with the provider that
        /// will serve the quotes: announce the dev revision while quoting
        /// under the production one and a caller picks an asset the backend
        /// will never quote, then gets refused with no other entry to fall
        /// back to. Here both come from the engine.
        ///
        /// ``requirements_json`` is the same JSON array of x402
        /// ``PaymentRequirements`` objects (camelCase wire names). Every
        /// entry is checked twice before the terms are returned, so this
        /// raises rather than announcing something unquotable:
        ///
        /// - against the **registry**, which answers "is this an asset
        ///   this provider knows";
        /// - against the **settlement backend's** ``GET /supported``,
        ///   which answers "is this a route its facilitator will
        ///   actually settle".
        ///
        /// The second is the one the free function cannot do: it has no
        /// facilitator to ask. A provider that passes the registry check
        /// can still announce a ``(scheme, network)`` its facilitator has
        /// never handled, and the caller finds out after signing an
        /// authorization. Behind the mock — which has no discovery
        /// surface — only the registry check applies.
        ///
        /// Reaches the facilitator over the network, so call it when
        /// publishing, not per request.
        fn pricing_terms(&self, capability: &str, requirements_json: &str) -> PyResult<String> {
            let id: [u8; 32] = self
                .provider_entity_id
                .as_slice()
                .try_into()
                .map_err(|_| PyValueError::new_err("provider entity id is not 32 bytes"))?;
            let terms = crate::payment_provider::author_pricing_terms(
                id,
                capability,
                requirements_json,
                self.registry_version == PRODUCTION_REGISTRY_VERSION,
            )
            .map_err(PyValueError::new_err)?;
            let accepts = crate::payment_provider::parse_requirements(requirements_json)
                .map_err(PyValueError::new_err)?;
            self.runtime
                .block_on(self.engine.check_settlement_routes(&accepts))
                .map_err(|e| PyValueError::new_err(e.to_string()))?;
            Ok(terms)
        }

        /// The asset registry revision this provider issues quotes under —
        /// ``"net-production-1"`` behind a real facilitator,
        /// ``"net-default-1"`` behind the mock (which additionally carries
        /// the valueless ``mock:net`` asset).
        ///
        /// Two uses. It tells :func:`build_pricing_terms` which revision to
        /// author against (``production_registry=True`` iff this reads
        /// ``"net-production-1"``), so announced terms and issued quotes name
        /// the same revision. And it makes the settlement backend
        /// *observable*: the one failure this surface must never have is
        /// quietly falling back to the mock for an operator who configured a
        /// facilitator URL, and a guarantee nothing can read is a guarantee
        /// nothing can test.
        #[getter]
        fn registry_version(&self) -> String {
            self.registry_version.clone()
        }

        /// The immutable billing events this provider recorded, oldest first —
        /// each a ``net.billing.event@1`` JSON string. Read-only (billing is
        /// emitted by the engine; this only reads). Requires a
        /// ``billing_log_path`` at construction, else raises ``ValueError``.
        /// Releases the GIL while reading.
        fn read_billing(&self, py: Python<'_>) -> PyResult<Vec<String>> {
            let Some(billing) = &self.billing else {
                return Err(PyValueError::new_err(
                    "no billing log configured — construct PaymentProvider with billing_log_path",
                ));
            };
            let billing = billing.clone();
            let runtime = self.runtime.clone();
            py.detach(move || {
                let events = runtime
                    .block_on(billing.read_all())
                    .map_err(|e| PyRuntimeError::new_err(format!("read billing log: {e}")))?;
                events
                    .iter()
                    .map(|e| {
                        serde_json::to_string(e).map_err(|err| {
                            PyRuntimeError::new_err(format!("serialize billing event: {err}"))
                        })
                    })
                    .collect()
            })
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
            // Fail-closed: EVERY tool must be priced. Pricing is looked up by the
            // original tool name (`lower_tool` does `ctx.pricing.get(&tool.name)`),
            // and an absent entry publishes that tool FREE — so a forgotten key
            // would silently leak a paid tool onto the free path, contradicting
            // this API's paid-only contract. (`ServerPublisher` already rejects
            // the reverse — pricing keys naming no tool.)
            let unpriced: Vec<&str> = tools
                .iter()
                .filter(|(name, _, _)| !pricing.contains_key(name))
                .map(|(name, _, _)| name.as_str())
                .collect();
            if !unpriced.is_empty() {
                return Err(PyValueError::new_err(format!(
                    "publish_paid_tools: {unpriced:?} have no pricing entry (would publish \
                     free) — every tool needs a net.pricing.terms@1 entry keyed by its \
                     name, or use NetMesh.publish_tools for free tools"
                )));
            }
            // The paid path = the shared publish scaffolding + this provider's
            // pricing + engine gate. The gate redeems quotes against THIS
            // provider's engine — the same engine the quote/pay wire serves — so
            // a paid tool serves once, after payment. (`ServerPublisher` rejects
            // a priced tool with no gate; here the gate is always set.)
            let admission: Arc<dyn PaymentAdmission> =
                Arc::new(EnginePaymentAdmission::new(self.engine.clone()));
            mesh_publish_tools_configured(
                py,
                self.node.clone(),
                self.runtime.clone(),
                tools,
                callback,
                version,
                owner_origin,
                allow_any_caller,
                pricing.into_iter().collect(),
                Some(admission),
            )
        }
    }
}

#[cfg(feature = "publish")]
pub use provider::PyPaymentProvider;
