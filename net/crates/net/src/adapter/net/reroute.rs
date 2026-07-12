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
use super::route::RoutingTable;

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
    #[allow(dead_code)]
    alternate: SocketAddr,
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

    /// Called when the failure detector marks a peer as failed.
    ///
    /// Finds all routes through the failed peer and reroutes them
    /// through an alternate peer. The original routes are saved
    /// for restoration on recovery.
    pub fn on_failure(&self, failed_node_id: u64) {
        // Resolve failed node's address
        let failed_addr = match self.peer_addrs.get(&failed_node_id) {
            Some(addr) => *addr,
            None => return, // unknown node, nothing to reroute
        };

        // Find all routes whose next-hop is the failed peer
        let affected: Vec<u64> = self
            .routing_table
            .all_routes()
            .into_iter()
            .filter(|(_, entry)| entry.next_hop == failed_addr)
            .map(|(dest_id, _)| dest_id)
            .collect();

        if affected.is_empty() {
            return;
        }

        // Pick an alternate per destination so that a heterogeneous
        // topology doesn't blackhole traffic through a peer that happens
        // to reach some but not all affected destinations.
        //
        // Resolution order, per destination:
        //   1. Routing table: `lookup_alternate(dest, failed_addr)`.
        //      Today's table stores one entry per destination, so this
        //      returns `None` when the affected entry *is* the
        //      failed-peer entry — forward-compat scaffolding for a
        //      future multi-route table.
        //   2. Proximity graph: `find_graph_alternate_for(...)` BFS.
        //   3. Last-resort fallback: any direct peer that isn't the
        //      failed one. Best-effort — if the fallback peer can't
        //      actually reach `dest_id`, the packet is dropped rather
        //      than blackholed; the failure detector will mark that
        //      peer dead next cycle if it's unreachable.
        //
        // `saved_routes` preserves the *original* next_hop so that
        // recovery can restore the pre-failure route. Use
        // `entry().or_insert(...)`: if the same destination already has
        // a saved route from a prior failure, keep that original —
        // overwriting would substitute a relay's addr for the true
        // next_hop and corrupt recovery.
        let mut rerouted = 0usize;
        for dest_id in &affected {
            let alt_addr = self
                .routing_table
                .lookup_alternate(*dest_id, failed_addr)
                .or_else(|| self.find_graph_alternate_for(failed_node_id, *dest_id))
                .or_else(|| {
                    self.peer_addrs
                        .iter()
                        .find(|e| *e.key() != failed_node_id)
                        .map(|e| *e.value())
                });
            let alt_addr = match alt_addr {
                Some(a) => a,
                None => continue, // truly nothing to try
            };

            self.saved_routes
                .entry(*dest_id)
                .and_modify(|existing| existing.alternate = alt_addr)
                .or_insert(SavedRoute {
                    failed_node_id,
                    alternate: alt_addr,
                });
            self.routing_table.add_route(*dest_id, alt_addr);
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
    fn find_graph_alternate_for(&self, failed_node_id: u64, dest_id: u64) -> Option<SocketAddr> {
        let graph = self.proximity_graph.as_ref()?;
        let dest_graph_id = node_id_to_graph_id(dest_id);

        if let Some(path) = graph.path_to(&dest_graph_id) {
            // path[0] is self; scan forward for the first hop that
            // (a) isn't the failed node, and (b) is a directly-connected
            // peer we can send UDP to.
            for hop in path.iter().skip(1) {
                let nid = graph_id_to_node_id(hop);
                if nid == failed_node_id {
                    continue;
                }
                if let Some(addr) = self.peer_addrs.get(&nid) {
                    return Some(*addr);
                }
            }
        }

        // Fallback: any direct peer from the graph that isn't the failed
        // node. Not topology-aware for this specific destination, but
        // better than nothing.
        for node in graph.all_nodes() {
            let nid = graph_id_to_node_id(&node.node_id);
            if nid != failed_node_id && nid != 0 {
                if let Some(addr) = self.peer_addrs.get(&nid) {
                    return Some(*addr);
                }
            }
        }
        None
    }

    /// True iff the proximity graph offers a *genuine* INDIRECT
    /// alternate path to `node_id` — one that routes around the direct
    /// `(self, node_id)` edge through another directly-connected peer.
    ///
    /// The direct-edge exclusion is load-bearing: the just-failed
    /// link's `(self → node_id)` edge lingers in the graph until it is
    /// swept, so an unrestricted shortest-path search would return
    /// `[self, node_id]` (the dead direct hop) and report "no
    /// alternate" even when an indirect route exists — poisoning a live
    /// peer's routes (cubic P1). `path_to_excluding_direct` forces the
    /// search around it.
    ///
    /// Deliberately WITHOUT the "any direct peer" fallback that
    /// `find_graph_alternate_for` ends with: a best-effort guess is not
    /// evidence that `node_id` is actually reachable. Used to decide
    /// whether to emit a poison-reverse route withdrawal for a
    /// just-failed direct peer (RT-5 review Finding 5). When a genuine
    /// indirect alternate exists, the peer is still reachable through
    /// another of our peers — a partial/transient failure of our direct
    /// link, not a node death — so "unreachable via me" would be false
    /// and the flood would poison working routes to a live node. A true
    /// node death leaves no such path (nothing else reaches it either),
    /// so the withdrawal still fires for the chain case RT-5 targets.
    pub fn has_graph_path_alternate(&self, node_id: u64) -> bool {
        let Some(graph) = self.proximity_graph.as_ref() else {
            return false;
        };
        // Exclude the (possibly dead) direct edge so an indirect route
        // isn't masked by it.
        let Some(path) = graph.path_to_excluding_direct(&node_id_to_graph_id(node_id)) else {
            return false;
        };
        // path[0] is self; a qualifying hop reaches `node_id` via
        // another directly-connected peer (the direct edge was
        // excluded, so path[1] is never `node_id` when a path exists).
        path.iter().skip(1).any(|hop| {
            let nid = graph_id_to_node_id(hop);
            nid != node_id && self.peer_addrs.contains_key(&nid)
        })
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
        let recovered_addr = match self.peer_addrs.get(&recovered_node_id) {
            Some(addr) => *addr,
            None => return,
        };

        // Find routes that were rerouted away from this peer.
        let to_restore: Vec<u64> = self
            .saved_routes
            .iter()
            .filter(|e| e.value().failed_node_id == recovered_node_id)
            .map(|e| *e.key())
            .collect();

        if to_restore.is_empty() {
            return;
        }

        // Restore original routes — using the CURRENT addr, which
        // may differ from the addr at on_failure time if the peer
        // rebinds.
        for dest_id in &to_restore {
            self.routing_table.add_route(*dest_id, recovered_addr);
            self.saved_routes.remove(dest_id);
        }

        self.recovery_count.fetch_add(1, Ordering::Relaxed);

        tracing::info!(
            recovered_node = format!("{:#x}", recovered_node_id),
            restored_routes = to_restore.len(),
            "restored {} routes to recovered peer",
            to_restore.len()
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

        // B fails but there's no alternate
        policy.on_failure(0x2222);

        // Route unchanged (still points to B — no better option)
        assert_eq!(rt.lookup(0x4444).unwrap(), addr_b);
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

    // RT-5 review Finding 5: `has_graph_path_alternate` gates whether
    // the failure detector emits a poison-reverse withdrawal for a
    // just-failed peer. It must report a GENUINE topological alternate
    // (reachable via another direct peer) and must NOT count
    // direct-only reachability or the "any direct peer" fallback.
    use super::super::behavior::proximity::{EnhancedPingwave, ProximityConfig};

    #[test]
    fn has_graph_path_alternate_true_when_reachable_via_another_peer() {
        const SELF: u64 = 0x1111;
        const X: u64 = 0x2222; // direct peer, intermediate hop
        const DEST: u64 = 0x3333; // "failed" node, reachable via X

        let peers = Arc::new(DashMap::new());
        let x_addr: SocketAddr = "127.0.0.1:2000".parse().unwrap();
        peers.insert(X, x_addr);

        let graph = Arc::new(ProximityGraph::new(
            node_id_to_graph_id(SELF),
            ProximityConfig::default(),
        ));
        // Origin=DEST forwarded by direct peer X → edges self→X and
        // X→DEST, so path_to(DEST) = [self, X, DEST].
        let pw = EnhancedPingwave::new(node_id_to_graph_id(DEST), 1, 3);
        graph.on_pingwave_from(pw, node_id_to_graph_id(X), x_addr);

        let policy = ReroutePolicy::new(make_routing_table(), peers).with_proximity_graph(graph);
        assert!(
            policy.has_graph_path_alternate(DEST),
            "DEST is reachable via direct peer X — a genuine alternate"
        );
        assert!(
            !policy.has_graph_path_alternate(0x9999),
            "a node with no path has no alternate"
        );
    }

    #[test]
    fn has_graph_path_alternate_false_when_only_direct() {
        const SELF: u64 = 0x1111;
        const DEST: u64 = 0x3333;

        let peers = Arc::new(DashMap::new());
        let dest_addr: SocketAddr = "127.0.0.1:3000".parse().unwrap();
        peers.insert(DEST, dest_addr);

        let graph = Arc::new(ProximityGraph::new(
            node_id_to_graph_id(SELF),
            ProximityConfig::default(),
        ));
        // Origin=DEST forwarded directly by DEST (from_node == origin)
        // → only the self→DEST edge, path_to(DEST) = [self, DEST].
        let pw = EnhancedPingwave::new(node_id_to_graph_id(DEST), 1, 3);
        graph.on_pingwave_from(pw, node_id_to_graph_id(DEST), dest_addr);

        let policy = ReroutePolicy::new(make_routing_table(), peers).with_proximity_graph(graph);
        assert!(
            !policy.has_graph_path_alternate(DEST),
            "only-direct reachability is NOT an alternate — the withdrawal must fire"
        );
    }

    #[test]
    fn has_graph_path_alternate_ignores_the_failed_direct_edge() {
        // cubic P1: the just-failed peer's direct edge lingers in the
        // graph until it is swept, so an unrestricted shortest-path
        // search returns [self, DEST] and masks the indirect route via
        // X — wrongly reporting "no alternate" and poisoning a live
        // peer. The check must route AROUND the direct edge.
        const SELF: u64 = 0x1111;
        const X: u64 = 0x2222; // live intermediate peer
        const DEST: u64 = 0x3333; // reachable BOTH directly and via X

        let peers = Arc::new(DashMap::new());
        let x_addr: SocketAddr = "127.0.0.1:2000".parse().unwrap();
        let dest_addr: SocketAddr = "127.0.0.1:3000".parse().unwrap();
        peers.insert(X, x_addr);
        peers.insert(DEST, dest_addr);

        let graph = Arc::new(ProximityGraph::new(
            node_id_to_graph_id(SELF),
            ProximityConfig::default(),
        ));
        // Direct edge self→DEST (the link that just failed but hasn't
        // been swept): a pingwave from DEST forwarded by DEST itself.
        graph.on_pingwave_from(
            EnhancedPingwave::new(node_id_to_graph_id(DEST), 1, 3),
            node_id_to_graph_id(DEST),
            dest_addr,
        );
        // Indirect: DEST is also reachable via live peer X (self→X,
        // X→DEST) — a distinct seq so it isn't deduped.
        graph.on_pingwave_from(
            EnhancedPingwave::new(node_id_to_graph_id(DEST), 2, 3),
            node_id_to_graph_id(X),
            x_addr,
        );

        let policy = ReroutePolicy::new(make_routing_table(), peers).with_proximity_graph(graph);
        assert!(
            policy.has_graph_path_alternate(DEST),
            "an indirect alternate via X exists — the lingering direct edge must not mask it",
        );
    }

    #[test]
    fn has_graph_path_alternate_false_without_graph() {
        let policy = ReroutePolicy::new(make_routing_table(), Arc::new(DashMap::new()));
        assert!(
            !policy.has_graph_path_alternate(0x3333),
            "no proximity graph → no alternate signal"
        );
    }
}
