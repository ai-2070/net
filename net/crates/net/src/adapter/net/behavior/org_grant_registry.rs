//! OA-3 §3.3 (OA3-4b2) — the runtime GRANT-AUDIENCE registries: the local,
//! operator-installed `(OrgCapabilityGrant, OrgAudienceSecret)` pairs a node
//! holds for grant-scoped private discovery.
//!
//! Two role-separated registries, each an immutable snapshot behind its own
//! `ArcSwap` on [`MeshNode`](crate::adapter::net::MeshNode):
//!
//! - **Provider** ([`ProviderGrantSnapshot`]) — grants this node's OWN org issued
//!   that apply to THIS provider. The bounded granted-emission projection
//!   (OA3-4b2 slice 3) fans one `build_granted` envelope out per active record.
//! - **Consumer** ([`ConsumerGrantSnapshot`]) — grants whose `grantee_org` is this
//!   node's own org. The live nonzero-grant ingest selector (OA3-4b2 slice 4)
//!   looks a record up by `(grant_id, audience_handle)` to build the
//!   [`AudienceAuthority::granted`](super::org_scoped_ingest::AudienceAuthority)
//!   an inbound granted envelope verifies against.
//!
//! # Why NOT folded into `NodeAuthority` (Kyra OA3-4b2)
//!
//! [`NodeAuthority`](super::org_authority::NodeAuthority) is the STABLE
//! owner-identity scaffold — membership cert, owner org, owner audience
//! credential, verification config. Grant audiences are dynamic, independently
//! installed, and numerous; folding them in would rotate the authority pointer
//! for routine grant churn (invalidating admission and the owner-scoped
//! emission's cached ciphertext), mix owner authority with delegated cross-org
//! credentials, and hurt secret-lifecycle isolation. Each registry is its own
//! `ArcSwap`, so one registry's mutation never invalidates the other — or the
//! authority.
//!
//! # Snapshot immutability + secret lifecycle
//!
//! A snapshot is a `BTreeMap` of `Arc<GrantAudienceRecord>` keyed by `grant_id`;
//! a mutation clones the map (Arc bumps only — never secret bytes) and swaps the
//! new snapshot in. A removed record's `Arc` is dropped from the new snapshot but
//! stays alive while any in-flight snapshot or emission still holds it; when the
//! last holder releases it, [`OrgAudienceSecret`]'s `Drop` zeroizes the discovery
//! key (witnessed in `org_grant`'s review-7 gate). No filesystem loading, no org
//! root, and NO dynamic issuance live here — the SDK/operator layer loads the
//! canonical OA2-F artifacts and installs them through the
//! [`MeshNode`](crate::adapter::net::MeshNode) APIs.

use std::collections::BTreeMap;
use std::sync::Arc;

use super::org::OrgId;
use super::org_grant::{OrgAudienceSecret, OrgCapabilityGrant};
use crate::adapter::net::identity::{EntityId, MAX_TOKEN_CLOCK_SKEW_SECS};

/// Hard cap on active PROVIDER grant-audience records (OA3-4b2, Kyra-pinned).
/// This is exactly the maximum number of granted envelopes a single emission may
/// fan out, so the emission layer can service every accepted record — the 257th
/// active record is refused ([`GrantAudienceInstallError::AtCapacity`]) rather
/// than accepted and later silently truncated. No active record is ever evicted
/// to admit another.
pub const MAX_PROVIDER_GRANT_AUDIENCES: usize = 256;

/// Hard cap on active CONSUMER grant-audience records. Mirrors the provider bound
/// (both are operator-install-only surfaces): a node holds a bounded set of
/// grants it was issued as grantee, and a new record past the cap is refused
/// fail-closed rather than evicting one already in use.
pub const MAX_CONSUMER_GRANT_AUDIENCES: usize = 256;

/// One installed grant paired with its out-of-band audience secret. Both are
/// owned; the record is only ever handled behind an `Arc`, so a snapshot copy is
/// an Arc bump and the secret bytes never move. The secret is structurally
/// non-serializable and zeroized on drop ([`OrgAudienceSecret`]).
pub struct GrantAudienceRecord {
    grant: OrgCapabilityGrant,
    secret: OrgAudienceSecret,
    /// Monotonic per-node install sequence, stamped by the installing node
    /// (never by the issuer, never on the wire). Identifies THIS installation
    /// of THIS grant id, so a lease-holder can remove only the record it
    /// installed — see [`ConsumerAudienceLease`]. Deliberately NOT part of
    /// [`records_identical`]: an idempotent re-install must stay a no-op, so
    /// the surviving record keeps its original sequence.
    install_seq: u64,
}

