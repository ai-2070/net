//! Stage 6: the anchor's **separately announced STUN endpoint**.
//!
//! `RtcConfig::stun_addr` binds a second UDP socket that answers STUN
//! and carries nothing else, so an anchor can announce a STUN endpoint
//! that is not its own ICE address. libwebrtc's
//! `UDPPort::OnReadPacket` consumes any datagram from a configured
//! STUN server as a STUN-server response before `GetConnection` looks
//! for a candidate pair, in both directions — which is why the two
//! roles need two tuples rather than a document telling integrators
//! not to collide them.
//!
//! These witnesses are on the driver itself: the socket, its address,
//! and the RTC socket's unchanged behaviour beside it. The
//! announcement, the leaf default and the collision check are their
//! own slices.

#![cfg(feature = "webrtc")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use net::adapter::net::rtc::{
    parse_xor_mapped_address, RtcConfig, RtcDriver, RtcDriverHandle, RtcPeerId, RtcStats,
    STUN_MAGIC_COOKIE,
};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

/// The driver's outputs, kept alive for the test's duration: a
/// dropped receiver would turn every driver send into an error and
/// make the witness about channel teardown instead of STUN.
#[derive(Debug)]
struct Driven {
    handle: RtcDriverHandle,
    _ingress: mpsc::Receiver<(Bytes, RtcPeerId)>,
    _closed: mpsc::Receiver<RtcPeerId>,
}

async fn spawn(config: RtcConfig) -> std::io::Result<Driven> {
    let (ingress_tx, ingress_rx) = mpsc::channel(16);
    let (closed_tx, closed_rx) = mpsc::channel(16);
    let handle = RtcDriver::spawn(
        config,
        "127.0.0.1:0".parse().expect("addr"),
        Arc::new(RtcStats::default()),
        ingress_tx,
        closed_tx,
    )
    .await?;
    Ok(Driven {
        handle,
        _ingress: ingress_rx,
        _closed: closed_rx,
    })
}

fn loopback_ephemeral() -> SocketAddr {
    "127.0.0.1:0".parse().expect("addr")
}

/// An unsolicited RFC 5389 binding request: no attributes, so it is a
/// gathering request and not an ICE connectivity check.
fn binding_request(txid: u8) -> Vec<u8> {
    let mut request = Vec::with_capacity(20);
    request.extend_from_slice(&0x0001u16.to_be_bytes()); // Binding Request
    request.extend_from_slice(&0u16.to_be_bytes()); // no attributes
    request.extend_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
    request.extend_from_slice(&[txid; 12]);
    request
}

/// Send one binding request to `target` from a throwaway socket and
/// return the XOR-MAPPED-ADDRESS it is told, plus the socket's own
/// address.
async fn probe(target: SocketAddr, txid: u8) -> Option<(SocketAddr, SocketAddr)> {
    let client = UdpSocket::bind(loopback_ephemeral())
        .await
        .expect("client socket");
    let mine = client.local_addr().expect("client addr");
    client
        .send_to(&binding_request(txid), target)
        .await
        .expect("send the binding request");
    let mut buf = [0u8; 256];
    let (n, from) = tokio::time::timeout(Duration::from_secs(5), client.recv_from(&mut buf))
        .await
        .ok()?
        .expect("recv");
    assert_eq!(from, target, "the answer must come from the target socket");
    parse_xor_mapped_address(&buf[..n]).map(|mapped| (mapped, mine))
}

/// The second socket answers a binding request with the requester's
/// own address, and it does so **without** `serve_stun`: the flag
/// governs the RTC socket, this endpoint is its own thing.
///
/// Inverse: drop the `stun_addr` arm from `RtcDriver::spawn` (bind
/// nothing extra) — `stun_local_addr()` is `None` and the unwrap
/// fails before a packet is sent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_stun_only_socket_answers_a_binding_request() {
    let driven = spawn(RtcConfig {
        serve_stun: false,
        ..RtcConfig::new()
            .with_bind_addr(loopback_ephemeral())
            .with_stun_addr(loopback_ephemeral())
    })
    .await
    .expect("spawn with a STUN endpoint");

    let stun_addr = driven
        .handle
        .stun_local_addr()
        .expect("a configured STUN bind is resolved post-bind");
    assert_ne!(
        stun_addr.port(),
        0,
        "a `:0` configuration must report the port it actually took, \
         not the wildcard it asked for"
    );

    let (mapped, client) = probe(stun_addr, 0xA5)
        .await
        .expect("the STUN endpoint must answer a binding request");
    assert_eq!(
        mapped, client,
        "XOR-MAPPED-ADDRESS must describe the requester's own reflexive address"
    );

    // R3-B for the second binding: a joined shutdown releases the
    // port, so a successor can rebind an explicit STUN address.
    driven.handle.shutdown_and_join().await;
    UdpSocket::bind(stun_addr)
        .await
        .expect("the STUN port must be free once shutdown has joined");
}

