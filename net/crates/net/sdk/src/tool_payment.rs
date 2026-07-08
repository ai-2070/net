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
//! A refusal carries two renderings of the same verdict: the human
//! message (the error body — byte-identical to what the wire has always
//! carried) and, for schematic-aware callers, a [`FailureSchematic`]
//! riding [`HDR_FAILURE_SCHEMATIC`]: *which invariant refused, who can
//! fix it, what recovery is safe*. Producers emit exactly one schematic
//! header, raw JSON bytes, single-encoded; consumers treat duplicates
//! or malformed bytes as absent and fall back to the human error.
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
use serde::{Deserialize, Serialize};

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

/// Reply header carrying the failure schematic on a payment refusal:
/// the JSON bytes of a [`FailureSchematic`], beside (never instead of)
/// the human message in the error body. Exactly one per reply;
/// consumers treat duplicates or malformed bytes as absent.
pub const HDR_FAILURE_SCHEMATIC: &str = "net-failure-schematic";

/// The application-error code for a payment refusal: no quote header on
/// a paid tool, a quote that is unpaid / frozen / already redeemed /
/// bound to another tool, or a gate failure (fail-closed). An
/// authorization verdict — demand-side gateways map it to `denied`,
/// never a tool-level error. Wire-identical to the MCP wrap path's
/// `ERR_PAYMENT`.
pub const ERR_PAYMENT: u16 = 0x8006;

/// The provider-side payment gate for natively-served paid tools:
/// redeem a paid quote for its one invocation. `Err(denial)` refuses
/// the invocation — the denial's `message` travels to the caller as
/// the body of the [`ERR_PAYMENT`] application error (byte-identical
/// to the pre-schematic wire) and its `schematic` rides
/// [`HDR_FAILURE_SCHEMATIC`]. `binding`, when present, is the
/// caller's signature over the invocation-binding transcript — a
/// present-but-invalid binding must reject, never fall back to bearer.
///
/// This is the SDK-native twin of the MCP adapter's `PaymentAdmission`
/// (same shape, same semantics); `net-payments` provides the
/// engine-backed implementation and the single denial-render site.
#[async_trait]
pub trait ToolPaymentGate: Send + Sync {
    async fn redeem(
        &self,
        tool_id: &str,
        quote_id: &str,
        binding: Option<&[u8]>,
    ) -> Result<(), GateDenial>;
}

/// Object tag of the failure schematic. SDK wire vocabulary (like
/// [`ERR_PAYMENT`]), not a payments envelope — `net-payments`'
/// `core/versioning.rs` registry cross-references it here. Unsigned;
/// additive within `@1` (a breaking change mints `@2`).
pub const TAG_PAYMENT_FAILURE: &str = "net.payment.failure@1";

/// Byte cap on the schematic's `message` copy — the error body carries
/// the full human message; the schematic's copy is bounded so the object
/// always fits its header budget.
pub const MAX_SCHEMATIC_MESSAGE_BYTES: usize = 512;

/// Byte budget for the encoded schematic: the substrate's header-value
/// cap (4096, mirrored here — the ungated module can't import the
/// `cortex` wire constant). [`FailureSchematic::to_header_bytes`]
/// refuses to encode past it; the producer then sends the human message
/// alone.
pub const MAX_SCHEMATIC_BYTES: usize = 4096;

/// The `net.payment.failure@1` value vocabulary: what producers may
/// emit for `stage`, `recovery.class`, `recovery.actor`, `funds_moved`,
/// and `prior_payment`. String-typed on the wire — consumers must
/// tolerate values outside this list (the vocabulary is additive within
/// `@1`); producers must not invent values outside it.
pub mod failure_vocab {
    /// `code` for the 0x8006 family. v1 ships only this; the shape
    /// generalizes to `policy`/`approval`/`delegation` later without a
    /// new object.
    pub const CODE_PAYMENT: &str = "payment";

    /// Refused at the serving handler before the engine was consulted
    /// (missing quote header, no gate configured).
    pub const STAGE_ADMISSION: &str = "admission";
    /// Refused by the engine's invocation gate (`redeem_for_invocation`).
    /// Reserved stages, no v1 producer: `quote`, `claim`, `verify`,
    /// `settle`, `completion` (pay path); `authoring`, `caller_policy`
    /// (demand side).
    pub const STAGE_REDEEM: &str = "redeem";

