//! End-to-end integration test for the Phase C `fold.query`
//! RPC service.
//!
//! Two `MeshNode`s — one hosting an `AggregatorDaemon` with the
//! query service installed, one acting as the querier via
//! `FoldQueryClient`. Pins:
//!
//! 1. Successful round-trip of a `LatestSummary` query.
//! 2. `SummarizeNow` forces a fresh tick on the host side.
//! 3. Unknown-kind requests return a typed `Server` error.
//! 4. Client cache short-circuits a second identical call.
//!
//! Run: `cargo test --features net --test aggregator_fold_query`

#![cfg(feature = "net")]

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::aggregator::{
    AggregatorConfig, AggregatorDaemon, FoldQueryClient, FoldQueryClientError, FoldQueryError,
    SummaryAnnouncement,
};
use net::adapter::net::behavior::fold::capability::{CapabilityFold, CapabilityMembership};
use net::adapter::net::behavior::fold::wire::SignedAnnouncement;
use net::adapter::net::behavior::fold::{EnvelopeMeta, FoldKind, NodeState};
use net::adapter::net::{
    ChannelConfig, ChannelConfigRegistry, ChannelId, ChannelName, ChannelPublisher, OnFailure,
    PublishConfig, Reliability, Visibility,
};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig, SubnetId};
// `ChannelPublisher` / `OnFailure` / `Reliability` are also imported through
// the `behavior::aggregator` umbrella above; the second `use` keeps the
// integration test's flat name surface readable.
#[allow(unused_imports)]
use net::adapter::net as _net;

const TEST_BUFFER_SIZE: usize = 256 * 1024;
const PSK: [u8; 32] = [0x42u8; 32];

fn test_config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5))
        .with_handshake(3, Duration::from_secs(2))
        .with_capability_gc_interval(Duration::from_millis(250));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: TEST_BUFFER_SIZE,
        recv_buffer_size: TEST_BUFFER_SIZE,
    };
    cfg
}

async fn build_node() -> Arc<MeshNode> {
    let keypair = EntityKeypair::generate();
    let node = MeshNode::new(keypair, test_config())
        .await
        .expect("MeshNode::new");
    Arc::new(node)
}

