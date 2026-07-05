//! The consent-gate composition — the one place `describe → validate →
//! consent → invoke` is sequenced.
//!
//! This is the single implementation of "invoke a capability, but only after
//! the schema-validation and consent gates pass" (bridge doctrine H2: one
//! consent engine, one implementation). Two callers share it, so the gate can
//! never fork between the two demand-side surfaces:
//!
//! - the stdio [`Shim`](super::shim::Shim) (`net mcp serve`), which flattens
//!   the [`GatedOutcome`] to a [`CallToolResult`] carrying the product failure
//!   strings; and
//! - the native SDK gateway exposed to the Python / TS / Go bindings, which
//!   maps the outcome to a **structured** result the caller inspects (a model
//!   self-repairs a [`GatedOutcome::ValidationFailed`]; a plugin relays a
//!   [`GatedOutcome::RequiresApproval`] as a pin instruction).
//!
//! The composition is deliberately protocol-neutral: it takes a
//! [`CapabilityGateway`] (the mesh seam), a [`ConsentPolicy`] (config allowlist
//! + in-memory pins), and a freshly-loaded [`PinStore`] snapshot. It knows
//! nothing about JSON-RPC, stdio, or PyO3.

use serde_json::{json, Value};

use super::backend::{CapabilityGateway, CapabilityId, GatewayError, InvokeSafety};
use super::consent::ConsentPolicy;
use super::pins::PinStore;
use super::validation;
use crate::spec::CallToolResult;

/// The structured outcome of a consent-gated invoke.
///
/// Every variant is a *terminal* answer — a caller maps it to its own surface
/// without re-deciding anything. The gate never trusts a wire-declared
/// credential status; a capability is invocable only when the consent policy or
/// an approved pin admits it.
#[derive(Debug)]
pub enum GatedOutcome {
    /// Consent + validation passed and the provider answered. The inner result
    /// may itself carry a **tool-level** error (`is_error = true`) — that is the
    /// provider's answer, not a gate failure, and rides back unchanged.
    Invoked(CallToolResult),
    /// Pre-flight validation failed against the descriptor's input schema. The
    /// string is the field-level reason, phrased for model self-repair; the call
    /// never reached the provider.
    ValidationFailed(String),
    /// The consent gate fired: the capability's credential status requires local
    /// approval that no allowlist entry or approved pin has granted. The caller
    /// surfaces the "request a pin" instruction; nothing was invoked.
    RequiresApproval,
    /// `describe` or `invoke` failed at the gateway itself — not found, a remote
    /// wrapper owner-scope rejection ([`GatewayError::Denied`]), transport, or no
    /// daemon.
    Failed(GatewayError),
}

