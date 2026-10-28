//! A machine that restarts with the same identity seed is reachable
//! again, both ways, without re-pairing
//! (`docs/internal/plans/APP_INTEGRATION_BLOCKERS_PLAN.md` Phase 1).
//!
//! Two defects made this fail, and each test here fails if either
//! comes back:
//!
//! - **The Noise key changed on every start.** A builder given an
//!   identity now derives its Noise key from it, so the restarted node
//!   answers on the same rendezvous `addr|pubkey|node_id` the survivor
//!   saved.
//! - **The survivor's reply-channel cache outlived the peer.** The
//!   caller remembers "already subscribed to this peer's reply channel"
//!   and cleared it only when the failure detector fired. A peer that
//!   came back before that had an empty roster, so the survivor's calls
//!   to it skipped the re-subscribe and timed out. Replacing a peer's
//!   session now clears that cache.

#![cfg(all(feature = "net", feature = "cortex"))]

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use net_sdk::identity::Identity;
use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::mesh_rpc::{CallOptions, CallOptionsTyped, Codec};
use serde::{Deserialize, Serialize};

const PSK: [u8; 32] = [0x42; 32];
const SEED_A: [u8; 32] = [0xA1; 32];
const SEED_B: [u8; 32] = [0xB2; 32];

#[derive(Debug, Serialize, Deserialize)]
struct Hello {
    from: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Name {
    name: String,
}

#[derive(Clone, Copy)]
struct Timing {
    heartbeat_ms: u64,
    session_timeout_ms: u64,
    /// How often A calls B while B restarts.
    poll_ms: u64,
}

async fn build(bind: &str, seed: [u8; 32], t: &Timing) -> Mesh {
    MeshBuilder::new(bind, &PSK)
        .unwrap()
        .identity(Identity::from_seed(seed))
        .heartbeat_ms(t.heartbeat_ms)
        .session_timeout_ms(t.session_timeout_ms)
        .build()
        .await
        .expect("build mesh")
}

fn serve_hello(mesh: &Mesh, name: &'static str) -> net_sdk::mesh_rpc::ServeHandle {
    mesh.serve_rpc_typed("bs.hello", Codec::Json, move |_req: Hello| async move {
        Ok(Name {
            name: name.to_string(),
        })
    })
    .expect("serve bs.hello")
}

/// One call with a short deadline.
async fn call(from: &Mesh, to: u64) -> Result<String, String> {
    from.call_typed::<Hello, Name>(
        to,
        "bs.hello",
        &Hello {
            from: format!("{:#x}", from.inner().node_id()),
        },
        CallOptionsTyped {
            raw: CallOptions {
                deadline: Some(Instant::now() + Duration::from_millis(1_000)),
                ..Default::default()
            },
            codec: Codec::Json,
        },
    )
    .await
    .map(|r| r.name)
    .map_err(|e| e.to_string())
}

/// Keep calling until one succeeds or `budget` runs out. Returns the
/// last error on timeout so a failure names its cause.
async fn call_until_ok(from: &Mesh, to: u64, budget: Duration) -> Result<String, String> {
    let start = Instant::now();
    let mut last = String::from("no attempt");
    while start.elapsed() < budget {
        match call(from, to).await {
            Ok(name) => return Ok(name),
            Err(e) => last = e,
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(last)
}

async fn restart_and_reconnect(t: Timing) {
    let a = build("127.0.0.1:0", SEED_A, &t).await;
    let b = build("127.0.0.1:0", SEED_B, &t).await;
    let addr_a: SocketAddr = a.inner().local_addr();
    let addr_b: SocketAddr = b.inner().local_addr();
    let (nid_a, nid_b) = (a.inner().node_id(), b.inner().node_id());
    let pub_a = *a.inner().public_key();
    let pub_b_before = *b.inner().public_key();

    // Pair: B dials A's rendezvous, the way `join` does (a routed
    // handshake that a started node answers without an explicit accept).
    a.start().expect("start A");
    b.start().expect("start B");
    b.connect_via(&addr_a.to_string(), &pub_a, nid_a)
        .await
        .expect("B connects to A");

    let _serve_a = serve_hello(&a, "A");
    let serve_b = serve_hello(&b, "B");
    assert_eq!(
        call(&b, nid_a).await.as_deref(),
        Ok("A"),
        "B -> A before restart"
    );
    assert_eq!(
        call(&a, nid_b).await.as_deref(),
        Ok("B"),
        "A -> B before restart"
    );

    // A keeps calling B through the restart, as an app's status poll
    // does — the case that kept A's old session with B busy.
    let a = std::sync::Arc::new(a);
    let poll = {
        let a = a.clone();
        tokio::spawn(async move {
            loop {
                let _ = call(&a, nid_b).await;
                tokio::time::sleep(Duration::from_millis(t.poll_ms)).await;
            }
        })
    };

    // Restart B: same seed, same port.
    drop(serve_b);
    b.shutdown().await.expect("shut down B");
    let b = build(&addr_b.to_string(), SEED_B, &t).await;
    assert_eq!(b.inner().node_id(), nid_b, "node id follows the seed");
    assert_eq!(
        *b.inner().public_key(),
        pub_b_before,
        "the Noise key must follow the seed, or every rendezvous naming it is stranded"
    );
    b.start().expect("restart B");
    let _serve_b = serve_hello(&b, "B");

    // B reconnects over what it saved, as `renew` does. A still holds
    // B's old session and has kept calling on it, so A's busy gate
    // defers B's handshake until B has been silent for three heartbeats;
    // `ensure_attached_via` re-dials through that window, where a single
    // `connect_via` (three attempts, ~15 s) can give up just short of it.
    b.ensure_attached_via(&addr_a.to_string(), &pub_a, nid_a)
        .await
        .expect("restarted B reconnects to A");

    let budget = Duration::from_secs(5);
    assert_eq!(
        call_until_ok(&b, nid_a, budget).await.as_deref(),
        Ok("A"),
        "B -> A after B restarted"
    );
    assert_eq!(
        call_until_ok(&a, nid_b, budget).await.as_deref(),
        Ok("B"),
        "A -> B after B restarted: A's cached reply subscription on the old \
         session must not survive the session being replaced"
    );
    poll.abort();
}

/// Net's default timing (5 s heartbeat, 30 s session timeout): B comes
/// back well inside A's session timeout, so A still holds B's old session
/// — and defers B's handshake for up to three heartbeats — when B dials.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restarted_peer_is_reachable_both_ways_within_the_session_timeout() {
    restart_and_reconnect(Timing {
        heartbeat_ms: 5_000,
        session_timeout_ms: 30_000,
        poll_ms: 1_000,
    })
    .await;
}

/// The PoC's `SESSION_MS` run: a short session timeout, where B's
/// reconnect used to succeed but A's calls back to B timed out.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restarted_peer_is_reachable_both_ways_with_a_short_session_timeout() {
    restart_and_reconnect(Timing {
        heartbeat_ms: 500,
        session_timeout_ms: 1_500,
        poll_ms: 1_000,
    })
    .await;
}

/// The SURVIVOR's side (CODE_REVIEW_2026_10_28 AIB-1). B restarts and
/// A calls `ensure_attached_via` at once, while the heartbeats B sent on
/// the old session before going down still read as recent. That used to
/// skip the handshake, leaving A on a session B no longer has: A's calls
/// timed out until A's session timeout fired.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_survivor_re_attaches_to_a_peer_that_just_restarted() {
    let t = Timing {
        heartbeat_ms: 5_000,
        session_timeout_ms: 30_000,
        poll_ms: 1_000,
    };
    let a = build("127.0.0.1:0", SEED_A, &t).await;
    let b = build("127.0.0.1:0", SEED_B, &t).await;
    let addr_a: SocketAddr = a.inner().local_addr();
    let addr_b: SocketAddr = b.inner().local_addr();
    let (nid_a, nid_b) = (a.inner().node_id(), b.inner().node_id());
    let (pub_a, pub_b) = (*a.inner().public_key(), *b.inner().public_key());

