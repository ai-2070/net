//! Subnet session admission — S3 of
//! `docs/internal/plans/SUBNET_AUTH_PLAN.md`.
//!
//! Verifier-side state for the challenge/presentation exchange that
//! turns a presented [`SubnetCredentialSet`](super::auth::SubnetCredentialSet)
//! into an immutable [`VerifiedSubnetContext`] on one session
//! incarnation:
//!
//! - [`SubnetChallengeStore`] mints, retains, and consumes the
//!   one-use 32-byte challenges;
//! - [`SubnetContextStore`] holds the compiled contexts, keyed by
//!   session incarnation so a replaced session can never inherit its
//!   predecessor's authority.
//!
//! Both are bounded and self-evicting. The `auth_failures` map is the
//! cautionary precedent here: it has no cap and no eviction site, so
//! neither structure copies its shape.

use std::time::{Duration, Instant};

use dashmap::DashMap;

use super::auth::{ExpectedBinding, VerifiedSubnetContext};
use crate::adapter::net::identity::EntityId;

/// How long an unconsumed challenge stays valid. Short: the subject
/// signs and returns it in one round trip, and an unbounded window
/// would only widen the replay surface.
pub const SUBNET_CHALLENGE_TTL: Duration = Duration::from_secs(30);

/// Maximum outstanding challenges per peer. A peer that floods
/// admission attempts evicts only its own oldest entries.
pub const MAX_CHALLENGES_PER_PEER: usize = 4;

/// Maximum peers with outstanding challenges. Node-wide backstop
/// against a fan-out flood from many sources.
pub const MAX_CHALLENGE_PEERS: usize = 4096;

/// Current unix time in seconds, saturating at the epoch on a
/// pre-epoch clock rather than panicking (same discipline as the
/// token and org modules).
pub fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// One outstanding challenge, bound to the exact session incarnation
/// it was issued on.
#[derive(Debug, Clone)]
struct PendingChallenge {
    nonce: [u8; 32],
    session_id: u64,
    issued_at: Instant,
}

/// Verifier-side challenge state.
///
/// A challenge is single-use: [`Self::consume`] removes it whether or
/// not verification later succeeds, so a captured presentation cannot
/// be replayed against the same nonce.
#[derive(Debug, Default)]
pub struct SubnetChallengeStore {
    /// `node_id → outstanding challenges`, newest last.
    by_peer: DashMap<u64, Vec<PendingChallenge>>,
}

impl SubnetChallengeStore {
    /// Empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Mint and retain a fresh challenge for `node_id` on
    /// `session_id`, returning the nonce to send.
    ///
    /// Aborts rather than unwinding on RNG failure: a predictable
    /// challenge would let a captured presentation be replayed, the
    /// exact property this exchange exists to provide (same stance as
    /// `PermissionToken` nonce generation).
    pub fn issue(&self, node_id: u64, session_id: u64, now: Instant) -> Option<[u8; 32]> {
        if !self.by_peer.contains_key(&node_id) && self.by_peer.len() >= MAX_CHALLENGE_PEERS {
            // Pruning is otherwise per-peer, on that peer's own next
            // touch, so the ceiling can be held entirely by peers that
            // requested one challenge and never returned — long past
            // TTL, still counted. Sweeping here keeps the bound a cap
            // on live challenges rather than on peers ever seen, and
            // costs O(peers) only on the refusal path, which a caller
            // reaches only after 4096 completed handshakes.
            self.by_peer.retain(|_, slot| {
                slot.retain(|c| now.duration_since(c.issued_at) < SUBNET_CHALLENGE_TTL);
                !slot.is_empty()
            });
            // Check-then-insert below can overshoot the ceiling by the
            // number of concurrent issuers; immaterial at this bound.
            if self.by_peer.len() >= MAX_CHALLENGE_PEERS {
                return None;
            }
        }
        let mut nonce = [0u8; 32];
        if let Err(e) = getrandom::fill(&mut nonce) {
            eprintln!(
                "FATAL: subnet challenge getrandom failure ({e:?}); aborting to avoid a predictable challenge"
            );
            std::process::abort();
        }
        let mut slot = self.by_peer.entry(node_id).or_default();
        slot.retain(|c| now.duration_since(c.issued_at) < SUBNET_CHALLENGE_TTL);
        if slot.len() >= MAX_CHALLENGES_PER_PEER {
            slot.remove(0);
        }
        slot.push(PendingChallenge {
            nonce,
            session_id,
            issued_at: now,
        });
        Some(nonce)
    }

    /// Consume the challenge matching `nonce` on `session_id`,
    /// returning the binding a presentation must satisfy.
    ///
    /// Removal is unconditional on match: consumed on accept AND on
    /// reject, so a failed attempt cannot retry the same nonce. An
    /// expired entry is dropped and reported as absent.
    pub fn consume(
        &self,
        node_id: u64,
        session_id: u64,
        nonce: &[u8; 32],
        verifier: &EntityId,
        now: Instant,
    ) -> Option<ExpectedBinding> {
        let mut slot = self.by_peer.get_mut(&node_id)?;
        slot.retain(|c| now.duration_since(c.issued_at) < SUBNET_CHALLENGE_TTL);
        let idx = slot.iter().position(|c| {
            c.session_id == session_id
                && bool::from(subtle::ConstantTimeEq::ct_eq(&c.nonce[..], &nonce[..]))
        })?;
        let found = slot.remove(idx);
        let empty = slot.is_empty();
        drop(slot);
        if empty {
            self.by_peer.remove_if(&node_id, |_, v| v.is_empty());
        }
        Some(ExpectedBinding {
            session_id: found.session_id,
            verifier: verifier.clone(),
            verifier_nonce: found.nonce,
        })
    }

