//! `serve_rpc`'s auto re-announce must never revert a concurrent explicit
//! announce.
//!
//! `serve_rpc` spawns a re-announce so peers learn about the new service
//! without the operator calling `announce_capabilities` by hand. That task used
//! to snapshot the capability baseline itself and hand the snapshot back as a
//! NEW baseline. Because the task is spawned, the snapshot could be taken
//! before an operator's `announce_capabilities(X)` and written back after it —
//! reverting `X` on the wire AND in this node's own capability fold, where it
//! stayed until something else announced. A tool published, withdrawn, and
//! re-published in quick succession would vanish from its own provider's index.
//!
//! The fix republishes the CURRENT baseline (read under `announce_mu`), so
//! there is no snapshot to go stale. This test drives the race directly: each
//! round registers a service and immediately announces a fresh tag, then waits
//! for the spawned re-announce to land and asserts the tag survived.

#![cfg(all(feature = "net", feature = "cortex"))]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::capability::{CapabilityFilter, CapabilitySet};
use net::adapter::net::cortex::{
    RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, RpcStatus,
};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig};

const PSK: [u8; 32] = [0x6Au8; 32];

struct Noop;

#[async_trait::async_trait]
impl RpcHandler for Noop {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: ctx.payload.body,
        })
    }
}

async fn build_node() -> Arc<MeshNode> {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5))
        .with_handshake(3, Duration::from_secs(2))
        // No rate-limit window: every announce in this test reaches the
        // announce path, so the spawned re-announce and the explicit one
        // actually contend (the race under test).
        .with_min_announce_interval(Duration::from_millis(0));
    Arc::new(MeshNode::new(EntityKeypair::generate(), cfg).await.unwrap())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_serve_rpc_auto_reannounce_never_reverts_a_concurrent_announce() {
    let node = build_node().await;
    let node_id = node.node_id();
    let mut handles = Vec::new();

    for round in 0..40u32 {
        let tag = format!("fresh-{round}");
        // Each registration spawns an auto re-announce that observes the
        // baseline as it is when the task RUNS — a burst of them queues several
        // behind the explicit announce below, which is the window the stale
        // snapshot used to reopen.
        for slot in 0..8u32 {
            handles.push(
                node.serve_rpc(&format!("svc-{round}-{slot}"), Arc::new(Noop))
                    .expect("serve_rpc"),
            );
        }
        // ...and the operator announces a new baseline immediately after, so
        // the two announces contend for `announce_mu`.
        node.announce_capabilities(CapabilitySet::new().add_tag(tag.clone()))
            .await
            .expect("announce");

        // Give the spawned re-announce time to land, then assert it republished
        // rather than reverted. The self-index is the announcement this node
        // would ship, so a revert here is not cosmetic: it drops the capability
        // from the wire — and from this node's own discovery — until something
        // else announces.
        let filter = CapabilityFilter::new().require_tag(&tag);
        let survived = wait_until(
            || node.find_nodes_by_filter(&filter).contains(&node_id),
            Duration::from_millis(500),
        )
        .await;
        assert!(
            survived,
            "round {round}: the auto re-announce reverted the baseline to a \
             pre-`{tag}` snapshot",
        );
        // Belt and braces: it must STAY announced — the spawned task may not
        // have run when the check above first passed.
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            node.find_nodes_by_filter(&filter).contains(&node_id),
            "round {round}: `{tag}` was reverted after the auto re-announce ran",
        );
    }
}

async fn wait_until<F: FnMut() -> bool>(mut cond: F, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    cond()
}
