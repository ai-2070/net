//! The org-scoped nRPC streaming **caller**: one sans-IO state machine
//! per shape (server-streaming, client-streaming, duplex) and the
//! signed-opening mint every protected caller shares with the unary
//! path.
//!
//! # Transport: the event plane, never an app stream
//!
//! Every frame a call emits — `REQUEST`, `REQUEST_CHUNK`,
//! `STREAM_GRANT`, `CANCEL` — rides `send_event_plane` on the call's
//! `<service>.requests` channel, exactly like the unary path. That is
//! what makes browser ↔ native interop work: the core's folds consume
//! channel events, not leaf app streams. Frames for the call come back
//! on `<service>.replies.<own origin>`.
//!
//! # Eager and lazy openings
//!
//! - **Server-streaming is EAGER**: the verb finalizes the
//!   `RpcRequestPayload` (streaming-response flag, optional response
//!   window header), mints the kind-1 proof over it and publishes the
//!   `REQUEST` before the verb returns.
//! - **Client-streaming and duplex are LAZY** (the core's contract):
//!   nothing is sent until the first [`StreamCallRegistry::send`] /
//!   [`StreamCallRegistry::finish_sending`]. The initial `REQUEST`'s
//!   body IS the first chunk — the proof is minted THERE, so the signed
//!   opening binds it — except the zero-item degenerate, which sends an
//!   empty body with `FLAG_RPC_REQUEST_END` on the initial `REQUEST`.
//!   Later items ride `DISPATCH_RPC_REQUEST_CHUNK`, and the terminal
//!   upload frame carries `FLAG_RPC_REQUEST_END`.
//!
//! # Provider pin, call ids, attribution
//!
//! A call rides the ONE provider it was opened against — the pinned
//! entity plus the `(peer, incarnation)` pair recorded at opening —
//! and is never re-resolved mid-call. `call_id`s are minted HERE (the
//! caller side), never by a provider: the provider echoes the caller's
//! id in every frame. Replies are matched on [`CallOwner`]'s four
//! facts **before** the entry is consumed — the same rule
//! [`crate::rpc::CallTable::deliver`] enforces for unary — so a frame
//! from another peer or session with the same `call_id` neither
//! delivers nor completes anything.
//!
//! # Drop emits exactly one CANCEL
//!
//! A [`CallHandle`] drop queues exactly one `DISPATCH_RPC_CANCEL` —
//! the §4.3 handle-drop contract, one logical terminal. An explicit
//! [`StreamCallRegistry::cancel`] and the later drop share one guard,
//! so between them exactly one CANCEL frame is ever queued. Typed
//! failures (`fail_incarnation` / `fail_peer` / `fail_all`) disarm the
//! guard first: when the transport is gone there is nothing to cancel
//! and the caller gets its typed failure instead.
//!
//! # Suspension is not a lease (§4.5)
//!
//! Deadlines are **absolute** (`deadline_ns`, unix nanoseconds). A
//! frozen or suspended tab whose ticker stops gets **no** lease
//! extension: the next [`StreamCallRegistry::advance`] sweep retires
//! everything overdue with its deadline terminal. There is **no
//! automatic resume** — a retired or failed call is never transparently
//! re-opened; re-opening is a fresh call with a fresh proof and MAY
//! repeat effects.
//!
//! Everything here is pull-driven: frames in through `on_*`, frames
//! out through [`StreamCallRegistry::take_outbound`], time in through
//! `advance(now)`. No timers, no runtime, no `unsafe`.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use bytes::Bytes;

use crate::control_plane::NodeId;
use crate::identity::EntityKeypair;
use crate::org::cert::{OrgId, OrgMembershipCert};
use crate::org::digest::org_request_digest;
use crate::org::entity::EntityId;
use crate::org::grant::{CapabilityAuthorityId, OrgCapabilityGrant, OrgDispatcherGrant};
use crate::org::proof::{
    OrgCallProof, OrgStreamCallProof, RpcCallShape, MAX_ORG_PROOF_TTL_SECS, ORG_ADMISSION_HEADER,
    STREAM_CALL_KIND_CLIENT_STREAMING, STREAM_CALL_KIND_DUPLEX, STREAM_CALL_KIND_SERVER_STREAMING,
};
use crate::rpc::CallOwner;
use crate::rpc_wire::{
    self, classify_streaming_chunk, encode_chunk_frame, encode_request_frame,
    encode_stream_grant_frame, RpcHeader, RpcRequestChunkPayload, RpcRequestPayload,
    RpcResponsePayload, RpcStatus, StreamingChunkKind, FLAG_RPC_CLIENT_STREAMING_REQUEST,
    FLAG_RPC_REQUEST_END, FLAG_RPC_STREAMING_RESPONSE, HEADER_NRPC_REQUEST_WINDOW_INITIAL,
    HEADER_NRPC_STREAM_WINDOW_INITIAL,
};

/// In-flight streaming calls a leaf will hold — the same bound the
/// unary [`crate::rpc::CallTable`] enforces, for the same reason: a
/// tab that leaked call slots would leak the frames and queued items
/// with them.
pub const MAX_IN_FLIGHT_CALLS: usize = crate::rpc::MAX_IN_FLIGHT_CALLS;

/// The accumulated `REQUEST_GRANT` ceiling per call, mirroring core's
/// `REQUEST_GRANT_PER_CALL_CAP`: a misbehaving provider cannot make a
/// caller's upload credit grow without bound.
pub const REQUEST_GRANT_PER_CALL_CAP: u32 = 1_000_000;