    /// Drop every challenge for a peer — session replacement, peer
    /// failure, and eviction all invalidate outstanding attempts.
    pub fn forget_peer(&self, node_id: u64) {
        self.by_peer.remove(&node_id);
    }

    /// Outstanding challenge count (diagnostics/tests).
    pub fn len(&self) -> usize {
        self.by_peer.iter().map(|e| e.value().len()).sum()
    }

    /// `true` when no challenge is outstanding.
    pub fn is_empty(&self) -> bool {
        self.by_peer.is_empty()
    }
}

/// Compiled per-session subnet contexts.
///
/// Keyed by `node_id` and validated against the live session id on
/// every read: a replaced incarnation reuses the node id, so the
/// stored `session_id` — not the map key — is what makes a stale
/// context unusable. This is the S3 answer to the plan's D3 note that
/// failure *suspicion* is the wrong lifetime boundary; nothing here
/// depends on the failure callback firing.
#[derive(Debug, Default)]
pub struct SubnetContextStore {
    by_peer: DashMap<u64, VerifiedSubnetContext>,
}

impl SubnetContextStore {
    /// Empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Install (or atomically replace) the context for `node_id`.
    /// Replacement publishes a whole new value — a live context is
    /// never mutated in place.
    pub fn install(&self, node_id: u64, ctx: VerifiedSubnetContext) {
        self.by_peer.insert(node_id, ctx);
    }

    /// Context for `node_id` iff it was compiled on `session_id`.
    /// A displaced incarnation gets `None` without any cleanup having
    /// had to run first.
    pub fn get_for_session(&self, node_id: u64, session_id: u64) -> Option<VerifiedSubnetContext> {
        let entry = self.by_peer.get(&node_id)?;
        (entry.session_id == session_id).then(|| entry.clone())
    }

    /// Drop the context for a peer (failure, eviction, withdrawal).
    pub fn forget_peer(&self, node_id: u64) {
        self.by_peer.remove(&node_id);
    }

    /// Drop contexts whose authority auth epoch is behind `current` —
    /// the off-path invalidation an accepted revocation floor
    /// triggers. Returns how many were dropped.
    pub fn invalidate_stale_epoch(&self, authority: &EntityId, current: u64) -> usize {
        let stale: Vec<u64> = self
            .by_peer
            .iter()
            .filter(|e| &e.value().authority == authority && e.value().subnet_auth_epoch < current)
            .map(|e| *e.key())
            .collect();
        for node_id in &stale {
            self.by_peer.remove(node_id);
        }
        stale.len()
    }

    /// Drop contexts minted under a superseded topology epoch.
    /// Reparenting changes what a path means, so old ancestry
    /// authority must not survive it.
    pub fn invalidate_stale_topology(&self, current_topology_epoch: u32) -> usize {
        let stale: Vec<u64> = self
            .by_peer
            .iter()
            .filter(|e| e.value().topology_epoch != current_topology_epoch)
            .map(|e| *e.key())
            .collect();
        for node_id in &stale {
            self.by_peer.remove(node_id);
        }
        stale.len()
    }

    /// Installed context count (diagnostics/tests).
    pub fn len(&self) -> usize {
        self.by_peer.len()
    }

    /// `true` when no context is installed.
    pub fn is_empty(&self) -> bool {
        self.by_peer.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The peer ceiling must cap *live* challenges, not peers ever
    /// seen. Per-peer pruning runs only when that peer is touched
    /// again, so peers that request one challenge and never return
    /// would otherwise hold the ceiling forever and wedge admission
    /// for every peer the store has not seen.
    #[test]
    fn the_peer_ceiling_reclaims_expired_peers_before_refusing() {
        let store = SubnetChallengeStore::new();
        let t0 = Instant::now();
        for peer in 0..MAX_CHALLENGE_PEERS as u64 {
            assert!(store.issue(peer, 1, t0).is_some(), "fill peer {peer}");
        }

        // While every entry is live the ceiling holds for a new peer
        // (a known peer is still served — it adds no map entry).
        let newcomer = MAX_CHALLENGE_PEERS as u64 + 7;
        assert!(store.issue(newcomer, 1, t0).is_none());
        assert!(store.issue(0, 2, t0).is_some());

        // Past TTL the same refusal path reclaims the expired peers
        // instead of refusing on their count.
        let expired = t0 + SUBNET_CHALLENGE_TTL;
        assert!(
            store.issue(newcomer, 1, expired).is_some(),
            "an all-expired ceiling must not refuse a new peer"
        );
        assert_eq!(store.len(), 1, "only the newcomer's challenge survives");
    }

    /// The sweep removes only expired entries — a live challenge
    /// issued shortly before the ceiling refusal is still consumable
    /// afterwards.
    #[test]
    fn the_ceiling_sweep_spares_live_challenges() {
        let store = SubnetChallengeStore::new();
        let t0 = Instant::now();
        for peer in 0..(MAX_CHALLENGE_PEERS as u64 - 1) {
            assert!(store.issue(peer, 1, t0).is_some());
        }
        let live_peer = MAX_CHALLENGE_PEERS as u64;
        let mid = t0 + SUBNET_CHALLENGE_TTL / 2;
        let live_nonce = store.issue(live_peer, 9, mid).expect("fills the ceiling");

        let later = t0 + SUBNET_CHALLENGE_TTL;
        let newcomer = live_peer + 1;
        assert!(store.issue(newcomer, 1, later).is_some());

        let verifier = EntityId::from_bytes([0xAB; 32]);
        assert!(
            store
                .consume(live_peer, 9, &live_nonce, &verifier, later)
                .is_some(),
            "the live challenge must survive the sweep"
        );
        assert!(
            store.consume(0, 1, &live_nonce, &verifier, later).is_none(),
            "swept peers are gone"
        );
    }
}
