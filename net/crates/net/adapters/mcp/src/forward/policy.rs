//! Forwarding policy — caller-side and destination-side, deny-by-default
//! (`MCP_CREDENTIAL_FORWARDING_PLAN.md` Phase 0).
//!
//! Forwarding happens only when the **caller** policy allows *sending* AND the
//! **destination** policy allows *accepting*. Every default here is hostile:
//! the caller kill switch is off, no secret is bound to any provider, and the
//! destination accepts no header. Deny wins on any mismatch, and a denial
//! names *which level* refused (global / per-header / per-capability /
//! per-identity) — never a header value.
//!
//! This module is schema + decision only. It moves no secret and reads no
//! secret store; the caller daemon's secret store and the `net secret set`
//! surface land in Phase 1, and sealing/injection in Phase 2. Defining the
//! deny-by-default shape now is what stops a later phase from quietly
//! forwarding `Authorization` because "the config didn't say not to."

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::de::{self, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::header::{HeaderError, HeaderName};

// ===========================================================================
// Caller-side policy (daemon `forwarding:` config)
// ===========================================================================

/// The caller daemon's forwarding configuration. Off by default: with
/// `enabled = false` (the [`Default`]) nothing is ever forwarded, whatever the
/// allowlists say.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ForwardingConfig {
    /// Global kill switch. **Default off.** Deny-wins: a false here refuses
    /// every forward regardless of any secret or plain-header allowlist.
    pub enabled: bool,
    /// Secret headers keyed by user-visible ref name (e.g. `github-token`).
    /// The ref name appears in audit — never encode a value or sensitive scope
    /// in it.
    pub secrets: BTreeMap<String, SecretPolicy>,
    /// Non-secret headers (trace / tenant ids) keyed by header name. Same
    /// allowlist shape as secrets — `plain_headers` is not a loophole.
    pub plain_headers: BTreeMap<String, PlainHeaderPolicy>,
}

/// Policy for one secret header ref.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SecretPolicy {
    /// The wire header this secret is injected as (e.g. `Authorization`).
    pub header: String,
    /// Where and to what this secret may be sent. Empty (the default) = never.
    #[serde(default)]
    pub allow: AllowList,
    /// Optional audit-legibility label (`purpose: github-api`). No enforcement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
}

/// Policy for one non-secret (plain) header.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PlainHeaderPolicy {
    /// Where and to what this header may be sent. Empty (the default) = never.
    pub allow: AllowList,
}

/// A destination-and-capability allowlist. Both dimensions must match for a
/// forward to be permitted; either being empty denies.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct AllowList {
    /// Which providers may receive the header. Default [`ProviderScope::None`]
    /// (deny all).
    pub providers: ProviderScope,
    /// Capability-id globs this header may accompany (e.g. `github.*`). Empty
    /// (default) matches nothing.
    pub capabilities: Vec<String>,
}

impl AllowList {
    /// Does this allowlist permit sending to `provider` for `capability`?
    /// Returns the level that refused on denial (capability checked before
    /// identity, so a capability miss is reported even when the provider would
    /// also fail).
    fn permits(&self, provider: &str, capability: &str) -> Result<(), DenialLevel> {
        if !self
            .capabilities
            .iter()
            .any(|glob| glob_match(glob, capability))
        {
            return Err(DenialLevel::PerCapability);
        }
        if !self.providers.matches(provider) {
            return Err(DenialLevel::PerIdentity);
        }
        Ok(())
    }
}

/// Which providers an allowlist covers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ProviderScope {
    /// No providers — the safe default; matches nothing.
    #[default]
    None,
    /// Any provider. Serialized as the string `"any"`; intended only for a
    /// small vetted set of non-secret headers (trace ids).
    Any,
    /// An explicit set of provider identifiers (node id or `org:<name>`).
    /// Matched exactly — org-attestation matching is a later refinement.
    Ids(Vec<String>),
}

impl ProviderScope {
    /// Does this scope include `provider`?
    pub fn matches(&self, provider: &str) -> bool {
        match self {
            ProviderScope::None => false,
            ProviderScope::Any => true,
            ProviderScope::Ids(ids) => ids.iter().any(|id| id == provider),
        }
    }
}

