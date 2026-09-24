// SPDX-License-Identifier: MIT OR Apache-2.0
//! Membership-only enrollment bundle and the issuer that produces it.
//!
//! A [`MembershipBundle`] is what a successful redemption delivers inside the
//! authenticated enrollment session: an issuer-signed [`MembershipReceipt`], the
//! mesh trust-domain [`Psk`], and a [`MeshContact`] to attach through. The
//! receipt records that this issuer enrolled this exact device for the invite's
//! exact relations. It carries **no** delegation chain, permission token,
//! invocation, management, organization, channel or subnet right, and is not
//! presented to any gate as authority.
//!
//! A standing PSK, once delivered, cannot be reclaimed by expiring or revoking
//! the invitation. [`MembershipBundle::verify_for`] makes the device refuse a
//! bundle whose PSK is not the trust domain the signed invite named.

use std::net::SocketAddr;

use super::invite::{InviteError, MembershipInvite, RedemptionIntent, Relation, RelayLocator};
use super::redeem::Refusal;
use super::service::BundleIssuer;
use super::{fingerprint, now_unix, push_lp, Reader};
use crate::bootstrap_credential::{Psk, TrustDomainId};
use crate::identity::{EntityId, Identity};

const RECEIPT_MAGIC: [u8; 4] = *b"NMP1";
const BUNDLE_MAGIC: [u8; 4] = *b"NMB1";
const RECEIPT_SIGNATURE_DOMAIN: &[u8] = b"net-mesh membership receipt v1";
const MAX_RECEIPT_BYTES: usize = 512;
const MAX_ADDR_BYTES: usize = 64;

/// Bundle decode/verification failure. Never echoes the PSK.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BundleError {
    /// Structural decode failure.
    #[error("malformed membership bundle: {0}")]
    Malformed(&'static str),
    /// The receipt signature does not verify.
    #[error("membership receipt signature is invalid")]
    BadSignature,
    /// The receipt does not match the invite or intent it answers.
    #[error("membership receipt does not match the invitation: {0}")]
    Mismatch(&'static str),
    /// The delivered PSK is not the invite's trust domain.
    #[error("delivered PSK is not the invitation's trust domain")]
    TrustDomain,
}

/// Issuer-signed record that `subject` was enrolled from one exact invite and
/// intent. Membership only: grants no right by itself.
#[derive(Clone, PartialEq, Eq)]
pub struct MembershipReceipt {
    issuer: EntityId,
    subject: EntityId,
    invite_digest: [u8; 32],
    intent_digest: [u8; 32],
    trust_domain: TrustDomainId,
    relations: Vec<Relation>,
    issued_at: u64,
    bytes: Vec<u8>,
}

impl core::fmt::Debug for MembershipReceipt {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MembershipReceipt")
            .field("issuer", &fingerprint(&self.issuer))
            .field("subject", &fingerprint(&self.subject))
            .field("trust_domain", &self.trust_domain)
            .field("relations", &self.relations)
            .field("issued_at", &self.issued_at)
            .finish()
    }
}

impl MembershipReceipt {
    /// Sign a receipt for `intent` against `invite`. The caller has already
    /// committed this claim in its ledger.
    pub fn sign(
        issuer: &Identity,
        invite: &MembershipInvite,
        intent: &RedemptionIntent,
        issued_at: u64,
    ) -> Self {
        let mut body = Vec::with_capacity(MAX_RECEIPT_BYTES);
        body.extend_from_slice(&RECEIPT_MAGIC);
        body.extend_from_slice(issuer.entity_id().as_bytes());
        body.extend_from_slice(intent.subject().as_bytes());
        body.extend_from_slice(&invite.digest());
        body.extend_from_slice(&intent.digest());
        body.extend_from_slice(invite.trust_domain().as_bytes());
        super::invite::put_relations(&mut body, invite.relations());
        body.extend_from_slice(&issued_at.to_le_bytes());
        let signature = issuer.sign(&signing_message(&body));
        body.extend_from_slice(&signature);
        Self {
            issuer: issuer.entity_id().clone(),
            subject: intent.subject().clone(),
            invite_digest: invite.digest(),
            intent_digest: intent.digest(),
            trust_domain: invite.trust_domain(),
            relations: invite.relations().to_vec(),
            issued_at,
            bytes: body,
        }
    }

