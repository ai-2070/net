// SPDX-License-Identifier: MIT OR Apache-2.0
//! Signed membership observations from named nodes (NET_CLI_PLAN_V3 V3-3).
//!
//! "Who is admitted here?" asked of a *remote* enforcement point. The asker
//! must hold the authority whose inventory it reads: a [`MembersRequest`] is
//! signed by the subnet authority root (one the node trusts for that
//! authority) or by the org root itself, names exactly one node, carries a
//! nonce, and is answered only within [`MEMBERS_FRESHNESS_SECS`] of its issue
//! time. The node answers with a [`MembersObservation`] signed by its own
//! entity key over that exact request:
//!
//! - subnet: the peers admitted to the scope (and inside it) at that node
//!   right now, live sessions only ([`net::adapter::net::MeshNode::admitted_subnet_peers`]);
//! - org: each named member's floor at that node, if it enforces the org.
//!
//! An observation is what ONE node saw at ONE time — never a roster.

use net::adapter::net::behavior::org::OrgId;
use net::adapter::net::identity::{EntityId, EntityKeypair};
use net::adapter::net::subnet::{SubnetRights, TopologySubnetId};

/// nRPC service every node serves membership observations on.
pub const MEMBERS_SERVICE: &str = "net.members.observe";
/// A request is answered only within this window of its issue time.
pub const MEMBERS_FRESHNESS_SECS: u64 = 300;
const MAX_SUBJECTS: usize = 1024;

const REQUEST_MAGIC: [u8; 4] = *b"NMOQ";
const OBSERVATION_MAGIC: [u8; 4] = *b"NMOA";
const REQUEST_DOMAIN: &[u8] = b"net-mesh members request v1";
const OBSERVATION_DOMAIN: &[u8] = b"net-mesh members observation v1";
const DIGEST_CONTEXT: &str = "net-mesh members request digest v1";

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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
}

/// What is being asked about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MembersTarget {
    /// Peers admitted to `scope` (and inside it) under `authority`.
    Subnet {
        /// The subnet authority.
        authority: EntityId,
        /// The scope (its subtree included).
        scope: TopologySubnetId,
    },
    /// The standing of `subjects` in `org`.
    Org {
        /// The organization.
        org: OrgId,
        /// The members to report floors for.
        subjects: Vec<EntityId>,
    },
}

/// A root-signed request for one node's membership observation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MembersRequest {
    target: MembersTarget,
    verifier: EntityId,
    nonce: [u8; 16],
    issued_at: u64,
    signer: EntityId,
    signature: [u8; 64],
}

