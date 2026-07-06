//! Operator-side mesh management — the `mesh.invite / approve / revoke /
//! devices` surface (Hermes V2 Phase 1), composing the three enrollment stores
//! into one coordinator.
//!
//! [`OperatorEnrollment`] ties together the pieces an operator needs to run
//! their mesh's device lifecycle, all **transport-independent**:
//!
//! * [`crate::enrollment::EnrollmentAuthority`] — mint invites, verify + approve
//!   join requests into `root → device` delegations;
//! * [`crate::devices::DeviceRegistry`] — the persistent inventory behind
//!   `mesh.devices()`;
//! * [`crate::revocation::RevocationStore`] — the enforcing floors a running
//!   `net wrap` provider honors.
//!
//! It holds the outstanding invites it minted, so [`OperatorEnrollment::approve`]
//! takes just the arriving [`JoinRequest`] and looks up the matching invite by
//! nonce (mirroring the plan's `mesh.approve(request)`), then records the device
//! and — on [`OperatorEnrollment::revoke`] — bumps the floor **and** stamps the
//! inventory in one call.
//!
//! # What lives elsewhere
//!
//! The one primitive this module does **not** provide is `mesh.join` — the
//! *device* side that dials the invite's rendezvous, submits its
//! [`JoinRequest`], and receives its delegation. That's the networked half
//! (Slice B2); the transport calls [`OperatorEnrollment::approve`] to produce
//! the [`Enrollment`] whose `chain` it sends back.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;

use parking_lot::Mutex;

use crate::devices::{
    default_device_registry_path, DeviceRecord, DeviceRegistry, DeviceRegistryError,
};
use crate::enrollment::{
    now_unix, reject, Enrollment, EnrollmentAuthority, EnrollmentError, InviteToken, JoinOutcome,
    JoinRequest,
};
use crate::identity::{EntityId, Identity};
use crate::revocation::{default_revocation_store_path, RevocationStore, RevocationStoreError};

/// The generation an ordinary revoke raises the floor to — revokes all current
/// generation-0 delegations. Matches `net identity revoke`'s default so the
/// facade and the CLI agree.
const DEFAULT_REVOKE_GENERATION: u32 = 1;

