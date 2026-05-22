//! `ReservationFold` — resource reservation lifecycle.
//!
//! Tracks reservation state across the mesh: each resource
//! (`u64` identifier) carries at most one entry whose payload
//! is a state-machine variant — `Free`, `Reserved`, or `Active`.
//! Holders advance their own resources through the state
//! machine; foreign publishers can claim only resources that are
//! `Free` or whose previous `Reserved` deadline has passed.
//!
//! ## State machine
//!
//! Three states:
//! - `Free` — nobody holds the resource.
//! - `Reserved { holder, until_unix_us }` — `holder` has claimed
//!   it; expires at `until_unix_us`.
//! - `Active { holder, job_id }` — `holder` has started running
//!   `job_id` against it.
//!
//! Legal transitions (publisher = `P`, existing holder = `H`):
//!
//! | from           | to             | who    | allowed |
//! |----------------|----------------|--------|---------|
//! | (no entry)     | any            | any    | yes     |
//! | Free           | Reserved       | any    | yes     |
//! | Free           | Active         | any    | yes (skip-Reserved fast path) |
//! | Free           | Free           | any    | no (no-op) |
//! | Reserved{H}    | Free           | P=H    | yes (release) |
//! | Reserved{H}    | Free           | P≠H    | no  (only holder releases) |
//! | Reserved{H}    | Reserved       | P=H    | yes (extend) |
//! | Reserved{H}    | Active{H}      | P=H    | yes (start job) |
//! | Reserved{H}    | Active{≠H}     | P=H    | no  (no transfer) |
//! | Reserved{H}    | Reserved       | P≠H, expired | yes (timeout takeover) |
//! | Reserved{H}    | Reserved       | P≠H, fresh   | no  (held) |
//! | Reserved{H}    | Active         | P≠H    | no |
//! | Active{H}      | Free           | P=H    | yes (complete) |
//! | Active{H}      | Free           | P≠H    | no  (only holder completes) |
//! | Active{H}      | Active{H}      | P=H    | yes (heartbeat / job_id change) |
//! | Active{H}      | Active{≠H}     | P=H    | no  (no transfer) |
//! | Active{H}      | Reserved       | P=H    | no  (illegal backward) |
//! | Active{H}      | any            | P≠H    | no |
//!
//! ## Anti-reorder
//!
//! Generation comparison kicks in only when publisher = prior
//! holder (`incoming.node_id == existing.node_id`). Cross-
//! publisher transitions (legitimate claim of a free / expired
//! resource) bypass generation comparison because the new
//! publisher's per-resource counter is independent of the prior
//! holder's. The Ed25519 verification at dispatch time
//! guarantees the publisher identity claim itself isn't forged;
//! the state-machine rules in [`ReservationFold::merge`] gate
//! whether the transition is legal.
//!
//! ## TTL-driven takeover
//!
//! The `until_unix_us` field on `Reserved` carries the
//! holder-declared deadline. After that wall-clock instant
//! passes, any publisher can issue a `Reserved{new_holder}`
//! announcement and merge replaces the stale reservation. This
//! is distinct from the fold-runtime's `expires_at` TTL (which
//! evicts the entry from the primary store altogether) — a
//! caller can deliberately set the fold TTL larger than the
//! reservation deadline so the Free state lingers for queries.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::adapter::net::current_timestamp_micros;

use super::state::{FoldEntry, FoldState, MergeAction, NoIndex, NodeId};
use super::{FoldKind, SignedAnnouncement};

/// Resource identifier — opaque `u64`. Callers map their
/// per-resource naming (host:GPU, pool/slot, ...) into this
/// space at the application layer.
pub type ResourceId = u64;

/// Job identifier — opaque `u64`. Stamped on `Active` state to
/// link the reservation to the running workload.
pub type JobId = u64;

