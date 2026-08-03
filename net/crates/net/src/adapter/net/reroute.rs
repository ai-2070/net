//! Failure-driven route invalidation.
//!
//! Watches the failure detector. When a peer dies, every route
//! candidate whose evidence depended on that peer is REMOVED — and
//! that is all failure handling does. It does not select, synthesize,
//! or install a replacement path: a surviving candidate simply wins
//! the next lookup, and a destination left with nothing becomes
//! unreachable, which is the truthful answer until discovery (a
//! pingwave, an authenticated capability announcement, a handshake)
//! produces fresh evidence.
//!
//! When a peer recovers, exactly ONE route is written: the route to
//! the recovered peer itself, read from its live session — current
//! evidence, not a saved record. Downstream routes are not restored;
//! a peer answering heartbeats again proves the peer is alive, not
//! that anything beyond it is still reachable, still advertised, or
//! still at the metric it once had. Those return through the same
//! discovery that installed them the first time.
//!
//! This module is wired into `MeshNode` via the `FailureDetector`'s
//! `on_failure` and `on_recovery` callbacks.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use dashmap::DashMap;

use super::route::{AlternateProvenance, RoutingTable};

/// Attempts a failure removal makes against a destination whose state
/// keeps changing under it before giving up. Contending writers move
/// at heartbeat cadence, so a second attempt all but always lands;
/// the bound exists so a pathological writer cannot pin this callback
/// in a loop.
const REMOVE_ATTEMPTS: usize = 4;

/// One coherent reading of a peer, for consumers outside the peer
/// table.
///
/// Deliberately a single snapshot rather than a set of probes. Two
/// independent lookups — "what address does this peer have" and "who
/// owns that address" — can disagree, and agreement between them was
/// never the property that mattered: what a caller needs is whether
/// ONE session, read once, is a direct adjacency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerSnapshot {
    /// The incarnation this reading is of.
    pub session_id: u64,
    /// Where datagrams for this peer go.
    pub send_addr: SocketAddr,
    /// `Some(addr)` when the peer OWNS `send_addr` — i.e. the session
    /// terminates where it points, and the peer is adjacent. `None`
    /// for a peer reached through a relay.
    pub owned_addr: Option<SocketAddr>,
}

