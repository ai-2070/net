//! Phase 4a — `RoutingFold` (additive).
//!
//! The new fold-flavored routing surface: each destination
//! ([`NodeId`]) carries one entry whose payload is the best
//! known route to it. Multiple publishers can announce routes
//! to the same destination; merge keeps the lowest-metric entry.
//!
//! ## Phase 4a vs 4b
//!
//! This commit ships the new fold as **pure-additive** —
//! callers of the legacy [`RoutingTable`](super::super::super::route::RoutingTable)
//! are NOT yet rewired and the legacy module is NOT deleted.
//! That's Phase 4b (next session): the cutover spans ~50 call
//! sites across `route.rs`, `router.rs`, `reroute.rs`,
//! `mesh.rs`, and `mod.rs`, plus the pingwave packet repurpose
//! into `SignedAnnouncement<RouteAnnouncement>` envelopes. Per
//! the stripped plan, that's an atomic same-PR cutover and
//! deserves a focused session.
//!
//! For now [`RoutingFold`] coexists with the legacy table —
//! callers that want the new shape can wire it up via the
//! framework's standard recipe (`Fold::new` → `registry.register` →
//! `node.set_fold_router`). Nothing depends on it yet.
//!
//! ## Merge semantics
//!
//! Lower metric wins. Ties break on freshness (the incoming
//! announcement displaces an equal-metric existing entry so a
//! stale route doesn't pin a route slot forever). Generation
//! is consulted only for SAME-publisher updates as the anti-
//! reorder mechanism; cross-publisher routes compete strictly
//! on metric.
//!
//! ## TTL
//!
//! `DEFAULT_TTL = 300s` matches the plan's recommendation —
//! generous enough to absorb pingwave jitter, tight enough that
//! a node that genuinely drops off the mesh times out of the
//! routing table within five minutes. The fold runtime's
//! background sweeper enforces this.

use std::net::SocketAddr;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::state::{FoldEntry, FoldState, MergeAction, NoIndex, NodeId};
use super::{FoldKind, SignedAnnouncement};

/// Wire payload for one route announcement.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct RouteAnnouncement {
    /// Destination this route reaches.
    pub destination: NodeId,
    /// Next-hop socket address on the wire. Receivers send
    /// packets here to reach `destination`.
    pub next_hop: SocketAddr,
    /// Route quality metric — lower is better. The pingwave
    /// path uses `hop_count + base_offset` to encode "direct=1,
    /// 1-hop=2, etc.", but the fold layer is agnostic — any
    /// monotonic comparator works.
    pub metric: u32,
    /// Publisher of this route (the router that observed the
    /// path). May differ from `node_id` on the
    /// [`SignedAnnouncement`]: a relay can publish a route
    /// it learned via pingwaves; the signature commits to
    /// the relay's identity, `via` carries the observation
    /// attribution.
    pub via: NodeId,
}

/// Query shapes the [`RoutingFold`] answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutingQuery {
    /// Best route to a single destination. Returns at most
    /// one row.
    Lookup(NodeId),
    /// Every destination known to be reachable via this fold.
    AllDestinations,
    /// Every destination reachable through a specific
    /// next-hop address. Used by the reroute path when a
    /// next-hop fails: callers need the list of destinations
    /// that route through the failed hop so they can solicit
    /// alternates.
    ViaNextHop(SocketAddr),
}

/// One row in a [`RoutingQuery`] result: the destination plus
/// the chosen route. Always returns the entry the fold's merge
/// rule picked as the winner.
pub type RouteRow = (NodeId, RouteAnnouncement);

/// Marker type for the [`FoldKind`] impl.
#[derive(Debug)]
pub struct RoutingFold;

impl FoldKind for RoutingFold {
    /// Reserved built-in fold id `2` per the plan's
    /// "Reserved range" note in [`FoldKind::KIND_ID`].
    const KIND_ID: u16 = 2;
    const CHANNEL_PREFIX: &'static str = "fold:route:";
    /// 5-minute TTL matches the plan; the background sweeper
    /// removes routes that haven't been refreshed by a
    /// pingwave / re-announcement within the window.
    const DEFAULT_TTL: Duration = Duration::from_secs(300);