/// The RTC socket is untouched: with `serve_stun` it still answers
/// the diagnostic probe that `UdpBlocked` evidence rests on, and that
/// counter stays specific to `rtc_addr` — traffic to the STUN
/// endpoint is a different published endpoint and is not charged to
/// it.
///
/// Inverse: point the STUN-only loop's responder at the RTC counter
/// (`stats.note_stun_binding_request()` in `stun_only_loop`) — the
/// second `assert_eq!` reads 2 and fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_rtc_socket_still_answers_its_own_probe() {
    let driven = spawn(RtcConfig {
        serve_stun: true,
        ..RtcConfig::new()
            .with_bind_addr(loopback_ephemeral())
            .with_stun_addr(loopback_ephemeral())
    })
    .await
    .expect("spawn with both");
    let stats = Arc::clone(driven.handle.stats());

    let rtc_addr = driven.handle.local_addr();
    let (mapped, client) = probe(rtc_addr, 0xB6)
        .await
        .expect("the diagnostic probe target must answer");
    assert_eq!(
        mapped, client,
        "the RTC socket's responder is unchanged: the requester's own address"
    );
    assert_eq!(
        stats.stun_binding_requests(),
        1,
        "a peer aiming at the published `rtc_addr` is what this counter means"
    );

    // The same request to the STUN endpoint is answered too, and is
    // not counted as an `rtc_addr` hit.
    let stun_addr = driven.handle.stun_local_addr().expect("stun addr");
    let (mapped, client) = probe(stun_addr, 0xC7)
        .await
        .expect("the STUN endpoint must answer");
    assert_eq!(mapped, client);
    assert_eq!(
        stats.stun_binding_requests(),
        1,
        "the STUN endpoint is a different published endpoint; charging its \
         traffic to `rtc_addr` would make the `UdpBlocked` observable ambiguous"
    );

    driven.handle.shutdown_and_join().await;
}

/// Kyra's acceptance boundary 1, in-process half: the announced STUN
/// endpoint is a **distinct** tuple from the announced RTC endpoint.
/// Two ports on one host in one process is the intended shape; what
/// is ruled out is one socket in both roles.
///
/// Inverse: return `Some(self.local_addr)` from `stun_local_addr()`
/// (the "reuse the RTC socket" shortcut) — the inequality fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_two_announced_endpoints_are_different_sockets() {
    let driven = spawn(
        RtcConfig::new()
            .with_bind_addr(loopback_ephemeral())
            .with_stun_addr(loopback_ephemeral()),
    )
    .await
    .expect("spawn with a STUN endpoint");

    let rtc_addr = driven.handle.local_addr();
    let stun_addr = driven.handle.stun_local_addr().expect("stun addr");
    assert_ne!(
        rtc_addr, stun_addr,
        "the STUN endpoint must not be the anchor's ICE address — that is the \
         configuration libwebrtc's receive dispatch breaks"
    );
    // And it is not an adjacent-port guess either: the value is the
    // bind's own truth, whatever the OS gave us.
    assert_ne!(stun_addr.port(), 0);

    driven.handle.shutdown_and_join().await;
}