/// Errors from the operator surface.
#[derive(Debug, thiserror::Error)]
pub enum OperatorError {
    /// No outstanding invite matches the request's nonce — never minted here,
    /// already redeemed (single-use), or expired and pruned.
    #[error("no outstanding invite matches this request")]
    UnknownInvite,
    /// The operator (a human or a policy) explicitly denied an otherwise-valid
    /// request. Distinct from a failed check — nothing about the invite or the
    /// signature was wrong; admission was refused.
    #[error("enrollment denied by the operator")]
    Denied,
    /// The enrollment handshake rejected the request.
    #[error(transparent)]
    Enrollment(#[from] EnrollmentError),
    /// The device registry could not be read or written.
    #[error(transparent)]
    Registry(#[from] DeviceRegistryError),
    /// The revocation store could not be read or written.
    #[error(transparent)]
    Revocation(#[from] RevocationStoreError),
}

/// The operator's device-lifecycle coordinator for one mesh root.
pub struct OperatorEnrollment {
    authority: EnrollmentAuthority,
    /// Minted-but-unredeemed invites, keyed by nonce, so `approve(request)` can
    /// find the invite a request references. Pruned on access.
    pending: Mutex<HashMap<[u8; 16], InviteToken>>,
    registry_path: PathBuf,
    revocation_path: PathBuf,
}

impl OperatorEnrollment {
    /// Build a coordinator for `root` (which owns the root signing key), with
    /// explicit store paths.
    pub fn new(root: Identity, registry_path: PathBuf, revocation_path: PathBuf) -> Self {
        Self {
            authority: EnrollmentAuthority::new(root),
            pending: Mutex::new(HashMap::new()),
            registry_path,
            revocation_path,
        }
    }

    /// Build a coordinator using the per-user default store paths (the same
    /// machine-shared files the CLI and a `net wrap` provider converge on).
    /// `None` when neither path resolves.
    pub fn with_default_paths(root: Identity) -> Option<Self> {
        Some(Self::new(
            root,
            default_device_registry_path()?,
            default_revocation_store_path()?,
        ))
    }

    /// The mesh root's entity id.
    pub fn root_id(&self) -> &EntityId {
        self.authority.root_id()
    }

    /// The displayed fingerprint of the mesh root (show it on the invite).
    pub fn root_fingerprint(&self) -> String {
        self.authority.root_fingerprint()
    }

    /// Mint an invite for this mesh, valid for `ttl`, and remember it so a
    /// later [`Self::approve`] can match a request to it. (`mesh.invite`.)
    pub fn invite(&self, rendezvous: impl Into<String>, ttl: Duration) -> InviteToken {
        self.invite_at(rendezvous, ttl, now_unix())
    }

    /// [`Self::invite`] with an explicit `now` — for deterministic tests.
    pub fn invite_at(&self, rendezvous: impl Into<String>, ttl: Duration, now: u64) -> InviteToken {
        let invite = self.authority.mint_invite_at(rendezvous, ttl, now);
        self.pending.lock().insert(invite.nonce, invite.clone());
        invite
    }

    /// Outstanding (minted, unredeemed, unexpired) invites at `now`.
    pub fn pending_invites(&self, now: u64) -> Vec<InviteToken> {
        let mut pending = self.pending.lock();
        pending.retain(|_, inv| !inv.is_expired(now));
        pending.values().cloned().collect()
    }

    /// Approve an arriving join request, reading the system clock. (`mesh.approve`.)
    pub fn approve(
        &self,
        request: &JoinRequest,
        grant_ttl: Duration,
        max_depth: u8,
    ) -> Result<Enrollment, OperatorError> {
        self.approve_at(request, now_unix(), grant_ttl, max_depth)
    }

    /// [`Self::approve`] with an explicit `now`. Looks up the invite the request
    /// references (by nonce), runs the fail-closed enrollment checks, records
    /// the device in the inventory, and drops the invite from the pending set
    /// (single-use).
    pub fn approve_at(
        &self,
        request: &JoinRequest,
        now: u64,
        grant_ttl: Duration,
        max_depth: u8,
    ) -> Result<Enrollment, OperatorError> {
        let invite = {
            let mut pending = self.pending.lock();
            pending.retain(|_, inv| !inv.is_expired(now));
            pending
                .get(&request.invite_nonce)
                .cloned()
                .ok_or(OperatorError::UnknownInvite)?
        };
        // The authority re-runs every check (incl. its own single-use ledger);
        // a rejection here leaves the invite in `pending` untouched.
        let enrollment = self
            .authority
            .approve(request, &invite, now, grant_ttl, max_depth)?;
        DeviceRegistry::record(
            &self.registry_path,
            DeviceRecord::new(
                enrollment.device.clone(),
                enrollment.name.clone(),
                enrollment.tags.clone(),
                now,
            ),
        )?;
        // Single-use: retire the invite only after a successful admission.
        self.pending.lock().remove(&request.invite_nonce);
        Ok(enrollment)
    }

    /// Approve an arriving request **only if the operator says so** — the model
    /// the V2 threat model wants ("a leaked invite lets someone *ask*, never
    /// admits them").
    ///
    /// Flow: find + validate the referenced invite for display
    /// ([`EnrollmentAuthority::verify_request`], **no** single-use spend); await
    /// `approver` (the operator's decision — e.g. a Telegram/desktop prompt);
    /// only on approval commit the admission ([`Self::approve`], which spends
    /// the invite against a *fresh* clock, so an invite that expired while the
    /// human deliberated is correctly rejected) and record the device. A denied
    /// request never burns the invite, so the real device can still use it.
    pub async fn approve_with<F, Fut>(
        &self,
        request: &JoinRequest,
        grant_ttl: Duration,
        max_depth: u8,
        approver: F,
    ) -> Result<Enrollment, OperatorError>
    where
        F: FnOnce(JoinRequest) -> Fut,
        Fut: Future<Output = bool>,
    {
        // Look up the invite (leave it in `pending`) and validate for display.
        let invite = {
            let now = now_unix();
            let mut pending = self.pending.lock();
            pending.retain(|_, inv| !inv.is_expired(now));
            pending
                .get(&request.invite_nonce)
                .cloned()
                .ok_or(OperatorError::UnknownInvite)?
        };
        self.authority
            .verify_request(request, &invite, now_unix())?;

        // The operator's decision — may take a while (a human on their phone).
        if !approver(request.clone()).await {
            return Err(OperatorError::Denied);
        }

        // Commit against a fresh clock: spends the invite + signs, re-running
        // every check so a race or an expiry-during-approval is caught.
        let enrollment =
            self.authority
                .approve(request, &invite, now_unix(), grant_ttl, max_depth)?;
        DeviceRegistry::record(
            &self.registry_path,
            DeviceRecord::new(
                enrollment.device.clone(),
                enrollment.name.clone(),
                enrollment.tags.clone(),
                now_unix(),
            ),
        )?;
        self.pending.lock().remove(&request.invite_nonce);
        Ok(enrollment)
    }

    /// Revoke a device, reading the system clock: raise its floor to generation
    /// 1 (kills all current delegations, matching `net identity revoke`) and
    /// stamp the inventory. (`mesh.revoke`.)
    pub fn revoke(&self, device: &EntityId) -> Result<(), OperatorError> {
        self.revoke_at(device, DEFAULT_REVOKE_GENERATION, now_unix())
    }

    /// [`Self::revoke`] with an explicit floor `generation` and `now`.
    ///
    /// Enforcement first: bump the [`RevocationStore`] floor (what a running
    /// provider honors), then stamp `revoked_at` in the inventory for display.
    /// A device absent from the inventory still gets its floor raised — the
    /// inventory stamp is best-effort metadata.
    pub fn revoke_at(
        &self,
        device: &EntityId,
        generation: u32,
        now: u64,
    ) -> Result<(), OperatorError> {
        RevocationStore::revoke_below(&self.revocation_path, device, generation)?;
        DeviceRegistry::mark_revoked(&self.registry_path, device, now)?;
        Ok(())
    }

    /// The enrolled devices in the inventory. (`mesh.devices`.)
    pub fn devices(&self) -> Result<Vec<DeviceRecord>, OperatorError> {
        Ok(DeviceRegistry::load(&self.registry_path)?
            .list()
            .into_iter()
            .cloned()
            .collect())
    }

    /// Prune a device from the inventory entirely (orthogonal to revoking its
    /// floor — see [`crate::devices`]). Returns whether a record existed.
    pub fn forget(&self, device: &EntityId) -> Result<bool, OperatorError> {
        Ok(DeviceRegistry::remove(&self.registry_path, device)?)
    }

    /// The **server side** of the enrollment RPC: turn serialized
    /// [`JoinRequest`] bytes into serialized [`JoinOutcome`] bytes. The
    /// transport (Slice B2b) just moves these — parse the request, run
    /// [`Self::approve`], and answer `Admitted { chain }` or a coded
    /// `Rejected`. Never returns an error itself: a malformed request or a
    /// rejected approval is a `Rejected` outcome the device can read, not a
    /// transport failure.
    pub fn handle_join_request(
        &self,
        request_bytes: &[u8],
        grant_ttl: Duration,
        max_depth: u8,
    ) -> Vec<u8> {
        let outcome = match JoinRequest::from_bytes(request_bytes) {
            Err(e) => JoinOutcome::Rejected {
                code: reject::MALFORMED,
                message: e.to_string(),
            },
            Ok(request) => match self.approve(&request, grant_ttl, max_depth) {
                Ok(enrollment) => JoinOutcome::Admitted {
                    chain: enrollment.chain.to_bytes(),
                },
                Err(e) => JoinOutcome::Rejected {
                    code: reject_code(&e),
                    message: e.to_string(),
                },
            },
        };
        outcome.to_bytes()
    }

    /// The approval-gated server side: like [`Self::handle_join_request`], but
    /// routes a valid request through `approver` before admitting it (see
    /// [`Self::approve_with`]). A denial answers a coded `Rejected`, never an
    /// out-of-band error.
    pub async fn handle_join_request_approved<F, Fut>(
        &self,
        request_bytes: &[u8],
        grant_ttl: Duration,
        max_depth: u8,
        approver: F,
    ) -> Vec<u8>
    where
        F: FnOnce(JoinRequest) -> Fut,
        Fut: Future<Output = bool>,
    {
        let outcome = match JoinRequest::from_bytes(request_bytes) {
            Err(e) => JoinOutcome::Rejected {
                code: reject::MALFORMED,
                message: e.to_string(),
            },
            Ok(request) => match self
                .approve_with(&request, grant_ttl, max_depth, approver)
                .await
            {
                Ok(enrollment) => JoinOutcome::Admitted {
                    chain: enrollment.chain.to_bytes(),
                },
                Err(e) => JoinOutcome::Rejected {
                    code: reject_code(&e),
                    message: e.to_string(),
                },
            },
        };
        outcome.to_bytes()
    }
}

/// Map an [`OperatorError`] to a stable [`reject`] code for a [`JoinOutcome`].
fn reject_code(err: &OperatorError) -> u16 {
    match err {
        OperatorError::UnknownInvite => reject::UNKNOWN_INVITE,
        OperatorError::Denied => reject::DENIED,
        OperatorError::Enrollment(e) => match e {
            EnrollmentError::MalformedInvite(_) | EnrollmentError::MalformedRequest(_) => {
                reject::MALFORMED
            }
            EnrollmentError::Expired => reject::EXPIRED,
            EnrollmentError::NonceMismatch
            | EnrollmentError::WrongMesh
            | EnrollmentError::BadSignature => reject::BAD_REQUEST,
            EnrollmentError::Replay => reject::REPLAY,
            EnrollmentError::LedgerSaturated | EnrollmentError::Token(_) => reject::INTERNAL,
        },
        OperatorError::Registry(_) | OperatorError::Revocation(_) => reject::INTERNAL,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delegation::{RevocationRegistry, DEFAULT_DELEGATION_DEPTH};

    const HOUR: Duration = Duration::from_secs(3600);
    const T0: u64 = 1_700_000_000;

    fn operator() -> (OperatorEnrollment, Identity, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let root = Identity::generate();
        let op = OperatorEnrollment::new(
            root.clone(),
            dir.path().join("devices.json"),
            dir.path().join("rev.json"),
        );
        (op, root, dir)
    }

    #[test]
    fn invite_then_approve_records_an_active_device() {
        let (op, _root, _dir) = operator();
        let invite = op.invite_at("relay://rv", HOUR, T0);
        let device = Identity::generate();
        let req = JoinRequest::create(&device, "pc", vec!["region:office".into()], &invite);

        let enrollment = op
            .approve_at(&req, T0, HOUR, DEFAULT_DELEGATION_DEPTH)
            .expect("valid join approves");
        assert_eq!(&enrollment.device, device.entity_id());

        let devices = op.devices().unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].name, "pc");
        assert!(!devices[0].is_revoked());
        // The invite is retired after use.
        assert!(op.pending_invites(T0).is_empty());
    }

    #[test]
    fn approve_is_single_use_through_the_facade() {
        let (op, _root, _dir) = operator();
        let invite = op.invite_at("relay://rv", HOUR, T0);
        let device = Identity::generate();
        let req = JoinRequest::create(&device, "pc", vec![], &invite);
        op.approve_at(&req, T0, HOUR, DEFAULT_DELEGATION_DEPTH)
            .unwrap();
        // The invite was retired, so a replay finds no matching invite.
        assert!(matches!(
            op.approve_at(&req, T0, HOUR, DEFAULT_DELEGATION_DEPTH),
            Err(OperatorError::UnknownInvite)
        ));
    }

    #[test]
    fn approve_rejects_a_request_for_an_unminted_invite() {
        let (op, _root, _dir) = operator();
        // A request built against an invite the operator never minted (an
        // attacker fabricating a nonce, or an invite from another mesh).
        let stray = InviteToken::mint_at(op.root_id(), "relay://rv", HOUR, T0);
        let device = Identity::generate();
        let req = JoinRequest::create(&device, "pc", vec![], &stray);
        assert!(matches!(
            op.approve_at(&req, T0, HOUR, DEFAULT_DELEGATION_DEPTH),
            Err(OperatorError::UnknownInvite)
        ));
    }

    #[test]
    fn revoke_bumps_the_floor_and_stamps_the_inventory() {
        let (op, root, _dir) = operator();
        let invite = op.invite_at("relay://rv", HOUR, T0);
        let device = Identity::generate();
        let req = JoinRequest::create(&device, "pc", vec![], &invite);
        let enrollment = op
            .approve_at(&req, T0, HOUR, DEFAULT_DELEGATION_DEPTH)
            .unwrap();

        // The device runs a gateway; its chain verifies before revocation.
        let gateway = Identity::generate();
        let gw_chain = enrollment
            .chain
            .extend_delegate(&device, gateway.entity_id())
            .unwrap();
        let reg = RevocationRegistry::new();
        gw_chain
            .verify(gateway.entity_id(), root.entity_id(), &reg, 0)
            .expect("gateway chain verifies pre-revoke");

        op.revoke_at(device.entity_id(), 1, T0 + 1).unwrap();

        // Inventory shows revoked.
        let rec = &op.devices().unwrap()[0];
        assert_eq!(rec.revoked_at, Some(T0 + 1));

        // Enforcement: the persisted floor, applied to a fresh registry, makes
        // the gateway chain fail verify — end-to-end revoke → deny.
        let enforced = RevocationRegistry::new();
        RevocationStore::load(&op.revocation_path)
            .unwrap()
            .apply_to(&enforced);
        assert!(gw_chain
            .verify(gateway.entity_id(), root.entity_id(), &enforced, 0)
            .is_err());
    }

    // A revoked device that was never in the inventory still gets its floor
    // raised (inventory stamp is best-effort).
    #[test]
    fn revoke_of_an_unknown_device_still_raises_the_floor() {
        let (op, _root, _dir) = operator();
        let ghost = Identity::generate();
        op.revoke(ghost.entity_id()).unwrap();
        let floor = RevocationStore::load(&op.revocation_path)
            .unwrap()
            .floor(ghost.entity_id());
        assert!(floor >= 1);
        assert!(op.devices().unwrap().is_empty());
    }

    #[test]
    fn forget_prunes_the_inventory_without_touching_floors() {
        let (op, _root, _dir) = operator();
        let invite = op.invite_at("relay://rv", HOUR, T0);
        let device = Identity::generate();
        let req = JoinRequest::create(&device, "pc", vec![], &invite);
        op.approve_at(&req, T0, HOUR, DEFAULT_DELEGATION_DEPTH)
            .unwrap();

        assert!(op.forget(device.entity_id()).unwrap());
        assert!(op.devices().unwrap().is_empty());
        // No floor was raised by forgetting.
        assert_eq!(
            RevocationStore::load(&op.revocation_path)
                .unwrap()
                .floor(device.entity_id()),
            0
        );
        assert!(!op.forget(device.entity_id()).unwrap());
    }

    #[test]
    fn pending_lists_and_prunes_invites() {
        let (op, _root, _dir) = operator();
        op.invite_at("relay://a", HOUR, T0);
        op.invite_at("relay://b", HOUR, T0);
        assert_eq!(op.pending_invites(T0).len(), 2);
        // After expiry they prune out.
        assert!(op.pending_invites(T0 + 3600).is_empty());
    }

    // The RPC-handler tests use the clock-reading `invite` (not `invite_at`)
    // because `handle_join_request` approves against the real clock.

    #[test]
    fn handle_join_request_admits_and_the_device_verifies() {
        let (op, root, _dir) = operator();
        let invite = op.invite("relay://rv", HOUR);
        let device = Identity::generate();
        let req = JoinRequest::create(&device, "pc", vec![], &invite);

        // Server: request bytes → outcome bytes.
        let outcome_bytes = op.handle_join_request(&req.to_bytes(), HOUR, DEFAULT_DELEGATION_DEPTH);
        // Device: parse + verify the grant anchors at the invited root + binds
        // to this device.
        let chain = JoinOutcome::from_bytes(&outcome_bytes)
            .unwrap()
            .into_chain(device.entity_id(), &invite.root)
            .expect("device accepts its grant");
        assert_eq!(&chain.leaf(), device.entity_id());
        assert_eq!(&chain.root(), root.entity_id());
        assert_eq!(op.devices().unwrap().len(), 1);
    }

    #[test]
    fn handle_join_request_rejects_malformed_bytes() {
        let (op, _root, _dir) = operator();
        let outcome = JoinOutcome::from_bytes(&op.handle_join_request(
            b"garbage",
            HOUR,
            DEFAULT_DELEGATION_DEPTH,
        ))
        .unwrap();
        assert!(matches!(outcome, JoinOutcome::Rejected { code, .. } if code == reject::MALFORMED));
    }

    #[test]
    fn handle_join_request_rejects_an_unminted_invite() {
        let (op, _root, _dir) = operator();
        // A request against an invite this operator never minted.
        let stray = InviteToken::mint(op.root_id(), "relay://rv", HOUR);
        let device = Identity::generate();
        let req = JoinRequest::create(&device, "pc", vec![], &stray);
        let outcome = JoinOutcome::from_bytes(&op.handle_join_request(
            &req.to_bytes(),
            HOUR,
            DEFAULT_DELEGATION_DEPTH,
        ))
        .unwrap();
        assert!(
            matches!(outcome, JoinOutcome::Rejected { code, .. } if code == reject::UNKNOWN_INVITE)
        );
    }

    #[test]
    fn handle_join_request_is_single_use() {
        let (op, _root, _dir) = operator();
        let invite = op.invite("relay://rv", HOUR);
        let device = Identity::generate();
        let req = JoinRequest::create(&device, "pc", vec![], &invite);
        let first = JoinOutcome::from_bytes(&op.handle_join_request(
            &req.to_bytes(),
            HOUR,
            DEFAULT_DELEGATION_DEPTH,
        ))
        .unwrap();
        assert!(matches!(first, JoinOutcome::Admitted { .. }));
        // Replay: the invite was retired, so the second attempt finds no invite.
        let second = JoinOutcome::from_bytes(&op.handle_join_request(
            &req.to_bytes(),
            HOUR,
            DEFAULT_DELEGATION_DEPTH,
        ))
        .unwrap();
        assert!(
            matches!(second, JoinOutcome::Rejected { code, .. } if code == reject::UNKNOWN_INVITE)
        );
    }
}
