//! Subnet AUTHORITY provisioning: named exports, trust-anchor
//! validation, the frozen binding DTOs, and the stable `subnet:` error
//! envelope.
//!
//! # Why this lives in the core
//!
//! These types started in `net-mesh-sdk`, which meant base `libnet`'s
//! JSON constructor could not reach them — the core cannot depend on the
//! SDK. That left Go and C unable to declare subnet trust anchors, a
//! security attachment, or a control channel at all, because both
//! bindings receive their node from that constructor (review-10 P1-7).
//! Nothing here needs the SDK: it is pure conversion and validation over
//! core authority types. Moving it down makes the same checked
//! conversion available to every constructor, and `net_sdk::subnet`
//! re-exports it so there is still exactly ONE definition.
//!
//! # Two layers
//!
//! Named exports and trust anchors are CONSTRUCTION state, resolved once
//! before a node exists. Everything that mutates a live node's authority
//! (installing gateway credentials, declaring boundaries, applying
//! control facts) is administration and stays on the SDK's `admin`
//! surface, above the node handle.
//!
//! # Errors
//!
//! Local construction/decode failures are [`SubnetProvisionError`],
//! whose display form is the stable `subnet:<kind>` envelope bindings
//! classify on — the kind is either a core [`SubnetAuthError`] reason
//! code (single-sourced from [`SubnetAuthError::wire_kind`]) or one of
//! the local DTO/configuration kinds in [`LOCAL_PROVISION_KINDS`].

use std::collections::BTreeMap;

use super::{
    SubnetAuthError, SubnetAuthorityConfig, SubnetControlOutcome, SubnetExportBinding,
    SubnetFactKind, SubnetRef, TopologySubnetId,
};

/// Who may call a subnet-exported service — a distinct enum rather than
/// the org facade's `OrgAccess` because `OrgAccess` implies encrypted
/// private visibility, and a subnet export is always publicly
/// discoverable (an execution boundary, not a discovery mode).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubnetExportAccess {
    /// Members of this node's own organization, under a dispatcher
    /// grant (`OrgAdmission::OwnerDelegated`).
    SameOrg,
    /// Members of another organization holding a capability grant this
    /// node's owner issued (`OrgAdmission::CrossOrgGranted`).
    Granted,
}

/// One named-export configuration entry (mesh construction input).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedSubnetExport {
    /// Provider-local label; non-empty, unique per mesh.
    pub name: String,
    /// Who may call services registered under this export.
    pub access: SubnetExportAccess,
    /// The exact authority-qualified crossing being exported.
    pub subnet: SubnetRef,
    /// The topology epoch the export was declared under.
    pub topology_epoch: u32,
}

/// The immutable, Rust-owned named-export map resolved once at node
/// construction (`SUBNET_AUTH_SDK_PLAN.md` §3.3/§3.5).
///
/// Held by the NODE — [`MeshNode::subnet_exports`] — so one name resolves
/// against one map at every language boundary, including the C ABI, which
/// has no wrapper object to hang a map on. It is construction state, not
/// capability discovery and not mutable authority state: nothing at
/// runtime adds to it, and a name is never announced or accepted from a
/// caller. Registration is one immutable lookup; there is no registry,
/// cache invalidation, sensing, or load-balancing behavior here.
///
/// [`MeshNode::subnet_exports`]: crate::adapter::net::MeshNode::subnet_exports
#[derive(Debug, Default)]
pub struct NamedSubnetExports {
    entries: BTreeMap<String, (SubnetExportAccess, SubnetExportBinding)>,
}

impl NamedSubnetExports {
    /// Validate and freeze a set of named exports. Empty and duplicate
    /// names fail before any node exists.
    pub fn try_new(
        exports: impl IntoIterator<Item = NamedSubnetExport>,
    ) -> Result<Self, SubnetProvisionError> {
        let mut entries = BTreeMap::new();
        for e in exports {
            if e.name.is_empty() {
                return Err(SubnetProvisionError::EmptyExportName);
            }
            let binding = SubnetExportBinding::new(e.subnet, e.topology_epoch);
            if entries
                .insert(e.name.clone(), (e.access, binding))
                .is_some()
            {
                return Err(SubnetProvisionError::DuplicateExportName);
            }
        }
        Ok(Self { entries })
    }