    /// Parse and verify against the embedded issuer. Use [`Self::verify_for`]
    /// to bind it to the expected invite and intent.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, BundleError> {
        if bytes.len() > MAX_RECEIPT_BYTES {
            return Err(BundleError::Malformed("receipt too large"));
        }
        let t = BundleError::Malformed("truncated receipt");
        let body_len = bytes.len().checked_sub(64).ok_or(t.clone())?;
        let (body, sig) = bytes.split_at(body_len);
        let mut r = Reader::new(body);
        if r.take_arr::<4>() != Some(RECEIPT_MAGIC) {
            return Err(BundleError::Malformed("bad receipt magic or version"));
        }
        let issuer = EntityId::from_bytes(r.take_arr::<32>().ok_or(t.clone())?);
        let subject = EntityId::from_bytes(r.take_arr::<32>().ok_or(t.clone())?);
        let invite_digest = r.take_arr::<32>().ok_or(t.clone())?;
        let intent_digest = r.take_arr::<32>().ok_or(t.clone())?;
        let trust_domain = TrustDomainId::from_bytes(r.take_arr::<16>().ok_or(t.clone())?);
        let relations = super::invite::take_relations(&mut r)
            .map_err(|_| BundleError::Malformed("bad relations"))?;
        let issued_at = r.take_u64().ok_or(t)?;
        if !r.done() {
            return Err(BundleError::Malformed("trailing receipt bytes"));
        }
        let mut signature = [0u8; 64];
        signature.copy_from_slice(sig);
        issuer
            .verify_bytes(&signing_message(body), &signature)
            .map_err(|_| BundleError::BadSignature)?;
        Ok(Self {
            issuer,
            subject,
            invite_digest,
            intent_digest,
            trust_domain,
            relations,
            issued_at,
            bytes: bytes.to_vec(),
        })
    }

    /// Require this receipt to answer exactly `intent` for `invite`, from the
    /// invite's issuer.
    pub fn verify_for(
        &self,
        invite: &MembershipInvite,
        intent: &RedemptionIntent,
    ) -> Result<(), BundleError> {
        let checks: [(bool, &'static str); 6] = [
            (self.issuer == *invite.issuer(), "issuer"),
            (self.subject == *intent.subject(), "subject"),
            (self.invite_digest == invite.digest(), "invite"),
            (self.intent_digest == intent.digest(), "intent"),
            (self.trust_domain == invite.trust_domain(), "trust domain"),
            (self.relations == invite.relations(), "relations"),
        ];
        match checks.iter().find(|(ok, _)| !ok) {
            Some((_, what)) => Err(BundleError::Mismatch(what)),
            None => Ok(()),
        }
    }

    /// Enrolling issuer.
    pub fn issuer(&self) -> &EntityId {
        &self.issuer
    }

    /// Enrolled device.
    pub fn subject(&self) -> &EntityId {
        &self.subject
    }

    /// Relations recorded (membership only in v1).
    pub fn relations(&self) -> &[Relation] {
        &self.relations
    }

    /// Issuance time (Unix seconds).
    pub fn issued_at(&self) -> u64 {
        self.issued_at
    }

    /// Canonical signed bytes (not secret).
    pub fn to_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

fn signing_message(body: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(RECEIPT_SIGNATURE_DOMAIN.len() + body.len());
    m.extend_from_slice(RECEIPT_SIGNATURE_DOMAIN);
    m.extend_from_slice(body);
    m
}

/// Where the enrolled device first attaches: a mesh node's direct socket
/// address and/or the blind relay it is registered with, its Noise static
/// public key and routing node id (for `Mesh::connect_via`). Direct is tried
/// first; the session authenticates the key end-to-end on either path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeshContact {
    /// Directly reachable mesh socket address, if known.
    pub addr: Option<SocketAddr>,
    /// The node's mesh Noise static public key.
    pub noise_pubkey: [u8; 32],
    /// The node's routing node id.
    pub node_id: u64,
    /// Blind relay the node is registered with, if any.
    pub relay: Option<RelayLocator>,
}

