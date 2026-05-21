//! End-to-end integration tests for Net adapter.
//!
//! These tests verify the full Net transport flow including:
//! - Noise handshake (NKpsk0)
//! - Encrypted packet transmission
//! - Event serialization/deserialization
//! - Reliable and unreliable modes
//!
//! Run tests:
//!   cargo test --features net --test integration_net

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::{NetAdapterConfig, PacketFlags, ReliabilityConfig, StaticKeypair};
use net::adapter::Adapter;
use net::event::{batch_process_nonce, Batch, InternalEvent};
use tokio::net::UdpSocket;
use tokio::sync::Barrier;

/// Buffer size for tests (256 KB - small enough to work on any system)
const TEST_BUFFER_SIZE: usize = 256 * 1024;

/// Helper to create a matching initiator/responder pair
fn create_config_pair(
    initiator_port: u16,
    responder_port: u16,
) -> (NetAdapterConfig, NetAdapterConfig) {
    let psk = [0x42u8; 32];
    let responder_keypair = StaticKeypair::generate();

    let initiator_addr: SocketAddr = format!("127.0.0.1:{}", initiator_port).parse().unwrap();
    let responder_addr: SocketAddr = format!("127.0.0.1:{}", responder_port).parse().unwrap();

    let initiator_config = NetAdapterConfig::initiator(
        initiator_addr,
        responder_addr,
        psk,
        responder_keypair.public,
    )
    .with_handshake(3, Duration::from_secs(2))
    .with_heartbeat_interval(Duration::from_millis(500))
    .with_session_timeout(Duration::from_secs(5))
    .with_socket_buffers(TEST_BUFFER_SIZE, TEST_BUFFER_SIZE);

    let responder_config =
        NetAdapterConfig::responder(responder_addr, initiator_addr, psk, responder_keypair)
            .with_handshake(3, Duration::from_secs(2))
            .with_heartbeat_interval(Duration::from_millis(500))
            .with_session_timeout(Duration::from_secs(5))
            .with_socket_buffers(TEST_BUFFER_SIZE, TEST_BUFFER_SIZE);

    (initiator_config, responder_config)
}

/// Find available ports for testing
async fn find_available_ports() -> (u16, u16) {
    let sock1 = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let sock2 = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let port1 = sock1.local_addr().unwrap().port();
    let port2 = sock2.local_addr().unwrap().port();
    drop(sock1);
    drop(sock2);
    // Small delay to ensure ports are released
    tokio::time::sleep(Duration::from_millis(10)).await;
    (port1, port2)
}

#[tokio::test]
async fn test_net_handshake() {
    let (port1, port2) = find_available_ports().await;
    let (initiator_config, responder_config) = create_config_pair(port1, port2);

    let barrier = Arc::new(Barrier::new(2));

    // Spawn responder
    let responder_barrier = barrier.clone();
    let responder_handle = tokio::spawn(async move {
        let mut adapter = net::adapter::net::NetAdapter::new(responder_config).unwrap();
        responder_barrier.wait().await;
        adapter.init().await
    });

    // Spawn initiator
    let initiator_barrier = barrier.clone();
    let initiator_handle = tokio::spawn(async move {
        let mut adapter = net::adapter::net::NetAdapter::new(initiator_config).unwrap();
        initiator_barrier.wait().await;
        adapter.init().await
    });

    // Wait for both to complete with timeout
    let timeout = Duration::from_secs(10);
    let results = tokio::time::timeout(
        timeout,
        futures::future::join(responder_handle, initiator_handle),
    )
    .await
    .expect("handshake timed out");

    // Check results
    let (responder_result, initiator_result) = results;
    responder_result
        .expect("responder task panicked")
        .expect("responder init failed");
    initiator_result
        .expect("initiator task panicked")
        .expect("initiator init failed");
}

