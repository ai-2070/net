//! Native sans-IO lifecycle tests for the org-scoped nRPC streaming
//! caller + provider (`S4Browser` lane 4.5: the leaf caller/provider
//! lifecycle for unary, server-streaming, client-streaming and duplex).
//!
//! Everything here drives frames in and reads frames out with a fake
//! clock (`advance(now)` semantics — one supplied unix-ns timeline),
//! and every fixture proof is minted through `crate::org`. The wire
//! assertions are byte-exact (exact `RpcResponsePayload` equality,
//! exact coarse bytes) — never "does not panic".

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use bytes::Bytes;

use net_leaf::channel::{self, Channel};
use net_leaf::control_plane::NodeId;
use net_leaf::counters::LeafCounters;
use net_leaf::identity::EntityKeypair;
use net_leaf::org::admission::{AdmissionDenied, CoarseAdmissionReason};
use net_leaf::org::cert::{OrgId, OrgKeypair, OrgMembershipCert, OrgRevocationBundle};
use net_leaf::org::entity::EntityId;
use net_leaf::org::grant::{
    CapabilityAuthorityId, DispatcherScope, GrantRights, GrantTargetScope, OrgCapabilityGrant,
    OrgDispatcherGrant,
};
use net_leaf::org::proof::{check_proof_expiry_at, RpcCallShape};
use net_leaf::org::replay::{AdmissionReplayGuard, DEFAULT_MAX_FAILED_ADMISSIONS_PER_PEER};
use net_leaf::org::revocation::RevocationFacts;
use net_leaf::org::MAX_TOKEN_CLOCK_SKEW_SECS;
use net_leaf::rpc::{CallOwner, CallTable};
use net_leaf::rpc_serve::{
    HandlerResult, OpenOutcome, ServeAccess, ServeAdmission, ServeCall, ServeOptions, ServePeer,
    ServeRegistry, SinkError,
};
use net_leaf::rpc_stream::{
    attach_signed_admission, CallHandle, CallOpenError, CallPin, OrgCallIntent, RetireReason,
    StreamCallRegistry, StreamOpen, StreamTerminal,
};
use net_leaf::rpc_wire::{
    self, classify_streaming_chunk, EventMeta, RpcFrame, RpcRequestChunkPayload, RpcRequestPayload,
    RpcResponsePayload, RpcStatus, StreamHandlerResult, StreamingChunkKind,
    FLAG_RPC_CLIENT_STREAMING_REQUEST, FLAG_RPC_REQUEST_END, FLAG_RPC_STREAMING_RESPONSE,
    HEADER_NRPC_STREAMING, HEADER_NRPC_STREAMING_CONTINUE, HEADER_NRPC_STREAMING_END,
    HEADER_NRPC_STREAM_WINDOW_INITIAL,
};

const SERVICE: &str = "svc.loop";
const NOW_SECS: u64 = 1_700_000_000;
const NOW_NS: u64 = 1_700_000_000_000_000_000;
const CALLER_NODE: NodeId = 0x00C0_FFEE_0000_0001;
const PROVIDER_NODE: NodeId = 0xBEEF_0000_0002;
const INC: u64 = 7;
const OLD_INC: u64 = 6;
const OTHER_PEER: NodeId = 0xDEAD_0000_0003;

fn service_capability() -> CapabilityAuthorityId {
    CapabilityAuthorityId::for_tag("nrpc:svc.loop")
}

/// The fixture world: one org root, one caller entity, one provider
/// entity, one session binding. Credential windows are minted around
/// `now_secs`, so both the fake-clock loopback and the real-clock node
/// fixture stay fresh.
struct World {
    org: OrgKeypair,
    caller_kp: Rc<EntityKeypair>,
    caller_entity: EntityId,
    provider_entity: EntityId,
    owner_org: OrgId,
    binding: [u8; 32],
    now_secs: u64,
}

impl World {
    fn new() -> Self {
        Self::at(NOW_SECS)
    }

    fn at(now_secs: u64) -> Self {
        let org = OrgKeypair::from_bytes([0x42; 32]);
        let caller_kp = EntityKeypair::from_secret([0x24; 32]);
        let caller_entity = EntityId::from_bytes(*caller_kp.entity_id());
        Self {
            owner_org: org.org_id(),
            org,
            caller_kp: Rc::new(caller_kp),
            caller_entity,
            provider_entity: EntityId::from_bytes([0x77; 32]),
            binding: [0xAB; 32],
            now_secs,
        }
    }

    fn membership(&self, generation: u32) -> OrgMembershipCert {
        OrgMembershipCert::issue_at(
            &self.org,
            self.caller_entity.clone(),
            generation,
            self.now_secs,
            self.now_secs + 3_600,
            0x1111_2222_3333_4444,
        )
    }

    fn dispatcher(&self) -> OrgDispatcherGrant {
        OrgDispatcherGrant::issue_at(
            &self.org,
            self.caller_entity.clone(),
            DispatcherScope::Exact(service_capability()),
            self.now_secs,
            self.now_secs + 3_600,
            0x5555_6666_7777_8888,
        )
    }

    /// A same-org intent with a membership at `generation`, binding a
    /// specific provider entity.
    fn intent_at(&self, generation: u32, provider: EntityId) -> OrgCallIntent {
        OrgCallIntent {
            keypair: Rc::clone(&self.caller_kp),
            membership: self.membership(generation),
            dispatcher_grant: self.dispatcher(),
            capability_grant: None,
            acting_org: self.owner_org,
            provider_org: self.owner_org,
            provider,
            capability: service_capability(),
            ttl_secs: 30,
        }
    }

    fn intent(&self, generation: u32) -> OrgCallIntent {
        self.intent_at(generation, self.provider_entity.clone())
    }

    /// A granted-mode intent whose capability grant names a DIFFERENT
    /// capability tag (the wrong-tag fixture).
    fn granted_intent_wrong_tag(&self, generation: u32) -> OrgCallIntent {
        let (grant, _secret) = OrgCapabilityGrant::try_issue(
            &self.org,
            self.owner_org,
            CapabilityAuthorityId::for_tag("nrpc:some.other"),
            GrantRights::INVOKE,
            GrantTargetScope::AnyNodeOwnedBy(self.owner_org),
            3_600,
            [0x11; 32],
            None,
            self.now_secs,
            0x9999_AAAA_BBBB_CCCC,
        )
        .expect("grant issues");
        let mut intent = self.intent(generation);
        intent.capability_grant = Some(grant);
        intent
    }
}

/// One frame with the correlation id its `EventMeta` carried.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Outgoing {
    call_id: u64,
    frame: RpcFrame,
}

/// The loopback wire: a `StreamCallRegistry` (caller) talking to a
/// `ServeRegistry` (provider) with full attribution facts and a fake
/// clock.
struct Loop {
    w: World,
    caller: StreamCallRegistry,
    serves: ServeRegistry,
    replay: AdmissionReplayGuard,
    facts: RevocationFacts,
    now: u64,
    served: Rc<RefCell<Vec<ServeCall>>>,
    request_route: u64,
    reply_route: u64,
    carrier: u64,
    #[allow(dead_code)]
    outcomes: Vec<OpenOutcome>,
}

impl Loop {
    fn new() -> Self {
        let w = World::new();
        let origin = w.caller_entity.origin_hash();
        let request_route =
            Channel::from_name(channel::request_channel(SERVICE).expect("channel")).canonical();
        let reply_route =
            Channel::from_name(channel::reply_channel(SERVICE, origin).expect("channel"))
                .canonical();
        let carrier = channel::publish_stream_id(reply_route);
        Self {
            caller: StreamCallRegistry::new(origin, 0x1000),
            serves: ServeRegistry::new(0x5EED),
            replay: AdmissionReplayGuard::with_defaults(),
            facts: RevocationFacts::default(),
            now: NOW_NS,
            served: Rc::new(RefCell::new(Vec::new())),
            request_route,
            reply_route,
            carrier,
            outcomes: Vec::new(),
            w,
        }
    }

    fn serve(&mut self, shape: RpcCallShape, access: ServeAccess) {
        self.serve_with_skew(shape, access, 0);
    }

    fn serve_with_skew(&mut self, shape: RpcCallShape, access: ServeAccess, skew_secs: u64) {
        let calls = Rc::clone(&self.served);
        let opts = ServeOptions {
            shape,
            access,
            provider_owner_org: self.w.owner_org,
            skew_secs,
            default_live_ns: 300 * 1_000_000_000,
            max_live_ns: 3600 * 1_000_000_000,
            policy: None,
        };
        self.serves
            .serve(
                SERVICE,
                opts,
                Rc::new(move |call| calls.borrow_mut().push(call)),
            )
            .expect("serve");
    }

    fn serve_peer(&self) -> ServePeer {
        self.serve_peer_with(Some(self.w.binding))
    }

    fn serve_peer_with(&self, session_binding: Option<[u8; 32]>) -> ServePeer {
        ServePeer {
            peer: CALLER_NODE,
            incarnation: INC,
            caller: self.w.caller_entity.clone(),
            session_binding,
        }
    }

    fn call_owner(&self) -> CallOwner {
        CallOwner {
            peer: PROVIDER_NODE,
            incarnation: INC,
            reply_route: self.reply_route,
            carrier_stream_id: self.carrier,
        }
    }

    fn pin(&self) -> CallPin {
        CallPin {
            peer: PROVIDER_NODE,
            incarnation: INC,
            provider: self.w.provider_entity.clone(),
            request_route: self.request_route,
            reply_route: self.reply_route,
            carrier_stream_id: self.carrier,
        }
    }

    fn open_ss(&mut self, open: StreamOpen, intent: OrgCallIntent) -> (u64, CallHandle) {
        let pin = self.pin();
        let handle = self
            .caller
            .open_server_streaming(pin, SERVICE, open, intent, Some(self.w.binding), self.now)
            .expect("open");
        (handle.call_id, handle)
    }

    fn open_cs(&mut self, open: StreamOpen, intent: OrgCallIntent) -> (u64, CallHandle) {
        let pin = self.pin();
        let handle = self
            .caller
            .open_client_streaming(pin, SERVICE, open, intent, Some(self.w.binding), self.now)
            .expect("open");
        (handle.call_id, handle)
    }

    #[allow(dead_code)]
    fn open_dx(&mut self, open: StreamOpen, intent: OrgCallIntent) -> (u64, CallHandle) {
        let pin = self.pin();
        let handle = self
            .caller
            .open_duplex(pin, SERVICE, open, intent, Some(self.w.binding), self.now)
            .expect("open");
        (handle.call_id, handle)
    }

    /// Decode everything the caller queued (no feeding).
    fn caller_out(&mut self) -> Vec<Outgoing> {
        self.caller
            .take_outbound()
            .into_iter()
            .map(|out| decode_outgoing(out.frame))
            .collect()
    }

    /// Advance the provider (pump + deadline sweep) and decode what it
    /// queued (no feeding).
    fn provider_out(&mut self) -> Vec<Outgoing> {
        self.serves.advance(self.now);
        self.serves
            .take_outbound()
            .into_iter()
            .map(|out| decode_outgoing(out.frame))
            .collect()
    }

    fn feed_provider(&mut self, frames: Vec<Outgoing>) {
        for Outgoing { call_id, frame } in frames {
            let peer = self.serve_peer();
            match frame {
                RpcFrame::Request(req) => {
                    let outcome = self.serves.on_request(
                        &peer,
                        SERVICE,
                        call_id,
                        req,
                        self.now,
                        &ServeAdmission {
                            provider: &self.w.provider_entity,
                            facts: &self.facts,
                            replay: &self.replay,
                        },
                    );
                    self.outcomes.push(outcome);
                }
                RpcFrame::RequestChunk(chunk) => {
                    self.serves.on_chunk(&peer, chunk);
                }
                RpcFrame::StreamGrant { call_id, credits } => {
                    self.serves.on_stream_grant(&peer, call_id, credits);
                }
                RpcFrame::Cancel { call_id } => {
                    self.serves.on_cancel(&peer, call_id);
                }
                other => panic!("the caller emitted a provider-bound frame: {other:?}"),
            }
        }
    }