    a.start().expect("start A");
    b.start().expect("start B");
    b.connect_via(&addr_a.to_string(), &pub_a, nid_a)
        .await
        .expect("B connects to A");
    let _serve_a = serve_hello(&a, "A");
    let serve_b = serve_hello(&b, "B");
    assert_eq!(call(&a, nid_b).await.as_deref(), Ok("B"), "A -> B before");

    drop(serve_b);
    b.shutdown().await.expect("shut down B");
    let b = build(&addr_b.to_string(), SEED_B, &t).await;
    b.start().expect("restart B");
    let _serve_b = serve_hello(&b, "B");

    assert!(
        a.inner().peer_session_is_live(nid_b),
        "precondition: A's old session with B still reads as live"
    );
    a.ensure_attached_via(&addr_b.to_string(), &pub_b, nid_b)
        .await
        .expect("A re-attaches to the restarted B");
    assert_eq!(
        call_until_ok(&a, nid_b, Duration::from_secs(3))
            .await
            .as_deref(),
        Ok("B"),
        "A -> B right after B restarted: A must not keep the session B lost"
    );
}

/// `ensure_attached_via` always dials now; on a healthy pair that must
/// stay cheap and harmless — it returns promptly and both directions keep
/// working on whatever session results.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ensure_attached_on_a_healthy_pair_is_prompt_and_harmless() {
    // A session that has carried calls keeps reply streams open, so the
    // peer's busy gate defers the dial; the answer is the peer's next
    // heartbeat on the existing session, so keep the interval short.
    let t = Timing {
        heartbeat_ms: 1_000,
        session_timeout_ms: 30_000,
        poll_ms: 1_000,
    };
    let a = build("127.0.0.1:0", SEED_A, &t).await;
    let b = build("127.0.0.1:0", SEED_B, &t).await;
    let addr_a: SocketAddr = a.inner().local_addr();
    let addr_b: SocketAddr = b.inner().local_addr();
    let (nid_a, nid_b) = (a.inner().node_id(), b.inner().node_id());
    let (pub_a, pub_b) = (*a.inner().public_key(), *b.inner().public_key());
    a.start().expect("start A");
    b.start().expect("start B");
    b.connect_via(&addr_a.to_string(), &pub_a, nid_a)
        .await
        .expect("B connects to A");
    let _serve_a = serve_hello(&a, "A");
    let _serve_b = serve_hello(&b, "B");
    assert_eq!(call(&a, nid_b).await.as_deref(), Ok("B"));

    let started = Instant::now();
    a.ensure_attached_via(&addr_b.to_string(), &pub_b, nid_b)
        .await
        .expect("A ensures B");
    b.ensure_attached_via(&addr_a.to_string(), &pub_a, nid_a)
        .await
        .expect("B ensures A");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "ensuring a healthy pair took {:?}",
        started.elapsed()
    );
    let budget = Duration::from_secs(3);
    assert_eq!(call_until_ok(&a, nid_b, budget).await.as_deref(), Ok("B"));
    assert_eq!(call_until_ok(&b, nid_a, budget).await.as_deref(), Ok("A"));
}

/// An explicit Noise key wins over the derived one, and a builder with
/// no identity still gets a fresh key per build.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_explicit_noise_key_overrides_the_derived_one() {
    let explicit = net::adapter::net::NoiseStaticKey::from_private([0x77; 32]);
    let with_key = MeshBuilder::new("127.0.0.1:0", &PSK)
        .unwrap()
        .identity(Identity::from_seed(SEED_A))
        .noise_static_key(explicit.clone())
        .build()
        .await
        .unwrap();
    assert_eq!(*with_key.inner().public_key(), explicit.public_key());

    let anon = || async {
        MeshBuilder::new("127.0.0.1:0", &PSK)
            .unwrap()
            .build()
            .await
            .unwrap()
    };
    assert_ne!(
        *anon().await.inner().public_key(),
        *anon().await.inner().public_key()
    );
}
