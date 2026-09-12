//! H3: cancellation and historical-close reclamation.
//!
//! Two independent leaks Kyra established from source:
//!
//! * a cancelled **initiator** never reached its explicit
//!   `deregister_direct_initiator`, and nothing else visits that
//!   registry — a recycled RTC generation is a different key, so the
//!   stale inbox was permanent;
//! * `max_peers` bounds *live sessions*, not close notifications, so
//!   a full channel plus a delayed consumer discarded a live peer's
//!   close with no counter, retry or pending state. The failure
//!   detector eventually noticing is not the promised reap →
//!   removal.
//!
//! Scope, honestly: the eviction the re-delivered notification runs
//! is `PeerEvictionCtx` — peer record, address/session indexes, ACK
//! cache, routing republication. It is **not** the full failure
//! callback (no reroute/withdrawal, capability/roster cleanup or
//! sensing disruption); those remain the failure plane's.
//!
//! Run: `cargo test --features "webrtc fixtures" --test rtc_reclaim`
#![cfg(all(feature = "webrtc", feature = "fixtures"))]

use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::rtc::{open_rtc_channel, RtcConfig};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, PeerAddr, SocketBufferConfig};

fn config(rtc: Option<RtcConfig>) -> MeshNodeConfig {
    let mut cfg = MeshNodeConfig::new("127.0.0.1:0".parse().expect("addr"), [0x51u8; 32]);
    cfg.socket_buffers = SocketBufferConfig::for_testing();
    // Long enough that a handshake **timeout** cannot be what
    // reclaims a registration inside any witness window here: the
    // close, the cancel or the successor must be.
    cfg.handshake_timeout = Duration::from_secs(120);
    cfg.rtc = rtc;
    cfg
}

async fn node(rtc: Option<RtcConfig>) -> Arc<MeshNode> {
    Arc::new(
        MeshNode::new(EntityKeypair::generate(), config(rtc))
            .await
            .expect("MeshNode::new"),
    )
}

fn rtc_config() -> RtcConfig {
    RtcConfig::new().with_bind_addr("127.0.0.1:0".parse().expect("addr"))
}

async fn wait_for<F: Fn() -> bool>(predicate: F, within: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + within;
    while tokio::time::Instant::now() < deadline {
        if predicate() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    predicate()
}

async fn pair() -> (Arc<MeshNode>, Arc<MeshNode>) {
    let a = node(Some(rtc_config())).await;
    let b = node(Some(rtc_config())).await;
    a.start();
    b.start();
    (a, b)
}

/// H3: cancelling an **initiator** after it has registered its inbox
/// leaves nothing behind, across churn.
///
/// The future is cancelled while parked on `inbox.next()` — the
/// exact point the explicit deregistration never ran from.
///
/// Inverse: go back to deregistering explicitly after the await
/// (delete the `DirectInboxGuard` from the post-`start()` initiator
/// branch) — each cancelled attempt leaves its inbox and the
/// registry grows by N.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelling_initiators_returns_the_registry_to_baseline() {
    let a = node(Some(rtc_config())).await;
    a.start();
    let baseline = a.pending_handshake_registrations();

    for _ in 0..6 {
        // A fresh peer — and so a fresh channel and a fresh
        // generation — per attempt: the churn shape that makes a
        // leaked entry permanent rather than displaced by the next
        // registration under the same key.
        let b = node(Some(rtc_config())).await;
        b.start();
        let b_pub = *b.public_key();
        let b_id = b.node_id();
        let (id_a, _id_b) = open_rtc_channel(&a, &b).await.expect("datachannel");

        let task = {
            let a = Arc::clone(&a);
            tokio::spawn(async move { a.connect_rtc(id_a, &b_pub, b_id).await })
        };
        // Let it register and park on a reply that will never come:
        // nobody is accepting on b for this channel.
        assert!(
            wait_for(
                || a.has_handshake_registration(PeerAddr::Rtc(id_a)),
                Duration::from_secs(5)
            )
            .await,
            "the initiator must have registered before it is cancelled"
        );
        task.abort();
        let _ = task.await;
        assert!(
            wait_for(
                || !a.has_handshake_registration(PeerAddr::Rtc(id_a)),
                Duration::from_secs(5)
            )
            .await,
            "a cancelled initiator must reclaim its own registration"
        );
        let _ = a.rtc_driver().expect("driver").close(id_a).await;
        drop(b);
    }

    assert_eq!(
        a.pending_handshake_registrations(),
        baseline,
        "six cancelled initiators must leave the registry where they found it"
    );
}

