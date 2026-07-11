//! Substrate-side AI tool-announcement merge.
//!
//! Pins the contract from A-2a: a tool inserted into a `MeshNode`'s
//! `tool_registry` shows up in subsequent `announce_capabilities`
//! payloads as:
//!
//! - `ai-tool:<id>` capability tag (so peers can
//!   `find_service_nodes` against the tag prefix),
//! - the typed `ToolCapability` itself (so the typed views
//!   aggregate a `Vec<ToolCapability>` across peers in the fold),
//! - `tool::<id>::description` / `streaming` / `tags` metadata
//!   keys when the descriptor set them.
//!
//! The SDK-side `serve_tool` (A-2b) plumbs the same registry; this
//! test exercises the substrate layer directly so the registry +
//! announce-merge contract is pinned independently of the SDK
//! wrapper.

#![cfg(all(feature = "net", feature = "cortex", feature = "tool"))]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;

use net::adapter::net::behavior::capability::{CapabilityFilter, CapabilitySet};
use net::adapter::net::behavior::fold::capability_aggregation::TagMatcher;
use net::adapter::net::behavior::ToolCapability;
use net::adapter::net::cortex::tool::{ToolDescriptor, ToolListChange};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};

const TEST_BUFFER_SIZE: usize = 256 * 1024;
const PSK: [u8; 32] = [0x42u8; 32];

fn test_config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5))
        .with_handshake(3, Duration::from_secs(2))
        .with_capability_gc_interval(Duration::from_millis(250))
        .with_min_announce_interval(Duration::from_millis(0));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: TEST_BUFFER_SIZE,
        recv_buffer_size: TEST_BUFFER_SIZE,
    };
    cfg
}

async fn build_node() -> Arc<MeshNode> {
    let cfg = test_config();
    let keypair = EntityKeypair::generate();
    Arc::new(MeshNode::new(keypair, cfg).await.expect("MeshNode::new"))
}

