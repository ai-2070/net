//! Attempt ownership, terminality and candidate retention, driven on
//! a real leaf in a real browser.
//!
//! **What is real here.** The subject is this module's own `Inner`
//! and the methods above it: `offer_peer`, `peer_accept_offer`,
//! `service_peer`, `settle_peer_deadline`, `harvest_candidates`,
//! `on_handshake`, `settle` and `close` are the production steps a
//! page reaches through `@net-mesh/browser`, and they run here
//! against real `RTCPeerConnection`s that really gather, really
//! answer each other and really open a DataChannel in the engine
//! this test runs in. The peer is a second real
//! [`crate::node::LeafNode`] with a transport of its own: it signs
//! the announcement this leaf verifies, the envelopes it files and
//! the handshakes it admits, so nothing below is accepted on a
//! test's authority.
//!
//! **What is not.** There is no anchor — a leaf cannot reach one
//! from a unit test — so the envelopes this leaf SENDS are dropped
//! at its transport (logged, exactly as production does when a
//! channel is not there) and the ones it RECEIVES are handed to
//! `accept_signal` directly instead of arriving over a relayed
//! session. Where a witness needs the offer to reach the peer, the
//! test carries it: that is the wire's job, not this module's, and
//! the end-to-end exchange through a real anchor is the browser
//! matrix's. What is witnessed here is every decision THIS module
//! takes about who owns an attempt, when one ends, and what is
//! retained across a phase boundary.
//!
//! Run:
//! ```text
//! CHROMEDRIVER=<path> \
//!   CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner \
//!   cargo test --target wasm32-unknown-unknown \
//!   --features mock-control-plane --lib
//! ```

use super::*;
use crate::bootstrap::{AnchorInfo, Credential};
use crate::enroll::Invite;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

/// The trust domain's pre-shared key, test-only and shared by
/// both sides — which is precisely what proves nothing about
/// identity, and why the responder half parks an unproven
/// establishment rather than installing a session.
const PSK: [u8; 32] = [3u8; 32];
const ANCHOR: NodeId = 0x00A1_1C00_0000_0001;

/// How long a real engine is given to produce host candidates.
///
/// Two connections in one page pair on host candidates with no
/// STUN server, which is the same configuration the anchorless
/// witness documents; they appear in a millisecond or two and
/// this is generous rather than tuned.
const GATHER_MS: i32 = 500;

/// This leaf, and the peer it talks about.
struct Pair {
    leaf: LeafNode,
    us: NodeId,
    /// The peer's node: it signs everything this leaf verifies.
    peer: RefCell<crate::node::LeafNode>,
    peer_id: NodeId,
    /// The peer's transport — a second real connection in this
    /// page, so an offer can really be answered and candidates
    /// really gathered.
    peer_transport: RtcLeafTransport,
    /// What the peer's channel delivered, waiting for
    /// [`Pair::pump_peer`].
    peer_inbox: Rc<RefCell<VecDeque<(NodeId, Bytes)>>>,
    /// How many offers the peer has made, so each one is numbered
    /// with a dialog of its own.
    minted: Cell<u64>,
}

fn pair() -> Pair {
    let anchor_identity = LeafIdentity::generate().expect("an anchor identity");
    let anchor_noise = *anchor_identity.noise().public_key();
    let credential = Credential {
        encoded: "net-bootstrap:witness".to_string(),
        invite: Invite {
            root: [7u8; 32],
            nonce: [9u8; 16],
            expires_at: clock::now_unix_secs() + 600,
            rendezvous: "https://anchor.test".to_string(),
        },
        anchor_noise_pubkey: anchor_noise,
        psk: PSK,
        bootstrap_url: "https://anchor.test".to_string(),
        psk_expires_at: clock::now_unix_secs() + 600,
    };
    let invite = credential.invite.clone();
    let info = AnchorInfo {
        node_id: ANCHOR,
        noise_pubkey: anchor_noise,
        rtc_addr: None,
        stun_addr: None,
    };

    // The org proof mint needs the entity keypair SHARED (`Rc`) beside
    // the identity's own copy, and `LeafIdentity` owns its entity half
    // with no way to read the scalar back out — so the witness draws the
    // two secrets itself and builds both, exactly as `identity_from`
    // does at connect (same secret twice, never a second one).
    let entity_secret = random32().expect("an entity secret");
    let identity = LeafIdentity::from_secrets(
        EntityKeypair::from_secret(entity_secret),
        random32().expect("a noise secret"),
    );
    let us = identity.node_id();
    let mut node = crate::node::LeafNode::new(identity, 0x5001);
    let org_keypair = Rc::new(EntityKeypair::from_secret(entity_secret));
    let control = AnchorControlPlane::bind(credential.bootstrap_url.clone(), credential, us, &info)
        .expect("a credential that pins this anchor's live key binds");

    let peer_identity = LeafIdentity::generate().expect("a peer identity");
    let peer_id = peer_identity.node_id();
    let mut peer = crate::node::LeafNode::new(peer_identity, 0x5002);
    let announcement = peer
        .build_announcement(&["chat".to_string()])
        .expect("the peer signs its own announcement");
    assert!(
        node.ingest_announcement(&announcement),
        "the peer's announcement must verify on its own — this leaf's \
         only source for its Noise static key"
    );
    // And this leaf's own, ingested at the peer: §5 discovery is
    // mutual, and the responder's establishment PROOF is only
    // emitted to a peer whose verified announcement says it is a
    // leaf — so a one-way exchange would leave the initiator
    // withholding the one packet that promotes an admission.
    let ours = node
        .build_announcement(&["chat".to_string()])
        .expect("this leaf signs its own announcement");
    assert!(
        peer.ingest_announcement(&ours),
        "and it must verify at the peer on its own"
    );

    // **Both sinks deliver.** The transports are joined by a real
    // DataChannel in the witnesses that open one, so each side's
    // inbound closure has to reach its node the way the bindgen
    // surface's own sink does — through a `Weak`, and queueing
    // rather than re-entering a borrow the pump may hold.
    let reaches: Rc<RefCell<Weak<RefCell<Inner>>>> = Rc::new(RefCell::new(Weak::new()));
    let sink = Rc::clone(&reaches);
    let transport = RtcLeafTransport::new(Rc::new(move |from, bytes| {
        if let Some(inner) = sink.borrow().upgrade() {
            if let Ok(mut guard) = inner.try_borrow_mut() {
                guard.inbox.push_back((from, bytes));
            }
        }
    }));
    let peer_inbox: Rc<RefCell<VecDeque<(NodeId, Bytes)>>> = Rc::new(RefCell::new(VecDeque::new()));
    let peer_sink = Rc::clone(&peer_inbox);
    let peer_transport = RtcLeafTransport::new(Rc::new(move |from, bytes| {
        peer_sink.borrow_mut().push_back((from, bytes));
    }));

    let inner = Rc::new(RefCell::new(Inner {
        node,
        org_keypair,
        serve_queue: Rc::new(RefCell::new(VecDeque::new())),
        transport,
        anchor: ANCHOR,
        control,
        dialog: 0,
        invite,
        psk: PSK,
        // No STUN: two connections in one page reach each other
        // on host candidates, and a STUN server would be a third
        // party these witnesses are about not having.
        ice_servers: Vec::new(),
        offers: HashMap::new(),
        peers: HashMap::new(),
        handshakes: HashMap::new(),
        admissions: HashMap::new(),
        next_routed: 0,
        anchor_candidates: VecDeque::new(),
        // This leaf did not bootstrap through an anchor, so there
        // is no bootstrap attempt to reconcile: `connect` is what
        // counts one, and counting a term for an attempt that was
        // never made would be the inverse of the defect the
        // partition assertion is for.
        bootstrap_settled: true,
        inbox: VecDeque::new(),
        outbox: Vec::new(),
        dispatching: false,
        listeners: Vec::new(),
        next_listener_id: 1,
        closed: false,
        retry: crate::retry::RetryPolicy::new(PEER_ICE_DEADLINE_MS),
        retry_listeners: Vec::new(),
        retry_triggers: Rc::new(RefCell::new(VecDeque::new())),
        retry_last: None,
    }));
    *reaches.borrow_mut() = Rc::downgrade(&inner);

    Pair {
        leaf: LeafNode { inner },
        us,
        peer: RefCell::new(peer),
        peer_id,
        peer_transport,
        peer_inbox,
        minted: Cell::new(0),
    }
}

