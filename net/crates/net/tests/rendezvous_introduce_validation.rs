//! Integration tests for `NAT_TRAVERSAL_V2_PLAN.md` Stage 1 —
//! closing review Finding 4 (the *direct* unsolicited-introduce
//! reflector).
//!
//! Threat model: an authenticated session peer X sends the responder
//! B an *unsolicited* `PunchIntroduce { peer, peer_reflex }` — one B
//! never asked for (B has no waiter installed). Before Stage 1, B's
//! dispatch fell straight through to `schedule_punch` and fired a
//! keep-alive train at the wire-supplied `peer_reflex`, so X could
//! steer B's UDP at an arbitrary victim with X's identity hidden
//! behind B.
//!
//! Stage 1 gates that path (`unsolicited_introduce_permitted`):
//!
//! - **Cached reflex, IP mismatch → drop.** If B holds a signed
//!   reflex for `peer` (from `peer`'s capability announcement) and
//!   `peer_reflex`'s IP differs from it, the introduce is dropped —
//!   no train fires. Only the port may legitimately differ (symmetric
//!   NAT), so the IP is the anti-reflection anchor.
//! - **Cached reflex, port-shifted, IP match → accept.** The
//!   symmetric-NAT self-report case: same IP, different port. The
//!   train fires at the announced IP (never an arbitrary victim).
//! - **No cached reflex → conservative per-source cap.** A fresh
//!   counterpart whose announcement hasn't folded yet (or a
//!   nonexistent id an attacker names to force this branch) can't be
//!   validated. The legitimate first attempts proceed; a flood from
//!   one source is capped.
//!
//! Observation model: B fires its keep-alive train at `peer_reflex`
//! via its real UDP socket. We point `peer_reflex` at a
//! test-controlled loopback listener and watch for the train — its
//! presence means "accepted", its absence "dropped". A's *announced*
//! reflex (what B caches) is pinned via `with_reflex_override`, so we
//! can make the announced IP loopback (matches the listener) or
//! non-loopback (forces the IP mismatch) at will.
//!
//! Run: `cargo test --features net,nat-traversal --test rendezvous_introduce_validation`

#![cfg(all(feature = "net", feature = "nat-traversal"))]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::traversal::rendezvous::{PunchIntroduce, RendezvousMsg};
use net::adapter::net::traversal::SUBPROTOCOL_RENDEZVOUS;
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};
use tokio::net::UdpSocket;

const TEST_BUFFER_SIZE: usize = 256 * 1024;
const PSK: [u8; 32] = [0x42u8; 32];

fn test_config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5))
        .with_handshake(3, Duration::from_secs(2));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: TEST_BUFFER_SIZE,
        recv_buffer_size: TEST_BUFFER_SIZE,
    };
    cfg
}

async fn build_node() -> Arc<MeshNode> {
    Arc::new(
        MeshNode::new(EntityKeypair::generate(), test_config())
            .await
            .expect("MeshNode::new"),
    )
}

/// A node whose *announced* reflex is pinned to `override_addr` —
/// this is what peers cache and what B validates an introduce's
/// `peer_reflex` against.
async fn build_node_with_reflex(override_addr: SocketAddr) -> Arc<MeshNode> {
    let cfg = test_config().with_reflex_override(override_addr);
    Arc::new(
        MeshNode::new(EntityKeypair::generate(), cfg)
            .await
            .expect("MeshNode::new"),
    )
}

async fn connect_pair(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
    let a_id = a.node_id();
    let b_pub = *b.public_key();
    let b_addr = b.local_addr();
    let b_id = b.node_id();
    let b_clone = b.clone();
    let accept = tokio::spawn(async move { b_clone.accept(a_id).await });
    a.connect(b_addr, &b_pub, b_id)
        .await
        .expect("connect failed");
    accept
        .await
        .expect("accept task panicked")
        .expect("accept failed");
}

async fn wait_for<F: Fn() -> bool>(limit: Duration, check: F) -> bool {
    let start = tokio::time::Instant::now();
    while start.elapsed() < limit {
        if check() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    check()
}

/// Current Unix epoch milliseconds — used as `fire_at_ms` so the
/// keep-alive train fires immediately (`base_lead == 0`).
fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Inject a raw unsolicited `PunchIntroduce` from `injector` to
/// `target` over their established session. `target` has no waiter,
/// so this drives the responder / unsolicited dispatch path.
async fn inject_introduce(
    injector: &Arc<MeshNode>,
    target_addr: SocketAddr,
    intro: PunchIntroduce,
) {
    let body = RendezvousMsg::PunchIntroduce(intro).encode();
    injector
        .send_subprotocol(target_addr, SUBPROTOCOL_RENDEZVOUS, &body)
        .await
        .expect("inject send_subprotocol");
}

/// `true` iff `listener` receives at least one datagram (a keep-alive)
/// within `limit` — i.e. B's train fired at this address.
async fn train_fired(listener: &UdpSocket, limit: Duration) -> bool {
    let mut buf = [0u8; 64];
    tokio::time::timeout(limit, listener.recv_from(&mut buf))
        .await
        .is_ok()
}

/// Count the datagrams `listener` receives within `limit` (drains
/// until the window closes). Used to measure how many trains landed
/// under the per-source cap.
async fn count_datagrams(listener: &UdpSocket, limit: Duration) -> usize {
    let mut buf = [0u8; 64];
    let mut n = 0usize;
    let deadline = tokio::time::Instant::now() + limit;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, listener.recv_from(&mut buf)).await {
            Ok(Ok(_)) => n += 1,
            _ => break,
        }
    }
    n
}

