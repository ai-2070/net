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
}

impl core::fmt::Debug for MembershipBundle {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MembershipBundle")
            .field("receipt", &self.receipt)
            .field("psk", &"<redacted>")
            .field("trust_domain", &self.psk.trust_domain())
            .field("contact", &self.contact)
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
        }
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
            .map_err(|_| BundleError::TrustDomain)
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
        }
    }

    fn check(&self, invite: &MembershipInvite) -> Result<(), Refusal> {
        if invite.issuer() != self.identity.entity_id() {
            return Err(Refusal::Invalid);
        }
        Ok(())
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
        let receipt = MembershipReceipt::sign(&self.identity, invite, intent, now_unix());
        Ok(MembershipBundle::new(receipt, self.psk.clone(), self.contact.clone()).to_bytes())
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
