//! OA-3 §3.3 of `docs/plans/ORG_CAPABILITY_AUTH_PLAN.md` — the ingest AUTHORITY
//! for grant-scoped private discovery: verify a decoded, outer-signature-
//! authenticated [`ScopedCapabilityAnnouncement`] against the local node's
//! audience material and, on success, produce a verified, decrypted descriptor
//! tagged with the fold partition it belongs to.
//!
//! This is the security heart of private discovery, kept as a PURE verification
//! layer — it never touches the live fold, gossip, or mesh state (that wiring is
//! OA3-4). Every path is fail-closed: any check that does not hold returns a
//! typed [`ScopedIngestError`] and no descriptor is ever revealed.
//!
//! Two mutually-exclusive audiences (§3.3):
//!
//! - **Owner** — an internal private capability of the node's OWN org. The local
//!   owner audience credential decrypts it; the envelope carries the reserved
//!   zero grant-id sentinel. The owner credential grants ONLY knowledge —
//!   internal invocation still requires `OwnerDelegated` admission (OA-2).
//! - **Granted** — a cross-org private capability. An installed
//!   `(OrgCapabilityGrant, OrgAudienceSecret)` pair for which B (the issuer /
//!   provider org) signed a DISCOVER grant to A (the local org) decrypts it.
//!
//! The envelope handed in is already outer-signature-verified (that is the
//! type-level invariant of [`ScopedCapabilityAnnouncement`]). This module adds
//! the remaining authority: the inline `owner_cert` binds the publishing
//! provider P to the expected org (with revocation floors and currentness), the
//! grant (for the cross-org case) authorizes A to discover on B's target, the
//! installed secret's commitment matches the signed grant, the envelope is
//! unexpired, and finally the AEAD opens under the matching audience key.

use super::org::OrgId;
use super::org_authority::OwnerAudienceCredential;
use super::org_grant::{OrgAudienceSecret, OrgCapabilityGrant};
use super::org_revocation::OrgRevocationState;
use super::org_scoped_ann::ScopedCapabilityAnnouncement;
use crate::adapter::net::identity::EntityId;

/// The fold partition a verified capability belongs to (§3.3). Owner, Grant, and
/// Public entries are mutually invisible and invisible to unscoped queries
/// (enforced by the fold in OA3-4). A scoped ingest only ever yields `Owner` or
/// `Grant`; `Public` is the partition the existing plaintext CAP-ANN path maps
/// to, included here so the fold has one exhaustive scope type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityAudienceScope {
    /// The plaintext, globally-discoverable partition (existing CAP-ANN).
    Public,
    /// An internal owner-scoped capability, keyed by the owner org and the owner
    /// audience handle.
    Owner {
        /// The owning org (also the local node's org for an owner ingest).
        org_id: OrgId,
        /// The owner audience routing handle.
        audience_handle: [u8; 32],
    },
    /// A cross-org granted capability, keyed by the grant id and its audience
    /// handle.
    Grant {
        /// The grant this capability was discovered under.
        grant_id: [u8; 32],
        /// The grant's audience routing handle.
        audience_handle: [u8; 32],
    },
}

/// The LOCAL audience material used to attempt an ingest. Borrowing (never
/// owning a copy of the raw key) — a deliberate improvement over the plan's
/// owned sketch: the discovery key is reached only through a borrow of the
/// non-serializable secret types, so this transient enum never becomes a place
/// a key could be copied out of or serialized from.
pub enum AudienceAuthority<'a> {
    /// The node's own owner audience credential.
    Owner {
        /// The node's org.
        owner_org: OrgId,
        /// The owner audience handle.
        audience_handle: [u8; 32],
        /// The owner audience decryption key (borrowed).
        discovery_key: &'a [u8; 32],
    },
    /// An installed cross-org grant and its out-of-band secret.
    Granted {
        /// The signed grant B → A.
        grant: &'a OrgCapabilityGrant,
        /// The out-of-band audience secret for that grant.
        secret: &'a OrgAudienceSecret,
    },
}

