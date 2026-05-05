//! Phase 2 nRPC service discovery — end-to-end test.
//!
//! Three nodes:
//!   - server_a: serves "echo".
//!   - server_b: serves "echo" too (a second replica).
//!   - caller: doesn't know about either; uses
//!     `Mesh::call_service("echo", ...)`.
//!
//! The discovery flow:
//!   1. Each server calls `serve_rpc("echo", handler)` — that
//!      registers the local-services entry + inbound dispatcher.
//!   2. Each server calls `announce_capabilities(...)` — the
//!      mesh auto-merges `nrpc:echo` into the announced tags;
//!      the announcement broadcasts to direct peers.
//!   3. The caller's local capability index picks up both
//!      announcements; `find_service_nodes("echo")` returns
//!      [server_a, server_b].
//!   4. `call_service("echo", payload, opts)` picks one (naive
//!      round-robin via call_id) and invokes `call(target, ...)`.
//!
//! Asserts: the call succeeds, returns the echoed body. Across
//! many calls, both servers are exercised at least once
//! (round-robin distribution).

#![cfg(all(feature = "net", feature = "cortex"))]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::cortex::{
    RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, RpcStatus,
};
use net::adapter::net::mesh_rpc::{CallOptions, RoutingPolicy};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};

const TEST_BUFFER_SIZE: usize = 256 * 1024;
const PSK: [u8; 32] = [0x42u8; 32];

fn test_config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5))
        .with_handshake(3, Duration::from_secs(2))
        .with_capability_gc_interval(Duration::from_millis(250))
        // Service-discovery test: re-announcements happen
        // immediately so the caller's index sees both servers
        // without waiting for the rate-limit window.
        .with_min_announce_interval(Duration::from_millis(0));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: TEST_BUFFER_SIZE,
        recv_buffer_size: TEST_BUFFER_SIZE,
    };
    cfg
}

async fn build_node() -> Arc<MeshNode> {
    let cfg = test_config();
    let keypair = EntityKeypair::generate();
    Arc::new(MeshNode::new(keypair, cfg).await.expect("MeshNode::new"))
}

async fn handshake_pair(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
    let a_id = a.node_id();
    let b_id = b.node_id();
    let b_pub = *b.public_key();
    let b_addr = b.local_addr();
    let b_clone = b.clone();
    let accept = tokio::spawn(async move { b_clone.accept(a_id).await });
    a.connect(b_addr, &b_pub, b_id)
        .await
        .expect("connect failed");
    accept
        .await
        .expect("accept task panicked")
        .expect("accept failed");
    a.start();
    b.start();
}

async fn wait_until<F: FnMut() -> bool>(mut cond: F, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    cond()
}

/// Counts how many invocations each server's handler got. Used
/// to confirm round-robin distribution actually visits both.
struct CountingEcho {
    count: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl RpcHandler for CountingEcho {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        self.count.fetch_add(1, Ordering::Relaxed);
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: ctx.payload.body,
        })
    }
}

#[tokio::test]
async fn call_service_discovers_servers_via_capability_announcements() {
    let server_a = build_node().await;
    let server_b = build_node().await;
    let caller = build_node().await;

    // Caller handshakes with both servers (so the caller can
    // direct-send REQUESTs once it discovers them).
    handshake_pair(&caller, &server_a).await;
    handshake_pair(&caller, &server_b).await;

    // Both servers register the "echo" service.
    let count_a = Arc::new(AtomicUsize::new(0));
    let count_b = Arc::new(AtomicUsize::new(0));
    let _serve_a = server_a
        .serve_rpc(
            "echo",
            Arc::new(CountingEcho {
                count: count_a.clone(),
            }),
        )
        .expect("serve_rpc A");
    let _serve_b = server_b
        .serve_rpc(
            "echo",
            Arc::new(CountingEcho {
                count: count_b.clone(),
            }),
        )
        .expect("serve_rpc B");

    // Both servers announce capabilities — the auto-merged
    // `nrpc:echo` tag broadcasts to the caller.
    server_a
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("announce A");
    server_b
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("announce B");

    // Wait for the caller's capability index to learn about both
    // servers. Capability propagation is best-effort + async so
    // we poll until both nodes show up.
    assert!(
        wait_until(
            || {
                let nodes = caller.find_service_nodes("echo");
                nodes.contains(&server_a.node_id()) && nodes.contains(&server_b.node_id())
            },
            Duration::from_secs(5),
        )
        .await,
        "caller's capability index must discover both servers; \
         currently sees {:?}",
        caller.find_service_nodes("echo"),
    );

    // Issue a batch of calls via call_service — the caller
    // doesn't know about target node IDs.
    let n: usize = 20;
    for i in 0..n {
        let body = format!("call-{i}").into_bytes();
        let reply = caller
            .call_service("echo", Bytes::from(body.clone()), CallOptions::default())
            .await
            .expect("call_service must succeed");
        assert_eq!(reply.body.as_ref(), body.as_slice());
    }

    // Both servers should have been visited by the round-robin
    // selector. Naive `call_id % len` rotates each call so with
    // n=20 and 2 candidates we expect ~10/10.
    let a = count_a.load(Ordering::Relaxed);
    let b = count_b.load(Ordering::Relaxed);
    assert_eq!(
        a + b,
        n,
        "every call must hit exactly one server (no double-dispatch)",
    );
    assert!(
        a > 0 && b > 0,
        "round-robin must visit both servers across {n} calls; got A={a}, B={b}",
    );
}

