//! SSDK S4b — the subnet AUTHORITY surface for Python
//! (`SUBNET_AUTH_SDK_PLAN.md` §6.2). Marshaling only over
//! `net_sdk::subnet`; every authority decision already happened in Rust,
//! and anything here that looks like a decision is a bug. Mirrors
//! `bindings/node/src/subnet.rs` one-for-one.
//!
//! Two layers:
//!
//! - **Construction** (`NetMesh(subnet_authorities=…, subnet_attachment=…,
//!   subnet_control_channel=…, subnet_exports=…)`): validated through the
//!   frozen Rust DTOs before the node exists, with the checked
//!   `NamedSubnetExports` retained beside the PyO3 handle.
//! - **The ordinary provider verb**: `serve_subnet_exported(mesh,
//!   service, export_name, handler)` — resolves the NAMED export; the
//!   application constructs no authority objects.
//! - **Administration** (`net.subnet.admin.*`): install gateway
//!   credential-set bytes, declare boundaries, apply signed control
//!   facts. Signed artifacts cross as opaque canonical wire `bytes`
//!   minted by `net-mesh subnet …`; nothing here signs.
//!
//! Errors carry the stable `subnet:<kind>` envelope from
//! `SubnetProvisionError` (pinned by
//! `tests/cross_lang_subnet/stable_kinds.json`), classified by
//! `net/subnet.py`.

use std::sync::Arc;
use std::time::Duration;

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use net_sdk::subnet::dto;

pyo3::create_exception!(
    _net,
    SubnetProvisionError,
    pyo3::exceptions::PyException,
    "A subnet provisioning/configuration failure — the stable \
     `subnet:<kind>` envelope (SUBNET_AUTH_SDK_PLAN.md R4). Always LOCAL \
     and startup-shaped: configuration, decode, or install refused before \
     (or without) any node-state mutation. Never a call-path domain — a \
     remote exported-call refusal surfaces through the org taxonomy. The \
     `kind` attribute is a core reason code or a local configuration kind."
);

/// Map a provisioning failure onto its stable `subnet:<kind>` wire
/// envelope — the single Rust source `net/subnet.py` classifies.
fn subnet_err_to_py(e: net_sdk::subnet::SubnetProvisionError) -> PyErr {
    SubnetProvisionError::new_err(e.to_string())
}

// ---------------------------------------------------------------------------
// Construction-time conversion (called from `NetMesh.__new__`)
// ---------------------------------------------------------------------------

/// The converted + validated construction-time subnet options.
pub(crate) struct SubnetConstruction {
    pub(crate) authorities: Vec<net_sdk::subnet::SubnetAuthorityConfig>,
    pub(crate) attachment: Option<net_sdk::subnet::TopologySubnetId>,
    pub(crate) control_channel: Option<net::adapter::net::ChannelName>,
    pub(crate) exports: net_sdk::subnet::NamedSubnetExports,
}

fn path_from_levels(levels: &[u32]) -> PyResult<dto::SubnetPathDto> {
    let mut out = Vec::with_capacity(levels.len());
    for &l in levels {
        let l = u8::try_from(l).map_err(|_| {
            subnet_err_to_py(net_sdk::subnet::SubnetProvisionError::InvalidPathLevel)
        })?;
        out.push(l);
    }
    Ok(dto::SubnetPathDto { levels: out })
}

/// `{ "authority_hex": str, "path": {"levels": [int]} }` → core ref DTO.
fn ref_from_dict(d: &Bound<'_, PyDict>) -> PyResult<dto::SubnetRefDto> {
    let authority_hex: String = d
        .get_item("authority_hex")?
        .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("subnet ref missing 'authority_hex'"))?
        .extract()?;
    let path = d
        .get_item("path")?
        .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("subnet ref missing 'path'"))?;
    let levels: Vec<u32> = path
        .cast::<PyDict>()?
        .get_item("levels")?
        .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("subnet path missing 'levels'"))?
        .extract()?;
    Ok(dto::SubnetRefDto {
        authority_hex,
        path: path_from_levels(&levels)?,
    })
}

