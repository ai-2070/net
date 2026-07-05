//! Local consent — capability identity, the credential-status vocabulary, and
//! the allowlist / pin consent gate.
//!
//! Graduated here from the MCP bridge adapter (`MCP_BRIDGE_SDK_PLAN.md` P0):
//! consent is not MCP-specific. Any surface that exposes mesh capabilities to
//! a model-driven caller — the `net mcp serve` shim, native agent
//! integrations, the language bindings — gates invocation on the same local
//! decision, so the one implementation lives in the SDK and the bridge
//! consumes and re-exports it (bridge doctrine #1: no logic in adapters or
//! bindings).
//!
//! Two invariants are load-bearing:
//!
//! - **Approvals stay out of band.** A model-driven caller may *request*
//!   access (a pending pin — see [`crate::pins`]); moving a request to
//!   approved happens exclusively through an operator surface (`net mcp pin
//!   approve`, or an allowlist entry), outside the model loop. Decisions are
//!   structured enums ([`ConsentDecision`]), never strings a consumer
//!   re-derives.
//! - **A wire-declared credential status is never trusted.** Discovery
//!   metadata is not cryptographically authenticated, so
//!   [`CredentialStatus::from_wire`] maps `"none"` — like anything
//!   unrecognised — to the *gated* [`CredentialStatus::Unknown`]. A discovered
//!   capability can only ever over-gate, never bypass consent.
//!
//! This is *local client consent*, not remote authorization: an approval here
//! satisfies the local gate for this user profile on this machine and nothing
//! wider; the remote provider's own scope enforcement always wins on top.

use std::collections::HashSet;

/// A capability's canonical identity: the provider node plus the capability
/// name. Structured, never a bare string. The display / wire form is
/// `provider/capability` — `/` qualifies the node
/// (`homelab/github.create_issue`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CapabilityId {
    /// The provider node qualifier (v0: node-namespaced). Never a mutable
    /// display alias — those never enter identifiers.
    pub provider: String,
    /// The capability / tool name (may itself contain `.` or `/`).
    pub capability: String,
}

/// Why a `provider/capability` string could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CapabilityIdError {
    /// No `/` separating the provider from the capability.
    #[error("capability id `{0}` must be `provider/capability` (missing `/`)")]
    MissingProvider(String),
    /// The provider or capability half was empty.
    #[error("capability id `{0}` has an empty provider or capability")]
    Empty(String),
}

impl CapabilityId {
    /// Build from parts. The provider is canonicalized (see
    /// `canonical_provider`) so a node id typed in a different spelling
    /// (hex, or with surrounding whitespace) yields the *same* identity — and
    /// therefore the same consent / pin-store key.
    pub fn new(provider: impl Into<String>, capability: impl Into<String>) -> Self {
        Self {
            provider: canonical_provider(&provider.into()),
            capability: capability.into(),
        }
    }

    /// Parse the `provider/capability` display form. Splits on the **first**
    /// `/` — the provider (a node qualifier) never contains `/`, so the
    /// remainder is the capability even when the capability name itself has a
    /// `/` (e.g. `homelab/svc/sub` → provider `homelab`, capability `svc/sub`).
    /// The provider is canonicalized, so `0x2a/echo`, ` 42/echo`, and `42/echo`
    /// all parse to one identity.
    pub fn parse(s: &str) -> Result<Self, CapabilityIdError> {
        let (provider, capability) = s
            .split_once('/')
            .ok_or_else(|| CapabilityIdError::MissingProvider(s.to_string()))?;
        let provider = canonical_provider(provider);
        if provider.is_empty() || capability.is_empty() {
            return Err(CapabilityIdError::Empty(s.to_string()));
        }
        Ok(Self::new(provider, capability))
    }

    /// The `provider/capability` display / wire form.
    pub fn display(&self) -> String {
        format!("{}/{}", self.provider, self.capability)
    }
}

impl std::fmt::Display for CapabilityId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.provider, self.capability)
    }
}

/// Canonicalize a provider node qualifier to one spelling, so a capability's
/// identity — and thus its consent / pin-store key — is independent of how the
/// node id was typed. Trims surrounding whitespace and, when the qualifier is a
/// node id (decimal or `0x`-hex), rewrites it to the decimal form that
/// discovery surfaces emit. A non-numeric qualifier (e.g. an in-memory test
/// double) is passed through trimmed. Routing layers accept the same spellings,
/// so identity and routing cannot disagree on the same node — an approved pin
/// recorded under `0x2a/echo` matches an invoke of `42/echo`.
fn canonical_provider(raw: &str) -> String {
    let trimmed = raw.trim();
    let numeric = match trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => trimmed.parse::<u64>(),
    };
    match numeric {
        Ok(n) => n.to_string(),
        Err(_) => trimmed.to_string(),
    }
}