/// Off unless configured, and configured means *bound*: a contested
/// address fails the spawn rather than announcing an endpoint nothing
/// is listening on, and with `stun_addr: None` the driver takes no
/// second port and has nothing to announce.
///
/// Inverse: make the bind non-fatal (`if let Ok(sock) = ...`) — the
/// `AddrInUse` expectation fails, and an anchor would publish a dead
/// endpoint.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unconfigured_stun_endpoint_binds_nothing() {
    // Hold a port, then ask the driver for it.
    let squatter = UdpSocket::bind(loopback_ephemeral())
        .await
        .expect("squatter socket");
    let contested = squatter.local_addr().expect("squatter addr");

    let err = spawn(
        RtcConfig::new()
            .with_bind_addr(loopback_ephemeral())
            .with_stun_addr(contested),
    )
    .await
    .expect_err("a STUN bind that cannot be taken must fail the spawn");
    assert_eq!(
        err.kind(),
        std::io::ErrorKind::AddrInUse,
        "the second socket is really bound, not assumed: {err}"
    );

    // Same squatter still holding the port: nothing is configured, so
    // the driver binds no second socket and announces nothing.
    let driven = spawn(RtcConfig::new().with_bind_addr(loopback_ephemeral()))
        .await
        .expect("spawn without a STUN endpoint");
    assert_eq!(
        driven.handle.stun_local_addr(),
        None,
        "no configuration, no endpoint: emission has nothing to announce"
    );
    assert_eq!(
        squatter.local_addr().expect("squatter addr"),
        contested,
        "the port the test holds is still the test's"
    );

    driven.handle.shutdown_and_join().await;
}

/// §6.12.1's own configuration, refused at the door: announcing one
/// endpoint as both the RTC address and the STUN address. Two
/// perfectly distinct sockets underneath do not save it — a peer
/// acts on what was announced, and its only symptom would be an ICE
/// timeout with no diagnostic.
///
/// Inverse: delete the `stun_public_addr == public_addr` arm of
/// `RtcConfig::stun_endpoint_conflict` — the spawn succeeds and
/// `expect_err` fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn announcing_one_endpoint_for_both_roles_is_refused() {
    let announced: SocketAddr = "203.0.113.7:7101".parse().expect("addr");
    let err = spawn(RtcConfig {
        public_addr: Some(announced),
        stun_public_addr: Some(announced),
        ..RtcConfig::new()
            .with_bind_addr(loopback_ephemeral())
            .with_stun_addr(loopback_ephemeral())
    })
    .await
    .expect_err("one announced endpoint in both roles must be refused");

    assert_eq!(
        err.kind(),
        std::io::ErrorKind::InvalidInput,
        "a configuration error, not an I/O accident: {err}"
    );
    let text = err.to_string();
    assert!(
        text.contains("stun_public_addr") && text.contains("public_addr"),
        "the error must name both fields: {text}"
    );
    assert!(
        text.contains("203.0.113.7:7101"),
        "the error must name the colliding value: {text}"
    );
    assert!(
        text.contains("distinct UDP endpoints") && text.contains("own STUN server"),
        "the error must say what is wrong and why: {text}"
    );
}

/// The same mistake one level down: one bind for both sockets. It is
/// caught as a configuration error rather than surfacing as the
/// `AddrInUse` the second bind would otherwise produce — which names
/// a port, not the mistake.
///
/// Inverse: delete the `stun_addr == bind_addr` arm — the spawn then
/// fails with `AddrInUse` and the `InvalidInput` assertion fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn binding_one_socket_for_both_roles_is_refused() {
    // An explicit port, because that is what makes the collision
    // detectable: two `:0` binds are two different sockets.
    let shared: SocketAddr = "127.0.0.1:34791".parse().expect("addr");
    let err = spawn(
        RtcConfig::new()
            .with_bind_addr(shared)
            .with_stun_addr(shared),
    )
    .await
    .expect_err("one bind for both sockets must be refused");

    assert_eq!(
        err.kind(),
        std::io::ErrorKind::InvalidInput,
        "the configuration is named, not the port: {err}"
    );
    let text = err.to_string();
    assert!(
        text.contains("stun_addr")
            && text.contains("bind_addr")
            && text.contains("127.0.0.1:34791"),
        "the error must name both fields and the colliding value: {text}"
    );
}

/// The negative: distinct values in both pairs are the ordinary
/// case, and the check refuses nothing it should not — the same IP
/// on different announced ports, which is the intended shape, and
/// two `ip:0` binds, which compare equal and are nonetheless two
/// different sockets because port 0 names no endpoint.
///
/// Inverse: drop the `rtc.port() != 0` exemption from
/// `stun_endpoint_conflict` — the two wildcard binds are read as a
/// collision and this spawn is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn distinct_endpoints_are_not_refused() {
    let driven = spawn(RtcConfig {
        public_addr: Some("203.0.113.7:7101".parse().expect("addr")),
        stun_public_addr: Some("203.0.113.7:3478".parse().expect("addr")),
        ..RtcConfig::new()
            .with_bind_addr("127.0.0.1:0".parse().expect("addr"))
            .with_stun_addr("127.0.0.1:0".parse().expect("addr"))
    })
    .await
    .expect("two ports on one host is the intended configuration");
    assert!(
        driven.handle.stun_local_addr().is_some(),
        "the STUN socket must still be bound"
    );
    driven.handle.shutdown_and_join().await;
}