    pub const CLASS_AUTOMATIC_RETRY: &str = "automatic_retry";
    /// "The quote exists — pay it, then retry"; distinct from requoting.
    pub const CLASS_PAYMENT_REQUIRED: &str = "payment_required";
    pub const CLASS_NEW_QUOTE_REQUIRED: &str = "new_quote_required";
    pub const CLASS_USER_ACTION_REQUIRED: &str = "user_action_required";
    pub const CLASS_OPERATOR_APPROVAL_REQUIRED: &str = "operator_approval_required";
    pub const CLASS_PROVIDER_CONFIGURATION_ERROR: &str = "provider_configuration_error";
    pub const CLASS_CALLER_CONFIGURATION_ERROR: &str = "caller_configuration_error";
    pub const CLASS_NETWORK_TRANSIENT: &str = "network_transient";
    /// Do not retry, do not requote — report the mismatch.
    pub const CLASS_SECURITY_VIOLATION: &str = "security_violation";
    pub const CLASS_NON_RECOVERABLE: &str = "non_recoverable";

    pub const ACTOR_CALLER_AGENT: &str = "caller_agent";
    pub const ACTOR_CALLER_USER: &str = "caller_user";
    pub const ACTOR_CALLER_OPERATOR: &str = "caller_operator";
    pub const ACTOR_PROVIDER_OPERATOR: &str = "provider_operator";

    pub const FUNDS_NO: &str = "no";
    pub const FUNDS_YES: &str = "yes";
    pub const FUNDS_UNKNOWN: &str = "unknown";

    pub const PRIOR_NONE: &str = "none";
    pub const PRIOR_PENDING: &str = "pending";
    pub const PRIOR_CONSUMED: &str = "consumed";
    pub const PRIOR_UNKNOWN: &str = "unknown";
}

/// The recovery instruction inside a [`FailureSchematic`]: who can fix
/// it and what the caller may safely do. Retry semantics, pinned:
/// the schematic-level `retryable` is the *coarse verdict* (may
/// retrying the operation succeed without changing configuration or
/// user/operator state?); `safe_to_retry` is the *recovery
/// instruction* (is retrying the same attempt part of the recommended
/// recovery?); `safe_to_requote` means a fresh quote + new payment is
/// sanctioned — it never implies the current proof can be reused, and
/// `false` on security rows means *do not just buy another quote and
/// try again*.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recovery {
    /// One of the `CLASS_*` values in [`failure_vocab`].
    pub class: String,
    /// One of the `ACTOR_*` values in [`failure_vocab`] — who can fix it.
    pub actor: String,
    pub safe_to_retry: bool,
    pub safe_to_requote: bool,
    /// Advisory snake_case hint (`request_new_quote`,
    /// `complete_payment`, `fix_payment_client`, `retry_later`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_action: Option<String>,
}

