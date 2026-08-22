//! The direct handshake's retry budget must be a real budget.
//!
//! `MeshNode::connect` retries `handshake_retries` times. Those retries
//! are only worth anything if a later attempt can still be answered,
//! and that turns entirely on whether the initiator RETRANSMITS its
//! `msg1` or mints a fresh one.
//!
//! The only direct responder is [`MeshNode::accept`], which is
//! ONE-SHOT: it returns after its first success and stops listening,
//! and the post-`start()` dispatch loop drops unsolicited direct
//! handshakes rather than answering them. So a responder that was
//! merely slow to be scheduled consumes the FIRST `msg1` it finds
//! buffered and answers exactly that one. An initiator that minted a
//! fresh handshake for its current attempt has already discarded the
//! state that `msg2` belongs to: it fails `read_message`, spends the
//! rest of its budget re-asking a question nobody is listening for,
//! and reports `Connection("handshake timeout")` — while its peer's
//! `accept()` reports success. The retry count cannot rescue that,
//! because every retry recreates the very mismatch.
//!
//! Retransmitting one `msg1` leaves every copy answerable by the one
//! state the initiator still holds, so the budget spans real time.
//!
//! # Properties under test
//!
//! - **A missed first window is survivable.** A responder scheduled
//!   after attempt 1's window closed still connects, and the resulting
//!   session is real — signed announcements decrypt in both
//!   directions, which they cannot do unless both sides derived the
//!   same keys.
//! - **The budget spans more than one missed window.** A responder
//!   that only arrives during a later attempt still connects.
//! - **An absent responder still fails.** The widened tolerance is not
//!   a way to hang: with no responder at all, `connect` returns
//!   `handshake timeout` inside the configured budget.

#![cfg(feature = "net")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};

const PSK: [u8; 32] = [0x42u8; 32];
const TEST_BUFFER_SIZE: usize = 256 * 1024;

/// A deliberately TIGHT per-attempt window. The point of these tests
/// is to make the retry budget — not the window — do the work, so the
/// window is small enough that a modest delay reliably overruns it
/// while the whole test still finishes in well under a second.
const WINDOW: Duration = Duration::from_millis(150);
const RETRIES: usize = 4;

/// `handshake_initiator` sleeps `100ms * attempt` between attempts, so
/// attempt N opens at roughly `N*WINDOW + 100ms*N(N-1)/2`.
fn attempt_opens_at(n: u32) -> Duration {
    let windows = WINDOW * (n - 1);
    let sleeps = Duration::from_millis(100 * u64::from((n - 1) * n) / 2);
    windows + sleeps
}

fn test_config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5))
        .with_handshake(RETRIES, WINDOW);
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: TEST_BUFFER_SIZE,
        recv_buffer_size: TEST_BUFFER_SIZE,
    };
    cfg
}

async fn build_node(seed: [u8; 32]) -> Arc<MeshNode> {
    Arc::new(
        MeshNode::new(EntityKeypair::from_bytes(seed), test_config())
            .await
            .expect("MeshNode::new"),
    )
}

/// Connect `initiator → responder`, but hold the responder's
/// `accept()` back by `late` so it misses the initiator's opening
/// window(s) — the scheduling stall a loaded runner produces, made
/// deterministic.
async fn connect_with_late_accept(
    initiator: &Arc<MeshNode>,
    responder: &Arc<MeshNode>,
    late: Duration,
) -> Result<(), String> {
    let i_id = initiator.node_id();
    let r_id = responder.node_id();
    let r_pub = *responder.public_key();
    let r_addr = responder.local_addr();
    let r = responder.clone();

    let accept = tokio::spawn(async move {
        tokio::time::sleep(late).await;
        r.accept(i_id).await
    });

    let connect = initiator.connect(r_addr, &r_pub, r_id).await;
    let accepted = accept.await.expect("accept task panicked");

    match (connect, accepted) {
        (Ok(_), Ok(_)) => Ok(()),
        (c, a) => Err(format!("connect={c:?} accept={a:?}")),
    }
}

/// Prove the pair share keys, not merely that both calls returned Ok:
/// capability announcements ride the session cipher, so a pin in each
/// direction is only reachable if both sides derived the same keys.
async fn assert_session_is_real(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
    a.start();
    b.start();
    a.announce_capabilities(CapabilitySet::new())
        .await
        .expect("a announces");
    b.announce_capabilities(CapabilitySet::new())
        .await
        .expect("b announces");

    let (a_id, b_id) = (a.node_id(), b.node_id());
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if a.peer_entity_id(b_id).is_some() && b.peer_entity_id(a_id).is_some() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the handshake reported success but no session-encrypted traffic crossed it");
}

/// A responder scheduled after attempt 1's window closed — the exact
/// shape of the loaded-runner failure — still connects.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_responder_that_misses_the_first_window_still_connects() {
    let a = build_node([0x11; 32]).await;
    let b = build_node([0x22; 32]).await;

    // Comfortably past attempt 1's window, comfortably inside the
    // budget: only a retry that is answerable can rescue this.
    let late = WINDOW + Duration::from_millis(70);
    assert!(late > WINDOW, "the responder must miss attempt 1");
    assert!(
        late < attempt_opens_at(RETRIES as u32),
        "the responder must still arrive inside the budget",
    );

    connect_with_late_accept(&a, &b, late)
        .await
        .expect("a late responder must still complete the handshake");

    assert_session_is_real(&a, &b).await;
}

/// The budget is worth more than one missed window: a responder that
/// only shows up for a later attempt still connects.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_budget_survives_more_than_one_missed_window() {
    let a = build_node([0x33; 32]).await;
    let b = build_node([0x44; 32]).await;

    // Inside attempt 3's window: two full windows have closed
    // unanswered before the responder exists at all.
    let late = attempt_opens_at(3) + Duration::from_millis(20);
    assert!(
        late < attempt_opens_at(RETRIES as u32),
        "the responder must still arrive inside the budget",
    );

    connect_with_late_accept(&a, &b, late)
        .await
        .expect("a responder arriving on a later attempt must still connect");

    assert_session_is_real(&a, &b).await;
}

/// Tolerating a late responder is not the same as waiting forever: with
/// no responder at all the budget is spent and the call fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_absent_responder_still_fails_inside_the_budget() {
    let a = build_node([0x55; 32]).await;
    let b = build_node([0x66; 32]).await;

    let started = Instant::now();
    let err = a
        .connect(b.local_addr(), b.public_key(), b.node_id())
        .await
        .expect_err("nobody accepted, so the handshake cannot complete");
    let elapsed = started.elapsed();

    assert!(
        format!("{err:?}").contains("handshake timeout"),
        "an unanswered handshake must report a timeout, got {err:?}",
    );
    // Every window plus every inter-attempt sleep, with slack for a
    // loaded runner — the point is that it is BOUNDED by the config.
    let budget = attempt_opens_at(RETRIES as u32) + WINDOW;
    assert!(
        elapsed < budget * 3,
        "the failure must land inside the configured budget, took {elapsed:?} (budget {budget:?})",
    );
}
