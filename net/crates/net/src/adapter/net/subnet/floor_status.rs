//! Authenticated subnet floor readback (NET_CLI_PLAN_V3 §6.1a item 6,
//! V3-4 slice 2).
//!
//! An owner commit is not fleet-wide enforcement, so removal reports each
//! enforcement point separately — and only from evidence a relay or last
//! hop cannot forge:
//!
//! - [`FloorStatusRequest`] is signed by an **authority root** and names
//!   exactly one verifier, a fresh nonce and its issue time; it may carry a
//!   subject-floor fact to apply first, which must name the same scope,
//!   epoch and subject.
//! - [`FloorStatusAttestation`] is signed by the **verifier's own entity
//!   key** over the request's digest, and states the apply outcome, the
//!   subject's per-right generations and revision as that verifier now
//!   holds them, and whether its floor store is durable.
//!
//! A verifier answers only a request naming itself, signed by one of its
//! configured roots, within [`FLOOR_STATUS_FRESHNESS_SECS`]. The caller
//! checks the attestation's signature against the verifier it named and
//! the digest against its own request (so its own nonce).

use ed25519_dalek::Signature;

use super::auth::{SubnetAuthError, SubnetRef, SubnetRights, SubnetSubjectFloor};
use super::control::SubnetControlFact;
use super::id::TopologySubnetId;
use crate::adapter::net::identity::{EntityId, EntityKeypair};

/// nRPC service name the verifier serves readback on.
pub const FLOOR_STATUS_SERVICE: &str = "net.subnet.floor";
/// A request is answered only within this many seconds of `issued_at`.
pub const FLOOR_STATUS_FRESHNESS_SECS: u64 = 300;

const REQUEST_SIG_DOMAIN: &[u8] = b"net.subnet.floor-status-request.v1";
const REQUEST_DIGEST_CONTEXT: &str = "net.subnet.floor-status-request.digest.v1";
const ATTESTATION_SIG_DOMAIN: &[u8] = b"net.subnet.floor-status-attestation.v1";
/// The largest fact a request may carry (tag + subject floor).
const MAX_APPLY_BYTES: usize = 1 + SubnetSubjectFloor::WIRE_SIZE;

/// Root-signed readback request (optionally: apply this floor first).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FloorStatusRequest {
    /// Authority-qualified scope queried.
    pub scope: SubnetRef,
    /// Topology epoch queried.
    pub topology_epoch: u32,
    /// The subject whose floor state is asked for.
    pub subject: EntityId,
    /// The one verifier that may answer.
    pub verifier: EntityId,
    /// Caller freshness; bound into the attestation via the digest.
    pub nonce: [u8; 16],
    /// Issue time (unix seconds).
    pub issued_at: u64,
    /// Framed subject-floor fact to apply before answering, if any.
    pub apply: Option<Vec<u8>>,
    /// Signing authority root.
    pub signer: EntityId,
    /// ed25519 over the request domain ‖ body.
    pub signature: [u8; 64],
}