impl<'a> AudienceAuthority<'a> {
    /// Build an owner authority from the node's org and its owner audience
    /// credential (borrows the credential's key — no copy).
    pub fn owner(owner_org: OrgId, credential: &'a OwnerAudienceCredential) -> Self {
        Self::Owner {
            owner_org,
            audience_handle: credential.audience_handle,
            discovery_key: credential.discovery_key(),
        }
    }

    /// Build a granted authority from an installed grant + secret pair.
    pub fn granted(grant: &'a OrgCapabilityGrant, secret: &'a OrgAudienceSecret) -> Self {
        Self::Granted { grant, secret }
    }

    /// The audience handle this authority decrypts for — used to select the
    /// matching authority for an incoming envelope BEFORE verification (a cheap
    /// pre-filter; verification still re-checks everything).
    pub fn audience_handle(&self) -> &[u8; 32] {
        match self {
            Self::Owner {
                audience_handle, ..
            } => audience_handle,
            Self::Granted { secret, .. } => &secret.audience_handle,
        }
    }
}

/// Per-ingest context: the local node's identity and the single clock sample /
/// revocation view every credential check shares.
pub struct ScopedIngestContext<'a> {
    /// The local node's owner org. For an owner ingest it must equal the
    /// envelope's `owner_org`; for a granted ingest it must equal the grant's
    /// `grantee_org` (the grant names A).
    pub local_owner_org: OrgId,
    /// Persisted revocation floors for the `owner_cert` generation check.
    pub floors: &'a OrgRevocationState,
    /// The single clock sample (unix seconds) all freshness checks use.
    pub now_secs: u64,
    /// Clock-skew tolerance applied to certificate/grant/envelope windows.
    pub skew_secs: u64,
}

/// A verified, decrypted scoped capability ready for the fold (OA3-4).
#[derive(Clone)]
pub struct VerifiedScopedCapability {
    scope: CapabilityAudienceScope,
    provider: EntityId,
    owner_org: OrgId,
    generation: u64,
    expires_at: u64,
    descriptor: Vec<u8>,
}

impl VerifiedScopedCapability {
    /// The fold partition this capability belongs to.
    pub fn scope(&self) -> &CapabilityAudienceScope {
        &self.scope
    }
    /// The publishing provider P.
    pub fn provider(&self) -> &EntityId {
        &self.provider
    }
    /// The org P belongs to (the local org for owner, the issuer B for granted).
    pub fn owner_org(&self) -> &OrgId {
        &self.owner_org
    }
    /// The announcement generation (freshness / dedup ordering).
    pub fn generation(&self) -> u64 {
        self.generation
    }
    /// The envelope expiry (unix seconds).
    pub fn expires_at(&self) -> u64 {
        self.expires_at
    }
    /// The decrypted capability descriptor plaintext (opaque here; parsed by the
    /// fold layer).
    pub fn descriptor(&self) -> &[u8] {
        &self.descriptor
    }
}

impl std::fmt::Debug for VerifiedScopedCapability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VerifiedScopedCapability")
            .field("scope", &self.scope)
            .field("provider", &self.provider)
            .field("owner_org", &self.owner_org)
            .field("generation", &self.generation)
            .field("expires_at", &self.expires_at)
            .field("descriptor_len", &self.descriptor.len())
            .finish()
    }
}

/// Why a scoped ingest was refused. Distinguishable, fail-closed reasons; none
/// reveals key material or acts as a decryption oracle (the AEAD failure is a
/// single opaque [`Self::DecryptFailed`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopedIngestError {
    /// The authority kind and the envelope kind disagree (an owner authority
    /// against a granted envelope, or vice versa).
    AudienceKindMismatch,
    /// The envelope's `owner_org` is not this node's org (owner ingest).
    NotForThisOwner,
    /// The audience handle does not match this authority.
    HandleMismatch,
    /// The inline `owner_cert` does not bind the publishing provider to the
    /// expected org (wrong `member`, wrong `org_id`, or the envelope's
    /// `owner_org` is not the grant's issuer).
    ProviderCertMismatch,
    /// The `owner_cert` signature or validity window failed.
    MembershipInvalid,
    /// The `owner_cert` generation is below the revocation floor for
    /// `(org, provider)`.
    MembershipRevoked,
    /// The envelope has expired.
    Expired,
    /// The grant's signature or validity window failed (granted ingest).
    GrantInvalid,
    /// The grant does not name this node's org as grantee.
    GrantWrongGrantee,
    /// The grant does not carry DISCOVER rights.
    GrantMissingDiscover,
    /// The envelope references a different grant id than the authority's grant.
    GrantIdMismatch,
    /// The installed secret is not the out-of-band key for the authority's grant
    /// (grant id or key commitment mismatch).
    SecretMismatch,
    /// The publishing provider is outside the grant's target scope.
    TargetNotCovered,
    /// The AEAD open failed (wrong key / tampered ciphertext). A single opaque
    /// reason — never a per-field oracle.
    DecryptFailed,
}