/// The same property for the **responder** — where the RAII guard
/// already existed and keeps its credit — plus the displacement
/// rule: a cancelled older attempt must not remove a successor's
/// registration.
///
/// Inverse: make the guard's removal unconditional (drop the
/// `Arc::ptr_eq` predicate) — the successor's registration
/// disappears with the cancelled predecessor.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cancelled_responder_does_not_strand_or_remove_a_successor() {
    let (a, b) = pair().await;
    let a_id = a.node_id();
    let (_id_a, id_b) = open_rtc_channel(&a, &b).await.expect("datachannel");
    let baseline = b.pending_handshake_registrations();

    let first = {
        let b = Arc::clone(&b);
        tokio::spawn(async move { b.accept_rtc(id_b, a_id).await })
    };
    assert!(
        wait_for(
            || b.has_handshake_registration(PeerAddr::Rtc(id_b)),
            Duration::from_secs(5)
        )
        .await,
        "the responder registers before waiting"
    );

    // A successor displaces the first attempt's registration.
    let second = {
        let b = Arc::clone(&b);
        tokio::spawn(async move { b.accept_rtc(id_b, a_id).await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;

    first.abort();
    let _ = first.await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    assert!(
        b.has_handshake_registration(PeerAddr::Rtc(id_b)),
        "the cancelled predecessor must not take the successor's registration down"
    );

    second.abort();
    let _ = second.await;
    assert!(
        wait_for(
            || b.pending_handshake_registrations() == baseline,
            Duration::from_secs(5)
        )
        .await,
        "with both cancelled the registry returns to baseline"
    );
}

/// H3: closing an endpoint drops the handshake inbox registered
/// under it. A recycled slot comes back at a new generation — a
/// different key — so an entry left here could never be displaced.
///
/// Inverse: delete the `registrations.remove(...)` from the close
/// notifier — the registration outlives the endpoint.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn closing_an_endpoint_clears_its_handshake_registration() {
    let (a, b) = pair().await;
    let a_id = a.node_id();
    let (_id_a, id_b) = open_rtc_channel(&a, &b).await.expect("datachannel");

    let responder = {
        let b = Arc::clone(&b);
        tokio::spawn(async move { b.accept_rtc(id_b, a_id).await })
    };
    assert!(
        wait_for(
            || b.has_handshake_registration(PeerAddr::Rtc(id_b)),
            Duration::from_secs(5)
        )
        .await,
        "the responder registers before waiting"
    );

    b.rtc_driver().expect("driver").close(id_b).await.ok();
    assert!(
        wait_for(
            || !b.has_handshake_registration(PeerAddr::Rtc(id_b)),
            Duration::from_secs(5)
        )
        .await,
        "the close must reclaim the registration keyed by that exact endpoint"
    );
    let _ = responder.await;
}

/// H3: a close the notification channel could not take is **not**
/// lost. It is recorded on the slot and re-offered on a later driver
/// turn, so the exact lifetime is evicted without waiting for the
/// failure detector — while a live peer keeps its session.
///
/// The consumer is held so the bounded channel (capacity
/// `max_peers`) genuinely overflows, which is Kyra's delayed-consumer
/// schedule rather than a single close on a large queue.
///
/// Inverse: drop `mark_pending_eviction` on a failed `try_send`
/// (back to `let _ = closed.try_send(...)`) — the deferred close is
/// discarded, the counter never moves and the peer stays installed
/// with a dead endpoint.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_close_the_channel_refused_is_re_delivered_not_dropped() {
    let a = node(Some(RtcConfig {
        max_peers: 2,
        ..rtc_config()
    }))
    .await;
    let b = node(Some(rtc_config())).await;
    a.start();
    b.start();
    let b_id = b.node_id();
    let (id_a, _id_b) = net::adapter::net::rtc::connect_rtc_loopback(&a, &b)
        .await
        .expect("rtc pair");
    assert_eq!(a.peer_endpoint(b_id), Some(PeerAddr::Rtc(id_a)));

    let driver = a.rtc_driver().expect("driver").clone();
    let deferred_before = driver.stats().close_notify_deferred();

    // Hold the consumer, then churn lifetimes until the two-deep
    // channel cannot take another close.
    a.set_rtc_close_consumer_paused(true);
    for _ in 0..8 {
        let Ok((id, _sdp)) = driver.create_offer().await else {
            break;
        };
        let _ = driver.close(id).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Now close the INSTALLED peer's endpoint: its notification is
    // the one that must not be lost.
    driver.close(id_a).await.expect("close the live endpoint");

    assert!(
        wait_for(
            || driver.stats().close_notify_deferred() > deferred_before,
            Duration::from_secs(10)
        )
        .await,
        "the held consumer must make the bounded channel refuse a close — \
         otherwise this witness is not testing the lossy path at all"
    );

    a.set_rtc_close_consumer_paused(false);

    assert!(
        wait_for(|| a.peer_endpoint(b_id).is_none(), Duration::from_secs(15)).await,
        "the exact lifetime whose close was refused must still be evicted, \
         without waiting for the failure detector"
    );
    assert!(
        wait_for(
            || (0..8).all(|slot| !driver.transport().has_pending_eviction(slot)),
            Duration::from_secs(10)
        )
        .await,
        "every deferred close must be re-delivered, not merely recorded"
    );
    // The bound is the point: a per-slot bit, not a queue that grows
    // with churn.
    assert!(
        driver.transport().retained_slots() <= 3,
        "lifetime churn must not grow the slot table; retained {}",
        driver.transport().retained_slots()
    );
    drop(b);
}