    fn feed_caller(&mut self, frames: Vec<Outgoing>) {
        for Outgoing { call_id, frame } in frames {
            let owner = self.call_owner();
            match frame {
                RpcFrame::Response { payload, .. } => {
                    self.caller.on_response(owner, call_id, payload);
                }
                RpcFrame::RequestGrant(grant) => {
                    self.caller.on_grant(owner, grant.call_id, grant.credits);
                }
                RpcFrame::DeadlineExceeded { .. } => {
                    self.caller.on_deadline_frame(owner, call_id);
                }
                other => panic!("the provider emitted a caller-bound frame: {other:?}"),
            }
        }
    }

    /// Pump both directions to quiescence.
    fn flush(&mut self) {
        for _ in 0..32 {
            let up = self.caller_out();
            let down = self.provider_out();
            if up.is_empty() && down.is_empty() {
                break;
            }
            self.feed_provider(up);
            self.feed_caller(down);
        }
    }

    /// Craft an opening REQUEST frame exactly as a caller would build
    /// one (optionally minted), for the refusal matrix.
    fn craft_request(
        &self,
        call_id: u64,
        flags: u16,
        body: &[u8],
        headers: Vec<(String, Vec<u8>)>,
        mint: Option<(&OrgCallIntent, RpcCallShape)>,
    ) -> Vec<u8> {
        let mut req = RpcRequestPayload {
            service: SERVICE.to_string(),
            deadline_ns: 0,
            flags,
            headers,
            body: Bytes::copy_from_slice(body),
        };
        if let Some((intent, shape)) = mint {
            attach_signed_admission(
                &mut req,
                intent,
                call_id,
                SERVICE,
                shape,
                Some(self.w.binding),
                self.now,
            )
            .expect("mint");
        }
        rpc_wire::encode_request_frame(
            self.w.caller_entity.origin_hash(),
            call_id,
            self.request_route,
            &req,
        )
        .expect("encode")
    }

    /// Feed one raw opening frame and return the typed outcome.
    fn feed_raw(&mut self, raw: Vec<u8>) -> OpenOutcome {
        let (call_id, req) = decode_request(raw);
        let peer = self.serve_peer();
        let outcome = self.serves.on_request(
            &peer,
            SERVICE,
            call_id,
            req,
            self.now,
            &ServeAdmission {
                provider: &self.w.provider_entity,
                facts: &self.facts,
                replay: &self.replay,
            },
        );
        self.outcomes.push(outcome.clone());
        outcome
    }
}

fn decode_outgoing(frame: Vec<u8>) -> Outgoing {
    let meta = EventMeta::from_bytes(&frame).expect("meta");
    let decoded = rpc_wire::decode_frame(Bytes::from(frame))
        .expect("decode")
        .expect("rpc frame");
    Outgoing {
        call_id: meta.seq_or_ts,
        frame: decoded,
    }
}

fn decode_request(raw: Vec<u8>) -> (u64, RpcRequestPayload) {
    let meta = EventMeta::from_bytes(&raw).expect("meta");
    let frame = rpc_wire::decode_frame(Bytes::from(raw))
        .expect("decode")
        .expect("rpc frame");
    let RpcFrame::Request(req) = frame else {
        panic!("expected an opening REQUEST");
    };
    (meta.seq_or_ts, req)
}

/// Count the terminal-shaped responses in a decoded batch.
fn terminals(frames: &[Outgoing]) -> usize {
    frames
        .iter()
        .filter(|o| match &o.frame {
            RpcFrame::Response { payload, .. } => matches!(
                classify_streaming_chunk(payload),
                StreamingChunkKind::Terminal | StreamingChunkKind::Unary
            ),
            RpcFrame::DeadlineExceeded { .. } => true,
            _ => false,
        })
        .count()
}

/// The response payloads of a decoded batch, in order.
fn responses(frames: &[Outgoing]) -> Vec<RpcResponsePayload> {
    frames
        .iter()
        .filter_map(|o| match &o.frame {
            RpcFrame::Response { payload, .. } => Some(payload.clone()),
            _ => None,
        })
        .collect()
}

fn end_terminal() -> RpcResponsePayload {
    RpcResponsePayload {
        status: RpcStatus::Ok,
        headers: vec![(
            HEADER_NRPC_STREAMING.to_string(),
            HEADER_NRPC_STREAMING_END.to_vec(),
        )],
        body: Bytes::new(),
    }
}

fn continue_chunk(body: &[u8]) -> RpcResponsePayload {
    RpcResponsePayload {
        status: RpcStatus::Ok,
        headers: vec![(
            HEADER_NRPC_STREAMING.to_string(),
            HEADER_NRPC_STREAMING_CONTINUE.to_vec(),
        )],
        body: Bytes::copy_from_slice(body),
    }
}

fn denied(reason: CoarseAdmissionReason) -> RpcResponsePayload {
    RpcResponsePayload {
        status: RpcStatus::AdmissionDenied,
        headers: Vec::new(),
        body: Bytes::copy_from_slice(&[reason.to_wire()]),
    }
}

fn empty_open() -> StreamOpen {
    StreamOpen {
        body: Bytes::new(),
        deadline_ns: 0,
        stream_window_initial: None,
        request_window_initial: None,
    }
}

// ---------------------------------------------------------------------------
// (a) per-shape happy paths: exact item order/content and exactly one terminal
// ---------------------------------------------------------------------------

#[test]
fn server_streaming_delivers_every_item_in_order_then_exactly_one_end_terminal() {
    let mut l = Loop::new();
    l.serve(RpcCallShape::ServerStreaming, ServeAccess::SameOrg);
    let intent = l.w.intent(5);
    let (id, handle) = l.open_ss(
        StreamOpen {
            body: Bytes::from_static(b"request"),
            ..empty_open()
        },
        intent,
    );
    // EAGER: the signed REQUEST is on the wire before the verb returns.
    let up = l.caller_out();
    assert_eq!(up.len(), 1, "eager opening queues exactly one REQUEST");
    assert!(matches!(&up[0].frame, RpcFrame::Request(req) if req.body.as_ref() == b"request"));
    l.feed_provider(up);
    assert_eq!(l.outcomes.last(), Some(&OpenOutcome::Admitted));

    let call = l.served.borrow()[0].clone();
    for item in [b"one".as_slice(), b"two".as_slice(), b"three".as_slice()] {
        call.send(item).expect("send");
    }
    call.finish(StreamHandlerResult::Ok);

    l.flush();
    let items: Vec<Bytes> = (0..8).map_while(|_| l.caller.next_item(id)).collect();
    assert_eq!(
        items,
        vec![
            Bytes::from_static(b"one"),
            Bytes::from_static(b"two"),
            Bytes::from_static(b"three")
        ],
        "exact item order and content"
    );
    assert_eq!(
        l.caller.terminal(id),
        Some(&StreamTerminal::Completed { body: Bytes::new() })
    );
    drop(handle);
    let after = l.caller_out();
    assert!(
        after
            .iter()
            .all(|o| !matches!(o.frame, RpcFrame::Cancel { .. })),
        "a completed call's handle drop emits no CANCEL"
    );
}

#[test]
fn client_streaming_binds_the_first_chunk_into_the_opening_and_returns_one_response() {
    let mut l = Loop::new();
    l.serve(RpcCallShape::ClientStreaming, ServeAccess::SameOrg);
    let intent = l.w.intent(5);
    let (id, handle) = l.open_cs(empty_open(), intent);
    // LAZY: nothing on the wire yet.
    assert_eq!(l.caller_out().len(), 0, "client-streaming opens lazily");

    l.caller.send(id, b"alpha", l.now).expect("send");
    l.caller.send(id, b"beta", l.now).expect("send");
    l.caller.finish_sending(id, l.now).expect("finish");
    let up = l.caller_out();
    assert_eq!(
        up.len(),
        3,
        "REQUEST(first) + chunk(second) + terminal upload"
    );
    match &up[0].frame {
        RpcFrame::Request(req) => {
            assert_eq!(
                req.body.as_ref(),
                b"alpha",
                "the REQUEST body IS the first chunk"
            );
            assert_eq!(
                req.flags & FLAG_RPC_REQUEST_END,
                0,
                "more items were to come"
            );
        }
        other => panic!("expected the opening REQUEST, got {other:?}"),
    }
    match &up[1].frame {
        RpcFrame::RequestChunk(chunk) => {
            assert_eq!(chunk.body.as_ref(), b"beta");
            assert_eq!(chunk.flags & FLAG_RPC_REQUEST_END, 0);
        }
        other => panic!("expected a chunk, got {other:?}"),
    }
    match &up[2].frame {
        RpcFrame::RequestChunk(chunk) => {
            assert_eq!(
                chunk.body.as_ref(),
                b"",
                "the terminal upload frame is empty"
            );
            assert_eq!(chunk.flags & FLAG_RPC_REQUEST_END, FLAG_RPC_REQUEST_END);
        }
        other => panic!("expected the terminal upload frame, got {other:?}"),
    }
    l.feed_provider(up);

    let call = l.served.borrow()[0].clone();
    let uploaded: Vec<Bytes> = (0..8).map_while(|_| call.poll_request()).collect();
    assert_eq!(
        uploaded,
        vec![Bytes::from_static(b"alpha"), Bytes::from_static(b"beta")]
    );
    assert!(call.request_ended(), "EOF is exactly FLAG_END");
    assert_eq!(call.poll_request(), None, "and nothing follows it");

    call.send(b"aggregate").expect("respond");
    call.finish(StreamHandlerResult::Ok);
    let down = l.provider_out();
    assert_eq!(down.len(), 1, "one terminal RESPONSE, always terminal");
    assert_eq!(
        responses(&down),
        vec![RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: Vec::new(),
            body: Bytes::from_static(b"aggregate")
        }],
        "the handler's own response is the terminal, verbatim"
    );
    l.feed_caller(down);
    assert_eq!(
        l.caller.terminal(id),
        Some(&StreamTerminal::Completed {
            body: Bytes::from_static(b"aggregate")
        })
    );
    drop(handle);
}

#[test]
fn the_one_item_opening_ends_on_the_request_frame_itself() {
    let mut l = Loop::new();
    l.serve(RpcCallShape::ClientStreaming, ServeAccess::SameOrg);
    let intent = l.w.intent(5);
    // The one-item path: the queued first item rides the initial
    // REQUEST WITH the end flag — one round trip.
    let (_id, handle) = l.open_cs(
        StreamOpen {
            body: Bytes::from_static(b"solo"),
            ..empty_open()
        },
        intent,
    );
    l.caller.finish_sending(_id, l.now).expect("finish");
    let up = l.caller_out();
    assert_eq!(up.len(), 1, "core's one-item path: one round trip");
    match &up[0].frame {
        RpcFrame::Request(req) => {
            assert_eq!(req.body.as_ref(), b"solo");
            assert_eq!(req.flags & FLAG_RPC_REQUEST_END, FLAG_RPC_REQUEST_END);
        }
        other => panic!("expected the opening REQUEST, got {other:?}"),
    }
    drop(handle);
}

#[test]
fn duplex_keeps_its_upload_and_response_halves_independent() {
    let mut l = Loop::new();
    l.serve(RpcCallShape::Duplex, ServeAccess::SameOrg);
    let intent = l.w.intent(5);
    let (id, handle) = l.open_dx(empty_open(), intent);
    l.caller.send(id, b"u1", l.now).expect("send");
    l.caller.send(id, b"u2", l.now).expect("send");
    l.caller.finish_sending(id, l.now).expect("half-close");
    let up = l.caller_out();
    l.feed_provider(up);

    let call = l.served.borrow()[0].clone();
    let uploaded: Vec<Bytes> = (0..8).map_while(|_| call.poll_request()).collect();
    assert_eq!(
        uploaded,
        vec![Bytes::from_static(b"u1"), Bytes::from_static(b"u2")]
    );
    assert!(call.request_ended());

    // The upload half is closed; the response half is still open and
    // keeps producing — independent halves under one record.
    call.send(b"d1").expect("respond");
    let down = l.provider_out();
    l.feed_caller(down);
    assert_eq!(l.caller.next_item(id), Some(Bytes::from_static(b"d1")));
    assert_eq!(l.caller.terminal(id), None, "the output half is not done");

    call.send(b"d2").expect("respond");
    call.finish(StreamHandlerResult::Ok);
    l.flush();
    assert_eq!(l.caller.next_item(id), Some(Bytes::from_static(b"d2")));
    assert_eq!(
        l.caller.terminal(id),
        Some(&StreamTerminal::Completed { body: Bytes::new() }),
        "exactly one terminal after the items"
    );
    drop(handle);
}

