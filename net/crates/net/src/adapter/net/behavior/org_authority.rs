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
//! [`ensure_secure_authority_dir`] enforces the boundary at its edges: on Unix
//! the resolved ancestor chain is checked (no group/other-writable, non-sticky
//! parent through which another account could swap the directory's entry), a
//! new authority directory is created no broader than 0700 (umask) and then
//! tightened to exactly 0700, and an existing one must be owned by the current
//! user and not group/other-writable. On Windows a newly-created directory is
//! restricted to the owner via an explicit DACL; a pre-existing directory's
//! ACL is operator-owned (the default `%APPDATA%` path is protected by profile
//! inheritance, a custom `--authority-dir` is operator-asserted). The user
//! account, SYSTEM, and local administrators are trusted principals.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::org::{current_timestamp, OrgError, OrgId, OrgMembershipCert, OrgRevocationBundle};
use super::org_revocation::{
    write_atomic, OrgRevocationError, OrgRevocationState, OrgRevocationStore,
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
pub const OWNER_AUDIENCE_KEY_VERSION: u8 = 1;

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
    /// Public-ish routing handle for the owner audience. Random;
    /// reveals nothing but linkage.
    pub audience_handle: [u8; 32],
    /// The audience decryption key. SECRET — 0600 at rest, never
    /// on the wire, never in a proof, never in `Debug` output.
    discovery_key: [u8; 32],
}

impl OwnerAudienceCredential {
    /// Encoded size of the explicit config codec:
    /// version byte ‖ handle (32) ‖ key (32).
    pub const ENCODED_SIZE: usize = 1 + 32 + 32;