impl Pair {
    /// Install a session with the peer by hand, so
    /// [`LeafNode::ensure_relayed_session`] has nothing to do and
    /// the offer path runs without an anchor to relay through.
    ///
    /// Real NKpsk0, both halves: the initiator installs on
    /// message 2 (it authenticated the responder's static key),
    /// which is the session §9 step 2 would have produced.
    fn install_session(&self) {
        let noise = *self.peer.borrow().identity().noise().public_key();
        let msg1 = with_node(&self.leaf.inner, |guard| {
            let slot = guard.transport.next_slot();
            guard
                .node
                .begin_handshake(self.peer_id, &PSK, &noise, slot)
                .expect("message 1")
        });
        let msg2 = self
            .peer
            .borrow_mut()
            .accept_handshake(self.us, &PSK, &msg1, 0)
            .expect("message 2");
        with_node(&self.leaf.inner, |guard| {
            guard
                .node
                .complete_handshake(self.peer_id, &msg2)
                .expect("the initiator installs on message 2");
        });
        assert!(self.leaf.inner.borrow().node.has_session(self.peer_id));
    }

    /// Run the peer's half: classify what its channel delivered,
    /// complete its own handshake, and put back whatever it
    /// produced.
    ///
    /// The peer is a plain [`crate::node::LeafNode`], so this is
    /// the loop `Inner::deliver` is for this leaf — and it is what
    /// makes the initiator's establishment PROOF a real packet
    /// this leaf has to verify, rather than a step a test asserts
    /// happened.
    fn pump_peer(&self) {
        let now = clock::now();
        let mut peer = self.peer.borrow_mut();
        while let Some((from, bytes)) = self.peer_inbox.borrow_mut().pop_front() {
            match peer.classify_datagram(from, bytes) {
                Inbound::Refused => {}
                Inbound::Session { from, packet, .. } => peer.on_datagram(from, packet, now),
                Inbound::Handshake { from, packet, .. } => {
                    if peer.is_handshaking(from) {
                        peer.complete_handshake(from, &packet)
                            .expect("the peer completes the handshake it began");
                    }
                }
            }
        }
        peer.tick(now);
        for out in peer.take_outbound() {
            let _ = self.peer_transport.send(out.peer, out.packet);
        }
    }

    /// Hand this leaf an envelope the peer really signed, through
    /// the production path: `accept_signal` checks the signature
    /// against the announcement this leaf verified, its
    /// `not_after` and the replay set, and `collect` files what
    /// survives that where the drive loop looks for it.
    fn deliver(&self, dialog: DialogId, kind: SignalKind, payload: Vec<u8>) {
        let envelope = self
            .peer
            .borrow()
            .sign_signal(self.us, dialog, kind, payload);
        with_node(&self.leaf.inner, |guard| {
            assert!(
                guard.node.accept_signal(envelope, clock::now()),
                "the peer's envelope must verify at its addressee"
            );
            let events = guard.node.drain_events();
            guard.collect(events);
        });
    }

    /// Wait until the channel has actually delivered something to
    /// this leaf's inbox.
    ///
    /// A DataChannel message arrives on a later turn of the event
    /// loop, so a pump that runs first observes nothing and a
    /// witness built on it would assert about a packet that had
    /// not landed.
    async fn await_arrival(&self) {
        for _ in 0..80 {
            if !self.leaf.inner.borrow().inbox.is_empty() {
                return;
            }
            gloo_timer_sleep(TICK_MS).await.ok();
        }
        panic!("the open channel must really carry the packet");
    }

    fn attempt(&self, dialog: DialogId) -> Attempt {
        Attempt {
            peer: self.peer_id,
            dialog,
        }
    }

    async fn offer(&self, within_ms: u64) -> DialogId {
        self.leaf
            .offer_peer(self.peer_id, crate::clock::Deadline::in_ms(within_ms))
            .await
            .unwrap_or_else(|_| panic!("the offer path must run with a session in place"))
    }

    fn transport(&self) -> RtcLeafTransport {
        self.leaf.inner.borrow().transport.clone()
    }

    /// Drive `dialog` to a really open DataChannel.
    ///
    /// The test is the wire, and only the wire: this leaf's offer
    /// never leaves it without an anchor, so the offer is read off
    /// the connection it was set on, answered by the peer's own
    /// engine, and the two candidate sets are carried across. Every
    /// application of an answer or a candidate on this side goes
    /// through `service_peer`, which is the production step.
    async fn open_channel(&self, dialog: DialogId) {
        let transport = self.transport();
        let sdp = local_sdp(
            &transport
                .peer_connection(self.peer_id)
                .expect("the attempt's connection"),
        );
        let answer = self
            .peer_transport
            .accept_offer(self.us, &Sdp(sdp), &[])
            .await
            .expect("the peer answers a real offer");
        self.deliver(dialog, SignalKind::Answer, answer.0.into_bytes());
        for _ in 0..80 {
            for (to, candidate) in transport.take_local_candidates() {
                if to == self.peer_id {
                    let _ = self
                        .peer_transport
                        .add_remote_candidate(self.us, &candidate)
                        .await;
                }
            }
            for (to, candidate) in self.peer_transport.take_local_candidates() {
                if to == self.us {
                    self.deliver(
                        dialog,
                        SignalKind::Candidate,
                        candidate_payload(&candidate).into_bytes(),
                    );
                }
            }
            let reading = self
                .leaf
                .service_peer(self.attempt(dialog))
                .await
                .unwrap_or_else(|_| panic!("the attempt is live"));
            if reading.state == "open" {
                assert!(transport.is_open(self.peer_id));
                return;
            }
            gloo_timer_sleep(TICK_MS).await.ok();
        }
        panic!("the pair must really reach an open DataChannel");
    }

    fn counter(&self, name: &str) -> u64 {
        counter(&self.leaf.counters_json(), name)
    }

    /// The four §10 terminal terms, summed.
    fn terms(&self) -> u64 {
        self.counter("ice_direct")
            + self.counter("ice_relayed")
            + self.counter("ice_failed")
            + self.counter("udp_blocked")
    }

