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

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use super::org::OrgId;
use super::org_grant::CapabilityAuthorityId;
use super::org_revocation::OrgRevocationState;
use super::org_scoped_ingest::{
    CapabilityAudienceScope, PreparedScopedCapability, VerifiedScopedCapability,
};
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
    /// The record declared more capabilities than the indexed layer's per-record
    /// association budget ([`MAX_DECLARATIONS_PER_RECORD`]) — refused FAIL-CLOSED
    /// by [`ScopedDiscoveryState`] BEFORE any store or index mutation, so a
    /// pathological owner descriptor cannot grow the index unbounded (Kyra
    /// OLB-2A.1). Produced only by the indexed layer, never the raw store; a
    /// resource bound, not an authority decision.
    TooManyDeclarations,
}

/// The most capability-authority ids one record may contribute to the index. An
/// owner descriptor is bounded by the wire cap (max 5,737 bytes of scoped
/// plaintext) but could still name enough tiny tags to inflate the index far
/// beyond what the per-scope / node-wide ROW caps bound; this bounds each
/// record's associations, so the node-wide association ceiling is
/// `row-cap × this` (Kyra OLB-2A.1 resource hardening). A granted descriptor
/// always names exactly one capability, so only pathological owner descriptors
/// are ever refused.
const MAX_DECLARATIONS_PER_RECORD: usize = 64;

/// A stored `(scope, provider)` key.
type ScopedKey = (CapabilityAudienceScope, EntityId);

/// The visible-set change an [`ScopedDiscoveryStore::ingest`] produced, so the
/// indexed [`ScopedDiscoveryState`] layer can update its sidecar index in the
/// SAME transaction as the store mutation.
///
/// `swept_live` matters because the fail-closed cardinality guard runs an
/// INTERNAL horizon sweep before refusing a new key: that sweep can demote a
/// live record to a tombstone even when the final `outcome` is
/// [`ScopedStoreOutcome::AtCapacity`], and such a record must leave the index
/// too (the wrapper-only hole the plan flags). The accepted key itself is not
/// listed — the caller already holds the incoming record and derives it from
/// `outcome`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedIngestReport {
    /// What the store did with the incoming record.
    pub outcome: ScopedStoreOutcome,
    /// `(scope, provider)` keys whose LIVE capability the ingest's internal
    /// capacity sweep demoted out of the live set. Empty unless the cardinality
    /// guard ran.
    pub swept_live: Vec<ScopedKey>,
}

/// Cap on the number of distinct capability ids a change stream names before it
/// collapses to [`DirtyCapabilities::RebuildAll`]. Keeps the delta bounded — a
/// burst wider than this reprojects everything rather than growing an unbounded
/// journal.
const MAX_DIRTY_CAPABILITIES: usize = 256;

/// The capabilities whose owner/grant provider set changed since a consumer last
/// drained this stream (OLB-2A.2), or a `RebuildAll` sentinel once more than
/// [`MAX_DIRTY_CAPABILITIES`] distinct capabilities were dirtied. A consumer
/// reconciles exactly the named capabilities, or — on `RebuildAll` — every
/// capability it has a standing interest in.
///
/// Crate-internal: this is drained destructively and belongs to the single
/// node-owned consumer of a stream, never a general public seam (Kyra OLB-2A.2).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum DirtyCapabilities {
    /// Nothing dirtied since the last drain.
    #[default]
    Clean,
    /// Exactly these capability ids were dirtied.
    Caps(BTreeSet<CapabilityAuthorityId>),
    /// More than the bound were dirtied — reproject everything.
    RebuildAll,
}

impl DirtyCapabilities {
    /// Merge a batch of dirtied capabilities, collapsing to `RebuildAll` past the
    /// bound. An empty batch is a no-op.
    fn mark(&mut self, caps: &BTreeSet<CapabilityAuthorityId>) {
        if caps.is_empty() {
            return;
        }
        match self {
            DirtyCapabilities::RebuildAll => {}
            DirtyCapabilities::Clean => {
                *self = if caps.len() > MAX_DIRTY_CAPABILITIES {
                    DirtyCapabilities::RebuildAll
                } else {
                    DirtyCapabilities::Caps(caps.clone())
                };
            }
            DirtyCapabilities::Caps(existing) => {
                existing.extend(caps.iter().copied());
                if existing.len() > MAX_DIRTY_CAPABILITIES {
                    *self = DirtyCapabilities::RebuildAll;
                }
            }
        }
    }

    /// Take the accumulated set, leaving the stream `Clean`.
    fn take(&mut self) -> DirtyCapabilities {
        std::mem::take(self)
    }
}

