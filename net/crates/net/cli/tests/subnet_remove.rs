//! `net-mesh subnet remove` (V3-4 slice 2): sign a subject floor, hand it to
//! each named verifier, and report per verifier from its own signed
//! attestation. The verifiers are real in-process mesh nodes; the CLI is a
//! real subprocess reaching each over its own attached session. One
//! verifier is durable, one keeps floors only in memory, one predates
//! readback — and the result says exactly that, with `complete` false until
//! every named verifier attests a persisted floor.
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use assert_cmd::prelude::*;
use net::adapter::net::identity::{EntityId, EntityKeypair};
use net::adapter::net::subnet::{
    admission::unix_now_secs, SubnetAuthError, SubnetAuthPresentation, SubnetAuthorityConfig,
    SubnetCredentialSet, SubnetGrant, SubnetRef, SubnetRights, TopologySubnetId,
};
use net::adapter::net::{MeshNode, MeshNodeConfig, SocketBufferConfig};
use serde_json::Value;

const PSK: [u8; 32] = [0x6C; 32];
const DAY: u64 = 24 * 60 * 60;

fn keygen(dir: &Path) -> (std::path::PathBuf, EntityId, String) {
    let key = dir.join("root.toml");
    Command::cargo_bin("net-mesh")
        .unwrap()
        .args(["subnet", "keygen", "--out"])
        .arg(&key)
        .assert()
        .code(0);
    let text = std::fs::read_to_string(&key).unwrap();
    let hex = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("entity_id_hex"))
        .unwrap()
        .trim_start_matches(['=', ' '])
        .trim()
        .trim_matches('"')
        .to_string();
    let bytes: [u8; 32] = hex::decode(&hex).unwrap().try_into().unwrap();
    (key, EntityId::from_bytes(bytes), hex)
}

fn config(root: &EntityId, store: Option<&Path>) -> MeshNodeConfig {
    let mut cfg = MeshNodeConfig::new("127.0.0.1:0".parse().unwrap(), PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5))
        .with_handshake(3, Duration::from_secs(2))
        .with_subnet_authority(SubnetAuthorityConfig {
            authority: root.clone(),
            roots: vec![root.clone()],
            maximum_grant_lifetime_secs: 7 * DAY,
        });
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: 256 * 1024,
        recv_buffer_size: 256 * 1024,
    };
    match store {
        Some(dir) => cfg.with_subnet_floor_store(dir),
        None => cfg,
    }
}

async fn verifier(root: &EntityId, store: Option<&Path>) -> Arc<MeshNode> {
    let node = Arc::new(
        MeshNode::new(EntityKeypair::generate(), config(root, store))
            .await
            .unwrap(),
    );
    node.start();
    node
}

fn contact(node: &MeshNode) -> String {
    format!(
        "{}@{}#{}",
        hex::encode(node.entity_id().as_bytes()),
        node.local_addr(),
        hex::encode(node.public_key())
    )
}

async fn remove(key: &Path, authority: &str, subject: &str, extra: Vec<String>) -> Value {
    let (key, authority, subject) = (
        key.to_path_buf(),
        authority.to_string(),
        subject.to_string(),
    );
    tokio::task::spawn_blocking(move || {
        let out = Command::cargo_bin("net-mesh")
            .unwrap()
            .env_remove("NET_MESH_CONFIG")
            .env_remove("NET_MESH_PROFILE")
            .args(["--output", "json", "subnet", "remove", "--root-key"])
            .arg(&key)
            .args([
                "--authority",
                &authority,
                "--scope",
                "3.7",
                "--topology-epoch",
                "0",
            ])
            .args(["--subject", &subject, "--minimum-generation", "2"])
            .args(["--psk-hex", &hex::encode(PSK), "--wait", "6s"])
            .args(extra)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        serde_json::from_slice(&out.stdout).unwrap()
    })
    .await
    .unwrap()
}

