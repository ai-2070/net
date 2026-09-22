// SPDX-License-Identifier: MIT OR Apache-2.0
//! Durable, issuer-bound ledger of membership invitations and their receipts.
//!
//! One live owner holds the store's exclusive lifetime lock (via core
//! `EnrollmentStorage`) and applies the transitions offer → claim → (optional
//! approval) → issue, plus revoke/deny and same-claimant receipt recovery.
//! Each mutation builds the next complete snapshot, persists it durably, and only
//! then publishes it in memory and answers.
//!
//! **This ledger is not a verifier.** It does not check invite signatures, device
//! proofs, request intent, issuer permissions or current revocation state. The
//! authenticated redemption owner must verify all of those before calling
//! [`EnrollmentLedger::claim`], [`EnrollmentLedger::issue`] or
//! [`EnrollmentLedger::recover`]; a successful call here is only a durable
//! single-use/ordering decision over already-verified inputs. It never signs,
//! encodes an invite, or delivers credentials by itself.
//!
//! Blocking filesystem I/O; methods take `&mut self`, so the owning service
//! serializes transitions and must not hold that borrow across a human or
//! network wait. After a post-rename durability failure the ledger is fenced
//! ([`LedgerError::Uncertain`]) until it is dropped and reopened. Returned
//! receipt payloads are secret-bearing; no `Debug` output or error carries them.
//! Removing secret bytes from the snapshot is not secure erasure.

use std::path::Path;

use net::adapter::net::behavior::enrollment_storage::{
    EnrollmentStorage, StorageError, MAX_SNAPSHOT_BYTES,
};

use super::policy::{ApprovalMode, InvitationPolicy, PolicyError};
use super::Reader;
use crate::identity::EntityId;

/// Fixed recovery window for an issued receipt, from successful issuance.
/// Independent of the invitation's first-issuance window; retries never extend it.
pub const RECEIPT_RECOVERY_WINDOW_SECS: u64 = 86_400;
/// Absolute record-count ceiling enforced by the decoder and [`LedgerLimits`].
pub const HARD_MAX_RECORDS: usize = 65_536;
/// Absolute per-receipt payload ceiling enforced by the decoder and [`LedgerLimits`].
pub const HARD_MAX_RESULT_BYTES: usize = 1024 * 1024;

const MAGIC: [u8; 4] = *b"NMEL";
const VERSION: u16 = 1;
const CHECKSUM_CONTEXT: &str = "net-mesh enrollment ledger snapshot v1";
const CHECKSUM_LEN: usize = 32;

/// Invitation identifier carried inside the signed link. Sensitive: `Debug` redacts it.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct InvitationId([u8; 16]);

impl InvitationId {
    /// Fresh CSPRNG identifier.
    pub fn random() -> Result<Self, LedgerError> {
        random_bytes().map(Self)
    }

    /// Wrap identifier bytes decoded from a verified invite.
    pub fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Raw identifier bytes (sensitive).
    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl core::fmt::Debug for InvitationId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("InvitationId(<redacted>)")
    }
}

macro_rules! public_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash)]
        pub struct $name([u8; 16]);

        impl $name {
            /// Wrap raw identifier bytes.
            pub fn from_bytes(bytes: [u8; 16]) -> Self {
                Self(bytes)
            }

            /// Raw identifier bytes.
            pub fn as_bytes(&self) -> &[u8; 16] {
                &self.0
            }
        }

        impl core::fmt::Display for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                self.0.iter().try_for_each(|b| write!(f, "{b:02x}"))
            }
        }

        impl core::fmt::Debug for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                write!(f, "{}({self})", stringify!($name))
            }
        }
    };
}

public_id!(
    /// Non-secret operator handle for approve/deny/revoke/status. Grants nothing.
    OfferId
);
public_id!(
    /// Non-secret identifier of one committed issuance. Grants nothing.
    ReceiptId
);

/// A new invitation to record. All digests are computed by the invite codec.
#[derive(Clone, Debug)]
pub struct OfferSpec {
    /// Identifier embedded in the signed invite.
    pub invitation_id: InvitationId,
    /// Digest of the complete signed invite.
    pub invite_digest: [u8; 32],
    /// Canonical digest of the exact authorized relations.
    pub scope_digest: [u8; 32],
    /// Optional full device identity allowed to redeem; `None` is bearer.
    pub intended_subject: Option<EntityId>,
    /// Immutable creation-time lifetime and approval policy.
    pub policy: InvitationPolicy,
}

/// A verified claimant: full device identity plus canonical request-intent digest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claimant {
    /// Full device identity whose proof the caller verified.
    pub subject: EntityId,
    /// Canonical digest of the complete request intent.
    pub intent_digest: [u8; 32],
}