impl std::fmt::Display for ScopedIngestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ScopedIngestError::AudienceKindMismatch => "audience kind mismatch",
            ScopedIngestError::NotForThisOwner => "envelope owner org is not this node's org",
            ScopedIngestError::HandleMismatch => "audience handle mismatch",
            ScopedIngestError::ProviderCertMismatch => "provider membership cert does not bind P",
            ScopedIngestError::MembershipInvalid => "provider membership cert invalid",
            ScopedIngestError::MembershipRevoked => "provider membership below revocation floor",
            ScopedIngestError::Expired => "scoped announcement expired",
            ScopedIngestError::GrantInvalid => "capability grant invalid",
            ScopedIngestError::GrantWrongGrantee => "grant does not name this org",
            ScopedIngestError::GrantMissingDiscover => "grant lacks DISCOVER rights",
            ScopedIngestError::GrantIdMismatch => "envelope grant id does not match the grant",
            ScopedIngestError::SecretMismatch => "installed secret does not match the grant",
            ScopedIngestError::TargetNotCovered => "provider outside the grant target scope",
            ScopedIngestError::DecryptFailed => "scoped announcement AEAD open failed",
        };
        f.write_str(s)
    }
}

impl std::error::Error for ScopedIngestError {}

/// Verify an outer-signature-authenticated scoped announcement against a local
/// audience authority, returning the decrypted, fold-ready capability on
/// success. Dispatches to the owner or granted pipeline by authority kind; each
/// pipeline independently re-checks the envelope's kind, so a mismatched
/// authority is refused rather than silently taking the wrong path.
pub fn verify_scoped_ingest(
    envelope: &ScopedCapabilityAnnouncement,
    authority: &AudienceAuthority<'_>,
    ctx: &ScopedIngestContext<'_>,
) -> Result<VerifiedScopedCapability, ScopedIngestError> {
    match authority {
        AudienceAuthority::Owner {
            owner_org,
            audience_handle,
            discovery_key,
        } => verify_owner_ingest(envelope, *owner_org, audience_handle, discovery_key, ctx),
        AudienceAuthority::Granted { grant, secret } => {
            verify_granted_ingest(envelope, grant, secret, ctx)
        }
    }
}

/// Owner-audience ingest (§3.3): an internal private capability of the node's own
/// org.
fn verify_owner_ingest(
    envelope: &ScopedCapabilityAnnouncement,
    owner_org: OrgId,
    audience_handle: &[u8; 32],
    discovery_key: &[u8; 32],
    ctx: &ScopedIngestContext<'_>,
) -> Result<VerifiedScopedCapability, ScopedIngestError> {
    // An owner authority only ingests an owner-scoped envelope (zero sentinel).
    if !envelope.is_owner_audience() {
        return Err(ScopedIngestError::AudienceKindMismatch);
    }
    // The envelope must be for THIS node's org, and the authority must be too.
    if owner_org != ctx.local_owner_org || envelope.owner_org() != &owner_org {
        return Err(ScopedIngestError::NotForThisOwner);
    }
    // The handle must match the owner credential.
    if envelope.audience_handle() != audience_handle {
        return Err(ScopedIngestError::HandleMismatch);
    }
    // The inline cert must vouch for THIS provider under THIS org, and be
    // currently valid + at/above the revocation floor.
    verify_provider_membership(envelope, &owner_org, ctx)?;
    // Envelope freshness.
    if is_expired(envelope, ctx) {
        return Err(ScopedIngestError::Expired);
    }
    // The AEAD open under the owner key both reveals the descriptor AND
    // cryptographically confirms the envelope was sealed to this owner audience.
    let descriptor = envelope
        .open_with(discovery_key)
        .map_err(|_| ScopedIngestError::DecryptFailed)?;
    Ok(VerifiedScopedCapability {
        scope: CapabilityAudienceScope::Owner {
            org_id: owner_org,
            audience_handle: *audience_handle,
        },
        provider: envelope.provider().clone(),
        owner_org,
        generation: envelope.generation(),
        expires_at: envelope.expires_at(),
        descriptor,
    })
}