#[test]
fn unary_round_trip_resolves_with_the_aggregate_body() {
    let mut l = Loop::new();
    l.serve(RpcCallShape::Unary, ServeAccess::SameOrg);
    let intent = l.w.intent(5);

    // The unary caller: the existing CallTable path + the shared mint.
    let mut table = CallTable::with_seed(1);
    let owner = l.call_owner();
    let (call_id, mut rx) = table
        .register(owner, l.request_route, 30_000)
        .expect("register");
    let raw = l.craft_request(
        call_id,
        0,
        b"ping",
        Vec::new(),
        Some((&intent, RpcCallShape::Unary)),
    );
    assert_eq!(l.feed_raw(raw), OpenOutcome::Admitted);

    let call = l.served.borrow()[0].clone();
    assert_eq!(call.poll_request(), Some(Bytes::from_static(b"ping")));
    assert!(call.request_ended(), "unary upload is complete by shape");
    call.send(b"pong").expect("respond");
    call.finish(StreamHandlerResult::Ok);

    let down = l.provider_out();
    assert_eq!(down.len(), 1, "exactly one RESPONSE, always terminal");
    assert_eq!(
        responses(&down),
        vec![RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: Vec::new(),
            body: Bytes::from_static(b"pong")
        }]
    );
    let counters = LeafCounters::new();
    assert!(table.deliver(down.into_iter().next().unwrap().frame, owner, &counters));
    assert_eq!(
        rx.try_recv().expect("receiver"),
        Some(Ok(Bytes::from_static(b"pong"))),
        "the unary future resolves with the aggregate body"
    );
}

// ---------------------------------------------------------------------------
// (b) the typed admission refusal set — EXACT reasons
// ---------------------------------------------------------------------------

#[test]
fn a_missing_proof_is_missing_header_and_garbage_is_malformed_proof() {
    let mut l = Loop::new();
    l.serve(RpcCallShape::ServerStreaming, ServeAccess::SameOrg);

    let raw = l.craft_request(0x2001, FLAG_RPC_STREAMING_RESPONSE, b"x", Vec::new(), None);
    assert_eq!(
        l.feed_raw(raw),
        OpenOutcome::Denied(AdmissionDenied::MissingHeader)
    );
    let down = l.provider_out();
    assert_eq!(
        responses(&down),
        vec![denied(CoarseAdmissionReason::Denied)]
    );

    let raw = l.craft_request(
        0x2002,
        FLAG_RPC_STREAMING_RESPONSE,
        b"x",
        vec![("net-org-admission".to_string(), b"garbage".to_vec())],
        None,
    );
    assert_eq!(
        l.feed_raw(raw),
        OpenOutcome::Denied(AdmissionDenied::MalformedProof)
    );
    let down = l.provider_out();
    assert_eq!(
        responses(&down),
        vec![denied(CoarseAdmissionReason::Denied)]
    );
}

#[test]
fn a_wrong_capability_tag_is_capability_mismatch() {
    let mut l = Loop::new();
    l.serve(RpcCallShape::ServerStreaming, ServeAccess::Granted);
    let intent = l.w.granted_intent_wrong_tag(5);
    let raw = l.craft_request(
        0x2003,
        FLAG_RPC_STREAMING_RESPONSE,
        b"x",
        Vec::new(),
        Some((&intent, RpcCallShape::ServerStreaming)),
    );
    assert_eq!(
        l.feed_raw(raw),
        OpenOutcome::Denied(AdmissionDenied::CapabilityMismatch)
    );
    let down = l.provider_out();
    assert_eq!(
        responses(&down),
        vec![denied(CoarseAdmissionReason::Denied)]
    );
}

#[test]
fn a_proof_bound_to_another_provider_is_binding_invalid() {
    let mut l = Loop::new();
    l.serve(RpcCallShape::ServerStreaming, ServeAccess::SameOrg);
    let mut intent = l.w.intent(5);
    // The proof names a different `callee` than the provider it lands
    // on: the reconstructed binding cannot verify the signature.
    intent.provider = EntityId::from_bytes([0x99; 32]);
    let raw = l.craft_request(
        0x2004,
        FLAG_RPC_STREAMING_RESPONSE,
        b"x",
        Vec::new(),
        Some((&intent, RpcCallShape::ServerStreaming)),
    );
    assert_eq!(
        l.feed_raw(raw),
        OpenOutcome::Denied(AdmissionDenied::BindingInvalid)
    );
}

#[test]
fn a_different_session_binding_never_admits() {
    let mut l = Loop::new();
    l.serve(RpcCallShape::ServerStreaming, ServeAccess::SameOrg);
    let intent = l.w.intent(5);
    let raw = l.craft_request(
        0x2005,
        FLAG_RPC_STREAMING_RESPONSE,
        b"x",
        Vec::new(),
        Some((&intent, RpcCallShape::ServerStreaming)),
    );
    // The SAME call_id and proof arriving on a session whose handshake
    // hash is not the signed one (or a hand-built session, `None`):
    // `SessionBindingMismatch`, both ways.
    let (call_id, req) = decode_request(raw.clone());
    let outcome = l.serves.on_request(
        &l.serve_peer_with(Some([0xCC; 32])),
        SERVICE,
        call_id,
        req,
        l.now,
        &ServeAdmission {
            provider: &l.w.provider_entity,
            facts: &l.facts,
            replay: &l.replay,
        },
    );
    assert_eq!(
        outcome,
        OpenOutcome::Denied(AdmissionDenied::SessionBindingMismatch)
    );

    let (call_id, req) = decode_request(raw);
    let outcome = l.serves.on_request(
        &l.serve_peer_with(None),
        SERVICE,
        call_id,
        req,
        l.now,
        &ServeAdmission {
            provider: &l.w.provider_entity,
            facts: &l.facts,
            replay: &l.replay,
        },
    );
    assert_eq!(
        outcome,
        OpenOutcome::Denied(AdmissionDenied::SessionBindingMismatch),
        "a session with no binding can never admit a protected stream"
    );
}

#[test]
fn flag_shape_mismatch_is_shape_mismatch_and_proof_kind_mismatch_is_shape_mismatch() {
    // (1) A CS-flagged REQUEST on a server-streaming registration: a
    // streaming registration's wrong flags are `ShapeMismatch`
    // (coarse `Denied`) — the core mapping (§1.5 step 4(b) + the
    // folds' contract-4 flag checks). RENAMED and RE-PINNED from the
    // old blanket `StreamingUnsupported`/`NotSupported`, which
    // contradicted core (review LEAF-22).
    let mut l = Loop::new();
    l.serve(RpcCallShape::ServerStreaming, ServeAccess::SameOrg);
    let intent = l.w.intent(5);
    let raw = l.craft_request(
        0x2006,
        FLAG_RPC_CLIENT_STREAMING_REQUEST,
        b"x",
        Vec::new(),
        Some((&intent, RpcCallShape::ClientStreaming)),
    );
    assert_eq!(
        l.feed_raw(raw),
        OpenOutcome::Denied(AdmissionDenied::ShapeMismatch),
        "the flags claim a shape this registration does not serve"
    );
    let down = l.provider_out();
    assert_eq!(
        responses(&down),
        vec![denied(CoarseAdmissionReason::Denied)],
        "typed ShapeMismatch refusal, coarse `Denied` byte, byte-exact"
    );

    // (2) A proof whose KIND disagrees with the flags (kind 3 proof on
    // CS flags): the structural flag check passes and admission's
    // shape check refuses it.
    let mut l = Loop::new();
    l.serve(RpcCallShape::ClientStreaming, ServeAccess::SameOrg);
    let intent = l.w.intent(5);
    let raw = l.craft_request(
        0x2007,
        FLAG_RPC_CLIENT_STREAMING_REQUEST,
        b"x",
        Vec::new(),
        Some((&intent, RpcCallShape::Duplex)),
    );
    assert_eq!(
        l.feed_raw(raw),
        OpenOutcome::Denied(AdmissionDenied::ShapeMismatch)
    );
    let down = l.provider_out();
    assert_eq!(
        responses(&down),
        vec![denied(CoarseAdmissionReason::Denied)]
    );
}

#[test]
fn streaming_flags_on_a_unary_registration_stay_streaming_unsupported() {
    // §1.5 step 4(a) is preserved: only a UNARY registration's
    // streaming flags read `StreamingUnsupported` (coarse
    // `NotSupported`) — "this service does not stream" must read
    // differently from "your flags do not match your registration".
    let mut l = Loop::new();
    l.serve(RpcCallShape::Unary, ServeAccess::SameOrg);
    let intent = l.w.intent(5);
    let raw = l.craft_request(
        0x2030,
        FLAG_RPC_STREAMING_RESPONSE,
        b"x",
        Vec::new(),
        Some((&intent, RpcCallShape::ServerStreaming)),
    );
    assert_eq!(
        l.feed_raw(raw),
        OpenOutcome::Denied(AdmissionDenied::StreamingUnsupported),
        "a unary registration never admits streaming flags"
    );
    let down = l.provider_out();
    assert_eq!(
        responses(&down),
        vec![denied(CoarseAdmissionReason::NotSupported)],
        "typed NotSupported refusal, byte-exact"
    );
}

#[test]
fn a_replayed_call_id_after_completion_is_replay_denied() {
    let mut l = Loop::new();
    l.serve(RpcCallShape::ServerStreaming, ServeAccess::SameOrg);
    let intent = l.w.intent(5);
    let raw = l.craft_request(
        0x2008,
        FLAG_RPC_STREAMING_RESPONSE,
        b"x",
        Vec::new(),
        Some((&intent, RpcCallShape::ServerStreaming)),
    );
    assert_eq!(l.feed_raw(raw.clone()), OpenOutcome::Admitted);

    // Run the call to its one terminal, so the key is no longer live.
    let call = l.served.borrow()[0].clone();
    call.finish(StreamHandlerResult::Ok);
    let down = l.provider_out();
    assert_eq!(terminals(&down), 1);
    assert_eq!(l.serves.live_calls(), 0);

    // The IDENTICAL opening again: the active registry is clear, the
    // replay guard is not.
    assert_eq!(
        l.feed_raw(raw),
        OpenOutcome::Denied(AdmissionDenied::Replay),
        "a late replay of an admitted proof is refused while retained"
    );
}

#[test]
fn a_used_proof_is_not_reusable_inside_the_final_sub_ms_of_the_max_skew_window() {
    // Retention (proof expiry + `MAX_TOKEN_CLOCK_SKEW_SECS`) must
    // STRICTLY dominate every acceptance window: inside the window's
    // final sub-millisecond `check_proof_expiry_at` still accepts, so
    // the used entry must still be retained. The old ms projection
    // FLOORED (`/ 1_000_000`), tying `expires_at == now` at the
    // horizon's last tick and letting the same proof re-enter as an
    // "expired overwrite" — "widening skew must never re-open an
    // already-used proof" broken at its boundary.
    let mut l = Loop::new();
    l.serve_with_skew(
        RpcCallShape::ServerStreaming,
        ServeAccess::SameOrg,
        MAX_TOKEN_CLOCK_SKEW_SECS,
    );
    let intent = l.w.intent(5); // ttl_secs = 30
    // Mint PAST a whole millisecond so the retention projection's
    // sub-ms remainder is nonzero — the floored projection loses it.
    l.now = NOW_NS + 700_000;
    let minted_at = l.now;
    let raw = l.craft_request(
        0x2020,
        FLAG_RPC_STREAMING_RESPONSE,
        b"x",
        Vec::new(),
        Some((&intent, RpcCallShape::ServerStreaming)),
    );
    assert_eq!(l.feed_raw(raw.clone()), OpenOutcome::Admitted);

    // Run the call to its one terminal, so the key is free and only
    // the replay record holds the used proof.
    let call = l.served.borrow()[0].clone();
    call.finish(StreamHandlerResult::Ok);
    let _ = l.provider_out();

    // The final sub-millisecond of the max-skew acceptance window.
    let expires_ns = minted_at + 30 * 1_000_000_000;
    let horizon_ns = expires_ns + MAX_TOKEN_CLOCK_SKEW_SECS * 1_000_000_000;
    l.now = horizon_ns - 500_000;
    assert!(
        check_proof_expiry_at(expires_ns, l.now, MAX_TOKEN_CLOCK_SKEW_SECS).is_ok(),
        "the expiry check still accepts at this instant"
    );
    assert_eq!(
        l.feed_raw(raw),
        OpenOutcome::Denied(AdmissionDenied::Replay),
        "a used proof is not reusable while the expiry check still accepts \
         (pre-fix: the floored projection tied expires_at == now at the \
         horizon's last tick and the replay re-admitted)"
    );
}

