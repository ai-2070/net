// SPDX-License-Identifier: MIT OR Apache-2.0
//! Signed membership invitation (`net-join:` link) and canonical redemption intent.
//!
//! A [`MembershipInvite`] binds, under one issuer signature: the full issuer
//! identity, a named trust domain and its public [`TrustDomainId`], the TCP
//! enrollment endpoint and its Noise static key, a random single-use
//! [`InvitationId`], the creation-time [`InvitationPolicy`], an optional intended
//! device identity and the exact authorized [`Relation`] set. It carries **no**
//! PSK, root key or audience secret, but a subject-unbound link is still bearer
//! authorization to redeem: treat [`MembershipInvite::encode`] output as secret.
//!
//! Decoding verifies the signature against the *embedded* issuer. That proves
//! integrity only: a human must confirm the issuer fingerprint is the intended
//! one, and the issuing owner must still check its own durable ledger, current
//! policy and authority before claiming or issuing. Nothing here performs a
//! network request, consumes an invitation or issues a credential.
//!
//! [`RedemptionIntent`] is the device's canonical request (invite digest, full
//! subject, exact relations); its digest is what the ledger's
//! [`Claimant`] binds. The connection-bound proof of key possession is a
//! separate, later redemption-transport concern.
//!
//! Distinct magics and signature domains keep these formats disjoint from the
//! legacy `NMI1`/`NMJ1`/`NMO1` delegation enrollment, which is unchanged.

use std::net::Ipv6Addr;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};

use super::policy::{ApprovalMode, InvitationPolicy, PolicyError};
use super::store::{Claimant, InvitationId, OfferSpec};
use super::{fingerprint, Reader};
use crate::bootstrap_credential::TrustDomainId;
use crate::identity::{EntityId, Identity};

/// Prefix of the copy-paste link form. Distinct from legacy `net-invite:`.
pub const JOIN_LINK_PREFIX: &str = "net-join:";
/// Maximum canonical invite size in bytes (before base64).
pub const MAX_INVITE_BYTES: usize = 1024;
/// Maximum trust-domain name length.
pub const MAX_TRUST_DOMAIN_NAME: usize = 64;
/// Maximum redemption endpoint length.
pub const MAX_ENDPOINT_BYTES: usize = 256;
/// Maximum number of relations in one invitation.
pub const MAX_RELATIONS: usize = 8;

const INVITE_MAGIC: [u8; 4] = *b"NMM1";
const INTENT_MAGIC: [u8; 4] = *b"NMN1";
const INVITE_SIGNATURE_DOMAIN: &[u8] = b"net-mesh membership invite v1";
const INVITE_DIGEST_CONTEXT: &str = "net-mesh membership invite digest v1";
const SCOPE_DIGEST_CONTEXT: &str = "net-mesh membership invite scope v1";
const INTENT_DIGEST_CONTEXT: &str = "net-mesh membership redemption intent v1";

const TAG_MESH: u8 = 1;