// `ProviderScope` deserializes from either the string `"any"`/`"*"` or a list
// of ids, so the config can write `providers: any` or `providers: [<id>, ...]`.
impl<'de> Deserialize<'de> for ProviderScope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ScopeVisitor;

        impl<'de> Visitor<'de> for ScopeVisitor {
            type Value = ProviderScope;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("the string \"any\" or a list of provider ids")
            }

            fn visit_str<E>(self, s: &str) -> Result<ProviderScope, E>
            where
                E: de::Error,
            {
                match s {
                    "any" | "*" => Ok(ProviderScope::Any),
                    other => Err(E::custom(format!(
                        "expected \"any\" or a list of provider ids, got {other:?}"
                    ))),
                }
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<ProviderScope, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut ids = Vec::new();
                while let Some(id) = seq.next_element::<String>()? {
                    ids.push(id);
                }
                Ok(ProviderScope::Ids(ids))
            }
        }

        deserializer.deserialize_any(ScopeVisitor)
    }
}

impl Serialize for ProviderScope {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            // `None` and an empty id list both mean "no providers"; serialize
            // the empty list so a round-trip preserves the deny.
            ProviderScope::None => serializer.collect_seq(std::iter::empty::<&str>()),
            ProviderScope::Any => serializer.serialize_str("any"),
            ProviderScope::Ids(ids) => serializer.collect_seq(ids),
        }
    }
}

/// The level of the policy hierarchy that refused a forward. Carries no header
/// value — only which gate said no (plan: "structured error naming the level
/// that denied, without naming values").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenialLevel {
    /// The global kill switch was off (caller), or no accept-list existed
    /// (destination).
    Global,
    /// No secret/header ref, or no accepted header name, matched.
    PerHeader,
    /// The capability was outside the allowlist glob.
    PerCapability,
    /// The provider/identity was outside the allowlist.
    PerIdentity,
}

impl fmt::Display for DenialLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            DenialLevel::Global => "global",
            DenialLevel::PerHeader => "per-header",
            DenialLevel::PerCapability => "per-capability",
            DenialLevel::PerIdentity => "per-identity",
        };
        f.write_str(s)
    }
}

/// A permitted send: the resolved wire header, plus the ref name that granted
/// it (secret ref, or the plain header name). Never carries the value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendGrant {
    /// The user-visible ref that granted the send (audit-legible).
    pub granted_by: String,
    /// The canonical wire header to inject the value as.
    pub header: HeaderName,
}

impl ForwardingConfig {
    /// Decide whether the caller may forward secret `secret_ref` to `provider`
    /// for `capability`. Deny wins; the error names the level that refused.
    ///
    /// Order (fail-closed at the first miss): global switch → the ref exists →
    /// its allowlist (capability, then identity).
    pub fn decide_secret(
        &self,
        secret_ref: &str,
        provider: &str,
        capability: &str,
    ) -> Result<SendGrant, DenialLevel> {
        if !self.enabled {
            return Err(DenialLevel::Global);
        }
        let policy = self.secrets.get(secret_ref).ok_or(DenialLevel::PerHeader)?;
        policy.allow.permits(provider, capability)?;
        // A misconfigured non-forwardable header (hop-by-hop) or an unparseable
        // one denies at the header level rather than injecting something unsafe.
        let header = HeaderName::parse(&policy.header).map_err(|_| DenialLevel::PerHeader)?;
        if !header.is_forwardable() {
            return Err(DenialLevel::PerHeader);
        }
        Ok(SendGrant {
            granted_by: secret_ref.to_string(),
            header,
        })
    }

    /// Decide whether the caller may forward the plain (non-secret) header
    /// `header_name` to `provider` for `capability`. Same deny-wins order.
    pub fn decide_plain(
        &self,
        header_name: &str,
        provider: &str,
        capability: &str,
    ) -> Result<SendGrant, DenialLevel> {
        if !self.enabled {
            return Err(DenialLevel::Global);
        }
        let target = HeaderName::parse(header_name).map_err(|_| DenialLevel::PerHeader)?;
        // Config keys are raw strings; match on the canonical form so
        // `X-Trace-Id` and `x-trace-id` resolve to the same policy.
        let entry = self
            .plain_headers
            .iter()
            .find(|(name, _)| HeaderName::parse(name).ok().as_ref() == Some(&target));
        let (ref_name, policy) = entry.ok_or(DenialLevel::PerHeader)?;
        policy.allow.permits(provider, capability)?;
        // A plain header must be end-to-end AND non-credential: a security-
        // sensitive header (Authorization / Cookie / Set-Cookie) never rides
        // the plain path — that's what the secret path (with its stricter
        // gates) is for. Checked *after* `permits` so a capability / provider
        // mismatch reports at the same level `decide_secret` would (its
        // forwardability check is likewise post-`permits`).
        if !target.is_forwardable() || target.is_security_sensitive() {
            return Err(DenialLevel::PerHeader);
        }
        Ok(SendGrant {
            granted_by: ref_name.clone(),
            header: target,
        })
    }
}