/// Granted-audience ingest (§3.3): a cross-org private capability discovered
/// under a B → A DISCOVER grant.
fn verify_granted_ingest(
    envelope: &ScopedCapabilityAnnouncement,
    grant: &OrgCapabilityGrant,
    secret: &OrgAudienceSecret,
    ctx: &ScopedIngestContext<'_>,
) -> Result<VerifiedScopedCapability, ScopedIngestError> {
    // A granted authority never ingests an owner-scoped envelope.
    if envelope.is_owner_audience() {
        return Err(ScopedIngestError::AudienceKindMismatch);
    }
    // The installed secret must be the out-of-band key for THIS grant: grant id
    // AND the key commitment in the signed grant (OA2-F `matches_grant`).
    if !secret.matches_grant(grant) {
        return Err(ScopedIngestError::SecretMismatch);
    }
    // The envelope's handle must be the grant's audience handle.
    if envelope.audience_handle() != &secret.audience_handle {
        return Err(ScopedIngestError::HandleMismatch);
    }
    // The envelope must reference THIS grant.
    if envelope.grant_id() != &grant.grant_id {
        return Err(ScopedIngestError::GrantIdMismatch);
    }
    // The grant must be signed by its issuer and currently valid.
    grant
        .verify()
        .map_err(|_| ScopedIngestError::GrantInvalid)?;
    grant
        .is_valid_at_with_skew(ctx.now_secs, ctx.skew_secs)
        .map_err(|_| ScopedIngestError::GrantInvalid)?;
    // The grant must name THIS node's org as grantee, and carry DISCOVER.
    if grant.grantee_org != ctx.local_owner_org {
        return Err(ScopedIngestError::GrantWrongGrantee);
    }
    // Defense in depth: a matching secret already implies a discovery binding,
    // which the structural rule ties to DISCOVER — but assert it explicitly.
    if !grant.permits_discover() {
        return Err(ScopedIngestError::GrantMissingDiscover);
    }
    // The envelope's claimed owner org must be the grant's ISSUER B (P is a
    // B-owned provider), and P must fall within the grant's target scope.
    if envelope.owner_org() != &grant.issuer_org {
        return Err(ScopedIngestError::ProviderCertMismatch);
    }
    if !grant
        .target_scope
        .covers(envelope.provider(), Some(&grant.issuer_org))
    {
        return Err(ScopedIngestError::TargetNotCovered);
    }
    // P's inline cert must vouch for P under the issuer org B, valid + above the
    // floor.
    verify_provider_membership(envelope, &grant.issuer_org, ctx)?;
    // Envelope freshness.
    if is_expired(envelope, ctx) {
        return Err(ScopedIngestError::Expired);
    }
    // The AEAD open under the grant's secret key reveals the descriptor.
    let descriptor = envelope
        .open_with(secret.discovery_key())
        .map_err(|_| ScopedIngestError::DecryptFailed)?;
    Ok(VerifiedScopedCapability {
        scope: CapabilityAudienceScope::Grant {
            grant_id: grant.grant_id,
            audience_handle: *envelope.audience_handle(),
        },
        provider: envelope.provider().clone(),
        owner_org: *envelope.owner_org(),
        generation: envelope.generation(),
        expires_at: envelope.expires_at(),
        descriptor,
    })
}