#[test]
fn a_failing_peer_exhausts_its_budget_and_is_refused_before_the_verify_work() {
    // §6 charges a failed admission to its peer (core's disposition,
    // incl. the D7 `AuthorityChanged` exception) and consults the
    // budget BEFORE the signature work. Each failing attempt below is
    // a full `verify_strict` pass ending in `BindingInvalid` — the
    // exact CPU the limiter exists to bound.
    let mut l = Loop::new();
    l.serve(RpcCallShape::ServerStreaming, ServeAccess::SameOrg);
    let doomed = l.w.intent_at(5, EntityId::from_bytes([0x66; 32]));
    for n in 0..u64::from(DEFAULT_MAX_FAILED_ADMISSIONS_PER_PEER) {
        let raw = l.craft_request(
            0x3000 + n,
            FLAG_RPC_STREAMING_RESPONSE,
            b"x",
            Vec::new(),
            Some((&doomed, RpcCallShape::ServerStreaming)),
        );
        assert_eq!(
            l.feed_raw(raw),
            OpenOutcome::Denied(AdmissionDenied::BindingInvalid)
        );
        let _ = l.provider_out();
    }

    // The budget is spent. The next attempt carries a proof that
    // would otherwise ADMIT — the refusal must come from the budget,
    // before any signature work.
    let good = l.w.intent(5);
    let raw = l.craft_request(
        0x3100,
        FLAG_RPC_STREAMING_RESPONSE,
        b"x",
        Vec::new(),
        Some((&good, RpcCallShape::ServerStreaming)),
    );
    assert_eq!(
        l.feed_raw(raw),
        OpenOutcome::Throttled,
        "a failing peer is refused before the verify work (pre-fix: no gate \
         existed — the 65th (valid) attempt reached `verify_org_admission` \
         and was ADMITTED)"
    );
    let down = l.provider_out();
    assert_eq!(
        responses(&down),
        vec![denied(CoarseAdmissionReason::Unavailable)],
        "core's Throttled disposition: the coarse Unavailable byte"
    );
    assert_eq!(l.serves.live_calls(), 0);
}

#[test]
fn live_call_id_reuse_is_active_call_owned_with_exactly_one_refusal() {
    let mut l = Loop::new();
    l.serve(RpcCallShape::ServerStreaming, ServeAccess::SameOrg);
    let intent = l.w.intent(5);
    let raw = l.craft_request(
        0x2009,
        FLAG_RPC_STREAMING_RESPONSE,
        b"x",
        Vec::new(),
        Some((&intent, RpcCallShape::ServerStreaming)),
    );
    assert_eq!(l.feed_raw(raw.clone()), OpenOutcome::Admitted);
    assert_eq!(
        l.feed_raw(raw),
        OpenOutcome::Denied(AdmissionDenied::ActiveCallOwned),
        "a live key is refused before any of it is decoded into state"
    );
    let down = l.provider_out();
    assert_eq!(terminals(&down), 1, "exactly one refusal terminal");
    assert_eq!(
        responses(&down),
        vec![denied(CoarseAdmissionReason::Denied)]
    );
    assert_eq!(l.serves.live_calls(), 1, "the live call is untouched");

    // And it still ends with its OWN single terminal afterwards.
    let call = l.served.borrow()[0].clone();
    call.finish(StreamHandlerResult::Ok);
    let down = l.provider_out();
    assert_eq!(responses(&down), vec![end_terminal()]);
}

#[test]
fn a_refusal_reusing_a_live_call_id_never_latches_the_calls_terminal() {
    // The caller's latch is first-writer-wins and LATCHING DISARMS the
    // drop-CANCEL guard: ANY refusal that rides the LIVE call's id —
    // the `ActiveCallOwned` duplicate-key refusal, or a crafted
    // REQUEST reusing the id with wrong flags / a wrong service —
    // would be consumed as that call's terminal (a self-inflicted
    // desync: the provider keeps running the handler while the caller
    // believes the call ended and will never CANCEL it). Exactly ONE
    // terminal per call_id on the wire: refusals under a live key go
    // out under a DISTINCT id.
    let mut l = Loop::new();
    l.serve(RpcCallShape::ClientStreaming, ServeAccess::SameOrg);
    let intent = l.w.intent(5);
    let (id, handle) = l.open_cs(
        StreamOpen {
            body: Bytes::from_static(b"u"),
            ..empty_open()
        },
        intent.clone(),
    );
    // The CS opening is lazy: flush it (the one-item path — `u` rides
    // the REQUEST with FLAG_END).
    l.caller.finish_sending(id, l.now).expect("half-close");
    let up = l.caller_out();
    l.feed_provider(up);
    assert_eq!(l.serves.live_calls(), 1, "the live call is admitted");

    // (1) A re-delivered opening for the SAME live key.
    let raw = l.craft_request(
        id,
        FLAG_RPC_CLIENT_STREAMING_REQUEST,
        b"u",
        Vec::new(),
        Some((&intent, RpcCallShape::ClientStreaming)),
    );
    assert_eq!(
        l.feed_raw(raw),
        OpenOutcome::Denied(AdmissionDenied::ActiveCallOwned),
        "the live key is refused before any of it is decoded into state"
    );

    // (2) A crafted WRONG-FLAGS REQUEST reusing the live call's id.
    let raw = l.craft_request(
        id,
        FLAG_RPC_STREAMING_RESPONSE,
        b"u",
        Vec::new(),
        None,
    );
    assert_eq!(
        l.feed_raw(raw),
        OpenOutcome::Denied(AdmissionDenied::ShapeMismatch),
        "the wrong-flags refusal keeps its core-mapped typed reason (LEAF-22)"
    );

    // (3) A crafted MALFORMED REQUEST (wrong service on the carrier)
    // reusing the live call's id.
    let req = RpcRequestPayload {
        service: "svc.other".to_string(),
        deadline_ns: 0,
        flags: FLAG_RPC_CLIENT_STREAMING_REQUEST,
        headers: Vec::new(),
        body: Bytes::from_static(b"u"),
    };
    let raw = rpc_wire::encode_request_frame(
        l.w.caller_entity.origin_hash(),
        id,
        l.request_route,
        &req,
    )
    .expect("encode");
    assert_eq!(
        l.feed_raw(raw),
        OpenOutcome::Malformed(
            "the request names a different service than its carrier serves".to_string()
        ),
    );

    // Every refusal is terminal-shaped on the wire, but NONE rides the
    // live call's id.
    let down = l.provider_out();
    assert_eq!(terminals(&down), 3, "three refusal terminals");
    assert_eq!(
        responses(&down),
        vec![
            denied(CoarseAdmissionReason::Denied),
            denied(CoarseAdmissionReason::Denied),
            RpcResponsePayload {
                status: RpcStatus::UnknownVersion,
                headers: vec![(
                    HEADER_NRPC_STREAMING.to_string(),
                    HEADER_NRPC_STREAMING_END.to_vec(),
                )],
                body: Bytes::from_static(
                    b"malformed request: the request names a different service than its carrier serves"
                ),
            },
        ],
        "the typed refusals, byte-exact"
    );
    assert!(
        down.iter().all(|o| o.call_id != id),
        "no refusal may ride the live call's id \
         (pre-fix: each did — the first was consumed as the call's terminal)"
    );

    // Fed to the caller they neither latch nor disarm anything.
    l.feed_caller(down);
    assert_eq!(
        l.caller.terminal(id),
        None,
        "the latch stays empty (pre-fix: the first refusal was consumed as \
         the call's terminal)"
    );

    // The drop-CANCEL guard is still armed: dropping the handle
    // cancels the live call (pre-fix: the false terminal disarmed it
    // and the drop emitted nothing).
    drop(handle);
    let out = l.caller_out();
    assert!(
        matches!(&out[..], [Outgoing { call_id, frame: RpcFrame::Cancel { .. } }] if *call_id == id),
        "the drop still emits the call's one CANCEL"
    );

    // The provider retires with the call's OWN single terminal — the
    // only terminal that ever rides this call_id — and the latch
    // consumes exactly that one.
    l.feed_provider(out);
    let down = l.provider_out();
    assert_eq!(terminals(&down), 1);
    assert!(down.iter().all(|o| o.call_id == id));
    l.feed_caller(down);
    assert_eq!(
        l.caller.terminal(id),
        Some(&StreamTerminal::Refused {
            status: RpcStatus::Cancelled,
            body: Bytes::from_static(b"server observed CANCEL during streaming handler execution"),
        }),
        "exactly one terminal is consumed by the latch — the call's own"
    );
}

// ---------------------------------------------------------------------------
// (c) upload half-close: EOF at FLAG_END; late chunks deliver nothing
// ---------------------------------------------------------------------------

#[test]
fn request_end_is_eof_and_a_late_chunk_delivers_and_cancels_nothing() {
    let mut l = Loop::new();
    l.serve(RpcCallShape::ClientStreaming, ServeAccess::SameOrg);
    let intent = l.w.intent(5);
    let (id, handle) = l.open_cs(empty_open(), intent);
    l.caller.send(id, b"only", l.now).expect("send");
    l.caller.finish_sending(id, l.now).expect("finish");
    let up = l.caller_out();
    l.feed_provider(up);

    let call = l.served.borrow()[0].clone();
    assert_eq!(call.poll_request(), Some(Bytes::from_static(b"only")));
    assert!(call.request_ended(), "EOF is exactly FLAG_END");

    // A late chunk AFTER end: delivers nothing, cancels nothing.
    let late = RpcRequestChunkPayload {
        call_id: id,
        flags: 0,
        headers: Vec::new(),
        body: Bytes::from_static(b"late"),
    };
    let peer = l.serve_peer();
    assert!(
        !l.serves.on_chunk(&peer, late),
        "a late chunk delivers nothing"
    );
    assert_eq!(
        call.poll_request(),
        None,
        "and never reopens the input half"
    );
    let down = l.provider_out();
    assert_eq!(down.len(), 0, "and cancels nothing");
    assert_eq!(l.caller.terminal(id), None, "the call is still live");

    call.send(b"result").expect("respond");
    call.finish(StreamHandlerResult::Ok);
    l.flush();
    assert_eq!(
        l.caller.terminal(id),
        Some(&StreamTerminal::Completed {
            body: Bytes::from_static(b"result")
        })
    );
    drop(handle);
}

#[test]
fn end_on_one_call_never_touches_a_sibling() {
    let mut l = Loop::new();
    l.serve(RpcCallShape::ClientStreaming, ServeAccess::SameOrg);
    let intent_a = l.w.intent(5);
    let (a_id, a_handle) = l.open_cs(empty_open(), intent_a);
    let intent_b = l.w.intent(5);
    let (b_id, b_handle) = l.open_cs(empty_open(), intent_b);
    l.caller.send(a_id, b"a1", l.now).expect("send");
    l.caller.send(b_id, b"b1", l.now).expect("send");
    l.caller.finish_sending(a_id, l.now).expect("end A");
    let up = l.caller_out();
    l.feed_provider(up);

    let calls = l.served.borrow().clone();
    assert_eq!(calls.len(), 2);
    // A ended; B's upload half is untouched and still delivers.
    l.caller.send(b_id, b"b2", l.now).expect("B still sends");
    l.caller.finish_sending(b_id, l.now).expect("end B");
    let up = l.caller_out();
    l.feed_provider(up);

    let a_items: Vec<Bytes> = (0..8).map_while(|_| calls[0].poll_request()).collect();
    let b_items: Vec<Bytes> = (0..8).map_while(|_| calls[1].poll_request()).collect();
    assert_eq!(a_items, vec![Bytes::from_static(b"a1")]);
    assert_eq!(
        b_items,
        vec![Bytes::from_static(b"b1"), Bytes::from_static(b"b2")],
        "END on one call never touches a sibling's upload"
    );
    for call in &calls {
        assert!(call.request_ended());
        call.send(b"done").expect("respond");
        call.finish(StreamHandlerResult::Ok);
    }
    l.flush();
    assert!(matches!(
        l.caller.terminal(a_id),
        Some(StreamTerminal::Completed { .. })
    ));
    assert!(matches!(
        l.caller.terminal(b_id),
        Some(StreamTerminal::Completed { .. })
    ));
    drop((a_handle, b_handle));
}

