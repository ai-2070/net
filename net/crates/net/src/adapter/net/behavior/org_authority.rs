//! Node ownership scaffolding — OA-1 §1.2 of
//! `docs/plans/ORG_CAPABILITY_AUTH_PLAN.md`.
//!
//! `net node adopt` provisions a node with THREE separately
//! versioned files in its authority directory (separately
//! versioned because visibility key material is not membership and
//! must not ride certificate-renewal semantics):
//!
//! ```text
//! owner-membership.json      // NodeAuthorityConfig + owner_cert
//! owner-audience.key         // owner audience handle + key
//! revocation-state.json      // persisted floor maxima (§1.5)
//! ```
//!
//! **One node, one owner.** Adoption refuses a certificate from a
//! different organization while an owner is already installed —
//! cross-org access is a B→A grant (OA-2), never co-membership.
//! Renewal (same org, fresh cert) overwrites the membership file
//! and touches nothing else.
//!
//! **Loud startup self-verification.** [`NodeAuthority::open`]
//! refuses to produce a value unless every file loads strictly AND
//! the certificate verifies for THIS node's entity id, inside its
//! window, at or above the persisted revocation floor. A node
//! never runs with ownership it cannot prove.
//!
//! **The owner audience credential grants only knowledge.** The
//! `owner-audience.key` material scaffolded here is consumed by
//! OA-3's owner-scoped discovery; holding it never authorizes
//! invocation. It deliberately implements NO serde / postcard
//! traits — the config-file encoding is the explicit codec below
//! ([`OwnerAudienceCredential::encode_config`]), and raw key
//! material never rides a wire object (plan §Deliberately-NOT-in-v1).
//! At-rest protection is a 0600 plain file, matching the repo's
//! `EntityKeypair` storage convention (plan Q2).
//!
//! **Local filesystem threat boundary (Gate-1).** The authority directory
//! is a TRUSTED local security boundary. Concurrent mutation by another
//! process running with write access to it — replacing directory entries or
//! the stable `.lock` sidecar mid-transaction — is explicitly OUT OF SCOPE:
//! a same-account attacker who can write into the authority directory can
//! already attack the surrounding configuration and process state, so
//! hardening one sidecar protocol against it while the rest of the local
//! boundary trusts the account would be incoherent. Supported Net writers
//! never unlink or replace the sidecar. R3-3 (`OrgRevocationStore::apply_bundle`)
//! detects sidecar replacement occurring BETWEEN legitimate transactions and
//! common operator/startup mistakes; it does not claim to protect against an
//! actor concurrently mutating directory entries DURING a transaction.
//! `ensure_secure_authority_dir` enforces the boundary at its edges. The
//! supplied path is first normalized ONCE (`normalize_authority_dir`): a
//! relative path is resolved against the current directory (a bare relative
//! name has an empty parent, so its ancestor chain would otherwise go
//! unchecked) and a trailing separator is stripped (so `symlink_metadata` on a
//! final symlink reports the link, not its followed target). On Unix
//! the resolved ancestor chain is checked (no group/other-writable, non-sticky
//! parent through which another account could swap the directory's entry), a
//! new authority directory is created no broader than 0700 (umask) and then
//! tightened to exactly 0700, and an existing one must be owned by the current
//! user and not group/other-writable. On Windows every missing component is
//! created ATOMICALLY with a protected, owner-only DACL (`CreateDirectoryW` +
//! `SECURITY_ATTRIBUTES`, no post-creation window), and a pre-existing directory
//! is re-validated against its BINARY DACL and fails closed unless every
//! write-capable ACE is a trusted principal. The user account, SYSTEM, and local
//! administrators are trusted principals.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::org::{current_timestamp, OrgError, OrgId, OrgMembershipCert, OrgRevocationBundle};
use super::org_revocation::{
    open_regular_nofollow, write_atomic, OrgRevocationError, OrgRevocationState,
    OrgRevocationStore, ProvisioningExpectation,
};
use crate::adapter::net::identity::EntityId;

/// File name of the membership config inside the authority dir.
pub const OWNER_MEMBERSHIP_FILE: &str = "owner-membership.json";
/// File name of the owner audience credential inside the authority
/// dir.
pub const OWNER_AUDIENCE_FILE: &str = "owner-audience.key";
/// File name of the persisted revocation maxima inside the
/// authority dir (see `org_revocation.rs`).
pub const REVOCATION_STATE_FILE: &str = "revocation-state.json";

/// Format version of `owner-membership.json`.
pub const NODE_AUTHORITY_CONFIG_VERSION: u32 = 1;

/// Format version byte of `owner-audience.key`.
///
/// v2 added the owning `OrgId` ahead of the handle, so the file states which
/// org it belongs to and `adopt` can refuse to carry it across an ownership
/// change (§2). v1 files are refused rather than migrated: a v1 file cannot
/// say which org it was minted for, which is precisely the ambiguity the
/// version exists to remove, and guessing "it belongs to whoever is adopting
/// now" would reinstate the bug. Re-provision the node.
pub const OWNER_AUDIENCE_KEY_VERSION: u8 = 2;

/// The node's ownership statement: which single organization owns
/// this node, proven by `owner_cert`. Serialized as
/// `owner-membership.json` (certs render as hex of their canonical
/// wire bytes).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeAuthorityConfig {
    /// Format version — unknown versions are a loud failure.
    pub version: u32,
    /// The owning organization. Redundant with
    /// `owner_cert.org_id` by construction; the load path verifies
    /// the two agree so a hand-edited file can't quietly claim one
    /// org while carrying another's cert.
    pub owner_org: OrgId,
    /// This node's membership certificate, issued by `owner_org`
    /// to this node's `EntityId`.
    pub owner_cert: OrgMembershipCert,
    /// The clock-skew tolerance (seconds) the adoption ceremony
    /// accepted, PERSISTED so production startup verifies with the
    /// SAME setting (review-9: `net node adopt --skew-secs N`
    /// succeeding and `MeshNode::new` refusing with zero skew was
    /// a ceremony/startup mismatch). `#[serde(default)]` keeps
    /// pre-review-9 files loading with strict 0. The token-module
    /// ceiling is still enforced at every verification, so a
    /// hand-edited oversized value refuses loudly.
    #[serde(default)]
    pub verification_skew_secs: u64,
}

impl NodeAuthorityConfig {
    /// Build a config from a certificate (the owner org is the
    /// cert's issuer) and the ceremony's accepted skew.
    pub fn new(owner_cert: OrgMembershipCert, verification_skew_secs: u64) -> Self {
        Self {
            version: NODE_AUTHORITY_CONFIG_VERSION,
            owner_org: owner_cert.org_id,
            owner_cert,
            verification_skew_secs,
        }
    }

    /// Structural/authenticity verification WITHOUT wall-clock
    /// bounds or revocation floors: format version, declared-owner
    /// consistency (`owner_org == owner_cert.org_id`), certificate
    /// signature + window shape + TTL ceiling, and the
    /// member-binding to `local_entity`.
    ///
    /// This is the check an EXISTING membership must pass before it
    /// may act as the one-node/one-owner lock during re-adoption
    /// (review-9): an inconsistent or forged file must never become
    /// an ownership-transfer mechanism. Wall-clock expiry and
    /// floors are deliberately NOT gates here — an authentic but
    /// expired or revoked membership is still authentic evidence of
    /// WHO owns the node, and renewal after expiry / after a floor
    /// raise is the standard recovery ceremony.
    pub fn verify_binding(&self, local_entity: &EntityId) -> Result<(), OrgAuthorityError> {
        if self.version != NODE_AUTHORITY_CONFIG_VERSION {
            return Err(OrgAuthorityError::UnsupportedVersion {
                path: OWNER_MEMBERSHIP_FILE.to_string(),
                found: self.version,
            });
        }
        if self.owner_org != self.owner_cert.org_id {
            return Err(OrgAuthorityError::OwnerOrgMismatch {
                declared: self.owner_org,
                cert_org: self.owner_cert.org_id,
            });
        }
        if self.owner_cert.member != *local_entity {
            return Err(OrgAuthorityError::CertNotForThisNode {
                cert_member: self.owner_cert.member.clone(),
                local_entity: local_entity.clone(),
            });
        }
        self.owner_cert
            .verify()
            .map_err(OrgAuthorityError::CertInvalid)
    }

    /// Self-verify this config for the local node, in the locked
    /// order: structural binding ([`Self::verify_binding`]) →
    /// wall-clock validity under the PERSISTED skew
    /// (`verification_skew_secs`, ceiling-enforced) → revocation
    /// floor against the supplied state.
    ///
    /// Takes an [`OrgRevocationState`] (not the store) so the
    /// adoption ceremony can verify against CANDIDATE floors —
    /// persisted maxima plus a not-yet-applied operator bundle —
    /// before any durable state changes (review-8 §7).
    pub fn self_verify(
        &self,
        local_entity: &EntityId,
        floors: &OrgRevocationState,
    ) -> Result<(), OrgAuthorityError> {
        self.self_verify_at(local_entity, floors, current_timestamp())
    }

    /// Explicit-time variant of [`Self::self_verify`] (AV-6 item 6):
    /// the wall-clock validity is checked against the caller-supplied
    /// `now_secs` rather than a fresh `current_timestamp()` read. The
    /// admission path captures ONE [`ClockSample`] and threads its
    /// `wall_secs()` through both the provider owner-cert check here
    /// and the caller credential checks, so a wall-clock step between
    /// the two can never open a window where the provider verifies
    /// against a different instant than the caller.
    ///
    /// [`ClockSample`]: super::admission_clock::ClockSample
    pub fn self_verify_at(
        &self,
        local_entity: &EntityId,
        floors: &OrgRevocationState,
        now_secs: u64,
    ) -> Result<(), OrgAuthorityError> {
        self.verify_binding(local_entity)?;
        self.owner_cert
            .is_valid_at_with_skew(now_secs, self.verification_skew_secs)
            .map_err(OrgAuthorityError::CertInvalid)?;
        let floor = floors.floor_for(&self.owner_cert.org_id, &self.owner_cert.member);
        if self.owner_cert.generation < floor {
            return Err(OrgAuthorityError::CertBelowFloor {
                generation: self.owner_cert.generation,
                floor,
            });
        }
        Ok(())
    }
}

/// The owner audience credential — a random 32-byte handle plus a
/// 32-byte discovery key, scaffolded at adopt time and consumed by
/// OA-3's owner-scoped discovery.
///
/// Grants ONLY knowledge (the ability to decrypt owner-scoped
/// announcements once OA-3 lands); never invocation authority.
///
/// Deliberately implements **no serde / postcard traits** — the
/// on-disk form is the explicit codec below, and the raw key must
/// never become embeddable in a wire object by a `derive` slipping
/// in. Rotation is config management (install new file, restart or
/// reload), not a mesh key-epoch protocol.
pub struct OwnerAudienceCredential {
    /// The org this credential belongs to.
    ///
    /// Bound into the on-disk codec so a credential can never be silently
    /// carried across an ownership change. Without it the file was three
    /// anonymous fields, and `adopt` — which preserves an existing
    /// `owner-audience.key` unconditionally — would happily seal a NEW org's
    /// private capability catalog under the PREVIOUS org's key. That key is
    /// org-wide and distributed to every node in that org by design, so every
    /// current and former node of the old org could read the new org's
    /// owner-scoped announcements straight off the wire.
    ///
    /// It is not a secret (it is the org's public id) and it is not
    /// authenticated by itself — it is a consistency binding, checked against
    /// the membership certificate at load. Forging it buys nothing: the check
    /// it defeats is the one that would have refused, and the resulting
    /// credential still cannot open anything the real audience key protects.
    pub owner_org: OrgId,
    /// Public-ish routing handle for the owner audience. Random;
    /// reveals nothing but linkage.
    pub audience_handle: [u8; 32],
    /// The audience decryption key. SECRET — 0600 at rest, never
    /// on the wire, never in a proof, never in `Debug` output.
    discovery_key: [u8; 32],
}

/// Type-level assertion (mirroring [`OrgAudienceSecret`], plan v1.3
/// carry-forward): `OwnerAudienceCredential` must never implement
/// `serde::Serialize`, so the raw owner discovery key can never become a
/// member of any wire object via a `derive` slipping in. If it ever gains
/// `Serialize`, the blanket impl below becomes ambiguous with the `()` impl
/// and this constant fails to compile (the inlined
/// `static_assertions::assert_not_impl_any` mechanism — the review-7 witness
/// covers BOTH the granted and owner secret types).
///
/// [`OrgAudienceSecret`]: super::org_grant::OrgAudienceSecret
const _: fn() = || {
    trait AmbiguousIfSerialize<A> {
        fn guard() {}
    }
    impl<T: ?Sized> AmbiguousIfSerialize<()> for T {}
    #[allow(dead_code)]
    struct IsSerialize;
    impl<T: ?Sized + serde::Serialize> AmbiguousIfSerialize<IsSerialize> for T {}
    let _ = <OwnerAudienceCredential as AmbiguousIfSerialize<_>>::guard;
};

impl OwnerAudienceCredential {
    /// Encoded size of the explicit config codec:
    /// version byte ‖ owner org (32) ‖ handle (32) ‖ key (32).
    pub const ENCODED_SIZE: usize = 1 + 32 + 32 + 32;

    /// Generate a fresh credential for `owner_org`. `getrandom` failure aborts
    /// — a predictable discovery key would let anyone decrypt owner-scoped
    /// announcements (same rationale as `EntityKeypair::generate`).
    pub fn generate(owner_org: OrgId) -> Self {
        let mut bytes = [0u8; 64];
        if let Err(e) = getrandom::fill(&mut bytes) {
            eprintln!(
                "FATAL: OwnerAudienceCredential getrandom failure ({e:?}); aborting to avoid predictable audience key"
            );
            std::process::abort();
        }
        let mut audience_handle = [0u8; 32];
        let mut discovery_key = [0u8; 32];
        audience_handle.copy_from_slice(&bytes[..32]);
        discovery_key.copy_from_slice(&bytes[32..]);
        // Zeroize the staging buffer — volatile writes prevent
        // optimizer elision.
        for byte in bytes.iter_mut() {
            // SAFETY: `byte` is a valid mutable reference into
            // `bytes` for this iteration, which is all
            // `ptr::write_volatile` requires.
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
        Self {
            owner_org,
            audience_handle,
            discovery_key,
        }
    }

    /// The audience decryption key. Deliberately a borrowing
    /// accessor rather than a public field so every use site is
    /// greppable.
    pub fn discovery_key(&self) -> &[u8; 32] {
        &self.discovery_key
    }

    /// Explicit config-file codec (NOT a wire format):
    /// `version ‖ owner_org ‖ handle ‖ key`, exactly
    /// [`Self::ENCODED_SIZE`] bytes.
    pub fn encode_config(&self) -> [u8; Self::ENCODED_SIZE] {
        let mut buf = [0u8; Self::ENCODED_SIZE];
        buf[0] = OWNER_AUDIENCE_KEY_VERSION;
        buf[1..33].copy_from_slice(self.owner_org.as_bytes());
        buf[33..65].copy_from_slice(&self.audience_handle);
        buf[65..97].copy_from_slice(&self.discovery_key);
        buf
    }

    /// Strict inverse of [`Self::encode_config`]: exact length,
    /// known version byte — anything else is corruption, loudly.
    #[expect(
        clippy::unwrap_used,
        reason = "length checked to be exactly ENCODED_SIZE above; fixed slices convert infallibly"
    )]
    pub fn decode_config(bytes: &[u8]) -> Result<Self, OrgAuthorityError> {
        if bytes.len() != Self::ENCODED_SIZE {
            return Err(OrgAuthorityError::CorruptFile {
                path: OWNER_AUDIENCE_FILE.to_string(),
                detail: format!(
                    "expected exactly {} bytes, found {}",
                    Self::ENCODED_SIZE,
                    bytes.len()
                ),
            });
        }
        if bytes[0] != OWNER_AUDIENCE_KEY_VERSION {
            return Err(OrgAuthorityError::UnsupportedVersion {
                path: OWNER_AUDIENCE_FILE.to_string(),
                found: bytes[0] as u32,
            });
        }
        let owner_org: [u8; 32] = bytes[1..33].try_into().unwrap();
        Ok(Self {
            owner_org: OrgId(owner_org),
            audience_handle: bytes[33..65].try_into().unwrap(),
            discovery_key: bytes[65..97].try_into().unwrap(),
        })
    }
}

impl std::fmt::Debug for OwnerAudienceCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OwnerAudienceCredential")
            .field("audience_handle", &hex::encode(self.audience_handle))
            .field("discovery_key", &"[REDACTED]")
            .finish()
    }
}