/// Convert + validate the four construction kwargs. Every failure
/// precedes node construction.
pub(crate) fn convert_subnet_construction(
    authorities: Option<&Bound<'_, PyList>>,
    attachment: Option<Vec<u32>>,
    control_channel: Option<&str>,
    exports: Option<&Bound<'_, PyList>>,
) -> PyResult<SubnetConstruction> {
    let mut core_authorities = Vec::new();
    if let Some(list) = authorities {
        for item in list.iter() {
            let d = item.cast::<PyDict>()?;
            let authority_hex: String = d
                .get_item("authority_hex")?
                .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("missing 'authority_hex'"))?
                .extract()?;
            let root_hexes: Vec<String> = d
                .get_item("root_hexes")?
                .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("missing 'root_hexes'"))?
                .extract()?;
            let maximum_grant_lifetime_secs: u64 = d
                .get_item("maximum_grant_lifetime_secs")?
                .ok_or_else(|| {
                    pyo3::exceptions::PyKeyError::new_err("missing 'maximum_grant_lifetime_secs'")
                })?
                .extract()?;
            let core = dto::SubnetAuthorityConfigDto {
                authority_hex,
                root_hexes,
                maximum_grant_lifetime_secs,
            }
            .to_core()
            .map_err(subnet_err_to_py)?;
            core_authorities.push(core);
        }
    }
    net_sdk::subnet::validate_subnet_authorities(&core_authorities).map_err(subnet_err_to_py)?;

    let attachment = match attachment {
        Some(levels) => Some(
            path_from_levels(&levels)?
                .to_core()
                .map_err(subnet_err_to_py)?,
        ),
        None => None,
    };

    let control_channel = match control_channel {
        Some(name) => Some(net::adapter::net::ChannelName::new(name).map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("invalid subnet_control_channel: {e}"))
        })?),
        None => None,
    };

    let mut named = Vec::new();
    if let Some(list) = exports {
        for item in list.iter() {
            let d = item.cast::<PyDict>()?;
            let name: String = d
                .get_item("name")?
                .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("export missing 'name'"))?
                .extract()?;
            let access: String = d
                .get_item("access")?
                .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("export missing 'access'"))?
                .extract()?;
            let binding = d
                .get_item("binding")?
                .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("export missing 'binding'"))?;
            let binding = binding.cast::<PyDict>()?;
            let subnet = binding
                .get_item("subnet")?
                .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("binding missing 'subnet'"))?;
            let topology_epoch: u32 = binding
                .get_item("topology_epoch")?
                .ok_or_else(|| {
                    pyo3::exceptions::PyKeyError::new_err("binding missing 'topology_epoch'")
                })?
                .extract()?;
            let core = dto::SubnetNamedExportDto {
                name,
                access,
                binding: dto::SubnetExportBindingDto {
                    subnet: ref_from_dict(subnet.cast::<PyDict>()?)?,
                    topology_epoch,
                },
            }
            .to_core()
            .map_err(subnet_err_to_py)?;
            named.push(core);
        }
    }
    let exports = net_sdk::subnet::NamedSubnetExports::try_new(named).map_err(subnet_err_to_py)?;

    Ok(SubnetConstruction {
        authorities: core_authorities,
        attachment,
        control_channel,
        exports,
    })
}

// ---------------------------------------------------------------------------
// Administration (the `net.subnet.admin.*` functions)
// ---------------------------------------------------------------------------

/// Decode and install this node's own gateway credential sets —
/// WHOLESALE REPLACE: pass every currently held set, not a delta. Every
/// artifact decodes BEFORE anything installs, so one malformed `bytes`
/// in the batch mutates no node state at all. Releases the GIL.
#[pyfunction]
pub fn install_subnet_gateway_credentials(
    py: Python<'_>,
    mesh: &crate::mesh_bindings::NetMesh,
    credential_sets: Vec<Vec<u8>>,
) -> PyResult<()> {
    let node = mesh.node_arc_clone()?;
    py.detach(|| net_sdk::subnet::admin::install_gateway_credentials_node(&node, &credential_sets))
        .map_err(subnet_err_to_py)
}