/// Result of a successful [`EnrollmentLedger::claim`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimOutcome {
    /// Claim committed (or resumed); issuance may proceed after rechecks.
    Ready,
    /// Claim committed (or resumed); an operator decision is still required.
    PendingApproval,
    /// This exact claimant was already issued; use [`EnrollmentLedger::recover`].
    AlreadyIssued(ReceiptId),
}

/// A committed receipt returned to its own claimant. `Debug` redacts the payload.
#[derive(Clone, PartialEq, Eq)]
pub struct Recovered {
    /// Identifier of the original issuance.
    pub receipt_id: ReceiptId,
    /// Exactly the bytes committed at issuance (secret-bearing).
    pub payload: Vec<u8>,
}

impl core::fmt::Debug for Recovered {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Recovered")
            .field("receipt_id", &self.receipt_id)
            .field("payload_len", &self.payload.len())
            .finish()
    }
}

/// Non-secret view of one offer's state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OfferState {
    /// Not yet claimed.
    Offered,
    /// Claimed; awaiting an operator decision.
    PendingApproval {
        /// The claimant awaiting approval.
        subject: EntityId,
    },
    /// Claimed and authorized; not yet issued.
    Ready {
        /// The winning claimant.
        subject: EntityId,
    },
    /// Issued exactly once.
    Issued {
        /// The claimant the receipt is bound to.
        subject: EntityId,
        /// Committed issuance identifier.
        receipt_id: ReceiptId,
        /// Issuance time (Unix seconds).
        issued_at: u64,
        /// Exclusive recovery deadline (Unix seconds).
        recovery_deadline: u64,
        /// Whether the secret payload is still retained for recovery.
        recoverable: bool,
    },
    /// Revoked by its owner before issuance.
    Revoked {
        /// The claimant at revocation time, if any.
        subject: Option<EntityId>,
    },
    /// Pending claim denied by its owner.
    Denied {
        /// The denied claimant.
        subject: EntityId,
    },
}

/// Non-secret status of one offer. Does not include the invitation identifier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OfferStatus {
    /// Operator handle.
    pub offer_id: OfferId,
    /// Optional intended device identity.
    pub intended_subject: Option<EntityId>,
    /// Approval policy recorded at creation.
    pub approval: ApprovalMode,
    /// Exclusive first-issuance deadline.
    pub expires_at: u64,
    /// Current transition state.
    pub state: OfferState,
}

/// Mutation capacity ceilings. They gate new mutations; the decoder applies the
/// hard maxima so a smaller configured limit never makes existing state unreadable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LedgerLimits {
    /// Maximum retained records (offers, claims, receipts and tombstones).
    pub max_records: usize,
    /// Maximum bytes of one issued receipt payload.
    pub max_result_bytes: usize,
    /// Maximum encoded snapshot size.
    pub max_snapshot_bytes: usize,
}

impl Default for LedgerLimits {
    fn default() -> Self {
        Self {
            max_records: 4096,
            max_result_bytes: 64 * 1024,
            max_snapshot_bytes: 16 * 1024 * 1024,
        }
    }
}

impl LedgerLimits {
    fn validate(&self) -> Result<(), LedgerError> {
        let ok = (1..=HARD_MAX_RECORDS).contains(&self.max_records)
            && (1..=HARD_MAX_RESULT_BYTES).contains(&self.max_result_bytes)
            && (1..=MAX_SNAPSHOT_BYTES).contains(&self.max_snapshot_bytes);
        ok.then_some(()).ok_or(LedgerError::InvalidLimits)
    }
}

/// What [`EnrollmentLedger::compact`] changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CompactReport {
    /// Receipts whose secret payload was dropped after their recovery deadline.
    pub payloads_discarded: usize,
    /// Terminal or expired records removed after their invitation expired.
    pub records_removed: usize,
}

