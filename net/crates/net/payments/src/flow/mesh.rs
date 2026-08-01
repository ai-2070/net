//! The mesh wire for the payment lifecycle: two nRPC services carrying
//! the [`ProviderChannel`] contract across machines.
//!
//! - `net.payments.quote.v1` — quote issuance. Request names the caller
//!   identity, the capability, and the announced template (base64 of the
//!   preserved bytes); the response is the provider-signed quote's
//!   canonical envelope bytes.
//! - `net.payments.pay.v1` — payment delivery. Request carries the quote
//!   envelope bytes + the x402 payload bytes; the response is the
//!   [`PayResponse`] wire projection (billing events travel as canonical
//!   bytes, signatures intact).
//!
//! Everything crosses the wire byte-preserved and base64-framed — no
//! re-serialization of signed material anywhere on the path. The
//! provider side delegates to [`InProcessProvider`], so a mesh handler
//! and a local test exercise identical code ("same lifecycle on every
//! network" extends to "same lifecycle at every distance").

use std::sync::Arc;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use net::adapter::net::identity::{EntityId, EntityKeypair};
use net_sdk::mesh::Mesh;
use net_sdk::mesh_rpc::{Codec, RpcError, ServeError, ServeHandle};
use serde::{Deserialize, Serialize};

use super::{ChannelError, Clock, InProcessProvider, PayResponse, ProviderChannel};
use crate::core::canonical::SignedEnvelope as _;
use crate::core::quote_request::{QuoteRequest, SeenNonces};
use crate::x402::payload::PaymentPayload;
use crate::x402::requirements::PaymentRequirements;
use crate::x402::X402Carry;

/// How long a signed quote request stays valid. Short: it is a bearer
/// credential inside its window, and a quote round trip does not need
/// more. Clamped by `MAX_REQUEST_LIFETIME_NS` regardless.
const QUOTE_REQUEST_TTL_NS: u64 = 30_000_000_000;

/// Clock-skew tolerance the provider allows on a request's freshness
/// window. There is no global clock, so some tolerance is required; it is
/// deliberately much smaller than the TTL so it widens the replay window
/// only marginally.
const QUOTE_REQUEST_SKEW_NS: u64 = 5_000_000_000;

/// Quote-issuance service name (nRPC; channel-safe, so `.v1` not `@1`).
pub const QUOTE_SERVICE: &str = "net.payments.quote.v1";
/// Payment-delivery service name.
pub const PAY_SERVICE: &str = "net.payments.pay.v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct QuoteWireRequest {
    /// Canonical bytes of the caller-signed `net.payment.quote_request@1`,
    /// base64.
    ///
    /// This replaced a bare `caller_hex` field. An `EntityId` is a public
    /// key, so a self-asserted one proves nothing: naming an admitted
    /// caller cleared provider admission, and naming a victim put their
    /// identity on the provider's signed billing event. The envelope
    /// carries the same identity plus a signature over it, bound to this
    /// provider, capability, template and a freshness window.
    request_b64: String,
    /// The announced template's preserved bytes, base64.
    ///
    /// Carried beside the envelope rather than inside it because the
    /// envelope binds them by hash: `verify` recomputes it, so substituted
    /// bytes are refused. Keeping them out of the signed transcript means
    /// the signature stays small and the preserved bytes stay preserved —
    /// they never round-trip through a second encoder.
    template_b64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct QuoteWireResponse {
    quote_b64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PayWireRequest {
    quote_b64: String,
    payload_b64: String,
}

/// Keeps both payment services registered; dropping it unregisters them.
pub struct PaymentServeHandle {
    _quote: ServeHandle,
    _pay: ServeHandle,
}

/// Register the provider side of the payment wire on `mesh`, delegating
/// to `provider` (the same [`InProcessProvider`] local flows use).
/// Provider policy runs inside quote issuance; the engine's replay index
/// and idempotency make the pay service safe under retries.
pub fn serve_payments(
    mesh: &Mesh,
    provider: Arc<InProcessProvider>,
) -> Result<PaymentServeHandle, ServeError> {
    let quote_provider = provider.clone();
    // Replay guard for quote-request nonces, shared across the service.
    // In-process by design: a replayed request costs a duplicate quote and
    // nothing else (quotes are free, and paying one still needs the
    // caller's settlement authorization), so this does not earn a place in
    // the locked store on the path of every quote. The guard that must be
    // durable is the one on payment, and that one is.
    let seen: Arc<parking_lot::Mutex<SeenNonces>> =
        Arc::new(parking_lot::Mutex::new(SeenNonces::new()));
    let quote =
        mesh.serve_rpc_typed(QUOTE_SERVICE, Codec::Json, move |req: QuoteWireRequest| {
            let provider = quote_provider.clone();
            let seen = seen.clone();
            async move {
                let request_bytes = BASE64
                    .decode(&req.request_b64)
                    .map_err(|e| format!("quote request is not base64: {e}"))?;
                // Decode only to learn which template and capability the
                // request claims; NOTHING is trusted until `verify` below
                // checks the signature over exactly these fields.
                let claimed: QuoteRequest = serde_json::from_slice(&request_bytes)
                    .map_err(|e| format!("quote request is not a valid envelope: {e}"))?;
                let template_bytes = BASE64
                    .decode(&req.template_b64)
                    .map_err(|e| format!("template is not base64: {e}"))?;
                let template: X402Carry<PaymentRequirements> =
                    X402Carry::from_bytes(template_bytes.clone()).map_err(|e| e.to_string())?;

                let now_ns = provider.now_ns();
                let verified = QuoteRequest::verify(
                    &request_bytes,
                    provider.provider_id(),
                    &claimed.capability,
                    &template_bytes,
                    now_ns,
                    QUOTE_REQUEST_SKEW_NS,
                )
                .map_err(|e| e.to_string())?;
                // Check-and-set before issuance, so two concurrent copies
                // of one request cannot both mint a quote.
                seen.lock()
                    .admit(
                        &verified.caller,
                        &verified.nonce,
                        verified.expires_at_ns,
                        now_ns,
                    )
                    .map_err(|e| e.to_string())?;

                // From here the caller identity is proven, not claimed.
                let issued = provider
                    .quote(
                        &verified.caller,
                        provider.provider_id(),
                        &verified.capability,
                        &template,
                    )
                    .await;
                let quote_bytes = match issued {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        // Nothing was issued, so nothing needs replay
                        // protection. Holding the nonce would retain state
                        // for work never done — the shape an attacker uses
                        // to fill the guard with denied requests — and
                        // would block this caller's legitimate retry once
                        // the denial is fixed (an allowlist edit, say).
                        seen.lock().release(&verified.caller, &verified.nonce);
                        return Err(e.message);
                    }
                };
                Ok(QuoteWireResponse {
                    quote_b64: BASE64.encode(quote_bytes),
                })
            }
        })?;

    let pay = mesh.serve_rpc_typed(PAY_SERVICE, Codec::Json, move |req: PayWireRequest| {
        let provider = provider.clone();
        async move {
            let quote_bytes = BASE64
                .decode(&req.quote_b64)
                .map_err(|e| format!("quote is not base64: {e}"))?;
            let payload_bytes = BASE64
                .decode(&req.payload_b64)
                .map_err(|e| format!("payload is not base64: {e}"))?;
            let payload: X402Carry<PaymentPayload> =
                X402Carry::from_bytes(payload_bytes).map_err(|e| e.to_string())?;
            let response: PayResponse = provider
                .pay(&quote_bytes, &payload)
                .await
                .map_err(|e| e.message)?;
            Ok::<PayResponse, String>(response)
        }
    })?;

    Ok(PaymentServeHandle {
        _quote: quote,
        _pay: pay,
    })
}