// ---------------------------------------------------------------------------
// (d) backpressure in both directions
// ---------------------------------------------------------------------------

#[test]
fn response_credit_parks_the_sender_and_each_grant_releases_exactly_its_chunks() {
    let mut l = Loop::new();
    l.serve(RpcCallShape::ServerStreaming, ServeAccess::SameOrg);
    let intent = l.w.intent(5);
    let (id, handle) = l.open_ss(
        StreamOpen {
            body: Bytes::from_static(b"r"),
            stream_window_initial: Some(2),
            ..empty_open()
        },
        intent,
    );
    let up = l.caller_out();
    l.feed_provider(up);

    let call = l.served.borrow()[0].clone();
    // Window 2: two sends RESOLVE and take their items; the rest park
    // and take NOTHING — with an idle reader, none of them resolve.
    assert_eq!(call.send(b"a"), Ok(()));
    assert_eq!(call.send(b"b"), Ok(()));
    for item in [b"c".as_slice(), b"d".as_slice(), b"e".as_slice()] {
        assert_eq!(
            call.send(item),
            Err(SinkError::WouldBlock),
            "a creditless send parks; the item was NOT taken"
        );
    }

    let down = l.provider_out();
    assert_eq!(
        responses(&down),
        vec![continue_chunk(b"a"), continue_chunk(b"b")],
        "exactly credit-count chunks"
    );
    assert_eq!(terminals(&down), 0, "a parked sender is not terminal");
    l.feed_caller(down);
    assert_eq!(l.caller.next_item(id), Some(Bytes::from_static(b"a")));
    assert_eq!(l.caller.next_item(id), Some(Bytes::from_static(b"b")));

    // Consuming granted 2 credits (one per consumed chunk): exactly 2
    // more sends resolve and exactly 2 more chunks go out.
    let up = l.caller_out();
    assert_eq!(up.len(), 2, "one STREAM_GRANT per consumed chunk");
    l.feed_provider(up);
    assert_eq!(call.send(b"c"), Ok(()));
    assert_eq!(call.send(b"d"), Ok(()));
    assert_eq!(call.send(b"e"), Err(SinkError::WouldBlock));
    let down = l.provider_out();
    assert_eq!(
        responses(&down),
        vec![continue_chunk(b"c"), continue_chunk(b"d")]
    );
    l.feed_caller(down);
    assert_eq!(l.caller.next_item(id), Some(Bytes::from_static(b"c")));
    assert_eq!(l.caller.next_item(id), Some(Bytes::from_static(b"d")));

    // The last item and the terminal.
    let up = l.caller_out();
    l.feed_provider(up);
    assert_eq!(call.send(b"e"), Ok(()));
    call.finish(StreamHandlerResult::Ok);
    let down = l.provider_out();
    assert_eq!(
        responses(&down),
        vec![continue_chunk(b"e"), end_terminal()],
        "the final chunk then exactly one terminal"
    );
    l.feed_caller(down);
    assert_eq!(l.caller.next_item(id), Some(Bytes::from_static(b"e")));
    assert_eq!(
        l.caller.terminal(id),
        Some(&StreamTerminal::Completed { body: Bytes::new() })
    );
    drop(handle);
}

#[test]
fn upload_chunks_wait_for_request_grants_and_consumption_grants_one_each() {
    let mut l = Loop::new();
    l.serve(RpcCallShape::ClientStreaming, ServeAccess::SameOrg);
    let intent = l.w.intent(5);
    let (id, handle) = l.open_cs(
        StreamOpen {
            request_window_initial: Some(2),
            ..empty_open()
        },
        intent,
    );
    // Window 2: TWO item frames go out (the opening item + one chunk);
    // the third `send` PARKS and takes NOTHING — core's `send().await`
    // permit semantics in pull form.
    assert_eq!(l.caller.send(id, b"a", l.now), Ok(()));
    assert_eq!(l.caller.send(id, b"b", l.now), Ok(()));
    assert_eq!(
        l.caller.send(id, b"c", l.now),
        Err(SinkError::WouldBlock),
        "a creditless send parks; the item was NOT taken"
    );
    let up = l.caller_out();
    assert_eq!(up.len(), 2, "exactly the opening window of item frames");
    l.feed_provider(up);

    let call = l.served.borrow()[0].clone();
    assert_eq!(call.poll_request(), Some(Bytes::from_static(b"a")));
    assert_eq!(call.poll_request(), Some(Bytes::from_static(b"b")));
    assert_eq!(
        call.poll_request(),
        None,
        "c was never sent — it is parked at the caller"
    );

    // Consumption grants exactly one credit per consumed chunk.
    let down = l.provider_out();
    let grants = down
        .iter()
        .filter(|o| matches!(o.frame, RpcFrame::RequestGrant(_)))
        .count();
    assert_eq!(grants, 2, "one REQUEST_GRANT per consumed chunk");
    l.feed_caller(down);

    // Credit released: the parked sends RESOLVE as grants arrive — and
    // park again exactly at the window.
    assert_eq!(l.caller.send(id, b"c", l.now), Ok(()));
    assert_eq!(l.caller.send(id, b"d", l.now), Ok(()));
    assert_eq!(l.caller.send(id, b"e", l.now), Err(SinkError::WouldBlock));
    // The terminal upload frame pays no credit; a parked item does not
    // block the half-close once its own send was refused.
    assert_eq!(l.caller.finish_sending(id, l.now), Ok(()));
    assert_eq!(
        l.caller.send(id, b"e", l.now),
        Err(SinkError::Closed),
        "after finish the upload half is closed"
    );
    let up = l.caller_out();
    assert_eq!(up.len(), 3, "CHUNK(c), CHUNK(d), and the FLAG_END frame");
    match &up[2].frame {
        RpcFrame::RequestChunk(chunk) => {
            assert_eq!(chunk.body.as_ref(), b"");
            assert_eq!(chunk.flags & FLAG_RPC_REQUEST_END, FLAG_RPC_REQUEST_END);
        }
        other => panic!("expected the terminal upload frame, got {other:?}"),
    }
    l.feed_provider(up);
    assert_eq!(call.poll_request(), Some(Bytes::from_static(b"c")));
    assert_eq!(call.poll_request(), Some(Bytes::from_static(b"d")));
    assert!(call.request_ended());
    let down = l.provider_out();
    let grants = down
        .iter()
        .filter(|o| matches!(o.frame, RpcFrame::RequestGrant(_)))
        .count();
    assert_eq!(grants, 2, "the remaining consumption grants one each");

    // End-to-end: the call still completes with one response.
    call.send(b"sum").expect("respond");
    call.finish(StreamHandlerResult::Ok);
    l.flush();
    assert_eq!(
        l.caller.terminal(id),
        Some(&StreamTerminal::Completed {
            body: Bytes::from_static(b"sum")
        })
    );
    drop(handle);
}

#[test]
fn request_grants_pace_per_consumed_chunk_past_the_old_lifetime_cap() {
    // Core grants one `REQUEST_GRANT` per consumed chunk for the LIFE
    // of the call (`RequestStream::poll_next` auto-grant): the
    // `REQUEST_GRANT_PER_CALL_CAP` constant bounds the CALLER's credit
    // BALANCE, never the provider's lifetime grant count. The old
    // provider pacing capped lifetime grants at
    // `initial + REQUEST_GRANT_PER_CALL_CAP` — every sustained upload
    // stalled in `SinkError::WouldBlock` to its deadline once spent.
    let mut l = Loop::new();
    l.serve(RpcCallShape::ClientStreaming, ServeAccess::SameOrg);
    let intent = l.w.intent(5);
    let (id, handle) = l.open_cs(
        StreamOpen {
            request_window_initial: Some(1),
            ..empty_open()
        },
        intent,
    );
    // The lazy opening carries the first item; consume it.
    assert_eq!(l.caller.send(id, b"x", l.now), Ok(()));
    let up = l.caller_out();
    l.feed_provider(up);
    let call = l.served.borrow()[0].clone();
    assert_eq!(call.poll_request(), Some(Bytes::from_static(b"x")));

    // A sustained upload driven straight at the provider (one
    // synthetic chunk consumed per step) so the witness crosses the
    // old cap (window 1 + 1_000_000) without minting a million wire
    // frames. The old pacing stopped granting at 1_000_001.
    let peer = l.serve_peer();
    const CHUNKS: u64 = 1_000_010;
    let mut grants = 0u64;
    for i in 0..CHUNKS {
        l.serves.on_chunk(
            &peer,
            RpcRequestChunkPayload {
                call_id: id,
                flags: 0,
                headers: Vec::new(),
                body: Bytes::new(),
            },
        );
        assert!(call.poll_request().is_some());
        if i % 50_000 == 49_999 {
            l.serves.advance(l.now);
            grants += l.serves.take_outbound().len() as u64;
        }
    }
    l.serves.advance(l.now);
    grants += l.serves.take_outbound().len() as u64;
    assert_eq!(
        grants,
        1 + CHUNKS,
        "one REQUEST_GRANT per consumed chunk, past the old lifetime cap \
         (pre-fix: grants stop at initial + REQUEST_GRANT_PER_CALL_CAP and \
         the upload stalls in WouldBlock to its deadline)"
    );
    drop(handle);
}

#[test]
fn a_second_single_response_send_is_refused_closed_not_silently_dropped() {
    // §2.7: a protected call can never drop an item and report an
    // earlier success. The single-response emitter (CS/unary, §2.2)
    // takes ONE body: the documented `SinkError::Closed` refusal, not
    // `Ok` followed by a pump-side silent discard.
    let mut l = Loop::new();
    l.serve(RpcCallShape::ClientStreaming, ServeAccess::SameOrg);
    let intent = l.w.intent(5);
    let (id, handle) = l.open_cs(
        StreamOpen {
            body: Bytes::from_static(b"req"),
            ..empty_open()
        },
        intent,
    );
    l.caller.finish_sending(id, l.now).expect("half-close");
    let up = l.caller_out();
    l.feed_provider(up);
    let call = l.served.borrow()[0].clone();
    assert_eq!(call.poll_request(), Some(Bytes::from_static(b"req")));

    assert_eq!(call.send(b"resp-1"), Ok(()));
    assert_eq!(
        call.send(b"resp-2"),
        Err(SinkError::Closed),
        "the single response is already filled — a second send is refused \
         TYPED (pre-fix: `Ok`, and the pump silently discarded the item)"
    );
    call.finish(StreamHandlerResult::Ok);
    let down = l.provider_out();
    assert_eq!(terminals(&down), 1);
    assert_eq!(
        responses(&down),
        vec![RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: Vec::new(),
            body: Bytes::from_static(b"resp-1"),
        }],
        "the ONE single response, byte-exact"
    );
    drop(handle);
}

#[test]
fn zero_windows_are_refused_at_open_and_at_admission() {
    // Caller-side: the `Some(0)` deadlock guard, at the verb.
    let mut l = Loop::new();
    l.serve(RpcCallShape::ServerStreaming, ServeAccess::SameOrg);
    let intent = l.w.intent(5);
    let pin = l.pin();
    let binding = l.w.binding;
    let now = l.now;
    let err = l
        .caller
        .open_server_streaming(
            pin,
            SERVICE,
            StreamOpen {
                stream_window_initial: Some(0),
                ..empty_open()
            },
            intent,
            Some(binding),
            now,
        )
        .expect_err("Some(0) is refused");
    assert_eq!(err, CallOpenError::ZeroWindow);

    // Provider-side: a window-0 header on a well-formed opening.
    let mut l = Loop::new();
    l.serve(RpcCallShape::ServerStreaming, ServeAccess::SameOrg);
    let intent = l.w.intent(5);
    let raw = l.craft_request(
        0x2010,
        FLAG_RPC_STREAMING_RESPONSE,
        b"x",
        vec![(HEADER_NRPC_STREAM_WINDOW_INITIAL.to_string(), b"0".to_vec())],
        Some((&intent, RpcCallShape::ServerStreaming)),
    );
    assert_eq!(
        l.feed_raw(raw),
        OpenOutcome::Denied(AdmissionDenied::ProviderPolicyRejected)
    );
    let down = l.provider_out();
    assert_eq!(
        responses(&down),
        vec![denied(CoarseAdmissionReason::Denied)]
    );
}

