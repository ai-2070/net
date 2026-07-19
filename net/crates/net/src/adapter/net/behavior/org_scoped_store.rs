//! OA-3 §3.3 — the consumer-side scoped-discovery store: where verified,
//! decrypted scoped capabilities live and are queried.
//!
//! # Why a separate store (design note)
//!
//! The plan sketches scoped capabilities as entries "in the fold, under
//! `Owner{…}` / `Grant{…}`". This implementation instead keeps them in a store
//! STRUCTURALLY SEPARATE from the plaintext [`CapabilityFold`], so the mutual
//! invisibility the plan requires (Owner ↔ Grant ↔ Public all invisible to one
//! another and to unscoped queries) is a property of the DATA STRUCTURE rather
//! than of every existing fold query remembering to filter a scope dimension. A
//! confidentiality leak would otherwise be one forgotten `WHERE scope = public`
//! away; here an unscoped query physically cannot reach a scoped entry because
//! it queries a different structure. The two named query surfaces
//! ([`ScopedDiscoveryStore::find_capabilities_for_grant`] and
//! [`ScopedDiscoveryStore::find_owner_private_capabilities`]) are the only way in.
//!
//! Entries arrive already verified and decrypted from the OA3-3 ingest authority
//! ([`verify_scoped_ingest`](super::org_scoped_ingest::verify_scoped_ingest)); this
//! layer never decrypts or verifies — it only stores, freshness-orders, expires,
//! and partitions.

use std::collections::BTreeMap;

use super::org_scoped_ingest::{CapabilityAudienceScope, VerifiedScopedCapability};
use crate::adapter::net::identity::EntityId;

/// Outcome of ingesting a verified scoped capability into the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopedStoreOutcome {
    /// A new `(scope, provider)` entry was stored.
    Inserted,
    /// A newer generation replaced an existing `(scope, provider)` entry.
    Updated,
    /// The incoming generation was not newer than the stored one — ignored
    /// (monotone freshness, mirroring the CAP-ANN `version` discipline).
    Stale,
    /// A `Public`-scoped capability was handed to the scoped store — refused.
    /// The scoped store holds only the Owner/Grant partitions; Public
    /// capabilities live in the plaintext fold. The OA3-3 verify path never
    /// produces a `Public` scope, so this is a defensive guard.
    RejectedPublic,
}

/// One stored scoped capability plus the freshness/expiry it is ordered by. When
/// `capability` is `None` the entry is a TOMBSTONE: the live capability was
/// swept (expired), but the `generation` high-water is retained until
/// `tombstone_until` so an OLDER generation can never revive the key after a
/// newer one was observed (Kyra OA3 closure — replay/rollback protection that
/// survives a sweep). `tombstone_until` is the max expiry ever seen for the key,
/// so it is bounded by the announcement TTL: once it passes, no
/// previously-accepted envelope for the key can still be in-window.
struct StoredEntry {
    generation: u64,
    expires_at: u64,
    tombstone_until: u64,
    capability: Option<VerifiedScopedCapability>,
}

/// A node's private-discovery store: verified scoped capabilities keyed by
/// `(audience scope, provider)`. Disjoint from the plaintext capability fold.
#[derive(Default)]
pub struct ScopedDiscoveryStore {
    entries: BTreeMap<(CapabilityAudienceScope, EntityId), StoredEntry>,
}

impl ScopedDiscoveryStore {
    /// A fresh, empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Ingest a verified scoped capability. At most one entry is kept per
    /// `(scope, provider)`; the newest generation wins, and an older-or-equal
    /// generation is [`ScopedStoreOutcome::Stale`] and ignored. A `Public` scope
    /// is refused ([`ScopedStoreOutcome::RejectedPublic`]).
    pub fn ingest(&mut self, capability: VerifiedScopedCapability) -> ScopedStoreOutcome {
        if matches!(capability.scope(), CapabilityAudienceScope::Public) {
            return ScopedStoreOutcome::RejectedPublic;
        }
        let key = (capability.scope().clone(), capability.provider().clone());
        let generation = capability.generation();
        let expires_at = capability.expires_at();
        match self.entries.get_mut(&key) {
            // An older-or-equal generation is Stale even against a TOMBSTONE — the
            // retained high-water blocks reviving a key with a rolled-back
            // generation after a newer one was seen (and swept).
            Some(existing) if generation <= existing.generation => ScopedStoreOutcome::Stale,
            Some(existing) => {
                // Newer generation: (re)populate the entry and extend the
                // tombstone watermark to the max expiry ever seen, so a later
                // sweep still blocks an older-generation replay.
                existing.generation = generation;
                existing.expires_at = expires_at;
                existing.tombstone_until = existing.tombstone_until.max(expires_at);
                existing.capability = Some(capability);
                ScopedStoreOutcome::Updated
            }
            None => {
                self.entries.insert(
                    key,
                    StoredEntry {
                        generation,
                        expires_at,
                        tombstone_until: expires_at,
                        capability: Some(capability),
                    },
                );
                ScopedStoreOutcome::Inserted
            }
        }
    }

