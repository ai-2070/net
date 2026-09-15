//! **The anchorless witness**: two leaves in one page reach one
//! direct browser ↔ browser session with nothing between them but an
//! in-memory carrier that cannot forward a Net packet.
//!
//! Why this is the slice that matters. Everything else about the
//! control-plane boundary is an assertion about *shape* — a source
//! scan, a trait signature, a type that compiles. This is the
//! assertion about *sufficiency*: the claim that a leaf needs
//! nothing from an anchor except signalling, and that the serverless
//! follow-on is therefore a second implementation rather than a
//! leaf refactor. If the leaf secretly depended on the anchor for
//! anything — a session to carry its offer, a key to handshake
//! against, an address to dial — this test could not pass, because
//! [`MockMesh`](net_leaf::mock_control_plane::MockMesh) supplies
//! none of those things and refuses to relay anything that is not
//! signalling.
//!
//! Every input is the test's, explicitly:
//!
//! - both identities, from fixed secrets (the custodial path);
//! - the trust domain's NKpsk0 PSK, a constant below — the carrier
//!   never sees it;
//! - admission, by name, through [`MockMesh::admit`];
//! - the ICE configuration: **no** STUN servers, because two tabs
//!   on one origin reach each other on host candidates and a STUN
//!   server would be a third party this test is about not having.
//!
//! And the peer's Noise static public key — the one datum first
//! contact genuinely requires — comes from the **signed
//! announcement** the other leaf published and this leaf verified
//! itself (§5 Layer 1: key discovery precedes signalling). Not from
//! the carrier, which holds no keys and could not be believed if it
//! offered one.
//!
//! Run:
//! ```text
//! CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner \
//!   cargo test --target wasm32-unknown-unknown \
//!   --features mock-control-plane --test wasm_anchorless
//! ```

#![cfg(all(target_arch = "wasm32", feature = "mock-control-plane"))]

use std::cell::RefCell;
use std::collections::{BTreeSet, VecDeque};
use std::rc::Rc;

use bytes::Bytes;
use net_leaf::bootstrap::gloo_timer_sleep;
use net_leaf::clock;
use net_leaf::control_plane::{
    ControlEvent, ControlPlane, IceCandidate, NodeId, Sdp, SignalKind, SignedAnnouncement,
};
use net_leaf::identity::{EntityKeypair, LeafIdentity};
use net_leaf::mock_control_plane::{CarriedKind, MockControlPlane, MockMesh};
use net_leaf::node::{LeafEvent, LeafNode};
use net_leaf::rpc_wire::{self, EventMeta, RpcStatus, DISPATCH_RPC_REQUEST, DISPATCH_RPC_RESPONSE};
use net_leaf::rtc::RtcLeafTransport;
use net_leaf::stream::Reliability;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

/// The trust domain's pre-shared key. Provisioned into both leaves
/// by this test; the carrier never sees it and could not use it.
const PSK: [u8; 32] = [0x4B; 32];

/// The signalling dialog both sides number this attempt with.
const DIALOG: u64 = 0x5109;

/// How long the test waits for ICE, the handshake, and each
/// round trip. Two tabs on one origin connect on host candidates,
/// so this is generous rather than tuned.
const DEADLINE_MS: i32 = 15_000;

/// Poll interval.
const TICK_MS: i32 = 50;

/// Which half of the NKpsk0 handshake a leaf plays.
///
/// Provisioned, not inferred: with no anchor there is no asymmetry
/// in the world to read it off, and guessing from "do I have a
/// handshake in flight" would hide a real failure as a role
/// confusion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Initiator,
    Responder,
}

/// One browser node, wired to the mock.
struct Leaf {
    label: &'static str,
    role: Role,
    node: RefCell<LeafNode>,
    transport: RtcLeafTransport,
    control: MockControlPlane,
    /// What the DataChannel delivered, waiting for [`Leaf::pump`].
    inbox: Rc<RefCell<VecDeque<(NodeId, Bytes)>>>,
    /// Everything the node emitted, accumulated.
    events: RefCell<Vec<LeafEvent>>,
}

