//! H2 regression (2026-07-31 channel-auth audit): an RPC channel ACL
//! installed by the operator must survive `serve_rpc*`.
//!
//! The SDK documents the hardening sequence as "register a strict
//! config, *then* serve". Pre-fix, `auto_register_rpc_channels` used
//! replacing inserts, so `serve_rpc` discarded that config and left the
//! service running with permissive defaults — no error, no log, and a
//! posture indistinguishable from never having configured anything.
//!
//! These tests drive the real public entry points against a real
//! `Mesh` built through `MeshBuilder`, and assert on the registry the
//! authorization path actually reads (`get_by_name`, including prefix
//! resolution).

#![cfg(all(feature = "net", feature = "cortex"))]

use std::sync::Arc;

use bytes::Bytes;
use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::mesh_rpc::{RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, RpcStatus};
use net_sdk::ChannelConfig;

use net::adapter::net::channel::{ChannelConfigRegistry, ChannelId, ChannelName};
use net::adapter::net::identity::EntityKeypair;

const PSK: [u8; 32] = [0x5Au8; 32];

async fn mesh() -> Mesh {
    MeshBuilder::new("127.0.0.1:0", &PSK)
        .unwrap()
        .build()
        .await
        .unwrap()
}

/// The registry the authorization path reads. `MeshBuilder` always
/// installs one, so the `expect` is structural.
fn configs(mesh: &Mesh) -> Arc<ChannelConfigRegistry> {
    mesh.inner()
        .channel_configs()
        .expect("MeshBuilder installs a ChannelConfigRegistry")
        .clone()
}

struct Noop;

#[async_trait::async_trait]
impl RpcHandler for Noop {
    async fn call(&self, _ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: Bytes::new(),
        })
    }
}

fn gated(name: &str, root: &EntityKeypair) -> ChannelConfig {
    let channel = ChannelName::new(name).expect("valid channel name");
    ChannelConfig::new(ChannelId::new(channel)).with_token_roots(vec![root.entity_id().clone()])
}

/// The headline case: a token-gated `<service>.requests` registered
/// before `serve_rpc` is still token-gated after it.
#[tokio::test]
async fn serve_rpc_preserves_operator_request_channel_acl() {
    let mesh = mesh().await;
    let root = EntityKeypair::generate();

    mesh.register_channel(gated("acl.svc.requests", &root));
    let handle = mesh
        .serve_rpc("acl.svc", Arc::new(Noop))
        .expect("serve_rpc");

    let cfg = configs(&mesh)
        .get_by_name("acl.svc.requests")
        .expect("request channel config must still exist")
        .clone();
    assert!(
        cfg.token_required(),
        "operator's token gate on `<service>.requests` was discarded by serve_rpc"
    );
    assert_eq!(
        cfg.token_roots.len(),
        1,
        "the operator's root of trust must survive verbatim"
    );
    assert_eq!(cfg.token_roots[0], *root.entity_id());

    drop(handle);
}

/// The reply family is the one H3 depends on: a prefix ACL registered
/// through the (newly real) `register_channel_prefix` must survive, and
/// must still resolve for a dynamically-named per-caller channel.
#[tokio::test]
async fn serve_rpc_preserves_operator_reply_prefix_acl() {
    let mesh = mesh().await;
    let root = EntityKeypair::generate();

    mesh.register_channel_prefix("acl2.svc.replies.", gated("acl2.svc.replies.prefix", &root));
    let handle = mesh
        .serve_rpc("acl2.svc", Arc::new(Noop))
        .expect("serve_rpc");

    // Resolve through the same path `authorize_subscribe` uses: an
    // exact-match miss falling through to the prefix table.
    let cfg = configs(&mesh)
        .get_by_name("acl2.svc.replies.00112233445566aa")
        .expect("reply prefix must still resolve for a per-caller channel")
        .clone();
    assert!(
        cfg.token_required(),
        "operator's reply-prefix gate was discarded by serve_rpc"
    );
    assert_eq!(cfg.token_roots[0], *root.entity_id());

    drop(handle);
}

/// Without a pre-installed config, auto-registration must still install
/// its permissive default — otherwise the dynamic per-caller reply
/// subscriptions nRPC depends on would fail closed against the SDK's
/// fail-closed registry. Guards against "fix H2 by registering nothing".
#[tokio::test]
async fn serve_rpc_still_installs_defaults_when_operator_configured_nothing() {
    let mesh = mesh().await;
    let handle = mesh
        .serve_rpc("acl3.svc", Arc::new(Noop))
        .expect("serve_rpc");

    let req = configs(&mesh)
        .get_by_name("acl3.svc.requests")
        .expect("default request channel must be auto-registered")
        .clone();
    assert!(!req.token_required(), "default is permissive");

    let reply = configs(&mesh)
        .get_by_name("acl3.svc.replies.00112233445566aa")
        .expect("default reply prefix must be auto-registered")
        .clone();
    assert!(!reply.token_required(), "default is permissive");

    drop(handle);
}

/// `register_channel` is documented as idempotent ("re-registering the
/// same channel replaces the prior config"), but pre-fix the canonical-
/// hash index was not: `insert` pushed the name into `by_hash`
/// unconditionally, so the second registration grew the bucket to
/// `[name, name]`, `get(hash)` read its own duplicate as a collision,
/// and canonical-hash lookup started returning `None` for a channel
/// that plainly exists.
///
/// (Serving the same service twice cannot reach this — `serve_rpc`
/// rejects with `AlreadyServing` before auto-registration runs a second
/// time. The reachable paths are an operator re-registering, and
/// register-then-serve; both are covered here.)
#[tokio::test]
async fn repeated_register_keeps_request_channel_resolvable_by_hash() {
    let mesh = mesh().await;
    let name = ChannelName::new("acl4.svc.requests").unwrap();
    let hash = ChannelId::new(name.clone()).hash();
    let cfg = || ChannelConfig::new(ChannelId::new(name.clone()));

    mesh.register_channel(cfg());
    mesh.register_channel(cfg());
    mesh.register_channel(cfg());

    assert!(
        configs(&mesh).get(hash).is_some(),
        "canonical-hash lookup must survive re-registration of one channel"
    );

    // …and still after auto-registration runs over the same name.
    let handle = mesh
        .serve_rpc("acl4.svc", Arc::new(Noop))
        .expect("serve_rpc");
    assert!(
        configs(&mesh).get(hash).is_some(),
        "canonical-hash lookup must survive register-then-serve"
    );

    drop(handle);
}
