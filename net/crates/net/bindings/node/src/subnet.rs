//! SSDK S4a — the subnet AUTHORITY surface for Node
//! (`SUBNET_AUTH_SDK_PLAN.md` §6.1). Marshaling only: every decision
//! already happened in `net_sdk::subnet`, and anything here that looks
//! like a decision is a bug.
//!
//! Topology config (`subnet`, `subnetPolicy` in `MeshOptions`) stays in
//! `subnets.rs`; this module is the authority plane:
//!
//! - **Construction**: `MeshOptions.subnetAuthorities` /
//!   `subnetAttachment` / `subnetControlChannel` / `subnetExports` —
//!   validated through the frozen Rust DTOs before a node exists, with
//!   the checked `NamedSubnetExports` retained beside the napi handle.
//! - **The ordinary provider verb**: `serveSubnetExported(mesh,
//!   service, exportName, handler)` — resolves the NAMED export; the
//!   application constructs no authority objects.
//! - **Administration** (`subnet.admin.*` in TS): install gateway
//!   credential-set bytes, declare boundaries, apply signed control
//!   facts. Signed artifacts cross as opaque canonical wire `Buffer`s
//!   minted by `net-mesh subnet …`; nothing here signs, and no signing
//!   key type exists on this surface.
//!
//! Errors carry the stable `subnet:<kind>` envelope from
//! `SubnetProvisionError` (pinned by
//! `tests/cross_lang_subnet/stable_kinds.json`), which `errors.ts`
//! classifies.

use napi::bindgen_prelude::*;
use napi_derive::napi;

// ---------------------------------------------------------------------------
// The frozen DTOs, as JS objects (pure data — usable in any build; the
// conversions below are what need the `org` feature's SDK dependency).
// ---------------------------------------------------------------------------

/// One subnet trust anchor: `{ authorityHex, rootHexes, maximumGrantLifetimeSecs }`.
#[napi(object)]
#[derive(Clone)]
pub struct SubnetAuthorityConfigJs {
    /// 32-byte authority entity id, 64 hex chars.
    pub authority_hex: String,
    /// Non-empty, duplicate-free root entity ids (64 hex chars each).
    pub root_hexes: Vec<String>,
    /// Per-authority grant-lifetime ceiling in seconds; must be nonzero.
    pub maximum_grant_lifetime_secs: u32,
}

/// A compact hierarchy path: 0..=4 levels, each 0..=255. Empty means
/// the authority-root (global) path.
#[napi(object)]
#[derive(Clone)]
pub struct SubnetPathJs {
    /// Path labels, outermost first.
    pub levels: Vec<u32>,
}

/// An authority-qualified crossing — NOT the topology `SubnetIdJs`;
/// equal paths under two authorities are unrelated.
#[napi(object)]
#[derive(Clone)]
pub struct SubnetRefJs {
    /// 32-byte authority entity id, 64 hex chars.
    pub authority_hex: String,
    /// Path under that authority.
    pub path: SubnetPathJs,
}

/// The two values an export binding captures.
#[napi(object)]
#[derive(Clone)]
pub struct SubnetExportBindingJs {
    /// The exported crossing.
    pub subnet: SubnetRefJs,
    /// The topology epoch the binding was declared under.
    pub topology_epoch: u32,
}

/// One named-export configuration entry.
#[napi(object)]
#[derive(Clone)]
pub struct SubnetNamedExportJs {
    /// Provider-local label; non-empty, unique per mesh. Never
    /// announced and never accepted from callers.
    pub name: String,
    /// `"sameOrg"` or `"granted"`.
    pub access: String,
    /// The exported crossing and epoch.
    pub binding: SubnetExportBindingJs,
}

/// A boundary declaration: `{ authorityHex, topologyEpoch, boundaries }`.
#[napi(object)]
#[derive(Clone)]
pub struct SubnetBoundaryDeclarationJs {
    /// 32-byte authority entity id, 64 hex chars.
    pub authority_hex: String,
    /// Epoch the boundaries are declared under.
    pub topology_epoch: u32,
    /// Subtree roots whose edge is a protected boundary.
    pub boundaries: Vec<SubnetPathJs>,
}

/// The control-fact outcome projection. `applied: false` is an
/// authenticated stale/idempotent outcome, not a transport failure.
#[napi(object)]
pub struct SubnetControlOutcomeJs {
    /// `"descriptor" | "gateway_advertisement" | "export_policy" |
    /// "revocation_floor"`.
    pub kind: String,
    /// Whether any state changed.
    pub applied: bool,
}