/// Payload-free invite/intent failures. None echoes bearer material.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum InviteError {
    /// Structural decode failure.
    #[error("malformed membership invite: {0}")]
    Malformed(&'static str),
    /// Encoded form exceeds [`MAX_INVITE_BYTES`].
    #[error("membership invite is too large")]
    TooLarge,
    /// The issuer signature does not verify.
    #[error("membership invite signature is invalid")]
    BadSignature,
    /// The enrollment endpoint is not an acceptable `host:port`.
    #[error("invalid redemption endpoint: {0}")]
    Endpoint(&'static str),
    /// Trust-domain name is empty, too long or has disallowed characters.
    #[error("invalid trust-domain name")]
    TrustDomainName,
    /// Relation set is empty, too large, unordered or duplicated.
    #[error("invalid relation set: {0}")]
    Relations(&'static str),
    /// Stored policy timestamps are inconsistent.
    #[error("invalid invitation policy")]
    Policy,
    /// The subject is not the invitation's intended device.
    #[error("subject is not the intended device")]
    WrongSubject,
    /// The intent does not match this exact invitation.
    #[error("redemption intent does not match the invitation")]
    IntentMismatch,
    /// A delivered PSK belongs to a different trust domain.
    #[error("trust domain does not match the invitation")]
    TrustDomainMismatch,
    /// The OS CSPRNG failed.
    #[error("CSPRNG unavailable")]
    Random,
}

impl From<PolicyError> for InviteError {
    fn from(_: PolicyError) -> Self {
        Self::Policy
    }
}

/// One independently authorized relation an invitation may grant.
///
/// v1 defines only mesh membership; organization, channel and subnet relations
/// arrive with their own verifiers and tags. Unknown tags are refused.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Relation {
    /// Membership-only device association with the issuer's mesh. Grants no
    /// delegation, invocation, management, organization, channel or subnet right.
    Mesh,
}

impl Relation {
    fn tag(self) -> u8 {
        match self {
            Self::Mesh => TAG_MESH,
        }
    }

    fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            TAG_MESH => Some(Self::Mesh),
            _ => None,
        }
    }
}

/// Canonical relation set: non-empty, bounded, strictly ascending by tag.
fn check_relations(relations: &[Relation]) -> Result<(), InviteError> {
    if relations.is_empty() {
        return Err(InviteError::Relations("empty"));
    }
    if relations.len() > MAX_RELATIONS {
        return Err(InviteError::Relations("too many"));
    }
    if relations.windows(2).any(|w| w[0].tag() >= w[1].tag()) {
        return Err(InviteError::Relations("unordered or duplicate"));
    }
    Ok(())
}

pub(super) fn put_relations(out: &mut Vec<u8>, relations: &[Relation]) {
    // Bounded by MAX_RELATIONS.
    out.push(relations.len() as u8);
    out.extend(relations.iter().map(|r| r.tag()));
}

pub(super) fn take_relations(r: &mut Reader<'_>) -> Result<Vec<Relation>, InviteError> {
    let n = r
        .take_arr::<1>()
        .ok_or(InviteError::Malformed("truncated"))?[0] as usize;
    if n > MAX_RELATIONS {
        return Err(InviteError::Relations("too many"));
    }
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let tag = r
            .take_arr::<1>()
            .ok_or(InviteError::Malformed("truncated"))?[0];
        out.push(Relation::from_tag(tag).ok_or(InviteError::Relations("unknown relation"))?);
    }
    check_relations(&out)?;
    Ok(out)
}

/// TCP address of the enrollment listener: `host:port`, where host is a DNS
/// name, an IPv4 literal or a bracketed IPv6 literal and the port is required.
/// No scheme, path or whitespace. The exact string is signed; the device resolves
/// it, and authenticates the responder by [`EnrollmentKey`], never by the name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnrollmentEndpoint(String);

impl EnrollmentEndpoint {
    /// Validate and wrap an endpoint string.
    pub fn parse(s: &str) -> Result<Self, InviteError> {
        if s.len() > MAX_ENDPOINT_BYTES {
            return Err(InviteError::Endpoint("too long"));
        }
        let (host, port) = s
            .rsplit_once(':')
            .ok_or(InviteError::Endpoint("port required"))?;
        if let Some(v6) = host.strip_prefix('[') {
            v6.strip_suffix(']')
                .and_then(|a| a.parse::<Ipv6Addr>().ok())
                .ok_or(InviteError::Endpoint("invalid IPv6 literal"))?;
        } else {
            check_host(host)?;
        }
        let port_ok = !port.is_empty()
            && port.len() <= 5
            && port.bytes().all(|b| b.is_ascii_digit())
            && matches!(port.parse::<u32>(), Ok(1..=65_535));
        if !port_ok {
            return Err(InviteError::Endpoint("invalid port"));
        }
        Ok(Self(s.to_owned()))
    }