/// **S6-07.1.** `stun_public_addr` without `stun_addr` is refused,
/// because it would announce an endpoint no socket ever serves.
///
/// The two flags are an endpoint and its NAT override, not two
/// spellings of one thing: `stun_addr` binds, and without it the
/// driver opens no second socket. An anchor that came up anyway put
/// a dead port into a signed announcement, and a browser aiming
/// `iceServers` at it has no symptom but a slow ICE failure. There
/// is no port to guess and nothing to silently strip — the operator
/// meant to serve STUN and one of two flags is missing.
///
/// Inverse: delete the `unserved_stun_endpoint` arm from
/// `RtcConfig::validate` — the spawn succeeds, `expect_err` fails,
/// and the second half below then reads the override back out of a
/// driver that bound nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn announcing_a_stun_endpoint_no_socket_serves_is_refused() {
    let override_addr: SocketAddr = "203.0.113.7:3478".parse().expect("addr");
    let err = spawn(RtcConfig::new().with_stun_public_addr(override_addr))
        .await
        .expect_err("an override with nothing bound behind it must be refused");
    assert_eq!(
        err.kind(),
        std::io::ErrorKind::InvalidInput,
        "the configuration is named, not a socket error: {err}"
    );
    let text = err.to_string();
    assert!(
        text.contains("stun_public_addr")
            && text.contains("stun_addr")
            && text.contains("203.0.113.7:3478"),
        "the error must name both flags and the endpoint that would be \
         announced: {text}"
    );

    // The same rule at the EMISSION point, so no path can announce
    // past the refusal: with no second socket there is nothing to
    // announce instead of, whatever the override says.
    let configured = RtcConfig::new().with_stun_public_addr(override_addr);
    assert_eq!(
        configured.advertised_stun_addr(None),
        None,
        "the bind is checked BEFORE the override is applied"
    );
    let bound: SocketAddr = "127.0.0.1:34793".parse().expect("addr");
    assert_eq!(
        configured.advertised_stun_addr(Some(bound)),
        Some(override_addr),
        "…and with a socket bound, the override is what is announced"
    );
    assert_eq!(
        RtcConfig::new().advertised_stun_addr(Some(bound)),
        Some(bound),
        "no override announces the bind's own truth, so `:0` works"
    );
}