    /// Answer a real offer the peer really made, through
    /// `peer_accept_offer`.
    ///
    /// A fresh dialog each time, so a second call is exactly the
    /// supersession a page performs when a peer re-offers.
    async fn answer_a_real_offer(&self) -> DialogId {
        self.minted.set(self.minted.get() + 1);
        let dialog = 0x0D1A_0000_0000_0000 | self.minted.get();
        let offer = self
            .peer_transport
            .create_offer(self.us, &[])
            .await
            .expect("the peer's offer");
        self.deliver(dialog, SignalKind::Offer, offer.0.into_bytes());
        let answered = self
            .leaf
            .peer_accept_offer(format!("{:016x}", self.peer_id))
            .await
            .unwrap_or_else(|_| panic!("the page answers the verified offer"));
        assert_eq!(answered, format!("{dialog:016x}"));
        dialog
    }

    /// Carry this leaf's ANSWER back to the peer and pair the two,
    /// so an answerer's channel really opens.
    async fn open_answer(&self, dialog: DialogId) {
        let transport = self.transport();
        let answer = local_sdp(
            &transport
                .peer_connection(self.peer_id)
                .expect("the attempt's connection"),
        );
        self.peer_transport
            .accept_answer(self.us, &Sdp(answer))
            .await
            .expect("the peer applies a real answer");
        for _ in 0..80 {
            for (to, candidate) in transport.take_local_candidates() {
                if to == self.peer_id {
                    let _ = self
                        .peer_transport
                        .add_remote_candidate(self.us, &candidate)
                        .await;
                }
            }
            for (to, candidate) in self.peer_transport.take_local_candidates() {
                if to == self.us {
                    self.deliver(
                        dialog,
                        SignalKind::Candidate,
                        candidate_payload(&candidate).into_bytes(),
                    );
                }
            }
            let reading = self
                .leaf
                .service_peer(self.attempt(dialog))
                .await
                .unwrap_or_else(|_| panic!("the attempt is live"));
            if reading.state == "open" {
                return;
            }
            gloo_timer_sleep(TICK_MS).await.ok();
        }
        panic!("the answerer's channel must really open");
    }

    /// The peer initiates §9 step 4 over the open channel.
    fn peer_initiates_direct_handshake(&self) {
        let our_noise = *self
            .leaf
            .inner
            .borrow()
            .node
            .identity()
            .noise()
            .public_key();
        let mut peer = self.peer.borrow_mut();
        let msg1 = peer
            .begin_handshake(self.us, &PSK, &our_noise, 1)
            .expect("the peer's message 1");
        let out = peer.route_outbound(self.us, msg1);
        self.peer_transport
            .send(out.peer, out.packet)
            .expect("the open channel carries message 1");
    }

    fn terminal(&self) -> Option<(IceTerm, &'static str)> {
        self.leaf
            .inner
            .borrow()
            .peers
            .get(&self.peer_id)
            .and_then(|dialog| dialog.terminal)
    }

    fn filed(&self) -> usize {
        self.leaf
            .inner
            .borrow()
            .peers
            .get(&self.peer_id)
            .map_or(0, |dialog| dialog.local.len())
    }
}

/// One counter out of `counters_json()`, which is the only public
/// reader for them.
fn counter(json: &str, name: &str) -> u64 {
    let key = format!("\"{name}\":\"");
    let at = json
        .find(&key)
        .unwrap_or_else(|| panic!("{name} is not in {json}"));
    let rest = &json[at + key.len()..];
    let end = rest.find('"').expect("a closing quote");
    rest[..end].parse().expect("a decimal counter")
}

/// The SDP a connection has set as its local description.
///
/// Read reflectively so this witness needs no additional `web-sys`
/// type: the test is standing in for the wire, and the offer it
/// carries has to be the one the engine actually negotiated
/// against.
fn local_sdp(pc: &web_sys::RtcPeerConnection) -> String {
    let description =
        js_sys::Reflect::get(pc, &JsValue::from_str("localDescription")).expect("localDescription");
    js_sys::Reflect::get(&description, &JsValue::from_str("sdp"))
        .ok()
        .and_then(|sdp| sdp.as_string())
        .expect("the offer was set as the local description")
}

// ─────────────────────── S6-02: ownership ───────────────────────

/// A predecessor's rejected continuation charges no term.
///
/// D1 parks in `create_offer`; D2 supersedes it. D1's rejection
/// path then runs `settle` — and it names D1, so the successor it
/// used to charge is untouched.
#[wasm_bindgen_test]
async fn a_superseded_attempts_rejected_continuation_charges_no_term() {
    let p = pair();
    p.install_session();
    let d1 = p.offer(5_000).await;
    let d2 = p.offer(5_000).await;
    assert_ne!(d1, d2, "a second offer mints its own dialog");
    assert_eq!(p.counter("ice_attempted"), 2);
    let failed = p.counter("ice_failed");
    assert_eq!(failed, 1, "the superseded attempt counted exactly one term");

    with_node(&p.leaf.inner, |guard| {
        guard.settle(p.attempt(d1), IceTerm::Failed, "failed");
    });
    assert_eq!(
        p.counter("ice_failed"),
        failed,
        "a term for a dialog that is no longer live is refused"
    );
    assert_eq!(
        p.terminal(),
        None,
        "and the attempt that replaced it is not settled by it"
    );

    // Control: the attempt that IS live settles, once.
    with_node(&p.leaf.inner, |guard| {
        guard.settle(p.attempt(d2), IceTerm::Failed, "failed");
        guard.settle(p.attempt(d2), IceTerm::Failed, "failed");
    });
    assert_eq!(p.counter("ice_failed"), failed + 1);
    assert_eq!(p.terminal(), Some((IceTerm::Failed, "failed")));
    p.leaf.close();
}

/// A stale dialog is refused **before** it can service the
/// replacement attempt.
///
/// The proxied ownership rule. A follower's request crosses a
/// channel and is served later, so by the time the leader acts the
/// attempt it named may have been superseded. `peer_candidate_in`
/// and `peer_handshake_in` resolve through `attempt_for`, which
/// compares the named dialog against the live one and refuses — so
/// the stale request never reaches `service_peer` at all.
///
/// The oracle is the **replacement's** state, not the refusal: a
/// refusal that arrived after the mutation would satisfy an
/// assertion about the error and miss the entire defect.
#[wasm_bindgen_test]
async fn a_stale_dialog_is_refused_before_it_services_the_replacement() {
    let p = pair();
    p.install_session();
    let d1 = p.offer(5_000).await;
    let d2 = p.offer(5_000).await;
    assert_ne!(d1, d2, "a second offer mints its own dialog");

    // What the replacement has sent so far. If the stale request
    // serviced d2, this moves.
    let before = p
        .leaf
        .service_peer(p.attempt(d2))
        .await
        .unwrap_or_else(|_| panic!("the replacement is live"))
        .sent;

    let peer_hex = format!("{:016x}", p.peer_id);
    let error = p
        .leaf
        .peer_candidate_in(peer_hex.clone(), format!("{d1:016x}"))
        .await
        .expect_err("a stale dialog is refused");
    let text = format!("{:?}", JsValue::from(error));
    assert!(
        text.contains("is not the live attempt"),
        "the refusal names the replacement rather than reporting no attempt: {text}"
    );

    let after = p
        .leaf
        .service_peer(p.attempt(d2))
        .await
        .unwrap_or_else(|_| panic!("the replacement is still live"))
        .sent;
    assert_eq!(
        after, before,
        "the replacement attempt was not serviced by a request issued for its predecessor"
    );

    // And the live dialog still drives its own attempt: the fence
    // refuses the stale request, not the operation.
    p.leaf
        .peer_candidate_in(peer_hex, format!("{d2:016x}"))
        .await
        .expect("the live dialog is served");
}