    /// Capabilities discovered under a specific grant — entries whose scope is
    /// `Grant` with this `grant_id`, filtered by `predicate`. EXPIRY-SAFE: an
    /// entry past its `expires_at` at `now_secs` is excluded even if it has not
    /// yet been swept, so sweeping is an optimization, not the correctness
    /// boundary (Kyra OA3 closure). Tombstones, owner entries, and entries from
    /// other grants are invisible.
    pub fn find_capabilities_for_grant<F>(
        &self,
        grant_id: &[u8; 32],
        now_secs: u64,
        mut predicate: F,
    ) -> Vec<&VerifiedScopedCapability>
    where
        F: FnMut(&VerifiedScopedCapability) -> bool,
    {
        self.entries
            .values()
            .filter(|e| now_secs < e.expires_at)
            .filter_map(|e| e.capability.as_ref())
            .filter(|c| {
                matches!(
                    c.scope(),
                    CapabilityAudienceScope::Grant { grant_id: g, .. } if g == grant_id
                )
            })
            .filter(|c| predicate(c))
            .collect()
    }

    /// Owner-scoped internal private capabilities, filtered by `predicate`.
    /// EXPIRY-SAFE (see [`Self::find_capabilities_for_grant`]). Grant entries,
    /// tombstones, and (structurally) public capabilities are invisible.
    pub fn find_owner_private_capabilities<F>(
        &self,
        now_secs: u64,
        mut predicate: F,
    ) -> Vec<&VerifiedScopedCapability>
    where
        F: FnMut(&VerifiedScopedCapability) -> bool,
    {
        self.entries
            .values()
            .filter(|e| now_secs < e.expires_at)
            .filter_map(|e| e.capability.as_ref())
            .filter(|c| matches!(c.scope(), CapabilityAudienceScope::Owner { .. }))
            .filter(|c| predicate(c))
            .collect()
    }

    /// Drop the live capability of each expired entry (leaving a generation
    /// tombstone), and fully forget a key once its tombstone watermark has passed
    /// (no previously-accepted envelope can still be in-window). Returns how many
    /// LIVE capabilities were dropped this call.
    pub fn sweep_expired(&mut self, now_secs: u64) -> usize {
        let mut swept = 0;
        self.entries.retain(|_, e| {
            if e.capability.is_some() && now_secs >= e.expires_at {
                e.capability = None; // live -> tombstone (generation high-water kept)
                swept += 1;
            }
            now_secs < e.tombstone_until
        });
        swept
    }

    /// Number of LIVE stored scoped capabilities (tombstones excluded).
    pub fn len(&self) -> usize {
        self.entries
            .values()
            .filter(|e| e.capability.is_some())
            .count()
    }