/// `RoutingPolicy::Sticky` consistently routes the same `key` to
/// the same server across calls. Pin the contract: 20 calls with
/// the same key all hit one server (count == 20 on one, 0 on the
/// other); a different key may hit either, but a given key is
/// stable.
#[tokio::test]
async fn sticky_routing_pins_a_key_to_one_server() {
    let server_a = build_node().await;
    let server_b = build_node().await;
    let caller = build_node().await;
    handshake_pair(&caller, &server_a).await;
    handshake_pair(&caller, &server_b).await;

    let count_a = Arc::new(AtomicUsize::new(0));
    let count_b = Arc::new(AtomicUsize::new(0));
    let _serve_a = server_a
        .serve_rpc(
            "echo",
            Arc::new(CountingEcho {
                count: count_a.clone(),
            }),
        )
        .expect("serve_rpc A");
    let _serve_b = server_b
        .serve_rpc(
            "echo",
            Arc::new(CountingEcho {
                count: count_b.clone(),
            }),
        )
        .expect("serve_rpc B");
    server_a
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("announce A");
    server_b
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("announce B");
    assert!(
        wait_until(
            || caller.find_service_nodes("echo").len() == 2,
            Duration::from_secs(5),
        )
        .await,
    );

    // 20 calls with key=42. All must land on the same server.
    let opts = CallOptions {
        routing_policy: RoutingPolicy::Sticky { key: 42 },
        ..Default::default()
    };
    for _ in 0..20 {
        caller
            .call_service("echo", Bytes::from_static(b"sticky"), opts.clone())
            .await
            .expect("call_service");
    }
    let a = count_a.load(Ordering::Relaxed);
    let b = count_b.load(Ordering::Relaxed);
    assert_eq!(a + b, 20, "every call must land on exactly one server");
    assert!(
        (a == 20 && b == 0) || (a == 0 && b == 20),
        "Sticky routing must pin all calls with the same key to one server; got A={a}, B={b}",
    );
}

/// `RoutingPolicy::Random` distributes calls across both servers.
/// We don't assert exact 50/50 (XX% variance is normal for small
/// N), just that both servers are hit.
#[tokio::test]
async fn random_routing_distributes_across_servers() {
    let server_a = build_node().await;
    let server_b = build_node().await;
    let caller = build_node().await;
    handshake_pair(&caller, &server_a).await;
    handshake_pair(&caller, &server_b).await;

    let count_a = Arc::new(AtomicUsize::new(0));
    let count_b = Arc::new(AtomicUsize::new(0));
    let _serve_a = server_a
        .serve_rpc(
            "echo",
            Arc::new(CountingEcho {
                count: count_a.clone(),
            }),
        )
        .unwrap();
    let _serve_b = server_b
        .serve_rpc(
            "echo",
            Arc::new(CountingEcho {
                count: count_b.clone(),
            }),
        )
        .unwrap();
    server_a
        .announce_capabilities(CapabilitySet::new())
        .await
        .unwrap();
    server_b
        .announce_capabilities(CapabilitySet::new())
        .await
        .unwrap();
    assert!(
        wait_until(
            || caller.find_service_nodes("echo").len() == 2,
            Duration::from_secs(5),
        )
        .await,
    );

    let opts = CallOptions {
        routing_policy: RoutingPolicy::Random,
        ..Default::default()
    };
    for _ in 0..40 {
        caller
            .call_service("echo", Bytes::from_static(b"r"), opts.clone())
            .await
            .expect("call_service");
    }
    let a = count_a.load(Ordering::Relaxed);
    let b = count_b.load(Ordering::Relaxed);
    assert_eq!(a + b, 40, "every call must land on exactly one server");
    assert!(
        a > 0 && b > 0,
        "Random must visit both servers across 40 calls; got A={a}, B={b}",
    );
}

/// `call_service` returns `RpcError::NoRoute` when no server has
/// announced the requested service.
#[tokio::test]
async fn call_service_with_no_servers_returns_no_route() {
    let caller = build_node().await;
    let other = build_node().await;
    handshake_pair(&caller, &other).await;
    // `other` has not registered any service.

    let err = caller
        .call_service(
            "nonexistent",
            Bytes::from_static(b"x"),
            CallOptions::default(),
        )
        .await
        .expect_err("call_service for unknown service must fail");
    match err {
        net::adapter::net::mesh_rpc::RpcError::NoRoute { reason, .. } => {
            assert!(
                reason.contains("nrpc:nonexistent"),
                "diagnostic should name the missing tag, got {reason:?}",
            );
        }
        other => panic!("expected NoRoute, got {other:?}"),
    }
}
