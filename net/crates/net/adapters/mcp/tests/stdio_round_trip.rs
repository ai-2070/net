//! Integration: `StdioMcpClient` against the hermetic `net-mcp-fixture`.
//!
//! Proves the full stdio JSON-RPC path end to end — spawn, `initialize`,
//! `tools/list`, and `tools/call` for a success, a typed-args call, a
//! tool-level error, and an unknown tool (a protocol error). This is the
//! Phase 0b translation core with no mesh in the picture.

use std::time::Duration;

use net_mcp::spec::{Implementation, PROTOCOL_VERSION};
use net_mcp::wrap::{McpError, StdioMcpClient};
use serde_json::json;

/// The fixture binary built from this same crate.
const FIXTURE: &str = env!("CARGO_BIN_EXE_net-mcp-fixture");

/// Per-call ceiling so a wedged fixture fails the test instead of hanging CI.
const CALL_TIMEOUT: Duration = Duration::from_secs(10);

fn client_info() -> Implementation {
    Implementation {
        name: "net-mcp-test".to_string(),
        version: "0.0.0".to_string(),
    }
}

async fn connect() -> StdioMcpClient {
    StdioMcpClient::spawn(FIXTURE, &[], &[], client_info())
        .await
        .expect("spawn fixture")
}

/// Await a future under [`CALL_TIMEOUT`], panicking with `what` on timeout.
async fn within<F, T>(what: &str, fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    tokio::time::timeout(CALL_TIMEOUT, fut)
        .await
        .unwrap_or_else(|_| panic!("{what} timed out"))
}

#[tokio::test]
async fn initialize_lists_and_calls_the_fixture_tools() {
    let client = connect().await;

    let init = within("initialize", client.initialize())
        .await
        .expect("initialize");
    assert_eq!(init.server_info.name, "net-mcp-fixture");
    assert_eq!(init.protocol_version, PROTOCOL_VERSION);

    let tools = within("list_tools", client.list_tools())
        .await
        .expect("list_tools");
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    for expected in ["echo", "add", "boom", "slow"] {
        assert!(
            names.contains(&expected),
            "missing tool {expected}: {names:?}"
        );
    }

    // echo round-trips its argument.
    let echo = within(
        "echo",
        client.call_tool("echo", json!({ "message": "hello mesh" })),
    )
    .await
    .expect("echo");
    assert!(!echo.is_error);
    assert_eq!(echo.text(), "hello mesh");

    // add returns the typed sum.
    let add = within("add", client.call_tool("add", json!({ "a": 2, "b": 3 })))
        .await
        .expect("add");
    assert_eq!(add.text(), "5");

    // boom is a TOOL-level error (is_error), not a protocol error — the call
    // itself succeeds.
    let boom = within("boom", client.call_tool("boom", json!({})))
        .await
        .expect("boom");
    assert!(boom.is_error, "boom must report a tool-level error");
}

#[tokio::test]
async fn unknown_tool_is_a_protocol_error() {
    let client = connect().await;
    within("initialize", client.initialize())
        .await
        .expect("initialize");

    // An undefined tool is a JSON-RPC error response, distinct from a
    // successful result carrying is_error.
    let result = within("unknown", client.call_tool("does_not_exist", json!({}))).await;
    assert!(
        matches!(result, Err(McpError::Protocol(_))),
        "expected a protocol error, got {result:?}",
    );
}

#[tokio::test]
async fn slow_tool_latency_is_tolerated() {
    let client = connect().await;
    within("initialize", client.initialize())
        .await
        .expect("initialize");

    let slow = within("slow", client.call_tool("slow", json!({ "ms": 50 })))
        .await
        .expect("slow");
    assert!(!slow.is_error);
    assert_eq!(slow.text(), "slept 50ms");
}

#[tokio::test]
async fn a_credential_env_never_appears_in_a_tool_result() {
    // Token-leak regression (doctrine #100). A credential configured on a
    // wrapped server lives in its child-process env and must never appear in a
    // tool RESULT the bridge returns. This is one of only two things the bridge
    // puts on the wire; the other — the capability announcement — is covered
    // structurally by `wrap::session`'s unit tests.
    const SENTINEL: &str = "SENTINEL-TOKEN-9f83aa-do-not-leak";

    let envs = vec![("SECRET_TOKEN".to_string(), SENTINEL.to_string())];
    let client = StdioMcpClient::spawn(FIXTURE, &[], &envs, client_info())
        .await
        .expect("spawn fixture with a credential");
    within("initialize", client.initialize())
        .await
        .expect("initialize");

    let echo = within(
        "echo",
        client.call_tool("echo", json!({ "message": "hello" })),
    )
    .await
    .expect("echo");
    let serialized = serde_json::to_string(&echo).expect("serialize result");
    assert!(
        !serialized.contains(SENTINEL),
        "a wrapped credential must never appear in a tool result",
    );
    // The tool still works — only the credential is withheld.
    assert_eq!(echo.text(), "hello");
}

#[tokio::test]
async fn big_tool_returns_a_large_payload() {
    let client = connect().await;
    within("initialize", client.initialize())
        .await
        .expect("initialize");

    let big = within("big", client.call_tool("big", json!({ "size": 100_000 })))
        .await
        .expect("big");
    assert!(!big.is_error);
    assert_eq!(
        big.text().len(),
        100_000,
        "the full large payload round-trips"
    );
}

#[tokio::test]
async fn bump_changes_the_tool_set_and_emits_list_changed() {
    let client = connect().await;
    within("initialize", client.initialize())
        .await
        .expect("initialize");

    // `bonus` is absent before the bump.
    let before = within("list", client.list_tools()).await.expect("list");
    assert!(!before.iter().any(|t| t.name == "bonus"));

    // Subscribe BEFORE triggering the change so the notification isn't missed.
    let mut changed = client.subscribe_list_changed();
    within("bump", client.call_tool("_bump", json!({})))
        .await
        .expect("bump");

    // The server emits tools/list_changed; the client forwards it to subscribers.
    tokio::time::timeout(CALL_TIMEOUT, changed.recv())
        .await
        .expect("list_changed within the ceiling")
        .expect("list_changed channel stayed open");

    // Re-reading the tool set now shows `bonus`.
    let after = within("relist", client.list_tools()).await.expect("relist");
    assert!(
        after.iter().any(|t| t.name == "bonus"),
        "bonus appears after a bump",
    );
}
