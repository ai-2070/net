//! One source subnet per daemon, with summary visibility enforced
//! (`AGGREGATOR_MULTI_HOST_PLAN.md`, slice Q1, decision D0).
//!
//! Every daemon here is booted by `boot()` from production TOML. The
//! subnet, the subnet rules and the summary visibility all come from the
//! config file; nothing is installed through a test-only path.
//!
//! The daemon serves subnet `1` with `summary_visibility = "subnet-local"`.
//! Its rules classify `region:eu` peers into `1` (same subnet) and
//! `region:us` peers into `2` (a sibling). The witnesses use real
//! delivery, not just the registered config:
//! - the same-subnet peer's Subscribe is admitted and it receives a
//!   summary the daemon itself published;
//! - the sibling's Subscribe is refused;
//! - a publish from the daemon's node reaches only the admitted peer;
//! - a group spawned later over RPC gets the same verdicts on its channel.

use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::aggregator::{
    FoldQueryClient, FoldQueryV2ClientError, FoldQueryV2Error, RegistryClient, RegistryClientError,
    RegistryRpcError, SummaryAnnouncement,
};
use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::behavior::fold::capability::CapabilityFold;
use net::adapter::net::behavior::fold::reservation::ReservationFold;
use net::adapter::net::behavior::fold::FoldKind;
use net::adapter::net::identity::EntityKeypair;
use net::adapter::net::{
    ChannelName, ChannelPublisher, MeshNode, MeshNodeConfig, OnFailure, PublishConfig, Reliability,
    SocketBufferConfig, SubnetId,
};
use net::adapter::Adapter;
use net_aggregator_daemon::{boot, BootedDaemon, Cli, DaemonError};
use tempfile::NamedTempFile;

const TEST_BUFFER_SIZE: usize = 256 * 1024;
const PSK: [u8; 32] = [0x42u8; 32];
const PSK_HEX: &str = "4242424242424242424242424242424242424242424242424242424242424242";
const DEADLINE: Duration = Duration::from_secs(5);

/// A daemon serving subnet 1, summaries subnet-local, with one static
/// capability group and a reservation template for the Spawn case.
/// `operators_any_admitted_peer` because the RPC caller is a leaf whose
/// node id is not known when the config is written.
const SCOPED: &str = r#"
    subnet = "1"
    summary_visibility = "subnet-local"
    operators_any_admitted_peer = true

    [[subnet_rule]]
    tag_prefix = "region:"
    level = 0
    values = { "eu" = 1, "us" = 2 }

    [[group]]
    name = "capability"
    fold_kinds = [1]
    replica_count = 1
    summary_interval_ms = 50

    [[template]]
    name = "reservation"
    fold_kinds = [3]
    summary_interval_ms = 50

    [[template]]
    name = "capability-again"
    fold_kinds = [1]
    summary_interval_ms = 50
"#;

fn leaf_config() -> MeshNodeConfig {
    let addr: std::net::SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5))
        .with_handshake(3, Duration::from_secs(2))
        .with_capability_gc_interval(Duration::from_millis(250))
        // One shard, so everything a subscriber receives lands on shard 0.
        .with_num_shards(1);
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: TEST_BUFFER_SIZE,
        recv_buffer_size: TEST_BUFFER_SIZE,
    };
    cfg
}

async fn try_boot(extra: &str) -> (Result<BootedDaemon, DaemonError>, NamedTempFile) {
    let toml = format!(
        r#"
            listen = "127.0.0.1:0"
            psk_hex = "{PSK_HEX}"
            {extra}
        "#
    );
    let cfg_file = NamedTempFile::new().expect("tempfile");
    std::fs::write(cfg_file.path(), toml).expect("write config");
    let cli = Cli {
        config: cfg_file.path().to_path_buf(),
        listen: None,
        verbose: 0,
        print_bootstrap: false,
        insecure_permissions: false,
    };
    (boot(cli).await, cfg_file)
}

async fn boot_scoped() -> (BootedDaemon, NamedTempFile) {
    boot_config(SCOPED).await
}

/// `SCOPED` without `operators_any_admitted_peer`: no caller is an
/// operator, so visibility alone decides who may query.
async fn boot_scoped_without_operators() -> (BootedDaemon, NamedTempFile) {
    let config = SCOPED.replace("operators_any_admitted_peer = true", "");
    assert_ne!(config, SCOPED, "the operator line was removed");
    boot_config(&config).await
}