// ---------------------------------------------------------------------------
// Conversions + verbs — need the SDK (`org` feature builds).
// ---------------------------------------------------------------------------

#[cfg(feature = "org")]
pub(crate) use gated::*;

#[cfg(feature = "org")]
mod gated {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    use net_sdk::subnet::dto;

    /// Map a provisioning failure onto its stable `subnet:<kind>` wire
    /// envelope — the single Rust source `errors.ts` classifies.
    fn subnet_error(e: net_sdk::subnet::SubnetProvisionError) -> Error {
        Error::from_reason(e.to_string())
    }

    fn path_dto(p: &SubnetPathJs) -> Result<dto::SubnetPathDto> {
        let mut levels = Vec::with_capacity(p.levels.len());
        for &l in &p.levels {
            let l = u8::try_from(l).map_err(|_| {
                // The DTO layer cannot express >255 (its levels are u8),
                // so this range check surfaces here, under its own
                // stable kind from the shared envelope.
                subnet_error(net_sdk::subnet::SubnetProvisionError::InvalidPathLevel)
            })?;
            levels.push(l);
        }
        Ok(dto::SubnetPathDto { levels })
    }

    fn ref_dto(r: &SubnetRefJs) -> Result<dto::SubnetRefDto> {
        Ok(dto::SubnetRefDto {
            authority_hex: r.authority_hex.clone(),
            path: path_dto(&r.path)?,
        })
    }

    /// Convert + validate the construction-time subnet options. Called
    /// by `NetMesh.create`; every failure precedes node construction.
    pub(crate) struct SubnetConstruction {
        pub(crate) authorities: Vec<net_sdk::subnet::SubnetAuthorityConfig>,
        pub(crate) attachment: Option<net_sdk::subnet::TopologySubnetId>,
        pub(crate) control_channel: Option<net::adapter::net::ChannelName>,
        pub(crate) exports: net_sdk::subnet::NamedSubnetExports,
    }

    pub(crate) fn convert_subnet_construction(
        authorities: &[SubnetAuthorityConfigJs],
        attachment: Option<&SubnetPathJs>,
        control_channel: Option<&str>,
        exports: &[SubnetNamedExportJs],
    ) -> Result<SubnetConstruction> {
        let mut core_authorities = Vec::with_capacity(authorities.len());
        for a in authorities {
            let d = dto::SubnetAuthorityConfigDto {
                authority_hex: a.authority_hex.clone(),
                root_hexes: a.root_hexes.clone(),
                maximum_grant_lifetime_secs: u64::from(a.maximum_grant_lifetime_secs),
            };
            core_authorities.push(d.to_core().map_err(subnet_error)?);
        }
        net_sdk::subnet::validate_subnet_authorities(&core_authorities).map_err(subnet_error)?;

        let attachment = match attachment {
            Some(p) => Some(path_dto(p)?.to_core().map_err(subnet_error)?),
            None => None,
        };

        let control_channel =
            match control_channel {
                Some(name) => Some(net::adapter::net::ChannelName::new(name).map_err(|e| {
                    Error::from_reason(format!("invalid subnetControlChannel: {e}"))
                })?),
                None => None,
            };

        let mut named = Vec::with_capacity(exports.len());
        for e in exports {
            let d = dto::SubnetNamedExportDto {
                name: e.name.clone(),
                access: e.access.clone(),
                binding: dto::SubnetExportBindingDto {
                    subnet: ref_dto(&e.binding.subnet)?,
                    topology_epoch: e.binding.topology_epoch,
                },
            };
            named.push(d.to_core().map_err(subnet_error)?);
        }
        let exports = net_sdk::subnet::NamedSubnetExports::try_new(named).map_err(subnet_error)?;

        Ok(SubnetConstruction {
            authorities: core_authorities,
            attachment,
            control_channel,
            exports,
        })
    }

    // -----------------------------------------------------------------
    // Administration (the TS `subnet.admin.*` namespace)
    // -----------------------------------------------------------------