/// Payload-free ledger failures. None of these carries secret bytes.
#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    /// Underlying protected storage failed (not uncertainty; see `Uncertain`).
    #[error(transparent)]
    Storage(StorageError),
    /// Durable state after a post-rename failure is unknown; reopen before use.
    #[error("enrollment ledger durability uncertain; close and reopen before use")]
    Uncertain,
    /// Snapshot failed integrity, bounds or transition validation.
    #[error("enrollment ledger snapshot is corrupt")]
    Corrupt,
    /// Snapshot version is not understood by this build.
    #[error("enrollment ledger snapshot version is unsupported")]
    UnsupportedVersion,
    /// The store belongs to a different issuer.
    #[error("enrollment ledger belongs to a different issuer")]
    IssuerMismatch,
    /// Configured limits are zero or exceed the hard ceilings.
    #[error("enrollment ledger limits are invalid")]
    InvalidLimits,
    /// A mutation would exceed a configured capacity; nothing was changed.
    #[error("enrollment ledger capacity exceeded")]
    Capacity,
    /// The invitation identifier or invite digest is already recorded.
    #[error("invitation already recorded")]
    Duplicate,
    /// No record for this invitation identifier.
    #[error("unknown invitation")]
    UnknownInvitation,
    /// No record for this offer handle.
    #[error("unknown offer")]
    UnknownOffer,
    /// Invitation time window refusal.
    #[error(transparent)]
    Policy(#[from] PolicyError),
    /// The claimant is not the invitation's intended subject.
    #[error("claimant is not the intended subject")]
    WrongSubject,
    /// A different claimant or intent already won this invitation.
    #[error("invitation is bound to a different claim")]
    ClaimConflict,
    /// The invitation has not been claimed.
    #[error("invitation has not been claimed")]
    NotClaimed,
    /// The claim still requires operator approval.
    #[error("invitation claim requires approval")]
    ApprovalRequired,
    /// Deny applies only to a claim awaiting approval.
    #[error("invitation claim is not pending approval")]
    NotPending,
    /// The invitation was revoked.
    #[error("invitation was revoked")]
    Revoked,
    /// The claim was denied.
    #[error("invitation claim was denied")]
    Denied,
    /// Already issued; never reissued. Revoke the granted relations separately.
    #[error("invitation already issued as receipt {0}")]
    AlreadyIssued(ReceiptId),
    /// Not issued yet, so there is nothing to recover.
    #[error("invitation has not been issued")]
    NotIssued,
    /// Recovery deadline passed or the payload was compacted away.
    #[error("receipt recovery window is closed")]
    RecoveryClosed,
    /// Empty or oversized receipt payload.
    #[error("receipt payload size is invalid")]
    InvalidResult,
    /// Revision counter or timestamp arithmetic would overflow.
    #[error("enrollment ledger counter overflow")]
    Overflow,
    /// The OS CSPRNG failed.
    #[error("CSPRNG unavailable")]
    Random,
}

impl From<StorageError> for LedgerError {
    fn from(e: StorageError) -> Self {
        match e {
            StorageError::Uncertain => Self::Uncertain,
            other => Self::Storage(other),
        }
    }
}

#[derive(Clone)]
struct Record {
    offer_id: OfferId,
    invitation_id: InvitationId,
    invite_digest: [u8; 32],
    scope_digest: [u8; 32],
    intended_subject: Option<EntityId>,
    policy: InvitationPolicy,
    state: State,
}

#[derive(Clone)]
enum State {
    Offered,
    Claimed {
        claim: Claimant,
        claimed_at: u64,
        ready: bool,
    },
    Issued {
        claim: Claimant,
        claimed_at: u64,
        receipt_id: ReceiptId,
        issued_at: u64,
        recovery_deadline: u64,
        payload: Option<Vec<u8>>,
    },
    Revoked {
        at: u64,
        claim: Option<Claimant>,
    },
    Denied {
        at: u64,
        claim: Claimant,
    },
}

/// Exclusive owner of one issuer's durable invitation ledger.
pub struct EnrollmentLedger {
    storage: EnrollmentStorage,
    issuer: EntityId,
    revision: u64,
    records: Vec<Record>,
    limits: LedgerLimits,
    uncertain: bool,
}

impl core::fmt::Debug for EnrollmentLedger {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EnrollmentLedger")
            .field("issuer", &super::fingerprint(&self.issuer))
            .field("revision", &self.revision)
            .field("records", &self.records.len())
            .field("uncertain", &self.uncertain)
            .finish()
    }
}

impl EnrollmentLedger {
    /// Initialize an empty ledger for `issuer` in a new protected directory.
    /// Refuses an existing path. No key material is stored.
    pub fn create(dir: &Path, issuer: EntityId, limits: LedgerLimits) -> Result<Self, LedgerError> {
        limits.validate()?;
        let storage = EnrollmentStorage::create(dir, &encode(&issuer, 0, &[]))?;
        Ok(Self {
            storage,
            issuer,
            revision: 0,
            records: Vec::new(),
            limits,
            uncertain: false,
        })
    }

    /// Open existing state, taking the lifetime lock. Missing, corrupt,
    /// unsupported or foreign-issuer state refuses; it never becomes empty.
    pub fn open(dir: &Path, issuer: EntityId, limits: LedgerLimits) -> Result<Self, LedgerError> {
        limits.validate()?;
        let storage = EnrollmentStorage::open(dir)?;
        let (revision, records) = decode(&storage.read()?, &issuer)?;
        Ok(Self {
            storage,
            issuer,
            revision,
            records,
            limits,
            uncertain: false,
        })
    }

    /// Issuer this ledger is bound to.
    pub fn issuer(&self) -> &EntityId {
        &self.issuer
    }

