//! Gang-claim commit step (plan §2 step 4): the single
//! `ReservationFold` CAS that actually grants — or refuses — an
//! island. A single-island gang is exactly one of these (plan §3,
//! locked decision 1): the island *is* the `ResourceId`, so the
//! claim is one existing reservation CAS, atomic and deadlock-free
//! with zero new protocol.
//!
//! The lifecycle a single-island job walks is `Reserved` (claim) →
//! `Active` (run) → `Free` (release), each one a local-AP CAS on the
//! reservation fold. The `→ Active` edge gets quorum-gating + a
//! fencing epoch in Phase D; here it is a plain optimistic CAS like
//! the others.

use crate::adapter::net::behavior::fold::{
    ApplyOutcome, EnvelopeMeta, Fold, FoldError, FoldKind, IslandId, JobId, NodeId,
    ReservationAnnouncement, ReservationFold, ReservationState, SignedAnnouncement, WireError,
};
use crate::adapter::net::identity::EntityKeypair;

/// Outcome of a single-island claim attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimOutcome {
    /// We hold the island — the CAS installed our state (the island
    /// was `Free` / unheld / already ours).
    Won,
    /// Someone else holds it — the CAS was rejected. The caller
    /// re-runs the match pipeline and retries elsewhere.
    Lost,
}

impl ClaimOutcome {
    fn from_apply(outcome: ApplyOutcome) -> Self {
        match outcome {
            ApplyOutcome::Inserted | ApplyOutcome::Replaced => ClaimOutcome::Won,
            ApplyOutcome::Rejected => ClaimOutcome::Lost,
        }
    }
}

/// Error from a claim attempt: either the announcement couldn't be
/// signed/encoded, or the fold rejected the apply at the runtime
/// level (distinct from a clean state-machine `Lost`).
#[derive(Debug)]
pub enum ClaimError {
    /// Signing / encoding the reservation announcement failed.
    Sign(WireError),
    /// The fold runtime refused the apply (decode / dispatch level,
    /// not a state-machine rejection — that surfaces as
    /// [`ClaimOutcome::Lost`]).
    Apply(FoldError),
}

impl std::fmt::Display for ClaimError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClaimError::Sign(e) => write!(f, "sign reservation announcement: {e}"),
            ClaimError::Apply(e) => write!(f, "apply reservation announcement: {e}"),
        }
    }
}

impl std::error::Error for ClaimError {}

impl From<WireError> for ClaimError {
    fn from(e: WireError) -> Self {
        ClaimError::Sign(e)
    }
}

impl From<FoldError> for ClaimError {
    fn from(e: FoldError) -> Self {
        ClaimError::Apply(e)
    }
}

/// Build a signed `Reserved` claim for one island. `until_unix_us`
/// is the wall-clock-micros deadline after which a foreign publisher
/// may take the reservation over (the fold's TTL-takeover); size it
/// from the claim-round latency (plan open question 3).
pub fn reserve_announcement(
    keypair: &EntityKeypair,
    node_id: NodeId,
    generation: u64,
    island: IslandId,
    until_unix_us: u64,
) -> Result<SignedAnnouncement<ReservationAnnouncement>, WireError> {
    sign_state(
        keypair,
        node_id,
        generation,
        island,
        ReservationState::Reserved {
            holder: node_id,
            until_unix_us,
        },
    )
}

/// Build a signed `Active` transition for one island — the holder
/// starting `job_id` against it. Legal only from `Reserved{self}` or
/// `Free` per the reservation state machine.
pub fn activate_announcement(
    keypair: &EntityKeypair,
    node_id: NodeId,
    generation: u64,
    island: IslandId,
    job_id: JobId,
) -> Result<SignedAnnouncement<ReservationAnnouncement>, WireError> {
    sign_state(
        keypair,
        node_id,
        generation,
        island,
        ReservationState::Active {
            holder: node_id,
            job_id,
        },
    )
}

