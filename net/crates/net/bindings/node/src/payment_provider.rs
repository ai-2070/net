//! Provider-side payment surface: pricing a capability + charging for it — the
//! supply-side twin of `capability_gateway.rs` and the Node mirror of the Python
//! `payment_provider.rs`. What a Node node needs to *be* a paid provider: author
//! `net.pricing.terms@1` ([`build_pricing_terms`]) and stand up a
//! [`PaymentProvider`](provider::PaymentProvider) that runs one shared
//! `PaymentEngine` behind the quote/pay wire and gates its priced tools (the MCP
//! wrap `payment_admission` path).
//!
//! Doctrine #1 holds: the engine + settlement logic is `net-payments`; this
//! marshals config in. The `PaymentProvider` class layers onto the free
//! `publishTools` path ([`crate::publish`]) — it reuses that module's tool
//! marshaling + `ToolInvoker` bridge + publication handle, adding pricing on
//! `WrapConfig.pricing` and an `EnginePaymentAdmission` gate. Behind `payments`
//! (the pricing author) + `publish` (the provider class).

#![cfg(feature = "payments")]
// napi-derive registers `build_pricing_terms` + the `PaymentProvider` methods
// via a generated `extern "C"` table the dead-code lint can't trace; under
// `cargo clippy --tests` the free `#[napi] fn` otherwise reads as unused (same
// guard as `blob.rs` / `publish.rs`).
#![allow(dead_code)]

use napi::bindgen_prelude::*;
use napi_derive::napi;

use net::adapter::net::identity::EntityId;
use net_payments::core::canonical::canonical_bytes;
use net_payments::core::registry::default_registry_v1;
use net_payments::core::terms::PricingTerms;
use net_payments::x402::requirements::PaymentRequirements;
use net_payments::x402::X402Carry;

/// Author the canonical `net.pricing.terms@1` JSON for a capability from a
/// provider entity id + a JSON array of x402 `PaymentRequirements`. Pure — the
/// napi wrapper below is a thin marshaling shell. Mirrors the Python
/// `author_pricing_terms`.
fn author_pricing_terms(
    provider_entity_id: [u8; 32],
    capability: &str,
    requirements_json: &str,
) -> std::result::Result<String, String> {
    let reqs: Vec<PaymentRequirements> = serde_json::from_str(requirements_json).map_err(|e| {
        format!("requirementsJson must be a JSON array of x402 PaymentRequirements objects: {e}")
    })?;
    if reqs.is_empty() {
        return Err(
            "at least one payment requirement is required — an empty accepts[] prices nothing"
                .to_string(),
        );
    }
    // Locally-originated x402: `author` is the sanctioned serialization point
    // (these templates originate here, so the bytes become the preserved
    // originals — no byte-preservation violation).
    let accepts = reqs
        .iter()
        .map(X402Carry::author)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| format!("author payment requirement: {e}"))?;
    let provider = EntityId::from_bytes(provider_entity_id);
    // The v1 default registry (mock + survey networks). Its `reference()` is
    // signer-independent (it hashes the asset content), so it matches any caller
    // authoring quotes under the same default registry.
    let registry = default_registry_v1(provider.clone());
    let reference = registry
        .reference()
        .map_err(|e| format!("registry reference: {e}"))?;
    let terms = PricingTerms::new(provider, capability, accepts, reference);
    let bytes = canonical_bytes(&terms).map_err(|e| format!("canonicalize terms: {e}"))?;
    String::from_utf8(bytes).map_err(|e| format!("terms are not UTF-8: {e}"))
}

