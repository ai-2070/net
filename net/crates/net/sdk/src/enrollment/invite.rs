// SPDX-License-Identifier: MIT OR Apache-2.0
//! Signed membership invitation (`netmesh-join_` token) and canonical redemption intent.
//!
//! A [`MembershipInvite`] binds, under one issuer signature: the full issuer
//! identity, a named trust domain and its public [`TrustDomainId`], where to
//! redeem — a direct TCP enrollment endpoint, a blind-relay [`RelayLocator`], or
//! both (direct is tried first) — and the enrollment Noise static key, a random single-use
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

/// Prefix of the join token: `netmesh-join_<base64url invite>`. Deliberately
/// not URL-shaped, so browsers and chat apps do not treat it as a link, and
/// fixed so secret scanners can recognize a leaked token. Distinct from the
/// legacy `net-invite:` form.
pub const JOIN_TOKEN_PREFIX: &str = "netmesh-join_";
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
const TAG_SUBNET: u8 = 2;
const TAG_ORG: u8 = 3;
const TAG_CHANNEL: u8 = 4;

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
/// Mesh membership, subnet attachment, organization membership and channel
/// credentials are defined. Unknown tags are refused.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Relation {
    /// Membership-only device association with the issuer's mesh. Grants no
    /// delegation, invocation, management, organization, channel or subnet right.
    Mesh,
    /// Endpoint attachment to one authority-qualified subnet scope with the
    /// exact rights of the invite's signed [`SubnetOffer`], delivered as a
    /// delegated credential set for this device only. Independent of mesh
    /// membership: it grants nothing outside that scope.
    Subnet,
    /// Membership of one organization, named by the invite's signed
    /// [`OrgOffer`], delivered as an `OrgMembershipCert` signed by that
    /// org's root for this device only. Proves belonging only: no dispatcher,
    /// capability or management right. Always operator-approved, because
    /// only the offline org root can sign the certificate.
    Org,
    /// Publish and/or subscribe on one canonical channel, named by the
    /// invite's signed [`ChannelOffer`], delivered as a token chain
    /// `root → issuing node → device` minted for this device only. In v1 it
    /// rides with [`Relation::Mesh`], or stands alone for a device already on
    /// the mesh; a subscribe right names the issuing node as the publisher.
    Channel,
}

impl Relation {
    fn tag(self) -> u8 {
        match self {
            Self::Mesh => TAG_MESH,
            Self::Subnet => TAG_SUBNET,
            Self::Org => TAG_ORG,
            Self::Channel => TAG_CHANNEL,
        }
    }

    fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            TAG_MESH => Some(Self::Mesh),
            TAG_SUBNET => Some(Self::Subnet),
            TAG_ORG => Some(Self::Org),
            TAG_CHANNEL => Some(Self::Channel),
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

/// Where a device that cannot be reached directly is reachable: a blind relay's
/// `host:port` (UDP for the mesh, TCP on the same port number for enrollment
/// splices) and the device's registration id there. The relay is blind: it
/// forwards ciphertext and cannot answer for the device, whose keys are
/// authenticated end-to-end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelayLocator {
    /// The relay's address.
    pub endpoint: EnrollmentEndpoint,
    /// The device's registration id on that relay.
    pub registration: [u8; 16],
}

pub(super) fn put_opt_endpoint(out: &mut Vec<u8>, endpoint: Option<&EnrollmentEndpoint>) {
    match endpoint {
        Some(e) => {
            out.push(1);
            super::push_lp(out, e.as_str().as_bytes());
        }
        None => out.push(0),
    }
}

pub(super) fn take_opt_endpoint(
    r: &mut Reader<'_>,
) -> Result<Option<EnrollmentEndpoint>, InviteError> {
    match r
        .take_arr::<1>()
        .ok_or(InviteError::Malformed("truncated"))?[0]
    {
        0 => Ok(None),
        1 => {
            let s = r
                .take_lp_string()
                .ok_or(InviteError::Malformed("bad endpoint"))?;
            Ok(Some(EnrollmentEndpoint::parse(&s)?))
        }
        _ => Err(InviteError::Malformed("bad endpoint flag")),
    }
}

pub(super) fn put_relay(out: &mut Vec<u8>, relay: Option<&RelayLocator>) {
    put_opt_endpoint(out, relay.map(|r| &r.endpoint));
    if let Some(r) = relay {
        out.extend_from_slice(&r.registration);
    }
}