/// Build a signed `Free` transition for one island — the holder
/// releasing it.
pub fn release_announcement(
    keypair: &EntityKeypair,
    node_id: NodeId,
    generation: u64,
    island: IslandId,
) -> Result<SignedAnnouncement<ReservationAnnouncement>, WireError> {
    sign_state(
        keypair,
        node_id,
        generation,
        island,
        ReservationState::Free,
    )
}

fn sign_state(
    keypair: &EntityKeypair,
    node_id: NodeId,
    generation: u64,
    island: IslandId,
    state: ReservationState,
) -> Result<SignedAnnouncement<ReservationAnnouncement>, WireError> {
    SignedAnnouncement::sign(
        keypair,
        ReservationFold::KIND_ID,
        0, // class (pool) — reserved
        node_id,
        generation,
        EnvelopeMeta::default(),
        ReservationAnnouncement {
            resource_id: island,
            state,
        },
    )
}

/// Attempt to claim one island: build a `Reserved` CAS and apply it
/// to the reservation fold. [`ClaimOutcome::Won`] if the CAS
/// installed (island was `Free` / unheld), [`ClaimOutcome::Lost`] if
/// rejected (held by someone with a live reservation). A single-
/// island gang in one call.
pub fn single_island_claim(
    reservations: &Fold<ReservationFold>,
    keypair: &EntityKeypair,
    node_id: NodeId,
    generation: u64,
    island: IslandId,
    until_unix_us: u64,
) -> Result<ClaimOutcome, ClaimError> {
    let ann = reserve_announcement(keypair, node_id, generation, island, until_unix_us)?;
    Ok(ClaimOutcome::from_apply(reservations.apply(ann)?))
}

/// Transition a held island `Reserved{self} → Active{self}` to start
/// `job_id`. [`ClaimOutcome::Lost`] if we no longer hold it (e.g. a
/// TTL takeover landed between reserve and activate).
pub fn activate_island(
    reservations: &Fold<ReservationFold>,
    keypair: &EntityKeypair,
    node_id: NodeId,
    generation: u64,
    island: IslandId,
    job_id: JobId,
) -> Result<ClaimOutcome, ClaimError> {
    let ann = activate_announcement(keypair, node_id, generation, island, job_id)?;
    Ok(ClaimOutcome::from_apply(reservations.apply(ann)?))
}

/// Release a held island back to `Free`. [`ClaimOutcome::Lost`] if
/// we weren't the holder (a foreign release is rejected by the fold).
pub fn release_island(
    reservations: &Fold<ReservationFold>,
    keypair: &EntityKeypair,
    node_id: NodeId,
    generation: u64,
    island: IslandId,
) -> Result<ClaimOutcome, ClaimError> {
    let ann = release_announcement(keypair, node_id, generation, island)?;
    Ok(ClaimOutcome::from_apply(reservations.apply(ann)?))
}

/// A claiming actor and its target reservation fold — the identity
/// (`keypair` / `node_id`) plus the monotonic per-publisher
/// `generation` counter, bundled so the gang/commit calls don't
/// thread them as four separate positional args (which, being three
/// `&_`/`u64`s in a row, are easy to transpose at the call site).
///
/// Construct one per claiming task; the orchestrators
/// ([`acquire_gang`](super::acquire_gang),
/// [`commit_active`](super::commit_active)) advance the generation
/// internally so every announcement this actor emits stays
/// strictly-monotonic (the reservation fold's anti-reorder rule).
pub struct Claimant<'a> {
    pub(super) reservations: &'a Fold<ReservationFold>,
    pub(super) keypair: &'a EntityKeypair,
    pub(super) node_id: NodeId,
    pub(super) generation: u64,
}

impl<'a> Claimant<'a> {
    /// Build a claimant. The generation counter starts at 1 (the
    /// reservation fold treats a first announcement as the baseline
    /// regardless, then requires strict-monotonic growth).
    pub fn new(
        reservations: &'a Fold<ReservationFold>,
        keypair: &'a EntityKeypair,
        node_id: NodeId,
    ) -> Self {
        Self {
            reservations,
            keypair,
            node_id,
            generation: 1,
        }
    }

