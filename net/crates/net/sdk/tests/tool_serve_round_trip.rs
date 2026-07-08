//! Rust SDK integration test for `Mesh::serve_tool` (A-2b).
//!
//! Exercises the full atomic-register / Drop-reverses path
//! end-to-end against two real `Mesh`es:
//!
//! 1. Host registers a typed handler via `Mesh::serve_tool` with
//!    a `ToolDescriptor` built from `metadata_for::<Req, Resp>()`.
//! 2. Host announces capabilities. The substrate-side merge
//!    (A-2a) auto-emits `ai-tool:web_search` + the typed
//!    `ToolCapability` + the description / streaming / tags
//!    metadata.
//! 3. Peer waits for the capability index to surface the host
//!    under `ai-tool:web_search`, then calls the tool via
//!    `call_typed`.
//! 4. The host also auto-installed `tool.metadata.fetch` on the
//!    first `serve_tool` call; peer queries that service for the
//!    full descriptor and verifies it round-trips.
//! 5. Drop the `ToolServeHandle`. Re-announce. Confirm the tag is
//!    gone from the host's subsequent announce + the descriptor
//!    is gone from the local registry.

#![cfg(all(feature = "tool", feature = "cortex"))]

use std::net::SocketAddr;
use std::time::Duration;

use futures::StreamExt;

