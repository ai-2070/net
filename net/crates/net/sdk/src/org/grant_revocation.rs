// SPDX-License-Identifier: MIT OR Apache-2.0
//! Grant revocation at enforcement points: the grant-side twin of
//! [`super::floors`].
//!
//! A capability grant or dispatcher grant is withdrawn before its
//! `not_after` by an [`OrgGrantRevocationBundle`] its issuing org signs.
//! This module carries one to a *running* node and brings back proof, over
//! the same nRPC service as floors ([`super::floors::ORG_FLOOR_SERVICE`]):
//! the operator names each node ([`OrgGrantRevocationRequest`], fresh
//! nonce); the node merges the bundle into its installed authority's
//! revocation store and answers with an [`OrgGrantRevocationAttestation`]
//! signed by its own entity key over that exact request, stating for every
//! entry whether the node now enforces it. The operator verifies each
//! attestation; a node that did not answer, or whose attestation does not
//! verify, is pending, never applied.
//!
//! Which bundles a node applies is the same rule as for floors: its own
//! org's. A provider is the node that enforces the capability grants its
//! owner org issued, so that is the org whose revocations it must hear.

use net::adapter::net::behavior::org::OrgId;
use net::adapter::net::behavior::org_authority::NodeAuthority;
use net::adapter::net::behavior::org_grant_revocation::{
    OrgGrantRevocationBundle, RevokedGrantKind,
};
use net::adapter::net::behavior::org_revocation::OrgRevocationError;
use net::adapter::net::identity::{EntityId, EntityKeypair};

use super::floors::{now_unix, push_lp, Cursor, ORG_FLOOR_FRESHNESS_SECS};

const REQUEST_MAGIC: [u8; 4] = *b"NOGR";
const ATTESTATION_MAGIC: [u8; 4] = *b"NOGA";
const REQUEST_DIGEST_CONTEXT: &str = "net-mesh org grant revocation request v1";
const ATTESTATION_DOMAIN: &[u8] = b"net-mesh org grant revocation attestation v1";
/// Bound on a request's embedded bundle.
const MAX_REQUEST_BUNDLE_BYTES: usize = 64 * 1024;
/// Bound on attested entries (a request's bundle cannot name more).
const MAX_ATTESTED_ENTRIES: usize = 1024;

/// `true` when `bytes` is a grant-revocation request (by its magic), so
/// a handler serving both floors and grant revocations can dispatch.
pub fn is_grant_revocation_request(bytes: &[u8]) -> bool {
    bytes.starts_with(&REQUEST_MAGIC)
}

/// Apply this org-signed grant-revocation bundle at exactly one named node,
/// and attest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrgGrantRevocationRequest {
    bundle: Vec<u8>,
    verifier: EntityId,
    nonce: [u8; 16],
    issued_at: u64,
}

impl OrgGrantRevocationRequest {
    /// A fresh request (random nonce) for `verifier` to apply `bundle`.
    pub fn new(bundle: &OrgGrantRevocationBundle, verifier: EntityId) -> Result<Self, String> {
        let bundle = bundle.to_bytes();
        if bundle.len() > MAX_REQUEST_BUNDLE_BYTES {
            return Err(format!(
                "the bundle is {} bytes; one request carries at most {MAX_REQUEST_BUNDLE_BYTES}",
                bundle.len()
            ));
        }
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce).map_err(|_| "no randomness".to_string())?;
        Ok(Self {
            bundle,
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
    pub fn bundle(&self) -> Result<OrgGrantRevocationBundle, String> {
        let bundle =
            OrgGrantRevocationBundle::from_bytes(&self.bundle).map_err(|e| e.to_string())?;
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
        let bad = || "malformed org grant revocation request".to_string();
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

/// What the node did with the bundle as a whole.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrgGrantRevocationOutcome {
    /// Applied and persisted.
    Applied,
    /// This node holds no authority for the bundle's org, so it enforces
    /// none of that org's grants.
    NotMember,
    /// Merged and published, but the durable write is uncertain (it may
    /// not survive a restart); not a success.
    Uncertain,
    /// The node refused or failed to apply it.
    Refused,
}

impl OrgGrantRevocationOutcome {
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

/// Where one revoked grant stands on the attesting node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrantEntryState {
    /// The node enforces the revocation.
    Revoked,
    /// The node does not hold the revocation.
    NotRevoked,
    /// The grant had already expired, so there was nothing left to revoke.
    Expired,
}

impl GrantEntryState {
    fn code(self) -> u8 {
        match self {
            Self::Revoked => 1,
            Self::NotRevoked => 0,
            Self::Expired => 2,
        }
    }
    fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            1 => Self::Revoked,
            0 => Self::NotRevoked,
            2 => Self::Expired,
            _ => return None,
        })
    }
    /// Stable lower-case name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Revoked => "revoked",
            Self::NotRevoked => "not_revoked",
            Self::Expired => "expired",
        }
    }
}

