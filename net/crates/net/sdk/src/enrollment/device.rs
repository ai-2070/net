// SPDX-License-Identifier: MIT OR Apache-2.0
//! Device-side durable join: persist identity and intent, redeem, verify, install.
//!
//! [`DeviceJoin::begin`] durably stores the device identity seed, the signed
//! invite and the canonical [`RedemptionIntent`] in a new protected directory
//! **before any network request**, so every retry after a crash or lost
//! response uses the same key and intent and recovers the same committed
//! issuance. [`DeviceJoin::redeem`] verifies a delivered bundle against the
//! invite and intent (receipt signature, subject, digests, relations and PSK
//! trust domain) and persists it before reporting it installed. A failure to
//! persist reports an error, never installed; a retry recovers the same bytes.
//!
//! Installed means the credentials are durably held here. It is not proof of
//! live mesh admission; attach with the bundle's PSK and contact and observe it.
//! The snapshot is secret-bearing (identity seed, PSK); `Debug` redacts both.
//!
//! **Leaving.** [`DeviceJoin::leave`] durably records the departure and erases
//! the delivered bundle (PSK and contact) in the same write, keeping the device
//! identity, invite and intent. A left join refuses [`DeviceJoin::redeem`]; the
//! only way back is an explicit [`DeviceJoin::rejoin`], after which redemption
//! must go back to the issuer and pass its *current* authorization (recovery),
//! never a locally retained credential. The storage's exclusive owner lock
//! orders leave against any in-flight redemption on the same state.

use std::path::Path;
use std::time::Duration;

use net::adapter::net::behavior::enrollment_storage::{EnrollmentStorage, StorageError};

use super::bundle::{BundleError, MembershipBundle};
use super::invite::{InviteError, MembershipInvite, RedemptionIntent};
use super::redeem::{redeem_with_path, RedeemError, RedeemOutcome, RedeemPath};
use super::store::ReceiptId;
use super::{fingerprint, Reader};
use crate::identity::Identity;

const MAGIC: [u8; 4] = *b"NMDJ";
const VERSION: u16 = 2;
const CHECKSUM_CONTEXT: &str = "net-mesh device join snapshot v1";
const MAX_SNAPSHOT_BYTES: usize = 64 * 1024;