/// Reservation lifecycle state. The fold's [`Payload`](ReservationFold)
/// is a [`ReservationAnnouncement`] that wraps one of these
/// alongside the `ResourceId` it pertains to.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ReservationState {
    /// Nobody holds the resource.
    Free,
    /// `holder` has claimed it; the claim expires at
    /// `until_unix_us` (wall-clock micros since epoch).
    Reserved {
        /// Publisher who claimed the resource.
        holder: NodeId,
        /// Wall-clock deadline after which any publisher may
        /// take over the resource via a fresh `Reserved`.
        until_unix_us: u64,
    },
    /// `holder` is running `job_id` against the resource.
    Active {
        /// Publisher running the job.
        holder: NodeId,
        /// Job identifier the holder stamps for cross-fold
        /// correlation (e.g. with the compute fold).
        job_id: JobId,
    },
}

impl ReservationState {
    /// Publisher currently holding the resource, if any.
    /// `Free` returns `None`.
    pub fn holder(&self) -> Option<NodeId> {
        match self {
            ReservationState::Free => None,
            ReservationState::Reserved { holder, .. } => Some(*holder),
            ReservationState::Active { holder, .. } => Some(*holder),
        }
    }
}

/// Wire payload for a single fold announcement: the
/// `ResourceId` being addressed plus the new
/// [`ReservationState`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReservationAnnouncement {
    /// Which resource this announcement is about. The fold's
    /// [`FoldKind::Key`](ReservationFold) is `ResourceId`,
    /// derived from this field via [`ReservationFold::key_for`].
    pub resource_id: ResourceId,
    /// New state to install (subject to the merge rules).
    pub state: ReservationState,
}

/// Query shapes the [`ReservationFold`] answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReservationQuery {
    /// Every resource currently in [`ReservationState::Free`].
    /// Useful for the scheduler to pick from.
    AllFree,
    /// Every resource currently held by `node_id` in either
    /// `Reserved` or `Active` state. Useful for cleanup when a
    /// node leaves the mesh.
    HeldBy(NodeId),
    /// State of a single resource. Returns at most one entry.
    State(ResourceId),
    /// Every resource currently in [`ReservationState::Active`].
    /// Useful for operator dashboards.
    AllActive,
}

/// Query result row.
pub type ReservationRow = (ResourceId, ReservationState);

/// Marker type for the [`FoldKind`] impl. `ReservationFold`
/// itself carries no state — that lives in the
/// [`super::Fold`] instance parameterized by this type.
#[derive(Debug)]
pub struct ReservationFold;

impl FoldKind for ReservationFold {
    /// Reserved built-in fold id `3` per the plan's
    /// "Reserved range" note in [`FoldKind::KIND_ID`].
    const KIND_ID: u16 = 3;
    const CHANNEL_PREFIX: &'static str = "fold:res:";
    /// 30-second runtime TTL — entries past this age are
    /// dropped from the primary store by the background
    /// sweeper. Distinct from the `until_unix_us` deadline on
    /// `Reserved`, which controls when a foreign publisher may
    /// take the reservation over.
    const DEFAULT_TTL: Duration = Duration::from_secs(30);

    type Key = ResourceId;
    type Payload = ReservationAnnouncement;
    type Query = ReservationQuery;
    type Result = Vec<ReservationRow>;
    type Index = NoIndex;

    fn key_for(_publisher: NodeId, payload: &Self::Payload) -> Self::Key {
        payload.resource_id
    }

    fn build_index() -> NoIndex {
        NoIndex
    }