pub(super) fn take_relay(r: &mut Reader<'_>) -> Result<Option<RelayLocator>, InviteError> {
    let Some(endpoint) = take_opt_endpoint(r)? else {
        return Ok(None);
    };
    let registration = r
        .take_arr::<16>()
        .ok_or(InviteError::Malformed("truncated"))?;
    Ok(Some(RelayLocator {
        endpoint,
        registration,
    }))
}

/// The subnet attachment an invite offers (with [`Relation::Subnet`]):
/// exactly one authority-qualified scope, its topology epoch and the rights
/// the delivered credential will carry. Signed with the rest of the invite.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubnetOffer {
    /// Authority-qualified scope attached to.
    pub scope: net::adapter::net::subnet::SubnetRef,
    /// Topology epoch the credential is minted under.
    pub topology_epoch: u32,
    /// Rights the credential carries; strict and non-empty.
    pub rights: net::adapter::net::subnet::SubnetRights,
}

fn put_subnet_offer(out: &mut Vec<u8>, offer: Option<&SubnetOffer>) {
    match offer {
        Some(o) => {
            out.push(1);
            out.extend_from_slice(o.scope.authority.as_bytes());
            out.extend_from_slice(&o.scope.path.raw().to_le_bytes());
            out.extend_from_slice(&o.topology_epoch.to_le_bytes());
            out.push(o.rights.bits());
        }
        None => out.push(0),
    }
}

fn take_subnet_offer(r: &mut Reader<'_>) -> Result<Option<SubnetOffer>, InviteError> {
    let t = InviteError::Malformed("truncated");
    match r.take_arr::<1>().ok_or(t.clone())?[0] {
        0 => Ok(None),
        1 => {
            let authority = EntityId::from_bytes(r.take_arr::<32>().ok_or(t.clone())?);
            let path = u32::from_le_bytes(r.take_arr::<4>().ok_or(t.clone())?);
            let topology_epoch = u32::from_le_bytes(r.take_arr::<4>().ok_or(t.clone())?);
            let rights = net::adapter::net::subnet::SubnetRights::try_from_bits(
                r.take_arr::<1>().ok_or(t)?[0],
            )
            .map_err(|_| InviteError::Malformed("bad subnet rights"))?;
            Ok(Some(SubnetOffer {
                scope: net::adapter::net::subnet::SubnetRef {
                    authority,
                    path: net::adapter::net::subnet::TopologySubnetId::from_raw(path),
                },
                topology_epoch,
                rights,
            }))
        }
        _ => Err(InviteError::Malformed("bad subnet offer flag")),
    }
}

/// A subnet offer exists exactly when the relation set names
/// [`Relation::Subnet`].
fn check_subnet_offer(
    relations: &[Relation],
    offer: Option<&SubnetOffer>,
) -> Result<(), InviteError> {
    if relations.contains(&Relation::Subnet) != offer.is_some() {
        return Err(InviteError::Relations("subnet relation and offer disagree"));
    }
    Ok(())
}

/// The organization an invite offers membership of (with [`Relation::Org`]).
/// Signed with the rest of the invite.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OrgOffer {
    /// The organization (its root's public key).
    pub org: net::adapter::net::behavior::org::OrgId,
}

fn put_org_offer(out: &mut Vec<u8>, offer: Option<&OrgOffer>) {
    match offer {
        Some(o) => {
            out.push(1);
            out.extend_from_slice(&o.org.0);
        }
        None => out.push(0),
    }
}

fn take_org_offer(r: &mut Reader<'_>) -> Result<Option<OrgOffer>, InviteError> {
    let t = InviteError::Malformed("truncated");
    match r.take_arr::<1>().ok_or(t.clone())?[0] {
        0 => Ok(None),
        1 => Ok(Some(OrgOffer {
            org: net::adapter::net::behavior::org::OrgId(r.take_arr::<32>().ok_or(t)?),
        })),
        _ => Err(InviteError::Malformed("bad org offer flag")),
    }
}

/// An org offer exists exactly when the relation set names [`Relation::Org`],
/// and such an invite always requires operator approval: the certificate is
/// signed by the offline org root at approval, never by the issuing node.
fn check_org_offer(
    relations: &[Relation],
    offer: Option<&OrgOffer>,
    mode: ApprovalMode,
) -> Result<(), InviteError> {
    let org = relations.contains(&Relation::Org);
    if org != offer.is_some() {
        return Err(InviteError::Relations("org relation and offer disagree"));
    }
    if org && mode != ApprovalMode::RequireApproval {
        return Err(InviteError::Relations(
            "an org relation requires operator approval",
        ));
    }
    Ok(())
}