    /// The validated endpoint string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn check_host(host: &str) -> Result<(), InviteError> {
    if host.is_empty() || host.len() > 253 {
        return Err(InviteError::Endpoint("invalid host"));
    }
    let label_ok = |l: &str| {
        !l.is_empty()
            && l.len() <= 63
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    };
    if host.split('.').all(label_ok) {
        Ok(())
    } else {
        Err(InviteError::Endpoint("invalid host"))
    }
}

/// X25519 Noise static public key of the enrollment responder. The device runs a
/// PSK-free Noise handshake that authenticates the responder by this key, so a
/// clean device can reach the issuer before holding the mesh PSK. Not secret.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct EnrollmentKey(pub [u8; 32]);

impl core::fmt::Debug for EnrollmentKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "EnrollmentKey({})", super::hex_lower(&self.0))
    }
}

/// Everything the issuer chooses for a new invitation.
#[derive(Clone, Debug)]
pub struct InviteSpec {
    /// Human-readable trust-domain label (`[A-Za-z0-9._-]`, 1..=64 bytes).
    pub trust_domain_name: String,
    /// Public id of the PSK trust domain the redeemed device will join.
    pub trust_domain: TrustDomainId,
    /// TCP address of the enrollment listener.
    pub endpoint: EnrollmentEndpoint,
    /// Noise static key the listener at `endpoint` must prove.
    pub enrollment_key: EnrollmentKey,
    /// Exact authorized relations (canonical order).
    pub relations: Vec<Relation>,
    /// Optional full device identity; `None` makes the link bearer authorization.
    pub intended_subject: Option<EntityId>,
    /// Creation time, expiry and approval mode.
    pub policy: InvitationPolicy,
}

/// An issuer-signed membership invitation whose signature has been verified.
#[derive(Clone, PartialEq, Eq)]
pub struct MembershipInvite {
    issuer: EntityId,
    trust_domain_name: String,
    trust_domain: TrustDomainId,
    endpoint: EnrollmentEndpoint,
    enrollment_key: EnrollmentKey,
    invitation_id: InvitationId,
    policy: InvitationPolicy,
    intended_subject: Option<EntityId>,
    relations: Vec<Relation>,
    /// Canonical signed bytes, signature included.
    bytes: Vec<u8>,
}

/// Redacts the invitation identifier and signature (bearer material).
impl core::fmt::Debug for MembershipInvite {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MembershipInvite")
            .field("issuer", &fingerprint(&self.issuer))
            .field("trust_domain_name", &self.trust_domain_name)
            .field("trust_domain", &self.trust_domain)
            .field("endpoint", &self.endpoint.as_str())
            .field("enrollment_key", &self.enrollment_key)
            .field("invitation_id", &"<redacted>")
            .field("policy", &self.policy)
            .field(
                "intended_subject",
                &self.intended_subject.as_ref().map(fingerprint),
            )
            .field("relations", &self.relations)
            .finish()
    }
}

impl MembershipInvite {
    /// Sign a new invitation with a fresh random [`InvitationId`].
    ///
    /// The caller must hold local operator authority for `issuer` and record the
    /// result in its durable ledger ([`Self::offer_spec`]) before sending it.
    pub fn sign(issuer: &Identity, spec: InviteSpec) -> Result<Self, InviteError> {
        check_trust_domain_name(&spec.trust_domain_name)?;
        check_relations(&spec.relations)?;
        let invitation_id = InvitationId::random().map_err(|_| InviteError::Random)?;
        let mut body = Vec::new();
        body.extend_from_slice(&INVITE_MAGIC);
        body.extend_from_slice(issuer.entity_id().as_bytes());
        super::push_lp(&mut body, spec.trust_domain_name.as_bytes());
        body.extend_from_slice(spec.trust_domain.as_bytes());
        super::push_lp(&mut body, spec.endpoint.as_str().as_bytes());
        body.extend_from_slice(&spec.enrollment_key.0);
        body.extend_from_slice(invitation_id.as_bytes());
        body.extend_from_slice(&spec.policy.created_at().to_le_bytes());
        body.extend_from_slice(&spec.policy.expires_at().to_le_bytes());
        body.push(match spec.policy.approval_mode() {
            ApprovalMode::Preauthorized => 0,
            ApprovalMode::RequireApproval => 1,
        });
        match &spec.intended_subject {
            Some(s) => {
                body.push(1);
                body.extend_from_slice(s.as_bytes());
            }
            None => body.push(0),
        }
        put_relations(&mut body, &spec.relations);
        let signature = issuer.sign(&signing_message(&body));
        body.extend_from_slice(&signature);
        if body.len() > MAX_INVITE_BYTES {
            return Err(InviteError::TooLarge);
        }
        Ok(Self {
            issuer: issuer.entity_id().clone(),
            trust_domain_name: spec.trust_domain_name,
            trust_domain: spec.trust_domain,
            endpoint: spec.endpoint,
            enrollment_key: spec.enrollment_key,
            invitation_id,
            policy: spec.policy,
            intended_subject: spec.intended_subject,
            relations: spec.relations,
            bytes: body,
        })
    }