/// Declare this node's protected boundary inventory — also wholesale.
/// `declaration` is `{authority_hex, topology_epoch, boundaries: [[int]]}`.
#[pyfunction]
pub fn declare_subnet_boundaries(
    mesh: &crate::mesh_bindings::NetMesh,
    declaration: &Bound<'_, PyDict>,
) -> PyResult<()> {
    let node = mesh.node_arc_clone()?;
    let authority_hex: String = declaration
        .get_item("authority_hex")?
        .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("missing 'authority_hex'"))?
        .extract()?;
    let topology_epoch: u32 = declaration
        .get_item("topology_epoch")?
        .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("missing 'topology_epoch'"))?
        .extract()?;
    let boundary_levels: Vec<Vec<u32>> = declaration
        .get_item("boundaries")?
        .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("missing 'boundaries'"))?
        .extract()?;
    let mut boundaries = Vec::with_capacity(boundary_levels.len());
    for levels in &boundary_levels {
        boundaries.push(path_from_levels(levels)?);
    }
    let (authority, epoch, boundaries) = dto::SubnetBoundaryDeclarationDto {
        authority_hex,
        topology_epoch,
        boundaries,
    }
    .to_core()
    .map_err(subnet_err_to_py)?;
    net_sdk::subnet::admin::declare_boundaries_node(&node, authority, epoch, boundaries)
        .map_err(subnet_err_to_py)
}

/// Apply one signed control fact from its outer wire frame — the ONE
/// door for floors and descriptive facts alike. Returns
/// `{"kind": str, "applied": bool}`; `applied=False` is an authenticated
/// stale/idempotent outcome, not a failure.
#[pyfunction]
pub fn apply_subnet_control_fact<'py>(
    py: Python<'py>,
    mesh: &crate::mesh_bindings::NetMesh,
    fact: &[u8],
) -> PyResult<Bound<'py, PyDict>> {
    let node = mesh.node_arc_clone()?;
    let fact = fact.to_vec();
    let outcome = py
        .detach(|| net_sdk::subnet::admin::apply_control_fact_node(&node, &fact))
        .map_err(subnet_err_to_py)?;
    let projected = dto::SubnetControlOutcomeDto::from(outcome);
    let d = PyDict::new(py);
    d.set_item("kind", projected.kind)?;
    d.set_item("applied", projected.applied)?;
    Ok(d)
}

// ---------------------------------------------------------------------------
// The ordinary provider verb
// ---------------------------------------------------------------------------

/// Serve a subnet-exported, organization-protected service against a
/// NAMED export configured at mesh construction.
///
/// `handler` is `handler(caller: dict, request: bytes) -> bytes` — the
/// same shape as `serve_org`, with the verified attribution. Resolution
/// is one immutable lookup on the map the mesh retained at construction;
/// an unknown name fails HERE, before any registration or announcement
/// (`subnet:unknown_export_name` rides the error). Announcement
/// visibility is always public: the external caller proves org authority
/// and never joins this node's subnet.
///
/// Requires an installed node authority.
#[pyfunction]
#[pyo3(signature = (mesh, service, export_name, handler, handler_timeout_ms=None))]
pub fn serve_subnet_exported(
    py: Python<'_>,
    mesh: &crate::mesh_bindings::NetMesh,
    service: String,
    export_name: String,
    handler: Py<PyAny>,
    handler_timeout_ms: Option<u64>,
) -> PyResult<crate::org_serve::PyOrgServeHandle> {
    if !handler.bind(py).is_callable() {
        return Err(pyo3::exceptions::PyTypeError::new_err(
            "serve_subnet_exported handler must be callable: \
             handler(caller: dict, request: bytes) -> bytes",
        ));
    }
    let node = mesh.node_arc_clone()?;
    let exports = mesh.subnet_exports_arc();
    let timeout = match handler_timeout_ms {
        Some(0) => Duration::from_secs(u64::from(u32::MAX)),
        Some(ms) => Duration::from_millis(ms),
        None => Duration::from_secs(60),
    };
    let callable = Arc::new(handler);

    // Same runtime rationale as `serve_org`: the SDK serve bridge spawns
    // with a bare `tokio::spawn` and this is a sync pyfunction.
    let runtime = mesh.runtime_arc();
    let handle = {
        let _guard = runtime.enter();
        net_sdk::subnet::serve_subnet_exported_bytes_node(
            node,
            &exports,
            &service,
            &export_name,
            move |caller: net_sdk::org::OrgCaller, body: bytes::Bytes| {
                let callable = callable.clone();
                async move {
                    crate::org_serve::run_py_org_handler(callable, caller, body, timeout).await
                }
            },
        )
        // Registration failures are provider-setup errors; the
        // unknown-export refusal carries its stable
        // `subnet:unknown_export_name` kind inside this message, which
        // `net.subnet` classifies.
        .map_err(|e| {
            SubnetProvisionError::new_err(format!("subnet-exported serve registration failed: {e}"))
        })?
    };

    Ok(crate::org_serve::PyOrgServeHandle::from_handle(handle))
}