/// A predecessor's expiry settlement cannot charge its successor.
///
/// The deadline settlement awaits a STUN probe, so the attempt
/// that opened it can be replaced while it is in flight. The
/// settlement carries its dialog now, and the probe's own
/// disposition is unchanged.
#[wasm_bindgen_test]
async fn an_expired_attempts_deadline_settlement_cannot_charge_its_successor() {
    let p = pair();
    p.install_session();
    let d1 = p.offer(5_000).await;
    let d2 = p.offer(5_000).await;
    let relayed = p.counter("ice_relayed");

    let state = p.leaf.settle_peer_deadline(p.attempt(d1)).await;
    assert_eq!(state, "iceTimeout", "no evidence of blocked UDP was found");
    assert_eq!(
        p.counter("ice_relayed"),
        relayed,
        "the expired predecessor charges nothing to the live attempt"
    );
    assert_eq!(p.terminal(), None);

    // Control: the live attempt's own expiry does settle.
    p.leaf.settle_peer_deadline(p.attempt(d2)).await;
    assert_eq!(p.counter("ice_relayed"), relayed + 1);
    assert_eq!(p.terminal(), Some((IceTerm::Relayed, "iceTimeout")));
    p.leaf.close();
}

/// A line is filed only under the attempt whose own connection
/// gathered it.
///
/// Three phases over one real transport, differing only in who
/// owns the live connection: the owner receives what it gathered,
/// a dialog whose connection has been replaced receives nothing,
/// and the same dialog pointed at the live connection fills
/// again.
#[wasm_bindgen_test]
async fn local_candidates_are_filed_only_under_the_connection_that_gathered_them() {
    let p = pair();
    p.install_session();
    let d1 = p.offer(20_000).await;
    let transport = p.transport();

    gloo_timer_sleep(GATHER_MS).await.ok();
    with_node(&p.leaf.inner, |guard| guard.harvest_candidates());
    let owned = p.filed();
    assert!(
        owned >= 1,
        "the owning attempt must receive what its own connection gathered"
    );

    // The transport's connection for this peer is REPLACED — what
    // a successor attempt's `create_offer` does — while the dialog
    // still records the predecessor's.
    transport
        .create_offer(p.peer_id, &[])
        .await
        .expect("a second connection for the same peer");
    gloo_timer_sleep(GATHER_MS).await.ok();
    with_node(&p.leaf.inner, |guard| guard.harvest_candidates());
    assert_eq!(
        p.filed(),
        owned,
        "a line gathered by a connection this dialog does not own is filed nowhere"
    );

    // Control: a third connection, adopted by the dialog, and the
    // same harvest fills it.
    transport
        .create_offer(p.peer_id, &[])
        .await
        .expect("a third connection");
    with_node(&p.leaf.inner, |guard| guard.adopt_connection(p.attempt(d1)));
    gloo_timer_sleep(GATHER_MS).await.ok();
    with_node(&p.leaf.inner, |guard| guard.harvest_candidates());
    assert!(
        p.filed() > owned,
        "the only thing that changed is ownership, so the harvest must fill again"
    );
    p.leaf.close();
}

// ────────────────── S6-04: candidate retention ──────────────────

/// A candidate that arrives before the page answers is retained
/// under the offer's dialog, and applied once there is a remote
/// description for it to be usable against.
///
/// The lines are the peer engine's own and the acceptance is this
/// engine's: `applied` is the count `addIceCandidate` resolved
/// for.
#[wasm_bindgen_test]
async fn a_candidate_that_arrives_before_the_page_answers_is_applied_once_it_can_be() {
    let p = pair();
    p.install_session();
    let dialog: DialogId = 0x0BEE_0000_0000_0001;
    let offer = p
        .peer_transport
        .create_offer(p.us, &[])
        .await
        .expect("the peer's offer");
    p.deliver(dialog, SignalKind::Offer, offer.0.into_bytes());

    gloo_timer_sleep(GATHER_MS).await.ok();
    let lines: Vec<IceCandidate> = p
        .peer_transport
        .take_local_candidates()
        .into_iter()
        .filter(|(to, _)| *to == p.us)
        .map(|(_, candidate)| candidate)
        .collect();
    assert!(
        !lines.is_empty(),
        "the peer's engine must have gathered a line to trickle"
    );
    for candidate in &lines {
        p.deliver(
            dialog,
            SignalKind::Candidate,
            candidate_payload(candidate).into_bytes(),
        );
    }
    assert_eq!(
        p.leaf
            .inner
            .borrow()
            .offers
            .get(&p.peer_id)
            .map(|pending| pending.early.len()),
        Some(lines.len()),
        "with no live attempt to file them on, they are held under the offer"
    );

    // An envelope naming another dialog is not adopted into it.
    p.deliver(
        dialog ^ 1,
        SignalKind::Candidate,
        candidate_payload(&lines[0]).into_bytes(),
    );
    assert_eq!(
        p.leaf
            .inner
            .borrow()
            .offers
            .get(&p.peer_id)
            .map(|pending| pending.early.len()),
        Some(lines.len()),
        "retention is under the exact authorized dialog"
    );

    let answered = p
        .leaf
        .peer_accept_offer(format!("{:016x}", p.peer_id))
        .await
        .unwrap_or_else(|_| panic!("the page answers the verified offer"));
    assert_eq!(answered, format!("{dialog:016x}"));

    let reading = p
        .leaf
        .service_peer(p.attempt(dialog))
        .await
        .unwrap_or_else(|_| panic!("the attempt is live"));
    assert_eq!(
        reading.applied,
        lines.len(),
        "every retained line went into the engine once the answer gave them a \
         remote description"
    );
    assert_eq!(reading.candidate_error, None);
    p.leaf.close();
}

/// An offerer's candidate with no remote description yet is held,
/// not spent.
///
/// The engine is never asked, so there is no refusal to report and
/// no line to lose — which is the difference between this and the
/// `InvalidStateError` a Chromium offerer used to log once per
/// candidate that arrived a poll ahead of the answer.
#[wasm_bindgen_test]
async fn a_candidate_with_no_remote_description_yet_is_held_not_spent() {
    let p = pair();
    p.install_session();
    let d = p.offer(20_000).await;
    // A real line from the peer's engine.
    p.peer_transport
        .create_offer(p.us, &[])
        .await
        .expect("the peer gathers");
    gloo_timer_sleep(GATHER_MS).await.ok();
    let line = p
        .peer_transport
        .take_local_candidates()
        .into_iter()
        .find(|(to, _)| *to == p.us)
        .map(|(_, candidate)| candidate)
        .expect("one gathered line");
    p.deliver(
        d,
        SignalKind::Candidate,
        candidate_payload(&line).into_bytes(),
    );

    let reading = p
        .leaf
        .service_peer(p.attempt(d))
        .await
        .unwrap_or_else(|_| panic!("the attempt is live"));
    assert_eq!(reading.applied, 0);
    assert_eq!(
        reading.candidate_error, None,
        "the engine was never asked, so it refused nothing"
    );
    assert_eq!(
        p.leaf
            .inner
            .borrow()
            .peers
            .get(&p.peer_id)
            .map(|dialog| dialog.deferred.len()),
        Some(1),
        "the line is retained under the attempt that authorized it"
    );
    p.leaf.close();
}

