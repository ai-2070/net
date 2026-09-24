// SPDX-License-Identifier: MIT OR Apache-2.0
//! Standalone subnet and organization joins (NET_CLI_PLAN_V3 V3-2, task 3
//! and the org half).
//!
//! A device already on the mesh redeems a subnet-only link (an invite whose
//! relations are exactly [`Relation::Subnet`]) or an org-only link (exactly
//! [`Relation::Org`]) over its existing session,
//! instead of the enrollment endpoint: no PSK is delivered and no transport
//! is set up. The device signs a [`SubnetRedeemRequest`] over the invite, the
//! destination node and its issue time. The issuing node
//! ([`answer_subnet_redeem`]) issues only when:
//!
//! - the request is fresh, names this node, and verifies under the device key;
//! - the session that delivered it has proven that same entity (checked by
//!   the serving handler, which passes the proven entity in);
//! - the invite is subnet-only, this node's own, and recorded in its ledger;
//! - the ledger's claim for this device is ready (preauthorized, or approved
//!   by the operator — otherwise the reply is [`SubnetRedeemReply::PendingApproval`]
//!   and the device asks again later);
//! - (checked by the handler) no subject floor at this node removes the
//!   device from the offered scope.
//!
//! The device keeps each membership in its own [`SubnetMembership`] store and
//! checks the credentials against the signed offer before persisting them.
//! Renewal ([`super::renew`]) works unchanged for these invites.
//!
//! An org-only link delivers the membership certificate the operator signed
//! for this exact claim at approval ([`super::org`]); the device adopts it.
//!
//! [`Relation::Subnet`]: super::invite::Relation::Subnet
//! [`Relation::Org`]: super::invite::Relation::Org

use std::path::Path;

use net::adapter::net::behavior::enrollment_storage::{EnrollmentStorage, StorageError};
use net::adapter::net::subnet::SubnetCredentialSet;

use super::bundle::{org_cert_matches, OrgCertSource, SubnetLeafIssuer};
use super::device::DeviceJoinError;
use super::invite::{MembershipInvite, RedemptionIntent, Relation, SubnetOffer};
use super::redeem::Refusal;
use super::service::SharedLedger;
use super::store::ClaimOutcome;
use super::Reader;
use crate::identity::{EntityId, Identity};

/// nRPC service the issuing node serves standalone (subnet or org)
/// redemption on.
pub const STANDALONE_REDEEM_SERVICE: &str = "net.enroll.standalone.redeem";
/// The same service, under its original name.
pub const SUBNET_REDEEM_SERVICE: &str = STANDALONE_REDEEM_SERVICE;
/// A redemption request is answered only within this window of its issue time.
pub const REDEEM_FRESHNESS_SECS: u64 = 300;

const MAGIC: [u8; 4] = *b"NMSJ";
const SIGNATURE_DOMAIN: &[u8] = b"net-mesh standalone subnet join v1";

/// Whether `invite` is a standalone subnet link (subnet relation only).
pub fn is_standalone_subnet(invite: &MembershipInvite) -> bool {
    invite.relations() == [Relation::Subnet] && invite.subnet().is_some()
}

/// Whether `invite` is a standalone organization link (org relation only).
pub fn is_standalone_org(invite: &MembershipInvite) -> bool {
    invite.relations() == [Relation::Org] && invite.org().is_some()
}

/// A device's signed request to redeem a standalone subnet link at one node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubnetRedeemRequest {
    invite: Vec<u8>,
    subject: EntityId,
    target_node: u64,
    issued_at: u64,
    nonce: [u8; 16],
    signature: [u8; 64],
}