#[tokio::test]
async fn test_net_send_receive_fire_and_forget() {
    let (port1, port2) = find_available_ports().await;
    let (initiator_config, responder_config) = create_config_pair(port1, port2);

    let barrier = Arc::new(Barrier::new(2));

    // Spawn responder that will receive events
    let responder_barrier = barrier.clone();
    let responder_handle = tokio::spawn(async move {
        let mut adapter = net::adapter::net::NetAdapter::new(responder_config).unwrap();
        responder_barrier.wait().await;
        adapter.init().await.expect("responder init failed");

        // Wait for events to arrive
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Poll for events
        let result = adapter.poll_shard(0, None, 100).await.expect("poll failed");
        adapter.shutdown().await.expect("shutdown failed");
        result.events.len()
    });

    // Spawn initiator that will send events
    let initiator_barrier = barrier.clone();
    let initiator_handle = tokio::spawn(async move {
        let mut adapter = net::adapter::net::NetAdapter::new(initiator_config).unwrap();
        initiator_barrier.wait().await;
        adapter.init().await.expect("initiator init failed");

        // Small delay to ensure connection is ready
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Send batch of events
        let events: Vec<InternalEvent> = (0..10)
            .map(|i| {
                let json = serde_json::json!({"index": i, "data": "test"});
                InternalEvent::from_value(json, i as u64, 0)
            })
            .collect();

        let batch = Batch {
            shard_id: 0,
            events,
            sequence_start: 0,
            process_nonce: batch_process_nonce(),
        };

        adapter
            .on_batch(std::sync::Arc::new(batch))
            .await
            .expect("send failed");

        // Give time for events to arrive
        tokio::time::sleep(Duration::from_millis(300)).await;

        adapter.shutdown().await.expect("shutdown failed");
    });

    // Wait for both
    let timeout = Duration::from_secs(10);
    let results = tokio::time::timeout(
        timeout,
        futures::future::join(responder_handle, initiator_handle),
    )
    .await
    .expect("test timed out");

    let (responder_result, initiator_result) = results;
    initiator_result.expect("initiator task panicked");
    let received_count = responder_result.expect("responder task panicked");

    // In fire-and-forget mode, we may not receive all events due to timing
    // but we should receive at least some
    assert!(
        received_count > 0,
        "expected to receive some events, got {}",
        received_count
    );
}

#[tokio::test]
async fn test_net_reliable_mode() {
    let (port1, port2) = find_available_ports().await;
    let (mut initiator_config, mut responder_config) = create_config_pair(port1, port2);

    // Enable reliable mode
    initiator_config.default_reliability = ReliabilityConfig::Light;
    responder_config.default_reliability = ReliabilityConfig::Light;

    let barrier = Arc::new(Barrier::new(2));

    // Spawn responder
    let responder_barrier = barrier.clone();
    let responder_handle = tokio::spawn(async move {
        let mut adapter = net::adapter::net::NetAdapter::new(responder_config).unwrap();
        responder_barrier.wait().await;
        adapter.init().await.expect("responder init failed");

        // Wait for events
        tokio::time::sleep(Duration::from_millis(500)).await;

        let result = adapter.poll_shard(0, None, 100).await.expect("poll failed");
        adapter.shutdown().await.expect("shutdown failed");
        result.events.len()
    });

    // Spawn initiator
    let initiator_barrier = barrier.clone();
    let initiator_handle = tokio::spawn(async move {
        let mut adapter = net::adapter::net::NetAdapter::new(initiator_config).unwrap();
        initiator_barrier.wait().await;
        adapter.init().await.expect("initiator init failed");

        tokio::time::sleep(Duration::from_millis(100)).await;

        // Send events
        let events: Vec<InternalEvent> = (0..5)
            .map(|i| {
                let json = serde_json::json!({"reliable_index": i});
                InternalEvent::from_value(json, i as u64, 0)
            })
            .collect();

        let batch = Batch {
            shard_id: 0,
            events,
            sequence_start: 0,
            process_nonce: batch_process_nonce(),
        };

        adapter
            .on_batch(std::sync::Arc::new(batch))
            .await
            .expect("send failed");
        tokio::time::sleep(Duration::from_millis(300)).await;
        adapter.shutdown().await.expect("shutdown failed");
    });

    let timeout = Duration::from_secs(10);
    let results = tokio::time::timeout(
        timeout,
        futures::future::join(responder_handle, initiator_handle),
    )
    .await
    .expect("test timed out");

    let (responder_result, initiator_result) = results;
    initiator_result.expect("initiator task panicked");
    let received_count = responder_result.expect("responder task panicked");

    assert!(
        received_count > 0,
        "expected to receive events in reliable mode"
    );
}

