//! OA2-E0.3 of `docs/plans/OA2E_INTEGRATION_DESIGN.md` — direct-
//! session caller identity for provider admission.
//!
//! On the inbound RPC path there are two "identities":
//!
//! - `from_node` — the AEAD-authenticated LAST-HOP session peer
//!   (the only cryptographically authenticated identity on the
//!   wire), resolved to a TOFU-pinned [`EntityId`] via
//!   `peer_entity_ids`.
//! - `EventMeta::origin_hash` — a wire-claimed origin; routing
//!   metadata, NOT authenticated on its own.
//!
//! For a RELAYED RPC the two diverge: `from_node` is the relay, not
//! the original caller. OA2-E v1 is therefore DIRECT-SESSION-ONLY
//! (design §E0.3): the authenticated caller is the session peer's
//! pinned entity, accepted only when the claimed origin matches
//! that entity's own origin hash. A relayed or forged-origin
//! request is refused rather than mistaken for the caller — relay
//! identity and caller identity are never collapsed. End-to-end
//! authenticated caller identity through a relay is deferred.
//!
//! Unwired in E0: the admission gate (E1) calls
//! [`resolve_direct_caller`] and feeds the result to
//! `AdmissionContext.authenticated_caller`.

use dashmap::DashMap;

use crate::adapter::net::identity::EntityId;

/// Why a direct-session caller identity could not be established.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallerIdentityError {
    /// No TOFU-pinned entity for the session peer `from_node` — the
    /// wire session resolved no authenticated identity (the
    /// loopback/test sentinel `from_node == 0`, or an unpinned
    /// peer). A protected call has no authenticated caller.
    Unavailable,
    /// The claimed origin hash does not match the session peer's
    /// pinned entity. Either a forged origin, or a RELAYED request
    /// whose `from_node` is the relay rather than the caller —
    /// direct-session-only v1 refuses both.
    OriginMismatch,
}

impl std::fmt::Display for CallerIdentityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable => {
                write!(f, "no authenticated entity for the inbound session peer")
            }
            Self::OriginMismatch => write!(
                f,
                "claimed origin does not match the direct session peer \
                 (relayed or forged; direct-session-only in v1)"
            ),
        }
    }
}

impl std::error::Error for CallerIdentityError {}

/// Resolve the AUTHENTICATED direct caller for an inbound protected
/// RPC (OA2-E0.3, direct-session-only v1).
///
/// `from_node` is the AEAD-authenticated last-hop session peer;
/// `claimed_origin_hash` is the wire `EventMeta::origin_hash`. The
/// caller is `from_node`'s TOFU-pinned entity, and the claim is
/// accepted ONLY when it equals that entity's own
/// [`EntityId::origin_hash`] — so a relay (`from_node` ≠ caller) or
/// a forged origin is refused, never mistaken for the caller.
pub fn resolve_direct_caller(
    peer_entity_ids: &DashMap<u64, EntityId>,
    from_node: u64,
    claimed_origin_hash: u64,
) -> Result<EntityId, CallerIdentityError> {
    // `from_node == 0` is the loopback/test sentinel — never a real
    // authenticated production session.
    if from_node == 0 {
        return Err(CallerIdentityError::Unavailable);
    }
    let caller = peer_entity_ids
        .get(&from_node)
        .map(|e| e.clone())
        .ok_or(CallerIdentityError::Unavailable)?;
    if caller.origin_hash() != claimed_origin_hash {
        return Err(CallerIdentityError::OriginMismatch);
    }
    Ok(caller)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::identity::EntityKeypair;

    fn entity(seed: u8) -> EntityId {
        EntityKeypair::from_bytes([seed; 32]).entity_id().clone()
    }

    #[test]
    fn direct_session_caller_resolves_when_origin_matches() {
        let caller = entity(0x11);
        let map: DashMap<u64, EntityId> = DashMap::new();
        let node_id = 0xABCD;
        map.insert(node_id, caller.clone());

        let resolved =
            resolve_direct_caller(&map, node_id, caller.origin_hash()).expect("direct caller");
        assert_eq!(resolved, caller);
    }

    #[test]
    fn unpinned_peer_and_sentinel_are_unavailable() {
        let map: DashMap<u64, EntityId> = DashMap::new();
        // Unpinned node.
        assert_eq!(
            resolve_direct_caller(&map, 0x1234, 999),
            Err(CallerIdentityError::Unavailable)
        );
        // Loopback/test sentinel.
        map.insert(0, entity(0x22));
        assert_eq!(
            resolve_direct_caller(&map, 0, 999),
            Err(CallerIdentityError::Unavailable)
        );
    }

    #[test]
    fn forged_origin_is_refused() {
        let peer = entity(0x33);
        let map: DashMap<u64, EntityId> = DashMap::new();
        let node_id = 0x77;
        map.insert(node_id, peer.clone());
        // Claim some other origin than the session peer's own.
        let forged = peer.origin_hash().wrapping_add(1);
        assert_eq!(
            resolve_direct_caller(&map, node_id, forged),
            Err(CallerIdentityError::OriginMismatch)
        );
    }

    #[test]
    fn relayed_request_is_not_mistaken_for_the_caller() {
        // The frame arrives on the session with the RELAY (the
        // last-hop peer), but claims the original caller's origin.
        let relay = entity(0x44);
        let caller = entity(0x55);
        assert_ne!(relay.origin_hash(), caller.origin_hash());
        let map: DashMap<u64, EntityId> = DashMap::new();
        let relay_node = 0x99;
        map.insert(relay_node, relay.clone());

        // from_node is the relay; the claimed origin is the caller's.
        // Direct-only v1 refuses (never returns `caller`).
        assert_eq!(
            resolve_direct_caller(&map, relay_node, caller.origin_hash()),
            Err(CallerIdentityError::OriginMismatch)
        );
        // And even a matching claim only ever yields the RELAY's own
        // identity — never an inferred end-to-end caller.
        assert_eq!(
            resolve_direct_caller(&map, relay_node, relay.origin_hash()).expect("relay entity"),
            relay
        );
    }
}