/// A node's signed statement of what it did with one
/// [`OrgGrantRevocationRequest`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrgGrantRevocationAttestation {
    request_digest: [u8; 32],
    /// The attesting node.
    pub verifier: EntityId,
    /// What it did.
    pub outcome: OrgGrantRevocationOutcome,
    /// The bundle's org.
    pub org: OrgId,
    /// Each entry the bundle names, and where it stands on this node.
    pub entries: Vec<(RevokedGrantKind, [u8; 32], GrantEntryState)>,
    signature: [u8; 64],
}

impl OrgGrantRevocationAttestation {
    fn body(&self) -> Vec<u8> {
        let mut out = ATTESTATION_MAGIC.to_vec();
        out.extend_from_slice(&self.request_digest);
        out.extend_from_slice(self.verifier.as_bytes());
        out.push(self.outcome.code());
        out.extend_from_slice(&self.org.0);
        out.extend_from_slice(&(self.entries.len() as u32).to_le_bytes());
        for (kind, id, state) in &self.entries {
            out.push(*kind as u8);
            out.extend_from_slice(id);
            out.push(state.code());
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
        let bad = || "malformed org grant revocation attestation".to_string();
        let body_len = bytes.len().checked_sub(64).ok_or_else(bad)?;
        let (body, sig) = bytes.split_at(body_len);
        let mut c = Cursor { buf: body };
        if c.arr::<4>() != Some(ATTESTATION_MAGIC) {
            return Err(bad());
        }
        let request_digest = c.arr::<32>().ok_or_else(bad)?;
        let verifier = EntityId::from_bytes(c.arr::<32>().ok_or_else(bad)?);
        let outcome = OrgGrantRevocationOutcome::from_code(c.arr::<1>().ok_or_else(bad)?[0])
            .ok_or_else(bad)?;
        let org = OrgId(c.arr::<32>().ok_or_else(bad)?);
        let count = c.u32().ok_or_else(bad)? as usize;
        if count > MAX_ATTESTED_ENTRIES {
            return Err(bad());
        }
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            let kind = match c.arr::<1>().ok_or_else(bad)?[0] {
                0x01 => RevokedGrantKind::Capability,
                0x02 => RevokedGrantKind::Dispatcher,
                _ => return Err(bad()),
            };
            let id = c.arr::<32>().ok_or_else(bad)?;
            let state =
                GrantEntryState::from_code(c.arr::<1>().ok_or_else(bad)?[0]).ok_or_else(bad)?;
            entries.push((kind, id, state));
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
            entries,
            signature,
        })
    }

    /// Is this the named node's signed answer to exactly `request`, about
    /// its bundle's org?
    pub fn verify_for(&self, request: &OrgGrantRevocationRequest) -> Result<(), String> {
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

    /// The attested state of one entry, if named.
    pub fn state_of(&self, kind: RevokedGrantKind, id: &[u8; 32]) -> Option<GrantEntryState> {
        self.entries
            .iter()
            .find(|(k, i, _)| *k == kind && i == id)
            .map(|(_, _, s)| *s)
    }
}

/// Node side: apply the requested grant revocations to this node's
/// installed authority (if any) and sign what happened. Refuses a stale
/// request or one addressed to another node; a bundle that does not verify
/// is attested `Refused`.
pub fn answer_org_grant_revocation(
    request_bytes: &[u8],
    keypair: &EntityKeypair,
    authority: Option<&NodeAuthority>,
    now: u64,
) -> Result<OrgGrantRevocationAttestation, String> {
    answer_org_grant_revocation_with(request_bytes, keypair, authority, now, None)
}

