//! Automatic rerouting policy.
//!
//! Watches the failure detector and updates the routing table when a peer
//! dies. When a peer recovers, restores the original route if it's better
//! than the current alternate.
//!
//! This module is wired into `MeshNode` via the `FailureDetector`'s
//! `on_failure` and `on_recovery` callbacks.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use dashmap::DashMap;

use super::behavior::proximity::ProximityGraph;
use super::route::{AlternateProvenance, RoutingTable};

/// Saved original route before reroute, for recovery.
///
/// We key on the peer's stable `node_id` rather than the original
/// `next_hop: SocketAddr`. After a NAT rebind / peer reconnect on
/// a different port / mobile-network change, `peer_addrs` reflects
/// the NEW address; an addr-keyed filter would return empty,
/// nothing would be restored, and the saved entry would persist
/// indefinitely. `on_recovery` re-resolves the current addr from
/// `peer_addrs[failed_node_id]` at recovery time, surviving NAT
/// rebinds.
struct SavedRoute {
    /// Node ID of the peer whose failure caused this reroute. The
    /// concrete `next_hop` SocketAddr at the time of saving may
    /// have changed (NAT rebind), so we re-resolve from
    /// `peer_addrs[failed_node_id]` at recovery time.
    failed_node_id: u64,
    /// The alternate we rerouted to
    alternate: SocketAddr,
    /// The transition token the reroute PRODUCED when it installed
    /// `alternate`, taken atomically from that transition. Recovery
    /// restores only while the table still carries exactly that token:
    /// anything else means a newer writer has spoken since, and a
    /// stale recovery must not undo it.
    alternate_token: u64,
}

/// The address out of an optional selected alternate. Only called on
/// the branch where an alternate was installed, so the `None` arm is
/// unreachable in practice; it degrades to an unspecified-but-inert
/// address rather than panicking on a future refactor.
fn alt_or(alternate: Option<(SocketAddr, AlternateProvenance)>) -> SocketAddr {
    alternate
        .map(|(addr, _)| addr)
        .unwrap_or_else(|| SocketAddr::from(([0, 0, 0, 0], 0)))
}

/// Policy that automatically reroutes traffic when peers fail.
///
/// When a peer is marked as failed by the `FailureDetector`:
/// 1. Find all routes whose next-hop is the failed peer's address
/// 2. For each, find an alternate peer (any other connected peer)
/// 3. Update the routing table to use the alternate
///
/// When the peer recovers:
/// 1. Restore the original routes (direct path is typically better)
pub struct ReroutePolicy {
    /// Routing table to update
    routing_table: Arc<RoutingTable>,
    /// Connected peers (node_id → addr mapping)
    peer_addrs: Arc<DashMap<u64, SocketAddr>>,
    /// Reverse address index (addr → node_id), used to confirm that a
    /// candidate next hop's address really is that peer's own direct
    /// session address before binding its identity into an installed
    /// route. `peer_addrs` alone can't answer that: a relayed
    /// (`connect_via`) peer records its RELAY's address there, and
    /// binding that pair would install a false adjacency. When absent
    /// (tests, benches), every install falls back to the legacy
    /// identity-less form.
    addr_to_node: Option<Arc<DashMap<SocketAddr, u64>>>,
    /// Probe for a peer's current session incarnation, so a delayed
    /// failure/recovery callback can tell that the peer it is about has
    /// since been replaced by a fresh session. Absent in tests/benches
    /// that model no session lifecycle.
    #[allow(clippy::type_complexity)]
    peer_incarnation: Option<Arc<dyn Fn(u64) -> Option<u64> + Send + Sync>>,
    /// Proximity graph for multi-hop alternate selection
    proximity_graph: Option<Arc<ProximityGraph>>,
    /// Saved original routes for recovery (dest_node_id → saved route)
    saved_routes: DashMap<u64, SavedRoute>,
    /// Total reroutes performed
    pub reroute_count: AtomicU64,
    /// Total recoveries performed
    pub recovery_count: AtomicU64,
}

/// Convert a u64 node_id to a 32-byte graph NodeId.
fn node_id_to_graph_id(node_id: u64) -> [u8; 32] {
    let mut id = [0u8; 32];
    id[0..8].copy_from_slice(&node_id.to_le_bytes());
    id
}

/// Extract u64 node_id from a 32-byte graph NodeId.
#[expect(
    clippy::unwrap_used,
    reason = "input is &[u8; 32]; slicing [0..8] then .try_into::<[u8; 8]>() is statically infallible"
)]
fn graph_id_to_node_id(id: &[u8; 32]) -> u64 {
    u64::from_le_bytes(id[0..8].try_into().unwrap())
}

impl ReroutePolicy {
    /// Create a new reroute policy.
    pub fn new(
        routing_table: Arc<RoutingTable>,
        peer_addrs: Arc<DashMap<u64, SocketAddr>>,
    ) -> Self {
        Self {
            routing_table,
            peer_addrs,
            addr_to_node: None,
            peer_incarnation: None,
            proximity_graph: None,
            saved_routes: DashMap::new(),
            reroute_count: AtomicU64::new(0),
            recovery_count: AtomicU64::new(0),
        }
    }

    /// Set the proximity graph for multi-hop alternate selection.
    pub fn with_proximity_graph(mut self, graph: Arc<ProximityGraph>) -> Self {
        self.proximity_graph = Some(graph);
        self
    }

    /// Wire the reverse address index so reroute/recovery installs can
    /// bind the authenticated identity of a confirmed-direct next hop
    /// (`SUBNET_AUTH_PLAN.md` D6). Production wires this; without it
    /// every install is legacy (address-only).
    pub fn with_addr_to_node(mut self, index: Arc<DashMap<SocketAddr, u64>>) -> Self {
        self.addr_to_node = Some(index);
        self
    }

