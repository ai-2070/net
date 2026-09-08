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
//! - **Foreign handshakes are drained, not charged.** Handshake
//!   datagrams that don't decrypt under this pairing's prologue —
//!   stale `msg1` copies left by an earlier pairing's retransmits —
//!   cost the responder loop iterations, never attempts.

#![cfg(feature = "net")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, NetHeader, SocketBufferConfig};

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

fn config_with_psk(psk: [u8; 32]) -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, psk)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5))
        .with_handshake(RETRIES, WINDOW);
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: TEST_BUFFER_SIZE,
        recv_buffer_size: TEST_BUFFER_SIZE,
    };
    cfg
}

fn test_config() -> MeshNodeConfig {
    config_with_psk(PSK)
}

async fn build_node(seed: [u8; 32]) -> Arc<MeshNode> {
    build_node_with_config(seed, test_config()).await
}

async fn build_node_with_config(seed: [u8; 32], cfg: MeshNodeConfig) -> Arc<MeshNode> {
    Arc::new(
        MeshNode::new(EntityKeypair::from_bytes(seed), cfg)
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

/// The per-source handshake budget the responder paces to, read from
/// the responder itself rather than mirrored: a mirrored copy makes a
/// budget change surface as an off-by-N counter mismatch in an
/// unrelated-looking assertion instead of a compile error.
const RESPONDER_BURST: usize = MeshNode::RESPONDER_HANDSHAKE_BURST as usize;

/// Build a well-formed handshake packet whose body cannot decrypt —
/// exactly what another pairing's `msg1` looks like to this
/// responder (handshake flag set, NKpsk0-sized body, wrong prologue).
fn foreign_handshake_packet() -> Vec<u8> {
    let mut packet = NetHeader::handshake(48).to_bytes().to_vec();
    packet.extend_from_slice(&[0x5a; 48]);
    packet
}

/// Spray `count` foreign handshakes at `target` from one source.
///
/// `send_to` returning means the datagram reached the kernel, NOT that
/// it is already in another socket's receive queue — loopback delivery
/// is not synchronous everywhere (notably Windows). So this makes no
/// ordering promise; pair it with [`await_classified`], which waits on
/// the responder's own counters, whenever a test needs the spray to be
/// provably ahead of a later `msg1`.
async fn spray_foreign_handshakes(target: &Arc<MeshNode>, count: usize) {
    let sprayer = tokio::net::UdpSocket::bind("127.0.0.1:0")
        .await
        .expect("bind sprayer");
    let packet = foreign_handshake_packet();
    for _ in 0..count {
        sprayer
            .send_to(&packet, target.local_addr())
            .await
            .expect("spray a foreign handshake");
    }
}

/// Block until `node`'s responder has classified at least `count`
/// handshake datagrams — drained (read, did not decrypt) or paced
/// (dropped before Noise).
///
/// This is the synchronisation point that lets the tests below assert
/// exact counts: it turns "sprayed, therefore surely already queued"
/// into "the responder says it has seen them", which is the same claim
/// the assertions rest on and is actually observable.
async fn await_classified(node: &Arc<MeshNode>, count: usize) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let seen = node.responder_handshakes_drained() + node.responder_handshakes_paced();
        if seen >= count as u64 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the responder classified only {seen} of the {count} sprayed handshakes",
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Handshake datagrams that belong to a DIFFERENT pairing must not
/// consume the responder's attempt budget.
///
/// This is the other half of the retransmit design. Because the
/// initiator retransmits identical `msg1` copies and `accept()` is
/// one-shot, a slow responder answers copy 1 and leaves the rest
/// parked on its socket; the node's next `accept()` reads them and
/// they cannot decrypt under the new pairing's prologue. Charging
/// each one an attempt let a handful of stale copies exhaust
/// `handshake_retries` in milliseconds — `accept()` returned `Err`
/// while the real initiator was still retransmitting into a node
/// with no responder, and the initiator reported `handshake timeout`
/// after its full budget. Observed in CI as a topology-setup flake.
///
/// The drain counter — not just a successful connect — is the
/// assertion: it proves the responder actually read and discarded
/// every sprayed copy, so the test cannot pass vacuously by the junk
/// never reaching the responder's recv loop.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn foreign_handshakes_do_not_consume_the_responder_budget() {
    let a = build_node([0x77; 32]).await;
    let b = build_node([0x88; 32]).await;

    // One more copy than `RETRIES`, so a budget-consuming responder
    // cannot survive it; still inside the per-source pacing budget,
    // so every copy reaches `read_message`.
    let sprayed = RETRIES + 1;
    assert!(
        sprayed <= RESPONDER_BURST,
        "the drain path, not the pacer, must be what handles these",
    );

    // Order the spray ahead of the initiator's `msg1` by construction
    // rather than by hoping UDP does it: start the responder, spray,
    // and wait on ITS counters before the initiator sends anything.
    let responder = b.clone();
    let initiator_id = a.node_id();
    let accept = tokio::spawn(async move { responder.accept(initiator_id).await });

    spray_foreign_handshakes(&b, sprayed).await;
    await_classified(&b, sprayed).await;

    let connect = a.connect(b.local_addr(), b.public_key(), b.node_id()).await;
    let accepted = accept.await.expect("accept task panicked");
    assert!(
        connect.is_ok() && accepted.is_ok(),
        "queued foreign handshakes must be drained, not counted: \
         connect={connect:?} accept={accepted:?}",
    );

    assert_eq!(
        b.responder_handshakes_drained(),
        sprayed as u64,
        "the responder must have read and discarded every sprayed copy",
    );
    assert_eq!(
        b.responder_handshakes_paced(),
        0,
        "nothing was over budget — the pacer must not have been involved",
    );

    assert_session_is_real(&a, &b).await;
}

/// Draining is bounded work: a source that keeps spraying has most of
/// its datagrams dropped before any Noise work, and the legitimate
/// initiator still connects.
///
/// Without pacing, "skip and keep waiting" would hand an off-path
/// sprayer a full Noise read per datagram for the whole deadline —
/// trading the old kill-the-accept bug for a starve-the-accept one.
///
/// The assertions are deliberately *invariants*, not an exact
/// drained/paced split. The split depends on how many wall-clock
/// pacing windows elapse while the responder is draining, and a
/// descheduled task on a loaded (llvm-cov) runner rolls the window
/// mid-loop: the budget refills, more datagrams are admitted, and an
/// `assert_eq!` on the split fails with no defect behind it —
/// reintroducing exactly the kind of flake this file exists to remove.
/// The pacer's arithmetic is pinned deterministically by
/// `handshake_pacer_rejects_floods_per_source` in the unit suite; what
/// belongs here is the behaviour: every datagram is accounted for,
/// pacing engaged, and the initiator got through anyway.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_one_source_handshake_flood_is_paced_and_does_not_starve_the_initiator() {
    let a = build_node([0x99; 32]).await;
    let b = build_node([0xaa; 32]).await;

    // Several times the per-window budget, so `paced > 0` survives
    // even if the responder stalls long enough to roll the window a
    // few times mid-drain.
    let flood = RESPONDER_BURST * 12;

    let responder = b.clone();
    let initiator_id = a.node_id();
    let accept = tokio::spawn(async move { responder.accept(initiator_id).await });

    spray_foreign_handshakes(&b, flood).await;
    await_classified(&b, flood).await;

    // Snapshot before the initiator adds traffic of its own.
    let drained = b.responder_handshakes_drained();
    let paced = b.responder_handshakes_paced();
    assert_eq!(
        drained + paced,
        flood as u64,
        "every sprayed datagram must be accounted for as drained or paced",
    );
    assert!(
        paced > 0,
        "a flood this far over budget must have been paced ({drained} drained, \
         {paced} paced of {flood})",
    );
    assert!(
        drained > 0,
        "pacing must not be so eager that nothing reaches Noise at all",
    );

    let connect = a.connect(b.local_addr(), b.public_key(), b.node_id()).await;
    let accepted = accept.await.expect("accept task panicked");
    assert!(
        connect.is_ok() && accepted.is_ok(),
        "a paced flood must not stop a legitimate initiator: \
         connect={connect:?} accept={accepted:?}",
    );

    assert_session_is_real(&a, &b).await;
}