    /// Resolve a configured export by its local name.
    pub fn resolve(&self, name: &str) -> Option<(SubnetExportAccess, &SubnetExportBinding)> {
        self.entries.get(name).map(|(a, b)| (*a, b))
    }

    /// The configured export names, in stable order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    /// Whether no exports are configured.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Validate a mesh's subnet trust-anchor configuration before node
/// construction: duplicate authorities, empty root sets, duplicate
/// roots, and zero lifetimes are configuration mistakes, not runtime
/// verification outcomes (`SUBNET_AUTH_SDK_PLAN.md` §3.3).
pub fn validate_subnet_authorities(
    authorities: &[SubnetAuthorityConfig],
) -> Result<(), SubnetProvisionError> {
    let mut seen = std::collections::BTreeSet::new();
    for a in authorities {
        if !seen.insert(a.authority.as_bytes()) {
            return Err(SubnetProvisionError::DuplicateAuthority);
        }
        if a.roots.is_empty() {
            return Err(SubnetProvisionError::EmptyAuthorityRoots);
        }
        let mut roots = std::collections::BTreeSet::new();
        for r in &a.roots {
            if !roots.insert(r.as_bytes()) {
                return Err(SubnetProvisionError::DuplicateAuthorityRoot);
            }
        }
        if a.maximum_grant_lifetime_secs == 0 {
            return Err(SubnetProvisionError::ZeroGrantLifetime);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The stable local error envelope (R4)
// ---------------------------------------------------------------------------

/// Local subnet provisioning/configuration failure. Display is the
/// stable `subnet:<kind>` envelope; bindings classify on the kind, not
/// on English prose.
///
/// Core [`SubnetAuthError`]s are `Copy` and carry no detail map, so
/// this wrapper invents no `k=v` payloads. Startup-shaped by design
/// (the `OrgProvisionError` doctrine): a node either provisions
/// correctly or it does not — these are never call-path error domains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubnetProvisionError {
    /// A core credential/fact failure — decode, verification, install,
    /// or apply. The kind is the core reason code, verbatim.
    Auth(SubnetAuthError),
    /// A named export with an empty name.
    EmptyExportName,
    /// Two named exports share one name.
    DuplicateExportName,
    /// `serve_subnet_exported` named an export that is not configured.
    UnknownExportName,
    /// An authority config with no roots (would fail closed forever).
    EmptyAuthorityRoots,
    /// An authority config listing one root twice.
    DuplicateAuthorityRoot,
    /// Two authority configs for the same authority id.
    DuplicateAuthority,
    /// `maximum_grant_lifetime_secs == 0`.
    ZeroGrantLifetime,
    /// A DTO hex field that is not exactly 64 hex characters.
    InvalidIdHex,
    /// A DTO path with more than four levels.
    PathTooDeep,
    /// A path level outside `0..=255` (reachable only from binding
    /// layers whose numeric types are wider than the DTO's `u8`).
    InvalidPathLevel,
    /// A DTO access string that is neither `sameOrg` nor `granted`.
    InvalidAccess,
}

/// Every LOCAL kind this facade can emit, beside the core
/// [`SubnetAuthError`] codes. Single source for the generated
/// stable-kind fixture; adding a variant without extending this list
/// breaks the exhaustiveness test below.
pub const LOCAL_PROVISION_KINDS: &[&str] = &[
    "empty_export_name",
    "duplicate_export_name",
    "unknown_export_name",
    "empty_authority_roots",
    "duplicate_authority_root",
    "duplicate_authority",
    "zero_grant_lifetime",
    "invalid_id_hex",
    "path_too_deep",
    "invalid_path_level",
    "invalid_access",
];

impl SubnetProvisionError {
    /// The stable kind token (without the `subnet:` prefix).
    ///
    /// `&'static str`, not `String` (review-10 P2-2): every token —
    /// core reason code and local kind alike — is static, so there is
    /// nothing to allocate. Core codes come from
    /// [`SubnetAuthError::wire_kind`], the single spelling table.
    pub const fn wire_kind(&self) -> &'static str {
        match self {
            // Single-sourced from the core — the one Rust match over
            // reason codes lives there, and `Display` uses it too.
            Self::Auth(e) => e.wire_kind(),
            Self::EmptyExportName => "empty_export_name",
            Self::DuplicateExportName => "duplicate_export_name",
            Self::UnknownExportName => "unknown_export_name",
            Self::EmptyAuthorityRoots => "empty_authority_roots",
            Self::DuplicateAuthorityRoot => "duplicate_authority_root",
            Self::DuplicateAuthority => "duplicate_authority",
            Self::ZeroGrantLifetime => "zero_grant_lifetime",
            Self::InvalidIdHex => "invalid_id_hex",
            Self::PathTooDeep => "path_too_deep",
            Self::InvalidPathLevel => "invalid_path_level",
            Self::InvalidAccess => "invalid_access",
        }
    }
}

impl std::fmt::Display for SubnetProvisionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "subnet:{}", self.wire_kind())
    }
}