async fn boot_config(config: &str) -> (BootedDaemon, NamedTempFile) {
    let (booted, file) = try_boot(config).await;
    let booted = booted.expect("daemon boot");
    booted.mesh.start();
    (booted, file)
}

/// A leaf attached directly to the daemon that announces `region:<r>`, so
/// the daemon's policy classifies it.
async fn leaf_in(hub: &BootedDaemon, region: &str) -> Arc<MeshNode> {
    let leaf = Arc::new(
        MeshNode::new(EntityKeypair::generate(), leaf_config())
            .await
            .expect("leaf MeshNode::new"),
    );
    leaf.start();
    leaf.connect_via(hub.bound_addr, &hub.public_key, hub.mesh.node_id())
        .await
        .expect("leaf attaches");
    leaf.announce_capabilities(CapabilitySet::new().add_tag(format!("region:{region}")))
        .await
        .expect("announce");
    leaf
}

async fn wait_until(mut cond: impl FnMut() -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + DEADLINE;
    while tokio::time::Instant::now() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    cond()
}

/// The canonical summary channel for `kind`, from a running group, so the
/// test never restates the naming rule.
async fn summary_channel(hub: &BootedDaemon, group: &str, kind: u16) -> ChannelName {
    let entry = hub.registry.get(group).expect("group registered");
    let snap = entry.snapshot().await;
    snap.replicas
        .first()
        .expect("a replica")
        .summary_channel_name(kind)
        .expect("channel name")
}

/// Wait until the daemon has classified both leaves, so a Subscribe is
/// judged against a known subnet rather than an unknown one.
async fn classified(hub: &BootedDaemon, eu: &MeshNode, us: &MeshNode) {
    let (eu_id, us_id) = (eu.node_id(), us.node_id());
    assert!(
        wait_until(|| hub.mesh.peer_subnet(eu_id) == Some(SubnetId::new(&[1]))
            && hub.mesh.peer_subnet(us_id) == Some(SubnetId::new(&[2])))
        .await,
        "the daemon classifies eu as 1 and us as 2"
    );
}