/// Secret-bearing enrollment result: receipt, trust-domain PSK and first contact.
#[derive(Clone, PartialEq, Eq)]
pub struct MembershipBundle {
    receipt: MembershipReceipt,
    psk: Psk,
    contact: MeshContact,
    /// Encoded `SubnetCredentialSet` for [`Relation::Subnet`] invites.
    subnet: Option<Vec<u8>>,
    /// The org-root-signed membership certificate for [`Relation::Org`]
    /// invites.
    org: Option<net::adapter::net::behavior::org::OrgMembershipCert>,
    /// The org's shared owner audience (encoded), when the operator supplied
    /// it at approval. **Secret**: it opens the org's private announcements.
    org_audience: Option<Vec<u8>>,
    /// Encoded `TokenChain` (root → issuing node → device) for
    /// [`Relation::Channel`] invites.
    channel: Option<Vec<u8>>,
}

impl core::fmt::Debug for MembershipBundle {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MembershipBundle")
            .field("receipt", &self.receipt)
            .field("psk", &"<redacted>")
            .field("trust_domain", &self.psk.trust_domain())
            .field("contact", &self.contact)
            .field("subnet", &self.subnet.is_some())
            .field("org", &self.org.is_some())
            .field(
                "org_audience",
                &self.org_audience.as_ref().map(|_| "<redacted>"),
            )
            .field("channel", &self.channel.is_some())
            .finish()
    }
}

impl MembershipBundle {
    /// Assemble a bundle.
    pub fn new(receipt: MembershipReceipt, psk: Psk, contact: MeshContact) -> Self {
        Self {
            receipt,
            psk,
            contact,
            subnet: None,
            org: None,
            org_audience: None,
            channel: None,
        }
    }

    /// Attach the device's channel token chain (for a channel invite).
    pub fn with_channel_chain(mut self, chain: &net::adapter::net::identity::TokenChain) -> Self {
        self.channel = Some(chain.to_bytes());
        self
    }

    /// The delivered channel token chain, if this bundle carries one.
    pub fn channel_chain(&self) -> Option<net::adapter::net::identity::TokenChain> {
        self.channel
            .as_deref()
            .and_then(|b| net::adapter::net::identity::TokenChain::from_bytes(b).ok())
    }

    /// Attach the org's shared owner audience (with an org membership).
    pub fn with_org_audience(
        mut self,
        audience: &net::adapter::net::behavior::org_authority::OwnerAudienceCredential,
    ) -> Self {
        self.org_audience = Some(audience.encode_config().to_vec());
        self
    }

    /// The delivered org owner audience, if the bundle carries one.
    pub fn org_audience(
        &self,
    ) -> Option<net::adapter::net::behavior::org_authority::OwnerAudienceCredential> {
        self.org_audience.as_deref().and_then(|b| {
            net::adapter::net::behavior::org_authority::OwnerAudienceCredential::decode_config(b)
                .ok()
        })
    }

    /// Attach the device's org membership certificate (for an org invite).
    pub fn with_org_membership(
        mut self,
        cert: net::adapter::net::behavior::org::OrgMembershipCert,
    ) -> Self {
        self.org = Some(cert);
        self
    }

    /// The delivered org membership certificate, if this bundle carries one.
    pub fn org_membership(&self) -> Option<&net::adapter::net::behavior::org::OrgMembershipCert> {
        self.org.as_ref()
    }

    /// Attach the device's subnet credential set (for a subnet invite).
    pub fn with_subnet_credentials(
        mut self,
        set: &net::adapter::net::subnet::SubnetCredentialSet,
    ) -> Self {
        self.subnet = Some(set.to_bytes());
        self
    }