/// Author the canonical `net.pricing.terms@1` JSON string that prices a
/// capability — to hand to `publishPaidTools` or announce at discovery.
///
/// `providerEntityId` is the node's 32-byte mesh entity id
/// (`PaymentProvider.providerEntityId`) — the identity that issues quotes for
/// these terms. Only the public id crosses; keys never do. `requirementsJson` is
/// a JSON array of x402 `PaymentRequirements` objects (`scheme`, `network`,
/// `amount`, `asset`, `payTo`, `maxTimeoutSeconds`, optional `extra` — the x402
/// camelCase wire names); one entry per acceptable `(scheme, network, asset)`.
/// Returns the canonical, byte-preserved terms string — opaque downstream and
/// echoed verbatim at discovery. Throws on a bad entity id, malformed JSON, or
/// an empty list.
#[napi]
pub fn build_pricing_terms(
    provider_entity_id: Buffer,
    capability: String,
    requirements_json: String,
) -> Result<String> {
    let id: [u8; 32] = provider_entity_id.as_ref().try_into().map_err(|_| {
        Error::from_reason(format!(
            "providerEntityId must be 32 bytes (got {})",
            provider_entity_id.len()
        ))
    })?;
    author_pricing_terms(id, &capability, &requirements_json).map_err(Error::from_reason)
}

// ---------------------------------------------------------------------------
// PaymentProvider — a Node node that PRICES + CHARGES for its own tools. One
// shared PaymentEngine serves the quote/pay wire AND gates the priced tools
// (redeem against the same engine). Needs the `publish` feature (the
// tool-publish building blocks) alongside `payments`.
// ---------------------------------------------------------------------------

#[cfg(feature = "publish")]
mod provider {
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    use napi::bindgen_prelude::*;
    use napi_derive::napi;
    use parking_lot::Mutex;

    use net::adapter::net::MeshNode;
    use net_mcp::spec::Implementation;
    use net_mcp::wrap::{OwnerScope, ServerPublisher, WrapConfig};
    use net_payments::billing::BillingLog;
    use net_payments::core::registry::default_registry_v1;
    use net_payments::engine::{AdmitAll, PaymentEngine};
    use net_payments::facilitator::mock::MockFacilitator;
    use net_payments::flow::mcp_gate::EnginePaymentAdmission;
    use net_payments::flow::mesh::{serve_payments, PaymentServeHandle};
    use net_payments::flow::{Clock, InProcessProvider, SystemClock};

    use crate::publish::{
        build_sdk_tools, build_tool_invoker, local_lowering_context, mesh_over, parse_owner_origin,
        LocalPublicationHandle, PublishOptions, PublishToolJs, ToolCallResultJs, ToolInvokeArgs,
    };
    use crate::NetMesh;

    /// A paid-capability provider over an embedded `NetMesh` node — the supply
    /// side. Construction stands up one `PaymentEngine` behind the quote/pay
    /// wire; `publishPaidTools` publishes priced tools gated by that same
    /// engine, so a quote paid over the wire is the quote the gate redeems. Hold
    /// the provider to keep the wire served.
    ///
    /// Construct with `new PaymentProvider(mesh, statePath, billingLogPath?)`.
    /// The node-holding state — the mesh node the provider serves over, plus the
    /// serve handle keeping the quote/pay services registered on it.
    /// [`close`](PaymentProvider::close) drops both so the node can be released.
    struct Serving {
        node: Arc<MeshNode>,
        /// Keeps the `net.payments.quote/pay` services registered on the node.
        _serve: PaymentServeHandle,
    }

    #[napi]
    pub struct PaymentProvider {
        engine: Arc<PaymentEngine>,
        provider_entity_id: Vec<u8>,
        /// The billing stream, when a `billingLogPath` was supplied — for the
        /// read-only `readBilling` surface. Holds no node reference.
        billing: Option<Arc<BillingLog>>,
        /// The node + serve handle, behind a lock so `close()` can drop them
        /// (releasing the node clone + unregistering the quote/pay wire). `None`
        /// once closed — a `#[napi]` class is GC-finalized, not scope-dropped,
        /// so without an explicit release the retained node clone would keep
        /// `NetMesh.shutdown()` (which needs sole ownership) failing until GC ran.
        serving: Mutex<Option<Serving>>,
    }