impl FloorStatusRequest {
    /// Sign a request as `root`. When `apply` is given it must be a subject
    /// floor for the same scope, epoch and subject.
    #[expect(
        clippy::too_many_arguments,
        reason = "each parameter is a distinct signed field"
    )]
    pub fn try_issue(
        root: &EntityKeypair,
        scope: SubnetRef,
        topology_epoch: u32,
        subject: EntityId,
        verifier: EntityId,
        nonce: [u8; 16],
        issued_at: u64,
        apply: Option<&SubnetSubjectFloor>,
    ) -> Result<Self, SubnetAuthError> {
        let apply = apply
            .map(|f| {
                if f.scope != scope || f.topology_epoch != topology_epoch || f.subject != subject {
                    return Err(SubnetAuthError::InvalidFormat);
                }
                Ok(SubnetControlFact::SubjectFloor(f.clone()).to_bytes())
            })
            .transpose()?;
        let mut request = Self {
            scope,
            topology_epoch,
            subject,
            verifier,
            nonce,
            issued_at,
            apply,
            signer: root.entity_id().clone(),
            signature: [0u8; 64],
        };
        let sig = root
            .try_sign(&request.signing_input())
            .map_err(|_| SubnetAuthError::InvalidSignature)?;
        request.signature = sig.to_bytes();
        Ok(request)
    }

    fn body(&self) -> Vec<u8> {
        let apply = self.apply.as_deref().unwrap_or(&[]);
        let mut out = Vec::with_capacity(1 + 32 + 4 + 4 + 32 + 32 + 16 + 8 + 32 + 2 + apply.len());
        out.push(1);
        out.extend_from_slice(self.scope.authority.as_bytes());
        out.extend_from_slice(&self.scope.path.raw().to_le_bytes());
        out.extend_from_slice(&self.topology_epoch.to_le_bytes());
        out.extend_from_slice(self.subject.as_bytes());
        out.extend_from_slice(self.verifier.as_bytes());
        out.extend_from_slice(&self.nonce);
        out.extend_from_slice(&self.issued_at.to_le_bytes());
        out.extend_from_slice(self.signer.as_bytes());
        out.extend_from_slice(&(apply.len() as u16).to_le_bytes());
        out.extend_from_slice(apply);
        out
    }

    fn signing_input(&self) -> Vec<u8> {
        let mut m = REQUEST_SIG_DOMAIN.to_vec();
        m.extend_from_slice(&self.body());
        m
    }

    /// Wire form: body ‖ signature.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = self.body();
        out.extend_from_slice(&self.signature);
        out
    }

    /// Strict decode; the signature is NOT verified here.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, SubnetAuthError> {
        let bad = SubnetAuthError::InvalidFormat;
        let fixed = 1 + 32 + 4 + 4 + 32 + 32 + 16 + 8 + 32 + 2;
        if bytes.len() < fixed + 64 {
            return Err(bad);
        }
        let mut off = 0usize;
        let mut take = |n: usize| {
            let s = &bytes[off..off + n];
            off += n;
            s
        };
        if take(1)[0] != 1 {
            return Err(bad);
        }
        let arr32 = |s: &[u8]| -> [u8; 32] { s.try_into().unwrap_or([0; 32]) };
        let authority = EntityId::from_bytes(arr32(take(32)));
        let path =
            TopologySubnetId::from_raw(u32::from_le_bytes(take(4).try_into().unwrap_or([0; 4])));
        let topology_epoch = u32::from_le_bytes(take(4).try_into().unwrap_or([0; 4]));
        let subject = EntityId::from_bytes(arr32(take(32)));
        let verifier = EntityId::from_bytes(arr32(take(32)));
        let nonce: [u8; 16] = take(16).try_into().unwrap_or([0; 16]);
        let issued_at = u64::from_le_bytes(take(8).try_into().unwrap_or([0; 8]));
        let signer = EntityId::from_bytes(arr32(take(32)));
        let apply_len = u16::from_le_bytes(take(2).try_into().unwrap_or([0; 2])) as usize;
        if apply_len > MAX_APPLY_BYTES || bytes.len() != fixed + apply_len + 64 {
            return Err(bad);
        }
        let apply = (apply_len > 0).then(|| take(apply_len).to_vec());
        let signature: [u8; 64] = take(64).try_into().unwrap_or([0; 64]);
        Ok(Self {
            scope: SubnetRef { authority, path },
            topology_epoch,
            subject,
            verifier,
            nonce,
            issued_at,
            apply,
            signer,
            signature,
        })
    }

    /// Verify the signature against `self.signer` (whether the signer is a
    /// configured root is the verifier's decision).
    pub fn verify_signature(&self) -> Result<(), SubnetAuthError> {
        self.signer
            .verify(
                &self.signing_input(),
                &Signature::from_bytes(&self.signature),
            )
            .map_err(|_| SubnetAuthError::InvalidSignature)
    }

    /// Digest binding an attestation to exactly this request (and so to
    /// the caller's nonce).
    pub fn digest(&self) -> [u8; 32] {
        blake3::derive_key(REQUEST_DIGEST_CONTEXT, &self.to_bytes())
    }

    /// The carried fact, decoded, if it is a subject floor for this
    /// request's scope, epoch and subject.
    pub fn apply_fact(&self) -> Result<Option<SubnetSubjectFloor>, SubnetAuthError> {
        let Some(bytes) = &self.apply else {
            return Ok(None);
        };
        match SubnetControlFact::from_bytes(bytes)? {
            SubnetControlFact::SubjectFloor(f)
                if f.scope == self.scope
                    && f.topology_epoch == self.topology_epoch
                    && f.subject == self.subject =>
            {
                Ok(Some(f))
            }
            _ => Err(SubnetAuthError::InvalidFormat),
        }
    }
}

/// What the verifier did with a carried floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloorApplyOutcome {
    /// The request carried no floor (status only).
    NotRequested,
    /// The floor changed this verifier's state.
    Applied,
    /// The floor verified but changed nothing (already held, or stale).
    Unchanged,
    /// The verifier refused the floor; the reason is the stable kind.
    Refused(SubnetAuthError),
}