    /// Committed revision; increases by one per durable mutation.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Record a new invitation. Refuses duplicates, expired policy and capacity.
    pub fn offer(&mut self, spec: OfferSpec, now: u64) -> Result<OfferId, LedgerError> {
        self.fence()?;
        if now >= spec.policy.expires_at() {
            return Err(PolicyError::Expired.into());
        }
        if self
            .records
            .iter()
            .any(|r| r.invitation_id == spec.invitation_id || r.invite_digest == spec.invite_digest)
        {
            return Err(LedgerError::Duplicate);
        }
        if self.records.len() >= self.limits.max_records {
            return Err(LedgerError::Capacity);
        }
        let offer_id = OfferId(random_bytes()?);
        if self.records.iter().any(|r| r.offer_id == offer_id) {
            return Err(LedgerError::Duplicate);
        }
        self.records.push(Record {
            offer_id,
            invitation_id: spec.invitation_id,
            invite_digest: spec.invite_digest,
            scope_digest: spec.scope_digest,
            intended_subject: spec.intended_subject,
            policy: spec.policy,
            state: State::Offered,
        });
        if let Err(e) = self.persist() {
            self.records.pop();
            return Err(e);
        }
        Ok(offer_id)
    }

    /// Bind a verified claimant to the invitation, or resume that exact claim.
    ///
    /// The caller must already have verified the invite, the device proof and
    /// the intent. The first committed claimant wins; a different subject or
    /// intent is refused. Refusals never mutate state.
    pub fn claim(
        &mut self,
        invitation: &InvitationId,
        claimant: &Claimant,
        now: u64,
    ) -> Result<ClaimOutcome, LedgerError> {
        self.fence()?;
        let i = self.by_invitation(invitation)?;
        let rec = &self.records[i];
        match &rec.state {
            State::Offered => {
                if rec
                    .intended_subject
                    .as_ref()
                    .is_some_and(|s| *s != claimant.subject)
                {
                    return Err(LedgerError::WrongSubject);
                }
                let ready = rec.policy.check_redemption_at(now)? == ApprovalMode::Preauthorized;
                self.transition(
                    i,
                    State::Claimed {
                        claim: claimant.clone(),
                        claimed_at: now,
                        ready,
                    },
                )?;
                Ok(if ready {
                    ClaimOutcome::Ready
                } else {
                    ClaimOutcome::PendingApproval
                })
            }
            State::Claimed { claim, ready, .. } => {
                if claim != claimant {
                    return Err(LedgerError::ClaimConflict);
                }
                rec.policy.check_redemption_at(now)?;
                Ok(if *ready {
                    ClaimOutcome::Ready
                } else {
                    ClaimOutcome::PendingApproval
                })
            }
            State::Issued {
                claim, receipt_id, ..
            } => {
                if claim != claimant {
                    return Err(LedgerError::ClaimConflict);
                }
                Ok(ClaimOutcome::AlreadyIssued(*receipt_id))
            }
            State::Revoked { .. } => Err(LedgerError::Revoked),
            State::Denied { .. } => Err(LedgerError::Denied),
        }
    }

    /// Approve exactly `claimant`'s pending claim, rechecking expiry.
    /// Idempotent for an already-ready identical claim.
    pub fn approve(
        &mut self,
        offer: &OfferId,
        claimant: &Claimant,
        now: u64,
    ) -> Result<(), LedgerError> {
        self.fence()?;
        let i = self.by_offer(offer)?;
        let rec = &self.records[i];
        match &rec.state {
            State::Claimed {
                claim,
                ready,
                claimed_at,
            } => {
                if claim != claimant {
                    return Err(LedgerError::ClaimConflict);
                }
                rec.policy.check_redemption_at(now)?;
                if *ready {
                    return Ok(());
                }
                let claimed_at = *claimed_at;
                self.transition(
                    i,
                    State::Claimed {
                        claim: claimant.clone(),
                        claimed_at,
                        ready: true,
                    },
                )
            }
            other => Err(terminal_error(other)),
        }
    }

    /// Deny exactly `claimant`'s pending claim. Terminal; idempotent.
    pub fn deny(
        &mut self,
        offer: &OfferId,
        claimant: &Claimant,
        now: u64,
    ) -> Result<(), LedgerError> {
        self.fence()?;
        let i = self.by_offer(offer)?;
        match &self.records[i].state {
            State::Claimed { claim, ready, .. } => {
                if claim != claimant {
                    return Err(LedgerError::ClaimConflict);
                }
                if *ready {
                    return Err(LedgerError::NotPending);
                }
                self.transition(
                    i,
                    State::Denied {
                        at: now,
                        claim: claimant.clone(),
                    },
                )
            }
            State::Denied { claim, .. } if claim == claimant => Ok(()),
            State::Denied { .. } => Err(LedgerError::ClaimConflict),
            other => Err(terminal_error(other)),
        }
    }