    #[napi]
    impl PaymentProvider {
        /// Build a provider over a started `mesh`. `statePath` is the settlement
        /// store file — it holds the replay/idempotency index and **must be
        /// durable + single-owner** (a temp path loses paid quotes across
        /// restarts). `billingLogPath` optionally records the immutable
        /// `net.billing.event@1` stream.
        #[napi(constructor)]
        pub fn new(
            mesh: &NetMesh,
            state_path: String,
            billing_log_path: Option<String>,
        ) -> Result<Self> {
            let node = mesh.node_arc_clone().map_err(|_| {
                Error::from_reason("payment provider: mesh node has been shut down")
            })?;
            // The provider payment identity IS the node's mesh identity: quotes
            // are signed by, and settlement tracked against, the same ed25519
            // identity peers see (matching the pricing terms' provider + the
            // caller-side payment identity). Borrowed in-process — no key
            // material crosses the boundary.
            let sdk_mesh = mesh_over(node.clone());
            let provider = Arc::new(sdk_mesh.entity_keypair().clone());
            let entity_id = provider.entity_id().clone();
            let provider_entity_id = entity_id.as_bytes().to_vec();
            let registry = default_registry_v1(entity_id);
            // `AdmitAll` gates QUOTE issuance — correct for a paid tool (anyone
            // may quote; PAYMENT is the real gate on the serve).
            let billing = billing_log_path.map(|bp| Arc::new(BillingLog::new(bp)));
            let mut engine = PaymentEngine::new(
                provider,
                Arc::new(MockFacilitator::new()),
                Arc::new(AdmitAll),
                registry,
                PathBuf::from(state_path),
            )
            .map_err(|e| Error::from_reason(format!("payment engine: {e}")))?;
            if let Some(b) = &billing {
                engine = engine.with_billing_log(b.clone());
            }
            let engine = Arc::new(engine);

            let clock: Arc<dyn Clock> = Arc::new(SystemClock);
            let in_process = Arc::new(InProcessProvider::new(engine.clone(), clock));
            // serve_payments registers the quote/pay RPC handlers, which spawn
            // tasks — so it must run inside napi's tokio runtime context. This
            // `#[napi(constructor)]` runs on the JS thread, which is NOT in that
            // context, so enter it explicitly (else `tokio::spawn` panics "no
            // reactor running"). The handlers themselves run later on the runtime.
            let serve = napi::bindgen_prelude::within_runtime_if_available(|| {
                serve_payments(&sdk_mesh, in_process)
            })
            .map_err(|e| Error::from_reason(format!("serve payments: {e}")))?;

            Ok(Self {
                engine,
                provider_entity_id,
                billing,
                serving: Mutex::new(Some(Serving {
                    node,
                    _serve: serve,
                })),
            })
        }

        /// Release the mesh node + tear down the quote/pay wire so the underlying
        /// `NetMesh` can be `shutdown()` deterministically (a `#[napi]` class is
        /// GC-finalized, not scope-dropped). Call it — plus `stop()`/`withdraw()`
        /// on any handles from `publishPaidTools` — before `mesh.shutdown()`.
        /// Idempotent; after `close()`, `publishPaidTools` throws. `readBilling`
        /// + `providerEntityId` still work (they hold no node reference).
        #[napi]
        pub fn close(&self) {
            let _ = self.serving.lock().take();
        }

        /// The node's 32-byte mesh entity id — the provider identity these tools
        /// price + quote under. Pass it to `buildPricingTerms`.
        #[napi(getter)]
        pub fn provider_entity_id(&self) -> Buffer {
            Buffer::from(self.provider_entity_id.clone())
        }

        /// The immutable billing events this provider recorded, oldest first —
        /// each a `net.billing.event@1` JSON string. Read-only (billing is
        /// emitted by the engine; this only reads). Requires a `billingLogPath`
        /// at construction, else rejects.
        #[napi]
        pub async fn read_billing(&self) -> Result<Vec<String>> {
            // Clone the `Arc` out of `&self` before the await (napi async `&self`
            // borrow must not cross it).
            let Some(billing) = self.billing.clone() else {
                return Err(Error::from_reason(
                    "no billing log configured — construct PaymentProvider with billingLogPath",
                ));
            };
            let events = billing
                .read_all()
                .await
                .map_err(|e| Error::from_reason(format!("read billing log: {e}")))?;
            events
                .iter()
                .map(|e| {
                    serde_json::to_string(e).map_err(|err| {
                        Error::from_reason(format!("serialize billing event: {err}"))
                    })
                })
                .collect()
        }