impl FloorApplyOutcome {
    fn code(self) -> (u8, u8) {
        match self {
            Self::NotRequested => (0, 0xFF),
            Self::Applied => (1, 0xFF),
            Self::Unchanged => (2, 0xFF),
            Self::Refused(e) => (
                3,
                SubnetAuthError::ALL
                    .iter()
                    .position(|k| *k == e)
                    .map(|i| i as u8)
                    .unwrap_or(0xFE),
            ),
        }
    }

    fn from_code(outcome: u8, reason: u8) -> Option<Self> {
        Some(match outcome {
            0 => Self::NotRequested,
            1 => Self::Applied,
            2 => Self::Unchanged,
            3 => Self::Refused(*SubnetAuthError::ALL.get(reason as usize)?),
            _ => return None,
        })
    }

    /// `not_requested` / `applied` / `unchanged` / `refused`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotRequested => "not_requested",
            Self::Applied => "applied",
            Self::Unchanged => "unchanged",
            Self::Refused(_) => "refused",
        }
    }
}

/// Verifier-signed readback answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FloorStatusAttestation {
    /// Digest of the request this answers.
    pub request_digest: [u8; 32],
    /// The answering verifier.
    pub verifier: EntityId,
    /// What happened to a carried floor.
    pub apply: FloorApplyOutcome,
    /// The subject's floor revision held after the answer (0 = none).
    pub revision: u64,
    /// Per-right generations held (ATTACH, ROUTE, EXPORT); 0 = none.
    pub generations: [u32; 3],
    /// Whether this verifier logs floors durably (survives its restart).
    pub persisted: bool,
    /// ed25519 by `verifier` over the attestation domain ‖ body.
    pub signature: [u8; 64],
}

impl FloorStatusAttestation {
    const BODY: usize = 1 + 32 + 32 + 1 + 1 + 8 + 12 + 1;

    fn body(&self) -> [u8; Self::BODY] {
        let mut out = [0u8; Self::BODY];
        let (outcome, reason) = self.apply.code();
        let mut off = 0;
        let mut put = |b: &[u8]| {
            out[off..off + b.len()].copy_from_slice(b);
            off += b.len();
        };
        put(&[1]);
        put(&self.request_digest);
        put(self.verifier.as_bytes());
        put(&[outcome, reason]);
        put(&self.revision.to_le_bytes());
        for g in self.generations {
            put(&g.to_le_bytes());
        }
        put(&[u8::from(self.persisted)]);
        out
    }

    fn signing_input(&self) -> Vec<u8> {
        let mut m = ATTESTATION_SIG_DOMAIN.to_vec();
        m.extend_from_slice(&self.body());
        m
    }

    /// Sign as `verifier`.
    pub fn sign(
        verifier: &EntityKeypair,
        request_digest: [u8; 32],
        apply: FloorApplyOutcome,
        revision: u64,
        generations: [u32; 3],
        persisted: bool,
    ) -> Result<Self, SubnetAuthError> {
        let mut a = Self {
            request_digest,
            verifier: verifier.entity_id().clone(),
            apply,
            revision,
            generations,
            persisted,
            signature: [0u8; 64],
        };
        a.signature = verifier
            .try_sign(&a.signing_input())
            .map_err(|_| SubnetAuthError::InvalidSignature)?
            .to_bytes();
        Ok(a)
    }