/// Why a signed opening could not be minted. Local, before anything
/// touches the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MintError {
    /// The request already carries a `net-org-admission` header. Only
    /// the mint sets it — refuse rather than append a second one and
    /// let the provider deny `MultipleHeaders`.
    CallerSuppliedProof,
    /// A streaming proof binds the receiving session's handshake hash
    /// (§1.3). A streaming mint without one fails LOCAL, fail-closed.
    MissingSessionBinding,
    /// The TTL must be `1..=MAX_ORG_PROOF_TTL_SECS`.
    BadTtl,
    /// The intent's capability is not `nrpc:<service>`.
    CapabilityMismatch,
    /// Digest or proof encoding failed.
    Encode(String),
    /// The finalized request (proof header included) exceeds the wire
    /// bounds.
    WireBounds(String),
}

/// Why a call could not be opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallOpenError {
    /// The signed opening could not be minted (or could never be).
    Mint(MintError),
    /// `Some(0)` for either window means "send must await a credit
    /// that can never arrive" — refused locally, exactly as the core
    /// refuses it at call time.
    ZeroWindow,
    /// The table is at [`MAX_IN_FLIGHT_CALLS`].
    TooManyCalls,
}

/// The refusal a sink hands back. `Closed` is the `RpcSinkClosed`-class
/// refusal: the call is retired, the producer is finished, or the
/// shape has no upload half to push into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SinkError {
    /// The sink is closed for further items (retired, finished, or not
    /// an upload sink at all — see [`StreamCallRegistry::send`]).
    Closed,
    /// No upload credit is available right now and **the item was NOT
    /// taken**. This is core's `send().await` permit wait in pull
    /// form: one credit per item frame (`REQUEST` body chunk or
    /// `REQUEST_CHUNK`), replenished by `REQUEST_GRANT`. Retry after
    /// a grant arrives — the JS surface keeps its promise pending and
    /// re-polls on the ticker/grant, so a `send()` resolves only as
    /// grants arrive.
    WouldBlock,
    /// The bounded queue is full. A protected call cannot drop an item
    /// and later report success, so this retires the call
    /// ([`RetireReason::ResourceExhausted`]).
    ResourceExhausted,
    /// The lazy opening could not be minted. The call cannot proceed;
    /// cancel it or let its deadline retire it.
    Mint(MintError),
}

/// Why a call retired locally, and the stable string the JS surface
/// serializes. Wire terminals observed from the provider are NOT this
/// type — they arrive verbatim as [`StreamTerminal::Refused`] or
/// [`StreamTerminal::Completed`] (§4.3: "midstream retirement arrives
/// as the stream's final `Err(AdmissionDenied(Denied))`").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetireReason {
    /// The absolute deadline fired (or the sweep found it overdue).
    Timeout,
    /// Explicit cancel or handle drop.
    Cancelled,
    /// A revocation floor rose past the credential's generation.
    Revoked,
    /// The session carrying the call went away.
    SessionLost,
    /// This tab stopped being the leader.
    LeaderLost,
    /// The node is closing.
    NodeClosed,
    /// The session was replaced by a successor incarnation.
    Replaced,
    /// An item could not be admitted to a bounded queue.
    ResourceExhausted,
}

impl RetireReason {
    /// The stable wire/JS spelling. The seven lifecycle strings are a
    /// frozen vocabulary; `resource-exhausted` is the extra.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
            Self::Revoked => "revoked",
            Self::SessionLost => "session-lost",
            Self::LeaderLost => "leader-lost",
            Self::NodeClosed => "node-closed",
            Self::Replaced => "replaced",
            Self::ResourceExhausted => "resource-exhausted",
        }
    }

    /// The wire terminal vocabulary this retirement maps onto.
    pub fn wire_reason(self) -> rpc_wire::StreamTerminalReason {
        use rpc_wire::StreamTerminalReason as W;
        match self {
            Self::Timeout => W::Timeout,
            Self::Cancelled => W::Cancelled,
            Self::Revoked => W::Revoked,
            Self::SessionLost | Self::Replaced => W::SessionReplaced,
            Self::LeaderLost | Self::NodeClosed => W::ServeHandleDropped,
            Self::ResourceExhausted => W::ResourceExhausted,
        }
    }
}

/// What a caller observes when its call ends. Exactly one of these is
/// latched per call; a second terminal event is a no-op.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamTerminal {
    /// The `Ok` terminal: the `end` marker (SS/DX, empty body) or the
    /// client-streaming single response's body.
    Completed {
        /// The terminal body — the CS aggregate, empty for SS/DX.
        body: Bytes,
    },
    /// A non-`Ok` terminal RESPONSE, verbatim: the status code plus the
    /// diagnostic body. `AdmissionDenied` carries the one-byte coarse
    /// reason (`0` Denied, `1` NotSupported, `2` Unavailable) as its
    /// body, so typed codes reach the JS surface unchanged.
    Refused {
        /// The terminal's `RpcStatus`.
        status: RpcStatus,
        /// The diagnostic body, byte-exact.
        body: Bytes,
    },
    /// A local retirement (deadline sweep, drop, session loss, …).
    Retired {
        /// Why.
        reason: RetireReason,
    },
}

/// The credential material and binding facts one protected call signs
/// with. Cloned into the call (cheaply: the keypair is shared), so a
/// lazy-opening shape can mint at its first `send`.
#[derive(Clone)]
pub struct OrgCallIntent {
    /// The caller's entity signing key. Shared (`Rc`) — the key never
    /// leaves the node and is never serialized here.
    pub keypair: Rc<EntityKeypair>,
    /// The caller's org membership certificate.
    pub membership: OrgMembershipCert,
    /// The dispatcher grant empowering the caller to act for its org.
    pub dispatcher_grant: OrgDispatcherGrant,
    /// The cross-org capability grant, for granted calls only.
    pub capability_grant: Option<OrgCapabilityGrant>,
    /// The org the caller acts for.
    pub acting_org: OrgId,
    /// The org that owns the provider.
    pub provider_org: OrgId,
    /// The pinned provider entity (`callee` of the binding).
    pub provider: EntityId,
    /// The capability being invoked; must equal `nrpc:<service>`.
    pub capability: CapabilityAuthorityId,
    /// Proof TTL in seconds: `1..=MAX_ORG_PROOF_TTL_SECS`.
    pub ttl_secs: u64,
}