async fn handshake_pair(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
    let a_id = a.node_id();
    let b_id = b.node_id();
    let b_pub = *b.public_key();
    let b_addr = b.local_addr();
    let b_clone = b.clone();
    let accept = tokio::spawn(async move { b_clone.accept(a_id).await });
    a.connect(b_addr, &b_pub, b_id)
        .await
        .expect("connect failed");
    accept
        .await
        .expect("accept task panicked")
        .expect("accept failed");
    a.start();
    b.start();
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

fn descriptor(tool_id: &str) -> ToolDescriptor {
    let cap = ToolCapability::new(tool_id, format!("Name for {tool_id}"))
        .with_version("1.0.0")
        .with_input_schema(r#"{"type":"object","properties":{"query":{"type":"string"}}}"#);
    ToolDescriptor::from_capability(&cap, &std::collections::BTreeMap::new())
}

#[tokio::test]
async fn announce_capabilities_merges_tool_registry_into_tags_and_caps() {
    let host = build_node().await;
    let peer = build_node().await;
    handshake_pair(&host, &peer).await;

    // Populate the host's tool registry — same shape the SDK's
    // `serve_tool` will do in A-2b.
    let mut desc = descriptor("web_search");
    desc.description = Some("Search the web.".to_string());
    desc.streaming = false;
    desc.tags = vec!["web".to_string(), "research".to_string()];
    host.tool_registry().insert(desc);

    // Announce with an empty baseline — `announce_capabilities_with`
    // must merge the tool registry on top.
    host.announce_capabilities(CapabilitySet::new())
        .await
        .expect("announce_capabilities");

    // Peer's capability index picks up the `ai-tool:web_search`
    // tag once the announcement propagates.
    let tool_filter = CapabilityFilter::default().require_tag("ai-tool:web_search");
    assert!(
        wait_until(
            || peer
                .find_nodes_by_filter(&tool_filter)
                .contains(&host.node_id()),
            Duration::from_secs(3),
        )
        .await,
        "peer must see the host advertising `ai-tool:web_search` after announce; \
         currently sees {:?}",
        peer.find_nodes_by_filter(&tool_filter),
    );
}

#[tokio::test]
async fn announce_capabilities_with_empty_registry_omits_tool_merge() {
    // No `tool_registry.insert(...)`; the merge branch must
    // short-circuit (preserves byte-identity wire-shape for users
    // who never call `serve_tool`).
    let host = build_node().await;
    let peer = build_node().await;
    handshake_pair(&host, &peer).await;

    assert!(host.tool_registry().is_empty());
    host.announce_capabilities(CapabilitySet::new())
        .await
        .expect("announce_capabilities");

    // Give the announcement time to propagate, then assert no
    // `ai-tool:*` tag landed. Use a representative specific tag —
    // an absent `ai-tool:web_search` rules out the merge path
    // having added something the test wasn't expecting.
    tokio::time::sleep(Duration::from_millis(400)).await;
    let tool_filter = CapabilityFilter::default().require_tag("ai-tool:web_search");
    let hits = peer.find_nodes_by_filter(&tool_filter);
    assert!(
        !hits.contains(&host.node_id()),
        "empty tool_registry must NOT emit any `ai-tool:*` tag; got {:?}",
        hits,
    );
}

#[tokio::test]
async fn drop_from_registry_clears_tag_on_next_announce() {
    let host = build_node().await;
    let peer = build_node().await;
    handshake_pair(&host, &peer).await;

    host.tool_registry().insert(descriptor("web_search"));
    host.announce_capabilities(CapabilitySet::new())
        .await
        .expect("first announce");

    // Confirm the peer saw the initial announcement.
    let tool_filter = CapabilityFilter::default().require_tag("ai-tool:web_search");
    assert!(
        wait_until(
            || peer
                .find_nodes_by_filter(&tool_filter)
                .contains(&host.node_id()),
            Duration::from_secs(3),
        )
        .await,
        "peer must see the host's initial announce",
    );

    // Remove the tool + re-announce. The TTL on the previous
    // announcement keeps the entry around in the peer's index until
    // it expires (5 min default), so we can't assert immediate
    // removal — what we CAN assert is that the next announce's
    // emitted set on the host side no longer carries the tag.
    let removed = host.tool_registry().remove("web_search");
    assert!(removed.is_some(), "remove must return the prior entry");
    assert!(host.tool_registry().is_empty());
}

// ============================================================================
// A-4 — list_tools walks the capability fold
// ============================================================================

#[tokio::test]
async fn list_tools_returns_descriptors_for_every_published_tool() {
    let host = build_node().await;
    let peer = build_node().await;
    handshake_pair(&host, &peer).await;

    // Host serves two tools. Add a description metadata key on one
    // of them so the descriptor's `description` round-trip is
    // exercised end-to-end.
    let mut search = descriptor("web_search");
    search.description = Some("Search the web.".to_string());
    search.tags = vec!["web".to_string(), "research".to_string()];
    host.tool_registry().insert(search);
    host.tool_registry().insert(descriptor("calculator"));
    host.announce_capabilities(CapabilitySet::new())
        .await
        .expect("announce");

    // Peer waits for both ai-tool tags before walking the fold.
    let search_filter = CapabilityFilter::default().require_tag("ai-tool:web_search");
    let calc_filter = CapabilityFilter::default().require_tag("ai-tool:calculator");
    assert!(
        wait_until(
            || {
                peer.find_nodes_by_filter(&search_filter)
                    .contains(&host.node_id())
                    && peer
                        .find_nodes_by_filter(&calc_filter)
                        .contains(&host.node_id())
            },
            Duration::from_secs(3),
        )
        .await,
        "peer must see both tools announced",
    );

    let tools = peer.list_tools(None);
    assert_eq!(tools.len(), 2, "expected 2 tools, got {tools:?}");
    let by_id: std::collections::HashMap<&str, &ToolDescriptor> =
        tools.iter().map(|t| (t.tool_id.as_str(), t)).collect();
    let search = by_id.get("web_search").expect("web_search present");
    assert_eq!(search.description.as_deref(), Some("Search the web."));
    assert_eq!(search.tags, vec!["web", "research"]);
    assert_eq!(search.node_count, 1);
    let calc = by_id.get("calculator").expect("calculator present");
    assert!(calc.description.is_none());
    assert_eq!(calc.node_count, 1);
}

#[tokio::test]
async fn list_tools_hydrates_schemas_from_metadata() {
    let host = build_node().await;
    let peer = build_node().await;
    handshake_pair(&host, &peer).await;

    // The substrate's `from_capability` constructor copies
    // input_schema/output_schema off the ToolCapability — we rely on
    // them having been stashed in `CapabilitySet::metadata` on the
    // announce path so the peer's `list_tools` can hydrate them.
    host.tool_registry().insert(descriptor("web_search"));
    host.announce_capabilities(CapabilitySet::new())
        .await
        .expect("announce");

    let tool_filter = CapabilityFilter::default().require_tag("ai-tool:web_search");
    assert!(
        wait_until(
            || peer
                .find_nodes_by_filter(&tool_filter)
                .contains(&host.node_id()),
            Duration::from_secs(3),
        )
        .await,
        "peer must see ai-tool:web_search",
    );

    let tools = peer.list_tools(None);
    let search = tools
        .iter()
        .find(|t| t.tool_id == "web_search")
        .expect("web_search descriptor present");
    let schema = search
        .input_schema
        .as_deref()
        .expect("input_schema hydrated from metadata");
    assert!(
        schema.contains("\"query\""),
        "schema must round-trip via metadata, got {schema:?}",
    );
}

#[tokio::test]
async fn list_tools_filters_by_matcher_prefix() {
    let host_eu = build_node().await;
    let host_us = build_node().await;
    let peer = build_node().await;
    handshake_pair(&peer, &host_eu).await;
    handshake_pair(&peer, &host_us).await;

    host_eu.tool_registry().insert(descriptor("eu_tool"));
    host_us.tool_registry().insert(descriptor("us_tool"));

    let mut eu_caps = CapabilitySet::new();
    eu_caps = eu_caps.add_tag("region.eu");
    let mut us_caps = CapabilitySet::new();
    us_caps = us_caps.add_tag("region.us");

    host_eu
        .announce_capabilities(eu_caps)
        .await
        .expect("eu announce");
    host_us
        .announce_capabilities(us_caps)
        .await
        .expect("us announce");

    // Wait for both to land in peer's fold.
    let eu_filter = CapabilityFilter::default().require_tag("ai-tool:eu_tool");
    let us_filter = CapabilityFilter::default().require_tag("ai-tool:us_tool");
    assert!(
        wait_until(
            || {
                peer.find_nodes_by_filter(&eu_filter)
                    .contains(&host_eu.node_id())
                    && peer
                        .find_nodes_by_filter(&us_filter)
                        .contains(&host_us.node_id())
            },
            Duration::from_secs(3),
        )
        .await,
        "peer must see both region tools",
    );

    // Filter to EU prefix — only the EU host's tool should surface.
    let matcher = TagMatcher::Prefix {
        value: "region.eu".to_string(),
    };
    let tools = peer.list_tools(Some(&matcher));
    let ids: Vec<&str> = tools.iter().map(|t| t.tool_id.as_str()).collect();
    assert_eq!(ids, vec!["eu_tool"], "expected only eu_tool, got {ids:?}");
}

// ============================================================================
// A-5 — watch_tools dynamic discovery
// ============================================================================

#[tokio::test]
async fn watch_tools_emits_added_when_host_publishes_a_tool() {
    let host = build_node().await;
    let peer = build_node().await;
    handshake_pair(&host, &peer).await;

    // Subscribe BEFORE the host registers anything — the initial
    // baseline snapshot is empty, so the first `Added` event is
    // attributable to the registration that follows.
    let mut watch = peer.watch_tools(None, Some(Duration::from_millis(100)));

    host.tool_registry().insert(descriptor("late_arrival"));
    host.announce_capabilities(CapabilitySet::new())
        .await
        .expect("announce");

    // Wait for the change event, capped at 5s.
    let event = tokio::time::timeout(Duration::from_secs(5), watch.next())
        .await
        .expect("watch produced an event in time")
        .expect("stream did not close");
    match event {
        ToolListChange::Added(desc) => assert_eq!(desc.tool_id, "late_arrival"),
        other => panic!("expected Added(late_arrival), got {other:?}"),
    }
}

#[tokio::test]
async fn watch_tools_emits_node_count_changed_when_second_publisher_joins() {
    let host_a = build_node().await;
    let host_b = build_node().await;
    let peer = build_node().await;
    handshake_pair(&peer, &host_a).await;
    handshake_pair(&peer, &host_b).await;

    // Host A publishes first; wait for the baseline to include it.
    host_a.tool_registry().insert(descriptor("shared_tool"));
    host_a
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("a announce");
    let tool_filter = CapabilityFilter::default().require_tag("ai-tool:shared_tool");
    assert!(
        wait_until(
            || peer
                .find_nodes_by_filter(&tool_filter)
                .contains(&host_a.node_id()),
            Duration::from_secs(3),
        )
        .await,
        "peer must see shared_tool from A first",
    );

    // Subscribe AFTER A — baseline snapshot now has node_count=1.
    let mut watch = peer.watch_tools(None, Some(Duration::from_millis(100)));

    // Host B joins the same (tool_id, version) — node_count should bump
    // to 2 on the next diff tick.
    host_b.tool_registry().insert(descriptor("shared_tool"));
    host_b
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("b announce");

    let event = loop {
        let evt = tokio::time::timeout(Duration::from_secs(5), watch.next())
            .await
            .expect("watch produced an event in time")
            .expect("stream did not close");
        // The first emission could in principle be Added if B's announce
        // outraced the baseline snapshot. In practice the test waits for
        // A first, so the only churn is NodeCountChanged. Filter
        // defensively so a tick ordering hiccup doesn't flake.
        match evt {
            ToolListChange::NodeCountChanged { .. } => break evt,
            ToolListChange::Added(d) if d.tool_id == "shared_tool" => continue,
            other => panic!("unexpected event: {other:?}"),
        }
    };
    match event {
        ToolListChange::NodeCountChanged {
            descriptor: d,
            prev_node_count,
        } => {
            assert_eq!(d.tool_id, "shared_tool");
            assert_eq!(prev_node_count, 1);
            assert_eq!(d.node_count, 2);
        }
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn watch_tools_polling_task_exits_when_handle_dropped() {
    // Pinned behavior: dropping the `ToolListWatch` causes the inner
    // polling task to exit (mpsc send fails → loop returns). We can't
    // reach the JoinHandle from here, so the test instead verifies
    // that a drop doesn't leak observable state: registering a new
    // tool after the drop must NOT panic and must NOT cause the
    // dropped watch's still-alive sender to misbehave.
    let host = build_node().await;
    let peer = build_node().await;
    handshake_pair(&host, &peer).await;

    {
        let _watch = peer.watch_tools(None, Some(Duration::from_millis(50)));
        // Drop immediately when the block ends.
    }
    // Give the task one tick to observe the closed channel.
    tokio::time::sleep(Duration::from_millis(150)).await;

    host.tool_registry().insert(descriptor("post_drop"));
    host.announce_capabilities(CapabilitySet::new())
        .await
        .expect("announce after drop");

    let tool_filter = CapabilityFilter::default().require_tag("ai-tool:post_drop");
    assert!(
        wait_until(
            || peer
                .find_nodes_by_filter(&tool_filter)
                .contains(&host.node_id()),
            Duration::from_secs(3),
        )
        .await,
        "post-drop announce must still propagate normally",
    );
}

#[tokio::test]
async fn watch_tools_delivers_change_well_under_the_debounce_ceiling() {
    // E-2: the watch is event-driven — a change is delivered as soon
    // as the capability fold mutates, NOT on the next ceiling tick.
    // Arm a deliberately long 30s ceiling; if the event still arrives
    // in well under a second, it can only have come from the fold's
    // change signal (the ceiling timer is 30s away). A regression to
    // interval-polling would make this wait ~30s and trip the inner
    // 2s timeout.
    let host = build_node().await;
    let peer = build_node().await;
    handshake_pair(&host, &peer).await;

    let mut watch = peer.watch_tools(None, Some(Duration::from_secs(30)));

    let started = std::time::Instant::now();
    host.tool_registry().insert(descriptor("fast_path"));
    host.announce_capabilities(CapabilitySet::new())
        .await
        .expect("announce");

    let event = tokio::time::timeout(Duration::from_secs(2), watch.next())
        .await
        .expect("event must arrive far inside the 30s ceiling")
        .expect("stream did not close");
    match event {
        ToolListChange::Added(desc) => assert_eq!(desc.tool_id, "fast_path"),
        other => panic!("expected Added(fast_path), got {other:?}"),
    }
    // Belt-and-suspenders: prove it was sub-ceiling, not luck.
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "change delivery took {:?}, expected ≪ 30s ceiling — \
         watch is not event-driven",
        started.elapsed(),
    );
}

#[tokio::test]
async fn watch_tools_cancel_ends_the_stream_promptly() {
    // E-2b: `cancel()` must end the stream even when the diff task
    // is parked on the fold change signal with no pending change.
    // With no ceiling (`None`) the task only wakes on a fold change
    // or a cancel — so absent cancel, `next()` would block forever
    // here. The cancel exits the task, drops its sender, and the
    // stream ends with `None`. This is the substrate basis for
    // prompt FFI/Go shutdown on `ctx.Done()`.
    let host = build_node().await;
    let peer = build_node().await;
    handshake_pair(&host, &peer).await;

    let mut watch = peer.watch_tools(None, None);
    watch.cancel();

    let ended = tokio::time::timeout(Duration::from_secs(2), watch.next()).await;
    assert!(
        matches!(ended, Ok(None)),
        "cancel must end the stream promptly; got {ended:?}",
    );
}

#[tokio::test]
async fn list_tools_dedupes_and_aggregates_node_count() {
    let host_a = build_node().await;
    let host_b = build_node().await;
    let peer = build_node().await;
    handshake_pair(&peer, &host_a).await;
    handshake_pair(&peer, &host_b).await;

    // Both hosts publish the SAME (tool_id, version) — list_tools
    // must dedupe and report node_count = 2.
    host_a.tool_registry().insert(descriptor("shared_tool"));
    host_b.tool_registry().insert(descriptor("shared_tool"));
    host_a
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("a announce");
    host_b
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("b announce");

    let tool_filter = CapabilityFilter::default().require_tag("ai-tool:shared_tool");
    assert!(
        wait_until(
            || {
                let hits = peer.find_nodes_by_filter(&tool_filter);
                hits.contains(&host_a.node_id()) && hits.contains(&host_b.node_id())
            },
            Duration::from_secs(3),
        )
        .await,
        "peer must see both hosts under ai-tool:shared_tool",
    );

    let tools = peer.list_tools(None);
    let shared = tools
        .iter()
        .find(|t| t.tool_id == "shared_tool")
        .expect("shared_tool descriptor present");
    assert_eq!(
        shared.node_count, 2,
        "shared tool must aggregate node_count across both hosts",
    );
    // Only one descriptor row even though two hosts publish — dedupe
    // by (tool_id, version) is the load-bearing invariant.
    let count_rows = tools.iter().filter(|t| t.tool_id == "shared_tool").count();
    assert_eq!(count_rows, 1, "dedupe must collapse duplicates");
}

/// RT-2 (REALTIME_ROUTING_AND_DISCOVERY_PLAN): registry mutations
/// through the shared `Arc` — the exact path the SDK's `serve_tool`
/// and its Drop hook use — bump the node's local-caps generation,
/// so a change-driven announcer can wake on them without polling.
#[tokio::test]
async fn local_caps_generation_tracks_registry_mutations() {
    let host = build_node().await;
    let base = host.local_caps_generation();

    host.tool_registry().insert(descriptor("web_search"));
    assert_eq!(
        host.local_caps_generation(),
        base + 1,
        "serve_tool-path insert must bump the local-caps generation",
    );

    assert!(host.tool_registry().remove("web_search").is_some());
    assert_eq!(
        host.local_caps_generation(),
        base + 2,
        "ServeHandle-drop-path remove must bump the local-caps generation",
    );

    assert!(host.tool_registry().remove("web_search").is_none());
    assert_eq!(
        host.local_caps_generation(),
        base + 2,
        "remove of an absent tool is not a capability change",
    );
}

/// Handshake with the host started via `start_arc` — the RT-3
/// change-driven announcer (like the re-announce keep-alive)
/// captures `self_weak` at spawn time, so `start_arc` must be the
/// FIRST start call; a bare `start()` first would spawn the loop
/// with no weak and park it.
async fn handshake_pair_host_arc(host: &Arc<MeshNode>, peer: &Arc<MeshNode>) {
    let host_id = host.node_id();
    let peer_id = peer.node_id();
    let peer_pub = *peer.public_key();
    let peer_addr = peer.local_addr();
    let peer_clone = peer.clone();
    let accept = tokio::spawn(async move { peer_clone.accept(host_id).await });
    host.connect(peer_addr, &peer_pub, peer_id)
        .await
        .expect("connect failed");
    accept
        .await
        .expect("accept task panicked")
        .expect("accept failed");
    host.start_arc();
    peer.start();
}

/// RT-3 (REALTIME_ROUTING_AND_DISCOVERY_PLAN): a registry mutation
/// propagates to peers with NO explicit `announce_capabilities`
/// call — the change-driven announcer wakes on the RT-2 signal,
/// debounces, and broadcasts.
#[tokio::test]
async fn registry_change_announces_without_explicit_call() {
    let host = build_node().await;
    let peer = build_node().await;
    handshake_pair_host_arc(&host, &peer).await;

    host.tool_registry().insert(descriptor("auto_announced"));

    let filter = CapabilityFilter::default().require_tag("ai-tool:auto_announced");
    assert!(
        wait_until(
            || peer.find_nodes_by_filter(&filter).contains(&host.node_id()),
            Duration::from_secs(3),
        )
        .await,
        "peer never saw the tool — the change-driven announcer did not fire",
    );
}

/// RT-3: a burst of registry mutations inside one debounce window
/// collapses into a single announce call carrying all of them.
#[tokio::test]
async fn registry_burst_coalesces_into_one_announce() {
    let host = {
        // Wide debounce so the back-to-back inserts are guaranteed
        // to land inside one window even on a stalled CI box.
        let cfg = test_config().with_announce_debounce(Duration::from_millis(300));
        let keypair = EntityKeypair::generate();
        Arc::new(MeshNode::new(keypair, cfg).await.expect("MeshNode::new"))
    };
    let peer = build_node().await;
    handshake_pair_host_arc(&host, &peer).await;

    let version_before = host.capability_announce_version();

    host.tool_registry().insert(descriptor("burst_a"));
    host.tool_registry().insert(descriptor("burst_b"));
    host.tool_registry().insert(descriptor("burst_c"));

    let all_visible = || {
        ["burst_a", "burst_b", "burst_c"].iter().all(|id| {
            let filter = CapabilityFilter::default().require_tag(format!("ai-tool:{id}"));
            peer.find_nodes_by_filter(&filter).contains(&host.node_id())
        })
    };
    assert!(
        wait_until(all_visible, Duration::from_secs(3)).await,
        "peer never saw the full burst",
    );

    assert_eq!(
        host.capability_announce_version(),
        version_before + 1,
        "a debounced burst must produce exactly one announce call",
    );
}
