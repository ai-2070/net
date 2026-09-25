//! `net-mesh relay serve` as a real subprocess: a device registers with it from
//! its mesh socket, a joiner binds a channel, and a mesh session runs through
//! it end-to-end. Loopback; the natsim rows cover NAT behavior.
use std::io::{BufRead, BufReader};
use std::net::SocketAddr;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::{
    ChannelName, EntityKeypair, MeshNode, MeshNodeConfig, PeerAddr, SocketBufferConfig,
};

struct Relay(Child);

impl Drop for Relay {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start_relay() -> (Relay, SocketAddr) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_net-mesh"))
        .env_remove("NET_MESH_CONFIG")
        .env_remove("NET_MESH_PROFILE")
        .args([
            "--output",
            "ndjson",
            "relay",
            "serve",
            "--bind",
            "127.0.0.1:0",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let ready: serde_json::Value = serde_json::from_str(&line).expect("ready row");
    assert_eq!(ready["event"], "ready", "{ready}");
    let addr = ready["relay"].as_str().unwrap().parse().unwrap();
    (Relay(child), addr)
}

async fn node() -> Arc<MeshNode> {
    let mut cfg = MeshNodeConfig::new("127.0.0.1:0".parse().unwrap(), [0x33; 32])
        .with_heartbeat_interval(Duration::from_millis(500))
        .with_session_timeout(Duration::from_secs(5))
        .with_handshake(3, Duration::from_secs(3));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: 256 * 1024,
        recv_buffer_size: 256 * 1024,
    };
    Arc::new(MeshNode::new(EntityKeypair::generate(), cfg).await.unwrap())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mesh_session_runs_through_net_mesh_relay_serve() {
    let (_relay, relay_addr) = start_relay();
    let device = node().await;
    let joiner = node().await;
    device.start();
    joiner.start();

    let registration = device.relay_register(relay_addr).await.expect("register");
    let via = joiner
        .relay_bind(relay_addr, registration.id())
        .await
        .expect("bind");
    assert!(matches!(via, PeerAddr::Relayed { .. }));
    joiner
        .connect_via_endpoint(via, device.public_key(), device.node_id())
        .await
        .expect("handshake through the relay");
    tokio::time::timeout(
        Duration::from_secs(5),
        joiner.subscribe_channel(device.node_id(), ChannelName::new("relay.cli").unwrap()),
    )
    .await
    .expect("no hang")
    .expect("request and ack cross the relay");
}

#[test]
fn relay_serve_refuses_a_non_literal_bind() {
    let out = Command::new(env!("CARGO_BIN_EXE_net-mesh"))
        .env_remove("NET_MESH_CONFIG")
        .env_remove("NET_MESH_PROFILE")
        .args(["relay", "serve", "--bind", "relay.example:7000"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{out:?}");
}