/// Where a caller-side call rides, pinned at opening (the Stage-3 pin
/// discipline: entity + `(peer, incarnation)`, never re-resolved).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallPin {
    /// The authenticated transport peer.
    pub peer: NodeId,
    /// The session incarnation the call was opened on.
    pub incarnation: u64,
    /// The pinned provider entity.
    pub provider: EntityId,
    /// Canonical route of `<service>.requests` — every caller→provider
    /// frame rides it.
    pub request_route: u64,
    /// Canonical route of `<service>.replies.<own origin>` — every
    /// provider→caller frame must declare it.
    pub reply_route: u64,
    /// The stream id of the reply channel: the fourth `CallOwner` fact.
    pub carrier_stream_id: u64,
}

impl CallPin {
    /// The four facts a reply must present to be this call's reply.
    pub fn owner(&self) -> CallOwner {
        CallOwner {
            peer: self.peer,
            incarnation: self.incarnation,
            reply_route: self.reply_route,
            carrier_stream_id: self.carrier_stream_id,
        }
    }
}

/// What one streaming open carries. `body` is the request payload
/// (server-streaming) or the first upload item (client-streaming /
/// duplex — their openings are lazy).
#[derive(Debug, Clone, Default)]
pub struct StreamOpen {
    /// The request body (SS) or first upload item (CS/DX).
    pub body: Bytes,
    /// Absolute deadline, unix nanoseconds. `0` = none. Swept
    /// absolutely: a suspended tab gets no extension.
    pub deadline_ns: u64,
    /// Response-direction window (SS/DX). `Some(0)` is refused.
    pub stream_window_initial: Option<u32>,
    /// Upload-direction window (CS/DX). `Some(0)` is refused.
    pub request_window_initial: Option<u32>,
}

/// One frame ready for the node's event plane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutFrame {
    /// The peer to send to (the pinned provider).
    pub peer: NodeId,
    /// The canonical channel route the frame rides.
    pub route: u64,
    /// The complete `EventMeta ‖ RpcRouteV1 ‖ payload` frame.
    pub frame: Vec<u8>,
}

/// The caller-side handle. Dropping it queues exactly ONE `CANCEL` —
/// unless the call already ended or a cancel already fired (the guard
/// is shared with the registry), in which case dropping emits nothing.
///
/// Suspension/closure (§4.5): dropping the handle (tab teardown) is a
/// cancellation, never a lease extension, and never an automatic
/// resume — re-opening is a fresh call with a fresh proof and MAY
/// repeat effects.
#[derive(Debug)]
pub struct CallHandle {
    /// The caller-minted correlation id.
    pub call_id: u64,
    /// The exactly-one-CANCEL guard, shared with the registry.
    fired: Rc<Cell<bool>>,
    /// The registry's outbound queue, shared so `Drop` needs no
    /// `&mut` registry.
    out: Rc<RefCell<VecDeque<OutFrame>>>,
    /// The pre-built CANCEL frame, pushed once on drop if unfired.
    on_drop: Option<OutFrame>,
}

impl Drop for CallHandle {
    fn drop(&mut self) {
        if self.fired.replace(true) {
            return;
        }
        if let Some(frame) = self.on_drop.take() {
            self.out.borrow_mut().push_back(frame);
        }
    }
}

/// The credential-independent facts a lazy machine mints its opening
/// with. Held with the intent until first `send`/`finish_sending`.
struct PendingOpen {
    intent: OrgCallIntent,
    session_binding: Option<[u8; 32]>,
}

/// One caller-side streaming call.
struct CallCore {
    shape: RpcCallShape,
    pin: CallPin,
    service: String,
    call_id: u64,
    /// Absolute local sweep bound; `0` = none.
    deadline_ns: u64,
    origin_hash: u64,
    /// Lazy shapes hold their opening until first use.
    pending: Option<PendingOpen>,
    /// The finalized opening, once sent.
    opened: bool,
    /// The window the opening advertises.
    stream_window_initial: Option<u32>,
    request_window_initial: Option<u32>,
    /// Upload half (CS/DX).
    upload_open: bool,
    /// The open verb's pre-supplied first item, awaiting the lazy
    /// opening (its body IS the first chunk).
    pending_first: Option<Bytes>,
    upload_credit: Option<u32>,
    end_sent: bool,
    /// Response half (SS/DX items; CS holds its single response).
    resp_items: VecDeque<Bytes>,
    /// Latched exactly once.
    terminal: Option<StreamTerminal>,
    /// The shared exactly-one-CANCEL guard.
    fired: Rc<Cell<bool>>,
}

impl CallCore {
    /// Latch a terminal exactly once. Returns whether this call's
    /// terminal was set by THIS invocation (first writer wins).
    ///
    /// Latching disarms the drop guard: the call has ended, so a later
    /// handle drop must emit no CANCEL (§4.3's one logical terminal).
    /// Callers that owe the server a CANCEL (`cancel`, the deadline
    /// sweep) push the frame BEFORE latching.
    fn latch(&mut self, terminal: StreamTerminal) -> bool {
        if self.terminal.is_some() {
            return false;
        }
        self.terminal = Some(terminal);
        self.fired.replace(true);
        true
    }
}

