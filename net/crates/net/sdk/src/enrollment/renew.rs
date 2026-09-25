// SPDX-License-Identifier: MIT OR Apache-2.0
//! Subnet leaf renewal (NET_CLI_PLAN_V3 V3-2, task 5).
//!
//! A device enrolled with a [`Relation::Subnet`] invite holds a delegated leaf
//! that expires. Before it does, the device asks the issuing node for a fresh
//! one with a [`SubnetRenewRequest`] signed by its own identity key over the
//! signed invite, a nonce and its issue time. The issuing node
//! ([`answer_renewal`]) re-issues only when:
//!
//! - the invite is its own and is the exact one recorded in its ledger;
//! - the ledger shows that invite **issued to this same device**;
//! - the invite carries a subnet offer;
//! - the request is fresh and signed by that device;
//! - (checked by the caller) the subject is not removed by a subject floor
//!   at this node — renewal never re-admits a removed device.
//!
//! The fresh leaf covers exactly the original offer; the device checks that
//! again before persisting it ([`super::device::DeviceJoin::replace_subnet_credentials`]).
//!
//! [`Relation::Subnet`]: super::invite::Relation::Subnet

use super::bundle::SubnetLeafIssuer;
use super::invite::MembershipInvite;
use super::redeem::Refusal;
use super::service::SharedLedger;
use super::store::LedgerError;
use super::Reader;
use crate::identity::{EntityId, Identity};

/// nRPC service the issuing node serves renewal on.
pub const SUBNET_RENEW_SERVICE: &str = "net.enroll.subnet.renew";
/// A renewal request is answered only within this window of its issue time.
pub const RENEW_FRESHNESS_SECS: u64 = 300;

const MAGIC: [u8; 4] = *b"NMSR";
const SIGNATURE_DOMAIN: &[u8] = b"net-mesh subnet leaf renewal v1";

/// A device's signed request for a fresh subnet leaf.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubnetRenewRequest {
    invite: Vec<u8>,
    subject: EntityId,
    issued_at: u64,
    nonce: [u8; 16],
    signature: [u8; 64],
}

impl SubnetRenewRequest {
    /// Sign a renewal request for `invite` as `device`.
    pub fn sign(
        device: &Identity,
        invite: &MembershipInvite,
        issued_at: u64,
    ) -> Result<Self, Refusal> {
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce).map_err(|_| Refusal::Unavailable)?;
        let mut request = Self {
            invite: invite.to_bytes().to_vec(),
            subject: device.entity_id().clone(),
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

    /// Strict decode; the signature is verified by [`answer_renewal`].
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Refusal> {
        let body_len = bytes.len().checked_sub(64).ok_or(Refusal::Invalid)?;
        let (body, sig) = bytes.split_at(body_len);
        let mut r = Reader::new(body);
        if r.take_arr::<4>() != Some(MAGIC) {
            return Err(Refusal::Invalid);
        }
        let invite = r.take_lp().ok_or(Refusal::Invalid)?.to_vec();
        let subject = EntityId::from_bytes(r.take_arr::<32>().ok_or(Refusal::Invalid)?);
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
            issued_at,
            nonce,
            signature,
        })
    }

    /// The requesting device.
    pub fn subject(&self) -> &EntityId {
        &self.subject
    }

    /// The invite the request is for, decoded and signature-checked
    /// against its embedded issuer.
    pub fn invite(&self) -> Result<MembershipInvite, Refusal> {
        MembershipInvite::from_bytes(&self.invite).map_err(|_| Refusal::Invalid)
    }
}

/// Issuing-node side: verify a renewal request against this node's ledger
/// and, if it is for a subnet invite issued to that same device, mint a
/// fresh leaf for exactly the original offer. The caller must additionally
/// refuse a subject its own floors have removed.
pub fn answer_renewal(
    request_bytes: &[u8],
    ledger: &SharedLedger,
    issuer: &SubnetLeafIssuer,
    now: u64,
) -> Result<net::adapter::net::subnet::SubnetCredentialSet, Refusal> {
    let request = SubnetRenewRequest::from_bytes(request_bytes)?;
    if request.issued_at > now.saturating_add(RENEW_FRESHNESS_SECS)
        || now > request.issued_at.saturating_add(RENEW_FRESHNESS_SECS)
    {
        return Err(Refusal::Expired);
    }
    request
        .subject
        .verify_bytes(&request.message(), &request.signature)
        .map_err(|_| Refusal::Invalid)?;
    let invite = request.invite()?;
    let offer = invite.subnet().ok_or(Refusal::Invalid)?;
    let ledger = ledger.lock();
    if ledger.issuer() != invite.issuer() {
        return Err(Refusal::Invalid);
    }
    let id = invite.invitation_id();
    if ledger.invite_digest(&id).map_err(|_| Refusal::Invalid)? != invite.digest() {
        return Err(Refusal::Invalid);
    }
    match ledger.issued_subject(&id) {
        Ok(subject) if subject == request.subject => {}
        Ok(_) => return Err(Refusal::Conflict),
        Err(LedgerError::Revoked) => return Err(Refusal::Revoked),
        Err(_) => return Err(Refusal::Invalid),
    }
    drop(ledger);
    issuer.issue(offer, &request.subject, now)
}