    /// Generate a fresh credential. `getrandom` failure aborts —
    /// a predictable discovery key would let anyone decrypt
    /// owner-scoped announcements (same rationale as
    /// `EntityKeypair::generate`).
    pub fn generate() -> Self {
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
    /// `version ‖ handle ‖ key`, exactly
    /// [`Self::ENCODED_SIZE`] bytes.
    pub fn encode_config(&self) -> [u8; Self::ENCODED_SIZE] {
        let mut buf = [0u8; Self::ENCODED_SIZE];
        buf[0] = OWNER_AUDIENCE_KEY_VERSION;
        buf[1..33].copy_from_slice(&self.audience_handle);
        buf[33..65].copy_from_slice(&self.discovery_key);
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
        Ok(Self {
            audience_handle: bytes[1..33].try_into().unwrap(),
            discovery_key: bytes[33..65].try_into().unwrap(),
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
                 (one node one owner — remove the existing authority explicitly to transfer)"
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
        let membership_path = dir.join(OWNER_MEMBERSHIP_FILE);
        let audience_path = dir.join(OWNER_AUDIENCE_FILE);
        let revocation_path = dir.join(REVOCATION_STATE_FILE);

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
        if let Some(existing) = read_optional(&membership_path)? {
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
        }

        // 2. Validate any preserved audience credential BEFORE the
        //    ceremony commits anything: no-follow regular-file
        //    handle, strict codec, AND the 0600 mode gate on the
        //    opened descriptor (a possibly-disclosed key must not
        //    be silently re-blessed by a renewal).
        let have_audience = match read_audience_checked(&audience_path)? {
            Some(bytes) => {
                let _ = OwnerAudienceCredential::decode_config(&bytes)?;
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
        let revocation = Arc::new(OrgRevocationStore::init(&revocation_path)?);
        if let Some(bundle) = owner_floors {
            revocation.apply_bundle(bundle)?;
        }

        // 8. Audience material: preserved, or created and written
        //    now (0600, atomic, fresh temp inode).
        if !have_audience {
            let audience = OwnerAudienceCredential::generate();
            write_atomic(&audience_path, &audience.encode_config())?;
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
        let membership_path = dir.join(OWNER_MEMBERSHIP_FILE);
        let audience_path = dir.join(OWNER_AUDIENCE_FILE);
        let revocation_path = dir.join(REVOCATION_STATE_FILE);

        // Gate-1: validate the authority directory boundary (owner-only,
        // owned by the current user) before reading any authority state.
        ensure_secure_authority_dir(dir)?;

        let membership_bytes = read_required(&membership_path)?;
        let config = parse_membership(&membership_bytes, &membership_path)?;

        let audience_bytes = read_audience_checked(&audience_path)?.ok_or_else(|| {
            let err = OrgAuthorityError::MissingFile {
                path: audience_path.display().to_string(),
            };
            tracing::error!("{err}");
            err
        })?;
        let audience = OwnerAudienceCredential::decode_config(&audience_bytes)?;

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

/// Windows analogue of Unix owner-only mode for a directory: strip inherited
/// ACEs and grant only the current user full control
/// (`icacls <dir> /inheritance:r /grant:r <user>:F`), so a freshly-created
/// authority directory is restricted to its owner regardless of where it was
/// placed. New files created inside inherit this owner-only DACL.
#[cfg(windows)]
fn restrict_dir_to_owner(dir: &Path) -> std::io::Result<()> {
    let user = match (std::env::var("USERDOMAIN"), std::env::var("USERNAME")) {
        (Ok(domain), Ok(user)) if !domain.is_empty() => format!("{domain}\\{user}"),
        (_, Ok(user)) if !user.is_empty() => user,
        _ => {
            return Err(std::io::Error::other(
                "USERNAME is not set; cannot restrict the authority directory ACL",
            ))
        }
    };
    let out = std::process::Command::new("icacls")
        .arg(dir)
        .arg("/inheritance:r")
        .arg("/grant:r")
        .arg(format!("{user}:F"))
        .output()?;
    if !out.status.success() {
        return Err(std::io::Error::other(format!(
            "icacls could not restrict {} to {user}: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
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
/// - Non-Unix: a newly-created directory is restricted to the owner via an
///   explicit DACL ([`restrict_dir_to_owner`], the Windows analogue of 0700).
///   A pre-existing directory's ACL is operator-owned and NOT re-validated
///   here: the default authority path under the per-user protected profile
///   (`%APPDATA%`) is covered by profile inheritance, and a custom
///   `--authority-dir` is operator-asserted (the CLI warns when one is set).
///   The user account, SYSTEM, and local administrators are trusted principals.
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
                // The directory's ACL is operator-owned; reading the Windows
                // DACL needs Win32 security APIs and is not done here. The
                // default path under the per-user profile is protected by
                // inheritance; a custom path is operator-asserted (CLI warns).
                tracing::debug!(
                    path = %dir.display(),
                    "authority directory exists; ACL is operator-owned and not \
                     re-validated on this platform",
                );
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if let Some(parent) = dir.parent() {
                    if !parent.as_os_str().is_empty() {
                        std::fs::create_dir_all(parent).map_err(io)?;
                    }
                }
                std::fs::create_dir(dir).map_err(io)?;
                // Restrict the freshly-created directory to the owner (Windows
                // analogue of 0700), so even a custom path is owner-only. Best-
                // effort: a failure warns rather than aborting, matching the
                // operator-asserted posture for custom Windows paths.
                #[cfg(windows)]
                if let Err(e) = restrict_dir_to_owner(dir) {
                    tracing::warn!(
                        path = %dir.display(), error = %e,
                        "could not restrict the authority directory ACL to the \
                         owner; ensure it is under a per-user protected location",
                    );
                }
            }
            Err(e) => return Err(io(e)),
        }
        Ok(())
    }
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
    #[cfg(not(unix))]
    {
        // NTFS ACLs have no clean 0o600 analog reachable from
        // `std::fs`; surface a stderr warning (mirrors the CLI
        // identity gate) rather than a silent no-op.
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
    #[cfg(unix)]
    #[test]
    fn adopt_creates_intermediate_parents_owner_only_under_permissive_umask() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = Scratch::new();
        // Permissive umask for the create window; restored immediately after.
        let saved = unsafe { libc::umask(0) };
        let a = scratch.dir().join("a");
        let b = a.join("b");
        let authority = b.join("authority");
        let kp = node_identity();
        let res = NodeAuthority::adopt(&authority, cert_for(&kp, 1), kp.entity_id(), 0, None);
        unsafe {
            libc::umask(saved);
        }
        res.expect("adopt creates a nested chain securely");
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
            let store = OrgRevocationStore::init(&raise_path).expect("init");
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
        let credential = OwnerAudienceCredential::generate();
        let encoded = credential.encode_config();
        let decoded = OwnerAudienceCredential::decode_config(&encoded).expect("decode");
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

    // ----------------- review-8 ceremony witnesses -----------------

    fn floors_bundle(member: &EntityId, generation: u32) -> OrgRevocationBundle {
        let mut floors = BTreeMap::new();
        floors.insert(member.clone(), generation);
        OrgRevocationBundle::try_issue(&org(), &floors).expect("issue")
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