fn row<'a>(result: &'a Value, node: &MeshNode) -> &'a Value {
    let entity = hex::encode(node.entity_id().as_bytes());
    result["verifiers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["verifier"] == entity.as_str())
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn removal_reports_each_named_verifier_from_its_own_attestation() {
    let tmp = tempfile::tempdir().unwrap();
    let (key, root, authority) = keygen(tmp.path());
    let durable = verifier(&root, Some(&tmp.path().join("floors"))).await;
    let volatile = verifier(&root, None).await;
    let older = verifier(&root, None).await;
    let _serving = [
        durable.serve_subnet_floor_status().unwrap(),
        volatile.serve_subnet_floor_status().unwrap(),
    ];
    let b = EntityKeypair::generate();
    let b_hex = hex::encode(b.entity_id().as_bytes());
    let every = |extra: &[&str]| {
        let mut args: Vec<String> = [&durable, &volatile, &older]
            .iter()
            .flat_map(|v| ["--verifier".to_string(), contact(v)])
            .collect();
        args.extend(extra.iter().map(|s| s.to_string()));
        args
    };

    // Dry run first: nothing applied anywhere, never complete.
    let dry = remove(
        &key,
        &authority,
        &b_hex,
        every(&["--revision", "1", "--dry-run"]),
    )
    .await;
    assert_eq!(dry["action"], "dry_run");
    assert_eq!(dry["complete"], false);
    assert_eq!(row(&dry, &durable)["apply"], "not_requested");
    assert_eq!(row(&dry, &durable)["state"], "not_applied");

    let result = remove(&key, &authority, &b_hex, every(&["--revision", "1"])).await;
    assert_eq!(result["rights"], "attach");
    assert_eq!(row(&result, &durable)["state"], "applied", "{result}");
    assert_eq!(row(&result, &durable)["attested"], true);
    assert_eq!(
        row(&result, &volatile)["state"],
        "applied_not_persisted",
        "{result}"
    );
    assert_eq!(row(&result, &older)["state"], "no_attestation", "{result}");
    assert_eq!(row(&result, &older)["attested"], false);
    assert_eq!(result["applied"], 1);
    assert_eq!(result["pending"], 2);
    assert_eq!(
        result["complete"], false,
        "one durable verifier is not fleet-wide removal"
    );

    // The durable verifier really enforces it: B's old grant is refused there.
    let set = SubnetCredentialSet::Direct(
        SubnetGrant::try_issue(
            &EntityKeypair::from_bytes(
                // The CLI's root key file holds the seed; re-derive for the grant.
                seed_of(&key),
            ),
            root.clone(),
            TopologySubnetId::new(&[3, 7]),
            0,
            b.entity_id().clone(),
            SubnetRights::ATTACH,
            1,
            unix_now_secs() - 60,
            DAY,
        )
        .unwrap(),
    );
    let b_node = Arc::new(MeshNode::new(b.clone(), config(&root, None)).await.unwrap());
    b_node.start();
    b_node
        .connect_via(
            durable.local_addr(),
            durable.public_key(),
            durable.node_id(),
        )
        .await
        .unwrap();
    let nonce = durable.issue_subnet_challenge(b.node_id()).unwrap();
    let presentation = SubnetAuthPresentation::try_issue(
        &b,
        set.credential_set_hash(),
        durable.peer_session_id(b.node_id()).unwrap(),
        durable.entity_id().clone(),
        nonce,
        SubnetRef {
            authority: root.clone(),
            path: TopologySubnetId::new(&[3, 7]),
        },
        SubnetRights::ATTACH,
    )
    .unwrap();
    assert_eq!(
        durable
            .admit_subnet_session(b.node_id(), &presentation, &set)
            .unwrap_err(),
        SubnetAuthError::Revoked
    );

    // Naming only verifiers that persist it: complete.
    let only = remove(
        &key,
        &authority,
        &b_hex,
        vec![
            "--verifier".into(),
            contact(&durable),
            "--revision".into(),
            "1".into(),
        ],
    )
    .await;
    assert_eq!(row(&only, &durable)["apply"], "unchanged");
    assert_eq!(only["complete"], true, "{only}");
}

fn seed_of(key: &Path) -> [u8; 32] {
    let text = std::fs::read_to_string(key).unwrap();
    let hex = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("seed_hex"))
        .unwrap()
        .trim_start_matches(['=', ' '])
        .trim()
        .trim_matches('"')
        .to_string();
    hex::decode(hex).unwrap().try_into().unwrap()
}
