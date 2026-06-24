//! Queue / retry / backpressure (plan piece 8 / Phase E) — the
//! workflow loop that ties the read pipeline to the commit.
//!
//! A `Reserved` reject loops back to the match: the world moves
//! between attempts (load shifts, islands free up, a contended island
//! is released), so each round re-runs [`match_islands`] rather than
//! retrying a stale candidate list. When no island can be secured
//! before the caller's deadline, the loop surfaces
//! [`StreamError::Backpressure`] — the substrate's existing
//! saturation signal — so scheduler queue-full and transport
//! queue-full are one thing the caller already knows how to handle.
//!
//! `now_us` + `backoff` are injected for deterministic tests (a fake
//! clock + a no-op), exactly as in [`super::acquire_gang`]; production
//! passes the crate's `current_timestamp_micros` and a jittered sleep.

use crate::adapter::net::behavior::fold::{
    CapabilityFold, Fold, IslandId, IslandTopologyFold, JobId, NodeId, ReservationFold,
};
use crate::adapter::net::identity::EntityKeypair;
use crate::adapter::net::stream::StreamError;

use super::claim::ClaimError;
use super::contention::claim_first_available;
use super::multi::{acquire_gang, GangClaim, GangOutcome};
use super::{match_islands, MatchCriteria};

/// What a successful schedule secured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scheduled {
    /// A single island (the common case — most jobs are whole-island).
    Single(IslandId),
    /// A multi-island gang, all islands held.
    Gang(Vec<IslandId>),
}

/// Failure from a schedule attempt.
#[derive(Debug)]
pub enum ScheduleError {
    /// No capacity could be secured before the deadline. The caller
    /// queues and retries later. Carries [`StreamError::Backpressure`]
    /// so gang-scheduler saturation and transport queue-full share one
    /// signal (plan piece 8: "reuses `StreamError::Backpressure`").
    Backpressure(StreamError),
    /// A claim attempt failed at the sign/apply level (distinct from a
    /// clean contention loss, which just drives a retry).
    Claim(ClaimError),
}

impl ScheduleError {
    fn backpressure() -> Self {
        ScheduleError::Backpressure(StreamError::Backpressure)
    }
}

impl std::fmt::Display for ScheduleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScheduleError::Backpressure(e) => write!(f, "gang scheduler saturated: {e}"),
            ScheduleError::Claim(e) => write!(f, "gang claim failed: {e}"),
        }
    }
}

impl std::error::Error for ScheduleError {}

impl From<ClaimError> for ScheduleError {
    fn from(e: ClaimError) -> Self {
        ScheduleError::Claim(e)
    }
}

/// Schedule a **single-island** job: re-match each round, reserve the
/// first available island, and on no-capacity / contention-loss back
/// off and retry until `deadline_us`. Returns the claimed island, or
/// [`ScheduleError::Backpressure`] when nothing could be secured in
/// time.
#[allow(clippy::too_many_arguments)]
pub fn schedule_single(
    capability_fold: &Fold<CapabilityFold>,
    topology_fold: &Fold<IslandTopologyFold>,
    reservations: &Fold<ReservationFold>,
    keypair: &EntityKeypair,
    node_id: NodeId,
    generation: &mut u64,
    criteria: &MatchCriteria,
    reserve_ttl_us: u64,
    deadline_us: u64,
    now_us: impl Fn() -> u64,
    mut backoff: impl FnMut(u32),
) -> Result<Scheduled, ScheduleError> {
    let mut attempt = 0u32;
    loop {
        // Re-match each round — the world moves between attempts.
        let islands = match_islands(capability_fold, topology_fold, criteria);
        if !islands.is_empty() {
            let until = now_us().saturating_add(reserve_ttl_us);
            if let Some(won) =
                claim_first_available(reservations, keypair, node_id, generation, &islands, until)?
            {
                return Ok(Scheduled::Single(won));
            }
        }
        // No capacity (empty match) or every matched island contended.
        if now_us() >= deadline_us {
            return Err(ScheduleError::backpressure());
        }
        backoff(attempt);
        attempt = attempt.saturating_add(1);
    }
}