/// Four-node topology: R + X both bridge A and B (A and B are NOT
/// directly connected). X is the injector — an authenticated session
/// peer of B — and also the second classification/announcement relay.
/// Returns `(A, R, B, X)`.
async fn topology(
    a: Arc<MeshNode>,
) -> (Arc<MeshNode>, Arc<MeshNode>, Arc<MeshNode>, Arc<MeshNode>) {
    let r = build_node().await;
    let b = build_node().await;
    let x = build_node().await;
    connect_pair(&a, &r).await;
    connect_pair(&b, &r).await;
    connect_pair(&a, &x).await;
    connect_pair(&b, &x).await;
    connect_pair(&r, &x).await;
    a.start();
    r.start();
    b.start();
    x.start();
    (a, r, b, x)
}

/// Cached reflex, IP mismatch → B drops the introduce, no train.
///
/// A's announced reflex IP is non-loopback (`203.0.113.10`); the
/// forged `peer_reflex` points at a loopback listener. B caches A's
/// announced reflex, sees the IP disagree, and must NOT fire.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsolicited_introduce_with_mismatched_ip_fires_no_train() {
    // A announces a non-loopback reflex; B will cache it.
    let a = build_node_with_reflex("203.0.113.10:9000".parse().unwrap()).await;
    let (a, _r, b, x) = topology(a).await;

    a.announce_capabilities(CapabilitySet::new())
        .await
        .expect("A announce");

    let a_id = a.node_id();
    // B must have folded A's announced (non-loopback) reflex before
    // we inject — otherwise the drop would be the *uncached* path,
    // not the IP-mismatch path we mean to test.
    let b_poll = b.clone();
    assert!(
        wait_for(Duration::from_secs(3), || {
            b_poll.peer_reflex_addr(a_id) == Some("203.0.113.10:9000".parse().unwrap())
        })
        .await,
        "B should cache A's announced non-loopback reflex; got {:?}",
        b.peer_reflex_addr(a_id),
    );

    // Victim listener on loopback — a different IP than A's announced
    // reflex, so validation must reject.
    let victim = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let victim_addr = victim.local_addr().unwrap();

    inject_introduce(
        &x,
        b.local_addr(),
        PunchIntroduce {
            peer: a_id,
            peer_reflex: victim_addr,
            fire_at_ms: now_unix_ms(),
        },
    )
    .await;

    assert!(
        !train_fired(&victim, Duration::from_millis(1200)).await,
        "IP-mismatched unsolicited introduce must be dropped — no keep-alive train",
    );
}

/// Cached reflex, port-shifted (same IP) → B accepts, train fires.
///
/// A's announced reflex is a loopback address; the forged
/// `peer_reflex` shares that IP on a different port (the symmetric-NAT
/// self-report shape). Validation keys on IP only, so the train fires
/// — at the announced IP, never an arbitrary victim.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsolicited_introduce_port_shifted_same_ip_fires_train() {
    // Victim listener first, so we can pin A's announced reflex to the
    // SAME loopback IP (different port) — exercising the accept path.
    let victim = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let victim_addr = victim.local_addr().unwrap();

    // A announces a loopback reflex on some other port.
    let announced: SocketAddr = "127.0.0.1:9000".parse().unwrap();
    assert_eq!(
        announced.ip(),
        victim_addr.ip(),
        "precondition: announced reflex shares the victim's (loopback) IP",
    );
    let a = build_node_with_reflex(announced).await;
    let (a, _r, b, x) = topology(a).await;

    a.announce_capabilities(CapabilitySet::new())
        .await
        .expect("A announce");

    let a_id = a.node_id();
    let b_poll = b.clone();
    assert!(
        wait_for(Duration::from_secs(3), || {
            b_poll.peer_reflex_addr(a_id) == Some(announced)
        })
        .await,
        "B should cache A's announced loopback reflex; got {:?}",
        b.peer_reflex_addr(a_id),
    );

    inject_introduce(
        &x,
        b.local_addr(),
        PunchIntroduce {
            peer: a_id,
            peer_reflex: victim_addr,
            fire_at_ms: now_unix_ms(),
        },
    )
    .await;

    assert!(
        train_fired(&victim, Duration::from_millis(1500)).await,
        "port-shifted (same-IP) unsolicited introduce must be accepted — train fires",
    );
}