    type Key = NodeId;
    type Payload = RouteAnnouncement;
    type Query = RoutingQuery;
    type Result = Vec<RouteRow>;
    type Index = NoIndex;

    fn key_for(_publisher: NodeId, payload: &Self::Payload) -> Self::Key {
        payload.destination
    }

    fn build_index() -> NoIndex {
        NoIndex
    }

    /// Lower-metric wins. Ties accept the incoming announcement
    /// so a fresher heartbeat at the same metric refreshes
    /// `expires_at` and prevents the route from timing out
    /// while a same-quality alternate is reachable. Same-
    /// publisher updates additionally require generation
    /// strictly monotonic (anti-reorder).
    fn merge(
        existing: Option<&FoldEntry<Self>>,
        incoming: &SignedAnnouncement<Self::Payload>,
    ) -> MergeAction {
        let Some(entry) = existing else {
            return MergeAction::Insert;
        };

        // Same-publisher anti-reorder: reject stale generations
        // before checking metric so a replay of an old (lower-
        // metric) announcement from the same publisher doesn't
        // resurrect a stale route.
        if entry.node_id == incoming.node_id && incoming.generation <= entry.generation {
            return MergeAction::Reject;
        }

        // Metric comparison: lower wins, equal accepts (refreshes
        // `expires_at`). Strictly higher loses.
        if incoming.payload.metric <= entry.payload.metric {
            MergeAction::Replace
        } else {
            MergeAction::Reject
        }
    }

    fn query(
        state: &FoldState<Self>,
        _index: &NoIndex,
        query: RoutingQuery,
    ) -> Vec<RouteRow> {
        match query {
            RoutingQuery::Lookup(dest) => state
                .entries
                .get(&dest)
                .map(|e| vec![(dest, e.payload.clone())])
                .unwrap_or_default(),
            RoutingQuery::AllDestinations => state
                .entries
                .iter()
                .map(|(k, e)| (*k, e.payload.clone()))
                .collect(),
            RoutingQuery::ViaNextHop(addr) => state
                .entries
                .iter()
                .filter(|(_, e)| e.payload.next_hop == addr)
                .map(|(k, e)| (*k, e.payload.clone()))
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use crate::adapter::net::behavior::fold::{
        ApplyOutcome, Fold, FoldRegistry, SignedAnnouncement,
    };
    use crate::adapter::net::identity::EntityKeypair;

    fn addr(port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), port)
    }

    fn sign_route(
        keypair: &EntityKeypair,
        publisher_node: NodeId,
        generation: u64,
        dest: NodeId,
        next_hop: SocketAddr,
        metric: u32,
        via: NodeId,
    ) -> SignedAnnouncement<RouteAnnouncement> {
        SignedAnnouncement::sign(
            keypair,
            RoutingFold::KIND_ID,
            0,
            publisher_node,
            generation,
            0,
            None,
            0,
            RouteAnnouncement {
                destination: dest,
                next_hop,
                metric,
                via,
            },
        )
        .expect("sign succeeds")
    }

    fn new_fold() -> Fold<RoutingFold> {
        Fold::with_sweep_interval(Duration::ZERO)
    }