impl Drop for OwnerAudienceCredential {
    fn drop(&mut self) {
        // Zeroize the key on drop (mirroring `OrgAudienceSecret`) — volatile
        // writes prevent optimizer elision so a lingering copy is not left in
        // freed memory.
        for byte in self.discovery_key.iter_mut() {
            // SAFETY: `byte` is a valid mutable reference into the owned array
            // for this iteration, which is all `ptr::write_volatile` requires.
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
    }
}

/// RAII volatile-scrub for a transient key-bearing byte buffer (the file-backed
/// owner audience material the ceremony reads/writes): zeroes its contents on
/// EVERY exit — normal return, `?`, or unwind — so a copy of the owner discovery
/// key never lingers in freed memory (Kyra OA3 closure). Volatile writes prevent
/// optimizer elision, matching the crate's hand-rolled scrub convention (no
/// `zeroize` crate for these buffers).
struct ScrubbedBytes(Vec<u8>);

impl ScrubbedBytes {
    fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

impl Drop for ScrubbedBytes {
    fn drop(&mut self) {
        for byte in self.0.iter_mut() {
            // SAFETY: `byte` is a valid mutable reference into the owned Vec.
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
    }
}

/// Errors from adoption / startup authority loading.
#[derive(Debug)]
pub enum OrgAuthorityError {
    /// A required authority file is missing at startup. Only
    /// `net node adopt` creates the files.
    MissingFile {
        /// The expected path.
        path: String,
    },
    /// An authority file exists but cannot be trusted.
    CorruptFile {
        /// The offending file.
        path: String,
        /// What failed.
        detail: String,
    },
    /// A file's declared format version is unknown to this build.
    UnsupportedVersion {
        /// The offending file.
        path: String,
        /// The declared version.
        found: u32,
    },
    /// `owner-membership.json` declares one org but carries a
    /// certificate issued by another.
    OwnerOrgMismatch {
        /// The `owner_org` the file declares.
        declared: OrgId,
        /// The org that actually signed the cert.
        cert_org: OrgId,
    },
    /// The certificate vouches for a different entity than this
    /// node.
    CertNotForThisNode {
        /// Who the cert names.
        cert_member: EntityId,
        /// Who this node is.
        local_entity: EntityId,
    },
    /// The certificate failed signature / TTL / window checks.
    CertInvalid(OrgError),
    /// The certificate's generation is below the persisted
    /// revocation floor — it has been revoked.
    CertBelowFloor {
        /// The cert's generation.
        generation: u32,
        /// The persisted floor.
        floor: u32,
    },
    /// Adoption refused: the node already belongs to a different
    /// organization (one node, one owner — cross-org access is a
    /// grant, never co-membership).
    AlreadyOwned {
        /// The currently installed owner.
        existing: OrgId,
        /// The org the new certificate names.
        requested: OrgId,
    },
    /// An adopt-time floor bundle was signed by an organization
    /// other than the candidate owner. Signed receipt is not trust
    /// establishment (review-8 §6): the owner-adoption ceremony
    /// tracks exactly one root, and foreign relying-party floors
    /// need their own explicitly pinned surface in a later phase.
    ForeignFloorBundle {
        /// The org that signed the supplied bundle.
        bundle_org: OrgId,
        /// The candidate owner org.
        owner_org: OrgId,
    },
    /// `owner-audience.key` is group/other-readable. Creation-time
    /// 0600 is insufficient — config management, copying, or manual
    /// edits can weaken it later — so both startup and re-adoption
    /// re-check and refuse (review-8 §10).
    PermissiveAudienceFile {
        /// The key file's path.
        path: String,
        /// The observed mode bits.
        mode: u32,
    },
    /// The Windows counterpart of [`Self::PermissiveAudienceFile`]:
    /// `owner-audience.key`'s OWN security descriptor grants access to an
    /// untrusted principal, or is owned by one.
    ///
    /// The Unix side has re-checked the key file's mode at every startup and
    /// re-adoption since review-8 §10, with the stated rationale that
    /// creation-time 0600 is insufficient because config management, copying,
    /// or manual edits weaken it later. Windows had no analog at all: the
    /// directory's descriptor was validated and the FILE's never was, so an
    /// explicit non-inherited ACE on the key itself — an `icacls` mistake, a
    /// restore tool, or the file arriving from a share during the org-wide key
    /// distribution this design REQUIRES — was invisible to every check (§11).
    PermissiveAudienceAcl {
        /// The key file's path.
        path: String,
        /// What made the descriptor unacceptable.
        reason: String,
    },
    /// The authority directory is not a safe local security boundary
    /// (Gate-1): on Unix it is owned by another user, is group/other-
    /// writable, or is not a directory. The authority directory is a
    /// TRUSTED local boundary — concurrent mutation by another process with
    /// write access to it is explicitly out of scope — but a wrong-owner or
    /// world-writable directory means that trust does not hold, so adoption
    /// and startup refuse loudly rather than provisioning or reading secrets
    /// inside it.
    InsecureAuthorityDir {
        /// The authority directory path.
        path: String,
        /// Why the directory was refused.
        reason: String,
    },
    /// Owner-cert emission was enabled on a node with no installed
    /// [`NodeAuthority`]. Emission is sourced EXCLUSIVELY from the
    /// loaded, self-verified authority (review-8 §3) — there is no
    /// raw-certificate bypass.
    NoAuthorityInstalled,
    /// The post-ceremony reload did not match the candidate this
    /// invocation installed (review-9): another writer raced the
    /// ceremony. The caller must not treat its candidate as
    /// installed.
    CeremonyRaced {
        /// What differed.
        detail: String,
    },
    /// Filesystem failure.
    Io {
        /// The path involved.
        path: String,
        /// The underlying error.
        reason: String,
    },
    /// The revocation store failed to initialize or open (its own
    /// loud error, wrapped).
    Revocation(OrgRevocationError),
}

impl std::fmt::Display for OrgAuthorityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingFile { path } => write!(
                f,
                "authority file missing: {path}; run `net node adopt` to provision"
            ),
            Self::CorruptFile { path, detail } => {
                write!(f, "authority file corrupt: {path} ({detail})")
            }
            Self::UnsupportedVersion { path, found } => {
                write!(f, "authority file {path} has unsupported version {found}")
            }
            Self::OwnerOrgMismatch { declared, cert_org } => write!(
                f,
                "owner-membership.json declares org {declared} but its certificate was issued by {cert_org}"
            ),
            Self::CertNotForThisNode {
                cert_member,
                local_entity,
            } => write!(
                f,
                "owner certificate names {cert_member}, but this node is {local_entity}"
            ),
            Self::CertInvalid(e) => write!(f, "owner certificate invalid: {e}"),
            Self::CertBelowFloor { generation, floor } => write!(
                f,
                "owner certificate generation {generation} is below the persisted revocation floor {floor}"
            ),
            Self::AlreadyOwned {
                existing,
                requested,
            } => write!(
                f,
                "node already owned by org {existing}; refusing adoption by {requested} \
                 (one node one owner). To transfer, remove the WHOLE authority directory \
                 and re-adopt — deleting only the membership file leaves the previous \
                 org's audience key in place, and the new org's private capabilities \
                 would be sealed under a key that org does not control"
            ),
            Self::ForeignFloorBundle {
                bundle_org,
                owner_org,
            } => write!(
                f,
                "floor bundle signed by org {bundle_org} but the candidate owner is \
                 {owner_org}; the adoption ceremony tracks only the owner root"
            ),
            Self::PermissiveAudienceFile { path, mode } => write!(
                f,
                "owner audience key {path} has permissive mode {mode:#o} (group/other \
                 readable); tighten to 0600 — refusing to treat a possibly-disclosed \
                 audience key as installed"
            ),
            Self::PermissiveAudienceAcl { path, reason } => write!(
                f,
                "owner audience key {path} has a permissive ACL ({reason}); refusing to \
                 treat a possibly-disclosed audience key as installed"
            ),
            Self::InsecureAuthorityDir { path, reason } => write!(
                f,
                "authority directory {path} is not a trusted local boundary: {reason}; \
                 it must be a directory owned by the current user and not group/other-\
                 writable (owner-only 0700) — refusing to provision or open authority \
                 state inside it"
            ),
            Self::NoAuthorityInstalled => write!(
                f,
                "owner-cert emission requires an installed node authority; run \
                 `net node adopt` and configure the authority directory first"
            ),
            Self::CeremonyRaced { detail } => write!(
                f,
                "adoption ceremony raced a concurrent writer ({detail}); the candidate \
                 authority is NOT installed"
            ),
            Self::Io { path, reason } => write!(f, "authority I/O at {path}: {reason}"),
            Self::Revocation(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for OrgAuthorityError {}

impl From<OrgRevocationError> for OrgAuthorityError {
    fn from(e: OrgRevocationError) -> Self {
        Self::Revocation(e)
    }
}

/// The node's loaded, self-verified authority: membership + owner
/// audience credential + persisted revocation store.
pub struct NodeAuthority {
    /// The verified ownership statement.
    pub config: NodeAuthorityConfig,
    /// The owner audience credential (knowledge only; OA-3
    /// consumes it).
    pub audience: OwnerAudienceCredential,
    /// The persisted revocation maxima (feeds announcement ingest
    /// via `MeshNode::install_org_revocation_store`).
    pub revocation: Arc<OrgRevocationStore>,
}

impl NodeAuthority {
    /// `net node adopt` — provision `dir` with the three authority
    /// files under the review-8 ceremony order: EVERYTHING is
    /// validated before anything durable changes, floors advance
    /// before membership, and membership publishes LAST — a failed
    /// re-adoption never advertises a renewal whose supporting
    /// state failed validation.
    ///
    /// ```text
    /// acquire the CEREMONY LOCK (authority.lock — serializes every
    ///    adoption on this directory end to end, review-9)
    /// → validate existing membership (STRUCTURALLY VERIFIED before
    ///    it may act as the one-node/one-owner lock — an
    ///    inconsistent file is never an ownership-transfer
    ///    mechanism, review-9)
    /// → validate existing audience credential (codec + 0600 mode)
    /// → strictly load persisted floor maxima (no creation yet)
    /// → verify the optional owner floor bundle
    ///    (signature + bundle.org_id == candidate owner — signed
    ///     receipt is never trust establishment)
    /// → compute CANDIDATE floors (persisted ∪ bundle)
    /// → verify the candidate certificate against candidate state
    /// → create/open the revocation store; durably apply the bundle
    /// → FINAL PHASE under the revocation-state lock: re-verify the
    ///    cert against the locked-reread floors, then publish
    ///    membership while still holding that lock — a concurrent
    ///    floor raise can never interleave between the last
    ///    verification and the membership rename (review-9)
    /// → write the audience file if it didn't exist
    /// → final reload via [`Self::open`] + EQUALITY CHECK against
    ///    the candidate — a raced ceremony refuses rather than
    ///    returning some other writer's authority (review-9)
    /// ```
    ///
    /// A refused adoption before the store-creation step leaves the
    /// directory byte-for-byte untouched. Once a valid monotone
    /// bundle has been durably applied, a LATER failure does not
    /// roll it back (revocation is monotone; that is fail-closed).
    ///
    /// # Interrupted-ceremony recovery (review-9 addendum)
    ///
    /// A failure AFTER durable changes begin (store created,
    /// audience written) but BEFORE the membership publication
    /// leaves a PARTIAL scaffold: revocation state and/or audience
    /// key exist, membership does not. That scaffold is fail-closed
    /// — [`Self::open`] (and therefore production startup) refuses
    /// a directory without a membership file, so no ownership is
    /// ever emitted from it — and RESUMABLE: re-running `adopt`
    /// preserves the audience credential and every persisted floor
    /// maximum and completes the ceremony. The contract is
    /// deliberately NOT "all three files or none": rolling back
    /// monotone floor state to recover atomicity would be the
    /// weaker failure mode.
    ///
    /// - `owner-membership.json` — refuses a DIFFERENT org's cert
    ///   while an owner is installed; same-org renewal overwrites.
    /// - `owner-audience.key` — generated fresh on first adopt,
    ///   PRESERVED on re-adopt; a corrupt or group/other-readable
    ///   existing credential refuses the ceremony.
    /// - `revocation-state.json` — initialized on first adopt,
    ///   PRESERVED on re-adopt (floor monotonicity survives
    ///   re-adoption).
    ///
    /// `skew_secs` (ceiling-enforced) is PERSISTED into the
    /// membership config so production startup verifies with the
    /// same tolerance the ceremony accepted (review-9).
    pub fn adopt(
        dir: &Path,
        owner_cert: OrgMembershipCert,
        local_entity: &EntityId,
        skew_secs: u64,
        owner_floors: Option<&OrgRevocationBundle>,
    ) -> Result<Self, OrgAuthorityError> {
        // Gate-1: normalize the authority path ONCE, up front — resolve a
        // relative path against the current directory and strip a trailing
        // separator so a final symlink cannot be followed. EVERY path below
        // (the security checks, the ceremony lock, and the authority files)
        // derives from this single normalized form.
        let dir_buf = normalize_authority_dir(dir).map_err(|e| OrgAuthorityError::Io {
            path: dir.display().to_string(),
            reason: format!("normalize authority directory: {e}"),
        })?;
        let dir: &Path = &dir_buf;

        let membership_path = dir.join(OWNER_MEMBERSHIP_FILE);
        let audience_path = dir.join(OWNER_AUDIENCE_FILE);
        let revocation_path = dir.join(REVOCATION_STATE_FILE);

        // `OrgId` is `Copy`, but `owner_cert` itself is moved into the config
        // at step 6 — capture the org up front so steps 7-9 can still name it.
        let owner_org_id = owner_cert.org_id;

        // Gate-1: the authority directory is a trusted local security
        // boundary. Create it owner-only (0700) if missing, or validate an
        // existing one (a directory owned by the current user and not
        // group/other-writable), BEFORE provisioning any secrets into it.
        ensure_secure_authority_dir(dir)?;

        // 0. The ceremony lock: one adoption at a time per
        //    authority directory, held from the ownership decision
        //    through the final reopen. Without it, two concurrent
        //    FIRST adoptions by different orgs both observe "no
        //    owner" and both succeed (review-9 red).
        let _ceremony = lock_ceremony(dir)?;

        // 1. One node, one owner: a different installed org
        //    refuses; a corrupt membership file refuses too (the
        //    check cannot be evaluated against garbage — the
        //    operator removes it explicitly). The existing file is
        //    STRUCTURALLY VERIFIED first (review-9): only an
        //    authentic membership — consistent declared owner,
        //    valid signature, naming THIS node — may act as the
        //    ownership lock; a hand-edited `owner_org` must not
        //    turn a corrupt file into an ownership transfer.
        let had_membership = if let Some(existing) = read_optional(&membership_path)? {
            let existing: NodeAuthorityConfig = parse_membership(&existing, &membership_path)?;
            existing.verify_binding(local_entity).map_err(|e| {
                tracing::error!(
                    "existing membership failed structural verification ({e}); refusing \
                     re-adoption until the operator repairs or removes it"
                );
                e
            })?;
            if existing.owner_org != owner_cert.org_id {
                return Err(OrgAuthorityError::AlreadyOwned {
                    existing: existing.owner_org,
                    requested: owner_cert.org_id,
                });
            }
            true
        } else {
            false
        };

        // 2. Validate any preserved audience credential BEFORE the
        //    ceremony commits anything: no-follow regular-file
        //    handle, strict codec, AND the 0600 mode gate on the
        //    opened descriptor (a possibly-disclosed key must not
        //    be silently re-blessed by a renewal).
        //
        //    §2 — the credential is ALSO a second ownership witness, and it is
        //    the one that survives the failure mode step 1 cannot see. Step 1
        //    only refuses a foreign org when `owner-membership.json` is
        //    present, and the `AlreadyOwned` error told operators to "remove
        //    the existing authority explicitly to transfer" — so the
        //    documented remediation was to DELETE that file, which skipped the
        //    gate entirely. Adoption under a new org then preserved the old
        //    org's audience key (step 8 only mints when absent) and sealed the
        //    new org's private catalog under it.
        //
        //    Checking the credential's own `owner_org` closes that: the key
        //    file states which org it belongs to, so a mismatch is refused as
        //    the ownership conflict it is, whether or not the membership file
        //    is still there.
        let have_audience = match read_audience_checked(&audience_path)? {
            Some(bytes) => {
                // The read buffer carries the raw key — scrub it on every exit.
                let bytes = ScrubbedBytes(bytes);
                let credential = OwnerAudienceCredential::decode_config(bytes.as_slice())?;
                if credential.owner_org != owner_cert.org_id {
                    tracing::error!(
                        "the audience key in this authority directory belongs to a different \
                         org; refusing to carry it across an ownership change. Remove the whole \
                         authority directory to re-provision this node under the new org."
                    );
                    return Err(OrgAuthorityError::AlreadyOwned {
                        existing: credential.owner_org,
                        requested: owner_cert.org_id,
                    });
                }
                true
            }
            None => false,
        };

        // 3. Strictly load persisted floor maxima if present — no
        //    store creation yet; a refused adoption must leave a
        //    fresh directory untouched.
        let persisted = OrgRevocationState::load_if_exists(&revocation_path)?
            .unwrap_or_else(OrgRevocationState::empty);

        // 4. Verify the optional owner floor bundle and bind it to
        //    the candidate owner root. Signed receipt is not trust
        //    establishment: only the org that issued the candidate
        //    certificate may seed floors through THIS ceremony.
        if let Some(bundle) = owner_floors {
            bundle
                .verify()
                .map_err(|e| OrgAuthorityError::Revocation(OrgRevocationError::InvalidBundle(e)))?;
            if bundle.org_id != owner_cert.org_id {
                return Err(OrgAuthorityError::ForeignFloorBundle {
                    bundle_org: bundle.org_id,
                    owner_org: owner_cert.org_id,
                });
            }
        }

        // 5. Candidate floors = persisted maxima ∪ supplied bundle.
        let mut candidate_floors = persisted;
        if let Some(bundle) = owner_floors {
            candidate_floors.merge_bundle(bundle);
        }

        // 6. Verify the candidate certificate against the candidate
        //    state — a cert the resulting floors would immediately
        //    revoke must never adopt successfully (review-8 §7).
        let config = NodeAuthorityConfig::new(owner_cert, skew_secs);
        config.self_verify(local_entity, &candidate_floors)?;

        // 7. All validation passed — durable changes begin.
        //    Revocation first (monotone, never rolled back): create
        //    or open the store and apply the bundle through the
        //    locked reread path.
        //    A membership certificate or an audience key already sitting in
        //    this directory proves the node was provisioned before, so the
        //    revocation state MUST exist too. Saying so here is what stops a
        //    re-adopt against a deleted `revocation-state.json` from silently
        //    re-creating it EMPTY and un-revoking every certificate the org
        //    has retired (the store cannot tell loss from a first adopt on its
        //    own — see `ProvisioningExpectation`).
        let expect = if had_membership || have_audience {
            ProvisioningExpectation::MustExist
        } else {
            ProvisioningExpectation::MayBeFresh
        };
        let revocation = Arc::new(OrgRevocationStore::init(&revocation_path, expect)?);
        if let Some(bundle) = owner_floors {
            revocation.apply_bundle(bundle)?;
        }

        // 8. Audience material: preserved, or created and written
        //    now (0600, atomic, fresh temp inode).
        if !have_audience {
            let audience = OwnerAudienceCredential::generate(owner_org_id);
            // The serialized key buffer scrubs on every exit: the source array
            // inline (before the `?`), the Vec copy via its RAII guard.
            let mut raw = audience.encode_config();
            let encoded = ScrubbedBytes(raw.to_vec());
            for byte in raw.iter_mut() {
                // SAFETY: `byte` is a valid mutable reference into the owned array.
                unsafe { std::ptr::write_volatile(byte, 0) };
            }
            write_atomic(&audience_path, encoded.as_slice())?;
        }

        // 9. FINAL PHASE under the revocation-state lock (review-9):
        //    re-verify the certificate against the locked-reread
        //    floors, then publish membership while STILL holding
        //    the lock — a concurrent floor raise (any process's
        //    apply_bundle holds this same lock to write) can never
        //    interleave between the last verification and the
        //    membership rename, so the command never returns
        //    success with an already-revoked certificate installed.
        {
            let _state_lock = super::org_revocation::lock_state_file(&revocation_path)
                .map_err(OrgAuthorityError::Revocation)?;
            let locked_floors = OrgRevocationState::load_if_exists(&revocation_path)
                .map_err(OrgAuthorityError::Revocation)?
                .unwrap_or_else(OrgRevocationState::empty);
            config.self_verify(local_entity, &locked_floors)?;

            let membership_bytes =
                serde_json::to_vec_pretty(&config).map_err(|e| OrgAuthorityError::Io {
                    path: membership_path.display().to_string(),
                    reason: format!("serialize: {e}"),
                })?;
            write_atomic(&membership_path, &membership_bytes)?;
        }

        // 10. Final self-verification through the real startup
        //     loader, plus the EQUALITY CHECK (review-9): the
        //     reopened membership must be exactly the candidate
        //     this invocation installed — never some other writer's
        //     authority returned as our success. (The ceremony lock
        //     makes a mismatch unreachable; the check is the
        //     belt-and-braces witness that it stays that way.)
        let opened = Self::open(dir, local_entity)?;
        if opened.config != config {
            return Err(OrgAuthorityError::CeremonyRaced {
                detail: format!(
                    "reopened membership (owner {}) differs from the installed candidate \
                     (owner {})",
                    opened.config.owner_org, config.owner_org
                ),
            });
        }
        Ok(opened)
    }

    /// Startup: load the three files LOUDLY (missing or corrupt is
    /// a refusal, never a default) and self-verify the membership
    /// for `local_entity` under the PERSISTED ceremony skew
    /// (review-9: adoption and production startup verify with the
    /// same tolerance — the ceiling is still enforced inside the
    /// certificate check, so an oversized persisted value refuses
    /// loudly). A node that cannot prove its ownership does not
    /// get a `NodeAuthority`.
    ///
    /// The audience key file's mode is re-checked here (review-8
    /// §10) ON THE OPENED no-follow handle (review-9): creation-time
    /// 0600 is insufficient, symlinks are refused, and there is no
    /// check-to-read window.
    pub fn open(dir: &Path, local_entity: &EntityId) -> Result<Self, OrgAuthorityError> {
        // Gate-1: normalize ONCE (relative → cwd; strip a trailing separator so
        // a final symlink is not followed) before reading any authority state.
        let dir_buf = normalize_authority_dir(dir).map_err(|e| OrgAuthorityError::Io {
            path: dir.display().to_string(),
            reason: format!("normalize authority directory: {e}"),
        })?;
        let dir: &Path = &dir_buf;

        let membership_path = dir.join(OWNER_MEMBERSHIP_FILE);
        let audience_path = dir.join(OWNER_AUDIENCE_FILE);
        let revocation_path = dir.join(REVOCATION_STATE_FILE);

        // Gate-1: validate the authority directory boundary (owner-only,
        // owned by the current user) before reading any authority state.
        ensure_secure_authority_dir(dir)?;

        let membership_bytes = read_required(&membership_path)?;
        let config = parse_membership(&membership_bytes, &membership_path)?;

        let audience_bytes =
            ScrubbedBytes(read_audience_checked(&audience_path)?.ok_or_else(|| {
                let err = OrgAuthorityError::MissingFile {
                    path: audience_path.display().to_string(),
                };
                tracing::error!("{err}");
                err
            })?);
        let audience = OwnerAudienceCredential::decode_config(audience_bytes.as_slice())?;

        let revocation = Arc::new(OrgRevocationStore::open_existing(&revocation_path)?);

        config
            .self_verify(local_entity, &revocation.snapshot())
            .inspect_err(|e| {
                tracing::error!("node authority self-verification failed: {e}");
            })?;

        Ok(Self {
            config,
            audience,
            revocation,
        })
    }

    /// The owning organization.
    pub fn owner_org(&self) -> OrgId {
        self.config.owner_org
    }

    /// The authority directory's three file names, for tooling.
    pub fn file_names() -> [&'static str; 3] {
        [
            OWNER_MEMBERSHIP_FILE,
            OWNER_AUDIENCE_FILE,
            REVOCATION_STATE_FILE,
        ]
    }
}

impl std::fmt::Debug for NodeAuthority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeAuthority")
            .field("owner_org", &self.config.owner_org)
            .field("audience", &self.audience)
            .field("revocation", &self.revocation)
            .finish()
    }
}

fn parse_membership(bytes: &[u8], path: &Path) -> Result<NodeAuthorityConfig, OrgAuthorityError> {
    let config: NodeAuthorityConfig =
        serde_json::from_slice(bytes).map_err(|e| OrgAuthorityError::CorruptFile {
            path: path.display().to_string(),
            detail: e.to_string(),
        })?;
    if config.version != NODE_AUTHORITY_CONFIG_VERSION {
        return Err(OrgAuthorityError::UnsupportedVersion {
            path: path.display().to_string(),
            found: config.version,
        });
    }
    Ok(config)
}

/// Acquire the ceremony lock for `dir` (`<dir>/authority.lock`):
/// exactly one adoption at a time per authority directory, held
/// from the ownership decision through the final reopen (review-9).
/// Blocking; released when the handle drops. The lock inode is
/// held to the full regular-file policy via
/// [`org_revocation::open_lock_file`](super::org_revocation) —
/// no-follow, non-blocking open, and a type check on the opened
/// descriptor, so a planted symlink or FIFO is refused rather than
/// followed or parked on (review-9).
fn lock_ceremony(dir: &Path) -> Result<std::fs::File, OrgAuthorityError> {
    let lock_path = dir.join("authority.lock");
    let io = |e: std::io::Error| OrgAuthorityError::Io {
        path: lock_path.display().to_string(),
        reason: format!("ceremony lock: {e}"),
    };
    super::org_revocation::open_lock_file(&lock_path).map_err(io)
}

/// Policy decision for an EXISTING authority directory's Unix metadata
/// (Gate-1): the owner must be the effective user and the directory must not
/// be group/other-writable. Returns `Some(reason)` on violation, `None` when
/// acceptable. Kept as a pure `u32` function — independent of the OS `stat`
/// call — so the decision is unit-testable on every platform.
#[cfg_attr(not(unix), allow(dead_code))]
fn authority_dir_policy_violation(owner_uid: u32, mode: u32, euid: u32) -> Option<String> {
    if owner_uid != euid {
        return Some(format!(
            "owned by uid {owner_uid}, not the current effective user {euid}"
        ));
    }
    if mode & 0o022 != 0 {
        return Some(format!("group/other-writable (mode {:04o})", mode & 0o777));
    }
    None
}

/// Policy decision for ONE resolved ancestor of the authority directory
/// (Gate-1, Unix). An ancestor is unsafe if it is owned by another non-root
/// account — that owner can rewrite its entries directly, and the sticky bit
/// does NOT constrain a directory's own owner — or if it is group/other-
/// writable without the sticky bit (a non-owner could then rename an owned
/// child). Root-owned ancestors are trusted (OS administrator). Returns
/// `Some(reason)` on violation. Pure `u32` logic, unit-testable on every
/// platform.
#[cfg_attr(not(unix), allow(dead_code))]
fn unix_ancestor_violation(owner_uid: u32, mode: u32, euid: u32) -> Option<String> {
    if owner_uid != euid && owner_uid != 0 {
        return Some(format!(
            "owned by uid {owner_uid}, neither the current user {euid} nor root"
        ));
    }
    if mode & 0o022 != 0 && mode & 0o1000 == 0 {
        return Some(format!(
            "group/other-writable without the sticky bit (mode {:04o})",
            mode & 0o7777
        ));
    }
    None
}

/// Validate the resolved ancestor chain of the authority directory (Gate-1,
/// Unix). Validating only the final directory as owner-only 0700 is
/// insufficient if an ancestor is group/other-writable WITHOUT the sticky
/// bit: another account with write access to that ancestor could rename the
/// owned authority directory's entry and plant a replacement, so subsequent
/// pathname-based operations would enter it. That is cross-account mutation
/// THROUGH the parent — inside the declared account boundary — distinct from
/// same-account TOCTOU inside the directory, which stays out of scope.
///
/// Each ancestor of `dir` (its parent up to the filesystem root) must be owned
/// by the effective user or by root — a foreign non-root owner can rewrite its
/// entries directly (sticky does not constrain a directory's own owner) — and
/// must be either not group/other-writable or group/other-writable WITH the
/// sticky bit (e.g. `/tmp` at 01777 — sticky forbids a non-owner from renaming
/// an owned child). Symlinked components are resolved by canonicalizing the
/// deepest existing ancestor before the walk.
#[cfg(unix)]
fn validate_unix_ancestor_chain(dir: &Path) -> Result<(), OrgAuthorityError> {
    use std::os::unix::fs::MetadataExt;
    let io = |e: std::io::Error| OrgAuthorityError::Io {
        path: dir.display().to_string(),
        reason: format!("authority directory ancestor: {e}"),
    };
    // `dir` may not exist yet (create path): find the deepest EXISTING
    // ancestor, then canonicalize it so symlinked components resolve to the
    // real chain that will actually be traversed.
    let mut cursor = dir;
    let existing = loop {
        match cursor.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => {
                if parent.exists() {
                    break parent;
                }
                cursor = parent;
            }
            // Reached the root with no existing ancestor left to check.
            _ => return Ok(()),
        }
    };
    // SAFETY: geteuid() reads the caller's effective uid; it has no
    // preconditions, cannot fail, and touches no memory.
    let euid = unsafe { libc::geteuid() };
    let real = std::fs::canonicalize(existing).map_err(io)?;
    for ancestor in real.ancestors() {
        let meta = std::fs::symlink_metadata(ancestor).map_err(io)?;
        if let Some(reason) = unix_ancestor_violation(meta.uid(), meta.mode(), euid) {
            return Err(OrgAuthorityError::InsecureAuthorityDir {
                path: dir.display().to_string(),
                reason: format!(
                    "ancestor {} {reason} — another account could replace the \
                     authority directory entry through it",
                    ancestor.display()
                ),
            });
        }
    }
    Ok(())
}

/// §12 — the Windows counterpart of [`validate_unix_ancestor_chain`].
///
/// The Unix walk refuses an authority directory whose ANCESTORS are writable
/// by another account, on the stated grounds that "another account with write
/// access to that ancestor could rename the owned authority directory's entry
/// and plant a replacement". Windows had no counterpart at all: for a
/// PRE-EXISTING directory only the leaf's owner and DACL were validated, so
/// the module doc's claim to protect a custom nested path held on one platform
/// only.
///
/// Two of the three attacks that gap allowed are now closed elsewhere, and
/// saying which matters, because it bounds what this walk is actually for:
///
///   * SUBSTITUTION (plant a replacement directory) already failed closed —
///     an attacker-created directory is owned by the attacker and
///     `validate_dacl_view`'s owner rule refuses it, and a junction is refused
///     because Rust's `FileType::is_dir()` is false for a name-surrogate
///     reparse point.
///   * ACE PROPAGATION from an ancestor is closed by the §20-residual
///     inheritance severing in `validate_existing_dir_dacl`.
///   * DELETION is what remains, and it is why this exists: a parent-owner can
///     remove `owner-membership.json` and downgrade the node to "no owner
///     installed". §2's audience-org binding now stops that becoming a
///     cross-org key inheritance, but the downgrade itself is still a
///     denial-of-service an ancestor's owner should not be able to cause.
///
/// Ownership is the criterion, not the DACL: on Windows an object's owner
/// holds implicit `WRITE_DAC` regardless of what the ACL says, so a foreign
/// owner anywhere on the chain can re-grant itself write access at will —
/// exactly the reasoning `validate_dacl_view` already applies to the leaf.
#[cfg(windows)]
fn validate_windows_ancestor_chain(dir: &Path) -> Result<(), OrgAuthorityError> {
    let io = |e: std::io::Error| OrgAuthorityError::Io {
        path: dir.display().to_string(),
        reason: format!("authority directory ancestor: {e}"),
    };
    // `dir` may not exist yet (create path): find the deepest EXISTING
    // ancestor, then canonicalize so symlinked components resolve to the real
    // chain that will actually be traversed.
    let mut cursor = dir;
    let existing = loop {
        match cursor.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => {
                if parent.exists() {
                    break parent;
                }
                cursor = parent;
            }
            _ => return Ok(()),
        }
    };
    let user_sid = current_process_sid_string().map_err(|e| OrgAuthorityError::Io {
        path: dir.display().to_string(),
        reason: format!("resolve current user SID: {e}"),
    })?;
    const LOCAL_SYSTEM: &str = "S-1-5-18";
    const ADMINISTRATORS: &str = "S-1-5-32-544";
    const TRUSTED_INSTALLER: &str =
        "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464";
    let trusted = |sid: &str| {
        sid == user_sid || sid == LOCAL_SYSTEM || sid == ADMINISTRATORS || sid == TRUSTED_INSTALLER
    };

