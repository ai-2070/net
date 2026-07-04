//! Injection targets, the never-for-stdio doctrine, and the
//! `accepts_forwarded_credentials` risk tag
//! (`MCP_CREDENTIAL_FORWARDING_PLAN.md` Phase 0).
//!
//! Forwarded headers are injected into a downstream request at the
//! destination. *Where* they can be injected is fixed here, not left to
//! adapter discretion, and one target is **deliberately absent**: a wrapped
//! stdio process. Per-call env mutation of a shared child process is a
//! cross-caller contamination bug factory, so stdio wrapping keeps pure
//! credential locality forever — forwarding doesn't apply and can't be bolted
//! on. The type system carries that rule: there is no stdio [`InjectionTarget`]
//! to construct, and [`forwarding_supported`] returns `false` for a stdio
//! transport.

use super::policy::AcceptPolicy;

/// Risk tag announced on a capability that accepts forwarded credentials
/// (plan doctrine #7 — honest labeling). Callers see it in describe / pinned
/// descriptions before anything is sent.
pub const RISK_TAG_ACCEPTS_FORWARDED_CREDENTIALS: &str = "accepts_forwarded_credentials";

/// How a wrapped/native capability reaches its downstream service — the axis
/// that decides whether forwarding is even possible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WrapTransport {
    /// A spawned stdio MCP server (single-user child process). Credential
    /// locality is permanent here; forwarding never applies.
    Stdio,
    /// A remote / HTTP-facing server or native HTTP-ish capability, where a
    /// forwarded header maps onto a request the destination makes.
    RemoteHttp,
}

impl WrapTransport {
    /// Whether forwarded invocation context can *ever* be injected for this
    /// transport. Always `false` for [`WrapTransport::Stdio`] — the never-for-
    /// stdio doctrine, enforced rather than documented.
    pub fn supports_forwarding(self) -> bool {
        matches!(self, WrapTransport::RemoteHttp)
    }
}

/// Where a forwarded header may be injected at the destination. There is no
/// stdio/env variant by construction — see the module doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InjectionTarget {
    /// An HTTP request header on the downstream call. The default and only
    /// target for remote-HTTP wrapped servers and native HTTP-ish capabilities.
    HttpHeader,
    /// A forwarded header mapped into a tool-argument template — an explicit,
    /// opt-in extension for MCP servers that take tokens as arguments. Never
    /// enabled implicitly.
    ArgumentTemplate,
}

/// Why forwarding is unavailable for a target/transport pair.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ForwardingUnsupported {
    /// The transport is stdio — forwarding never applies (doctrine).
    #[error("credential forwarding is never supported for a wrapped stdio process")]
    StdioNeverForwards,
}

/// Resolve the injection target for a `(transport, target)` pair, refusing any
/// stdio transport unconditionally. This is the single choke point future
/// phases route through, so "just forward `Authorization` into the stdio
/// child" has nowhere to be written.
pub fn resolve_injection(
    transport: WrapTransport,
    target: InjectionTarget,
) -> Result<InjectionTarget, ForwardingUnsupported> {
    if !transport.supports_forwarding() {
        return Err(ForwardingUnsupported::StdioNeverForwards);
    }
    Ok(target)
}

/// Convenience alias for the guard, readable at call sites that only care
/// whether forwarding is possible at all.
pub fn forwarding_supported(transport: WrapTransport) -> bool {
    transport.supports_forwarding()
}

/// The risk tags a destination [`AcceptPolicy`] contributes to a capability's
/// announcement. Returns the `accepts_forwarded_credentials` tag when the
/// accept-list includes a security-sensitive header, and nothing otherwise —
/// so a wrapper accepting only `X-Tenant-Id` carries no stealth credential
/// surface, and one accepting `Authorization` cannot hide it.
pub fn risk_tags_for_accept_policy(policy: &AcceptPolicy) -> Vec<&'static str> {
    if policy.implies_credential_tag() {
        vec![RISK_TAG_ACCEPTS_FORWARDED_CREDENTIALS]
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stdio_never_forwards() {
        assert!(!WrapTransport::Stdio.supports_forwarding());
        assert!(!forwarding_supported(WrapTransport::Stdio));
        for target in [
            InjectionTarget::HttpHeader,
            InjectionTarget::ArgumentTemplate,
        ] {
            assert_eq!(
                resolve_injection(WrapTransport::Stdio, target).unwrap_err(),
                ForwardingUnsupported::StdioNeverForwards,
            );
        }
    }

    #[test]
    fn remote_http_forwards_to_the_requested_target() {
        assert!(WrapTransport::RemoteHttp.supports_forwarding());
        assert_eq!(
            resolve_injection(WrapTransport::RemoteHttp, InjectionTarget::HttpHeader).unwrap(),
            InjectionTarget::HttpHeader,
        );
        assert_eq!(
            resolve_injection(WrapTransport::RemoteHttp, InjectionTarget::ArgumentTemplate)
                .unwrap(),
            InjectionTarget::ArgumentTemplate,
        );
    }

    #[test]
    fn credential_accept_list_is_tagged_and_plain_is_not() {
        let creds = AcceptPolicy::from_flag("Authorization", false).unwrap();
        assert_eq!(
            risk_tags_for_accept_policy(&creds),
            vec![RISK_TAG_ACCEPTS_FORWARDED_CREDENTIALS],
        );

        let plain = AcceptPolicy::from_flag("X-Tenant-Id", false).unwrap();
        assert!(risk_tags_for_accept_policy(&plain).is_empty());

        let none = AcceptPolicy::default();
        assert!(risk_tags_for_accept_policy(&none).is_empty());

        // A vendor bearer credential in the accept-list also earns the tag — a
        // wrapper accepting `x-api-key` can't hide its credential surface.
        let vendor = AcceptPolicy::from_flag("X-Api-Key", false).unwrap();
        assert_eq!(
            risk_tags_for_accept_policy(&vendor),
            vec![RISK_TAG_ACCEPTS_FORWARDED_CREDENTIALS],
        );
    }
}
