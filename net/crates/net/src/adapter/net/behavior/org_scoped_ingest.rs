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

use super::capability::CapabilitySet;
use super::org::OrgId;
use super::org_authority::OwnerAudienceCredential;
use super::org_grant::{CapabilityAuthorityId, OrgAudienceSecret, OrgCapabilityGrant};
use super::org_revocation::OrgRevocationState;
use super::org_scoped_ann::ScopedCapabilityAnnouncement;
use crate::adapter::net::identity::EntityId;

/// The fold partition a verified capability belongs to (§3.3). Owner, Grant, and
/// Public entries are mutually invisible and invisible to unscoped queries
/// (enforced by the separate scoped store in OA3-4). A scoped ingest only ever
/// yields `Owner` or `Grant`; `Public` is the partition the existing plaintext
/// CAP-ANN path maps to, included here so the scope type is exhaustive.
///
/// `Ord` is derived so the scope can key the scoped store's `BTreeMap`; the
/// order carries no semantics beyond deterministic iteration.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
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

/// The local node's own membership standing, for the §9 self-revocation gate.
#[derive(Clone, Debug)]
pub struct LocalMemberStanding {
    /// This node's entity id — the `member` a floor for it would name.
    pub member: EntityId,
    /// The generation of the membership certificate this node is running
    /// under. Revoked when a floor for `(local_owner_org, member)` rises
    /// ABOVE it (`generation < floor`, the same boundary every other floor
    /// check in the tree uses).
    pub generation: u32,
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
    /// The LOCAL node's own membership in `local_owner_org` — its entity id and
    /// the generation of the certificate it is running under (§9).
    ///
    /// Checked against `floors` before any inbound envelope is admitted, so a
    /// node that its own org has revoked stops ingesting. Without this, ingest
    /// authority checked only the PUBLISHER's standing: revoking a member
    /// raised its floor everywhere, so every other node refused ITS
    /// announcements and ITS invocations — but the revoked node's copy of
    /// `owner-audience.key` and the owner `audience_handle` are unchanged, so
    /// it kept ingesting and storing every owner-scoped announcement from
    /// every remaining node in the org: the full name list of the org's
    /// internal private capabilities, indefinitely.
    ///
    /// This is defense in depth, not a replacement for key rotation. A
    /// revoked node still HOLDS the symmetric owner key and can decrypt
    /// anything it captured off the wire; closing that needs the §3.4 hard
    /// cutover (redistribute `owner-audience.key` to every node in the org),
    /// which no code path triggers today. What this does close is continued
    /// acceptance of FUTURE announcements through the live ingest authority,
    /// and it gives an operator a signal that a rotation is due.
    ///
    /// `None` for a node with no installed membership of its own (a pure
    /// consumer holding only granted audiences), which cannot be floored in
    /// `local_owner_org` and so has nothing to check.
    pub local_member: Option<LocalMemberStanding>,
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
    /// The inline `owner_cert` membership generation this record was admitted
    /// against (§3.3). Retained so a query can enforce CURRENTNESS: if the
    /// revocation floor for `(owner_org, provider)` later rises above this
    /// generation, the record must become non-queryable even before the next
    /// sweep — mirroring the ingest-time `cert.generation < floor` gate at read
    /// time (Kyra OA3-5 closure).
    provider_cert_generation: u32,
    /// For a GRANTED record, the verified signature of the exact grant that
    /// admitted it (`None` for owner records). Public and already verified; it
    /// binds the whole canonical grant (capability, issuer, grantee, target,
    /// rights, audience binding, validity, nonce). A query enforces EXACT
    /// grant-authority currentness against this: a `remove`-then-`install` of a
    /// DIFFERENT grant sharing the same `grant_id`/handle must not re-expose the
    /// old record — its stored signature won't match the newly-installed grant
    /// (Kyra OA3-4b2 closure). No secret copy is needed.
    grant_signature: Option<[u8; 64]>,
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
    /// The inline membership-cert generation this record was admitted against —
    /// used for query-time revocation currentness (§3.3, Kyra OA3-5 closure).
    pub fn provider_cert_generation(&self) -> u32 {
        self.provider_cert_generation
    }
    /// The verified grant signature that admitted a GRANTED record (`None` for
    /// owner records) — used for exact grant-authority query currentness (Kyra
    /// OA3-4b2 closure).
    pub fn grant_signature(&self) -> Option<&[u8; 64]> {
        self.grant_signature.as_ref()
    }
    /// The decrypted capability descriptor plaintext (opaque here; parsed by the
    /// fold layer).
    pub fn descriptor(&self) -> &[u8] {
        &self.descriptor
    }