/// How a capability's credential exposure is classified. The value rides on
/// the capability announcement as a `credential_status` metadata tag and gates
/// invocation at the consuming side's consent gate.
///
/// **Conservative by construction.** Detection failure must never become a
/// permission bypass — "unknown is spicy until proven boring." The only status
/// that is *not* gated ([`CredentialStatus::None`]) can be reached **only**
/// through an explicit forced downward override at the supply side, never
/// through detection and never from a wire value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialStatus {
    /// Secrets are configured — env additions or secret-shaped variables.
    Credentialed,
    /// A known external-API server (talks to a third-party service).
    ExternalApi,
    /// Could not classify. Treated **exactly** like `Credentialed` for
    /// consent — the spicy default.
    Unknown,
    /// Explicitly declared to carry no credentials. Reachable only via the
    /// forced downward override; never inferred.
    None,
}

impl CredentialStatus {
    /// The wire/tag form carried in the announcement metadata.
    pub fn as_str(self) -> &'static str {
        match self {
            CredentialStatus::Credentialed => "credentialed",
            CredentialStatus::ExternalApi => "external_api",
            CredentialStatus::Unknown => "unknown",
            CredentialStatus::None => "none",
        }
    }

    /// Parse the wire/tag form back to a status — the demand side reads it off
    /// a discovered capability's metadata to decide consent.
    ///
    /// **Trust boundary: a wire `"none"` is NOT trusted.** The ungated
    /// [`Self::None`] is reachable on the supply side only through the local
    /// operator's forced downgrade; it must never be granted by a value read
    /// off the wire. Discovery metadata is not cryptographically
    /// authenticated in v0, so a hostile or compromised provider could
    /// self-declare `credential_status = "none"` to slip past the allowlist /
    /// pin gate. So `"none"` — like `"unknown"`, anything unrecognised, or an
    /// absent value — maps to [`Self::Unknown`] (gated). This keeps "spicy until
    /// proven boring" across the trust boundary: a discovered capability can
    /// only ever *over*-gate here, never bypass consent. An operator who trusts
    /// a specific remote capability admits it explicitly (allowlist or pin).
    /// (Trusting `"none"` from a cryptographically-verified same-root provider
    /// is a later refinement, tied to the owner root-identity model.)
    pub fn from_wire(s: &str) -> Self {
        match s {
            "credentialed" => CredentialStatus::Credentialed,
            "external_api" => CredentialStatus::ExternalApi,
            // "none", "unknown", and anything unrecognised are all gated — a
            // wire value can never reach the ungated `None` (see above).
            _ => CredentialStatus::Unknown,
        }
    }

    /// Parse a **trusted, locally produced** status label — the exact
    /// [`Self::as_str`] forms — back to the enum. Unlike
    /// [`Self::from_wire`], which gates every unrecognised value and never
    /// yields the ungated `None`, this accepts `"none"` verbatim, because
    /// the caller vouches for the label's origin (e.g. a language binding
    /// marshaling the operator's own forced downgrade back into a helper
    /// call). Returns `None` for an unknown label rather than guessing —
    /// never use this on a value read off the wire.
    pub fn from_label(s: &str) -> Option<Self> {
        match s {
            "credentialed" => Some(CredentialStatus::Credentialed),
            "external_api" => Some(CredentialStatus::ExternalApi),
            "unknown" => Some(CredentialStatus::Unknown),
            "none" => Some(CredentialStatus::None),
            _ => None,
        }
    }

    /// Does invoking this capability require local consent (an allowlist
    /// entry or an approved pin)? Everything except an explicitly-boring
    /// `None` is gated — credentialed, external, and unknown alike.
    pub fn requires_consent(self) -> bool {
        !matches!(self, CredentialStatus::None)
    }
}

/// Whether a capability may be invoked right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsentDecision {
    /// Freely invocable — either it carries no credentials, or the operator
    /// has allowlisted / pinned it.
    Allowed,
    /// Blocked pending local approval (allowlist entry or an approved pin).
    RequiresApproval,
}

impl ConsentDecision {
    /// Is this capability blocked pending approval?
    pub fn requires_approval(self) -> bool {
        matches!(self, ConsentDecision::RequiresApproval)
    }
}