impl MembersRequest {
    /// Sign a request for `verifier`'s observation of `target` with
    /// `root`: the subnet authority root, or the org root's key
    /// (`EntityKeypair::from_bytes(*org_keypair.secret_bytes())`).
    pub fn sign(
        target: MembersTarget,
        verifier: EntityId,
        root: &EntityKeypair,
    ) -> Result<Self, String> {
        if let MembersTarget::Org { subjects, .. } = &target {
            if subjects.len() > MAX_SUBJECTS {
                return Err("too many subjects".to_string());
            }
        }
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce).map_err(|_| "no randomness".to_string())?;
        let mut request = Self {
            target,
            verifier,
            nonce,
            issued_at: now_unix(),
            signer: root.entity_id().clone(),
            signature: [0u8; 64],
        };
        request.signature = root
            .try_sign(&request.message())
            .map_err(|e| e.to_string())?
            .to_bytes();
        Ok(request)
    }

    fn body(&self) -> Vec<u8> {
        let mut out = REQUEST_MAGIC.to_vec();
        match &self.target {
            MembersTarget::Subnet { authority, scope } => {
                out.push(0);
                out.extend_from_slice(authority.as_bytes());
                out.extend_from_slice(&scope.raw().to_le_bytes());
            }
            MembersTarget::Org { org, subjects } => {
                out.push(1);
                out.extend_from_slice(&org.0);
                out.extend_from_slice(&(subjects.len() as u32).to_le_bytes());
                for s in subjects {
                    out.extend_from_slice(s.as_bytes());
                }
            }
        }
        out.extend_from_slice(self.verifier.as_bytes());
        out.extend_from_slice(&self.nonce);
        out.extend_from_slice(&self.issued_at.to_le_bytes());
        out.extend_from_slice(self.signer.as_bytes());
        out
    }

    fn message(&self) -> Vec<u8> {
        let mut m = REQUEST_DOMAIN.to_vec();
        m.extend_from_slice(&self.body());
        m
    }

    /// Wire form: body ‖ signature.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = self.body();
        out.extend_from_slice(&self.signature);
        out
    }

    /// Strict decode (the signature is checked by [`answer_members`]).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        let bad = || "malformed members request".to_string();
        let body_len = bytes.len().checked_sub(64).ok_or_else(bad)?;
        let (body, sig) = bytes.split_at(body_len);
        let mut c = Cursor { buf: body };
        if c.arr::<4>() != Some(REQUEST_MAGIC) {
            return Err(bad());
        }
        let target = match c.arr::<1>().ok_or_else(bad)?[0] {
            0 => MembersTarget::Subnet {
                authority: EntityId::from_bytes(c.arr::<32>().ok_or_else(bad)?),
                scope: TopologySubnetId::from_raw(
                    c.arr::<4>().map(u32::from_le_bytes).ok_or_else(bad)?,
                ),
            },
            1 => {
                let org = OrgId(c.arr::<32>().ok_or_else(bad)?);
                let n = c.u32().ok_or_else(bad)? as usize;
                if n > MAX_SUBJECTS {
                    return Err(bad());
                }
                let mut subjects = Vec::with_capacity(n);
                for _ in 0..n {
                    subjects.push(EntityId::from_bytes(c.arr::<32>().ok_or_else(bad)?));
                }
                MembersTarget::Org { org, subjects }
            }
            _ => return Err(bad()),
        };
        let verifier = EntityId::from_bytes(c.arr::<32>().ok_or_else(bad)?);
        let nonce = c.arr::<16>().ok_or_else(bad)?;
        let issued_at = c.u64().ok_or_else(bad)?;
        let signer = EntityId::from_bytes(c.arr::<32>().ok_or_else(bad)?);
        if !c.buf.is_empty() {
            return Err(bad());
        }
        let mut signature = [0u8; 64];
        signature.copy_from_slice(sig);
        Ok(Self {
            target,
            verifier,
            nonce,
            issued_at,
            signer,
            signature,
        })
    }

    /// The node asked.
    pub fn verifier(&self) -> &EntityId {
        &self.verifier
    }

    /// What is asked about.
    pub fn target(&self) -> &MembersTarget {
        &self.target
    }

    /// What an observation must be bound to.
    pub fn digest(&self) -> [u8; 32] {
        blake3::derive_key(DIGEST_CONTEXT, &self.to_bytes())
    }
}

/// How the node answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MembersOutcome {
    /// Observed and reported.
    Observed,
    /// This node does not verify for that subnet authority.
    NotVerifier,
    /// This node does not enforce that org.
    NotMember,
}

impl MembersOutcome {
    fn code(self) -> u8 {
        match self {
            Self::Observed => 0,
            Self::NotVerifier => 1,
            Self::NotMember => 2,
        }
    }
    fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            0 => Self::Observed,
            1 => Self::NotVerifier,
            2 => Self::NotMember,
            _ => return None,
        })
    }
    /// Stable lower-case name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Observed => "observed",
            Self::NotVerifier => "not_verifier",
            Self::NotMember => "not_member",
        }
    }
}

/// One peer admitted to a subnet at the observing node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdmittedPeer {
    /// The admitted subject.
    pub subject: EntityId,
    /// Where it is attached.
    pub attachment: TopologySubnetId,
    /// Its rights there.
    pub rights: SubnetRights,
    /// Its credential generation.
    pub generation: u32,
    /// Credential expiry (unix seconds).
    pub expires_at: u64,
}

/// A node's signed observation for one [`MembersRequest`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MembersObservation {
    request_digest: [u8; 32],
    /// The observing node.
    pub verifier: EntityId,
    /// When it observed (unix seconds).
    pub observed_at: u64,
    /// How it answered.
    pub outcome: MembersOutcome,
    /// Subnet: peers admitted to the scope there, now.
    pub admitted: Vec<AdmittedPeer>,
    /// Org: each named member's floor there.
    pub floors: Vec<(EntityId, u32)>,
    signature: [u8; 64],
}

