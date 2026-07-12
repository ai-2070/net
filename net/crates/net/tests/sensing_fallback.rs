//! SI-0 test 10 (SENSING_INTEREST_COALESCING_PLAN §4.11, real
//! path): the old-relay fallback.
//!
//! Selection must be driven by REAL capability-fold state, and the
//! fallback frames must traverse an ACTUAL old-version relay over
//! the real routed dispatch path — not merely exercise the
//! feature-selection function. Topology: A — B — C, where B (the
//! next hop toward provider C) does NOT advertise `net.sensing@1`
//! while C does. A must select the routed end-to-end fallback, and
//! the fallback payload must reach C through B with B never
//! decrypting or dispatching it (the untrusted-relay property the
//! three_node suite pins; re-asserted here for the sensing payload).
//!
//! No sensing subprotocol exists yet (SI-0 is in-process), so the
//! fallback payload rides the same routed encrypted transport the
//! wire frames will use (`send_routed`) — the property under test is
//! the PATH (selection from fold tags + opaque old-relay traversal),
//! not the frame codec (SI-1).
//!
//! Run: `cargo test --features net --test sensing_fallback`

#![cfg(feature = "net")]

use std::net::SocketAddr;
use std::time::Duration;

use net::adapter::net::behavior::capability::{CapabilityAnnouncement, CapabilitySet};
use net::adapter::net::behavior::fold::capability::capability_tags_for;
use net::adapter::net::behavior::sensing::{
    select_sensing_path, SensingPath, SENSING_CAPABILITY_TAG,
};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};
use net::adapter::Adapter;
use net::event::{batch_process_nonce, Batch, InternalEvent};
use tokio::net::UdpSocket;

const PSK: [u8; 32] = [0x42u8; 32];
const TEST_BUFFER_SIZE: usize = 256 * 1024;

async fn find_ports(n: usize) -> Vec<u16> {
    let mut ports = Vec::with_capacity(n);
    let mut sockets = Vec::with_capacity(n);
    for _ in 0..n {
        let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        ports.push(sock.local_addr().unwrap().port());
        sockets.push(sock);
    }
    drop(sockets);
    ports
}

fn mk_config(addr: SocketAddr) -> MeshNodeConfig {
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_num_shards(2)
        .with_handshake(3, Duration::from_secs(3))
        .with_heartbeat_interval(Duration::from_millis(500))
        .with_session_timeout(Duration::from_secs(10));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: TEST_BUFFER_SIZE,
        recv_buffer_size: TEST_BUFFER_SIZE,
    };
    cfg
}

fn sensing_probe_batch(tag: &str) -> Batch {
    let events: Vec<InternalEvent> = (0..4)
        .map(|i| {
            InternalEvent::from_value(serde_json::json!({"tag": tag, "index": i}), i as u64, 0)
        })
        .collect();
    Batch {
        shard_id: 0,
        events,
        sequence_start: 0,
        process_nonce: batch_process_nonce(),
    }
}