/// One atomic capture of a private-discovery change stream (Kyra OLB-2A.2): the
/// mutation/sweep generation at the drain instant paired with the capabilities
/// dirtied since the previous drain. Captured under the state lock in ONE
/// operation, so a consumer can never checkpoint a generation and separately
/// miss a delta that committed between two reads. Crate-internal.
///
/// Consumerless in 2A.2 (allowed while consumerless per the OLB-2A closure): its
/// single owner is the node-owned reconciler that lands with the centralized
/// mutate→publish→wake helper in OLB-2A.3. Exercised by the change-stream
/// witnesses today.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PrivateDiscoveryChangeBatch {
    /// The mutation/sweep generation at the drain instant.
    pub generation: u64,
    /// The capabilities dirtied since the previous drain.
    pub dirty: DirtyCapabilities,
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
    ) -> ScopedIngestReport {
        if matches!(capability.scope(), CapabilityAudienceScope::Public) {
            return ScopedIngestReport {
                outcome: ScopedStoreOutcome::RejectedPublic,
                swept_live: Vec::new(),
            };
        }
        let key = (capability.scope().clone(), capability.provider().clone());
        let generation = capability.generation();
        let expires_at = capability.expires_at();
        match self.entries.get_mut(&key) {
            // An older-or-equal generation is Stale even against a TOMBSTONE — the
            // retained high-water blocks reviving a key with a rolled-back
            // generation after a newer one was seen (and swept).
            Some(existing) if generation <= existing.generation => ScopedIngestReport {
                outcome: ScopedStoreOutcome::Stale,
                swept_live: Vec::new(),
            },
            Some(existing) => {
                // Newer generation: (re)populate the entry and extend the
                // tombstone watermark to the max expiry ever seen, so a later
                // sweep still blocks an older-generation replay.
                existing.generation = generation;
                existing.expires_at = expires_at;
                existing.tombstone_until = existing.tombstone_until.max(expires_at);
                existing.capability = Some(capability);
                ScopedIngestReport {
                    outcome: ScopedStoreOutcome::Updated,
                    swept_live: Vec::new(),
                }
            }
            None => {
                // Fail-closed cardinality (Kyra OA3-5): reclaim only
                // FULLY-FORGOTTEN keys (tombstone horizon passed) before admitting
                // a new one — NEVER evict an unexpired high-water mark, or an older
                // generation could replay after its tombstone was dropped. If the
                // store is still full of in-horizon entries, refuse the new key;
                // the provider is re-admitted once a slot frees. (Updates to
                // already-known keys, handled above, are never capacity-gated.)
                //
                // Each internal sweep surfaces the live records it demoted, so the
                // indexed layer drops them from its index even when this ingest
                // ultimately refuses the new key (AtCapacity).
                let mut swept_live = Vec::new();
                if self.entries.len() >= Self::MAX_ENTRIES {
                    swept_live.append(&mut self.sweep_expired(now_secs));
                    if self.entries.len() >= Self::MAX_ENTRIES {
                        return ScopedIngestReport {
                            outcome: ScopedStoreOutcome::AtCapacity,
                            swept_live,
                        };
                    }
                }
                // Per-scope share, checked AFTER the global sweep so a
                // reclaimable slot in this scope is counted. Same fail-closed
                // discipline: refuse the new key rather than evict a live one,
                // so one audience filling its share can never roll back
                // another audience's freshness — or its own.
                if self.entries_in_scope(capability.scope()) >= Self::MAX_ENTRIES_PER_SCOPE {
                    swept_live.append(&mut self.sweep_expired(now_secs));
                    if self.entries_in_scope(capability.scope()) >= Self::MAX_ENTRIES_PER_SCOPE {
                        return ScopedIngestReport {
                            outcome: ScopedStoreOutcome::AtCapacity,
                            swept_live,
                        };
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
                ScopedIngestReport {
                    outcome: ScopedStoreOutcome::Inserted,
                    swept_live,
                }
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
    /// (no previously-accepted envelope can still be in-window). Returns the
    /// `(scope, provider)` keys whose LIVE capability was dropped this call —
    /// every key that transitioned out of the live set (whether it became a
    /// tombstone or was demoted and then forgotten in the same pass), so the
    /// indexed layer can drop exactly those from its index. Pure tombstone
    /// garbage collection changes no live record and is not reported.
    pub fn sweep_expired(&mut self, now_secs: u64) -> Vec<ScopedKey> {
        let mut swept = Vec::new();
        self.entries.retain(|key, e| {
            if e.capability.is_some() && now_secs >= e.expires_at {
                e.capability = None; // live -> tombstone (generation high-water kept)
                swept.push(key.clone());
            }
            now_secs < e.tombstone_until
        });
        swept
    }

    /// The LIVE record stored under `key`, if any (tombstones read as absent).
    /// Lets the indexed [`ScopedDiscoveryState`] apply fresh expiry/floor
    /// currentness to an index bucket hit without exposing the entry map.
    fn live_record(&self, key: &ScopedKey) -> Option<&VerifiedScopedCapability> {
        self.entries.get(key).and_then(|e| e.capability.as_ref())
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

/// A sidecar capability index over the LIVE records in a [`ScopedDiscoveryStore`]
/// (OLB-2A). It is pure storage acceleration — never authority — so it lets an
/// owner-plane capability query be a SINGLE indexed bucket lookup instead of a
/// full store scan with a per-record descriptor decode. It carries no expiry or
/// floor state: every query still applies fresh expiry and revocation-floor
/// currentness to each bucket hit against the store.
///
/// It maintains, for the LIVE set only:
/// - `owner_by_capability`: for each capability, the ordered `(owner scope,
///   provider)` keys that declare it — one map lookup answers an owner query and
///   iterates only matching providers, never visiting an unrelated (e.g. grant)
///   scope;
/// - `declarations_by_record`: what each live `(scope, provider)` declared —
///   owner AND grant — so a record's associations drop on removal without a
///   re-decode, and a mutation's dirtied capabilities are computable for either
///   change stream.
#[derive(Default)]
struct ScopedCapabilityIndex {
    owner_by_capability: BTreeMap<CapabilityAuthorityId, BTreeSet<ScopedKey>>,
    declarations_by_record: BTreeMap<ScopedKey, Arc<[CapabilityAuthorityId]>>,
}

impl ScopedCapabilityIndex {
    /// Index a newly LIVE record. The key must not already be indexed — an
    /// `Updated` record goes through [`Self::replace_record`], which removes the
    /// old declarations first. Only OWNER records enter the owner-capability
    /// projection; a grant record is recorded only in `declarations_by_record`
    /// (the grant plane is served by the store scan and is never an owner-query
    /// answer).
    fn insert_record(&mut self, key: ScopedKey, cap_ids: Arc<[CapabilityAuthorityId]>) {
        if matches!(key.0, CapabilityAudienceScope::Owner { .. }) {
            for cap in cap_ids.iter() {
                self.owner_by_capability
                    .entry(*cap)
                    .or_default()
                    .insert(key.clone());
            }
        }
        self.declarations_by_record.insert(key, cap_ids);
    }

    /// Drop a record that is no longer live from every structure. A key that was
    /// never indexed (declared nothing, or a tombstone GC touching no live row)
    /// is a no-op.
    fn remove_record(&mut self, key: &ScopedKey) {
        let Some(cap_ids) = self.declarations_by_record.remove(key) else {
            return;
        };
        if matches!(key.0, CapabilityAudienceScope::Owner { .. }) {
            for cap in cap_ids.iter() {
                if let Some(bucket) = self.owner_by_capability.get_mut(cap) {
                    bucket.remove(key);
                    if bucket.is_empty() {
                        self.owner_by_capability.remove(cap);
                    }
                }
            }
        }
    }

    /// Re-index an `Updated` record: drop the old declarations, then add the new.
    fn replace_record(&mut self, key: ScopedKey, cap_ids: Arc<[CapabilityAuthorityId]>) {
        self.remove_record(&key);
        self.insert_record(key, cap_ids);
    }
}

/// A [`ScopedDiscoveryStore`] plus a transactionally-maintained capability index
/// (OLB-2A). Every mutation updates the store and the index under one call, so
/// the index's live membership is always exactly the store's live set, and the
/// owner-plane capability query is served from the index with no descriptor
/// decode. The store's storage, cardinality, rollback, and currentness semantics
/// are unchanged — the index is a downstream mirror, never an authority.
#[derive(Default)]
pub struct ScopedDiscoveryState {
    store: ScopedDiscoveryStore,
    index: ScopedCapabilityIndex,
    /// Monotone private-discovery MUTATION/SWEEP generation over EITHER private
    /// partition (owner or grant). Advances once per ingest/sweep that changes a
    /// capability's live provider bucket. It is NOT yet a complete
    /// query-visible-set generation: a wall-clock expiry crossing or a
    /// revocation-floor raise can change query results before this advances —
    /// event-driven expiry and floor-raise dirtying arrive in OLB-2A.3. A consumer
    /// polls it to detect that a store mutation moved private discovery.
    revision: u64,
    /// The same MUTATION/SWEEP generation restricted to the OWNER partition, so
    /// valid grant-audience churn never advances it. Same wall-clock/floor caveat
    /// as `revision`.
    owner_revision: u64,
    /// Capabilities dirtied (owner or grant) since the last global drain.
    pending_global: DirtyCapabilities,
    /// Capabilities dirtied in the OWNER partition since the last owner drain.
    pending_owner: DirtyCapabilities,
    /// Earliest-first live expiries: each expiry deadline mapped to the number of
    /// LIVE records that expire at it. [`Self::next_visible_expiry`] reads the
    /// first key in O(log n), so the node's exact-expiry timer arms to exactly
    /// the next query-visible deadline instead of scanning the store or waiting a
    /// fixed 60 s. Tombstones are excluded — they are not query-visible, so they
    /// never gate a wake. Maintained in the SAME transaction as the store, index,
    /// and revisions (OLB-2A.3.2).
    live_expiries: BTreeMap<u64, u32>,
    /// Each LIVE key's current expiry, so an update that moves a record's expiry
    /// or a sweep/capacity-demotion that drops it can release the record's
    /// [`Self::live_expiries`] slot without a scan. Its key set is exactly the
    /// live set's (a tombstone or a never-live key is absent).
    expiry_by_key: BTreeMap<ScopedKey, u64>,
}

/// Add the capabilities a live record declared — read from the index BEFORE the
/// record is removed — to the affected sets, tagging the owner stream when the
/// record's scope is `Owner`.
fn note_removed_record(
    index: &ScopedCapabilityIndex,
    key: &ScopedKey,
    global: &mut BTreeSet<CapabilityAuthorityId>,
    owner: &mut BTreeSet<CapabilityAuthorityId>,
) {
    if let Some(caps) = index.declarations_by_record.get(key) {
        note_caps(&key.0, caps, global, owner);
    }
}

/// Add `caps` to the affected sets, tagging the owner stream when `scope` is
/// `Owner`.
fn note_caps(
    scope: &CapabilityAudienceScope,
    caps: &[CapabilityAuthorityId],
    global: &mut BTreeSet<CapabilityAuthorityId>,
    owner: &mut BTreeSet<CapabilityAuthorityId>,
) {
    let is_owner = matches!(scope, CapabilityAudienceScope::Owner { .. });
    for c in caps {
        global.insert(*c);
        if is_owner {
            owner.insert(*c);
        }
    }
}

impl ScopedDiscoveryState {
    /// A fresh, empty indexed store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Ingest a [`PreparedScopedCapability`] — a verified record with its declared
    /// capabilities decoded and bound to it — maintaining the index and both
    /// change streams in the SAME transaction. Consuming the prepared object
    /// (rather than an independently-supplied record and declaration set) makes a
    /// divergence between the stored row and its index buckets unrepresentable
    /// (Kyra OLB-2A.1). The internal capacity sweep's demotions leave the index
    /// too, even when the outcome is [`ScopedStoreOutcome::AtCapacity`].
    ///
    /// Refused FAIL-CLOSED as [`ScopedStoreOutcome::TooManyDeclarations`], with NO
    /// store, index, or change-stream mutation, if the record declares more than
    /// [`MAX_DECLARATIONS_PER_RECORD`] capabilities.
    pub fn ingest(
        &mut self,
        prepared: PreparedScopedCapability,
        now_secs: u64,
    ) -> ScopedStoreOutcome {
        let (capability, cap_ids) = prepared.into_parts();
        // Resource-bound hardening: refuse before touching any state so a
        // pathological owner descriptor cannot inflate the index.
        if cap_ids.len() > MAX_DECLARATIONS_PER_RECORD {
            return ScopedStoreOutcome::TooManyDeclarations;
        }
        let scope = capability.scope().clone();
        let key = (scope.clone(), capability.provider().clone());
        // Read the stored expiry before the store consumes the record; the store
        // keys its `StoredEntry` off this exact `expires_at()`, so expiry tracking
        // and the store agree on every deadline.
        let expires_at = capability.expires_at();
        let report = self.store.ingest(capability, now_secs);
        let mut global = BTreeSet::new();
        let mut owner = BTreeSet::new();
        // Demotions from the internal capacity sweep are disjoint from the
        // incoming key; drop them from the index (and dirty their caps) first.
        for swept in &report.swept_live {
            note_removed_record(&self.index, swept, &mut global, &mut owner);
            self.index.remove_record(swept);
            self.forget_live_expiry(swept);
        }
        match report.outcome {
            ScopedStoreOutcome::Inserted => {
                note_caps(&scope, &cap_ids, &mut global, &mut owner);
                self.index.insert_record(key.clone(), cap_ids);
                self.track_live_expiry(&key, expires_at);
            }
            ScopedStoreOutcome::Updated => {
                // Dirty the union of the old and new declarations — the query
                // trusts the index, so both the vacated and the new buckets moved.
                note_removed_record(&self.index, &key, &mut global, &mut owner);
                note_caps(&scope, &cap_ids, &mut global, &mut owner);
                self.index.replace_record(key.clone(), cap_ids);
                // Move the record onto its (possibly new) expiry; a revival from a
                // tombstone had no prior slot, an in-place update releases the old.
                self.track_live_expiry(&key, expires_at);
            }
            // `TooManyDeclarations` is returned early above and never produced by
            // the raw store, so it cannot appear here; listed for exhaustiveness.
            ScopedStoreOutcome::Stale
            | ScopedStoreOutcome::RejectedPublic
            | ScopedStoreOutcome::AtCapacity
            | ScopedStoreOutcome::TooManyDeclarations => {}
        }
        self.record_change(&global, &owner);
        report.outcome
    }

    /// Sweep expired records, dropping every demoted key from the index. Returns
    /// how many LIVE capabilities were dropped this call.
    pub fn sweep_expired(&mut self, now_secs: u64) -> usize {
        let removed = self.store.sweep_expired(now_secs);
        let dropped = removed.len();
        let mut global = BTreeSet::new();
        let mut owner = BTreeSet::new();
        for key in &removed {
            note_removed_record(&self.index, key, &mut global, &mut owner);
        }
        self.record_change(&global, &owner);
        for key in &removed {
            self.index.remove_record(key);
            self.forget_live_expiry(key);
        }
        dropped
    }

    /// Advance the change generations and dirty streams for a mutation that
    /// touched `global` (and, where owner-scoped, `owner`) capability buckets.
    /// Empty sets are a no-op — a mutation that changed the store's live set but
    /// no query-visible capability bucket (e.g. a record declaring nothing)
    /// advances neither generation. The owner stream is a subset of the global
    /// stream, so valid grant-audience churn never advances the owner generation.
    fn record_change(
        &mut self,
        global: &BTreeSet<CapabilityAuthorityId>,
        owner: &BTreeSet<CapabilityAuthorityId>,
    ) {
        if !global.is_empty() {
            self.revision = self.revision.wrapping_add(1);
            self.pending_global.mark(global);
        }
        if !owner.is_empty() {
            self.owner_revision = self.owner_revision.wrapping_add(1);
            self.pending_owner.mark(owner);
        }
    }

    /// Record that the LIVE record at `key` now expires at `expires_at`, moving it
    /// off any prior deadline it held. An in-place update releases its old slot
    /// (the `insert` returns the prior expiry); a fresh insert or a revival from a
    /// tombstone has no prior slot. Two `BTreeMap` touches, never a scan.
    fn track_live_expiry(&mut self, key: &ScopedKey, expires_at: u64) {
        if let Some(previous) = self.expiry_by_key.insert(key.clone(), expires_at) {
            self.release_expiry_slot(previous);
        }
        *self.live_expiries.entry(expires_at).or_insert(0) += 1;
    }

    /// Drop the LIVE record at `key` from expiry tracking — a sweep or a
    /// capacity-demotion moved it out of the live set. A key with no tracked
    /// expiry (a tombstone, or a record that never went live) is a no-op.
    fn forget_live_expiry(&mut self, key: &ScopedKey) {
        if let Some(previous) = self.expiry_by_key.remove(key) {
            self.release_expiry_slot(previous);
        }
    }

    /// Release one live reference to `deadline`, dropping the deadline entirely
    /// once its last live record leaves — so [`Self::next_visible_expiry`] never
    /// reports a deadline no live record still holds.
    fn release_expiry_slot(&mut self, deadline: u64) {
        if let Some(count) = self.live_expiries.get_mut(&deadline) {
            *count -= 1;
            if *count == 0 {
                self.live_expiries.remove(&deadline);
            }
        }
    }

    /// The earliest expiry among LIVE records, or `None` when the live set is
    /// empty. The node's exact-expiry timer arms to exactly this deadline; a
    /// mutation that introduces an earlier one advances a generation and so wakes
    /// the timer through the change watch, which re-reads this and re-arms. O(log
    /// n) — never a store scan.
    ///
    /// Bound to the mutation/sweep generations, NOT wall-clock: a record whose
    /// deadline has passed still appears here until a sweep removes it (reads stay
    /// expiry-safe via the store's read-time `now < expires_at` filter, so a
    /// not-yet-swept expiry is invisible to queries regardless). A record that
    /// declares no capability advances no generation, so it does not wake the
    /// timer; such an inert record is reclaimed by the 60 s GC retention backstop.
    pub fn next_visible_expiry(&self) -> Option<u64> {
        self.live_expiries.keys().next().copied()
    }

    /// The private-discovery mutation/sweep generation over EITHER partition — a
    /// read-only poll, safe to expose, useful for source recapture and
    /// publish-if-current checks. See [`Self::revision`] for the caveat that it
    /// does not yet reflect wall-clock expiry or floor raises.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The mutation/sweep generation restricted to the OWNER partition.
    pub fn owner_revision(&self) -> u64 {
        self.owner_revision
    }

    /// Atomically capture the GLOBAL change stream — the current generation AND
    /// the capabilities dirtied since the last drain — leaving the stream
    /// `Clean`. One locked operation, so a consumer can never checkpoint a
    /// generation and separately miss a delta that committed between two reads
    /// (Kyra OLB-2A.2). Crate-internal and DESTRUCTIVE: reserved for the single
    /// node-owned consumer of this stream (landing in OLB-2A.3), not a general
    /// public seam. Consumerless — and so `#[allow(dead_code)]` — until then.
    #[allow(dead_code)]
    pub(crate) fn take_global_change_batch(&mut self) -> PrivateDiscoveryChangeBatch {
        PrivateDiscoveryChangeBatch {
            generation: self.revision,
            dirty: self.pending_global.take(),
        }
    }

    /// Atomically capture the OWNER change stream (generation + dirty), leaving
    /// it `Clean`. Crate-internal and destructive; see
    /// [`Self::take_global_change_batch`]. Consumerless until OLB-2A.3.
    #[allow(dead_code)]
    pub(crate) fn take_owner_change_batch(&mut self) -> PrivateDiscoveryChangeBatch {
        PrivateDiscoveryChangeBatch {
            generation: self.owner_revision,
            dirty: self.pending_owner.take(),
        }
    }

    /// Owner-scoped private providers declaring `capability` (or every owner
    /// record when `None`), each paired with its provider entity, freshness-
    /// filtered.
    ///
    /// For a specific capability this is a SINGLE indexed bucket lookup —
    /// `owner_by_capability[cap]` — then a fresh expiry and revocation-floor
    /// currentness filter on each hit, with no descriptor decode and no visit to
    /// any unrelated (e.g. grant) scope. The `None` path (a test seam) enumerates
    /// every owner record via the store scan. Results are ordered
    /// deterministically (by scope, then provider).
    pub fn find_owner_private_providers(
        &self,
        capability: Option<&CapabilityAuthorityId>,
        now_secs: u64,
        floors: &OrgRevocationState,
    ) -> Vec<(PrivateCapabilityProvider, EntityId)> {
        let Some(cap) = capability else {
            return self
                .store
                .find_owner_private_capabilities(now_secs, floors, |_| true)
                .into_iter()
                .map(|c| {
                    (
                        PrivateCapabilityProvider::from_verified(c),
                        c.provider().clone(),
                    )
                })
                .collect();
        };
        let mut out = Vec::new();
        let Some(keys) = self.index.owner_by_capability.get(cap) else {
            return out;
        };
        for key in keys {
            let Some(rec) = self.store.live_record(key) else {
                continue;
            };
            if now_secs < rec.expires_at() && is_current(rec, floors) {
                out.push((
                    PrivateCapabilityProvider::from_verified(rec),
                    rec.provider().clone(),
                ));
            }
        }
        out
    }

    /// Grant-scoped providers under `grant_id`, filtered by `predicate`.
    /// Delegates to the store scan: the granted plane binds its capability at
    /// ingest, so it is not capability-indexed here.
    pub fn find_capabilities_for_grant<F>(
        &self,
        grant_id: &[u8; 32],
        now_secs: u64,
        floors: &OrgRevocationState,
        predicate: F,
    ) -> Vec<&VerifiedScopedCapability>
    where
        F: FnMut(&VerifiedScopedCapability) -> bool,
    {
        self.store
            .find_capabilities_for_grant(grant_id, now_secs, floors, predicate)
    }

    /// Number of LIVE stored scoped capabilities (tombstones excluded).
    pub fn len(&self) -> usize {
        self.store.len()
    }

    /// Whether the store holds no LIVE scoped capabilities.
    pub fn is_empty(&self) -> bool {
        self.store.is_empty()
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
                store.ingest(owner_cap_n(index, 1, 10_000), 1).outcome,
                ScopedStoreOutcome::Inserted
            );
        }
        assert_eq!(store.len(), cap);
        // A further DISTINCT provider is refused; nothing is evicted (every entry
        // is in-horizon at now=1, so the fail-closed sweep frees no slot).
        assert_eq!(
            store.ingest(owner_cap_n(u64::MAX, 1, 10_000), 1).outcome,
            ScopedStoreOutcome::AtCapacity
        );
        assert_eq!(store.len(), cap);
        // An UPDATE to an already-known key is never capacity-gated.
        assert_eq!(
            store.ingest(owner_cap_n(0, 2, 10_000), 1).outcome,
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
                store
                    .ingest(scoped_cap_in(hostile.clone(), index, 1, 10_000), 1)
                    .outcome,
                ScopedStoreOutcome::Inserted
            );
        }
        assert_eq!(
            store
                .ingest(scoped_cap_in(hostile.clone(), u64::MAX, 1, 10_000), 1)
                .outcome,
            ScopedStoreOutcome::AtCapacity,
            "the hostile scope must be capped at its own share",
        );

        // The owner partition is untouched and still admits.
        assert_eq!(
            store.ingest(owner_cap_n(0, 1, 10_000), 1).outcome,
            ScopedStoreOutcome::Inserted,
            "a flooded grant scope must not deny owner-scoped discovery",
        );
        // As does an unrelated grant.
        let other = CapabilityAudienceScope::Grant {
            grant_id: [0x0Cu8; 32],
            audience_handle: [0x0Du8; 32],
        };
        assert_eq!(
            store.ingest(scoped_cap_in(other, 0, 1, 10_000), 1).outcome,
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
            store.ingest(owner_cap_n(0, 2, 10_000), 1).outcome,
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
            store.ingest(owner_cap_n(u64::MAX, 1, 10_000), 1).outcome,
            ScopedStoreOutcome::AtCapacity
        );
        // Replay P at the OLDER generation 1: still Stale — the gen-2 high-water
        // was never evicted under capacity pressure.
        assert_eq!(
            store.ingest(owner_cap_n(0, 1, 10_000), 1).outcome,
            ScopedStoreOutcome::Stale
        );
    }

    #[test]
    fn ingest_reports_insert_update_and_stale() {
        let mut store = ScopedDiscoveryStore::new();
        assert_eq!(
            store.ingest(owner_cap(3, 1, 1000), 0).outcome,
            ScopedStoreOutcome::Inserted
        );
        // Newer generation for the same (scope, provider) updates.
        assert_eq!(
            store.ingest(owner_cap(3, 2, 1000), 0).outcome,
            ScopedStoreOutcome::Updated
        );
        // Older-or-equal generation is stale and ignored.
        assert_eq!(
            store.ingest(owner_cap(3, 2, 1000), 0).outcome,
            ScopedStoreOutcome::Stale
        );
        assert_eq!(
            store.ingest(owner_cap(3, 1, 1000), 0).outcome,
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
        assert_eq!(
            store.ingest(public, 0).outcome,
            ScopedStoreOutcome::RejectedPublic
        );
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
        assert_eq!(store.sweep_expired(2000).len(), 1);
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
            store.ingest(grant_cap(grant, 4, 2, 2000), 0).outcome, // gen 2, expires 2000
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
            store.ingest(grant_cap(grant, 4, 1, 5000), 0).outcome,
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

    // ----- OLB-2A: the indexed `ScopedDiscoveryState` -----

    use crate::adapter::net::behavior::capability::CapabilitySet;
    use crate::adapter::net::behavior::org_scoped_ingest::PreparedScopedCapability;

    /// The single owner scope the owner fixtures live in.
    fn owner_scope() -> CapabilityAudienceScope {
        CapabilityAudienceScope::Owner {
            org_id: org(1),
            audience_handle: [0x11; 32],
        }
    }

    /// The capability-authority id a service tag is indexed under.
    fn cap_id(tag: &str) -> CapabilityAuthorityId {
        CapabilityAuthorityId::for_tag(tag)
    }

    /// A real canonical descriptor declaring `tags`, decoded once at ingest
    /// exactly as the production path does.
    fn descriptor(tags: &[&str]) -> Vec<u8> {
        let mut caps = CapabilitySet::new();
        for t in tags {
            caps = caps.add_tag(*t);
        }
        caps.to_bytes_compact()
    }

    fn owner_cap_declaring(
        provider_seed: u8,
        generation: u64,
        expires_at: u64,
        tags: &[&str],
    ) -> VerifiedScopedCapability {
        VerifiedScopedCapability::for_test(
            owner_scope(),
            provider(provider_seed),
            org(1),
            generation,
            expires_at,
            FIXTURE_CERT_GEN,
            None,
            descriptor(tags),
        )
    }

    /// Like [`owner_cap_declaring`] but with a `u64`-indexed provider, for fills
    /// wider than the 256 the `u8` seed spans.
    fn owner_cap_declaring_n(
        provider_index: u64,
        generation: u64,
        expires_at: u64,
        tags: &[&str],
    ) -> VerifiedScopedCapability {
        VerifiedScopedCapability::for_test(
            owner_scope(),
            provider_n(provider_index),
            org(1),
            generation,
            expires_at,
            FIXTURE_CERT_GEN,
            None,
            descriptor(tags),
        )
    }

    /// Ingest through the indexed state as production does: prepare the verified
    /// record (decoding its declarations once and binding them) then ingest.
    fn ingest_indexed(
        state: &mut ScopedDiscoveryState,
        cap: VerifiedScopedCapability,
        now: u64,
    ) -> ScopedStoreOutcome {
        state.ingest(PreparedScopedCapability::prepare(cap), now)
    }

    /// The indexed owner query returns exactly the providers that declared the
    /// asked-for capability — a bucket lookup with no descriptor decode, across a
    /// multi-provider store.
    #[test]
    fn indexed_owner_query_matches_only_the_declared_capability() {
        let mut state = ScopedDiscoveryState::new();
        ingest_indexed(
            &mut state,
            owner_cap_declaring(3, 1, 10_000, &["nrpc:a"]),
            0,
        );
        ingest_indexed(
            &mut state,
            owner_cap_declaring(4, 1, 10_000, &["nrpc:b"]),
            0,
        );

        let a = state.find_owner_private_providers(Some(&cap_id("nrpc:a")), 0, &no_floors());
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].0.provider, provider(3));

        let b = state.find_owner_private_providers(Some(&cap_id("nrpc:b")), 0, &no_floors());
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].0.provider, provider(4));

        assert!(state
            .find_owner_private_providers(Some(&cap_id("nrpc:none")), 0, &no_floors())
            .is_empty());
    }

    /// A provider whose descriptor declares several tags is indexed under each.
    #[test]
    fn a_multi_tag_owner_record_is_indexed_under_every_capability() {
        let mut state = ScopedDiscoveryState::new();
        ingest_indexed(
            &mut state,
            owner_cap_declaring(3, 1, 10_000, &["nrpc:a", "nrpc:b"]),
            0,
        );
        assert_eq!(
            state
                .find_owner_private_providers(Some(&cap_id("nrpc:a")), 0, &no_floors())
                .len(),
            1
        );
        assert_eq!(
            state
                .find_owner_private_providers(Some(&cap_id("nrpc:b")), 0, &no_floors())
                .len(),
            1
        );
    }

    /// The load-bearing index-maintenance witness: because the query trusts the
    /// index and never re-decodes the descriptor, an `Updated` record that now
    /// declares a DIFFERENT capability must be re-indexed — the old capability
    /// must stop returning it, or the query would answer with a provider that no
    /// longer declares that capability.
    #[test]
    fn an_updated_descriptor_reindexes_the_record() {
        let mut state = ScopedDiscoveryState::new();
        ingest_indexed(
            &mut state,
            owner_cap_declaring(3, 1, 10_000, &["nrpc:a"]),
            0,
        );
        assert_eq!(
            state
                .find_owner_private_providers(Some(&cap_id("nrpc:a")), 0, &no_floors())
                .len(),
            1
        );

        // gen 2 declares nrpc:b instead of nrpc:a.
        assert_eq!(
            ingest_indexed(
                &mut state,
                owner_cap_declaring(3, 2, 10_000, &["nrpc:b"]),
                0
            ),
            ScopedStoreOutcome::Updated
        );
        assert!(
            state
                .find_owner_private_providers(Some(&cap_id("nrpc:a")), 0, &no_floors())
                .is_empty(),
            "the old capability is re-indexed away"
        );
        assert_eq!(
            state
                .find_owner_private_providers(Some(&cap_id("nrpc:b")), 0, &no_floors())
                .len(),
            1,
            "the new capability is indexed"
        );
    }

    /// The indexed query applies fresh expiry per bucket hit — an expired record
    /// is excluded even before any sweep touches the index.
    #[test]
    fn the_indexed_owner_query_excludes_an_expired_record_before_sweep() {
        let mut state = ScopedDiscoveryState::new();
        ingest_indexed(&mut state, owner_cap_declaring(3, 1, 1000, &["nrpc:a"]), 0);
        let a = cap_id("nrpc:a");
        assert_eq!(
            state
                .find_owner_private_providers(Some(&a), 500, &no_floors())
                .len(),
            1,
            "visible before expiry"
        );
        assert!(
            state
                .find_owner_private_providers(Some(&a), 2000, &no_floors())
                .is_empty(),
            "excluded past expiry with no sweep"
        );
    }

    /// The indexed query applies fresh revocation-floor currentness per bucket
    /// hit — a floor raised above the admitted generation retracts an indexed
    /// record at query time, no sweep needed.
    #[test]
    fn the_indexed_owner_query_applies_floor_currentness() {
        let org_kp = OrgKeypair::from_bytes([7u8; 32]);
        let org_id = org_kp.org_id();
        let member = EntityId::from_bytes([9u8; 32]);
        let mut state = ScopedDiscoveryState::new();
        ingest_indexed(
            &mut state,
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
                descriptor(&["nrpc:a"]),
            ),
            0,
        );
        let a = cap_id("nrpc:a");
        assert_eq!(
            state
                .find_owner_private_providers(Some(&a), 0, &no_floors())
                .len(),
            1
        );
        let floor_above = floor_state(&org_kp, &member, FIXTURE_CERT_GEN + 1);
        assert!(
            state
                .find_owner_private_providers(Some(&a), 0, &floor_above)
                .is_empty(),
            "a raised floor retracts the indexed record at query time"
        );
    }

    /// `sweep_expired` reports every `(scope, provider)` key whose live capability
    /// it dropped, so the indexed layer updates in the same transaction.
    #[test]
    fn sweep_expired_reports_the_demoted_live_keys() {
        let mut store = ScopedDiscoveryStore::new();
        store.ingest(owner_cap(3, 1, 1000), 0); // expires 1000
        store.ingest(grant_cap([0xAA; 32], 4, 1, 5000), 0); // expires 5000
                                                            // At t=2000 only the owner entry has expired.
        let removed = store.sweep_expired(2000);
        assert_eq!(removed, vec![(owner_scope(), provider(3))]);
    }

    /// The ingest's INTERNAL capacity sweep surfaces the live records it demoted,
    /// so they leave the index even though this ingest's own outcome is about the
    /// NEW key — the wrapper-only hole the plan flags.
    #[test]
    fn the_internal_capacity_sweep_reports_its_demotions() {
        let mut store = ScopedDiscoveryStore::new();
        // Fill the owner scope's share with entries that all expire at 1000.
        for index in 0..ScopedDiscoveryStore::MAX_ENTRIES_PER_SCOPE as u64 {
            store.ingest(owner_cap_n(index, 1, 1000), 0);
        }
        // At t=2000 a new provider trips the per-scope guard, whose internal sweep
        // demotes every expired entry; the report lists them and the freed slots
        // admit the new key.
        let report = store.ingest(owner_cap_n(u64::MAX, 1, 5000), 2000);
        assert_eq!(report.outcome, ScopedStoreOutcome::Inserted);
        assert_eq!(
            report.swept_live.len(),
            ScopedDiscoveryStore::MAX_ENTRIES_PER_SCOPE
        );
    }

    // ----- OLB-2A.2: change generations + affected-capability deltas -----

    fn grant_cap_declaring(
        grant_id: [u8; 32],
        provider_seed: u8,
        generation: u64,
        expires_at: u64,
        tag: &str,
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
            descriptor(&[tag]),
        )
    }

    fn one_cap(tag: &str) -> DirtyCapabilities {
        DirtyCapabilities::Caps([cap_id(tag)].into_iter().collect())
    }

    /// An owner ingest that changes a capability's provider set advances BOTH
    /// generations and names the capability in both delta streams; draining
    /// leaves each stream clean.
    #[test]
    fn an_owner_ingest_advances_both_generations_and_dirties_its_capability() {
        let mut state = ScopedDiscoveryState::new();
        assert_eq!(state.revision(), 0);
        assert_eq!(state.owner_revision(), 0);

        ingest_indexed(
            &mut state,
            owner_cap_declaring(3, 1, 10_000, &["nrpc:a"]),
            0,
        );
        assert_eq!(state.revision(), 1);
        assert_eq!(state.owner_revision(), 1);
        assert_eq!(state.take_global_change_batch().dirty, one_cap("nrpc:a"));
        assert_eq!(state.take_owner_change_batch().dirty, one_cap("nrpc:a"));

        // Drained.
        assert_eq!(
            state.take_global_change_batch().dirty,
            DirtyCapabilities::Clean
        );
        assert_eq!(
            state.take_owner_change_batch().dirty,
            DirtyCapabilities::Clean
        );
    }

    /// Grant-audience churn advances the GLOBAL stream but NEVER the owner
    /// stream — an owner-private consumer is not woken by cross-org grant
    /// movement.
    #[test]
    fn grant_churn_never_advances_the_owner_stream() {
        let mut state = ScopedDiscoveryState::new();
        ingest_indexed(
            &mut state,
            grant_cap_declaring([0xAA; 32], 4, 1, 10_000, "nrpc:g"),
            0,
        );
        assert_eq!(state.revision(), 1, "global advances");
        assert_eq!(state.owner_revision(), 0, "owner does not");
        assert_eq!(state.take_global_change_batch().dirty, one_cap("nrpc:g"));
        assert_eq!(
            state.take_owner_change_batch().dirty,
            DirtyCapabilities::Clean
        );
    }

    /// A Stale re-ingest advances no generation and dirties nothing.
    #[test]
    fn a_stale_ingest_advances_no_generation() {
        let mut state = ScopedDiscoveryState::new();
        ingest_indexed(
            &mut state,
            owner_cap_declaring(3, 2, 10_000, &["nrpc:a"]),
            0,
        );
        let (rev, owner_rev) = (state.revision(), state.owner_revision());
        let _ = state.take_global_change_batch().dirty;
        let _ = state.take_owner_change_batch().dirty;

        assert_eq!(
            ingest_indexed(
                &mut state,
                owner_cap_declaring(3, 1, 10_000, &["nrpc:a"]),
                0
            ),
            ScopedStoreOutcome::Stale
        );
        assert_eq!(state.revision(), rev, "stale advances nothing");
        assert_eq!(state.owner_revision(), owner_rev);
        assert_eq!(
            state.take_global_change_batch().dirty,
            DirtyCapabilities::Clean
        );
    }

    /// An update dirties BOTH the vacated and the newly declared capability: the
    /// query trusts the index, so both buckets moved.
    #[test]
    fn an_update_dirties_the_old_and_new_capabilities() {
        let mut state = ScopedDiscoveryState::new();
        ingest_indexed(
            &mut state,
            owner_cap_declaring(3, 1, 10_000, &["nrpc:a"]),
            0,
        );
        let _ = state.take_global_change_batch().dirty;
        let _ = state.take_owner_change_batch().dirty;

        assert_eq!(
            ingest_indexed(
                &mut state,
                owner_cap_declaring(3, 2, 10_000, &["nrpc:b"]),
                0
            ),
            ScopedStoreOutcome::Updated
        );
        let expected: std::collections::BTreeSet<_> =
            [cap_id("nrpc:a"), cap_id("nrpc:b")].into_iter().collect();
        assert_eq!(
            state.take_global_change_batch().dirty,
            DirtyCapabilities::Caps(expected)
        );
    }

    /// Expiring a record via sweep dirties its capability and advances the stream.
    #[test]
    fn a_sweep_dirties_the_expired_capability() {
        let mut state = ScopedDiscoveryState::new();
        ingest_indexed(&mut state, owner_cap_declaring(3, 1, 1000, &["nrpc:a"]), 0);
        let rev = state.revision();
        let _ = state.take_global_change_batch().dirty;
        let _ = state.take_owner_change_batch().dirty;

        assert_eq!(state.sweep_expired(2000), 1);
        assert_eq!(state.revision(), rev + 1);
        assert_eq!(state.take_global_change_batch().dirty, one_cap("nrpc:a"));
    }

    /// Past the bound the dirty stream collapses to `RebuildAll` rather than
    /// growing an unbounded set. Each record declares ONE distinct capability
    /// (within the per-record budget), so the collapse is driven by the number of
    /// distinct dirtied capabilities across records, not one wide descriptor.
    #[test]
    fn the_delta_collapses_to_rebuild_all_past_the_bound() {
        let mut state = ScopedDiscoveryState::new();
        for i in 0..=MAX_DIRTY_CAPABILITIES as u64 {
            let tag = format!("nrpc:svc{i}");
            ingest_indexed(&mut state, owner_cap_declaring_n(i, 1, 10_000, &[&tag]), 0);
        }
        assert_eq!(
            state.take_global_change_batch().dirty,
            DirtyCapabilities::RebuildAll
        );
    }

    /// A record declaring no capability (a non-decoding descriptor) changes the
    /// store but no query-visible bucket, so it advances no generation.
    #[test]
    fn a_record_declaring_no_capability_advances_nothing() {
        let mut state = ScopedDiscoveryState::new();
        ingest_indexed(
            &mut state,
            VerifiedScopedCapability::for_test(
                owner_scope(),
                provider(3),
                org(1),
                1,
                10_000,
                FIXTURE_CERT_GEN,
                None,
                b"not-a-capability-set".to_vec(),
            ),
            0,
        );
        assert_eq!(state.revision(), 0);
        assert_eq!(
            state.take_global_change_batch().dirty,
            DirtyCapabilities::Clean
        );
    }

    // ----- OLB-2A closure (Kyra bounded review) -----

    /// The change batch captures the generation AND the dirty delta as ONE locked
    /// operation, so a consumer can never checkpoint a generation and separately
    /// miss a delta that committed between two reads (Kyra OLB-2A.2). A second
    /// drain is `Clean` at the same, monotone generation.
    #[test]
    fn the_change_batch_captures_generation_and_delta_atomically() {
        let mut state = ScopedDiscoveryState::new();
        ingest_indexed(
            &mut state,
            owner_cap_declaring(3, 1, 10_000, &["nrpc:a"]),
            0,
        );

        let batch = state.take_global_change_batch();
        assert_eq!(batch.generation, state.revision());
        assert_eq!(batch.generation, 1);
        assert_eq!(batch.dirty, one_cap("nrpc:a"));

        let drained = state.take_global_change_batch();
        assert_eq!(drained.generation, 1, "generation is not reset by a drain");
        assert_eq!(drained.dirty, DirtyCapabilities::Clean);

        // The owner stream still holds its (undrained) delta at its own generation.
        let owner = state.take_owner_change_batch();
        assert_eq!(owner.generation, state.owner_revision());
        assert_eq!(owner.dirty, one_cap("nrpc:a"));
    }

    /// A record declaring more capabilities than the per-record association budget
    /// is refused FAIL-CLOSED: no store row, no index answer, and no change-stream
    /// movement (Kyra OLB-2A.1 resource hardening).
    #[test]
    fn a_record_declaring_too_many_capabilities_is_refused_fail_closed() {
        let mut state = ScopedDiscoveryState::new();
        let tags: Vec<String> = (0..=MAX_DECLARATIONS_PER_RECORD)
            .map(|i| format!("nrpc:svc{i}"))
            .collect();
        let tag_refs: Vec<&str> = tags.iter().map(String::as_str).collect();

        let outcome = ingest_indexed(&mut state, owner_cap_declaring(3, 1, 10_000, &tag_refs), 0);
        assert_eq!(outcome, ScopedStoreOutcome::TooManyDeclarations);

        assert_eq!(state.len(), 0, "no row stored");
        assert!(
            state
                .find_owner_private_providers(Some(&cap_id("nrpc:svc0")), 0, &no_floors())
                .is_empty(),
            "no index association"
        );
        assert_eq!(state.revision(), 0, "no generation advance");
        assert_eq!(
            state.take_global_change_batch().dirty,
            DirtyCapabilities::Clean,
            "no dirty"
        );
    }

    /// The internal capacity sweep's demotions flow through the WRAPPER even when
    /// the incoming key is refused `AtCapacity` (Kyra OLB-2A.2): a live row demoted
    /// to a retained tombstone leaves the owner index, advances both generations,
    /// and lands in both dirty streams.
    #[test]
    fn an_internal_capacity_demotion_is_visible_through_the_state_on_at_capacity() {
        let mut state = ScopedDiscoveryState::new();
        // Fill the owner scope's share with records carrying a LONG tombstone
        // watermark (gen 1, expiry 10_000) but a SHORT current expiry (gen 2,
        // expiry 1000); at t=2000 a sweep demotes them to RETAINED tombstones.
        for index in 0..ScopedDiscoveryStore::MAX_ENTRIES_PER_SCOPE as u64 {
            ingest_indexed(
                &mut state,
                owner_cap_declaring_n(index, 1, 10_000, &["nrpc:a"]),
                0,
            );
            ingest_indexed(
                &mut state,
                owner_cap_declaring_n(index, 2, 1000, &["nrpc:a"]),
                0,
            );
        }
        let _ = state.take_global_change_batch();
        let _ = state.take_owner_change_batch();
        let (rev, owner_rev) = (state.revision(), state.owner_revision());
        assert_eq!(
            state
                .find_owner_private_providers(Some(&cap_id("nrpc:a")), 500, &no_floors())
                .len(),
            ScopedDiscoveryStore::MAX_ENTRIES_PER_SCOPE,
            "all live before the sweep"
        );

        // A new provider at t=2000 trips the per-scope guard; its internal sweep
        // demotes every filler to a retained tombstone (watermark 10_000 > 2000),
        // so the scope stays full and the new key is refused.
        let outcome = ingest_indexed(
            &mut state,
            owner_cap_declaring_n(u64::MAX, 1, 20_000, &["nrpc:b"]),
            2000,
        );
        assert_eq!(outcome, ScopedStoreOutcome::AtCapacity);

        assert!(
            state
                .find_owner_private_providers(Some(&cap_id("nrpc:a")), 2000, &no_floors())
                .is_empty(),
            "the demoted records left the owner index"
        );
        assert_eq!(state.revision(), rev + 1, "global generation advanced");
        assert_eq!(
            state.owner_revision(),
            owner_rev + 1,
            "owner generation advanced"
        );
        assert_eq!(state.take_global_change_batch().dirty, one_cap("nrpc:a"));
        assert_eq!(state.take_owner_change_batch().dirty, one_cap("nrpc:a"));
    }

    // ----- OLB-2A.3.2: next_visible_expiry min-tracking -----

    /// An empty live set has no next expiry; an insert exposes exactly the
    /// earliest live deadline, and a later insert never hides it.
    #[test]
    fn next_visible_expiry_tracks_the_earliest_live_deadline() {
        let mut state = ScopedDiscoveryState::new();
        assert_eq!(
            state.next_visible_expiry(),
            None,
            "empty live set has no deadline"
        );

        ingest_indexed(&mut state, owner_cap_declaring(3, 1, 5000, &["nrpc:a"]), 0);
        assert_eq!(state.next_visible_expiry(), Some(5000));

        // A LATER deadline does not move the minimum.
        ingest_indexed(&mut state, owner_cap_declaring(4, 1, 9000, &["nrpc:a"]), 0);
        assert_eq!(state.next_visible_expiry(), Some(5000));

        // An EARLIER deadline does — this is the edge the timer must re-arm to.
        ingest_indexed(&mut state, owner_cap_declaring(5, 1, 1000, &["nrpc:a"]), 0);
        assert_eq!(state.next_visible_expiry(), Some(1000));
    }

    /// An update MOVES a record's expiry contribution: a record that pushes its
    /// deadline later releases its old, earlier slot, so the minimum rises to the
    /// next live deadline instead of pinning to a deadline no live record holds.
    #[test]
    fn an_update_moves_the_records_expiry_slot() {
        let mut state = ScopedDiscoveryState::new();
        ingest_indexed(&mut state, owner_cap_declaring(3, 1, 1000, &["nrpc:a"]), 0);
        assert_eq!(state.next_visible_expiry(), Some(1000));

        // Same provider, newer generation, later expiry: an Updated record.
        assert_eq!(
            ingest_indexed(&mut state, owner_cap_declaring(3, 2, 5000, &["nrpc:a"]), 0),
            ScopedStoreOutcome::Updated
        );
        assert_eq!(
            state.next_visible_expiry(),
            Some(5000),
            "the update released the vacated 1000 slot"
        );
    }

    /// Records sharing a deadline are reference-counted: the shared deadline
    /// survives until its LAST live holder leaves, so moving one of two records
    /// off it does not prematurely expose a later deadline.
    #[test]
    fn records_sharing_a_deadline_are_reference_counted() {
        let mut state = ScopedDiscoveryState::new();
        ingest_indexed(&mut state, owner_cap_declaring(3, 1, 1000, &["nrpc:a"]), 0);
        ingest_indexed(&mut state, owner_cap_declaring(4, 1, 1000, &["nrpc:a"]), 0);
        assert_eq!(state.next_visible_expiry(), Some(1000));

        // Move provider 3 off 1000; provider 4 still holds it.
        ingest_indexed(&mut state, owner_cap_declaring(3, 2, 5000, &["nrpc:a"]), 0);
        assert_eq!(
            state.next_visible_expiry(),
            Some(1000),
            "provider 4 still holds the shared deadline"
        );

        // Move provider 4 off 1000 too; only 5000 remains.
        ingest_indexed(&mut state, owner_cap_declaring(4, 2, 5000, &["nrpc:a"]), 0);
        assert_eq!(state.next_visible_expiry(), Some(5000));
    }

    /// A sweep advances the next expiry to the surviving record: the swept
    /// deadline is released with the live record it belonged to, and a fully
    /// emptied live set reports no deadline.
    #[test]
    fn a_sweep_advances_next_visible_expiry_to_the_survivor() {
        let mut state = ScopedDiscoveryState::new();
        ingest_indexed(&mut state, owner_cap_declaring(3, 1, 1000, &["nrpc:a"]), 0);
        ingest_indexed(&mut state, owner_cap_declaring(4, 1, 5000, &["nrpc:a"]), 0);
        assert_eq!(state.next_visible_expiry(), Some(1000));

        // Sweep at 2000: provider 3 (expiry 1000) is demoted; provider 4 survives.
        assert_eq!(state.sweep_expired(2000), 1);
        assert_eq!(
            state.next_visible_expiry(),
            Some(5000),
            "the swept 1000 slot was released with its live record"
        );

        // Sweep past the survivor: no live record, no deadline.
        assert_eq!(state.sweep_expired(6000), 1);
        assert_eq!(state.next_visible_expiry(), None);
    }

    /// Expiry tracking is partition-agnostic: a GRANT record's deadline gates the
    /// next expiry exactly as an owner record's does, so the one node timer sweeps
    /// grant expiries too (the granted plane is not owner-indexed, but it still
    /// expires).
    #[test]
    fn a_grant_records_deadline_also_gates_the_next_expiry() {
        let mut state = ScopedDiscoveryState::new();
        ingest_indexed(&mut state, owner_cap_declaring(3, 1, 2000, &["nrpc:a"]), 0);
        ingest_indexed(
            &mut state,
            grant_cap_declaring([0xAA; 32], 4, 1, 800, "nrpc:g"),
            0,
        );
        assert_eq!(
            state.next_visible_expiry(),
            Some(800),
            "the earlier grant deadline gates the timer"
        );
    }
}