    /// Take the next generation, advancing the counter. The
    /// orchestrators thread `&mut self.generation` directly into their
    /// inner loops; this accessor exists for tests that need a fresh
    /// monotonic generation after a gang acquire (e.g. to release or
    /// activate the held islands), hence `#[cfg(test)]`.
    #[cfg(test)]
    pub(super) fn next_gen(&mut self) -> u64 {
        let g = self.generation;
        self.generation += 1;
        g
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::adapter::net::behavior::fold::{Fold, ReservationQuery, ReservationState};
    use crate::adapter::net::current_timestamp_micros;
    use crate::adapter::net::identity::EntityKeypair;

    fn new_reservations() -> Fold<ReservationFold> {
        Fold::with_sweep_interval(Duration::ZERO)
    }

    fn fresh_deadline() -> u64 {
        current_timestamp_micros() + 60_000_000
    }

    #[test]
    fn single_island_claim_wins_an_unheld_island() {
        let fold = new_reservations();
        let kp = EntityKeypair::generate();
        let node = kp.entity_id().node_id();
        let got = single_island_claim(&fold, &kp, node, 1, 0x10, fresh_deadline()).unwrap();
        assert_eq!(got, ClaimOutcome::Won);
        let state = fold.query(ReservationQuery::State(0x10));
        assert_eq!(state[0].1.holder(), Some(node));
    }

    #[test]
    fn second_claimant_loses_a_held_island() {
        let fold = new_reservations();
        let a = EntityKeypair::generate();
        let b = EntityKeypair::generate();
        let (na, nb) = (a.entity_id().node_id(), b.entity_id().node_id());

        assert_eq!(
            single_island_claim(&fold, &a, na, 1, 0x10, fresh_deadline()).unwrap(),
            ClaimOutcome::Won,
        );
        // B tries the same fresh-held island → Lost.
        assert_eq!(
            single_island_claim(&fold, &b, nb, 1, 0x10, fresh_deadline()).unwrap(),
            ClaimOutcome::Lost,
        );
        // A still holds it.
        assert_eq!(
            fold.query(ReservationQuery::State(0x10))[0].1.holder(),
            Some(na),
        );
    }

    #[test]
    fn full_lifecycle_reserve_activate_release() {
        let fold = new_reservations();
        let kp = EntityKeypair::generate();
        let node = kp.entity_id().node_id();

        assert_eq!(
            single_island_claim(&fold, &kp, node, 1, 0x10, fresh_deadline()).unwrap(),
            ClaimOutcome::Won,
        );
        assert_eq!(
            activate_island(&fold, &kp, node, 2, 0x10, 0x7B).unwrap(),
            ClaimOutcome::Won,
        );
        assert!(matches!(
            fold.query(ReservationQuery::State(0x10))[0].1,
            ReservationState::Active { job_id: 0x7B, .. }
        ));
        assert_eq!(
            release_island(&fold, &kp, node, 3, 0x10).unwrap(),
            ClaimOutcome::Won,
        );
        assert_eq!(
            fold.query(ReservationQuery::State(0x10))[0].1,
            ReservationState::Free,
        );
    }

    #[test]
    fn foreign_release_is_rejected_as_lost() {
        let fold = new_reservations();
        let a = EntityKeypair::generate();
        let b = EntityKeypair::generate();
        let (na, nb) = (a.entity_id().node_id(), b.entity_id().node_id());

        single_island_claim(&fold, &a, na, 1, 0x10, fresh_deadline()).unwrap();
        // B tries to release A's island → rejected.
        assert_eq!(
            release_island(&fold, &b, nb, 1, 0x10).unwrap(),
            ClaimOutcome::Lost,
        );
        assert_eq!(
            fold.query(ReservationQuery::State(0x10))[0].1.holder(),
            Some(na),
        );
    }
}