/// Device join failures. None carries secret bytes.
#[derive(Debug, thiserror::Error)]
pub enum DeviceJoinError {
    /// Protected storage failed; `Uncertain` requires close and reopen.
    #[error(transparent)]
    Storage(#[from] StorageError),
    /// Persisted state failed integrity or validation.
    #[error("device join state is corrupt")]
    Corrupt,
    /// The invite/intent could not be bound to this device.
    #[error(transparent)]
    Invite(#[from] InviteError),
    /// Redemption failed or was refused; state is unchanged and resumable.
    #[error(transparent)]
    Redeem(#[from] RedeemError),
    /// The delivered bundle did not verify; nothing was installed.
    #[error(transparent)]
    Bundle(#[from] BundleError),
    /// This device left the mesh; only an explicit rejoin restores the join.
    #[error("this device left the mesh at unix {at}; rejoin explicitly to redeem again")]
    Left {
        /// When the departure was recorded (Unix seconds).
        at: u64,
    },
}

/// Result of one redemption attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinStatus {
    /// Claim recorded; the operator must approve. Retry later.
    PendingApproval,
    /// Bundle verified and durably installed.
    Installed {
        /// Committed issuance, if learned in this attempt (`None` when already
        /// installed before it).
        receipt_id: Option<ReceiptId>,
    },
}

/// Exclusive owner of one device's in-progress or installed join.
pub struct DeviceJoin {
    storage: EnrollmentStorage,
    identity: Identity,
    invite: MembershipInvite,
    intent: RedemptionIntent,
    bundle: Option<MembershipBundle>,
    left_at: Option<u64>,
    path: Option<RedeemPath>,
}

impl core::fmt::Debug for DeviceJoin {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DeviceJoin")
            .field("device", &fingerprint(self.identity.entity_id()))
            .field("invite", &self.invite)
            .field("bundle", &self.bundle)
            .finish()
    }
}

impl DeviceJoin {
    /// Start joining `invite` as `identity`, persisting both and the intent in a
    /// new protected directory before any network use. Refuses an existing path
    /// (use [`Self::open`] to resume) and an identity the invite is not bound to.
    pub fn begin(
        dir: &Path,
        invite: &MembershipInvite,
        identity: Identity,
    ) -> Result<Self, DeviceJoinError> {
        let intent = RedemptionIntent::for_invite(invite, identity.entity_id().clone())?;
        let snapshot = encode(&identity, invite, &intent, None, None);
        let storage = EnrollmentStorage::create(dir, &snapshot)?;
        Ok(Self {
            storage,
            identity,
            invite: invite.clone(),
            intent,
            bundle: None,
            left_at: None,
            path: None,
        })
    }

    /// Resume a join from `dir`, taking its exclusive lock. Missing, corrupt or
    /// inconsistent state refuses; it is never reset.
    pub fn open(dir: &Path) -> Result<Self, DeviceJoinError> {
        let storage = EnrollmentStorage::open(dir)?;
        let (identity, invite, intent, bundle, left_at) = decode(&storage.read()?)?;
        Ok(Self {
            storage,
            identity,
            invite,
            intent,
            bundle,
            left_at,
            path: None,
        })
    }

    /// Redeem (or recover) over the invite's enrollment session and install the
    /// verified bundle. Already installed: returns without network use.
    pub async fn redeem(&mut self, timeout: Duration) -> Result<JoinStatus, DeviceJoinError> {
        if let Some(at) = self.left_at {
            return Err(DeviceJoinError::Left { at });
        }
        if self.bundle.is_some() {
            return Ok(JoinStatus::Installed { receipt_id: None });
        }
        let (outcome, path) =
            redeem_with_path(&self.invite, &self.identity, &self.intent, timeout).await?;
        self.path = Some(path);
        let (receipt_id, bytes) = match outcome {
            RedeemOutcome::PendingApproval => return Ok(JoinStatus::PendingApproval),
            RedeemOutcome::Issued {
                receipt_id, bundle, ..
            } => (receipt_id, bundle),
        };
        self.install(&bytes)?;
        Ok(JoinStatus::Installed {
            receipt_id: Some(receipt_id),
        })
    }

    /// Verify and durably install delivered bundle bytes.
    fn install(&mut self, bytes: &[u8]) -> Result<(), DeviceJoinError> {
        let bundle = MembershipBundle::from_bytes(bytes)?;
        bundle.verify_for(&self.invite, &self.intent)?;
        let snapshot = encode(
            &self.identity,
            &self.invite,
            &self.intent,
            Some(&bundle),
            None,
        );
        self.storage.replace(&snapshot)?;
        self.bundle = Some(bundle);
        Ok(())
    }

    /// Leave the mesh: durably record the departure at `now` (Unix seconds)
    /// and erase the delivered bundle in the same write. Idempotent: returns
    /// `false` when this join had already left (the original time is kept).
    /// Nothing is sent anywhere; the issuer is not notified and copies of the
    /// PSK held by other processes are not affected.
    pub fn leave(&mut self, now: u64) -> Result<bool, DeviceJoinError> {
        if self.left_at.is_some() {
            return Ok(false);
        }
        let snapshot = encode(&self.identity, &self.invite, &self.intent, None, Some(now));
        self.storage.replace(&snapshot)?;
        self.bundle = None;
        self.left_at = Some(now);
        Ok(true)
    }

    /// Explicitly restore a left join. Nothing is reinstalled here: the next
    /// [`Self::redeem`] must obtain the bundle from the issuer again, under
    /// its current authorization. No-op for a join that has not left.
    pub fn rejoin(&mut self) -> Result<(), DeviceJoinError> {
        if self.left_at.is_none() {
            return Ok(());
        }
        let snapshot = encode(&self.identity, &self.invite, &self.intent, None, None);
        self.storage.replace(&snapshot)?;
        self.left_at = None;
        Ok(())
    }

    /// When this device left the mesh, if it has.
    pub fn left_at(&self) -> Option<u64> {
        self.left_at
    }

    /// The device identity used for this join (persisted before redemption).
    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    /// The invite being redeemed.
    pub fn invite(&self) -> &MembershipInvite {
        &self.invite
    }

    /// The path the last redemption session in this process ran over
    /// (direct or relayed); `None` before one ran.
    pub fn last_path(&self) -> Option<RedeemPath> {
        self.path
    }

    /// The installed bundle, once verified and persisted.
    pub fn bundle(&self) -> Option<&MembershipBundle> {
        self.bundle.as_ref()
    }
}

// MAGIC | u16 VERSION | seed[32] | u32 invite | u32 intent | u8 has_bundle
// [u32 bundle] | (v2) u8 left [u64 left_at] | blake3 checksum[32] over
// everything before it. A left join holds no bundle.
fn encode(
    identity: &Identity,
    invite: &MembershipInvite,
    intent: &RedemptionIntent,
    bundle: Option<&MembershipBundle>,
    left_at: Option<u64>,
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&identity.to_bytes());
    super::push_lp(&mut out, invite.to_bytes());
    super::push_lp(&mut out, &intent.to_bytes());
    match bundle {
        Some(b) => {
            out.push(1);
            super::push_lp(&mut out, &b.to_bytes());
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
    let sum = blake3::derive_key(CHECKSUM_CONTEXT, &out);
    out.extend_from_slice(&sum);
    out
}

type Decoded = (
    Identity,
    MembershipInvite,
    RedemptionIntent,
    Option<MembershipBundle>,
    Option<u64>,
);

fn decode(bytes: &[u8]) -> Result<Decoded, DeviceJoinError> {
    let corrupt = || DeviceJoinError::Corrupt;
    if bytes.len() > MAX_SNAPSHOT_BYTES {
        return Err(corrupt());
    }
    let body_len = bytes.len().checked_sub(32).ok_or_else(corrupt)?;
    let (body, sum) = bytes.split_at(body_len);
    if blake3::derive_key(CHECKSUM_CONTEXT, body) != sum {
        return Err(corrupt());
    }
    let mut r = Reader::new(body);
    if r.take_arr::<4>() != Some(MAGIC) {
        return Err(corrupt());
    }
    // v1 snapshots (before leave existed) have no left section.
    let version = r.take_u16().ok_or_else(corrupt)?;
    if version != 1 && version != VERSION {
        return Err(corrupt());
    }
    let identity = Identity::from_seed(r.take_arr::<32>().ok_or_else(corrupt)?);
    let invite =
        MembershipInvite::from_bytes(r.take_lp().ok_or_else(corrupt)?).map_err(|_| corrupt())?;
    let intent =
        RedemptionIntent::from_bytes(r.take_lp().ok_or_else(corrupt)?).map_err(|_| corrupt())?;
    let bundle = match r.take_arr::<1>().ok_or_else(corrupt)?[0] {
        0 => None,
        1 => Some(
            MembershipBundle::from_bytes(r.take_lp().ok_or_else(corrupt)?)
                .map_err(|_| corrupt())?,
        ),
        _ => return Err(corrupt()),
    };
    let left_at = if version == 1 {
        None
    } else {
        match r.take_arr::<1>().ok_or_else(corrupt)?[0] {
            0 => None,
            1 => Some(r.take_u64().ok_or_else(corrupt)?),
            _ => return Err(corrupt()),
        }
    };
    if left_at.is_some() && bundle.is_some() {
        return Err(corrupt());
    }
    if !r.done() || intent.subject() != identity.entity_id() {
        return Err(corrupt());
    }
    intent.check_against(&invite).map_err(|_| corrupt())?;
    if let Some(b) = &bundle {
        b.verify_for(&invite, &intent).map_err(|_| corrupt())?;
    }
    Ok((identity, invite, intent, bundle, left_at))
}