#[tokio::test]
async fn test_net_multiple_streams() {
    let (port1, port2) = find_available_ports().await;
    let (initiator_config, responder_config) = create_config_pair(port1, port2);

    let barrier = Arc::new(Barrier::new(2));

    // Spawn responder
    let responder_barrier = barrier.clone();
    let responder_handle = tokio::spawn(async move {
        let mut adapter = net::adapter::net::NetAdapter::new(responder_config).unwrap();
        responder_barrier.wait().await;
        adapter.init().await.expect("responder init failed");

        tokio::time::sleep(Duration::from_millis(500)).await;

        // Poll from multiple shards/streams
        let shard0 = adapter
            .poll_shard(0, None, 100)
            .await
            .expect("poll shard 0 failed");
        let shard1 = adapter
            .poll_shard(1, None, 100)
            .await
            .expect("poll shard 1 failed");

        adapter.shutdown().await.expect("shutdown failed");
        (shard0.events.len(), shard1.events.len())
    });

    // Spawn initiator
    let initiator_barrier = barrier.clone();
    let initiator_handle = tokio::spawn(async move {
        let mut adapter = net::adapter::net::NetAdapter::new(initiator_config).unwrap();
        initiator_barrier.wait().await;
        adapter.init().await.expect("initiator init failed");

        tokio::time::sleep(Duration::from_millis(100)).await;

        // Send to shard 0
        let events0: Vec<InternalEvent> = (0..5)
            .map(|i| {
                InternalEvent::from_value(serde_json::json!({"shard": 0, "i": i}), i as u64, 0)
            })
            .collect();
        adapter
            .on_batch(std::sync::Arc::new(Batch {
                shard_id: 0,
                events: events0,
                sequence_start: 0,
                process_nonce: batch_process_nonce(),
            }))
            .await
            .expect("send to shard 0 failed");

        // Send to shard 1
        let events1: Vec<InternalEvent> = (0..3)
            .map(|i| {
                InternalEvent::from_value(serde_json::json!({"shard": 1, "i": i}), i as u64, 1)
            })
            .collect();
        adapter
            .on_batch(std::sync::Arc::new(Batch {
                shard_id: 1,
                events: events1,
                sequence_start: 0,
                process_nonce: batch_process_nonce(),
            }))
            .await
            .expect("send to shard 1 failed");

        tokio::time::sleep(Duration::from_millis(300)).await;
        adapter.shutdown().await.expect("shutdown failed");
    });

    let timeout = Duration::from_secs(10);
    let results = tokio::time::timeout(
        timeout,
        futures::future::join(responder_handle, initiator_handle),
    )
    .await
    .expect("test timed out");

    let (responder_result, initiator_result) = results;
    initiator_result.expect("initiator task panicked");
    let (count0, count1) = responder_result.expect("responder task panicked");

    // Should receive events on both streams
    assert!(
        count0 > 0 || count1 > 0,
        "expected events on at least one stream"
    );
}

#[tokio::test]
async fn test_net_health_check() {
    let (port1, port2) = find_available_ports().await;
    let (initiator_config, responder_config) = create_config_pair(port1, port2);

    let barrier = Arc::new(Barrier::new(2));

    // Spawn responder
    let responder_barrier = barrier.clone();
    let responder_handle = tokio::spawn(async move {
        let mut adapter = net::adapter::net::NetAdapter::new(responder_config).unwrap();

        // Not healthy before init
        assert!(
            !adapter.is_healthy().await,
            "should not be healthy before init"
        );

        responder_barrier.wait().await;
        adapter.init().await.expect("init failed");

        // Healthy after init
        assert!(adapter.is_healthy().await, "should be healthy after init");

        // Keep alive for initiator
        tokio::time::sleep(Duration::from_millis(500)).await;

        adapter.shutdown().await.expect("shutdown failed");

        // Not healthy after shutdown
        assert!(
            !adapter.is_healthy().await,
            "should not be healthy after shutdown"
        );
    });

    // Spawn initiator
    let initiator_barrier = barrier.clone();
    let initiator_handle = tokio::spawn(async move {
        let mut adapter = net::adapter::net::NetAdapter::new(initiator_config).unwrap();
        initiator_barrier.wait().await;
        adapter.init().await.expect("init failed");

        assert!(adapter.is_healthy().await, "initiator should be healthy");

        tokio::time::sleep(Duration::from_millis(400)).await;
        adapter.shutdown().await.expect("shutdown failed");
    });

    let timeout = Duration::from_secs(10);
    let (r1, r2) = tokio::time::timeout(
        timeout,
        futures::future::join(responder_handle, initiator_handle),
    )
    .await
    .expect("test timed out");
    r1.expect("responder task failed");
    r2.expect("initiator task failed");
}