/// **S6-07.2.** The check is on the pair this anchor would
/// ADVERTISE, not on the two pairs that happen to be spelled out.
///
/// RTC `public_addr` `127.0.0.1:7101` beside a STUN *bind* of
/// `127.0.0.1:7101` announces one endpoint in both roles: each half
/// falls back to the other level. Comparing public-with-public and
/// bind-with-bind saw two different pairs and let a known,
/// deliberate collision through — the exact configuration error
/// fail-fast validation exists for.
///
/// Inverse: change `rtc_announced`/`stun_announced` in
/// `RtcConfig::stun_endpoint_conflict` back to `self.public_addr`
/// and `self.stun_public_addr` — the pre-bind `expect` below fails.
/// (The spawn still refuses, because the post-bind
/// `resolved_endpoint_conflict` catches the same pair once both
/// sockets exist. Which is why the pre-bind detection is asserted
/// on its own: §6.12.1 asks for it *before anything is bound*, and
/// a refusal that arrives only after two sockets were taken is a
/// different promise.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_advertised_pair_that_collides_only_after_fallback_is_refused() {
    let shared: SocketAddr = "127.0.0.1:7101".parse().expect("addr");
    let config = RtcConfig {
        public_addr: Some(shared),
        ..RtcConfig::new().with_stun_addr(shared)
    };

    // Detected BEFORE anything is bound, which is what §6.12.1
    // asks for: neither explicit pair collides — there is no
    // `stun_public_addr` and no `bind_addr` — and the pair this
    // anchor would advertise is one endpoint in both roles.
    let pre_bind = config
        .stun_endpoint_conflict()
        .expect("the resolved advertised pair must be detected pre-bind");
    assert!(
        pre_bind.contains("public_addr")
            && pre_bind.contains("stun_addr")
            && pre_bind.contains("127.0.0.1:7101"),
        "the diagnostic must name which fields resolved to the collision: {pre_bind}"
    );

    let err = spawn(config)
        .await
        .expect_err("the resolved advertised pair is one endpoint in both roles");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "{err}");
    assert!(
        err.to_string().contains("127.0.0.1:7101"),
        "the error must name the colliding endpoint: {err}"
    );

    // And the mirror image, where the STUN side carries the
    // override and the RTC side falls back to its bind.
    let err = spawn(RtcConfig {
        stun_public_addr: Some(shared),
        ..RtcConfig::new()
            .with_bind_addr(shared)
            .with_stun_addr("127.0.0.1:0".parse().expect("addr"))
    })
    .await
    .expect_err("…and the other way round");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "{err}");

    // The control: the intended shape — one host, two ports —
    // still starts, so the widened comparison refuses nothing it
    // should not.
    let driven = spawn(RtcConfig {
        public_addr: Some(shared),
        ..RtcConfig::new().with_stun_addr("127.0.0.1:7102".parse().expect("addr"))
    })
    .await
    .expect("two ports on one host is the intended configuration");
    driven.handle.shutdown_and_join().await;
}

/// **S6-07, post-bind.** The resolved pair is checked again once
/// both sockets exist, which is the only check a `:0` bind can be
/// held to: port 0 names no endpoint before binding and exactly one
/// afterwards.
///
/// Driven through the public helper with the bound values supplied,
/// because an OS that happens to place a wildcard STUN bind on the
/// port an RTC `public_addr` names is not a thing a test can
/// arrange — and the input to the check is exactly these two
/// values.
///
/// Inverse: make `resolved_endpoint_conflict` return `None`
/// unconditionally — the first assertion fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_resolved_advertised_pair_is_checked_after_binding() {
    let announced: SocketAddr = "203.0.113.7:7101".parse().expect("addr");
    let rtc_bound: SocketAddr = "127.0.0.1:5001".parse().expect("addr");

    // A `:0` STUN bind the OS placed on the very endpoint the RTC
    // side announces. Pre-bind this configuration is exempt — port
    // 0 names nothing — so this is the arm that catches it.
    let wildcard = RtcConfig {
        public_addr: Some(announced),
        ..RtcConfig::new().with_stun_addr("203.0.113.7:0".parse().expect("addr"))
    };
    assert_eq!(
        wildcard.stun_endpoint_conflict(),
        None,
        "port 0 names no endpoint, so the pre-bind check must stay quiet"
    );
    let conflict = wildcard
        .resolved_endpoint_conflict(rtc_bound, Some(announced))
        .expect("the resolved pair is one endpoint in both roles");
    assert!(
        conflict.contains("203.0.113.7:7101"),
        "the error must name the resolved endpoint: {conflict}"
    );

    // Controls: a different resolved port is fine, and an anchor
    // with no second socket has no pair to collide.
    assert_eq!(
        wildcard
            .resolved_endpoint_conflict(rtc_bound, Some("203.0.113.7:7102".parse().expect("addr"))),
        None
    );
    assert_eq!(wildcard.resolved_endpoint_conflict(rtc_bound, None), None);
}