/// The consumer-side consent state: the config allowlist plus the set of
/// pinned capabilities. Both admit an otherwise-gated capability; the
/// distinction (config vs. user-approved pin) matters for auditing, not for
/// the gate here.
#[derive(Debug, Clone, Default)]
pub struct ConsentPolicy {
    /// Capabilities allowlisted in config — pre-approved by the operator.
    allowlist: HashSet<CapabilityId>,
    /// Capabilities with an approved pin. Persisted out of process via
    /// [`crate::pins::PinStore`] in the real build; held here for the
    /// consumer's lifetime.
    pinned: HashSet<CapabilityId>,
}

impl ConsentPolicy {
    /// An empty policy: nothing allowlisted or pinned. With no entries **every**
    /// discovered capability requires approval — a wire credential status
    /// (including `none`) is not trusted (see [`CredentialStatus::from_wire`]),
    /// so a capability is invocable only once it is allowlisted or pinned.
    pub fn new() -> Self {
        Self::default()
    }

    /// Allowlist `id` (from consumer config) — a standing pre-approval.
    pub fn allow(&mut self, id: CapabilityId) {
        self.allowlist.insert(id);
    }

    /// Record an approved pin for `id`.
    pub fn pin(&mut self, id: CapabilityId) {
        self.pinned.insert(id);
    }

    /// Remove a pin.
    pub fn unpin(&mut self, id: &CapabilityId) {
        self.pinned.remove(id);
    }

    /// Is `id` pinned?
    pub fn is_pinned(&self, id: &CapabilityId) -> bool {
        self.pinned.contains(id)
    }

    /// The pinned capabilities, for listing surfaces.
    pub fn pinned(&self) -> impl Iterator<Item = &CapabilityId> {
        self.pinned.iter()
    }

    /// Decide whether `id`, with the given wire credential status, may be
    /// invoked. Every wire status is gated ([`CredentialStatus::from_wire`]
    /// never trusts a wire value to the ungated `None` — see its trust-boundary
    /// note), so a discovered capability is invocable only when the operator
    /// has allowlisted or pinned it.
    pub fn decide(&self, id: &CapabilityId, credential_status: &str) -> ConsentDecision {
        // Kept for robustness: `from_wire` never yields a non-consent status
        // today, so this branch does not fire for any wire value — but it keeps
        // the gate honest if a trusted-status path is ever added.
        if !CredentialStatus::from_wire(credential_status).requires_consent() {
            return ConsentDecision::Allowed;
        }
        if self.allowlist.contains(id) || self.pinned.contains(id) {
            return ConsentDecision::Allowed;
        }
        ConsentDecision::RequiresApproval
    }