impl std::error::Error for SubnetProvisionError {}

impl From<SubnetAuthError> for SubnetProvisionError {
    fn from(e: SubnetAuthError) -> Self {
        Self::Auth(e)
    }
}

/// The wire token for a control-fact kind, as projected into
/// [`dto::SubnetControlOutcomeDto`] and the stable-kind fixture.
/// Exhaustive on purpose: a new core fact kind fails compilation here
/// rather than silently classifying as something else.
pub fn fact_kind_wire(kind: SubnetFactKind) -> &'static str {
    match kind {
        SubnetFactKind::Descriptor => "descriptor",
        SubnetFactKind::GatewayAdvertisement => "gateway_advertisement",
        SubnetFactKind::ExportPolicy => "export_policy",
        SubnetFactKind::RevocationFloor => "revocation_floor",
    }
}

// ---------------------------------------------------------------------------
// Binding DTOs — one Rust conversion module (plan §6)
// ---------------------------------------------------------------------------

/// The frozen binding DTOs. Node/NAPI, Python/PyO3, Go config JSON, and
/// C PODs all convert through these into core values; no DTO derives
/// directly into a core authority type, and no binding re-implements a
/// conversion.
pub mod dto {
    use serde::{Deserialize, Serialize};

    use super::*;
    use crate::adapter::net::identity::EntityId;