    /// The delivered subnet credential set, if this bundle carries one.
    pub fn subnet_credentials(&self) -> Option<net::adapter::net::subnet::SubnetCredentialSet> {
        self.subnet
            .as_deref()
            .and_then(|b| net::adapter::net::subnet::SubnetCredentialSet::from_bytes(b).ok())
    }

    /// Canonical bytes. **Secret** (contains the PSK).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(4 + 4 + self.receipt.bytes.len() + 32 + 5 + 64 + 40);
        out.extend_from_slice(&BUNDLE_MAGIC);
        push_lp(&mut out, &self.receipt.bytes);
        out.extend_from_slice(self.psk.expose_bytes());
        match self.contact.addr {
            Some(addr) => {
                out.push(1);
                push_lp(&mut out, addr.to_string().as_bytes());
            }
            None => out.push(0),
        }
        out.extend_from_slice(&self.contact.noise_pubkey);
        out.extend_from_slice(&self.contact.node_id.to_le_bytes());
        super::invite::put_relay(&mut out, self.contact.relay.as_ref());
        match &self.subnet {
            Some(set) => {
                out.push(1);
                push_lp(&mut out, set);
            }
            None => out.push(0),
        }
        match &self.org {
            Some(cert) => {
                out.push(1);
                push_lp(&mut out, &cert.to_bytes());
            }
            None => out.push(0),
        }
        match &self.org_audience {
            Some(audience) => {
                out.push(1);
                push_lp(&mut out, audience);
            }
            None => out.push(0),
        }
        match &self.channel {
            Some(chain) => {
                out.push(1);
                push_lp(&mut out, chain);
            }
            None => out.push(0),
        }
        out
    }

    /// Parse and verify the receipt signature. Call [`Self::verify_for`] next.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, BundleError> {
        let t = BundleError::Malformed("truncated bundle");
        let mut r = Reader::new(bytes);
        if r.take_arr::<4>() != Some(BUNDLE_MAGIC) {
            return Err(BundleError::Malformed("bad bundle magic or version"));
        }
        let receipt_len = r.take_u32().ok_or(t.clone())? as usize;
        if receipt_len > MAX_RECEIPT_BYTES {
            return Err(BundleError::Malformed("receipt too large"));
        }
        let receipt = MembershipReceipt::from_bytes(r.take(receipt_len).ok_or(t.clone())?)?;
        let psk = Psk::new(r.take_arr::<32>().ok_or(t.clone())?);
        let addr = match r.take_arr::<1>().ok_or(t.clone())?[0] {
            0 => None,
            1 => {
                let addr_len = r.take_u32().ok_or(t.clone())? as usize;
                if addr_len > MAX_ADDR_BYTES {
                    return Err(BundleError::Malformed("contact address too long"));
                }
                Some(
                    std::str::from_utf8(r.take(addr_len).ok_or(t.clone())?)
                        .ok()
                        .and_then(|s| s.parse::<SocketAddr>().ok())
                        .ok_or(BundleError::Malformed("bad contact address"))?,
                )
            }
            _ => return Err(BundleError::Malformed("bad contact address flag")),
        };
        let noise_pubkey = r.take_arr::<32>().ok_or(t.clone())?;
        let node_id = r.take_u64().ok_or(t)?;
        let relay = super::invite::take_relay(&mut r)
            .map_err(|_| BundleError::Malformed("bad contact relay"))?;
        if addr.is_none() && relay.is_none() {
            return Err(BundleError::Malformed("contact has no address or relay"));
        }
        let subnet = match r
            .take_arr::<1>()
            .ok_or(BundleError::Malformed("truncated bundle"))?[0]
        {
            0 => None,
            1 => {
                let bytes = r
                    .take_lp()
                    .ok_or(BundleError::Malformed("truncated bundle"))?
                    .to_vec();
                net::adapter::net::subnet::SubnetCredentialSet::from_bytes(&bytes)
                    .map_err(|_| BundleError::Malformed("bad subnet credentials"))?;
                Some(bytes)
            }
            _ => return Err(BundleError::Malformed("bad subnet flag")),
        };
        let org = match r
            .take_arr::<1>()
            .ok_or(BundleError::Malformed("truncated bundle"))?[0]
        {
            0 => None,
            1 => Some(
                net::adapter::net::behavior::org::OrgMembershipCert::from_bytes(
                    r.take_lp()
                        .ok_or(BundleError::Malformed("truncated bundle"))?,
                )
                .map_err(|_| BundleError::Malformed("bad org membership certificate"))?,
            ),
            _ => return Err(BundleError::Malformed("bad org flag")),
        };
        let org_audience = match r
            .take_arr::<1>()
            .ok_or(BundleError::Malformed("truncated bundle"))?[0]
        {
            0 => None,
            1 => {
                let bytes = r
                    .take_lp()
                    .ok_or(BundleError::Malformed("truncated bundle"))?
                    .to_vec();
                net::adapter::net::behavior::org_authority::OwnerAudienceCredential::decode_config(
                    &bytes,
                )
                .map_err(|_| BundleError::Malformed("bad org audience"))?;
                Some(bytes)
            }
            _ => return Err(BundleError::Malformed("bad org audience flag")),
        };
        let channel = match r
            .take_arr::<1>()
            .ok_or(BundleError::Malformed("truncated bundle"))?[0]
        {
            0 => None,
            1 => {
                let bytes = r
                    .take_lp()
                    .ok_or(BundleError::Malformed("truncated bundle"))?
                    .to_vec();
                net::adapter::net::identity::TokenChain::from_bytes(&bytes)
                    .map_err(|_| BundleError::Malformed("bad channel chain"))?;
                Some(bytes)
            }
            _ => return Err(BundleError::Malformed("bad channel flag")),
        };
        if !r.done() {
            return Err(BundleError::Malformed("trailing bundle bytes"));
        }
        Ok(Self {
            receipt,
            psk,
            contact: MeshContact {
                addr,
                noise_pubkey,
                node_id,
                relay,
            },
            subnet,
            org,
            org_audience,
            channel,
        })
    }

    /// Device-side check: the receipt answers exactly this invite and intent,
    /// and the PSK is the trust domain the signed invite named.
    pub fn verify_for(
        &self,
        invite: &MembershipInvite,
        intent: &RedemptionIntent,
    ) -> Result<(), BundleError> {
        self.receipt.verify_for(invite, intent)?;
        invite
            .check_trust_domain(self.psk.trust_domain())
            .map_err(|_| BundleError::TrustDomain)?;
        self.verify_subnet_for(invite, intent)?;
        self.verify_channel_for(invite, intent)?;
        match (invite.org(), &self.org) {
            (None, None) => {}
            (Some(offer), Some(cert)) if org_cert_matches(offer, intent.subject(), cert) => {}
            _ => return Err(BundleError::Mismatch("org membership")),
        }
        // An audience only with a membership, and only the offered org's.
        match (
            invite.org(),
            self.org_audience.is_some(),
            self.org_audience(),
        ) {
            (_, false, _) => Ok(()),
            (Some(offer), true, Some(audience)) if audience.owner_org == offer.org => Ok(()),
            _ => Err(BundleError::Mismatch("org audience")),
        }
    }

    /// The delivered subnet credentials must be exactly what the signed
    /// offer names, for exactly this device: same authority, scope, epoch
    /// and rights, subject = the intent's subject. A bundle for a subnet
    /// invite without them, or one carrying them for any other invite, is
    /// refused. (The credential chain's signatures are the verifier's to
    /// check at admission; the device checks it is receiving what it was
    /// offered.)
    fn verify_subnet_for(
        &self,
        invite: &MembershipInvite,
        intent: &RedemptionIntent,
    ) -> Result<(), BundleError> {
        match (invite.subnet(), self.subnet_credentials(), &self.subnet) {
            (None, None, None) => Ok(()),
            (Some(offer), Some(set), Some(_)) => {
                let leaf = set.leaf();
                let exact = leaf.authority == offer.scope.authority
                    && leaf.scope == offer.scope.path
                    && leaf.topology_epoch == offer.topology_epoch
                    && leaf.rights == offer.rights
                    && &leaf.subject == intent.subject();
                if exact {
                    Ok(())
                } else {
                    Err(BundleError::Mismatch("subnet credentials"))
                }
            }
            _ => Err(BundleError::Mismatch("subnet credentials")),
        }
    }

    /// The delivered channel chain must be exactly what the signed offer
    /// names, for exactly this device: anchored at the offered root, scoped
    /// to the offered channel, verifying link by link for every offered
    /// right, and with a leaf carrying exactly the offered rights for the
    /// intent's subject. A channel invite's bundle without one, or a chain
    /// on any other invite, is refused.
    fn verify_channel_for(
        &self,
        invite: &MembershipInvite,
        intent: &RedemptionIntent,
    ) -> Result<(), BundleError> {
        match (invite.channel(), self.channel_chain(), &self.channel) {
            (None, None, None) => Ok(()),
            (Some(offer), Some(chain), Some(_))
                if channel_chain_matches(offer, intent.subject(), &chain) =>
            {
                Ok(())
            }
            _ => Err(BundleError::Mismatch("channel chain")),
        }
    }

    /// The signed membership receipt.
    pub fn receipt(&self) -> &MembershipReceipt {
        &self.receipt
    }

    /// The mesh PSK (secret).
    pub fn psk(&self) -> &Psk {
        &self.psk
    }

    /// First mesh contact.
    pub fn contact(&self) -> &MeshContact {
        &self.contact
    }
}