    /// Invalidate an unissued invitation. Idempotent for revoked/denied offers.
    /// An issued invitation reports [`LedgerError::AlreadyIssued`]; revoke the
    /// granted relations through their own mechanisms instead.
    pub fn revoke(&mut self, offer: &OfferId, now: u64) -> Result<(), LedgerError> {
        self.fence()?;
        let i = self.by_offer(offer)?;
        let claim = match &self.records[i].state {
            State::Offered => None,
            State::Claimed { claim, .. } => Some(claim.clone()),
            State::Issued { receipt_id, .. } => {
                return Err(LedgerError::AlreadyIssued(*receipt_id))
            }
            State::Revoked { .. } | State::Denied { .. } => return Ok(()),
        };
        self.transition(i, State::Revoked { at: now, claim })
    }

    /// Commit `payload` as the single issuance for this ready claim.
    ///
    /// Rechecks expiry: an unissued invitation at or after expiry cannot issue,
    /// including after late approval. The caller must recheck current authority
    /// and must not deliver `payload` unless this returns `Ok`.
    pub fn issue(
        &mut self,
        invitation: &InvitationId,
        claimant: &Claimant,
        payload: &[u8],
        now: u64,
    ) -> Result<ReceiptId, LedgerError> {
        self.fence()?;
        let i = self.by_invitation(invitation)?;
        let rec = &self.records[i];
        let claimed_at = match &rec.state {
            State::Claimed {
                claim,
                ready,
                claimed_at,
            } => {
                if claim != claimant {
                    return Err(LedgerError::ClaimConflict);
                }
                if !*ready {
                    return Err(LedgerError::ApprovalRequired);
                }
                *claimed_at
            }
            other => return Err(terminal_error(other)),
        };
        rec.policy.check_redemption_at(now)?;
        if payload.is_empty() || payload.len() > self.limits.max_result_bytes {
            return Err(LedgerError::InvalidResult);
        }
        let recovery_deadline = now
            .checked_add(RECEIPT_RECOVERY_WINDOW_SECS)
            .ok_or(LedgerError::Overflow)?;
        let receipt_id = ReceiptId(random_bytes()?);
        if self.records.iter().any(
            |r| matches!(&r.state, State::Issued { receipt_id: other, .. } if *other == receipt_id),
        ) {
            return Err(LedgerError::Duplicate);
        }
        self.transition(
            i,
            State::Issued {
                claim: claimant.clone(),
                claimed_at,
                receipt_id,
                issued_at: now,
                recovery_deadline,
                payload: Some(payload.to_vec()),
            },
        )?;
        Ok(receipt_id)
    }

    /// Return the committed issuance to its own claimant, byte-identical, before
    /// the recovery deadline. Never mints again. The caller must require a fresh
    /// proof and recheck current credential/transport validity.
    pub fn recover(
        &self,
        invitation: &InvitationId,
        claimant: &Claimant,
        now: u64,
    ) -> Result<Recovered, LedgerError> {
        self.fence()?;
        let i = self.by_invitation(invitation)?;
        match &self.records[i].state {
            State::Issued {
                claim,
                receipt_id,
                recovery_deadline,
                payload,
                ..
            } => {
                if claim != claimant {
                    return Err(LedgerError::ClaimConflict);
                }
                match payload {
                    Some(bytes) if now < *recovery_deadline => Ok(Recovered {
                        receipt_id: *receipt_id,
                        payload: bytes.clone(),
                    }),
                    _ => Err(LedgerError::RecoveryClosed),
                }
            }
            State::Offered | State::Claimed { .. } => Err(LedgerError::NotIssued),
            State::Revoked { .. } => Err(LedgerError::Revoked),
            State::Denied { .. } => Err(LedgerError::Denied),
        }
    }

    /// Non-secret status of one offer.
    pub fn status(&self, offer: &OfferId) -> Result<OfferStatus, LedgerError> {
        self.fence()?;
        Ok(status_of(&self.records[self.by_offer(offer)?]))
    }

    /// Non-secret status of every retained offer, in creation order.
    pub fn statuses(&self) -> Result<Vec<OfferStatus>, LedgerError> {
        self.fence()?;
        Ok(self.records.iter().map(status_of).collect())
    }