    /// Whether the store holds no LIVE scoped capabilities.
    pub fn is_empty(&self) -> bool {
        !self.entries.values().any(|e| e.capability.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::org::OrgId;

    fn provider(seed: u8) -> EntityId {
        EntityId::from_bytes([seed; 32])
    }

    fn org(seed: u8) -> OrgId {
        OrgId::from_bytes([seed; 32])
    }

    fn owner_cap(provider_seed: u8, generation: u64, expires_at: u64) -> VerifiedScopedCapability {
        VerifiedScopedCapability::for_test(
            CapabilityAudienceScope::Owner {
                org_id: org(1),
                audience_handle: [0x11; 32],
            },
            provider(provider_seed),
            org(1),
            generation,
            expires_at,
            b"owner-descriptor".to_vec(),
        )
    }

    fn grant_cap(
        grant_id: [u8; 32],
        provider_seed: u8,
        generation: u64,
        expires_at: u64,
    ) -> VerifiedScopedCapability {
        VerifiedScopedCapability::for_test(
            CapabilityAudienceScope::Grant {
                grant_id,
                audience_handle: [0x22; 32],
            },
            provider(provider_seed),
            org(2),
            generation,
            expires_at,
            b"grant-descriptor".to_vec(),
        )
    }

    #[test]
    fn ingest_reports_insert_update_and_stale() {
        let mut store = ScopedDiscoveryStore::new();
        assert_eq!(
            store.ingest(owner_cap(3, 1, 1000)),
            ScopedStoreOutcome::Inserted
        );
        // Newer generation for the same (scope, provider) updates.
        assert_eq!(
            store.ingest(owner_cap(3, 2, 1000)),
            ScopedStoreOutcome::Updated
        );
        // Older-or-equal generation is stale and ignored.
        assert_eq!(
            store.ingest(owner_cap(3, 2, 1000)),
            ScopedStoreOutcome::Stale
        );
        assert_eq!(
            store.ingest(owner_cap(3, 1, 1000)),
            ScopedStoreOutcome::Stale
        );
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn public_scope_is_refused() {
        let mut store = ScopedDiscoveryStore::new();
        let public = VerifiedScopedCapability::for_test(
            CapabilityAudienceScope::Public,
            provider(3),
            org(1),
            1,
            1000,
            b"x".to_vec(),
        );
        assert_eq!(store.ingest(public), ScopedStoreOutcome::RejectedPublic);
        assert!(store.is_empty());
    }

    #[test]
    fn owner_and_grant_partitions_are_mutually_invisible() {
        let mut store = ScopedDiscoveryStore::new();
        let grant_x = [0xAA; 32];
        let grant_y = [0xBB; 32];
        store.ingest(owner_cap(3, 1, 1000));
        store.ingest(grant_cap(grant_x, 4, 1, 1000));
        store.ingest(grant_cap(grant_y, 5, 1, 1000));
        assert_eq!(store.len(), 3);

        // The grant-X query sees only grant-X providers — not owner, not grant-Y.
        let x = store.find_capabilities_for_grant(&grant_x, 0, |_| true);
        assert_eq!(x.len(), 1);
        assert_eq!(x[0].provider(), &provider(4));

        // The grant-Y query sees only grant-Y.
        let y = store.find_capabilities_for_grant(&grant_y, 0, |_| true);
        assert_eq!(y.len(), 1);
        assert_eq!(y[0].provider(), &provider(5));

        // The owner query sees only the owner entry — no grants.
        let owner = store.find_owner_private_capabilities(0, |_| true);
        assert_eq!(owner.len(), 1);
        assert_eq!(owner[0].provider(), &provider(3));

        // A grant query for an unknown grant sees nothing.
        assert!(store
            .find_capabilities_for_grant(&[0xCC; 32], 0, |_| true)
            .is_empty());
    }

    #[test]
    fn predicate_filters_within_a_partition() {
        let mut store = ScopedDiscoveryStore::new();
        let grant = [0xAA; 32];
        store.ingest(grant_cap(grant, 4, 1, 1000));
        store.ingest(grant_cap(grant, 5, 1, 1000));
        // Predicate selecting only provider(5).
        let hits = store.find_capabilities_for_grant(&grant, 0, |c| c.provider() == &provider(5));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].provider(), &provider(5));
    }

    #[test]
    fn distinct_providers_under_one_grant_coexist() {
        let mut store = ScopedDiscoveryStore::new();
        let grant = [0xAA; 32];
        store.ingest(grant_cap(grant, 4, 1, 1000));
        store.ingest(grant_cap(grant, 5, 1, 1000));
        assert_eq!(
            store.find_capabilities_for_grant(&grant, 0, |_| true).len(),
            2
        );
    }

    #[test]
    fn sweep_removes_only_expired_entries() {
        let mut store = ScopedDiscoveryStore::new();
        store.ingest(owner_cap(3, 1, 1000)); // expires 1000
        store.ingest(grant_cap([0xAA; 32], 4, 1, 5000)); // expires 5000
                                                         // At t=2000 the owner entry (expires 1000) is gone; the grant survives.
        assert_eq!(store.sweep_expired(2000), 1);
        assert_eq!(store.len(), 1);
        assert!(store
            .find_owner_private_capabilities(2000, |_| true)
            .is_empty());
        assert_eq!(
            store
                .find_capabilities_for_grant(&[0xAA; 32], 2000, |_| true)
                .len(),
            1
        );
    }

    #[test]
    fn queries_exclude_expired_entries_before_any_sweep() {
        // Expiry safety is a property of the QUERY, not of remembering to sweep.
        let mut store = ScopedDiscoveryStore::new();
        let grant = [0xAA; 32];
        store.ingest(grant_cap(grant, 4, 1, 1000)); // expires 1000
        assert_eq!(
            store
                .find_capabilities_for_grant(&grant, 500, |_| true)
                .len(),
            1,
            "visible before expiry"
        );
        assert!(
            store
                .find_capabilities_for_grant(&grant, 2000, |_| true)
                .is_empty(),
            "excluded past expiry even with no sweep",
        );
    }

    #[test]
    fn a_swept_newer_generation_cannot_be_revived_by_an_older_one() {
        // gen1 (long TTL) then gen2 (newer, short TTL). gen2 expires and is swept,
        // but the older gen1 envelope is still in-window — replaying it must NOT
        // revive the key (the generation high-water survives the sweep).
        let mut store = ScopedDiscoveryStore::new();
        let grant = [0xAA; 32];
        store.ingest(grant_cap(grant, 4, 1, 5000)); // gen 1, expires 5000
        assert_eq!(
            store.ingest(grant_cap(grant, 4, 2, 2000)), // gen 2, expires 2000
            ScopedStoreOutcome::Updated
        );
        // Sweep at t=3000: gen 2's live capability (expired at 2000) becomes a
        // tombstone; the watermark (max expiry seen = 5000) is retained.
        store.sweep_expired(3000);
        assert!(store
            .find_capabilities_for_grant(&grant, 3000, |_| true)
            .is_empty());
        // Replay the OLDER generation 1 (still unexpired at 3000): refused.
        assert_eq!(
            store.ingest(grant_cap(grant, 4, 1, 5000)),
            ScopedStoreOutcome::Stale
        );
        assert!(store
            .find_capabilities_for_grant(&grant, 3000, |_| true)
            .is_empty());
    }
}