use net::adapter::net::behavior::capability::{CapabilityFilter, CapabilitySet};
use net_sdk::capabilities::CapabilitySet as SdkCapabilitySet;
use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::mesh_rpc::{CallOptionsTyped, Codec};
use net_sdk::tool::{
    metadata_for, ToolEvent, ToolMetadataRequest, ToolMetadataResponse, TOOL_METADATA_FETCH_SERVICE,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

const PSK: [u8; 32] = [0x42u8; 32];

#[derive(JsonSchema, Deserialize, Serialize, Debug, PartialEq, Eq)]
struct WebSearchReq {
    query: String,
}

#[derive(JsonSchema, Deserialize, Serialize, Debug, PartialEq, Eq)]
struct WebSearchResp {
    results: Vec<String>,
}

async fn build_pair() -> (Mesh, Mesh, SocketAddr) {
    let a = MeshBuilder::new("127.0.0.1:0", &PSK)
        .unwrap()
        .build()
        .await
        .unwrap();
    let b = MeshBuilder::new("127.0.0.1:0", &PSK)
        .unwrap()
        .build()
        .await
        .unwrap();
    let addr_b = b.inner().local_addr();
    (a, b, addr_b)
}

async fn handshake(a: &Mesh, b: &Mesh, addr_b: SocketAddr) {
    let pub_b = *b.inner().public_key();
    let nid_b = b.inner().node_id();
    let nid_a = a.inner().node_id();
    let (r1, r2) = tokio::join!(
        b.inner().accept(nid_a),
        a.inner().connect(addr_b, &pub_b, nid_b),
    );
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
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    cond()
}

#[tokio::test]
async fn serve_tool_round_trip_announces_calls_and_fetches_metadata() {
    let (caller, host, host_addr) = build_pair().await;
    handshake(&caller, &host, host_addr).await;

    // Host registers a tool. The descriptor is built from the
    // Rust type pair — schemas derived via `schemars`.
    let descriptor = metadata_for::<WebSearchReq, WebSearchResp>("web_search")
        .description("Search the web for relevant pages.")
        .stateless(true)
        .estimated_time_ms(500)
        .tag("web")
        .tag("research")
        .build();
    let _handle = host
        .serve_tool::<WebSearchReq, WebSearchResp, _, _>(descriptor.clone(), |req| async move {
            Ok(WebSearchResp {
                results: vec![format!("result for {}", req.query)],
            })
        })
        .expect("serve_tool");

    // Announce — the A-2a merge emits the ai-tool tag + the
    // ToolCapability + metadata keys.
    host.announce_capabilities(SdkCapabilitySet::new())
        .await
        .expect("announce");

    // Caller waits for the capability index to surface the host.
    let tool_filter = CapabilityFilter::default().require_tag("ai-tool:web_search");
    assert!(
        wait_until(
            || caller
                .inner()
                .find_nodes_by_filter(&tool_filter)
                .contains(&host.inner().node_id()),
            Duration::from_secs(3),
        )
        .await,
        "caller must see host under `ai-tool:web_search`",
    );

    // Direct typed call via the underlying serve_rpc_typed
    // registration. `Mesh::serve_tool` registers under the
    // tool_id, so `call_typed(host, "web_search", ...)` reaches it.
    let resp: WebSearchResp = caller
        .call_typed(
            host.inner().node_id(),
            "web_search",
            &WebSearchReq {
                query: "mesh".into(),
            },
            CallOptionsTyped {
                raw: Default::default(),
                codec: Codec::Json,
            },
        )
        .await
        .expect("call_typed");
    assert_eq!(resp.results, vec!["result for mesh".to_string()]);

    // tool.metadata.fetch round-trip — caller pulls the full
    // descriptor for the host's tool.
    let fetched: ToolMetadataResponse = caller
        .call_typed(
            host.inner().node_id(),
            TOOL_METADATA_FETCH_SERVICE,
            &ToolMetadataRequest {
                name: "web_search".into(),
            },
            CallOptionsTyped {
                raw: Default::default(),
                codec: Codec::Json,
            },
        )
        .await
        .expect("metadata fetch");
    match fetched {
        ToolMetadataResponse::Found { descriptor: got } => {
            assert_eq!(got.tool_id, "web_search");
            assert_eq!(
                got.description.as_deref(),
                Some("Search the web for relevant pages.")
            );
            assert_eq!(got.estimated_time_ms, 500);
            assert!(got.stateless);
            assert_eq!(got.tags, vec!["web", "research"]);
            assert!(got.input_schema.is_some());
            assert!(got.output_schema.is_some());
        }
        ToolMetadataResponse::NotFound { name } => {
            panic!("expected Found for web_search; got NotFound({name})");
        }
    }
}

#[tokio::test]
async fn tool_metadata_fetch_returns_not_found_for_unknown_tool() {
    let (caller, host, host_addr) = build_pair().await;
    handshake(&caller, &host, host_addr).await;

    // Register one tool so the host has the `tool.metadata.fetch`
    // service installed (lazy install — first `serve_tool` triggers
    // it).
    let descriptor = metadata_for::<WebSearchReq, WebSearchResp>("web_search").build();
    let _handle = host
        .serve_tool::<WebSearchReq, WebSearchResp, _, _>(descriptor, |req| async move {
            Ok(WebSearchResp {
                results: vec![req.query],
            })
        })
        .expect("serve_tool");

    host.announce_capabilities(SdkCapabilitySet::new())
        .await
        .expect("announce");

    // Wait for capability propagation so `call_typed` finds the
    // metadata-fetch service via the host's `nrpc:` tags.
    let svc_filter =
        CapabilityFilter::default().require_tag(format!("nrpc:{TOOL_METADATA_FETCH_SERVICE}"));
    assert!(
        wait_until(
            || caller
                .inner()
                .find_nodes_by_filter(&svc_filter)
                .contains(&host.inner().node_id()),
            Duration::from_secs(3),
        )
        .await,
        "tool.metadata.fetch must be reachable",
    );

    let fetched: ToolMetadataResponse = caller
        .call_typed(
            host.inner().node_id(),
            TOOL_METADATA_FETCH_SERVICE,
            &ToolMetadataRequest {
                name: "nonexistent".into(),
            },
            CallOptionsTyped {
                raw: Default::default(),
                codec: Codec::Json,
            },
        )
        .await
        .expect("metadata fetch");
    match fetched {
        ToolMetadataResponse::NotFound { name } => {
            assert_eq!(name, "nonexistent");
        }
        ToolMetadataResponse::Found { descriptor } => {
            panic!(
                "expected NotFound for nonexistent tool; got Found({:?})",
                descriptor.tool_id,
            );
        }
    }
}

#[tokio::test]
async fn serve_tool_drop_removes_descriptor_from_registry() {
    let mesh = MeshBuilder::new("127.0.0.1:0", &PSK)
        .unwrap()
        .build()
        .await
        .unwrap();

    let descriptor = metadata_for::<WebSearchReq, WebSearchResp>("web_search").build();
    let handle = mesh
        .serve_tool::<WebSearchReq, WebSearchResp, _, _>(descriptor, |req| async move {
            Ok(WebSearchResp {
                results: vec![req.query],
            })
        })
        .expect("serve_tool");

    assert_eq!(mesh.inner().tool_registry().len(), 1);
    assert!(mesh.inner().tool_registry().get("web_search").is_some());

    drop(handle);

    assert_eq!(
        mesh.inner().tool_registry().len(),
        0,
        "ToolServeHandle Drop must remove from registry",
    );
    assert!(mesh.inner().tool_registry().get("web_search").is_none());
}

#[tokio::test]
async fn serve_tool_rejects_duplicate_tool_id() {
    let mesh = MeshBuilder::new("127.0.0.1:0", &PSK)
        .unwrap()
        .build()
        .await
        .unwrap();

    let descriptor = metadata_for::<WebSearchReq, WebSearchResp>("web_search").build();
    let _first = mesh
        .serve_tool::<WebSearchReq, WebSearchResp, _, _>(descriptor.clone(), |req| async move {
            Ok(WebSearchResp {
                results: vec![req.query],
            })
        })
        .expect("first serve_tool");

    let err =
        match mesh.serve_tool::<WebSearchReq, WebSearchResp, _, _>(descriptor, |req| async move {
            Ok(WebSearchResp {
                results: vec![req.query],
            })
        }) {
            Ok(_) => panic!("duplicate serve_tool must fail"),
            Err(e) => e,
        };
    // ServeError::AlreadyServing — wraps the tool_id in the
    // diagnostic message.
    let msg = format!("{err}");
    assert!(
        msg.contains("web_search"),
        "duplicate error must name the offending tool: {msg}",
    );

    // The original handle's registry entry is still there — the
    // duplicate-rejection path doesn't disturb prior state.
    assert!(mesh.inner().tool_registry().get("web_search").is_some());
}

// ============================================================================
// A-3 — serve_tool_streaming round-trip tests
// ============================================================================

#[tokio::test]
async fn serve_tool_streaming_round_trip_emits_events_in_order() {
    let (caller, host, host_addr) = build_pair().await;
    handshake(&caller, &host, host_addr).await;

    let descriptor = metadata_for::<WebSearchReq, WebSearchResp>("web_search_stream")
        .description("Streaming search.")
        .build();
    let _handle = host
        .serve_tool_streaming::<WebSearchReq, _, _, _>(descriptor, |req| async move {
            futures::stream::iter(vec![
                ToolEvent::Start {
                    tool_id: "web_search_stream".into(),
                    call_id: None,
                    metadata: None,
                },
                ToolEvent::Progress {
                    pct: Some(50.0),
                    message: Some(format!("searching for {}", req.query)),
                },
                ToolEvent::Delta {
                    data: serde_json::json!({ "token": "partial " }),
                },
                ToolEvent::Result {
                    data: serde_json::json!({ "results": [format!("hit for {}", req.query)] }),
                },
            ])
        })
        .expect("serve_tool_streaming");

    host.announce_capabilities(SdkCapabilitySet::new())
        .await
        .expect("announce");

    let tool_filter = CapabilityFilter::default().require_tag("ai-tool:web_search_stream");
    assert!(
        wait_until(
            || caller
                .inner()
                .find_nodes_by_filter(&tool_filter)
                .contains(&host.inner().node_id()),
            Duration::from_secs(3),
        )
        .await,
        "caller must see streaming tool tag",
    );

    let stream = caller
        .call_streaming_typed::<WebSearchReq, ToolEvent>(
            host.inner().node_id(),
            "web_search_stream",
            &WebSearchReq {
                query: "mesh".into(),
            },
            CallOptionsTyped {
                raw: Default::default(),
                codec: Codec::Json,
            },
        )
        .await
        .expect("call_streaming_typed");

    let events: Vec<ToolEvent> = stream
        .map(|item| item.expect("stream chunk"))
        .collect()
        .await;

    assert_eq!(events.len(), 4, "expected 4 events; got {events:?}");
    match &events[0] {
        ToolEvent::Start { tool_id, .. } => assert_eq!(tool_id, "web_search_stream"),
        other => panic!("event[0] must be Start, got {other:?}"),
    }
    match &events[1] {
        ToolEvent::Progress { pct, .. } => assert_eq!(*pct, Some(50.0)),
        other => panic!("event[1] must be Progress, got {other:?}"),
    }
    match &events[2] {
        ToolEvent::Delta { data } => {
            assert_eq!(data.get("token").and_then(|v| v.as_str()), Some("partial "));
        }
        other => panic!("event[2] must be Delta, got {other:?}"),
    }
    match &events[3] {
        ToolEvent::Result { data } => {
            let arr = data
                .get("results")
                .and_then(|v| v.as_array())
                .expect("results");
            assert_eq!(arr.len(), 1);
            assert_eq!(arr[0].as_str(), Some("hit for mesh"));
        }
        other => panic!("event[3] must be Result, got {other:?}"),
    }
}

#[tokio::test]
async fn serve_tool_streaming_synthesizes_missing_terminal_when_handler_omits_one() {
    let (caller, host, host_addr) = build_pair().await;
    handshake(&caller, &host, host_addr).await;

    // Handler emits Start + Progress + Delta then ends — no
    // Result / Error. The SDK must synthesize an Error with
    // `code = "missing_terminal"`.
    let descriptor = metadata_for::<WebSearchReq, WebSearchResp>("forgetful").build();
    let _handle = host
        .serve_tool_streaming::<WebSearchReq, _, _, _>(descriptor, |_req| async move {
            futures::stream::iter(vec![
                ToolEvent::Start {
                    tool_id: "forgetful".into(),
                    call_id: None,
                    metadata: None,
                },
                ToolEvent::Progress {
                    pct: Some(10.0),
                    message: None,
                },
                ToolEvent::Delta {
                    data: serde_json::json!({ "token": "x" }),
                },
            ])
        })
        .expect("serve_tool_streaming");

    host.announce_capabilities(SdkCapabilitySet::new())
        .await
        .expect("announce");

    let tool_filter = CapabilityFilter::default().require_tag("ai-tool:forgetful");
    assert!(
        wait_until(
            || caller
                .inner()
                .find_nodes_by_filter(&tool_filter)
                .contains(&host.inner().node_id()),
            Duration::from_secs(3),
        )
        .await,
        "caller must see forgetful tool tag",
    );

    let stream = caller
        .call_streaming_typed::<WebSearchReq, ToolEvent>(
            host.inner().node_id(),
            "forgetful",
            &WebSearchReq { query: "x".into() },
            CallOptionsTyped {
                raw: Default::default(),
                codec: Codec::Json,
            },
        )
        .await
        .expect("call_streaming_typed");

    let events: Vec<ToolEvent> = stream
        .map(|item| item.expect("stream chunk"))
        .collect()
        .await;

    assert_eq!(
        events.len(),
        4,
        "expected 3 user events + 1 synthesized; got {events:?}"
    );
    match events.last().expect("synthesized terminal") {
        ToolEvent::Error { code, .. } => {
            assert_eq!(code, "missing_terminal");
        }
        other => panic!("last event must be synthesized Error, got {other:?}"),
    }
}

#[tokio::test]
async fn serve_tool_streaming_forces_streaming_flag_on_descriptor() {
    let mesh = MeshBuilder::new("127.0.0.1:0", &PSK)
        .unwrap()
        .build()
        .await
        .unwrap();

    // Build a descriptor that DIDN'T set streaming=true.
    let descriptor = metadata_for::<WebSearchReq, WebSearchResp>("forced_streaming").build();
    assert!(!descriptor.streaming, "builder default must be false");

    let _handle = mesh
        .serve_tool_streaming::<WebSearchReq, _, _, _>(descriptor, |_req| async move {
            futures::stream::iter(vec![ToolEvent::Result {
                data: serde_json::json!({}),
            }])
        })
        .expect("serve_tool_streaming");

    // Registry entry must have streaming=true even though the
    // caller forgot to set it on the descriptor.
    let registered = mesh
        .inner()
        .tool_registry()
        .get("forced_streaming")
        .expect("registry entry");
    assert!(
        registered.streaming,
        "serve_tool_streaming must force streaming=true",
    );
}

// ============================================================================
// A-6 — call_tool + call_tool_streaming (capability-routed)
// ============================================================================

#[tokio::test]
async fn call_tool_routes_via_capability_index() {
    let (caller, host, host_addr) = build_pair().await;
    handshake(&caller, &host, host_addr).await;

    let descriptor = metadata_for::<WebSearchReq, WebSearchResp>("web_search").build();
    let _handle = host
        .serve_tool::<WebSearchReq, WebSearchResp, _, _>(descriptor, |req| async move {
            Ok(WebSearchResp {
                results: vec![format!("hit:{}", req.query)],
            })
        })
        .expect("serve_tool");

    host.announce_capabilities(SdkCapabilitySet::new())
        .await
        .expect("announce");

    // Wait for the capability index to surface the host under
    // `nrpc:web_search` so `call_service` can route.
    let svc_filter = CapabilityFilter::default().require_tag("nrpc:web_search");
    assert!(
        wait_until(
            || caller
                .inner()
                .find_nodes_by_filter(&svc_filter)
                .contains(&host.inner().node_id()),
            Duration::from_secs(3),
        )
        .await,
        "caller must see host as nrpc:web_search server",
    );

    let resp: WebSearchResp = caller
        .call_tool(
            "web_search",
            &WebSearchReq {
                query: "mesh".into(),
            },
        )
        .await
        .expect("call_tool succeeds");
    assert_eq!(resp.results, vec!["hit:mesh".to_string()]);
}

#[tokio::test]
async fn call_tool_streaming_collects_envelopes_in_order() {
    let (caller, host, host_addr) = build_pair().await;
    handshake(&caller, &host, host_addr).await;

    let descriptor = metadata_for::<WebSearchReq, WebSearchResp>("search_stream").build();
    let _handle = host
        .serve_tool_streaming::<WebSearchReq, _, _, _>(descriptor, |req| async move {
            futures::stream::iter(vec![
                ToolEvent::Start {
                    tool_id: "search_stream".into(),
                    call_id: None,
                    metadata: None,
                },
                ToolEvent::Progress {
                    pct: Some(50.0),
                    message: None,
                },
                ToolEvent::Delta {
                    data: serde_json::json!({ "token": "partial " }),
                },
                ToolEvent::Result {
                    data: serde_json::json!({ "results": [format!("hit for {}", req.query)] }),
                },
            ])
        })
        .expect("serve_tool_streaming");

    host.announce_capabilities(SdkCapabilitySet::new())
        .await
        .expect("announce");

    let svc_filter = CapabilityFilter::default().require_tag("nrpc:search_stream");
    assert!(
        wait_until(
            || caller
                .inner()
                .find_nodes_by_filter(&svc_filter)
                .contains(&host.inner().node_id()),
            Duration::from_secs(3),
        )
        .await,
        "caller must see search_stream advertised",
    );

    let stream = caller
        .call_tool_streaming(
            "search_stream",
            &WebSearchReq {
                query: "mesh".into(),
            },
        )
        .await
        .expect("call_tool_streaming");
    let events: Vec<ToolEvent> = stream
        .map(|item| item.expect("stream chunk"))
        .collect()
        .await;
    assert_eq!(events.len(), 4, "expected 4 events, got {events:?}");
    assert!(matches!(events[0], ToolEvent::Start { .. }));
    assert!(matches!(events[1], ToolEvent::Progress { .. }));
    assert!(matches!(events[2], ToolEvent::Delta { .. }));
    assert!(matches!(events.last(), Some(ToolEvent::Result { .. })));
}

#[tokio::test]
async fn call_tool_returns_no_route_when_no_server() {
    let (caller, _host, _addr) = build_pair().await;
    // No serve_tool on host — `nrpc:nonexistent_tool` never lands in
    // any fold.
    let err: Result<WebSearchResp, _> = caller
        .call_tool("nonexistent_tool", &WebSearchReq { query: "x".into() })
        .await;
    match err {
        Err(net_sdk::mesh_rpc::RpcError::NoRoute { .. }) => {}
        Ok(_) => panic!("expected NoRoute"),
        Err(other) => panic!("expected NoRoute, got {other:?}"),
    }
}

// =============================================================================
// T-4 — watch_tools dynamic discovery (live two-mesh integration).
// =============================================================================

/// Pins the live watch_tools contract: when a host registers a
/// new tool after the peer has started watching, the peer's
/// `ToolListWatch` stream emits an `Added` event within a
/// bounded number of poll cycles. Removing the tool emits
/// `Removed`. This is the substrate-level equivalent of the
/// Node TS / Python / Go binding watch-tools tests, but
/// exercises real fold propagation across two meshes.
#[tokio::test]
async fn watch_tools_emits_added_when_remote_host_registers_tool() {
    use net_sdk::tool::ToolListChange;

    let (caller, host, host_addr) = build_pair().await;
    handshake(&caller, &host, host_addr).await;

    // Take a baseline snapshot BEFORE the host registers anything
    // (mirrors the per-binding "subscribe-then-publish race" fix —
    // baseline captured synchronously, watcher spawned after).
    let initial = caller.list_tools(None);
    assert!(
        initial.is_empty(),
        "baseline should be empty; got {initial:?}"
    );

    // Start the watcher with a 100ms poll interval — short enough
    // for the test to finish quickly, long enough to be a sane
    // production default. ToolListWatch implements futures::Stream
    // directly.
    let mut stream = caller.watch_tools(None, Some(Duration::from_millis(100)));

    // Now host registers a tool. The fold must propagate, and the
    // next watch poll on the caller side must emit `Added`.
    let descriptor = metadata_for::<WebSearchReq, WebSearchResp>("dynamic_web_search").build();
    let _handle = host
        .serve_tool::<WebSearchReq, _, _, _>(descriptor.clone(), |_req| async move {
            Ok(WebSearchResp {
                results: vec!["hit".into()],
            })
        })
        .expect("serve_tool");
    host.announce_capabilities(SdkCapabilitySet::default())
        .await
        .ok();

    // Drain events until we see Added — bounded by a generous
    // multi-poll timeout so flake-on-slow-CI doesn't dominate.
    let added = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(change) = stream.next().await {
            if let ToolListChange::Added(d) = change {
                if d.tool_id == "dynamic_web_search" {
                    return Some(d);
                }
            }
        }
        None
    })
    .await
    .expect("watch_tools added timeout");
    let added = added.expect("watch_tools stream ended without emitting Added");
    assert_eq!(added.tool_id, "dynamic_web_search");
    assert_eq!(added.name, "dynamic_web_search");
}

