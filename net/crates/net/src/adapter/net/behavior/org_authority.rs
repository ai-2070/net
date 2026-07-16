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

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::org::{OrgError, OrgId, OrgMembershipCert, OrgRevocationBundle};
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
}

impl NodeAuthorityConfig {
    /// Build a config from a certificate (the owner org is the
    /// cert's issuer).
    pub fn new(owner_cert: OrgMembershipCert) -> Self {
        Self {
            version: NODE_AUTHORITY_CONFIG_VERSION,
            owner_org: owner_cert.org_id,
            owner_cert,
        }
    }

    /// Self-verify this config for the local node, in the locked
    /// order: config consistency (`owner_org == owner_cert.org_id`)
    /// → member binding (`owner_cert.member == local_entity`) →
    /// certificate signature + TTL ceiling + validity window →
    /// revocation floor against the supplied state.
    ///
    /// Takes an [`OrgRevocationState`] (not the store) so the
    /// adoption ceremony can verify against CANDIDATE floors —
    /// persisted maxima plus a not-yet-applied operator bundle —
    /// before any durable state changes (review-8 §7).
    pub fn self_verify(
        &self,
        local_entity: &EntityId,
        floors: &OrgRevocationState,
        skew_secs: u64,
    ) -> Result<(), OrgAuthorityError> {
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
            .is_valid_with_skew(skew_secs)
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
    /// Owner-cert emission was enabled on a node with no installed
    /// [`NodeAuthority`]. Emission is sourced EXCLUSIVELY from the
    /// loaded, self-verified authority (review-8 §3) — there is no
    /// raw-certificate bypass.
    NoAuthorityInstalled,
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
            Self::NoAuthorityInstalled => write!(
                f,
                "owner-cert emission requires an installed node authority; run \
                 `net node adopt` and configure the authority directory first"
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
    /// validate existing membership (one node one owner)
    /// → validate existing audience credential (codec + 0600 mode)
    /// → strictly load persisted floor maxima (no creation yet)
    /// → verify the optional owner floor bundle
    ///    (signature + bundle.org_id == candidate owner — signed
    ///     receipt is never trust establishment)
    /// → compute CANDIDATE floors (persisted ∪ bundle)
    /// → verify the candidate certificate against candidate state
    /// → create/open the revocation store; durably apply the bundle
    /// → re-verify the cert against the store's post-apply view
    /// → write the audience file if it didn't exist
    /// → publish membership LAST
    /// → final full self-verification via [`Self::open`]
    /// ```
    ///
    /// A refused adoption before the store-creation step leaves the
    /// directory byte-for-byte untouched. Once a valid monotone
    /// bundle has been durably applied, a LATER failure does not
    /// roll it back (revocation is monotone; that is fail-closed).
    ///
    /// - `owner-membership.json` — refuses a DIFFERENT org's cert
    ///   while an owner is installed; same-org renewal overwrites.
    /// - `owner-audience.key` — generated fresh on first adopt,
    ///   PRESERVED on re-adopt; a corrupt or group/other-readable
    ///   existing credential refuses the ceremony.
    /// - `revocation-state.json` — initialized on first adopt,
    ///   PRESERVED on re-adopt (floor monotonicity survives
    ///   re-adoption).
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

        // 1. One node, one owner: a different installed org
        //    refuses; a corrupt membership file refuses too (the
        //    check cannot be evaluated against garbage — the
        //    operator removes it explicitly).
        if let Some(existing) = read_optional(&membership_path)? {
            let existing: NodeAuthorityConfig = parse_membership(&existing, &membership_path)?;
            if existing.owner_org != owner_cert.org_id {
                return Err(OrgAuthorityError::AlreadyOwned {
                    existing: existing.owner_org,
                    requested: owner_cert.org_id,
                });
            }
        }

        // 2. Validate any preserved audience credential BEFORE the
        //    ceremony commits anything: strict codec AND the 0600
        //    mode gate (a possibly-disclosed key must not be
        //    silently re-blessed by a renewal).
        let have_audience = match read_optional(&audience_path)? {
            Some(bytes) => {
                check_audience_permissions(&audience_path)?;
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
        let config = NodeAuthorityConfig::new(owner_cert);
        config.self_verify(local_entity, &candidate_floors, skew_secs)?;

        // 7. All validation passed — durable changes begin.
        //    Revocation first (monotone, never rolled back): create
        //    or open the store and apply the bundle through the
        //    locked reread path.
        let revocation = Arc::new(OrgRevocationStore::init(&revocation_path)?);
        if let Some(bundle) = owner_floors {
            revocation.apply_bundle(bundle)?;
        }
        // Re-verify against the store's post-apply live view: a
        // concurrent writer may have advanced floors between the
        // candidate check and the locked apply. Membership has not
        // been written yet, so a refusal here still publishes
        // nothing.
        config.self_verify(local_entity, &revocation.snapshot(), skew_secs)?;

        // 8. Audience material: preserved, or created and written
        //    now (0600, atomic, fresh temp inode).
        if !have_audience {
            let audience = OwnerAudienceCredential::generate();
            write_atomic(&audience_path, &audience.encode_config())?;
        }

        // 9. Membership LAST.
        let membership_bytes =
            serde_json::to_vec_pretty(&config).map_err(|e| OrgAuthorityError::Io {
                path: membership_path.display().to_string(),
                reason: format!("serialize: {e}"),
            })?;
        write_atomic(&membership_path, &membership_bytes)?;

        // 10. Final self-verification through the real startup
        //     loader — the ceremony's result is exactly what the
        //     node will load, or the ceremony failed.
        Self::open(dir, local_entity, skew_secs)
    }

    /// Startup: load the three files LOUDLY (missing or corrupt is
    /// a refusal, never a default) and self-verify the membership
    /// for `local_entity`. A node that cannot prove its ownership
    /// does not get a `NodeAuthority`.
    ///
    /// The audience key file's mode is re-checked here (review-8
    /// §10): creation-time 0600 is insufficient because config
    /// management, copying, or manual edits can weaken it later —
    /// a group/other-readable key refuses startup.
    pub fn open(
        dir: &Path,
        local_entity: &EntityId,
        skew_secs: u64,
    ) -> Result<Self, OrgAuthorityError> {
        let membership_path = dir.join(OWNER_MEMBERSHIP_FILE);
        let audience_path = dir.join(OWNER_AUDIENCE_FILE);
        let revocation_path = dir.join(REVOCATION_STATE_FILE);

        let membership_bytes = read_required(&membership_path)?;
        let config = parse_membership(&membership_bytes, &membership_path)?;

        let audience_bytes = read_required(&audience_path)?;
        check_audience_permissions(&audience_path).inspect_err(|e| {
            tracing::error!("{e}");
        })?;
        let audience = OwnerAudienceCredential::decode_config(&audience_bytes)?;

        let revocation = Arc::new(OrgRevocationStore::open_existing(&revocation_path)?);

        config
            .self_verify(local_entity, &revocation.snapshot(), skew_secs)
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

/// The ssh-style mode gate for the audience key: refuse any
/// group/other bits. Re-checked on every load, not just at
/// creation (review-8 §10).
#[cfg(unix)]
fn check_audience_permissions(path: &Path) -> Result<(), OrgAuthorityError> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(path).map_err(|e| OrgAuthorityError::Io {
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
    Ok(())
}

/// Non-Unix: NTFS ACLs have no clean 0o600 analog reachable from
/// `std::fs`; surface a stderr warning (mirrors the CLI identity
/// gate) rather than a silent no-op.
#[cfg(not(unix))]
fn check_audience_permissions(path: &Path) -> Result<(), OrgAuthorityError> {
    eprintln!(
        "warning: audience-key permission gate is a no-op on this platform; \
         ACLs on {} are not validated — manage them out-of-band.",
        path.display()
    );
    Ok(())
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, OrgAuthorityError> {
    match std::fs::read(path) {
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

        let opened = NodeAuthority::open(scratch.dir(), kp.entity_id(), 0).expect("open");
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
        let err = NodeAuthority::open(scratch.dir(), kp.entity_id(), 0).expect_err("missing");
        assert!(matches!(err, OrgAuthorityError::MissingFile { .. }));

        NodeAuthority::adopt(scratch.dir(), cert_for(&kp, 1), kp.entity_id(), 0, None)
            .expect("adopt");

        // Corrupt membership: open is loud, and so is re-adopt —
        // the one-owner check cannot be evaluated against garbage,
        // so the operator must remove the corrupt file explicitly.
        std::fs::write(scratch.dir().join(OWNER_MEMBERSHIP_FILE), b"{ nope").expect("write");
        let err = NodeAuthority::open(scratch.dir(), kp.entity_id(), 0).expect_err("corrupt");
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
        let err = NodeAuthority::open(scratch.dir(), kp.entity_id(), 0).expect_err("corrupt key");
        assert!(matches!(err, OrgAuthorityError::CorruptFile { .. }));

        // Unknown audience-key version byte.
        let mut bad = [0u8; OwnerAudienceCredential::ENCODED_SIZE];
        bad[0] = 9;
        std::fs::write(scratch.dir().join(OWNER_AUDIENCE_FILE), bad).expect("write");
        let err = NodeAuthority::open(scratch.dir(), kp.entity_id(), 0).expect_err("bad version");
        assert!(matches!(err, OrgAuthorityError::UnsupportedVersion { .. }));

        // Remove the bad audience file (fresh one regenerates on
        // adopt), then delete the revocation state: open is loud.
        std::fs::remove_file(scratch.dir().join(OWNER_AUDIENCE_FILE)).expect("remove");
        NodeAuthority::adopt(scratch.dir(), cert_for(&kp, 1), kp.entity_id(), 0, None)
            .expect("adopt");
        std::fs::remove_file(scratch.dir().join(REVOCATION_STATE_FILE)).expect("remove");
        let err = NodeAuthority::open(scratch.dir(), kp.entity_id(), 0).expect_err("no floors");
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

        let err = NodeAuthority::open(scratch.dir(), kp.entity_id(), 0).expect_err("floored");
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
        NodeAuthority::open(scratch.dir(), kp.entity_id(), 0).expect("open after renewal");
    }

    #[test]
    fn membership_file_rejects_org_cert_mismatch() {
        let scratch = Scratch::new();
        let kp = node_identity();
        NodeAuthority::adopt(scratch.dir(), cert_for(&kp, 1), kp.entity_id(), 0, None)
            .expect("adopt");

        // Hand-edit the file to claim a different owner_org while
        // keeping the original cert.
        let path = scratch.dir().join(OWNER_MEMBERSHIP_FILE);
        let mut config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).expect("read")).expect("parse");
        config["owner_org"] = serde_json::Value::String(hex::encode([0x99u8; 32]));
        std::fs::write(&path, serde_json::to_vec(&config).expect("ser")).expect("write");

        let err = NodeAuthority::open(scratch.dir(), kp.entity_id(), 0).expect_err("mismatch");
        assert!(matches!(err, OrgAuthorityError::OwnerOrgMismatch { .. }));
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
        let reopened = NodeAuthority::open(scratch.dir(), kp.entity_id(), 0).expect("open");
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
        let err = NodeAuthority::open(scratch.dir(), kp.entity_id(), 0)
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
        NodeAuthority::open(scratch.dir(), kp.entity_id(), 0).expect("open after tighten");
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
        NodeAuthority::open(scratch.dir(), kp.entity_id(), 0).expect("open");
    }
}