impl SubnetRedeemRequest {
    /// Sign a request to redeem `invite` at `target_node` as `device`.
    pub fn sign(
        device: &Identity,
        invite: &MembershipInvite,
        target_node: u64,
        issued_at: u64,
    ) -> Result<Self, Refusal> {
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce).map_err(|_| Refusal::Unavailable)?;
        let mut request = Self {
            invite: invite.to_bytes().to_vec(),
            subject: device.entity_id().clone(),
            target_node,
            issued_at,
            nonce,
            signature: [0u8; 64],
        };
        request.signature = device.sign(&request.message());
        Ok(request)
    }

    fn body(&self) -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        super::push_lp(&mut out, &self.invite);
        out.extend_from_slice(self.subject.as_bytes());
        out.extend_from_slice(&self.target_node.to_le_bytes());
        out.extend_from_slice(&self.issued_at.to_le_bytes());
        out.extend_from_slice(&self.nonce);
        out
    }

    fn message(&self) -> Vec<u8> {
        let mut m = SIGNATURE_DOMAIN.to_vec();
        m.extend_from_slice(&self.body());
        m
    }

    /// Wire form: body ‖ signature.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = self.body();
        out.extend_from_slice(&self.signature);
        out
    }

    /// Strict decode; the signature is verified by [`answer_subnet_redeem`].
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Refusal> {
        let body_len = bytes.len().checked_sub(64).ok_or(Refusal::Invalid)?;
        let (body, sig) = bytes.split_at(body_len);
        let mut r = Reader::new(body);
        if r.take_arr::<4>() != Some(MAGIC) {
            return Err(Refusal::Invalid);
        }
        let invite = r.take_lp().ok_or(Refusal::Invalid)?.to_vec();
        let subject = EntityId::from_bytes(r.take_arr::<32>().ok_or(Refusal::Invalid)?);
        let target_node = r.take_u64().ok_or(Refusal::Invalid)?;
        let issued_at = r.take_u64().ok_or(Refusal::Invalid)?;
        let nonce = r.take_arr::<16>().ok_or(Refusal::Invalid)?;
        if !r.done() {
            return Err(Refusal::Invalid);
        }
        let mut signature = [0u8; 64];
        signature.copy_from_slice(sig);
        Ok(Self {
            invite,
            subject,
            target_node,
            issued_at,
            nonce,
            signature,
        })
    }

    /// The requesting device.
    pub fn subject(&self) -> &EntityId {
        &self.subject
    }

    /// The invite the request is for (decoded and signature-checked).
    pub fn invite(&self) -> Result<MembershipInvite, Refusal> {
        MembershipInvite::from_bytes(&self.invite).map_err(|_| Refusal::Invalid)
    }
}

/// The issuing node's answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubnetRedeemReply {
    /// Credentials for exactly the offer, for this device.
    Issued(Box<SubnetCredentialSet>),
    /// The org membership certificate approved for this device, with the
    /// org's shared owner audience (encoded) when the operator supplied one.
    OrgIssued {
        /// The approved membership certificate.
        cert: Box<net::adapter::net::behavior::org::OrgMembershipCert>,
        /// The org's owner audience (**secret**), if supplied.
        audience: Option<Vec<u8>>,
    },
    /// The link requires operator approval; ask again once approved.
    PendingApproval,
}

impl SubnetRedeemReply {
    /// Wire form: `0 ‖ credential set`, `1`, or
    /// `2 ‖ lp(membership certificate) ‖ u8 has_audience [‖ audience]`.
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            Self::Issued(set) => {
                let mut out = vec![0u8];
                out.extend_from_slice(&set.to_bytes());
                out
            }
            Self::PendingApproval => vec![1u8],
            Self::OrgIssued { cert, audience } => {
                let mut out = vec![2u8];
                super::push_lp(&mut out, &cert.to_bytes());
                match audience {
                    Some(a) => {
                        out.push(1);
                        out.extend_from_slice(a);
                    }
                    None => out.push(0),
                }
                out
            }
        }
    }

    /// Strict decode.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        match bytes.split_first() {
            Some((0, set)) => SubnetCredentialSet::from_bytes(set)
                .map(|set| Self::Issued(Box::new(set)))
                .map_err(|e| format!("subnet credentials: {e}")),
            Some((1, [])) => Ok(Self::PendingApproval),
            Some((2, rest)) => {
                let bad = || "malformed org membership reply".to_string();
                let mut r = Reader::new(rest);
                let cert = net::adapter::net::behavior::org::OrgMembershipCert::from_bytes(
                    r.take_lp().ok_or_else(bad)?,
                )
                .map_err(|e| format!("org membership certificate: {e}"))?;
                let audience = match r.take_arr::<1>().ok_or_else(bad)?[0] {
                    0 => None,
                    1 => {
                        let a = r
                            .take(net::adapter::net::behavior::org_authority::OwnerAudienceCredential::ENCODED_SIZE)
                            .ok_or_else(bad)?
                            .to_vec();
                        net::adapter::net::behavior::org_authority::OwnerAudienceCredential::decode_config(&a).map_err(|_| bad())?;
                        Some(a)
                    }
                    _ => return Err(bad()),
                };
                if !r.done() {
                    return Err(bad());
                }
                Ok(Self::OrgIssued {
                    cert: Box::new(cert),
                    audience,
                })
            }
            _ => Err("malformed standalone subnet reply".to_string()),
        }
    }
}