// ===========================================================================
// Destination-side policy (wrapper / native capability accept-list)
// ===========================================================================

/// Why building an [`AcceptPolicy`] failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AcceptError {
    /// A header name could not be canonicalized.
    #[error(transparent)]
    Header(#[from] HeaderError),
    /// A hop-by-hop header was listed — these are never forwardable.
    #[error("hop-by-hop header {name:?} cannot be accepted")]
    HopByHop {
        /// The offending name.
        name: String,
    },
    /// `cookie` / `set-cookie` was listed without the explicit force opt-in.
    #[error("accepting {name:?} requires the explicit cookie override")]
    CookieRequiresForce {
        /// The offending name.
        name: String,
    },
}

/// The destination's accept-list: exactly the header names a wrapper (or native
/// capability) will inject downstream. **Deny-by-default** — an empty policy
/// (the [`Default`]) accepts nothing, and any unlisted header is stripped.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AcceptPolicy {
    accepted: BTreeSet<HeaderName>,
}

impl AcceptPolicy {
    /// Build from header-name strings. Hop-by-hop headers are rejected always;
    /// `cookie` / `set-cookie` require `force` (session cookies are ambient
    /// authority in its worst form). There is deliberately no wildcard — every
    /// accepted header is named.
    pub fn from_names<I, S>(names: I, force: bool) -> Result<Self, AcceptError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut accepted = BTreeSet::new();
        for raw in names {
            let name = HeaderName::parse(raw.as_ref())?;
            if name.is_hop_by_hop() {
                return Err(AcceptError::HopByHop {
                    name: name.as_str().to_string(),
                });
            }
            if !force && matches!(name.as_str(), "cookie" | "set-cookie") {
                return Err(AcceptError::CookieRequiresForce {
                    name: name.as_str().to_string(),
                });
            }
            accepted.insert(name);
        }
        Ok(AcceptPolicy { accepted })
    }

    /// Parse a comma-separated flag value (`--accept-forwarded-headers
    /// Authorization,X-Tenant-Id`). Empty / whitespace-only entries are
    /// ignored; an all-empty string yields the deny-all default.
    pub fn from_flag(csv: &str, force: bool) -> Result<Self, AcceptError> {
        let names = csv
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>();
        Self::from_names(names, force)
    }

    /// Is `name` accepted (and therefore injected downstream rather than
    /// stripped)?
    pub fn accepts(&self, name: &HeaderName) -> bool {
        self.accepted.contains(name)
    }

    /// Whether the accept-list is empty (accepts nothing).
    pub fn is_empty(&self) -> bool {
        self.accepted.is_empty()
    }

    /// The accepted header names, ascending.
    pub fn accepted(&self) -> impl Iterator<Item = &HeaderName> {
        self.accepted.iter()
    }

    /// Does accepting any of these headers make the capability one that takes
    /// forwarded credentials? True when a security-sensitive header
    /// (`authorization`, `cookie`, `set-cookie`) is accepted — the trigger for
    /// the `accepts_forwarded_credentials` risk tag.
    pub fn implies_credential_tag(&self) -> bool {
        self.accepted.iter().any(HeaderName::is_security_sensitive)
    }

    /// Partition `present` into (accepted, stripped) by canonical name. The
    /// destination injects the accepted names downstream and strips the rest,
    /// logging the stripped names (never values).
    pub fn partition<'a, I>(&self, present: I) -> (Vec<HeaderName>, Vec<HeaderName>)
    where
        I: IntoIterator<Item = &'a HeaderName>,
    {
        let mut kept = Vec::new();
        let mut stripped = Vec::new();
        for name in present {
            if self.accepts(name) {
                kept.push(name.clone());
            } else {
                stripped.push(name.clone());
            }
        }
        (kept, stripped)
    }
}