impl GrantAudienceRecord {
    /// The signed grant.
    pub fn grant(&self) -> &OrgCapabilityGrant {
        &self.grant
    }
    /// The install sequence stamped when this record entered the registry.
    pub fn install_seq(&self) -> u64 {
        self.install_seq
    }
    /// Stamp the install sequence (installing node only, before the record is
    /// wrapped in its `Arc` and published).
    pub(crate) fn with_install_seq(mut self, install_seq: u64) -> Self {
        self.install_seq = install_seq;
        self
    }
    /// The out-of-band audience secret (borrowing accessor — the raw key is never
    /// copied out).
    pub fn secret(&self) -> &OrgAudienceSecret {
        &self.secret
    }
    /// The grant id this record is keyed by.
    pub fn grant_id(&self) -> &[u8; 32] {
        &self.grant.grant_id
    }
    /// The audience routing handle (from the secret; equal to the grant's signed
    /// binding handle, checked at install via `matches_grant`).
    pub fn audience_handle(&self) -> &[u8; 32] {
        &self.secret.audience_handle
    }
}

impl std::fmt::Debug for GrantAudienceRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `grant` and `secret` both redact/short-hex their sensitive fields.
        f.debug_struct("GrantAudienceRecord")
            .field("grant", &self.grant)
            .field("secret", &self.secret)
            .finish()
    }
}

/// Why installing a grant-audience record was refused. Distinguishable,
/// fail-closed reasons (manual `Display` + `Error`, org-family house style).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantAudienceInstallError {
    /// No node authority is installed, so the owner-org invariants cannot be
    /// checked — a node must be adopted before it holds grant audiences.
    NoAuthority,
    /// The grant's signature or structural validity failed (`grant.verify`) — a
    /// zero (reserved) grant id also surfaces here.
    GrantInvalid,
    /// The grant is expired or not yet valid at the install clock.
    GrantNotCurrent,
    /// The grant does not carry DISCOVER rights (INVOKE-only grants hold no
    /// audience and can never seal/open a scoped announcement).
    MissingDiscover,
    /// The grant carries no discovery binding (defense in depth — the structural
    /// rule ties this to DISCOVER, re-checked here explicitly).
    NoDiscoveryBinding,
    /// The out-of-band secret is not this grant's key (grant id or key commitment
    /// mismatch, or a handle that does not match the signed binding).
    SecretMismatch,
    /// Provider install: the grant's issuer org is not this node's owner org.
    WrongProviderIssuer,
    /// Provider install: the grant's target scope does not cover THIS provider.
    ProviderNotCovered,
    /// Consumer install: the grant's grantee org is not this node's owner org.
    WrongConsumerGrantee,
    /// A DIFFERENT grant/secret is already installed under this grant id.
    /// Replacement is an explicit remove-then-install, never a silent overwrite.
    Conflict,
    /// The registry is at capacity and no expired record could be reclaimed —
    /// refused fail-closed rather than evicting an active record.
    AtCapacity,
}

impl std::fmt::Display for GrantAudienceInstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            GrantAudienceInstallError::NoAuthority => "no node authority installed",
            GrantAudienceInstallError::GrantInvalid => "grant signature or structure invalid",
            GrantAudienceInstallError::GrantNotCurrent => "grant expired or not yet valid",
            GrantAudienceInstallError::MissingDiscover => "grant lacks DISCOVER rights",
            GrantAudienceInstallError::NoDiscoveryBinding => "grant carries no discovery binding",
            GrantAudienceInstallError::SecretMismatch => "audience secret does not match the grant",
            GrantAudienceInstallError::WrongProviderIssuer => {
                "grant issuer org is not this provider's owner org"
            }
            GrantAudienceInstallError::ProviderNotCovered => {
                "grant target scope does not cover this provider"
            }
            GrantAudienceInstallError::WrongConsumerGrantee => {
                "grant grantee org is not this consumer's owner org"
            }
            GrantAudienceInstallError::Conflict => {
                "a different grant is already installed under this grant id"
            }
            GrantAudienceInstallError::AtCapacity => "grant-audience registry at capacity",
        };
        f.write_str(s)
    }
}

impl std::error::Error for GrantAudienceInstallError {}

/// Ownership proof for one CONSUMER grant-audience installation (OSDK S0, Kyra
/// v0.3 ruling §2 — "the lease must own a specific registry installation, not
/// merely a grant ID").
///
/// A caller that installs a consumer audience and later wants to withdraw it
/// cannot safely remove by `grant_id` alone: between install and removal, other
/// code (the low-level operator API, another SDK client) may have removed that
/// record and installed a DIFFERENT grant under the same id. Removing by id
/// would then destroy an installation the holder never owned. The lease pins the
/// exact installation via the node-local [`GrantAudienceRecord::install_seq`], and
/// [`remove_consumer_grant_audience_if_current`] compares under the registry
/// mutex, so replacement cannot race between the check and the removal.
///
/// Carries no secret bytes: a grant id (public, in the signed grant) and a
/// node-local counter.
///
/// [`remove_consumer_grant_audience_if_current`]:
///     crate::adapter::net::MeshNode::remove_consumer_grant_audience_if_current
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConsumerAudienceLease {
    grant_id: [u8; 32],
    install_seq: u64,
}