/// Issuing-node side, for a subnet-only link (see [`answer_standalone_redeem`]).
pub fn answer_subnet_redeem(
    request_bytes: &[u8],
    session_subject: Option<&EntityId>,
    this_node: u64,
    ledger: &SharedLedger,
    issuer: &SubnetLeafIssuer,
    now: u64,
) -> Result<SubnetRedeemReply, Refusal> {
    answer_standalone_redeem(
        request_bytes,
        session_subject,
        this_node,
        ledger,
        Some(issuer),
        None,
        now,
    )
}

/// Issuing-node side. `session_subject` is the entity the delivering session
/// has proven (`None` if it has proven none); `this_node` is this node's id.
/// A subnet-only link needs `subnet`; an org-only link needs `org`, which
/// holds the certificates the operator signed at approval. The caller must
/// additionally refuse a subject its own floors removed.
pub fn answer_standalone_redeem(
    request_bytes: &[u8],
    session_subject: Option<&EntityId>,
    this_node: u64,
    ledger: &SharedLedger,
    subnet: Option<&SubnetLeafIssuer>,
    org: Option<&dyn OrgCertSource>,
    now: u64,
) -> Result<SubnetRedeemReply, Refusal> {
    let request = SubnetRedeemRequest::from_bytes(request_bytes)?;
    if request.issued_at > now.saturating_add(REDEEM_FRESHNESS_SECS)
        || now > request.issued_at.saturating_add(REDEEM_FRESHNESS_SECS)
    {
        return Err(Refusal::Expired);
    }
    request
        .subject
        .verify_bytes(&request.message(), &request.signature)
        .map_err(|_| Refusal::Invalid)?;
    if request.target_node != this_node {
        return Err(Refusal::Invalid);
    }
    // The credential is for the entity proven on the delivering session,
    // not merely for whoever signed: none proven yet is "not now"; a
    // different one is a conflict.
    match session_subject {
        None => return Err(Refusal::Unavailable),
        Some(proven) if proven != &request.subject => return Err(Refusal::Conflict),
        Some(_) => {}
    }
    let invite = request.invite()?;
    if !is_standalone_subnet(&invite) && !is_standalone_org(&invite) {
        return Err(Refusal::Invalid);
    }
    let intent = RedemptionIntent::for_invite(&invite, request.subject.clone())
        .map_err(|_| Refusal::Invalid)?;
    let claimant = intent.claimant();
    let id = invite.invitation_id();
    let mut ledger = ledger.lock();
    if ledger.issuer() != invite.issuer() {
        return Err(Refusal::Invalid);
    }
    if ledger.invite_digest(&id).map_err(super::service::refusal)? != invite.digest() {
        return Err(Refusal::Invalid);
    }
    // What this link delivers, now: fresh subnet credentials, or the
    // certificate the operator approved for exactly this claim.
    let deliver = || -> Result<SubnetRedeemReply, Refusal> {
        if let Some(offer) = invite.subnet() {
            let issuer = subnet.ok_or(Refusal::Unavailable)?;
            return issuer
                .issue(offer, &request.subject, now)
                .map(|set| SubnetRedeemReply::Issued(Box::new(set)));
        }
        let offer = invite.org().ok_or(Refusal::Invalid)?;
        let source = org.ok_or(Refusal::Unavailable)?;
        let cert = source
            .cert_for(&claimant)
            .filter(|cert| org_cert_matches(offer, &request.subject, cert))
            .ok_or(Refusal::Unavailable)?;
        let audience = source
            .audience_for(&claimant)
            .filter(|a| a.owner_org == offer.org)
            .map(|a| a.encode_config().to_vec());
        Ok(SubnetRedeemReply::OrgIssued {
            cert: Box::new(cert),
            audience,
        })
    };
    match ledger
        .claim(&id, &claimant, now)
        .map_err(super::service::refusal)?
    {
        ClaimOutcome::PendingApproval => Ok(SubnetRedeemReply::PendingApproval),
        ClaimOutcome::Ready => {
            let reply = deliver()?;
            let payload = match &reply {
                SubnetRedeemReply::Issued(set) => set.to_bytes(),
                SubnetRedeemReply::OrgIssued { cert, .. } => cert.to_bytes(),
                SubnetRedeemReply::PendingApproval => return Err(Refusal::Unavailable),
            };
            ledger
                .issue(&id, &claimant, &payload, now)
                .map_err(super::service::refusal)?;
            Ok(reply)
        }
        // Issued before (a lost reply, or a restarted device). The ledger
        // answers `AlreadyIssued` only to the identical claimant, so this is
        // the same device: it gets fresh subnet credentials for the same
        // offer (as renewal would), or the same approved certificate. Any
        // other claimant was refused by `claim` above.
        ClaimOutcome::AlreadyIssued(_) => {
            drop(ledger);
            deliver()
        }
    }
}