    let real = std::fs::canonicalize(existing).map_err(io)?;
    for ancestor in real.ancestors() {
        // A drive root (`\\?\C:\`) has no meaningful owner to check and is
        // administered by the system; stop rather than refuse every path on
        // the volume.
        if ancestor.parent().is_none() {
            break;
        }
        let view = match read_object_security(ancestor) {
            Ok(view) => view,
            // An ancestor we cannot READ the descriptor of is not evidence of
            // compromise — it is commonly a permissions boundary above the
            // user's profile. Fail OPEN here deliberately: the leaf checks are
            // what carry the security property, and refusing every node whose
            // `C:\Users` is opaque would make the product unusable for a
            // hardening that is defense in depth.
            Err(_) => continue,
        };
        if !trusted(&view.owner_sid) {
            return Err(OrgAuthorityError::InsecureAuthorityDir {
                path: dir.display().to_string(),
                reason: format!(
                    "ancestor {} is owned by untrusted principal {} — that owner holds \
                     implicit WRITE_DAC over the component and can remove or replace the \
                     authority directory's entry through it, whatever the authority \
                     directory's own ACL says",
                    ancestor.display(),
                    view.owner_sid
                ),
            });
        }
    }
    Ok(())
}
/// The current process user's SID, copied into an aligned owned buffer derived
/// from the process access token — NOT from `USERNAME` (spoofable). Returned as
/// `Vec<u32>` so `as_ptr()` is 4-byte aligned: a `SID`'s trailing
/// `SubAuthority` array is `u32` and the Win32 SID APIs require aligned access
/// (a byte `Vec` would be only 1-aligned). A SID is self-contained (no interior
/// pointers), so the copy is a valid `PSID` for as long as the buffer lives.
#[cfg(windows)]
#[allow(clippy::multiple_unsafe_ops_per_block)]
fn process_user_sid() -> std::io::Result<Vec<u32>> {
    type Handle = *mut std::ffi::c_void;
    extern "system" {
        fn GetCurrentProcess() -> Handle;
        fn OpenProcessToken(process: Handle, desired: u32, token: *mut Handle) -> i32;
        fn GetTokenInformation(
            token: Handle,
            class: i32,
            info: *mut std::ffi::c_void,
            len: u32,
            ret_len: *mut u32,
        ) -> i32;
        fn GetLengthSid(sid: *const std::ffi::c_void) -> u32;
        fn CopySid(dest_len: u32, dest: *mut std::ffi::c_void, src: *const std::ffi::c_void)
            -> i32;
        fn CloseHandle(handle: Handle) -> i32;
    }
    const TOKEN_QUERY: u32 = 0x0008;
    const TOKEN_USER: i32 = 1; // TOKEN_INFORMATION_CLASS::TokenUser
                               // SAFETY: the standard token → TOKEN_USER → SID copy. Every return value is
                               // checked; `buf` is sized by the first probe; `psid` points INTO `buf`,
                               // which stays alive across GetLengthSid + CopySid; the copy target `sid`
                               // is `GetLengthSid` bytes rounded up to whole `u32`s (so 4-aligned and
                               // large enough); the token handle is closed on every path.
    unsafe {
        let mut token: Handle = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut len: u32 = 0;
        GetTokenInformation(token, TOKEN_USER, std::ptr::null_mut(), 0, &mut len);
        if len == 0 {
            let e = std::io::Error::last_os_error();
            CloseHandle(token);
            return Err(e);
        }
        let mut buf = vec![0u8; len as usize];
        let ok = GetTokenInformation(token, TOKEN_USER, buf.as_mut_ptr().cast(), len, &mut len);
        CloseHandle(token);
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        // TOKEN_USER's first field is a SID_AND_ATTRIBUTES whose first field is
        // the PSID (a pointer into `buf`). `read_unaligned` because a `Vec<u8>`
        // is only byte-aligned.
        let psid = (buf.as_ptr() as *const *const std::ffi::c_void).read_unaligned();
        let sid_len = GetLengthSid(psid);
        if sid_len == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let words = (sid_len as usize).div_ceil(4);
        let mut sid = vec![0u32; words];
        if CopySid(sid_len, sid.as_mut_ptr().cast(), psid) == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(sid)
    }
}

/// Convert a `PSID` to its canonical string form (`S-1-5-…`) via
/// `ConvertSidToStringSidW`. `psid` must point at a valid SID that outlives the
/// call (from [`process_user_sid`] or a live security descriptor buffer).
#[cfg(windows)]
#[allow(clippy::multiple_unsafe_ops_per_block)]
fn sid_to_string(psid: *const std::ffi::c_void) -> std::io::Result<String> {
    use std::os::windows::ffi::OsStringExt;
    extern "system" {
        fn ConvertSidToStringSidW(sid: *const std::ffi::c_void, out: *mut *mut u16) -> i32;
        fn LocalFree(mem: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
    }
    // SAFETY: `psid` is a valid SID pointer per the contract above. The
    // LocalAlloc'd wide string is measured, copied out, then LocalFree'd; the
    // out-param is checked.
    unsafe {
        let mut out: *mut u16 = std::ptr::null_mut();
        if ConvertSidToStringSidW(psid, &mut out) == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut n = 0usize;
        while *out.add(n) != 0 {
            n += 1;
        }
        let s = std::ffi::OsString::from_wide(std::slice::from_raw_parts(out, n))
            .to_string_lossy()
            .into_owned();
        LocalFree(out.cast());
        Ok(s)
    }
}

/// The current process user's SID as a string (`S-1-5-21-…`). Thin wrapper over
/// [`process_user_sid`] + [`sid_to_string`] so the token → SID path lives in one
/// place. Used as a trusted principal when validating an existing DACL.
#[cfg(windows)]
fn current_process_sid_string() -> std::io::Result<String> {
    let sid = process_user_sid()?;
    sid_to_string(sid.as_ptr().cast())
}

/// Atomically create ONE directory with a PROTECTED, owner-only DACL (Gate-1,
/// Windows). `CreateDirectoryW` is called with a security descriptor whose DACL
/// grants ONLY `sid` full control, inheritable onto child files and directories
/// (`OI|CI`), and is marked `SE_DACL_PROTECTED` so NO inheritable ACEs from the
/// parent are merged in. Applying the protected DACL AT creation removes the
/// post-creation-`icacls` window in which the directory briefly existed under
/// inherited permissions, and — because creation is atomic — a failure leaves NO
/// directory behind (no fail-once / pass-on-retry residue). `sid` must be an
/// aligned, valid SID (see [`process_user_sid`]).
#[cfg(windows)]
#[allow(clippy::multiple_unsafe_ops_per_block)]
fn create_dir_with_owner_only_dacl(path: &Path, sid: &[u32]) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    type Pv = *mut std::ffi::c_void;
    #[repr(C)]
    struct SecurityAttributes {
        n_length: u32,
        lp_security_descriptor: Pv,
        b_inherit_handle: i32,
    }
    // The absolute-form SECURITY_DESCRIPTOR (pointer fields, so 8-aligned).
    #[repr(C)]
    struct SecurityDescriptor {
        revision: u8,
        sbz1: u8,
        control: u16,
        owner: Pv,
        group: Pv,
        sacl: Pv,
        dacl: Pv,
    }
    extern "system" {
        fn InitializeAcl(acl: Pv, len: u32, revision: u32) -> i32;
        fn AddAccessAllowedAceEx(acl: Pv, revision: u32, flags: u32, mask: u32, sid: Pv) -> i32;
        fn InitializeSecurityDescriptor(sd: Pv, revision: u32) -> i32;
        fn SetSecurityDescriptorDacl(sd: Pv, present: i32, dacl: Pv, defaulted: i32) -> i32;
        fn SetSecurityDescriptorControl(sd: Pv, mask: u16, bits: u16) -> i32;
        fn CreateDirectoryW(path: *const u16, sa: *const SecurityAttributes) -> i32;
    }
    const ACL_REVISION: u32 = 2;
    const SD_REVISION: u32 = 1;
    const OBJECT_INHERIT_ACE: u32 = 0x1;
    const CONTAINER_INHERIT_ACE: u32 = 0x2;
    const FILE_ALL_ACCESS: u32 = 0x001F_01FF;
    // SE_DACL_PROTECTED: do not merge inheritable ACEs from the parent, so the
    // grant is EXACTLY the owner (the OI|CI flags still propagate the owner ACE
    // down to child files/dirs, keeping them owner-only).
    const SE_DACL_PROTECTED: u16 = 0x1000;

    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    wide.push(0);
    // 4-byte-aligned ACL storage; one ACE plus a user SID fits easily in 512 B.
    let mut acl_buf = [0u32; 128];
    let mut sd = SecurityDescriptor {
        revision: 0,
        sbz1: 0,
        control: 0,
        owner: std::ptr::null_mut(),
        group: std::ptr::null_mut(),
        sacl: std::ptr::null_mut(),
        dacl: std::ptr::null_mut(),
    };
    // SAFETY: `acl_buf` (4-aligned, 512 B) receives one ACCESS_ALLOWED ACE for
    // `sid` (aligned, valid). `sd` is a stack SECURITY_DESCRIPTOR whose DACL
    // points into `acl_buf`; `sa` points at `sd`. All of `wide`, `acl_buf`,
    // `sd`, `sid` outlive the single CreateDirectoryW call in this scope, so no
    // pointer dangles. Every Win32 return value is checked.
    unsafe {
        let acl: Pv = acl_buf.as_mut_ptr().cast();
        let sid_ptr = sid.as_ptr() as Pv;
        let sd_ptr: Pv = (&mut sd as *mut SecurityDescriptor).cast();
        if InitializeAcl(acl, (acl_buf.len() * 4) as u32, ACL_REVISION) == 0 {
            return Err(std::io::Error::last_os_error());
        }
        if AddAccessAllowedAceEx(
            acl,
            ACL_REVISION,
            OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE,
            FILE_ALL_ACCESS,
            sid_ptr,
        ) == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        if InitializeSecurityDescriptor(sd_ptr, SD_REVISION) == 0 {
            return Err(std::io::Error::last_os_error());
        }
        if SetSecurityDescriptorDacl(sd_ptr, 1, acl, 0) == 0 {
            return Err(std::io::Error::last_os_error());
        }
        if SetSecurityDescriptorControl(sd_ptr, SE_DACL_PROTECTED, SE_DACL_PROTECTED) == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let sa = SecurityAttributes {
            n_length: std::mem::size_of::<SecurityAttributes>() as u32,
            lp_security_descriptor: sd_ptr,
            b_inherit_handle: 0,
        };
        if CreateDirectoryW(wide.as_ptr(), &sa) == 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Sever inheritance on an EXISTING directory by writing a PROTECTED,
/// owner-only DACL onto it (Gate-1, Windows) — the repair half of the §20
/// residual.
///
/// An unprotected directory keeps merging inheritable aces from its ancestors,
/// so whoever controls any ancestor can propagate access onto
/// `owner-audience.key` AFTER validation passes, without holding any right on
/// the authority directory itself. Refusing such a directory outright would be
/// safe but would reject the common and legitimate
/// `mkdir ~/net-authority && net node adopt --authority-dir ~/net-authority`,
/// and every directory created by config management — a usability regression
/// in exchange for a hazard we can simply remove.
///
/// So: repair rather than refuse. The caller validates every OTHER rule first
/// (owner trusted, no untrusted aces, no NULL DACL) and only then calls this,
/// so a foreign-owned or already-compromised directory is never silently
/// "fixed" into looking clean — the repair is for a directory that is fine
/// today and cannot prove it will stay that way.
#[cfg(windows)]
#[allow(clippy::multiple_unsafe_ops_per_block)]
fn protect_existing_dir_dacl(path: &Path, sid: &[u32]) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    type Pv = *mut std::ffi::c_void;
    extern "system" {
        fn InitializeAcl(acl: Pv, len: u32, revision: u32) -> i32;
        fn AddAccessAllowedAceEx(acl: Pv, revision: u32, flags: u32, mask: u32, sid: Pv) -> i32;
        fn SetNamedSecurityInfoW(
            object_name: *mut u16,
            object_type: u32,
            security_info: u32,
            owner: Pv,
            group: Pv,
            dacl: Pv,
            sacl: Pv,
        ) -> u32;
    }
    const ACL_REVISION: u32 = 2;
    const OBJECT_INHERIT_ACE: u32 = 0x1;
    const CONTAINER_INHERIT_ACE: u32 = 0x2;
    const FILE_ALL_ACCESS: u32 = 0x001F_01FF;
    const SE_FILE_OBJECT: u32 = 1;
    const DACL_SECURITY_INFORMATION: u32 = 0x0000_0004;
    // Setting this is what actually severs inheritance: the DACL stops merging
    // inheritable aces from the parent chain.
    const PROTECTED_DACL_SECURITY_INFORMATION: u32 = 0x8000_0000;
    const ERROR_SUCCESS: u32 = 0;

    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    wide.push(0);
    // 4-byte-aligned ACL storage; one ACE plus a user SID fits easily in 512 B.
    let mut acl_buf = [0u32; 128];
    // SAFETY: `acl_buf` (4-aligned, 512 B) receives one ACCESS_ALLOWED ACE for
    // `sid` (aligned, valid, outlives the call). `wide` is NUL-terminated and
    // outlives the call. `SetNamedSecurityInfoW` copies the ACL, so nothing
    // dangles after return. Every Win32 return value is checked.
    unsafe {
        let acl: Pv = acl_buf.as_mut_ptr().cast();
        let sid_ptr = sid.as_ptr() as Pv;
        if InitializeAcl(acl, (acl_buf.len() * 4) as u32, ACL_REVISION) == 0 {
            return Err(std::io::Error::last_os_error());
        }
        // OI|CI so the owner-only grant propagates to the authority FILES,
        // which is what gives `owner-audience.key` its ACL on NTFS.
        if AddAccessAllowedAceEx(
            acl,
            ACL_REVISION,
            OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE,
            FILE_ALL_ACCESS,
            sid_ptr,
        ) == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        let rc = SetNamedSecurityInfoW(
            wide.as_mut_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            acl,
            std::ptr::null_mut(),
        );
        if rc != ERROR_SUCCESS {
            return Err(std::io::Error::from_raw_os_error(rc as i32));
        }
    }
    Ok(())
}

/// Create every missing component of `dir` (missing intermediate parents AND the
/// final directory) each with a PROTECTED owner-only DACL (Gate-1, Windows).
/// Mirrors the Unix `create_missing_components_0700`: a non-recursive,
/// shallowest-first walk, so a component another account plants between the
/// existence check and the create fails loudly (`CreateDirectoryW` →
/// `ERROR_ALREADY_EXISTS`) rather than being adopted, and no intermediate is
/// left under inherited (broad) permissions. A child DACL cannot stop a writable
/// PARENT's owner from replacing the child entry, so securely creating the whole
/// missing chain — not just the leaf — is what protects a custom nested
/// `--authority-dir`.
#[cfg(windows)]
fn create_missing_components_owner_only(dir: &Path, sid: &[u32]) -> std::io::Result<()> {
    let mut missing: Vec<&Path> = Vec::new();
    let mut cursor: &Path = dir;
    loop {
        if cursor.exists() {
            break;
        }
        match cursor.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => {
                missing.push(cursor);
                cursor = parent;
            }
            // Reached a parentless component (e.g. a nonexistent drive root):
            // do not try to create it; the shallowest real create fails loudly.
            _ => break,
        }
    }
    for component in missing.iter().rev() {
        create_dir_with_owner_only_dacl(component, sid)?;
    }
    Ok(())
}

/// One access-allowed / -denied ACE from a DACL, in inspectable form (Gate-1,
/// Windows). `sid` is the granted principal's string SID for the simple ACE
/// types (0 = ACCESS_ALLOWED, 1 = ACCESS_DENIED); for any other ACE type it is a
/// sentinel so a validator treats it conservatively.
#[cfg(windows)]
#[derive(Debug, Clone)]
struct AceInfo {
    sid: String,
    mask: u32,
    ace_type: u8,
    /// Inheritance / inherited-from-parent flags — inspected by the witnesses
    /// (OI|CI on a fresh dir, INHERITED on a child file), not by the production
    /// validator (which reasons about the mask + SID).
    #[allow(dead_code)]
    flags: u8,
}

/// The security-relevant view of a filesystem object's security descriptor
/// (Gate-1, Windows), read via the BINARY Win32 security APIs — not by parsing
/// localizable `icacls` text. `null_dacl` (a present-but-NULL or absent DACL,
/// both of which grant everyone full access) must fail closed.
#[cfg(windows)]
#[derive(Debug)]
struct DaclView {
    /// Owner SID string — a PRODUCTION criterion, checked by
    /// [`validate_existing_dir_dacl`].
    ///
    /// An earlier revision carried this `#[allow(dead_code)]` with the note
    /// "a foreign owner cannot itself grant access — the DACL governs that."
    /// That is false on Windows: an object's owner is implicitly granted
    /// `READ_CONTROL` and `WRITE_DAC` on every access check unless an
    /// `OWNER RIGHTS` (`S-1-3-4`) ACE is present in the DACL, and nothing
    /// here requires one. A foreign owner can therefore re-open the boundary
    /// at will AFTER validation passes — set a DACL that names only the
    /// victim, let adoption provision `owner-audience.key` into the
    /// directory, then rewrite the DACL and read the raw owner discovery key.
    /// Ownership is a write-capable path that never appears as an ACE, so the
    /// ACE walk below cannot see it (§3).
    owner_sid: String,
    /// `SE_DACL_PROTECTED` — a PRODUCTION criterion, checked by
    /// [`validate_dacl_view`].
    ///
    /// An earlier revision carried this `#[allow(dead_code)]` with the note
    /// "NOT a production criterion for a pre-existing dir, which may
    /// legitimately inherit an owner-only ACL from a protected parent
    /// profile". That reasoning describes what the ACL looks like NOW and
    /// misses what an UNPROTECTED ACL means: it stays subject to Windows'
    /// automatic inheritance propagation, so whoever controls any ancestor can
    /// push a new inheritable ACE down into this directory — and onto the
    /// files in it — at any time AFTER validation returns.
    ///
    /// That is the same class of hole as the foreign-OWNER one above, one
    /// level up: the §20 inheritance rule refuses an inheritable untrusted ACE
    /// that is present at validation time, and this refuses a descriptor that
    /// cannot stop one being added a second later. Without it the §20 fix
    /// rested on "only a trusted principal has WRITE_DAC on this directory",
    /// which is not the privilege the attack needs — the ancestor's owner
    /// needs no rights on the authority directory at all.
    ///
    /// This is the third field in this struct to arrive `#[allow(dead_code)]`
    /// with a note asserting it is not a production criterion. For `owner_sid`
    /// that assertion was wrong and became §3. Treat the pattern as a smell.
    protected: bool,
    null_dacl: bool,
    aces: Vec<AceInfo>,
}

/// Placeholder SID recorded for an ACE whose type does not carry its SID at
/// the fixed byte-8 offset the simple ALLOWED/DENIED types use (object and
/// callback ACEs). It is deliberately not a parseable SID string, so it can
/// never compare equal to a trusted principal — a write-capable ACE bearing
/// it therefore fails closed in [`validate_existing_dir_dacl`] (§4).
#[cfg(windows)]
const NON_SIMPLE_ACE_SID: &str = "<non-simple-ace>";

/// The access-mask bits that make an ACE write-capable (able to mutate the
/// object, its contents, or its ACL/owner). Any allowed ACE carrying one of
/// these for a non-trusted principal breaks the owner-only invariant.
#[cfg(windows)]
const WRITE_MASK: u32 = 0x0000_0002 // FILE_WRITE_DATA / FILE_ADD_FILE
    | 0x0000_0004 // FILE_APPEND_DATA / FILE_ADD_SUBDIRECTORY
    | 0x0000_0010 // FILE_WRITE_EA
    | 0x0000_0040 // FILE_DELETE_CHILD
    | 0x0000_0100 // FILE_WRITE_ATTRIBUTES
    | 0x0001_0000 // DELETE
    | 0x0004_0000 // WRITE_DAC
    | 0x0008_0000 // WRITE_OWNER
    | 0x1000_0000 // GENERIC_ALL
    | 0x4000_0000; // GENERIC_WRITE

/// Read an existing filesystem object's owner + DACL through the binary Win32
/// security APIs (Gate-1, Windows). Uses `GetFileSecurityW` into a caller-owned
/// aligned buffer (no `LocalFree` bookkeeping): the returned owner / DACL
/// pointers borrow into that buffer, so every string is copied out before it is
/// dropped. Walks the ACL by index so no ACE ordering or count is assumed.
#[cfg(windows)]
#[allow(clippy::multiple_unsafe_ops_per_block)]
fn read_object_security(path: &Path) -> std::io::Result<DaclView> {
    use std::os::windows::ffi::OsStrExt;
    type Pv = *mut std::ffi::c_void;
    extern "system" {
        fn GetFileSecurityW(name: *const u16, info: u32, sd: Pv, len: u32, needed: *mut u32)
            -> i32;
        fn GetSecurityDescriptorOwner(sd: Pv, owner: *mut Pv, defaulted: *mut i32) -> i32;
        fn GetSecurityDescriptorControl(sd: Pv, control: *mut u16, revision: *mut u32) -> i32;
        fn GetSecurityDescriptorDacl(
            sd: Pv,
            present: *mut i32,
            dacl: *mut Pv,
            defaulted: *mut i32,
        ) -> i32;
        fn GetAce(acl: Pv, index: u32, ace: *mut Pv) -> i32;
    }
    const OWNER_SECURITY_INFORMATION: u32 = 0x0000_0001;
    const DACL_SECURITY_INFORMATION: u32 = 0x0000_0004;
    const SE_DACL_PROTECTED: u16 = 0x1000;
    const INFO: u32 = OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;

    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    wide.push(0);
    // SAFETY: `wide` is a valid NUL-terminated path. The first GetFileSecurityW
    // sizes the SD; `sd_buf` (Vec<u32>, 4-aligned) then receives the whole
    // self-relative descriptor. GetSecurityDescriptorOwner/Control/Dacl return
    // pointers INTO `sd_buf`, valid while it lives; each ACE is fetched by index
    // in `[0, ace_count)` and its {type, flags, mask, SID} read at fixed offsets
    // (SID at byte 8 for the simple ACE types). All strings are copied out
    // before `sd_buf` drops; every Win32 return value is checked.
    unsafe {
        let mut needed: u32 = 0;
        // Probe for the required length (this call is expected to fail).
        GetFileSecurityW(wide.as_ptr(), INFO, std::ptr::null_mut(), 0, &mut needed);
        if needed == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut sd_buf = vec![0u32; (needed as usize).div_ceil(4)];
        let sd: Pv = sd_buf.as_mut_ptr().cast();
        if GetFileSecurityW(
            wide.as_ptr(),
            INFO,
            sd,
            (sd_buf.len() * 4) as u32,
            &mut needed,
        ) == 0
        {
            return Err(std::io::Error::last_os_error());
        }

        let mut owner: Pv = std::ptr::null_mut();
        let mut defaulted: i32 = 0;
        if GetSecurityDescriptorOwner(sd, &mut owner, &mut defaulted) == 0 || owner.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let owner_sid = sid_to_string(owner)?;

        let mut control: u16 = 0;
        let mut revision: u32 = 0;
        if GetSecurityDescriptorControl(sd, &mut control, &mut revision) == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let protected = control & SE_DACL_PROTECTED != 0;

        let mut present: i32 = 0;
        let mut dacl: Pv = std::ptr::null_mut();
        let mut dacl_defaulted: i32 = 0;
        if GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut dacl_defaulted) == 0 {
            return Err(std::io::Error::last_os_error());
        }
        // An absent (present == 0) or NULL DACL grants everyone full access.
        if present == 0 || dacl.is_null() {
            return Ok(DaclView {
                owner_sid,
                protected,
                null_dacl: true,
                aces: Vec::new(),
            });
        }

        // ACL header: { AclRevision u8, Sbz1 u8, AclSize u16, AceCount u16, .. }.
        let ace_count = (dacl.cast::<u8>().add(4) as *const u16).read_unaligned();
        let mut aces = Vec::with_capacity(ace_count as usize);
        for i in 0..u32::from(ace_count) {
            let mut ace: Pv = std::ptr::null_mut();
            if GetAce(dacl, i, &mut ace) == 0 {
                return Err(std::io::Error::last_os_error());
            }
            let base = ace.cast::<u8>();
            let ace_type = *base;
            let flags = *base.add(1);
            // Mask sits at byte 4 for every ACE_HEADER-prefixed ACE.
            let mask = (base.add(4) as *const u32).read_unaligned();
            // The SID begins at byte 8 for the SIMPLE ACE types only; other
            // (e.g. object / callback) ACEs place it elsewhere, so record the
            // sentinel and let the validator treat a write-capable one
            // conservatively. `validate_existing_dir_dacl` honors that: it
            // skips only DENY types, so a sentinel-SID grant reaches the
            // trusted-principal check and — never matching one — refuses.
            let sid = if ace_type == 0 || ace_type == 1 {
                sid_to_string(base.add(8).cast())?
            } else {
                String::from(NON_SIMPLE_ACE_SID)
            };
            aces.push(AceInfo {
                sid,
                mask,
                ace_type,
                flags,
            });
        }
        Ok(DaclView {
            owner_sid,
            protected,
            null_dacl: false,
            aces,
        })
    }
}

