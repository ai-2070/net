// SPDX-License-Identifier: MIT OR Apache-2.0
//! Org membership removal at enforcement points (NET_CLI_PLAN_V3 V3-2 org
//! O2).
//!
//! A membership is revoked by a **floor**: an org-root-signed
//! [`OrgRevocationBundle`] raising `(member, minimum generation)`, which kills
//! every certificate for that member below the floor. Nodes of the org merge
//! floors monotonically, persist them before publishing the live view, and
//! reload them at restart; admission checks every call against that view.
//!
//! This module carries a floor to a *running* node and brings back proof:
//! the operator names each node ([`OrgFloorRequest`], with a fresh nonce);
//! the node applies the bundle to its installed authority's revocation store
//! and answers with an [`OrgFloorAttestation`] signed by its own entity key
//! over that exact request, naming the outcome and its effective floor for
//! every member in the bundle. The operator verifies each attestation
//! ([`OrgFloorAttestation::verify_for`]); a node that did not answer, or
//! whose attestation does not verify, is pending, never applied.
//!
//! A floor bundle authenticates itself (root-signed, and it can only raise),
//! so any node may be asked to apply one by any peer.

use net::adapter::net::behavior::org::{OrgId, OrgRevocationBundle};
use net::adapter::net::behavior::org_authority::NodeAuthority;
use net::adapter::net::behavior::org_revocation::OrgRevocationError;
use net::adapter::net::identity::{EntityId, EntityKeypair};

/// nRPC service every node serves floor application on.
pub const ORG_FLOOR_SERVICE: &str = "net.org.floor.apply";
/// A request is answered only within this window of its issue time.
pub const ORG_FLOOR_FRESHNESS_SECS: u64 = 300;

const REQUEST_MAGIC: [u8; 4] = *b"NOFR";
const ATTESTATION_MAGIC: [u8; 4] = *b"NOFA";
const REQUEST_DIGEST_CONTEXT: &str = "net-mesh org floor request v1";
const ATTESTATION_DOMAIN: &[u8] = b"net-mesh org floor attestation v1";
/// Bound on a request's embedded bundle (the bundle's own cap is larger;
/// removal names few members).
const MAX_REQUEST_BUNDLE_BYTES: usize = 64 * 1024;
const MAX_ATTESTED_FLOORS: usize = 1024;

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn push_lp(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
}

struct Cursor<'a> {
    buf: &'a [u8],
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if n > self.buf.len() {
            return None;
        }
        let (head, tail) = self.buf.split_at(n);
        self.buf = tail;
        Some(head)
    }
    fn arr<const N: usize>(&mut self) -> Option<[u8; N]> {
        self.take(N)?.try_into().ok()
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.arr()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.arr()?))
    }
    fn lp(&mut self, max: usize) -> Option<&'a [u8]> {
        let n = self.u32()? as usize;
        if n > max {
            return None;
        }
        self.take(n)
    }
}

/// Apply this root-signed floor bundle at exactly one named node, and attest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrgFloorRequest {
    bundle: Vec<u8>,
    verifier: EntityId,
    nonce: [u8; 16],
    issued_at: u64,
}

impl OrgFloorRequest {
    /// A fresh request (random nonce) for `verifier` to apply `bundle`.
    pub fn new(bundle: &OrgRevocationBundle, verifier: EntityId) -> Result<Self, String> {
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce).map_err(|_| "no randomness".to_string())?;
        Ok(Self {
            bundle: bundle.to_bytes(),
            verifier,
            nonce,
            issued_at: now_unix(),
        })
    }

    /// The node asked to apply it.
    pub fn verifier(&self) -> &EntityId {
        &self.verifier
    }

    /// The embedded bundle, decoded and signature-checked.
    pub fn bundle(&self) -> Result<OrgRevocationBundle, String> {
        let bundle = OrgRevocationBundle::from_bytes(&self.bundle).map_err(|e| e.to_string())?;
        bundle.verify().map_err(|e| e.to_string())?;
        Ok(bundle)
    }

    /// Wire form.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = REQUEST_MAGIC.to_vec();
        push_lp(&mut out, &self.bundle);
        out.extend_from_slice(self.verifier.as_bytes());
        out.extend_from_slice(&self.nonce);
        out.extend_from_slice(&self.issued_at.to_le_bytes());
        out
    }

    /// Strict decode.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        let bad = || "malformed org floor request".to_string();
        let mut c = Cursor { buf: bytes };
        if c.arr::<4>() != Some(REQUEST_MAGIC) {
            return Err(bad());
        }
        let bundle = c.lp(MAX_REQUEST_BUNDLE_BYTES).ok_or_else(bad)?.to_vec();
        let verifier = EntityId::from_bytes(c.arr::<32>().ok_or_else(bad)?);
        let nonce = c.arr::<16>().ok_or_else(bad)?;
        let issued_at = c.u64().ok_or_else(bad)?;
        if !c.buf.is_empty() {
            return Err(bad());
        }
        Ok(Self {
            bundle,
            verifier,
            nonce,
            issued_at,
        })
    }

    /// What an attestation must be bound to.
    pub fn digest(&self) -> [u8; 32] {
        blake3::derive_key(REQUEST_DIGEST_CONTEXT, &self.to_bytes())
    }
}