#[tokio::test]
async fn test_net_large_batch() {
    let (port1, port2) = find_available_ports().await;
    let (initiator_config, responder_config) = create_config_pair(port1, port2);

    let barrier = Arc::new(Barrier::new(2));

    // Spawn responder
    let responder_barrier = barrier.clone();
    let responder_handle = tokio::spawn(async move {
        let mut adapter = net::adapter::net::NetAdapter::new(responder_config).unwrap();
        responder_barrier.wait().await;
        adapter.init().await.expect("responder init failed");

        // Wait longer for large batch
        tokio::time::sleep(Duration::from_millis(1000)).await;

        let result = adapter
            .poll_shard(0, None, 1000)
            .await
            .expect("poll failed");
        adapter.shutdown().await.expect("shutdown failed");
        result.events.len()
    });

    // Spawn initiator sending large batch
    let initiator_barrier = barrier.clone();
    let initiator_handle = tokio::spawn(async move {
        let mut adapter = net::adapter::net::NetAdapter::new(initiator_config).unwrap();
        initiator_barrier.wait().await;
        adapter.init().await.expect("initiator init failed");

        tokio::time::sleep(Duration::from_millis(100)).await;

        // Send 100 events (should span multiple packets)
        let events: Vec<InternalEvent> = (0..100)
            .map(|i| {
                InternalEvent::from_value(
                    serde_json::json!({
                        "index": i,
                        "data": "some payload data to make the event larger"
                    }),
                    i as u64,
                    0,
                )
            })
            .collect();

        let batch = Batch {
            shard_id: 0,
            events,
            sequence_start: 0,
            process_nonce: batch_process_nonce(),
        };

        adapter
            .on_batch(std::sync::Arc::new(batch))
            .await
            .expect("send failed");
        tokio::time::sleep(Duration::from_millis(500)).await;
        adapter.shutdown().await.expect("shutdown failed");
    });

    let timeout = Duration::from_secs(15);
    let results = tokio::time::timeout(
        timeout,
        futures::future::join(responder_handle, initiator_handle),
    )
    .await
    .expect("test timed out");

    let (responder_result, initiator_result) = results;
    initiator_result.expect("initiator task panicked");
    let received_count = responder_result.expect("responder task panicked");

    // Should receive a significant portion of the events
    assert!(
        received_count >= 10,
        "expected at least 10 events from large batch, got {}",
        received_count
    );
}

#[tokio::test]
async fn test_net_adapter_name() {
    let psk = [0x42u8; 32];
    let peer_pubkey = [0x24u8; 32];

    let config = NetAdapterConfig::initiator(
        "127.0.0.1:0".parse().unwrap(),
        "127.0.0.1:9999".parse().unwrap(),
        psk,
        peer_pubkey,
    );

    let adapter = net::adapter::net::NetAdapter::new(config).unwrap();
    assert_eq!(adapter.name(), "net");
}

#[tokio::test]
async fn test_net_flush() {
    // This test verifies that flush() doesn't error on an initialized adapter
    let (port1, port2) = find_available_ports().await;
    let (initiator_config, responder_config) = create_config_pair(port1, port2);

    // Use barriers to synchronize: one for starting handshake, one for shutdown
    let start_barrier = Arc::new(Barrier::new(2));
    let shutdown_barrier = Arc::new(Barrier::new(2));

    // Spawn responder
    let responder_start = start_barrier.clone();
    let responder_shutdown = shutdown_barrier.clone();
    let responder_handle = tokio::spawn(async move {
        let mut adapter = net::adapter::net::NetAdapter::new(responder_config).unwrap();
        responder_start.wait().await;
        adapter.init().await.expect("responder init failed");

        // Keep connection alive while initiator does its work
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Wait for initiator to finish its work before shutting down
        responder_shutdown.wait().await;
        adapter.shutdown().await.expect("responder shutdown failed");
    });

    // Spawn initiator
    let initiator_start = start_barrier.clone();
    let initiator_shutdown = shutdown_barrier.clone();
    let initiator_handle = tokio::spawn(async move {
        let mut adapter = net::adapter::net::NetAdapter::new(initiator_config).unwrap();
        initiator_start.wait().await;
        adapter.init().await.expect("initiator init failed");

        // Small delay to ensure connection is stable
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Flush should not error
        adapter.flush().await.expect("flush should not fail");

        // Signal ready to shutdown, then shutdown
        initiator_shutdown.wait().await;
        adapter.shutdown().await.expect("initiator shutdown failed");
    });

    let timeout = Duration::from_secs(10);
    let results = tokio::time::timeout(
        timeout,
        futures::future::join(responder_handle, initiator_handle),
    )
    .await
    .expect("test timed out");

    results.0.expect("responder task panicked");
    results.1.expect("initiator task panicked");
}