/// The standard [`BundleIssuer`]: signs a membership receipt and delivers the
/// current trust-domain PSK and mesh contact. Local and bounded.
pub struct MembershipIssuer {
    identity: Identity,
    psk: Psk,
    contact: MeshContact,
    subnet: Option<SubnetLeafIssuer>,
    org: Option<std::sync::Arc<dyn OrgCertSource>>,
    channels: Vec<crate::channel_issuer::ChannelLeafIssuer>,
}

/// Delegated subnet leaf issuance for [`Relation::Subnet`] invites: an
/// issuer key under a root-signed `SubnetIssuerGrant`. The root never sits
/// on the enrollment node; a leaf can never exceed the grant's envelope.
#[derive(Clone)]
pub struct SubnetLeafIssuer {
    grant: net::adapter::net::subnet::SubnetIssuerGrant,
    key: net::adapter::net::identity::EntityKeypair,
    generation: u32,
    lifetime_secs: u64,
}

impl core::fmt::Debug for SubnetLeafIssuer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SubnetLeafIssuer")
            .field("issuer", &fingerprint(&self.grant.issuer))
            .field("scope", &self.grant.scope)
            .field("generation", &self.generation)
            .finish()
    }
}

impl SubnetLeafIssuer {
    /// Issue leaves as `key` under `grant` (the key must be the grant's
    /// issuer), at `generation`, each valid for at most `lifetime_secs`
    /// and never beyond the grant.
    pub fn new(
        grant: net::adapter::net::subnet::SubnetIssuerGrant,
        key: net::adapter::net::identity::EntityKeypair,
        generation: u32,
        lifetime_secs: u64,
    ) -> Result<Self, &'static str> {
        if key.entity_id() != &grant.issuer {
            return Err("the issuer key is not the issuer the grant names");
        }
        if lifetime_secs == 0 {
            return Err("leaf lifetime must be positive");
        }
        Ok(Self {
            grant,
            key,
            generation,
            lifetime_secs,
        })
    }

    /// Whether `offer` lies inside this issuer's envelope (authority,
    /// epoch, scope within the grant's subtree, rights within its ceiling).
    pub fn covers(&self, offer: &super::invite::SubnetOffer) -> bool {
        offer.scope.authority == self.grant.authority
            && offer.topology_epoch == self.grant.topology_epoch
            && self.grant.scope.is_ancestor_or_self_of(offer.scope.path)
            && self.grant.maximum_rights.contains(offer.rights)
    }

    /// The issuer grant (for display and status).
    pub fn grant(&self) -> &net::adapter::net::subnet::SubnetIssuerGrant {
        &self.grant
    }

    pub(crate) fn issue(
        &self,
        offer: &super::invite::SubnetOffer,
        subject: &EntityId,
        now: u64,
    ) -> Result<net::adapter::net::subnet::SubnetCredentialSet, Refusal> {
        if !self.covers(offer) {
            return Err(Refusal::Unavailable);
        }
        // Back-date the start for clock skew, but count the lifetime from
        // `now`: a short-lived leaf must not be born (nearly) expired.
        let not_before = now.saturating_sub(60).max(self.grant.not_before);
        let not_after = now
            .saturating_add(self.lifetime_secs)
            .min(self.grant.not_after);
        let duration = not_after
            .saturating_sub(not_before)
            .min(net::adapter::net::subnet::auth::MAX_SUBNET_GRANT_LIFETIME_SECS);
        if duration == 0 {
            return Err(Refusal::Unavailable);
        }
        let leaf = net::adapter::net::subnet::SubnetGrant::try_issue(
            &self.key,
            offer.scope.authority.clone(),
            offer.scope.path,
            offer.topology_epoch,
            subject.clone(),
            offer.rights,
            self.generation,
            not_before,
            duration,
        )
        .map_err(|_| Refusal::Unavailable)?;
        Ok(net::adapter::net::subnet::SubnetCredentialSet::OneHop {
            issuer_grant: self.grant.clone(),
            leaf,
        })
    }
}