/// Create every missing component of `dir` (intermediate parents AND the final
/// directory) with permissions no broader than 0700 (Gate-1, Unix). A
/// permissive umask must not leave a 0777 intermediate parent through which
/// another account could later replace the final authority directory. Uses a
/// non-recursive `DirBuilder::create` per component, shallowest-first, so a
/// component another account planted between the existence check and the create
/// fails loudly with `AlreadyExists` rather than being silently adopted.
#[cfg(unix)]
fn create_missing_components_0700(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    // Collect the missing chain from `dir` up to the deepest existing ancestor.
    let mut missing: Vec<&Path> = Vec::new();
    let mut cursor: &Path = dir;
    loop {
        if cursor.exists() {
            break;
        }
        missing.push(cursor);
        match cursor.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => cursor = parent,
            _ => break,
        }
    }
    // Create shallowest → deepest so each component's parent already exists.
    for component in missing.iter().rev() {
        std::fs::DirBuilder::new().mode(0o700).create(component)?;
    }
    Ok(())
}

/// Normalize an authority directory path ONCE, before any validation, lock, or
/// file I/O (Gate-1). Two path hazards are closed here, at a single choke point:
///
/// - A bare relative path (`authority`) has an EMPTY [`Path::parent`], so the
///   ancestor-chain check would traverse nothing and provision beneath an
///   unvalidated working directory; the path is resolved against the current
///   directory captured once here.
/// - A trailing separator makes a follow-symlink `stat` resolve a FINAL symlink
///   to its target (`symlink_metadata("link/")` reports the target directory
///   while `symlink_metadata("link")` reports the link itself), which would slip
///   a final-symlink authority directory past the "not a directory" refusal and
///   redirect authority I/O into the link target. Re-collecting
///   [`Path::components`] strips trailing separators (and `.` / redundant
///   separators) WITHOUT following any symlink or touching the filesystem,
///   preserving the root prefix and the intended final component.
///
/// The returned path is the one EVERY subsequent step (prevalidation, creation,
/// postvalidation, lock acquisition, file operations) must use.
fn normalize_authority_dir(dir: &Path) -> std::io::Result<PathBuf> {
    let base = if dir.is_absolute() {
        dir.to_path_buf()
    } else {
        std::env::current_dir()?.join(dir)
    };
    // `components()` yields no trailing-separator artifact and drops `CurDir`
    // (`.`) components; collecting rebuilds a clean, symlink-preserving path.
    Ok(base.components().collect())
}

/// Create or validate the authority directory as a trusted local security
/// boundary (Gate-1). This is the DEDICATED authority scaffold — the only
/// layer that may create the directory owner-only or tighten its mode; the
/// generic, path-agnostic [`OrgRevocationStore`] API never chmods a supplied
/// parent directory.
///
/// Threat boundary (see the module docs): the authority directory is TRUSTED.
/// Concurrent mutation by another process running with write access to it is
/// explicitly out of scope — a same-account attacker who can write here can
/// already attack the surrounding configuration and process state.
///
/// - Unix: the resolved ancestor chain is validated first
///   ([`validate_unix_ancestor_chain`]) so no other account owns, or can
///   replace the directory's entry through, an ancestor. A MISSING directory —
///   and any missing intermediate parents — is then created no broader than
///   0700 ([`create_missing_components_0700`]; never a umask-moded 0777
///   intermediate) and the completed chain is RE-validated; an EXISTING one
///   must be a directory owned by the effective user and not group/other-
///   writable; either way it is finally tightened to EXACTLY 0700 by the owner
///   before use (a restrictive-umask create + owner chmod(0700) is safe; the
///   dangerous pattern was permissive create then tighten). State / lock /
///   audience files are 0600.
/// - Windows: every MISSING component (intermediate parents AND the final
///   directory) is created ATOMICALLY with a protected, owner-only DACL
///   ([`create_missing_components_owner_only`] → [`create_dir_with_owner_only_dacl`]:
///   `CreateDirectoryW` with a `SECURITY_ATTRIBUTES` whose DACL grants only the
///   process TOKEN SID full control, `OI|CI`-inheritable onto child files, and
///   is `SE_DACL_PROTECTED` so no parent ACEs are merged). There is no
///   post-creation window under inherited permissions, and a failure leaves NO
///   directory behind — so a retry cannot adopt an insecure residue. Securely
///   creating the WHOLE missing chain (not just the leaf) is what protects a
///   custom nested path, since a child DACL cannot stop a writable parent's
///   owner from replacing the child entry. A pre-existing directory is
///   re-validated against its BINARY DACL ([`validate_existing_dir_dacl`]) and
///   fails closed unless every write-capable ACE grants only a trusted
///   principal. The user account, SYSTEM, and local administrators are trusted
///   principals.
fn ensure_secure_authority_dir(dir: &Path) -> Result<(), OrgAuthorityError> {
    let io = |e: std::io::Error| OrgAuthorityError::Io {
        path: dir.display().to_string(),
        reason: format!("authority directory: {e}"),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        // Validate the trusted-ancestor chain BEFORE any operation on `dir`
        // (no authority file is created or read before this).
        validate_unix_ancestor_chain(dir)?;
        match std::fs::symlink_metadata(dir) {
            Ok(meta) => {
                if !meta.file_type().is_dir() {
                    return Err(OrgAuthorityError::InsecureAuthorityDir {
                        path: dir.display().to_string(),
                        reason: "path exists but is not a directory".to_string(),
                    });
                }
                // SAFETY: geteuid() reads the caller's effective uid; it has
                // no preconditions, cannot fail, and touches no memory.
                let euid = unsafe { libc::geteuid() };
                if let Some(reason) = authority_dir_policy_violation(meta.uid(), meta.mode(), euid)
                {
                    return Err(OrgAuthorityError::InsecureAuthorityDir {
                        path: dir.display().to_string(),
                        reason,
                    });
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Create every missing component (intermediate parents AND the
                // final dir) no broader than 0700 — a permissive umask must not
                // leave a 0777 intermediate through which another account could
                // later replace the final directory. Then RE-validate the now-
                // complete resolved chain, which also closes a race where
                // another account created an intermediate between the
                // prevalidation above and this creation.
                create_missing_components_0700(dir).map_err(io)?;
                validate_unix_ancestor_chain(dir)?;
            }
            Err(e) => return Err(io(e)),
        }
        // Tighten to EXACTLY 0700 (create was no-broader-than-0700; an existing
        // owner-controlled dir may carry group/other READ bits a lax umask
        // left). This scaffold owns the tightening.
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(io)?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        match std::fs::symlink_metadata(dir) {
            Ok(meta) => {
                if !meta.file_type().is_dir() {
                    return Err(OrgAuthorityError::InsecureAuthorityDir {
                        path: dir.display().to_string(),
                        reason: "path exists but is not a directory".to_string(),
                    });
                }
                // Gate-1 (Windows): re-validate the EXISTING directory's BINARY
                // DACL and fail CLOSED unless every write-capable ACE is a
                // trusted principal. A modified `%APPDATA%` directory, a
                // permissive custom `--authority-dir`, or a directory left by an
                // older or aborted run must NOT be adopted merely because it
                // exists — that was the operator-asserted downgrade Kyra flagged.
                #[cfg(windows)]
                {
                    // §12 — the ancestor chain first, mirroring the Unix
                    // ordering: a component another account owns can have the
                    // authority directory's entry removed through it whatever
                    // the leaf's own ACL says, so validating the leaf first
                    // would be checking the wrong thing in the wrong order.
                    validate_windows_ancestor_chain(dir)?;
                    validate_existing_dir_dacl(dir)?;
                }
                #[cfg(not(windows))]
                tracing::debug!(
                    path = %dir.display(),
                    "authority directory exists; binary ACL validation is \
                     unavailable on this platform",
                );
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Gate-1 (Windows): create every missing component (intermediate
                // parents AND the final directory) ATOMICALLY with a protected
                // owner-only DACL — there is no post-creation window under
                // inherited permissions, and a failure leaves NO directory
                // behind, so a retry cannot adopt an insecure residue.
                #[cfg(windows)]
                {
                    // §12 — validate the existing part of the chain BEFORE
                    // creating into it, then RE-validate the completed chain,
                    // exactly as the Unix path does. The second pass closes the
                    // race where another account creates an intermediate
                    // between the pre-check and the create.
                    validate_windows_ancestor_chain(dir)?;
                    let sid = process_user_sid().map_err(io)?;
                    create_missing_components_owner_only(dir, &sid).map_err(io)?;
                    validate_windows_ancestor_chain(dir)?;
                }
                #[cfg(not(windows))]
                {
                    if let Some(parent) = dir.parent() {
                        if !parent.as_os_str().is_empty() {
                            std::fs::create_dir_all(parent).map_err(io)?;
                        }
                    }
                    std::fs::create_dir(dir).map_err(io)?;
                }
            }
            Err(e) => return Err(io(e)),
        }
        Ok(())
    }
}

/// Validate an EXISTING authority directory's DACL (Gate-1, Windows) — fail
/// CLOSED unless every write-capable ALLOWED ACE grants only a trusted
/// principal: the current process user, Local System (`S-1-5-18`), or the local
/// Administrators group (`S-1-5-32-544`). A present-but-NULL or absent DACL
/// (everyone full access) is rejected outright. Read-only grants to other
/// principals are tolerated — only write-capable ACEs threaten the owner-only
/// invariant. Reads the BINARY security descriptor ([`read_object_security`]),
/// never localizable `icacls` text.
#[cfg(windows)]
fn validate_existing_dir_dacl(dir: &Path) -> Result<(), OrgAuthorityError> {
    let view = read_object_security(dir).map_err(|e| OrgAuthorityError::Io {
        path: dir.display().to_string(),
        reason: format!("read authority directory security descriptor: {e}"),
    })?;
    let user_sid = current_process_sid_string().map_err(|e| OrgAuthorityError::Io {
        path: dir.display().to_string(),
        reason: format!("resolve current user SID: {e}"),
    })?;
    // §20 residual — an unprotected DACL still merges inheritable aces from
    // the ancestor chain, so a clean ace list here is not a durable statement:
    // the owner of ANY ancestor can propagate access onto the authority files
    // afterwards, needing no permission on this directory at all.
    //
    // Repair rather than refuse. Every OTHER rule is checked FIRST against the
    // descriptor as found, so a foreign-owned directory, one with untrusted
    // aces, or one with a NULL DACL is never silently "fixed" into looking
    // clean — those still fail, loudly and unrepaired. Only a directory that
    // is fine today, and merely cannot prove it will stay that way, gets its
    // inheritance severed. Refusing instead would reject the ordinary
    // `mkdir && net node adopt`, trading a real usability regression for a
    // hazard we can simply remove.
    if !view.protected {
        validate_dacl_rules(&view, &user_sid, dir)?;
        let sid = process_user_sid().map_err(|e| OrgAuthorityError::Io {
            path: dir.display().to_string(),
            reason: format!("resolve current user SID: {e}"),
        })?;
        protect_existing_dir_dacl(dir, &sid).map_err(|e| OrgAuthorityError::Io {
            path: dir.display().to_string(),
            reason: format!("sever DACL inheritance on the authority directory: {e}"),
        })?;
        tracing::info!(
            path = %dir.display(),
            "severed DACL inheritance on the pre-existing authority directory so \
             an ancestor cannot later propagate access onto the authority files",
        );
        // Re-read and validate for real: the repair must have taken, and the
        // full rule set (including `protected`) is what has to pass.
        let view = read_object_security(dir).map_err(|e| OrgAuthorityError::Io {
            path: dir.display().to_string(),
            reason: format!("re-read security descriptor after severing inheritance: {e}"),
        })?;
        return validate_dacl_view(&view, &user_sid, dir);
    }
    validate_dacl_view(&view, &user_sid, dir)
}

/// The PURE half of [`validate_existing_dir_dacl`]: given an already-read
/// security descriptor and the current user's SID, decide whether the object
/// is owner-only.
///
/// Split out so the two fail-closed rules below are unit-testable against
/// synthetic descriptors. The foreign-OWNER rule in particular cannot be
/// witnessed end-to-end without a second user account and elevation, which no
/// CI runner (and no developer workstation, by default) provides — so without
/// this seam it would ship with no test at all, which is how it came to be
/// `#[allow(dead_code)]` in the first place.
#[cfg(windows)]
fn validate_dacl_view(
    view: &DaclView,
    user_sid: &str,
    dir: &Path,
) -> Result<(), OrgAuthorityError> {
    validate_dacl_rules(view, user_sid, dir)?;
    // §20 residual — see `validate_existing_dir_dacl`, which repairs an
    // unprotected directory before reaching here. By this point the flag is a
    // hard requirement: an unprotected DACL keeps merging inheritable aces
    // from the ancestor chain, so the rule set above describes only this
    // instant and nothing prevents an ancestor's owner from propagating access
    // onto the authority files a moment later — without needing any permission
    // on this directory.
    if !view.protected {
        return Err(OrgAuthorityError::InsecureAuthorityDir {
            path: dir.display().to_string(),
            reason: format!(
                "authority directory does not have a PROTECTED DACL (SE_DACL_PROTECTED is \
                 unset), so it still inherits from its parents — anyone who can write an \
                 inheritable ace on ANY ancestor can propagate access onto \
                 {OWNER_AUDIENCE_FILE} after this validation passes, without needing any \
                 permission on the authority directory itself. Sever inheritance on this \
                 directory (`icacls <dir> /inheritance:r`) or let `net node adopt` create it"
            ),
        });
    }
    Ok(())
}

/// Every owner-only rule EXCEPT the `SE_DACL_PROTECTED` requirement.
///
/// Split from [`validate_dacl_view`] so [`validate_existing_dir_dacl`] can
/// establish that a pre-existing directory is otherwise sound BEFORE severing
/// its inheritance: a foreign-owned directory, or one already carrying
/// untrusted aces, must fail rather than be repaired into passing.
#[cfg(windows)]
fn validate_dacl_rules(
    view: &DaclView,
    user_sid: &str,
    dir: &Path,
) -> Result<(), OrgAuthorityError> {
    if view.null_dacl {
        return Err(OrgAuthorityError::InsecureAuthorityDir {
            path: dir.display().to_string(),
            reason: "authority directory has a NULL/absent DACL (grants everyone full access)"
                .to_string(),
        });
    }
    const LOCAL_SYSTEM: &str = "S-1-5-18";
    const ADMINISTRATORS: &str = "S-1-5-32-544";
    let trusted = |sid: &str| sid == user_sid || sid == LOCAL_SYSTEM || sid == ADMINISTRATORS;

    // §3 — OWNERSHIP FIRST. The owner holds implicit `WRITE_DAC` (and
    // `READ_CONTROL`) regardless of what the DACL says, absent an
    // `OWNER RIGHTS` ACE that nothing here requires. A foreign owner can
    // therefore rewrite the ACL after this validation returns, so no ACE
    // walk can substitute for this check. The Unix path already enforces the
    // equivalent — `authority_dir_policy_violation` refuses `owner_uid != euid`
    // before looking at mode bits.
    if !trusted(&view.owner_sid) {
        return Err(OrgAuthorityError::InsecureAuthorityDir {
            path: dir.display().to_string(),
            reason: format!(
                "authority directory is owned by untrusted principal {} — the owner holds \
                 implicit WRITE_DAC and can re-grant itself access at any time, so a \
                 restrictive DACL is not sufficient. Only the current user, SYSTEM, and \
                 Administrators are trusted owners",
                view.owner_sid
            ),
        });
    }

    for ace in &view.aces {
        // §4 — fail CLOSED on any ACE we could not fully parse.
        //
        // Type 0 (`ACCESS_ALLOWED_ACE`) is not the only access-GRANTING type:
        // `ACCESS_ALLOWED_OBJECT_ACE` (5), `ACCESS_ALLOWED_CALLBACK_ACE` (9),
        // and `ACCESS_ALLOWED_CALLBACK_OBJECT_ACE` (11) all grant, and their
        // SID does not sit at the fixed byte-8 offset the simple types use —
        // so `read_object_security` records the `NON_SIMPLE_ACE_SID` sentinel
        // for them rather than a real SID.
        //
        // An earlier revision skipped every `ace_type != 0`, which silently
        // dropped exactly those grants: a conditional ACE (SDDL `XA`) granting
        // Everyone full control under a tautological condition would pass
        // validation while Windows' own access check honored it. That also
        // contradicted `read_object_security`'s stated intent, which is to
        // record the sentinel and "let the validator treat a write-capable one
        // conservatively".
        //
        // So: skip only the DENY types (a deny can never broaden access), and
        // treat everything else as a grant. An unparsed SID can never match a
        // trusted principal, so a write-capable one refuses on the check below.
        const ACCESS_DENIED: u8 = 1;
        const ACCESS_DENIED_OBJECT: u8 = 6;
        const ACCESS_DENIED_CALLBACK: u8 = 10;
        const ACCESS_DENIED_CALLBACK_OBJECT: u8 = 12;
        if matches!(
            ace.ace_type,
            ACCESS_DENIED
                | ACCESS_DENIED_OBJECT
                | ACCESS_DENIED_CALLBACK
                | ACCESS_DENIED_CALLBACK_OBJECT
        ) {
            continue;
        }
        // Audit / alarm ACE types (2, 3, 7, 8, 13, 14, 15, 17…) live in the
        // SACL, not the DACL, so they should not appear here at all. If one
        // does, it is not something we understand — the checks below still
        // apply, which is the conservative outcome.

        // §20 — INHERITANCE FIRST, before any read/write distinction.
        //
        // The authority files are NOT given an explicit DACL on Windows:
        // `write_atomic_phased` sets `mode(0o600)` under `#[cfg(unix)]` only,
        // so on NTFS each file gets whatever it INHERITS from this directory.
        // An `OBJECT_INHERIT` ACE therefore propagates onto
        // `owner-audience.key` — the raw owner discovery key, which decrypts
        // every OwnerScoped announcement for the org.
        //
        // A read-only grant looked harmless and was skipped by the
        // write-capability check below, so a directory carrying
        // `(A;OICI;FR;;;WD)` validated, adopted, and handed Everyone read
        // access to the key. Verified on live NTFS: validator accepted,
        // adopt succeeded, Everyone could read the key file. The earlier
        // §3/§4 witnesses missed it because both used write-capable ACEs.
        //
        // So: any untrusted ACE that propagates to child OBJECTS is refused
        // whatever it grants. `CONTAINER_INHERIT` alone (subdirectories) is
        // covered by the same rule since the flags travel together in
        // practice and the authority dir has no legitimate subdirectories.
        const OBJECT_INHERIT_ACE: u8 = 0x01;
        const CONTAINER_INHERIT_ACE: u8 = 0x02;
        if ace.flags & (OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE) != 0 && !trusted(&ace.sid) {
            return Err(OrgAuthorityError::InsecureAuthorityDir {
                path: dir.display().to_string(),
                reason: format!(
                    "authority directory carries an INHERITABLE ace for untrusted principal \
                     {} (ace type {}, mask {:#010x}, flags {:#04x}) — authority files inherit \
                     this directory's ACL on Windows, so it would propagate onto \
                     {OWNER_AUDIENCE_FILE}. Only the owner, SYSTEM, and Administrators may \
                     hold an inheritable ace here, read-only or not",
                    ace.sid, ace.ace_type, ace.mask, ace.flags
                ),
            });
        }

        if ace.mask & WRITE_MASK == 0 {
            // A NON-inheriting read grant on the directory itself is
            // tolerated: it confers `FILE_LIST_DIRECTORY` and nothing more.
            // The authority file names are fixed compile-time constants
            // (`NodeAuthority::file_names`), so listing them discloses
            // nothing the attacker did not already know, and the ace cannot
            // reach the files' contents because it does not propagate.
            continue;
        }
        if !trusted(&ace.sid) {
            return Err(OrgAuthorityError::InsecureAuthorityDir {
                path: dir.display().to_string(),
                reason: format!(
                    "authority directory grants write access to untrusted principal {} \
                     (ace type {}, mask {:#010x}) — only the owner, SYSTEM, and \
                     Administrators are trusted",
                    ace.sid, ace.ace_type, ace.mask
                ),
            });
        }
    }
    Ok(())
}

/// Open, mode-check, and read the audience key through ONE
/// no-follow handle (review-9): symlinks and non-regular files are
/// refused, the ssh-style group/other gate (review-8 §10) runs on
/// the opened descriptor's metadata, and the bytes are read from
/// that same descriptor — no check-to-read window. `Ok(None)` when
/// the file does not exist.
fn read_audience_checked(path: &Path) -> Result<Option<Vec<u8>>, OrgAuthorityError> {
    use std::io::Read;
    let file = match super::org_revocation::open_regular_nofollow(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(OrgAuthorityError::Io {
                path: path.display().to_string(),
                reason: e.to_string(),
            })
        }
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = file.metadata().map_err(|e| OrgAuthorityError::Io {
            path: path.display().to_string(),
            reason: e.to_string(),
        })?;
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(OrgAuthorityError::PermissiveAudienceFile {
                path: path.display().to_string(),
                mode,
            });
        }
    }
    #[cfg(windows)]
    {
        // §11 — validate the key file's OWN descriptor, not just the
        // directory's.
        //
        // This was a bare `eprintln!` no-op, so the Unix invariant
        // ("creation-time 0600 is insufficient — re-check at every startup and
        // re-adoption", review-8 §10) had no Windows counterpart at all. The
        // directory was validated and the FILE never was, so an explicit,
        // non-inherited ace on `owner-audience.key` itself was invisible to
        // every check: an `icacls` mistake, a backup/restore tool, or the file
        // arriving from a share during the org-wide key distribution this
        // design REQUIRES.
        //
        // Strictly tighter than the directory rule, which tolerates a
        // read-only untrusted non-inheritable ace. For the raw owner discovery
        // key there is no harmless read — it decrypts every OwnerScoped
        // announcement for the org — so ANY untrusted grant refuses, whatever
        // its mask.
        validate_audience_file_acl(path)?;
    }
    #[cfg(not(any(unix, windows)))]
    {
        eprintln!(
            "warning: audience-key permission gate is a no-op on this platform; \
             ACLs on {} are not validated — manage them out-of-band.",
            path.display()
        );
    }
    let mut file = file;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|e| OrgAuthorityError::Io {
            path: path.display().to_string(),
            reason: e.to_string(),
        })?;
    Ok(Some(bytes))
}

