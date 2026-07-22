//! OA-3 §3.3 — the consumer-side scoped-discovery store: where verified,
//! decrypted scoped capabilities live and are queried.
//!
//! # Why a separate store (design note)
//!
//! The plan sketches scoped capabilities as entries "in the fold, under
//! `Owner{…}` / `Grant{…}`". This implementation instead keeps them in a store
//! STRUCTURALLY SEPARATE from the plaintext [`CapabilityFold`](super::fold::CapabilityFold), so the mutual
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

use super::org::OrgId;
use super::org_revocation::OrgRevocationState;
use super::org_scoped_ingest::{CapabilityAudienceScope, VerifiedScopedCapability};
use crate::adapter::net::identity::EntityId;

/// One verified private-discovery candidate (OSDK S1).
///
/// An owned projection of a [`VerifiedScopedCapability`] already admitted by
/// [`verify_scoped_ingest`](super::org_scoped_ingest::verify_scoped_ingest) —
/// the whole envelope chain (outer signature, owner certificate and
/// currentness, audience selection, AEAD open, descriptor binding) ran before
/// the record was stored, and the query that produced this additionally applied
/// expiry and revocation-floor currentness.
///
/// Owned rather than borrowed so a caller never holds the discovery-store lock
/// across an `await`. Carries no ciphertext, no descriptor bytes, and no
/// audience material: discovery says WHERE a capability lives, never that you
/// may invoke it — invocation authority is the separate per-call proof.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivateCapabilityProvider {
    /// The provider entity that announced the capability.
    pub provider: EntityId,
    /// The organization that owns the provider (proved by the provider's
    /// membership certificate at ingest).
    pub owner_org: OrgId,
    /// Effective expiry — the minimum of the envelope, owner-certificate, and
    /// (for granted records) grant windows.
    pub expires_at: u64,
    /// The announcement generation this candidate was learned from.
    pub generation: u64,
}