/// Run the consent-gated invoke of `id` with `tool_args`:
///
/// 1. **describe** — the single source of truth for the input schema (for
///    validation) and the credential status (for consent). A describe failure
///    is [`GatedOutcome::Failed`] (never a silent bypass).
/// 2. **validate** — pre-flight the arguments against the schema so a bad arg is
///    never round-tripped to the provider.
/// 3. **consent gate** — a credentialed / external / unknown / `none` capability
///    needs an allowlist entry or an approved pin. Display never implied
///    invocation.
/// 4. **invoke** — route to the provider with the retry safety derived from the
///    wire status (a resilience hint, *not* the security gate above).
///
/// `pins` is a **freshly-loaded** snapshot: the caller reloads the store per
/// call so an out-of-band `net mcp pin approve` takes effect immediately. A
/// `None` store keeps consent in-memory (the `consent` policy only). A store
/// that failed to read is passed as `None` by the caller — a broken store must
/// never *grant* consent (fail closed).
pub async fn gated_invoke<G: CapabilityGateway + ?Sized>(
    gateway: &G,
    consent: &ConsentPolicy,
    pins: Option<&PinStore>,
    id: &CapabilityId,
    tool_args: Value,
) -> GatedOutcome {
    // A no-argument invocation can arrive as JSON `null`: the host omitted
    // `arguments` on a promoted pinned tool (which deserializes to
    // `Value::Null`), or passed `"arguments": null` explicitly. MCP tool
    // arguments are an object, so normalize `null` to `{}` here — the one place
    // every demand-side caller routes through — rather than at each call site.
    let tool_args = if tool_args.is_null() {
        json!({})
    } else {
        tool_args
    };

    // [0] Describe first — schema (for validation) + credential status (for
    //     consent). One source of truth for both.
    let detail = match gateway.describe(id).await {
        Ok(d) => d,
        Err(e) => return GatedOutcome::Failed(e),
    };

    // [1] Pre-flight validation — never round-trip a bad arg to the provider.
    if let Err(v) = validation::validate_args(&tool_args, &detail.input_schema) {
        return GatedOutcome::ValidationFailed(v.to_string());
    }

    // [2] Consent gate — an allowlist entry or an approved pin admits the
    //     capability; otherwise the consent rule stands. The store snapshot is
    //     the fresh one the caller loaded.
    let gated = consent.requires_approval(id, &detail.credential_status)
        && !pins.map(|p| p.is_approved(id)).unwrap_or(false);
    if gated {
        return GatedOutcome::RequiresApproval;
    }

    // [3] Route to the provider. The retry policy is derived from the provider's
    //     declared status: only an uncredentialed tool is duplicate-safe (may
    //     retry a timeout); a credentialed one is at-most-once so a lost reply
    //     never duplicates a real side effect. This is a resilience hint, NOT the
    //     security gate above (which never trusts a wire status).
    let safety = InvokeSafety::from_credential_status(&detail.credential_status);
    match gateway.invoke(id, tool_args, safety).await {
        Ok(result) => GatedOutcome::Invoked(result),
        Err(e) => GatedOutcome::Failed(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::serve::backend::{CapabilityDetail, CapabilitySummary};
    use crate::wrap::invoke::OWNER_SCOPE_REJECTION;
    use async_trait::async_trait;

    /// A minimal gateway whose describe returns a fixed detail and whose invoke
    /// echoes the arguments (or denies, or 404s), so the gate composition can be
    /// tested in isolation from the mesh.
    struct StubGateway {
        detail: CapabilityDetail,
        deny: bool,
    }

    #[async_trait]
    impl CapabilityGateway for StubGateway {
        async fn search(&self, _query: &str) -> Result<Vec<CapabilitySummary>, GatewayError> {
            Ok(Vec::new())
        }

        async fn describe(&self, id: &CapabilityId) -> Result<CapabilityDetail, GatewayError> {
            if id == &self.detail.id {
                Ok(self.detail.clone())
            } else {
                Err(GatewayError::NotFound(id.display()))
            }
        }

        async fn invoke(
            &self,
            id: &CapabilityId,
            arguments: Value,
            _safety: InvokeSafety,
        ) -> Result<CallToolResult, GatewayError> {
            if self.deny {
                return Err(GatewayError::Denied(OWNER_SCOPE_REJECTION.to_string()));
            }
            Ok(CallToolResult::text_ok(format!(
                "invoked {} with {}",
                id.display(),
                arguments
            )))
        }
    }

    /// A detail with a schema that requires a string field `message`.
    fn detail(cred: &str) -> CapabilityDetail {
        CapabilityDetail {
            id: CapabilityId::parse("42/echo").unwrap(),
            name: "echo".to_string(),
            description: None,
            input_schema: json!({
                "type": "object",
                "properties": { "message": { "type": "string" } },
                "required": ["message"],
            }),
            output_schema: None,
            compat_tier: "mcp_bridge".to_string(),
            credential_status: cred.to_string(),
            substitutability: "provider_local".to_string(),
            version: String::new(),
        }
    }

    fn echo_id() -> CapabilityId {
        CapabilityId::parse("42/echo").unwrap()
    }

    #[tokio::test]
    async fn unknown_capability_is_failed_not_found() {
        let gw = StubGateway {
            detail: detail("none"),
            deny: false,
        };
        let consent = ConsentPolicy::new();
        let out = gated_invoke(
            &gw,
            &consent,
            None,
            &CapabilityId::parse("42/nope").unwrap(),
            json!({}),
        )
        .await;
        assert!(matches!(out, GatedOutcome::Failed(GatewayError::NotFound(_))), "{out:?}");
    }

    #[tokio::test]
    async fn bad_arguments_fail_validation_before_the_provider() {
        // Allow the capability so the ONLY thing that can stop it is validation.
        let gw = StubGateway {
            detail: detail("none"),
            deny: false,
        };
        let mut consent = ConsentPolicy::new();
        consent.allow(echo_id());
        // `message` is required but absent.
        let out = gated_invoke(&gw, &consent, None, &echo_id(), json!({})).await;
        assert!(matches!(out, GatedOutcome::ValidationFailed(_)), "{out:?}");
    }

    #[tokio::test]
    async fn a_wire_none_still_requires_approval_when_unadmitted() {
        // The trust boundary: even a self-declared `none` credential status is
        // gated until an allowlist entry or approved pin admits it.
        let gw = StubGateway {
            detail: detail("none"),
            deny: false,
        };
        let consent = ConsentPolicy::new();
        let out = gated_invoke(&gw, &consent, None, &echo_id(), json!({ "message": "hi" })).await;
        assert!(matches!(out, GatedOutcome::RequiresApproval), "{out:?}");
    }

    #[tokio::test]
    async fn an_allowlisted_capability_invokes() {
        let gw = StubGateway {
            detail: detail("credentialed"),
            deny: false,
        };
        let mut consent = ConsentPolicy::new();
        consent.allow(echo_id());
        let out = gated_invoke(&gw, &consent, None, &echo_id(), json!({ "message": "hi" })).await;
        match out {
            GatedOutcome::Invoked(result) => {
                assert!(!result.is_error);
                assert!(result.text().contains("hi"));
            }
            other => panic!("expected Invoked, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_pinned_capability_invokes() {
        // An in-memory pin (as the static consent policy carries) admits it —
        // the persistent-store path is the same predicate, exercised by the
        // shim's own pin-store tests.
        let gw = StubGateway {
            detail: detail("external_api"),
            deny: false,
        };
        let mut consent = ConsentPolicy::new();
        consent.pin(echo_id());
        let out = gated_invoke(&gw, &consent, None, &echo_id(), json!({ "message": "hi" })).await;
        assert!(matches!(out, GatedOutcome::Invoked(_)), "{out:?}");
    }

    #[tokio::test]
    async fn a_wrapper_denied_invoke_surfaces_as_failed_denied() {
        let gw = StubGateway {
            detail: detail("none"),
            deny: true,
        };
        let mut consent = ConsentPolicy::new();
        consent.allow(echo_id());
        let out = gated_invoke(&gw, &consent, None, &echo_id(), json!({ "message": "hi" })).await;
        assert!(matches!(out, GatedOutcome::Failed(GatewayError::Denied(_))), "{out:?}");
    }
}