    /// Wire a peer-incarnation probe so failure/recovery decisions can
    /// be abandoned when the peer they concern has been replaced by a
    /// fresh session mid-callback.
    pub fn with_peer_incarnation(
        mut self,
        probe: Arc<dyn Fn(u64) -> Option<u64> + Send + Sync>,
    ) -> Self {
        self.peer_incarnation = Some(probe);
        self
    }

    /// The identity to bind for a next hop at `addr`, iff the reverse
    /// index and the forward map agree it is that peer's own DIRECT
    /// session address. Disagreement (a relayed entry, a stale index,
    /// address reuse mid-flight) yields `None` and the caller installs
    /// a legacy route instead — reachable for ordinary routing,
    /// unresolvable for protected forwarding.
    fn direct_identity_for(&self, addr: SocketAddr) -> Option<u64> {
        let node_id = *self.addr_to_node.as_ref()?.get(&addr)?.value();
        (self.peer_addrs.get(&node_id).map(|e| *e.value()) == Some(addr)).then_some(node_id)
    }

    /// Install `dest → addr` as an ORDINARY candidate, but ONLY while
    /// the destination still matches `observed`.
    ///
    /// Ordinary is the right — and only sound — provenance for an
    /// alternate this policy synthesizes. Binding the identity of
    /// whoever currently owns `addr` would manufacture protected
    /// evidence: direct adjacency proves who receives the next hop, not
    /// that the peer ever advertised reachability to this destination.
    /// An alternate that is ALREADY a protected candidate keeps its
    /// provenance, and that path runs through
    /// [`RoutingTable::apply_failure_if_unchanged`] instead.
    ///
    /// Returns the produced outcome when the write landed.
    fn install_ordinary_if_unchanged(
        &self,
        dest_id: u64,
        observed: crate::adapter::net::route::RouteObservation,
        addr: SocketAddr,
    ) -> Option<crate::adapter::net::route::TransitionOutcome> {
        self.routing_table.install_if_unchanged(
            dest_id,
            observed,
            addr,
            crate::adapter::net::route::AlternateProvenance::Ordinary,
        )
    }

    /// The peer's current session incarnation, when a probe is wired.
    /// `None` both when no probe exists and when the peer is absent —
    /// callers compare two readings, so either way an unchanged
    /// reading means "no replacement observed".
    fn incarnation_of(&self, node_id: u64) -> Option<u64> {
        self.peer_incarnation.as_ref().and_then(|p| p(node_id))
    }

    /// Called when the failure detector marks a peer as failed.
    ///
    /// Finds all routes through the failed peer and reroutes them
    /// through an alternate peer. The original routes are saved
    /// for restoration on recovery.
    pub fn on_failure(&self, failed_node_id: u64) {
        self.on_failure_for_incarnation(failed_node_id, 0);
    }