/// Serve standalone subnet redemption only (see [`serve_standalone_redeem`]).
#[cfg(feature = "cortex")]
pub fn serve_subnet_redeem(
    node: &std::sync::Arc<net::adapter::net::MeshNode>,
    ledger: SharedLedger,
    issuer: SubnetLeafIssuer,
) -> Result<net::adapter::net::mesh_rpc::ServeHandle, net::adapter::net::mesh_rpc::ServeError> {
    serve_standalone_redeem(node, ledger, Some(issuer), None)
}

/// Serve standalone redemption on [`STANDALONE_REDEEM_SERVICE`]: bind the
/// request to the entity the delivering session proved, answer with
/// [`answer_standalone_redeem`], and refuse a subject this node's own subnet
/// floors removed from an offered subnet scope. Subnet-only links need
/// `subnet`, org-only links `org`. Drop the handle to stop serving.
#[cfg(feature = "cortex")]
pub fn serve_standalone_redeem(
    node: &std::sync::Arc<net::adapter::net::MeshNode>,
    ledger: SharedLedger,
    subnet: Option<SubnetLeafIssuer>,
    org: Option<std::sync::Arc<dyn OrgCertSource>>,
) -> Result<net::adapter::net::mesh_rpc::ServeHandle, net::adapter::net::mesh_rpc::ServeError> {
    node.serve_rpc(
        STANDALONE_REDEEM_SERVICE,
        std::sync::Arc::new(RedeemHandler {
            node: std::sync::Arc::downgrade(node),
            ledger,
            subnet,
            org,
        }),
    )
}

#[cfg(feature = "cortex")]
struct RedeemHandler {
    node: std::sync::Weak<net::adapter::net::MeshNode>,
    ledger: SharedLedger,
    subnet: Option<SubnetLeafIssuer>,
    org: Option<std::sync::Arc<dyn OrgCertSource>>,
}