// ─────────────────────── S6-03: terminality ──────────────────────

/// An open channel does not outlive its attempt's deadline.
///
/// The channel really opens — this leaf's offer is carried to the
/// peer's engine, answered, and both candidate sets are exchanged
/// — and then Noise is withheld. Before the repair the reading
/// stayed `open` with `direct:false` and `remainingMs:0` for as
/// long as anybody asked, and `acceptPeer` polled it forever.
#[wasm_bindgen_test]
async fn an_open_channel_does_not_outlive_its_attempts_deadline() {
    let p = pair();
    p.install_session();
    let d = p.offer(4_000).await;
    let attempt = p.attempt(d);
    let transport = p.transport();
    let relayed = p.counter("ice_relayed");

    p.open_channel(d).await;
    assert!(transport.is_open(p.peer_id));
    assert_eq!(p.terminal(), None, "an open channel is not a terminal");

    // Noise withheld: nothing calls the handshake, so no session
    // installs. The attempt must still end at its own deadline.
    let mut ended = None;
    for _ in 0..120 {
        let reading = p
            .leaf
            .service_peer(attempt)
            .await
            .unwrap_or_else(|_| panic!("a live attempt stays readable"));
        if reading.state != "open" && reading.state != "gathering" {
            ended = Some(reading);
            break;
        }
        gloo_timer_sleep(TICK_MS).await.ok();
    }
    let reading = ended.expect("the attempt must end at its deadline with the channel open");
    assert_eq!(
        reading.state, "failed",
        "ICE connected; the session did not"
    );
    assert!(!reading.direct);
    assert_eq!(reading.remaining_ms, 0);
    assert_eq!(
        p.terminal(),
        Some((IceTerm::Relayed, "failed")),
        "the pair keeps its routed session, and the term says so"
    );
    assert_eq!(p.counter("ice_relayed"), relayed + 1);
    assert!(
        !transport.is_open(p.peer_id),
        "the terminal transition closed the attempt's channel"
    );

    // Terminal means terminal: the reading is stable and nothing
    // is counted twice.
    let again = p
        .leaf
        .service_peer(attempt)
        .await
        .unwrap_or_else(|_| panic!("a settled attempt is still readable"));
    assert_eq!(again.state, "failed");
    assert_eq!(p.counter("ice_relayed"), relayed + 1);
    p.leaf.close();
}

/// A retired attempt admits no late establishment, and the
/// unproven one it had is destroyed rather than flagged.
#[wasm_bindgen_test]
async fn a_retired_attempt_admits_no_late_establishment() {
    let p = pair();
    p.install_session();
    let d = p.offer(20_000).await;
    let attempt = p.attempt(d);
    let our_noise = *p.leaf.inner.borrow().node.identity().noise().public_key();

    // The channel really opens, so message 2 really goes back on
    // it: an admission whose answer could not be sent would be a
    // failed attempt rather than a live one holding an unproven
    // establishment.
    p.open_channel(d).await;

    // Control: inside a live attempt the peer's real message 1 is
    // admitted — and what that produces is an UNPROVEN provisional
    // establishment, not a session.
    let msg1 = p
        .peer
        .borrow_mut()
        .begin_handshake(p.us, &PSK, &our_noise, 1)
        .expect("the peer's message 1");
    with_node(&p.leaf.inner, |guard| {
        guard.on_handshake(p.peer_id, &msg1, false);
    });
    assert!(
        p.leaf
            .inner
            .borrow()
            .node
            .provisional_attempt(p.peer_id)
            .is_some(),
        "message 1 parks keys, and only a verified proof promotes them"
    );
    assert_eq!(
        p.leaf.inner.borrow().admissions.get(&p.peer_id).copied(),
        Some(attempt),
        "the admission is owned by the attempt whose channel carried it"
    );

    // The terminal transition retires it, in the same borrow.
    let failed = p.counter("ice_failed");
    with_node(&p.leaf.inner, |guard| {
        guard.settle(attempt, IceTerm::Failed, "failed");
    });
    assert!(
        p.leaf
            .inner
            .borrow()
            .node
            .provisional_attempt(p.peer_id)
            .is_none(),
        "the unproven establishment is dropped with the attempt that admitted it"
    );
    assert!(
        !with_node(&p.leaf.inner, |guard| guard
            .node
            .retire_provisional(p.peer_id)),
        "and the retirement really happened: a second one finds nothing to drop"
    );
    assert!(p.leaf.inner.borrow().admissions.is_empty());

    // A late message 1 for the ended attempt is refused outright.
    let late = p
        .peer
        .borrow_mut()
        .begin_handshake(p.us, &PSK, &our_noise, 2)
        .expect("a second message 1");
    with_node(&p.leaf.inner, |guard| {
        guard.on_handshake(p.peer_id, &late, false);
    });
    assert!(
        p.leaf
            .inner
            .borrow()
            .node
            .provisional_attempt(p.peer_id)
            .is_none(),
        "a direct message 1 after the terminal transition is not admitted"
    );
    assert!(p.leaf.inner.borrow().admissions.is_empty());
    assert_eq!(
        p.counter("ice_failed"),
        failed + 1,
        "and no second term is charged for it"
    );
    p.leaf.close();
}

/// Close reconciles every pending attempt and detaches what it
/// registered.
///
/// The ticker stops with the node, so an attempt still pending
/// here is one the §10 partition never reconciles — and the
/// `online` listener's only consumer is that ticker.
#[wasm_bindgen_test]
async fn close_reconciles_every_pending_attempt_and_detaches_its_listeners() {
    let p = pair();
    p.install_session();
    let _d = p.offer(20_000).await;
    p.deliver(0x0FEE, SignalKind::Offer, b"v=0\r\n".to_vec());
    assert_eq!(p.counter("ice_attempted"), 1);
    assert!(p.terms() < 1, "the attempt is still pending");

    let queue = Rc::clone(&p.leaf.inner.borrow().retry_triggers);
    p.leaf
        .arm_network_retry()
        .unwrap_or_else(|_| panic!("a browser has a window"));
    let window = web_sys::window().expect("a window");
    window
        .dispatch_event(&web_sys::Event::new("online").expect("an event"))
        .expect("dispatch");
    assert_eq!(queue.borrow().len(), 1, "the armed listener files triggers");

    p.leaf.close();

    assert_eq!(
        p.terms(),
        p.counter("ice_attempted"),
        "every attempt this node held is reconciled exactly once"
    );
    {
        let guard = p.leaf.inner.borrow();
        assert!(guard.peers.is_empty(), "no attempt outlives the close");
        assert!(guard.offers.is_empty(), "nor a held offer");
        assert!(guard.handshakes.is_empty(), "nor a pending handshake owner");
        assert!(guard.admissions.is_empty(), "nor an admitted establishment");
        assert!(guard.retry_listeners.is_empty());
    }
    window
        .dispatch_event(&web_sys::Event::new("online").expect("an event"))
        .expect("dispatch");
    assert_eq!(
        queue.borrow().len(),
        1,
        "the listener is detached, not merely unread: a closed node's queue \
         has no consumer left"
    );
}