/// [`answer_org_grant_revocation`], plus `foreign`: how to apply a bundle
/// signed by an org other than this node's own (see
/// [`super::floors::ForeignApply`]). `None` answers it `NotMember`.
pub fn answer_org_grant_revocation_with(
    request_bytes: &[u8],
    keypair: &EntityKeypair,
    authority: Option<&NodeAuthority>,
    now: u64,
    foreign: Option<&super::floors::ForeignApply<'_>>,
) -> Result<OrgGrantRevocationAttestation, String> {
    let request = OrgGrantRevocationRequest::from_bytes(request_bytes)?;
    if request.issued_at > now.saturating_add(ORG_FLOOR_FRESHNESS_SECS)
        || now > request.issued_at.saturating_add(ORG_FLOOR_FRESHNESS_SECS)
    {
        return Err("stale org grant revocation request".to_string());
    }
    if request.verifier() != keypair.entity_id() {
        return Err("this request names another node".to_string());
    }
    let bundle = request.bundle()?;
    let entries = bundle.entries();
    let (outcome, entries) = match authority {
        Some(authority) if bundle.org_id == authority.config.owner_org || foreign.is_some() => {
            let outcome = if bundle.org_id == authority.config.owner_org {
                match authority.revocation.apply_grant_revocations(&bundle) {
                    Ok(_) => OrgGrantRevocationOutcome::Applied,
                    Err(OrgRevocationError::DurabilityUncertain { .. }) => {
                        OrgGrantRevocationOutcome::Uncertain
                    }
                    Err(_) => OrgGrantRevocationOutcome::Refused,
                }
            } else {
                // Another org's revocations (its dispatcher grants, when it
                // is a grantee here): only through the node's own rule.
                match foreign.and_then(|apply| {
                    apply(
                        &net::adapter::net::behavior::org_revocation_relay::RevocationMaterial::grants(
                            &bundle,
                        ),
                    )
                }) {
                    Some(Ok(())) => OrgGrantRevocationOutcome::Applied,
                    Some(Err(_)) => OrgGrantRevocationOutcome::Refused,
                    // Not an org this node takes material from.
                    None => OrgGrantRevocationOutcome::NotMember,
                }
            };
            let view = authority.revocation.snapshot();
            let attested = entries
                .iter()
                .map(|e| {
                    let state = if view.is_grant_revoked(&bundle.org_id, e.kind, &e.grant_id) {
                        GrantEntryState::Revoked
                    } else if now
                        > e.not_after
                            .saturating_add(net::adapter::net::identity::MAX_TOKEN_CLOCK_SKEW_SECS)
                    {
                        GrantEntryState::Expired
                    } else {
                        GrantEntryState::NotRevoked
                    };
                    (e.kind, e.grant_id, state)
                })
                .collect();
            if outcome == OrgGrantRevocationOutcome::NotMember {
                (outcome, Vec::new())
            } else {
                (outcome, attested)
            }
        }
        // No authority, or another org's: this node enforces none of the
        // bundle org's grants.
        _ => (OrgGrantRevocationOutcome::NotMember, Vec::new()),
    };
    let mut attestation = OrgGrantRevocationAttestation {
        request_digest: request.digest(),
        verifier: keypair.entity_id().clone(),
        outcome,
        org: bundle.org_id,
        entries,
        signature: [0u8; 64],
    };
    attestation.signature = keypair
        .try_sign(&attestation.message())
        .map_err(|e| e.to_string())?
        .to_bytes();
    Ok(attestation)
}

/// Ask `verifier_node` to apply `request` and return its attestation,
/// decoded but not yet verified (call
/// [`OrgGrantRevocationAttestation::verify_for`]).
#[cfg(feature = "cortex")]
pub async fn request_org_grant_revocation(
    node: &std::sync::Arc<net::adapter::net::MeshNode>,
    verifier_node: u64,
    request: &OrgGrantRevocationRequest,
    timeout: std::time::Duration,
) -> Result<OrgGrantRevocationAttestation, String> {
    let opts = net::adapter::net::mesh_rpc::CallOptions {
        deadline: Some(std::time::Instant::now() + timeout),
        ..Default::default()
    };
    let reply = node
        .call(
            verifier_node,
            super::floors::ORG_FLOOR_SERVICE,
            bytes::Bytes::from(request.to_bytes()),
            opts,
        )
        .await
        .map_err(|e| e.to_string())?;
    OrgGrantRevocationAttestation::from_bytes(&reply.body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use net::adapter::net::behavior::org::OrgKeypair;
    use net::adapter::net::behavior::org_grant_revocation::RevokedGrant;

    fn bundle() -> OrgGrantRevocationBundle {
        let org = OrgKeypair::generate();
        OrgGrantRevocationBundle::try_issue(
            &org,
            [RevokedGrant {
                kind: RevokedGrantKind::Capability,
                grant_id: [7u8; 32],
                not_after: now_unix() + 3_600,
            }],
        )
        .expect("bundle")
    }

    #[test]
    fn request_round_trips_and_is_distinguishable_from_a_floor_request() {
        let node = EntityKeypair::generate();
        let request =
            OrgGrantRevocationRequest::new(&bundle(), node.entity_id().clone()).expect("request");
        let bytes = request.to_bytes();
        assert!(is_grant_revocation_request(&bytes));
        assert_eq!(
            OrgGrantRevocationRequest::from_bytes(&bytes).expect("decode"),
            request
        );
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(OrgGrantRevocationRequest::from_bytes(&trailing).is_err());
        assert!(super::super::floors::OrgFloorRequest::from_bytes(&bytes).is_err());
    }

    #[test]
    fn a_node_without_authority_attests_not_member() {
        let node = EntityKeypair::generate();
        let request =
            OrgGrantRevocationRequest::new(&bundle(), node.entity_id().clone()).expect("request");
        let attestation = answer_org_grant_revocation(&request.to_bytes(), &node, None, now_unix())
            .expect("answer");
        assert_eq!(attestation.outcome, OrgGrantRevocationOutcome::NotMember);
        attestation.verify_for(&request).expect("verifies");
        let decoded =
            OrgGrantRevocationAttestation::from_bytes(&attestation.to_bytes()).expect("decode");
        assert_eq!(decoded, attestation);
    }

    #[test]
    fn stale_and_misaddressed_requests_are_refused() {
        let node = EntityKeypair::generate();
        let request =
            OrgGrantRevocationRequest::new(&bundle(), node.entity_id().clone()).expect("request");
        let late = now_unix() + ORG_FLOOR_FRESHNESS_SECS + 10;
        assert!(answer_org_grant_revocation(&request.to_bytes(), &node, None, late).is_err());
        let other = EntityKeypair::generate();
        assert!(
            answer_org_grant_revocation(&request.to_bytes(), &other, None, now_unix()).is_err()
        );
    }
}