/// The caller side of the payment wire: a [`ProviderChannel`] that
/// resolves the provider node from the capability id's provider segment
/// (`<node_id>/<capability>`, decimal or `0x`-hex — the same spellings
/// the consent surface canonicalizes) and calls the two services
/// directly. Direct addressing on purpose: the node that signed the
/// quote is the only node that can accept its payment; discovery-routed
/// payments to an equivalent provider would fail the quote's provider
/// binding (correctly, but pointlessly).
pub struct MeshPaymentChannel {
    mesh: Arc<Mesh>,
    /// The caller identity, needed to **sign** each quote request.
    ///
    /// The channel holds the keypair rather than taking an `EntityId`
    /// because a quote request is a signed envelope now: the identity a
    /// request names is only worth something if the requester proves it
    /// holds the key. A public-only keypair therefore cannot request
    /// quotes over the mesh at all, and fails loudly at `quote` rather
    /// than sending an unsigned request that the provider would refuse.
    caller: Arc<EntityKeypair>,
    /// Timestamps the request's freshness window.
    ///
    /// There is no global clock in this crate, and a quote request is
    /// checked against the *provider's* clock — so the caller must stamp
    /// from the same source the rest of the flow does, or every request
    /// looks like it was issued in the future. Tests inject a fixed
    /// instant on both sides; production uses `SystemClock` on both.
    clock: Arc<dyn Clock>,
    /// Per-request counter feeding the quote-request nonce.
    ///
    /// The clock alone cannot separate two requests: `Clock` is a public
    /// seam with no monotonicity requirement, so a fixed or coarse
    /// implementation would derive one nonce for two legitimate
    /// back-to-back requests and the provider would refuse the second as
    /// a replay. A counter cannot repeat within a process, and a
    /// transport-level retransmit re-sends the already-serialized request
    /// rather than re-deriving, so retry idempotency is unaffected.
    request_seq: std::sync::atomic::AtomicU64,
}

impl MeshPaymentChannel {
    pub fn new(mesh: Arc<Mesh>, caller: Arc<EntityKeypair>, clock: Arc<dyn Clock>) -> Self {
        Self {
            mesh,
            caller,
            clock,
            request_seq: std::sync::atomic::AtomicU64::new(0),
        }
    }