/// A Noise wait that was parked when its attempt was replaced
/// reports the supersession, and charges the successor nothing.
///
/// The wait is the longest await on this surface. It used to read
/// the CURRENT peer's session and deadline, so a predecessor
/// parked in it could report the successor's installation as its
/// own success — and could settle the successor on its way out.
#[wasm_bindgen_test]
async fn a_parked_noise_wait_reports_the_supersession_and_charges_nothing() {
    let p = pair();
    p.install_session();
    let d1 = p.offer(20_000).await;
    p.open_channel(d1).await;

    // The peer never answers message 1 — its transport's inbound
    // sink is nothing — so the wait really parks.
    let parked = Rc::new(Cell::new(None));
    let reported = Rc::clone(&parked);
    let driving = LeafNode {
        inner: Rc::clone(&p.leaf.inner),
    };
    let attempt = p.attempt(d1);
    wasm_bindgen_futures::spawn_local(async move {
        let outcome = driving.run_handshake(attempt).await;
        reported.set(Some(outcome.is_err()));
    });
    gloo_timer_sleep(150).await.ok();
    assert_eq!(parked.get(), None, "the handshake must still be waiting");
    assert_eq!(
        p.leaf.inner.borrow().handshakes.get(&p.peer_id).copied(),
        Some(HandshakeOwner::Direct(attempt)),
        "and it must own the pending handshake while it waits"
    );

    let failed = p.counter("ice_failed");
    let d2 = p.offer(20_000).await;
    assert_ne!(d1, d2);
    assert_eq!(
        p.counter("ice_failed"),
        failed + 1,
        "the supersession counted the replaced attempt, once"
    );

    // The parked wait now resumes.
    for _ in 0..40 {
        if parked.get().is_some() {
            break;
        }
        gloo_timer_sleep(TICK_MS).await.ok();
    }
    assert_eq!(
        parked.get(),
        Some(true),
        "the wait must end as a refusal, not as the successor's success"
    );
    assert_eq!(
        p.counter("ice_failed"),
        failed + 1,
        "and it must charge the attempt that replaced it nothing"
    );
    assert_eq!(p.terminal(), None, "the successor is still live");
    p.leaf.close();
}

/// A routed establishment that was retired installs nothing
/// afterwards, however valid the message 2 that arrives.
///
/// The failure path used to clear only the relay addressing, so a
/// delayed message 2 installed a session with neither a direct
/// transport nor a relay entry to reach the peer on. The packets
/// here are real NKpsk0; what stands in for the 5 s wait is the
/// revocation that wait performs.
#[wasm_bindgen_test]
async fn a_retired_routed_establishment_installs_nothing_afterwards() {
    let noise_of = |p: &Pair| *p.peer.borrow().identity().noise().public_key();

    // Control: the same packets, the same admission, with the
    // establishment still owned — and the session installs.
    let control = pair();
    let (routed, msg2) = routed_exchange(&control, noise_of(&control));
    assert!(control
        .leaf
        .inner
        .borrow()
        .owns_routed_handshake(control.peer_id, routed));
    with_node(&control.leaf.inner, |guard| {
        guard.on_handshake(control.peer_id, &msg2, true);
    });
    assert!(
        control
            .leaf
            .inner
            .borrow()
            .node
            .has_session(control.peer_id),
        "a live routed establishment does install its session"
    );
    control.leaf.close();

    let p = pair();
    let (routed, msg2) = routed_exchange(&p, noise_of(&p));
    // What the failed wait does: revoke the ownership AND clear
    // the addressing.
    with_node(&p.leaf.inner, |guard| {
        guard.revoke_routed_handshake(p.peer_id, routed);
        guard.node.clear_peer_relay(p.peer_id);
    });
    with_node(&p.leaf.inner, |guard| {
        guard.on_handshake(p.peer_id, &msg2, true);
    });
    assert!(
        !p.leaf.inner.borrow().node.has_session(p.peer_id),
        "a retired routed establishment must install nothing, whatever arrives"
    );
    assert!(p.leaf.inner.borrow().node.peer_relay(p.peer_id).is_none());
    assert_eq!(p.terms(), 0, "and nothing is counted for it");
    p.leaf.close();
}

/// Begin a routed establishment the way
/// [`LeafNode::ensure_relayed_session`] does, and answer it with
/// the peer's real message 2.
fn routed_exchange(p: &Pair, noise: [u8; 32]) -> (u64, Bytes) {
    let msg1 = with_node(&p.leaf.inner, |guard| {
        guard.node.set_peer_relay(p.peer_id, ANCHOR);
        let slot = guard.transport.next_slot();
        let msg1 = guard
            .node
            .begin_handshake(p.peer_id, &PSK, &noise, slot)
            .expect("message 1");
        guard.claim_routed_handshake(p.peer_id);
        msg1
    });
    let routed = match p.leaf.inner.borrow().handshakes.get(&p.peer_id) {
        Some(HandshakeOwner::Routed(routed)) => *routed,
        other => panic!("the routed establishment must own the handshake: {other:?}"),
    };
    let msg2 = p
        .peer
        .borrow_mut()
        .accept_handshake(p.us, &PSK, &msg1, 0)
        .expect("the peer's message 2");
    (routed, msg2)
}

/// A verified inbound establishment is credited to the attempt
/// that admitted it — and the whole §9 step 4 exchange is real.
///
/// The peer initiates over the open channel, this leaf parks the
/// keys, the peer's own `complete_handshake` sends the signed
/// establishment proof, and only THEN is a session installed and
/// a term counted. Nothing here asserts that a step happened:
/// every packet is one node's output and the other's input.
#[wasm_bindgen_test]
async fn a_verified_establishment_is_credited_to_the_attempt_that_admitted_it() {
    let p = pair();
    // A routed session first: the answer to an offer is a signed
    // frame on it, which is §9 step 2 and what step 4 replaces.
    p.install_session();
    let d = p.answer_a_real_offer().await;
    p.open_answer(d).await;
    assert_eq!(
        p.counter("ice_direct"),
        0,
        "an open channel is not a session"
    );

    p.peer_initiates_direct_handshake();
    p.await_arrival().await;
    // Message 1 → provisional; message 2 → the peer's proof; the
    // proof → the promotion. Each hop is one pump on one side.
    for _ in 0..40 {
        with_node(&p.leaf.inner, |guard| guard.pump());
        p.pump_peer();
        if p.terminal().is_some() {
            break;
        }
        gloo_timer_sleep(TICK_MS).await.ok();
    }
    assert_eq!(
        p.terminal(),
        Some((IceTerm::Direct, "open")),
        "the attempt that admitted the establishment is the one credited"
    );
    assert_eq!(p.counter("ice_direct"), 1);
    assert!(p.leaf.inner.borrow().node.has_session(p.peer_id));
    assert!(
        p.leaf.inner.borrow().node.peer_relay(p.peer_id).is_none(),
        "and the pair is off the relay it answered over"
    );
    assert!(
        p.leaf.inner.borrow().admissions.is_empty(),
        "and the admission is consumed, not left for a second promotion"
    );
    let reading = p
        .leaf
        .service_peer(p.attempt(d))
        .await
        .unwrap_or_else(|_| panic!("the attempt is readable"));
    assert!(
        reading.direct,
        "the page sees a direct pair, once it is one"
    );
    p.leaf.close();
}