    /// [`Self::on_failure`] for the exact incarnation the detector
    /// declared failed.
    ///
    /// `failed_epoch` is the session id the failure verdict is ABOUT
    /// (0 when the caller has none). Sampling "the peer's current
    /// session" at callback entry cannot substitute for it: production
    /// runs substantial sensing work before this call, so a delayed
    /// callback for a dead session can find a REPLACEMENT session
    /// already installed and read it twice — seeing no change and
    /// treating the live replacement as the thing that failed.
    pub fn on_failure_for_incarnation(&self, failed_node_id: u64, failed_epoch: u64) {
        // Resolve failed node's address
        let failed_addr = match self.peer_addrs.get(&failed_node_id) {
            Some(addr) => *addr,
            None => return, // unknown node, nothing to reroute
        };
        // Refuse outright when the peer has already moved on: this
        // failure is about an incarnation that is no longer installed.
        let current_incarnation = self.incarnation_of(failed_node_id);
        if failed_epoch != 0 {
            if let Some(current) = current_incarnation {
                if current != failed_epoch {
                    return;
                }
            }
        }
        // Re-checked before each write as well, to catch a replacement
        // that lands mid-selection.
        let failed_incarnation = current_incarnation;

        // Find all destinations riding through the failed peer, over
        // BOTH candidates — identity-qualified: a bound candidate is
        // affected iff it is bound to the failed peer (its address may
        // even have drifted); an ordinary candidate is affected by the
        // address match. An address match alone must not reroute a
        // route identity-bound to a DIFFERENT peer that happens to sit
        // at a reused or shared address — that peer supplied its own
        // route evidence and did not fail.
        let mut affected: Vec<u64> = self
            .routing_table
            .all_route_candidates()
            .into_iter()
            .filter(|(_, entry)| match entry.next_hop_id {
                Some(id) => id == failed_node_id,
                None => entry.next_hop == failed_addr,
            })
            .map(|(dest_id, _)| dest_id)
            .collect();
        affected.sort_unstable();
        affected.dedup();

        if affected.is_empty() {
            return;
        }

        // Pick an alternate per destination so that a heterogeneous
        // topology doesn't blackhole traffic through a peer that happens
        // to reach some but not all affected destinations.
        //
        // Resolution order, per destination. EVERY source excludes both
        // the failed identity and the failed transport (address) — a
        // routed peer records its relay's address, so an identity-only
        // exclusion would select a candidate whose only path is the
        // dead one:
        //   1. Routing table: `lookup_alternate_excluding(...)`. The
        //      table holds an ordinary and a protected candidate per
        //      destination, so this can genuinely answer with the other
        //      one when only the failed peer's candidate is excluded.
        //   2. Proximity graph: `find_graph_alternate_for(...)` BFS,
        //      preferring a hop whose forward/reverse indexes agree so
        //      protected recovery lands on one that can carry identity.
        //   3. Last-resort fallback: any direct peer that is neither
        //      the failed identity nor on its address. Best-effort — if
        //      the fallback peer can't actually reach `dest_id`, the
        //      packet is dropped rather than blackholed; the failure
        //      detector will mark that peer dead next cycle if it's
        //      unreachable.
        //
        // `saved_routes` preserves the *original* next_hop so that
        // recovery can restore the pre-failure route. Use
        // `entry().or_insert(...)`: if the same destination already has
        // a saved route from a prior failure, keep that original —
        // overwriting would substitute a relay's addr for the true
        // next_hop and corrupt recovery.
        let mut rerouted = 0usize;
        for dest_id in &affected {
            // Observe BEFORE selection: everything below (graph scans,
            // peer probes) takes time, and the write at the end is
            // conditioned on this exact reading.
            let Some(observed) = self.routing_table.observe(*dest_id) else {
                continue; // destination vanished under us
            };
            // A destination whose ONLY candidates are stale or
            // inactive has already aged out. The failure may still
            // need to remove that dead state, but it must not
            // synthesize fresh reachability from an expired candidate
            // — that would resurrect a destination nothing has
            // advertised for longer than the route lifetime.
            let expired = !observed.reachable();

            // Identity-aware exclusion: a bound candidate can be in
            // `affected` via its binding while its ADDRESS drifted off
            // `failed_addr` — the address-only exclusion would then
            // hand the failed peer's own route back as its "alternate".
            // A surviving candidate keeps its own provenance; anything
            // this policy synthesizes is ordinary.
            let alternate = if expired {
                None
            } else {
                self.routing_table
                    .alternate_candidate_excluding(*dest_id, failed_addr, failed_node_id)
                    .or_else(|| {
                        self.find_graph_alternate_for(failed_node_id, failed_addr, *dest_id)
                            .map(|addr| (addr, AlternateProvenance::Ordinary))
                    })
                    .or_else(|| {
                        // Last-resort fallback: any direct peer that is
                        // neither the failed identity NOR sitting on the
                        // failed transport. A routed peer records its
                        // relay's address, so an identity-only exclusion
                        // would "reroute" straight back onto the dead
                        // address.
                        self.peer_addrs
                            .iter()
                            .find(|e| *e.key() != failed_node_id && *e.value() != failed_addr)
                            .map(|e| (*e.value(), AlternateProvenance::Ordinary))
                    })
            };
            // The peer may have been replaced by a fresh incarnation
            // while we were selecting — this failure is not about that
            // peer.
            if self.incarnation_of(failed_node_id) != failed_incarnation {
                continue;
            }

            // ONE atomic candidate transition: verify the observation,
            // drop every candidate this failure invalidates, keep the
            // ones it does not, and install the alternate only if
            // nothing live survives. Writing an alternate beside a
            // failed candidate would leave the dead hop selectable —
            // `lookup_authenticated` would keep handing protected
            // forwarding a failed protected candidate however good the
            // ordinary alternate installed next to it.
            let Some(outcome) = self.routing_table.apply_failure_if_unchanged(
                *dest_id,
                observed,
                failed_node_id,
                failed_addr,
                alternate,
            ) else {
                continue; // stale observation — someone else spoke
            };
            if !outcome.removed_any && !outcome.installed {
                continue; // nothing of ours to record
            }
            // Record the transition together with the token it
            // PRODUCED. Re-reading the table to discover the token
            // would let a third party's write land in between and be
            // recorded as ours — which is how a later recovery ends up
            // undoing a newer writer.
            //
            // Removal-only transitions are recorded too (with the
            // token the removal produced — 0 when it emptied the
            // destination). The route through the failed peer is gone
            // either way, and recovery is what puts it back when that
            // peer returns; dropping the record here would make a
            // no-alternate failure permanent until something
            // re-learned the destination.
            let recorded = outcome.token;
            self.saved_routes
                .entry(*dest_id)
                .and_modify(|existing| {
                    existing.alternate = alt_or(alternate);
                    existing.alternate_token = recorded;
                })
                .or_insert(SavedRoute {
                    failed_node_id,
                    alternate: alt_or(alternate),
                    alternate_token: recorded,
                });
            rerouted += 1;
        }

        if rerouted > 0 {
            self.reroute_count.fetch_add(1, Ordering::Relaxed);
            tracing::info!(
                failed_node = format!("{:#x}", failed_node_id),
                affected_routes = affected.len(),
                rerouted,
                "auto-rerouted routes away from failed peer"
            );
        }
    }

    /// Pick an alternate for a single destination via the proximity graph.
    ///
    /// Queries `path_to(dest)`. If a path exists whose first hop is both
    /// not the failed node AND is a directly-connected peer of ours,
    /// that hop is the alternate. If the first hop IS the failed node,
    /// tries the next hop. Falls back to any direct peer reachable from
    /// the graph's snapshot if no path works.
    ///
    /// Returns the address of the best alternate, or None if the graph
    /// has no suggestions.
    /// Every candidate source excludes the failed TRANSPORT as well as
    /// the failed identity: a routed peer records the failed relay's
    /// address, so an identity-only exclusion would select a peer whose
    /// only path is the dead address.
    fn find_graph_alternate_for(
        &self,
        failed_node_id: u64,
        failed_addr: SocketAddr,
        dest_id: u64,
    ) -> Option<SocketAddr> {
        let graph = self.proximity_graph.as_ref()?;
        let dest_graph_id = node_id_to_graph_id(dest_id);
        let usable = |nid: u64| -> Option<SocketAddr> {
            if nid == failed_node_id || nid == 0 {
                return None;
            }
            let addr = self.peer_addrs.get(&nid).map(|e| *e.value())?;
            if addr == failed_addr {
                return None;
            }
            Some(addr)
        };

        if let Some(path) = graph.path_to(&dest_graph_id) {
            // path[0] is self; scan forward for the first hop that is
            // neither the failed node nor on its transport, and is a
            // directly-connected peer we can send UDP to. Prefer one
            // whose forward and reverse indexes agree: that peer's
            // address is genuinely its own, so the datagram lands
            // where the graph thinks it does. (The alternate is
            // installed ORDINARY either way — a direct adjacency is
            // not evidence that the peer advertised reachability to
            // this destination.)
            let mut fallback = None;
            for hop in path.iter().skip(1) {
                let Some(addr) = usable(graph_id_to_node_id(hop)) else {
                    continue;
                };
                if self.direct_identity_for(addr).is_some() {
                    return Some(addr);
                }
                fallback.get_or_insert(addr);
            }
            if let Some(addr) = fallback {
                return Some(addr);
            }
        }

        // Fallback: any direct peer from the graph that is neither the
        // failed node nor on its transport. Not topology-aware for this
        // specific destination, but better than nothing.
        graph
            .all_nodes()
            .into_iter()
            .find_map(|node| usable(graph_id_to_node_id(&node.node_id)))
    }