impl PrivateCapabilityProvider {
    pub(crate) fn from_verified(c: &VerifiedScopedCapability) -> Self {
        Self {
            provider: c.provider().clone(),
            owner_org: *c.owner_org(),
            expires_at: c.expires_at(),
            generation: c.generation(),
        }
    }
}

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
    /// The store is at `ScopedDiscoveryStore::MAX_ENTRIES` and a NEW
    /// `(scope, provider)` key could not be admitted without evicting an
    /// unexpired high-water mark — refused FAIL-CLOSED (Kyra OA3-5). Rollback
    /// protection is never surrendered to admit a new provider; updates to
    /// already-known keys are always permitted, and the provider is re-admitted
    /// once a horizon-passed entry frees a slot.
    AtCapacity,
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

    /// Hard cap on stored `(scope, provider)` entries (live + tombstone). A flood
    /// of distinct providers — each a valid, org-certified envelope — must not
    /// grow the private-discovery store without bound before exposure (Kyra
    /// OA3-5). Enforced FAIL-CLOSED in [`Self::ingest`]: at the cap, only
    /// fully-forgotten (tombstone-horizon-passed) keys are reclaimed, and if the
    /// store is still full a NEW key is refused
    /// ([`ScopedStoreOutcome::AtCapacity`]) rather than evicting an unexpired
    /// high-water mark — so a distinct-provider flood can never roll a known
    /// provider's freshness backward. Updates to already-known keys are never
    /// capacity-gated.
    const MAX_ENTRIES: usize = 8192;

    /// Per-scope cap: no single audience may occupy more than this many of the
    /// [`Self::MAX_ENTRIES`] slots.
    ///
    /// The global cap alone is a bound that is correct in isolation and does
    /// not COMPOSE. Owner-scoped discovery and every installed grant share one
    /// budget, so a single grantor org — which owns its org key and can mint
    /// provider certificates for free — could publish 8192 valid envelopes
    /// under one DISCOVER grant and permanently occupy the whole store,
    /// including the slots this node needs for its OWN owner-scoped
    /// capabilities.
    ///
    /// That was reachable specifically because the fail-closed cardinality fix
    /// (which is correct, and stays) removed eviction: the earlier
    /// evict-to-low-water version self-healed, whereas fail-closed plus an
    /// attacker-chosen retention horizon does not. Clamping `expires_at` at
    /// ingest bounds the horizon; this bounds the blast radius per audience,
    /// so exhausting one scope cannot deny any other.
    ///
    /// Sized so the owner partition plus a full complement of installed grants
    /// each get a meaningful share rather than racing for one pool.
    const MAX_ENTRIES_PER_SCOPE: usize = 1024;

    /// Live + tombstoned entries currently held for `scope`.
    fn entries_in_scope(&self, scope: &CapabilityAudienceScope) -> usize {
        self.entries.keys().filter(|(s, _)| s == scope).count()
    }

    /// Ingest a verified scoped capability. At most one entry is kept per
    /// `(scope, provider)`; the newest generation wins, and an older-or-equal
    /// generation is [`ScopedStoreOutcome::Stale`] and ignored. A `Public` scope
    /// is refused ([`ScopedStoreOutcome::RejectedPublic`]); a NEW key that would
    /// exceed `Self::MAX_ENTRIES` with no forgettable slot to reclaim is refused
    /// [`ScopedStoreOutcome::AtCapacity`]. `now_secs` drives the fail-closed
    /// horizon sweep.
    pub fn ingest(
        &mut self,
        capability: VerifiedScopedCapability,
        now_secs: u64,
    ) -> ScopedStoreOutcome {
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
                // Fail-closed cardinality (Kyra OA3-5): reclaim only
                // FULLY-FORGOTTEN keys (tombstone horizon passed) before admitting
                // a new one — NEVER evict an unexpired high-water mark, or an older
                // generation could replay after its tombstone was dropped. If the
                // store is still full of in-horizon entries, refuse the new key;
                // the provider is re-admitted once a slot frees. (Updates to
                // already-known keys, handled above, are never capacity-gated.)
                if self.entries.len() >= Self::MAX_ENTRIES {
                    self.sweep_expired(now_secs);
                    if self.entries.len() >= Self::MAX_ENTRIES {
                        return ScopedStoreOutcome::AtCapacity;
                    }
                }
                // Per-scope share, checked AFTER the global sweep so a
                // reclaimable slot in this scope is counted. Same fail-closed
                // discipline: refuse the new key rather than evict a live one,
                // so one audience filling its share can never roll back
                // another audience's freshness — or its own.
                if self.entries_in_scope(capability.scope()) >= Self::MAX_ENTRIES_PER_SCOPE {
                    self.sweep_expired(now_secs);
                    if self.entries_in_scope(capability.scope()) >= Self::MAX_ENTRIES_PER_SCOPE {
                        return ScopedStoreOutcome::AtCapacity;
                    }
                }
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
    /// boundary (Kyra OA3 closure). CURRENTNESS-SAFE: an entry whose provider
    /// membership floor in `floors` has risen above the generation it was
    /// admitted against is excluded at read time, so a floor raised AFTER a
    /// successful insert retracts the record immediately — without waiting for a
    /// re-announce or sweep (Kyra OA3-5 closure). Tombstones, owner entries, and
    /// entries from other grants are invisible.
    pub fn find_capabilities_for_grant<F>(
        &self,
        grant_id: &[u8; 32],
        now_secs: u64,
        floors: &OrgRevocationState,
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
            .filter(|c| is_current(c, floors))
            .filter(|c| predicate(c))
            .collect()
    }

    /// Owner-scoped internal private capabilities, filtered by `predicate`.
    /// EXPIRY-SAFE and CURRENTNESS-SAFE (see
    /// [`Self::find_capabilities_for_grant`]). Grant entries, tombstones, and
    /// (structurally) public capabilities are invisible.
    pub fn find_owner_private_capabilities<F>(
        &self,
        now_secs: u64,
        floors: &OrgRevocationState,
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
            .filter(|c| is_current(c, floors))
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

/// Query-time revocation currentness (Kyra OA3-5 closure): a stored record stays
/// visible only while its provider membership floor is still at or below the
/// generation it was admitted against. If the floor for `(owner_org, provider)`
/// has since RISEN above that generation the record is stale and must not be
/// returned — the exact `cert.generation < floor` gate the ingest path applied,
/// re-evaluated against the CURRENT floor view so a post-insert revocation
/// retracts the record without a re-announce or sweep.
fn is_current(cap: &VerifiedScopedCapability, floors: &OrgRevocationState) -> bool {
    floors.floor_for(cap.owner_org(), cap.provider()) <= cap.provider_cert_generation()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::org::{OrgId, OrgKeypair, OrgRevocationBundle};
    use std::collections::BTreeMap;

    /// Fixed membership-cert generation the store fixtures are admitted against.
    /// The currentness witness raises a floor above this to retract a record.
    const FIXTURE_CERT_GEN: u32 = 5;

    /// An empty floor view — the default for tests that don't exercise
    /// query-time revocation currentness (every record admitted against
    /// [`FIXTURE_CERT_GEN`] stays visible under a floor of 0).
    fn no_floors() -> OrgRevocationState {
        OrgRevocationState::empty()
    }

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
            FIXTURE_CERT_GEN,
            None,
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
            FIXTURE_CERT_GEN,
            Some([0x5A; 64]),
            b"grant-descriptor".to_vec(),
        )
    }

    /// A distinct provider entity per index — the `u8` `provider` seed only spans
    /// 256, too few for the cardinality flood.
    fn provider_n(index: u64) -> EntityId {
        let mut bytes = [0u8; 32];
        bytes[..8].copy_from_slice(&index.to_le_bytes());
        EntityId::from_bytes(bytes)
    }

    fn owner_cap_n(
        provider_index: u64,
        generation: u64,
        expires_at: u64,
    ) -> VerifiedScopedCapability {
        VerifiedScopedCapability::for_test(
            CapabilityAudienceScope::Owner {
                org_id: org(1),
                audience_handle: [0x11; 32],
            },
            provider_n(provider_index),
            org(1),
            generation,
            expires_at,
            FIXTURE_CERT_GEN,
            None,
            b"owner-descriptor".to_vec(),
        )
    }

    /// Build a capability in an ARBITRARY scope, so a test can exercise the
    /// per-scope share (§4) rather than only the owner partition.
    fn scoped_cap_in(
        scope: CapabilityAudienceScope,
        provider_index: u64,
        generation: u64,
        expires_at: u64,
    ) -> VerifiedScopedCapability {
        VerifiedScopedCapability::for_test(
            scope,
            provider_n(provider_index),
            org(1),
            generation,
            expires_at,
            FIXTURE_CERT_GEN,
            Some([0x5Au8; 64]),
            b"granted-descriptor".to_vec(),
        )
    }

    /// OA3-5b (Kyra closure): a distinct-provider flood is bounded at
    /// MAX_ENTRIES and refused FAIL-CLOSED (`AtCapacity`) — never by evicting a
    /// known provider's unexpired high-water mark. Updates to known keys are
    /// never capacity-gated.
    #[test]
    fn ingest_bounds_cardinality_fail_closed_under_a_distinct_provider_flood() {
        let mut store = ScopedDiscoveryStore::new();
        // A single-audience flood is now bounded by the PER-SCOPE share, which
        // binds before the global cap (§4).
        let cap = ScopedDiscoveryStore::MAX_ENTRIES_PER_SCOPE;
        for index in 0..cap as u64 {
            assert_eq!(
                store.ingest(owner_cap_n(index, 1, 10_000), 1),
                ScopedStoreOutcome::Inserted
            );
        }
        assert_eq!(store.len(), cap);
        // A further DISTINCT provider is refused; nothing is evicted (every entry
        // is in-horizon at now=1, so the fail-closed sweep frees no slot).
        assert_eq!(
            store.ingest(owner_cap_n(u64::MAX, 1, 10_000), 1),
            ScopedStoreOutcome::AtCapacity
        );
        assert_eq!(store.len(), cap);
        // An UPDATE to an already-known key is never capacity-gated.
        assert_eq!(
            store.ingest(owner_cap_n(0, 2, 10_000), 1),
            ScopedStoreOutcome::Updated
        );
        assert_eq!(store.len(), cap);
    }

    /// §4 — exhausting ONE audience must not deny any other.
    ///
    /// `MAX_ENTRIES` alone is a bound that is correct in isolation and does not
    /// compose: owner discovery and every installed grant shared one 8192-slot
    /// pool, so a single grantor org — which owns its org key and mints
    /// provider certificates for free — could fill the whole store under one
    /// DISCOVER grant and lock this node out of its OWN owner-scoped
    /// capabilities.
    ///
    /// That became reachable when eviction was (correctly) removed for the
    /// rollback-preservation fix: the earlier evict-to-low-water version
    /// self-healed, fail-closed does not.
    #[test]
    fn one_exhausted_scope_never_denies_another() {
        let mut store = ScopedDiscoveryStore::new();
        let hostile = CapabilityAudienceScope::Grant {
            grant_id: [0x7Au8; 32],
            audience_handle: [0x7Bu8; 32],
        };

        // A hostile grantor fills its entire share.
        for index in 0..ScopedDiscoveryStore::MAX_ENTRIES_PER_SCOPE as u64 {
            assert_eq!(
                store.ingest(scoped_cap_in(hostile.clone(), index, 1, 10_000), 1),
                ScopedStoreOutcome::Inserted
            );
        }
        assert_eq!(
            store.ingest(scoped_cap_in(hostile.clone(), u64::MAX, 1, 10_000), 1),
            ScopedStoreOutcome::AtCapacity,
            "the hostile scope must be capped at its own share",
        );

        // The owner partition is untouched and still admits.
        assert_eq!(
            store.ingest(owner_cap_n(0, 1, 10_000), 1),
            ScopedStoreOutcome::Inserted,
            "a flooded grant scope must not deny owner-scoped discovery",
        );
        // As does an unrelated grant.
        let other = CapabilityAudienceScope::Grant {
            grant_id: [0x0Cu8; 32],
            audience_handle: [0x0Du8; 32],
        };
        assert_eq!(
            store.ingest(scoped_cap_in(other, 0, 1, 10_000), 1),
            ScopedStoreOutcome::Inserted,
            "a flooded grant scope must not deny an unrelated grant",
        );

        // And the global cap is nowhere near reached — proving the per-scope
        // share, not the global bound, is what stopped the flood.
        assert!(store.len() < ScopedDiscoveryStore::MAX_ENTRIES);
    }

    /// OA3-5b (Kyra closure): capacity pressure never rolls a known provider's
    /// freshness backward. A stored gen-2 high-water survives a full-store flood,
    /// so an older gen-1 replay stays Stale (the flaw in the evict-based version).
    #[test]
    fn capacity_pressure_never_rolls_back_a_known_high_water() {
        let mut store = ScopedDiscoveryStore::new();
        // P (index 0) at generation 2, far-future expiry.
        assert_eq!(
            store.ingest(owner_cap_n(0, 2, 10_000), 1),
            ScopedStoreOutcome::Inserted
        );
        // Fill this scope's share with distinct providers. The per-scope cap
        // (§4) binds before the global one for a single-audience flood, which
        // is the pressure this test is about.
        for index in 1..ScopedDiscoveryStore::MAX_ENTRIES_PER_SCOPE as u64 {
            store.ingest(owner_cap_n(index, 1, 10_000), 1);
        }
        assert_eq!(store.len(), ScopedDiscoveryStore::MAX_ENTRIES_PER_SCOPE);
        // A brand-new provider is refused rather than evicting P's high-water.
        assert_eq!(
            store.ingest(owner_cap_n(u64::MAX, 1, 10_000), 1),
            ScopedStoreOutcome::AtCapacity
        );
        // Replay P at the OLDER generation 1: still Stale — the gen-2 high-water
        // was never evicted under capacity pressure.
        assert_eq!(
            store.ingest(owner_cap_n(0, 1, 10_000), 1),
            ScopedStoreOutcome::Stale
        );
    }

    #[test]
    fn ingest_reports_insert_update_and_stale() {
        let mut store = ScopedDiscoveryStore::new();
        assert_eq!(
            store.ingest(owner_cap(3, 1, 1000), 0),
            ScopedStoreOutcome::Inserted
        );
        // Newer generation for the same (scope, provider) updates.
        assert_eq!(
            store.ingest(owner_cap(3, 2, 1000), 0),
            ScopedStoreOutcome::Updated
        );
        // Older-or-equal generation is stale and ignored.
        assert_eq!(
            store.ingest(owner_cap(3, 2, 1000), 0),
            ScopedStoreOutcome::Stale
        );
        assert_eq!(
            store.ingest(owner_cap(3, 1, 1000), 0),
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
            FIXTURE_CERT_GEN,
            None,
            b"x".to_vec(),
        );
        assert_eq!(store.ingest(public, 0), ScopedStoreOutcome::RejectedPublic);
        assert!(store.is_empty());
    }

    #[test]
    fn owner_and_grant_partitions_are_mutually_invisible() {
        let mut store = ScopedDiscoveryStore::new();
        let grant_x = [0xAA; 32];
        let grant_y = [0xBB; 32];
        store.ingest(owner_cap(3, 1, 1000), 0);
        store.ingest(grant_cap(grant_x, 4, 1, 1000), 0);
        store.ingest(grant_cap(grant_y, 5, 1, 1000), 0);
        assert_eq!(store.len(), 3);

        // The grant-X query sees only grant-X providers — not owner, not grant-Y.
        let x = store.find_capabilities_for_grant(&grant_x, 0, &no_floors(), |_| true);
        assert_eq!(x.len(), 1);
        assert_eq!(x[0].provider(), &provider(4));

        // The grant-Y query sees only grant-Y.
        let y = store.find_capabilities_for_grant(&grant_y, 0, &no_floors(), |_| true);
        assert_eq!(y.len(), 1);
        assert_eq!(y[0].provider(), &provider(5));

        // The owner query sees only the owner entry — no grants.
        let owner = store.find_owner_private_capabilities(0, &no_floors(), |_| true);
        assert_eq!(owner.len(), 1);
        assert_eq!(owner[0].provider(), &provider(3));

        // A grant query for an unknown grant sees nothing.
        assert!(store
            .find_capabilities_for_grant(&[0xCC; 32], 0, &no_floors(), |_| true)
            .is_empty());
    }

    #[test]
    fn predicate_filters_within_a_partition() {
        let mut store = ScopedDiscoveryStore::new();
        let grant = [0xAA; 32];
        store.ingest(grant_cap(grant, 4, 1, 1000), 0);
        store.ingest(grant_cap(grant, 5, 1, 1000), 0);
        // Predicate selecting only provider(5).
        let hits = store
            .find_capabilities_for_grant(&grant, 0, &no_floors(), |c| c.provider() == &provider(5));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].provider(), &provider(5));
    }

    #[test]
    fn distinct_providers_under_one_grant_coexist() {
        let mut store = ScopedDiscoveryStore::new();
        let grant = [0xAA; 32];
        store.ingest(grant_cap(grant, 4, 1, 1000), 0);
        store.ingest(grant_cap(grant, 5, 1, 1000), 0);
        assert_eq!(
            store
                .find_capabilities_for_grant(&grant, 0, &no_floors(), |_| true)
                .len(),
            2
        );
    }

    #[test]
    fn sweep_removes_only_expired_entries() {
        let mut store = ScopedDiscoveryStore::new();
        store.ingest(owner_cap(3, 1, 1000), 0); // expires 1000
        store.ingest(grant_cap([0xAA; 32], 4, 1, 5000), 0); // expires 5000
                                                            // At t=2000 the owner entry (expires 1000) is gone; the grant survives.
        assert_eq!(store.sweep_expired(2000), 1);
        assert_eq!(store.len(), 1);
        assert!(store
            .find_owner_private_capabilities(2000, &no_floors(), |_| true)
            .is_empty());
        assert_eq!(
            store
                .find_capabilities_for_grant(&[0xAA; 32], 2000, &no_floors(), |_| true)
                .len(),
            1
        );
    }

    #[test]
    fn queries_exclude_expired_entries_before_any_sweep() {
        // Expiry safety is a property of the QUERY, not of remembering to sweep.
        let mut store = ScopedDiscoveryStore::new();
        let grant = [0xAA; 32];
        store.ingest(grant_cap(grant, 4, 1, 1000), 0); // expires 1000
        assert_eq!(
            store
                .find_capabilities_for_grant(&grant, 500, &no_floors(), |_| true)
                .len(),
            1,
            "visible before expiry"
        );
        assert!(
            store
                .find_capabilities_for_grant(&grant, 2000, &no_floors(), |_| true)
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
        store.ingest(grant_cap(grant, 4, 1, 5000), 0); // gen 1, expires 5000
        assert_eq!(
            store.ingest(grant_cap(grant, 4, 2, 2000), 0), // gen 2, expires 2000
            ScopedStoreOutcome::Updated
        );
        // Sweep at t=3000: gen 2's live capability (expired at 2000) becomes a
        // tombstone; the watermark (max expiry seen = 5000) is retained.
        store.sweep_expired(3000);
        assert!(store
            .find_capabilities_for_grant(&grant, 3000, &no_floors(), |_| true)
            .is_empty());
        // Replay the OLDER generation 1 (still unexpired at 3000): refused.
        assert_eq!(
            store.ingest(grant_cap(grant, 4, 1, 5000), 0),
            ScopedStoreOutcome::Stale
        );
        assert!(store
            .find_capabilities_for_grant(&grant, 3000, &no_floors(), |_| true)
            .is_empty());
    }

    /// A revocation state that floors `(org_kp's org, member)` at `floor`, built
    /// through a real signed bundle so `floor_for` keys it exactly the way the
    /// ingest path does. Used by the currentness witness.
    fn floor_state(org_kp: &OrgKeypair, member: &EntityId, floor: u32) -> OrgRevocationState {
        let mut floors_map = BTreeMap::new();
        floors_map.insert(member.clone(), floor);
        let bundle = OrgRevocationBundle::try_issue(org_kp, &floors_map).expect("issue bundle");
        let mut state = OrgRevocationState::empty();
        state.merge_bundle(&bundle);
        state
    }

    /// OA3-5 (Kyra closure) — query-time revocation CURRENTNESS: a record
    /// admitted against a membership generation becomes non-queryable the instant
    /// the provider's revocation floor rises above that generation, with no
    /// re-announce and no sweep. A floor at exactly the admitted generation still
    /// returns the record (the ingest gate is `cert.generation < floor`, so
    /// equality is admissible); one generation higher retracts it. The entry
    /// stays physically stored — retraction is a read-time filter, not eviction.
    #[test]
    fn a_raised_provider_floor_retracts_a_stored_record_at_query_time() {
        // The floor is keyed by the ISSUING org's derived id, so the stored
        // record must carry that same org (not the synthetic `org(n)` fixtures).
        let org_kp = OrgKeypair::from_bytes([7u8; 32]);
        let org_id = org_kp.org_id();
        let member = EntityId::from_bytes([9u8; 32]);

        let mut store = ScopedDiscoveryStore::new();
        store.ingest(
            VerifiedScopedCapability::for_test(
                CapabilityAudienceScope::Owner {
                    org_id,
                    audience_handle: [0x11; 32],
                },
                member.clone(),
                org_id,
                1,
                10_000,
                FIXTURE_CERT_GEN,
                None,
                b"owner-descriptor".to_vec(),
            ),
            0,
        );

        // Visible under the empty floor view it was admitted against.
        assert_eq!(
            store
                .find_owner_private_capabilities(0, &no_floors(), |_| true)
                .len(),
            1
        );

        // A floor at EXACTLY the admitted generation is still current.
        let floor_at = floor_state(&org_kp, &member, FIXTURE_CERT_GEN);
        assert_eq!(
            store
                .find_owner_private_capabilities(0, &floor_at, |_| true)
                .len(),
            1,
            "a floor equal to the admitted generation keeps the record"
        );

        // Raise the floor ABOVE the admitted generation: the record disappears
        // immediately from the owner-scoped query.
        let floor_above = floor_state(&org_kp, &member, FIXTURE_CERT_GEN + 1);
        assert!(
            store
                .find_owner_private_capabilities(0, &floor_above, |_| true)
                .is_empty(),
            "a floor above the admitted generation retracts the record at query time"
        );

        // Retraction is a read-time filter, not an eviction: the entry is still
        // physically present (a fresh higher-generation cert could revive it).
        assert_eq!(store.len(), 1);
    }
}