/// Test data transfer in the reverse direction: responder → initiator.
/// The existing tests only verify initiator → responder. This catches
/// key derivation bugs where the tx/rx key swap is incorrect.
#[tokio::test]
async fn test_net_responder_to_initiator() {
    let (port1, port2) = find_available_ports().await;
    let (initiator_config, responder_config) = create_config_pair(port1, port2);

    let barrier = Arc::new(Barrier::new(2));

    // Responder: sends events
    let responder_barrier = barrier.clone();
    let responder_handle = tokio::spawn(async move {
        let mut adapter = net::adapter::net::NetAdapter::new(responder_config).unwrap();
        responder_barrier.wait().await;
        adapter.init().await.expect("responder init failed");

        tokio::time::sleep(Duration::from_millis(100)).await;

        // Send a batch (responder → initiator)
        let events: Vec<InternalEvent> = (0..5)
            .map(|i| {
                let json = serde_json::json!({"from": "responder", "i": i});
                InternalEvent::from_value(json, i as u64, 0)
            })
            .collect();
        adapter
            .on_batch(std::sync::Arc::new(Batch {
                shard_id: 0,
                events,
                sequence_start: 0,
                process_nonce: batch_process_nonce(),
            }))
            .await
            .expect("responder send failed");

        tokio::time::sleep(Duration::from_millis(300)).await;
        adapter.shutdown().await.expect("shutdown failed");
    });

    // Initiator: receives events
    let initiator_barrier = barrier.clone();
    let initiator_handle = tokio::spawn(async move {
        let mut adapter = net::adapter::net::NetAdapter::new(initiator_config).unwrap();
        initiator_barrier.wait().await;
        adapter.init().await.expect("initiator init failed");

        // Wait for responder's events to arrive
        tokio::time::sleep(Duration::from_millis(500)).await;

        let result = adapter.poll_shard(0, None, 100).await.expect("poll failed");
        adapter.shutdown().await.expect("shutdown failed");
        result.events.len()
    });

    let timeout = Duration::from_secs(10);
    let results = tokio::time::timeout(
        timeout,
        futures::future::join(responder_handle, initiator_handle),
    )
    .await
    .expect("test timed out");

    let (resp_result, init_result) = results;
    resp_result.expect("responder panicked");
    let init_received = init_result.expect("initiator panicked");

    assert!(
        init_received > 0,
        "initiator should receive responder's events, got {}",
        init_received
    );
}

// Unit tests for low-level components
mod unit {
    use super::*;
    use bytes::{Bytes, BytesMut};
    use net::adapter::net::{EventFrame, NetHeader, PacketPool, HEADER_SIZE, NONCE_SIZE};

    #[test]
    fn test_event_frame_serialization() {
        let data = Bytes::from_static(b"test event data");
        let events = vec![data.clone()];
        let mut buf = BytesMut::new();
        let written = EventFrame::write_events(&events, &mut buf);

        // Should be length prefix (4 bytes) + data
        assert_eq!(written, 4 + data.len());

        // Verify we can read it back
        let read_events = EventFrame::read_events(buf.freeze(), 1);
        assert_eq!(read_events.len(), 1);
        assert_eq!(read_events[0], data);
    }

    #[test]
    fn test_packet_header_roundtrip() {
        let nonce = [0x42u8; NONCE_SIZE];
        let header = NetHeader::new(0x1234, 0x5678, 42, nonce, 100, 5, PacketFlags::RELIABLE);

        let bytes = header.to_bytes();
        assert_eq!(bytes.len(), HEADER_SIZE);

        let parsed = NetHeader::from_bytes(&bytes).unwrap();
        assert_eq!(parsed.session_id, 0x1234);
        assert_eq!(parsed.stream_id, 0x5678);
        assert_eq!(parsed.sequence, 42);
        assert_eq!(parsed.nonce, nonce);
        assert_eq!(parsed.payload_len, 100);
        assert_eq!(parsed.event_count, 5);
        assert!(parsed.flags.is_reliable());
    }

    #[test]
    fn test_packet_pool_allocation() {
        let key = [0u8; 32];
        let pool = PacketPool::new(4, &key, 0x1234);

        // Should be able to get 4 builders
        let mut builders = Vec::new();
        for _ in 0..4 {
            builders.push(pool.get());
        }

        // Pool should now allocate new ones
        let extra = pool.get();
        drop(extra);

        // Return builders
        drop(builders);

        // Should be able to get them again
        let _b = pool.get();
    }

    #[test]
    fn test_session_keys_generation() {
        let keypair = StaticKeypair::generate();

        // Public key should be 32 bytes
        assert_eq!(keypair.public.len(), 32);

        // Private key should be 32 bytes
        assert_eq!(keypair.private.len(), 32);

        // Keys should not be all zeros
        assert!(keypair.public.iter().any(|&b| b != 0));
        assert!(keypair.private.iter().any(|&b| b != 0));
    }
}
