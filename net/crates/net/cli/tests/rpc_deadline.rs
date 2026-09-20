// SPDX-License-Identifier: MIT OR Apache-2.0
//! Handler-side effects outlive a caller timeout; the CLI must not retry.
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::dataforts::blob::{
    TransferRpcRequest, TransferRpcResponse, TRANSFER_SERVICE,
};
use net_sdk::mesh_rpc::{RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, RpcStatus};
use tokio::sync::Notify;

#[derive(Default)]
struct EffectHandler {
    effects: AtomicUsize,
    delay: AtomicBool,
    entered: Notify,
    release: Notify,
    completed: Notify,
}

#[async_trait::async_trait]
impl RpcHandler for EffectHandler {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        let request: TransferRpcRequest = postcard::from_bytes(&ctx.payload.body).unwrap();
        assert_eq!(request, TransferRpcRequest::Cancel { stream_id: 42 });
        // A synthetic non-idempotent accepted effect at the real remote handler.
        // This tests CLI retry semantics, not transfer engine cancellation itself.
        self.effects.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        if self.delay.load(Ordering::SeqCst) {
            self.release.notified().await;
        }
        self.completed.notify_one();
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: Vec::new(),
            body: postcard::to_allocvec(&TransferRpcResponse::Cancelled { existed: true })
                .unwrap()
                .into(),
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn remote_effect_occurs_once_despite_timeout_and_late_response() {
    let mesh = net_sdk::MeshBuilder::new("127.0.0.1:0", &[0x42; 32])
        .unwrap()
        .build()
        .await
        .unwrap();
    let handler = Arc::new(EffectHandler::default());
    handler.delay.store(true, Ordering::SeqCst);
    let _serve = mesh.serve_rpc(TRANSFER_SERVICE, handler.clone()).unwrap();
    mesh.start();
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    std::fs::write(&config, "").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let spawn = || {
        tokio::process::Command::new(assert_cmd::cargo::cargo_bin("net-mesh"))
            .env_remove("NET_MESH_CONFIG")
            .env_remove("NET_MESH_PROFILE")
            .arg("--config")
            .arg(&config)
            .args([
                "--output",
                "json",
                "--timeout",
                "2s",
                "transfer",
                "cancel",
                "42",
                "--node-addr",
                &mesh.local_addr().to_string(),
                "--node-id",
                &mesh.node_id().to_string(),
                "--node-pubkey",
                &hex::encode(mesh.public_key()),
                "--psk-hex",
                &"42".repeat(32),
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap()
    };
    let child = spawn();
    tokio::time::timeout(Duration::from_secs(2), handler.entered.notified())
        .await
        .unwrap();
    assert_eq!(handler.effects.load(Ordering::SeqCst), 1);
    let output = tokio::time::timeout(Duration::from_secs(5), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(7),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    assert!(diagnostic.contains("may have committed"));
    assert!(diagnostic.contains("did not retry"));
    assert!(!diagnostic.contains(&"42".repeat(32)));
    assert_eq!(handler.effects.load(Ordering::SeqCst), 1);

    // Completion remains possible after the caller has already reported timeout.
    handler.release.notify_one();
    tokio::time::timeout(Duration::from_secs(2), handler.completed.notified())
        .await
        .unwrap();
    assert_eq!(handler.effects.load(Ordering::SeqCst), 1);

    // Positive control: another explicitly requested invocation succeeds. No
    // transport/codec failure can make the timeout witness vacuously pass.
    handler.delay.store(false, Ordering::SeqCst);
    let output = tokio::time::timeout(Duration::from_secs(5), spawn().wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["cancelled"], true);
    assert!(output.stdout.ends_with(b"\n"));
    assert_eq!(handler.effects.load(Ordering::SeqCst), 2);
}