impl core::fmt::Debug for MembershipIssuer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MembershipIssuer")
            .field("issuer", &fingerprint(self.identity.entity_id()))
            .field("trust_domain", &self.psk.trust_domain())
            .field("contact", &self.contact)
            .finish()
    }
}

impl MembershipIssuer {
    /// Issue as `identity` (the ledger's issuer), delivering `psk` and `contact`.
    pub fn new(identity: Identity, psk: Psk, contact: MeshContact) -> Self {
        Self {
            identity,
            psk,
            contact,
            subnet: None,
            org: None,
            channels: Vec::new(),
        }
    }

    /// Also mint channel chains for [`Relation::Channel`] invites, from
    /// these root grants (one per channel).
    pub fn with_channel_issuers(
        mut self,
        issuers: Vec<crate::channel_issuer::ChannelLeafIssuer>,
    ) -> Self {
        self.channels = issuers;
        self
    }

    /// Deliver operator-approved org membership certificates for
    /// [`Relation::Org`] invites.
    pub fn with_org_certs(mut self, source: std::sync::Arc<dyn OrgCertSource>) -> Self {
        self.org = Some(source);
        self
    }

    /// Also issue subnet credentials for [`Relation::Subnet`] invites.
    pub fn with_subnet_issuer(mut self, issuer: SubnetLeafIssuer) -> Self {
        self.subnet = Some(issuer);
        self
    }

