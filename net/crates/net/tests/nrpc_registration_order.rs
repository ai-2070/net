//! OA2-E0 (Kyra E0 review) — registration publication order across
//! ALL FOUR serving shapes.
//!
//! Every serve shape must have published its token-owned local
//! service registration AND refreshed the self-index BEFORE it
//! exposes the draining bridge, so no inbound event is ever processed
//! before the local registration/discovery state exists. Pre-fix the
//! client-streaming and duplex paths spawned the bridge first and
//! never called `index_self_with_local_services`, so a service could
//! be reachable on the wire while invisible to discovery and the
//! callee gate.
//!
//! By the time each `serve_rpc*` returns, the witness asserts both:
//! the `<service>.requests` dispatcher is registered, AND the
//! self-announcement carries the `nrpc:<service>` tag (the self-index
//! ran). E0 claimed a GENERAL nRPC registration invariant, so this
//! covers unary, response-streaming, client-streaming, and duplex.

#![cfg(all(feature = "net", feature = "cortex"))]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;

use net::adapter::net::cortex::{
    RequestStream, RpcClientStreamingHandler, RpcContext, RpcDuplexHandler, RpcHandler,
    RpcHandlerError, RpcResponsePayload, RpcResponseSink, RpcStatus, RpcStreamingContext,
    RpcStreamingHandler,
};
use net::adapter::net::{ChannelName, EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};

const TEST_BUFFER_SIZE: usize = 256 * 1024;
const PSK: [u8; 32] = [0x42u8; 32];

fn test_config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5))
        .with_handshake(3, Duration::from_secs(2));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: TEST_BUFFER_SIZE,
        recv_buffer_size: TEST_BUFFER_SIZE,
    };
    cfg
}

async fn build_node() -> Arc<MeshNode> {
    Arc::new(
        MeshNode::new(EntityKeypair::generate(), test_config())
            .await
            .expect("MeshNode::new"),
    )
}

// --- trivial handlers, one per serving shape ---------------------------

struct UnaryH;
#[async_trait::async_trait]
impl RpcHandler for UnaryH {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: ctx.payload.body,
        })
    }
}

struct RespStreamH;
#[async_trait::async_trait]
impl RpcStreamingHandler for RespStreamH {
    async fn call(&self, _ctx: RpcContext, sink: RpcResponseSink) -> Result<(), RpcHandlerError> {
        sink.send(b"chunk".to_vec());
        Ok(())
    }
}

struct ClientStreamH;
#[async_trait::async_trait]
impl RpcClientStreamingHandler for ClientStreamH {
    async fn call(
        &self,
        _ctx: RpcStreamingContext,
        mut requests: RequestStream,
    ) -> Result<RpcResponsePayload, RpcHandlerError> {
        while requests.next().await.is_some() {}
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: bytes::Bytes::new(),
        })
    }
}

struct DuplexH;
#[async_trait::async_trait]
impl RpcDuplexHandler for DuplexH {
    async fn call(
        &self,
        _ctx: RpcStreamingContext,
        mut requests: RequestStream,
        responses: RpcResponseSink,
    ) -> Result<(), RpcHandlerError> {
        while let Some(req) = requests.next().await {
            responses.send(req.to_vec());
        }
        Ok(())
    }
}

/// After a serve call returns, both invariants must already hold:
/// the request dispatcher is registered AND the self-index carries
/// `nrpc:<service>`.
fn assert_published(node: &Arc<MeshNode>, service: &str) {
    let req_hash = ChannelName::new(&format!("{service}.requests"))
        .unwrap()
        .hash();
    assert!(
        node.rpc_inbound_dispatcher_registered(req_hash),
        "{service}: request dispatcher must be registered on return",
    );
    assert!(
        node.test_capability_fold_has(node.node_id()),
        "{service}: a self-announcement must exist on return",
    );
    assert!(
        node.test_capability_fold_get(node.node_id())
            .has_tag(&format!("nrpc:{service}")),
        "{service}: self-index must carry nrpc:<service> on return",
    );
}

#[tokio::test]
async fn unary_serve_publishes_and_self_indexes_before_return() {
    let node = build_node().await;
    let _h = node.serve_rpc("u-svc", Arc::new(UnaryH)).expect("serve");
    assert_published(&node, "u-svc");
}

#[tokio::test]
async fn response_streaming_serve_publishes_and_self_indexes_before_return() {
    let node = build_node().await;
    let _h = node
        .serve_rpc_streaming("s-svc", Arc::new(RespStreamH))
        .expect("serve");
    assert_published(&node, "s-svc");
}

/// Pre-fix regression: client-streaming spawned the bridge before
/// inserting the service tag and never self-indexed.
#[tokio::test]
async fn client_streaming_serve_publishes_and_self_indexes_before_return() {
    let node = build_node().await;
    let _h = node
        .serve_rpc_client_stream("cs-svc", Arc::new(ClientStreamH))
        .expect("serve");
    assert_published(&node, "cs-svc");
}

/// Pre-fix regression: duplex spawned the bridge before inserting the
/// service tag and never self-indexed.
#[tokio::test]
async fn duplex_serve_publishes_and_self_indexes_before_return() {
    let node = build_node().await;
    let _h = node
        .serve_rpc_duplex("dx-svc", Arc::new(DuplexH))
        .expect("serve");
    assert_published(&node, "dx-svc");
}