/// Publish one frame on `channel` from the daemon's own node and report
/// how many subscribers the fan-out attempted and reached. The daemon's
/// node and registry are the ones under test.
async fn publish_from_daemon(hub: &BootedDaemon, channel: &ChannelName) -> (usize, usize) {
    let publisher = ChannelPublisher::new(
        channel.clone(),
        PublishConfig {
            reliability: Reliability::FireAndForget,
            on_failure: OnFailure::BestEffort,
            max_inflight: 16,
        },
    );
    let report = hub
        .mesh
        .publish(&publisher, net_sdk::Bytes::from_static(b"probe"))
        .await
        .expect("publish");
    (report.attempted, report.delivered)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn subnet_local_summaries_reach_the_same_subnet_and_not_a_sibling() {
    let (hub, _cfg) = boot_scoped().await;
    assert_eq!(hub.mesh.local_subnet(), SubnetId::new(&[1]));
    let eu = leaf_in(&hub, "eu").await;
    let us = leaf_in(&hub, "us").await;
    classified(&hub, &eu, &us).await;

    let channel = summary_channel(&hub, "capability", CapabilityFold::KIND_ID).await;
    let hub_id = hub.mesh.node_id();
    eu.subscribe_channel(hub_id, channel.clone())
        .await
        .expect("the same-subnet peer's Subscribe is admitted");
    assert!(
        us.subscribe_channel(hub_id, channel.clone()).await.is_err(),
        "the sibling's Subscribe is refused under subnet-local"
    );

    // Real delivery of a summary the daemon itself produced. Summaries are
    // only republished when they change, so change the counted fold after
    // the subscription: one more peer announcing adds an entry.
    let _third = leaf_in(&hub, "eu").await;
    let mut got: Option<SummaryAnnouncement> = None;
    let deadline = tokio::time::Instant::now() + DEADLINE;
    while got.is_none() && tokio::time::Instant::now() < deadline {
        let polled = eu.poll_shard(0, None, 64).await.expect("poll");
        got = polled
            .events
            .iter()
            .find_map(|e| postcard::from_bytes::<SummaryAnnouncement>(&e.raw).ok());
        if got.is_none() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
    let summary = got.expect("the same-subnet subscriber receives a daemon summary");
    assert_eq!(summary.fold_kind, CapabilityFold::KIND_ID);
    assert_eq!(summary.source_subnet, SubnetId::new(&[1]));

    // The daemon's fan-out on that channel reaches exactly the admitted
    // subscriber.
    assert_eq!(publish_from_daemon(&hub, &channel).await, (1, 1));
}

/// A group spawned over RPC after boot gets the same verdicts: its
/// channel was registered at boot from the template.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_spawned_group_gets_the_same_verdicts() {
    let (hub, _cfg) = boot_scoped().await;
    let eu = leaf_in(&hub, "eu").await;
    let us = leaf_in(&hub, "us").await;
    classified(&hub, &eu, &us).await;
    let hub_id = hub.mesh.node_id();

    let client = RegistryClient::new(eu.clone()).with_deadline(Duration::from_secs(3));
    client
        .spawn(hub_id, "reservation", "reservations", 1)
        .await
        .expect("spawn from the reservation template");

    let channel = summary_channel(&hub, "reservations", ReservationFold::KIND_ID).await;
    eu.subscribe_channel(hub_id, channel.clone())
        .await
        .expect("same-subnet Subscribe admitted on the spawned group's channel");
    assert!(
        us.subscribe_channel(hub_id, channel.clone()).await.is_err(),
        "sibling Subscribe refused on the spawned group's channel"
    );
    assert_eq!(publish_from_daemon(&hub, &channel).await, (1, 1));

    // One group per fold kind: a second capability group is refused at
    // Spawn, with the owner named.
    match client
        .spawn(hub_id, "capability-again", "capability-2", 1)
        .await
    {
        Err(RegistryClientError::Server(RegistryRpcError::SpawnRejected(msg))) => {
            assert!(
                msg.contains("already aggregated by group \"capability\""),
                "{msg}"
            );
        }
        other => panic!("expected SpawnRejected(kind occupied), got {other:?}"),
    }
    assert!(hub.registry.get("capability-2").is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn boot_refuses_configs_that_break_one_subnet_per_daemon() {
    let refusal = |extra: &'static str| async move {
        match try_boot(extra).await.0 {
            Ok(_) => panic!("expected boot to be refused for:\n{extra}"),
            Err(e) => e,
        }
    };

    // The upgrade case decided for Q1: a pre-Q1 config labelling a group
    // with a non-global subnet, and no daemon `subnet`.
    let e = refusal(
        r#"
        [[group]]
        name = "alpha"
        source_subnet = "3.7"
        fold_kinds = [1]
        replica_count = 1
        summary_interval_ms = 50
        "#,
    )
    .await;
    assert!(
        matches!(e, DaemonError::SourceSubnetMismatch { what: "group", .. }),
        "{e:?}"
    );
    assert!(e.to_string().contains("set `subnet = \"3.7\"`"), "{e}");

    // A non-global subnet with no rules to classify peers into it.
    let e = refusal(r#"subnet = "1""#).await;
    assert!(
        matches!(e, DaemonError::MissingSubnetPolicy { .. }),
        "{e:?}"
    );

    // A template naming another subnet.
    let e = refusal(
        r#"
        subnet = "1"
        [[subnet_rule]]
        tag_prefix = "region:"
        level = 0
        values = { "eu" = 1 }

        [[template]]
        name = "elsewhere"
        source_subnet = "2"
        fold_kinds = [1]
        summary_interval_ms = 50
        "#,
    )
    .await;
    assert!(
        matches!(
            e,
            DaemonError::SourceSubnetMismatch {
                what: "template",
                ..
            }
        ),
        "{e:?}"
    );

    // A second group of an occupied fold kind.
    let e = refusal(
        r#"
        [[group]]
        name = "a"
        fold_kinds = [1]
        replica_count = 1
        summary_interval_ms = 50

        [[group]]
        name = "b"
        fold_kinds = [1]
        replica_count = 1
        summary_interval_ms = 50
        "#,
    )
    .await;
    assert!(
        matches!(e, DaemonError::FoldKindOccupied { kind: 1, .. }),
        "{e:?}"
    );
}

/// Q3: the daemon serves `fold.query.v2` with the same boundary as its
/// summary channels. The same-subnet caller gets the group's records; the
/// sibling is refused. Access comes only from the TOML (subnet, rules,
/// `summary_visibility`), with no operators configured.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_daemon_answers_fold_query_v2_inside_its_visibility_only() {
    let (hub, _cfg) = boot_scoped_without_operators().await;
    let eu = leaf_in(&hub, "eu").await;
    let us = leaf_in(&hub, "us").await;
    classified(&hub, &eu, &us).await;
    let hub_id = hub.mesh.node_id();
    // Let the group tick once so it has a summary to return.
    tokio::time::sleep(Duration::from_millis(150)).await;

    let client = |node: &Arc<MeshNode>| {
        FoldQueryClient::new(node.clone())
            .with_ttl(Duration::ZERO)
            .with_deadline(Duration::from_secs(3))
    };
    let records = client(&eu)
        .query_group_latest(hub_id, CapabilityFold::KIND_ID, Some("capability"))
        .await
        .expect("same-subnet caller admitted");
    assert!(!records.is_empty(), "the group has a summary to return");
    assert!(records.iter().all(|r| r.group == "capability"));
    assert!(records
        .iter()
        .all(|r| r.summary.source_subnet == SubnetId::new(&[1])));

    match client(&us)
        .query_group_latest(hub_id, CapabilityFold::KIND_ID, None)
        .await
    {
        Err(FoldQueryV2ClientError::Server(FoldQueryV2Error::NotAuthorized)) => {}
        other => panic!("expected NotAuthorized for the sibling, got {other:?}"),
    }
}

/// `operators_any_admitted_peer = true` makes every admitted peer an
/// operator, and operators bypass summary visibility (D0). So the sibling
/// that is refused above is admitted here. Pinned so the bypass is a
/// decision someone sees, not a surprise.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn any_admitted_peer_operators_bypass_query_visibility() {
    let (hub, _cfg) = boot_scoped().await;
    let eu = leaf_in(&hub, "eu").await;
    let us = leaf_in(&hub, "us").await;
    classified(&hub, &eu, &us).await;
    let records = FoldQueryClient::new(us.clone())
        .with_ttl(Duration::ZERO)
        .with_deadline(Duration::from_secs(3))
        .query_group_latest(hub.mesh.node_id(), CapabilityFold::KIND_ID, None)
        .await
        .expect("an any-admitted-peer operator is admitted outside visibility");
    assert!(records.iter().all(|r| r.group == "capability"));
}

/// A `SummarizeNow` from a peer publishes what its tick found.
///
/// The group's background tick is an hour away, so a summary the
/// subscriber receives can only come from the on-demand tick. An
/// on-demand tick that only buffered its summaries would also hide them
/// from that background tick, which would see nothing new, so
/// subscribers would miss the change until the fold changed again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn summarize_now_publishes_what_it_ticks() {
    let config = SCOPED
        .replace("operators_any_admitted_peer = true", "")
        .replace(
            "fold_kinds = [1]\n    replica_count = 1\n    summary_interval_ms = 50",
            "fold_kinds = [1]\n    replica_count = 1\n    summary_interval_ms = 3600000",
        );
    assert!(config.contains("3600000"), "the static group ticks hourly");
    let (hub, _cfg) = boot_config(&config).await;
    let eu = leaf_in(&hub, "eu").await;
    let us = leaf_in(&hub, "us").await;
    classified(&hub, &eu, &us).await;
    let hub_id = hub.mesh.node_id();

    let channel = summary_channel(&hub, "capability", CapabilityFold::KIND_ID).await;
    eu.subscribe_channel(hub_id, channel)
        .await
        .expect("same-subnet Subscribe admitted");

    // A same-subnet peer that is not an operator asks for a fresh read.
    let records = FoldQueryClient::new(eu.clone())
        .with_ttl(Duration::ZERO)
        .with_deadline(Duration::from_secs(3))
        .query_group_summarize_now(hub_id, CapabilityFold::KIND_ID, Some("capability"))
        .await
        .expect("SummarizeNow admitted");
    let generation = records
        .first()
        .expect("the on-demand tick summarizes the fold")
        .summary
        .generation;

    let mut got: Option<SummaryAnnouncement> = None;
    let deadline = tokio::time::Instant::now() + DEADLINE;
    while got.is_none() && tokio::time::Instant::now() < deadline {
        let polled = eu.poll_shard(0, None, 64).await.expect("poll");
        got = polled
            .events
            .iter()
            .filter_map(|e| postcard::from_bytes::<SummaryAnnouncement>(&e.raw).ok())
            .find(|s| s.fold_kind == CapabilityFold::KIND_ID);
        if got.is_none() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
    let published = got.expect("the subscriber receives the on-demand tick's summary");
    assert_eq!(published.generation, generation);
}