async fn handshake(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
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

fn sign_cap(
    kp: &EntityKeypair,
    publisher: u64,
    class: u64,
    state: NodeState,
) -> SignedAnnouncement<CapabilityMembership> {
    SignedAnnouncement::sign(
        kp,
        CapabilityFold::KIND_ID,
        class,
        publisher,
        1,
        EnvelopeMeta::default(),
        CapabilityMembership {
            class_hash: class,
            tags: Vec::new(),
            hardware: None,
            state,
            region: None,
            price_quote: None,
            reflex_addr: None,
            noise_pubkey: None,
            rtc_bootstrap: None,
            rtc_addr: None,
            rtc_stun_addr: None,
            allowed_nodes: Vec::new(),
            allowed_subnets: Vec::new(),
            allowed_groups: Vec::new(),
            metadata: BTreeMap::new(),
            owner: None,
        },
    )
    .expect("sign")
}

/// Build a two-node mesh: `host` runs an AggregatorDaemon with
/// the query service installed; `querier` connects to it. Returns
/// `(host, querier, agg, _serve_handle)`. Hold the serve handle —
/// dropping it tears down the service registration.
async fn build_aggregator_pair() -> (
    Arc<MeshNode>,
    Arc<MeshNode>,
    Arc<AggregatorDaemon>,
    net::adapter::net::mesh_rpc::ServeHandle,
) {
    let host = build_node().await;
    let querier = build_node().await;
    handshake(&host, &querier).await;

    // Prime the host's capability fold with three publishers in
    // mixed states so the summary has visibly non-zero buckets.
    let kp = EntityKeypair::generate();
    let fold = host.capability_fold();
    fold.apply(sign_cap(&kp, 0xA, 1, NodeState::Idle)).unwrap();
    fold.apply(sign_cap(&kp, 0xB, 2, NodeState::Idle)).unwrap();
    fold.apply(sign_cap(&kp, 0xC, 3, NodeState::Busy)).unwrap();

    let cfg = AggregatorConfig::new(SubnetId::new(&[3, 7]))
        .with_fold_kind(CapabilityFold::KIND_ID)
        .with_interval(Duration::from_secs(60));
    let agg = Arc::new(AggregatorDaemon::new(cfg, host.clone()).expect("new aggregator"));
    // Run one tick synchronously so the latest-summaries buffer
    // is primed — `LatestSummary` queries read from it directly.
    agg.tick_once();
    let serve_handle = agg
        .install_query_service(&host)
        .expect("install_query_service");

    (host, querier, agg, serve_handle)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn latest_summary_round_trips_across_handshake() {
    let (host, querier, _agg, _serve) = build_aggregator_pair().await;
    let host_node_id = host.node_id();
    let client = FoldQueryClient::new(querier).with_deadline(Duration::from_secs(2));

    let summaries = client
        .query_latest(host_node_id, CapabilityFold::KIND_ID)
        .await
        .expect("query_latest");
    assert_eq!(summaries.len(), 1, "expected one summary row");
    let summary = &summaries[0];
    assert_eq!(summary.fold_kind, CapabilityFold::KIND_ID);
    assert_eq!(summary.source_subnet, SubnetId::new(&[3, 7]));
    let idle = summary
        .buckets
        .iter()
        .find(|(n, _)| n == "idle")
        .map(|(_, c)| *c)
        .unwrap_or(0);
    let busy = summary
        .buckets
        .iter()
        .find(|(n, _)| n == "busy")
        .map(|(_, c)| *c)
        .unwrap_or(0);
    assert_eq!(idle, 2, "two idle publishers primed");
    assert_eq!(busy, 1, "one busy publisher primed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn summarize_now_forces_fresh_tick_on_host() {
    let (host, querier, agg, _serve) = build_aggregator_pair().await;
    let host_node_id = host.node_id();
    let client = FoldQueryClient::new(querier).with_deadline(Duration::from_secs(2));

    let before = agg.generation();
    let _ = client
        .query_summarize_now(host_node_id, CapabilityFold::KIND_ID)
        .await
        .expect("query_summarize_now");
    let after = agg.generation();
    assert!(
        after > before,
        "generation must advance after SummarizeNow (before={before}, after={after})"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unknown_kind_returns_server_error() {
    let (host, querier, _agg, _serve) = build_aggregator_pair().await;
    let host_node_id = host.node_id();
    let client = FoldQueryClient::new(querier).with_deadline(Duration::from_secs(2));

    let result = client.query_latest(host_node_id, 0xDEAD).await;
    match result {
        Err(FoldQueryClientError::Server(FoldQueryError::UnknownKind { kind })) => {
            assert_eq!(kind, 0xDEAD);
        }
        Err(other) => panic!("expected Server(UnknownKind), got {other:?}"),
        Ok(s) => panic!("expected error, got {s:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wire_publish_summary_reaches_subscriber_on_remote_node() {
    // Pin the Phase B slice 3 wire-publish path end-to-end:
    // - Host installs a channel registry + an aggregator that
    //   registers its summary channels with `Visibility::Global`.
    // - Querier subscribes to `summary/0x0001` on the host.
    // - Host's aggregator publishes; the substrate fan-out
    //   reports a non-zero delivered count, proving the wire
    //   path admitted the subscriber.
    let summary_channel = ChannelName::new("summary/0x0001").expect("channel name");
    let channel_cfg = || {
        ChannelConfig::new(ChannelId::parse(summary_channel.as_str()).expect("id"))
            .with_visibility(Visibility::Global)
    };

    // Both nodes need the channel in their registry so the
    // subscribe-gate + publish-fanout visibility checks pass.
    let host = {
        let mut node = MeshNode::new(EntityKeypair::generate(), test_config())
            .await
            .expect("MeshNode::new host");
        let registry = Arc::new(ChannelConfigRegistry::new());
        registry.insert(channel_cfg());
        node.set_channel_configs(registry);
        Arc::new(node)
    };
    let querier = {
        let mut node = MeshNode::new(EntityKeypair::generate(), test_config())
            .await
            .expect("MeshNode::new querier");
        let registry = Arc::new(ChannelConfigRegistry::new());
        registry.insert(channel_cfg());
        node.set_channel_configs(registry);
        Arc::new(node)
    };

    handshake(&host, &querier).await;

    // Prime the host's capability fold.
    let kp = EntityKeypair::generate();
    let fold = host.capability_fold();
    fold.apply(sign_cap(&kp, 0xA, 1, NodeState::Idle)).unwrap();

    // Build + install the aggregator on the host.
    let cfg = AggregatorConfig::new(SubnetId::new(&[3]))
        .with_fold_kind(CapabilityFold::KIND_ID)
        .with_visibility(Visibility::Global);
    let agg = Arc::new(AggregatorDaemon::new(cfg, host.clone()).expect("new"));
    agg.register_summary_channels().expect("register channels");

    // Querier subscribes to the host's summary channel.
    querier
        .subscribe_channel(host.node_id(), summary_channel.clone())
        .await
        .expect("subscribe");

    // Give the subscribe a tick to propagate through the
    // membership-ack path so the host's roster has the querier
    // before the publish fan-out.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Tick + publish.
    let published = agg.tick_and_publish().await.expect("tick_and_publish");
    assert_eq!(published, 1, "one summary should publish");

    // Sanity: also verify via a direct publish that the
    // subscriber is on the roster — the aggregator's internal
    // publish goes through `mesh.publish`, which uses the same
    // fan-out path. A `delivered > 0` here proves the channel is
    // wired correctly even if the aggregator's report isn't
    // surfaced.
    let publisher = ChannelPublisher::new(
        summary_channel.clone(),
        PublishConfig {
            reliability: Reliability::FireAndForget,
            on_failure: OnFailure::BestEffort,
            max_inflight: 16,
        },
    );
    let test_payload = postcard::to_allocvec(&SummaryAnnouncement {
        source_subnet: SubnetId::new(&[3]),
        fold_kind: CapabilityFold::KIND_ID,
        generation: 999,
        buckets: vec![("idle".to_string(), 1)],
    })
    .expect("encode");
    let report = host
        .publish(&publisher, bytes::Bytes::from(test_payload))
        .await
        .expect("publish");
    assert_eq!(report.attempted, 1, "querier should be the only subscriber");
    assert!(
        report.delivered >= 1,
        "subscriber on roster should receive payload (got delivered={})",
        report.delivered,
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cache_hit_short_circuits_second_identical_call() {
    let (host, querier, agg, _serve) = build_aggregator_pair().await;
    let host_node_id = host.node_id();
    let client = FoldQueryClient::new(querier).with_deadline(Duration::from_secs(2));

    let first = client
        .query_latest(host_node_id, CapabilityFold::KIND_ID)
        .await
        .expect("first call");
    let gen_after_first = agg.generation();

    let second = client
        .query_latest(host_node_id, CapabilityFold::KIND_ID)
        .await
        .expect("second call");
    let gen_after_second = agg.generation();

    assert_eq!(first, second, "cached call must match");
    assert_eq!(
        gen_after_first, gen_after_second,
        "second call should not have ticked the host's generation (cache hit)"
    );
}

// ============================================================================
// fold.query.v2 and the daemon-level v1 handler
// (AGGREGATOR_MULTI_HOST_PLAN.md slice Q2; decisions D0, D8, D9)
// ============================================================================

mod v2 {
    use super::*;

    use net::adapter::net::behavior::aggregator::{
        decode_strict, install_fold_query_services, AggregatorRegistry, FoldQueryAccess,
        FoldQueryErrorKind, FoldQueryOp, FoldQueryV2ClientError, FoldQueryV2Error,
        FoldQueryV2Request, FoldQueryV2Response, RegistryAdminPolicy, StrictDecodeError,
        SummaryRecord, FOLD_QUERY_V1_NOT_AUTHORIZED, FOLD_QUERY_V2_SERVICE,
    };
    use net::adapter::net::behavior::capability::CapabilitySet;
    use net::adapter::net::behavior::fold::reservation::ReservationFold;
    use net::adapter::net::behavior::lifecycle::LifecycleGroup;
    use net::adapter::net::cortex::rpc::{
        RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload,
    };
    use net::adapter::net::mesh_rpc::RpcError;
    use net::adapter::net::{SubnetPolicy, SubnetRule};

    // ---- frozen layouts and strict decoding (no network) ------------------

    /// Golden bytes for every v2 shape. These are wire contracts: if one
    /// of these fails, the layout changed, and that needs a new service
    /// name (D9), not an updated constant.
    #[test]
    fn golden_bytes_pin_the_frozen_layouts() {
        let req = FoldQueryV2Request {
            kind: 1,
            op: FoldQueryOp::LatestSummary,
            group: Some("g".into()),
        };
        // kind varint 1 | op variant 0 | Some | len 1 | 'g'
        assert_eq!(
            postcard::to_allocvec(&req).unwrap(),
            [0x01, 0x00, 0x01, 0x01, 0x67]
        );
        let all = FoldQueryV2Request {
            kind: 3,
            op: FoldQueryOp::SummarizeNow,
            group: None,
        };
        // kind varint 3 | op variant 1 | None
        assert_eq!(postcard::to_allocvec(&all).unwrap(), [0x03, 0x01, 0x00]);

        let record = SummaryRecord {
            group: "g".into(),
            summary: SummaryAnnouncement {
                source_subnet: SubnetId::new(&[1]),
                fold_kind: 1,
                generation: 2,
                buckets: vec![("idle".into(), 3)],
            },
            refresh_ms: None,
            boot_epoch: Some(4),
            stream_seq: None,
        };
        let resp = FoldQueryV2Response::Summaries {
            kind: 1,
            records: vec![record],
        };
        let source_subnet = postcard::to_allocvec(&SubnetId::new(&[1])).unwrap();
        let mut expected = vec![
            0x00, // Summaries
            0x01, // kind 1
            0x01, // one record
            0x01, 0x67, // group "g"
        ];
        expected.extend_from_slice(&source_subnet);
        expected.extend_from_slice(&[
            0x01, // fold_kind 1
            0x02, // generation 2
            0x01, // one bucket
            0x04, b'i', b'd', b'l', b'e', // "idle"
            0x03, // 3
            0x00, // refresh_ms None
            0x01, 0x04, // boot_epoch Some(4)
            0x00, // stream_seq None
        ]);
        assert_eq!(postcard::to_allocvec(&resp).unwrap(), expected);

        let errors = [
            (
                FoldQueryV2Error::UnknownKind { kind: 9 },
                vec![0x01, 0x00, 0x09],
            ),
            (
                FoldQueryV2Error::UnknownGroup("g".into()),
                vec![0x01, 0x01, 0x01, 0x67],
            ),
            (FoldQueryV2Error::NotAuthorized, vec![0x01, 0x02]),
            (
                FoldQueryV2Error::DecodeFailed("x".into()),
                vec![0x01, 0x03, 0x01, 0x78],
            ),
        ];
        for (error, bytes) in errors {
            let resp = FoldQueryV2Response::Error(error.clone());
            assert_eq!(postcard::to_allocvec(&resp).unwrap(), bytes, "{error:?}");
        }
    }

    /// `postcard::from_bytes` ignores trailing input, which is how an
    /// appended field would be silently dropped. `decode_strict` refuses
    /// it, and a v1-shaped (shorter) request does not decode as v2.
    #[test]
    fn strict_decoding_refuses_trailing_and_short_input() {
        let mut bytes = postcard::to_allocvec(&FoldQueryV2Request {
            kind: 1,
            op: FoldQueryOp::LatestSummary,
            group: None,
        })
        .unwrap();
        assert!(decode_strict::<FoldQueryV2Request>(&bytes).is_ok());
        bytes.push(0x00);
        assert_eq!(
            decode_strict::<FoldQueryV2Request>(&bytes),
            Err(StrictDecodeError::TrailingBytes(1))
        );
        // The lenient decoder would have accepted it.
        assert!(postcard::from_bytes::<FoldQueryV2Request>(&bytes).is_ok());

        // A v1 request is `{ kind, op }`: two bytes, no group.
        let v1 = [0x01, 0x00];
        assert!(matches!(
            decode_strict::<FoldQueryV2Request>(&v1),
            Err(StrictDecodeError::Codec(_))
        ));
    }

    /// The access rule is the channel fan-out's own (`subnet_visible`):
    /// spot-check it from the publisher's side, plus the operator bypass.
    #[test]
    fn access_follows_the_fan_out_visibility_rule() {
        let local = SubnetId::new(&[1]);
        let same = Some(SubnetId::new(&[1]));
        let child = Some(SubnetId::new(&[1, 4]));
        let parent = Some(SubnetId::GLOBAL);
        let sibling = Some(SubnetId::new(&[2]));
        let unknown = None;
        let access = |visibility| FoldQueryAccess {
            visibility,
            operators: RegistryAdminPolicy::Closed,
        };

        let local_only = access(Visibility::SubnetLocal);
        assert!(local_only.admits(local, 7, same));
        assert!(!local_only.admits(local, 7, sibling));
        assert!(!local_only.admits(local, 7, unknown));

        let upward = access(Visibility::ParentVisible);
        assert!(upward.admits(local, 7, same));
        assert!(upward.admits(local, 7, parent), "an ancestor sees it");
        assert!(!upward.admits(local, 7, child), "not pushed downward");
        assert!(!upward.admits(local, 7, sibling));
        assert!(!upward.admits(local, 7, unknown));

        let everyone = access(Visibility::Global);
        assert!(everyone.admits(local, 7, sibling));
        assert!(everyone.admits(local, 7, unknown));

        // A global publisher reaches an unclassified caller, as fan-out does.
        assert!(local_only.admits(SubnetId::GLOBAL, 7, unknown));

        // Operators bypass visibility, and only operators do.
        let operators = FoldQueryAccess {
            visibility: Visibility::SubnetLocal,
            operators: RegistryAdminPolicy::operators([7]),
        };
        assert!(operators.admits(local, 7, sibling));
        assert!(!operators.admits(local, 8, sibling));
    }

    // ---- live round trips -------------------------------------------------

    fn region_policy() -> Arc<SubnetPolicy> {
        Arc::new(
            SubnetPolicy::new().add_rule(SubnetRule::new("region:", 0).map("eu", 1).map("us", 2)),
        )
    }

    /// A host in `subnet` (with the region policy when `classify`), plus a
    /// directly connected querier announcing `region:<region>`.
    async fn host_and_querier(
        subnet: SubnetId,
        classify: bool,
        region: &str,
    ) -> (Arc<MeshNode>, Arc<MeshNode>) {
        let mut cfg = test_config().with_subnet(subnet);
        if classify {
            cfg = cfg.with_subnet_policy(region_policy());
        }
        let host = Arc::new(
            MeshNode::new(EntityKeypair::generate(), cfg)
                .await
                .expect("host"),
        );
        let querier = build_node().await;
        handshake(&host, &querier).await;
        querier
            .announce_capabilities(CapabilitySet::new().add_tag(format!("region:{region}")))
            .await
            .expect("announce");
        if classify {
            let q = querier.node_id();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            while host.peer_subnet(q).is_none() && tokio::time::Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            assert!(host.peer_subnet(q).is_some(), "host classifies the querier");
        }
        (host, querier)
    }

    /// Register a one-replica group aggregating `kind`, with a primed
    /// latest-summary buffer.
    async fn add_group(host: &Arc<MeshNode>, registry: &AggregatorRegistry, name: &str, kind: u16) {
        let cfg = AggregatorConfig::new(host.local_subnet())
            .with_fold_kind(kind)
            .with_interval(Duration::from_secs(60));
        let mesh = host.clone();
        let mut seed = [0u8; 32];
        seed[..name.len()].copy_from_slice(name.as_bytes());
        let group = LifecycleGroup::<AggregatorDaemon>::spawn(1, seed, move |_| {
            Arc::new(AggregatorDaemon::new(cfg.clone(), mesh.clone()).expect("aggregator"))
        })
        .await
        .expect("spawn group");
        let entry = registry.register(name, group).expect("register");
        entry.snapshot().await.replicas[0].tick_once();
    }

    fn open_access() -> FoldQueryAccess {
        FoldQueryAccess {
            visibility: Visibility::Global,
            operators: RegistryAdminPolicy::Closed,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn v2_round_trips_by_group_all_groups_and_unknowns() {
        let (host, querier) = host_and_querier(SubnetId::new(&[3, 7]), false, "eu").await;
        let kp = EntityKeypair::generate();
        host.capability_fold()
            .apply(sign_cap(&kp, 0xA, 1, NodeState::Idle))
            .unwrap();
        let registry = Arc::new(AggregatorRegistry::new());
        add_group(&host, &registry, "caps-a", CapabilityFold::KIND_ID).await;
        add_group(&host, &registry, "caps-b", CapabilityFold::KIND_ID).await;
        add_group(&host, &registry, "res", ReservationFold::KIND_ID).await;
        let _serve =
            install_fold_query_services(&host, registry.clone(), open_access()).expect("install");

        let client = FoldQueryClient::new(querier.clone())
            .with_ttl(Duration::ZERO)
            .with_deadline(Duration::from_secs(3));
        let target = host.node_id();

        // One named group.
        let one = client
            .query_group_latest(target, CapabilityFold::KIND_ID, Some("caps-b"))
            .await
            .expect("named group");
        assert!(!one.is_empty());
        assert!(one.iter().all(|r| r.group == "caps-b"));
        assert!(one
            .iter()
            .all(|r| r.summary.fold_kind == CapabilityFold::KIND_ID));
        assert!(one
            .iter()
            .all(|r| r.refresh_ms.is_none() && r.boot_epoch.is_none() && r.stream_seq.is_none()));

        // Every group aggregating the kind, and only those.
        let all = client
            .query_group_latest(target, CapabilityFold::KIND_ID, None)
            .await
            .expect("all groups");
        let mut groups: Vec<&str> = all.iter().map(|r| r.group.as_str()).collect();
        groups.sort_unstable();
        groups.dedup();
        assert_eq!(groups, ["caps-a", "caps-b"]);

        // SummarizeNow is answered too.
        let fresh = client
            .query_group_summarize_now(target, ReservationFold::KIND_ID, Some("res"))
            .await
            .expect("summarize now");
        assert!(fresh.iter().all(|r| r.group == "res"));

        // Unknowns are typed.
        match client
            .query_group_latest(target, CapabilityFold::KIND_ID, Some("nope"))
            .await
        {
            Err(FoldQueryV2ClientError::Server(FoldQueryV2Error::UnknownGroup(g))) => {
                assert_eq!(g, "nope")
            }
            other => panic!("expected UnknownGroup, got {other:?}"),
        }
        match client
            .query_group_latest(target, ReservationFold::KIND_ID, Some("caps-a"))
            .await
        {
            Err(FoldQueryV2ClientError::Server(FoldQueryV2Error::UnknownKind { kind })) => {
                assert_eq!(kind, ReservationFold::KIND_ID)
            }
            other => panic!("expected UnknownKind, got {other:?}"),
        }

        // The daemon-level v1 handler answers v1's frozen shape from the
        // same registry.
        let v1 = client
            .query_latest(target, CapabilityFold::KIND_ID)
            .await
            .expect("v1 query");
        assert_eq!(v1.len(), all.len());
    }

    /// D0's access check, live: the host serves subnet 1 with
    /// subnet-local summaries and classifies callers by region.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn v2_admits_inside_visibility_and_operators_and_refuses_the_rest() {
        async fn setup(
            region: &str,
            operator: bool,
        ) -> (
            Arc<MeshNode>,
            Arc<MeshNode>,
            (
                net::adapter::net::mesh_rpc::ServeHandle,
                net::adapter::net::mesh_rpc::ServeHandle,
            ),
            FoldQueryClient,
        ) {
            let (host, querier) = host_and_querier(SubnetId::new(&[1]), true, region).await;
            let registry = Arc::new(AggregatorRegistry::new());
            add_group(&host, &registry, "caps", CapabilityFold::KIND_ID).await;
            let operators = if operator {
                RegistryAdminPolicy::operators([querier.node_id()])
            } else {
                RegistryAdminPolicy::Closed
            };
            let access = FoldQueryAccess {
                visibility: Visibility::SubnetLocal,
                operators,
            };
            let serve = install_fold_query_services(&host, registry, access).expect("install");
            let client = FoldQueryClient::new(querier.clone())
                .with_ttl(Duration::ZERO)
                .with_deadline(Duration::from_secs(3));
            (host, querier, serve, client)
        }

        // Same subnet: admitted.
        let (host, _q, _s, client) = setup("eu", false).await;
        client
            .query_group_latest(host.node_id(), CapabilityFold::KIND_ID, None)
            .await
            .expect("same-subnet caller admitted");

        // Sibling subnet: refused on v2 with the typed error, and on the
        // daemon-level v1 handler with the application status, which the
        // v1 client reports as NotAuthorized (not as a transport failure).
        let (host, _q, _s, client) = setup("us", false).await;
        match client
            .query_group_latest(host.node_id(), CapabilityFold::KIND_ID, None)
            .await
        {
            Err(FoldQueryV2ClientError::Server(FoldQueryV2Error::NotAuthorized)) => {}
            other => panic!("expected NotAuthorized, got {other:?}"),
        }
        match client
            .query_latest(host.node_id(), CapabilityFold::KIND_ID)
            .await
        {
            Err(e @ FoldQueryClientError::NotAuthorized) => {
                assert_eq!(e.error_kind(), FoldQueryErrorKind::Unauthorized);
                // The wire status the client recognizes is frozen.
                assert_eq!(FOLD_QUERY_V1_NOT_AUTHORIZED, 0x8001);
            }
            other => panic!("expected the v1 refusal as NotAuthorized, got {other:?}"),
        }

        // Sibling subnet, but named as an operator: admitted.
        let (host, _q, _s, client) = setup("us", true).await;
        client
            .query_group_latest(host.node_id(), CapabilityFold::KIND_ID, None)
            .await
            .expect("operator admitted outside visibility");
    }

    /// A v2 handler that never answers in time.
    struct Slow;

    #[async_trait::async_trait]
    impl RpcHandler for Slow {
        async fn call(&self, _ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
            tokio::time::sleep(Duration::from_secs(5)).await;
            Err(RpcHandlerError::Internal("too late".into()))
        }
    }

    /// `ServerTooOld` comes from the observed `NotFound` for the v2
    /// service and nothing else: a timeout stays a timeout.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_client_maps_only_not_found_to_server_too_old() {
        // A v1-only host: the per-daemon v1 service, no v2.
        let (host, querier, _agg, _serve) = build_aggregator_pair().await;
        let client = FoldQueryClient::new(querier)
            .with_ttl(Duration::ZERO)
            .with_deadline(Duration::from_secs(3));
        match client
            .query_group_latest(host.node_id(), CapabilityFold::KIND_ID, None)
            .await
        {
            Err(FoldQueryV2ClientError::ServerTooOld) => {}
            other => panic!("expected ServerTooOld, got {other:?}"),
        }

        // A host whose v2 service exists but never answers in time.
        let slow_host = build_node().await;
        let querier = build_node().await;
        handshake(&slow_host, &querier).await;
        let _slow = slow_host
            .serve_rpc(FOLD_QUERY_V2_SERVICE, Arc::new(Slow))
            .expect("serve slow v2");
        let client = FoldQueryClient::new(querier)
            .with_ttl(Duration::ZERO)
            .with_deadline(Duration::from_millis(500));
        match client
            .query_group_latest(slow_host.node_id(), CapabilityFold::KIND_ID, None)
            .await
        {
            Err(FoldQueryV2ClientError::Transport(e)) => {
                assert!(
                    !matches!(e, RpcError::ServerError { .. }),
                    "a timeout must not look like a server answer: {e:?}"
                );
            }
            other => panic!("expected a transport timeout, got {other:?}"),
        }
    }
}
