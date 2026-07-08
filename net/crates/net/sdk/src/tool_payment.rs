//! The native tool payment seam: the wire vocabulary and the provider
//! gate for **paid tools served straight from the SDK**
//! (`Mesh::serve_tool_paid`) — no MCP adapter required.
//!
//! The invariant this module completes: **an announced price is an
//! enforced price on every serving path.** `Mesh::serve_tool` refuses
//! a priced descriptor outright (`ServeError::UnenforceablePricing`);
//! `serve_tool_paid` is the sanctioned alternative — it wraps the
//! handler so the quote is redeemed through a [`ToolPaymentGate`]
//! *before* the handler runs, failing closed with [`ERR_PAYMENT`].
//!
//! The wire shapes are **identical** to the MCP wrap path (which
//! re-exports these constants), so a demand-side gateway pays a native
//! tool and a wrapped tool the same way: the quote id rides
//! [`HDR_PAYMENT_QUOTE`], the optional possession proof rides
//! [`HDR_PAYMENT_BINDING`], and a refusal is the application error
//! [`ERR_PAYMENT`] — mapped to `denied` by callers, never a tool-level
//! error.
//!
//! The SDK never verifies payments itself — it holds no payment state
//! and parses no payment objects. The gate is the seam: `net-payments`
//! implements it over its `PaymentEngine` (settled, billed, unfrozen,
//! bound to this tool, never redeemed before — at-most-once under the
//! engine's store lock); tests script it.
//!
//! (`Mesh::serve_tool_paid` / `Mesh::serve_tool` /
//! `ServeError::UnenforceablePricing` are named as plain spans, not
//! intra-doc links: they live behind the `net`/`cortex` features, and
//! this module is intentionally ungated so gate implementors don't pull
//! the full `tool` surface — a link would dangle under
//! `--no-default-features`.)

use async_trait::async_trait;

/// Request header carrying the paid invocation's quote id — the binding
/// between the payment (settled out-of-band via the payment services)
/// and this invocation. Wire-identical to the MCP wrap path.
pub const HDR_PAYMENT_QUOTE: &str = "net-payment-quote";

/// Request header carrying the caller's ed25519 signature over the
/// invocation-binding transcript (quote id + tool id) — proof that the
/// invoker *is* the identity the quote was paid by, not merely someone
/// who saw the quote id. Replay of the header is harmless: redemption
/// is at-most-once regardless. Optional (bearer fallback); providers
/// may require it by policy.
pub const HDR_PAYMENT_BINDING: &str = "net-payment-quote-sig";

/// The application-error code for a payment refusal: no quote header on
/// a paid tool, a quote that is unpaid / frozen / already redeemed /
/// bound to another tool, or a gate failure (fail-closed). An
/// authorization verdict — demand-side gateways map it to `denied`,
/// never a tool-level error. Wire-identical to the MCP wrap path's
/// `ERR_PAYMENT`.
pub const ERR_PAYMENT: u16 = 0x8006;

/// The provider-side payment gate for natively-served paid tools:
/// redeem a paid quote for its one invocation. `Err(reason)` refuses
/// the invocation (the reason travels to the caller inside the
/// [`ERR_PAYMENT`] application error); `binding`, when present, is the
/// caller's signature over the invocation-binding transcript — a
/// present-but-invalid binding must reject, never fall back to bearer.
///
/// This is the SDK-native twin of the MCP adapter's `PaymentAdmission`
/// (same shape, same semantics); `net-payments` provides the
/// engine-backed implementation.
#[async_trait]
pub trait ToolPaymentGate: Send + Sync {
    async fn redeem(
        &self,
        tool_id: &str,
        quote_id: &str,
        binding: Option<&[u8]>,
    ) -> Result<(), String>;
}