impl ConsumerAudienceLease {
    pub(crate) fn new(grant_id: [u8; 32], install_seq: u64) -> Self {
        Self {
            grant_id,
            install_seq,
        }
    }
    /// The grant id this lease covers.
    pub fn grant_id(&self) -> &[u8; 32] {
        &self.grant_id
    }
    /// The exact installation this lease owns.
    pub fn install_seq(&self) -> u64 {
        self.install_seq
    }
}

/// The result of a CONSUMER grant-audience install through the leased API
/// (OSDK S0). `Installed` hands back the ownership proof; `AlreadyPresent`
/// deliberately does NOT — an identical record was already installed by someone
/// else, so this caller owns nothing and must never remove it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsumerAudienceInstall {
    /// A new record was installed; the lease owns it.
    Installed(ConsumerAudienceLease),
    /// An identical record was already present — idempotent no-op, non-owning.
    AlreadyPresent,
}

/// The result of a successful install (Kyra OA3-4b2 idempotency contract).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantAudienceInstalled {
    /// A new record was installed — the snapshot pointer rotated.
    Installed,
    /// An IDENTICAL record was already present — an idempotent no-op that does
    /// NOT rotate the snapshot pointer (routine re-install must not churn the
    /// emission's cached ciphertext).
    AlreadyPresent,
}

/// The shared, role-agnostic record set both snapshots wrap: verified records
/// keyed by `grant_id`. All mutation is copy-on-write — a new set is built and
/// the caller swaps it into the registry's `ArcSwap`.
#[derive(Default, Clone, Debug)]
struct GrantAudienceRecords {
    by_grant_id: BTreeMap<[u8; 32], Arc<GrantAudienceRecord>>,
}

impl GrantAudienceRecords {
    fn get(&self, grant_id: &[u8; 32]) -> Option<&Arc<GrantAudienceRecord>> {
        self.by_grant_id.get(grant_id)
    }

    fn len(&self) -> usize {
        self.by_grant_id.len()
    }

    fn records(&self) -> impl Iterator<Item = &Arc<GrantAudienceRecord>> {
        self.by_grant_id.values()
    }

    /// Copy-on-write install. `Ok(None)` = the identical record was already
    /// present (idempotent); `Ok(Some(next))` = a new set to publish. A new key
    /// past `capacity` first reclaims records whose grant has since expired
    /// (`not_after <= now_secs`); if the set is still full it is refused
    /// [`GrantAudienceInstallError::AtCapacity`]. Node-context invariants
    /// (validity, rights, org/target) are checked BEFORE this by the caller —
    /// this layer owns only idempotency, conflict, and capacity.
    fn install(
        &self,
        record: Arc<GrantAudienceRecord>,
        capacity: usize,
        now_secs: u64,
    ) -> Result<Option<Self>, GrantAudienceInstallError> {
        let grant_id = *record.grant_id();
        if let Some(existing) = self.by_grant_id.get(&grant_id) {
            return if records_identical(existing, &record) {
                Ok(None)
            } else {
                Err(GrantAudienceInstallError::Conflict)
            };
        }
        let mut next = self.by_grant_id.clone();
        if next.len() >= capacity {
            // Reclaim only FULLY-EXPIRED records (installed valid, since expired)
            // — a not-yet-valid record can never have been installed. Never evict
            // an active record to admit a new one.
            // §25 — reclaim only records that are expired WITH SKEW, matching
            // every other validity decision in the grant family
            // (`is_valid_at_with_skew`). A bare `not_after > now` sweeps a
            // record that is still valid within tolerance, and `now_secs` is
            // wall-clock (`current_timestamp`), so a single forward NTP jump
            // coinciding with one install at capacity would delete ALL of the
            // installed records at once — granted-envelope fanout then stops
            // silently, with no error surfaced and no way to notice short of
            // re-installing.
            let horizon = now_secs.saturating_sub(MAX_TOKEN_CLOCK_SKEW_SECS);
            let before = next.len();
            next.retain(|_, r| r.grant().not_after > horizon);
            let swept = before - next.len();
            if swept > 8 {
                tracing::warn!(
                    swept,
                    remaining = next.len(),
                    "org grant registry: capacity sweep reclaimed an unusually \
                     large number of installed records at once; if this was not \
                     a mass expiry, check for a wall-clock jump",
                );
            }
            if next.len() >= capacity {
                return Err(GrantAudienceInstallError::AtCapacity);
            }
        }
        next.insert(grant_id, record);
        Ok(Some(Self { by_grant_id: next }))
    }