    /// Convenience: does invoking `id` (with `credential_status`) require
    /// approval the operator has not granted?
    pub fn requires_approval(&self, id: &CapabilityId, credential_status: &str) -> bool {
        self.decide(id, credential_status).requires_approval()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cap(s: &str) -> CapabilityId {
        CapabilityId::parse(s).unwrap()
    }

    #[test]
    fn parses_provider_and_capability_on_first_slash() {
        let id = CapabilityId::parse("homelab/github.create_issue").unwrap();
        assert_eq!(id.provider, "homelab");
        assert_eq!(id.capability, "github.create_issue");
        assert_eq!(id.display(), "homelab/github.create_issue");

        // Capability names may themselves contain `/` — only the first split
        // is the provider boundary.
        let nested = CapabilityId::parse("homelab/svc/sub").unwrap();
        assert_eq!(nested.provider, "homelab");
        assert_eq!(nested.capability, "svc/sub");
    }

    #[test]
    fn rejects_missing_or_empty_halves() {
        assert_eq!(
            CapabilityId::parse("bareword"),
            Err(CapabilityIdError::MissingProvider("bareword".to_string())),
        );
        assert_eq!(
            CapabilityId::parse("/cap"),
            Err(CapabilityIdError::Empty("/cap".to_string())),
        );
        assert_eq!(
            CapabilityId::parse("prov/"),
            Err(CapabilityIdError::Empty("prov/".to_string())),
        );
    }

    #[test]
    fn display_round_trips_through_parse() {
        let id = CapabilityId::new("node-b", "time.now");
        assert_eq!(CapabilityId::parse(&id.display()).unwrap(), id);
    }

    #[test]
    fn provider_node_id_is_canonicalized_across_spellings() {
        // A node id typed as hex or with whitespace must yield the SAME
        // identity as the decimal form discovery emits — otherwise a pin
        // recorded under one spelling never admits an invoke of the other.
        let decimal = CapabilityId::parse("42/echo").unwrap();
        assert_eq!(decimal.provider, "42");
        for spelling in ["0x2a/echo", "0X2A/echo", " 42/echo", "42 /echo"] {
            let id = CapabilityId::parse(spelling).unwrap();
            assert_eq!(id, decimal, "`{spelling}` must canonicalize to `42/echo`");
            assert_eq!(id.display(), "42/echo");
        }
        // A non-numeric qualifier (test double) is preserved (just trimmed).
        assert_eq!(CapabilityId::new(" nodeb ", "echo").provider, "nodeb");
        // The capability half is never touched by provider canonicalization.
        assert_eq!(
            CapabilityId::parse("0x10/svc/sub").unwrap(),
            CapabilityId::new("16", "svc/sub"),
        );
    }

    #[test]
    fn from_wire_gates_everything_except_credentialed_and_external() {
        // Only the two explicitly-spicy statuses round-trip verbatim.
        assert_eq!(
            CredentialStatus::from_wire("credentialed"),
            CredentialStatus::Credentialed,
        );
        assert_eq!(
            CredentialStatus::from_wire("external_api"),
            CredentialStatus::ExternalApi,
        );
        // "unknown", a garbled value, or an absent one all gate as Unknown.
        for spicy in ["unknown", "", "bogus", "credentialed ", "None", "UNKNOWN"] {
            let parsed = CredentialStatus::from_wire(spicy);
            assert_eq!(parsed, CredentialStatus::Unknown, "{spicy:?}");
            assert!(parsed.requires_consent());
        }
    }

    #[test]
    fn from_label_round_trips_exactly_and_rejects_unknowns() {
        // The trusted-input parse is exact: every `as_str` form round-trips
        // (INCLUDING the ungated `none` — the caller vouches for the label),
        // and anything else is `None`, never a guess.
        for status in [
            CredentialStatus::Credentialed,
            CredentialStatus::ExternalApi,
            CredentialStatus::Unknown,
            CredentialStatus::None,
        ] {
            assert_eq!(CredentialStatus::from_label(status.as_str()), Some(status));
        }
        for bad in ["", "bogus", "None", "credentialed ", "NONE"] {
            assert_eq!(CredentialStatus::from_label(bad), None, "{bad:?}");
        }
    }

    /// Trust-boundary invariant: a wire-declared `"none"` never reaches the
    /// ungated `None` — it gates like anything else, so a discovered
    /// capability cannot self-declare its way past the consent gate.
    #[test]
    fn wire_none_is_gated_not_trusted() {
        let parsed = CredentialStatus::from_wire(CredentialStatus::None.as_str());
        assert_eq!(parsed, CredentialStatus::Unknown);
        assert!(
            parsed.requires_consent(),
            "a wire `none` must be gated, not trusted",
        );
    }

    #[test]
    fn a_wire_none_is_gated_not_trusted_by_the_policy() {
        // A discovered capability's self-declared `none` is not trusted across
        // the demand-side trust boundary — it is gated like any other status.
        let policy = ConsentPolicy::new();
        assert_eq!(
            policy.decide(&cap("b/echo"), "none"),
            ConsentDecision::RequiresApproval,
        );
    }

    #[test]
    fn spicy_statuses_require_approval_by_default() {
        let policy = ConsentPolicy::new();
        for status in ["credentialed", "external_api", "unknown", "none"] {
            assert_eq!(
                policy.decide(&cap("b/tool"), status),
                ConsentDecision::RequiresApproval,
                "{status} must be gated",
            );
        }
    }

    #[test]
    fn unrecognised_status_is_gated_like_unknown() {
        // A garbled / absent status must over-gate, never bypass.
        let policy = ConsentPolicy::new();
        assert!(policy.requires_approval(&cap("b/tool"), ""));
        assert!(policy.requires_approval(&cap("b/tool"), "bogus"));
    }

    #[test]
    fn allowlist_admits_a_gated_capability() {
        let mut policy = ConsentPolicy::new();
        let id = cap("b/github.create_issue");
        assert!(policy.requires_approval(&id, "credentialed"));
        policy.allow(id.clone());
        assert_eq!(policy.decide(&id, "credentialed"), ConsentDecision::Allowed);
        // A different capability is still gated.
        assert!(policy.requires_approval(&cap("b/other"), "credentialed"));
    }

    #[test]
    fn pin_admits_and_lists() {
        let mut policy = ConsentPolicy::new();
        let id = cap("b/slack.post");
        policy.pin(id.clone());
        assert!(policy.is_pinned(&id));
        assert_eq!(policy.decide(&id, "external_api"), ConsentDecision::Allowed);
        assert_eq!(policy.pinned().collect::<Vec<_>>(), vec![&id]);
        policy.unpin(&id);
        assert!(!policy.is_pinned(&id));
        assert!(policy.requires_approval(&id, "external_api"));
    }
}