    fn check(&self, invite: &MembershipInvite) -> Result<(), Refusal> {
        if invite.issuer() != self.identity.entity_id() {
            return Err(Refusal::Invalid);
        }
        Ok(())
    }
}

/// Is `cert` a validly signed membership of exactly the offered org, for
/// exactly `subject`? (Its validity window is the adopting node's to check,
/// at adoption and at every use.)
pub fn org_cert_matches(
    offer: &super::invite::OrgOffer,
    subject: &EntityId,
    cert: &net::adapter::net::behavior::org::OrgMembershipCert,
) -> bool {
    cert.org_id == offer.org && &cert.member == subject && cert.verify().is_ok()
}

/// Is `chain` exactly the offered channel credential for `subject`: it
/// anchors at the offered root, verifies link by link for every offered
/// right on the offered channel, and its leaf carries exactly the offered
/// rights (no more, no less) for `subject`?
pub fn channel_chain_matches(
    offer: &super::invite::ChannelOffer,
    subject: &EntityId,
    chain: &net::adapter::net::identity::TokenChain,
) -> bool {
    use net::adapter::net::identity::{RevocationRegistry, TokenScope};
    let Some(leaf) = chain.tokens.last() else {
        return false;
    };
    if &leaf.subject != subject || leaf.scope != offer.rights {
        return false;
    }
    let revocation = RevocationRegistry::new();
    let roots = [offer.root.clone()];
    [TokenScope::PUBLISH, TokenScope::SUBSCRIBE]
        .into_iter()
        .filter(|right| offer.rights.contains(*right))
        .all(|right| {
            chain
                .verify_authorizes(
                    right,
                    offer.channel.hash(),
                    subject,
                    &roots,
                    &revocation,
                    CHANNEL_CLOCK_SKEW_SECS,
                )
                .is_ok()
        })
}