/// A concurrent `accept()` must not destroy a `connect()`'s reply.
///
/// Pre-`start()` there is no dispatch loop, so `accept()`'s responder
/// and `connect()`'s initiator both poll the node's one socket — and
/// tokio hands each datagram to exactly one waiter. The responder sees
/// the peer's `msg2` as a handshake datagram that does not decrypt
/// under the pairing it is accepting, which is precisely the shape it
/// now drains. Draining it does not merely fail to help: that datagram
/// is the only copy, and because the drain loop no longer backs off
/// between datagrams it is well placed to swallow every retransmit
/// too, so the initiator times out against a peer that answered every
/// single time.
///
/// The hub below accepts a peer that never arrives, so its responder
/// camps on the shared socket for the whole budget while the hub's own
/// `connect()` runs. The responder is given a head start on purpose so
/// it is deterministically the one that reads the reply — without it
/// the two racing `recv_from`s make the witness a coin flip. The
/// connect must complete anyway.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_concurrent_accept_does_not_swallow_a_connects_reply() {
    let hub = build_node([0x21; 32]).await;
    let peer = build_node([0x23; 32]).await;

    // Nobody will ever send this node's msg1, so the hub's responder
    // stays in its drain loop for the accept's entire budget.
    let absent_peer = 0x0000_dead_beef_u64;

    let hub_id = hub.node_id();
    let responder = peer.clone();
    let peer_accept = tokio::spawn(async move { responder.accept(hub_id).await });

    // Its own task, and first on the socket: the accept must be the
    // consumer that wins `msg2`, or the test proves nothing.
    let camper = hub.clone();
    let accept_task = tokio::spawn(async move { camper.accept(absent_peer).await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let connected = hub
        .connect(peer.local_addr(), peer.public_key(), peer.node_id())
        .await;
    let accepted = accept_task.await.expect("accept task panicked");

    assert!(
        accepted.is_err(),
        "the peer that never arrives cannot be accepted, got {accepted:?}",
    );
    connected.expect("a concurrent accept must not swallow the connect's msg2");
    peer_accept
        .await
        .expect("peer accept task panicked")
        .expect("the peer's side of the handshake must complete");

    assert_session_is_real(&hub, &peer).await;
}

/// The diagnosis must survive the attempts that follow it.
///
/// A responder's retry budget is its own, not the initiator's, so a
/// misconfigured peer can fall silent while the responder still has
/// attempts to burn. Every one of those later attempts sees an empty
/// wire and times out with nothing to report — so if the rejection is
/// per-attempt state, the operator gets a bare `handshake timeout` for
/// a key mismatch that WAS diagnosed, three attempts ago, and thrown
/// away.
///
/// One foreign handshake, delivered early and never repeated, is the
/// smallest shape of that.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_decrypt_failure_survives_the_silent_attempts_that_follow_it() {
    let responder = build_node([0xee; 32]).await;
    // A peer that never actually shows up, so nothing but the single
    // foreign datagram below ever reaches the responder.
    let absent_peer = 0x0bad_0bad_0bad_0badu64;

    let node = responder.clone();
    let accept = tokio::spawn(async move { node.accept(absent_peer).await });

    spray_foreign_handshakes(&responder, 1).await;
    await_classified(&responder, 1).await;
    assert!(
        !accept.is_finished(),
        "the rejection must land while the responder still has attempts left, \
         or this witness proves nothing",
    );

    let err = accept
        .await
        .expect("accept task panicked")
        .expect_err("nobody completed a handshake");
    let text = format!("{err:?}");
    assert!(
        text.contains("did not decrypt"),
        "a rejection from an earlier attempt must still name the cause, got {text}",
    );
}