impl MembersObservation {
    fn body(&self) -> Vec<u8> {
        let mut out = OBSERVATION_MAGIC.to_vec();
        out.extend_from_slice(&self.request_digest);
        out.extend_from_slice(self.verifier.as_bytes());
        out.extend_from_slice(&self.observed_at.to_le_bytes());
        out.push(self.outcome.code());
        out.extend_from_slice(&(self.admitted.len() as u32).to_le_bytes());
        for p in &self.admitted {
            out.extend_from_slice(p.subject.as_bytes());
            out.extend_from_slice(&p.attachment.raw().to_le_bytes());
            out.push(p.rights.bits());
            out.extend_from_slice(&p.generation.to_le_bytes());
            out.extend_from_slice(&p.expires_at.to_le_bytes());
        }
        out.extend_from_slice(&(self.floors.len() as u32).to_le_bytes());
        for (s, f) in &self.floors {
            out.extend_from_slice(s.as_bytes());
            out.extend_from_slice(&f.to_le_bytes());
        }
        out
    }

    fn message(&self) -> Vec<u8> {
        let mut m = OBSERVATION_DOMAIN.to_vec();
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
        let bad = || "malformed members observation".to_string();
        let body_len = bytes.len().checked_sub(64).ok_or_else(bad)?;
        let (body, sig) = bytes.split_at(body_len);
        let mut c = Cursor { buf: body };
        if c.arr::<4>() != Some(OBSERVATION_MAGIC) {
            return Err(bad());
        }
        let request_digest = c.arr::<32>().ok_or_else(bad)?;
        let verifier = EntityId::from_bytes(c.arr::<32>().ok_or_else(bad)?);
        let observed_at = c.u64().ok_or_else(bad)?;
        let outcome =
            MembersOutcome::from_code(c.arr::<1>().ok_or_else(bad)?[0]).ok_or_else(bad)?;
        let n = c.u32().ok_or_else(bad)? as usize;
        if n > MAX_SUBJECTS {
            return Err(bad());
        }
        let mut admitted = Vec::with_capacity(n);
        for _ in 0..n {
            admitted.push(AdmittedPeer {
                subject: EntityId::from_bytes(c.arr::<32>().ok_or_else(bad)?),
                attachment: TopologySubnetId::from_raw(
                    c.arr::<4>().map(u32::from_le_bytes).ok_or_else(bad)?,
                ),
                rights: SubnetRights::try_from_bits(c.arr::<1>().ok_or_else(bad)?[0])
                    .map_err(|_| bad())?,
                generation: c.u32().ok_or_else(bad)?,
                expires_at: c.u64().ok_or_else(bad)?,
            });
        }
        let n = c.u32().ok_or_else(bad)? as usize;
        if n > MAX_SUBJECTS {
            return Err(bad());
        }
        let mut floors = Vec::with_capacity(n);
        for _ in 0..n {
            let s = EntityId::from_bytes(c.arr::<32>().ok_or_else(bad)?);
            floors.push((s, c.u32().ok_or_else(bad)?));
        }
        if !c.buf.is_empty() {
            return Err(bad());
        }
        let mut signature = [0u8; 64];
        signature.copy_from_slice(sig);
        Ok(Self {
            request_digest,
            verifier,
            observed_at,
            outcome,
            admitted,
            floors,
            signature,
        })
    }

    /// Is this the named node's signed answer to exactly `request`?
    pub fn verify_for(&self, request: &MembersRequest) -> Result<(), String> {
        if &self.verifier != request.verifier() {
            return Err("observed by a different node than the one asked".to_string());
        }
        if self.request_digest != request.digest() {
            return Err("observation is for a different request".to_string());
        }
        self.verifier
            .verify_bytes(&self.message(), &self.signature)
            .map_err(|_| "observation signature does not verify".to_string())
    }
}

fn scope_contains(scope: TopologySubnetId, at: TopologySubnetId) -> bool {
    scope.is_ancestor_or_self_of(at)
}

