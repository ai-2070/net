//! SSDK — the subnet **authority** facade (`SUBNET_AUTH_SDK_PLAN.md`).
//!
//! Topology stays in [`crate::subnets`]; this module is the authority
//! plane, and it has exactly two layers:
//!
//! **The ordinary application surface is two verbs.** A provider serves
//! against a *named export* configured at mesh construction, and a
//! caller invokes with its existing organization authority:
//!
//! ```ignore
//! // mesh construction (operator configuration):
//! let mesh = MeshBuilder::new(addr, &psk)?
//!     .subnet_authority(authority_config)
//!     .subnet_attachment(attachment)
//!     .subnet_export("factory-export", SubnetExportAccess::Granted, subnet_ref, 0)
//!     .build().await?;
//!
//! // provider — names the export, constructs no authority objects:
//! mesh.serve_subnet_exported("fleet.telemetry", "factory-export",
//!     |caller: OrgCaller, req: Request| async move { Ok(answer(caller, req)) })?;
//!
//! // caller — organization credentials only:
//! let resp: Response = org.call_exported("fleet.telemetry", &request).await?;
//! ```
//!
//! Neither verb accepts roots, credentials, boundaries, topology epochs,
//! or a [`SubnetRef`]. The export *name* is a provider-local
//! configuration label: never announced, never accepted from callers.
//!
//! **Everything else is administration**, under [`admin`]: installing
//! gateway credential-set bytes, declaring boundaries, applying signed
//! control facts. Signed artifacts cross this facade as opaque canonical
//! wire bytes minted by `net-mesh subnet …`; nothing here signs, and no
//! signing key type is re-exported. Native Rust applications that
//! intentionally need the low-level issuer API use the core crate.
//!
//! # Errors
//!
//! Local construction/decode/install failures are
//! [`SubnetProvisionError`], whose display form is the stable
//! `subnet:<kind>` envelope bindings classify on — the kind is either a
//! core [`SubnetAuthError`] reason code (single-sourced from the core's
//! own `Display`) or one of the local DTO/configuration kinds in
//! [`LOCAL_PROVISION_KINDS`]. Network call errors continue through
//! `OrgSdkError`/`RpcError`; there is no second remote denial taxonomy.

use std::collections::BTreeMap;
use std::sync::Arc;

use net::adapter::net::subnet::SubnetAuthError;
use net::adapter::net::MeshNode;