    /// Called when the failure detector marks a peer as recovered.
    ///
    /// Restores original routes that were rerouted when this peer failed.
    /// The direct path is typically better (fewer hops, lower latency)
    /// than the alternate.
    ///
    /// The filter is `entry.failed_node_id == recovered_node_id`
    /// rather than `entry.next_hop == recovered_addr`. An addr-based
    /// filter would miss every reroute when the peer reconnected
    /// from a different SocketAddr (NAT rebind, mobile network
    /// change, reconnect on different port) — `saved_routes` would
    /// accumulate indefinitely across mobile / NAT-changing peers,
    /// and routes would stay pinned to alternates after the peer
    /// had actually recovered. The identity-based filter survives
    /// addr changes; `recovered_addr` is re-resolved from
    /// `peer_addrs` at recovery time so the restored route uses the
    /// current addr.
    pub fn on_recovery(&self, recovered_node_id: u64) {
        self.on_recovery_for_incarnation(recovered_node_id, 0);
    }

    /// [`Self::on_recovery`] for the exact incarnation the detector
    /// declared recovered — the symmetric guard to
    /// [`Self::on_failure_for_incarnation`].
    pub fn on_recovery_for_incarnation(&self, recovered_node_id: u64, recovered_epoch: u64) {
        let recovered_addr = match self.peer_addrs.get(&recovered_node_id) {
            Some(addr) => *addr,
            None => return,
        };
        if recovered_epoch != 0 {
            if let Some(current) = self.incarnation_of(recovered_node_id) {
                if current != recovered_epoch {
                    return;
                }
            }
        }

        // Take the saved transitions for this peer by REMOVING them
        // under the map's own entry lock, rather than snapshotting and
        // deleting later. A snapshot-then-delete leaves a window where
        // a fresh failure can record a new transition for the same
        // destination and have this recovery delete it — the recovery
        // would then own a decision it never made.
        let mut to_restore: Vec<(u64, u64)> = Vec::new();
        self.saved_routes.retain(|dest_id, saved| {
            if saved.failed_node_id == recovered_node_id {
                to_restore.push((*dest_id, saved.alternate_token));
                false // this recovery consumes it
            } else {
                true
            }
        });

        if to_restore.is_empty() {
            return;
        }

        // Restore original routes — using the CURRENT addr, which may
        // differ from the addr at on_failure time if the peer rebinds.
        //
        // Restoration is conditional on the destination still holding
        // exactly the alternate this policy installed. A newer writer
        // (a fresh authenticated learned route, another failure's
        // reroute) means the pre-failure path is no longer the right
        // answer, and a stale recovery must not undo it. The saved
        // entry is dropped either way: this recovery event is spent,
        // and leaving it would retry the same stale restore forever.
        let mut restored = 0usize;
        for (dest_id, alternate_token) in &to_restore {
            match self.routing_table.observe(*dest_id) {
                // The destination still exists: restore only while it
                // carries exactly the token our own transition
                // produced. Any other token means a newer writer has
                // spoken and the pre-failure path is no longer the
                // right answer.
                Some(observed) => {
                    if observed.token != *alternate_token {
                        continue;
                    }
                    if self
                        .install_ordinary_if_unchanged(*dest_id, observed, recovered_addr)
                        .is_some()
                    {
                        restored += 1;
                    }
                }
                // The destination is absent. That is ours to restore
                // ONLY if our own transition is what emptied it
                // (`alternate_token == 0`, the token a destination
                // that no longer exists reports). Any nonzero saved
                // token means we left a candidate behind and someone
                // else has since removed it — not our state to
                // recreate.
                None if *alternate_token == 0 => {
                    self.routing_table.add_route(*dest_id, recovered_addr);
                    restored += 1;
                }
                None => continue,
            }
        }

        if restored == 0 {
            return;
        }
        self.recovery_count.fetch_add(1, Ordering::Relaxed);

        tracing::info!(
            recovered_node = format!("{:#x}", recovered_node_id),
            restored_routes = restored,
            "restored {restored} routes to recovered peer",
        );
    }