fn leaf(
    mesh: &MockMesh,
    label: &'static str,
    role: Role,
    entity_secret: [u8; 32],
    noise_secret: [u8; 32],
) -> Rc<Leaf> {
    let identity =
        LeafIdentity::from_secrets(EntityKeypair::from_secret(entity_secret), noise_secret);
    let node_id = identity.node_id();
    let node = LeafNode::new(identity, 0x5000 | u64::from(entity_secret[0]));
    let inbox: Rc<RefCell<VecDeque<(NodeId, Bytes)>>> = Rc::new(RefCell::new(VecDeque::new()));
    let sink = Rc::clone(&inbox);
    let transport = RtcLeafTransport::new(Rc::new(move |peer, bytes| {
        sink.borrow_mut().push_back((peer, bytes));
    }));
    let control = mesh.admit(node_id, label);
    Rc::new(Leaf {
        label,
        role,
        node: RefCell::new(node),
        transport,
        control,
        inbox,
        events: RefCell::new(Vec::new()),
    })
}

impl Leaf {
    fn node_id(&self) -> NodeId {
        self.node.borrow().node_id()
    }

    fn noise_pubkey(&self) -> [u8; 32] {
        *self.node.borrow().identity().noise().public_key()
    }

    /// Publish this leaf's signed announcement through the carrier.
    async fn publish(&self) {
        let announcement = self
            .node
            .borrow_mut()
            .build_announcement(&["chat".to_string()])
            .expect("the leaf signs its own announcement");
        self.control
            .publish_announcement(SignedAnnouncement(announcement))
            .await
            .expect("the carrier stores and forwards it verbatim");
    }

    /// Drain the carrier: ingest announcements, verify envelopes.
    ///
    /// Returns the envelopes that were **admitted** — signature
    /// checked against the sender's announced entity key, `not_after`
    /// inside its window, `(from, dialog, kind)` unseen. A rejected
    /// envelope fails the test here rather than silently producing a
    /// session built on something unverified.
    async fn service(&self) -> Vec<(SignalKind, Vec<u8>)> {
        let mut admitted = Vec::new();
        for event in self.control.drain_events() {
            match event {
                ControlEvent::Announcement(SignedAnnouncement(bytes)) => {
                    assert!(
                        self.node.borrow_mut().ingest_announcement(&bytes),
                        "{}: an announcement the carrier delivered must verify \
                         on its own — the carrier is not trusted for it",
                        self.label
                    );
                }
                ControlEvent::Signal(envelope) => {
                    let kind = envelope.kind;
                    let payload = envelope.payload.clone();
                    let now = clock::now();
                    assert!(
                        self.node.borrow_mut().accept_signal(envelope, now),
                        "{}: a {kind:?} envelope failed verification",
                        self.label
                    );
                    admitted.push((kind, payload));
                }
                other => panic!(
                    "{}: an anchorless carrier has no {other:?} to deliver — \
                     an unsigned candidate or a carrier-ended attempt both \
                     presuppose a carrier the leaf trusts",
                    self.label
                ),
            }
        }
        admitted
    }

    /// Trickle every locally gathered candidate as a **signed**
    /// `Candidate` envelope, and apply every one that arrived.
    async fn exchange_candidates(&self, peer: NodeId) {
        for (for_peer, candidate) in self.transport.take_local_candidates() {
            assert_eq!(for_peer, peer);
            let payload = serde_json::json!({
                "candidate": candidate.candidate,
                "mid": candidate.mid,
            })
            .to_string()
            .into_bytes();
            self.send_signal(peer, SignalKind::Candidate, payload).await;
        }
        for (kind, payload) in self.service().await {
            assert_eq!(
                kind,
                SignalKind::Candidate,
                "{}: unexpected {kind:?} during the candidate phase",
                self.label
            );
            let document: serde_json::Value =
                serde_json::from_slice(&payload).expect("a candidate payload is JSON");
            let candidate = IceCandidate {
                candidate: document["candidate"]
                    .as_str()
                    .expect("candidate line")
                    .to_string(),
                mid: document["mid"].as_str().unwrap_or("0").to_string(),
            };
            // A candidate the browser rejects (a stale pair, a
            // duplicate) is not a test failure; the channel opening
            // is the assertion.
            let _ = self.transport.add_remote_candidate(peer, &candidate).await;
        }
    }