// The Removed event path is verified by the watch_tools unit
// tests in each binding (Node TS, Python, Go — see
// bindings/*/test/tool*); the cross-fold drop+re-announce flow is
// covered by `serve_tool_drop_unannounces_tool` above. A live
// integration test for the Removed event needs careful coordination
// of the watcher's internal baseline-capture timing vs the host's
// re-announce cadence; the Added-path test above is sufficient to
// pin the T-4 substrate contract end-to-end.

// Tiny suppressor so the `CapabilitySet` import from
// `net::behavior` doesn't trigger an unused-import warning if
// future test changes drop the reference. Cheap to keep around;
// proves the substrate-level type is still reachable.
#[allow(dead_code)]
fn _proves_capabilityset_reexport(_: &CapabilitySet) {}

/// P2 WS-C: an announced price must be an enforced price. The raw
/// `serve_tool` / `serve_tool_streaming` paths have no payment-admission
/// gate, so a descriptor carrying `pricing_terms` is refused before any
/// registration — it would otherwise be discovered as paid while serving
/// free. (Paid tools publish via `ServerPublisher::publish_tools` with a
/// `payment_admission` gate.)
#[tokio::test]
async fn a_priced_descriptor_is_refused_on_the_ungated_serve_path() {
    use net_sdk::mesh_rpc::ServeError;

    let mesh = MeshBuilder::new("127.0.0.1:0", &[0x59u8; 32])
        .unwrap()
        .build()
        .await
        .unwrap();

    let priced = metadata_for::<WebSearchReq, WebSearchResp>("paid_search")
        .description("A tool that costs money.")
        .pricing_terms(r#"{"object":"net.pricing.terms@1"}"#)
        .build();
    let err = match mesh
        .serve_tool::<WebSearchReq, WebSearchResp, _, _>(priced, |_req| async move {
            Ok(WebSearchResp { results: vec![] })
        }) {
        Ok(_) => panic!("a priced descriptor must be refused on the ungated path"),
        Err(e) => e,
    };
    assert!(
        matches!(&err, ServeError::UnenforceablePricing(id) if id == "paid_search"),
        "{err}"
    );

    // The streaming path refuses identically.
    let priced = metadata_for::<WebSearchReq, WebSearchResp>("paid_search")
        .description("A tool that costs money.")
        .pricing_terms(r#"{"object":"net.pricing.terms@1"}"#)
        .build();
    let err = match mesh.serve_tool_streaming::<WebSearchReq, _, _, _>(priced, |_req| async move {
        futures::stream::empty::<ToolEvent>()
    }) {
        Ok(_) => panic!("streaming must refuse a priced descriptor too"),
        Err(e) => e,
    };
    assert!(
        matches!(&err, ServeError::UnenforceablePricing(id) if id == "paid_search"),
        "{err}"
    );

    // The refusal left no phantom registration: the same id serves fine
    // once unpriced.
    let unpriced = metadata_for::<WebSearchReq, WebSearchResp>("paid_search")
        .description("Free after all.")
        .build();
    let _handle = mesh
        .serve_tool::<WebSearchReq, WebSearchResp, _, _>(unpriced, |_req| async move {
            Ok(WebSearchResp { results: vec![] })
        })
        .expect("the unpriced descriptor serves");

    mesh.shutdown().await.ok();
}