// ---------------------------------------------------------------------------
// (e) retirement reasons map to the exact terminal vocabulary
// ---------------------------------------------------------------------------

#[test]
fn deadline_expiry_emits_the_timeout_terminal_and_latches_once() {
    let mut l = Loop::new();
    l.serve(RpcCallShape::ServerStreaming, ServeAccess::SameOrg);
    let intent = l.w.intent(5);
    let deadline_ns = NOW_NS + 1_000_000_000;
    let (id, handle) = l.open_ss(
        StreamOpen {
            body: Bytes::from_static(b"r"),
            deadline_ns,
            ..empty_open()
        },
        intent,
    );
    let up = l.caller_out();
    l.feed_provider(up);
    assert_eq!(l.serves.live_calls(), 1);

    // A frozen tab's ticker stopping extends nothing: the first sweep
    // after the absolute end retires the call.
    l.now = deadline_ns + 1;
    let down = l.provider_out();
    assert_eq!(
        responses(&down),
        vec![RpcResponsePayload {
            status: RpcStatus::Timeout,
            headers: Vec::new(),
            body: Bytes::from_static(b"stream deadline_ns exceeded")
        }],
        "deadline → Timeout, byte-exact"
    );
    assert_eq!(l.serves.live_calls(), 0);

    // A second retirement is a no-op: no second terminal, ever.
    let peer = l.serve_peer();
    assert!(!l.serves.on_cancel(&peer, id));
    l.now += 10_000_000_000;
    assert_eq!(l.provider_out().len(), 0, "exactly one terminal latched");
    drop(handle);
}

#[test]
fn cancel_emits_the_cancelled_terminal_verbatim() {
    let mut l = Loop::new();
    l.serve(RpcCallShape::ServerStreaming, ServeAccess::SameOrg);
    let intent = l.w.intent(5);
    let (id, handle) = l.open_ss(
        StreamOpen {
            body: Bytes::from_static(b"r"),
            ..empty_open()
        },
        intent,
    );
    let up = l.caller_out();
    l.feed_provider(up);

    let call = l.served.borrow()[0].clone();
    call.send(b"streaming").expect("send");

    let peer = l.serve_peer();
    assert!(l.serves.on_cancel(&peer, id));
    assert_eq!(
        l.provider_out(),
        vec![Outgoing {
            call_id: id,
            frame: RpcFrame::Response {
                call_id: id,
                payload: RpcResponsePayload {
                    status: RpcStatus::Cancelled,
                    headers: Vec::new(),
                    body: Bytes::from_static(
                        b"server observed CANCEL during streaming handler execution"
                    ),
                },
            },
        }],
        "cancel → Cancelled, byte-exact; the queued item is discarded"
    );
    assert_eq!(
        call.send(b"after"),
        Err(SinkError::Closed),
        "the sink's typed closed refusal is the handler-side observable"
    );
    // A second cancel cannot produce a second terminal.
    assert!(!l.serves.on_cancel(&peer, id));
    assert_eq!(l.provider_out().len(), 0);
    drop(handle);
}

#[test]
fn success_drains_queued_items_in_order_before_the_end_terminal() {
    let mut l = Loop::new();
    l.serve(RpcCallShape::ServerStreaming, ServeAccess::SameOrg);
    let intent = l.w.intent(5);
    let (id, handle) = l.open_ss(
        StreamOpen {
            body: Bytes::from_static(b"r"),
            stream_window_initial: Some(3),
            ..empty_open()
        },
        intent,
    );
    let up = l.caller_out();
    l.feed_provider(up);

    // The handler queues everything it has credit for and finishes
    // BEFORE the drain: producer finished is not terminal, and a
    // creditless send parks (takes nothing) rather than over-queue.
    let call = l.served.borrow()[0].clone();
    for item in [b"a".as_slice(), b"b".as_slice(), b"c".as_slice()] {
        assert_eq!(call.send(item), Ok(()));
    }
    assert_eq!(call.send(b"d"), Err(SinkError::WouldBlock));
    call.finish(StreamHandlerResult::Ok);

    let down = l.provider_out();
    assert_eq!(
        responses(&down),
        vec![
            continue_chunk(b"a"),
            continue_chunk(b"b"),
            continue_chunk(b"c"),
            end_terminal()
        ],
        "F-S1: Completed(_) drains queued items in order BEFORE its terminal"
    );
    l.feed_caller(down);
    while l.caller.next_item(id).is_some() {}
    assert_eq!(
        l.caller.terminal(id),
        Some(&StreamTerminal::Completed { body: Bytes::new() })
    );
    drop(handle);
}

#[test]
fn a_floor_raise_retires_the_stale_call_with_denied_and_the_coarse_zero_byte() {
    let mut l = Loop::new();
    l.serve(RpcCallShape::ServerStreaming, ServeAccess::SameOrg);
    let intent = l.w.intent(3); // membership generation 3
    let (id, handle) = l.open_ss(
        StreamOpen {
            body: Bytes::from_static(b"r"),
            ..empty_open()
        },
        intent,
    );
    let up = l.caller_out();
    l.feed_provider(up);
    let call = l.served.borrow()[0].clone();
    call.send(b"before").expect("send");

    // A floor rises past the call's membership generation.
    let raised = l
        .facts
        .merge_floors(l.w.owner_org, &[(l.w.caller_entity.clone(), 4)]);
    assert_eq!(raised, 1);
    assert_eq!(l.serves.raise_floors(&l.facts), vec![id]);

    let down = l.provider_out();
    assert_eq!(
        responses(&down),
        vec![RpcResponsePayload {
            status: RpcStatus::AdmissionDenied,
            headers: Vec::new(),
            body: Bytes::from_static(&[0]),
        }],
        "the frozen Revoked → Denied + &[0] byte, verbatim"
    );
    assert_eq!(call.retired(), Some(RetireReason::Revoked));
    assert_eq!(call.send(b"after"), Err(SinkError::Closed));
    l.feed_caller(down);
    assert_eq!(
        l.caller.terminal(id),
        Some(&StreamTerminal::Refused {
            status: RpcStatus::AdmissionDenied,
            body: Bytes::from_static(&[0])
        }),
        "the caller maps it through the terminal vocabulary verbatim"
    );
    drop(handle);
}

// ---------------------------------------------------------------------------
// (f) attribution: wrong peer / old session deliver nothing
// ---------------------------------------------------------------------------

#[test]
fn wrong_peer_and_old_session_frames_deliver_nothing() {
    let mut l = Loop::new();
    l.serve(RpcCallShape::ClientStreaming, ServeAccess::SameOrg);
    let intent = l.w.intent(5);
    let (id, handle) = l.open_cs(empty_open(), intent);
    l.caller.send(id, b"mine", l.now).expect("send");
    let up = l.caller_out();
    l.feed_provider(up);
    let call = l.served.borrow()[0].clone();
    assert_eq!(call.poll_request(), Some(Bytes::from_static(b"mine")));

    let spoofed = RpcRequestChunkPayload {
        call_id: id,
        flags: 0,
        headers: Vec::new(),
        body: Bytes::from_static(b"spoofed"),
    };
    let wrong_peer = ServePeer {
        peer: OTHER_PEER,
        incarnation: INC,
        caller: l.w.caller_entity.clone(),
        session_binding: Some(l.w.binding),
    };
    let old_session = ServePeer {
        peer: CALLER_NODE,
        incarnation: OLD_INC,
        caller: l.w.caller_entity.clone(),
        session_binding: Some(l.w.binding),
    };
    assert!(
        !l.serves.on_chunk(&wrong_peer, spoofed.clone()),
        "another peer with the same call id delivers nothing"
    );
    assert!(
        !l.serves.on_chunk(&old_session, spoofed),
        "an old session with the same call id delivers nothing"
    );
    assert!(!l.serves.on_cancel(&wrong_peer, id));
    assert!(!l.serves.on_stream_grant(&wrong_peer, id, 10));
    assert_eq!(
        call.poll_request(),
        None,
        "the spoofed chunk was not queued"
    );
    assert_eq!(l.provider_out().len(), 0, "and nothing was emitted");

    // The real call is unaffected.
    l.caller.finish_sending(id, l.now).expect("finish");
    let up = l.caller_out();
    l.feed_provider(up);
    assert!(call.request_ended());
    call.send(b"fine").expect("respond");
    call.finish(StreamHandlerResult::Ok);
    l.flush();
    assert_eq!(
        l.caller.terminal(id),
        Some(&StreamTerminal::Completed {
            body: Bytes::from_static(b"fine")
        })
    );

    // Caller side symmetric: a wrong-owner response delivers nothing
    // and the correct one can still arrive afterwards.
    let payload = RpcResponsePayload {
        status: RpcStatus::Ok,
        headers: Vec::new(),
        body: Bytes::from_static(b"spoof"),
    };
    let wrong_owner = CallOwner {
        peer: PROVIDER_NODE,
        incarnation: OLD_INC,
        reply_route: l.reply_route,
        carrier_stream_id: l.carrier,
    };
    let intent2 = l.w.intent(5);
    let (id2, handle2) = l.open_cs(empty_open(), intent2);
    assert!(
        !l.caller.on_response(wrong_owner, id2, payload.clone()),
        "an old-session response neither delivers nor completes"
    );
    assert_eq!(l.caller.terminal(id2), None);
    assert!(l.caller.on_response(l.call_owner(), id2, payload));
    assert_eq!(
        l.caller.terminal(id2),
        Some(&StreamTerminal::Completed {
            body: Bytes::from_static(b"spoof")
        })
    );
    drop((handle, handle2));
}

#[test]
fn a_vanished_provider_retires_at_the_callers_own_deadline_with_the_timeout_terminal() {
    // §4.5: closure retires ownership "with the same deadline" — when
    // the provider dies mid-stream (its session may even survive as a
    // shared leader's), the caller's observable is the call's OWN
    // deadline terminal at the absolute UNEXTENDED end: the `Timeout`
    // class via `stream_terminal_payload`, latched once, never resumed.
    let mut l = Loop::new();
    l.serve(RpcCallShape::ServerStreaming, ServeAccess::SameOrg);
    let intent = l.w.intent(5);
    let deadline_ns = NOW_NS + 5_000_000_000;
    let (id, handle) = l.open_ss(
        StreamOpen {
            body: Bytes::from_static(b"request"),
            deadline_ns,
            ..empty_open()
        },
        intent,
    );
    // The eager opening goes out and the provider VANISHES: the frames
    // leave and nothing ever comes back.
    let up = l.caller_out();
    assert_eq!(up.len(), 1);
    l.feed_provider(up); // delivered into the void
    let _ = l.serves.unserve(SERVICE); // the serving side is gone
    assert_eq!(l.serves.live_calls(), 0);

    // No lease extension: one nanosecond before the end there is no
    // terminal, and at the end there is exactly one.
    l.now = deadline_ns - 1;
    l.caller.advance(l.now);
    assert_eq!(
        l.caller.terminal(id),
        None,
        "the absolute end is unextended — no early terminal"
    );
    l.now = deadline_ns;
    l.caller.advance(l.now);
    assert_eq!(
        l.caller.terminal(id),
        Some(&StreamTerminal::Refused {
            status: RpcStatus::Timeout,
            body: Bytes::from_static(b"stream deadline_ns exceeded")
        }),
        "the caller's own deadline terminal — the `Timeout` class via \
         stream_terminal_payload, byte-identical to the wire timeout"
    );

    // Latched once: later sweeps and late frames are no-ops.
    l.caller.advance(l.now + 60_000_000_000);
    assert_eq!(
        l.caller.terminal(id),
        Some(&StreamTerminal::Refused {
            status: RpcStatus::Timeout,
            body: Bytes::from_static(b"stream deadline_ns exceeded")
        })
    );
    let payload = RpcResponsePayload {
        status: RpcStatus::Ok,
        headers: Vec::new(),
        body: Bytes::from_static(b"late"),
    };
    assert!(!l.caller.on_response(l.call_owner(), id, payload));

    // Exactly one CANCEL went with the sweep ("tell the server"), and
    // no automatic resume ever follows.
    let out = l.caller_out();
    assert_eq!(out.len(), 1);
    assert!(matches!(&out[0].frame, RpcFrame::Cancel { call_id } if *call_id == id));
    assert_eq!(l.caller_out().len(), 0);
    drop(handle);
}