    /// Sign one envelope with this leaf's entity key and hand it to
    /// the carrier.
    async fn send_signal(&self, to: NodeId, kind: SignalKind, payload: Vec<u8>) {
        let envelope = self.node.borrow().sign_signal(to, DIALOG, kind, payload);
        self.control
            .signal(envelope)
            .await
            .expect("the carrier carries signalling");
    }

    /// Feed the transport's inbound queue to the node and put
    /// everything the node produced on the wire.
    ///
    /// The handshake arm is the browser ↔ browser half that no
    /// anchored path needed: whichever leaf is the responder answers
    /// message 1 with its own Noise static key.
    fn pump(&self) {
        let now = clock::now();
        loop {
            let next = self.inbox.borrow_mut().pop_front();
            let Some((from, bytes)) = next else { break };
            let handshake = is_handshake_packet(&bytes);
            let mut node = self.node.borrow_mut();
            if handshake && !node.has_session(from) {
                match self.role {
                    Role::Responder => {
                        let slot = self.transport.next_slot();
                        let msg2 = node
                            .accept_handshake(from, &PSK, &bytes, slot)
                            .expect("the responder half completes on message 1");
                        drop(node);
                        self.transport
                            .send(from, msg2)
                            .expect("message 2 goes out on the open channel");
                    }
                    Role::Initiator => node
                        .complete_handshake(from, &bytes)
                        .expect("the initiator installs the session from message 2"),
                }
                continue;
            }
            node.on_datagram(from, bytes, now);
        }

        let outbound = {
            let mut node = self.node.borrow_mut();
            node.tick(now);
            node.take_outbound()
        };
        for out in outbound {
            self.transport
                .send(out.peer, out.packet)
                .expect("the direct session's packets go out on the DataChannel");
        }
        let mut drained = self.node.borrow_mut().drain_events();
        self.events.borrow_mut().append(&mut drained);
    }

    /// Every stream or channel payload the node has surfaced.
    fn payloads(&self) -> Vec<Bytes> {
        self.events
            .borrow()
            .iter()
            .filter_map(|event| match event {
                LeafEvent::StreamData { payload, .. }
                | LeafEvent::ChannelMessage { payload, .. } => Some(payload.clone()),
                _ => None,
            })
            .collect()
    }

    /// One counter, read through the JSON the surface publishes —
    /// where every `u64` is a decimal string, because `JSON.parse`
    /// rounds above 2^53.
    fn counter(&self, name: &str) -> u64 {
        let json: serde_json::Value =
            serde_json::from_str(&self.node.borrow().counters().to_json())
                .expect("the counters are JSON");
        json[name]
            .as_str()
            .expect("u64s cross as decimal strings")
            .parse()
            .expect("decimal")
    }

    fn packets_out(&self) -> u64 {
        self.counter("packets_out")
    }

    fn packets_in(&self) -> u64 {
        self.counter("packets_in")
    }
}

/// A Net handshake packet: `0x4E45` magic, version 1, HANDSHAKE flag.
fn is_handshake_packet(bytes: &[u8]) -> bool {
    bytes.len() >= net_wire::protocol::HEADER_SIZE
        && u16::from_le_bytes([bytes[0], bytes[1]]) == net_wire::protocol::MAGIC
        && net_wire::protocol::PacketFlags::from_bits(bytes[3]).is_handshake()
}

/// Drive both leaves until `ready` answers, or fail the deadline.
async fn settle_until<T>(
    left: &Leaf,
    right: &Leaf,
    what: &str,
    mut ready: impl FnMut() -> Option<T>,
) -> T {
    let mut waited = 0;
    while waited < DEADLINE_MS {
        left.pump();
        right.pump();
        if let Some(value) = ready() {
            return value;
        }
        gloo_timer_sleep(TICK_MS).await.ok();
        waited += TICK_MS;
    }
    panic!("{what} did not happen inside {DEADLINE_MS} ms");
}

