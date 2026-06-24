//! Task lease (plan piece 2 / locked decision 1) — ownership of
//! *executing* a task, with failover on owner death.
//!
//! This is the **easy** lease: one task, at most one owner, AP all the
//! way down, no two contenders for the same id. It is a
//! [`ReservationFold`] claim at *task-id* granularity — emphatically
//! **not** Thunderdome's exclusive-capability `Active` (N contenders,
//! one resource, CP on commit). The two are different leases with
//! different consistency; conflating them is the central error the
//! plan is written to prevent, so the task lease lives here, over the
//! plain reservation primitive, and never touches the gang-claim
//! machinery.
//!
//! Failover falls out of the reservation TTL: the owner renews its
//! `Reserved` before `until_unix_us`; if it dies and stops renewing,
//! the deadline lapses and any node may take the lease over with a
//! fresh `Reserved` — no sweeper, no coordination.

use crate::adapter::net::behavior::fold::{
    ApplyOutcome, EnvelopeMeta, Fold, FoldError, FoldKind, NodeId, ReservationAnnouncement,
    ReservationFold, ReservationQuery, ReservationState, SignedAnnouncement, WireError,
};
use crate::adapter::net::identity::EntityKeypair;

use super::types::TaskId;

/// Outcome of [`TaskLease::acquire`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskLeaseOutcome {
    /// We now hold the lease — it was free, its previous deadline had
    /// lapsed, or it was already ours (a renew).
    Acquired,
    /// A live foreign lease holds it. Back off and retry; on owner
    /// death the deadline lapses and a later `acquire` takes over.
    Contended,
}

/// Error from a lease op: signing/encoding or a fold-runtime apply
/// failure (distinct from a clean [`TaskLeaseOutcome::Contended`]).
#[derive(Debug)]
pub enum TaskLeaseError {
    /// Signing / encoding the reservation announcement failed.
    Sign(WireError),
    /// The fold runtime refused the apply (decode / dispatch level).
    Apply(FoldError),
}

impl std::fmt::Display for TaskLeaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskLeaseError::Sign(e) => write!(f, "sign task-lease announcement: {e}"),
            TaskLeaseError::Apply(e) => write!(f, "apply task-lease announcement: {e}"),
        }
    }
}

impl std::error::Error for TaskLeaseError {}

impl From<WireError> for TaskLeaseError {
    fn from(e: WireError) -> Self {
        TaskLeaseError::Sign(e)
    }
}

impl From<FoldError> for TaskLeaseError {
    fn from(e: FoldError) -> Self {
        TaskLeaseError::Apply(e)
    }
}

/// A task-lease holder bound to a reservation fold + identity. The
/// task id is used directly as the
/// [`ResourceId`](crate::adapter::net::behavior::fold::ResourceId).
pub struct TaskLease<'a> {
    reservations: &'a Fold<ReservationFold>,
    keypair: &'a EntityKeypair,
    node_id: NodeId,
    generation: u64,
}

impl<'a> TaskLease<'a> {
    /// Bind a lease actor to a reservation fold. Generation starts at
    /// 1 (the fold treats a first announcement as the baseline, then
    /// requires strict-monotonic growth — so renew/release stay
    /// anti-reorder-correct).
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

    /// This actor's node id.
    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    /// Acquire — or renew — the lease for `task` until `until_unix_us`
    /// (wall-clock micros). [`TaskLeaseOutcome::Acquired`] if we hold
    /// it after the CAS; [`TaskLeaseOutcome::Contended`] if a live
    /// foreign lease refused us. Renewing (re-acquiring our own live
    /// lease) returns `Acquired`.
    pub fn acquire(
        &mut self,
        task: TaskId,
        until_unix_us: u64,
    ) -> Result<TaskLeaseOutcome, TaskLeaseError> {
        let outcome = self.apply(
            task,
            ReservationState::Reserved {
                holder: self.node_id,
                until_unix_us,
            },
        )?;
        Ok(match outcome {
            ApplyOutcome::Inserted | ApplyOutcome::Replaced => TaskLeaseOutcome::Acquired,
            ApplyOutcome::Rejected => TaskLeaseOutcome::Contended,
        })
    }