/// **S6-08.** The STUN socket's release is a recorded fact, so a
/// join taken AFTER the socket and its task are gone finishes
/// instead of waiting for a change that can never come again.
///
/// The lost completion: the spawn-time watch receiver is dropped,
/// and `watch::Sender::send` neither notifies nor stores when there
/// is no receiver. So the guard's `send(true)` returned `Err` and
/// left the value `false`; a later `subscribe()` read `false`, and
/// the no-handle branch waited on a change that had already
/// happened. The port really was released — which is why a witness
/// that only rebinds the port passes over the defect.
///
/// Covers repeated, late, and concurrent joins plus a join after an
/// abort. Each one is bounded by this test's own timeout, so the
/// defect is a failure and not a hang.
///
/// Inverse: restore the pre-repair publish at BOTH places release
/// is recorded for this owner — `StunSocket::drop` and the tail of
/// `TaskRelease::join` — back to `let _ = self.done.send(true)`.
/// Then the late join stalls and its `expect` fails with
/// `Elapsed(())`. Both, because either one alone still stores the
/// value: they are two recorders of one fact, and the pre-repair
/// code had neither. `TaskRelease::join` is shared with the driver
/// task's own owner — one mechanism, two owners — so the sibling
/// witness below reverts the same line beside `SessionTable::drop`.
/// Kyra's extracted-source reproduction
/// (`stun-lifecycle/src/main.rs`) is the same mechanism outside the
/// driver.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_stun_sockets_release_is_observable_by_a_late_joiner() {
    let driven = spawn(RtcConfig::new().with_stun_addr(loopback_ephemeral()))
        .await
        .expect("spawn");
    let stun_addr = driven.handle.stun_local_addr().expect("stun addr");

    // First join: the owner of the handle.
    tokio::time::timeout(Duration::from_secs(5), driven.handle.shutdown_and_join())
        .await
        .expect("the first join must finish");

    // The port is free — the observation that is true both with and
    // without the defect, which is why it is not the assertion.
    UdpSocket::bind(stun_addr)
        .await
        .expect("a successor must be able to rebind the STUN port")
        .local_addr()
        .expect("bound");

    // A LATE join, taken after the socket and the task are gone.
    tokio::time::timeout(
        Duration::from_millis(500),
        driven.handle.shutdown_and_join(),
    )
    .await
    .expect("a late join must observe the recorded release, not wait for it");

    // Repeated, and concurrent: three joiners at once, none of them
    // the original owner.
    tokio::time::timeout(Duration::from_millis(500), async {
        tokio::join!(
            driven.handle.shutdown_and_join(),
            driven.handle.shutdown_and_join(),
            driven.handle.shutdown_and_join(),
        );
    })
    .await
    .expect("concurrent late joiners must all finish");
}

/// **S6-08, the abort path.** `shutdown_detached` aborts from a
/// destructor and cannot await; a join afterwards must still
/// finish.
///
/// This is the branch that includes a task aborted before its first
/// poll: such a task never runs its own guard, so nothing it does
/// records the release. Two things make the join finish anyway —
/// the abort leaves the join handle in place instead of dropping
/// it, so a later joiner can own and await it, and that joiner
/// records the release durably.
///
/// Inverse: restore the pre-repair state of the abort path —
/// `if let Some(task) = self.task.lock().take()` in
/// `TaskRelease::abort` (dropping the handle) **and** `send`
/// instead of `send_replace` in `StunSocket::drop` and at the tail
/// of `TaskRelease::join`. The join then finds no handle,
/// subscribes to a release nobody stored, and its `expect` fails
/// with `Elapsed(())`.
///
/// All three, and the reason is worth stating: with a durable
/// record the handle need not be retained *for a task that ran*,
/// and with the handle retained the record is not needed *by the
/// joiner that owns it*. Only a task aborted before its first poll
/// needs both — it runs no guard, so nothing but a joiner owning
/// the handle can establish that it is gone. This witness does not
/// force that schedule (a multi-threaded runtime polls the task
/// promptly), so it discriminates against the pre-repair
/// combination rather than against the handle-retention arm alone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_join_after_an_aborting_shutdown_still_finishes() {
    let driven = spawn(RtcConfig::new().with_stun_addr(loopback_ephemeral()))
        .await
        .expect("spawn");
    let stun_addr = driven.handle.stun_local_addr().expect("stun addr");
    let rtc_addr = driven.handle.local_addr();

    // The destructor's path: signal, abort, do not await.
    driven.handle.shutdown_detached();

    tokio::time::timeout(Duration::from_secs(5), driven.handle.shutdown_and_join())
        .await
        .expect("a join after an abort must finish");

    // And both ports are genuinely free by the time it returned,
    // which is what the join promises.
    UdpSocket::bind(stun_addr).await.expect("stun port free");
    UdpSocket::bind(rtc_addr).await.expect("rtc port free");
}