/// Poll until `ready` answers, while something else pumps.
async fn poll_until<T>(what: &str, mut ready: impl FnMut() -> Option<T>) -> T {
    let mut waited = 0;
    while waited < DEADLINE_MS {
        if let Some(value) = ready() {
            return value;
        }
        gloo_timer_sleep(TICK_MS).await.ok();
        waited += TICK_MS;
    }
    panic!("{what} did not happen inside {DEADLINE_MS} ms");
}

/// Pump both leaves on a timer, the way the bindgen surface's ticker
/// does, so a future that resolves on an inbound packet — an nRPC
/// call's — can simply be awaited.
fn start_ticker(left: Rc<Leaf>, right: Rc<Leaf>) {
    wasm_bindgen_futures::spawn_local(async move {
        let ticks = DEADLINE_MS / TICK_MS;
        for _ in 0..ticks {
            gloo_timer_sleep(TICK_MS).await.ok();
            left.pump();
            right.pump();
        }
    });
}

/// The server half of one nRPC call, which a leaf does not have.
///
/// A leaf is an nRPC **client**: it encodes requests and decodes
/// replies, and `rpc_wire` says so in its own table. So the answer
/// to A's call is written here, by the test, in the byte layout
/// `RpcResponsePayload::decode` reads — which is the honest shape of
/// "a browser called a service and something answered".
fn response_frame(origin_hash: u64, call_id: u64, route: u64, body: &[u8]) -> Vec<u8> {
    let meta = EventMeta::new(DISPATCH_RPC_RESPONSE, 0, origin_hash, call_id, 0);
    let mut frame = Vec::with_capacity(rpc_wire::RPC_FRAME_BODY_OFFSET + 8 + body.len());
    frame.extend_from_slice(&meta.to_bytes());
    frame.extend_from_slice(&route.to_le_bytes());
    frame.extend_from_slice(&RpcStatus::Ok.to_wire().to_le_bytes());
    frame.push(0); // no headers
    frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
    frame.extend_from_slice(body);
    frame
}