// ---------------------------------------------------------------------------
// (g) handle drop emits exactly one CANCEL
// ---------------------------------------------------------------------------

#[test]
fn dropping_the_handle_emits_exactly_one_cancel() {
    let mut l = Loop::new();
    l.serve(RpcCallShape::ServerStreaming, ServeAccess::SameOrg);
    let intent = l.w.intent(5);
    let (id, handle) = l.open_ss(
        StreamOpen {
            body: Bytes::from_static(b"r"),
            ..empty_open()
        },
        intent,
    );
    let _ = l.caller_out(); // discard the REQUEST
    drop(handle);
    let out = l.caller_out();
    assert_eq!(out.len(), 1, "frame-level count at the outbound queue: ONE");
    assert!(
        matches!(&out[0].frame, RpcFrame::Cancel { call_id } if *call_id == id),
        "the one frame is the call's CANCEL"
    );
    // Nothing further, ever.
    l.caller.advance(l.now);
    assert_eq!(l.caller_out().len(), 0);
}

#[test]
fn explicit_cancel_and_later_drop_share_the_one_cancel_guard() {
    let mut l = Loop::new();
    l.serve(RpcCallShape::ServerStreaming, ServeAccess::SameOrg);
    let intent = l.w.intent(5);
    let (id, handle) = l.open_ss(
        StreamOpen {
            body: Bytes::from_static(b"r"),
            ..empty_open()
        },
        intent,
    );
    let _ = l.caller_out();
    assert!(l.caller.cancel(id), "the explicit cancel fires");
    drop(handle);
    let out = l.caller_out();
    let cancels = out
        .iter()
        .filter(|o| matches!(o.frame, RpcFrame::Cancel { .. }))
        .count();
    assert_eq!(cancels, 1, "one logical terminal between cancel and drop");
}

// ---------------------------------------------------------------------------
// The coordinator's node-level two-branch revocation witness
// ---------------------------------------------------------------------------

mod node_level {
    use super::*;
    use net_leaf::clock;
    use net_leaf::identity::LeafIdentity;
    use net_leaf::node::LeafNode;
    use net_leaf::session::{routing_id, rtc_addr};
    use net_wire::channel::membership::{self, AckReason, MembershipMsg};
    use net_wire::crypto::{handshake_prologue, NoiseHandshake, StaticKeypair};
    use net_wire::parsed_packet::ParsedPacket;
    use net_wire::pool::PacketBuilder;
    use net_wire::protocol::PacketFlags;
    use net_wire::session::NetSession;

    const PSK: [u8; 32] = [0x4B; 32];

    /// The fixture peer identity — its entity is the World's caller
    /// entity, so the membership certs bind it.
    fn peer_identity() -> LeafIdentity {
        LeafIdentity::from_secrets(EntityKeypair::from_secret([0x24; 32]), [0x7F; 32])
    }

    fn peer_static() -> StaticKeypair {
        let secret = x25519_dalek::StaticSecret::from([7u8; 32]);
        let public = x25519_dalek::PublicKey::from(&secret);
        StaticKeypair::from_keys([7u8; 32], *public.as_bytes())
    }

    /// The node.rs native fixture: a leaf with a live session against a
    /// real responder session, so the whole receive path runs natively.
    fn connected(peer: NodeId) -> (LeafNode, NetSession) {
        let mut node = LeafNode::new(
            LeafIdentity::from_secrets(EntityKeypair::from_secret([0x11; 32]), [0xEE; 32]),
            0x1000,
        );
        let peer_key = peer_static();
        let prologue = handshake_prologue(routing_id(node.node_id()), routing_id(peer));
        let mut responder =
            NoiseHandshake::responder_with_prologue(&PSK, &peer_key, &prologue).expect("responder");

        let msg1 = node
            .begin_handshake(peer, &PSK, peer_key.public_key(), 0)
            .expect("msg1");
        let parsed = ParsedPacket::parse(msg1, rtc_addr(0, 1)).expect("parses");
        responder.read_message(&parsed.payload).expect("reads msg1");
        let msg2 = responder.write_message(&[]).expect("msg2");
        let msg2_packet = PacketBuilder::new(&[0u8; 32], 0).build_handshake(&msg2);

        node.set_peer_rtc_addr(peer, Some("198.51.100.7:4433".into()));
        node.complete_handshake(peer, &msg2_packet)
            .expect("install");
        let keys = responder.into_session_keys().expect("keys");
        (node, NetSession::new(keys, rtc_addr(1, 1), 2, false))
    }

    /// A packet the peer would send, exercising the node's whole
    /// inbound path.
    fn peer_packet(
        peer_session: &NetSession,
        stream_id: u64,
        subprotocol_id: u16,
        channel_hash: u16,
        payload: &[u8],
    ) -> Bytes {
        peer_session.open_stream_with(stream_id, true, 1);
        let seq = peer_session.get_or_create_stream(stream_id).next_tx_seq();
        let events = [Bytes::copy_from_slice(payload)];
        let mut builder = peer_session.thread_local_pool().get();
        builder.set_channel_hash(channel_hash);
        builder.set_origin_hash(0xFEED_FACE_0000_0001);
        builder.build_subprotocol(
            stream_id,
            seq,
            &events,
            PacketFlags::RELIABLE,
            subprotocol_id,
        )
    }

    /// Decrypt one packet the leaf built, as the peer would.
    fn decrypt(peer_session: &NetSession, parsed: &ParsedPacket) -> Bytes {
        let aad = parsed.header.aad();
        let counter = u64::from_le_bytes(parsed.header.nonce[4..12].try_into().expect("nonce"));
        peer_session
            .rx_cipher()
            .decrypt_to_bytes(counter, &aad, parsed.payload.clone())
            .expect("the peer decrypts")
    }

    /// Drain the node's outbound and unwrap every event frame
    /// (`[len: u32le][data]…`, `net_wire::protocol::EventFrame`).
    fn drain_events(peer_session: &NetSession, node: &mut LeafNode) -> Vec<Bytes> {
        let mut events = Vec::new();
        for out in node.take_outbound() {
            let Some(parsed) = ParsedPacket::parse(out.packet, rtc_addr(0, 1)) else {
                continue;
            };
            let plaintext = decrypt(peer_session, &parsed);
            let mut rest = plaintext.as_ref();
            while rest.len() >= 4 {
                let len = u32::from_le_bytes(rest[0..4].try_into().expect("len prefix")) as usize;
                if rest.len() < 4 + len {
                    break;
                }
                events.push(Bytes::copy_from_slice(&rest[4..4 + len]));
                rest = &rest[4 + len..];
            }
        }
        events
    }

    /// The Acks among some decoded events, decoded.
    fn acks(events: &[Bytes]) -> Vec<MembershipMsg> {
        events
            .iter()
            .filter_map(|e| membership::decode(e).ok())
            .collect()
    }

    #[test]
    fn a_revocation_bundle_retires_the_stale_call_while_a_compliant_sibling_keeps_delivering() {
        let world = World::at(clock::now_unix_secs());
        let peer_ident = peer_identity();
        let peer = peer_ident.node_id();
        let (mut node, peer_session) = connected(peer);

        // The attribution pin: a verified announcement from the peer
        // (signature + R1 node-id binding at ingest).
        let signed = net_leaf::announce::build_announcement(
            &peer_ident,
            &["test.anchor".to_string()],
            1,
            clock::now_unix_nanos(),
            300,
        )
        .expect("announcement");
        assert!(node.ingest_announcement(&signed), "the pin is installed");
        assert_eq!(
            node.peer_entity_id(peer),
            Some(world.caller_entity.clone()),
            "the AEAD-authenticated entity is the TOFU pin"
        );

        // Serve server-streaming org-protected.
        let calls: Rc<RefCell<Vec<ServeCall>>> = Rc::new(RefCell::new(Vec::new()));
        let handler_calls = Rc::clone(&calls);
        let opts = ServeOptions {
            shape: RpcCallShape::ServerStreaming,
            access: ServeAccess::SameOrg,
            provider_owner_org: world.owner_org,
            skew_secs: 0,
            default_live_ns: 300 * 1_000_000_000,
            max_live_ns: 3600 * 1_000_000_000,
            policy: None,
        };
        node.org_serve(
            SERVICE,
            opts,
            Rc::new(move |call| handler_calls.borrow_mut().push(call)),
        )
        .expect("serve");

        // Two openings from the same peer session: membership
        // generation 3 (stale once the floor rises) and 5 (compliant).
        let request_route =
            Channel::from_name(channel::request_channel(SERVICE).expect("c")).canonical();
        let stream_id = channel::publish_stream_id(request_route);
        let binding = node.peer_session_binding(peer).expect("session binding");
        let node_entity = EntityId::from_bytes(*node.identity().entity().entity_id());
        for (call_id, generation) in [(0x3001u64, 3u32), (0x3002, 5)] {
            let intent = world.intent_at(generation, node_entity.clone());
            let mut req = RpcRequestPayload {
                service: SERVICE.to_string(),
                deadline_ns: 0,
                flags: FLAG_RPC_STREAMING_RESPONSE,
                headers: Vec::new(),
                body: Bytes::from_static(b"open"),
            };
            attach_signed_admission(
                &mut req,
                &intent,
                call_id,
                SERVICE,
                RpcCallShape::ServerStreaming,
                Some(binding),
                clock::now_unix_nanos(),
            )
            .expect("mint");
            let frame = rpc_wire::encode_request_frame(
                world.caller_entity.origin_hash(),
                call_id,
                request_route,
                &req,
            )
            .expect("frame");
            let packet = peer_packet(&peer_session, stream_id, 0, request_route as u16, &frame);
            node.on_datagram(peer, packet, clock::now());
        }
        assert_eq!(calls.borrow().len(), 2, "both openings were admitted");

        // Both calls are live and delivering.
        let handles = calls.borrow().clone();
        handles[0].send(b"gen3-item").expect("send");
        handles[1].send(b"gen5-item").expect("send");
        node.tick(clock::now());
        assert_eq!(handles[0].retired(), None);
        assert_eq!(handles[1].retired(), None);

        // Feed a signed revocation bundle: floor 4 for the caller.
        let mut floors = BTreeMap::new();
        floors.insert(world.caller_entity.clone(), 4u32);
        let bundle = OrgRevocationBundle::issue_at(&world.org, &floors, world.now_secs)
            .expect("bundle issues");
        let raised = node
            .ingest_org_revocation_bundle(&bundle.to_bytes())
            .expect("bundle verifies");
        assert_eq!(raised, 1, "exactly one floor rose");

        // TWO BRANCHES: the generation-3 call retires Denied (the
        // frozen `Revoked → AdmissionDenied(Denied) + &[0]` byte is
        // asserted byte-exact in the loopback twin above) ...
        assert_eq!(handles[0].retired(), Some(RetireReason::Revoked));
        assert_eq!(
            handles[0].send(b"after"),
            Err(SinkError::Closed),
            "its sink refuses typed"
        );
        // ... while the compliant sibling keeps delivering.
        assert_eq!(handles[1].retired(), None);
        handles[1]
            .send(b"gen5-more")
            .expect("the sibling keeps delivering");
        handles[1].finish(HandlerResult::Ok);
        node.tick(clock::now());
        assert_eq!(
            handles[1].retired(),
            None,
            "and completes without retirement"
        );
    }