/// Queue the call's one CANCEL frame, unless the guard already fired.
fn push_cancel(out: &Rc<RefCell<VecDeque<OutFrame>>>, core: &CallCore) {
    if core.fired.replace(true) {
        return;
    }
    out.borrow_mut().push_back(OutFrame {
        peer: core.pin.peer,
        route: core.pin.request_route,
        frame: rpc_wire::encode_cancel_frame(core.origin_hash, core.call_id, core.pin.request_route),
    });
}

/// The caller-side call table: one machine per open call, an outbound
/// frame queue the node pumps, and the shared signed-opening mint.
pub struct StreamCallRegistry {
    origin_hash: u64,
    next_call_id: u64,
    calls: HashMap<u64, CallCore>,
    out: Rc<RefCell<VecDeque<OutFrame>>>,
}

impl core::fmt::Debug for StreamCallRegistry {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("StreamCallRegistry")
            .field("in_flight", &self.calls.len())
            .finish()
    }
}

impl StreamCallRegistry {
    /// A table stamping `origin_hash` on its frames, with a call-id
    /// sequence seeded from the CSPRNG (see
    /// [`crate::rpc::CallTable::with_seed`] for why it is not zero).
    ///
    /// The seed's bit 63 is FLIPPED: the unary table and this one then
    /// walk opposite half-spaces of the `u64` id space from seeds
    /// 2^63 apart, so two wrapping counters cannot mint equal ids
    /// before 2^63 draws from one of them. One id space, partitioned —
    /// because a unary reply and a streaming reply for the same
    /// service share the reply channel and only `call_id` distinguishes
    /// them.
    pub fn new(origin_hash: u64, call_id_seed: u64) -> Self {
        Self {
            origin_hash,
            next_call_id: call_id_seed ^ (1 << 63),
            calls: HashMap::new(),
            out: Rc::new(RefCell::new(VecDeque::new())),
        }
    }

    /// How many calls are in flight.
    pub fn in_flight(&self) -> usize {
        self.calls.len()
    }

    /// Whether `call_id` is this registry's call AND `presented`
    /// matches its four facts — the routing peek the node takes before
    /// consuming a frame. The `on_*` entry points re-verify before
    /// consuming (the matched-before-removal rule), so a peek is never
    /// an authorization.
    pub fn owns(&self, presented: CallOwner, call_id: u64) -> bool {
        self.calls
            .get(&call_id)
            .is_some_and(|c| c.pin.owner() == presented)
    }

    /// Open a SERVER-STREAMING call — EAGER: the signed `REQUEST` is
    /// queued before this returns.
    pub fn open_server_streaming(
        &mut self,
        pin: CallPin,
        service: &str,
        open: StreamOpen,
        intent: OrgCallIntent,
        session_binding: Option<[u8; 32]>,
        now_unix_ns: u64,
    ) -> Result<CallHandle, CallOpenError> {
        self.open(
            RpcCallShape::ServerStreaming,
            pin,
            service,
            open,
            intent,
            session_binding,
            now_unix_ns,
        )
    }

    /// Open a CLIENT-STREAMING call — LAZY: nothing is sent until the
    /// first [`Self::send`] or [`Self::finish_sending`].
    pub fn open_client_streaming(
        &mut self,
        pin: CallPin,
        service: &str,
        open: StreamOpen,
        intent: OrgCallIntent,
        session_binding: Option<[u8; 32]>,
        now_unix_ns: u64,
    ) -> Result<CallHandle, CallOpenError> {
        self.open(
            RpcCallShape::ClientStreaming,
            pin,
            service,
            open,
            intent,
            session_binding,
            now_unix_ns,
        )
    }