#[cfg(feature = "cortex")]
impl RedeemHandler {
    fn answer(&self, session_peer: u64, body: &[u8]) -> Result<Vec<u8>, Refusal> {
        let node = self.node.upgrade().ok_or(Refusal::Unavailable)?;
        let request = SubnetRedeemRequest::from_bytes(body)?;
        let invite = request.invite()?;
        // A delegated leaf never satisfies a subject floor: a removed device
        // gets nothing it could not use anyway, and nothing it should have.
        if let Some(offer) = invite.subnet() {
            if node.subnet_floor_registry().subject_refuses(
                &offer.scope.authority,
                offer.topology_epoch,
                request.subject(),
                offer.scope.path,
                offer.rights,
                0,
                true,
            ) {
                return Err(Refusal::Revoked);
            }
        }
        let proven = node
            .peer_identity_established(session_peer)
            .then(|| node.peer_entity_id(session_peer))
            .flatten();
        answer_standalone_redeem(
            body,
            proven.as_ref(),
            node.node_id(),
            &self.ledger,
            self.subnet.as_ref(),
            self.org.as_deref(),
            super::now_unix(),
        )
        .map(|reply| reply.to_bytes())
    }
}

#[cfg(feature = "cortex")]
#[async_trait::async_trait]
impl net::adapter::net::cortex::rpc::RpcHandler for RedeemHandler {
    async fn call(
        &self,
        ctx: net::adapter::net::cortex::rpc::RpcContext,
    ) -> Result<
        net::adapter::net::cortex::rpc::RpcResponsePayload,
        net::adapter::net::cortex::rpc::RpcHandlerError,
    > {
        use net::adapter::net::cortex::rpc::{RpcResponsePayload, RpcStatus};
        Ok(match self.answer(ctx.session_peer, &ctx.payload.body) {
            Ok(reply) => RpcResponsePayload {
                status: RpcStatus::Ok,
                headers: Vec::new(),
                body: bytes::Bytes::from(reply),
            },
            Err(refusal) => RpcResponsePayload {
                status: RpcStatus::Unauthorized,
                headers: Vec::new(),
                body: bytes::Bytes::from(format!("subnet join refused: {refusal}")),
            },
        })
    }
}

/// Device side: redeem the standalone subnet link `invite` at `issuer_node`
/// over this node's session, signed as `device`. The reply's credentials
/// are not yet checked against the offer — [`SubnetMembership::install`]
/// does that before persisting.
#[cfg(feature = "cortex")]
pub async fn request_subnet_redeem(
    node: &std::sync::Arc<net::adapter::net::MeshNode>,
    issuer_node: u64,
    device: &Identity,
    invite: &MembershipInvite,
    timeout: std::time::Duration,
) -> Result<SubnetRedeemReply, String> {
    // The issuer binds the credential to the entity this session has
    // proven; prove it first (a cheap no-op once established).
    node.prove_identity_to(issuer_node)
        .await
        .map_err(|e| format!("proving this device's identity to the issuer: {e}"))?;
    let request = SubnetRedeemRequest::sign(device, invite, issuer_node, super::now_unix())
        .map_err(|e| e.to_string())?;
    let opts = net::adapter::net::mesh_rpc::CallOptions {
        deadline: Some(std::time::Instant::now() + timeout),
        ..Default::default()
    };
    let reply = node
        .call(
            issuer_node,
            SUBNET_REDEEM_SERVICE,
            bytes::Bytes::from(request.to_bytes()),
            opts,
        )
        .await
        .map_err(|e| e.to_string())?;
    SubnetRedeemReply::from_bytes(&reply.body)
}

// ---- device-side membership store ------------------------------------------

const STORE_MAGIC: [u8; 4] = *b"NMSM";
const STORE_VERSION: u16 = 1;
const STORE_CHECKSUM: &str = "net-mesh standalone subnet membership v1";

/// One standalone subnet membership held by a device: the signed link, the
/// node it was redeemed at, and (once issued) its credentials. Durable and
/// owner-locked like the join state; a left membership keeps its record but
/// no credentials.
pub struct SubnetMembership {
    storage: EnrollmentStorage,
    invite: MembershipInvite,
    subject: EntityId,
    issuer_node: u64,
    credentials: Option<SubnetCredentialSet>,
    left_at: Option<u64>,
}