    /// Copy-on-write remove. `None` = the grant id was not present (no-op — the
    /// caller must not rotate the pointer); `Some(next)` = the record was
    /// removed and the surviving set should be published.
    fn without(&self, grant_id: &[u8; 32]) -> Option<Self> {
        if !self.by_grant_id.contains_key(grant_id) {
            return None;
        }
        let mut next = self.by_grant_id.clone();
        next.remove(grant_id);
        Some(Self { by_grant_id: next })
    }
}

/// Two installs are idempotent iff the grant is byte-identical (signature
/// included) AND the secret's handle + raw key match. One DISCOVER grant mints
/// one unique key by construction, so a same-`grant_id` install with any
/// different bytes is a CONFLICT, never a silent replacement.
fn records_identical(existing: &GrantAudienceRecord, incoming: &GrantAudienceRecord) -> bool {
    // §18: the raw-key comparison is CONSTANT-TIME. Every other equality here
    // is over public material (the signed grant, the routing handle), but
    // `discovery_key` is the secret itself, and this is the one place in the
    // grant family where two secrets are compared to each other. `==` on
    // `[u8; 32]` short-circuits on the first differing byte.
    //
    // No attacker was constructed for this: reaching it requires the local
    // operator/SDK install API, so a caller who can supply candidate keys and
    // time the result already holds the install path. It is fixed because a
    // secret-vs-secret comparison should not depend on that argument staying
    // true — an install surface exposed over RPC later would inherit an oracle
    // silently.
    existing.grant == incoming.grant
        && existing.secret.audience_handle == incoming.secret.audience_handle
        && constant_time_eq_32(
            existing.secret.discovery_key(),
            incoming.secret.discovery_key(),
        )
}

/// Branch-free, data-independent equality for a 32-byte secret.
///
/// Accumulates the XOR of every byte pair and compares once, so the running
/// time is independent of WHERE the first difference falls. `black_box` on the
/// accumulator keeps the optimizer from reintroducing an early exit.
fn constant_time_eq_32(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut diff = 0u8;
    for i in 0..32 {
        diff |= a[i] ^ b[i];
    }
    std::hint::black_box(diff) == 0
}

/// The common, role-agnostic install invariants (Kyra OA3-4b2 slice 2): the grant
/// verifies, is currently valid, carries DISCOVER + a discovery binding, and the
/// out-of-band secret is this grant's key. Consumes `grant`/`secret` and returns
/// the validated (but not-yet-stored) record.
fn validate_common(
    grant: OrgCapabilityGrant,
    secret: OrgAudienceSecret,
    now_secs: u64,
    skew_secs: u64,
) -> Result<GrantAudienceRecord, GrantAudienceInstallError> {
    grant
        .verify()
        .map_err(|_| GrantAudienceInstallError::GrantInvalid)?;
    grant
        .is_valid_at_with_skew(now_secs, skew_secs)
        .map_err(|_| GrantAudienceInstallError::GrantNotCurrent)?;
    if !grant.permits_discover() {
        return Err(GrantAudienceInstallError::MissingDiscover);
    }
    if grant.discovery.is_none() {
        return Err(GrantAudienceInstallError::NoDiscoveryBinding);
    }
    if !secret.matches_grant(&grant) {
        return Err(GrantAudienceInstallError::SecretMismatch);
    }
    // `install_seq` is stamped by the installing node (`with_install_seq`)
    // once the node-context invariants pass; validation itself is
    // node-agnostic and cannot mint a sequence.
    Ok(GrantAudienceRecord {
        grant,
        secret,
        install_seq: 0,
    })
}

/// Validate a PROVIDER-side record (Kyra OA3-4b2 slice 2): the common invariants
/// plus `issuer_org == provider_owner_org` and the grant's target scope covering
/// THIS provider. `provider_owner_org` and `provider_entity` come from the node's
/// installed authority + identity.
pub(crate) fn validate_provider_record(
    grant: OrgCapabilityGrant,
    secret: OrgAudienceSecret,
    provider_owner_org: &OrgId,
    provider_entity: &EntityId,
    now_secs: u64,
    skew_secs: u64,
) -> Result<GrantAudienceRecord, GrantAudienceInstallError> {
    let record = validate_common(grant, secret, now_secs, skew_secs)?;
    if &record.grant.issuer_org != provider_owner_org {
        return Err(GrantAudienceInstallError::WrongProviderIssuer);
    }
    if !record
        .grant
        .target_scope
        .covers(provider_entity, Some(provider_owner_org))
    {
        return Err(GrantAudienceInstallError::ProviderNotCovered);
    }
    Ok(record)
}