/// Match `text` against a glob `pattern` supporting `*` (any run of
/// characters). No `?` or character classes — capability ids only need `*`.
fn glob_match(pattern: &str, text: &str) -> bool {
    let (p, t) = (pattern.as_bytes(), text.as_bytes());
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut mark = 0usize;
    while ti < t.len() {
        if pi < p.len() && p[pi] == b'*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_json(json: serde_json::Value) -> ForwardingConfig {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn default_config_forwards_nothing() {
        let cfg = ForwardingConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(
            cfg.decide_secret("github-token", "node-1", "github.issues"),
            Err(DenialLevel::Global),
        );
    }

    #[test]
    fn kill_switch_off_denies_even_with_a_matching_allowlist() {
        let cfg = cfg_json(serde_json::json!({
            "enabled": false,
            "secrets": {
                "github-token": {
                    "header": "Authorization",
                    "allow": { "providers": ["node-1"], "capabilities": ["github.*"] }
                }
            }
        }));
        assert_eq!(
            cfg.decide_secret("github-token", "node-1", "github.issues"),
            Err(DenialLevel::Global),
        );
    }

    #[test]
    fn secret_send_is_allowed_only_on_a_full_match() {
        let cfg = cfg_json(serde_json::json!({
            "enabled": true,
            "secrets": {
                "github-token": {
                    "header": "Authorization",
                    "allow": { "providers": ["node-1"], "capabilities": ["github.*"] }
                }
            }
        }));
        // Full match → allowed, resolving the wire header.
        let grant = cfg
            .decide_secret("github-token", "node-1", "github.issues")
            .unwrap();
        assert_eq!(grant.granted_by, "github-token");
        assert_eq!(grant.header.as_str(), "authorization");
        // Wrong capability → per-capability denial.
        assert_eq!(
            cfg.decide_secret("github-token", "node-1", "slack.post"),
            Err(DenialLevel::PerCapability),
        );
        // Wrong provider → per-identity denial.
        assert_eq!(
            cfg.decide_secret("github-token", "node-evil", "github.issues"),
            Err(DenialLevel::PerIdentity),
        );
        // Unknown ref → per-header denial.
        assert_eq!(
            cfg.decide_secret("no-such-ref", "node-1", "github.issues"),
            Err(DenialLevel::PerHeader),
        );
    }

    #[test]
    fn provider_scope_any_parses_and_matches() {
        let cfg = cfg_json(serde_json::json!({
            "enabled": true,
            "plain_headers": {
                "X-Trace-Id": { "allow": { "providers": "any", "capabilities": ["*"] } }
            }
        }));
        let grant = cfg.decide_plain("x-trace-id", "any-node", "any.cap").unwrap();
        assert_eq!(grant.header.as_str(), "x-trace-id");
        assert_eq!(grant.granted_by, "X-Trace-Id");
    }

    #[test]
    fn plain_header_lookup_is_case_insensitive() {
        let cfg = cfg_json(serde_json::json!({
            "enabled": true,
            "plain_headers": {
                "X-Trace-Id": { "allow": { "providers": "any", "capabilities": ["svc.*"] } }
            }
        }));
        // Requested with different casing than the config key.
        assert!(cfg.decide_plain("X-TRACE-ID", "n", "svc.a").is_ok());
        assert_eq!(
            cfg.decide_plain("x-trace-id", "n", "other.a"),
            Err(DenialLevel::PerCapability),
        );
    }

    #[test]
    fn plain_path_refuses_security_sensitive_headers() {
        // Even with a fully-matching allowlist, a credential header can't ride
        // the plain path — that's what the (stricter) secret path is for.
        let cfg = cfg_json(serde_json::json!({
            "enabled": true,
            "plain_headers": {
                "Authorization": { "allow": { "providers": "any", "capabilities": ["*"] } }
            }
        }));
        assert_eq!(
            cfg.decide_plain("Authorization", "n", "any.cap"),
            Err(DenialLevel::PerHeader),
        );
    }

    #[test]
    fn plain_denial_order_matches_secret() {
        // A non-forwardable plain header + capability mismatch reports
        // PerCapability (permits runs first) — the same level decide_secret
        // reports for the analogous case, now that the forwardability check is
        // post-permits in both.
        let cfg = cfg_json(serde_json::json!({
            "enabled": true,
            "plain_headers": {
                "Connection": { "allow": { "providers": "any", "capabilities": ["svc.*"] } }
            }
        }));
        assert_eq!(
            cfg.decide_plain("Connection", "n", "other.x"),
            Err(DenialLevel::PerCapability),
        );
    }

    #[test]
    fn empty_allowlist_denies() {
        let cfg = cfg_json(serde_json::json!({
            "enabled": true,
            "secrets": { "t": { "header": "Authorization" } }
        }));
        // No providers, no capabilities → capability check fails first.
        assert_eq!(
            cfg.decide_secret("t", "node-1", "github.issues"),
            Err(DenialLevel::PerCapability),
        );
    }

    #[test]
    fn unknown_config_field_is_rejected() {
        // Fail-closed on a typo'd security field rather than silently ignoring it.
        let parsed: Result<ForwardingConfig, _> =
            serde_json::from_value(serde_json::json!({ "enabledd": true }));
        assert!(parsed.is_err(), "unknown field must be rejected");
    }

    #[test]
    fn provider_scope_round_trips() {
        for scope in [
            ProviderScope::Any,
            ProviderScope::Ids(vec!["a".into(), "b".into()]),
            ProviderScope::None,
        ] {
            let v = serde_json::to_value(&scope).unwrap();
            let back: ProviderScope = serde_json::from_value(v).unwrap();
            // `None` serializes as an empty list and reads back as empty `Ids`,
            // which matches nothing just the same.
            assert_eq!(back.matches("x"), scope.matches("x"));
        }
    }

    #[test]
    fn accept_policy_denies_by_default() {
        let p = AcceptPolicy::default();
        assert!(p.is_empty());
        assert!(!p.accepts(&HeaderName::parse("Authorization").unwrap()));
    }

    #[test]
    fn accept_policy_parses_flag_and_auto_tags() {
        let p = AcceptPolicy::from_flag("Authorization, X-Tenant-Id", false).unwrap();
        assert!(p.accepts(&HeaderName::parse("authorization").unwrap()));
        assert!(p.accepts(&HeaderName::parse("x-tenant-id").unwrap()));
        assert!(!p.accepts(&HeaderName::parse("x-other").unwrap()));
        assert!(p.implies_credential_tag(), "Authorization ⇒ credential tag");

        let plain = AcceptPolicy::from_flag("X-Tenant-Id", false).unwrap();
        assert!(!plain.implies_credential_tag(), "no sensitive header ⇒ no tag");
    }

    #[test]
    fn accept_policy_rejects_hop_by_hop_and_gates_cookie() {
        assert!(matches!(
            AcceptPolicy::from_flag("Connection", false).unwrap_err(),
            AcceptError::HopByHop { .. },
        ));
        assert!(matches!(
            AcceptPolicy::from_flag("Cookie", false).unwrap_err(),
            AcceptError::CookieRequiresForce { .. },
        ));
        // With force, a cookie is accepted (still auto-tags).
        let forced = AcceptPolicy::from_flag("Cookie", true).unwrap();
        assert!(forced.accepts(&HeaderName::parse("cookie").unwrap()));
        assert!(forced.implies_credential_tag());
    }

    #[test]
    fn accept_policy_partitions_present_headers() {
        let p = AcceptPolicy::from_flag("Authorization", false).unwrap();
        let present = [
            HeaderName::parse("Authorization").unwrap(),
            HeaderName::parse("X-Sneaky").unwrap(),
        ];
        let (kept, stripped) = p.partition(present.iter());
        assert_eq!(kept, vec![HeaderName::parse("authorization").unwrap()]);
        assert_eq!(stripped, vec![HeaderName::parse("x-sneaky").unwrap()]);
    }

    #[test]
    fn glob_matches_capability_patterns() {
        assert!(glob_match("github.*", "github.issues"));
        assert!(glob_match("github.*", "github."));
        assert!(!glob_match("github.*", "gitlab.issues"));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("a*c", "abbbc"));
        assert!(!glob_match("a*c", "abbb"));
        assert!(glob_match("exact", "exact"));
        assert!(!glob_match("exact", "exacts"));
    }
}