/// A promotion whose admitting attempt was superseded is credited
/// to nobody — not to the attempt that replaced it.
///
/// The proof is already in this leaf's inbox when the supersession
/// happens, which is the race: the peer proved itself to an
/// attempt that no longer exists. The successor must not be handed
/// a `direct` term it never negotiated, and the retired
/// establishment must install nothing.
#[wasm_bindgen_test]
async fn a_promotion_whose_attempt_was_superseded_is_credited_to_nobody() {
    let p = pair();
    p.install_session();
    let d1 = p.answer_a_real_offer().await;
    p.open_answer(d1).await;
    p.peer_initiates_direct_handshake();
    p.await_arrival().await;

    // One pump admits message 1 and sends message 2; one peer pump
    // completes the peer's handshake and puts its PROOF on the
    // channel, where this leaf's sink queues it — undelivered,
    // because nothing has pumped this side since.
    with_node(&p.leaf.inner, |guard| guard.pump());
    assert_eq!(
        p.leaf.inner.borrow().admissions.get(&p.peer_id).copied(),
        Some(p.attempt(d1)),
        "the admission is owned by the attempt whose channel carried it"
    );
    // The proof is a real packet on a real channel, so it lands on
    // a later turn of the event loop — and nothing pumps THIS side
    // while it does, which is what makes the supersession below a
    // race the proof loses rather than a sequence a test staged.
    let mut in_flight = false;
    for _ in 0..80 {
        p.pump_peer();
        if !p.leaf.inner.borrow().inbox.is_empty() {
            in_flight = true;
            break;
        }
        gloo_timer_sleep(TICK_MS).await.ok();
    }
    assert!(
        in_flight,
        "the peer's proof must be in flight for this to be the race it claims"
    );

    // The page answers a second offer: the first attempt is
    // superseded, counted, and its establishment retired.
    let d2 = p.answer_a_real_offer().await;
    assert_ne!(d1, d2);
    let direct = p.counter("ice_direct");

    for _ in 0..10 {
        with_node(&p.leaf.inner, |guard| guard.pump());
        gloo_timer_sleep(TICK_MS).await.ok();
    }
    assert_eq!(
        p.counter("ice_direct"),
        direct,
        "the proof of a retired attempt credits nothing"
    );
    assert_eq!(
        p.terminal(),
        None,
        "least of all the attempt that replaced it"
    );
    assert!(
        p.leaf.inner.borrow().node.peer_relay(p.peer_id).is_some(),
        "the successor is still answering over the relay: nothing promoted it"
    );
    assert!(
        p.leaf
            .inner
            .borrow()
            .node
            .provisional_attempt(p.peer_id)
            .is_none(),
        "and the retired establishment's keys went with the attempt"
    );
    p.leaf.close();
}

/// Retention is BOUNDED, in both queues, and the line past the bound
/// has a stated fate: it is dropped, the earliest arrivals are kept,
/// and the drop is warned rather than silent.
///
/// A retention buffer with an unasserted cap is memory growth waiting
/// for a peer that trickles a large relay set at a page that never
/// answers. The payloads here are synthetic because what is under
/// test is the ledger's arithmetic, not the engine's opinion of a
/// candidate line; every one still arrives inside an envelope the
/// peer really signed and this leaf really verified.
#[wasm_bindgen_test]
async fn trickled_candidate_retention_is_bounded_in_both_queues() {
    let over = HELD_CANDIDATES + 1;
    let line = |n: usize| {
        candidate_payload(&IceCandidate {
            candidate: format!("candidate:{n} 1 udp 2130706431 192.0.2.{n} 5000 typ host"),
            mid: "0".to_string(),
        })
        .into_bytes()
    };

    // The pending-offer queue: no live attempt to file them on.
    let p = pair();
    p.install_session();
    let dialog: DialogId = 0x0BEE_0000_0000_0002;
    p.deliver(dialog, SignalKind::Offer, b"v=0\r\n".to_vec());
    for n in 0..over {
        p.deliver(dialog, SignalKind::Candidate, line(n));
    }
    assert_eq!(
        p.leaf
            .inner
            .borrow()
            .offers
            .get(&p.peer_id)
            .map(|pending| pending.early.len()),
        Some(HELD_CANDIDATES),
        "the offer holds at most the bound"
    );
    assert_eq!(
        p.leaf
            .inner
            .borrow()
            .offers
            .get(&p.peer_id)
            .and_then(|pending| pending.early.front().cloned()),
        Some(line(0)),
        "and it is the EARLIEST arrivals that are kept: host candidates \
         come first and are what the interval this exists for is about"
    );

    // The live attempt's queue: held for want of a remote description.
    let q = pair();
    q.install_session();
    let d = q.offer(20_000).await;
    for n in 0..over {
        q.deliver(d, SignalKind::Candidate, line(n));
    }
    let reading = q
        .leaf
        .service_peer(q.attempt(d))
        .await
        .unwrap_or_else(|_| panic!("the attempt is live"));
    assert_eq!(reading.applied, 0, "no remote description, nothing applied");
    assert_eq!(
        q.leaf
            .inner
            .borrow()
            .peers
            .get(&q.peer_id)
            .map(|dialog| dialog.deferred.len()),
        Some(HELD_CANDIDATES),
        "the attempt holds at most the bound, and the excess is dropped \
         with a warning rather than queued"
    );
    p.leaf.close();
    q.leaf.close();
}

// ───────── R4-10 / R5-L2: per-peer stream identity, owned subscriptions ─────────

/// A second real peer for this leaf, discovered and established the
/// way [`Pair`]'s first one is.
///
/// Two peers is the whole point: a stream id comes from a label or a
/// publish contract and neither involves the peer, so one label
/// opened to two peers is one id on two sessions. That is the
/// composition R4-10 is about, and it cannot be witnessed with one
/// peer.
fn second_peer(p: &Pair) -> (crate::node::LeafNode, NodeId) {
    let identity = LeafIdentity::generate().expect("a second peer identity");
    let peer_id = identity.node_id();
    let mut peer = crate::node::LeafNode::new(identity, 0x5003);
    let theirs = peer
        .build_announcement(&["chat".to_string()])
        .expect("the second peer signs its own announcement");
    let ours = with_node(&p.leaf.inner, |guard| {
        assert!(
            guard.node.ingest_announcement(&theirs),
            "the second peer's announcement must verify on its own"
        );
        guard
            .node
            .build_announcement(&["chat".to_string()])
            .expect("this leaf signs its own")
    });
    assert!(peer.ingest_announcement(&ours));

    let noise = *peer.identity().noise().public_key();
    let msg1 = with_node(&p.leaf.inner, |guard| {
        let slot = guard.transport.next_slot();
        guard
            .node
            .begin_handshake(peer_id, &PSK, &noise, slot)
            .expect("message 1")
    });
    let msg2 = peer
        .accept_handshake(p.us, &PSK, &msg1, 0)
        .expect("message 2");
    with_node(&p.leaf.inner, |guard| {
        guard
            .node
            .complete_handshake(peer_id, &msg2)
            .expect("the initiator installs on message 2");
    });
    (peer, peer_id)
}