/// A—B—C: B is an old relay (no `net.sensing@1`), C a sensing-capable
/// provider. A's fold drives selection to the routed fallback, and
/// the fallback payload traverses B opaquely end-to-end.
#[tokio::test]
async fn old_relay_fallback_selects_and_traverses_the_real_routed_path() {
    let ports = find_ports(3).await;

    let id_a = EntityKeypair::generate();
    let id_b = EntityKeypair::generate();
    let id_c = EntityKeypair::generate();
    let (nid_b, nid_c) = (id_b.node_id(), id_c.node_id());
    let (eid_b, eid_c) = (id_b.entity_id().clone(), id_c.entity_id().clone());
    let nid_a = id_a.node_id();

    let addr_a: SocketAddr = format!("127.0.0.1:{}", ports[0]).parse().unwrap();
    let addr_b: SocketAddr = format!("127.0.0.1:{}", ports[1]).parse().unwrap();
    let addr_c: SocketAddr = format!("127.0.0.1:{}", ports[2]).parse().unwrap();

    let node_a = MeshNode::new(id_a, mk_config(addr_a)).await.unwrap();
    let node_b = MeshNode::new(id_b, mk_config(addr_b)).await.unwrap();
    let node_c = MeshNode::new(id_c, mk_config(addr_c)).await.unwrap();

    let pub_b = *node_b.public_key();
    let pub_c = *node_c.public_key();

    // Sessions: A↔B (the only link A actually uses toward C's
    // route), A↔C (end-to-end keys for the fallback session), B↔C
    // (B's forwarding link).
    let (r1, r2) = tokio::join!(node_b.accept(nid_a), async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        node_a.connect(addr_b, &pub_b, nid_b).await
    });
    r1.expect("B accept A");
    r2.expect("A connect B");
    let (r1, r2) = tokio::join!(node_c.accept(nid_a), async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        node_a.connect(addr_c, &pub_c, nid_c).await
    });
    r1.expect("C accept A");
    r2.expect("A connect C");
    let (r1, r2) = tokio::join!(node_c.accept(nid_b), async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        node_b.connect(addr_c, &pub_c, nid_c).await
    });
    r1.expect("C accept B");
    r2.expect("B connect C");

    // A's route to C goes through B; B forwards direct.
    node_a.router().add_route(nid_c, addr_b);
    node_b.router().add_route(nid_c, addr_c);

    node_a.start();
    node_b.start();
    node_c.start();
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Populate A's capability fold with the mixed-version reality:
    // B (the next hop) is an old build — no sensing tag; C (the
    // provider) advertises net.sensing@1. Injection uses the same
    // path a received announcement takes, so `capability_tags_for`
    // reads exactly what the announce flood would have produced.
    let caps_b = CapabilitySet::new().add_tag("mesh.relay");
    node_a
        .test_inject_capability_announcement(CapabilityAnnouncement::new(nid_b, eid_b, 1, caps_b));
    let caps_c = CapabilitySet::new()
        .add_tag("print.document")
        .add_tag(SENSING_CAPABILITY_TAG);
    node_a
        .test_inject_capability_announcement(CapabilityAnnouncement::new(nid_c, eid_c, 1, caps_c));

    // Selection from REAL fold state: next hop lacks the tag, the
    // provider has it → routed end-to-end fallback (plan §4.11).
    let tags_b = capability_tags_for(node_a.capability_fold(), nid_b);
    let tags_c = capability_tags_for(node_a.capability_fold(), nid_c);
    assert_eq!(
        select_sensing_path(&tags_b, &tags_c),
        SensingPath::DirectFallback,
        "old relay + capable provider must select the routed fallback",
    );
    // Cross-checks on the same fold state: a capable next hop would
    // coalesce; an incapable provider is Unknown territory.
    assert_eq!(
        select_sensing_path(&tags_c, &tags_c),
        SensingPath::Coalesced
    );
    assert_eq!(
        select_sensing_path(&tags_c, &tags_b),
        SensingPath::Unsupported
    );

    // The fallback path itself: A sends the sensing payload
    // end-to-end (encrypted for C), routed THROUGH the old relay B.
    let probe = sensing_probe_batch("sensing_fallback_probe");
    node_a
        .send_routed(nid_c, &probe)
        .await
        .expect("routed fallback send");
    tokio::time::sleep(Duration::from_millis(1500)).await;

    // C received the fallback payload intact…
    let c_result = node_c.poll_shard(0, None, 100).await.unwrap();
    assert!(
        !c_result.events.is_empty(),
        "the fallback payload never reached the provider through the old relay",
    );
    for event in &c_result.events {
        let json: serde_json::Value = event.parse().unwrap();
        assert_eq!(json["tag"], "sensing_fallback_probe");
    }
    // …and the old relay never decrypted or dispatched it: zero
    // silent breakage, zero silent inspection.
    let b_result = node_b.poll_shard(0, None, 100).await.unwrap();
    assert_eq!(
        b_result.events.len(),
        0,
        "the old relay must forward the fallback frames opaquely",
    );

    node_a.shutdown().await.unwrap();
    node_b.shutdown().await.unwrap();
    node_c.shutdown().await.unwrap();
}