    /// Drop receipt payloads past their recovery deadline, then remove records
    /// whose invitation has expired and which retain no recoverable payload.
    /// A removed invitation is thereafter unknown (still refused).
    pub fn compact(&mut self, now: u64) -> Result<CompactReport, LedgerError> {
        self.fence()?;
        let previous = self.records.clone();
        let mut report = CompactReport::default();
        for rec in &mut self.records {
            if let State::Issued {
                payload,
                recovery_deadline,
                ..
            } = &mut rec.state
            {
                if now >= *recovery_deadline && payload.take().is_some() {
                    report.payloads_discarded += 1;
                }
            }
        }
        let before = self.records.len();
        self.records.retain(|r| {
            let recoverable = matches!(
                &r.state,
                State::Issued {
                    payload: Some(_),
                    ..
                }
            );
            recoverable || now < r.policy.expires_at()
        });
        report.records_removed = before - self.records.len();
        if report == CompactReport::default() {
            return Ok(report);
        }
        if let Err(e) = self.persist() {
            self.records = previous;
            return Err(e);
        }
        Ok(report)
    }

    fn fence(&self) -> Result<(), LedgerError> {
        if self.uncertain {
            Err(LedgerError::Uncertain)
        } else {
            Ok(())
        }
    }

    fn by_invitation(&self, id: &InvitationId) -> Result<usize, LedgerError> {
        self.records
            .iter()
            .position(|r| r.invitation_id == *id)
            .ok_or(LedgerError::UnknownInvitation)
    }

    fn by_offer(&self, id: &OfferId) -> Result<usize, LedgerError> {
        self.records
            .iter()
            .position(|r| r.offer_id == *id)
            .ok_or(LedgerError::UnknownOffer)
    }

    /// Replace one record's state, durably, restoring it on failure.
    fn transition(&mut self, i: usize, next: State) -> Result<(), LedgerError> {
        let previous = std::mem::replace(&mut self.records[i].state, next);
        if let Err(e) = self.persist() {
            self.records[i].state = previous;
            return Err(e);
        }
        Ok(())
    }

    /// Persist the current in-memory candidate as the next revision. On error the
    /// caller restores its previous view; `Uncertain` additionally fences.
    fn persist(&mut self) -> Result<(), LedgerError> {
        let revision = self.revision.checked_add(1).ok_or(LedgerError::Overflow)?;
        let bytes = encode(&self.issuer, revision, &self.records);
        if bytes.len() > self.limits.max_snapshot_bytes {
            return Err(LedgerError::Capacity);
        }
        match self.storage.replace(&bytes) {
            Ok(()) => {
                self.revision = revision;
                Ok(())
            }
            Err(StorageError::Uncertain) => {
                self.uncertain = true;
                Err(LedgerError::Uncertain)
            }
            Err(e) => Err(e.into()),
        }
    }
}

fn terminal_error(state: &State) -> LedgerError {
    match state {
        State::Offered => LedgerError::NotClaimed,
        State::Claimed { .. } => LedgerError::NotIssued,
        State::Issued { receipt_id, .. } => LedgerError::AlreadyIssued(*receipt_id),
        State::Revoked { .. } => LedgerError::Revoked,
        State::Denied { .. } => LedgerError::Denied,
    }
}

fn status_of(r: &Record) -> OfferStatus {
    let state = match &r.state {
        State::Offered => OfferState::Offered,
        State::Claimed { claim, ready, .. } => {
            let subject = claim.subject.clone();
            if *ready {
                OfferState::Ready { subject }
            } else {
                OfferState::PendingApproval { subject }
            }
        }
        State::Issued {
            claim,
            receipt_id,
            issued_at,
            recovery_deadline,
            payload,
            ..
        } => OfferState::Issued {
            subject: claim.subject.clone(),
            receipt_id: *receipt_id,
            issued_at: *issued_at,
            recovery_deadline: *recovery_deadline,
            recoverable: payload.is_some(),
        },
        State::Revoked { claim, .. } => OfferState::Revoked {
            subject: claim.as_ref().map(|c| c.subject.clone()),
        },
        State::Denied { claim, .. } => OfferState::Denied {
            subject: claim.subject.clone(),
        },
    };
    OfferStatus {
        offer_id: r.offer_id,
        intended_subject: r.intended_subject.clone(),
        approval: r.policy.approval_mode(),
        expires_at: r.policy.expires_at(),
        state,
    }
}

fn random_bytes() -> Result<[u8; 16], LedgerError> {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).map_err(|_| LedgerError::Random)?;
    Ok(b)
}

// ---- snapshot codec -------------------------------------------------------
//
// MAGIC | u16 VERSION | issuer[32] | u64 revision | u32 count | records |
// blake3-keyed checksum[32] over everything before it. Integers little-endian.
// Record: offer16 | invitation16 | invite_digest32 | scope_digest32 |
//   u8 has_intended [+32] | u64 created | u64 expires | u8 mode | u8 tag | body.
// Bodies: 0 Offered; 1 Claimed claim64 u64 claimed_at u8 ready;
//   2 Issued claim64 u64 claimed_at receipt16 u64 issued_at u64 deadline
//     u8 has_payload [u32 len + bytes]; 3 Revoked u64 at u8 has_claim [claim64];
//   4 Denied u64 at claim64.