/// Whether an installed provider grant is currently ELIGIBLE to seal a granted
/// envelope (Kyra OA3-4b2 closure). Installation validated the grant once, but
/// emission is a later authority decision under a potentially newer same-org
/// authority configuration and a non-monotonic wall clock, so re-check the full
/// validity window (not just `not_after`), the issuer applicability, and target
/// coverage before building an envelope. No repeated signature verification cost
/// beyond `verify()` inside `is_valid_at_with_skew` — the immutable stored bytes
/// were already signature-verified at installation. An inactive record is simply
/// omitted for this emission; the snapshot stays installed for when it becomes
/// active.
pub(crate) fn grant_active_for_emission(
    grant: &OrgCapabilityGrant,
    provider_entity: &EntityId,
    provider_owner_org: &OrgId,
    now_secs: u64,
    skew_secs: u64,
) -> bool {
    grant.is_valid_at_with_skew(now_secs, skew_secs).is_ok()
        && &grant.issuer_org == provider_owner_org
        && grant
            .target_scope
            .covers(provider_entity, Some(provider_owner_org))
}

/// Validate a CONSUMER-side record (Kyra OA3-4b2 slice 2): the common invariants
/// plus `grantee_org == consumer_owner_org` (the grant names A, this node).
pub(crate) fn validate_consumer_record(
    grant: OrgCapabilityGrant,
    secret: OrgAudienceSecret,
    consumer_owner_org: &OrgId,
    now_secs: u64,
    skew_secs: u64,
) -> Result<GrantAudienceRecord, GrantAudienceInstallError> {
    let record = validate_common(grant, secret, now_secs, skew_secs)?;
    if &record.grant.grantee_org != consumer_owner_org {
        return Err(GrantAudienceInstallError::WrongConsumerGrantee);
    }
    Ok(record)
}

/// An immutable snapshot of the PROVIDER grant-audience registry. Read lock-free
/// off the node's `ArcSwap`; the granted-emission projection iterates its active
/// records, and the send seqlock pointer-compares the exact `Arc` it sealed
/// under against the currently-installed one (OA3-4b2 slice 3).
#[derive(Default, Debug)]
pub struct ProviderGrantSnapshot(GrantAudienceRecords);

impl ProviderGrantSnapshot {
    /// The per-role capacity ceiling.
    pub const CAPACITY: usize = MAX_PROVIDER_GRANT_AUDIENCES;

    /// An empty snapshot (the node's initial state).
    pub fn empty() -> Self {
        Self::default()
    }

    /// The record for `grant_id`, if installed.
    pub fn get(&self, grant_id: &[u8; 32]) -> Option<&Arc<GrantAudienceRecord>> {
        self.0.get(grant_id)
    }

    /// Every installed record, in deterministic `grant_id` order (the granted
    /// emission fans out in this order).
    pub fn records(&self) -> impl Iterator<Item = &Arc<GrantAudienceRecord>> {
        self.0.records()
    }

    /// The number of installed records.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.0.len() == 0
    }

    /// Install a validated record; see [`GrantAudienceRecords::install`].
    pub(crate) fn with_record(
        &self,
        record: Arc<GrantAudienceRecord>,
        now_secs: u64,
    ) -> Result<Option<Self>, GrantAudienceInstallError> {
        Ok(self.0.install(record, Self::CAPACITY, now_secs)?.map(Self))
    }

    /// Remove `grant_id`; see [`GrantAudienceRecords::without`].
    pub(crate) fn without(&self, grant_id: &[u8; 32]) -> Option<Self> {
        self.0.without(grant_id).map(Self)
    }
}

/// An immutable snapshot of the CONSUMER grant-audience registry. The inbound
/// nonzero-grant ingest selector looks a record up by `grant_id` and confirms the
/// envelope's `audience_handle` before building the granted authority (OA3-4b2
/// slice 4).
#[derive(Default, Debug)]
pub struct ConsumerGrantSnapshot(GrantAudienceRecords);

impl ConsumerGrantSnapshot {
    /// The per-role capacity ceiling.
    pub const CAPACITY: usize = MAX_CONSUMER_GRANT_AUDIENCES;

    /// An empty snapshot (the node's initial state).
    pub fn empty() -> Self {
        Self::default()
    }

    /// The record for `grant_id`, if installed. The ingest selector additionally
    /// checks the envelope's audience handle against the record before use.
    pub fn get(&self, grant_id: &[u8; 32]) -> Option<&Arc<GrantAudienceRecord>> {
        self.0.get(grant_id)
    }

    /// Every installed record, in deterministic `grant_id` order.
    pub fn records(&self) -> impl Iterator<Item = &Arc<GrantAudienceRecord>> {
        self.0.records()
    }