/// The inline `owner_cert` must vouch for the publishing provider P under
/// `expected_org`, be currently valid (signature + window), and sit at or above
/// the revocation floor for `(expected_org, P)`.
fn verify_provider_membership(
    envelope: &ScopedCapabilityAnnouncement,
    expected_org: &OrgId,
    ctx: &ScopedIngestContext<'_>,
) -> Result<(), ScopedIngestError> {
    let cert = envelope.owner_cert();
    if envelope.provider() != &cert.member || &cert.org_id != expected_org {
        return Err(ScopedIngestError::ProviderCertMismatch);
    }
    cert.is_valid_at_with_skew(ctx.now_secs, ctx.skew_secs)
        .map_err(|_| ScopedIngestError::MembershipInvalid)?;
    let floor = ctx.floors.floor_for(expected_org, envelope.provider());
    if cert.generation < floor {
        return Err(ScopedIngestError::MembershipRevoked);
    }
    Ok(())
}

/// An envelope is expired once the clock passes its `expires_at`, with the same
/// skew tolerance applied to certificate/grant windows.
fn is_expired(envelope: &ScopedCapabilityAnnouncement, ctx: &ScopedIngestContext<'_>) -> bool {
    ctx.now_secs >= envelope.expires_at().saturating_add(ctx.skew_secs)
}

#[cfg(test)]
mod tests {
    use super::super::org::{
        current_timestamp, OrgKeypair, OrgMembershipCert, OrgRevocationBundle,
    };
    use super::super::org_grant::{CapabilityAuthorityId, GrantRights, GrantTargetScope};
    use super::*;
    use crate::adapter::net::identity::EntityKeypair;
    use std::collections::BTreeMap;

    const NOW: u64 = 10_000;
    const SKEW: u64 = 60;

    fn provider_kp() -> EntityKeypair {
        EntityKeypair::from_bytes([2u8; 32])
    }

    // ---------------- owner-audience fixtures ----------------

    struct OwnerFixture {
        provider: EntityKeypair,
        org: OrgKeypair,
        credential: OwnerAudienceCredential,
        envelope: ScopedCapabilityAnnouncement,
        descriptor: Vec<u8>,
    }

    fn owner_fixture() -> OwnerFixture {
        let provider = provider_kp();
        let org = OrgKeypair::from_bytes([1u8; 32]);
        let credential = OwnerAudienceCredential::generate();
        let cert = OrgMembershipCert::issue_at(
            &org,
            provider.entity_id().clone(),
            5,
            0,
            1_000_000,
            0x1234,
        );
        let descriptor = b"owner-capability-descriptor".to_vec();
        let envelope = ScopedCapabilityAnnouncement::build_owner(
            &provider,
            org.org_id(),
            cert,
            credential.audience_handle,
            credential.discovery_key(),
            3,
            20_000,
            &descriptor,
        )
        .expect("build owner envelope");
        OwnerFixture {
            provider,
            org,
            credential,
            envelope,
            descriptor,
        }
    }

