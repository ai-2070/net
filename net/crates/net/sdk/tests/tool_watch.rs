//! Rust SDK integration test for `Mesh::watch_tools` (E-6).
//!
//! Proves the SDK surface is the event-driven substrate watch, not a
//! re-poll: a single node serving its own tool fires an `Added` to a
//! local watcher off the capability fold's change signal (the self-
//! index path applies to the local fold), and `cancel()` ends a
//! ceiling-less stream promptly. Single-node — no peer handshake — so
//! it runs everywhere, unlike the mesh-pair round-trip tests.

#![cfg(all(feature = "tool", feature = "cortex"))]

use std::time::Duration;

use futures::StreamExt;

use net_sdk::capabilities::CapabilitySet as SdkCapabilitySet;
use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::tool::{metadata_for, ToolListChange};
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

async fn build_node() -> Mesh {
    MeshBuilder::new("127.0.0.1:0", &PSK)
        .unwrap()
        .build()
        .await
        .unwrap()
}

#[tokio::test]
async fn watch_tools_delivers_self_served_tool_under_the_ceiling() {
    // Arm a deliberately long 30s ceiling. If the Added still lands in
    // well under a second, it came from the fold change signal, not a
    // ceiling tick — i.e. the SDK method is the event-driven substrate
    // watch and not a re-poll. A regression to interval polling would
    // wait ~30s and trip the inner 2s timeout.
    let node = build_node().await;

    let mut watch = node.watch_tools(None, Some(Duration::from_secs(30)));

    let descriptor = metadata_for::<WebSearchReq, WebSearchResp>("web_search")
        .description("Search the web.")
        .build();
    let _handle = node
        .serve_tool::<WebSearchReq, WebSearchResp, _, _>(descriptor, |req| async move {
            Ok(WebSearchResp {
                results: vec![format!("result for {}", req.query)],
            })
        })
        .expect("serve_tool");

    let started = std::time::Instant::now();
    node.announce_capabilities(SdkCapabilitySet::new())
        .await
        .expect("announce");

    let event = tokio::time::timeout(Duration::from_secs(2), watch.next())
        .await
        .expect("event must arrive far inside the 30s ceiling")
        .expect("stream did not close");
    match event {
        ToolListChange::Added(desc) => assert_eq!(desc.tool_id, "web_search"),
        other => panic!("expected Added(web_search), got {other:?}"),
    }
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "delivery took {:?}, expected ≪ 30s ceiling — watch is not event-driven",
        started.elapsed(),
    );
}

#[tokio::test]
async fn watch_tools_cancel_ends_a_ceilingless_stream() {
    // With no ceiling, the substrate task only wakes on a fold change
    // or a cancel. Absent cancel, `next()` would block forever here.
    // `ToolListWatch::cancel` exits the task, drops its sender, and the
    // stream ends with `None` promptly.
    let node = build_node().await;

    let mut watch = node.watch_tools(None, None);
    watch.cancel();

    let ended = tokio::time::timeout(Duration::from_secs(2), watch.next()).await;
    assert!(
        matches!(ended, Ok(None)),
        "cancel must end the stream promptly; got {ended:?}",
    );
}