    /// Open a DUPLEX call — LAZY, with independent upload/response
    /// halves.
    pub fn open_duplex(
        &mut self,
        pin: CallPin,
        service: &str,
        open: StreamOpen,
        intent: OrgCallIntent,
        session_binding: Option<[u8; 32]>,
        now_unix_ns: u64,
    ) -> Result<CallHandle, CallOpenError> {
        self.open(
            RpcCallShape::Duplex,
            pin,
            service,
            open,
            intent,
            session_binding,
            now_unix_ns,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn open(
        &mut self,
        shape: RpcCallShape,
        pin: CallPin,
        service: &str,
        open: StreamOpen,
        intent: OrgCallIntent,
        session_binding: Option<[u8; 32]>,
        now_unix_ns: u64,
    ) -> Result<CallHandle, CallOpenError> {
        if matches!(open.stream_window_initial, Some(0))
            || matches!(open.request_window_initial, Some(0))
        {
            return Err(CallOpenError::ZeroWindow);
        }
        // Fail-fast local mint preconditions (they are re-checked
        // inside the mint, which the unary path shares).
        check_mint_preconditions(&intent, service, shape, session_binding)?;
        if self.calls.len() >= MAX_IN_FLIGHT_CALLS {
            return Err(CallOpenError::TooManyCalls);
        }
        let call_id = self.next_call_id;
        self.next_call_id = self.next_call_id.wrapping_add(1);

        let eager = matches!(shape, RpcCallShape::ServerStreaming);
        let mut core = CallCore {
            shape,
            pin,
            service: service.to_string(),
            call_id,
            deadline_ns: open.deadline_ns,
            origin_hash: self.origin_hash,
            pending: Some(PendingOpen {
                intent,
                session_binding,
            }),
            opened: false,
            stream_window_initial: open.stream_window_initial,
            request_window_initial: open.request_window_initial,
            upload_open: !eager,
            pending_first: None,
            upload_credit: open.request_window_initial,
            end_sent: false,
            resp_items: VecDeque::new(),
            terminal: None,
            fired: Rc::new(Cell::new(false)),
        };
        let fired = Rc::clone(&core.fired);

        if eager {
            // EAGER opening (SS): the body is the whole request.
            let body = open.body;
            emit_open(&self.out, &mut core, body, false, now_unix_ns)
                .map_err(CallOpenError::Mint)?;
        } else if !open.body.is_empty() {
            core.pending_first = Some(open.body);
        }
        let handle = CallHandle {
            call_id,
            fired,
            out: Rc::clone(&self.out),
            on_drop: Some(OutFrame {
                peer: core.pin.peer,
                route: core.pin.request_route,
                frame: rpc_wire::encode_cancel_frame(
                    core.origin_hash,
                    call_id,
                    core.pin.request_route,
                ),
            }),
        };
        self.calls.insert(call_id, core);
        Ok(handle)
    }

    /// Push one upload item (CS/DX) — core's `send().await` permit
    /// semantics in pull form: ONE item frame per `send`, ONE credit
    /// per frame. With an upload window and no credit the item is NOT
    /// taken and [`SinkError::WouldBlock`] comes back; the caller
    /// retries after a `REQUEST_GRANT` arrives (the JS surface keeps
    /// its promise pending and re-polls on the ticker/grant), so a
    /// `send()` resolves only as grants arrive.
    pub fn send(&mut self, call_id: u64, item: &[u8], now_unix_ns: u64) -> Result<(), SinkError> {
        let Some(core) = self.calls.get_mut(&call_id) else {
            return Err(SinkError::Closed);
        };
        if !core.upload_open
            || core.terminal.is_some()
            || !matches!(
                core.shape,
                RpcCallShape::ClientStreaming | RpcCallShape::Duplex
            )
        {
            return Err(SinkError::Closed);
        }
        if !core.opened {
            // LAZY opening: the initial REQUEST's body IS the first
            // chunk — the open verb's body when it supplied one, else
            // THIS item — minted here so the signed opening binds it.
            match core.pending_first.take() {
                Some(first) => {
                    if core.upload_credit == Some(0) {
                        core.pending_first = Some(first);
                        return Err(SinkError::WouldBlock);
                    }
                    emit_open(&self.out, core, first, false, now_unix_ns)
                        .map_err(SinkError::Mint)?;
                    // Fall through: `item` is the SECOND chunk.
                }
                None => {
                    if core.upload_credit == Some(0) {
                        return Err(SinkError::WouldBlock);
                    }
                    emit_open(
                        &self.out,
                        core,
                        Bytes::copy_from_slice(item),
                        false,
                        now_unix_ns,
                    )
                    .map_err(SinkError::Mint)?;
                    return Ok(());
                }
            }
        }
        // One credit per item frame.
        if core.upload_credit == Some(0) {
            return Err(SinkError::WouldBlock);
        }
        let out = Rc::clone(&self.out);
        emit_upload_chunk(&out, core, Bytes::copy_from_slice(item), 0, true)
    }

    /// Half-close the upload (DX `finish_sending`) / finish it (CS
    /// `finish`). The terminal upload frame carries
    /// `FLAG_RPC_REQUEST_END` and pays no credit; the zero-item
    /// degenerate and core's one-item path ride the initial `REQUEST`
    /// (the one-item frame pays its one credit).
    pub fn finish_sending(&mut self, call_id: u64, now_unix_ns: u64) -> Result<(), SinkError> {
        let Some(core) = self.calls.get_mut(&call_id) else {
            return Err(SinkError::Closed);
        };
        if !core.upload_open
            || core.terminal.is_some()
            || !matches!(
                core.shape,
                RpcCallShape::ClientStreaming | RpcCallShape::Duplex
            )
        {
            return Err(SinkError::Closed);
        }
        if !core.opened {
            let body = match core.pending_first.take() {
                Some(first) => {
                    if core.upload_credit == Some(0) {
                        // The one-item opening cannot go out without
                        // its credit: park the whole finish, item and
                        // half-close together.
                        core.pending_first = Some(first);
                        return Err(SinkError::WouldBlock);
                    }
                    first
                }
                // Zero-item degenerate: empty body + FLAG_END, no
                // credit.
                None => Bytes::new(),
            };
            core.upload_open = false;
            emit_open(&self.out, core, body, true, now_unix_ns).map_err(SinkError::Mint)?;
            return Ok(());
        }
        core.upload_open = false;
        let out = Rc::clone(&self.out);
        emit_upload_chunk(&out, core, Bytes::new(), FLAG_RPC_REQUEST_END, false)
    }

    /// Pull the next response item. Consuming an item on a
    /// window-issuing call auto-grants one `STREAM_GRANT` credit — one
    /// per consumed chunk, the same pacing the provider uses for
    /// `REQUEST_GRANT`.
    pub fn next_item(&mut self, call_id: u64) -> Option<Bytes> {
        let core = self.calls.get_mut(&call_id)?;
        let item = core.resp_items.pop_front()?;
        if core.stream_window_initial.is_some() {
            self.out.borrow_mut().push_back(OutFrame {
                peer: core.pin.peer,
                route: core.pin.request_route,
                frame: encode_stream_grant_frame(
                    core.origin_hash,
                    call_id,
                    core.pin.request_route,
                    1,
                ),
            });
        }
        Some(item)
    }

    /// Explicitly grant `credits` response-direction chunks
    /// (`STREAM_GRANT`). Zero is ignored, exactly as the core fold
    /// ignores it.
    pub fn grant(&mut self, call_id: u64, credits: u32) {
        if credits == 0 {
            return;
        }
        let Some(core) = self.calls.get(&call_id) else {
            return;
        };
        if core.terminal.is_some() {
            return;
        }
        self.out.borrow_mut().push_back(OutFrame {
            peer: core.pin.peer,
            route: core.pin.request_route,
            frame: encode_stream_grant_frame(
                core.origin_hash,
                call_id,
                core.pin.request_route,
                credits,
            ),
        });
    }

    /// Explicitly cancel a call: exactly one `CANCEL` frame (shared
    /// guard with handle drop) and a latched `Cancelled` terminal.
    /// Returns whether this invocation fired it.
    pub fn cancel(&mut self, call_id: u64) -> bool {
        let Some(core) = self.calls.get_mut(&call_id) else {
            return false;
        };
        if core.terminal.is_some() {
            return false;
        }
        // The CANCEL goes out FIRST: latching disarms the drop guard.
        push_cancel(&self.out, core);
        core.latch(StreamTerminal::Retired {
            reason: RetireReason::Cancelled,
        });
        true
    }

    /// The latched terminal, if any. Exactly one is ever produced; a
    /// second terminal event is a no-op.
    pub fn terminal(&self, call_id: u64) -> Option<&StreamTerminal> {
        self.calls.get(&call_id).and_then(|c| c.terminal.as_ref())
    }

    /// Drop a finished call's state. A call with a latched terminal
    /// stays until this (or a handle drop) releases it, so the
    /// terminal remains observable.
    pub fn forget(&mut self, call_id: u64) -> bool {
        self.calls.remove(&call_id).is_some()
    }

    /// An inbound provider→caller RESPONSE, presented with the four
    /// facts a reply must show. Matched BEFORE consumption: a
    /// wrong-owner frame neither delivers nor completes anything and
    /// the correct reply can still arrive afterwards.
    pub fn on_response(
        &mut self,
        presented: CallOwner,
        call_id: u64,
        payload: RpcResponsePayload,
    ) -> bool {
        let Some(core) = self.calls.get_mut(&call_id) else {
            return false;
        };
        if core.pin.owner() != presented || core.terminal.is_some() {
            return false;
        }
        if matches!(core.shape, RpcCallShape::ClientStreaming) {
            // One terminal RESPONSE, always terminal — the CS single
            // response IS the terminal.
            core.latch(terminal_of(payload));
            return true;
        }
        // SS/DX: the marker vocabulary decides. A non-`Ok` status is
        // terminal regardless of headers; `Ok` + `end` is terminal;
        // `Ok` + `continue` is an item.
        match classify_streaming_chunk(&payload) {
            StreamingChunkKind::Continue => {
                core.resp_items.push_back(payload.body);
                true
            }
            StreamingChunkKind::Terminal | StreamingChunkKind::Unary => {
                core.latch(terminal_of(payload));
                true
            }
        }
    }

    /// An inbound `REQUEST_GRANT` (upload-direction credit). Capped at
    /// [`REQUEST_GRANT_PER_CALL_CAP`] accumulated per call, mirroring
    /// the core's `add_request_grant_credits`.
    pub fn on_grant(&mut self, presented: CallOwner, call_id: u64, credits: u32) -> bool {
        let Some(core) = self.calls.get_mut(&call_id) else {
            return false;
        };
        if core.pin.owner() != presented {
            // A wrong-session grant releases no credit.
            return false;
        }
        if core.terminal.is_some() {
            return false;
        }
        if let Some(credit) = core.upload_credit.as_mut() {
            let remaining = REQUEST_GRANT_PER_CALL_CAP.saturating_sub(*credit);
            *credit = credit.saturating_add(credits.min(remaining));
        }
        true
    }

    /// An inbound `DEADLINE_EXCEEDED` frame — decoded exactly like the
    /// provider's `Timeout` terminal (§10.2), so the two are
    /// indistinguishable at the caller.
    pub fn on_deadline_frame(&mut self, presented: CallOwner, call_id: u64) -> bool {
        let Some(core) = self.calls.get_mut(&call_id) else {
            return false;
        };
        if core.pin.owner() != presented || core.terminal.is_some() {
            return false;
        }
        core.latch(StreamTerminal::Refused {
            status: RpcStatus::Timeout,
            body: Bytes::from_static(b"stream deadline_ns exceeded"),
        });
        true
    }

    /// The pull-driven sweep: process drop guards, retire overdue
    /// calls with their deadline terminal, and pump queued uploads.
    ///
    /// **Suspension contract (§4.5):** deadlines are absolute — a
    /// frozen tab's ticker stopping extends nothing. The first
    /// `advance` after wake retires everything overdue, and nothing is
    /// ever transparently re-opened.
    pub fn advance(&mut self, now_unix_ns: u64) {
        let ids: Vec<u64> = self.calls.keys().copied().collect();
        for call_id in ids {
            let Some(core) = self.calls.get_mut(&call_id) else {
                continue;
            };
            if core.terminal.is_some() {
                continue;
            }
            if core.fired.get() {
                // Handle drop: Drop already queued its CANCEL (it held
                // the pre-built frame).
                core.latch(StreamTerminal::Retired {
                    reason: RetireReason::Cancelled,
                });
                continue;
            }
            if core.deadline_ns != 0 && now_unix_ns >= core.deadline_ns {
                // Tell the server, so it is not left running work
                // nobody awaits (the unary sweep's rule) — the frame
                // goes first, latching disarms the drop guard.
                push_cancel(&self.out, core);
                core.latch(StreamTerminal::Retired {
                    reason: RetireReason::Timeout,
                });
                continue;
            }
        }
        // Release calls whose handle is gone: their terminal is
        // unobservable and their drop already fired.
        self.calls
            .retain(|_, c| !(c.terminal.is_some() && Rc::strong_count(&c.fired) == 1));
    }

    /// Drain the frames to hand to the node's event plane.
    pub fn take_outbound(&mut self) -> Vec<OutFrame> {
        self.out.borrow_mut().drain(..).collect()
    }

    /// The pinned session was replaced: retire its calls typed
    /// (`Replaced` by default), no CANCEL (the transport they rode is
    /// gone).
    pub fn fail_incarnation(&mut self, incarnation: u64, reason: RetireReason) -> usize {
        let mut n = 0;
        for core in self.calls.values_mut() {
            if core.pin.incarnation == incarnation && core.latch(StreamTerminal::Retired { reason })
            {
                n += 1;
            }
        }
        n
    }

    /// The peer went away entirely.
    pub fn fail_peer(&mut self, peer: NodeId, reason: RetireReason) -> usize {
        let mut n = 0;
        for core in self.calls.values_mut() {
            if core.pin.peer == peer && core.latch(StreamTerminal::Retired { reason }) {
                n += 1;
            }
        }
        n
    }

    /// Node close / leader loss: every call fails typed, and the drop
    /// guards are disarmed so teardown emits no CANCELs.
    pub fn fail_all(&mut self, reason: RetireReason) -> usize {
        let mut n = 0;
        for core in self.calls.values_mut() {
            if core.latch(StreamTerminal::Retired { reason }) {
                n += 1;
            }
        }
        n
    }
}

/// The wire terminal's caller-side view: `Ok` completes (the CS
/// aggregate or the empty `end` body), anything else is a typed
/// refusal carried verbatim.
fn terminal_of(payload: RpcResponsePayload) -> StreamTerminal {
    if payload.status.is_ok() {
        StreamTerminal::Completed { body: payload.body }
    } else {
        StreamTerminal::Refused {
            status: payload.status,
            body: payload.body,
        }
    }
}

/// Emit one upload `REQUEST_CHUNK` (the caller has checked — and, for
/// `pay_credit`, paid — its credit). `FLAG_RPC_REQUEST_END` rides the
/// terminal upload frame and pays no credit of its own. An
/// un-encodable chunk retires the call: it cannot be dropped and the
/// call still report success.
fn emit_upload_chunk(
    out: &Rc<RefCell<VecDeque<OutFrame>>>,
    core: &mut CallCore,
    body: Bytes,
    flags: u16,
    pay_credit: bool,
) -> Result<(), SinkError> {
    let chunk = RpcRequestChunkPayload {
        call_id: core.call_id,
        flags,
        headers: Vec::new(),
        body,
    };
    let frame = match encode_chunk_frame(core.origin_hash, core.pin.request_route, &chunk) {
        Ok(frame) => frame,
        Err(_) => {
            push_cancel(out, core);
            core.latch(StreamTerminal::Retired {
                reason: RetireReason::ResourceExhausted,
            });
            return Err(SinkError::ResourceExhausted);
        }
    };
    if pay_credit {
        if let Some(credit) = core.upload_credit.as_mut() {
            *credit = credit.saturating_sub(1);
        }
    }
    if flags & FLAG_RPC_REQUEST_END != 0 {
        core.end_sent = true;
    }
    out.borrow_mut().push_back(OutFrame {
        peer: core.pin.peer,
        route: core.pin.request_route,
        frame,
    });
    Ok(())
}

/// Finalize and emit the signed opening `REQUEST`. The proof is minted
/// HERE over the finalized payload (the digest strips the admission
/// header, so caller and provider agree), then exactly one proof header
/// is appended and the FINAL wire is validated.
fn emit_open(
    out: &Rc<RefCell<VecDeque<OutFrame>>>,
    core: &mut CallCore,
    body: Bytes,
    end_on_request: bool,
    now_unix_ns: u64,
) -> Result<(), MintError> {
    let pending = core
        .pending
        .take()
        .ok_or_else(|| MintError::Encode("the opening was already minted".to_string()))?;
    let mut flags = match core.shape {
        RpcCallShape::ServerStreaming => FLAG_RPC_STREAMING_RESPONSE,
        RpcCallShape::ClientStreaming => FLAG_RPC_CLIENT_STREAMING_REQUEST,
        RpcCallShape::Duplex => FLAG_RPC_STREAMING_RESPONSE | FLAG_RPC_CLIENT_STREAMING_REQUEST,
        RpcCallShape::Unary => {
            return Err(MintError::Encode(
                "unary calls ride the unary path".to_string(),
            ))
        }
    };
    if end_on_request {
        flags |= FLAG_RPC_REQUEST_END;
    }
    let mut headers: Vec<RpcHeader> = Vec::new();
    if let Some(window) = core.stream_window_initial {
        headers.push((
            HEADER_NRPC_STREAM_WINDOW_INITIAL.to_string(),
            window.to_string().into_bytes(),
        ));
    }
    if let Some(window) = core.request_window_initial {
        headers.push((
            HEADER_NRPC_REQUEST_WINDOW_INITIAL.to_string(),
            window.to_string().into_bytes(),
        ));
    }
    let mut req = RpcRequestPayload {
        service: core.service.clone(),
        deadline_ns: core.deadline_ns,
        flags,
        headers,
        body,
    };
    let req_body_was_empty = req.body.is_empty();
    attach_signed_admission(
        &mut req,
        &pending.intent,
        core.call_id,
        &core.service,
        core.shape,
        pending.session_binding,
        now_unix_ns,
    )?;
    let frame =
        encode_request_frame(core.origin_hash, core.call_id, core.pin.request_route, &req)
            .map_err(|e| MintError::Encode(format!("request frame: {e}")))?;
    out.borrow_mut().push_back(OutFrame {
        peer: core.pin.peer,
        route: core.pin.request_route,
        frame,
    });
    core.opened = true;
    // The opening's body IS a pushed chunk (core's `send().await`
    // pays one credit per chunk — the lazy REQUEST included); the
    // zero-item degenerate (empty + END) pushes none.
    let carries_item = !req_body_was_empty || !end_on_request;
    if carries_item {
        if let Some(credit) = core.upload_credit.as_mut() {
            *credit = credit.saturating_sub(1);
        }
    }
    if end_on_request {
        core.end_sent = true;
        core.upload_open = false;
    }
    Ok(())
}

/// The local mint preconditions, checked at open so a lazy shape fails
/// at its verb rather than at its first `send`.
fn check_mint_preconditions(
    intent: &OrgCallIntent,
    service: &str,
    shape: RpcCallShape,
    session_binding: Option<[u8; 32]>,
) -> Result<(), CallOpenError> {
    if intent.ttl_secs == 0 || intent.ttl_secs > MAX_ORG_PROOF_TTL_SECS {
        return Err(CallOpenError::Mint(MintError::BadTtl));
    }
    if intent.capability != CapabilityAuthorityId::for_tag(&format!("nrpc:{service}")) {
        return Err(CallOpenError::Mint(MintError::CapabilityMismatch));
    }
    if !matches!(shape, RpcCallShape::Unary) && session_binding.is_none() {
        return Err(CallOpenError::Mint(MintError::MissingSessionBinding));
    }
    Ok(())
}

/// Mint the `net-org-admission` proof header and append it — the leaf
/// mirror of the core's `attach_signed_admission`
/// (`mesh_rpc.rs:8126-8226`), with the named differences documented
/// below:
///
/// - A caller-supplied `net-org-admission` header is **refused**; only
///   the mint sets it (the exactly-one-header discipline).
/// - A streaming mint carries the receiving session's full handshake
///   hash (§1.3); a streaming mint without one fails LOCAL. The unary
///   proof carries no session binding.
/// - The digest is [`org_request_digest`] over the finalized request —
///   it strips the admission header, so caller and provider derive the
///   same digest.
/// - TTL must be `1..=MAX_ORG_PROOF_TTL_SECS`.
/// - The intent's capability must equal `nrpc:<service>`.
/// - The finalized request is validated with `validate_wire_bounds`.
///   Core additionally enforces its one-packet budget there because
///   core cannot fragment; **the leaf's transport fragments**, so the
///   field ceilings are the whole bound and a one-packet measurement
///   would refuse frames this transport delivers correctly.
pub fn attach_signed_admission(
    req: &mut RpcRequestPayload,
    intent: &OrgCallIntent,
    call_id: u64,
    service: &str,
    shape: RpcCallShape,
    session_binding: Option<[u8; 32]>,
    now_unix_ns: u64,
) -> Result<(), MintError> {
    if req
        .headers
        .iter()
        .any(|(name, _)| name == ORG_ADMISSION_HEADER)
    {
        return Err(MintError::CallerSuppliedProof);
    }
    if intent.ttl_secs == 0 || intent.ttl_secs > MAX_ORG_PROOF_TTL_SECS {
        return Err(MintError::BadTtl);
    }
    let capability = CapabilityAuthorityId::for_tag(&format!("nrpc:{service}"));
    if intent.capability != capability {
        return Err(MintError::CapabilityMismatch);
    }
    // §1.3: a STREAMING proof binds the RECEIVING session's full Noise
    // handshake hash. The unary proof binds none.
    let binding = match shape {
        RpcCallShape::Unary => None,
        _ => Some(session_binding.ok_or(MintError::MissingSessionBinding)?),
    };
    let request_digest =
        org_request_digest(req).map_err(|e| MintError::Encode(format!("request digest: {e}")))?;
    let proof_expires_at_unix_ns =
        now_unix_ns.saturating_add(intent.ttl_secs.saturating_mul(1_000_000_000));
    let header_value = match (shape, binding) {
        (RpcCallShape::Unary, _) => OrgCallProof::sign_for_call(
            intent.keypair.as_ref(),
            intent.membership.clone(),
            intent.dispatcher_grant.clone(),
            intent.capability_grant.clone(),
            intent.acting_org,
            intent.provider_org,
            intent.provider.clone(),
            call_id,
            capability,
            proof_expires_at_unix_ns,
            request_digest,
        )
        .encode()
        .map_err(|e| MintError::Encode(format!("proof encode: {e}")))?,
        (_, Some(session_binding)) => {
            let kind = match shape {
                RpcCallShape::ServerStreaming => STREAM_CALL_KIND_SERVER_STREAMING,
                RpcCallShape::ClientStreaming => STREAM_CALL_KIND_CLIENT_STREAMING,
                RpcCallShape::Duplex => STREAM_CALL_KIND_DUPLEX,
                RpcCallShape::Unary => unreachable!("handled above"),
            };
            OrgStreamCallProof::sign_for_stream_call(
                intent.keypair.as_ref(),
                intent.membership.clone(),
                intent.dispatcher_grant.clone(),
                intent.capability_grant.clone(),
                intent.acting_org,
                intent.provider_org,
                intent.provider.clone(),
                call_id,
                capability,
                proof_expires_at_unix_ns,
                request_digest,
                kind,
                session_binding,
            )
            .encode()
            .map_err(|e| MintError::Encode(format!("proof encode: {e}")))?
        }
        (_, None) => return Err(MintError::MissingSessionBinding),
    };
    req.headers
        .push((ORG_ADMISSION_HEADER.to_string(), header_value));
    req.validate_wire_bounds()
        .map_err(|e| MintError::WireBounds(format!("{e}")))?;
    Ok(())
}