/// The channel credential an invite offers (with [`Relation::Channel`]):
/// one canonical channel, the token root its chain anchors at, and the
/// rights the device's leaf will carry (publish and/or subscribe, never
/// more). Signed with the rest of the invite.
///
/// The encoding carries the canonical `u64` channel hash next to the name;
/// decoding refuses a hash that is not the name's, so a policy is never
/// keyed by the name's `u16` wire hint or by a hash the name does not own.
/// With a subscribe right the publisher is the issuing node (the invite's
/// issuer, reached at the bundle's contact): the subscribe ACK is a routing
/// fact about that node, not a proof of its full identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelOffer {
    /// The canonical channel.
    pub channel: net::adapter::net::channel::ChannelName,
    /// The token root the delivered chain anchors at.
    pub root: EntityId,
    /// Rights the device's leaf carries: a non-empty subset of
    /// publish/subscribe.
    pub rights: net::adapter::net::identity::TokenScope,
}

impl ChannelOffer {
    /// Whether `rights` is a non-empty subset of publish/subscribe.
    pub fn rights_are_channel_link(rights: net::adapter::net::identity::TokenScope) -> bool {
        rights.bits() != 0 && crate::channel_issuer::CHANNEL_LINK_RIGHTS.contains(rights)
    }
}

fn put_channel_offer(out: &mut Vec<u8>, offer: Option<&ChannelOffer>) {
    match offer {
        Some(o) => {
            out.push(1);
            super::push_lp(out, o.channel.as_str().as_bytes());
            out.extend_from_slice(&o.channel.hash().to_le_bytes());
            out.extend_from_slice(o.root.as_bytes());
            // Bounded: channel-link rights fit in the low byte.
            out.push(o.rights.bits() as u8);
        }
        None => out.push(0),
    }
}

fn take_channel_offer(r: &mut Reader<'_>) -> Result<Option<ChannelOffer>, InviteError> {
    let t = InviteError::Malformed("truncated");
    match r.take_arr::<1>().ok_or(t.clone())?[0] {
        0 => Ok(None),
        1 => {
            let name = r
                .take_lp_string()
                .ok_or(InviteError::Malformed("bad channel name"))?;
            let channel = net::adapter::net::channel::ChannelName::new(&name)
                .map_err(|_| InviteError::Malformed("bad channel name"))?;
            let hash = u64::from_le_bytes(r.take_arr::<8>().ok_or(t.clone())?);
            if hash != channel.hash() {
                return Err(InviteError::Malformed("channel hash is not the name's"));
            }
            let root = EntityId::from_bytes(r.take_arr::<32>().ok_or(t.clone())?);
            let rights = net::adapter::net::identity::TokenScope::from_bits(u32::from(
                r.take_arr::<1>().ok_or(t)?[0],
            ));
            if !ChannelOffer::rights_are_channel_link(rights) {
                return Err(InviteError::Malformed("bad channel rights"));
            }
            Ok(Some(ChannelOffer {
                channel,
                root,
                rights,
            }))
        }
        _ => Err(InviteError::Malformed("bad channel offer flag")),
    }
}