/// Schedule a **multi-island gang** of `gang_size` islands: match for
/// candidates, take the top `gang_size` by the selection policy, and
/// acquire them all-or-none via [`acquire_gang`]. Returns the held
/// set, or [`ScheduleError::Backpressure`] when fewer than
/// `gang_size` islands match or the gang couldn't be assembled before
/// the deadline.
#[allow(clippy::too_many_arguments)]
pub fn schedule_gang(
    capability_fold: &Fold<CapabilityFold>,
    topology_fold: &Fold<IslandTopologyFold>,
    reservations: &Fold<ReservationFold>,
    keypair: &EntityKeypair,
    node_id: NodeId,
    generation: &mut u64,
    criteria: &MatchCriteria,
    job: JobId,
    gang_size: usize,
    reserve_ttl_us: u64,
    deadline_us: u64,
    now_us: impl Fn() -> u64 + Copy,
    backoff: impl FnMut(u32),
) -> Result<Scheduled, ScheduleError> {
    // Match for candidates and take the top `gang_size` by selection.
    let mut candidates = match_islands(capability_fold, topology_fold, criteria);
    if candidates.len() < gang_size {
        // Not enough islands match right now → saturation.
        return Err(ScheduleError::backpressure());
    }
    candidates.truncate(gang_size);

    let claim = GangClaim {
        job,
        islands: candidates,
        deadline_us,
    };
    match acquire_gang(
        reservations,
        keypair,
        node_id,
        generation,
        &claim,
        reserve_ttl_us,
        now_us,
        backoff,
    )? {
        GangOutcome::Held(islands) => Ok(Scheduled::Gang(islands)),
        GangOutcome::DeadlineExceeded => Err(ScheduleError::backpressure()),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use super::*;
    use crate::adapter::net::behavior::fold::{
        CapabilityFilter, CapabilityMembership, CapabilityQuery, EnvelopeMeta, FoldKind, GpuSet,
        IslandRecord, NodeState, ReservationQuery, SignedAnnouncement,
    };
    use crate::adapter::net::behavior::gang::{
        single_island_claim, NumericFilter, SelectionPolicy,
    };
    use crate::adapter::net::current_timestamp_micros;

    fn new_fold<K: FoldKind>() -> Fold<K> {
        Fold::with_sweep_interval(Duration::ZERO)
    }

    fn announce_capability(fold: &Fold<CapabilityFold>, kp: &EntityKeypair, node: u64) {
        let membership = CapabilityMembership {
            class_hash: 0x67_70_75,
            tags: vec!["gpu:h100".into()],
            hardware: None,
            state: NodeState::Idle,
            region: None,
            price_quote: None,
            reflex_addr: None,
            allowed_nodes: Vec::new(),
            allowed_subnets: Vec::new(),
            allowed_groups: Vec::new(),
            metadata: BTreeMap::new(),
        };
        fold.apply(
            SignedAnnouncement::sign(
                kp,
                CapabilityFold::KIND_ID,
                membership.class_hash,
                node,
                1,
                EnvelopeMeta::default(),
                membership,
            )
            .unwrap(),
        )
        .unwrap();
    }

    fn announce_island(
        fold: &Fold<IslandTopologyFold>,
        kp: &EntityKeypair,
        node: u64,
        id: IslandId,
    ) {
        let record = IslandRecord {
            id,
            gpus: GpuSet::new(vec![0, 1, 2, 3, 4, 5, 6, 7]),
            host: node,
            warm_models: vec![0xA1],
            load: 0.2,
            p50_latency_us: 1_000,
        };
        fold.apply(
            SignedAnnouncement::sign(
                kp,
                IslandTopologyFold::KIND_ID,
                0,
                node,
                1,
                EnvelopeMeta::default(),
                record,
            )
            .unwrap(),
        )
        .unwrap();
    }

    fn criteria() -> MatchCriteria {
        MatchCriteria {
            capability: CapabilityQuery::Composite(CapabilityFilter {
                tags_all: vec!["gpu:h100".into()],
                ..Default::default()
            }),
            numeric: NumericFilter {
                min_gpus: 8,
                ..Default::default()
            },
            selection: SelectionPolicy::LeastLoaded,
            prefer_warm_model: None,
        }
    }

    fn fresh() -> u64 {
        current_timestamp_micros() + 60_000_000
    }

    #[test]
    fn schedule_single_claims_when_capacity_exists() {
        let caps = new_fold::<CapabilityFold>();
        let topo = new_fold::<IslandTopologyFold>();
        let res = new_fold::<ReservationFold>();
        let kp = EntityKeypair::generate();
        let node = kp.entity_id().node_id();
        announce_capability(&caps, &kp, node);
        announce_island(&topo, &kp, node, 0xA0);

        let mut gen = 1;
        let got = schedule_single(
            &caps, &topo, &res, &kp, node, &mut gen, &criteria(), 60_000_000, fresh(),
            current_timestamp_micros, |_| {},
        )
        .unwrap();
        assert_eq!(got, Scheduled::Single(0xA0));
    }

    #[test]
    fn schedule_single_surfaces_backpressure_when_no_capacity() {
        // No islands announced → match is always empty → after the
        // deadline, backpressure.
        let caps = new_fold::<CapabilityFold>();
        let topo = new_fold::<IslandTopologyFold>();
        let res = new_fold::<ReservationFold>();
        let kp = EntityKeypair::generate();
        let node = kp.entity_id().node_id();
        announce_capability(&caps, &kp, node); // capability but no island

        // Injected clock: starts at 1, deadline 3 → a couple rounds
        // then give up.
        let clock = AtomicU64::new(1);
        let mut gen = 1;
        let err = schedule_single(
            &caps, &topo, &res, &kp, node, &mut gen, &criteria(), 60_000_000, 3,
            || clock.fetch_add(1, Ordering::Relaxed), |_| {},
        )
        .unwrap_err();
        assert!(
            matches!(err, ScheduleError::Backpressure(StreamError::Backpressure)),
            "no capacity must surface as StreamError::Backpressure, got {err:?}",
        );
    }

    #[test]
    fn schedule_single_retries_and_wins_after_a_contended_island_frees() {
        let caps = new_fold::<CapabilityFold>();
        let topo = new_fold::<IslandTopologyFold>();
        let res = new_fold::<ReservationFold>();
        let kp = EntityKeypair::generate();
        let node = kp.entity_id().node_id();
        announce_capability(&caps, &kp, node);
        announce_island(&topo, &kp, node, 0xA0);

        // A competitor holds the only matching island.
        let other = EntityKeypair::generate();
        let on = other.entity_id().node_id();
        single_island_claim(&res, &other, on, 1, 0xA0, fresh()).unwrap();

        // The backoff hook frees it after the first failed round.
        let mut released = false;
        let backoff = |_a: u32| {
            if !released {
                crate::adapter::net::behavior::gang::release_island(&res, &other, on, 2, 0xA0)
                    .unwrap();
                released = true;
            }
        };

        let mut gen = 1;
        let got = schedule_single(
            &caps, &topo, &res, &kp, node, &mut gen, &criteria(), 60_000_000, u64::MAX,
            current_timestamp_micros, backoff,
        )
        .unwrap();
        assert_eq!(got, Scheduled::Single(0xA0));
        assert_eq!(
            res.query(ReservationQuery::State(0xA0))[0].1.holder(),
            Some(node),
        );
    }

    #[test]
    fn schedule_gang_acquires_top_k_islands() {
        let caps = new_fold::<CapabilityFold>();
        let topo = new_fold::<IslandTopologyFold>();
        let res = new_fold::<ReservationFold>();
        let kp = EntityKeypair::generate();
        let node = kp.entity_id().node_id();
        announce_capability(&caps, &kp, node);
        for id in [0xA0, 0xA1, 0xA2] {
            announce_island(&topo, &kp, node, id);
        }

        let mut gen = 1;
        let got = schedule_gang(
            &caps, &topo, &res, &kp, node, &mut gen, &criteria(), 42, 2, 60_000_000, fresh(),
            current_timestamp_micros, |_| {},
        )
        .unwrap();
        match got {
            Scheduled::Gang(islands) => assert_eq!(islands.len(), 2),
            other => panic!("expected a 2-island gang, got {other:?}"),
        }
    }

    #[test]
    fn schedule_gang_backpressures_when_too_few_islands_match() {
        let caps = new_fold::<CapabilityFold>();
        let topo = new_fold::<IslandTopologyFold>();
        let res = new_fold::<ReservationFold>();
        let kp = EntityKeypair::generate();
        let node = kp.entity_id().node_id();
        announce_capability(&caps, &kp, node);
        announce_island(&topo, &kp, node, 0xA0); // only ONE island

        let mut gen = 1;
        let err = schedule_gang(
            &caps, &topo, &res, &kp, node, &mut gen, &criteria(), 42, 3, 60_000_000, fresh(),
            current_timestamp_micros, |_| {},
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ScheduleError::Backpressure(StreamError::Backpressure)
        ));
    }
}