/// What the node did with the floor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrgFloorOutcome {
    /// Applied and persisted: the node's floors are at least the bundle's.
    Applied,
    /// This node holds no org authority, so it enforces no org membership.
    NotMember,
    /// Merged and published, but the durable write is uncertain (it may not
    /// survive a restart); not a success.
    Uncertain,
    /// The node refused or failed to apply it.
    Refused,
}

impl OrgFloorOutcome {
    fn code(self) -> u8 {
        match self {
            Self::Applied => 0,
            Self::NotMember => 1,
            Self::Uncertain => 2,
            Self::Refused => 3,
        }
    }
    fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            0 => Self::Applied,
            1 => Self::NotMember,
            2 => Self::Uncertain,
            3 => Self::Refused,
            _ => return None,
        })
    }
    /// Stable lower-case name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::NotMember => "not_member",
            Self::Uncertain => "uncertain",
            Self::Refused => "refused",
        }
    }
}

/// A node's signed statement of what it did with one [`OrgFloorRequest`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrgFloorAttestation {
    request_digest: [u8; 32],
    /// The attesting node.
    pub verifier: EntityId,
    /// What it did.
    pub outcome: OrgFloorOutcome,
    /// The bundle's org.
    pub org: OrgId,
    /// This node's effective floor, after applying, for each member the
    /// bundle names (never lower than the bundle's on `Applied`).
    pub floors: Vec<(EntityId, u32)>,
    signature: [u8; 64],
}

impl OrgFloorAttestation {
    fn body(&self) -> Vec<u8> {
        let mut out = ATTESTATION_MAGIC.to_vec();
        out.extend_from_slice(&self.request_digest);
        out.extend_from_slice(self.verifier.as_bytes());
        out.push(self.outcome.code());
        out.extend_from_slice(&self.org.0);
        out.extend_from_slice(&(self.floors.len() as u32).to_le_bytes());
        for (member, floor) in &self.floors {
            out.extend_from_slice(member.as_bytes());
            out.extend_from_slice(&floor.to_le_bytes());
        }
        out
    }

    fn message(&self) -> Vec<u8> {
        let mut m = ATTESTATION_DOMAIN.to_vec();
        m.extend_from_slice(&self.body());
        m
    }

    /// Wire form: body ‖ signature.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = self.body();
        out.extend_from_slice(&self.signature);
        out
    }

    /// Strict decode (verify with [`Self::verify_for`]).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        let bad = || "malformed org floor attestation".to_string();
        let body_len = bytes.len().checked_sub(64).ok_or_else(bad)?;
        let (body, sig) = bytes.split_at(body_len);
        let mut c = Cursor { buf: body };
        if c.arr::<4>() != Some(ATTESTATION_MAGIC) {
            return Err(bad());
        }
        let request_digest = c.arr::<32>().ok_or_else(bad)?;
        let verifier = EntityId::from_bytes(c.arr::<32>().ok_or_else(bad)?);
        let outcome =
            OrgFloorOutcome::from_code(c.arr::<1>().ok_or_else(bad)?[0]).ok_or_else(bad)?;
        let org = OrgId(c.arr::<32>().ok_or_else(bad)?);
        let count = c.u32().ok_or_else(bad)? as usize;
        if count > MAX_ATTESTED_FLOORS {
            return Err(bad());
        }
        let mut floors = Vec::with_capacity(count);
        for _ in 0..count {
            let member = EntityId::from_bytes(c.arr::<32>().ok_or_else(bad)?);
            floors.push((member, c.u32().ok_or_else(bad)?));
        }
        if !c.buf.is_empty() {
            return Err(bad());
        }
        let mut signature = [0u8; 64];
        signature.copy_from_slice(sig);
        Ok(Self {
            request_digest,
            verifier,
            outcome,
            org,
            floors,
            signature,
        })
    }

    /// Is this the named node's signed answer to exactly `request`, about
    /// its bundle's org?
    pub fn verify_for(&self, request: &OrgFloorRequest) -> Result<(), String> {
        if &self.verifier != request.verifier() {
            return Err("attested by a different node than the one asked".to_string());
        }
        if self.request_digest != request.digest() {
            return Err("attestation is for a different request".to_string());
        }
        if self.org != request.bundle()?.org_id {
            return Err("attestation names a different org".to_string());
        }
        self.verifier
            .verify_bytes(&self.message(), &self.signature)
            .map_err(|_| "attestation signature does not verify".to_string())
    }

    /// The attested effective floor for `member`, if named.
    pub fn floor_of(&self, member: &EntityId) -> Option<u32> {
        self.floors
            .iter()
            .find(|(m, _)| m == member)
            .map(|(_, f)| *f)
    }
}