/// The v1 failure schematic — the structured verdict riding
/// [`HDR_FAILURE_SCHEMATIC`] beside the human error body (which stays
/// byte-identical to what the wire has always carried). Rendered once,
/// from the typed engine decision, never parsed back out of strings.
///
/// The v1 reason ↔ recovery mapping (redeem + admission stages) is the
/// caller-facing contract — agents branch on it:
///
/// | reason | stage | class | actor | retryable | safe_to_retry | safe_to_requote | funds_moved | prior_payment |
/// |---|---|---|---|---|---|---|---|---|
/// | `missing_quote` | admission | new_quote_required | caller_agent | false | false | true | no | none |
/// | `gate_missing` | admission | provider_configuration_error | provider_operator | false | false | false | no | none |
/// | `unknown_quote` | redeem | new_quote_required | caller_agent | false | false | true | no | none |
/// | `binding_malformed` | redeem | caller_configuration_error | caller_operator | false | false | false | unknown | unknown |
/// | `binding_rejected` | redeem | security_violation | caller_operator | false | false | false | unknown | unknown |
/// | `payer_record_corrupt` | redeem | provider_configuration_error | provider_operator | false | false | false | unknown | unknown |
/// | `quote_frozen` | redeem | non_recoverable | caller_operator | false | false | false | unknown | unknown |
/// | `not_settled` | redeem | payment_required | caller_agent | true | true | true | no | none |
/// | `settlement_pending` | redeem | automatic_retry | caller_agent | true | true | true | unknown | pending |
/// | `wrong_tool_binding` | redeem | security_violation | caller_operator | false | false | false | unknown | unknown |
/// | `already_redeemed` | redeem | new_quote_required | caller_agent | false | false | true | yes | consumed |
/// | `engine_unavailable` | redeem | provider_configuration_error | provider_operator | true | true | true | unknown | unknown |
///
/// Binding-failure rows are deliberately `unknown`/`unknown`: a failed
/// possession proof learns nothing about payment state.
///
/// Reserved reasons (documented now, no v1 producer — future surfaces
/// must use these names): `insufficient_funds`, `no_wallet_configured`,
/// `network_not_allowed`, `quote_expired`, `tier_below_required`,
/// `checker_unavailable`, `facilitator_rejected`. Reserved freeze
/// subreasons: `quote_frozen_replay | _wrong_chain | _reorg | _amount`.
///
/// Redaction is contract: no bearer material, no key references beyond
/// names, no payment blobs, no filesystem paths, no serde/transport
/// detail, no facilitator response bodies. Built only from typed
/// decision fields — never by inspecting an engine error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureSchematic {
    /// Always [`TAG_PAYMENT_FAILURE`].
    pub object: String,
    /// Stable top-level family; v1 ships `"payment"`.
    pub code: String,
    /// Where in the lifecycle the refusal fired (`STAGE_*`).
    pub stage: String,
    /// The specific verdict, snake_case. Additive within `@1`;
    /// consumers must tolerate reasons they don't know.
    pub reason: String,
    /// The human message — the same string as the error body in the
    /// common case, capped at [`MAX_SCHEMATIC_MESSAGE_BYTES`] (the body
    /// carries it in full). Where the body embeds free-form provider- or
    /// facilitator-supplied text (e.g. a freeze reason), this copy is the
    /// redaction-safe rendering instead — built only from typed fields,
    /// per the object's redaction contract.
    pub message: String,
    /// Coarse verdict: may retrying the operation succeed without
    /// changing configuration or user/operator state?
    pub retryable: bool,
    pub recovery: Recovery,
    /// Always `false` for anything these stages refuse — the invariant,
    /// stated as data.
    pub handler_executed: bool,
    /// The **money** fact (`FUNDS_*`): whether the payment associated
    /// with this quote/proof is known to have moved funds. Never a
    /// fresh charge caused by this rejected invocation — a refusal
    /// never charges.
    pub funds_moved: String,
    /// The **instrument** fact (`PRIOR_*`): lifecycle state of the
    /// payment attached to the quote — `none` (never paid), `pending`
    /// (settled, not yet billed), `consumed` (billed and redeemed), or
    /// `unknown`.
    pub prior_payment: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_id: Option<String>,
    /// Unknown fields from newer producers, preserved (never rejected)
    /// and re-emitted on serialize.
    #[serde(flatten)]
    pub extra: std::collections::BTreeMap<String, serde_json::Value>,
}

impl FailureSchematic {
    /// Serialize for the [`HDR_FAILURE_SCHEMATIC`] reply header: raw
    /// JSON bytes, single-encoded. `None` if the encoding exceeds
    /// [`MAX_SCHEMATIC_BYTES`] — the producer then sends the human
    /// message alone (the schematic is a sidecar, never worth failing a
    /// reply over).
    pub fn to_header_bytes(&self) -> Option<Vec<u8>> {
        let bytes = serde_json::to_vec(self).ok()?;
        (bytes.len() <= MAX_SCHEMATIC_BYTES).then_some(bytes)
    }

    /// Parse a schematic from a reply header, tolerantly: `None` for
    /// malformed JSON, invalid UTF-8, or a foreign object tag —
    /// consumers fall back to the human error body. Unknown reasons and
    /// extra fields parse fine (the tolerance contract).
    pub fn from_header_bytes(bytes: &[u8]) -> Option<Self> {
        let parsed: Self = serde_json::from_slice(bytes).ok()?;
        (parsed.object == TAG_PAYMENT_FAILURE).then_some(parsed)
    }

    /// Truncate a human message to [`MAX_SCHEMATIC_MESSAGE_BYTES`] on a
    /// char boundary, for the schematic's `message` copy.
    pub fn cap_message(message: &str) -> String {
        if message.len() <= MAX_SCHEMATIC_MESSAGE_BYTES {
            return message.to_string();
        }
        let mut end = MAX_SCHEMATIC_MESSAGE_BYTES;
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        message[..end].to_string()
    }