    /// `{ authority_hex, root_hexes, maximum_grant_lifetime_secs }`.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct SubnetAuthorityConfigDto {
        /// 32-byte entity id, 64 hex chars.
        pub authority_hex: String,
        /// Non-empty, duplicate-free set of 32-byte root entity ids.
        pub root_hexes: Vec<String>,
        /// Per-authority grant lifetime ceiling; must be nonzero.
        pub maximum_grant_lifetime_secs: u64,
    }

    /// `{ levels }` — 0..=4 `u8` labels; empty means the authority
    /// root (global) path.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct SubnetPathDto {
        /// Path labels, outermost first.
        pub levels: Vec<u8>,
    }

    /// `{ authority_hex, path }` — the authority-qualified crossing.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct SubnetRefDto {
        /// 32-byte authority entity id, 64 hex chars.
        pub authority_hex: String,
        /// Path under that authority.
        pub path: SubnetPathDto,
    }

    /// `{ authority_hex, topology_epoch, boundaries }`.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct SubnetBoundaryDeclarationDto {
        /// 32-byte authority entity id, 64 hex chars.
        pub authority_hex: String,
        /// Epoch the boundaries are declared under.
        pub topology_epoch: u32,
        /// Subtree roots whose edge is a protected boundary.
        pub boundaries: Vec<SubnetPathDto>,
    }

    /// `{ subnet, topology_epoch }` — exactly the two values the core
    /// binding captures.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct SubnetExportBindingDto {
        /// The exported crossing.
        pub subnet: SubnetRefDto,
        /// The epoch the binding was declared under.
        pub topology_epoch: u32,
    }

    /// `{ name, access, binding }` — one named-export config entry.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct SubnetNamedExportDto {
        /// Provider-local label.
        pub name: String,
        /// `"sameOrg"` or `"granted"`.
        pub access: String,
        /// The exported crossing and epoch.
        pub binding: SubnetExportBindingDto,
    }

    /// `{ kind, applied }` — the exact control-fact outcome projection.
    /// `applied: false` is an authenticated stale/idempotent outcome,
    /// not a transport failure.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct SubnetControlOutcomeDto {
        /// `"descriptor" | "gateway_advertisement" | "export_policy" |
        /// "revocation_floor"`.
        pub kind: String,
        /// Whether any state changed.
        pub applied: bool,
    }

    impl From<SubnetControlOutcome> for SubnetControlOutcomeDto {
        fn from(o: SubnetControlOutcome) -> Self {
            Self {
                kind: fact_kind_wire(o.kind).to_string(),
                applied: o.applied,
            }
        }
    }

    /// Strict 64-hex-char → 32-byte entity id. Case-insensitive,
    /// no prefixes, no whitespace.
    pub fn entity_id_from_hex(hex: &str) -> Result<EntityId, SubnetProvisionError> {
        let bytes = hex.as_bytes();
        if bytes.len() != 64 {
            return Err(SubnetProvisionError::InvalidIdHex);
        }
        let mut out = [0u8; 32];
        for (i, chunk) in bytes.as_chunks::<2>().0.iter().enumerate() {
            let hi = hex_val(chunk[0]).ok_or(SubnetProvisionError::InvalidIdHex)?;
            let lo = hex_val(chunk[1]).ok_or(SubnetProvisionError::InvalidIdHex)?;
            out[i] = (hi << 4) | lo;
        }
        Ok(EntityId::from_bytes(out))
    }

    fn hex_val(c: u8) -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    }

    impl SubnetPathDto {
        /// Convert through the core's own strict constructor.
        pub fn to_core(&self) -> Result<TopologySubnetId, SubnetProvisionError> {
            TopologySubnetId::try_new(&self.levels).map_err(|_| SubnetProvisionError::PathTooDeep)
        }
    }

    impl SubnetRefDto {
        /// Convert into the authority-qualified core type.
        pub fn to_core(&self) -> Result<SubnetRef, SubnetProvisionError> {
            Ok(SubnetRef {
                authority: entity_id_from_hex(&self.authority_hex)?,
                path: self.path.to_core()?,
            })
        }
    }

    impl SubnetAuthorityConfigDto {
        /// Convert into the core trust-anchor config; structural rules
        /// (non-empty, duplicate-free roots, nonzero lifetime) are
        /// enforced by [`validate_subnet_authorities`] over the whole
        /// set, after conversion.
        pub fn to_core(&self) -> Result<SubnetAuthorityConfig, SubnetProvisionError> {
            let authority = entity_id_from_hex(&self.authority_hex)?;
            let mut roots = Vec::with_capacity(self.root_hexes.len());
            for r in &self.root_hexes {
                roots.push(entity_id_from_hex(r)?);
            }
            Ok(SubnetAuthorityConfig {
                authority,
                roots,
                maximum_grant_lifetime_secs: self.maximum_grant_lifetime_secs,
            })
        }
    }

    impl SubnetBoundaryDeclarationDto {
        /// Convert into the `(authority, epoch, boundaries)` triple
        /// [`MeshNode::declare_subnet_boundaries`] consumes.
        ///
        /// [`MeshNode::declare_subnet_boundaries`]: crate::adapter::net::MeshNode::declare_subnet_boundaries
        pub fn to_core(
            &self,
        ) -> Result<(EntityId, u32, Vec<TopologySubnetId>), SubnetProvisionError> {
            let authority = entity_id_from_hex(&self.authority_hex)?;
            let mut boundaries = Vec::with_capacity(self.boundaries.len());
            for b in &self.boundaries {
                boundaries.push(b.to_core()?);
            }
            Ok((authority, self.topology_epoch, boundaries))
        }
    }

    impl SubnetNamedExportDto {
        /// Convert into a checked named-export entry.
        pub fn to_core(&self) -> Result<NamedSubnetExport, SubnetProvisionError> {
            let access = match self.access.as_str() {
                "sameOrg" | "same_org" => SubnetExportAccess::SameOrg,
                "granted" => SubnetExportAccess::Granted,
                _ => return Err(SubnetProvisionError::InvalidAccess),
            };
            Ok(NamedSubnetExport {
                name: self.name.clone(),
                access,
                subnet: self.binding.subnet.to_core()?,
                topology_epoch: self.binding.topology_epoch,
            })
        }
    }
}