/// OSDK-L R2 — load a GRANT-side [`OrgAudienceSecret`] from an out-of-band
/// 0600 file, validating the OPENED OBJECT rather than a pathname.
///
/// This is the loader `OrgAudienceSecret::decode_config`'s doc says does not
/// exist, and says why one that "merely wrapped this call would imply a safety
/// it does not provide". It exists now because the language SDKs need a way to
/// supply audience material WITHOUT the raw discovery key crossing an FFI
/// boundary into garbage-collected memory: the binding hands over a path, and
/// the key's whole lifetime stays inside Rust.
///
/// Structurally mirrors [`read_audience_checked`], which is the only correct
/// model in this tree, and adds the two things a grant-side path needs that the
/// authority directory got from `ensure_secure_authority_dir`:
///
/// 1. the ancestor chain is validated FIRST, because a grant secret may live
///    anywhere the operator put it rather than inside an already-policed
///    authority directory — an ancestor another account can write through lets
///    that account swap the entry before we ever open it;
/// 2. the read is EXACT and rejects trailing bytes, so a file that is a valid
///    secret followed by anything else is refused rather than silently
///    accepted.
///
/// Then, on the already-open descriptor: regular-file check, Unix mode from
/// `File::metadata` (never `metadata(path)` then `open(path)` — that pair is
/// the TOCTOU), and on Windows the file's OWN protected DACL (§11), because
/// org-wide key distribution means this file may arrive from a share or a
/// restore tool carrying an explicit ace the containing directory says nothing
/// about.
///
/// # Platform asymmetry, inherited deliberately
///
/// The ancestor walk is Unix-only, matching `ensure_secure_authority_dir`: on
/// Windows a pre-existing ancestor chain is not walked, because a writable
/// ancestor's owner could replace the entry and no child DACL can prevent it —
/// so the check would imply a guarantee it cannot make. Windows instead leans
/// on the file's own protected DACL (§11), which is the stronger check for the
/// object actually read. Operators placing grant secrets outside a
/// protected directory on Windows must manage that chain out of band, exactly
/// as the CLI already warns for custom authority paths.
///
/// # Secret handling
///
/// The file bytes land in a [`ScrubbedBytes`] that zeroes on EVERY exit —
/// return, `?`, or unwind — so the key never lingers in freed heap where a core
/// dump or swap could reach it. No error returned from here embeds file
/// content, and the error `Display` renders only the path.
///
/// # Residual (inherited, not multiplied)
///
/// `decode_config` returns `Self` by value, and a Rust move is a memcpy that
/// does not run `Drop` on the source, so that one hop strands a copy in a dead
/// stack frame (`org_grant.rs` §27/§29). This loader adds no further by-value
/// hop; closing the inherited one end to end needs an ownership refactor
/// through `OrgCredentials` and the audience registry, which OSDK-L does not
/// attempt.
pub fn load_grant_audience_secret(
    path: &Path,
) -> Result<super::org_grant::OrgAudienceSecret, OrgAuthorityError> {
    use std::io::Read;

    let encoded_size = super::org_grant::OrgAudienceSecret::ENCODED_SIZE;

    // 1. Ancestor chain FIRST — establish the path is in a trusted location
    //    before opening anything. A bare relative name has an empty parent, so
    //    resolve against the current directory the way `normalize_authority_dir`
    //    does, or the chain would go unchecked.
    #[cfg(unix)]
    {
        let parent = match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => std::env::current_dir().map_err(|e| OrgAuthorityError::Io {
                path: path.display().to_string(),
                reason: format!("resolve audience secret parent: {e}"),
            })?,
        };
        validate_unix_ancestor_chain(&parent)?;
    }

    // 2. Open without following symlinks / reparse points.
    let mut file = open_regular_nofollow(path).map_err(|e| OrgAuthorityError::Io {
        path: path.display().to_string(),
        reason: format!("open audience secret: {e}"),
    })?;

    // 3-4. Validate the ALREADY-OPEN object: a regular file, owner-only.
    let meta = file.metadata().map_err(|e| OrgAuthorityError::Io {
        path: path.display().to_string(),
        reason: format!("stat audience secret: {e}"),
    })?;
    if !meta.is_file() {
        return Err(OrgAuthorityError::Io {
            path: path.display().to_string(),
            reason: "audience secret is not a regular file".to_string(),
        });
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(OrgAuthorityError::PermissiveAudienceFile {
                path: path.display().to_string(),
                mode,
            });
        }
    }
    // 5. Windows: the file's own descriptor, not merely its directory.
    #[cfg(windows)]
    {
        validate_audience_file_acl(path)?;
    }

    // 6-7. Exact read into scrub-on-drop storage, then prove there is nothing
    //      after it. Reading one extra byte is how "exactly N" is checked
    //      without trusting the stat size, which can change under us.
    let mut buf = ScrubbedBytes(vec![0u8; encoded_size]);
    file.read_exact(&mut buf.0)
        .map_err(|e| OrgAuthorityError::Io {
            path: path.display().to_string(),
            reason: format!("audience secret is shorter than {encoded_size} bytes: {e}"),
        })?;
    let mut trailing = [0u8; 1];
    match file.read(&mut trailing) {
        Ok(0) => {}
        Ok(_) => {
            return Err(OrgAuthorityError::CorruptFile {
                path: path.display().to_string(),
                detail: format!("audience secret has trailing bytes after {encoded_size}"),
            })
        }
        Err(e) => {
            return Err(OrgAuthorityError::Io {
                path: path.display().to_string(),
                reason: format!("audience secret trailing-byte probe: {e}"),
            })
        }
    }

    // 8. Decode. `buf` scrubs on the way out of every branch below.
    super::org_grant::OrgAudienceSecret::decode_config(buf.as_slice()).map_err(|_| {
        // Deliberately does not forward the decode error's payload — nothing
        // derived from key bytes reaches a message.
        OrgAuthorityError::CorruptFile {
            path: path.display().to_string(),
            detail: "audience secret failed to decode".to_string(),
        }
    })
}

/// §11 — validate `owner-audience.key`'s OWN security descriptor on Windows.
///
/// The directory check ([`validate_existing_dir_dacl`]) governs what a NEW
/// file inherits; it says nothing about an ace written directly onto an
/// existing key file, and nothing about the file's owner. Both are reachable
/// without touching the directory at all.
///
/// The rule is deliberately stricter than the directory's: any grant to an
/// untrusted principal refuses regardless of mask. The directory tolerates a
/// read-only untrusted non-inheritable ace because reading a DIRECTORY leaks
/// only file names; reading THIS file leaks the raw owner discovery key, which
/// decrypts every OwnerScoped announcement for the org.
///
/// `SE_DACL_PROTECTED` is deliberately NOT required here, unlike on the
/// directory: a file provisioned inside a protected owner-only directory
/// correctly inherits that directory's ace and is normally unprotected. The
/// directory rule is what bounds inheritance; this rule bounds what is written
/// onto the file itself.
#[cfg(windows)]
fn validate_audience_file_acl(path: &Path) -> Result<(), OrgAuthorityError> {
    let view = read_object_security(path).map_err(|e| OrgAuthorityError::Io {
        path: path.display().to_string(),
        reason: format!("read audience key security descriptor: {e}"),
    })?;
    let user_sid = current_process_sid_string().map_err(|e| OrgAuthorityError::Io {
        path: path.display().to_string(),
        reason: format!("resolve current user SID: {e}"),
    })?;
    validate_audience_acl_view(&view, &user_sid, path)
}

/// The PURE half of [`validate_audience_file_acl`], split out for the same
/// reason as [`validate_dacl_view`]: the foreign-owner and untrusted-ace cases
/// cannot be produced end-to-end without a second account, so without this
/// seam they would ship untested.
#[cfg(windows)]
fn validate_audience_acl_view(
    view: &DaclView,
    user_sid: &str,
    path: &Path,
) -> Result<(), OrgAuthorityError> {
    if view.null_dacl {
        return Err(OrgAuthorityError::PermissiveAudienceAcl {
            path: path.display().to_string(),
            reason: "NULL/absent DACL grants everyone full access".to_string(),
        });
    }
    const LOCAL_SYSTEM: &str = "S-1-5-18";
    const ADMINISTRATORS: &str = "S-1-5-32-544";
    let trusted = |sid: &str| sid == user_sid || sid == LOCAL_SYSTEM || sid == ADMINISTRATORS;

    // Ownership first, for the same reason as the directory rule: the owner
    // holds implicit WRITE_DAC and can re-grant itself access after this
    // returns, so no ace walk can substitute for it.
    if !trusted(&view.owner_sid) {
        return Err(OrgAuthorityError::PermissiveAudienceAcl {
            path: path.display().to_string(),
            reason: format!(
                "owned by untrusted principal {} — the owner holds implicit WRITE_DAC and \
                 can re-grant itself read access at any time",
                view.owner_sid
            ),
        });
    }

    for ace in &view.aces {
        // Deny aces can never broaden access.
        const ACCESS_DENIED: u8 = 1;
        const ACCESS_DENIED_OBJECT: u8 = 6;
        const ACCESS_DENIED_CALLBACK: u8 = 10;
        const ACCESS_DENIED_CALLBACK_OBJECT: u8 = 12;
        if matches!(
            ace.ace_type,
            ACCESS_DENIED
                | ACCESS_DENIED_OBJECT
                | ACCESS_DENIED_CALLBACK
                | ACCESS_DENIED_CALLBACK_OBJECT
        ) {
            continue;
        }
        // Everything else grants. An unparsed (non-simple) ace carries the
        // sentinel SID, which can never compare equal to a trusted principal,
        // so it fails closed here exactly as it does on the directory.
        if !trusted(&ace.sid) {
            return Err(OrgAuthorityError::PermissiveAudienceAcl {
                path: path.display().to_string(),
                reason: format!(
                    "grants access to untrusted principal {} (ace type {}, mask {:#010x}); \
                     the raw owner discovery key must be readable only by its owner, \
                     SYSTEM, and Administrators",
                    ace.sid, ace.ace_type, ace.mask
                ),
            });
        }
    }
    Ok(())
}

/// No-follow optional read for the non-secret authority files
/// (membership): symlinks and non-regular files are refused
/// (review-9 filesystem policy).
fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, OrgAuthorityError> {
    match super::org_revocation::read_regular_nofollow(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(OrgAuthorityError::Io {
            path: path.display().to_string(),
            reason: e.to_string(),
        }),
    }
}

fn read_required(path: &Path) -> Result<Vec<u8>, OrgAuthorityError> {
    read_optional(path)?.ok_or_else(|| {
        let err = OrgAuthorityError::MissingFile {
            path: path.display().to_string(),
        };
        tracing::error!("{err}");
        err
    })
}