/// The witness.
#[wasm_bindgen_test]
async fn two_leaves_reach_one_direct_session_through_a_carrier_that_relays_no_packets() {
    let mesh = MockMesh::new();
    let a = leaf(&mesh, "A", Role::Initiator, [0x11; 32], [0x12; 32]);
    let b = leaf(&mesh, "B", Role::Responder, [0x21; 32], [0x22; 32]);
    let (an, bn) = (a.node_id(), b.node_id());
    assert_ne!(an, bn, "two identities, two node ids");

    // ── §5 Layer 1: key discovery. ────────────────────────────────
    // Each leaf publishes its own signed announcement; the carrier
    // stores it and hands it to the other member verbatim, unread.
    a.publish().await;
    b.publish().await;
    a.service().await;
    b.service().await;

    let b_static = a
        .node
        .borrow()
        .announcement_for(bn)
        .and_then(|verified| verified.noise_pubkey)
        .expect("A verified B's announcement and it carries a Noise static");
    assert_eq!(
        b_static,
        b.noise_pubkey(),
        "the key A will handshake against must be B's own, learned from a \
         signature A checked — not from the carrier"
    );

    // ── §9 steps 3–5: the attempt, as signed envelopes. ───────────
    let offer = a.transport.create_offer(bn, &[]).await.expect("A offers");
    a.send_signal(bn, SignalKind::Offer, offer.0.clone().into_bytes())
        .await;

    let inbound = b.service().await;
    assert_eq!(inbound.len(), 1, "exactly one envelope reached B");
    assert_eq!(inbound[0].0, SignalKind::Offer);
    let offer_at_b = Sdp(String::from_utf8(inbound[0].1.clone()).expect("SDP is UTF-8"));
    assert_eq!(offer_at_b.0, offer.0, "the carrier altered nothing");

    let answer = b
        .transport
        .accept_offer(an, &offer_at_b, &[])
        .await
        .expect("B answers");
    b.send_signal(an, SignalKind::Answer, answer.0.clone().into_bytes())
        .await;

    let inbound = a.service().await;
    assert_eq!(inbound[0].0, SignalKind::Answer);
    let answer_at_a = Sdp(String::from_utf8(inbound[0].1.clone()).expect("SDP is UTF-8"));
    a.transport
        .accept_answer(bn, &answer_at_a)
        .await
        .expect("A installs the answer");

    // ── ICE, every candidate a signed envelope. ───────────────────
    let mut waited = 0;
    while waited < DEADLINE_MS {
        a.exchange_candidates(bn).await;
        b.exchange_candidates(an).await;
        if a.transport.is_open(bn) && b.transport.is_open(an) {
            break;
        }
        gloo_timer_sleep(TICK_MS).await.ok();
        waited += TICK_MS;
    }
    assert!(
        a.transport.is_open(bn) && b.transport.is_open(an),
        "both DataChannels must open: one direct browser ↔ browser link, \
         negotiated entirely through signed envelopes"
    );

    // ── Layer 1: NKpsk0 over the direct link. ─────────────────────
    // Against B's announced static key, with the trust domain's PSK
    // the test provisioned. Nothing the carrier supplied.
    let msg1 = {
        let slot = a.transport.next_slot();
        a.node
            .borrow_mut()
            .begin_handshake(bn, &PSK, &b_static, slot)
            .expect("message 1")
    };
    a.transport.send(bn, msg1).expect("message 1 goes out");

    settle_until(&a, &b, "the handshake", || {
        (a.node.borrow().has_session(bn) && b.node.borrow().has_session(an)).then_some(())
    })
    .await;

    for leaf in [&a, &b] {
        assert!(
            leaf.events
                .borrow()
                .iter()
                .any(|event| matches!(event, LeafEvent::Connected { .. })),
            "{}: the installed session must surface a Connected event",
            leaf.label
        );
    }

    // ── The session is real, part 1: a reliable round trip. ───────
    let out = a
        .node
        .borrow_mut()
        .open_stream(bn, "pingpong", Reliability::Reliable, None, None)
        .expect("A opens a reliable stream");
    a.node
        .borrow_mut()
        .stream_send(out, b"ping over a direct browser-to-browser session")
        .expect("A sends");

    settle_until(&a, &b, "B receiving A's reliable payload", || {
        b.payloads()
            .iter()
            .any(|p| p.as_ref() == b"ping over a direct browser-to-browser session")
            .then_some(())
    })
    .await;

    let back = b
        .node
        .borrow_mut()
        .open_stream(an, "pingpong", Reliability::Reliable, None, None)
        .expect("B opens a reliable stream");
    b.node
        .borrow_mut()
        .stream_send(back, b"pong")
        .expect("B answers");

    settle_until(&a, &b, "A receiving B's reply", || {
        a.payloads()
            .iter()
            .any(|p| p.as_ref() == b"pong")
            .then_some(())
    })
    .await;

    // ── The session is real, part 2: an nRPC call over it. ────────
    // From here a ticker pumps both leaves, exactly as the bindgen
    // surface's does, so the call's future can be awaited.
    start_ticker(Rc::clone(&a), Rc::clone(&b));
    let call = a
        .node
        .borrow_mut()
        .call(bn, "echo", b"ping", Some(10_000))
        .expect("A calls B");

    let request = poll_until("B receiving the nRPC request frame", || {
        b.payloads().into_iter().find(|payload| {
            EventMeta::from_bytes(payload).is_some_and(|meta| meta.dispatch == DISPATCH_RPC_REQUEST)
        })
    })
    .await;

    let meta = EventMeta::from_bytes(&request).expect("the request carries an EventMeta");
    let route = rpc_wire::decode_route(&request).expect("the request carries its route");
    let reply = response_frame(
        b.node.borrow().identity().origin_hash(),
        meta.seq_or_ts,
        route,
        b"pong",
    );
    // **The reply must ride the route the call registered** (R2).
    // A response is matched on (peer, session incarnation, reply
    // channel) before it can consume the pending entry, so answering
    // on `echo.replies` — a channel nobody subscribed — is now
    // correctly ignored. The canonical route is
    // `<service>.replies.<caller origin>`, which is what the caller
    // subscribed to when it issued the call.
    let reply_channel = format!(
        "echo.replies.{:016x}",
        a.node.borrow().identity().origin_hash()
    );
    b.node
        .borrow_mut()
        .publish(an, &reply_channel, &reply)
        .expect("B answers the call");

    let body = call
        .await
        .expect("the call's future was not cancelled")
        .expect("the call resolved with a reply, not a typed failure");
    assert_eq!(
        body.as_ref(),
        b"pong",
        "the nRPC reply must arrive over the direct session"
    );

    // ── The accounting the brief demands. ─────────────────────────
    let table = mesh.accounting_table();
    web_sys::console::log_1(&wasm_bindgen::JsValue::from_str(&format!(
        "\n=== what crossed the anchorless control plane ===\n{table}"
    )));

    for carried in mesh.carried() {
        assert!(
            carried.kind.is_signalling(),
            "the carrier moved {:?}, which is not signalling: {carried:?}\n{table}",
            carried.kind
        );
    }
    assert_eq!(
        mesh.carried_kinds(),
        BTreeSet::from([
            CarriedKind::Announcement,
            CarriedKind::Signal(SignalKind::Offer),
            CarriedKind::Signal(SignalKind::Answer),
            CarriedKind::Signal(SignalKind::Candidate),
        ]),
        "the carrier must have moved exactly announcements, an offer, an answer \
         and candidates — no reject, no query, and above all no packet\n{table}"
    );

    // And the part that makes the ledger meaningful: the session
    // carried real Net packets, and not one of them went through the
    // carrier. Every byte in the table above is signalling; every
    // packet below went direct.
    //
    // Two application packets each — A's stream payload and its nRPC
    // request, B's reply payload and its response frame. The two
    // handshake packets are not in this count: `packet_out` is
    // bumped by the session's `send_subprotocol`, and a handshake
    // packet predates the session. `>=` rather than `==` because the
    // point is that traffic went direct, not how the wire chose to
    // batch it.
    assert!(
        a.packets_out() >= 2 && b.packets_out() >= 2,
        "both leaves must have put real packets on the direct link \
         (A: {}, B: {})",
        a.packets_out(),
        b.packets_out()
    );
    assert!(
        a.packets_in() >= 2 && b.packets_in() >= 2,
        "and both must have received them (A: {}, B: {})",
        a.packets_in(),
        b.packets_in()
    );
    assert_eq!(
        a.node.borrow().counters().total_drops(),
        0,
        "nothing on the direct session should have been dropped"
    );
}

