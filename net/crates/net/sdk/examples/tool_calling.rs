//! End-to-end AI tool-calling example.
//!
//! Demonstrates the full Rust SDK flow with two meshes in one
//! process and a mocked LLM:
//!
//! 1. Tool author declares `web_search` + `calculator` with the
//!    `#[tool]` proc macro.
//! 2. Host registers both tools (`web_search_register` /
//!    `calculator_register`) and announces capabilities so the
//!    agent's fold picks them up.
//! 3. Agent enumerates tools via `Mesh::list_tools(None)`.
//! 4. Agent lowers each descriptor to OpenAI's tool-definition
//!    shape via `formats::openai::to_openai_tool` — exactly what
//!    you'd feed into a real `POST /v1/chat/completions` payload's
//!    `tools` array.
//! 5. A mocked LLM responds with a `tool_calls[]` reply naming the
//!    `web_search` tool.
//! 6. Agent parses the reply via `formats::openai::lower_openai_tool_call`
//!    and dispatches it to the mesh via `Mesh::call_tool`.
//! 7. The handler runs on the host, the typed response comes back,
//!    the agent prints it.
//!
//! Real agent code substitutes a real LLM client for the mocked
//! `pretend_llm_reply()` step; everything else stays the same.
//!
//! ```text
//! cargo run --example tool_calling --features net,macros
//! ```
//!
//! The same shape works across hosts: replace the `127.0.0.1:0`
//! addresses with real network addresses + a shared PSK and you
//! have multi-host AI tool calling over the mesh.

use std::time::Duration;

use net_sdk::macros::tool;
use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::tool::formats::openai;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Tool authoring — single attribute, schema derivation, atomic register.
// ---------------------------------------------------------------------------

#[derive(JsonSchema, Deserialize, Serialize, Debug)]
struct WebSearchReq {
    /// Free-text query string.
    query: String,
}

#[derive(JsonSchema, Deserialize, Serialize, Debug)]
struct WebSearchResp {
    results: Vec<String>,
}

#[tool(
    description = "Search the web for relevant pages.",
    tag = "web",
    tag = "research",
    estimated_time_ms = 500
)]
async fn web_search(req: WebSearchReq) -> Result<WebSearchResp, String> {
    // Real code would dispatch to a search backend here. For the
    // example, just synthesize a result so we exercise the round-trip.
    Ok(WebSearchResp {
        results: vec![format!("first hit for '{}'", req.query)],
    })
}

#[derive(JsonSchema, Deserialize, Serialize, Debug)]
struct CalcReq {
    /// Arithmetic expression to evaluate, e.g. `"2 + 2"`.
    expression: String,
}

#[derive(JsonSchema, Deserialize, Serialize, Debug)]
struct CalcResp {
    answer: String,
}

#[tool(
    description = "Evaluate a simple arithmetic expression.",
    tag = "math",
    estimated_time_ms = 10
)]
async fn calculator(req: CalcReq) -> Result<CalcResp, String> {
    Ok(CalcResp {
        answer: format!("(would compute `{}`)", req.expression),
    })
}

// ---------------------------------------------------------------------------
// Mocked LLM — stands in for a real `POST /v1/chat/completions` call.
// ---------------------------------------------------------------------------

/// Pretend an OpenAI-compatible model responded with a `tool_calls`
/// entry asking the agent to invoke `web_search`. In a real
/// integration this comes from the provider's HTTP response.
fn pretend_llm_reply(_lowered_tools: &[serde_json::Value]) -> serde_json::Value {
    serde_json::json!({
        "id": "call_demo_abc123",
        "type": "function",
        "function": {
            "name": "web_search",
            "arguments": "{\"query\":\"how does net's capability fold work\"}"
        }
    })
}

// ---------------------------------------------------------------------------
// Driver
// ---------------------------------------------------------------------------

const PSK: [u8; 32] = [0x42u8; 32];

async fn build_pair() -> (Mesh, Mesh) {
    let host = MeshBuilder::new("127.0.0.1:0", &PSK)
        .unwrap()
        .build()
        .await
        .unwrap();
    let agent = MeshBuilder::new("127.0.0.1:0", &PSK)
        .unwrap()
        .build()
        .await
        .unwrap();
    // Handshake.
    let host_addr = host.inner().local_addr();
    let host_pub = *host.inner().public_key();
    let host_id = host.inner().node_id();
    let agent_id = agent.inner().node_id();
    let (r1, r2) = tokio::join!(
        host.inner().accept(agent_id),
        agent.inner().connect(host_addr, &host_pub, host_id),
    );
    r1.unwrap();
    r2.unwrap();
    host.inner().start();
    agent.inner().start();
    (host, agent)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let (host, agent) = build_pair().await;

    // (1) HOST: register two tools via the macro-generated *_register fns.
    let _h1 = web_search_register(&host).expect("register web_search");
    let _h2 = calculator_register(&host).expect("register calculator");
    host.announce_capabilities(Default::default())
        .await
        .unwrap();
    println!("[host] registered 2 tools, announced capabilities");

    // (2) AGENT: wait for the fold to surface the host's tools, then
    // walk them with list_tools.
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if agent.list_tools(None).len() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let tools = agent.list_tools(None);
    println!("[agent] list_tools() = {} tool(s):", tools.len());
    for t in &tools {
        println!(
            "  - {} v{}  ({}; tags={:?})",
            t.tool_id,
            t.version,
            t.description.as_deref().unwrap_or("(no description)"),
            t.tags,
        );
    }

    // (3) AGENT: lower descriptors to OpenAI's tool-definition shape.
    // In a real integration this would feed straight into
    //   POST https://api.openai.com/v1/chat/completions { ..., "tools": [...] }
    let lowered: Vec<_> = tools.iter().map(openai::to_openai_tool).collect();
    println!(
        "[agent] lowered to OpenAI tools array (first one shown):\n{}",
        serde_json::to_string_pretty(&lowered[0]).unwrap(),
    );

    // (4) Pretend the LLM responded with a tool_call asking for web_search.
    let llm_reply = pretend_llm_reply(&lowered);
    println!(
        "[llm]   replied with tool_call:\n{}",
        serde_json::to_string_pretty(&llm_reply).unwrap(),
    );

    // (5) AGENT: parse the LLM reply, dispatch via call_tool.
    let spec = openai::lower_openai_tool_call(&llm_reply).expect("parse tool_call");
    println!(
        "[agent] dispatching call_tool({:?}, {:?})",
        spec.name, spec.arguments_json,
    );
    let req: WebSearchReq = serde_json::from_str(&spec.arguments_json)?;
    let resp: WebSearchResp = agent.call_tool(&spec.name, &req).await?;
    println!("[agent] response: {:?}", resp);

    // (6) Echo the tool result back into the (mocked) chat history.
    // Real agent code packages this as a `{"role": "tool", "tool_call_id":
    // spec.provider_call_id, "content": ...}` message and feeds it back
    // into the next chat-completion call.
    println!(
        "[agent] would feed back: {{role: \"tool\", tool_call_id: {:?}, content: {:?}}}",
        spec.provider_call_id, resp,
    );

    Ok(())
}