/// Carry every packet this leaf has queued for `peer_id` to `peer`.
///
/// There is no anchor and no channel in these witnesses, so the
/// establishment PROOF the responder is waiting for has to be
/// carried. It is a real packet the peer really verifies: without it
/// the peer holds keys and no attributed session, and nothing it
/// sends would be accepted.
fn carry_to(p: &Pair, peer_id: NodeId, peer: &mut crate::node::LeafNode) {
    let queued = with_node(&p.leaf.inner, |guard| guard.node.take_outbound());
    let now = clock::now();
    for out in queued {
        if out.peer == peer_id {
            peer.on_datagram(p.us, out.packet, now);
        }
    }
    peer.tick(now);
    assert_eq!(
        peer.take_verified_admissions(),
        vec![p.us],
        "the proof is what makes this leaf's session real at the peer"
    );
    peer.drain_events();
}

/// Put everything `peer` has queued through this leaf's real inbound
/// path — the queue its transport closure feeds — and pump.
fn carry_from(p: &Pair, peer_id: NodeId, peer: &mut crate::node::LeafNode) {
    let queued = peer.take_outbound();
    with_node(&p.leaf.inner, |guard| {
        for out in queued {
            guard.inbox.push_back((peer_id, out.packet));
        }
        guard.pump();
    });
}

/// A stream options object as a page would pass one.
fn stream_opts(peer: NodeId, label: &str) -> JsValue {
    js_sys::JSON::parse(&format!(
        "{{\"label\":\"{label}\",\"peer\":\"{peer:016x}\",\"reliability\":\"reliable\"}}"
    ))
    .expect("a stream options object")
}

/// A callback that records the event JSON it is handed.
fn recorder(into: &Rc<RefCell<Vec<String>>>) -> js_sys::Function {
    let sink = Rc::clone(into);
    let closure = Closure::wrap(Box::new(move |json: JsValue| {
        sink.borrow_mut()
            .push(json.as_string().expect("the event JSON string"));
    }) as Box<dyn FnMut(JsValue)>);
    let function: js_sys::Function = closure.as_ref().unchecked_ref::<js_sys::Function>().clone();
    closure.forget();
    function
}

/// **R4-10.** Two wrappers under ONE label on distinct peers each
/// receive their own peer's payload, and not the other's.
///
/// The subject is the production `LeafStream::on_message` filter,
/// registered on a real bindgen node with two real established
/// sessions, fed real packets two real peer nodes produced. Before
/// the peer rode on `StreamData` the filter had only the numeric
/// stream id to work with — and one label is one id — so both
/// wrappers matched both arrivals and one peer's bytes were
/// delivered into the other peer's inbox.
///
/// The assertion is **exact nonce correlation**, per inbox, against
/// the event the production encoder emits. Counts cannot carry this
/// property: in the broken case each inbox holds exactly one entry
/// too, and both hold the same peer's nonce.
#[wasm_bindgen_test]
fn two_streams_under_one_label_do_not_cross_peers() {
    const NONCE_A: &[u8] = b"nonce-for-the-first-peer";
    const NONCE_B: &[u8] = b"nonce-for-the-second-peer";
    const LABEL: &str = "inbox";

    let p = pair();
    p.install_session();
    let mut first = p.peer.borrow_mut();
    carry_to(&p, p.peer_id, &mut first);
    drop(first);
    let (mut second, second_id) = second_peer(&p);
    carry_to(&p, second_id, &mut second);

    // One label, two peers, and the ids really are the same number.
    let to_first = p
        .leaf
        .open_stream(stream_opts(p.peer_id, LABEL))
        .expect("a stream to the first peer");
    let to_second = p
        .leaf
        .open_stream(stream_opts(second_id, LABEL))
        .expect("a stream to the second peer");
    assert_eq!(
        to_first.stream_id_hex(),
        to_second.stream_id_hex(),
        "one label is one id: the derivation does not involve the peer"
    );
    assert_ne!(to_first.peer_node_hex(), to_second.peer_node_hex());
    assert_eq!(to_first.peer_node_hex(), format!("{:016x}", p.peer_id));
    assert_eq!(to_second.peer_node_hex(), format!("{second_id:016x}"));

    let first_inbox: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let second_inbox: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    to_first.on_message(recorder(&first_inbox));
    to_second.on_message(recorder(&second_inbox));

    // Each peer sends its own nonce on the shared id.
    let stream_id = u64::from_str_radix(&to_first.stream_id_hex(), 16).expect("hex");
    for (peer, peer_id, nonce) in [
        (&mut *p.peer.borrow_mut(), p.peer_id, NONCE_A),
        (&mut second, second_id, NONCE_B),
    ] {
        let handle = peer
            .open_stream(
                p.us,
                LABEL,
                crate::stream::Reliability::Reliable,
                Some(stream_id),
                None,
            )
            .expect("the peer's own half");
        peer.stream_send(handle, nonce).expect("the peer sends");
        carry_from(&p, peer_id, peer);
    }

    let expected = |peer_node: NodeId, incarnation: &str, payload: &'static [u8]| {
        LeafEvent::StreamData {
            peer_node,
            incarnation: incarnation.parse().expect("a decimal incarnation"),
            stream_id,
            seq: 0,
            payload: Bytes::from_static(payload),
        }
        .to_json()
    };
    assert_eq!(
        *first_inbox.borrow(),
        vec![expected(p.peer_id, &to_first.incarnation(), NONCE_A)],
        "the first peer's wrapper must hold the first peer's nonce and nothing else"
    );
    assert_eq!(
        *second_inbox.borrow(),
        vec![expected(second_id, &to_second.incarnation(), NONCE_B)],
        "the second peer's wrapper must hold the second peer's nonce and nothing else"
    );

    p.leaf.close();
}

/// **R5-L2.** A stream's subscription is owned: close gives it back.
///
/// `on_message` registers a filter closure on the NODE and the
/// closure captures the consumer's callback, so a close that removed
/// only the node's per-stream state left the registration attached —
/// still invoked for every event the node produced, still holding
/// its consumer alive, for as long as the node lived. Repeated
/// open/close therefore accumulated one permanent closure per open.
///
/// The witness is the count, flat across churn. No heap measurement:
/// the retention is the registration, and the registration is
/// countable.
#[wasm_bindgen_test]
fn repeated_stream_open_and_close_leaves_the_listener_count_flat() {
    let p = pair();
    p.install_session();
    let baseline = p.leaf.listener_count();

    let seen: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    for _ in 0..8 {
        let stream = p
            .leaf
            .open_stream(stream_opts(p.peer_id, "churn"))
            .expect("open");
        stream.on_message(recorder(&seen));
        stream.on_message(recorder(&seen));
        assert_eq!(
            p.leaf.listener_count(),
            baseline + 2,
            "each registration is one listener on the node"
        );
        stream.close();
        assert_eq!(
            p.leaf.listener_count(),
            baseline,
            "close must give back every registration the stream took"
        );
    }
    assert_eq!(p.leaf.listener_count(), baseline);

    // And the token cancels one registration on its own, once.
    let stream = p
        .leaf
        .open_stream(stream_opts(p.peer_id, "churn"))
        .expect("open");
    let token = stream.on_message(recorder(&seen));
    let other = stream.on_message(recorder(&seen));
    assert_ne!(token, other, "a token names exactly one registration");
    assert!(stream.remove_listener(&token));
    assert_eq!(p.leaf.listener_count(), baseline + 1);
    assert!(
        !stream.remove_listener(&token),
        "a second cancellation is already-gone, not a second removal"
    );
    assert!(
        !stream.remove_listener("not-a-token"),
        "an unparseable token names no registration"
    );
    stream.close();
    assert_eq!(p.leaf.listener_count(), baseline);

    p.leaf.close();
}