    /// This schematic as its reply-header entry
    /// (([`HDR_FAILURE_SCHEMATIC`], JSON bytes)) — `None` when it
    /// exceeds the wire budget; the reply then carries the human
    /// message alone. Producers attach exactly one.
    pub fn header_entry(&self) -> Option<(String, Vec<u8>)> {
        self.to_header_bytes()
            .map(|bytes| (HDR_FAILURE_SCHEMATIC.to_string(), bytes))
    }

    /// The `missing_quote` admission verdict: a paid tool invoked
    /// without a quote header. Authored by the serving handler (SDK
    /// native or MCP wrap) — the engine was never consulted, nothing
    /// was consumed.
    pub fn missing_quote(tool_id: &str) -> Self {
        Self {
            object: TAG_PAYMENT_FAILURE.to_string(),
            code: failure_vocab::CODE_PAYMENT.to_string(),
            stage: failure_vocab::STAGE_ADMISSION.to_string(),
            reason: "missing_quote".to_string(),
            message: "paid tool invoked without a payment quote header".to_string(),
            retryable: false,
            recovery: Recovery {
                class: failure_vocab::CLASS_NEW_QUOTE_REQUIRED.to_string(),
                actor: failure_vocab::ACTOR_CALLER_AGENT.to_string(),
                safe_to_retry: false,
                safe_to_requote: true,
                next_action: Some("request_new_quote".to_string()),
            },
            handler_executed: false,
            funds_moved: failure_vocab::FUNDS_NO.to_string(),
            prior_payment: failure_vocab::PRIOR_NONE.to_string(),
            quote_id: None,
            tool_id: Some(tool_id.to_string()),
            extra: Default::default(),
        }
    }

    /// The `gate_missing` admission verdict: the tool is priced but the
    /// provider wired no payment gate — a provider configuration error,
    /// refused fail-closed before any caller state is touched.
    pub fn gate_missing(tool_id: &str) -> Self {
        Self {
            object: TAG_PAYMENT_FAILURE.to_string(),
            code: failure_vocab::CODE_PAYMENT.to_string(),
            stage: failure_vocab::STAGE_ADMISSION.to_string(),
            reason: "gate_missing".to_string(),
            message: "tool is priced but the provider has no payment gate configured \
                      (fail-closed)"
                .to_string(),
            retryable: false,
            recovery: Recovery {
                class: failure_vocab::CLASS_PROVIDER_CONFIGURATION_ERROR.to_string(),
                actor: failure_vocab::ACTOR_PROVIDER_OPERATOR.to_string(),
                safe_to_retry: false,
                safe_to_requote: false,
                next_action: Some("configure_payment_gate".to_string()),
            },
            handler_executed: false,
            funds_moved: failure_vocab::FUNDS_NO.to_string(),
            prior_payment: failure_vocab::PRIOR_NONE.to_string(),
            quote_id: None,
            tool_id: Some(tool_id.to_string()),
            extra: Default::default(),
        }
    }
}

/// A gate refusal, both renderings together: the human `message`
/// (travels as the error body, byte-identical to the pre-schematic
/// wire) and the structured `schematic` (rides
/// [`HDR_FAILURE_SCHEMATIC`]). The refusal type of both gate traits;
/// `net-payments`' `flow::redeem_via_engine` is the single render
/// site for engine denials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateDenial {
    pub message: String,
    pub schematic: FailureSchematic,
}