/// A channel offer exists exactly when the relation set names
/// [`Relation::Channel`]. It rides with [`Relation::Mesh`], or stands alone
/// (a standalone channel link, for a device already on the mesh); its
/// rights are publish and/or subscribe only.
fn check_channel_offer(
    relations: &[Relation],
    offer: Option<&ChannelOffer>,
) -> Result<(), InviteError> {
    let channel = relations.contains(&Relation::Channel);
    if channel != offer.is_some() {
        return Err(InviteError::Relations(
            "channel relation and offer disagree",
        ));
    }
    if channel && !relations.contains(&Relation::Mesh) && relations != [Relation::Channel] {
        return Err(InviteError::Relations(
            "a channel relation rides with mesh membership or stands alone",
        ));
    }
    if offer.is_some_and(|o| !ChannelOffer::rights_are_channel_link(o.rights)) {
        return Err(InviteError::Relations(
            "channel rights are publish and/or subscribe",
        ));
    }
    Ok(())
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
    /// Direct TCP address of the enrollment listener, if one is known.
    pub endpoint: Option<EnrollmentEndpoint>,
    /// Blind relay the device is registered with, if any. At least one of
    /// `endpoint` and `relay` is required.
    pub relay: Option<RelayLocator>,
    /// Noise static key the enrollment responder must prove, on either path.
    pub enrollment_key: EnrollmentKey,
    /// The subnet attachment offered; required exactly when `relations`
    /// names [`Relation::Subnet`].
    pub subnet: Option<SubnetOffer>,
    /// The organization offered; required exactly when `relations` names
    /// [`Relation::Org`].
    pub org: Option<OrgOffer>,
    /// The channel credential offered; required exactly when `relations`
    /// names [`Relation::Channel`].
    pub channel: Option<ChannelOffer>,
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
    endpoint: Option<EnrollmentEndpoint>,
    relay: Option<RelayLocator>,
    enrollment_key: EnrollmentKey,
    subnet: Option<SubnetOffer>,
    org: Option<OrgOffer>,
    channel: Option<ChannelOffer>,
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
            .field("endpoint", &self.endpoint.as_ref().map(|e| e.as_str()))
            .field("relay", &self.relay)
            .field("subnet", &self.subnet)
            .field("org", &self.org)
            .field("channel", &self.channel)
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
        if spec.endpoint.is_none() && spec.relay.is_none() {
            return Err(InviteError::Endpoint("no direct endpoint or relay"));
        }
        check_subnet_offer(&spec.relations, spec.subnet.as_ref())?;
        check_org_offer(
            &spec.relations,
            spec.org.as_ref(),
            spec.policy.approval_mode(),
        )?;
        check_channel_offer(&spec.relations, spec.channel.as_ref())?;
        let invitation_id = InvitationId::random().map_err(|_| InviteError::Random)?;
        let mut body = Vec::new();
        body.extend_from_slice(&INVITE_MAGIC);
        body.extend_from_slice(issuer.entity_id().as_bytes());
        super::push_lp(&mut body, spec.trust_domain_name.as_bytes());
        body.extend_from_slice(spec.trust_domain.as_bytes());
        put_opt_endpoint(&mut body, spec.endpoint.as_ref());
        put_relay(&mut body, spec.relay.as_ref());
        put_subnet_offer(&mut body, spec.subnet.as_ref());
        put_org_offer(&mut body, spec.org.as_ref());
        put_channel_offer(&mut body, spec.channel.as_ref());
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
            relay: spec.relay,
            enrollment_key: spec.enrollment_key,
            subnet: spec.subnet,
            org: spec.org,
            channel: spec.channel,
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
        let endpoint = take_opt_endpoint(&mut r)?;
        let relay = take_relay(&mut r)?;
        if endpoint.is_none() && relay.is_none() {
            return Err(InviteError::Endpoint("no direct endpoint or relay"));
        }
        let subnet = take_subnet_offer(&mut r)?;
        let org = take_org_offer(&mut r)?;
        let channel = take_channel_offer(&mut r)?;
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
        check_subnet_offer(&relations, subnet.as_ref())?;
        check_org_offer(&relations, org.as_ref(), mode)?;
        check_channel_offer(&relations, channel.as_ref())?;
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
            relay,
            enrollment_key,
            subnet,
            org,
            channel,
            invitation_id,
            policy,
            intended_subject,
            relations,
            bytes: bytes.to_vec(),
        })
    }

    /// The join token, `netmesh-join_<base64url invite>`. It carries the
    /// address, keys and policy inside the signed invite; show them to humans
    /// through inspection, not by reading the token.
    /// **Bearer material** unless subject-bound: never log it.
    pub fn encode(&self) -> String {
        let mut s = String::from(JOIN_TOKEN_PREFIX);
        s.push_str(&URL_SAFE_NO_PAD.encode(&self.bytes));
        s
    }

    /// Parse and verify a `netmesh-join_` token. Tolerates surrounding
    /// whitespace only. Offline: performs no network request and consumes
    /// nothing.
    pub fn decode(token: &str) -> Result<Self, InviteError> {
        let body = token
            .trim()
            .strip_prefix(JOIN_TOKEN_PREFIX)
            .ok_or(InviteError::Malformed("missing netmesh-join_ prefix"))?;
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

    /// Direct redemption endpoint, if the issuer knew one.
    pub fn endpoint(&self) -> Option<&EnrollmentEndpoint> {
        self.endpoint.as_ref()
    }

    /// The subnet attachment offered, with [`Relation::Subnet`].
    pub fn subnet(&self) -> Option<&SubnetOffer> {
        self.subnet.as_ref()
    }

    /// The organization offered, with [`Relation::Org`].
    pub fn org(&self) -> Option<&OrgOffer> {
        self.org.as_ref()
    }

    /// The channel credential offered, with [`Relation::Channel`].
    pub fn channel(&self) -> Option<&ChannelOffer> {
        self.channel.as_ref()
    }

    /// Blind relay to fall back to when the direct endpoint is unreachable.
    pub fn relay(&self) -> Option<&RelayLocator> {
        self.relay.as_ref()
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