const TAG_OFFERED: u8 = 0;
const TAG_CLAIMED: u8 = 1;
const TAG_ISSUED: u8 = 2;
const TAG_REVOKED: u8 = 3;
const TAG_DENIED: u8 = 4;

fn checksum(body: &[u8]) -> [u8; CHECKSUM_LEN] {
    let mut h = blake3::Hasher::new_derive_key(CHECKSUM_CONTEXT);
    h.update(body);
    *h.finalize().as_bytes()
}

fn encode(issuer: &EntityId, revision: u64, records: &[Record]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(issuer.as_bytes());
    out.extend_from_slice(&revision.to_le_bytes());
    // Record count is bounded by HARD_MAX_RECORDS via LedgerLimits.
    out.extend_from_slice(&(records.len() as u32).to_le_bytes());
    for r in records {
        out.extend_from_slice(&r.offer_id.0);
        out.extend_from_slice(&r.invitation_id.0);
        out.extend_from_slice(&r.invite_digest);
        out.extend_from_slice(&r.scope_digest);
        match &r.intended_subject {
            Some(s) => {
                out.push(1);
                out.extend_from_slice(s.as_bytes());
            }
            None => out.push(0),
        }
        out.extend_from_slice(&r.policy.created_at().to_le_bytes());
        out.extend_from_slice(&r.policy.expires_at().to_le_bytes());
        out.push(match r.policy.approval_mode() {
            ApprovalMode::Preauthorized => 0,
            ApprovalMode::RequireApproval => 1,
        });
        match &r.state {
            State::Offered => out.push(TAG_OFFERED),
            State::Claimed {
                claim,
                claimed_at,
                ready,
            } => {
                out.push(TAG_CLAIMED);
                put_claim(&mut out, claim);
                out.extend_from_slice(&claimed_at.to_le_bytes());
                out.push(u8::from(*ready));
            }
            State::Issued {
                claim,
                claimed_at,
                receipt_id,
                issued_at,
                recovery_deadline,
                payload,
            } => {
                out.push(TAG_ISSUED);
                put_claim(&mut out, claim);
                out.extend_from_slice(&claimed_at.to_le_bytes());
                out.extend_from_slice(&receipt_id.0);
                out.extend_from_slice(&issued_at.to_le_bytes());
                out.extend_from_slice(&recovery_deadline.to_le_bytes());
                match payload {
                    Some(p) => {
                        out.push(1);
                        // Bounded by HARD_MAX_RESULT_BYTES via LedgerLimits.
                        out.extend_from_slice(&(p.len() as u32).to_le_bytes());
                        out.extend_from_slice(p);
                    }
                    None => out.push(0),
                }
            }
            State::Revoked { at, claim } => {
                out.push(TAG_REVOKED);
                out.extend_from_slice(&at.to_le_bytes());
                match claim {
                    Some(c) => {
                        out.push(1);
                        put_claim(&mut out, c);
                    }
                    None => out.push(0),
                }
            }
            State::Denied { at, claim } => {
                out.push(TAG_DENIED);
                out.extend_from_slice(&at.to_le_bytes());
                put_claim(&mut out, claim);
            }
        }
    }
    let sum = checksum(&out);
    out.extend_from_slice(&sum);
    out
}

fn put_claim(out: &mut Vec<u8>, c: &Claimant) {
    out.extend_from_slice(c.subject.as_bytes());
    out.extend_from_slice(&c.intent_digest);
}

fn decode(bytes: &[u8], issuer: &EntityId) -> Result<(u64, Vec<Record>), LedgerError> {
    let body_len = bytes
        .len()
        .checked_sub(CHECKSUM_LEN)
        .ok_or(LedgerError::Corrupt)?;
    let (body, sum) = bytes.split_at(body_len);
    let mut r = Reader::new(body);
    if r.take_arr::<4>() != Some(MAGIC) {
        return Err(LedgerError::Corrupt);
    }
    // Version is checked before the checksum so a future format reports as
    // unsupported rather than corrupt; the checksum context is per-version.
    match r.take_u16() {
        Some(VERSION) => {}
        Some(_) => return Err(LedgerError::UnsupportedVersion),
        None => return Err(LedgerError::Corrupt),
    }
    if checksum(body) != sum {
        return Err(LedgerError::Corrupt);
    }
    let stored_issuer = EntityId::from_bytes(r.take_arr::<32>().ok_or(LedgerError::Corrupt)?);
    if stored_issuer != *issuer {
        return Err(LedgerError::IssuerMismatch);
    }
    let revision = r.take_u64().ok_or(LedgerError::Corrupt)?;
    let count = r.take_u32().ok_or(LedgerError::Corrupt)? as usize;
    if count > HARD_MAX_RECORDS {
        return Err(LedgerError::Corrupt);
    }
    let mut records: Vec<Record> = Vec::with_capacity(count);
    for _ in 0..count {
        let rec = decode_record(&mut r).ok_or(LedgerError::Corrupt)?;
        let receipt = |r: &Record| match &r.state {
            State::Issued { receipt_id, .. } => Some(*receipt_id),
            _ => None,
        };
        let duplicate = records.iter().any(|o| {
            o.offer_id == rec.offer_id
                || o.invitation_id == rec.invitation_id
                || o.invite_digest == rec.invite_digest
                || (receipt(&rec).is_some() && receipt(o) == receipt(&rec))
        });
        if duplicate {
            return Err(LedgerError::Corrupt);
        }
        records.push(rec);
    }
    if !r.done() {
        return Err(LedgerError::Corrupt);
    }
    Ok((revision, records))
}

