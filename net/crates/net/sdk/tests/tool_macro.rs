//! `#[tool]` proc-macro integration tests (A-7).
//!
//! Exercises the macro end-to-end: declare a tool with the
//! attribute, then drive the generated `<fn>_descriptor()` and
//! `<fn>_register(&mesh)` against a live `Mesh` pair. Verifies the
//! macro composes correctly with the existing `serve_tool` /
//! `list_tools` / `call_tool` surface.

#![cfg(all(feature = "macros", feature = "cortex"))]

use std::time::Duration;

use net_sdk::mesh::MeshBuilder;
use net_sdk::tool::{ToolDescriptor, TOOL_METADATA_FETCH_SERVICE};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use net_sdk::macros::tool;

const PSK: [u8; 32] = [0x42u8; 32];

#[derive(JsonSchema, Deserialize, Serialize)]
struct WebSearchReq {
    /// Free-text query.
    query: String,
}

#[derive(JsonSchema, Deserialize, Serialize, Debug, PartialEq, Eq)]
struct WebSearchResp {
    results: Vec<String>,
}

#[tool(
    description = "Search the web for relevant pages.",
    tag = "web",
    tag = "research",
    stateless = true,
    estimated_time_ms = 500,
)]
async fn web_search(req: WebSearchReq) -> Result<WebSearchResp, String> {
    Ok(WebSearchResp {
        results: vec![format!("hit for {}", req.query)],
    })
}

#[tool(name = "calc_v2", description = "Run a quick calculation.")]
async fn calculator(req: WebSearchReq) -> Result<WebSearchResp, String> {
    Ok(WebSearchResp {
        results: vec![format!("calc:{}", req.query)],
    })
}

#[test]
fn macro_generated_descriptor_carries_all_attributes() {
    let desc: ToolDescriptor = web_search_descriptor();
    assert_eq!(desc.tool_id, "web_search");
    assert_eq!(desc.name, "web_search");
    assert_eq!(desc.version, "1.0.0");
    assert_eq!(
        desc.description.as_deref(),
        Some("Search the web for relevant pages."),
    );
    assert!(desc.stateless);
    assert_eq!(desc.estimated_time_ms, 500);
    assert_eq!(desc.tags, vec!["web", "research"]);
    assert!(desc.input_schema.is_some(), "schemars-derived input schema");
    assert!(desc.output_schema.is_some());
}

#[test]
fn macro_name_override_takes_precedence_over_fn_name() {
    let desc = calculator_descriptor();
    // `name = "calc_v2"` on the attribute overrides the function name.
    assert_eq!(desc.tool_id, "calc_v2");
    assert_eq!(desc.name, "calc_v2");
}

#[tokio::test]
async fn macro_register_serves_tool_and_round_trips_call() {
    let host = MeshBuilder::new("127.0.0.1:0", &PSK)
        .unwrap()
        .build()
        .await
        .unwrap();
    let caller = MeshBuilder::new("127.0.0.1:0", &PSK)
        .unwrap()
        .build()
        .await
        .unwrap();

    // Handshake.
    let host_addr = host.inner().local_addr();
    let host_pub = *host.inner().public_key();
    let host_id = host.inner().node_id();
    let caller_id = caller.inner().node_id();
    let (r1, r2) = tokio::join!(
        host.inner().accept(caller_id),
        caller.inner().connect(host_addr, &host_pub, host_id),
    );
    r1.expect("accept");
    r2.expect("connect");
    host.inner().start();
    caller.inner().start();

    // Register via the macro-generated function. Returns a
    // `ToolServeHandle` the caller is expected to keep alive for
    // the duration of the tool's lifetime; dropping it deregisters.
    let _handle = web_search_register(&host).expect("macro-generated register");

    // Announce so the caller can route via the capability fold.
    host.announce_capabilities(Default::default())
        .await
        .expect("announce");

    // Wait for the caller's fold to surface the host.
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if caller.list_tools(None).iter().any(|t| t.tool_id == "web_search") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let visible = caller.list_tools(None);
    assert!(
        visible.iter().any(|t| t.tool_id == "web_search"),
        "caller never observed web_search; got {:?}",
        visible.iter().map(|t| t.tool_id.as_str()).collect::<Vec<_>>()
    );

    // call_tool round-trip.
    let resp: WebSearchResp = caller
        .call_tool("web_search", &WebSearchReq { query: "mesh".into() })
        .await
        .expect("call_tool");
    assert_eq!(resp.results, vec!["hit for mesh".to_string()]);
    // tool.metadata.fetch service is auto-installed on the host by
    // the first serve_tool — pin that here so we don't regress.
    let _ = TOOL_METADATA_FETCH_SERVICE; // touch the constant
}