/// Convenience: the conventional authority directory under a
/// config root (`<config_root>/authority`). The CLI resolves the
/// config root; the core stays path-agnostic beyond this join.
pub fn authority_dir(config_root: &Path) -> PathBuf {
    config_root.join("authority")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::org::{OrgKeypair, OrgRevocationBundle};
    use crate::adapter::net::identity::EntityKeypair;
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEST_DIR_SEQ: AtomicUsize = AtomicUsize::new(0);

    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "net-org-authority-{}-{}",
                std::process::id(),
                TEST_DIR_SEQ.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir).expect("create scratch dir");
            Self(dir)
        }
        fn dir(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Run `body` in an ISOLATED child process so process-global state it
    /// mutates — the `umask` and the current directory — cannot contaminate the
    /// parallel unit-test suite. A shared in-process mutex would not suffice:
    /// it would only serialize tests that opt into it, while EVERY file-touching
    /// test in this binary observes the leaked mask / cwd. The parent re-execs
    /// THIS test binary filtered to exactly `test_path` with `ISOLATED_CHILD_ENV`
    /// set; the child (env present) runs `body` in its own process and reports
    /// pass/fail through its exit status (libtest exits non-zero if `body`
    /// panics). `test_path` MUST be the fully-qualified name of the calling
    /// `#[test]` so the child re-enters it. In the child this returns after
    /// `body`; in the parent it asserts the child exited 0, surfacing the
    /// child's captured stdout/stderr on failure.
    #[cfg(unix)]
    fn run_in_isolated_child(test_path: &str, body: impl FnOnce()) {
        const ISOLATED_CHILD_ENV: &str = "NET_AUTHORITY_ISOLATED_CHILD";
        if std::env::var_os(ISOLATED_CHILD_ENV).is_some() {
            body();
            return;
        }
        let exe = std::env::current_exe().expect("locate the running test binary");
        let out = std::process::Command::new(exe)
            .args(["--exact", "--nocapture", "--test-threads=1", test_path])
            .env(ISOLATED_CHILD_ENV, "1")
            .output()
            .expect("spawn isolated child test process");
        assert!(
            out.status.success(),
            "isolated child `{test_path}` failed (exit {:?})\n\
             --- child stdout ---\n{}\n--- child stderr ---\n{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
    }

    fn org() -> OrgKeypair {
        OrgKeypair::from_bytes([0x42u8; 32])
    }

    fn node_identity() -> EntityKeypair {
        EntityKeypair::from_bytes([0x24u8; 32])
    }

    fn cert_for(kp: &EntityKeypair, generation: u32) -> OrgMembershipCert {
        OrgMembershipCert::try_issue(&org(), kp.entity_id().clone(), generation, 3600)
            .expect("issue")
    }

    #[test]
    fn adopt_provisions_all_three_files_and_open_succeeds() {
        let scratch = Scratch::new();
        let kp = node_identity();

        let authority =
            NodeAuthority::adopt(scratch.dir(), cert_for(&kp, 1), kp.entity_id(), 0, None)
                .expect("adopt");
        assert_eq!(authority.owner_org(), org().org_id());
        for name in NodeAuthority::file_names() {
            assert!(
                scratch.dir().join(name).exists(),
                "{name} must exist after adopt"
            );
        }

        let opened = NodeAuthority::open(scratch.dir(), kp.entity_id()).expect("open");
        assert_eq!(opened.config, authority.config);
        assert_eq!(
            opened.audience.audience_handle,
            authority.audience.audience_handle
        );
        assert_eq!(
            opened.audience.discovery_key(),
            authority.audience.discovery_key()
        );
    }

    /// Review-9: the ceremony lock inode is held to the full
    /// regular-file policy — a planted FIFO refuses the ceremony
    /// (and cannot park the open waiting for a reader) instead of
    /// carrying the lock.
    #[cfg(unix)]
    #[test]
    fn non_regular_ceremony_lock_is_refused() {
        let scratch = Scratch::new();
        let kp = node_identity();
        let status = std::process::Command::new("mkfifo")
            .arg(scratch.dir().join("authority.lock"))
            .status()
            .expect("run mkfifo");
        assert!(status.success(), "mkfifo failed");

        let err = NodeAuthority::adopt(scratch.dir(), cert_for(&kp, 1), kp.entity_id(), 0, None)
            .expect_err("FIFO ceremony lock must refuse");
        assert!(matches!(err, OrgAuthorityError::Io { .. }), "got: {err}");
        assert!(
            !scratch.dir().join(OWNER_MEMBERSHIP_FILE).exists(),
            "refused ceremony must not publish membership"
        );
    }

    /// Review-9 addendum: an adoption interrupted AFTER durable
    /// state exists (floors, audience) but BEFORE the membership
    /// publication leaves a fail-closed, RESUMABLE scaffold —
    /// startup refuses it, a re-run completes it, and the durable
    /// state (floor maxima, audience credential) survives.
    #[test]
    fn interrupted_adoption_is_fail_closed_and_resumable() {
        let scratch = Scratch::new();
        let kp = node_identity();

        // Manufacture the partial scaffold deterministically: a
        // completed ceremony minus its membership publication — the
        // exact on-disk shape a crash (or a refused final
        // verification, cf. the racing-floor witness) leaves
        // behind.
        let first = NodeAuthority::adopt(scratch.dir(), cert_for(&kp, 1), kp.entity_id(), 0, None)
            .expect("adopt");
        let mut floors = BTreeMap::new();
        floors.insert(EntityId::from_bytes([9u8; 32]), 7u32);
        let bundle = OrgRevocationBundle::try_issue(&org(), &floors).expect("issue");
        first.revocation.apply_bundle(&bundle).expect("apply");
        let handle_before = first.audience.audience_handle;
        drop(first);
        std::fs::remove_file(scratch.dir().join(OWNER_MEMBERSHIP_FILE)).expect("interrupt");

        // Fail-closed: no membership → startup refuses → no
        // ownership is ever emitted from the partial scaffold.
        let err = NodeAuthority::open(scratch.dir(), kp.entity_id())
            .expect_err("partial scaffold must refuse startup");
        assert!(
            matches!(err, OrgAuthorityError::MissingFile { .. }),
            "got: {err}"
        );

        // Resumable: a re-run completes the ceremony, preserving
        // the audience credential and every persisted floor.
        let resumed =
            NodeAuthority::adopt(scratch.dir(), cert_for(&kp, 2), kp.entity_id(), 0, None)
                .expect("re-run completes the ceremony");
        assert_eq!(resumed.audience.audience_handle, handle_before);
        assert_eq!(
            resumed
                .revocation
                .floor_for(&org().org_id(), &EntityId::from_bytes([9u8; 32])),
            7,
            "monotone floor state survives the interruption"
        );
        NodeAuthority::open(scratch.dir(), kp.entity_id()).expect("startup succeeds after resume");
    }

    #[cfg(unix)]
    #[test]
    fn audience_key_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = Scratch::new();
        let kp = node_identity();
        NodeAuthority::adopt(scratch.dir(), cert_for(&kp, 1), kp.entity_id(), 0, None)
            .expect("adopt");
        let mode = std::fs::metadata(scratch.dir().join(OWNER_AUDIENCE_FILE))
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o077,
            0,
            "owner-audience.key must not be group/other readable (mode {mode:o})"
        );
    }

    /// Gate-1: the PURE authority-directory policy decision (owner must be the
    /// effective user; the directory must not be group/other-writable). A
    /// plain `u32` function, so it runs on every platform — the wrong-owner
    /// case cannot be produced from an integration test without root.
    #[test]
    fn authority_dir_policy_rejects_wrong_owner_and_world_writable() {
        // Owner == effective user, owner-only → accepted.
        assert_eq!(authority_dir_policy_violation(1000, 0o700, 1000), None);
        // Group/other READ (not write) is accepted; the scaffold tightens it.
        assert_eq!(authority_dir_policy_violation(1000, 0o755, 1000), None);
        // Wrong owner → refused.
        assert!(authority_dir_policy_violation(0, 0o700, 1000)
            .unwrap()
            .contains("uid 0"));
        // Group-writable → refused.
        assert!(authority_dir_policy_violation(1000, 0o770, 1000)
            .unwrap()
            .contains("group/other-writable"));
        // Other-writable → refused.
        assert!(authority_dir_policy_violation(1000, 0o707, 1000)
            .unwrap()
            .contains("group/other-writable"));
    }

    /// Gate-1: the PURE ancestor-policy decision (Unix). An ancestor must be
    /// owned by the effective user or root, and not group/other-writable
    /// without the sticky bit. Runs on every platform (plain u32 logic); the
    /// foreign-owned cases cannot be produced from an integration test without
    /// root.
    #[test]
    fn unix_ancestor_violation_covers_ownership_and_sticky() {
        // euid-owned, owner-only → accepted.
        assert_eq!(unix_ancestor_violation(1000, 0o755, 1000), None);
        // root-owned (uid 0) → accepted, even world-writable + sticky (/tmp).
        assert_eq!(unix_ancestor_violation(0, 0o1777, 1000), None);
        // current-user-owned, sticky + writable → accepted.
        assert_eq!(unix_ancestor_violation(1000, 0o1777, 1000), None);
        // Foreign-owned non-root → refused, even at a tame 0755.
        assert!(unix_ancestor_violation(1234, 0o755, 1000)
            .unwrap()
            .contains("uid 1234"));
        // Foreign-owned + sticky → still refused (sticky does not bind the owner).
        assert!(unix_ancestor_violation(1234, 0o1777, 1000)
            .unwrap()
            .contains("uid 1234"));
        // euid-owned but group/other-writable non-sticky → refused.
        assert!(unix_ancestor_violation(1000, 0o0777, 1000)
            .unwrap()
            .contains("sticky"));
    }

    /// Gate-1 (Unix): adopting into a MISSING authority directory creates it
    /// owner-only (0700), and the provisioned membership + state files are
    /// owner-only (0600). Red-witness: reverting the `DirBuilder::mode(0o700)`
    /// create to a plain `create_dir_all` leaves the dir at the umask default.
    #[cfg(unix)]
    #[test]
    fn adopt_creates_owner_only_authority_dir_and_files() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = Scratch::new();
        let kp = node_identity();
        // A fresh authority subdir that does NOT exist yet.
        let authority_dir = scratch.dir().join("authority");
        NodeAuthority::adopt(&authority_dir, cert_for(&kp, 1), kp.entity_id(), 0, None)
            .expect("adopt");
        let dir_mode = std::fs::metadata(&authority_dir)
            .expect("dir metadata")
            .permissions()
            .mode();
        assert_eq!(
            dir_mode & 0o777,
            0o700,
            "authority dir must be owner-only 0700 (mode {dir_mode:o})",
        );
        for name in [
            OWNER_MEMBERSHIP_FILE,
            REVOCATION_STATE_FILE,
            OWNER_AUDIENCE_FILE,
        ] {
            let mode = std::fs::metadata(authority_dir.join(name))
                .expect("file metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0, "{name} must be owner-only (mode {mode:o})");
        }
    }

    /// Gate-1 (Unix): adoption refuses an EXISTING authority directory that is
    /// group/other-writable — the trusted-boundary precondition does not hold,
    /// so no secrets are provisioned into it.
    #[cfg(unix)]
    #[test]
    fn adopt_refuses_group_or_other_writable_authority_dir() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = Scratch::new();
        let kp = node_identity();
        // The scratch dir already exists; make it world-writable.
        std::fs::set_permissions(scratch.dir(), std::fs::Permissions::from_mode(0o777))
            .expect("chmod 0777");
        let err = NodeAuthority::adopt(scratch.dir(), cert_for(&kp, 1), kp.entity_id(), 0, None)
            .expect_err("a group/other-writable authority dir must be refused");
        assert!(
            matches!(
                &err,
                OrgAuthorityError::InsecureAuthorityDir { reason, .. }
                    if reason.contains("group/other-writable")
            ),
            "got: {err}",
        );
        // Restore owner-only so Scratch::drop can clean up.
        let _ = std::fs::set_permissions(scratch.dir(), std::fs::Permissions::from_mode(0o700));
    }

    /// Gate-1 (Unix): adoption into an owner-only authority dir is refused
    /// when an ANCESTOR is group/other-writable and NOT sticky — another
    /// account could rename the owned directory's entry through that parent
    /// and plant a replacement.
    #[cfg(unix)]
    #[test]
    fn adopt_refuses_writable_nonsticky_ancestor() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = Scratch::new();
        let shared = scratch.dir().join("shared");
        std::fs::create_dir_all(&shared).expect("mkdir shared");
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o0777))
            .expect("chmod 0777 non-sticky");
        let kp = node_identity();
        let err = NodeAuthority::adopt(
            &shared.join("authority"),
            cert_for(&kp, 1),
            kp.entity_id(),
            0,
            None,
        )
        .expect_err("a writable-nonsticky ancestor must be refused");
        assert!(
            matches!(
                &err,
                OrgAuthorityError::InsecureAuthorityDir { reason, .. } if reason.contains("sticky")
            ),
            "got: {err}",
        );
        let _ = std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o0755));
    }

    /// Gate-1 (Unix): a group/other-writable ancestor WITH the sticky bit
    /// (e.g. `/tmp` at 01777) is accepted when the owned child is created
    /// there — sticky forbids a non-owner from renaming the owned entry.
    #[cfg(unix)]
    #[test]
    fn adopt_accepts_sticky_writable_ancestor() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = Scratch::new();
        let shared = scratch.dir().join("sticky-shared");
        std::fs::create_dir_all(&shared).expect("mkdir");
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o1777))
            .expect("chmod 1777 sticky");
        let kp = node_identity();
        NodeAuthority::adopt(
            &shared.join("authority"),
            cert_for(&kp, 1),
            kp.entity_id(),
            0,
            None,
        )
        .expect("a sticky writable ancestor with an owned child is accepted");
        let _ = std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o0755));
    }

    /// Gate-1 (Unix): a SYMLINKED parent component is resolved before the
    /// ancestor walk, so an authority path that resolves through an insecure
    /// (writable-nonsticky) real ancestor is refused.
    #[cfg(unix)]
    #[test]
    fn adopt_refuses_symlinked_insecure_ancestor() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = Scratch::new();
        let insecure = scratch.dir().join("insecure");
        std::fs::create_dir_all(&insecure).expect("mkdir insecure");
        std::fs::set_permissions(&insecure, std::fs::Permissions::from_mode(0o0777))
            .expect("chmod 0777");
        let link = scratch.dir().join("link");
        std::os::unix::fs::symlink(&insecure, &link).expect("symlink");
        let kp = node_identity();
        let err = NodeAuthority::adopt(
            &link.join("authority"),
            cert_for(&kp, 1),
            kp.entity_id(),
            0,
            None,
        )
        .expect_err("a symlinked insecure ancestor must be refused");
        assert!(
            matches!(&err, OrgAuthorityError::InsecureAuthorityDir { .. }),
            "got: {err}",
        );
        let _ = std::fs::set_permissions(&insecure, std::fs::Permissions::from_mode(0o0755));
    }

    /// Gate-1 (all platforms): adopting into a MISSING nested authority
    /// directory creates it and provisions all three files. Exercises the
    /// create path (Unix atomic-0700 create; Windows create + owner-only
    /// DACL).
    #[test]
    fn adopt_into_a_missing_subdir_creates_the_authority_dir() {
        let scratch = Scratch::new();
        let kp = node_identity();
        let authority = scratch.dir().join("nested").join("authority");
        NodeAuthority::adopt(&authority, cert_for(&kp, 1), kp.entity_id(), 0, None)
            .expect("adopt into a fresh subdir");
        for name in NodeAuthority::file_names() {
            assert!(authority.join(name).exists(), "{name} must be provisioned");
        }
    }

    /// Gate-1 (Unix): adopting into a MISSING nested chain creates every
    /// intermediate parent owner-only (0700), even under a permissive umask —
    /// a naive create_dir_all would leave 0777 intermediates through which
    /// another account could later replace the final directory.
    ///
    /// Runs in an ISOLATED child process: `umask` is process-global, so setting
    /// it in the shared parallel test process contaminated every concurrently
    /// running file-creating test (observed as spurious 022-mode failures). The
    /// child sets the permissive mask, never restores it, and exits.
    #[cfg(unix)]
    #[test]
    fn adopt_creates_intermediate_parents_owner_only_under_permissive_umask() {
        run_in_isolated_child(
            "adapter::net::behavior::org_authority::tests::\
             adopt_creates_intermediate_parents_owner_only_under_permissive_umask",
            || {
                use std::os::unix::fs::PermissionsExt;
                let scratch = Scratch::new();
                // SAFETY: `umask` has no preconditions and only affects THIS
                // (single-threaded) child process; it is deliberately never
                // restored because the child exits right after the assertions.
                unsafe { libc::umask(0) };
                let a = scratch.dir().join("a");
                let b = a.join("b");
                let authority = b.join("authority");
                let kp = node_identity();
                NodeAuthority::adopt(&authority, cert_for(&kp, 1), kp.entity_id(), 0, None)
                    .expect("adopt creates a nested chain securely");
                for comp in [&a, &b, &authority] {
                    let mode = std::fs::metadata(comp)
                        .expect("metadata")
                        .permissions()
                        .mode();
                    assert_eq!(
                        mode & 0o077,
                        0,
                        "{} must be owner-only (mode {mode:o})",
                        comp.display()
                    );
                }
            },
        );
    }

    /// Gate-1 (Unix): a bare relative authority path is resolved against the
    /// current directory and adopted when that directory (and its ancestors)
    /// are secure. A bare relative name has an empty `parent()`, so this proves
    /// normalization runs and the resolved ancestor chain is actually checked.
    /// Isolated child: it mutates the process current directory.
    #[cfg(unix)]
    #[test]
    fn adopt_resolves_secure_relative_path_against_cwd() {
        run_in_isolated_child(
            "adapter::net::behavior::org_authority::tests::\
             adopt_resolves_secure_relative_path_against_cwd",
            || {
                use std::os::unix::fs::PermissionsExt;
                let scratch = Scratch::new();
                // Secure cwd: owner-only, euid-owned; the temp root is trusted.
                std::fs::set_permissions(scratch.dir(), std::fs::Permissions::from_mode(0o700))
                    .expect("chmod 0700");
                std::env::set_current_dir(scratch.dir()).expect("set cwd");
                let kp = node_identity();
                NodeAuthority::adopt(
                    Path::new("authority"),
                    cert_for(&kp, 1),
                    kp.entity_id(),
                    0,
                    None,
                )
                .expect("a relative authority path under a secure cwd must adopt");
                assert!(
                    scratch
                        .dir()
                        .join("authority")
                        .join(OWNER_MEMBERSHIP_FILE)
                        .exists(),
                    "the authority dir must be created under the resolved cwd",
                );
            },
        );
    }

    /// Gate-1 (Unix): a bare relative authority path under a HOSTILE current
    /// directory (group/other-writable without the sticky bit — another account
    /// could rename the authority entry through it) is refused, and nothing is
    /// provisioned. Isolated child: it mutates the process current directory.
    #[cfg(unix)]
    #[test]
    fn adopt_refuses_relative_path_under_writable_nonsticky_cwd() {
        run_in_isolated_child(
            "adapter::net::behavior::org_authority::tests::\
             adopt_refuses_relative_path_under_writable_nonsticky_cwd",
            || {
                use std::os::unix::fs::PermissionsExt;
                let scratch = Scratch::new();
                std::fs::set_permissions(scratch.dir(), std::fs::Permissions::from_mode(0o777))
                    .expect("chmod 0777");
                std::env::set_current_dir(scratch.dir()).expect("set cwd");
                let kp = node_identity();
                let err = NodeAuthority::adopt(
                    Path::new("authority"),
                    cert_for(&kp, 1),
                    kp.entity_id(),
                    0,
                    None,
                )
                .expect_err("a relative path under a writable-nonsticky cwd must be refused");
                assert!(
                    matches!(&err, OrgAuthorityError::InsecureAuthorityDir { .. }),
                    "got: {err}",
                );
                assert!(
                    !scratch.dir().join("authority").exists(),
                    "no authority dir may be created when the cwd chain is refused",
                );
            },
        );
    }

    /// Gate-1 (Unix): a relative authority path whose resolved cwd sits BENEATH
    /// a foreign-owned (non-root) ancestor is refused — that owner could replace
    /// the directory entry through the ancestor. Requires root to create the
    /// foreign-owned ancestor; a no-op otherwise. Isolated child: it mutates the
    /// process current directory.
    #[cfg(unix)]
    #[test]
    fn adopt_refuses_relative_path_beneath_foreign_owned_ancestor() {
        run_in_isolated_child(
            "adapter::net::behavior::org_authority::tests::\
             adopt_refuses_relative_path_beneath_foreign_owned_ancestor",
            || {
                use std::os::unix::ffi::OsStrExt;
                use std::os::unix::fs::DirBuilderExt;
                // SAFETY: geteuid() has no preconditions and cannot fail.
                if unsafe { libc::geteuid() } != 0 {
                    eprintln!("skipped: requires root to create a foreign-owned ancestor");
                    return;
                }
                let scratch = Scratch::new(); // root-owned
                let foreign = scratch.dir().join("foreign");
                std::fs::DirBuilder::new()
                    .mode(0o755)
                    .create(&foreign)
                    .expect("mkdir foreign");
                let cpath =
                    std::ffi::CString::new(foreign.as_os_str().as_bytes()).expect("cstring");
                // SAFETY: `cpath` is a valid NUL-terminated path; chown touches
                // no Rust memory and its result is checked.
                let rc = unsafe { libc::chown(cpath.as_ptr(), 12345, 12345) };
                assert_eq!(rc, 0, "chown to a foreign uid must succeed as root");
                let work = foreign.join("work");
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&work)
                    .expect("mkdir work");
                std::env::set_current_dir(&work).expect("set cwd");
                let kp = node_identity();
                let err = NodeAuthority::adopt(
                    Path::new("authority"),
                    cert_for(&kp, 1),
                    kp.entity_id(),
                    0,
                    None,
                )
                .expect_err("a relative path beneath a foreign-owned ancestor must be refused");
                assert!(
                    matches!(&err, OrgAuthorityError::InsecureAuthorityDir { .. }),
                    "got: {err}",
                );
            },
        );
    }

    /// Gate-1 (Unix): a relative MISSING nested chain is created owner-only
    /// (0700) under a secure cwd and re-validated — proving normalization feeds
    /// the secure-creation path for relative inputs too. Isolated child: it
    /// mutates the process current directory and umask.
    #[cfg(unix)]
    #[test]
    fn adopt_creates_relative_nested_missing_chain_owner_only() {
        run_in_isolated_child(
            "adapter::net::behavior::org_authority::tests::\
             adopt_creates_relative_nested_missing_chain_owner_only",
            || {
                use std::os::unix::fs::PermissionsExt;
                let scratch = Scratch::new();
                std::fs::set_permissions(scratch.dir(), std::fs::Permissions::from_mode(0o700))
                    .expect("chmod 0700");
                std::env::set_current_dir(scratch.dir()).expect("set cwd");
                // SAFETY: single-threaded child; the permissive mask proves the
                // scaffold forces 0700 on every created relative component.
                unsafe { libc::umask(0) };
                let kp = node_identity();
                NodeAuthority::adopt(
                    Path::new("nested/authority"),
                    cert_for(&kp, 1),
                    kp.entity_id(),
                    0,
                    None,
                )
                .expect("a relative nested chain under a secure cwd must adopt");
                for rel in ["nested", "nested/authority"] {
                    let mode = std::fs::metadata(scratch.dir().join(rel))
                        .expect("metadata")
                        .permissions()
                        .mode();
                    assert_eq!(mode & 0o077, 0, "{rel} must be owner-only (mode {mode:o})");
                }
                assert!(scratch
                    .dir()
                    .join("nested/authority")
                    .join(OWNER_MEMBERSHIP_FILE)
                    .exists());
            },
        );
    }

    /// Gate-1 (Unix): a FINAL authority component that is a symlink is refused —
    /// including with a TRAILING SEPARATOR. `symlink_metadata("link/")` follows
    /// the link and reports its (attacker-chosen) target directory, which would
    /// slip past the "not a directory" refusal and redirect authority I/O into
    /// the target; `symlink_metadata("link")` reports the link. Normalization
    /// strips the trailing separator so BOTH forms hit the refusal. Uses
    /// absolute paths (no cwd change), so it needs no isolated child.
    #[cfg(unix)]
    #[test]
    fn adopt_refuses_final_symlink_even_with_trailing_separator() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = Scratch::new();
        // Clean, owner-only ancestor so the ONLY possible refusal is the symlink.
        std::fs::set_permissions(scratch.dir(), std::fs::Permissions::from_mode(0o700))
            .expect("chmod 0700");
        let target = scratch.dir().join("target");
        std::fs::create_dir(&target).expect("mkdir target");
        let link = scratch.dir().join("link");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        let kp = node_identity();

        let bare = NodeAuthority::adopt(&link, cert_for(&kp, 1), kp.entity_id(), 0, None)
            .expect_err("a final-symlink authority dir must be refused");
        assert!(
            matches!(&bare, OrgAuthorityError::InsecureAuthorityDir { .. }),
            "got: {bare}",
        );

        let mut trailing = link.clone().into_os_string();
        trailing.push("/");
        let slashed = NodeAuthority::adopt(
            Path::new(&trailing),
            cert_for(&kp, 1),
            kp.entity_id(),
            0,
            None,
        )
        .expect_err("a final-symlink authority dir with a trailing '/' must be refused");
        assert!(
            matches!(&slashed, OrgAuthorityError::InsecureAuthorityDir { .. }),
            "got: {slashed}",
        );

        assert!(
            !target.join(OWNER_MEMBERSHIP_FILE).exists(),
            "refusing the symlink must not provision into its target",
        );
    }

    /// Gate-1 (Windows): a freshly-created authority directory carries a
    /// PROTECTED, owner-only DACL — granted to the process token SID with full
    /// control and object+container inheritance — and both a probe file created
    /// directly in it and the provisioned authority files are owner-only, with
    /// no write-capable ACE for any broad principal. Inspects the BINARY security
    /// descriptor via `read_object_security` (`GetFileSecurityW` + `GetAce`),
    /// never localizable icacls text.
    #[cfg(windows)]
    #[test]
    fn adopt_windows_authority_dir_and_files_are_owner_only() {
        const OI: u8 = 0x01;
        const CI: u8 = 0x02;
        const FILE_ALL_ACCESS: u32 = 0x001F_01FF;
        const LOCAL_SYSTEM: &str = "S-1-5-18";
        const ADMINISTRATORS: &str = "S-1-5-32-544";

        let scratch = Scratch::new();
        let kp = node_identity();
        let authority = scratch.dir().join("nested").join("authority");
        NodeAuthority::adopt(&authority, cert_for(&kp, 1), kp.entity_id(), 0, None).expect("adopt");

        let user = current_process_sid_string().expect("user sid");
        let trusted = |sid: &str| sid == user || sid == LOCAL_SYSTEM || sid == ADMINISTRATORS;

        // Directory: protected DACL, owner is a trusted principal, one
        // full-control inheritable ACE for the owner, and NO write-capable grant
        // to a non-trusted principal.
        let dir_view = read_object_security(&authority).expect("read dir sd");
        assert!(!dir_view.null_dacl, "dir DACL must not be NULL");
        assert!(
            dir_view.protected,
            "dir DACL must be protected (parent inheritance stripped)"
        );
        assert!(
            trusted(&dir_view.owner_sid),
            "dir owner must be a trusted principal, got {}",
            dir_view.owner_sid
        );
        let owner_ace = dir_view
            .aces
            .iter()
            .find(|a| a.ace_type == 0 && a.sid == user)
            .expect("dir must carry an allowed ACE for the owner");
        assert_eq!(
            owner_ace.mask & FILE_ALL_ACCESS,
            FILE_ALL_ACCESS,
            "owner ACE must grant full control"
        );
        assert_eq!(
            owner_ace.flags & (OI | CI),
            OI | CI,
            "owner ACE must be object+container inheritable"
        );
        for ace in &dir_view.aces {
            if ace.ace_type == 0 && ace.mask & WRITE_MASK != 0 {
                assert!(
                    trusted(&ace.sid),
                    "dir grants write to non-trusted {}",
                    ace.sid
                );
            }
        }

        // Child files are owner-only. `owner_only` asserts the SECURITY property
        // — the owner holds a full-control ACE and NO non-trusted principal has
        // any write-capable ACE — rather than the inheritance-model-dependent
        // binary INHERITED_ACE flag (legacy CreateFile inheritance copies the
        // ACE down flagless even though icacls renders it `(I)`).
        let owner_only = |view: &DaclView| -> bool {
            let owner_full = view.aces.iter().any(|a| {
                a.ace_type == 0 && a.sid == user && a.mask & FILE_ALL_ACCESS == FILE_ALL_ACCESS
            });
            let no_foreign_write = view
                .aces
                .iter()
                .all(|a| a.ace_type != 0 || a.mask & WRITE_MASK == 0 || trusted(&a.sid));
            !view.null_dacl && owner_full && no_foreign_write
        };

        // A file created DIRECTLY in the protected dir is owner-only — it can be
        // so only by inheriting the dir's owner-only ACE (a non-inheriting create
        // would pick up the token default DACL).
        let probe = authority.join("probe");
        std::fs::write(&probe, b"x").expect("write probe file");
        let probe_view = read_object_security(&probe).expect("read probe sd");
        assert!(
            owner_only(&probe_view),
            "a file created in the protected dir must be owner-only; got {:?}",
            probe_view.aces,
        );

        // Every provisioned authority file is likewise owner-only (`write_atomic`
        // gives each an explicit owner-only DACL via `create_new` + rename).
        let file_view =
            read_object_security(&authority.join(OWNER_MEMBERSHIP_FILE)).expect("read file sd");
        assert!(
            owner_only(&file_view),
            "a provisioned authority file must be owner-only; got {:?}",
            file_view.aces,
        );
    }

    /// Gate-1 (Windows): an EXISTING authority directory is re-validated against
    /// its BINARY DACL — `validate_existing_dir_dacl` ACCEPTS an owner-only
    /// directory and REJECTS one that grants a broad principal (Everyone) write
    /// access, both directly and through the full `adopt` ceremony. Proves a
    /// pre-existing directory is checked, not adopted on trust.
    #[cfg(windows)]
    #[test]
    fn existing_windows_dir_dacl_is_revalidated_binary() {
        let scratch = Scratch::new();
        let kp = node_identity();

        // Accept: adopt builds a protected owner-only dir; re-validation passes.
        let ok_dir = scratch.dir().join("secure");
        NodeAuthority::adopt(&ok_dir, cert_for(&kp, 1), kp.entity_id(), 0, None)
            .expect("adopt secure");
        validate_existing_dir_dacl(&ok_dir).expect("an owner-only dir must validate");

        // Reject: a directory granting Everyone (S-1-1-0) full control fails
        // closed — directly and through adopt.
        let bad_dir = scratch.dir().join("permissive");
        std::fs::create_dir(&bad_dir).expect("mkdir permissive");
        let status = std::process::Command::new("icacls")
            .arg(&bad_dir)
            .arg("/grant")
            .arg("*S-1-1-0:(OI)(CI)F") // Everyone, by SID
            .status()
            .expect("run icacls");
        assert!(status.success(), "icacls grant Everyone must succeed");
        let err = validate_existing_dir_dacl(&bad_dir)
            .expect_err("a dir granting Everyone write must be refused");
        assert!(
            matches!(&err, OrgAuthorityError::InsecureAuthorityDir { .. }),
            "got: {err}",
        );
        let adopt_err = NodeAuthority::adopt(&bad_dir, cert_for(&kp, 2), kp.entity_id(), 0, None)
            .expect_err("adopt into an Everyone-writable dir must be refused");
        assert!(
            matches!(&adopt_err, OrgAuthorityError::InsecureAuthorityDir { .. }),
            "got: {adopt_err}",
        );
    }

    /// Test-only: apply an SDDL DACL string to `path` through the Win32
    /// security APIs (`ConvertStringSecurityDescriptorToSecurityDescriptorW`
    /// + `SetFileSecurityW`).
    ///
    /// Needed because the ACE shapes these witnesses must produce — object and
    /// callback (conditional) ACEs — have no `icacls` syntax, and PowerShell's
    /// `Set-Acl` lives in a module that is not autoloadable in every
    /// environment. Going straight to the API also matches the rule the
    /// production reader follows: binary security APIs, never localizable
    /// text.
    #[cfg(windows)]
    #[allow(clippy::multiple_unsafe_ops_per_block)]
    fn apply_sddl(path: &Path, sddl: &str) -> std::io::Result<()> {
        use std::os::windows::ffi::OsStrExt;
        type Pv = *mut std::ffi::c_void;
        extern "system" {
            fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl: *const u16,
                revision: u32,
                sd: *mut Pv,
                size: *mut u32,
            ) -> i32;
            fn SetFileSecurityW(name: *const u16, info: u32, sd: Pv) -> i32;
            fn LocalFree(mem: Pv) -> Pv;
        }
        const SDDL_REVISION_1: u32 = 1;
        const DACL_SECURITY_INFORMATION: u32 = 0x0000_0004;
        const PROTECTED_DACL_SECURITY_INFORMATION: u32 = 0x8000_0000;

        let mut wide_path: Vec<u16> = path.as_os_str().encode_wide().collect();
        wide_path.push(0);
        let mut wide_sddl: Vec<u16> = std::ffi::OsStr::new(sddl).encode_wide().collect();
        wide_sddl.push(0);

        // SAFETY: both buffers are valid NUL-terminated UTF-16. The convert
        // call allocates a self-relative descriptor with LocalAlloc, which we
        // free with LocalFree on every path. `sd` is only dereferenced by
        // SetFileSecurityW between those two points, and both return values
        // are checked.
        unsafe {
            let mut sd: Pv = std::ptr::null_mut();
            if ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide_sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                std::ptr::null_mut(),
            ) == 0
            {
                return Err(std::io::Error::last_os_error());
            }
            let ok = SetFileSecurityW(
                wide_path.as_ptr(),
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                sd,
            );
            LocalFree(sd);
            if ok == 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        Ok(())
    }

    /// §3 (Windows): a directory owned by an untrusted principal must fail
    /// closed even when its DACL names ONLY the current user.
    ///
    /// On Windows an object's owner is implicitly granted `READ_CONTROL` and
    /// `WRITE_DAC` on every access check unless an `OWNER RIGHTS` (`S-1-3-4`)
    /// ACE is present, and nothing here requires one. So a foreign owner can
    /// hand us a directory whose ACL looks perfect, wait for `adopt` to
    /// provision `owner-audience.key` into it, then rewrite the ACL and read
    /// the raw owner discovery key — which decrypts every `OwnerScoped`
    /// announcement for the org.
    ///
    /// The realistic setup is `C:\ProgramData`, which by default grants
    /// `BUILTIN\Users` create-folder and `CREATOR OWNER` full control on
    /// subfolders: any low-privileged user can pre-create the directory and
    /// is then its owner.
    ///
    /// Driven through [`validate_dacl_view`] with a synthetic descriptor,
    /// because producing a genuinely foreign-owned directory needs a second
    /// account and elevation that CI does not have. The ACE list here is
    /// deliberately CLEAN — the only thing wrong is the owner — so a pass
    /// cannot be attributed to the ACE walk.
    ///
    /// Red-witness: deleting the owner check makes this validate.
    #[cfg(windows)]
    #[test]
    fn a_foreign_owned_dir_fails_closed_despite_a_clean_dacl() {
        const FILE_ALL_ACCESS: u32 = 0x001F_01FF;
        let user = current_process_sid_string().expect("user sid");
        // A well-known SID that is never the current user, SYSTEM, or
        // Administrators: "Guests" (S-1-5-32-546).
        let foreign = "S-1-5-32-546";
        assert_ne!(user, foreign, "fixture SID must not be the test principal");

        let clean_ace = AceInfo {
            sid: user.clone(),
            mask: FILE_ALL_ACCESS,
            ace_type: 0,
            flags: 0x03, // OI|CI
        };
        let dir = Path::new("C:\\ProgramData\\net-authority");

        // Same DACL, trusted owner -> accepted. Establishes that the ACE list
        // below is not itself the reason for the refusal.
        let owned_by_us = DaclView {
            owner_sid: user.clone(),
            protected: true,
            null_dacl: false,
            aces: vec![clean_ace.clone()],
        };
        validate_dacl_view(&owned_by_us, &user, dir)
            .expect("an owner-only dir owned by the current user validates");

        // Only the owner differs.
        let owned_by_foreign = DaclView {
            owner_sid: foreign.to_string(),
            protected: true,
            null_dacl: false,
            aces: vec![clean_ace],
        };
        let err = validate_dacl_view(&owned_by_foreign, &user, dir)
            .expect_err("a foreign-owned authority directory must be refused");
        match &err {
            OrgAuthorityError::InsecureAuthorityDir { reason, .. } => assert!(
                reason.contains("owned by untrusted principal") && reason.contains(foreign),
                "the refusal must name ownership as the cause and identify the owner; got: {reason}",
            ),
            other => panic!("wrong error variant: {other}"),
        }
    }

    /// §12 — the Windows ancestor walk runs, accepts a normal chain, and
    /// refuses a foreign-owned component.
    ///
    /// The refusal half cannot be produced end-to-end without a second user
    /// account, so it is driven through the same pure-view seam the §3 owner
    /// rule uses. The ACCEPT half is live: every test in this file adopts under
    /// `%TEMP%`, whose chain is `C:\Users\<me>\AppData\Local\Temp` — a real,
    /// multi-component, user-and-system-owned path. If the walk were too
    /// strict (or the trusted set too narrow), all 30+ of them would fail
    /// rather than this one, which is a stronger signal than any single
    /// assertion here.
    #[cfg(windows)]
    #[test]
    fn windows_ancestor_chain_accepts_a_normal_path() {
        let scratch = Scratch::new();
        let nested = scratch.dir().join("a").join("b");

        // A chain rooted in the user's own profile validates, including the
        // not-yet-created components.
        validate_windows_ancestor_chain(&nested)
            .expect("a user-owned ancestor chain must validate");

        // And once created, it still validates — the post-creation re-check in
        // `ensure_secure_authority_dir` depends on this.
        std::fs::create_dir_all(&nested).expect("create nested");
        validate_windows_ancestor_chain(&nested).expect("the completed chain must validate too");
    }

    /// The refusal half of §12, driven purely: a foreign-owned ancestor is
    /// rejected because that owner holds implicit WRITE_DAC over the component
    /// and can remove the authority directory's entry through it — whatever
    /// the authority directory's own ACL says.
    ///
    /// This mirrors `validate_dacl_view`'s §3 rule one level up, and the
    /// two views below differ ONLY in the owner, so the refusal is
    /// attributable to ownership alone.
    #[cfg(windows)]
    #[test]
    fn a_foreign_owned_ancestor_is_refused() {
        let user = current_process_sid_string().expect("user sid");
        let foreign = "S-1-5-21-1111111111-2222222222-3333333333-1001";
        let dir = Path::new("C:\\ProgramData\\net-authority");
        let ace = AceInfo {
            sid: user.clone(),
            mask: 0x001F_01FF,
            ace_type: 0,
            flags: 0x03,
        };

        // Owner trusted -> the ACE list is not itself a cause for refusal.
        let ours = DaclView {
            owner_sid: user.clone(),
            protected: true,
            null_dacl: false,
            aces: vec![ace.clone()],
        };
        validate_dacl_view(&ours, &user, dir).expect("a user-owned component validates");

        // Only the owner differs.
        let theirs = DaclView {
            owner_sid: foreign.to_string(),
            protected: true,
            null_dacl: false,
            aces: vec![ace],
        };
        let err = validate_dacl_view(&theirs, &user, dir)
            .expect_err("a foreign-owned component must be refused");
        match &err {
            OrgAuthorityError::InsecureAuthorityDir { reason, .. } => assert!(
                reason.contains("WRITE_DAC") && reason.contains(foreign),
                "the refusal must explain WHY ownership is the criterion; got: {reason}",
            ),
            other => panic!("wrong error variant: {other}"),
        }
    }
    /// §20 residual, LIVE on NTFS: adopting into a pre-existing directory that
    /// inherits from its parent SEVERS that inheritance, so an ancestor cannot
    /// later propagate access onto the authority files.
    ///
    /// The synthetic test below pins the rule; this pins that the repair
    /// actually runs and takes effect against the real Win32 APIs. `mkdir`
    /// (here `create_dir_all`) produces exactly the unprotected shape an
    /// operator gets from `mkdir ~/net-authority` or from config management.
    #[cfg(windows)]
    #[test]
    fn adopting_into_an_inheriting_directory_severs_inheritance() {
        let scratch = Scratch::new();
        let dir = scratch.dir().join("pre-existing");
        std::fs::create_dir_all(&dir).expect("plain mkdir");

        // Precondition: a plainly-created directory inherits — otherwise this
        // test would prove nothing about the repair.
        let before = read_object_security(&dir).expect("read before");
        assert!(
            !before.protected,
            "precondition: a plainly-created directory must be unprotected, \
             else the repair under test is never exercised",
        );

        let kp = node_identity();
        let authority = NodeAuthority::adopt(&dir, cert_for(&kp, 1), kp.entity_id(), 0, None)
            .expect("adopt into a pre-existing inheriting directory must succeed");
        assert_eq!(authority.owner_org(), org().org_id());

        let after = read_object_security(&dir).expect("read after");
        assert!(
            after.protected,
            "adopt must sever inheritance on the directory it provisions secrets into",
        );
    }

    /// §20 residual — an UNPROTECTED DACL is refused even when the ACL, as it
    /// stands at validation time, is impeccable.
    ///
    /// This is the gap the §20 fix left open. That fix refuses an inheritable
    /// untrusted ACE that is PRESENT; it accepted a descriptor which cannot
    /// stop one being added a moment later. With inheritance un-severed, the
    /// owner of any ancestor propagates a read ACE onto `owner-audience.key`
    /// without touching the authority directory or holding any right on it —
    /// so the accepted-residual argument ("only a trusted principal has
    /// WRITE_DAC here") named the wrong privilege.
    ///
    /// The two views below are IDENTICAL apart from `protected`, so the
    /// refusal can only be attributable to that flag.
    #[cfg(windows)]
    #[test]
    fn an_unprotected_dacl_is_refused_even_when_its_aces_are_clean() {
        let user = current_process_sid_string().expect("user sid");
        let dir = Path::new("C:\\ProgramData\\net-authority");
        let clean_ace = AceInfo {
            sid: user.clone(),
            mask: 0x001F_01FF, // FILE_ALL_ACCESS, for the OWNER only
            ace_type: 0,
            flags: 0x03, // OI|CI — trusted, so §20 tolerates it
        };

        // Protected: accepted. Establishes the ACE list is not the cause.
        let protected = DaclView {
            owner_sid: user.clone(),
            protected: true,
            null_dacl: false,
            aces: vec![clean_ace.clone()],
        };
        validate_dacl_view(&protected, &user, dir)
            .expect("a protected owner-only dir must validate");

        // Only `protected` differs.
        let unprotected = DaclView {
            owner_sid: user.clone(),
            protected: false,
            null_dacl: false,
            aces: vec![clean_ace],
        };
        let err = validate_dacl_view(&unprotected, &user, dir)
            .expect_err("an unprotected authority directory must be refused");
        match &err {
            OrgAuthorityError::InsecureAuthorityDir { reason, .. } => assert!(
                reason.contains("PROTECTED") && reason.contains("inherit"),
                "the refusal must name inheritance as the cause, so an operator \
                 knows to sever it rather than hunting for a bad ace; got: {reason}",
            ),
            other => panic!("wrong error variant: {other}"),
        }
    }

    /// §11 — the audience key's OWN descriptor is validated, and the rule is
    /// tighter than the directory's: a READ-ONLY grant to an untrusted
    /// principal refuses.
    ///
    /// The directory tolerates exactly that ace shape (reading a directory
    /// leaks file names). Reading THIS file leaks the raw owner discovery key,
    /// which decrypts every OwnerScoped announcement for the org — so the two
    /// rules must differ, and this pins the difference.
    #[cfg(windows)]
    #[test]
    fn an_untrusted_read_ace_on_the_audience_key_is_refused() {
        let user = current_process_sid_string().expect("user sid");
        let everyone = "S-1-1-0";
        let key = Path::new("C:\\ProgramData\\net-authority\\owner-audience.key");
        let owner_ace = AceInfo {
            sid: user.clone(),
            mask: 0x001F_01FF,
            ace_type: 0,
            flags: 0x10, // INHERITED from the protected authority dir
        };

        // Owner-only: accepted. NB `protected` is false — a file inheriting
        // from a protected directory is normally unprotected, and requiring it
        // here would refuse every legitimately provisioned key.
        let clean = DaclView {
            owner_sid: user.clone(),
            protected: false,
            null_dacl: false,
            aces: vec![owner_ace.clone()],
        };
        validate_audience_acl_view(&clean, &user, key)
            .expect("an owner-only inherited ace must validate");

        // Add a READ-ONLY ace for Everyone — harmless on the directory,
        // fatal here.
        let leaked = DaclView {
            owner_sid: user.clone(),
            protected: false,
            null_dacl: false,
            aces: vec![
                owner_ace,
                AceInfo {
                    sid: everyone.to_string(),
                    mask: 0x0000_0001, // FILE_READ_DATA only
                    ace_type: 0,
                    flags: 0x00, // NOT inheritable, NOT inherited
                },
            ],
        };
        let err = validate_audience_acl_view(&leaked, &user, key)
            .expect_err("a read ace for Everyone on the audience key must refuse");
        match &err {
            OrgAuthorityError::PermissiveAudienceAcl { reason, .. } => assert!(
                reason.contains(everyone),
                "the refusal must name the principal; got: {reason}",
            ),
            other => panic!("wrong error variant: {other}"),
        }
    }

    /// The ownership half of §11: a foreign OWNER holds implicit WRITE_DAC, so
    /// an impeccable ace list does not save it. Mirrors the §3 directory rule.
    #[cfg(windows)]
    #[test]
    fn a_foreign_owned_audience_key_is_refused() {
        let user = current_process_sid_string().expect("user sid");
        let foreign = "S-1-5-21-1111111111-2222222222-3333333333-1001";
        let key = Path::new("C:\\ProgramData\\net-authority\\owner-audience.key");
        let only_us = AceInfo {
            sid: user.clone(),
            mask: 0x001F_01FF,
            ace_type: 0,
            flags: 0x10,
        };
        let view = DaclView {
            owner_sid: foreign.to_string(),
            protected: false,
            null_dacl: false,
            aces: vec![only_us],
        };
        let err = validate_audience_acl_view(&view, &user, key)
            .expect_err("a foreign-owned audience key must refuse");
        match &err {
            OrgAuthorityError::PermissiveAudienceAcl { reason, .. } => assert!(
                reason.contains("WRITE_DAC") && reason.contains(foreign),
                "the refusal must explain WHY ownership matters; got: {reason}",
            ),
            other => panic!("wrong error variant: {other}"),
        }
    }

    /// §4 companion, driven purely: a write-capable ACE bearing the
    /// [`NON_SIMPLE_ACE_SID`] sentinel refuses regardless of its exact type
    /// code, and a read-only one is still tolerated. Covers the object /
    /// callback ACE type codes (5, 9, 11) individually — the live
    /// conditional-ACE witness can only exercise whichever one PowerShell
    /// happens to emit.
    #[cfg(windows)]
    #[test]
    fn every_non_simple_grant_type_fails_closed_when_write_capable() {
        let user = current_process_sid_string().expect("user sid");
        let dir = Path::new("C:\\ProgramData\\net-authority");
        // `flags` is a parameter now: §20 refuses an untrusted INHERITABLE ace
        // whatever it grants, so the read-only tolerance below has to use a
        // non-inheriting ace to isolate the property it is testing.
        let view_with = |ace_type: u8, mask: u32, flags: u8| DaclView {
            owner_sid: user.clone(),
            protected: true,
            null_dacl: false,
            aces: vec![
                AceInfo {
                    sid: user.clone(),
                    mask: 0x001F_01FF,
                    ace_type: 0,
                    flags: 0x03,
                },
                AceInfo {
                    sid: NON_SIMPLE_ACE_SID.to_string(),
                    mask,
                    ace_type,
                    flags,
                },
            ],
        };

        // ACCESS_ALLOWED_OBJECT (5), _CALLBACK (9), _CALLBACK_OBJECT (11).
        // Non-inheriting, so the refusal is the write capability specifically.
        for ace_type in [5u8, 9, 11] {
            let err = validate_dacl_view(&view_with(ace_type, WRITE_MASK, 0x00), &user, dir)
                .expect_err("a write-capable unparsed ACE must be refused");
            assert!(
                matches!(&err, OrgAuthorityError::InsecureAuthorityDir { .. }),
                "ace_type {ace_type}: got {err}",
            );
        }

        // Read-only (FILE_READ_DATA) and NON-inheriting: tolerated. It confers
        // `FILE_LIST_DIRECTORY` on this directory and cannot reach the files'
        // contents, and the authority file names are compile-time constants.
        validate_dacl_view(&view_with(9, 0x0000_0001, 0x00), &user, dir)
            .expect("a read-only NON-inheriting non-simple ACE is tolerated");

        // …but the SAME read-only ace, made inheritable, is refused (§20): on
        // Windows the authority files inherit this directory's ACL, so it
        // would propagate onto the audience key.
        for flags in [0x01u8, 0x02, 0x03] {
            let err = validate_dacl_view(&view_with(9, 0x0000_0001, flags), &user, dir)
                .expect_err("an inheritable untrusted ace must be refused even read-only");
            match &err {
                OrgAuthorityError::InsecureAuthorityDir { reason, .. } => assert!(
                    reason.contains("INHERITABLE"),
                    "flags {flags:#04x}: the refusal must cite inheritance; got {reason}",
                ),
                other => panic!("flags {flags:#04x}: wrong variant: {other}"),
            }
        }

        // And the DENY forms are skipped rather than treated as grants —
        // including inheritable ones, since a deny never broadens access.
        for ace_type in [1u8, 6, 10, 12] {
            validate_dacl_view(&view_with(ace_type, WRITE_MASK, 0x03), &user, dir)
                .unwrap_or_else(|e| panic!("deny ace_type {ace_type} must not refuse: {e}"));
        }
    }

    /// §4 (Windows): a write-capable ACE whose type is NOT one of the simple
    /// ALLOWED/DENIED forms must fail closed.
    ///
    /// `read_object_security` cannot locate the SID of an object / callback
    /// ACE (it does not sit at the fixed byte-8 offset), so it records the
    /// [`NON_SIMPLE_ACE_SID`] sentinel. The validator previously skipped every
    /// `ace_type != 0`, which dropped those grants entirely — while Windows'
    /// own access check honored them.
    ///
    /// A conditional ACE (SDDL `XA`, type 9 = `ACCESS_ALLOWED_CALLBACK_ACE`)
    /// granting Everyone full control under a tautological condition is the
    /// convenient shape: true for every token, so it is a real world-writable
    /// grant. The prior witness used `icacls /grant`, which emits a type-0 ACE
    /// and therefore passed while this variant slipped through.
    ///
    /// Red-witness: restoring `if ace.ace_type != 0 { continue; }` makes this
    /// directory validate, and the `expect_err` fails.
    #[cfg(windows)]
    #[test]
    fn a_non_simple_write_capable_ace_fails_closed() {
        let scratch = Scratch::new();
        let dir = scratch.dir().join("conditional-ace");
        std::fs::create_dir(&dir).expect("mkdir");

        // D:P = protected DACL, no inheritance. XA = callback (conditional)
        // allow ACE. OICI = object+container inheritable. FA = full access.
        // WD = Everyone. The condition `Member_of{SID(WD)}` holds for every
        // token, so this grants Everyone full control in practice.
        //
        // Applied through the Win32 SDDL API rather than `icacls` (which has
        // no conditional-ACE syntax) or PowerShell `Set-Acl` (whose Security
        // module is not autoloadable in every environment, including some CI
        // images) — and consistent with this module's rule of using the binary
        // security APIs over localizable tooling.
        apply_sddl(&dir, "D:P(XA;OICI;FA;;;WD;(Member_of{SID(WD)}))")
            .expect("applying the conditional ACE must succeed");

        // Precondition: the ACE really is a non-simple type carrying the
        // sentinel and a write-capable mask. Without this the test could pass
        // for the wrong reason (e.g. Set-Acl silently emitting a type-0 ACE).
        let view = read_object_security(&dir).expect("read sd");
        let sentinel_write = view
            .aces
            .iter()
            .find(|a| a.sid == NON_SIMPLE_ACE_SID && a.mask & WRITE_MASK != 0)
            .unwrap_or_else(|| {
                panic!(
                    "expected a write-capable non-simple ACE; got {:?}",
                    view.aces
                )
            });
        assert_ne!(
            sentinel_write.ace_type, 0,
            "the ACE under test must not be a simple ALLOWED ace",
        );

        let err = validate_existing_dir_dacl(&dir)
            .expect_err("a write-capable non-simple ACE must be refused");
        assert!(
            matches!(&err, OrgAuthorityError::InsecureAuthorityDir { .. }),
            "got: {err}",
        );

        // And through the full ceremony, not only the validator.
        let kp = node_identity();
        let adopt_err = NodeAuthority::adopt(&dir, cert_for(&kp, 1), kp.entity_id(), 0, None)
            .expect_err("adopt into a conditionally-world-writable dir must be refused");
        assert!(
            matches!(&adopt_err, OrgAuthorityError::InsecureAuthorityDir { .. }),
            "got: {adopt_err}",
        );
    }

    /// §20 (Windows): an untrusted INHERITABLE ace is refused whatever it
    /// grants — because authority files INHERIT this directory's ACL.
    ///
    /// `write_atomic_phased` sets `mode(0o600)` under `#[cfg(unix)]` only.
    /// There is no Windows explicit-DACL branch, so on NTFS every provisioned
    /// authority file gets what it inherits from the directory. An
    /// `OBJECT_INHERIT` ace therefore lands on `owner-audience.key` — the raw
    /// owner discovery key, which decrypts every OwnerScoped announcement for
    /// the org.
    ///
    /// The validator used to skip any ace with no write bits ("a read-only
    /// grant to anyone is tolerated"), so `(A;OICI;FR;;;WD)` validated,
    /// adopted, and handed Everyone read access to the key. Confirmed against
    /// live NTFS before the fix: validator accepted, adopt succeeded,
    /// Everyone could read the key. The §3/§4 witnesses missed it because
    /// both used write-capable aces.
    ///
    /// This asserts the FILE's ACL, not merely the validator's verdict — the
    /// verdict alone would not have caught the original defect's consequence.
    ///
    /// Red-witness: moving the inheritance check back below the
    /// `mask & WRITE_MASK == 0` early-continue makes the directory validate
    /// and Everyone regain read on the key.
    #[cfg(windows)]
    #[test]
    fn an_untrusted_inheritable_read_ace_is_refused_and_never_reaches_the_key() {
        const FILE_READ_DATA: u32 = 0x0000_0001;
        const EVERYONE: &str = "S-1-1-0";
        let scratch = Scratch::new();
        let kp = node_identity();
        let user = current_process_sid_string().expect("user sid");

        // Owner full control + Everyone READ, both object+container
        // inheritable. Everyone holds NO write bit — exactly the shape the
        // old write-only check waved through.
        let dir = scratch.dir().join("inheritable-read");
        std::fs::create_dir(&dir).expect("mkdir");
        apply_sddl(&dir, &format!("D:P(A;OICI;FA;;;{user})(A;OICI;FR;;;WD)"))
            .expect("apply inheritable read ace");

        // Precondition: the ace really is inheritable, read-only, untrusted —
        // otherwise this could pass for an unrelated reason.
        let view = read_object_security(&dir).expect("read dir sd");
        let probe = view
            .aces
            .iter()
            .find(|a| a.sid == EVERYONE)
            .expect("Everyone ace present");
        assert_eq!(
            probe.mask & WRITE_MASK,
            0,
            "the ace under test is read-only"
        );
        assert_ne!(
            probe.flags & 0x01,
            0,
            "the ace under test is OBJECT_INHERIT"
        );

        let err = validate_existing_dir_dacl(&dir)
            .expect_err("an untrusted inheritable ace must be refused");
        match &err {
            OrgAuthorityError::InsecureAuthorityDir { reason, .. } => assert!(
                reason.contains("INHERITABLE"),
                "the refusal must name inheritance as the cause; got: {reason}",
            ),
            other => panic!("wrong error variant: {other}"),
        }

        // Through the full ceremony, with the CONSEQUENCE asserted.
        let adopt_err = NodeAuthority::adopt(&dir, cert_for(&kp, 1), kp.entity_id(), 0, None)
            .expect_err("adopt into an inheritable-read dir must be refused");
        assert!(
            matches!(&adopt_err, OrgAuthorityError::InsecureAuthorityDir { .. }),
            "got: {adopt_err}",
        );
        assert!(
            !dir.join(OWNER_AUDIENCE_FILE).exists(),
            "a refused adoption must provision no key material",
        );

        // Positive control: the same shape WITHOUT the Everyone ace adopts,
        // and the provisioned key is not readable by Everyone. Proves the
        // refusal is the inheritable ace and not the fixture, and pins the
        // property this test exists for.
        let ok_dir = scratch.dir().join("owner-only");
        std::fs::create_dir(&ok_dir).expect("mkdir");
        apply_sddl(&ok_dir, &format!("D:P(A;OICI;FA;;;{user})")).expect("apply owner-only");
        validate_existing_dir_dacl(&ok_dir).expect("an owner-only dir validates");
        NodeAuthority::adopt(&ok_dir, cert_for(&kp, 1), kp.entity_id(), 0, None)
            .expect("adopt into an owner-only dir");
        let key_view =
            read_object_security(&ok_dir.join(OWNER_AUDIENCE_FILE)).expect("read key sd");
        assert!(
            !key_view
                .aces
                .iter()
                .any(|a| a.ace_type == 0 && a.sid == EVERYONE && a.mask & FILE_READ_DATA != 0),
            "Everyone must not be able to read the audience key; got {:?}",
            key_view.aces,
        );
    }

    /// §4 companion: a DENY ACE — simple or not — must NOT be treated as a
    /// grant. Skipping only the deny types (rather than everything that is not
    /// type 0) is what makes the fail-closed rule above safe; without this
    /// witness the fix could over-refuse an ordinary hardened directory.
    #[cfg(windows)]
    #[test]
    fn a_deny_ace_does_not_make_an_owner_only_dir_invalid() {
        let scratch = Scratch::new();
        let kp = node_identity();
        let dir = scratch.dir().join("with-deny");
        NodeAuthority::adopt(&dir, cert_for(&kp, 1), kp.entity_id(), 0, None).expect("adopt");
        validate_existing_dir_dacl(&dir).expect("baseline owner-only dir validates");

        // Add an explicit DENY for Everyone. It cannot broaden access, so the
        // directory stays valid.
        let status = std::process::Command::new("icacls")
            .arg(&dir)
            .arg("/deny")
            .arg("*S-1-1-0:(OI)(CI)W")
            .status()
            .expect("run icacls /deny");
        assert!(status.success(), "icacls deny must succeed");

        let view = read_object_security(&dir).expect("read sd");
        assert!(
            view.aces.iter().any(|a| a.ace_type == 1),
            "precondition: a simple DENY ace must be present; got {:?}",
            view.aces,
        );
        validate_existing_dir_dacl(&dir)
            .expect("a DENY ace must not invalidate an owner-only directory");
    }

    /// Gate-1 (Windows, item 9): when protected creation cannot complete (an
    /// uncreatable path), `adopt` fails CLOSED — no residual directory, no
    /// authority files — and a retry stays fail-closed, so there is no
    /// fail-once / pass-on-retry adoption of an insecure residue.
    #[cfg(windows)]
    #[test]
    fn adopt_fails_closed_on_uncreatable_windows_path_without_residue() {
        let scratch = Scratch::new();
        let kp = node_identity();
        // `|` is invalid in an NTFS name, so CreateDirectoryW fails deterministically.
        let bad_parent = scratch.dir().join("inva|lid");
        let authority = bad_parent.join("authority");

        let e1 = NodeAuthority::adopt(&authority, cert_for(&kp, 1), kp.entity_id(), 0, None)
            .expect_err("adopt onto an uncreatable path must fail");
        assert!(matches!(&e1, OrgAuthorityError::Io { .. }), "got: {e1}");
        assert!(
            !bad_parent.exists(),
            "no residual directory may be left behind",
        );
        for name in NodeAuthority::file_names() {
            assert!(
                !authority.join(name).exists(),
                "no authority file may be provisioned on failure",
            );
        }
        // Retry: still fail-closed — no pre-existing insecure directory to adopt.
        let e2 = NodeAuthority::adopt(&authority, cert_for(&kp, 2), kp.entity_id(), 0, None)
            .expect_err("retry must remain fail-closed");
        assert!(matches!(&e2, OrgAuthorityError::Io { .. }), "got: {e2}");
    }

    #[test]
    fn readopt_same_org_preserves_audience_and_floors() {
        let scratch = Scratch::new();
        let kp = node_identity();

        let first = NodeAuthority::adopt(scratch.dir(), cert_for(&kp, 1), kp.entity_id(), 0, None)
            .expect("adopt");
        // Raise a floor between adoptions (for a DIFFERENT member,
        // so the re-adopt cert stays valid).
        let mut floors = BTreeMap::new();
        floors.insert(EntityId::from_bytes([9u8; 32]), 7u32);
        let bundle = OrgRevocationBundle::try_issue(&org(), &floors).expect("issue");
        first.revocation.apply_bundle(&bundle).expect("apply");
        let handle_before = first.audience.audience_handle;
        drop(first);

        // Renewal: same org, fresh cert.
        let second = NodeAuthority::adopt(scratch.dir(), cert_for(&kp, 2), kp.entity_id(), 0, None)
            .expect("re-adopt");
        assert_eq!(
            second.audience.audience_handle, handle_before,
            "re-adopt must preserve the audience credential"
        );
        assert_eq!(
            second
                .revocation
                .floor_for(&org().org_id(), &EntityId::from_bytes([9u8; 32])),
            7,
            "re-adopt must preserve persisted floors"
        );
        assert_eq!(second.config.owner_cert.generation, 2);
    }

    #[test]
    fn adopt_refuses_second_owner() {
        let scratch = Scratch::new();
        let kp = node_identity();
        NodeAuthority::adopt(scratch.dir(), cert_for(&kp, 1), kp.entity_id(), 0, None)
            .expect("adopt");

        let other_org = OrgKeypair::from_bytes([0x99u8; 32]);
        let foreign_cert =
            OrgMembershipCert::try_issue(&other_org, kp.entity_id().clone(), 1, 3600)
                .expect("issue");
        let err = NodeAuthority::adopt(scratch.dir(), foreign_cert, kp.entity_id(), 0, None)
            .expect_err("one node one owner");
        assert!(matches!(err, OrgAuthorityError::AlreadyOwned { .. }));
    }

    #[test]
    fn adopt_refuses_cert_for_another_entity_and_expired_cert() {
        let scratch = Scratch::new();
        let kp = node_identity();
        let stranger = EntityKeypair::from_bytes([0x55u8; 32]);

        // Cert names someone else.
        let err = NodeAuthority::adopt(
            scratch.dir(),
            cert_for(&stranger, 1),
            kp.entity_id(),
            0,
            None,
        )
        .expect_err("wrong member");
        assert!(matches!(err, OrgAuthorityError::CertNotForThisNode { .. }));

        // Expired cert.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs();
        let expired = OrgMembershipCert::issue_at(
            &org(),
            kp.entity_id().clone(),
            1,
            now - 2000,
            now - 1000,
            7,
        );
        let err = NodeAuthority::adopt(scratch.dir(), expired, kp.entity_id(), 0, None)
            .expect_err("expired");
        assert!(matches!(err, OrgAuthorityError::CertInvalid(_)));

        // Nothing was installed by the refused adoptions.
        assert!(!scratch.dir().join(OWNER_MEMBERSHIP_FILE).exists());
    }

    #[test]
    fn open_is_loud_on_missing_or_corrupt_files() {
        let scratch = Scratch::new();
        let kp = node_identity();

        // Nothing adopted: membership missing.
        let err = NodeAuthority::open(scratch.dir(), kp.entity_id()).expect_err("missing");
        assert!(matches!(err, OrgAuthorityError::MissingFile { .. }));

        NodeAuthority::adopt(scratch.dir(), cert_for(&kp, 1), kp.entity_id(), 0, None)
            .expect("adopt");

        // Corrupt membership: open is loud, and so is re-adopt —
        // the one-owner check cannot be evaluated against garbage,
        // so the operator must remove the corrupt file explicitly.
        std::fs::write(scratch.dir().join(OWNER_MEMBERSHIP_FILE), b"{ nope").expect("write");
        let err = NodeAuthority::open(scratch.dir(), kp.entity_id()).expect_err("corrupt");
        assert!(matches!(err, OrgAuthorityError::CorruptFile { .. }));
        let err = NodeAuthority::adopt(scratch.dir(), cert_for(&kp, 1), kp.entity_id(), 0, None)
            .expect_err("adopt over corrupt membership is loud");
        assert!(matches!(err, OrgAuthorityError::CorruptFile { .. }));

        // Remove the corrupt file (the explicit operator action),
        // re-adopt, then corrupt the audience key (truncated).
        std::fs::remove_file(scratch.dir().join(OWNER_MEMBERSHIP_FILE)).expect("remove");
        NodeAuthority::adopt(scratch.dir(), cert_for(&kp, 1), kp.entity_id(), 0, None)
            .expect("adopt");
        std::fs::write(scratch.dir().join(OWNER_AUDIENCE_FILE), [1u8; 10]).expect("write");
        let err = NodeAuthority::open(scratch.dir(), kp.entity_id()).expect_err("corrupt key");
        assert!(matches!(err, OrgAuthorityError::CorruptFile { .. }));

        // Unknown audience-key version byte.
        let mut bad = [0u8; OwnerAudienceCredential::ENCODED_SIZE];
        bad[0] = 9;
        std::fs::write(scratch.dir().join(OWNER_AUDIENCE_FILE), bad).expect("write");
        let err = NodeAuthority::open(scratch.dir(), kp.entity_id()).expect_err("bad version");
        assert!(matches!(err, OrgAuthorityError::UnsupportedVersion { .. }));

        // Remove the bad audience file (fresh one regenerates on
        // adopt), then delete the revocation state: open is loud.
        std::fs::remove_file(scratch.dir().join(OWNER_AUDIENCE_FILE)).expect("remove");
        NodeAuthority::adopt(scratch.dir(), cert_for(&kp, 1), kp.entity_id(), 0, None)
            .expect("adopt");
        std::fs::remove_file(scratch.dir().join(REVOCATION_STATE_FILE)).expect("remove");
        let err = NodeAuthority::open(scratch.dir(), kp.entity_id()).expect_err("no floors");
        assert!(matches!(err, OrgAuthorityError::Revocation(_)));
    }

    /// The startup half of the restart witness: floors raised past
    /// the installed cert's generation make `open` refuse loudly —
    /// a revoked node cannot come back up claiming ownership.
    #[test]
    fn open_refuses_floored_cert() {
        let scratch = Scratch::new();
        let kp = node_identity();
        let authority =
            NodeAuthority::adopt(scratch.dir(), cert_for(&kp, 1), kp.entity_id(), 0, None)
                .expect("adopt");

        let mut floors = BTreeMap::new();
        floors.insert(kp.entity_id().clone(), 5u32);
        let bundle = OrgRevocationBundle::try_issue(&org(), &floors).expect("issue");
        authority.revocation.apply_bundle(&bundle).expect("apply");
        drop(authority);

        let err = NodeAuthority::open(scratch.dir(), kp.entity_id()).expect_err("floored");
        assert!(matches!(
            err,
            OrgAuthorityError::CertBelowFloor {
                generation: 1,
                floor: 5
            }
        ));

        // Renewal at the floor restores startup.
        NodeAuthority::adopt(scratch.dir(), cert_for(&kp, 5), kp.entity_id(), 0, None)
            .expect("renew at floor");
        NodeAuthority::open(scratch.dir(), kp.entity_id()).expect("open after renewal");
    }

    #[test]
    fn membership_file_rejects_org_cert_mismatch() {
        let scratch = Scratch::new();
        let kp = node_identity();
        NodeAuthority::adopt(scratch.dir(), cert_for(&kp, 1), kp.entity_id(), 0, None)
            .expect("adopt");

        // Hand-edit the file to claim owner B while keeping A's
        // cert — the reviewer's ownership-transfer counterexample
        // (review-9): the declared root is B, so an unverified
        // precheck would let a valid B candidate overwrite A.
        let org_b = OrgKeypair::from_bytes([0x99u8; 32]);
        let path = scratch.dir().join(OWNER_MEMBERSHIP_FILE);
        let mut config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).expect("read")).expect("parse");
        config["owner_org"] = serde_json::Value::String(hex::encode(org_b.org_id().as_bytes()));
        std::fs::write(&path, serde_json::to_vec(&config).expect("ser")).expect("write");
        let tampered = std::fs::read(&path).expect("read tampered");

        // Startup refuses…
        let err = NodeAuthority::open(scratch.dir(), kp.entity_id()).expect_err("mismatch");
        assert!(matches!(err, OrgAuthorityError::OwnerOrgMismatch { .. }));

        // …and so does RE-ADOPTION with a perfectly valid B-issued
        // candidate: the inconsistent existing membership fails
        // structural verification BEFORE it may act as the
        // ownership lock — never an ownership-transfer mechanism.
        let cert_b =
            OrgMembershipCert::try_issue(&org_b, kp.entity_id().clone(), 1, 3600).expect("issue B");
        let err = NodeAuthority::adopt(scratch.dir(), cert_b, kp.entity_id(), 0, None)
            .expect_err("inconsistent existing membership must refuse re-adoption");
        assert!(matches!(err, OrgAuthorityError::OwnerOrgMismatch { .. }));
        // Membership bytes are untouched by the refused ceremony.
        assert_eq!(std::fs::read(&path).expect("read"), tampered);
    }

    /// Review-9: two concurrent FIRST adoptions by different orgs
    /// — the ceremony lock admits exactly one owner; the loser is
    /// refused with `AlreadyOwned` and the persisted owner equals
    /// the winner's candidate.
    #[test]
    fn concurrent_first_adoptions_admit_exactly_one_owner() {
        for attempt in 0..8 {
            let scratch = Scratch::new();
            let kp = node_identity();
            let org_b = OrgKeypair::from_bytes([0x99u8; 32]);
            let cert_a = cert_for(&kp, 1);
            let cert_b = OrgMembershipCert::try_issue(&org_b, kp.entity_id().clone(), 1, 3600)
                .expect("issue B");

            let dir_a = scratch.dir().to_path_buf();
            let dir_b = scratch.dir().to_path_buf();
            let entity_a = kp.entity_id().clone();
            let entity_b = kp.entity_id().clone();
            let t_a = std::thread::spawn(move || {
                NodeAuthority::adopt(&dir_a, cert_a, &entity_a, 0, None).map(|a| a.owner_org())
            });
            let t_b = std::thread::spawn(move || {
                NodeAuthority::adopt(&dir_b, cert_b, &entity_b, 0, None).map(|a| a.owner_org())
            });
            let result_a = t_a.join().expect("A thread");
            let result_b = t_b.join().expect("B thread");

            let winners = [result_a.is_ok(), result_b.is_ok()]
                .iter()
                .filter(|ok| **ok)
                .count();
            assert_eq!(
                winners, 1,
                "attempt {attempt}: exactly one adoption may win"
            );
            let (winner_org, loser) = match (result_a, result_b) {
                (Ok(org), loser) => (org, loser),
                (loser, Ok(org)) => (org, loser),
                (Err(a), Err(b)) => panic!("attempt {attempt}: no winner ({a}; {b})"),
            };
            let loser_err = loser.expect_err("loser refuses");
            assert!(
                matches!(loser_err, OrgAuthorityError::AlreadyOwned { .. }),
                "attempt {attempt}: loser must see AlreadyOwned, got {loser_err}"
            );
            // The persisted owner equals the successful candidate.
            let opened = NodeAuthority::open(scratch.dir(), kp.entity_id()).expect("open");
            assert_eq!(opened.owner_org(), winner_org);
        }
    }

    /// Review-9: an adoption racing a concurrent floor raise never
    /// returns success with an already-revoked certificate — the
    /// final verification and the membership write happen under the
    /// revocation-state lock, so the raise either lands before
    /// (candidate refused) or after (normal revocation of an
    /// installed cert, retracted at runtime), never in between.
    #[test]
    fn adopt_racing_floor_raise_never_installs_revoked_cert() {
        let scratch = Scratch::new();
        let kp = node_identity();
        let revocation_path = scratch.dir().join(REVOCATION_STATE_FILE);

        // A "raise in flight": another writer holds the state lock
        // and publishes floor 5 while the gen-3 adoption is racing.
        let raise_path = revocation_path.clone();
        let member = kp.entity_id().clone();
        let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
        let raiser = std::thread::spawn(move || {
            std::fs::create_dir_all(raise_path.parent().expect("parent")).expect("mkdir");
            let store = OrgRevocationStore::init(&raise_path, ProvisioningExpectation::MayBeFresh)
                .expect("init");
            started_tx.send(()).expect("signal");
            let mut floors = BTreeMap::new();
            floors.insert(member, 5u32);
            let bundle = OrgRevocationBundle::try_issue(&org(), &floors).expect("issue");
            store.apply_bundle(&bundle).expect("raise to 5");
        });
        started_rx.recv().expect("raiser started");

        // The gen-3 adoption races the raise. Whichever interleave
        // the scheduler picks, success with generation 3 installed
        // is unreachable: either the candidate/locked verification
        // sees floor 5 (refusal), or — if adoption fully completed
        // before the raise — the final open below fails.
        let adoption =
            NodeAuthority::adopt(scratch.dir(), cert_for(&kp, 3), kp.entity_id(), 0, None);
        raiser.join().expect("raiser join");

        match adoption {
            Err(e) => {
                assert!(
                    matches!(e, OrgAuthorityError::CertBelowFloor { .. }),
                    "refusal must be the floor, got {e}"
                );
                assert!(
                    !scratch.dir().join(OWNER_MEMBERSHIP_FILE).exists(),
                    "a refused adoption must not publish membership"
                );
            }
            Ok(authority) => {
                // The adoption completed before the raise reached
                // the lock. The installed authority must then fail
                // startup verification against the raised floors —
                // exactly the revoked-cert-at-startup contract.
                assert_eq!(authority.config.owner_cert.generation, 3);
                let err = NodeAuthority::open(scratch.dir(), kp.entity_id())
                    .expect_err("post-raise startup refuses the floored cert");
                assert!(matches!(err, OrgAuthorityError::CertBelowFloor { .. }));
            }
        }
    }

    /// Review-9: the ceremony's accepted skew is PERSISTED and used
    /// by production startup — `adopt --skew-secs N` succeeding and
    /// startup refusing with zero skew was a ceremony/startup
    /// mismatch. The ceiling still binds a hand-edited value.
    #[test]
    fn persisted_skew_carries_from_ceremony_to_startup() {
        let scratch = Scratch::new();
        let kp = node_identity();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs();
        // Validly signed, expired 30 s ago: acceptable ONLY with
        // skew ≥ 30.
        let expired =
            OrgMembershipCert::issue_at(&org(), kp.entity_id().clone(), 1, now - 3600, now - 30, 7);
        let authority = NodeAuthority::adopt(scratch.dir(), expired, kp.entity_id(), 120, None)
            .expect("skew-120 ceremony accepts");
        assert_eq!(authority.config.verification_skew_secs, 120);

        // Production startup uses the SAME persisted tolerance.
        NodeAuthority::open(scratch.dir(), kp.entity_id())
            .expect("startup verifies with the persisted ceremony skew");

        // A hand-edited oversized skew refuses loudly at startup —
        // the token ceiling binds every verification.
        let path = scratch.dir().join(OWNER_MEMBERSHIP_FILE);
        let mut config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).expect("read")).expect("parse");
        config["verification_skew_secs"] = serde_json::Value::from(999_999u64);
        std::fs::write(&path, serde_json::to_vec(&config).expect("ser")).expect("write");
        let err = NodeAuthority::open(scratch.dir(), kp.entity_id()).expect_err("over ceiling");
        assert!(matches!(err, OrgAuthorityError::CertInvalid(_)));
    }

    /// Review-9 filesystem policy: symlinked authority files are
    /// refused — membership and the audience key are opened
    /// no-follow, with the audience mode checked on the opened
    /// handle.
    #[cfg(unix)]
    #[test]
    fn symlinked_authority_files_are_refused() {
        let scratch = Scratch::new();
        let kp = node_identity();
        NodeAuthority::adopt(scratch.dir(), cert_for(&kp, 1), kp.entity_id(), 0, None)
            .expect("adopt");

        // Audience key behind a symlink (target itself 0600): the
        // review-9 red — metadata-following checks passed this.
        let key_path = scratch.dir().join(OWNER_AUDIENCE_FILE);
        let moved = scratch.dir().join("moved-audience.key");
        std::fs::rename(&key_path, &moved).expect("move key");
        std::os::unix::fs::symlink(&moved, &key_path).expect("plant symlink");
        assert!(
            NodeAuthority::open(scratch.dir(), kp.entity_id()).is_err(),
            "symlinked audience key must refuse"
        );
        std::fs::remove_file(&key_path).expect("remove link");
        std::fs::rename(&moved, &key_path).expect("restore");
        NodeAuthority::open(scratch.dir(), kp.entity_id()).expect("regular key opens");

        // Membership behind a symlink: equally refused.
        let membership = scratch.dir().join(OWNER_MEMBERSHIP_FILE);
        let moved = scratch.dir().join("moved-membership.json");
        std::fs::rename(&membership, &moved).expect("move membership");
        std::os::unix::fs::symlink(&moved, &membership).expect("plant symlink");
        assert!(
            NodeAuthority::open(scratch.dir(), kp.entity_id()).is_err(),
            "symlinked membership must refuse"
        );
    }

    #[test]
    fn audience_codec_round_trips_and_debug_redacts() {
        let credential = OwnerAudienceCredential::generate(org().org_id());
        let encoded = credential.encode_config();
        let decoded = OwnerAudienceCredential::decode_config(&encoded).expect("decode");
        assert_eq!(decoded.owner_org, credential.owner_org);
        assert_eq!(decoded.audience_handle, credential.audience_handle);
        assert_eq!(decoded.discovery_key(), credential.discovery_key());

        // Trailing byte is corruption.
        let mut trailing = encoded.to_vec();
        trailing.push(0);
        assert!(OwnerAudienceCredential::decode_config(&trailing).is_err());

        // Debug never leaks the key.
        let debug = format!("{credential:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains(&hex::encode(credential.discovery_key())));
    }

    /// §2 — the codec must actually CARRY the org, at a distinct offset from
    /// the handle and the key.
    ///
    /// Without this the previous format was three anonymous fields, and an
    /// `owner-audience.key` said nothing about which org it belonged to — the
    /// ambiguity `adopt` then resolved by simply keeping whatever was there.
    #[test]
    fn audience_codec_binds_the_owning_org() {
        let org_a = OrgKeypair::from_bytes([0xA1u8; 32]);
        let org_b = OrgKeypair::from_bytes([0xB2u8; 32]);
        let a = OwnerAudienceCredential::generate(org_a.org_id());
        let b = OwnerAudienceCredential::generate(org_b.org_id());

        assert_eq!(a.owner_org, org_a.org_id());
        assert_eq!(b.owner_org, org_b.org_id());

        // The org occupies its own bytes: encoding differs in the org region
        // even though both are freshly generated.
        let (ea, eb) = (a.encode_config(), b.encode_config());
        assert_eq!(&ea[1..33], org_a.org_id().as_bytes());
        assert_eq!(&eb[1..33], org_b.org_id().as_bytes());
        // ...and the secret is NOT in that region.
        assert_ne!(&ea[1..33], a.discovery_key());

        // A v1-shaped file (no org field) is refused rather than guessed at.
        // Guessing "it belongs to whoever is adopting now" is exactly the
        // §2 bug, so a shorter file must not decode.
        let v1_shaped = &ea[..1 + 32 + 32];
        assert!(
            OwnerAudienceCredential::decode_config(v1_shaped).is_err(),
            "a pre-binding audience file must be refused, never migrated by \
             assuming the current adopter's org",
        );
    }

    // ----------------- review-8 ceremony witnesses -----------------

    fn floors_bundle(member: &EntityId, generation: u32) -> OrgRevocationBundle {
        let mut floors = BTreeMap::new();
        floors.insert(member.clone(), generation);
        OrgRevocationBundle::try_issue(&org(), &floors).expect("issue")
    }

    /// §2 — the org-wide audience key must never be carried across an
    /// ownership change.
    ///
    /// The attack is the DOCUMENTED remediation path. `AlreadyOwned` told
    /// operators to "remove the existing authority explicitly to transfer", so
    /// they deleted `owner-membership.json` — which is the one file the
    /// ownership gate read. Adoption under the new org then succeeded, and
    /// step 8 preserved the OLD org's `owner-audience.key` (it only mints when
    /// absent). The new org's private capability catalog was then sealed under
    /// a key that, by design, has been distributed to every node of the old
    /// org.
    ///
    /// The credential now states its own org, so the audience file is a second
    /// ownership witness and the deletion no longer helps.
    #[test]
    fn adopt_refuses_to_inherit_another_orgs_audience_key() {
        let scratch = Scratch::new();
        let kp = node_identity();
        let org_a = OrgKeypair::from_bytes([0xA1u8; 32]);
        let org_b = OrgKeypair::from_bytes([0xB2u8; 32]);

        // Node is adopted by org A.
        let cert_a =
            OrgMembershipCert::try_issue(&org_a, kp.entity_id().clone(), 1, 3600).expect("issue A");
        let authority_a =
            NodeAuthority::adopt(scratch.dir(), cert_a, kp.entity_id(), 0, None).expect("adopt A");
        let a_key = *authority_a.audience.discovery_key();
        let a_handle = authority_a.audience.audience_handle;
        drop(authority_a);

        // Operator follows the old guidance literally and deletes ONLY the
        // membership file, then re-adopts under org B.
        std::fs::remove_file(scratch.dir().join(OWNER_MEMBERSHIP_FILE)).expect("remove membership");
        let cert_b =
            OrgMembershipCert::try_issue(&org_b, kp.entity_id().clone(), 1, 3600).expect("issue B");
        let err = NodeAuthority::adopt(scratch.dir(), cert_b, kp.entity_id(), 0, None)
            .expect_err("B must not inherit A's audience key");
        assert!(
            matches!(
                err,
                OrgAuthorityError::AlreadyOwned { existing, requested }
                    if existing == org_a.org_id() && requested == org_b.org_id()
            ),
            "expected AlreadyOwned(A -> B), got {err:?}",
        );

        // A's key is untouched — the refusal did not rotate or clobber it,
        // so recovering the original ownership is still possible.
        let reopened = read_audience_checked(&scratch.dir().join(OWNER_AUDIENCE_FILE))
            .expect("read audience")
            .expect("audience present");
        let cred = OwnerAudienceCredential::decode_config(&reopened).expect("decode");
        assert_eq!(cred.owner_org, org_a.org_id());
        assert_eq!(cred.discovery_key(), &a_key);
        assert_eq!(cred.audience_handle, a_handle);
    }

    /// Positive control: re-adopting under the SAME org still preserves the
    /// audience key, which is the behaviour the preservation branch exists for
    /// (rotating it on every renewal would partition the node from its own
    /// org's scoped discovery). Without this, a regression that refused every
    /// preserved credential would pass the test above.
    #[test]
    fn readopt_under_the_same_org_preserves_the_audience_key() {
        let scratch = Scratch::new();
        let kp = node_identity();

        let first = NodeAuthority::adopt(scratch.dir(), cert_for(&kp, 1), kp.entity_id(), 0, None)
            .expect("first adopt");
        let key = *first.audience.discovery_key();
        let handle = first.audience.audience_handle;
        drop(first);

        let second = NodeAuthority::adopt(scratch.dir(), cert_for(&kp, 1), kp.entity_id(), 0, None)
            .expect("same-org re-adopt must succeed");
        assert_eq!(
            second.audience.discovery_key(),
            &key,
            "a same-org renewal must NOT rotate the audience key",
        );
        assert_eq!(second.audience.audience_handle, handle);
    }

    /// Review-8 §7: a certificate the supplied bundle immediately
    /// revokes must never adopt successfully — and a refused first
    /// adoption leaves the directory untouched.
    #[test]
    fn adopt_with_floors_refuses_immediately_revoked_cert() {
        let scratch = Scratch::new();
        let kp = node_identity();
        let bundle = floors_bundle(kp.entity_id(), 5);

        let err = NodeAuthority::adopt(
            scratch.dir(),
            cert_for(&kp, 3),
            kp.entity_id(),
            0,
            Some(&bundle),
        )
        .expect_err("generation 3 under candidate floor 5 must refuse");
        assert!(matches!(
            err,
            OrgAuthorityError::CertBelowFloor {
                generation: 3,
                floor: 5
            }
        ));
        // Nothing durable was created by the refused ceremony.
        for name in NodeAuthority::file_names() {
            assert!(
                !scratch.dir().join(name).exists(),
                "{name} must not exist after a refused adoption"
            );
        }

        // A cert AT the candidate floor adopts, with the floors
        // durably applied in the same ceremony.
        let authority = NodeAuthority::adopt(
            scratch.dir(),
            cert_for(&kp, 5),
            kp.entity_id(),
            0,
            Some(&bundle),
        )
        .expect("generation 5 at floor 5 adopts");
        assert_eq!(
            authority
                .revocation
                .floor_for(&org().org_id(), kp.entity_id()),
            5
        );
        // And the persisted floors survive a fresh open.
        let reopened = NodeAuthority::open(scratch.dir(), kp.entity_id()).expect("open");
        assert_eq!(
            reopened
                .revocation
                .floor_for(&org().org_id(), kp.entity_id()),
            5
        );
    }

    /// Review-8 §6: signed receipt is not trust establishment — a
    /// bundle signed by any org other than the candidate owner
    /// refuses BEFORE durable state changes.
    #[test]
    fn adopt_refuses_foreign_floor_bundle() {
        let scratch = Scratch::new();
        let kp = node_identity();
        let org_b = OrgKeypair::from_bytes([0x99u8; 32]);
        let mut floors = BTreeMap::new();
        floors.insert(kp.entity_id().clone(), 5u32);
        let foreign = OrgRevocationBundle::try_issue(&org_b, &floors).expect("issue");

        let err = NodeAuthority::adopt(
            scratch.dir(),
            cert_for(&kp, 1),
            kp.entity_id(),
            0,
            Some(&foreign),
        )
        .expect_err("B-signed bundle under A adoption must refuse");
        assert!(matches!(err, OrgAuthorityError::ForeignFloorBundle { .. }));
        // No file — in particular no B floor — was persisted.
        for name in NodeAuthority::file_names() {
            assert!(!scratch.dir().join(name).exists());
        }
    }

    /// Review-8 §8: a same-org renewal against corrupt preserved
    /// state refuses BEFORE membership publishes — the previous
    /// membership bytes remain exactly as they were.
    #[test]
    fn renewal_against_corrupt_audience_leaves_membership_untouched() {
        let scratch = Scratch::new();
        let kp = node_identity();
        NodeAuthority::adopt(scratch.dir(), cert_for(&kp, 1), kp.entity_id(), 0, None)
            .expect("adopt");
        let m1 = std::fs::read(scratch.dir().join(OWNER_MEMBERSHIP_FILE)).expect("read M1");

        // Corrupt the preserved audience credential, then attempt a
        // same-org renewal (a fresh generation-2 cert).
        std::fs::write(scratch.dir().join(OWNER_AUDIENCE_FILE), [1u8; 10]).expect("corrupt");
        let err = NodeAuthority::adopt(scratch.dir(), cert_for(&kp, 2), kp.entity_id(), 0, None)
            .expect_err("renewal over corrupt audience must refuse");
        assert!(matches!(err, OrgAuthorityError::CorruptFile { .. }));

        // Membership is byte-for-byte the pre-renewal M1.
        let after = std::fs::read(scratch.dir().join(OWNER_MEMBERSHIP_FILE)).expect("read");
        assert_eq!(after, m1, "failed renewal must not advertise M2");
    }

    /// Review-8 §10: a group/other-readable audience key refuses
    /// startup AND re-adoption — creation-time 0600 is not trusted
    /// to persist.
    #[cfg(unix)]
    #[test]
    fn permissive_audience_key_refuses_open_and_readopt() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = Scratch::new();
        let kp = node_identity();
        NodeAuthority::adopt(scratch.dir(), cert_for(&kp, 1), kp.entity_id(), 0, None)
            .expect("adopt");
        let key_path = scratch.dir().join(OWNER_AUDIENCE_FILE);

        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o644))
            .expect("chmod 644");
        let err = NodeAuthority::open(scratch.dir(), kp.entity_id())
            .expect_err("permissive key must refuse startup");
        assert!(matches!(
            err,
            OrgAuthorityError::PermissiveAudienceFile { mode: 0o644, .. }
        ));
        let err = NodeAuthority::adopt(scratch.dir(), cert_for(&kp, 2), kp.entity_id(), 0, None)
            .expect_err("re-adopt must not silently preserve a permissive key");
        assert!(matches!(
            err,
            OrgAuthorityError::PermissiveAudienceFile { .. }
        ));

        // Tightening the mode restores both paths.
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))
            .expect("chmod 600");
        NodeAuthority::open(scratch.dir(), kp.entity_id()).expect("open after tighten");
    }

    /// Review-8 §10: pre-created permissive temp files (the old
    /// predictable names, or any `.tmp.` litter) cannot weaken the
    /// final key's mode — the writer always creates a fresh 0600
    /// inode.
    #[cfg(unix)]
    #[test]
    fn pre_created_permissive_temps_cannot_weaken_the_final_key() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = Scratch::new();
        let kp = node_identity();

        // Litter the directory with permissive would-be temps,
        // including the previous implementation's predictable
        // `with_extension`-shaped name.
        let pid = std::process::id();
        for name in [
            format!("owner-audience.tmp.{pid}"),
            format!("owner-audience.key.tmp.{pid}"),
            format!("owner-audience.key.tmp.{pid}.0.00000000"),
        ] {
            let p = scratch.dir().join(name);
            std::fs::write(&p, b"attacker").expect("pre-create");
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644))
                .expect("chmod 644");
        }

        NodeAuthority::adopt(scratch.dir(), cert_for(&kp, 1), kp.entity_id(), 0, None)
            .expect("adopt");
        let mode = std::fs::metadata(scratch.dir().join(OWNER_AUDIENCE_FILE))
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o077,
            0,
            "final audience key must be owner-only, got {mode:o}"
        );
        // And the ceremony's result actually loads.
        NodeAuthority::open(scratch.dir(), kp.entity_id()).expect("open");
    }
}