    /// The number of installed records.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.0.len() == 0
    }

    /// Install a validated record; see [`GrantAudienceRecords::install`].
    pub(crate) fn with_record(
        &self,
        record: Arc<GrantAudienceRecord>,
        now_secs: u64,
    ) -> Result<Option<Self>, GrantAudienceInstallError> {
        Ok(self.0.install(record, Self::CAPACITY, now_secs)?.map(Self))
    }

    /// Remove `grant_id`; see [`GrantAudienceRecords::without`].
    pub(crate) fn without(&self, grant_id: &[u8; 32]) -> Option<Self> {
        self.0.without(grant_id).map(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::org::{current_timestamp, OrgKeypair};
    use crate::adapter::net::behavior::org_grant::{
        CapabilityAuthorityId, GrantRights, GrantTargetScope,
    };
    use crate::adapter::net::identity::EntityKeypair;

    const SKEW: u64 = 60;

    fn provider_kp() -> EntityKeypair {
        EntityKeypair::from_bytes([0x21u8; 32])
    }

    fn provider_entity() -> EntityId {
        provider_kp().entity_id().clone()
    }

    fn org_b() -> OrgKeypair {
        // The PROVIDER's own org (issuer B).
        OrgKeypair::from_bytes([0x42u8; 32])
    }

    fn org_a() -> OrgKeypair {
        // The GRANTEE org (consumer A).
        OrgKeypair::from_bytes([0x77u8; 32])
    }

    fn cap() -> CapabilityAuthorityId {
        CapabilityAuthorityId::for_tag("nrpc:billing")
    }

    /// A canonical B→A DISCOVER grant over an exact provider node, plus its
    /// out-of-band secret.
    fn canonical_pair() -> (OrgCapabilityGrant, OrgAudienceSecret) {
        let (grant, secret) = OrgCapabilityGrant::try_issue(
            &org_b(),
            org_a().org_id(),
            cap(),
            GrantRights::DISCOVER.union(GrantRights::INVOKE),
            GrantTargetScope::ExactNode(provider_entity()),
            3600,
        )
        .expect("issue grant");
        (grant, secret.expect("DISCOVER mints a secret"))
    }

    // ---------------- validation (node-context invariants) ----------------

    #[test]
    fn valid_canonical_pair_installs_both_roles() {
        let now = current_timestamp();
        // Provider side: issuer B is the provider's owner org; target covers P.
        let (g, s) = canonical_pair();
        let record =
            validate_provider_record(g, s, &org_b().org_id(), &provider_entity(), now, SKEW)
                .expect("provider record valid");
        assert_eq!(record.grant().grantee_org, org_a().org_id());

        // Consumer side: grantee A is the consumer's owner org.
        let (g, s) = canonical_pair();
        let record = validate_consumer_record(g, s, &org_a().org_id(), now, SKEW)
            .expect("consumer record valid");
        assert_eq!(record.grant().issuer_org, org_b().org_id());
    }

    /// Kyra OA3-4b2 closure: emission re-checks the FULL grant validity — a record
    /// installed valid may be inactive at a later emission (future not_before,
    /// lapsed not_after, or inapplicable under a since-changed authority), and is
    /// then omitted for that round. Drives the pure `grant_active_for_emission`
    /// gate `granted_envelopes` uses.
    #[test]
    fn grant_active_for_emission_rechecks_window_issuer_and_target() {
        let issuer = org_b();
        let owner = issuer.org_id();
        let provider = provider_entity();
        let exact = GrantTargetScope::ExactNode(provider.clone());
        // Build an INVOKE grant (no discovery binding needed) with an explicit
        // window — `grant_active_for_emission` checks window + issuer + target,
        // never DISCOVER (that is an install-time invariant).
        let mk = |not_before: u64, not_after: u64, target: GrantTargetScope| {
            OrgCapabilityGrant::issue_at(
                &issuer,
                [1u8; 32],
                org_a().org_id(),
                cap(),
                GrantRights::INVOKE,
                target,
                None,
                not_before,
                not_after,
                7,
            )
        };
        let now = 10_000u64;
        let skew = 0u64;

        // Inside the validity window → active (an envelope would be built).
        assert!(grant_active_for_emission(
            &mk(now - 100, now + 100, exact.clone()),
            &provider,
            &owner,
            now,
            skew
        ));
        // Evaluated BEFORE not_before → inactive (no envelope).
        assert!(!grant_active_for_emission(
            &mk(now + 50, now + 100, exact.clone()),
            &provider,
            &owner,
            now,
            skew
        ));
        // Evaluated AT/AFTER not_after → inactive (no envelope).
        assert!(!grant_active_for_emission(
            &mk(now - 100, now, exact.clone()),
            &provider,
            &owner,
            now,
            skew
        ));
        // Issuer org that is not this provider's owner → inactive.
        assert!(!grant_active_for_emission(
            &mk(now - 100, now + 100, exact.clone()),
            &provider,
            &org_a().org_id(),
            now,
            skew
        ));
        // Target scope that does not cover this provider → inactive.
        let other = EntityKeypair::from_bytes([0x44u8; 32]).entity_id().clone();
        assert!(!grant_active_for_emission(
            &mk(now - 100, now + 100, GrantTargetScope::ExactNode(other)),
            &provider,
            &owner,
            now,
            skew
        ));
    }

    #[test]
    fn provider_install_refuses_wrong_issuer_and_target() {
        let now = current_timestamp();
        // Wrong provider owner org: the grant was issued by B, but this node
        // claims org A as its owner.
        let (g, s) = canonical_pair();
        assert_eq!(
            validate_provider_record(g, s, &org_a().org_id(), &provider_entity(), now, SKEW)
                .unwrap_err(),
            GrantAudienceInstallError::WrongProviderIssuer
        );

        // Right issuer, wrong target node: the grant targets a DIFFERENT exact
        // provider, so it does not cover this node.
        let other = EntityKeypair::from_bytes([0x33u8; 32]).entity_id().clone();
        let (g, s) = OrgCapabilityGrant::try_issue(
            &org_b(),
            org_a().org_id(),
            cap(),
            GrantRights::DISCOVER,
            GrantTargetScope::ExactNode(other),
            3600,
        )
        .expect("issue");
        let s = s.expect("secret");
        assert_eq!(
            validate_provider_record(g, s, &org_b().org_id(), &provider_entity(), now, SKEW)
                .unwrap_err(),
            GrantAudienceInstallError::ProviderNotCovered
        );
    }

    #[test]
    fn consumer_install_refuses_wrong_grantee() {
        let now = current_timestamp();
        let (g, s) = canonical_pair();
        // This node claims org B as its owner, but the grant names A as grantee.
        assert_eq!(
            validate_consumer_record(g, s, &org_b().org_id(), now, SKEW).unwrap_err(),
            GrantAudienceInstallError::WrongConsumerGrantee
        );
    }

    #[test]
    fn invoke_only_grant_is_refused() {
        let now = current_timestamp();
        let (grant, secret) = OrgCapabilityGrant::try_issue(
            &org_b(),
            org_a().org_id(),
            cap(),
            GrantRights::INVOKE,
            GrantTargetScope::ExactNode(provider_entity()),
            3600,
        )
        .expect("issue invoke-only");
        assert!(secret.is_none(), "INVOKE-only mints no secret");
        // Pair the INVOKE-only grant with an UNRELATED secret so the missing-
        // discover reason (not a secret mismatch) surfaces first.
        let (_g2, other_secret) = canonical_pair();
        assert_eq!(
            validate_consumer_record(grant, other_secret, &org_a().org_id(), now, SKEW)
                .unwrap_err(),
            GrantAudienceInstallError::MissingDiscover
        );
    }

    #[test]
    fn mismatched_secret_is_refused() {
        let now = current_timestamp();
        // A discover grant, but paired with a secret from a DIFFERENT grant.
        let (grant, _secret) = canonical_pair();
        let (_other_grant, other_secret) = OrgCapabilityGrant::try_issue(
            &org_b(),
            org_a().org_id(),
            CapabilityAuthorityId::for_tag("nrpc:other"),
            GrantRights::DISCOVER,
            GrantTargetScope::ExactNode(provider_entity()),
            3600,
        )
        .expect("issue other");
        let other_secret = other_secret.expect("secret");
        assert_eq!(
            validate_consumer_record(grant, other_secret, &org_a().org_id(), now, SKEW)
                .unwrap_err(),
            GrantAudienceInstallError::SecretMismatch
        );
    }

    // ---------------- snapshot RMW (idempotency / conflict / capacity) ------

    fn provider_record() -> Arc<GrantAudienceRecord> {
        let now = current_timestamp();
        let (g, s) = canonical_pair();
        Arc::new(
            validate_provider_record(g, s, &org_b().org_id(), &provider_entity(), now, SKEW)
                .expect("valid"),
        )
    }

    #[test]
    fn install_is_idempotent_and_conflict_is_refused() {
        let now = current_timestamp();
        let snap = ProviderGrantSnapshot::empty();
        let record = provider_record();
        let grant_id = *record.grant_id();

        // First install rotates the snapshot.
        let snap = snap
            .with_record(Arc::clone(&record), now)
            .expect("install ok")
            .expect("a new snapshot was produced");
        assert_eq!(snap.len(), 1);
        assert!(snap.get(&grant_id).is_some());

        // Re-installing the IDENTICAL record is an idempotent no-op (no new
        // snapshot — the pointer must not rotate).
        assert!(
            snap.with_record(Arc::clone(&record), now)
                .expect("idempotent ok")
                .is_none(),
            "identical re-install produces no new snapshot"
        );

        // A DIFFERENT grant/secret under the SAME grant id is a conflict. Force
        // the id to collide (conflict is decided on the stored bytes, not a fresh
        // verify, so re-signing is unnecessary) and pair it with a foreign secret.
        let (mut clashing_grant, _s) = canonical_pair();
        clashing_grant.grant_id = grant_id;
        let (_g, foreign_secret) = OrgCapabilityGrant::try_issue(
            &org_b(),
            org_a().org_id(),
            CapabilityAuthorityId::for_tag("nrpc:clash"),
            GrantRights::DISCOVER,
            GrantTargetScope::ExactNode(provider_entity()),
            3600,
        )
        .expect("issue");
        let foreign_secret = foreign_secret.expect("secret");
        let clashing = Arc::new(GrantAudienceRecord {
            grant: clashing_grant,
            secret: foreign_secret,
            install_seq: 0,
        });
        assert_eq!(
            snap.with_record(clashing, now).unwrap_err(),
            GrantAudienceInstallError::Conflict
        );
    }

    /// A distinct provider record per index — a distinct exact-node target grant.
    fn distinct_provider_record(index: u64, ttl_secs: u64) -> Arc<GrantAudienceRecord> {
        let now = current_timestamp();
        let mut seed = [0u8; 32];
        seed[..8].copy_from_slice(&index.to_le_bytes());
        let target = EntityKeypair::from_bytes(seed).entity_id().clone();
        let (grant, secret) = OrgCapabilityGrant::try_issue(
            &org_b(),
            org_a().org_id(),
            cap(),
            GrantRights::DISCOVER,
            GrantTargetScope::ExactNode(target.clone()),
            ttl_secs,
        )
        .expect("issue");
        let secret = secret.expect("secret");
        // This node IS `target`, so target coverage holds.
        Arc::new(
            validate_provider_record(grant, secret, &org_b().org_id(), &target, now, SKEW)
                .expect("valid"),
        )
    }

    #[test]
    fn capacity_is_fail_closed_and_never_evicts_active() {
        let now = current_timestamp();
        let mut snap = ProviderGrantSnapshot::empty();
        for index in 0..ProviderGrantSnapshot::CAPACITY as u64 {
            snap = snap
                .with_record(distinct_provider_record(index, 3600), now)
                .expect("install ok")
                .expect("new snapshot");
        }
        assert_eq!(snap.len(), ProviderGrantSnapshot::CAPACITY);
        // A further DISTINCT record is refused — every existing record is active
        // (far-future expiry), so the fail-closed sweep frees nothing.
        assert_eq!(
            snap.with_record(distinct_provider_record(u64::MAX, 3600), now)
                .unwrap_err(),
            GrantAudienceInstallError::AtCapacity
        );
        assert_eq!(snap.len(), ProviderGrantSnapshot::CAPACITY);
    }

    #[test]
    fn capacity_reclaims_only_expired_records() {
        // Fill to capacity with records whose grants expire soon, then advance
        // the clock past their expiry: a new install reclaims the expired slots
        // rather than refusing.
        let base = current_timestamp();
        let mut snap = ProviderGrantSnapshot::empty();
        for index in 0..ProviderGrantSnapshot::CAPACITY as u64 {
            // A short TTL so the grants expire within the test window.
            snap = snap
                .with_record(distinct_provider_record(index, 120), base)
                .expect("ok")
                .expect("new");
        }
        assert_eq!(snap.len(), ProviderGrantSnapshot::CAPACITY);
        // At a clock well past every grant's not_after, the sweep frees the whole
        // set, so a fresh record installs.
        let later = base + 10_000;
        let fresh = snap
            .with_record(distinct_provider_record(u64::MAX, 3600), later)
            .expect("ok")
            .expect("new after reclaim");
        assert_eq!(fresh.len(), 1, "expired records were reclaimed");
    }

    #[test]
    fn remove_is_a_noop_when_absent_and_releases_the_record_when_present() {
        let now = current_timestamp();
        let record = provider_record();
        let grant_id = *record.grant_id();
        let snap = ProviderGrantSnapshot::empty()
            .with_record(Arc::clone(&record), now)
            .expect("ok")
            .expect("new");

        // Removing an ABSENT grant id is a no-op (no new snapshot — no pointer
        // rotation).
        assert!(snap.without(&[0xEE; 32]).is_none());

        // Removing the present record produces a new, empty snapshot. The
        // outstanding `record` Arc keeps the value alive (scrub-on-last-release):
        // dropping the snapshot alone must not drop the record while a holder
        // remains.
        let removed = snap.without(&grant_id).expect("removed");
        assert!(removed.is_empty());
        drop(snap);
        // The value is still alive because `record` holds it; the secret's key is
        // zeroized only when this last Arc drops (Drop witnessed in org_grant).
        assert_eq!(Arc::strong_count(&record), 1);
        assert_eq!(record.grant_id(), &grant_id);
    }
}