    /// State-machine merge — see module doc for the full table.
    fn merge(
        existing: Option<&FoldEntry<Self>>,
        incoming: &SignedAnnouncement<Self::Payload>,
    ) -> MergeAction {
        let Some(entry) = existing else {
            // No prior entry → first announcement always
            // installs. The state machine starts from whatever
            // the publisher claims; subsequent transitions are
            // gated against this baseline.
            return MergeAction::Insert;
        };

        let publisher = incoming.node_id;
        let same_publisher = entry.node_id == publisher;

        // Same-publisher updates: gate on generation strictly
        // monotonic (the standard anti-reorder mechanism) AND
        // legal state transition.
        if same_publisher {
            if incoming.generation <= entry.generation {
                return MergeAction::Reject;
            }
            return if legal_same_publisher(&entry.payload.state, &incoming.payload.state, publisher)
            {
                MergeAction::Replace
            } else {
                MergeAction::Reject
            };
        }

        // Cross-publisher transitions: no generation check (the
        // new publisher's counter is independent of the prior
        // holder's). The Ed25519 verify at dispatch time
        // already authenticated the publisher claim; here we
        // only check the state-machine legality.
        match (&entry.payload.state, &incoming.payload.state) {
            // Free → anyone can claim.
            (ReservationState::Free, _) => MergeAction::Replace,
            // Reserved{H} → only legal foreign update is a
            // takeover of an expired reservation by a fresh
            // `Reserved` from the new holder.
            (
                ReservationState::Reserved {
                    until_unix_us: deadline,
                    ..
                },
                ReservationState::Reserved {
                    holder: new_holder, ..
                },
            ) => {
                if *new_holder == publisher && reservation_expired(*deadline) {
                    MergeAction::Replace
                } else {
                    MergeAction::Reject
                }
            }
            // Active{H} cannot be changed by anyone but H.
            (ReservationState::Active { .. }, _) => MergeAction::Reject,
            // Reserved{H} → anything-non-Reserved by ≠H is
            // rejected; only the holder can complete or release.
            (ReservationState::Reserved { .. }, _) => MergeAction::Reject,
        }
    }

    fn query(
        state: &FoldState<Self>,
        _index: &NoIndex,
        query: ReservationQuery,
    ) -> Vec<ReservationRow> {
        match query {
            ReservationQuery::AllFree => state
                .entries
                .iter()
                .filter(|(_, e)| matches!(e.payload.state, ReservationState::Free))
                .map(|(k, e)| (*k, e.payload.state.clone()))
                .collect(),
            ReservationQuery::AllActive => state
                .entries
                .iter()
                .filter(|(_, e)| matches!(e.payload.state, ReservationState::Active { .. }))
                .map(|(k, e)| (*k, e.payload.state.clone()))
                .collect(),
            ReservationQuery::HeldBy(node_id) => state
                .entries
                .iter()
                .filter(|(_, e)| e.payload.state.holder() == Some(node_id))
                .map(|(k, e)| (*k, e.payload.state.clone()))
                .collect(),
            ReservationQuery::State(resource_id) => state
                .entries
                .get(&resource_id)
                .map(|e| vec![(resource_id, e.payload.state.clone())])
                .unwrap_or_default(),
        }
    }
}

/// Same-publisher legal transitions. `publisher` is the actor
/// who emitted the incoming announcement; for same-publisher
/// updates the caller has already established that
/// `incoming.node_id == existing.node_id`, so any state
/// referring to a holder MUST name `publisher` (else the
/// publisher is trying to claim someone else's slot via their
/// own announcement, which is illegal).
fn legal_same_publisher(from: &ReservationState, to: &ReservationState, publisher: NodeId) -> bool {
    // Helper: does this state name `publisher` as holder (if it
    // has one)? `Free` has no holder and trivially passes.
    let same_holder = |s: &ReservationState| match s.holder() {
        None => true,
        Some(h) => h == publisher,
    };
    if !same_holder(to) {
        // Publisher trying to install a state with a foreign
        // holder via their own announcement. Reject — holder
        // claims must match the publisher identity.
        return false;
    }

    use ReservationState::*;
    match (from, to) {
        // Free → Free is a no-op (Reject keeps the gauge
        // honest; no metric churn).
        (Free, Free) => false,
        // Free → Reserved / Active: same publisher claiming a
        // free resource.
        (Free, Reserved { .. }) | (Free, Active { .. }) => true,
        // Reserved → anything by the holder is fine (extend,
        // start, release).
        (Reserved { .. }, _) => true,
        // Active → Free: complete.
        (Active { .. }, Free) => true,
        // Active → Active: heartbeat / job_id swap by the
        // holder is allowed (operators legitimately mutate
        // job_id when a single reservation drives sequential
        // jobs).
        (Active { .. }, Active { .. }) => true,
        // Active → Reserved: illegal backward transition. A
        // holder must release (Active → Free) before re-
        // reserving.
        (Active { .. }, Reserved { .. }) => false,
    }
}

