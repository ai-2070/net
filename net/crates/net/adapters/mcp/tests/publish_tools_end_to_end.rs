//! End-to-end supply side for **local tool publication** (V2 Phase 2, Slice
//! A1): a node announces its *own* in-process tools via
//! `ServerPublisher::publish_tools` with a callback-backed [`ToolInvoker`] —
//! no wrapped MCP server — and another node discovers, describes, and invokes
//! them through the *same* announce/describe/serve path `net wrap` uses.
//!
//! This is the inverse of `wrap_end_to_end.rs`: the invoke seam is a plain
//! Rust closure instead of a `tools/call` on a child process, proving the
//! publication machinery is source-agnostic and — crucially — that the
//! existing demand-side discovery + describe protocol reads a local
//! publication with no consume-side change (the "Mac Hermes lists and invokes
//! pc/*" acceptance, at the wire layer).
//!
//! No fixture: the two-node mesh harness is pure SDK, so this runs on a plain
//! `cargo test -p net-mesh-mcp` (unlike the fixture-gated wrap tests).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use net_mcp::bridge::{DescribeResponse, DESCRIBE_SERVICE};
use net_mcp::spec::{CallToolResult, Tool};
use net_mcp::wrap::{
    CredentialStatus, LoweringContext, McpError, OwnerScope, ServerPublisher, Substitutability,
    ToolInvoker, WrapConfig,
};
use net_sdk::capabilities::CapabilityFilter;
use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::mesh_rpc::CallOptions;
use serde_json::json;

/// A callback-backed invoker: it runs plain Rust instead of a wrapped MCP
/// server. `echo` returns its `message`; `add` sums `a` + `b` as structured
/// content; anything else is a tool-level error. A call counter proves the
/// closure actually executed for the round-trip (not a cached announcement).
#[derive(Default)]
struct LocalTools {
    calls: AtomicUsize,
}

#[async_trait]
impl ToolInvoker for LocalTools {
    async fn call_tool(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<CallToolResult, McpError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        match name {
            "echo" => {
                let msg = arguments
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                Ok(CallToolResult::text_ok(msg))
            }
            "add" => {
                let a = arguments.get("a").and_then(|v| v.as_i64()).unwrap_or(0);
                let b = arguments.get("b").and_then(|v| v.as_i64()).unwrap_or(0);
                let mut result = CallToolResult::text_ok((a + b).to_string());
                result.structured_content = Some(json!({ "sum": a + b }));
                Ok(result)
            }
            other => Ok(CallToolResult::text_error(format!("unknown tool: {other}"))),
        }
    }
}

fn tool(name: &str, description: &str, schema: serde_json::Value) -> Tool {
    Tool {
        name: name.to_string(),
        title: None,
        description: Some(description.to_string()),
        input_schema: schema,
        output_schema: None,
    }
}

fn local_tools() -> Vec<Tool> {
    vec![
        tool(
            "echo",
            "Return the message.",
            json!({ "type": "object", "properties": { "message": { "type": "string" } } }),
        ),
        tool(
            "add",
            "Sum two integers.",
            json!({
                "type": "object",
                "properties": { "a": { "type": "integer" }, "b": { "type": "integer" } }
            }),
        ),
    ]
}

fn ctx() -> LoweringContext {
    LoweringContext {
        server_version: "hermes-1.0".to_string(),
        credential_status: CredentialStatus::None,
        substitutability: Substitutability::ProviderLocal,
    }
}

async fn build_mesh(psk: &[u8; 32]) -> Mesh {
    MeshBuilder::new("127.0.0.1:0", psk)
        .expect("mesh builder")
        .build()
        .await
        .expect("build mesh")
}