/// **S6-08, the PRE-EXISTING analogous defect on the primary RTC
/// join.** Not newly introduced STUN code: `RtcDriverHandle`'s own
/// `done` channel has carried the same unretained-receiver shape
/// since H1, and `await_teardown` waited without a bound for a
/// release that `send` had already failed to store. Repaired by the
/// same mechanism, witnessed separately so the two owners are not
/// confused for one.
///
/// Inverse: restore the pre-repair publish at both of THIS owner's
/// recorders — `SessionTable::drop` and the tail of
/// `TaskRelease::join` — back to `let _ = self.done.send(true)`.
/// The late join on a driver configured with NO STUN socket then
/// stalls and its `expect` fails with `Elapsed(())`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_rtc_drivers_own_release_is_observable_by_a_late_joiner() {
    // No STUN socket at all, so nothing here can pass because of
    // the STUN endpoint's release.
    let driven = spawn(RtcConfig::new().with_bind_addr(loopback_ephemeral()))
        .await
        .expect("spawn");
    assert_eq!(
        driven.handle.stun_local_addr(),
        None,
        "this witness is about the driver task, not the STUN socket"
    );
    let rtc_addr = driven.handle.local_addr();

    tokio::time::timeout(Duration::from_secs(5), driven.handle.shutdown_and_join())
        .await
        .expect("the first join must finish");
    UdpSocket::bind(rtc_addr)
        .await
        .expect("a successor must be able to rebind the RTC port");

    tokio::time::timeout(
        Duration::from_millis(500),
        driven.handle.shutdown_and_join(),
    )
    .await
    .expect("a late join must observe the recorded release");
    tokio::time::timeout(Duration::from_millis(500), async {
        tokio::join!(
            driven.handle.shutdown_and_join(),
            driven.handle.shutdown_and_join(),
        );
    })
    .await
    .expect("concurrent late joiners must all finish");
}

/// **S6-08, the escalation.** A task that CANNOT be aborted must not
/// prevent a joining shutdown from returning.
///
/// This is the defect CI found at the repaired head, and it is not
/// only a test problem: `abort()` is cooperative — it lands at an
/// await point — so a driver wedged in a blocking syscall never
/// reaches one and the cancellation never takes effect. The join's
/// post-abort wait was unbounded, which turned "a task that cannot
/// be cancelled" into "shutdown never returns", for an operator as
/// much as for a witness.
///
/// Bounding it narrows the promise, deliberately and loudly: the
/// release record is published only when the task is genuinely
/// gone, so a caller that returns from a stuck join has been told
/// through `tracing::error!` that the socket is STILL BOUND rather
/// than being told nothing. What must not happen is the caller
/// never returning at all.
///
/// `set_block_loop_ms` blocks the worker thread rather than parking
/// it at an `await`; the existing `set_stall_loop` parks, where
/// abort DOES land, which is why that witness passes either way and
/// this one is a different row.
///
/// Inverse: restore `let _ = handle.await;` (unbounded) after
/// `handle.abort()` in `TaskRelease::join` — this test hangs until
/// its own outer timeout and the `expect` fails, which is exactly
/// what CI observed as `TERMINATING [>180.000s]`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_driver_that_cannot_be_aborted_does_not_hang_the_join() {
    let driven = spawn(RtcConfig::new().with_bind_addr(loopback_ephemeral()))
        .await
        .expect("spawn");
    // Long enough that the join's two bounded waits (2 s each) both
    // elapse well inside it, so the escalation arm is what returns.
    driven.handle.hooks().set_block_loop_ms(12_000);
    // Let the loop reach the block: until it does, this is an
    // ordinary cooperative driver and the row proves nothing.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let started = std::time::Instant::now();
    tokio::time::timeout(Duration::from_secs(8), driven.handle.shutdown_and_join())
        .await
        .expect(
            "a joining shutdown must return even when the task cannot be \
             cancelled — an uncancellable task is a diagnostic, not a hang",
        );
    let waited = started.elapsed();
    assert!(
        waited < Duration::from_secs(8),
        "the join returned only because the outer bound fired: {waited:?}"
    );

    // And it is not silent about it. The release is NOT recorded —
    // the port really is still bound — so a second join must also
    // return bounded rather than parking on a record that will
    // never arrive.
    tokio::time::timeout(Duration::from_secs(8), driven.handle.shutdown_and_join())
        .await
        .expect("a second join must also return, not wait for a release that is pending");

    // Release the wedge so the runtime's own teardown is not the
    // thing this test measures.
    driven.handle.hooks().set_block_loop_ms(0);
}