    #[test]
    fn first_announcement_installs_the_route() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        let outcome = fold
            .apply(sign_route(&kp, 0xAA, 1, 0x42, addr(7000), 1, 0xAA))
            .expect("apply");
        assert_eq!(outcome, ApplyOutcome::Inserted);
        let q = fold.query(RoutingQuery::Lookup(0x42));
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].1.next_hop, addr(7000));
        assert_eq!(q[0].1.metric, 1);
    }

    #[test]
    fn lower_metric_replaces_existing_route_regardless_of_publisher() {
        let fold = new_fold();
        let kp_a = EntityKeypair::generate();
        let kp_b = EntityKeypair::generate();

        fold.apply(sign_route(&kp_a, 0xAA, 1, 0x42, addr(7000), 5, 0xAA))
            .expect("first");
        let outcome = fold
            .apply(sign_route(&kp_b, 0xBB, 1, 0x42, addr(8000), 2, 0xBB))
            .expect("better");
        assert_eq!(outcome, ApplyOutcome::Replaced);

        let q = fold.query(RoutingQuery::Lookup(0x42));
        assert_eq!(q[0].1.metric, 2);
        assert_eq!(q[0].1.next_hop, addr(8000));
    }

    #[test]
    fn equal_metric_replaces_to_refresh_expires_at() {
        // The fold runtime stamps a fresh `expires_at` on every
        // accepted apply. Accepting equal-metric refreshes keep
        // a still-reachable destination from timing out when
        // its primary heartbeat happens less often than the
        // pingwave cadence — matches the legacy `RoutingTable`'s
        // `updated_at` refresh on same-metric arrivals.
        let fold = new_fold();
        let kp_a = EntityKeypair::generate();
        let kp_b = EntityKeypair::generate();

        fold.apply(sign_route(&kp_a, 0xAA, 1, 0x42, addr(7000), 3, 0xAA))
            .expect("first");
        let outcome = fold
            .apply(sign_route(&kp_b, 0xBB, 1, 0x42, addr(8000), 3, 0xBB))
            .expect("equal");
        assert_eq!(outcome, ApplyOutcome::Replaced);

        let q = fold.query(RoutingQuery::Lookup(0x42));
        // The incoming announcement wins on equal metric — the
        // legacy `RoutingTable::add_route_with_metric` kept the
        // existing entry's next_hop on equal metric and just
        // refreshed `updated_at`. The fold lets the new entry
        // win because the runtime-level expiry semantics are
        // tied to `expires_at` on the ENTRY, not the metric, so
        // either entry's "freshness wins" semantics is correct
        // — we pick "incoming wins" for symmetry with the
        // strict-monotonic merge path.
        assert_eq!(q[0].1.next_hop, addr(8000));
    }

    #[test]
    fn higher_metric_does_not_overwrite_existing_route() {
        let fold = new_fold();
        let kp_a = EntityKeypair::generate();
        let kp_b = EntityKeypair::generate();

        fold.apply(sign_route(&kp_a, 0xAA, 1, 0x42, addr(7000), 1, 0xAA))
            .expect("direct");
        let outcome = fold
            .apply(sign_route(&kp_b, 0xBB, 1, 0x42, addr(8000), 5, 0xBB))
            .expect("indirect");
        assert_eq!(outcome, ApplyOutcome::Rejected);

        let q = fold.query(RoutingQuery::Lookup(0x42));
        assert_eq!(q[0].1.metric, 1);
        assert_eq!(q[0].1.next_hop, addr(7000));
    }

    #[test]
    fn same_publisher_stale_generation_is_rejected_even_with_lower_metric() {
        // A replayed announcement (lower generation) from the
        // SAME publisher must lose to the current installed
        // entry, even if its metric is artificially lower.
        // This is the anti-reorder gate; a relay that replays
        // an old metric=1 announcement after the publisher
        // moved to metric=3 would otherwise re-pin the stale
        // route.
        let fold = new_fold();
        let kp = EntityKeypair::generate();

        fold.apply(sign_route(&kp, 0xAA, 5, 0x42, addr(7000), 3, 0xAA))
            .expect("gen=5");
        let outcome = fold
            .apply(sign_route(&kp, 0xAA, 3, 0x42, addr(7000), 1, 0xAA))
            .expect("stale gen=3");
        assert_eq!(outcome, ApplyOutcome::Rejected);

        let q = fold.query(RoutingQuery::Lookup(0x42));
        assert_eq!(q[0].1.metric, 3);
    }

    #[test]
    fn same_publisher_higher_generation_lower_metric_replaces() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();

        fold.apply(sign_route(&kp, 0xAA, 1, 0x42, addr(7000), 5, 0xAA))
            .expect("first");
        let outcome = fold
            .apply(sign_route(&kp, 0xAA, 2, 0x42, addr(7000), 1, 0xAA))
            .expect("better");
        assert_eq!(outcome, ApplyOutcome::Replaced);
        assert_eq!(
            fold.query(RoutingQuery::Lookup(0x42))[0].1.metric,
            1
        );
    }

    #[test]
    fn query_all_destinations_returns_every_installed_route() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        for (dest, port) in [(0x10, 7001), (0x11, 7002), (0x12, 7003)] {
            fold.apply(sign_route(&kp, 0xAA, 1, dest, addr(port), 1, 0xAA))
                .unwrap();
        }
        let mut dests: Vec<_> = fold
            .query(RoutingQuery::AllDestinations)
            .into_iter()
            .map(|(d, _)| d)
            .collect();
        dests.sort();
        assert_eq!(dests, vec![0x10, 0x11, 0x12]);
    }

    #[test]
    fn query_via_next_hop_finds_routes_through_a_specific_address() {
        // The reroute path uses this query: when next_hop=X
        // fails, which destinations route through it? Solicit
        // alternates for all of them.
        let fold = new_fold();
        let kp = EntityKeypair::generate();

        // 3 destinations through `addr(7000)`, 1 through `addr(8000)`.
        fold.apply(sign_route(&kp, 0xAA, 1, 0x10, addr(7000), 1, 0xAA))
            .unwrap();
        fold.apply(sign_route(&kp, 0xAA, 1, 0x11, addr(7000), 1, 0xAA))
            .unwrap();
        fold.apply(sign_route(&kp, 0xAA, 1, 0x12, addr(7000), 1, 0xAA))
            .unwrap();
        fold.apply(sign_route(&kp, 0xAA, 1, 0x13, addr(8000), 1, 0xAA))
            .unwrap();

        let via_7000: std::collections::HashSet<_> = fold
            .query(RoutingQuery::ViaNextHop(addr(7000)))
            .into_iter()
            .map(|(d, _)| d)
            .collect();
        assert_eq!(via_7000, [0x10, 0x11, 0x12].into_iter().collect());

        let via_8000: Vec<_> = fold
            .query(RoutingQuery::ViaNextHop(addr(8000)))
            .into_iter()
            .map(|(d, _)| d)
            .collect();
        assert_eq!(via_8000, vec![0x13]);

        let via_unknown = fold.query(RoutingQuery::ViaNextHop(addr(9999)));
        assert!(via_unknown.is_empty());
    }

    #[test]
    fn runtime_ttl_sweeps_stale_routes() {
        // Match the reservation-fold test shape: ttl_secs=0
        // entry, sweep, observe the eviction. Routing's
        // DEFAULT_TTL=300s would be tedious; the ttl override
        // makes the test deterministic.
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        let ann = SignedAnnouncement::sign(
            &kp,
            RoutingFold::KIND_ID,
            0,
            0xAA,
            1,
            0,
            Some(0),
            0,
            RouteAnnouncement {
                destination: 0x42,
                next_hop: addr(7000),
                metric: 1,
                via: 0xAA,
            },
        )
        .unwrap();
        fold.apply(ann).unwrap();
        assert_eq!(fold.metrics().entries(), 1);

        std::thread::sleep(Duration::from_millis(10));
        let n = fold.sweep_expired_now();
        assert_eq!(n, 1);
        assert_eq!(fold.metrics().expiries(), 1);
        assert!(fold.query(RoutingQuery::Lookup(0x42)).is_empty());
    }

    #[test]
    fn routing_fold_plugs_into_registry_and_dispatches_signed_envelopes() {
        let registry = FoldRegistry::new();
        let fold: Arc<Fold<RoutingFold>> = Arc::new(new_fold());
        registry.register(fold.clone());

        let kp = EntityKeypair::generate();
        let ann = sign_route(&kp, 0xAA, 1, 0x42, addr(7000), 1, 0xAA);
        let bytes = ann.encode().expect("encode");
        let outcome = registry.dispatch(&bytes, kp.entity_id()).expect("dispatch");
        assert_eq!(outcome, ApplyOutcome::Inserted);
    }
}