// The minimum public types needed to configure a named export and
// observe outcomes. Deliberately absent: grants, floors, presentations,
// keypairs — issuance and verification are not runtime SDK operations.
pub use net::adapter::net::subnet::{
    SubnetAuthorityConfig, SubnetControlOutcome, SubnetExportBinding, SubnetFactKind, SubnetRef,
    TopologySubnetId,
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

/// The immutable, Rust-owned named-export map resolved once at mesh
/// construction (`SUBNET_AUTH_SDK_PLAN.md` §3.3/§3.5).
///
/// Stored beside the mesh handle — the SDK [`Mesh`](crate::mesh::Mesh)
/// carries one, and every language binding retains the same checked map
/// beside its `Arc<MeshNode>` — never inside capability discovery or
/// mutable core authority state. Registration resolves a name with one
/// immutable map lookup; there is no registry, cache invalidation,
/// sensing, or load-balancing behavior here.
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

/// The canonical relative path of the committed cross-language
/// stable-kind fixture, from the core crate root.
pub const STABLE_KINDS_FIXTURE_PATH: &str = "tests/cross_lang_subnet/stable_kinds.json";

/// Render the cross-language stable-kind fixture EXACTLY as it is
/// committed — the single renderer (review-10 P3-1).
///
/// `gen_subnet_kind_fixture` writes this string verbatim and the
/// fixture test byte-compares the committed file against it, so the
/// "committed output is exact" claim is a byte equality rather than a
/// field-by-field comparison that unexpected keys, reordering, or
/// formatting drift could slip past.
///
/// Deterministic by construction: no timestamps, no randomness, no
/// map iteration order — `serde_json`'s pretty printer over a literal
/// object, plus a trailing newline.
pub fn render_stable_kind_fixture() -> String {
    let fact_kinds = [
        SubnetFactKind::Descriptor,
        SubnetFactKind::GatewayAdvertisement,
        SubnetFactKind::ExportPolicy,
        SubnetFactKind::RevocationFloor,
    ]
    .map(fact_kind_wire);

    let fixture = serde_json::json!({
        "version": 1,
        "prefix": "subnet:",
        "auth_kinds": subnet_auth_kinds(),
        "local_kinds": LOCAL_PROVISION_KINDS,
        "fact_kinds": fact_kinds,
        "access": ["sameOrg", "granted"],
    });
    let mut body = serde_json::to_string_pretty(&fixture).expect("fixture serializes");
    body.push('\n');
    body
}

/// Every core [`SubnetAuthError`] reason code, in canonical order.
///
/// A thin projection of the core's own [`SubnetAuthError::ALL`] +
/// [`SubnetAuthError::wire_kind`] (review-10 P2-2). This used to keep a
/// SECOND copy of every token, so the generated fixture and every
/// language consumer stayed green while an intermediate core spelling
/// changed underneath them — the fixture guard only spot-checked the
/// first and last codes against the core. There is now exactly one
/// spelling table, in the core, and `Display` renders through it too.
pub fn subnet_auth_kinds() -> Vec<&'static str> {
    SubnetAuthError::ALL
        .iter()
        .map(SubnetAuthError::wire_kind)
        .collect()
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
    use net::adapter::net::identity::EntityId;

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
        for (i, chunk) in bytes.chunks_exact(2).enumerate() {
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
        /// Convert into the pieces
        /// [`admin::declare_boundaries_node`] consumes.
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

// ---------------------------------------------------------------------------
// Administration — explicitly advanced (plan §3.4/§6)
// ---------------------------------------------------------------------------

/// Runtime subnet administration. Everything here is an operator
/// surface, deliberately NOT placed beside ordinary service calls:
/// installing gateway credentials, declaring boundaries, and applying
/// signed control facts are authority-state mutations, and the ordinary
/// application never performs them.
///
/// All three are node-based seams (bindings hold `Arc<MeshNode>`).
/// Artifacts arrive as opaque canonical wire bytes; every decode
/// happens here, in Rust, before any node state is touched.
pub mod admin {
    use super::*;
    use net::adapter::net::subnet::{SubnetBoundarySet, SubnetCredentialSet};

    /// Decode and install this node's own gateway credential sets —
    /// WHOLESALE REPLACE: the substrate compiles and atomically
    /// publishes the complete set, so pass every currently-held
    /// credential, not a delta.
    ///
    /// Fail-closed ordering: every artifact is decoded BEFORE anything
    /// is installed, so a malformed byte string in the batch mutates no
    /// node state at all.
    pub fn install_gateway_credentials_node(
        node: &Arc<MeshNode>,
        credential_sets: &[Vec<u8>],
    ) -> Result<(), SubnetProvisionError> {
        let mut decoded = Vec::with_capacity(credential_sets.len());
        for bytes in credential_sets {
            decoded.push(SubnetCredentialSet::from_bytes(bytes)?);
        }
        node.install_subnet_gateway_credentials(&decoded)?;
        Ok(())
    }

    /// Declare this node's protected boundary inventory — also
    /// wholesale: the set replaces the previous declaration.
    ///
    /// The core declaration is infallible after construction
    /// ([`SubnetBoundarySet::new`] sorts and deduplicates), so the
    /// `Result` covers only future DTO/path conversion at this seam —
    /// it never fabricates a declaration-time authorization failure.
    pub fn declare_boundaries_node(
        node: &Arc<MeshNode>,
        authority: net::adapter::net::identity::EntityId,
        topology_epoch: u32,
        boundaries: Vec<TopologySubnetId>,
    ) -> Result<(), SubnetProvisionError> {
        node.declare_subnet_boundaries(SubnetBoundarySet::new(
            authority,
            topology_epoch,
            boundaries,
        ));
        Ok(())
    }

    /// Apply one signed control fact from its outer wire frame — the
    /// ONE door every arrival path shares (floors and the three
    /// descriptive kinds alike). `applied: false` in the outcome is an
    /// authenticated stale/idempotent result, not a failure.
    pub fn apply_control_fact_node(
        node: &Arc<MeshNode>,
        fact: &[u8],
    ) -> Result<SubnetControlOutcome, SubnetProvisionError> {
        Ok(node.apply_subnet_control_fact(fact)?)
    }
}

// ---------------------------------------------------------------------------
// The provider verb (plan §3.5) — needs the org admission plane
// ---------------------------------------------------------------------------

#[cfg(feature = "cortex")]
mod serve {
    use bytes::Bytes;

    use super::*;
    use crate::mesh::Mesh;
    use crate::mesh_rpc::{
        Codec, ServeError, ServeHandle, NRPC_TYPED_BAD_REQUEST, NRPC_TYPED_HANDLER_ERROR,
    };
    use crate::org::{OrgCaller, OrgHandlerError};
    use net::adapter::net::behavior::org_admission::OrgAdmission;

    /// Register a subnet-exported service on a NODE against a NAMED
    /// export — the one implementation of the exported serve pipeline;
    /// [`Mesh::serve_subnet_exported_bytes`] delegates here and so does
    /// every language binding (each passing the checked
    /// [`NamedSubnetExports`] it retains beside its `Arc<MeshNode>`).
    ///
    /// Resolution is one immutable map lookup, and an unknown name
    /// fails HERE — before any registration or announcement. The seam
    /// owns the canonical request/reply channel registration (the same
    /// shared `install_rpc_service_defaults` path `serve_org` uses),
    /// the verified-`OrgCaller` projection, the trivial v1 provider
    /// policy (the application decision is the handler, which holds the
    /// verified facts), and raw handler error mapping. Low-level Rust
    /// callers that need a pre-replay-insert provider-policy veto or a
    /// dynamic binding keep `MeshNode::serve_rpc_subnet_exported`.
    ///
    /// `#[doc(hidden)]` — applications use
    /// [`Mesh::serve_subnet_exported`]; this is the binding seam.
    #[doc(hidden)]
    pub fn serve_subnet_exported_bytes_node<F, Fut>(
        node: std::sync::Arc<MeshNode>,
        named_exports: &NamedSubnetExports,
        service: &str,
        export_name: &str,
        handler: F,
    ) -> Result<ServeHandle, ServeError>
    where
        F: Fn(OrgCaller, Bytes) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<Bytes, OrgHandlerError>> + Send + 'static,
    {
        let Some((access, binding)) = named_exports.resolve(export_name) else {
            // The stable kind rides the registration error so bindings
            // classify without a second error taxonomy; nothing was
            // registered or announced.
            return Err(ServeError::InvalidProtectedRegistration(format!(
                "subnet:{}: no configured subnet export named {export_name:?}",
                SubnetProvisionError::UnknownExportName.wire_kind(),
            )));
        };
        let admission = match access {
            SubnetExportAccess::SameOrg => OrgAdmission::OwnerDelegated,
            SubnetExportAccess::Granted => OrgAdmission::CrossOrgGranted,
        };
        let raw = crate::org::org_bytes_handler(handler);
        crate::org::auto_register_org_channels(&node, service);
        node.serve_rpc_subnet_exported(
            service,
            raw,
            admission,
            binding.clone(),
            std::sync::Arc::new(|_| true),
        )
    }

    impl Mesh {
        /// Serve a subnet-exported, organization-protected service
        /// against a named export configured at mesh construction
        /// (`SUBNET_AUTH_SDK_PLAN.md` §3.5).
        ///
        /// The ordinary provider surface: name the service, name the
        /// export, provide the handler. No authority objects are
        /// constructed in application code — the export was resolved
        /// into a checked binding when the mesh was built, and dispatch
        /// revalidates the exact crossing against this node's LIVE
        /// gateway authority on every call, before organization
        /// admission. Announcement visibility is always public; the
        /// external caller proves org authority and never joins this
        /// node's subnet.
        pub fn serve_subnet_exported<Req, Resp, F, Fut>(
            &self,
            service: &str,
            export_name: &str,
            handler: F,
        ) -> Result<ServeHandle, ServeError>
        where
            Req: serde::de::DeserializeOwned + Send + Sync + 'static,
            Resp: serde::Serialize + Send + Sync + 'static,
            F: Fn(OrgCaller, Req) -> Fut + Send + Sync + 'static,
            Fut: std::future::Future<Output = Result<Resp, String>> + Send + 'static,
        {
            let codec = Codec::Json;
            let inner = std::sync::Arc::new(handler);
            self.serve_subnet_exported_bytes(service, export_name, move |caller, body: Bytes| {
                let inner = inner.clone();
                async move {
                    let req: Req =
                        codec
                            .decode(&body)
                            .map_err(|e| OrgHandlerError::Application {
                                code: NRPC_TYPED_BAD_REQUEST,
                                message: format!("subnet-exported handler: bad request: {e}"),
                            })?;
                    let resp = inner(caller, req).await.map_err(|message| {
                        OrgHandlerError::Application {
                            code: NRPC_TYPED_HANDLER_ERROR,
                            message,
                        }
                    })?;
                    let out = codec.encode(&resp).map_err(|e| {
                        OrgHandlerError::Internal(format!(
                            "subnet-exported handler: response encode: {e}"
                        ))
                    })?;
                    Ok(Bytes::from(out))
                }
            })
        }

        /// [`serve_subnet_exported`](Self::serve_subnet_exported)
        /// without the codec — bytes in, bytes out; the seam language
        /// bindings register against.
        pub fn serve_subnet_exported_bytes<F, Fut>(
            &self,
            service: &str,
            export_name: &str,
            handler: F,
        ) -> Result<ServeHandle, ServeError>
        where
            F: Fn(OrgCaller, Bytes) -> Fut + Send + Sync + 'static,
            Fut: std::future::Future<Output = Result<Bytes, OrgHandlerError>> + Send + 'static,
        {
            serve_subnet_exported_bytes_node(
                self.node().clone(),
                self.subnet_exports(),
                service,
                export_name,
                handler,
            )
        }
    }
}

#[cfg(feature = "cortex")]
pub use serve::serve_subnet_exported_bytes_node;

// ---------------------------------------------------------------------------
// Unit witnesses — the pure parts
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use net::adapter::net::identity::EntityKeypair;

    fn subnet_ref(seed: u8, levels: &[u8]) -> SubnetRef {
        SubnetRef {
            authority: EntityKeypair::from_bytes([seed; 32]).entity_id().clone(),
            path: TopologySubnetId::new(levels),
        }
    }

    fn export(name: &str) -> NamedSubnetExport {
        NamedSubnetExport {
            name: name.to_string(),
            access: SubnetExportAccess::Granted,
            subnet: subnet_ref(0xE1, &[3, 9]),
            topology_epoch: 0,
        }
    }

    #[test]
    fn named_exports_refuse_empty_and_duplicate_names() {
        assert_eq!(
            NamedSubnetExports::try_new([export("")]).unwrap_err(),
            SubnetProvisionError::EmptyExportName,
        );
        assert_eq!(
            NamedSubnetExports::try_new([export("a"), export("a")]).unwrap_err(),
            SubnetProvisionError::DuplicateExportName,
        );
        let ok = NamedSubnetExports::try_new([export("a"), export("b")]).expect("distinct names");
        assert!(ok.resolve("a").is_some());
        assert!(ok.resolve("missing").is_none());
    }

    #[test]
    fn authority_validation_refuses_the_configuration_mistakes() {
        let root = EntityKeypair::from_bytes([0xA0; 32]);
        let auth = |roots: Vec<_>, life| SubnetAuthorityConfig {
            authority: root.entity_id().clone(),
            roots,
            maximum_grant_lifetime_secs: life,
        };
        assert_eq!(
            validate_subnet_authorities(&[auth(vec![], 60)]).unwrap_err(),
            SubnetProvisionError::EmptyAuthorityRoots,
        );
        assert_eq!(
            validate_subnet_authorities(&[auth(
                vec![root.entity_id().clone(), root.entity_id().clone()],
                60
            )])
            .unwrap_err(),
            SubnetProvisionError::DuplicateAuthorityRoot,
        );
        assert_eq!(
            validate_subnet_authorities(&[auth(vec![root.entity_id().clone()], 0)]).unwrap_err(),
            SubnetProvisionError::ZeroGrantLifetime,
        );
        let dup = auth(vec![root.entity_id().clone()], 60);
        assert_eq!(
            validate_subnet_authorities(&[dup.clone(), dup]).unwrap_err(),
            SubnetProvisionError::DuplicateAuthority,
        );
    }

    /// The display envelope is exactly `subnet:<kind>` for every
    /// variant, and the local kind list is complete — an exhaustive
    /// match with no wildcard, so a new variant fails compilation
    /// until both this match and [`LOCAL_PROVISION_KINDS`] learn it.
    #[test]
    fn provision_error_display_is_the_stable_envelope() {
        use SubnetProvisionError as P;
        let locals = [
            P::EmptyExportName,
            P::DuplicateExportName,
            P::UnknownExportName,
            P::EmptyAuthorityRoots,
            P::DuplicateAuthorityRoot,
            P::DuplicateAuthority,
            P::ZeroGrantLifetime,
            P::InvalidIdHex,
            P::PathTooDeep,
            P::InvalidPathLevel,
            P::InvalidAccess,
        ];
        fn assert_covered(e: SubnetProvisionError) {
            use SubnetProvisionError as P;
            match e {
                P::Auth(_)
                | P::EmptyExportName
                | P::DuplicateExportName
                | P::UnknownExportName
                | P::EmptyAuthorityRoots
                | P::DuplicateAuthorityRoot
                | P::DuplicateAuthority
                | P::ZeroGrantLifetime
                | P::InvalidIdHex
                | P::PathTooDeep
                | P::InvalidPathLevel
                | P::InvalidAccess => {}
            }
        }
        let _ = assert_covered;
        assert_eq!(locals.len(), LOCAL_PROVISION_KINDS.len());
        for (e, kind) in locals.iter().zip(LOCAL_PROVISION_KINDS) {
            assert_eq!(e.to_string(), format!("subnet:{kind}"));
            assert_eq!(&e.wire_kind(), kind);
        }
        // Core codes pass through verbatim under the same prefix.
        let auth = P::Auth(SubnetAuthError::ScopeNotAncestor);
        assert_eq!(auth.to_string(), "subnet:scope_not_ancestor");
    }

    /// The fixture's core-kind list matches the core's own `Display`
    /// for EVERY variant — not a spot check of the two ends
    /// (review-10 P2-2).
    ///
    /// This is now a tautology by construction (`subnet_auth_kinds` IS
    /// the core's table), and that is the point: the assertion that
    /// used to need writing is the one the type system now makes
    /// unrepresentable. It stays as the regression that fails if anyone
    /// reintroduces a second spelling table here.
    #[test]
    fn subnet_auth_kinds_match_core_display() {
        let kinds = subnet_auth_kinds();
        assert_eq!(
            kinds.len(),
            SubnetAuthError::ALL.len(),
            "one entry per core variant",
        );
        for (kind, variant) in kinds.iter().zip(SubnetAuthError::ALL) {
            assert!(!kind.is_empty());
            assert_eq!(
                *kind,
                variant.to_string(),
                "the fixture token must be the core's own Display, verbatim",
            );
        }
    }

    #[test]
    fn dto_conversions_are_strict() {
        use dto::*;
        // Hex: wrong length, wrong chars.
        assert!(entity_id_from_hex("ab").is_err());
        assert!(entity_id_from_hex(&"g".repeat(64)).is_err());
        let ok = entity_id_from_hex(&"ab".repeat(32)).expect("64 hex chars");
        assert_eq!(ok.as_bytes()[0], 0xAB);

        // Paths: five levels refuse; four and empty pass.
        assert_eq!(
            SubnetPathDto {
                levels: vec![1, 2, 3, 4, 5]
            }
            .to_core()
            .unwrap_err(),
            SubnetProvisionError::PathTooDeep,
        );
        assert!(SubnetPathDto { levels: vec![] }.to_core().is_ok());
        assert!(SubnetPathDto {
            levels: vec![3, 9, 1, 4]
        }
        .to_core()
        .is_ok());

        // Access strings: both spellings of sameOrg, granted, nothing else.
        let named = |access: &str| SubnetNamedExportDto {
            name: "x".to_string(),
            access: access.to_string(),
            binding: SubnetExportBindingDto {
                subnet: SubnetRefDto {
                    authority_hex: "cd".repeat(32),
                    path: SubnetPathDto { levels: vec![3] },
                },
                topology_epoch: 0,
            },
        };
        assert!(named("sameOrg").to_core().is_ok());
        assert!(named("same_org").to_core().is_ok());
        assert!(named("granted").to_core().is_ok());
        assert_eq!(
            named("public").to_core().unwrap_err(),
            SubnetProvisionError::InvalidAccess,
        );
    }

    #[test]
    fn fact_kinds_project_to_stable_tokens() {
        assert_eq!(fact_kind_wire(SubnetFactKind::Descriptor), "descriptor");
        assert_eq!(
            fact_kind_wire(SubnetFactKind::GatewayAdvertisement),
            "gateway_advertisement"
        );
        assert_eq!(
            fact_kind_wire(SubnetFactKind::ExportPolicy),
            "export_policy"
        );
        assert_eq!(
            fact_kind_wire(SubnetFactKind::RevocationFloor),
            "revocation_floor"
        );
    }
}