    /// Number of active reroutes (routes currently using alternates).
    pub fn active_reroutes(&self) -> usize {
        self.saved_routes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn make_routing_table() -> Arc<RoutingTable> {
        Arc::new(RoutingTable::new(0x1111))
    }

    #[test]
    fn test_reroute_on_failure() {
        let rt = make_routing_table();
        let peers = Arc::new(DashMap::new());

        let addr_b: SocketAddr = "127.0.0.1:2000".parse().unwrap();
        let addr_c: SocketAddr = "127.0.0.1:3000".parse().unwrap();

        peers.insert(0x2222u64, addr_b);
        peers.insert(0x3333u64, addr_c);

        // Route to 0x4444 goes through B
        rt.add_route(0x4444, addr_b);

        let policy = ReroutePolicy::new(rt.clone(), peers);

        // B fails
        policy.on_failure(0x2222);

        // Route should now go through C
        let next_hop = rt.lookup(0x4444).unwrap();
        assert_eq!(next_hop, addr_c, "should reroute to C");
        assert_eq!(policy.active_reroutes(), 1);
        assert_eq!(policy.reroute_count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_recovery_restores_original() {
        let rt = make_routing_table();
        let peers = Arc::new(DashMap::new());

        let addr_b: SocketAddr = "127.0.0.1:2000".parse().unwrap();
        let addr_c: SocketAddr = "127.0.0.1:3000".parse().unwrap();

        peers.insert(0x2222u64, addr_b);
        peers.insert(0x3333u64, addr_c);

        rt.add_route(0x4444, addr_b);

        let policy = ReroutePolicy::new(rt.clone(), peers);

        // B fails → reroute to C
        policy.on_failure(0x2222);
        assert_eq!(rt.lookup(0x4444).unwrap(), addr_c);

        // B recovers → restore to B
        policy.on_recovery(0x2222);
        assert_eq!(rt.lookup(0x4444).unwrap(), addr_b);
        assert_eq!(policy.active_reroutes(), 0);
        assert_eq!(policy.recovery_count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_no_alternate_does_nothing() {
        let rt = make_routing_table();
        let peers = Arc::new(DashMap::new());

        let addr_b: SocketAddr = "127.0.0.1:2000".parse().unwrap();
        peers.insert(0x2222u64, addr_b);

        rt.add_route(0x4444, addr_b);

        let policy = ReroutePolicy::new(rt.clone(), peers);

        // B fails and there is no alternate. The dead candidate is
        // REMOVED rather than left pointing at a failed peer: keeping
        // it would have `lookup` hand out a known-dead next hop until
        // age-out. With nothing to install, the destination becomes
        // unreachable — which is the truthful answer.
        policy.on_failure(0x2222);
        assert_eq!(
            rt.lookup(0x4444),
            None,
            "a failed candidate must not survive as a live route",
        );
        assert_eq!(
            policy.active_reroutes(),
            1,
            "the removal is recorded so recovery can undo it",
        );

        // B comes back: the route it carried is restored.
        policy.on_recovery(0x2222);
        assert_eq!(
            rt.lookup(0x4444),
            Some(addr_b),
            "recovery restores the route the failure removed",
        );
        assert_eq!(policy.active_reroutes(), 0);
    }

    /// Regression: `on_failure` used to `insert` into `saved_routes`,
    /// which meant a second failure (e.g., the alternate itself going
    /// down) would overwrite the original `next_hop` with the alternate's
    /// address. On recovery, the route would then be "restored" to the
    /// wrong peer — the alternate's old address, not the true original.
    ///
    /// Fix: use `entry().or_insert(...)` so the original next_hop is
    /// preserved across repeated failures. Only `alternate` is updated.
    #[test]
    fn test_regression_repeated_failures_preserve_original_next_hop() {
        let rt = make_routing_table();
        let peers = Arc::new(DashMap::new());

        let addr_b: SocketAddr = "127.0.0.1:2000".parse().unwrap();
        let addr_c: SocketAddr = "127.0.0.1:3000".parse().unwrap();
        let addr_d: SocketAddr = "127.0.0.1:4000".parse().unwrap();

        peers.insert(0x2222u64, addr_b);
        peers.insert(0x3333u64, addr_c);
        peers.insert(0x4444u64, addr_d);

        // Route to 0x5555 originally goes through B.
        rt.add_route(0x5555, addr_b);

        let policy = ReroutePolicy::new(rt.clone(), peers.clone());

        // B fails — route is rerouted to an alternate (C or D).
        policy.on_failure(0x2222);
        let first_alt = rt.lookup(0x5555).unwrap();
        assert_ne!(first_alt, addr_b);

        // The alternate also fails. Temporarily remove B from the
        // peer_addrs map before triggering its reroute, so the second
        // on_failure is *forced* to pick something other than B. Without
        // this, the reroute logic is free to land back on B — making a
        // passing test meaningless because the routing table would end
        // up pointing at B even with the buggy code.
        let first_alt_node_id = *peers
            .iter()
            .find(|e| *e.value() == first_alt)
            .unwrap()
            .key();
        peers.remove(&0x2222u64);
        policy.on_failure(first_alt_node_id);
        let second_alt = rt.lookup(0x5555).unwrap();
        assert_ne!(
            second_alt, addr_b,
            "second reroute must pick a non-B alternate"
        );
        peers.insert(0x2222u64, addr_b);

        // B recovers. The original route must be restored to B, not to
        // the alternate that transiently held `next_hop` before the fix.
        policy.on_recovery(0x2222);
        let restored = rt.lookup(0x5555).unwrap();
        assert_eq!(
            restored, addr_b,
            "recovery must restore the true original next_hop (B), not a \
             previously chosen alternate"
        );
    }

    // ========================================================================
    // on_recovery must match by node_id, not next_hop addr,
    // so NAT rebinds / reconnects on different ports still restore.
    // ========================================================================

    /// A peer fails, then recovers from a DIFFERENT SocketAddr (NAT
    /// rebind, reconnect on different port, mobile network change).
    /// `on_recovery` must restore the saved routes to the new addr.
    /// Pre-fix the filter `entry.next_hop == recovered_addr` missed
    /// because the saved `next_hop` held the OLD addr while
    /// `recovered_addr` was the NEW one — no routes restored, and
    /// the saved entry leaked.
    #[test]
    fn on_recovery_restores_routes_after_nat_rebind() {
        let rt = make_routing_table();
        let peers = Arc::new(DashMap::new());

        let addr_b_old: SocketAddr = "127.0.0.1:2000".parse().unwrap();
        let addr_b_new: SocketAddr = "127.0.0.1:2999".parse().unwrap(); // post-rebind
        let addr_c: SocketAddr = "127.0.0.1:3000".parse().unwrap();

        peers.insert(0x2222u64, addr_b_old);
        peers.insert(0x3333u64, addr_c);

        // Route to 0x5555 originally goes through B.
        rt.add_route(0x5555, addr_b_old);

        let policy = ReroutePolicy::new(rt.clone(), peers.clone());

        // B fails — route is rerouted to C.
        policy.on_failure(0x2222);
        assert_eq!(
            rt.lookup(0x5555).unwrap(),
            addr_c,
            "reroute must pick the alternate after failure"
        );
        assert_eq!(policy.active_reroutes(), 1);

        // B comes back from a different SocketAddr (NAT rebind):
        // peer_addrs now reflects the new addr.
        peers.insert(0x2222u64, addr_b_new);
        policy.on_recovery(0x2222);

        // Pre-fix: nothing happens (filter on next_hop addr fails).
        // Post-fix: the route is restored to the NEW addr because
        //  the filter is on node_id, and the addr is re-resolved
        //  from peer_addrs.
        assert_eq!(
            rt.lookup(0x5555).unwrap(),
            addr_b_new,
            "recovery after NAT rebind must restore the route to the NEW addr"
        );
        assert_eq!(
            policy.active_reroutes(),
            0,
            "saved_routes entry must be dropped after recovery (no leak)"
        );
    }

    /// Variant: peer fails, several saved routes through it, then
    /// reconnects from a different addr — ALL saved routes must
    /// restore. Pre-fix the addr-based filter missed all of them
    /// and `saved_routes` leaked entries linearly with the number
    /// of dest_ids that were ever rerouted through a NAT-changing
    /// peer.
    #[test]
    fn on_recovery_restores_multiple_routes_after_nat_rebind() {
        let rt = make_routing_table();
        let peers = Arc::new(DashMap::new());

        let addr_b_old: SocketAddr = "127.0.0.1:2000".parse().unwrap();
        let addr_b_new: SocketAddr = "127.0.0.1:9999".parse().unwrap();
        let addr_c: SocketAddr = "127.0.0.1:3000".parse().unwrap();

        peers.insert(0x2222u64, addr_b_old);
        peers.insert(0x3333u64, addr_c);

        rt.add_route(0x4444, addr_b_old);
        rt.add_route(0x5555, addr_b_old);

        let policy = ReroutePolicy::new(rt.clone(), peers.clone());
        policy.on_failure(0x2222);
        assert_eq!(policy.active_reroutes(), 2);

        peers.insert(0x2222u64, addr_b_new);
        policy.on_recovery(0x2222);

        assert_eq!(rt.lookup(0x4444).unwrap(), addr_b_new);
        assert_eq!(rt.lookup(0x5555).unwrap(), addr_b_new);
        assert_eq!(
            policy.active_reroutes(),
            0,
            "all saved_routes entries must be dropped after recovery",
        );
    }

    #[test]
    fn test_multiple_routes_through_failed_peer() {
        let rt = make_routing_table();
        let peers = Arc::new(DashMap::new());

        let addr_b: SocketAddr = "127.0.0.1:2000".parse().unwrap();
        let addr_c: SocketAddr = "127.0.0.1:3000".parse().unwrap();

        peers.insert(0x2222u64, addr_b);
        peers.insert(0x3333u64, addr_c);

        // Two routes through B
        rt.add_route(0x4444, addr_b);
        rt.add_route(0x5555, addr_b);
        // One route through C (unaffected)
        rt.add_route(0x6666, addr_c);

        let policy = ReroutePolicy::new(rt.clone(), peers);

        policy.on_failure(0x2222);

        // Both B routes should be rerouted to C
        assert_eq!(rt.lookup(0x4444).unwrap(), addr_c);
        assert_eq!(rt.lookup(0x5555).unwrap(), addr_c);
        // C route unchanged
        assert_eq!(rt.lookup(0x6666).unwrap(), addr_c);
        assert_eq!(policy.active_reroutes(), 2);
    }

    /// An alternate this policy SYNTHESIZES is always ordinary, even
    /// when its address belongs to a confirmed direct peer.
    ///
    /// Direct adjacency proves who receives the next hop; it does not
    /// prove that peer ever advertised reachability to this
    /// destination. Binding its identity would manufacture protected
    /// evidence out of a graph guess, and protected forwarding would
    /// then resolve a hop nobody claimed. Only a candidate that was
    /// ALREADY protected keeps that provenance.
    #[test]
    fn a_synthesized_alternate_is_ordinary_even_via_a_direct_peer() {
        let rt = make_routing_table();
        let peers: Arc<DashMap<u64, SocketAddr>> = Arc::new(DashMap::new());
        let addr_to_node: Arc<DashMap<SocketAddr, u64>> = Arc::new(DashMap::new());

        let addr_b: SocketAddr = "127.0.0.1:2000".parse().unwrap();
        let addr_c: SocketAddr = "127.0.0.1:3000".parse().unwrap();
        peers.insert(0x2222u64, addr_b);
        peers.insert(0x3333u64, addr_c);
        addr_to_node.insert(addr_b, 0x2222u64);
        addr_to_node.insert(addr_c, 0x3333u64);

        rt.add_route(0x4444, addr_b);
        let policy =
            ReroutePolicy::new(rt.clone(), peers.clone()).with_addr_to_node(addr_to_node.clone());

        // B fails → the fallback alternate is C, a fully
        // forward/reverse-confirmed direct peer. The route still
        // installs ORDINARY.
        policy.on_failure(0x2222);
        assert_eq!(rt.lookup(0x4444), Some(addr_c), "traffic reroutes to C");
        assert_eq!(
            rt.lookup_authenticated(0x4444),
            None,
            "C's direct adjacency is not evidence that C advertised \
             reachability to this destination",
        );

        // Recovery restores through the same rule.
        let addr_b2: SocketAddr = "127.0.0.1:2001".parse().unwrap();
        peers.insert(0x2222u64, addr_b2);
        addr_to_node.insert(addr_b2, 0x2222u64);
        policy.on_recovery(0x2222);
        assert_eq!(rt.lookup(0x4444), Some(addr_b2));
        assert_eq!(rt.lookup_authenticated(0x4444), None);
    }

    /// An alternate that is ALREADY a protected candidate keeps its
    /// provenance across the failure transition — the failed candidate
    /// goes, the surviving authenticated one stays authenticated.
    #[test]
    fn an_existing_protected_alternate_keeps_its_provenance() {
        let rt = make_routing_table();
        let peers: Arc<DashMap<u64, SocketAddr>> = Arc::new(DashMap::new());
        let addr_b: SocketAddr = "127.0.0.1:2000".parse().unwrap();
        let addr_c: SocketAddr = "127.0.0.1:3000".parse().unwrap();
        peers.insert(0x2222u64, addr_b);
        peers.insert(0x3333u64, addr_c);

        // Ordinary candidate through the doomed B, protected through C.
        rt.add_route(0x4444, addr_b);
        rt.add_authenticated_route_with_metric(0x4444, addr_c, 0x3333, 3);

        let policy = ReroutePolicy::new(rt.clone(), peers);
        policy.on_failure(0x2222);

        assert_eq!(
            rt.lookup_authenticated(0x4444),
            Some((0x3333, addr_c)),
            "the surviving protected candidate must remain protected",
        );
        assert_eq!(rt.lookup(0x4444), Some(addr_c));
    }

    /// THE candidate-safety case: a failed PROTECTED candidate must be
    /// removed by the failure transition, not merely shadowed by an
    /// ordinary alternate installed beside it. Otherwise
    /// `lookup_authenticated` keeps handing protected forwarding a
    /// dead hop.
    #[test]
    fn a_failed_protected_candidate_is_removed_not_shadowed() {
        let rt = make_routing_table();
        let peers: Arc<DashMap<u64, SocketAddr>> = Arc::new(DashMap::new());
        let addr_b: SocketAddr = "127.0.0.1:2000".parse().unwrap();
        let addr_c: SocketAddr = "127.0.0.1:3000".parse().unwrap();
        peers.insert(0x2222u64, addr_b);
        peers.insert(0x3333u64, addr_c);

        // The ONLY candidate is protected, bound to the doomed B.
        rt.add_authenticated_route(0x4444, addr_b, 0x2222);
        assert_eq!(rt.lookup_authenticated(0x4444), Some((0x2222, addr_b)));

        let policy = ReroutePolicy::new(rt.clone(), peers);
        policy.on_failure(0x2222);

        assert_eq!(
            rt.lookup_authenticated(0x4444),
            None,
            "the failed protected candidate must be gone — leaving it \
             keeps protected forwarding selecting a dead hop however \
             good the ordinary alternate installed beside it",
        );
        assert_eq!(
            rt.lookup(0x4444),
            Some(addr_c),
            "ordinary forwarding still gets an alternate",
        );
    }

    /// A destination whose only candidate has already aged out must
    /// not be RESURRECTED by failure handling: invalidation may remove
    /// dead state, but it must not synthesize fresh reachability from
    /// an expired candidate.
    #[test]
    fn failure_does_not_resurrect_an_expired_destination() {
        use std::time::Duration;

        let rt = make_routing_table();
        let peers: Arc<DashMap<u64, SocketAddr>> = Arc::new(DashMap::new());
        let addr_b: SocketAddr = "127.0.0.1:2000".parse().unwrap();
        let addr_c: SocketAddr = "127.0.0.1:3000".parse().unwrap();
        peers.insert(0x2222u64, addr_b);
        peers.insert(0x3333u64, addr_c);

        rt.add_route(0x4444, addr_b);
        rt.backdate_for_test(0x4444, Duration::from_millis(200));
        rt.set_max_route_age(Duration::from_millis(50));
        assert_eq!(rt.lookup(0x4444), None, "precondition: aged out");

        let policy = ReroutePolicy::new(rt.clone(), peers);
        policy.on_failure(0x2222);

        assert_eq!(
            rt.lookup(0x4444),
            None,
            "a failure must not manufacture a fresh route for a \
             destination that had already expired",
        );
    }

    /// A route installed BETWEEN the failure snapshot and the write
    /// survives: the reroute is conditioned on the state it selected
    /// against, so a stale decision skips instead of clobbering.
    #[test]
    fn a_fresh_route_landing_mid_failure_is_not_clobbered() {
        let rt = make_routing_table();
        let peers: Arc<DashMap<u64, SocketAddr>> = Arc::new(DashMap::new());
        let addr_b: SocketAddr = "127.0.0.1:2000".parse().unwrap();
        let addr_c: SocketAddr = "127.0.0.1:3000".parse().unwrap();
        peers.insert(0x2222u64, addr_b);
        peers.insert(0x3333u64, addr_c);
        rt.add_route(0x4444, addr_b);

        let policy = ReroutePolicy::new(rt.clone(), peers.clone());

        // Model the race directly against the conditional writer: the
        // observation is taken, THEN a fresh route lands, THEN the
        // reroute tries to write.
        let observed = rt.observe(0x4444).expect("present");
        rt.add_authenticated_route(0x4444, addr_c, 0x3333); // fresh, mid-selection
        assert!(
            policy
                .install_ordinary_if_unchanged(0x4444, observed, addr_c)
                .is_none(),
            "a write conditioned on pre-race state must not land",
        );
        assert_eq!(
            rt.lookup_authenticated(0x4444),
            Some((0x3333, addr_c)),
            "the freshly installed authenticated route must survive the stale reroute",
        );
    }

    /// A stale RECOVERY cannot undo a newer route: restoration is
    /// conditional on the destination still holding exactly the
    /// alternate this policy installed.
    #[test]
    fn a_route_installed_after_the_reroute_survives_recovery() {
        let rt = make_routing_table();
        let peers: Arc<DashMap<u64, SocketAddr>> = Arc::new(DashMap::new());
        let addr_b: SocketAddr = "127.0.0.1:2000".parse().unwrap();
        let addr_c: SocketAddr = "127.0.0.1:3000".parse().unwrap();
        let addr_d: SocketAddr = "127.0.0.1:4000".parse().unwrap();
        peers.insert(0x2222u64, addr_b);
        peers.insert(0x3333u64, addr_c);
        peers.insert(0x4444u64, addr_d);
        rt.add_route(0x5555, addr_b);

        let policy = ReroutePolicy::new(rt.clone(), peers.clone());
        policy.on_failure(0x2222);
        assert_ne!(rt.lookup(0x5555), Some(addr_b), "precondition: rerouted");

        // A newer writer speaks after the reroute.
        rt.add_authenticated_route(0x5555, addr_d, 0x4444);

        policy.on_recovery(0x2222);
        assert_eq!(
            rt.lookup_authenticated(0x5555),
            Some((0x4444, addr_d)),
            "a stale recovery must not restore over a newer route",
        );
        assert_eq!(
            policy.active_reroutes(),
            0,
            "the spent recovery drops its saved entry either way",
        );
    }

    /// A delayed failure callback must not touch routes belonging to a
    /// REPLACEMENT session of the same NodeID.
    #[test]
    fn a_delayed_failure_does_not_reroute_a_new_incarnation() {
        use std::sync::atomic::{AtomicU64, Ordering as AtOrd};

        let rt = make_routing_table();
        let peers: Arc<DashMap<u64, SocketAddr>> = Arc::new(DashMap::new());
        let addr_b: SocketAddr = "127.0.0.1:2000".parse().unwrap();
        let addr_c: SocketAddr = "127.0.0.1:3000".parse().unwrap();
        peers.insert(0x2222u64, addr_b);
        peers.insert(0x3333u64, addr_c);
        rt.add_authenticated_route(0x4444, addr_b, 0x2222);

        // The probe reports a NEW incarnation the moment the policy
        // re-reads it — i.e. B re-handshaked while selection ran.
        let reads = Arc::new(AtomicU64::new(0));
        let probe_reads = reads.clone();
        let policy = ReroutePolicy::new(rt.clone(), peers.clone()).with_peer_incarnation(Arc::new(
            move |node_id| {
                if node_id != 0x2222 {
                    return Some(1);
                }
                Some(probe_reads.fetch_add(1, AtOrd::Relaxed))
            },
        ));

        policy.on_failure(0x2222);
        assert_eq!(
            rt.lookup_authenticated(0x4444),
            Some((0x2222, addr_b)),
            "a failure about a dead session must not rewrite the \
             replacement incarnation's route",
        );
        assert!(
            reads.load(AtOrd::Relaxed) >= 2,
            "the probe is re-read at write time"
        );
    }

    /// Failure invalidation is identity-qualified: when B fails at
    /// address X, a route identity-bound to C that sits at X (address
    /// reuse / shared relay) is NOT rerouted, a legacy entry at X is,
    /// and a route bound to B is rerouted even from a DRIFTED address.
    #[test]
    fn failure_invalidation_never_affects_another_identity_at_a_reused_address() {
        let rt = make_routing_table();
        let peers: Arc<DashMap<u64, SocketAddr>> = Arc::new(DashMap::new());
        let x: SocketAddr = "127.0.0.1:2000".parse().unwrap();
        let drifted: SocketAddr = "127.0.0.1:2999".parse().unwrap();
        let addr_d: SocketAddr = "127.0.0.1:4000".parse().unwrap();
        peers.insert(0x2222u64, x); // failing peer B, at X
        peers.insert(0x4444u64, addr_d); // surviving alternate D

        // Bound to C at B's address (reuse), legacy at B's address,
        // and bound to B at an address that already drifted off X.
        rt.add_authenticated_route_with_metric(0x5555, x, 0x3333, 3);
        rt.add_route(0x6666, x);
        rt.add_authenticated_route_with_metric(0x7777, drifted, 0x2222, 3);

        let policy = ReroutePolicy::new(rt.clone(), peers);
        policy.on_failure(0x2222);

        assert_eq!(
            rt.lookup_authenticated(0x5555),
            Some((0x3333, x)),
            "a route bound to another identity must survive B's failure"
        );
        assert_eq!(
            rt.lookup(0x6666),
            Some(addr_d),
            "the legacy entry at B's address reroutes to the alternate"
        );
        assert_eq!(
            rt.lookup(0x7777),
            Some(addr_d),
            "a route bound to B reroutes even though its address drifted"
        );
    }

    /// No alternate source may select the FAILED TRANSPORT. A routed
    /// peer records its relay's address, so excluding only the failed
    /// NodeID would "reroute" straight back onto the dead address.
    #[test]
    fn alternate_selection_excludes_the_failed_transport() {
        let rt = make_routing_table();
        let peers: Arc<DashMap<u64, SocketAddr>> = Arc::new(DashMap::new());
        let relay_addr: SocketAddr = "127.0.0.1:9000".parse().unwrap();
        let live_addr: SocketAddr = "127.0.0.1:3000".parse().unwrap();

        // R is the failing relay at X; D is a ROUTED peer that records
        // R's address as its own transport; C is genuinely elsewhere.
        peers.insert(0x2222u64, relay_addr); // R (failing)
        peers.insert(0x4444u64, relay_addr); // D — routed via R
        peers.insert(0x3333u64, live_addr); // C — live alternate

        rt.add_route(0x5555, relay_addr);

        let policy = ReroutePolicy::new(rt.clone(), peers);
        policy.on_failure(0x2222);

        let chosen = rt.lookup(0x5555).expect("a route remains installed");
        assert_ne!(
            chosen, relay_addr,
            "the failed transport must not be selected as its own alternate, \
             however the candidate peer is reached",
        );
        assert_eq!(chosen, live_addr, "the genuinely live peer is chosen");
    }
}