    /// The 16-witness matrix blocker: an inbound Subscribe for a served
    /// call's reply channel must be ACKED (a caller's
    /// `ensure_reply_subscription` waits on the Ack before it will send
    /// a REQUEST), rostered, and the reply route must then DELIVER.
    #[test]
    fn a_reply_channel_subscribe_is_acked_rostered_and_the_reply_route_delivers() {
        let world = World::at(clock::now_unix_secs());
        let peer_ident = peer_identity();
        let peer = peer_ident.node_id();
        let (mut node, peer_session) = connected(peer);
        let signed = net_leaf::announce::build_announcement(
            &peer_ident,
            &["test.anchor".to_string()],
            1,
            clock::now_unix_nanos(),
            300,
        )
        .expect("announcement");
        assert!(node.ingest_announcement(&signed));

        let calls: Rc<RefCell<Vec<ServeCall>>> = Rc::new(RefCell::new(Vec::new()));
        let handler_calls = Rc::clone(&calls);
        node.org_serve(
            SERVICE,
            ServeOptions {
                shape: RpcCallShape::ServerStreaming,
                access: ServeAccess::SameOrg,
                provider_owner_org: world.owner_org,
                skew_secs: 0,
                default_live_ns: 300 * 1_000_000_000,
                max_live_ns: 3600 * 1_000_000_000,
                policy: None,
            },
            Rc::new(move |call| handler_calls.borrow_mut().push(call)),
        )
        .expect("serve");
        let _ = drain_events(&peer_session, &mut node);

        // The caller subscribes its OWN reply channel and waits.
        let caller_origin = world.caller_entity.origin_hash();
        let reply_name = format!("{SERVICE}.replies.{caller_origin:016x}");
        let reply_route = Channel::new(&reply_name).expect("channel").canonical();
        let subscribe = MembershipMsg::Subscribe {
            channel: net_wire::channel::name::ChannelName::new(&reply_name).expect("name"),
            nonce: 77,
            token: None,
            queue_group: None,
        };
        let packet = peer_packet(
            &peer_session,
            u64::from(net_leaf::channel::SUBPROTOCOL_MEMBERSHIP),
            net_leaf::channel::SUBPROTOCOL_MEMBERSHIP,
            reply_route as u16,
            &membership::encode(&subscribe),
        );
        node.on_datagram(peer, packet, clock::now());
        let seen = acks(&drain_events(&peer_session, &mut node));
        assert!(
            seen.contains(&MembershipMsg::Ack {
                nonce: 77,
                accepted: true,
                reason: None
            }),
            "the Subscribe is ACKED — without this the caller dies at its \
             membership-ack timeout before the first REQUEST: {seen:?}"
        );
        assert!(
            node.is_channel_subscriber(peer, &reply_name),
            "and the subscription is rostered"
        );

        // Unsubscribe is idempotent and always accepted; a re-subscribe
        // re-admits (core's arm semantics).
        let unsubscribe = MembershipMsg::Unsubscribe {
            channel: net_wire::channel::name::ChannelName::new(&reply_name).expect("name"),
            nonce: 80,
        };
        let packet = peer_packet(
            &peer_session,
            u64::from(net_leaf::channel::SUBPROTOCOL_MEMBERSHIP),
            net_leaf::channel::SUBPROTOCOL_MEMBERSHIP,
            reply_route as u16,
            &membership::encode(&unsubscribe),
        );
        node.on_datagram(peer, packet, clock::now());
        let seen = acks(&drain_events(&peer_session, &mut node));
        assert!(seen.contains(&MembershipMsg::Ack {
            nonce: 80,
            accepted: true,
            reason: None
        }));
        assert!(!node.is_channel_subscriber(peer, &reply_name));
        let packet = peer_packet(
            &peer_session,
            u64::from(net_leaf::channel::SUBPROTOCOL_MEMBERSHIP),
            net_leaf::channel::SUBPROTOCOL_MEMBERSHIP,
            reply_route as u16,
            &membership::encode(&subscribe),
        );
        node.on_datagram(peer, packet, clock::now());
        let _ = drain_events(&peer_session, &mut node);
        assert!(node.is_channel_subscriber(peer, &reply_name));

        // THE REPLY ROUTE DELIVERS: a served call's terminal rides
        // exactly the channel the caller subscribed.
        let request_route =
            Channel::from_name(channel::request_channel(SERVICE).expect("c")).canonical();
        let binding = node.peer_session_binding(peer).expect("session binding");
        let node_entity = EntityId::from_bytes(*node.identity().entity().entity_id());
        let intent = world.intent_at(5, node_entity);
        let mut req = RpcRequestPayload {
            service: SERVICE.to_string(),
            deadline_ns: 0,
            flags: FLAG_RPC_STREAMING_RESPONSE,
            headers: Vec::new(),
            body: Bytes::from_static(b"open"),
        };
        attach_signed_admission(
            &mut req,
            &intent,
            0x3201,
            SERVICE,
            RpcCallShape::ServerStreaming,
            Some(binding),
            clock::now_unix_nanos(),
        )
        .expect("mint");
        let frame =
            rpc_wire::encode_request_frame(caller_origin, 0x3201, request_route, &req).expect("f");
        let packet = peer_packet(
            &peer_session,
            channel::publish_stream_id(request_route),
            0,
            request_route as u16,
            &frame,
        );
        node.on_datagram(peer, packet, clock::now());
        assert_eq!(calls.borrow().len(), 1, "the opening was admitted");
        let call = calls.borrow()[0].clone();
        call.send(b"item").expect("send");
        call.finish(net_leaf::rpc_serve::HandlerResult::Ok);
        node.tick(clock::now());

        let mut responses = Vec::new();
        for event in drain_events(&peer_session, &mut node) {
            if let Ok(Some(rpc_wire::RpcFrame::Response { payload, .. })) =
                rpc_wire::decode_frame(event.clone())
            {
                assert_eq!(
                    rpc_wire::decode_route(&event),
                    Some(reply_route),
                    "every RESPONSE rides exactly the reply route the caller subscribed"
                );
                responses.push(payload);
            }
        }
        assert_eq!(
            responses,
            vec![super::continue_chunk(b"item"), super::end_terminal(),],
            "the items and the one terminal deliver in order on the reply route"
        );
    }

    /// The discriminating negatives of `authorize_subscribe`: a peer
    /// may subscribe only the reply channel carrying its OWN origin,
    /// and only channels this leaf serves.
    #[test]
    fn a_foreign_origin_or_unknown_channel_subscribe_is_refused_typed() {
        let world = World::at(clock::now_unix_secs());
        let peer_ident = peer_identity();
        let peer = peer_ident.node_id();
        let (mut node, peer_session) = connected(peer);
        let signed = net_leaf::announce::build_announcement(
            &peer_ident,
            &["test.anchor".to_string()],
            1,
            clock::now_unix_nanos(),
            300,
        )
        .expect("announcement");
        assert!(node.ingest_announcement(&signed));
        node.org_serve(
            SERVICE,
            ServeOptions {
                shape: RpcCallShape::ServerStreaming,
                access: ServeAccess::SameOrg,
                provider_owner_org: world.owner_org,
                skew_secs: 0,
                default_live_ns: 300 * 1_000_000_000,
                max_live_ns: 3600 * 1_000_000_000,
                policy: None,
            },
            Rc::new(|_call| {}),
        )
        .expect("serve");
        let _ = drain_events(&peer_session, &mut node);

        let caller_origin = world.caller_entity.origin_hash();
        let foreign = format!("{SERVICE}.replies.{:016x}", caller_origin ^ 1);
        let unknown = format!("nosuchsvc.replies.{caller_origin:016x}");
        for (name, nonce, want) in [
            (foreign, 78u64, AckReason::Unauthorized),
            (unknown, 79, AckReason::UnknownChannel),
        ] {
            let subscribe = MembershipMsg::Subscribe {
                channel: net_wire::channel::name::ChannelName::new(&name).expect("name"),
                nonce,
                token: None,
                queue_group: None,
            };
            let hash = Channel::new(&name).expect("channel").canonical();
            let packet = peer_packet(
                &peer_session,
                u64::from(net_leaf::channel::SUBPROTOCOL_MEMBERSHIP),
                net_leaf::channel::SUBPROTOCOL_MEMBERSHIP,
                hash as u16,
                &membership::encode(&subscribe),
            );
            node.on_datagram(peer, packet, clock::now());
            let seen = acks(&drain_events(&peer_session, &mut node));
            assert!(
                seen.contains(&MembershipMsg::Ack {
                    nonce,
                    accepted: false,
                    reason: Some(want)
                }),
                "{name:?} must be refused {want:?}: {seen:?}"
            );
            assert!(!node.is_channel_subscriber(peer, &name));
        }
    }

    /// Fix-(1) witness at the NODE VERB (the wasm `callOrgStreaming`
    /// seam): a windowed server-streaming call OPENS with a real
    /// `stream_window_initial` (the mint carries the window header and
    /// the proof over the finalized request), then grants exactly one
    /// `STREAM_GRANT` per consumed chunk. Together with the
    /// provider-side `response_credit_parks_the_pump_...` this is the
    /// end-to-end window property.
    #[test]
    fn a_windowed_server_streaming_opens_at_the_node_verb_and_grants_one_credit_per_consumed_chunk()
    {
        let world = World::at(clock::now_unix_secs());
        let peer_ident = peer_identity();
        let peer = peer_ident.node_id();
        let (mut node, peer_session) = connected(peer);
        let signed = net_leaf::announce::build_announcement(
            &peer_ident,
            &["test.anchor".to_string()],
            1,
            clock::now_unix_nanos(),
            300,
        )
        .expect("announcement");
        assert!(node.ingest_announcement(&signed));

        // The peer is the PROVIDER here: the intent binds the pinned
        // provider entity (= the peer's announced entity).
        let intent = world.intent_at(5, world.caller_entity.clone());
        let handle = node
            .call_org_server_stream(
                peer,
                SERVICE,
                StreamOpen {
                    body: Bytes::from_static(b"request"),
                    deadline_ns: 0,
                    stream_window_initial: Some(24),
                    request_window_initial: None,
                },
                intent,
            )
            .expect("a windowed SS call MUST open at the verb");
        let call_id = handle.call_id;

        // The opening frame is a well-formed minted REQUEST carrying
        // the window header AND exactly one proof header.
        let request_route =
            Channel::from_name(channel::request_channel(SERVICE).expect("c")).canonical();
        // The node's OWN reply channel — the one `ensure_reply_subscription`
        // registers and the pin's `reply_route` names. (Addressing the
        // peer's origin here instead lands the frame on the Application
        // plane — the four-fact `CallOwner` doing its job.)
        let reply_route =
            Channel::from_name(channel::reply_channel(SERVICE, node.origin_hash()).expect("c"))
                .canonical();
        let mut request = None;
        for event in drain_events(&peer_session, &mut node) {
            if let Ok(Some(rpc_wire::RpcFrame::Request(req))) =
                rpc_wire::decode_frame(event.clone())
            {
                assert_eq!(rpc_wire::decode_route(&event), Some(request_route));
                request = Some(req);
            }
        }
        let request = request.expect("the eager opening goes out");
        assert_eq!(request.flags, FLAG_RPC_STREAMING_RESPONSE);
        assert!(
            request.headers.iter().any(|(n, v)| {
                n.eq_ignore_ascii_case(HEADER_NRPC_STREAM_WINDOW_INITIAL) && v == b"24"
            }),
            "the window header rides the opening verbatim: {:?}",
            request.headers
        );
        assert_eq!(
            request
                .headers
                .iter()
                .filter(|(n, _)| n == "net-org-admission")
                .count(),
            1,
            "exactly one minted proof header — the window header does \
             not break the mint/digest path"
        );

        // The provider streams 2 items + end within the 24-credit
        // window; consuming them grants exactly one credit each.
        for (body, terminal) in [(b"one".as_slice(), false), (b"two", false), (b"", true)] {
            let payload = if terminal {
                super::end_terminal()
            } else {
                super::continue_chunk(body)
            };
            let frame =
                rpc_wire::encode_response_frame(0x5EED, call_id, reply_route, &payload).expect("f");
            let packet = peer_packet(
                &peer_session,
                channel::publish_stream_id(reply_route),
                0,
                reply_route as u16,
                &frame,
            );
            node.on_datagram(peer, packet, clock::now());
        }
        assert_eq!(
            node.org_call_next(call_id),
            Some(Bytes::from_static(b"one"))
        );
        assert_eq!(
            node.org_call_next(call_id),
            Some(Bytes::from_static(b"two"))
        );
        assert_eq!(
            node.org_call_terminal(call_id),
            Some(StreamTerminal::Completed { body: Bytes::new() })
        );

        let mut grants = Vec::new();
        for event in drain_events(&peer_session, &mut node) {
            if let Ok(Some(rpc_wire::RpcFrame::StreamGrant { credits, .. })) =
                rpc_wire::decode_frame(event)
            {
                grants.push(credits);
            }
        }
        assert_eq!(
            grants,
            vec![1, 1],
            "exactly one STREAM_GRANT per consumed chunk"
        );
    }
}