        /// Publish priced tools, gated by this provider's payment engine. `tools`
        /// + `handler` + `options` are exactly as on `NetMesh.publishTools`;
        /// `pricing` maps a tool name to its `net.pricing.terms@1` JSON (from
        /// `buildPricingTerms`). A priced tool serves only **after** its quote is
        /// paid + redeemed (at-most-once, against this same engine). Fail-closed:
        /// an empty `pricing` map throws (use `NetMesh.publishTools` for free
        /// tools); a pricing key naming no published tool is a publish error.
        /// Resolves to a `LocalPublicationHandle` — hold it to keep serving.
        #[napi]
        pub fn publish_paid_tools<'env>(
            &self,
            env: &'env Env,
            tools: Vec<PublishToolJs>,
            handler: Function<'_, ToolInvokeArgs, Promise<ToolCallResultJs>>,
            pricing: std::collections::HashMap<String, String>,
            options: Option<PublishOptions>,
        ) -> Result<PromiseRaw<'env, LocalPublicationHandle>> {
            if pricing.is_empty() {
                return Err(Error::from_reason(
                    "publishPaidTools requires a non-empty pricing map \
                     (tool name -> net.pricing.terms@1 JSON from buildPricingTerms); \
                     use NetMesh.publishTools for free tools",
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
                .filter(|t| !pricing.contains_key(&t.name))
                .map(|t| t.name.as_str())
                .collect();
            if !unpriced.is_empty() {
                return Err(Error::from_reason(format!(
                    "publishPaidTools: {unpriced:?} have no pricing entry (would publish \
                     free) — every tool needs a net.pricing.terms@1 entry keyed by its \
                     name, or use NetMesh.publishTools for free tools"
                )));
            }
            // Validate + marshal synchronously (before the publish round-trip),
            // then build the invoker on the JS thread — everything after is
            // `Send` state for the future.
            let sdk_tools = build_sdk_tools(&tools)?;
            let opts = options.unwrap_or(PublishOptions {
                version: None,
                owner_origin: None,
                allow_any_caller: None,
                handler_timeout_ms: None,
            });
            let allow_any_caller = opts.allow_any_caller.unwrap_or(false);
            // `allowAnyCaller` overrides `ownerOrigin` (the scope becomes
            // `any()`), so don't validate a value that's about to be ignored.
            let owner_origin = if allow_any_caller {
                None
            } else {
                parse_owner_origin(opts.owner_origin)?
            };
            let ctx = local_lowering_context(opts.version);
            let invoker = build_tool_invoker(handler, opts.handler_timeout_ms)?;
            let pricing: BTreeMap<String, String> = pricing.into_iter().collect();
            let engine = self.engine.clone();
            // Clone the live node out from behind the lock (no guard crosses the
            // future). Closed → nothing to serve over.
            let Some(node) = self.serving.lock().as_ref().map(|s| s.node.clone()) else {
                return Err(Error::from_reason(
                    "payment provider has been closed — construct a new one to publish",
                ));
            };

            env.spawn_future(async move {
                let mesh = Arc::new(mesh_over(node));
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
                config.pricing = pricing;
                // The gate: redeem quotes against THIS provider's engine — the
                // same engine the quote/pay wire serves — so a paid tool serves
                // once, after payment. A priced tool with no gate is a wrap
                // error; here the gate is always set.
                config.payment_admission = Some(Arc::new(EnginePaymentAdmission::new(engine)));

                let publisher = ServerPublisher::new(mesh);
                let handle = publisher
                    .publish_tools(&sdk_tools, invoker, ctx, config)
                    .await
                    .map_err(|e| Error::from_reason(format!("publishPaidTools failed: {e}")))?;
                Ok(LocalPublicationHandle::wrap(handle))
            })
        }
    }
}

// `PaymentProvider` (in `mod provider`) auto-registers with napi via the
// generated table — no crate-root re-export needed (unlike the Python binding's
// `add_class`).

// ---------------------------------------------------------------------------
// Pure marshaling tests — the pricing author is the binding's whole job here, so
// its shape is pinned (runs under the napi test-link workaround).
// ---------------------------------------------------------------------------

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
