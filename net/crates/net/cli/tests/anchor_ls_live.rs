//! R6 — `net-mesh anchor ls` against a LIVE mesh.
//!
//! The CLI's in-process Deck client is built with `mesh: None`, so
//! `rtc_anchors()` on it was structurally empty: the listing could
//! never show an anchor no matter how many announced. This boots a
//! real anchor (RTC configured, `rtc-anchor` announced) plus a plain
//! peer, then drives the real binary against the anchor and asserts
//! the anchor is listed with its addresses and the plain peer is not.
//!
//! Run: `cargo test -p net-cli --features rtc-bootstrap --test anchor_ls_live`
//! (the file is `#![cfg(feature = "rtc-bootstrap")]`, which is where
//! `anchor ls` itself lives; `--features webrtc` compiles nothing here.)
#![cfg(feature = "rtc-bootstrap")]

use std::sync::Arc;
use std::time::Duration;

use assert_cmd::Command as AssertCommand;
use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::rtc::RtcConfig;
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig};
use net_sdk::rtc_bootstrap::serve_anchor_directory;

const PSK: [u8; 32] = [0x42u8; 32];
const PSK_HEX: &str = "4242424242424242424242424242424242424242424242424242424242424242";

async fn node(rtc: Option<RtcConfig>) -> Arc<MeshNode> {
    let mut cfg = MeshNodeConfig::new("127.0.0.1:0".parse().expect("addr"), PSK)
        .with_heartbeat_interval(Duration::from_millis(100));
    cfg.rtc = rtc;
    // NOT started here: a direct handshake to an already-started
    // node is dropped (mesh.rs documents the missing responder-side
    // registry), so every pair is connected first and started after.
    Arc::new(
        MeshNode::new(EntityKeypair::generate(), cfg)
            .await
            .expect("MeshNode::new"),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn anchor_ls_lists_a_live_announcing_anchor_and_not_a_plain_peer() {
    // The daemon the CLI attaches to. It is the one that has to have
    // INGESTED the announcements — and, since the two address fields
    // are locally-filled projections that never travel, the one that
    // has to ANSWER for them. It serves the anchor directory.
    let daemon_mesh = net_sdk::Mesh::builder("127.0.0.1:0", &PSK)
        .expect("builder")
        .build()
        .await
        .expect("daemon mesh");
    let daemon = Arc::clone(daemon_mesh.node());
    let public: std::net::SocketAddr = "203.0.113.9:7443".parse().expect("addr");
    let anchor = node(Some(RtcConfig {
        public_addr: Some(public),
        ..RtcConfig::new()
            .with_bind_addr("127.0.0.1:0".parse().expect("addr"))
            .with_bootstrap_url("https://anchor.example.com")
    }))
    .await;
    let plain = node(None).await;

    for peer in [&anchor, &plain] {
        let daemon_id = daemon.node_id();
        let peer_clone = Arc::clone(peer);
        let accept = tokio::spawn(async move { peer_clone.accept(daemon_id).await });
        daemon
            .connect(peer.local_addr(), peer.public_key(), peer.node_id())
            .await
            .expect("udp handshake");
        accept.await.expect("accept task").expect("accept");
    }
    for n in [&daemon, &anchor, &plain] {
        n.start_arc();
    }
    let _directory = serve_anchor_directory(&daemon_mesh).expect("serve the anchor directory");
    for peer in [&anchor, &plain] {
        peer.announce_capabilities(CapabilitySet::new())
            .await
            .expect("announce");
    }

    // Wait for the daemon to have ingested the anchor's own signed
    // announcement — the CLI reads what the daemon knows.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline
        && !daemon
            .rtc_anchors()
            .iter()
            .any(|row| row.node_id == anchor.node_id())
    {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let out = AssertCommand::cargo_bin("net-mesh")
        .expect("binary")
        .args([
            "--output",
            "json",
            "anchor",
            "ls",
            "--node-addr",
            &daemon.local_addr().to_string(),
            "--node-pubkey",
            &hex::encode(daemon.public_key()),
            "--node-id",
            &format!("{:#x}", daemon.node_id()),
            "--psk-hex",
            PSK_HEX,
        ])
        .timeout(Duration::from_secs(60))
        .output()
        .expect("run anchor ls");
    assert!(
        out.status.success(),
        "anchor ls failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let rows: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout was not JSON ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    });
    let rows = rows.as_array().expect("an array of rows");
    let anchor_hex = format!("{:#x}", anchor.node_id());
    let listed = rows
        .iter()
        .find(|row| row["node"] == serde_json::Value::String(anchor_hex.clone()))
        .unwrap_or_else(|| panic!("the announcing anchor is missing from {rows:?}"));
    assert_eq!(listed["rtc_addr"], public.to_string());
    assert_eq!(listed["rtc_bootstrap"], "https://anchor.example.com");

    let plain_hex = format!("{:#x}", plain.node_id());
    assert!(
        !rows
            .iter()
            .any(|row| row["node"] == serde_json::Value::String(plain_hex.clone())),
        "a peer that never claimed the anchor role must not be listed"
    );
}

/// Without a daemon to attach to, the listing REFUSES rather than
/// printing an empty array — an empty list from a client with no
/// mesh is the exact failure this repair is about.
#[test]
fn anchor_ls_without_a_daemon_refuses_rather_than_printing_nothing() {
    let out = AssertCommand::cargo_bin("net-mesh")
        .expect("binary")
        .args(["--output", "json", "anchor", "ls"])
        .timeout(Duration::from_secs(30))
        .output()
        .expect("run anchor ls");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr).to_lowercase();
    assert!(
        stderr.contains("daemon") || stderr.contains("live mesh"),
        "the refusal must say why: {stderr}"
    );
}