    /// Parse canonical bytes and verify the embedded issuer's signature.
    /// Rejects oversize input, bad magic, truncation, trailing bytes, invalid
    /// endpoint/name/policy/relations and any tampering.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, InviteError> {
        if bytes.len() > MAX_INVITE_BYTES {
            return Err(InviteError::TooLarge);
        }
        let body_len = bytes
            .len()
            .checked_sub(64)
            .ok_or(InviteError::Malformed("truncated"))?;
        let (body, sig) = bytes.split_at(body_len);
        let t = InviteError::Malformed("truncated");
        let mut r = Reader::new(body);
        if r.take_arr::<4>() != Some(INVITE_MAGIC) {
            return Err(InviteError::Malformed("bad magic or version"));
        }
        let issuer = EntityId::from_bytes(r.take_arr::<32>().ok_or(t.clone())?);
        let name = r
            .take_lp_string()
            .ok_or(InviteError::Malformed("bad trust-domain name"))?;
        check_trust_domain_name(&name)?;
        let trust_domain = TrustDomainId::from_bytes(r.take_arr::<16>().ok_or(t.clone())?);
        let endpoint = r
            .take_lp_string()
            .ok_or(InviteError::Malformed("bad endpoint"))?;
        let endpoint = EnrollmentEndpoint::parse(&endpoint)?;
        let enrollment_key = EnrollmentKey(r.take_arr::<32>().ok_or(t.clone())?);
        let invitation_id = InvitationId::from_bytes(r.take_arr::<16>().ok_or(t.clone())?);
        let created = r.take_u64().ok_or(t.clone())?;
        let expires = r.take_u64().ok_or(t.clone())?;
        let mode = match r.take_arr::<1>().ok_or(t.clone())?[0] {
            0 => ApprovalMode::Preauthorized,
            1 => ApprovalMode::RequireApproval,
            _ => return Err(InviteError::Malformed("bad approval mode")),
        };
        let policy =
            InvitationPolicy::from_stored(created, expires, mode).ok_or(InviteError::Policy)?;
        let intended_subject = match r.take_arr::<1>().ok_or(t.clone())?[0] {
            0 => None,
            1 => Some(EntityId::from_bytes(r.take_arr::<32>().ok_or(t.clone())?)),
            _ => return Err(InviteError::Malformed("bad intended-subject flag")),
        };
        let relations = take_relations(&mut r)?;
        if !r.done() {
            return Err(InviteError::Malformed("trailing bytes"));
        }
        let mut signature = [0u8; 64];
        signature.copy_from_slice(sig);
        issuer
            .verify_bytes(&signing_message(body), &signature)
            .map_err(|_| InviteError::BadSignature)?;
        Ok(Self {
            issuer,
            trust_domain_name: name,
            trust_domain,
            endpoint,
            enrollment_key,
            invitation_id,
            policy,
            intended_subject,
            relations,
            bytes: bytes.to_vec(),
        })
    }

    /// The `net-join:` link. **Bearer material** unless subject-bound: never log it.
    pub fn encode(&self) -> String {
        let mut s = String::from(JOIN_LINK_PREFIX);
        s.push_str(&URL_SAFE_NO_PAD.encode(&self.bytes));
        s
    }

    /// Parse and verify a `net-join:` link. Tolerates surrounding whitespace only.
    /// Offline: performs no network request and consumes nothing.
    pub fn decode(link: &str) -> Result<Self, InviteError> {
        let body = link
            .trim()
            .strip_prefix(JOIN_LINK_PREFIX)
            .ok_or(InviteError::Malformed("missing net-join: prefix"))?;
        // base64 expands 3 bytes to 4 characters; refuse before decoding.
        if body.len() > MAX_INVITE_BYTES.div_ceil(3) * 4 {
            return Err(InviteError::TooLarge);
        }
        let bytes = URL_SAFE_NO_PAD
            .decode(body)
            .map_err(|_| InviteError::Malformed("invalid base64"))?;
        Self::from_bytes(&bytes)
    }

    /// Canonical signed bytes (bearer material).
    pub fn to_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Full issuer identity the signature verified against. Not proof that this
    /// is the *intended* issuer: compare [`Self::issuer_fingerprint`] out of band.
    pub fn issuer(&self) -> &EntityId {
        &self.issuer
    }

    /// Human-comparable issuer fingerprint.
    pub fn issuer_fingerprint(&self) -> String {
        fingerprint(&self.issuer)
    }

    /// Trust-domain label (untrusted display text, integrity-protected).
    pub fn trust_domain_name(&self) -> &str {
        &self.trust_domain_name
    }

    /// Public id of the bound trust domain.
    pub fn trust_domain(&self) -> TrustDomainId {
        self.trust_domain
    }

    /// Refuse a delivered trust domain that is not the one this invite names.
    pub fn check_trust_domain(&self, delivered: TrustDomainId) -> Result<(), InviteError> {
        if delivered == self.trust_domain {
            Ok(())
        } else {
            Err(InviteError::TrustDomainMismatch)
        }
    }

    /// Redemption endpoint.
    pub fn endpoint(&self) -> &EnrollmentEndpoint {
        &self.endpoint
    }

    /// Noise static key the enrollment responder must prove.
    pub fn enrollment_key(&self) -> EnrollmentKey {
        self.enrollment_key
    }

    /// Single-use identifier (sensitive; `Debug` redacts it).
    pub fn invitation_id(&self) -> InvitationId {
        self.invitation_id
    }

    /// Creation-time policy (expiry and approval mode).
    pub fn policy(&self) -> InvitationPolicy {
        self.policy
    }

    /// Intended device, if the link is subject-bound.
    pub fn intended_subject(&self) -> Option<&EntityId> {
        self.intended_subject.as_ref()
    }

    /// Whether anyone holding the link may redeem it first.
    pub fn is_bearer(&self) -> bool {
        self.intended_subject.is_none()
    }

    /// Exact authorized relations.
    pub fn relations(&self) -> &[Relation] {
        &self.relations
    }

    /// Digest of the complete signed invite, as stored by the ledger.
    pub fn digest(&self) -> [u8; 32] {
        blake3::derive_key(INVITE_DIGEST_CONTEXT, &self.bytes)
    }

    /// Canonical digest of the authorized scope (issuer, trust domain, relations).
    pub fn scope_digest(&self) -> [u8; 32] {
        let mut buf = Vec::with_capacity(32 + 16 + 1 + MAX_RELATIONS);
        buf.extend_from_slice(self.issuer.as_bytes());
        buf.extend_from_slice(self.trust_domain.as_bytes());
        put_relations(&mut buf, &self.relations);
        blake3::derive_key(SCOPE_DIGEST_CONTEXT, &buf)
    }

    /// The ledger record for this invitation.
    pub fn offer_spec(&self) -> OfferSpec {
        OfferSpec {
            invitation_id: self.invitation_id,
            invite_digest: self.digest(),
            scope_digest: self.scope_digest(),
            intended_subject: self.intended_subject.clone(),
            policy: self.policy,
        }
    }
}