    fn provider_node(capability: &str) -> Result<u64, ChannelError> {
        let provider = capability.split('/').next().unwrap_or_default();
        let parsed = if let Some(hex_part) = provider.strip_prefix("0x") {
            u64::from_str_radix(hex_part, 16).ok()
        } else {
            provider.parse::<u64>().ok()
        };
        parsed.ok_or_else(|| ChannelError {
            message: format!(
                "capability `{capability}` has no resolvable provider node id — the mesh \
                 payment channel needs `<node_id>/<capability>`"
            ),
            retryable: false,
        })
    }

    fn map_rpc_error(e: RpcError) -> ChannelError {
        let retryable = matches!(e, RpcError::Timeout { .. } | RpcError::NoRoute { .. });
        ChannelError {
            message: e.to_string(),
            retryable,
        }
    }
}

#[async_trait::async_trait]
impl ProviderChannel for MeshPaymentChannel {
    async fn quote(
        &self,
        caller: &EntityId,
        provider: &EntityId,
        capability: &str,
        template: &X402Carry<PaymentRequirements>,
    ) -> Result<Vec<u8>, ChannelError> {
        // The flow's caller and this channel's signing identity must be
        // the same, or the request would name one identity and be signed
        // by another — which the provider refuses. Catch it here, where
        // the message can say what is actually wrong.
        if caller != self.caller.entity_id() {
            return Err(ChannelError {
                message: "the flow's caller identity does not match this channel's signing                           identity — a quote request must be signed by the identity it names"
                    .to_string(),
                retryable: false,
            });
        }
        let node = Self::provider_node(capability)?;
        let now_ns = self.clock.now_ns();
        let sequence = self
            .request_seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nonce =
            QuoteRequest::derive_nonce(caller, capability, template.bytes(), now_ns, sequence);
        let mut request = QuoteRequest::new(
            provider.clone(),
            caller.clone(),
            capability,
            template.bytes(),
            now_ns,
            QUOTE_REQUEST_TTL_NS,
            nonce,
        );
        request.sign_with(&self.caller).map_err(|e| ChannelError {
            message: format!("signing the quote request: {e}"),
            retryable: false,
        })?;
        let request_bytes =
            crate::core::canonical::canonical_bytes(&request).map_err(|e| ChannelError {
                message: e.to_string(),
                retryable: false,
            })?;

        let response: QuoteWireResponse = self
            .mesh
            .call_typed(
                node,
                QUOTE_SERVICE,
                &QuoteWireRequest {
                    request_b64: BASE64.encode(request_bytes),
                    template_b64: BASE64.encode(template.bytes()),
                },
                Default::default(),
            )
            .await
            .map_err(Self::map_rpc_error)?;
        BASE64
            .decode(&response.quote_b64)
            .map_err(|e| ChannelError {
                message: format!("quote is not base64: {e}"),
                retryable: false,
            })
    }

    async fn pay(
        &self,
        quote_bytes: &[u8],
        payload: &X402Carry<PaymentPayload>,
    ) -> Result<PayResponse, ChannelError> {
        // The quote carries its provider identity, but routing needs the
        // node id — recover it from the quote's capability binding.
        let quote =
            crate::core::quote::PaymentQuote::from_json_bytes(quote_bytes).map_err(|e| {
                ChannelError {
                    message: e.to_string(),
                    retryable: false,
                }
            })?;
        let node = Self::provider_node(&quote.capability)?;
        self.mesh
            .call_typed(
                node,
                PAY_SERVICE,
                &PayWireRequest {
                    quote_b64: BASE64.encode(quote_bytes),
                    payload_b64: BASE64.encode(payload.bytes()),
                },
                Default::default(),
            )
            .await
            .map_err(Self::map_rpc_error)
    }
}

/// The provider-side gate for **natively-served** paid tools
/// ([`net_sdk::tool_payment::ToolPaymentGate`], consumed by
/// `Mesh::serve_tool_paid`): each paid invoke's quote is redeemed
/// against the [`crate::PaymentEngine`] — settled, billed, unfrozen, bound to
/// this tool, never redeemed before, at-most-once under the store lock.
/// The SDK-native twin of the MCP wrap path's `EnginePaymentAdmission`
/// (`flow/mcp_gate.rs`), byte-identical semantics.
pub struct EngineToolPaymentGate {
    engine: Arc<crate::engine::PaymentEngine>,
}

impl EngineToolPaymentGate {
    pub fn new(engine: Arc<crate::engine::PaymentEngine>) -> Self {
        Self { engine }
    }
}

#[async_trait::async_trait]
impl net_sdk::tool_payment::ToolPaymentGate for EngineToolPaymentGate {
    async fn redeem(
        &self,
        tool_id: &str,
        quote_id: &str,
        binding: Option<&[u8]>,
    ) -> Result<(), net_sdk::tool_payment::GateDenial> {
        // Single-sourced with the MCP gate (`mcp_gate::EnginePaymentAdmission`)
        // so the fail-closed mapping cannot drift — see `flow::redeem_via_engine`.
        crate::flow::redeem_via_engine(&self.engine, tool_id, quote_id, binding).await
    }
}
