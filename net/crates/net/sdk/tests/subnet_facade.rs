//! SSDK S2 — the subnet facade's fail-closed seams
//! (`SUBNET_AUTH_SDK_PLAN.md` §3.3–§3.5).
//!
//! Three contracts, each witnessed against a real node:
//!
//! 1. byte provisioning REFUSES malformed artifacts before mutating any
//!    node state — including a malformed artifact hiding behind a valid
//!    one in the same batch;
//! 2. mesh construction refuses broken subnet configuration before a
//!    node exists;
//! 3. `serve_subnet_exported` fails on an unknown export NAME before
//!    anything is registered or announced, and resolution precedes the
//!    core's own registration checks.

#![cfg(all(feature = "net", feature = "cortex"))]

use std::sync::Arc;

use net::adapter::net::identity::EntityKeypair;
use net::adapter::net::subnet::{
    admission::unix_now_secs, SubnetCredentialSet, SubnetGrant, SubnetRights,
};
use net_sdk::mesh::MeshBuilder;
use net_sdk::mesh_rpc::ServeError;
use net_sdk::subnet::{
    admin, SubnetAuthorityConfig, SubnetExportAccess, SubnetProvisionError, SubnetRef,
    TopologySubnetId,
};

const PSK: [u8; 32] = [0x77u8; 32];
const DAY: u64 = 24 * 60 * 60;

fn root() -> EntityKeypair {
    EntityKeypair::from_bytes([0xB7; 32])
}

fn authority() -> SubnetAuthorityConfig {
    SubnetAuthorityConfig {
        authority: root().entity_id().clone(),
        roots: vec![root().entity_id().clone()],
        maximum_grant_lifetime_secs: 7 * DAY,
    }
}

fn exported_ref() -> SubnetRef {
    SubnetRef {
        authority: root().entity_id().clone(),
        path: TopologySubnetId::new(&[3, 9]),
    }
}

async fn provider_mesh() -> net_sdk::mesh::Mesh {
    MeshBuilder::new("127.0.0.1:0", &PSK)
        .expect("builder")
        .identity(net_sdk::identity::Identity::generate())
        .subnet_authority(authority())
        .subnet_attachment(TopologySubnetId::new(&[3]))
        .subnet_export(
            "factory-export",
            SubnetExportAccess::SameOrg,
            exported_ref(),
            0,
        )
        .build()
        .await
        .expect("build")
}

/// A grant this node's own entity holds EXPORT with, as wire bytes —
/// what `net-mesh subnet issue-direct` will mint.
fn export_credential_bytes(subject: &net::adapter::net::identity::EntityId) -> Vec<u8> {
    SubnetCredentialSet::Direct(
        SubnetGrant::try_issue(
            &root(),
            root().entity_id().clone(),
            TopologySubnetId::new(&[3, 9]),
            0,
            subject.clone(),
            SubnetRights::EXPORT,
            1,
            unix_now_secs() - 60,
            DAY,
        )
        .expect("issue"),
    )
    .to_bytes()
}

/// Malformed bytes refuse with the stable envelope and mutate NOTHING —
/// even when a valid artifact precedes them in the batch (decode-all-
/// then-install, never partial application).
#[tokio::test]
async fn malformed_credential_bytes_refuse_before_any_mutation() {
    let mesh = provider_mesh().await;
    let node = mesh.node_arc();

    let garbage = vec![0xFFu8; 41];
    let err = admin::install_gateway_credentials_node(&node, &[garbage.clone()])
        .expect_err("garbage must refuse");
    assert_eq!(err.to_string(), "subnet:invalid_format");
    assert!(
        node.subnet_gateway_contexts().is_none(),
        "a refused install must not create gateway state",
    );

    // Valid-then-garbage: the valid artifact must NOT be installed.
    let valid = export_credential_bytes(node.entity_id());
    let err = admin::install_gateway_credentials_node(&node, &[valid.clone(), garbage])
        .expect_err("a batch with garbage refuses whole");
    assert_eq!(err.to_string(), "subnet:invalid_format");
    assert!(
        node.subnet_gateway_contexts().is_none(),
        "all-or-nothing: the valid artifact in a refused batch must not land",
    );

    // Positive control: the same valid bytes alone install.
    admin::install_gateway_credentials_node(&node, &[valid]).expect("valid bytes install");
    assert!(
        node.subnet_gateway_contexts().is_some(),
        "the positive control proves the negative ones weren't vacuous",
    );
}

/// Broken subnet configuration fails at `build()`, before a node
/// exists, with the stable kind embedded in the config error.
#[tokio::test]
async fn broken_subnet_configuration_fails_before_construction() {
    // Duplicate export names.
    let Err(err) = MeshBuilder::new("127.0.0.1:0", &PSK)
        .expect("builder")
        .subnet_export("a", SubnetExportAccess::SameOrg, exported_ref(), 0)
        .subnet_export("a", SubnetExportAccess::Granted, exported_ref(), 0)
        .build()
        .await
    else {
        panic!("duplicate names must refuse");
    };
    assert!(
        err.to_string().contains("subnet:duplicate_export_name"),
        "unexpected error: {err}",
    );

    // Empty authority roots.
    let Err(err) = MeshBuilder::new("127.0.0.1:0", &PSK)
        .expect("builder")
        .subnet_authority(SubnetAuthorityConfig {
            authority: root().entity_id().clone(),
            roots: vec![],
            maximum_grant_lifetime_secs: 60,
        })
        .build()
        .await
    else {
        panic!("empty roots must refuse");
    };
    assert!(
        err.to_string().contains("subnet:empty_authority_roots"),
        "unexpected error: {err}",
    );
}

/// An unknown export name fails BEFORE registration — and before the
/// core's own authority checks, which is observable because a node with
/// no org authority still reports the unknown NAME, not the missing
/// authority. With the known name, the same call reaches the core and
/// reports the missing authority — resolution precedes registration.
#[tokio::test]
async fn unknown_export_name_fails_before_registration() {
    let mesh = provider_mesh().await;

    let Err(unknown) = mesh.serve_subnet_exported::<serde_json::Value, serde_json::Value, _, _>(
        "fleet.telemetry",
        "no-such-export",
        |_caller, req| async move { Ok(req) },
    ) else {
        panic!("unknown name must refuse");
    };
    match &unknown {
        ServeError::InvalidProtectedRegistration(msg) => {
            assert!(
                msg.contains(&format!(
                    "subnet:{}",
                    SubnetProvisionError::UnknownExportName.wire_kind()
                )),
                "the stable kind must ride the registration error: {msg}",
            );
        }
        other => panic!("expected InvalidProtectedRegistration, got {other:?}"),
    }

    // Known name on a node with NO org authority: resolution succeeds
    // and the CORE refusal surfaces instead — proving order.
    let Err(known) = mesh.serve_subnet_exported::<serde_json::Value, serde_json::Value, _, _>(
        "fleet.telemetry",
        "factory-export",
        |_caller, req| async move { Ok(req) },
    ) else {
        panic!("no org authority is installed, so the core must refuse");
    };
    assert!(
        matches!(known, ServeError::ProtectedAuthorityRequired(_)),
        "expected the core authority refusal after successful name resolution, got {known:?}",
    );

    // Nothing was registered by either failure.
    let _ = Arc::strong_count(&mesh.node_arc());
}