fn check_trust_domain_name(name: &str) -> Result<(), InviteError> {
    let ok = !name.is_empty()
        && name.len() <= MAX_TRUST_DOMAIN_NAME
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
    ok.then_some(()).ok_or(InviteError::TrustDomainName)
}

fn signing_message(body: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(INVITE_SIGNATURE_DOMAIN.len() + body.len());
    m.extend_from_slice(INVITE_SIGNATURE_DOMAIN);
    m.extend_from_slice(body);
    m
}

/// The device's canonical redemption request for one exact invitation.
///
/// Persist it (with the device key) before the first redeem attempt so a retry
/// presents the identical intent. Its digest is the ledger claim's intent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RedemptionIntent {
    invite_digest: [u8; 32],
    subject: EntityId,
    relations: Vec<Relation>,
}

impl RedemptionIntent {
    /// Request exactly the invitation's relations for `subject`, refusing a
    /// subject other than a bound invitation's intended device.
    pub fn for_invite(invite: &MembershipInvite, subject: EntityId) -> Result<Self, InviteError> {
        let intent = Self {
            invite_digest: invite.digest(),
            subject,
            relations: invite.relations.clone(),
        };
        intent.check_against(invite)?;
        Ok(intent)
    }

    /// Owner-side check that this intent names this exact invite, its exact
    /// relations, and (if bound) its intended subject. Does not prove key
    /// possession; the redemption transport verifies that separately.
    pub fn check_against(&self, invite: &MembershipInvite) -> Result<(), InviteError> {
        if invite
            .intended_subject
            .as_ref()
            .is_some_and(|s| *s != self.subject)
        {
            return Err(InviteError::WrongSubject);
        }
        if self.invite_digest != invite.digest() || self.relations != invite.relations {
            return Err(InviteError::IntentMismatch);
        }
        Ok(())
    }