/// Has the publisher-declared `until_unix_us` deadline passed?
/// Returns `true` if a foreign publisher is now allowed to take
/// over the reservation.
fn reservation_expired(until_unix_us: u64) -> bool {
    current_timestamp_micros() >= until_unix_us
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use crate::adapter::net::behavior::fold::{
        ApplyOutcome, EnvelopeMeta, Fold, FoldRegistry, SignedAnnouncement,
    };
    use crate::adapter::net::identity::EntityKeypair;

    /// Build a reservation announcement signed by `keypair`,
    /// claiming `node_id` as the publisher. The keypair's
    /// `EntityId` is what the dispatch path's `verify` checks;
    /// the `node_id` is the routing-layer publisher claim that
    /// merge rules compare against.
    fn sign_res(
        keypair: &EntityKeypair,
        node_id: NodeId,
        generation: u64,
        resource_id: ResourceId,
        state: ReservationState,
    ) -> SignedAnnouncement<ReservationAnnouncement> {
        SignedAnnouncement::sign(
            keypair,
            ReservationFold::KIND_ID,
            0, // class (pool) — currently unused; the wire field is reserved
            node_id,
            generation,
            EnvelopeMeta::default(),
            ReservationAnnouncement { resource_id, state },
        )
        .expect("sign succeeds")
    }

    /// Build a Fold<ReservationFold> with the background sweeper
    /// disabled — tests drive state synchronously without
    /// relying on the runtime's scheduler.
    fn new_fold() -> Fold<ReservationFold> {
        Fold::with_sweep_interval(Duration::ZERO)
    }

    fn fresh_deadline_us() -> u64 {
        current_timestamp_micros() + 60_000_000
    }

    fn expired_deadline_us() -> u64 {
        // SystemTime::now() may be near 0 in some sandboxed test
        // environments, so saturate at 0.
        current_timestamp_micros().saturating_sub(60_000_000)
    }

    // ----------------------------------------------------------
    // First-time installs: no prior entry, any state lands.
    // ----------------------------------------------------------

    #[test]
    fn first_announcement_installs_regardless_of_state() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        for (i, state) in [
            ReservationState::Free,
            ReservationState::Reserved {
                holder: 0xA,
                until_unix_us: fresh_deadline_us(),
            },
            ReservationState::Active {
                holder: 0xA,
                job_id: 7,
            },
        ]
        .into_iter()
        .enumerate()
        {
            let outcome = fold
                .apply(sign_res(&kp, 0xA, 1, i as u64, state))
                .expect("apply");
            assert_eq!(outcome, ApplyOutcome::Inserted);
        }
        assert_eq!(fold.metrics().applies_inserted(), 3);
    }

    // ----------------------------------------------------------
    // Same-publisher legal transitions.
    // ----------------------------------------------------------

    #[test]
    fn holder_can_reserve_then_activate_then_release() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        let r = 0x99;

        // Start: nothing.
        // gen=1: Reserved{0xA}.
        let outcome = fold
            .apply(sign_res(
                &kp,
                0xA,
                1,
                r,
                ReservationState::Reserved {
                    holder: 0xA,
                    until_unix_us: fresh_deadline_us(),
                },
            ))
            .unwrap();
        assert_eq!(outcome, ApplyOutcome::Inserted);

        // gen=2: holder extends. Replace.
        let outcome = fold
            .apply(sign_res(
                &kp,
                0xA,
                2,
                r,
                ReservationState::Reserved {
                    holder: 0xA,
                    until_unix_us: fresh_deadline_us() + 10_000_000,
                },
            ))
            .unwrap();
        assert_eq!(outcome, ApplyOutcome::Replaced);

        // gen=3: holder starts the job → Active.
        let outcome = fold
            .apply(sign_res(
                &kp,
                0xA,
                3,
                r,
                ReservationState::Active {
                    holder: 0xA,
                    job_id: 42,
                },
            ))
            .unwrap();
        assert_eq!(outcome, ApplyOutcome::Replaced);

        // gen=4: holder bumps the job_id (sequential job on
        // the same reservation). Legal heartbeat shape.
        let outcome = fold
            .apply(sign_res(
                &kp,
                0xA,
                4,
                r,
                ReservationState::Active {
                    holder: 0xA,
                    job_id: 43,
                },
            ))
            .unwrap();
        assert_eq!(outcome, ApplyOutcome::Replaced);

        // gen=5: holder releases.
        let outcome = fold
            .apply(sign_res(&kp, 0xA, 5, r, ReservationState::Free))
            .unwrap();
        assert_eq!(outcome, ApplyOutcome::Replaced);

        // Final state: Free.
        let q = fold.query(ReservationQuery::State(r));
        assert_eq!(q, vec![(r, ReservationState::Free)]);
    }

    #[test]
    fn holder_cannot_transition_active_back_to_reserved() {
        // Backward transition is illegal — operators must
        // release first, then re-reserve.
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        let r = 0x77;

        fold.apply(sign_res(
            &kp,
            0xA,
            1,
            r,
            ReservationState::Active {
                holder: 0xA,
                job_id: 1,
            },
        ))
        .unwrap();

        let outcome = fold
            .apply(sign_res(
                &kp,
                0xA,
                2,
                r,
                ReservationState::Reserved {
                    holder: 0xA,
                    until_unix_us: fresh_deadline_us(),
                },
            ))
            .unwrap();
        assert_eq!(outcome, ApplyOutcome::Rejected);

        // Active state must survive the rejected backward
        // transition.
        let q = fold.query(ReservationQuery::State(r));
        assert_eq!(
            q,
            vec![(
                r,
                ReservationState::Active {
                    holder: 0xA,
                    job_id: 1
                }
            )]
        );
    }

    #[test]
    fn publisher_cannot_install_state_naming_a_different_holder() {
        // The publisher's own announcement can't install a
        // Reserved{holder=B} from publisher=A. This guards
        // against a publisher claiming a resource on someone
        // else's behalf — the signature only authenticates
        // identity, not authorization to act on behalf of B.
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        let r = 0x55;

        let outcome = fold
            .apply(sign_res(
                &kp,
                0xA,
                1,
                r,
                ReservationState::Reserved {
                    holder: 0xB,
                    until_unix_us: fresh_deadline_us(),
                },
            ))
            .unwrap();
        // First install — accepted regardless. The state-
        // machine gate only applies to transitions; the
        // initial baseline lands as-is. (A separate higher-
        // level gate could refuse this; the fold runtime
        // doesn't because the holder-vs-publisher constraint
        // only makes sense in transition context.)
        assert_eq!(outcome, ApplyOutcome::Inserted);

        // Now publisher A tries to UPDATE to Reserved{holder=C}
        // — illegal, A is same publisher trying to install a
        // state whose holder is neither A nor consistent with
        // the prior holder claim.
        let outcome = fold
            .apply(sign_res(
                &kp,
                0xA,
                2,
                r,
                ReservationState::Reserved {
                    holder: 0xC,
                    until_unix_us: fresh_deadline_us(),
                },
            ))
            .unwrap();
        assert_eq!(outcome, ApplyOutcome::Rejected);
    }

    #[test]
    fn stale_generation_from_same_publisher_is_rejected() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        let r = 0x33;

        fold.apply(sign_res(
            &kp,
            0xA,
            5,
            r,
            ReservationState::Reserved {
                holder: 0xA,
                until_unix_us: fresh_deadline_us(),
            },
        ))
        .unwrap();

        // Replay at gen=5 → rejected.
        let outcome = fold
            .apply(sign_res(
                &kp,
                0xA,
                5,
                r,
                ReservationState::Reserved {
                    holder: 0xA,
                    until_unix_us: fresh_deadline_us() + 100,
                },
            ))
            .unwrap();
        assert_eq!(outcome, ApplyOutcome::Rejected);

        // Lower gen=4 → rejected.
        let outcome = fold
            .apply(sign_res(&kp, 0xA, 4, r, ReservationState::Free))
            .unwrap();
        assert_eq!(outcome, ApplyOutcome::Rejected);
    }

    // ----------------------------------------------------------
    // Cross-publisher rules.
    // ----------------------------------------------------------

    #[test]
    fn foreign_publisher_can_claim_a_free_resource() {
        let fold = new_fold();
        let kp_a = EntityKeypair::generate();
        let kp_b = EntityKeypair::generate();
        let r = 0x11;
        // Sample the deadline ONCE so the apply and the query
        // expectation compare the same micros value — re-
        // sampling `fresh_deadline_us()` between sign and
        // assertion lets the wall clock drift the byte-level
        // shape and fails the equality check.
        let deadline = fresh_deadline_us();

        // A publishes Free.
        fold.apply(sign_res(&kp_a, 0xA, 1, r, ReservationState::Free))
            .unwrap();

        // B claims it via Reserved{holder=B}.
        let outcome = fold
            .apply(sign_res(
                &kp_b,
                0xB,
                1,
                r,
                ReservationState::Reserved {
                    holder: 0xB,
                    until_unix_us: deadline,
                },
            ))
            .unwrap();
        assert_eq!(outcome, ApplyOutcome::Replaced);

        let q = fold.query(ReservationQuery::State(r));
        assert_eq!(
            q[0].1,
            ReservationState::Reserved {
                holder: 0xB,
                until_unix_us: deadline,
            }
        );
    }

    #[test]
    fn foreign_publisher_cannot_steal_a_fresh_reservation() {
        let fold = new_fold();
        let kp_a = EntityKeypair::generate();
        let kp_b = EntityKeypair::generate();
        let r = 0x22;

        // A reserves with a fresh deadline.
        fold.apply(sign_res(
            &kp_a,
            0xA,
            1,
            r,
            ReservationState::Reserved {
                holder: 0xA,
                until_unix_us: fresh_deadline_us(),
            },
        ))
        .unwrap();

        // B tries to claim it — rejected, still held.
        let outcome = fold
            .apply(sign_res(
                &kp_b,
                0xB,
                1,
                r,
                ReservationState::Reserved {
                    holder: 0xB,
                    until_unix_us: fresh_deadline_us(),
                },
            ))
            .unwrap();
        assert_eq!(outcome, ApplyOutcome::Rejected);

        // A still holds.
        let q = fold.query(ReservationQuery::State(r));
        assert_eq!(q[0].1.holder(), Some(0xA));
    }

    #[test]
    fn foreign_publisher_can_take_over_an_expired_reservation() {
        // The reservation's `until_unix_us` is in the past →
        // B's claim is now legal.
        let fold = new_fold();
        let kp_a = EntityKeypair::generate();
        let kp_b = EntityKeypair::generate();
        let r = 0x44;

        fold.apply(sign_res(
            &kp_a,
            0xA,
            1,
            r,
            ReservationState::Reserved {
                holder: 0xA,
                until_unix_us: expired_deadline_us(),
            },
        ))
        .unwrap();

        let outcome = fold
            .apply(sign_res(
                &kp_b,
                0xB,
                1,
                r,
                ReservationState::Reserved {
                    holder: 0xB,
                    until_unix_us: fresh_deadline_us(),
                },
            ))
            .unwrap();
        assert_eq!(outcome, ApplyOutcome::Replaced);

        let q = fold.query(ReservationQuery::State(r));
        assert_eq!(q[0].1.holder(), Some(0xB));
    }

    #[test]
    fn foreign_publisher_cannot_release_someone_elses_reservation() {
        // Third-party release would silently free a resource
        // the rightful holder is still working with — security
        // boundary.
        let fold = new_fold();
        let kp_a = EntityKeypair::generate();
        let kp_b = EntityKeypair::generate();
        let r = 0x66;

        fold.apply(sign_res(
            &kp_a,
            0xA,
            1,
            r,
            ReservationState::Reserved {
                holder: 0xA,
                until_unix_us: fresh_deadline_us(),
            },
        ))
        .unwrap();

        let outcome = fold
            .apply(sign_res(&kp_b, 0xB, 1, r, ReservationState::Free))
            .unwrap();
        assert_eq!(outcome, ApplyOutcome::Rejected);
    }

    #[test]
    fn foreign_publisher_cannot_change_active_state() {
        // Active is strictly holder-controlled — only A can
        // release / extend / heartbeat A's active reservation.
        let fold = new_fold();
        let kp_a = EntityKeypair::generate();
        let kp_b = EntityKeypair::generate();
        let r = 0x88;

        fold.apply(sign_res(
            &kp_a,
            0xA,
            1,
            r,
            ReservationState::Active {
                holder: 0xA,
                job_id: 7,
            },
        ))
        .unwrap();

        // B tries to release — rejected.
        let outcome = fold
            .apply(sign_res(&kp_b, 0xB, 1, r, ReservationState::Free))
            .unwrap();
        assert_eq!(outcome, ApplyOutcome::Rejected);

        // B tries to take over via Reserved — rejected (Active
        // cannot be foreign-overridden even if expired; there's
        // no "deadline" on Active).
        let outcome = fold
            .apply(sign_res(
                &kp_b,
                0xB,
                1,
                r,
                ReservationState::Reserved {
                    holder: 0xB,
                    until_unix_us: fresh_deadline_us(),
                },
            ))
            .unwrap();
        assert_eq!(outcome, ApplyOutcome::Rejected);
    }

    // ----------------------------------------------------------
    // Concurrent-claim semantics: deterministic winner via
    // apply-order. Whoever's apply reaches Replace first
    // becomes the holder; the loser sees the post-replace
    // state and gets Rejected (not-Free anymore).
    // ----------------------------------------------------------

    #[test]
    fn first_claim_wins_on_concurrent_reservation() {
        let fold = new_fold();
        let kp_a = EntityKeypair::generate();
        let kp_b = EntityKeypair::generate();
        let r = 0xAA;

        // Both A and B see Free, both attempt Reserved. The
        // fold's `apply` serializes on the state write lock —
        // whoever lands first wins, the other is rejected.
        fold.apply(sign_res(&kp_a, 0xA, 1, r, ReservationState::Free))
            .unwrap();

        // A reserves first.
        let outcome_a = fold
            .apply(sign_res(
                &kp_a,
                0xA,
                2,
                r,
                ReservationState::Reserved {
                    holder: 0xA,
                    until_unix_us: fresh_deadline_us(),
                },
            ))
            .unwrap();
        assert_eq!(outcome_a, ApplyOutcome::Replaced);

        // B sees the reserved-by-A state, tries to claim →
        // Rejected.
        let outcome_b = fold
            .apply(sign_res(
                &kp_b,
                0xB,
                1,
                r,
                ReservationState::Reserved {
                    holder: 0xB,
                    until_unix_us: fresh_deadline_us(),
                },
            ))
            .unwrap();
        assert_eq!(outcome_b, ApplyOutcome::Rejected);
    }

    // ----------------------------------------------------------
    // TTL-driven entry expiry: the fold runtime sweeps entries
    // past `expires_at`. A reservation with very short
    // `ttl_secs` falls out of the store entirely, distinct
    // from the `until_unix_us` takeover mechanism.
    // ----------------------------------------------------------

    #[test]
    fn runtime_ttl_sweeps_stale_reservation_entries() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        let r = 0xCC;

        // Build an announcement with ttl_secs=0 so the runtime
        // marks it expired immediately.
        let ann = SignedAnnouncement::sign(
            &kp,
            ReservationFold::KIND_ID,
            0,
            0xA,
            1,
            EnvelopeMeta {
                ttl_secs: Some(0),
                ..Default::default()
            },
            ReservationAnnouncement {
                resource_id: r,
                state: ReservationState::Reserved {
                    holder: 0xA,
                    until_unix_us: fresh_deadline_us(),
                },
            },
        )
        .unwrap();
        fold.apply(ann).unwrap();

        assert_eq!(fold.metrics().entries(), 1);
        std::thread::sleep(Duration::from_millis(10));
        let n = fold.sweep_expired_now();
        assert_eq!(n, 1);
        assert_eq!(fold.metrics().entries(), 0);
        assert_eq!(fold.metrics().expiries(), 1);
    }

    // ----------------------------------------------------------
    // Query variants.
    // ----------------------------------------------------------

    #[test]
    fn query_returns_only_free_resources_for_all_free() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        fold.apply(sign_res(&kp, 0xA, 1, 1, ReservationState::Free))
            .unwrap();
        fold.apply(sign_res(
            &kp,
            0xA,
            1,
            2,
            ReservationState::Reserved {
                holder: 0xA,
                until_unix_us: fresh_deadline_us(),
            },
        ))
        .unwrap();
        fold.apply(sign_res(
            &kp,
            0xA,
            1,
            3,
            ReservationState::Active {
                holder: 0xA,
                job_id: 7,
            },
        ))
        .unwrap();
        fold.apply(sign_res(&kp, 0xA, 1, 4, ReservationState::Free))
            .unwrap();

        let free: Vec<_> = fold
            .query(ReservationQuery::AllFree)
            .into_iter()
            .map(|(id, _)| id)
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        let mut sorted = free;
        sorted.sort();
        assert_eq!(sorted, vec![1, 4]);
    }

    #[test]
    fn query_returns_only_active_resources_for_all_active() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        fold.apply(sign_res(&kp, 0xA, 1, 1, ReservationState::Free))
            .unwrap();
        fold.apply(sign_res(
            &kp,
            0xA,
            1,
            2,
            ReservationState::Active {
                holder: 0xA,
                job_id: 7,
            },
        ))
        .unwrap();
        fold.apply(sign_res(
            &kp,
            0xA,
            1,
            3,
            ReservationState::Active {
                holder: 0xA,
                job_id: 8,
            },
        ))
        .unwrap();

        let active_ids: std::collections::HashSet<_> = fold
            .query(ReservationQuery::AllActive)
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(active_ids, [2, 3].into_iter().collect());
    }

    #[test]
    fn query_held_by_finds_reserved_and_active_resources() {
        let fold = new_fold();
        let kp_a = EntityKeypair::generate();
        let kp_b = EntityKeypair::generate();

        fold.apply(sign_res(
            &kp_a,
            0xA,
            1,
            1,
            ReservationState::Reserved {
                holder: 0xA,
                until_unix_us: fresh_deadline_us(),
            },
        ))
        .unwrap();
        fold.apply(sign_res(
            &kp_a,
            0xA,
            1,
            2,
            ReservationState::Active {
                holder: 0xA,
                job_id: 7,
            },
        ))
        .unwrap();
        fold.apply(sign_res(
            &kp_b,
            0xB,
            1,
            3,
            ReservationState::Reserved {
                holder: 0xB,
                until_unix_us: fresh_deadline_us(),
            },
        ))
        .unwrap();

        let held_by_a: std::collections::HashSet<_> = fold
            .query(ReservationQuery::HeldBy(0xA))
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(held_by_a, [1, 2].into_iter().collect());

        let held_by_b: Vec<_> = fold
            .query(ReservationQuery::HeldBy(0xB))
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(held_by_b, vec![3]);
    }

    #[test]
    fn reservation_fold_plugs_into_registry_and_dispatches_signed_envelopes() {
        // The runtime is fold-agnostic; this confirms
        // ReservationFold composes with the FoldRegistry +
        // dispatch path on top.
        let registry = FoldRegistry::new();
        let fold: Arc<Fold<ReservationFold>> = Arc::new(new_fold());
        registry.register(fold.clone());

        let kp = EntityKeypair::generate();
        let ann = sign_res(
            &kp,
            0xA,
            1,
            0x123,
            ReservationState::Reserved {
                holder: 0xA,
                until_unix_us: fresh_deadline_us(),
            },
        );
        let bytes = ann.encode().expect("encode");
        let outcome = registry.dispatch(&bytes, kp.entity_id()).expect("dispatch");
        assert_eq!(outcome, ApplyOutcome::Inserted);
    }
}