    /// Release the lease for `task`. Returns `true` if we were the
    /// holder and freed it; `false` if the fold refused (we weren't
    /// the holder — a foreign release is rejected).
    pub fn release(&mut self, task: TaskId) -> Result<bool, TaskLeaseError> {
        let outcome = self.apply(task, ReservationState::Free)?;
        Ok(matches!(
            outcome,
            ApplyOutcome::Inserted | ApplyOutcome::Replaced
        ))
    }

    /// The node currently holding `task`'s lease, if any (a `Reserved`
    /// holder). `None` if free / unheld.
    pub fn current_holder(&self, task: TaskId) -> Option<NodeId> {
        self.reservations
            .query(ReservationQuery::State(task))
            .first()
            .and_then(|(_, state)| state.holder())
    }

    fn apply(
        &mut self,
        task: TaskId,
        state: ReservationState,
    ) -> Result<ApplyOutcome, TaskLeaseError> {
        let gen = self.generation;
        self.generation += 1;
        let ann = SignedAnnouncement::sign(
            self.keypair,
            ReservationFold::KIND_ID,
            0,
            self.node_id,
            gen,
            EnvelopeMeta::default(),
            ReservationAnnouncement {
                resource_id: task,
                state,
            },
        )?;
        Ok(self.reservations.apply(ann)?)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::adapter::net::current_timestamp_micros;
    use crate::adapter::net::identity::EntityKeypair;

    fn new_reservations() -> Fold<ReservationFold> {
        Fold::with_sweep_interval(Duration::ZERO)
    }

    fn fresh() -> u64 {
        current_timestamp_micros() + 60_000_000
    }

    #[test]
    fn acquire_then_renew_then_release() {
        let fold = new_reservations();
        let kp = EntityKeypair::generate();
        let node = kp.entity_id().node_id();
        let mut lease = TaskLease::new(&fold, &kp, node);

        assert_eq!(lease.acquire(0x1A, fresh()).unwrap(), TaskLeaseOutcome::Acquired);
        assert_eq!(lease.current_holder(0x1A), Some(node));
        // Renew (re-acquire our own live lease).
        assert_eq!(lease.acquire(0x1A, fresh()).unwrap(), TaskLeaseOutcome::Acquired);
        // Release.
        assert!(lease.release(0x1A).unwrap());
        assert_eq!(lease.current_holder(0x1A), None);
    }

    #[test]
    fn second_owner_is_contended_while_lease_is_live() {
        let fold = new_reservations();
        let a = EntityKeypair::generate();
        let b = EntityKeypair::generate();
        let (na, nb) = (a.entity_id().node_id(), b.entity_id().node_id());
        let mut la = TaskLease::new(&fold, &a, na);
        let mut lb = TaskLease::new(&fold, &b, nb);

        assert_eq!(la.acquire(5, fresh()).unwrap(), TaskLeaseOutcome::Acquired);
        // B can't take a live lease.
        assert_eq!(lb.acquire(5, fresh()).unwrap(), TaskLeaseOutcome::Contended);
        // A still holds it; B's release is refused.
        assert_eq!(la.current_holder(5), Some(na));
        assert!(!lb.release(5).unwrap());
    }

    /// Failover: the owner dies (its `Reserved` deadline lapses) and a
    /// new owner takes the lease over — no sweeper, no coordination.
    #[test]
    fn expired_lease_fails_over_to_a_new_owner() {
        let fold = new_reservations();
        let a = EntityKeypair::generate();
        let b = EntityKeypair::generate();
        let (na, nb) = (a.entity_id().node_id(), b.entity_id().node_id());
        let mut la = TaskLease::new(&fold, &a, na);
        let mut lb = TaskLease::new(&fold, &b, nb);

        // A acquires with an ALREADY-EXPIRED deadline (simulating a
        // dead owner whose lease lapsed) then stops renewing.
        let expired = current_timestamp_micros().saturating_sub(60_000_000);
        assert_eq!(la.acquire(9, expired).unwrap(), TaskLeaseOutcome::Acquired);
        // B takes over the lapsed lease.
        assert_eq!(lb.acquire(9, fresh()).unwrap(), TaskLeaseOutcome::Acquired);
        assert_eq!(lb.current_holder(9), Some(nb));
    }
}