impl std::fmt::Display for GateDenial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The plan's canonical example: `already_redeemed`.
    fn example() -> FailureSchematic {
        FailureSchematic {
            object: TAG_PAYMENT_FAILURE.to_string(),
            code: failure_vocab::CODE_PAYMENT.to_string(),
            stage: failure_vocab::STAGE_REDEEM.to_string(),
            reason: "already_redeemed".to_string(),
            message: "quote already redeemed — one payment, one serve".to_string(),
            retryable: false,
            recovery: Recovery {
                class: failure_vocab::CLASS_NEW_QUOTE_REQUIRED.to_string(),
                actor: failure_vocab::ACTOR_CALLER_AGENT.to_string(),
                safe_to_retry: false,
                safe_to_requote: true,
                next_action: Some("request_new_quote".to_string()),
            },
            handler_executed: false,
            funds_moved: failure_vocab::FUNDS_YES.to_string(),
            prior_payment: failure_vocab::PRIOR_CONSUMED.to_string(),
            quote_id: Some("q_fixture".to_string()),
            tool_id: Some("paid_echo".to_string()),
            extra: Default::default(),
        }
    }

    /// The golden wire shape: field names, order, and values are the
    /// `@1` contract — drift fails here, not in a consumer.
    #[test]
    fn the_golden_wire_shape_is_pinned() {
        let json = serde_json::to_string(&example()).expect("serialize");
        assert_eq!(
            json,
            "{\"object\":\"net.payment.failure@1\",\"code\":\"payment\",\
             \"stage\":\"redeem\",\"reason\":\"already_redeemed\",\
             \"message\":\"quote already redeemed — one payment, one serve\",\
             \"retryable\":false,\"recovery\":{\"class\":\"new_quote_required\",\
             \"actor\":\"caller_agent\",\"safe_to_retry\":false,\
             \"safe_to_requote\":true,\"next_action\":\"request_new_quote\"},\
             \"handler_executed\":false,\"funds_moved\":\"yes\",\
             \"prior_payment\":\"consumed\",\"quote_id\":\"q_fixture\",\
             \"tool_id\":\"paid_echo\"}"
        );
        let back = FailureSchematic::from_header_bytes(json.as_bytes()).expect("round-trip");
        assert_eq!(back, example());
    }

    /// The tolerance contract: a future producer's unknown reason, a
    /// new top-level field, and a new recovery field all parse — the
    /// top-level extra is preserved and re-emitted.
    #[test]
    fn unknown_reasons_and_extra_fields_are_tolerated() {
        let raw = "{\"object\":\"net.payment.failure@1\",\"code\":\"payment\",\
                   \"stage\":\"authoring\",\"reason\":\"insufficient_funds\",\
                   \"message\":\"m\",\"retryable\":true,\
                   \"recovery\":{\"class\":\"user_action_required\",\
                   \"actor\":\"caller_user\",\"safe_to_retry\":false,\
                   \"safe_to_requote\":true,\"urgency\":\"low\"},\
                   \"handler_executed\":false,\"funds_moved\":\"no\",\
                   \"prior_payment\":\"none\",\"required_amount\":\"100000\"}";
        let parsed = FailureSchematic::from_header_bytes(raw.as_bytes()).expect("tolerant parse");
        assert_eq!(parsed.reason, "insufficient_funds");
        assert_eq!(
            parsed.extra.get("required_amount").and_then(|v| v.as_str()),
            Some("100000")
        );
        let re = serde_json::to_string(&parsed).expect("serialize");
        assert!(re.contains("required_amount"), "{re}");
    }

    /// The discipline rule's consumer half: malformed bytes, invalid
    /// UTF-8, and a foreign object tag are all "no schematic" — never
    /// an error, never a guess.
    #[test]
    fn malformed_or_foreign_headers_parse_to_none() {
        assert!(FailureSchematic::from_header_bytes(b"{ not json").is_none());
        assert!(FailureSchematic::from_header_bytes(&[0xFF, 0xFE]).is_none());
        let mut foreign = example();
        foreign.object = "net.billing.event@1".to_string();
        let bytes = serde_json::to_vec(&foreign).expect("serialize");
        assert!(FailureSchematic::from_header_bytes(&bytes).is_none());
    }

    /// The wire budget: the canonical example fits; an over-budget
    /// schematic refuses to encode rather than overflow the header.
    #[test]
    fn header_bytes_respect_the_wire_budget() {
        let bytes = example().to_header_bytes().expect("example fits");
        assert!(bytes.len() <= MAX_SCHEMATIC_BYTES);

        let mut oversized = example();
        oversized.extra.insert(
            "huge".to_string(),
            serde_json::Value::String("x".repeat(MAX_SCHEMATIC_BYTES)),
        );
        assert!(
            oversized.to_header_bytes().is_none(),
            "an over-budget schematic must not be attached"
        );
    }

    #[test]
    fn message_capping_is_char_boundary_safe() {
        // Two-byte chars force the cap onto a non-boundary index.
        let long = "é".repeat(MAX_SCHEMATIC_MESSAGE_BYTES);
        let capped = FailureSchematic::cap_message(&long);
        assert!(capped.len() <= MAX_SCHEMATIC_MESSAGE_BYTES);
        assert!(capped.chars().all(|c| c == 'é'));
        assert_eq!(FailureSchematic::cap_message("fits"), "fits");
    }
}
