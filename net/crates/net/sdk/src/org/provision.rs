//! OSDK §7 — node provisioning: installing an adopted authority and provider
//! grant audiences.
//!
//! These are the two steps that make the org facade *usable* — distinct from
//! the call-path verbs, and distinct from issuance:
//!
//! - **Adoption** (the one-time ceremony that writes the authority files) is
//!   `net node adopt`. It is NOT here and NOT in any binding — it mints
//!   material, which is operator/CLI territory.
//! - **Installing** an already-adopted authority is node STARTUP: every node
//!   loads its own provisioned identity to function. Without it, `mesh.org(..)`
//!   fails `NodeAuthorityRequired` and a granted service cannot seal envelopes.
//!   That is what this module does, and it must be reachable from every binding
//!   or the org surface is inert there.
//!
//! Provisioning errors are their own concern — a node either starts correctly
//! or it does not — so they are NOT folded into the four call-path domains
//! (`OrgSdkError`). A binding surfaces them as a plain error, not something a
//! caller branches on per-request.

use std::path::Path;
use std::sync::Arc;

use net::adapter::net::behavior::org_authority::{
    load_grant_audience_secret, NodeAuthority, OrgAuthorityError,
};
use net::adapter::net::MeshNode;

use super::types::OrgCapabilityGrant;
use crate::mesh::Mesh;

/// Why a provisioning step failed. Setup errors, surfaced as-is by bindings.
#[derive(Debug)]
pub enum OrgProvisionError {
    /// Opening or installing the adopted authority failed — missing/corrupt
    /// files, a mode/DACL violation, or the authority names a different entity
    /// than this node.
    Authority(OrgAuthorityError),
    /// A provider grant's wire bytes did not decode.
    GrantDecode(net::adapter::net::behavior::org::OrgError),
    /// The provider audience-secret file was refused (the same checked loader
    /// credentials use: no symlink follow, regular file, owner-only, exact
    /// size).
    SecretFile(String),
    /// The provider grant/secret could not be installed — wrong issuer org,
    /// target not covered, expired, no DISCOVER, or the registry is full.
    AudienceInstall(net::adapter::net::behavior::org_grant_registry::GrantAudienceInstallError),
}

impl std::fmt::Display for OrgProvisionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Authority(e) => write!(f, "install_org_authority: {e}"),
            Self::GrantDecode(e) => write!(f, "provider grant did not decode: {e}"),
            Self::SecretFile(e) => write!(f, "provider audience secret: {e}"),
            Self::AudienceInstall(e) => write!(f, "provider audience install: {e}"),
        }
    }
}

impl std::error::Error for OrgProvisionError {}

/// Install an adopted node authority from `dir` — the directory `net node
/// adopt` wrote (OSDK §7). Node-based so every binding reaches it identically.
///
/// Loads the files, self-verifies them against this node's own identity (so a
/// directory adopted for a different entity is refused), and installs the
/// authority plus its revocation store in one step. The node's identity must be
/// the one the membership cert names — i.e. the mesh must have been built with
/// the matching `identity_seed`.
pub fn install_org_authority_node(
    node: &Arc<MeshNode>,
    dir: &Path,
) -> Result<(), OrgProvisionError> {
    let entity = node.entity_id().clone();
    let authority = NodeAuthority::open(dir, &entity).map_err(OrgProvisionError::Authority)?;
    node.install_node_authority(Arc::new(authority))
        .map_err(OrgProvisionError::Authority)?;
    // Enable owner-cert (and scoped-envelope) emission. The core keeps this a
    // separate toggle, but `owner_cert_under` — which derives BOTH the emission
    // cert AND the scoped announcement's audience key — returns `None` while it
    // is off, so a node with an installed authority but emission disabled emits
    // NO scoped announcements (owner-scoped or granted) and is undiscoverable.
    // A binding's single "install my org authority" startup step wants an
    // org-ready node, so we enable it here; the in-process facade tests call it
    // separately (idempotent). Without this, a binding provider is silently
    // invisible — surfaced by the live Node cell (`org_live.test.ts`).
    node.set_owner_cert_emission(true)
        .map_err(OrgProvisionError::Authority)?;
    Ok(())
}

/// Install a PROVIDER grant audience so a granted service can seal envelopes
/// (OSDK §7): the grant this node's org issued, as wire bytes, plus its
/// out-of-band secret as a **path**.
///
/// The secret crosses as a path, never bytes — the same asymmetry as
/// credentials, for the same reason (the raw discovery key never enters a
/// binding's heap). Loaded through the checked loader.
///
/// A same-org (`OrgAccess::SameOrg`) provider does NOT need this — it seals
/// under the owner audience carried by the installed authority. Only a
/// `Granted` provider does.
pub fn install_provider_grant_audience_node(
    node: &Arc<MeshNode>,
    grant_bytes: &[u8],
    secret_path: &Path,
) -> Result<(), OrgProvisionError> {
    let grant =
        OrgCapabilityGrant::from_bytes(grant_bytes).map_err(OrgProvisionError::GrantDecode)?;
    let secret = load_grant_audience_secret(secret_path)
        .map_err(|e| OrgProvisionError::SecretFile(e.to_string()))?;
    node.install_provider_grant_audience(grant, secret)
        .map_err(OrgProvisionError::AudienceInstall)?;
    Ok(())
}

impl Mesh {
    /// Install an adopted node authority from `dir` (OSDK §7).
    ///
    /// Required before [`Mesh::org`] can bind or a granted service can serve —
    /// see [`install_org_authority_node`]. Thin delegation, one implementation.
    pub fn install_org_authority(&self, dir: &Path) -> Result<(), OrgProvisionError> {
        install_org_authority_node(self.node(), dir)
    }

    /// Install a provider grant audience from wire bytes + a secret file path
    /// (OSDK §7). See [`install_provider_grant_audience_node`].
    pub fn install_provider_grant_audience(
        &self,
        grant_bytes: &[u8],
        secret_path: &Path,
    ) -> Result<(), OrgProvisionError> {
        install_provider_grant_audience_node(self.node(), grant_bytes, secret_path)
    }
}