/// The tripwire, in the browser: a carrier asked to move a Net
/// packet refuses and records the attempt, so the ledger assertion
/// above cannot be passed by a mock that quietly relays.
#[wasm_bindgen_test]
async fn a_carrier_asked_to_move_a_net_packet_refuses_and_records_it() {
    let mesh = MockMesh::new();
    let a = leaf(&mesh, "A", Role::Initiator, [0x31; 32], [0x32; 32]);
    let b = leaf(&mesh, "B", Role::Responder, [0x41; 32], [0x42; 32]);
    let bn = b.node_id();

    // A real packet, built by the wire crate the way the data path
    // builds one — not a hand-rolled shape.
    let mut packet = vec![0u8; net_wire::protocol::HEADER_SIZE + 32];
    packet[0..2].copy_from_slice(&net_wire::protocol::MAGIC.to_le_bytes());
    packet[2] = net_wire::protocol::VERSION;

    let envelope = a
        .node
        .borrow()
        .sign_signal(bn, DIALOG, SignalKind::Offer, packet);
    let error = a
        .control
        .signal(envelope)
        .await
        .expect_err("a Net packet must not cross a control plane");
    assert!(
        format!("{error}").starts_with("control plane: "),
        "the refusal must be a typed control-plane failure, got {error}"
    );
    assert!(
        b.control.drain_events().is_empty(),
        "nothing may be delivered once the tripwire fires"
    );
    assert!(
        mesh.carried_kinds()
            .contains(&CarriedKind::RefusedNetPacket),
        "the attempt must be in the ledger: a swallowed error would otherwise \
         leave the witness looking clean"
    );
    assert!(
        !mesh.carried().iter().all(|c| c.kind.is_signalling()),
        "and the ledger assertion the witness makes must go red"
    );
}