/// No cached reflex → the conservative per-source cap admits the
/// introduce (the legitimate fresh-mesh / announcement-not-yet-folded
/// race still punches). We name a counterpart id B has never heard of,
/// which deterministically forces the uncached branch.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsolicited_introduce_uncached_counterpart_still_punches() {
    let a = build_node().await;
    let (_a, _r, b, x) = topology(a).await;

    let victim = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let victim_addr = victim.local_addr().unwrap();

    // A nonexistent counterpart id — B has no cached reflex for it,
    // so `unsolicited_introduce_permitted` takes the uncached path.
    let phantom_peer: u64 = 0xF00D_BEEF_DEAD_0001;
    assert!(
        b.peer_reflex_addr(phantom_peer).is_none(),
        "precondition: B has no cached reflex for the phantom counterpart",
    );

    inject_introduce(
        &x,
        b.local_addr(),
        PunchIntroduce {
            peer: phantom_peer,
            peer_reflex: victim_addr,
            fire_at_ms: now_unix_ms(),
        },
    )
    .await;

    assert!(
        train_fired(&victim, Duration::from_millis(1500)).await,
        "an uncached-counterpart introduce (the legitimate race) must still punch",
    );
}

/// The temporary per-source cap engages: a single session peer that
/// floods unsolicited introduces for uncached counterparts gets only
/// the first few trains (cap = 4 / 10 s window), not all of them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsolicited_introduce_flood_from_one_source_is_capped() {
    let a = build_node().await;
    let (_a, _r, b, x) = topology(a).await;

    let victim = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let victim_addr = victim.local_addr().unwrap();

    // Fire six introduces from the same source (X) for six distinct
    // uncached counterparts, all inside one window. Cap is 4 → four
    // trains of three keep-alives (12 datagrams), never six (18).
    let fire_at = now_unix_ms();
    for i in 0..6u64 {
        inject_introduce(
            &x,
            b.local_addr(),
            PunchIntroduce {
                peer: 0xABCD_0000_0000_0000 | i,
                peer_reflex: victim_addr,
                fire_at_ms: fire_at,
            },
        )
        .await;
    }

    // Each accepted introduce emits exactly three keep-alives
    // (offsets 0/100/250 ms). Drain for a bit over the train span.
    let got = count_datagrams(&victim, Duration::from_millis(1500)).await;
    assert!(
        got <= 12,
        "per-source cap should admit at most 4 trains (12 datagrams); got {got}",
    );
    assert!(
        got >= 3,
        "at least the first introduce must punch (control against a vacuous cap); got {got}",
    );
}

/// The global concurrent-train ceiling engages across *distinct*
/// sources: three session peers, each under its own per-source budget
/// (4 / window), collectively exceed the global ceiling (8), so only
/// the ceiling's worth of trains are admitted — a Sybil source set
/// can't multiply the aggregate. (`NAT_TRAVERSAL_V2_PLAN.md` Stage 2,
/// Finding 5.)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unsolicited_trains_are_globally_capped_across_sources() {
    // Responder B plus three independent injectors, each with its own
    // session to B.
    let b = build_node().await;
    let x1 = build_node().await;
    let x2 = build_node().await;
    let x3 = build_node().await;
    connect_pair(&x1, &b).await;
    connect_pair(&x2, &b).await;
    connect_pair(&x3, &b).await;
    b.start();
    x1.start();
    x2.start();
    x3.start();

    let b_addr = b.local_addr();

    // One distinct victim listener per introduce. Distinct
    // `peer_reflex` addresses matter: the punch-observer map is keyed
    // by `peer_reflex`, so a shared victim would let each new introduce
    // evict (and instantly complete) the prior observer, releasing its
    // slot before the next arrives — the trains would never overlap and
    // the ceiling would never bind. With distinct victims each train
    // holds its slot for `punch_deadline` (the victim never answers),
    // so all 12 genuinely contend for the 8 global slots at once.
    let mut victims = Vec::new();
    for _ in 0..12 {
        victims.push(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    }

    // 3 sources × 4 introduces = 12, all under the per-source cap (4),
    // so the per-source budget admits every one — only the global
    // ceiling (8) can gate them.
    let fire_at = now_unix_ms();
    let mut idx = 0usize;
    for (src_i, x) in [&x1, &x2, &x3].iter().enumerate() {
        for j in 0..4u64 {
            let victim_addr = victims[idx].local_addr().unwrap();
            inject_introduce(
                x,
                b_addr,
                PunchIntroduce {
                    peer: 0xC0DE_0000_0000_0000 | ((src_i as u64) << 8) | j,
                    peer_reflex: victim_addr,
                    fire_at_ms: fire_at,
                },
            )
            .await;
            idx += 1;
        }
    }

    // Let the admitted trains land (keep-alives fire at 0/100/250 ms).
    tokio::time::sleep(Duration::from_millis(700)).await;

    // Count how many victims received a train. The global ceiling caps
    // this at 8; a rejected introduce never reaches the keep-alive
    // sender, so its victim stays silent.
    let mut trains = 0;
    for v in &victims {
        if train_fired(v, Duration::from_millis(50)).await {
            trains += 1;
        }
    }
    assert!(
        trains <= 8,
        "global concurrent ceiling should admit at most 8 trains; got {trains}",
    );
    assert!(
        trains >= 4,
        "several trains should be admitted across sources (control against a \
         vacuous ceiling); got {trains}",
    );
}