/// Clock skew tolerated when a device checks a freshly minted chain.
const CHANNEL_CLOCK_SKEW_SECS: u64 = 60;

/// Where an issuing node finds the membership certificate the operator
/// signed, with the offline org root, when approving one exact claim.
pub trait OrgCertSource: Send + Sync + 'static {
    /// The certificate approved for `claimant`, if any.
    fn cert_for(
        &self,
        claimant: &super::store::Claimant,
    ) -> Option<net::adapter::net::behavior::org::OrgMembershipCert>;

    /// The org's shared owner audience the operator supplied with that
    /// approval, if any. Delivered with the certificate so the member can
    /// open (and be found in) the org's private announcements.
    fn audience_for(
        &self,
        _claimant: &super::store::Claimant,
    ) -> Option<net::adapter::net::behavior::org_authority::OwnerAudienceCredential> {
        None
    }
}

impl BundleIssuer for MembershipIssuer {
    fn issue(
        &self,
        invite: &MembershipInvite,
        intent: &RedemptionIntent,
    ) -> Result<Vec<u8>, Refusal> {
        self.check(invite)?;
        // Never deliver a PSK from a trust domain other than the one signed.
        invite
            .check_trust_domain(self.psk.trust_domain())
            .map_err(|_: InviteError| Refusal::Unavailable)?;
        let now = now_unix();
        let receipt = MembershipReceipt::sign(&self.identity, invite, intent, now);
        let mut bundle = MembershipBundle::new(receipt, self.psk.clone(), self.contact.clone());
        if let Some(offer) = invite.subnet() {
            // A subnet invite without a configured leaf issuer cannot be
            // honoured; refuse rather than deliver a partial bundle.
            let issuer = self.subnet.as_ref().ok_or(Refusal::Unavailable)?;
            bundle = bundle.with_subnet_credentials(&issuer.issue(offer, intent.subject(), now)?);
        }
        if let Some(offer) = invite.org() {
            // Only the org root signs membership; this node merely delivers
            // what the operator signed for exactly this claim at approval.
            let cert = self
                .org
                .as_ref()
                .and_then(|source| source.cert_for(&intent.claimant()))
                .filter(|cert| org_cert_matches(offer, intent.subject(), cert))
                .ok_or(Refusal::Unavailable)?;
            bundle = bundle.with_org_membership(cert);
            let audience = self
                .org
                .as_ref()
                .and_then(|source| source.audience_for(&intent.claimant()))
                .filter(|a| a.owner_org == offer.org);
            if let Some(audience) = audience {
                bundle = bundle.with_org_audience(&audience);
            }
        }
        if let Some(offer) = invite.channel() {
            // Minted only under a root grant covering exactly this offer;
            // without one the invite cannot be honoured.
            let chain = self
                .channels
                .iter()
                .find(|issuer| issuer.covers(offer))
                .ok_or(Refusal::Unavailable)?
                .issue(intent.subject(), offer.rights)
                .map_err(|_| Refusal::Unavailable)?;
            bundle = bundle.with_channel_chain(&chain);
        }
        Ok(bundle.to_bytes())
    }

    fn may_recover(
        &self,
        invite: &MembershipInvite,
        _intent: &RedemptionIntent,
    ) -> Result<(), Refusal> {
        self.check(invite)?;
        // After a transport-secret rotation the committed bundle carries a
        // stale PSK; refuse rather than silently re-deliver or re-issue.
        invite
            .check_trust_domain(self.psk.trust_domain())
            .map_err(|_| Refusal::RecoveryClosed)
    }
}