impl SubnetMembership {
    /// Record the intent to redeem `invite` (a standalone subnet link) at
    /// `issuer_node` as `subject`, before any redemption attempt. Refuses a
    /// directory that already exists.
    pub fn begin(
        dir: &Path,
        invite: &MembershipInvite,
        subject: EntityId,
        issuer_node: u64,
    ) -> Result<Self, DeviceJoinError> {
        if !is_standalone_subnet(invite) {
            return Err(DeviceJoinError::Corrupt);
        }
        let storage =
            EnrollmentStorage::create(dir, &encode(invite, &subject, issuer_node, None, None))?;
        Ok(Self {
            storage,
            invite: invite.clone(),
            subject,
            issuer_node,
            credentials: None,
            left_at: None,
        })
    }

    /// Open and validate a stored membership (taking its owner lock).
    pub fn open(dir: &Path) -> Result<Self, DeviceJoinError> {
        let storage = EnrollmentStorage::open(dir)?;
        let (invite, subject, issuer_node, credentials, left_at) = decode(&storage.read()?)?;
        if let Some(set) = &credentials {
            if !matches_offer(
                invite.subnet().ok_or(DeviceJoinError::Corrupt)?,
                &subject,
                set,
            ) {
                return Err(DeviceJoinError::Corrupt);
            }
        }
        Ok(Self {
            storage,
            invite,
            subject,
            issuer_node,
            credentials,
            left_at,
        })
    }

    /// Install (or replace, on renewal) credentials after checking they are
    /// exactly what the signed offer names, for this device. Refused once
    /// the membership has been left.
    pub fn install(&mut self, set: &SubnetCredentialSet) -> Result<(), DeviceJoinError> {
        if let Some(at) = self.left_at {
            return Err(DeviceJoinError::Left { at });
        }
        let offer = self.invite.subnet().ok_or(DeviceJoinError::Corrupt)?;
        if !matches_offer(offer, &self.subject, set) {
            return Err(DeviceJoinError::Bundle(
                super::bundle::BundleError::Mismatch("subnet credentials"),
            ));
        }
        self.storage.replace(&encode(
            &self.invite,
            &self.subject,
            self.issuer_node,
            Some(set),
            None,
        ))?;
        self.credentials = Some(set.clone());
        Ok(())
    }

    /// Leave: durably erase the credentials and record the departure.
    /// Idempotent; returns `false` if already left.
    pub fn leave(&mut self, now: u64) -> Result<bool, DeviceJoinError> {
        if self.left_at.is_some() {
            return Ok(false);
        }
        self.storage.replace(&encode(
            &self.invite,
            &self.subject,
            self.issuer_node,
            None,
            Some(now),
        ))?;
        self.credentials = None;
        self.left_at = Some(now);
        Ok(true)
    }

    /// The signed standalone link.
    pub fn invite(&self) -> &MembershipInvite {
        &self.invite
    }

    /// The offer the link carries.
    pub fn offer(&self) -> Option<&SubnetOffer> {
        self.invite.subnet()
    }

    /// The device this membership is for.
    pub fn subject(&self) -> &EntityId {
        &self.subject
    }

    /// The node the link is redeemed and renewed at.
    pub fn issuer_node(&self) -> u64 {
        self.issuer_node
    }

    /// Installed credentials, if issued and not left.
    pub fn credentials(&self) -> Option<&SubnetCredentialSet> {
        self.credentials.as_ref()
    }

    /// When the membership was left, if it was.
    pub fn left_at(&self) -> Option<u64> {
        self.left_at
    }
}

impl std::fmt::Debug for SubnetMembership {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubnetMembership")
            .field("invitation", &self.invite.invitation_id())
            .field("issuer_node", &self.issuer_node)
            .field("installed", &self.credentials.is_some())
            .field("left_at", &self.left_at)
            .finish()
    }
}