/// Serve renewal on [`SUBNET_RENEW_SERVICE`] from an issuing node: verify
/// with [`answer_renewal`], and refuse a subject this node's own floors have
/// removed from the offered scope — renewal never re-admits a removed
/// device. Drop the handle to stop serving.
#[cfg(feature = "cortex")]
pub fn serve_subnet_renewal(
    node: &std::sync::Arc<net::adapter::net::MeshNode>,
    ledger: SharedLedger,
    issuer: SubnetLeafIssuer,
) -> Result<net::adapter::net::mesh_rpc::ServeHandle, net::adapter::net::mesh_rpc::ServeError> {
    node.serve_rpc(
        SUBNET_RENEW_SERVICE,
        std::sync::Arc::new(RenewHandler {
            node: std::sync::Arc::downgrade(node),
            ledger,
            issuer,
        }),
    )
}

#[cfg(feature = "cortex")]
struct RenewHandler {
    node: std::sync::Weak<net::adapter::net::MeshNode>,
    ledger: SharedLedger,
    issuer: SubnetLeafIssuer,
}

#[cfg(feature = "cortex")]
impl RenewHandler {
    fn answer(&self, body: &[u8]) -> Result<Vec<u8>, Refusal> {
        let node = self.node.upgrade().ok_or(Refusal::Unavailable)?;
        let request = SubnetRenewRequest::from_bytes(body)?;
        let invite = request.invite()?;
        let offer = invite.subnet().ok_or(Refusal::Invalid)?;
        // A delegated leaf never satisfies a subject floor, so any floor for
        // this subject covering the offer means renewal is pointless — and
        // handing out fresh credentials to a removed device is refused.
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
        let now = super::now_unix();
        answer_renewal(body, &self.ledger, &self.issuer, now).map(|set| set.to_bytes())
    }
}

#[cfg(feature = "cortex")]
#[async_trait::async_trait]
impl net::adapter::net::cortex::rpc::RpcHandler for RenewHandler {
    async fn call(
        &self,
        ctx: net::adapter::net::cortex::rpc::RpcContext,
    ) -> Result<
        net::adapter::net::cortex::rpc::RpcResponsePayload,
        net::adapter::net::cortex::rpc::RpcHandlerError,
    > {
        use net::adapter::net::cortex::rpc::{RpcResponsePayload, RpcStatus};
        Ok(match self.answer(&ctx.payload.body) {
            Ok(set) => RpcResponsePayload {
                status: RpcStatus::Ok,
                headers: Vec::new(),
                body: bytes::Bytes::from(set),
            },
            Err(refusal) => RpcResponsePayload {
                status: RpcStatus::Unauthorized,
                headers: Vec::new(),
                body: bytes::Bytes::from(format!("renewal refused: {refusal}")),
            },
        })
    }
}

/// Device side: ask `issuer_node` for a fresh subnet leaf for `invite`,
/// signed as `device`. Returns the new credential set (not yet checked
/// against the offer — [`super::device::DeviceJoin::replace_subnet_credentials`]
/// does that before persisting).
#[cfg(feature = "cortex")]
pub async fn request_subnet_renewal(
    node: &std::sync::Arc<net::adapter::net::MeshNode>,
    issuer_node: u64,
    device: &Identity,
    invite: &MembershipInvite,
    timeout: std::time::Duration,
) -> Result<net::adapter::net::subnet::SubnetCredentialSet, String> {
    let request =
        SubnetRenewRequest::sign(device, invite, super::now_unix()).map_err(|e| e.to_string())?;
    let opts = net::adapter::net::mesh_rpc::CallOptions {
        deadline: Some(std::time::Instant::now() + timeout),
        ..Default::default()
    };
    let reply = node
        .call(
            issuer_node,
            SUBNET_RENEW_SERVICE,
            bytes::Bytes::from(request.to_bytes()),
            opts,
        )
        .await
        .map_err(|e| e.to_string())?;
    net::adapter::net::subnet::SubnetCredentialSet::from_bytes(&reply.body)
        .map_err(|e| format!("renewed credentials: subnet:{e}"))
}