    /// Decode and install this node's own gateway credential sets —
    /// WHOLESALE REPLACE: pass every currently held set, not a delta.
    /// Every artifact decodes BEFORE anything installs, so a malformed
    /// `Buffer` in the batch mutates no node state at all.
    #[napi]
    pub fn install_subnet_gateway_credentials(
        mesh: &crate::NetMesh,
        credential_sets: Vec<Buffer>,
    ) -> Result<()> {
        let node = mesh.node_arc_clone()?;
        let sets: Vec<Vec<u8>> = credential_sets.iter().map(|b| b.to_vec()).collect();
        net_sdk::subnet::admin::install_gateway_credentials_node(&node, &sets).map_err(subnet_error)
    }

    /// Declare this node's protected boundary inventory — also
    /// wholesale: the set replaces the previous declaration.
    #[napi]
    pub fn declare_subnet_boundaries(
        mesh: &crate::NetMesh,
        declaration: SubnetBoundaryDeclarationJs,
    ) -> Result<()> {
        let node = mesh.node_arc_clone()?;
        let mut boundaries = Vec::with_capacity(declaration.boundaries.len());
        for b in &declaration.boundaries {
            boundaries.push(path_dto(b)?);
        }
        let (authority, epoch, boundaries) = dto::SubnetBoundaryDeclarationDto {
            authority_hex: declaration.authority_hex,
            topology_epoch: declaration.topology_epoch,
            boundaries,
        }
        .to_core()
        .map_err(subnet_error)?;
        net_sdk::subnet::admin::declare_boundaries_node(&node, authority, epoch, boundaries)
            .map_err(subnet_error)
    }

    /// Apply one signed control fact from its outer wire frame — the
    /// ONE door for floors and descriptive facts alike.
    #[napi]
    pub fn apply_subnet_control_fact(
        mesh: &crate::NetMesh,
        fact: Buffer,
    ) -> Result<SubnetControlOutcomeJs> {
        let node = mesh.node_arc_clone()?;
        let outcome =
            net_sdk::subnet::admin::apply_control_fact_node(&node, &fact).map_err(subnet_error)?;
        let projected = dto::SubnetControlOutcomeDto::from(outcome);
        Ok(SubnetControlOutcomeJs {
            kind: projected.kind,
            applied: projected.applied,
        })
    }

    // -----------------------------------------------------------------
    // The ordinary provider verb
    // -----------------------------------------------------------------

    /// Serve a subnet-exported, organization-protected service against
    /// a NAMED export configured at mesh construction.
    ///
    /// Resolution is one immutable lookup on the map the mesh retained
    /// at `create()`; an unknown name fails HERE, before anything is
    /// registered or announced. The handler receives the same
    /// `{ caller, request }` shape as `serveOrg` — the verified
    /// attribution, never caller-claimed — and announcement visibility
    /// is always public: the external caller proves org authority and
    /// never joins this node's subnet.
    #[napi]
    pub fn serve_subnet_exported(
        mesh: &crate::NetMesh,
        service: String,
        export_name: String,
        handler: Function<'_, crate::org::OrgRequest, Promise<Buffer>>,
        handler_timeout_ms: Option<u32>,
    ) -> Result<crate::org::OrgServeHandle> {
        let node = mesh.node_arc_clone()?;
        let exports = mesh.subnet_exports_arc();
        let tsfn: crate::org::OrgHandlerTsfn = handler
            .build_threadsafe_function()
            .callee_handled::<false>()
            .build()?;
        let tsfn = Arc::new(tsfn);
        let timeout = match handler_timeout_ms {
            Some(0) => Duration::from_secs(u64::from(u32::MAX)),
            Some(ms) => Duration::from_millis(u64::from(ms)),
            None => Duration::from_secs(60),
        };

        // Same runtime rationale as `serve_org`: the SDK serve bridge
        // spawns with a bare `tokio::spawn` and this is a sync napi fn.
        let handle = {
            let _rt_guard = crate::org::org_serve_runtime().enter();
            net_sdk::subnet::serve_subnet_exported_bytes_node(
                node,
                &exports,
                &service,
                &export_name,
                move |caller: net_sdk::org::OrgCaller, body: bytes::Bytes| {
                    let tsfn = tsfn.clone();
                    async move { crate::org::dispatch_to_js(tsfn, caller, body, timeout).await }
                },
            )
            // Registration failures are provider-setup errors, not call
            // domains. The unknown-export refusal carries its stable
            // `subnet:unknown_export_name` kind inside this message.
            .map_err(|e| {
                Error::from_reason(format!("subnet-exported serve registration failed: {e}"))
            })?
        };

        Ok(crate::org::OrgServeHandle::from_handle(handle))
    }
}