/// Node side: check the request (fresh, addressed here, signed by a root
/// with authority over the target) and sign what this node observes now.
/// `node` supplies the live views.
pub fn answer_members(
    request_bytes: &[u8],
    node: &net::adapter::net::MeshNode,
    now: u64,
) -> Result<MembersObservation, String> {
    let request = MembersRequest::from_bytes(request_bytes)?;
    if request.issued_at > now.saturating_add(MEMBERS_FRESHNESS_SECS)
        || now > request.issued_at.saturating_add(MEMBERS_FRESHNESS_SECS)
    {
        return Err("stale members request".to_string());
    }
    if request.verifier() != node.entity_id() {
        return Err("this request names another node".to_string());
    }
    request
        .signer
        .verify_bytes(&request.message(), &request.signature)
        .map_err(|_| "members request signature does not verify".to_string())?;
    let (outcome, admitted, floors) = match &request.target {
        MembersTarget::Subnet { authority, scope } => {
            match node.subnet_authority_config(authority) {
                None => (MembersOutcome::NotVerifier, Vec::new(), Vec::new()),
                Some(config) => {
                    if !config.roots.contains(&request.signer) {
                        return Err("the signer is not a root of that subnet authority".to_string());
                    }
                    let admitted = node
                        .admitted_subnet_peers()
                        .into_iter()
                        .filter(|(_, ctx)| {
                            &ctx.authority == authority && scope_contains(*scope, ctx.attachment)
                        })
                        .map(|(_, ctx)| AdmittedPeer {
                            subject: ctx.subject.clone(),
                            attachment: ctx.attachment,
                            rights: ctx.rights,
                            generation: ctx.generation,
                            expires_at: ctx.expires_at,
                        })
                        .take(MAX_SUBJECTS)
                        .collect();
                    (MembersOutcome::Observed, admitted, Vec::new())
                }
            }
        }
        MembersTarget::Org { org, subjects } => {
            if request.signer.as_bytes() != &org.0 {
                return Err("the signer is not that org's root".to_string());
            }
            match node.node_authority().filter(|a| &a.owner_org() == org) {
                None => (MembersOutcome::NotMember, Vec::new(), Vec::new()),
                Some(authority) => {
                    let floors = subjects
                        .iter()
                        .map(|s| (s.clone(), authority.revocation.floor_for(org, s)))
                        .collect();
                    (MembersOutcome::Observed, Vec::new(), floors)
                }
            }
        }
    };
    let keypair = node.entity_keypair();
    let mut observation = MembersObservation {
        request_digest: request.digest(),
        verifier: keypair.entity_id().clone(),
        observed_at: now,
        outcome,
        admitted,
        floors,
        signature: [0u8; 64],
    };
    observation.signature = keypair
        .try_sign(&observation.message())
        .map_err(|e| e.to_string())?
        .to_bytes();
    Ok(observation)
}

/// Serve membership observations on [`MEMBERS_SERVICE`]. Drop the handle
/// to stop serving.
#[cfg(feature = "cortex")]
pub fn serve_members(
    node: &std::sync::Arc<net::adapter::net::MeshNode>,
) -> Result<net::adapter::net::mesh_rpc::ServeHandle, net::adapter::net::mesh_rpc::ServeError> {
    node.serve_rpc(
        MEMBERS_SERVICE,
        std::sync::Arc::new(MembersHandler {
            node: std::sync::Arc::downgrade(node),
        }),
    )
}

#[cfg(feature = "cortex")]
struct MembersHandler {
    node: std::sync::Weak<net::adapter::net::MeshNode>,
}

#[cfg(feature = "cortex")]
#[async_trait::async_trait]
impl net::adapter::net::cortex::rpc::RpcHandler for MembersHandler {
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
            Some(node) => answer_members(&ctx.payload.body, &node, now_unix()),
        };
        Ok(match answered {
            Ok(o) => RpcResponsePayload {
                status: RpcStatus::Ok,
                headers: Vec::new(),
                body: bytes::Bytes::from(o.to_bytes()),
            },
            Err(e) => RpcResponsePayload {
                status: RpcStatus::Unauthorized,
                headers: Vec::new(),
                body: bytes::Bytes::from(format!("members refused: {e}")),
            },
        })
    }
}

/// Ask `verifier_node` for its observation, decoded but not yet verified
/// (call [`MembersObservation::verify_for`]).
#[cfg(feature = "cortex")]
pub async fn request_members(
    node: &std::sync::Arc<net::adapter::net::MeshNode>,
    verifier_node: u64,
    request: &MembersRequest,
    timeout: std::time::Duration,
) -> Result<MembersObservation, String> {
    let opts = net::adapter::net::mesh_rpc::CallOptions {
        deadline: Some(std::time::Instant::now() + timeout),
        ..Default::default()
    };
    let reply = node
        .call(
            verifier_node,
            MEMBERS_SERVICE,
            bytes::Bytes::from(request.to_bytes()),
            opts,
        )
        .await
        .map_err(|e| e.to_string())?;
    MembersObservation::from_bytes(&reply.body)
}