    /// Test-only constructor for a capability of an arbitrary scope, bypassing
    /// the envelope/verify pipeline — used by the scoped-store unit tests (and to
    /// witness the store's `Public`-rejection guard, which the real verify path
    /// can never produce).
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn for_test(
        scope: CapabilityAudienceScope,
        provider: EntityId,
        owner_org: OrgId,
        generation: u64,
        expires_at: u64,
        provider_cert_generation: u32,
        grant_signature: Option<[u8; 64]>,
        descriptor: Vec<u8>,
    ) -> Self {
        Self {
            scope,
            provider,
            owner_org,
            generation,
            expires_at,
            provider_cert_generation,
            grant_signature,
            descriptor,
        }
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
            .field("provider_cert_generation", &self.provider_cert_generation)
            .field("has_grant_signature", &self.grant_signature.is_some())
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
    /// THIS node's own membership is below the revocation floor for
    /// `(local_owner_org, self)` — the local org revoked us, so we stop
    /// ingesting scoped announcements entirely (§9). Distinct from
    /// [`Self::MembershipRevoked`], which is about the PUBLISHER: an operator
    /// seeing this needs to rotate `owner-audience.key`, not investigate a
    /// peer.
    LocalMembershipRevoked,
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
    /// The decrypted descriptor is not in the canonical current granted shape:
    /// not exactly one capability tag, carries metadata, or is not canonically
    /// compact-encoded. A provider must not smuggle arbitrary discovery state
    /// under a grant's audience authority (Kyra OA3-4b2 closure).
    DescriptorInvalid,
    /// The decrypted descriptor names a capability OTHER than the one the grant
    /// authorizes — a provider holding a valid C1 grant/secret tried to advertise
    /// a C2 capability under C1's discovery authority (Kyra OA3-4b2 closure).
    DescriptorOutsideGrant,
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
            ScopedIngestError::LocalMembershipRevoked => {
                "this node's own membership is below its org's revocation floor \
                 (rotate the owner audience key)"
            }
            ScopedIngestError::Expired => "scoped announcement expired",
            ScopedIngestError::GrantInvalid => "capability grant invalid",
            ScopedIngestError::GrantWrongGrantee => "grant does not name this org",
            ScopedIngestError::GrantMissingDiscover => "grant lacks DISCOVER rights",
            ScopedIngestError::GrantIdMismatch => "envelope grant id does not match the grant",
            ScopedIngestError::SecretMismatch => "installed secret does not match the grant",
            ScopedIngestError::TargetNotCovered => "provider outside the grant target scope",
            ScopedIngestError::DecryptFailed => "scoped announcement AEAD open failed",
            ScopedIngestError::DescriptorInvalid => {
                "granted descriptor is not the canonical single-capability shape"
            }
            ScopedIngestError::DescriptorOutsideGrant => {
                "granted descriptor names a capability the grant does not authorize"
            }
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
    // §9 — the LOCAL node's own standing, before any authority-specific work.
    //
    // Every other check here concerns the PUBLISHER. Nothing asked whether the
    // reader is still a member in good standing, so raising a floor for this
    // node stopped everyone else from accepting it while leaving it free to
    // keep ingesting the org's private capability list from every remaining
    // node. Checked at the shared entry rather than inside `verify_owner_ingest`
    // so a granted authority is covered too: a node revoked from its own org
    // should not keep harvesting cross-org discoveries either.

    if let Some(local) = &ctx.local_member {
        let floor = ctx.floors.floor_for(&ctx.local_owner_org, &local.member);
        if local.generation < floor {
            return Err(ScopedIngestError::LocalMembershipRevoked);
        }
    }
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
    // Envelope freshness, under the CLAMPED expiry: an owner-scoped record may
    // never outlive the membership certificate that vouched for its provider.
    let expires_at = effective_expires_at(envelope, &[envelope.owner_cert().not_after]);
    if is_expired_at(expires_at, ctx) {
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
        expires_at,
        provider_cert_generation: envelope.owner_cert().generation,
        // Owner records carry no grant authority.
        grant_signature: None,
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
    // Envelope freshness, under the CLAMPED expiry: a granted record may
    // outlive neither the provider's membership certificate NOR the grant that
    // authorized the discovery. Without the grant bound, a grantor org could
    // mint providers whose records survive the grant itself.
    let expires_at = effective_expires_at(
        envelope,
        &[envelope.owner_cert().not_after, grant.not_after],
    );
    if is_expired_at(expires_at, ctx) {
        return Err(ScopedIngestError::Expired);
    }
    // The AEAD open under the grant's secret key reveals the descriptor.
    let descriptor = envelope
        .open_with(secret.discovery_key())
        .map_err(|_| ScopedIngestError::DecryptFailed)?;
    // The descriptor must name EXACTLY the capability the grant authorizes — a
    // valid-key holder must not advertise an unrelated capability under this
    // grant's confidential-discovery authority (Kyra OA3-4b2 closure).
    descriptor_binds_grant_capability(&descriptor, &grant.capability)?;
    Ok(VerifiedScopedCapability {
        scope: CapabilityAudienceScope::Grant {
            grant_id: grant.grant_id,
            audience_handle: *envelope.audience_handle(),
        },
        provider: envelope.provider().clone(),
        owner_org: *envelope.owner_org(),
        generation: envelope.generation(),
        expires_at,
        provider_cert_generation: envelope.owner_cert().generation,
        grant_signature: Some(grant.signature),
        descriptor,
    })
}

/// The granted-descriptor authority bind (Kyra OA3-4b2 closure): a granted
/// envelope may confidentially advertise ONLY the exact capability its grant
/// authorizes. For the current nRPC-only granted slice the descriptor's canonical
/// current shape is a single capability tag with no metadata; anything else is a
/// [`ScopedIngestError::DescriptorInvalid`] (rather than accepting arbitrary
/// global `CapabilitySet` state), and a tag naming a different capability than the
/// grant is a [`ScopedIngestError::DescriptorOutsideGrant`]. Canonicity is
/// enforced by requiring the descriptor to equal its own re-encoding, so a
/// non-canonical framing cannot slip a second logical form past the shape check.
fn descriptor_binds_grant_capability(
    descriptor: &[u8],
    capability: &CapabilityAuthorityId,
) -> Result<(), ScopedIngestError> {
    let caps = CapabilitySet::from_bytes(descriptor).ok_or(ScopedIngestError::DescriptorInvalid)?;
    // Exactly one tag, no metadata.
    let mut tags = caps.tags.iter();
    let (Some(tag), None) = (tags.next(), tags.next()) else {
        return Err(ScopedIngestError::DescriptorInvalid);
    };
    if !caps.metadata.is_empty() {
        return Err(ScopedIngestError::DescriptorInvalid);
    }
    // Canonical compact encoding: the descriptor must be its own re-encoding.
    if caps.to_bytes_compact() != descriptor {
        return Err(ScopedIngestError::DescriptorInvalid);
    }
    // The single tag must name exactly the grant's authorized capability.
    if &CapabilityAuthorityId::for_tag(&tag.to_string()) != capability {
        return Err(ScopedIngestError::DescriptorOutsideGrant);
    }
    Ok(())
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

/// The expiry this record is actually admitted under: the envelope's own
/// `expires_at` CLAMPED to every credential that authorized it.
///
/// `expires_at` is attacker-chosen — it is a plaintext field the publisher
/// picks. The publisher is expected to bound it (`mesh.rs`:
/// `base_expiry.min(grant.not_after).min(owner_cert.not_after)`), but a rule
/// enforced only on the honest sender is not a rule. An insider, or any
/// provider under a malicious grantor org, could publish `expires_at =
/// u64::MAX` and the record stayed discoverable forever:
///
///  * expiry is not a revocation floor, so a lapsing certificate raises
///    nothing and retracts nothing;
///  * `saturating_add(skew)` keeps `u64::MAX` permanently unexpired;
///  * no sweep, relay check, or query surface re-derived the bound.
///
/// Clamping here makes the stored `expires_at` the true composite bound, which
/// is why no separate window needs to be carried on the record: the credential
/// lifetimes are immutable signed statements, so `min()` of them at ingest is
/// exactly what a query-time re-check would recompute. The existing
/// expiry-safe queries and tombstone horizon then enforce it for free.
fn effective_expires_at(envelope: &ScopedCapabilityAnnouncement, bounds: &[u64]) -> u64 {
    bounds.iter().copied().fold(envelope.expires_at(), u64::min)
}

/// An envelope is expired once the clock passes `expires_at`, with the same
/// skew tolerance applied to certificate/grant windows. Takes the EFFECTIVE
/// expiry (see [`effective_expires_at`]) rather than reading the envelope's
/// claim directly, so a clamped-to-the-past record is refused as born-expired
/// instead of being stored under a lifetime nothing authorized.
fn is_expired_at(effective_expires_at: u64, ctx: &ScopedIngestContext<'_>) -> bool {
    ctx.now_secs >= effective_expires_at.saturating_add(ctx.skew_secs)
}

#[cfg(test)]
mod tests {
    use super::super::org::{
        current_timestamp, OrgKeypair, OrgMembershipCert, OrgRevocationBundle,
    };
    use super::super::org_grant::{CapabilityAuthorityId, GrantRights, GrantTargetScope};
    use super::super::org_scoped_ann::ScopedAnnouncementError;
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
        let credential = OwnerAudienceCredential::generate(org.org_id());
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
            local_member: None,
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

    /// §3 — the publisher's `expires_at` is a CEILING request, not a grant of
    /// lifetime. An owner record may never outlive the membership certificate
    /// that vouched for its provider.
    ///
    /// Before the clamp, `expires_at` was stored verbatim, so an insider could
    /// publish `u64::MAX` and stay discoverable forever: expiry is not a
    /// revocation floor (a lapsing certificate raises nothing),
    /// `saturating_add(skew)` keeps `u64::MAX` unexpired, and no query surface
    /// re-derived the bound. The rule was enforced on the honest SENDER
    /// (`mesh.rs` clamps at emission) and nowhere on the untrusted receiver.
    #[test]
    fn owner_ingest_clamps_expiry_to_the_provider_certificate() {
        let provider = provider_kp();
        let org = OrgKeypair::from_bytes([1u8; 32]);
        let credential = OwnerAudienceCredential::generate(org.org_id());
        // Certificate lapses at 12_000 — well before the envelope's claim.
        let cert_not_after = 12_000;
        let cert = OrgMembershipCert::issue_at(
            &org,
            provider.entity_id().clone(),
            5,
            0,
            cert_not_after,
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
            u64::MAX, // the attacker's claim: never expire
            &descriptor,
        )
        .expect("build owner envelope");

        let floors = OrgRevocationState::empty();
        let authority = AudienceAuthority::owner(org.org_id(), &credential);
        let ctx = owner_ctx(org.org_id(), &floors);
        let verified = verify_scoped_ingest(&envelope, &authority, &ctx).expect("owner ingest");

        assert_eq!(
            verified.expires_at(),
            cert_not_after,
            "the record must be admitted under the CERTIFICATE's lifetime, not \
             the publisher's claim",
        );
        assert_ne!(
            verified.expires_at(),
            u64::MAX,
            "storing the claim verbatim makes the record permanently \
             discoverable after the certificate lapses",
        );
    }

    /// The same rule for a granted record, which has TWO ceilings: the
    /// provider's certificate and the grant that authorized the discovery.
    /// Without the grant bound, a grantor org could mint providers whose
    /// records outlive the grant itself.
    #[test]
    fn granted_ingest_clamps_expiry_to_the_grant() {
        let now = current_timestamp();
        let provider = provider_kp();
        let issuer = OrgKeypair::from_bytes([1u8; 32]); // B
        let a_org = OrgKeypair::from_bytes([9u8; 32]).org_id(); // A
        let (grant, secret) = OrgCapabilityGrant::try_issue(
            &issuer,
            a_org,
            CapabilityAuthorityId::for_tag("nrpc:svc"),
            GrantRights::INVOKE.union(GrantRights::DISCOVER),
            exact_target(&provider),
            3600,
        )
        .expect("issue grant");
        let secret = secret.expect("discover mints a secret");
        // Certificate deliberately OUTLIVES the grant, so the grant is the
        // binding ceiling and the assertion cannot pass by accident on the
        // certificate bound alone.
        let cert = OrgMembershipCert::issue_at(
            &issuer,
            provider.entity_id().clone(),
            5,
            now.saturating_sub(3600),
            now + 100_000,
            0x1234,
        );
        let descriptor = CapabilitySet::new().add_tag("nrpc:svc").to_bytes_compact();
        let envelope = ScopedCapabilityAnnouncement::build_granted(
            &provider,
            issuer.org_id(),
            cert.clone(),
            grant.grant_id,
            secret.audience_handle,
            secret.discovery_key(),
            4,
            u64::MAX, // the attacker's claim
            &descriptor,
        )
        .expect("build granted envelope");

        let floors = OrgRevocationState::empty();
        let authority = AudienceAuthority::granted(&grant, &secret);
        let ctx = granted_ctx(a_org, now, &floors);
        let verified = verify_scoped_ingest(&envelope, &authority, &ctx).expect("granted ingest");

        assert_eq!(
            verified.expires_at(),
            grant.not_after,
            "a granted record must be bounded by the GRANT, which here expires \
             before the provider certificate",
        );
        assert!(
            grant.not_after < cert.not_after,
            "precondition: the grant must be the shorter ceiling, else this \
             test would pass on the certificate bound alone",
        );
    }

    /// A record whose clamp puts it in the PAST is born-expired and refused,
    /// rather than stored under a lifetime nothing authorized.
    #[test]
    fn a_record_clamped_into_the_past_is_refused_as_expired() {
        let provider = provider_kp();
        let org = OrgKeypair::from_bytes([1u8; 32]);
        let credential = OwnerAudienceCredential::generate(org.org_id());
        // Certificate already lapsed relative to NOW (10_000) + SKEW.
        let cert = OrgMembershipCert::issue_at(
            &org,
            provider.entity_id().clone(),
            5,
            0,
            NOW - (SKEW * 2),
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
            u64::MAX,
            &descriptor,
        )
        .expect("build owner envelope");

        let floors = OrgRevocationState::empty();
        let authority = AudienceAuthority::owner(org.org_id(), &credential);
        let ctx = owner_ctx(org.org_id(), &floors);
        // NB: the lapsed certificate is ALSO caught by the membership window
        // check, so this asserts only that the composition refuses — it does
        // not claim the clamp is the sole reason.
        assert!(
            verify_scoped_ingest(&envelope, &authority, &ctx).is_err(),
            "a record with no live credential behind it must not be stored",
        );
    }

    /// §9 — a node its OWN org has revoked stops ingesting scoped
    /// announcements, even though its audience key still decrypts them.
    ///
    /// Every other check in this module concerns the PUBLISHER. Raising a floor
    /// for a member made every OTHER node refuse its announcements and its
    /// invocations — but its copy of `owner-audience.key` and the owner
    /// `audience_handle` are unchanged, so it kept ingesting and storing the
    /// org's full internal private-capability list from every remaining node,
    /// indefinitely. Nothing looked at the reader's own cert.
    ///
    /// Note what this does NOT fix: the revoked node still holds the symmetric
    /// owner key and can decrypt anything it captured off the wire. Closing
    /// that requires the §3.4 hard cutover (redistribute the key to every node
    /// in the org), which no code path triggers. This closes continued
    /// acceptance of FUTURE announcements and gives operators a signal.
    ///
    /// Red-witness: deleting the `local_member` check in `verify_scoped_ingest`
    /// makes the floored ingest succeed.
    #[test]
    fn a_locally_revoked_node_stops_ingesting() {
        let f = owner_fixture();
        // The READER is a different node from the publisher, so a floor on it
        // cannot trip the publisher's `MembershipRevoked` check. Without that
        // separation the test would pass identically with the new check
        // deleted — the publisher gate would refuse for its own reasons.
        let local = EntityKeypair::from_bytes([0x5Au8; 32]).entity_id().clone();
        assert_ne!(
            &local,
            f.provider.entity_id(),
            "the reader must be distinct from the publisher",
        );
        let authority = AudienceAuthority::owner(f.org.org_id(), &f.credential);
        fn ctx_at<'a>(
            org: OrgId,
            member: &EntityId,
            floors: &'a OrgRevocationState,
            generation: u32,
        ) -> ScopedIngestContext<'a> {
            ScopedIngestContext {
                local_owner_org: org,
                floors,
                now_secs: NOW,
                skew_secs: SKEW,
                local_member: Some(LocalMemberStanding {
                    member: member.clone(),
                    generation,
                }),
            }
        }
        let org_id = f.org.org_id();

        // Positive control: no floor, ingest succeeds. Without this a later
        // refusal could be an artifact of the fixture rather than the floor.
        let no_floors = OrgRevocationState::empty();
        assert!(
            verify_scoped_ingest(
                &f.envelope,
                &authority,
                &ctx_at(org_id, &local, &no_floors, 3)
            )
            .is_ok(),
            "an in-good-standing reader ingests normally",
        );

        // The org raises a floor ABOVE this reader's own generation — signed by
        // the org root, exactly as `net org issue-floors` would.
        let mut floors_map = std::collections::BTreeMap::new();
        floors_map.insert(local.clone(), 4u32);
        let bundle = OrgRevocationBundle::try_issue(&f.org, &floors_map).expect("bundle");
        let mut floored = OrgRevocationState::empty();
        floored.merge_bundle(&bundle);

        assert_eq!(
            verify_scoped_ingest(
                &f.envelope,
                &authority,
                &ctx_at(org_id, &local, &floored, 3)
            )
            .unwrap_err(),
            ScopedIngestError::LocalMembershipRevoked,
            "a locally revoked reader must refuse the ingest",
        );

        // Boundary: a cert AT the floor is still alive (`generation < floor`),
        // matching every other floor comparison in the tree.
        assert!(
            verify_scoped_ingest(
                &f.envelope,
                &authority,
                &ctx_at(org_id, &local, &floored, 4)
            )
            .is_ok(),
            "a cert AT the floor is not revoked",
        );
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
            local_member: None,
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
        // The descriptor is the canonical single-capability shape naming exactly
        // the grant's capability (Kyra OA3-4b2 closure): the granted-ingest path
        // now binds the descriptor to `grant.capability`.
        let descriptor = CapabilitySet::new().add_tag("nrpc:svc").to_bytes_compact();
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
            local_member: None,
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
        // The verified record retains the exact grant signature (Kyra OA3-4b2
        // closure) for exact-authority query currentness.
        assert_eq!(verified.grant_signature(), Some(&f.grant.signature));
    }

    /// Kyra OA3-4b2 closure: the granted descriptor is bound to the grant's
    /// capability. A valid-key holder may advertise ONLY the exact capability the
    /// grant authorizes, in the canonical single-tag shape.
    #[test]
    fn granted_ingest_binds_descriptor_to_the_grant_capability() {
        let p = provider_kp();
        let f = granted_fixture(exact_target(&p));
        let floors = OrgRevocationState::empty();
        let authority = AudienceAuthority::granted(&f.grant, &f.secret);
        let ctx = granted_ctx(f.a_org, f.now + 5, &floors);

        // Build a granted envelope over the fixture's grant/secret with a chosen
        // descriptor — only the descriptor varies.
        let build = |descriptor: &[u8]| {
            let cert = OrgMembershipCert::issue_at(
                &f.issuer,
                p.entity_id().clone(),
                5,
                f.now.saturating_sub(3600),
                f.now + 3600,
                0x1234,
            );
            ScopedCapabilityAnnouncement::build_granted(
                &p,
                f.issuer.org_id(),
                cert,
                f.grant.grant_id,
                f.secret.audience_handle,
                f.secret.discovery_key(),
                4,
                f.now + 3600,
                descriptor,
            )
            .expect("build")
        };

        // C1 (the grant's capability) → accepted.
        let c1 = CapabilitySet::new().add_tag("nrpc:svc").to_bytes_compact();
        assert!(verify_scoped_ingest(&build(&c1), &authority, &ctx).is_ok());

        // C2 (a different capability) → refused, outside the grant.
        let c2 = CapabilitySet::new()
            .add_tag("nrpc:other")
            .to_bytes_compact();
        assert_eq!(
            verify_scoped_ingest(&build(&c2), &authority, &ctx).unwrap_err(),
            ScopedIngestError::DescriptorOutsideGrant
        );

        // C1 + C2 (two tags — not the canonical single-capability shape) → refused.
        let c1c2 = CapabilitySet::new()
            .add_tag("nrpc:svc")
            .add_tag("nrpc:other")
            .to_bytes_compact();
        assert_eq!(
            verify_scoped_ingest(&build(&c1c2), &authority, &ctx).unwrap_err(),
            ScopedIngestError::DescriptorInvalid
        );

        // A descriptor carrying metadata is not the canonical shape → refused.
        let with_meta = CapabilitySet::new()
            .add_tag("nrpc:svc")
            .with_metadata("k".to_string(), "v".to_string())
            .to_bytes_compact();
        assert_eq!(
            verify_scoped_ingest(&build(&with_meta), &authority, &ctx).unwrap_err(),
            ScopedIngestError::DescriptorInvalid
        );

        // Malformed (not a canonical CapabilitySet) → refused.
        assert_eq!(
            verify_scoped_ingest(&build(&[0xFFu8; 7]), &authority, &ctx).unwrap_err(),
            ScopedIngestError::DescriptorInvalid
        );
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
        let credential = OwnerAudienceCredential::generate(f.issuer.org_id());
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

    /// OA3-6 exit gate (§3.5 "expired-grant holder cannot decrypt a NEW audience's
    /// envelopes — per-grant key independence"): a rotation replaces an EXPIRED
    /// grant G1 (key K1) with a freshly-issued successor G2 (key K2) over the same
    /// capability/provider. A former grantee holding only K1 cannot decrypt G2's
    /// envelope AT THE AEAD BOUNDARY, and the expired G1 grant is no longer
    /// admissible for discovery. Deterministic windows (no sleep) via `issue_at`.
    #[test]
    fn an_expired_grants_key_cannot_decrypt_a_freshly_issued_successors_envelope() {
        let now = current_timestamp();
        let provider = provider_kp();
        let issuer = OrgKeypair::from_bytes([1u8; 32]); // B
        let a_org = OrgKeypair::from_bytes([9u8; 32]).org_id(); // grantee A
        let capability = CapabilityAuthorityId::for_tag("nrpc:svc");
        let target = GrantTargetScope::ExactNode(provider.entity_id().clone());
        // A single provider cert valid across `now`.
        let cert = OrgMembershipCert::issue_at(
            &issuer,
            provider.entity_id().clone(),
            5,
            now.saturating_sub(3600),
            now + 3600,
            0x1234,
        );
        // The descriptor names exactly the granted capability (closure-1 bind).
        let descriptor = CapabilitySet::new().add_tag("nrpc:svc").to_bytes_compact();

        // G1 — expired an hour ago (fresh audience material via mint).
        let (secret1, binding1) = OrgAudienceSecret::mint([0x11u8; 32]);
        let g1 = OrgCapabilityGrant::issue_at(
            &issuer,
            [0x11u8; 32],
            a_org,
            capability,
            GrantRights::DISCOVER,
            target.clone(),
            Some(binding1),
            now.saturating_sub(7200),
            now.saturating_sub(3600),
            0xA1,
        );
        // G2 — the currently-valid successor with FRESH audience material.
        let (secret2, binding2) = OrgAudienceSecret::mint([0x22u8; 32]);
        let g2 = OrgCapabilityGrant::issue_at(
            &issuer,
            [0x22u8; 32],
            a_org,
            capability,
            GrantRights::DISCOVER,
            target.clone(),
            Some(binding2),
            now.saturating_sub(3600),
            now + 3600,
            0xA2,
        );

        // Distinct grant id, handle, and key — the successor is an independent
        // revocation + audience boundary.
        assert_ne!(g1.grant_id, g2.grant_id);
        assert_ne!(secret1.audience_handle, secret2.audience_handle);
        assert_ne!(secret1.discovery_key(), secret2.discovery_key());

        // G2's envelope (its own expiry unexpired so only the GRANT window matters
        // for the G1 refusal below).
        let g2_env = ScopedCapabilityAnnouncement::build_granted(
            &provider,
            issuer.org_id(),
            cert.clone(),
            g2.grant_id,
            secret2.audience_handle,
            secret2.discovery_key(),
            4,
            now + 3600,
            &descriptor,
        )
        .expect("build G2 envelope");

        // AEAD-boundary independence: G2 opens under K2 but NOT under K1 — call
        // `open_with` DIRECTLY so the failure is the AEAD open, not an earlier
        // handle/grant-id mismatch in the ingest pipeline (Kyra's precise witness).
        assert_eq!(
            g2_env
                .open_with(secret2.discovery_key())
                .expect("K2 opens G2"),
            descriptor
        );
        assert_eq!(
            g2_env.open_with(secret1.discovery_key()).unwrap_err(),
            ScopedAnnouncementError::SealOpenFailed,
            "the former grant's key cannot decrypt the successor's envelope",
        );

        // G2 verifies + ingests successfully under G2 + K2.
        let floors = OrgRevocationState::empty();
        let g2_authority = AudienceAuthority::granted(&g2, &secret2);
        let ctx = granted_ctx(a_org, now, &floors);
        assert!(verify_scoped_ingest(&g2_env, &g2_authority, &ctx).is_ok());

        // The EXPIRED G1 grant is no longer admissible for discovery. Its own
        // envelope's expiry is far future, so the refusal comes from the GRANT
        // validity check (which runs before envelope-expiry) — GrantInvalid, not
        // Expired. Pin the actual variant, not the one the prose might suggest.
        let g1_env = ScopedCapabilityAnnouncement::build_granted(
            &provider,
            issuer.org_id(),
            cert,
            g1.grant_id,
            secret1.audience_handle,
            secret1.discovery_key(),
            4,
            now + 3600,
            &descriptor,
        )
        .expect("build G1 envelope");
        let g1_authority = AudienceAuthority::granted(&g1, &secret1);
        assert_eq!(
            verify_scoped_ingest(&g1_env, &g1_authority, &ctx).unwrap_err(),
            ScopedIngestError::GrantInvalid,
        );
    }
}
