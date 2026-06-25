//! Single-island contention (plan Phase B): walk an ordered island
//! list claiming the first one that's free, and the brutal-test
//! proof that N contending claimants over M oversubscribed islands
//! yield exactly one winner per island with zero double-grants.
//!
//! No new protocol is needed — the island *is* the `ResourceId`, so
//! one island is one [`super::single_island_claim`] CAS, and the
//! reservation fold's write lock serializes concurrent claims into a
//! deterministic single winner. This module is the "loser re-queries
//! and tries the next island" loop on top of that, plus the test
//! that pins the invariant under real concurrency.

use crate::adapter::net::behavior::fold::{Fold, IslandId, NodeId, ReservationFold};
use crate::adapter::net::identity::EntityKeypair;

use super::claim::{single_island_claim, ClaimError, ClaimOutcome};

/// Walk `islands` in claim order (as produced by
/// [`super::match_islands`]) and reserve the first one that's free.
/// Returns `Some(island)` on the first win, `None` if every island
/// in the list was already held (the caller re-runs the match
/// pipeline and/or backs off — Phase E).
///
/// `generation` is advanced past every attempt so the caller's next
/// announcement for the won island (activate / release) uses a fresh
/// strictly-higher generation, keeping the per-publisher anti-reorder
/// invariant intact. A rejected attempt is a clean "someone else has
/// it" — distinct from a [`ClaimError`], which is a sign/apply-level
/// failure and aborts the walk.
pub fn claim_first_available(
    reservations: &Fold<ReservationFold>,
    keypair: &EntityKeypair,
    node_id: NodeId,
    generation: &mut u64,
    islands: &[IslandId],
    until_unix_us: u64,
) -> Result<Option<IslandId>, ClaimError> {
    for &island in islands {
        let gen = *generation;
        *generation += 1;
        match single_island_claim(reservations, keypair, node_id, gen, island, until_unix_us)? {
            ClaimOutcome::Won => return Ok(Some(island)),
            ClaimOutcome::Lost => continue,
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::{Arc, Barrier};
    use std::time::Duration;

    use super::*;
    use crate::adapter::net::behavior::fold::{ReservationQuery, ReservationState};
    use crate::adapter::net::current_timestamp_micros;

    fn new_reservations() -> Fold<ReservationFold> {
        Fold::with_sweep_interval(Duration::ZERO)
    }

    fn fresh_deadline() -> u64 {
        current_timestamp_micros() + 60_000_000
    }

    #[test]
    fn claim_first_available_skips_held_islands() {
        let fold = new_reservations();
        let holder = EntityKeypair::generate();
        let hn = holder.entity_id().node_id();
        // Island 0x10 and 0x11 are already held by `holder`.
        let mut g = 1;
        claim_first_available(&fold, &holder, hn, &mut g, &[0x10, 0x11], fresh_deadline()).unwrap();
        // (Only 0x10 was claimed — the helper stops at the first win.)
        // Hold 0x11 too so the next claimant must skip both.
        super::single_island_claim(&fold, &holder, hn, g, 0x11, fresh_deadline()).unwrap();

        let claimant = EntityKeypair::generate();
        let cn = claimant.entity_id().node_id();
        let mut cg = 1;
        let got = claim_first_available(
            &fold,
            &claimant,
            cn,
            &mut cg,
            &[0x10, 0x11, 0x12],
            fresh_deadline(),
        )
        .unwrap();
        assert_eq!(got, Some(0x12), "must skip the two held islands");
        assert_eq!(
            fold.query(ReservationQuery::State(0x12))[0].1.holder(),
            Some(cn)
        );
    }

    #[test]
    fn claim_first_available_returns_none_when_all_held() {
        let fold = new_reservations();
        let holder = EntityKeypair::generate();
        let hn = holder.entity_id().node_id();
        for (i, island) in [0x10, 0x11].iter().enumerate() {
            super::single_island_claim(&fold, &holder, hn, i as u64 + 1, *island, fresh_deadline())
                .unwrap();
        }
        let claimant = EntityKeypair::generate();
        let cn = claimant.entity_id().node_id();
        let mut cg = 1;
        let got = claim_first_available(
            &fold,
            &claimant,
            cn,
            &mut cg,
            &[0x10, 0x11],
            fresh_deadline(),
        )
        .unwrap();
        assert_eq!(got, None);
    }

    /// Brutal test #1 (plan Phase B): N daemons contend over M
    /// oversubscribed islands, sustained → exactly one winner per
    /// island per round, losers re-query, zero double-grants.
    ///
    /// Each of N claimant threads (distinct identities) races to
    /// `claim_first_available` over the same island list, all
    /// released together by a barrier to maximize contention. We
    /// assert: every island ends with exactly one holder; the set of
    /// winners' islands is exactly the island set (each claimed
    /// once); no two threads hold the same island; and at most M
    /// threads won (N − M lost and got `None`).
    #[test]
    fn concurrent_claimants_yield_one_winner_per_island() {
        const N: usize = 24; // claimants
        const M: u64 = 8; // islands (oversubscribed: N > M)

        let fold = Arc::new(new_reservations());
        let islands: Vec<IslandId> = (0x100..0x100 + M).collect();
        let barrier = Arc::new(Barrier::new(N));
        let deadline = fresh_deadline();

        let handles: Vec<_> = (0..N)
            .map(|_| {
                let fold = fold.clone();
                let barrier = barrier.clone();
                let islands = islands.clone();
                std::thread::spawn(move || {
                    let kp = EntityKeypair::generate();
                    let node = kp.entity_id().node_id();
                    let mut gen = 1u64;
                    // All threads line up, then claim at once.
                    barrier.wait();
                    let won = claim_first_available(&fold, &kp, node, &mut gen, &islands, deadline)
                        .expect("claim attempt");
                    won.map(|island| (node, island))
                })
            })
            .collect();

        let winners: Vec<(NodeId, IslandId)> = handles
            .into_iter()
            .filter_map(|h| h.join().unwrap())
            .collect();

        // Exactly M winners — every island claimed once, no more.
        assert_eq!(
            winners.len() as u64,
            M,
            "exactly one winner per island, no partial/extra holds",
        );
        // Each island claimed by exactly one winner.
        let won_islands: HashSet<IslandId> = winners.iter().map(|(_, i)| *i).collect();
        assert_eq!(won_islands.len() as u64, M, "no island claimed twice");
        assert_eq!(
            won_islands,
            islands.iter().copied().collect::<HashSet<_>>(),
            "every island ended up claimed",
        );
        // No two winners share an identity-island pairing that the
        // fold disagrees with: the fold's recorded holder for each
        // island matches the winner that claimed it.
        for (node, island) in &winners {
            let state = fold.query(ReservationQuery::State(*island));
            assert!(
                matches!(state[0].1, ReservationState::Reserved { holder, .. } if holder == *node),
                "island {island:#x} must be Reserved by its claimed winner {node:#x}",
            );
        }
    }
}