    /// Wire form: body ‖ signature.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = self.body().to_vec();
        out.extend_from_slice(&self.signature);
        out
    }

    /// Strict decode; the signature is NOT verified here.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, SubnetAuthError> {
        let bad = SubnetAuthError::InvalidFormat;
        if bytes.len() != Self::BODY + 64 || bytes[0] != 1 {
            return Err(bad);
        }
        let request_digest: [u8; 32] = bytes[1..33].try_into().map_err(|_| bad)?;
        let verifier = EntityId::from_bytes(bytes[33..65].try_into().map_err(|_| bad)?);
        let apply = FloorApplyOutcome::from_code(bytes[65], bytes[66]).ok_or(bad)?;
        let revision = u64::from_le_bytes(bytes[67..75].try_into().map_err(|_| bad)?);
        let mut generations = [0u32; 3];
        for (i, g) in generations.iter_mut().enumerate() {
            let at = 75 + i * 4;
            *g = u32::from_le_bytes(bytes[at..at + 4].try_into().map_err(|_| bad)?);
        }
        let persisted = match bytes[87] {
            0 => false,
            1 => true,
            _ => return Err(bad),
        };
        let signature: [u8; 64] = bytes[Self::BODY..].try_into().map_err(|_| bad)?;
        Ok(Self {
            request_digest,
            verifier,
            apply,
            revision,
            generations,
            persisted,
            signature,
        })
    }

    /// Caller side: accept this attestation only if it is signed by the
    /// verifier `request` named and answers exactly `request`.
    pub fn verify_for(&self, request: &FloorStatusRequest) -> Result<(), SubnetAuthError> {
        if self.verifier != request.verifier {
            return Err(SubnetAuthError::WrongVerifier);
        }
        if self.request_digest != request.digest() {
            return Err(SubnetAuthError::WrongChallenge);
        }
        self.verifier
            .verify(
                &self.signing_input(),
                &Signature::from_bytes(&self.signature),
            )
            .map_err(|_| SubnetAuthError::InvalidSignature)
    }

    /// Whether the held state covers `rights` at `minimum_generation`.
    pub fn covers(&self, rights: SubnetRights, minimum_generation: u32) -> bool {
        [
            SubnetRights::ATTACH,
            SubnetRights::ROUTE,
            SubnetRights::EXPORT,
        ]
        .iter()
        .zip(self.generations)
        .all(|(bit, held)| !rights.contains(*bit) || held >= minimum_generation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(root: &EntityKeypair) -> SubnetRef {
        SubnetRef {
            authority: root.entity_id().clone(),
            path: TopologySubnetId::new(&[3, 7]),
        }
    }

    #[test]
    fn request_and_attestation_round_trip_and_bind_each_other() {
        let root = EntityKeypair::generate();
        let verifier = EntityKeypair::generate();
        let subject = EntityKeypair::generate();
        let floor = SubnetSubjectFloor::try_issue(
            &root,
            scope(&root),
            0,
            subject.entity_id().clone(),
            SubnetRights::ATTACH,
            2,
            1,
            100,
        )
        .unwrap();
        let request = FloorStatusRequest::try_issue(
            &root,
            scope(&root),
            0,
            subject.entity_id().clone(),
            verifier.entity_id().clone(),
            [9; 16],
            100,
            Some(&floor),
        )
        .unwrap();
        let back = FloorStatusRequest::from_bytes(&request.to_bytes()).unwrap();
        assert_eq!(back, request);
        back.verify_signature().unwrap();
        assert_eq!(back.apply_fact().unwrap(), Some(floor));

        let attestation = FloorStatusAttestation::sign(
            &verifier,
            request.digest(),
            FloorApplyOutcome::Refused(SubnetAuthError::InvalidFormat),
            1,
            [2, 0, 0],
            true,
        )
        .unwrap();
        let back = FloorStatusAttestation::from_bytes(&attestation.to_bytes()).unwrap();
        assert_eq!(back, attestation);
        back.verify_for(&request).unwrap();
        assert!(back.covers(SubnetRights::ATTACH, 2));
        assert!(!back.covers(SubnetRights::ATTACH.union(SubnetRights::ROUTE), 2));

        // Bound to the exact request: another nonce does not verify.
        let other = FloorStatusRequest::try_issue(
            &root,
            scope(&root),
            0,
            subject.entity_id().clone(),
            verifier.entity_id().clone(),
            [8; 16],
            100,
            None,
        )
        .unwrap();
        assert_eq!(
            back.verify_for(&other),
            Err(SubnetAuthError::WrongChallenge)
        );
        // Signed by someone else: refused.
        let forged = FloorStatusAttestation::sign(
            &EntityKeypair::generate(),
            request.digest(),
            FloorApplyOutcome::Applied,
            1,
            [2, 0, 0],
            true,
        )
        .unwrap();
        assert_eq!(
            forged.verify_for(&request),
            Err(SubnetAuthError::WrongVerifier)
        );
        let mut tampered = attestation.to_bytes();
        tampered[80] ^= 1;
        let tampered = FloorStatusAttestation::from_bytes(&tampered).unwrap();
        assert_eq!(
            tampered.verify_for(&request),
            Err(SubnetAuthError::InvalidSignature)
        );
    }

    #[test]
    fn a_request_cannot_carry_a_floor_for_another_subject() {
        let root = EntityKeypair::generate();
        let floor = SubnetSubjectFloor::try_issue(
            &root,
            scope(&root),
            0,
            EntityKeypair::generate().entity_id().clone(),
            SubnetRights::ATTACH,
            2,
            1,
            100,
        )
        .unwrap();
        assert_eq!(
            FloorStatusRequest::try_issue(
                &root,
                scope(&root),
                0,
                EntityKeypair::generate().entity_id().clone(),
                EntityKeypair::generate().entity_id().clone(),
                [0; 16],
                100,
                Some(&floor),
            ),
            Err(SubnetAuthError::InvalidFormat)
        );
    }
}