/// Are `set`'s leaf fields exactly the offer's, for `subject`?
fn matches_offer(offer: &SubnetOffer, subject: &EntityId, set: &SubnetCredentialSet) -> bool {
    let leaf = set.leaf();
    leaf.authority == offer.scope.authority
        && leaf.scope == offer.scope.path
        && leaf.topology_epoch == offer.topology_epoch
        && leaf.rights == offer.rights
        && &leaf.subject == subject
}

// MAGIC | u16 VERSION | lp invite | subject[32] | u64 issuer_node |
// u8 has_set [lp set] | u8 left [u64 left_at] | blake3 checksum[32].
fn encode(
    invite: &MembershipInvite,
    subject: &EntityId,
    issuer_node: u64,
    set: Option<&SubnetCredentialSet>,
    left_at: Option<u64>,
) -> Vec<u8> {
    let mut out = STORE_MAGIC.to_vec();
    out.extend_from_slice(&STORE_VERSION.to_le_bytes());
    super::push_lp(&mut out, invite.to_bytes());
    out.extend_from_slice(subject.as_bytes());
    out.extend_from_slice(&issuer_node.to_le_bytes());
    match set {
        Some(set) => {
            out.push(1);
            super::push_lp(&mut out, &set.to_bytes());
        }
        None => out.push(0),
    }
    match left_at {
        Some(at) => {
            out.push(1);
            out.extend_from_slice(&at.to_le_bytes());
        }
        None => out.push(0),
    }
    let sum = blake3::derive_key(STORE_CHECKSUM, &out);
    out.extend_from_slice(&sum);
    out
}

type Decoded = (
    MembershipInvite,
    EntityId,
    u64,
    Option<SubnetCredentialSet>,
    Option<u64>,
);

fn decode(bytes: &[u8]) -> Result<Decoded, DeviceJoinError> {
    let corrupt = DeviceJoinError::Corrupt;
    let body_len = bytes
        .len()
        .checked_sub(32)
        .ok_or(DeviceJoinError::Corrupt)?;
    let (body, sum) = bytes.split_at(body_len);
    if blake3::derive_key(STORE_CHECKSUM, body) != sum {
        return Err(corrupt);
    }
    let mut r = Reader::new(body);
    if r.take_arr::<4>() != Some(STORE_MAGIC) || r.take_u16() != Some(STORE_VERSION) {
        return Err(DeviceJoinError::Corrupt);
    }
    let invite = MembershipInvite::from_bytes(r.take_lp().ok_or(DeviceJoinError::Corrupt)?)
        .map_err(|_| DeviceJoinError::Corrupt)?;
    if !is_standalone_subnet(&invite) {
        return Err(DeviceJoinError::Corrupt);
    }
    let subject = EntityId::from_bytes(r.take_arr::<32>().ok_or(DeviceJoinError::Corrupt)?);
    let issuer_node = r.take_u64().ok_or(DeviceJoinError::Corrupt)?;
    let set = match r.take_arr::<1>().ok_or(DeviceJoinError::Corrupt)? {
        [0] => None,
        [1] => Some(
            SubnetCredentialSet::from_bytes(r.take_lp().ok_or(DeviceJoinError::Corrupt)?)
                .map_err(|_| DeviceJoinError::Corrupt)?,
        ),
        _ => return Err(DeviceJoinError::Corrupt),
    };
    let left_at = match r.take_arr::<1>().ok_or(DeviceJoinError::Corrupt)? {
        [0] => None,
        [1] => Some(r.take_u64().ok_or(DeviceJoinError::Corrupt)?),
        _ => return Err(DeviceJoinError::Corrupt),
    };
    if !r.done() || (set.is_some() && left_at.is_some()) {
        return Err(DeviceJoinError::Corrupt);
    }
    Ok((invite, subject, issuer_node, set, left_at))
}

/// Opening a store refuses one whose storage layer reports it busy; exposed
/// so callers can tell "held by another process" apart from corruption.
pub fn is_busy(e: &DeviceJoinError) -> bool {
    matches!(e, DeviceJoinError::Storage(StorageError::Busy))
}