    fn owner_ctx(org: OrgId, floors: &OrgRevocationState) -> ScopedIngestContext<'_> {
        ScopedIngestContext {
            local_owner_org: org,
            floors,
            now_secs: NOW,
            skew_secs: SKEW,
        }
    }

    #[test]
    fn owner_ingest_happy_path() {
        let f = owner_fixture();
        let floors = OrgRevocationState::empty();
        let authority = AudienceAuthority::owner(f.org.org_id(), &f.credential);
        let ctx = owner_ctx(f.org.org_id(), &floors);
        let verified = verify_scoped_ingest(&f.envelope, &authority, &ctx).expect("owner ingest");
        assert_eq!(
            verified.scope(),
            &CapabilityAudienceScope::Owner {
                org_id: f.org.org_id(),
                audience_handle: f.credential.audience_handle,
            }
        );
        assert_eq!(verified.provider(), f.provider.entity_id());
        assert_eq!(verified.descriptor(), f.descriptor.as_slice());
    }

    #[test]
    fn owner_ingest_rejects_a_granted_authority() {
        // A granted authority against an owner envelope → kind mismatch.
        let f = owner_fixture();
        let a_org = OrgKeypair::from_bytes([9u8; 32]);
        let (grant, secret) = OrgCapabilityGrant::try_issue(
            &f.org,
            a_org.org_id(),
            CapabilityAuthorityId::for_tag("nrpc:svc"),
            GrantRights::DISCOVER,
            GrantTargetScope::AnyNodeOwnedBy(f.org.org_id()),
            3600,
        )
        .expect("issue grant");
        let secret = secret.expect("discover mints a secret");
        let floors = OrgRevocationState::empty();
        let authority = AudienceAuthority::granted(&grant, &secret);
        let ctx = owner_ctx(f.org.org_id(), &floors);
        assert_eq!(
            verify_scoped_ingest(&f.envelope, &authority, &ctx).unwrap_err(),
            ScopedIngestError::AudienceKindMismatch
        );
    }

    #[test]
    fn owner_ingest_rejects_wrong_owner_and_wrong_handle() {
        let f = owner_fixture();
        let floors = OrgRevocationState::empty();

        // Wrong local owner org.
        let other = OrgKeypair::from_bytes([7u8; 32]);
        let authority = AudienceAuthority::owner(other.org_id(), &f.credential);
        let ctx = owner_ctx(other.org_id(), &floors);
        assert_eq!(
            verify_scoped_ingest(&f.envelope, &authority, &ctx).unwrap_err(),
            ScopedIngestError::NotForThisOwner
        );

        // Right owner, wrong handle.
        let wrong_handle = AudienceAuthority::Owner {
            owner_org: f.org.org_id(),
            audience_handle: [0xAAu8; 32],
            discovery_key: f.credential.discovery_key(),
        };
        let ctx = owner_ctx(f.org.org_id(), &floors);
        assert_eq!(
            verify_scoped_ingest(&f.envelope, &wrong_handle, &ctx).unwrap_err(),
            ScopedIngestError::HandleMismatch
        );
    }

    #[test]
    fn owner_ingest_rejects_wrong_key_as_decrypt_failure() {
        // Handle matches but the key does not → the AEAD open fails.
        let f = owner_fixture();
        let floors = OrgRevocationState::empty();
        let wrong_key = [0x55u8; 32];
        let authority = AudienceAuthority::Owner {
            owner_org: f.org.org_id(),
            audience_handle: f.credential.audience_handle,
            discovery_key: &wrong_key,
        };
        let ctx = owner_ctx(f.org.org_id(), &floors);
        assert_eq!(
            verify_scoped_ingest(&f.envelope, &authority, &ctx).unwrap_err(),
            ScopedIngestError::DecryptFailed
        );
    }

    #[test]
    fn owner_ingest_rejects_a_revoked_provider() {
        let f = owner_fixture();
        // Floor for (org, provider) above the cert generation (5) → revoked.
        let mut floors_map = BTreeMap::new();
        floors_map.insert(f.provider.entity_id().clone(), 6u32);
        let bundle = OrgRevocationBundle::try_issue(&f.org, &floors_map).expect("bundle");
        let mut floors = OrgRevocationState::empty();
        floors.merge_bundle(&bundle);
        let authority = AudienceAuthority::owner(f.org.org_id(), &f.credential);
        let ctx = owner_ctx(f.org.org_id(), &floors);
        assert_eq!(
            verify_scoped_ingest(&f.envelope, &authority, &ctx).unwrap_err(),
            ScopedIngestError::MembershipRevoked
        );
    }

    #[test]
    fn owner_ingest_rejects_an_expired_envelope() {
        let f = owner_fixture();
        let floors = OrgRevocationState::empty();
        let authority = AudienceAuthority::owner(f.org.org_id(), &f.credential);
        // Envelope expires_at = 20_000; clock well past that + skew.
        let ctx = ScopedIngestContext {
            local_owner_org: f.org.org_id(),
            floors: &floors,
            now_secs: 20_000 + SKEW,
            skew_secs: SKEW,
        };
        assert_eq!(
            verify_scoped_ingest(&f.envelope, &authority, &ctx).unwrap_err(),
            ScopedIngestError::Expired
        );
    }

    // ---------------- granted-audience fixtures ----------------
    //
    // `OrgCapabilityGrant::try_issue` stamps a WALL-CLOCK validity window, so the
    // granted fixtures anchor every window and the clock sample to
    // `current_timestamp()` (the owner fixtures need no grant and use a fixed
    // sample). Windows are generous; the envelope expiry is set independently so
    // the "expired envelope" witness fires while the grant is still valid.

    struct GrantedFixture {
        provider: EntityKeypair,
        issuer: OrgKeypair, // B
        a_org: OrgId,       // grantee A
        grant: OrgCapabilityGrant,
        secret: OrgAudienceSecret,
        envelope: ScopedCapabilityAnnouncement,
        descriptor: Vec<u8>,
        now: u64,
    }

    fn granted_fixture_expiring_in(target: GrantTargetScope, expiry_offset: u64) -> GrantedFixture {
        let now = current_timestamp();
        let provider = provider_kp();
        let issuer = OrgKeypair::from_bytes([1u8; 32]); // B
        let a_org = OrgKeypair::from_bytes([9u8; 32]).org_id(); // A
        let (grant, secret) = OrgCapabilityGrant::try_issue(
            &issuer,
            a_org,
            CapabilityAuthorityId::for_tag("nrpc:svc"),
            GrantRights::INVOKE.union(GrantRights::DISCOVER),
            target,
            3600,
        )
        .expect("issue grant");
        let secret = secret.expect("discover mints a secret");
        // P's cert, issued by B, with a window that brackets `now`.
        let cert = OrgMembershipCert::issue_at(
            &issuer,
            provider.entity_id().clone(),
            5,
            now.saturating_sub(3600),
            now + 3600,
            0x1234,
        );
        let descriptor = b"granted-capability-descriptor".to_vec();
        let envelope = ScopedCapabilityAnnouncement::build_granted(
            &provider,
            issuer.org_id(), // owner_org = issuer B
            cert,
            grant.grant_id,
            secret.audience_handle,
            secret.discovery_key(),
            4,
            now + expiry_offset,
            &descriptor,
        )
        .expect("build granted envelope");
        GrantedFixture {
            provider,
            issuer,
            a_org,
            grant,
            secret,
            envelope,
            descriptor,
            now,
        }
    }

    fn granted_fixture(target: GrantTargetScope) -> GrantedFixture {
        granted_fixture_expiring_in(target, 3600)
    }

    fn exact_target(p: &EntityKeypair) -> GrantTargetScope {
        GrantTargetScope::ExactNode(p.entity_id().clone())
    }

    fn granted_ctx(
        a_org: OrgId,
        now_secs: u64,
        floors: &OrgRevocationState,
    ) -> ScopedIngestContext<'_> {
        ScopedIngestContext {
            local_owner_org: a_org,
            floors,
            now_secs,
            skew_secs: SKEW,
        }
    }

    #[test]
    fn granted_ingest_happy_path_exact_target() {
        let p = provider_kp();
        let f = granted_fixture(exact_target(&p));
        let floors = OrgRevocationState::empty();
        let authority = AudienceAuthority::granted(&f.grant, &f.secret);
        let ctx = granted_ctx(f.a_org, f.now + 5, &floors);
        let verified = verify_scoped_ingest(&f.envelope, &authority, &ctx).expect("granted ingest");
        assert_eq!(
            verified.scope(),
            &CapabilityAudienceScope::Grant {
                grant_id: f.grant.grant_id,
                audience_handle: f.secret.audience_handle,
            }
        );
        assert_eq!(verified.owner_org(), &f.issuer.org_id());
        assert_eq!(verified.descriptor(), f.descriptor.as_slice());
    }

    #[test]
    fn granted_ingest_happy_path_any_owned_by() {
        let f = granted_fixture(GrantTargetScope::AnyNodeOwnedBy(
            OrgKeypair::from_bytes([1u8; 32]).org_id(),
        ));
        let floors = OrgRevocationState::empty();
        let authority = AudienceAuthority::granted(&f.grant, &f.secret);
        let ctx = granted_ctx(f.a_org, f.now + 5, &floors);
        assert!(verify_scoped_ingest(&f.envelope, &authority, &ctx).is_ok());
    }

    #[test]
    fn granted_ingest_rejects_owner_authority() {
        let p = provider_kp();
        let f = granted_fixture(exact_target(&p));
        let credential = OwnerAudienceCredential::generate();
        let floors = OrgRevocationState::empty();
        let authority = AudienceAuthority::owner(f.issuer.org_id(), &credential);
        let ctx = granted_ctx(f.issuer.org_id(), f.now + 5, &floors);
        // Owner authority against a granted envelope → kind mismatch.
        assert_eq!(
            verify_scoped_ingest(&f.envelope, &authority, &ctx).unwrap_err(),
            ScopedIngestError::AudienceKindMismatch
        );
    }

    #[test]
    fn granted_ingest_rejects_wrong_grantee() {
        let p = provider_kp();
        let f = granted_fixture(exact_target(&p));
        let floors = OrgRevocationState::empty();
        let authority = AudienceAuthority::granted(&f.grant, &f.secret);
        // Local org is not the grantee A.
        let ctx = granted_ctx(
            OrgKeypair::from_bytes([0x33u8; 32]).org_id(),
            f.now + 5,
            &floors,
        );
        assert_eq!(
            verify_scoped_ingest(&f.envelope, &authority, &ctx).unwrap_err(),
            ScopedIngestError::GrantWrongGrantee
        );
    }

    #[test]
    fn granted_ingest_rejects_a_mismatched_secret() {
        let p = provider_kp();
        let f = granted_fixture(exact_target(&p));
        // A secret from an unrelated grant does not match f.grant.
        let (_other_grant, other_secret) = OrgCapabilityGrant::try_issue(
            &f.issuer,
            f.a_org,
            CapabilityAuthorityId::for_tag("nrpc:other"),
            GrantRights::DISCOVER,
            GrantTargetScope::AnyNodeOwnedBy(f.issuer.org_id()),
            3600,
        )
        .expect("issue other grant");
        let other_secret = other_secret.expect("secret");
        let floors = OrgRevocationState::empty();
        let authority = AudienceAuthority::granted(&f.grant, &other_secret);
        let ctx = granted_ctx(f.a_org, f.now + 5, &floors);
        assert_eq!(
            verify_scoped_ingest(&f.envelope, &authority, &ctx).unwrap_err(),
            ScopedIngestError::SecretMismatch
        );
    }

    #[test]
    fn granted_ingest_rejects_a_provider_outside_the_exact_target() {
        // Grant targets a DIFFERENT exact node; P is not it.
        let other = EntityKeypair::from_bytes([0x44u8; 32]);
        let f = granted_fixture(GrantTargetScope::ExactNode(other.entity_id().clone()));
        let floors = OrgRevocationState::empty();
        let authority = AudienceAuthority::granted(&f.grant, &f.secret);
        let ctx = granted_ctx(f.a_org, f.now + 5, &floors);
        assert_eq!(
            verify_scoped_ingest(&f.envelope, &authority, &ctx).unwrap_err(),
            ScopedIngestError::TargetNotCovered
        );
    }

    #[test]
    fn granted_ingest_rejects_a_revoked_provider() {
        let p = provider_kp();
        let f = granted_fixture(exact_target(&p));
        let mut floors_map = BTreeMap::new();
        floors_map.insert(f.provider.entity_id().clone(), 6u32);
        let bundle = OrgRevocationBundle::try_issue(&f.issuer, &floors_map).expect("bundle");
        let mut floors = OrgRevocationState::empty();
        floors.merge_bundle(&bundle);
        let authority = AudienceAuthority::granted(&f.grant, &f.secret);
        let ctx = granted_ctx(f.a_org, f.now + 5, &floors);
        assert_eq!(
            verify_scoped_ingest(&f.envelope, &authority, &ctx).unwrap_err(),
            ScopedIngestError::MembershipRevoked
        );
    }

    #[test]
    fn granted_ingest_rejects_an_expired_envelope() {
        // The envelope expires shortly after `now`; the grant and cert windows
        // stay valid, so the clock lands PAST the envelope expiry but inside the
        // grant — isolating the Expired reason from GrantInvalid.
        let p = provider_kp();
        let f = granted_fixture_expiring_in(exact_target(&p), 100);
        let floors = OrgRevocationState::empty();
        let authority = AudienceAuthority::granted(&f.grant, &f.secret);
        let ctx = granted_ctx(f.a_org, f.now + 300, &floors);
        assert_eq!(
            verify_scoped_ingest(&f.envelope, &authority, &ctx).unwrap_err(),
            ScopedIngestError::Expired
        );
    }
}
