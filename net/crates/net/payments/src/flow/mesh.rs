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
use net::adapter::net::identity::EntityId;
use net_sdk::mesh::Mesh;
use net_sdk::mesh_rpc::{Codec, RpcError, ServeError, ServeHandle};
use serde::{Deserialize, Serialize};

use super::{ChannelError, InProcessProvider, PayResponse, ProviderChannel};
use crate::x402::payload::PaymentPayload;
use crate::x402::requirements::PaymentRequirements;
use crate::x402::X402Carry;

/// Quote-issuance service name (nRPC; channel-safe, so `.v1` not `@1`).
pub const QUOTE_SERVICE: &str = "net.payments.quote.v1";
/// Payment-delivery service name.
pub const PAY_SERVICE: &str = "net.payments.pay.v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct QuoteWireRequest {
    caller_hex: String,
    capability: String,
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
    let quote =
        mesh.serve_rpc_typed(QUOTE_SERVICE, Codec::Json, move |req: QuoteWireRequest| {
            let provider = quote_provider.clone();
            async move {
                let caller = decode_entity(&req.caller_hex)?;
                let template_bytes = BASE64
                    .decode(&req.template_b64)
                    .map_err(|e| format!("template is not base64: {e}"))?;
                let template: X402Carry<PaymentRequirements> =
                    X402Carry::from_bytes(template_bytes).map_err(|e| e.to_string())?;
                let quote_bytes = provider
                    .quote(&caller, &req.capability, &template)
                    .await
                    .map_err(|e| e.message)?;
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

fn decode_entity(hex_str: &str) -> Result<EntityId, String> {
    let bytes: [u8; 32] = hex::decode(hex_str)
        .map_err(|e| format!("caller id is not hex: {e}"))?
        .try_into()
        .map_err(|_| "caller id must be 32 bytes".to_string())?;
    Ok(EntityId::from_bytes(bytes))
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
}

impl MeshPaymentChannel {
    pub fn new(mesh: Arc<Mesh>) -> Self {
        Self { mesh }
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
        capability: &str,
        template: &X402Carry<PaymentRequirements>,
    ) -> Result<Vec<u8>, ChannelError> {
        let node = Self::provider_node(capability)?;
        let response: QuoteWireResponse = self
            .mesh
            .call_typed(
                node,
                QUOTE_SERVICE,
                &QuoteWireRequest {
                    caller_hex: hex::encode(caller.as_bytes()),
                    capability: capability.to_string(),
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
/// against the [`PaymentEngine`] — settled, billed, unfrozen, bound to
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
    ) -> Result<(), String> {
        // Single-sourced with the MCP gate (`mcp_gate::EnginePaymentAdmission`)
        // so the fail-closed mapping cannot drift — see `flow::redeem_via_engine`.
        crate::flow::redeem_via_engine(&self.engine, tool_id, quote_id, binding).await
    }
}