/// Policy that removes invalidated routes when peers fail.
///
/// When a peer is marked as failed by the `FailureDetector`:
/// 1. Find every destination with a candidate riding through the
///    failed peer — the protected candidate by bound identity, the
///    ordinary one by transport address.
/// 2. Remove exactly those candidates, atomically per destination,
///    keeping every candidate the failure does not invalidate.
///
/// When the peer recovers:
/// 1. Install the route to the peer ITSELF from its live session —
///    protected exactly when that session is a direct adjacency.
///
/// Nothing else is written on either edge. Replacement paths come
/// from discovery, which is the only writer with current evidence.
pub struct ReroutePolicy {
    /// Routing table to update
    routing_table: Arc<RoutingTable>,
    /// Connected peers (node_id → addr mapping)
    peer_addrs: Arc<DashMap<u64, SocketAddr>>,
    /// One coherent reading of a peer — incarnation and transport
    /// together — so a delayed failure/recovery callback can tell both
    /// that the peer it is about has been replaced and whether the
    /// session it is about is an adjacency at all. Absent in
    /// tests/benches that model no session lifecycle.
    #[allow(clippy::type_complexity)]
    peer_snapshot: Option<Arc<dyn Fn(u64) -> Option<PeerSnapshot> + Send + Sync>>,
    /// Revalidates that a verdict is still the latest one the detector
    /// issued for a node, at the moment of mutation. Absent in
    /// tests/benches with no detector.
    ///
    /// A `OnceLock` rather than a builder field because the wiring is
    /// circular: the detector's callbacks hold this policy, so the
    /// policy exists first. Set once, during construction, before any
    /// verdict can be issued.
    #[allow(clippy::type_complexity)]
    verdict_is_current: std::sync::OnceLock<Arc<dyn Fn(u64, u64) -> bool + Send + Sync>>,
    /// Failure events that removed at least one candidate.
    pub reroute_count: AtomicU64,
    /// Recoveries that installed the peer's own route.
    pub recovery_count: AtomicU64,
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
            peer_snapshot: None,
            verdict_is_current: std::sync::OnceLock::new(),
            reroute_count: AtomicU64::new(0),
            recovery_count: AtomicU64::new(0),
        }
    }

    /// Wire a peer-snapshot probe so failure/recovery decisions can be
    /// abandoned when the peer they concern has been replaced by a
    /// fresh session mid-callback, and so adjacency is decided from one
    /// coherent reading rather than from two maps agreeing.
    pub fn with_peer_snapshot(
        mut self,
        probe: Arc<dyn Fn(u64) -> Option<PeerSnapshot> + Send + Sync>,
    ) -> Self {
        self.peer_snapshot = Some(probe);
        self
    }

    /// Wire the detector's verdict-order check, so a delayed failure
    /// callback that a recovery has already superseded refuses at the
    /// point of mutation.
    ///
    /// Takes `&self` and is idempotent-by-first-write: the detector
    /// that answers this check is the same one whose callbacks hold
    /// this policy, so it cannot exist yet when the policy is built.
    pub fn set_verdict_check(&self, probe: Arc<dyn Fn(u64, u64) -> bool + Send + Sync>) {
        let _ = self.verdict_is_current.set(probe);
    }

    /// One coherent reading of a peer, when a probe is wired. `None`
    /// both when no probe exists and when the peer is absent — callers
    /// compare two readings, so either way an unchanged reading means
    /// "no replacement observed".
    fn snapshot_of(&self, node_id: u64) -> Option<PeerSnapshot> {
        self.peer_snapshot.as_ref().and_then(|p| p(node_id))
    }

    /// The peer's current session incarnation.
    fn incarnation_of(&self, node_id: u64) -> Option<u64> {
        self.snapshot_of(node_id).map(|s| s.session_id)
    }

    /// Whether `verdict_seq` is still the detector's latest word on
    /// `node_id`. With no detector wired (tests, benches) every verdict
    /// is treated as current — there is nothing that could supersede
    /// it.
    fn verdict_still_current(&self, node_id: u64, verdict_seq: u64) -> bool {
        match (self.verdict_is_current.get(), verdict_seq) {
            (Some(check), seq) if seq != 0 => check(node_id, seq),
            _ => true,
        }
    }

    /// Called when the failure detector marks a peer as failed.
    ///
    /// Removes every route candidate the failure invalidates.
    pub fn on_failure(&self, failed_node_id: u64) {
        self.on_failure_for_incarnation(failed_node_id, 0);
    }

    /// [`Self::on_failure`] for a detector verdict, carrying both the
    /// incarnation it is about and its position in the verdict order
    /// for that peer.
    pub fn on_failure_for_verdict(&self, failed_node_id: u64, failed_epoch: u64, verdict_seq: u64) {
        self.apply_failure(failed_node_id, failed_epoch, verdict_seq);
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
        self.apply_failure(failed_node_id, failed_epoch, 0);
    }

    fn apply_failure(&self, failed_node_id: u64, failed_epoch: u64, verdict_seq: u64) {
        // Resolve failed node's address
        let failed_addr = match self.peer_addrs.get(&failed_node_id) {
            Some(addr) => *addr,
            None => return, // unknown node, nothing to remove
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
        // And refuse when a LATER verdict has superseded this one. The
        // incarnation check cannot catch that case: a failure and the
        // recovery that supersedes it name the same epoch, so a delayed
        // failure callback passes the incarnation test and then tears
        // down routes the recovery left live.
        if !self.verdict_still_current(failed_node_id, verdict_seq) {
            return;
        }
        // Re-checked before each write as well, to catch a replacement
        // that lands mid-callback.
        let failed_incarnation = current_incarnation;

        // Find all destinations riding through the failed peer, over
        // BOTH candidates — identity-qualified: a bound candidate is
        // affected iff it is bound to the failed peer (its address may
        // even have drifted); an ordinary candidate is affected by the
        // address match. An address match alone must not remove a
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

        let mut removed = 0usize;
        for dest_id in &affected {
            if self.remove_one(
                *dest_id,
                failed_node_id,
                failed_addr,
                failed_incarnation,
                verdict_seq,
            ) {
                removed += 1;
            }
        }

        if removed > 0 {
            self.reroute_count.fetch_add(1, Ordering::Relaxed);
            tracing::info!(
                failed_node = format!("{:#x}", failed_node_id),
                affected_routes = affected.len(),
                removed,
                "removed route candidates invalidated by failed peer"
            );
        }
    }

    /// Remove the failed peer's candidates from ONE destination.
    /// Returns whether anything was removed.
    ///
    /// The removal is observation-conditioned even though there is no
    /// selection work between read and write. The predicate alone
    /// cannot carry the evidence the entry checks established outside
    /// the entry guard: `failed_addr` was resolved from the CURRENT
    /// peer map at callback entry, and a bound candidate matches by
    /// identity alone — so a replacement session installing its own
    /// route between our incarnation re-check and an unconditional
    /// removal would have that live route deleted by a verdict about
    /// its dead predecessor. The token check turns that interleaving
    /// into a refusal and a re-observe instead.
    ///
    /// A refusal re-observes and retries (bounded): unlike an install,
    /// this write must eventually happen while the failed candidate
    /// remains — dead evidence someone else's unrelated write leaves in
    /// place is still dead.
    fn remove_one(
        &self,
        dest_id: u64,
        failed_node_id: u64,
        failed_addr: SocketAddr,
        failed_incarnation: Option<u64>,
        verdict_seq: u64,
    ) -> bool {
        for _ in 0..REMOVE_ATTEMPTS {
            let Some(observed) = self.routing_table.observe(dest_id) else {
                return false; // destination already gone
            };
            // Someone else may already have removed (or replaced) what
            // this failure invalidates.
            let invalidates_any = observed
                .protected
                .is_some_and(|p| p.next_hop_id == Some(failed_node_id))
                || observed.ordinary.is_some_and(|o| o.next_hop == failed_addr);
            if !invalidates_any {
                return false;
            }
            // The peer may have been replaced by a fresh incarnation
            // while this callback ran — the failure is not about that
            // peer — or a later verdict may have superseded this one.
            if self.incarnation_of(failed_node_id) != failed_incarnation
                || !self.verdict_still_current(failed_node_id, verdict_seq)
            {
                return false;
            }
            match self.routing_table.remove_failed_candidates_if_unchanged(
                dest_id,
                observed,
                failed_node_id,
                failed_addr,
            ) {
                Some(outcome) => return outcome.removed_any,
                None => continue, // another writer spoke — re-observe
            }
        }
        tracing::debug!(
            dest = format!("{dest_id:#x}"),
            peer = format!("{failed_node_id:#x}"),
            "failure removal gave up after repeated contention; the \
             candidate ages out unless a fresh write settles it first"
        );
        false
    }

    /// Called when the failure detector marks a peer as recovered.
    ///
    /// Installs the route to the recovered peer itself — and nothing
    /// else. See the module doc.
    pub fn on_recovery(&self, recovered_node_id: u64) {
        self.on_recovery_for_incarnation(recovered_node_id, 0);
    }

    /// [`Self::on_recovery`] for the exact incarnation the detector
    /// declared recovered — the symmetric guard to
    /// [`Self::on_failure_for_incarnation`].
    pub fn on_recovery_for_incarnation(&self, recovered_node_id: u64, recovered_epoch: u64) {
        self.apply_recovery(recovered_node_id, recovered_epoch, 0);
    }

    /// [`Self::on_recovery`] for a detector verdict, carrying both the
    /// incarnation and its position in that peer's verdict order.
    pub fn on_recovery_for_verdict(
        &self,
        recovered_node_id: u64,
        recovered_epoch: u64,
        verdict_seq: u64,
    ) {
        self.apply_recovery(recovered_node_id, recovered_epoch, verdict_seq);
    }

    /// The one write recovery may make: the route to the recovered
    /// peer ITSELF, from the peer's live session.
    ///
    /// This case exists because recovery is not always a reconnection.
    /// A false-positive suspicion — missed heartbeats on a session
    /// that never died — recovers on the SAME session: no handshake
    /// fires, so no install path runs, while the failure edge already
    /// removed the peer's route. The live session is readable right
    /// now and IS the evidence: protected exactly when it is a direct
    /// adjacency, ordinary when the peer is reached through a relay.
    /// (Without a snapshot probe there is no ownership evidence at
    /// all, so the install is ordinary.)
    ///
    /// The install is conditional — on the observation taken here, or
    /// on the destination still being absent — and is NOT retried on
    /// refusal: an install that loses its race lost to a writer with
    /// evidence at least as current, and discovery settles anything
    /// left. That asymmetry with the failure path is deliberate; dead
    /// evidence must go, a liveness optimization may simply miss.
    fn apply_recovery(&self, recovered_node_id: u64, recovered_epoch: u64, verdict_seq: u64) {
        // Read the recovered peer ONCE. Both the address to install and
        // whether the session is an adjacency come from the same
        // snapshot, so recovery cannot install a route at an address
        // taken from one reading with a provenance justified by another.
        let snapshot = self.snapshot_of(recovered_node_id);
        let recovered_addr = match snapshot {
            Some(s) => s.send_addr,
            None => match self.peer_addrs.get(&recovered_node_id) {
                Some(addr) => *addr,
                None => return,
            },
        };
        if recovered_epoch != 0 {
            if let Some(current) = snapshot.map(|s| s.session_id) {
                if current != recovered_epoch {
                    return;
                }
            }
        }
        let is_adjacent = snapshot.is_some_and(|s| s.owned_addr == Some(recovered_addr));
        let provenance = if is_adjacent {
            AlternateProvenance::Protected(recovered_node_id)
        } else {
            AlternateProvenance::Ordinary
        };

        // The verdict gate sits as close to the write as it can get: a
        // failure verdict issued after this recovery must win, or a
        // delayed recovery would hand back a route to a peer the
        // detector has already re-declared dead — and the detector
        // only fires on the NEXT transition, so nothing would remove
        // it again until then.
        if !self.verdict_still_current(recovered_node_id, verdict_seq) {
            return;
        }

        let installed = match self.routing_table.observe(recovered_node_id) {
            Some(observed) => {
                // Something already speaks for this destination. If it
                // is exactly the route this recovery would write, do
                // not write it again — a no-op rewrite still bumps the
                // transition token and spuriously refuses every
                // conditional writer mid-flight.
                let already_current = match provenance {
                    AlternateProvenance::Protected(id) => observed.protected.is_some_and(|p| {
                        p.live && p.next_hop == recovered_addr && p.next_hop_id == Some(id)
                    }),
                    AlternateProvenance::Ordinary => observed
                        .effective
                        .is_some_and(|e| e.live && e.next_hop == recovered_addr),
                };
                if already_current {
                    return;
                }
                self.routing_table
                    .install_metered_if_unchanged(
                        recovered_node_id,
                        observed,
                        recovered_addr,
                        provenance,
                        1,
                    )
                    .is_some()
            }
            // The failure that removed the peer's last candidate
            // removed the destination with it.
            None => self.routing_table.install_metered_if_absent(
                recovered_node_id,
                recovered_addr,
                provenance,
                1,
            ),
        };

        if installed {
            self.recovery_count.fetch_add(1, Ordering::Relaxed);
            tracing::info!(
                recovered_node = format!("{:#x}", recovered_node_id),
                addr = %recovered_addr,
                protected = is_adjacent,
                "installed recovered peer's own route from its live session"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn make_routing_table() -> Arc<RoutingTable> {
        Arc::new(RoutingTable::new(0x1111))
    }

    /// A failure REMOVES the candidate it invalidates. It does not
    /// select another connected peer as a stand-in: C being alive is
    /// not evidence that C can reach the destination, so the
    /// destination becomes unreachable — the truthful answer.
    #[test]
    fn a_failure_removes_the_candidate_it_invalidates() {
        let rt = make_routing_table();
        let peers = Arc::new(DashMap::new());

        let addr_b: SocketAddr = "127.0.0.1:2000".parse().unwrap();
        let addr_c: SocketAddr = "127.0.0.1:3000".parse().unwrap();

        peers.insert(0x2222u64, addr_b);
        peers.insert(0x3333u64, addr_c);

        // Route to 0x4444 goes through B; C never advertised it.
        rt.add_route(0x4444, addr_b);

        let policy = ReroutePolicy::new(rt.clone(), peers);
        policy.on_failure(0x2222);

        assert_eq!(
            rt.lookup(0x4444),
            None,
            "the dead candidate is removed and no alternate is invented \
             from C's mere liveness",
        );
        assert_eq!(policy.reroute_count.load(Ordering::Relaxed), 1);
    }

    /// A surviving candidate wins WITHOUT any synthesis: removal of the
    /// failed candidate is enough for the next lookup to select the
    /// remaining one, at its own provenance.
    #[test]
    fn a_surviving_candidate_wins_without_synthesis() {
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
    /// removed by the failure transition. Under the old shape it could
    /// merely be shadowed by an alternate installed beside it, leaving
    /// `lookup_authenticated` handing protected forwarding a dead hop.
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

        assert_eq!(rt.lookup_authenticated(0x4444), None);
        assert_eq!(
            rt.lookup(0x4444),
            None,
            "and nothing is invented in its place — the destination is \
             unreachable until something re-advertises it",
        );
    }

    /// Failure invalidation is identity-qualified: when B fails at
    /// address X, a route identity-bound to C that sits at X (address
    /// reuse / shared relay) is NOT touched, a legacy entry at X is
    /// removed, and a route bound to B is removed even from a DRIFTED
    /// address.
    #[test]
    fn failure_invalidation_never_affects_another_identity_at_a_reused_address() {
        let rt = make_routing_table();
        let peers: Arc<DashMap<u64, SocketAddr>> = Arc::new(DashMap::new());
        let x: SocketAddr = "127.0.0.1:2000".parse().unwrap();
        let drifted: SocketAddr = "127.0.0.1:2999".parse().unwrap();
        peers.insert(0x2222u64, x); // failing peer B, at X

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
            None,
            "the legacy entry at B's transport is removed"
        );
        assert_eq!(
            rt.lookup(0x7777),
            None,
            "a route bound to B is removed even though its address drifted"
        );
    }

    /// A destination whose only candidate has already aged out is
    /// still cleaned up, and nothing resurrects it: invalidation may
    /// remove dead state, never manufacture fresh reachability.
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

    /// A route installed BETWEEN the failure observation and the write
    /// survives: the removal is conditioned on the state it observed,
    /// so a stale removal refuses instead of clobbering.
    #[test]
    fn a_fresh_route_landing_mid_failure_is_not_clobbered() {
        let rt = make_routing_table();
        let addr_b: SocketAddr = "127.0.0.1:2000".parse().unwrap();
        let addr_c: SocketAddr = "127.0.0.1:3000".parse().unwrap();
        rt.add_route(0x4444, addr_b);

        // Model the race directly against the conditional writer: the
        // observation is taken, THEN a fresh route lands, THEN the
        // removal tries to write.
        let observed = rt.observe(0x4444).expect("present");
        rt.add_authenticated_route(0x4444, addr_c, 0x3333); // fresh, mid-callback
        assert!(
            rt.remove_failed_candidates_if_unchanged(0x4444, observed, 0x2222, addr_b)
                .is_none(),
            "a removal conditioned on pre-race state must not land",
        );
        assert_eq!(
            rt.lookup_authenticated(0x4444),
            Some((0x3333, addr_c)),
            "the freshly installed authenticated route must survive",
        );
    }

    /// A delayed failure callback must not touch routes belonging to a
    /// REPLACEMENT session of the same NodeID.
    #[test]
    fn a_delayed_failure_does_not_remove_a_new_incarnations_route() {
        use std::sync::atomic::{AtomicU64, Ordering as AtOrd};

        let rt = make_routing_table();
        let peers: Arc<DashMap<u64, SocketAddr>> = Arc::new(DashMap::new());
        let addr_b: SocketAddr = "127.0.0.1:2000".parse().unwrap();
        let addr_c: SocketAddr = "127.0.0.1:3000".parse().unwrap();
        peers.insert(0x2222u64, addr_b);
        peers.insert(0x3333u64, addr_c);
        rt.add_authenticated_route(0x4444, addr_b, 0x2222);

        // The probe reports a NEW incarnation the moment the policy
        // re-reads it — i.e. B re-handshaked while the callback ran.
        let reads = Arc::new(AtomicU64::new(0));
        let probe_reads = reads.clone();
        let policy = ReroutePolicy::new(rt.clone(), peers.clone()).with_peer_snapshot(Arc::new(
            move |node_id| {
                let session_id = if node_id != 0x2222 {
                    1
                } else {
                    probe_reads.fetch_add(1, AtOrd::Relaxed)
                };
                Some(PeerSnapshot {
                    session_id,
                    send_addr: addr_b,
                    owned_addr: Some(addr_b),
                })
            },
        ));

        policy.on_failure(0x2222);
        assert_eq!(
            rt.lookup_authenticated(0x4444),
            Some((0x2222, addr_b)),
            "a failure about a dead session must not remove the \
             replacement incarnation's route",
        );
        assert!(
            reads.load(AtOrd::Relaxed) >= 2,
            "the probe is re-read at write time"
        );
    }

    /// A failure verdict a recovery has already superseded must not
    /// mutate anything, even though both name the SAME incarnation.
    ///
    /// The detector records a failure, releases its shard locks, and
    /// only then invokes the callback. A heartbeat arriving in that
    /// window marks the same session healthy and runs recovery first.
    /// Incarnation comparison cannot reject the late failure —
    /// `failure.epoch == recovery.epoch == E` is correct for both — so
    /// without a verdict ORDER the stale failure tears down a live
    /// peer's routes, and nothing removes-then-reinstalls them until
    /// the next advertisement.
    #[test]
    fn a_superseded_failure_verdict_refuses_at_the_mutation() {
        let rt = make_routing_table();
        let peers: Arc<DashMap<u64, SocketAddr>> = Arc::new(DashMap::new());
        let addr_b: SocketAddr = "127.0.0.1:2000".parse().unwrap();
        let addr_c: SocketAddr = "127.0.0.1:3000".parse().unwrap();
        peers.insert(0x2222u64, addr_b);
        peers.insert(0x3333u64, addr_c);
        rt.add_route(0x4444, addr_b);

        let policy =
            ReroutePolicy::new(rt.clone(), peers).with_peer_snapshot(Arc::new(move |_node_id| {
                // One live incarnation throughout — the failure and the
                // recovery are about the SAME session, which is exactly
                // the case the epoch cannot separate.
                Some(PeerSnapshot {
                    session_id: 7,
                    send_addr: addr_b,
                    owned_addr: Some(addr_b),
                })
            }));
        // The detector has moved on: verdict 1 is no longer current,
        // verdict 2 is.
        policy.set_verdict_check(Arc::new(|_node_id, seq| seq == 2));

        // The delayed failure carries verdict 1 and the right epoch.
        policy.on_failure_for_verdict(0x2222, 7, 1);

        assert_eq!(
            rt.lookup(0x4444),
            Some(addr_b),
            "a failure verdict a later verdict has superseded must mutate nothing — \
             the epoch matches, so only the verdict order can reject it"
        );

        // The current verdict still works, so the guard rejects
        // staleness rather than everything.
        policy.on_failure_for_verdict(0x2222, 7, 2);
        assert_eq!(
            rt.lookup(0x4444),
            None,
            "the CURRENT verdict must still remove the dead candidate"
        );
    }

    /// Recovery does NOT restore downstream candidates — neither the
    /// ordinary one at its old metric nor the protected one.
    ///
    /// A peer answering heartbeats again proves the peer is alive. It
    /// does not prove the peer still reaches, still exports, or still
    /// advertises anything beyond itself, and it is not the
    /// authenticated advertisement that produced a protected
    /// candidate. Downstream routes return through discovery.
    #[test]
    fn recovery_does_not_restore_downstream_candidates() {
        let rt = make_routing_table();
        let peers: Arc<DashMap<u64, SocketAddr>> = Arc::new(DashMap::new());
        let addr_b: SocketAddr = "127.0.0.1:2000".parse().unwrap();
        peers.insert(0x2222u64, addr_b);

        // Downstream ordinary (3 hops) and downstream protected, both
        // through B — plus B's own route.
        rt.add_route_with_metric(0x4444, addr_b, 3);
        rt.add_authenticated_route_with_metric(0x5555, addr_b, 0x2222, 3);
        rt.add_authenticated_route(0x2222, addr_b, 0x2222);

        let policy = ReroutePolicy::new(rt.clone(), peers);
        policy.on_failure(0x2222);
        assert_eq!(rt.lookup(0x4444), None, "downstream ordinary removed");
        assert_eq!(
            rt.lookup_authenticated(0x5555),
            None,
            "downstream protected removed"
        );
        assert_eq!(rt.lookup(0x2222), None, "B's own route removed");

        policy.on_recovery(0x2222);

        assert_eq!(
            rt.lookup(0x4444),
            None,
            "B answering heartbeats is not evidence it still reaches 0x4444"
        );
        assert_eq!(
            rt.lookup_authenticated(0x5555),
            None,
            "and it is certainly not an authenticated advertisement — \
             protected forwarding stays failed closed until B re-announces"
        );
        assert_eq!(
            rt.lookup(0x2222),
            Some(addr_b),
            "the ONE route recovery writes is the peer's own"
        );
    }

    /// The route to the RECOVERED PEER ITSELF comes back from the live
    /// session's own transport, protected exactly when that session is
    /// an adjacency.
    ///
    /// This matters because recovery is not always a reconnection: a
    /// false-positive suspicion recovers on the SAME session, so no
    /// handshake runs to reinstall what the failure removed.
    #[test]
    fn recovery_installs_the_peers_own_route_from_its_live_transport() {
        let rt = make_routing_table();
        let peers: Arc<DashMap<u64, SocketAddr>> = Arc::new(DashMap::new());
        let addr_b: SocketAddr = "127.0.0.1:2000".parse().unwrap();
        peers.insert(0x2222u64, addr_b);

        // B's own direct route — the shape `add_direct_route` installs.
        rt.add_authenticated_route(0x2222, addr_b, 0x2222);

        let policy =
            ReroutePolicy::new(rt.clone(), peers).with_peer_snapshot(Arc::new(move |node_id| {
                (node_id == 0x2222).then_some(PeerSnapshot {
                    session_id: 1,
                    send_addr: addr_b,
                    owned_addr: Some(addr_b), // still a DIRECT adjacency
                })
            }));

        policy.on_failure(0x2222);
        assert_eq!(
            rt.lookup_authenticated(0x2222),
            None,
            "the failure removes B's own protected candidate"
        );

        policy.on_recovery(0x2222);

        assert_eq!(
            rt.lookup_authenticated(0x2222),
            Some((0x2222, addr_b)),
            "the peer's own route is restored as PROTECTED, because the live session \
             terminating at that address is exactly what makes the hop authenticated"
        );
        assert_eq!(policy.recovery_count.load(Ordering::Relaxed), 1);
    }

    /// The same recovery, when the live session is ROUTED, installs an
    /// ORDINARY candidate instead.
    ///
    /// The peer is reachable, so the route comes back; but a relay's
    /// address is not the peer's own attachment, so nothing about that
    /// session authenticates the hop. Reading the transport is what
    /// separates the two — a saved record could not, because the
    /// session may have changed class while the peer was down.
    #[test]
    fn recovery_of_a_now_routed_peer_installs_only_an_ordinary_candidate() {
        let rt = make_routing_table();
        let peers: Arc<DashMap<u64, SocketAddr>> = Arc::new(DashMap::new());
        let relay: SocketAddr = "127.0.0.1:9000".parse().unwrap();
        peers.insert(0x2222u64, relay);

        rt.add_authenticated_route(0x2222, relay, 0x2222);

        let policy =
            ReroutePolicy::new(rt.clone(), peers).with_peer_snapshot(Arc::new(move |node_id| {
                (node_id == 0x2222).then_some(PeerSnapshot {
                    session_id: 1,
                    send_addr: relay,
                    owned_addr: None, // reached THROUGH a relay now
                })
            }));

        policy.on_failure(0x2222);
        policy.on_recovery(0x2222);

        assert_eq!(
            rt.lookup(0x2222),
            Some(relay),
            "the peer is reachable again through its relay"
        );
        assert_eq!(
            rt.lookup_authenticated(0x2222),
            None,
            "but a routed session is not an adjacency, so the installed candidate \
             carries no identity — the class comes from the LIVE transport"
        );
    }

    /// Recovery installs at the peer's CURRENT address. After a NAT
    /// rebind the peer map already reflects the new attachment, and
    /// nothing keyed on the old address is consulted.
    #[test]
    fn recovery_installs_at_the_current_address_after_a_rebind() {
        let rt = make_routing_table();
        let peers: Arc<DashMap<u64, SocketAddr>> = Arc::new(DashMap::new());
        let addr_b_old: SocketAddr = "127.0.0.1:2000".parse().unwrap();
        let addr_b_new: SocketAddr = "127.0.0.1:2999".parse().unwrap(); // post-rebind
        peers.insert(0x2222u64, addr_b_old);

        rt.add_route(0x2222, addr_b_old);

        let policy = ReroutePolicy::new(rt.clone(), peers.clone());
        policy.on_failure(0x2222);
        assert_eq!(rt.lookup(0x2222), None);

        // B comes back from a different SocketAddr: the peer map
        // reflects the new attachment before the recovery fires.
        peers.insert(0x2222u64, addr_b_new);
        policy.on_recovery(0x2222);

        assert_eq!(
            rt.lookup(0x2222),
            Some(addr_b_new),
            "the recovered peer's route points at its CURRENT address"
        );
        assert_eq!(
            rt.lookup_authenticated(0x2222),
            None,
            "with no snapshot probe there is no ownership evidence, so \
             the install is ordinary"
        );
    }

    /// A stale recovery for a REPLACED session installs nothing: the
    /// epoch it names is no longer the session that exists.
    #[test]
    fn a_stale_recovery_for_a_replaced_session_installs_nothing() {
        let rt = make_routing_table();
        let peers: Arc<DashMap<u64, SocketAddr>> = Arc::new(DashMap::new());
        let addr_b: SocketAddr = "127.0.0.1:2000".parse().unwrap();
        peers.insert(0x2222u64, addr_b);

        let policy =
            ReroutePolicy::new(rt.clone(), peers).with_peer_snapshot(Arc::new(move |_node_id| {
                Some(PeerSnapshot {
                    session_id: 9, // the replacement
                    send_addr: addr_b,
                    owned_addr: Some(addr_b),
                })
            }));

        // The delayed recovery is about session 7, which is gone.
        policy.on_recovery_for_incarnation(0x2222, 7);

        assert_eq!(
            rt.lookup(0x2222),
            None,
            "a recovery about a dead session must not install anything — \
             the replacement's own install path speaks for the new session"
        );
        assert_eq!(policy.recovery_count.load(Ordering::Relaxed), 0);
    }

    /// A recovery verdict a later failure has superseded installs
    /// nothing — the mirror of the superseded-failure case, and the
    /// more dangerous direction: the detector only fires on the next
    /// TRANSITION, so a route handed back to a re-declared-dead peer
    /// would sit there until something else moved.
    #[test]
    fn a_superseded_recovery_verdict_installs_nothing() {
        let rt = make_routing_table();
        let peers: Arc<DashMap<u64, SocketAddr>> = Arc::new(DashMap::new());
        let addr_b: SocketAddr = "127.0.0.1:2000".parse().unwrap();
        peers.insert(0x2222u64, addr_b);

        let policy =
            ReroutePolicy::new(rt.clone(), peers).with_peer_snapshot(Arc::new(move |_node_id| {
                Some(PeerSnapshot {
                    session_id: 7,
                    send_addr: addr_b,
                    owned_addr: Some(addr_b),
                })
            }));
        // Verdict 3 (a fresh failure) has superseded verdict 2 (this
        // recovery).
        policy.set_verdict_check(Arc::new(|_node_id, seq| seq == 3));

        policy.on_recovery_for_verdict(0x2222, 7, 2);

        assert_eq!(
            rt.lookup(0x2222),
            None,
            "a superseded recovery must not hand a route back to a peer \
             the detector has already re-declared dead"
        );
    }

    /// When the peer's own route is already current — a handshake or
    /// announcement reinstalled it first — recovery writes nothing,
    /// not even a same-value rewrite: a no-op write still bumps the
    /// transition token and spuriously refuses conditional writers.
    #[test]
    fn recovery_skips_when_the_peers_route_is_already_current() {
        let rt = make_routing_table();
        let peers: Arc<DashMap<u64, SocketAddr>> = Arc::new(DashMap::new());
        let addr_b: SocketAddr = "127.0.0.1:2000".parse().unwrap();
        peers.insert(0x2222u64, addr_b);

        rt.add_authenticated_route(0x2222, addr_b, 0x2222);

        let policy =
            ReroutePolicy::new(rt.clone(), peers).with_peer_snapshot(Arc::new(move |node_id| {
                (node_id == 0x2222).then_some(PeerSnapshot {
                    session_id: 1,
                    send_addr: addr_b,
                    owned_addr: Some(addr_b),
                })
            }));

        let token_before = rt.observe(0x2222).expect("present").token;
        policy.on_recovery(0x2222);
        let token_after = rt.observe(0x2222).expect("still present").token;

        assert_eq!(
            token_before, token_after,
            "an already-current route must not be rewritten"
        );
        assert_eq!(policy.recovery_count.load(Ordering::Relaxed), 0);
    }
}