/// Node side: apply the requested floor to this node's installed authority
/// (if any) and sign what happened. Refuses a stale request or one addressed
/// to another node; a bundle that does not verify is attested `Refused`.
pub fn answer_org_floor(
    request_bytes: &[u8],
    keypair: &EntityKeypair,
    authority: Option<&NodeAuthority>,
    now: u64,
) -> Result<OrgFloorAttestation, String> {
    let request = OrgFloorRequest::from_bytes(request_bytes)?;
    if request.issued_at > now.saturating_add(ORG_FLOOR_FRESHNESS_SECS)
        || now > request.issued_at.saturating_add(ORG_FLOOR_FRESHNESS_SECS)
    {
        return Err("stale org floor request".to_string());
    }
    if request.verifier() != keypair.entity_id() {
        return Err("this request names another node".to_string());
    }
    let bundle = request.bundle()?;
    let members: Vec<EntityId> = bundle.floors().iter().map(|f| f.0.clone()).collect();
    let (outcome, floors) = match authority {
        None => (OrgFloorOutcome::NotMember, Vec::new()),
        Some(authority) => {
            let outcome = match authority.revocation.apply_bundle(&bundle) {
                Ok(_) => OrgFloorOutcome::Applied,
                Err(OrgRevocationError::DurabilityUncertain { .. }) => OrgFloorOutcome::Uncertain,
                Err(_) => OrgFloorOutcome::Refused,
            };
            let floors = members
                .into_iter()
                .map(|m| {
                    let floor = authority.revocation.floor_for(&bundle.org_id, &m);
                    (m, floor)
                })
                .collect();
            (outcome, floors)
        }
    };
    let mut attestation = OrgFloorAttestation {
        request_digest: request.digest(),
        verifier: keypair.entity_id().clone(),
        outcome,
        org: bundle.org_id,
        floors,
        signature: [0u8; 64],
    };
    attestation.signature = keypair
        .try_sign(&attestation.message())
        .map_err(|e| e.to_string())?
        .to_bytes();
    Ok(attestation)
}

/// Serve floor application on [`ORG_FLOOR_SERVICE`]. Drop the handle to
/// stop serving.
#[cfg(feature = "cortex")]
pub fn serve_org_floor(
    node: &std::sync::Arc<net::adapter::net::MeshNode>,
) -> Result<net::adapter::net::mesh_rpc::ServeHandle, net::adapter::net::mesh_rpc::ServeError> {
    node.serve_rpc(
        ORG_FLOOR_SERVICE,
        std::sync::Arc::new(FloorHandler {
            node: std::sync::Arc::downgrade(node),
        }),
    )
}

#[cfg(feature = "cortex")]
struct FloorHandler {
    node: std::sync::Weak<net::adapter::net::MeshNode>,
}

#[cfg(feature = "cortex")]
#[async_trait::async_trait]
impl net::adapter::net::cortex::rpc::RpcHandler for FloorHandler {
    async fn call(
        &self,
        ctx: net::adapter::net::cortex::rpc::RpcContext,
    ) -> Result<
        net::adapter::net::cortex::rpc::RpcResponsePayload,
        net::adapter::net::cortex::rpc::RpcHandlerError,
    > {
        use net::adapter::net::cortex::rpc::{RpcResponsePayload, RpcStatus};
        let answered = match self.node.upgrade() {
            None => Err("node is shutting down".to_string()),
            Some(node) => {
                let authority = node.node_authority();
                let body = ctx.payload.body.clone();
                let keypair = node.entity_keypair_arc();
                // Floor application persists (fsync): off the async runtime.
                tokio::task::spawn_blocking(move || {
                    answer_org_floor(&body, &keypair, authority.as_deref(), now_unix())
                })
                .await
                .unwrap_or_else(|_| Err("org floor task failed".to_string()))
            }
        };
        Ok(match answered {
            Ok(attestation) => RpcResponsePayload {
                status: RpcStatus::Ok,
                headers: Vec::new(),
                body: bytes::Bytes::from(attestation.to_bytes()),
            },
            Err(e) => RpcResponsePayload {
                status: RpcStatus::Unauthorized,
                headers: Vec::new(),
                body: bytes::Bytes::from(format!("org floor refused: {e}")),
            },
        })
    }
}

/// Ask `verifier_node` to apply `request` and return its attestation,
/// decoded but not yet verified (call [`OrgFloorAttestation::verify_for`]).
#[cfg(feature = "cortex")]
pub async fn request_org_floor(
    node: &std::sync::Arc<net::adapter::net::MeshNode>,
    verifier_node: u64,
    request: &OrgFloorRequest,
    timeout: std::time::Duration,
) -> Result<OrgFloorAttestation, String> {
    let opts = net::adapter::net::mesh_rpc::CallOptions {
        deadline: Some(std::time::Instant::now() + timeout),
        ..Default::default()
    };
    let reply = node
        .call(
            verifier_node,
            ORG_FLOOR_SERVICE,
            bytes::Bytes::from(request.to_bytes()),
            opts,
        )
        .await
        .map_err(|e| e.to_string())?;
    OrgFloorAttestation::from_bytes(&reply.body)
}