    /// Full device identity making the request.
    pub fn subject(&self) -> &EntityId {
        &self.subject
    }

    /// Requested relations.
    pub fn relations(&self) -> &[Relation] {
        &self.relations
    }

    /// Canonical bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(4 + 32 + 32 + 1 + MAX_RELATIONS);
        out.extend_from_slice(&INTENT_MAGIC);
        out.extend_from_slice(&self.invite_digest);
        out.extend_from_slice(self.subject.as_bytes());
        put_relations(&mut out, &self.relations);
        out
    }

    /// Parse canonical bytes; rejects bad magic, truncation, invalid relation
    /// sets and trailing bytes. Call [`Self::check_against`] before use.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, InviteError> {
        let t = InviteError::Malformed("truncated intent");
        let mut r = Reader::new(bytes);
        if r.take_arr::<4>() != Some(INTENT_MAGIC) {
            return Err(InviteError::Malformed("bad intent magic or version"));
        }
        let invite_digest = r.take_arr::<32>().ok_or(t.clone())?;
        let subject = EntityId::from_bytes(r.take_arr::<32>().ok_or(t)?);
        let relations = take_relations(&mut r)?;
        if !r.done() {
            return Err(InviteError::Malformed("trailing bytes"));
        }
        Ok(Self {
            invite_digest,
            subject,
            relations,
        })
    }

    /// Canonical digest over [`Self::to_bytes`].
    pub fn digest(&self) -> [u8; 32] {
        blake3::derive_key(INTENT_DIGEST_CONTEXT, &self.to_bytes())
    }

    /// The ledger claimant for this intent. Only meaningful after the caller
    /// verified key possession for [`Self::subject`].
    pub fn claimant(&self) -> Claimant {
        Claimant {
            subject: self.subject.clone(),
            intent_digest: self.digest(),
        }
    }
}