/// Draining must not swallow the diagnosis. A real initiator with the
/// wrong PSK is indistinguishable from a stale foreign `msg1` at this
/// layer, so it is drained too — but the responder's error names the
/// decrypt failure instead of reporting a bare timeout, which is the
/// only signal an operator has that the key exchange itself failed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wrong_psk_initiator_is_reported_as_a_decrypt_failure_not_a_bare_timeout() {
    let responder = build_node([0xbb; 32]).await;
    let stranger = build_node_with_config([0xcc; 32], config_with_psk([0x11u8; 32])).await;

    let r = responder.clone();
    let stranger_id = stranger.node_id();
    let accept = tokio::spawn(async move { r.accept(stranger_id).await });

    // Fails on both sides by construction: the responder cannot read
    // a msg1 keyed with a different PSK, so it never answers.
    let _ = stranger
        .connect(
            responder.local_addr(),
            responder.public_key(),
            responder.node_id(),
        )
        .await
        .expect_err("a PSK mismatch cannot produce a session");

    let err = accept
        .await
        .expect("accept task panicked")
        .expect_err("the responder cannot complete a mismatched handshake");
    let text = format!("{err:?}");
    assert!(
        text.contains("did not decrypt"),
        "the failure must name the decrypt failure, got {text}",
    );
    assert!(
        responder.responder_handshakes_drained() > 0,
        "the mismatched msg1 must have been read and drained",
    );
}