fn take_bool(r: &mut Reader<'_>) -> Option<bool> {
    match r.take_arr::<1>()?[0] {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    }
}

fn take_claim(r: &mut Reader<'_>) -> Option<Claimant> {
    Some(Claimant {
        subject: EntityId::from_bytes(r.take_arr::<32>()?),
        intent_digest: r.take_arr::<32>()?,
    })
}

/// Decode and validate one record; `None` on any structural or semantic violation.
fn decode_record(r: &mut Reader<'_>) -> Option<Record> {
    let offer_id = OfferId(r.take_arr::<16>()?);
    let invitation_id = InvitationId(r.take_arr::<16>()?);
    let invite_digest = r.take_arr::<32>()?;
    let scope_digest = r.take_arr::<32>()?;
    let intended_subject = if take_bool(r)? {
        Some(EntityId::from_bytes(r.take_arr::<32>()?))
    } else {
        None
    };
    let created = r.take_u64()?;
    let expires = r.take_u64()?;
    let mode = match r.take_arr::<1>()?[0] {
        0 => ApprovalMode::Preauthorized,
        1 => ApprovalMode::RequireApproval,
        _ => return None,
    };
    let policy = InvitationPolicy::from_stored(created, expires, mode)?;
    let in_window = |t: u64| t >= created && t < expires;
    let subject_ok = |c: &Claimant| intended_subject.as_ref().is_none_or(|s| *s == c.subject);
    let state = match r.take_arr::<1>()?[0] {
        TAG_OFFERED => State::Offered,
        TAG_CLAIMED => {
            let claim = take_claim(r)?;
            let claimed_at = r.take_u64()?;
            let ready = take_bool(r)?;
            // A preauthorized claim is ready on commit; it is never pending.
            let valid = subject_ok(&claim)
                && in_window(claimed_at)
                && (ready || mode == ApprovalMode::RequireApproval);
            valid.then_some(State::Claimed {
                claim,
                claimed_at,
                ready,
            })?
        }
        TAG_ISSUED => {
            let claim = take_claim(r)?;
            let claimed_at = r.take_u64()?;
            let receipt_id = ReceiptId(r.take_arr::<16>()?);
            let issued_at = r.take_u64()?;
            let recovery_deadline = r.take_u64()?;
            let payload = if take_bool(r)? {
                let len = r.take_u32()? as usize;
                if len == 0 || len > HARD_MAX_RESULT_BYTES {
                    return None;
                }
                Some(r.take(len)?.to_vec())
            } else {
                None
            };
            // A deadline beyond the fixed window would be a silent extension.
            let valid = subject_ok(&claim)
                && in_window(claimed_at)
                && in_window(issued_at)
                && issued_at >= claimed_at
                && issued_at.checked_add(RECEIPT_RECOVERY_WINDOW_SECS) == Some(recovery_deadline);
            valid.then_some(State::Issued {
                claim,
                claimed_at,
                receipt_id,
                issued_at,
                recovery_deadline,
                payload,
            })?
        }
        TAG_REVOKED => {
            let at = r.take_u64()?;
            let claim = if take_bool(r)? {
                Some(take_claim(r)?)
            } else {
                None
            };
            claim
                .as_ref()
                .is_none_or(subject_ok)
                .then_some(State::Revoked { at, claim })?
        }
        TAG_DENIED => {
            let at = r.take_u64()?;
            let claim = take_claim(r)?;
            // Only an approval-required claim can be pending, hence deniable.
            (subject_ok(&claim) && mode == ApprovalMode::RequireApproval)
                .then_some(State::Denied { at, claim })?
        }
        _ => return None,
    };
    Some(Record {
        offer_id,
        invitation_id,
        invite_digest,
        scope_digest,
        intended_subject,
        policy,
        state,
    })
}