/// Bidirectionally connect two meshes and start their receive loops (the SDK
/// cross-node test idiom, as `wrap_end_to_end.rs`).
async fn handshake(a: &Mesh, b: &Mesh) {
    let pub_b = *b.inner().public_key();
    let nid_b = b.inner().node_id();
    let nid_a = a.inner().node_id();
    let addr_b = b.inner().local_addr();
    let (r1, r2) = tokio::join!(b.inner().accept(nid_a), async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        a.inner().connect(addr_b, &pub_b, nid_b).await
    });
    r1.expect("accept");
    r2.expect("connect");
    a.inner().start();
    b.inner().start();
}

async fn wait_until<F: FnMut() -> bool>(mut cond: F, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    cond()
}

async fn call_bounded(
    caller: &Mesh,
    target: u64,
    service: &str,
    body: Bytes,
) -> Result<net_sdk::mesh_rpc::RpcReply, String> {
    match tokio::time::timeout(
        Duration::from_secs(5),
        caller.call(target, service, body, CallOptions::default()),
    )
    .await
    {
        Ok(Ok(reply)) => Ok(reply),
        Ok(Err(e)) => Err(format!("rpc error: {e:?}")),
        Err(_) => Err("call timed out".to_string()),
    }
}

/// Retry a call a few times — the first cross-node call to a freshly-served
/// handler can lose its reply before the per-caller reply subscription
/// propagates; a retry lands (same rationale as `wrap_end_to_end.rs`).
async fn call_retry(
    caller: &Mesh,
    target: u64,
    service: &str,
    body: impl Fn() -> Bytes,
) -> Result<net_sdk::mesh_rpc::RpcReply, String> {
    let mut last = String::new();
    for _ in 0..5 {
        match call_bounded(caller, target, service, body()).await {
            Ok(reply) => return Ok(reply),
            Err(e) => {
                last = e;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
    Err(last)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_tools_are_discovered_described_and_invoked_across_two_nodes() {
    let caller = build_mesh(&[0x51u8; 32]).await; // node A (consumer)
    let host = Arc::new(build_mesh(&[0x51u8; 32]).await); // node B (provider)
    handshake(&caller, &host).await;
    let b_id = host.inner().node_id();

    // Owner-only to the caller's origin — admits it for both describe + invoke.
    let config = WrapConfig::owner_only(
        net_mcp::spec::Implementation {
            name: "hermes".to_string(),
            version: "1.0".to_string(),
        },
        caller.origin_hash(),
    );
    let invoker = Arc::new(LocalTools::default());
    let publisher = ServerPublisher::new(Arc::clone(&host));
    let publication = publisher
        .publish_tools(&local_tools(), Arc::clone(&invoker) as Arc<dyn ToolInvoker>, ctx(), config)
        .await
        .expect("publish local tools on the host");
    assert!(publication.tools().iter().any(|t| t == "echo"));
    assert!(publication.tools().iter().any(|t| t == "add"));
    assert!(publication.skipped_tools().is_empty());

    // The consumer discovers the provider advertising `echo` through the fold —
    // the same discovery path a wrapped MCP server uses.
    let filter = CapabilityFilter::new().require_tag("echo");
    assert!(
        wait_until(
            || caller.find_nodes(&filter).contains(&b_id),
            Duration::from_secs(5),
        )
        .await,
        "consumer discovers the local `echo` capability",
    );

    // Describe: the consumer reads the local tool's schema + description off the
    // same `mcp.bridge.describe` service — this is what the demand-side gateway
    // fetches to validate arguments and render the tool.
    let describe_reply = call_retry(&caller, b_id, DESCRIBE_SERVICE, || Bytes::from_static(b"{}"))
        .await
        .expect("describe succeeds");
    let described: DescribeResponse =
        serde_json::from_slice(describe_reply.body.as_ref()).expect("decode DescribeResponse");
    let echo = described
        .tools
        .iter()
        .find(|t| t.tool_id == "echo")
        .expect("echo present in the described catalog");
    assert_eq!(echo.description.as_deref(), Some("Return the message."));
    assert_eq!(echo.input_schema["properties"]["message"]["type"], "string");
    assert_eq!(echo.credential_status, "none");
    assert_eq!(echo.substitutability, "provider_local");
    // The schema content hash rides over the wire so a consumer can cache by it.
    assert_eq!(echo.schema_hash, net_mcp::wrap::schema_hash(&echo.input_schema));
    assert!(!echo.schema_hash.is_empty());

    // Invoke: the mesh call reaches the callback invoker and the result
    // round-trips, structured content included.
    let reply = call_retry(&caller, b_id, "echo", || {
        Bytes::from(serde_json::to_vec(&json!({ "message": "hi local" })).unwrap())
    })
    .await
    .expect("echo call succeeds");
    let result: CallToolResult =
        serde_json::from_slice(reply.body.as_ref()).expect("decode CallToolResult");
    assert!(!result.is_error);
    assert_eq!(result.text(), "hi local");

    let add_reply = call_retry(&caller, b_id, "add", || {
        Bytes::from(serde_json::to_vec(&json!({ "a": 2, "b": 3 })).unwrap())
    })
    .await
    .expect("add call succeeds");
    let add_result: CallToolResult =
        serde_json::from_slice(add_reply.body.as_ref()).expect("decode add result");
    assert_eq!(add_result.structured_content, Some(json!({ "sum": 5 })));
    assert!(
        invoker.calls.load(Ordering::Relaxed) >= 2,
        "the callback invoker actually ran for each invoke",
    );

    // Withdraw reverses the publication (services + announcement).
    publication.withdraw().await.expect("withdraw");
    assert!(
        !host
            .find_nodes(&CapabilityFilter::new().require_tag("echo"))
            .contains(&b_id),
        "withdraw clears the announced tool set",
    );

    caller.shutdown().await.ok();
    drop(publisher);
    if let Ok(host) = Arc::try_unwrap(host) {
        host.shutdown().await.ok();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_caller_outside_the_owner_scope_cannot_invoke_a_local_tool() {
    // The owner-scope gate applies to `publish_tools` exactly as to
    // `publish_server`: an excluded caller can discover but never invoke.
    let caller = build_mesh(&[0x52u8; 32]).await;
    let host = Arc::new(build_mesh(&[0x52u8; 32]).await);
    handshake(&caller, &host).await;
    let b_id = host.inner().node_id();

    let mut config = WrapConfig::owner_only(
        net_mcp::spec::Implementation {
            name: "hermes".to_string(),
            version: "1.0".to_string(),
        },
        0xDEAD_BEEF,
    );
    config.scope = OwnerScope::owner_only(0xDEAD_BEEF); // not the caller
    let invoker = Arc::new(LocalTools::default());
    let publisher = ServerPublisher::new(Arc::clone(&host));
    let _publication = publisher
        .publish_tools(&local_tools(), invoker as Arc<dyn ToolInvoker>, ctx(), config)
        .await
        .expect("publish local tools");

    let filter = CapabilityFilter::new().require_tag("echo");
    assert!(
        wait_until(
            || caller.find_nodes(&filter).contains(&b_id),
            Duration::from_secs(5),
        )
        .await,
        "the capability is reachable (discovery never implies invocation)",
    );

    // The owner-scope gate denies the excluded caller — either a structured
    // rejection or a dropped reply (an ultra-fast rejection can outrace the
    // reply subscription), both `Err`. Either way, no result reaches it.
    let outcome = call_bounded(
        &caller,
        b_id,
        "echo",
        Bytes::from(serde_json::to_vec(&json!({ "message": "denied" })).unwrap()),
    )
    .await;
    assert!(
        outcome.is_err(),
        "a caller outside the owner scope must not get a result; got {outcome:?}",
    );

    caller.shutdown().await.ok();
    drop(_publication);
    drop(publisher);
    if let Ok(host) = Arc::try_unwrap(host) {
        host.shutdown().await.ok();
    }
}
