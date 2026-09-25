//! The org-scoped nRPC streaming **provider**: the runtime-free mirror
//! of the core's folds plus `run_stream_call_supervisor`.
//!
//! One [`ServeRegistry`] per node holds the services this leaf serves
//! and every live protected call, keyed on the **AEAD-authenticated**
//! `(peer, incarnation, call_id)` triple. A frame from another peer or
//! session with the same `call_id` neither delivers nor completes
//! anything: request-side frames miss the key and are dropped silently
//! (the core folds' behavior), and nothing here ever mints a call id —
//! the provider echoes the caller's `call_id` in every frame
//! (`EventMeta.seq_or_ts` is the caller's).
//!
//! # Lifecycle (§2.2 / §2.6), in the pull model
//!
//! The record holds `input: Open | Ended | Closed`, an output half and
//! a FIRST-WRITER-WINS `terminal` latch. Handler return ⇒ `Draining`
//! semantics: the producer gate closes, already-queued response items
//! drain in order (only `Completed(_)` drains — every retirement
//! discards) and then the one terminal is emitted through
//! [`rpc_wire::stream_terminal_payload`]. Retirement order is §2.2's,
//! performed synchronously and in order by `retire_into`: latch the
//! terminal reason FIRST, stop emission, close and discard queued
//! input, fence the handler's sink ([`SinkError::Closed`]-class
//! refusals for anything that follows; its result is DISCARDED), drop
//! the remaining response queue, then emit exactly ONE terminal.
//!
//! Deadlines are absolute (`0` on the wire = the provider default
//! bound), swept by [`ServeRegistry::advance`]: a frozen or suspended
//! tab whose ticker stops gets **no** lease extension, the first sweep
//! after wake retires everything overdue with its deadline terminal,
//! and nothing is ever transparently re-opened (§4.5).
//!
//! Everything is pull-driven: frames in through `on_*`, frames out
//! through [`ServeRegistry::take_outbound`], time in through
//! `advance(now)`. No timers, no runtime, no `unsafe`.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;

use bytes::Bytes;

use crate::control_plane::NodeId;
use crate::org::admission::{
    verify_org_admission, AdmissionContext, AdmissionDenied, Admitted, CoarseAdmissionReason,
    OrgAdmission,
};
use crate::org::cert::OrgId;
use crate::org::digest::org_request_digest;
use crate::org::entity::EntityId;
use crate::org::grant::CapabilityAuthorityId;
use crate::org::proof::{OrgCallProof, OrgStreamCallProof, RpcCallShape, ORG_ADMISSION_HEADER};
use crate::org::replay::{AdmissionFailureLimiter, AdmissionReplayGuard};
use crate::org::revocation::RevocationFacts;
use crate::rpc_wire::{
    self, encode_request_grant_frame, encode_response_frame, stream_terminal_payload,
    RpcRequestChunkPayload, RpcRequestPayload, RpcResponsePayload, RpcStatus, StreamHandlerResult,
    StreamTerminalReason, FLAG_RPC_CLIENT_STREAMING_REQUEST, FLAG_RPC_REQUEST_END,
    FLAG_RPC_STREAMING_RESPONSE, HEADER_NRPC_REQUEST_WINDOW_INITIAL, HEADER_NRPC_STREAMING,
    HEADER_NRPC_STREAMING_CONTINUE, HEADER_NRPC_STREAMING_END, HEADER_NRPC_STREAM_WINDOW_INITIAL,
};

pub use crate::rpc_stream::{RetireReason, SinkError, REQUEST_GRANT_PER_CALL_CAP};

/// The handler result vocabulary, re-exported so caller, provider and
/// the JS surface share one type.
pub use rpc_wire::StreamHandlerResult as HandlerResult;

/// Bounded handler-visible queues, per call, in each direction.
const MAX_QUEUED_ITEMS: usize = 1024;

/// Q1's provider lifetime default: 300 s when the caller omits a
/// deadline.
pub const DEFAULT_LIVE_NS: u64 = 300 * 1_000_000_000;
/// Q1's maximum protected lifetime: an explicit deadline beyond
/// `now + MAX_LIVE_NS` is REFUSED, never clamped.
pub const MAX_LIVE_NS: u64 = 3600 * 1_000_000_000;

/// Who may call a served service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServeAccess {
    /// Same-org: the caller acts for THIS leaf's owner org
    /// (`OrgAdmission::OwnerDelegated`).
    SameOrg,
    /// Granted: cross-org capability grant required
    /// (`OrgAdmission::CrossOrgGranted`).
    Granted,
}

impl ServeAccess {
    /// The admission mode this access mode verifies under.
    pub const fn org_mode(self) -> OrgAdmission {
        match self {
            Self::SameOrg => OrgAdmission::OwnerDelegated,
            Self::Granted => OrgAdmission::CrossOrgGranted,
        }
    }
}

/// The provider-local policy closure: given the proof's unary prefix,
/// allow or veto. Runs LAST in admission (Locked #6); a veto is
/// [`AdmissionDenied::ProviderPolicyRejected`] and its replay slot
/// stays consumed (Owner Q4) while the active-call reservation is
/// released.
pub type ServePolicy = Rc<dyn Fn(&OrgCallProof) -> bool>;

/// How one service is served.
#[derive(Clone)]
pub struct ServeOptions {
    /// The shape this registration serves. A REQUEST whose flags claim
    /// another shape is refused typed before admission.
    pub shape: RpcCallShape,
    /// Same-org or granted authority.
    pub access: ServeAccess,
    /// The org that owns THIS leaf (never fold state, never a wire
    /// claim).
    pub provider_owner_org: OrgId,
    /// Clock-skew allowance passed to admission (≤ 300 s).
    pub skew_secs: u64,
    /// Bound applied when the caller omits a deadline.
    pub default_live_ns: u64,
    /// Hard cap: an explicit deadline beyond `now + max_live_ns` is
    /// REFUSED, never clamped.
    pub max_live_ns: u64,
    /// Provider-local policy hook (`None` = admit on the merits).
    pub policy: Option<ServePolicy>,
}

impl core::fmt::Debug for ServeOptions {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ServeOptions")
            .field("shape", &self.shape)
            .field("access", &self.access)
            .field("provider_owner_org", &self.provider_owner_org)
            .finish()
    }
}

/// The handler hook one admitted call is handed: verified caller
/// facts, the request-item source and the response sink, all reachable
/// later from wherever the handler runs (a JS trampoline queues the
/// [`ServeCall`] and drives it after the node's borrow is released —
/// the handle owns its state and never borrows the node).
///
/// # F-S3.1-2 — the handler-drop level, exactly
///
/// "the retire supervisor may drop the handler future without a final
/// poll — cancellation is observed through the retirement observables
/// (the terminal item, the sink's typed closed refusal, and the
/// retired signal), never assumed as a handler-side event".
///
/// A detached observer holding the [`ServeCall`] — its
/// [`ServeCall::retired`] signal — CAN observe retirement.
/// Suspension/closure (§4.5) is the same contract: a frozen tab
/// retires ownership on its absolute deadline with no lease extension
/// and no automatic resume.
pub type ServeHandler = Rc<dyn Fn(ServeCall)>;

/// The authenticated attribution facts one inbound call presents.
/// `caller` is the AEAD-authenticated session entity — never a wire
/// claim — and `session_binding` is the RECEIVING session's handshake
/// hash (§1.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServePeer {
    /// The authenticated transport peer.
    pub peer: NodeId,
    /// The session incarnation the frame arrived on.
    pub incarnation: u64,
    /// The authenticated caller entity (TOFU-pinned).
    pub caller: EntityId,
    /// The receiving session's Noise handshake hash, when it has one.
    pub session_binding: Option<[u8; 32]>,
}

/// The node-held admission inputs one opening verifies against.
pub struct ServeAdmission<'a> {
    /// This leaf's entity (`ctx.provider`).
    pub provider: &'a EntityId,
    /// The merged raise-only revocation view.
    pub facts: &'a RevocationFacts,
    /// The replay guard (insert-or-deny at admission step 10).
    pub replay: &'a AdmissionReplayGuard,
}

/// What an opening REQUEST produced — the typed refusal is the exact
/// `AdmissionDenied` reason; the wire frame carries only its coarse
/// byte ("denial is not a credential oracle").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenOutcome {
    /// Admitted: the per-shape call state exists and the handler hook
    /// was invoked.
    Admitted,
    /// Typed admission refusal.
    Denied(AdmissionDenied),
    /// The §6 failed-admission throttle refused this peer BEFORE any
    /// signature work (core's `OpeningRefusal::Throttled`): the wire
    /// frame carries the coarse `Unavailable` byte, and the peer's
    /// failure budget is NOT charged again (the limiter already
    /// counted the denial).
    Throttled,
    /// Structural refusal before admission (malformed request, wrong
    /// service on the carrier): `UnknownVersion` + `end` + diagnostic.
    Malformed(String),
}

/// One frame ready for the node's event plane (provider → caller; all
/// ride the call's reply route).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeOutFrame {
    /// The authenticated caller peer.
    pub peer: NodeId,
    /// The canonical reply route the frame rides.
    pub route: u64,
    /// The complete `EventMeta ‖ RpcRouteV1 ‖ payload` frame.
    pub frame: Vec<u8>,
}

struct ServeShared {
    caller: Admitted,
    /// The caller's membership generation, for revocation sweeps.
    generation: u32,
    acting_org: OrgId,
    member: EntityId,
    input: VecDeque<Bytes>,
    input_ended: bool,
    /// How many request items the handler has consumed (grant pacing).
    consumed: u64,
    output: VecDeque<Bytes>,
    /// Response-direction credit (§ flow control): `Some(n)` pays one
    /// per [`ServeCall::send`] (the item takes nothing when `Some(0)`),
    /// refilled by `STREAM_GRANT`; `None` = unbounded.
    resp_credit: Option<u32>,
    /// The handler's result, once [`ServeCall::finish`] ran.
    result: Option<StreamHandlerResult>,
    /// The single-response emitter's shape (CS/unary, §2.2): at most
    /// ONE response item may ever be queued — a second [`ServeCall::send`]
    /// is refused [`SinkError::Closed`], never queued to be silently
    /// discarded by the pump.
    single_response: bool,
    /// Producer-finished gate (§2.2: producer finished is NOT
    /// terminal): further sends are refused and a retained sink clone
    /// cannot extend the drain.
    producer_done: bool,
    /// Retirement fence: later sends get the typed closed refusal and
    /// a later result is DISCARDED.
    closed: bool,
    /// The retired signal (an observable, never a handler-side event —
    /// see [`ServeHandler`], F-S3.1-2).
    retired: Option<RetireReason>,
    /// The call's ONE terminal has committed — by completion or by
    /// retirement — and its registry entry is gone. Nothing further can
    /// happen to it; a holder may release it (§23 audit, LEAF-13).
    settled: bool,
}

/// The handler-side handle: verified caller facts, request-item
/// source, response sink and the retired signal. `Clone` shares the
/// one call record — a detached observer holding a clone can observe
/// retirement (F-S3.1-2).
///
/// Suspension/closure (§4.5): retirement is by absolute deadline with
/// no lease extension and no automatic resume; observe it through
/// [`Self::retired`] and the sink's typed refusals.
#[derive(Clone)]
pub struct ServeCall {
    inner: Rc<RefCell<ServeShared>>,
}

impl ServeCall {
    /// The verified org caller — [`Admitted`], never the wire-claimed
    /// origin.
    pub fn caller(&self) -> Admitted {
        self.inner.borrow().caller.clone()
    }

    /// Pull the next request item. `None` at `FLAG_RPC_REQUEST_END` is
    /// EOF — a late chunk after END delivers nothing (and cancels
    /// nothing); each consumed item earns one `REQUEST_GRANT` credit
    /// when the call is upload-windowed.
    pub fn poll_request(&self) -> Option<Bytes> {
        let mut sh = self.inner.borrow_mut();
        let item = sh.input.pop_front()?;
        sh.consumed += 1;
        Some(item)
    }

    /// Whether the upload half has ended (EOF once the queue drains).
    pub fn request_ended(&self) -> bool {
        let sh = self.inner.borrow();
        sh.input_ended && sh.input.is_empty()
    }

    /// Push one response item (SS/DX stream items; the single response
    /// body on CS/unary). [`SinkError::Closed`] is the
    /// `RpcSinkClosed`-class refusal: retired, finished, or the single
    /// response is already filled. On a response-windowed call this is
    /// core's send-side permit wait in pull form: one credit per item,
    /// and [`SinkError::WouldBlock`] takes NOTHING — retry after a
    /// `STREAM_GRANT` arrives (the JS surface keeps its promise
    /// pending and re-polls), so a `send()` resolves only as grants
    /// arrive even with an idle reader.
    pub fn send(&self, item: &[u8]) -> Result<(), SinkError> {
        let mut sh = self.inner.borrow_mut();
        if sh.closed || sh.producer_done {
            return Err(SinkError::Closed);
        }
        if sh.single_response && !sh.output.is_empty() {
            // The single response is already filled (§2.2): a second
            // send can NEVER succeed, so it is refused TYPED here — a
            // queued extra would only be silently discarded by the
            // pump, a drop followed by an earlier `Ok` (§2.7).
            return Err(SinkError::Closed);
        }
        if sh.resp_credit == Some(0) {
            return Err(SinkError::WouldBlock);
        }
        if sh.output.len() >= MAX_QUEUED_ITEMS {
            return Err(SinkError::ResourceExhausted);
        }
        if let Some(credit) = sh.resp_credit.as_mut() {
            *credit -= 1;
        }
        sh.output.push_back(Bytes::copy_from_slice(item));
        Ok(())
    }

    /// Hand back the handler's result and close the producer gate.
    /// `Draining` follows: already-queued items still drain in order
    /// (§2.6), and the result is the terminal — an `Err` handler never
    /// reports `Ok`. A result delivered after retirement is DISCARDED.
    pub fn finish(&self, result: StreamHandlerResult) {
        let mut sh = self.inner.borrow_mut();
        if sh.closed || sh.producer_done {
            return;
        }
        sh.producer_done = true;
        sh.input_ended = true;
        sh.input.clear();
        sh.result = Some(result);
    }

    /// The retired signal: `Some(reason)` once the call retired. A
    /// detached holder can observe this (F-S3.1-2).
    pub fn retired(&self) -> Option<RetireReason> {
        self.inner.borrow().retired
    }

    /// Whether the call's one terminal has committed (completion OR
    /// retirement). Unlike [`Self::retired`], this is also `true` after a
    /// normal completion — the signal a relay holding this call needs to
    /// know it can release it.
    pub fn settled(&self) -> bool {
        self.inner.borrow().settled
    }
}

/// The registry-side record (§2.6).
struct ServeCallState {
    key: (NodeId, u64, u64),
    service: String,
    shape: RpcCallShape,
    /// This leaf's origin hash (the frame stamp).
    origin_hash: u64,
    /// The reply route: `<service>.replies.<authenticated caller
    /// origin>` — derived from the AEAD-authenticated entity, never
    /// from the wire-claimed origin.
    reply_route: u64,
    /// Absolute end + which bound produced it (§2.1).
    end_ns: u64,
    bound: DeadlineBound,
    /// Upload window the caller opted into (`None` = unbounded).
    request_window_initial: Option<u32>,
    /// Request grants already emitted — one per consumed chunk for the
    /// life of the call (core's unbounded per-consumption pacing; see
    /// [`emit_request_grants`]).
    request_granted: u64,
    /// §2.6's input half.
    input: InputHalf,
    /// FIRST-WRITER-WINS terminal latch (§2.6).
    terminal: Option<StreamTerminalReason>,
    shared: Rc<RefCell<ServeShared>>,
}

/// The input half (§2.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputHalf {
    /// Accepting request chunks.
    Open,
    /// The caller sent END. Remaining output is unaffected.
    Ended,
    /// The consumer is gone (handler returned/retired): further chunks
    /// are refused and discarded.
    Closed,
}

/// Which bound produced the effective end (§2.1) — it selects the
/// terminal reason, so it is not a diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeadlineBound {
    /// The caller's deadline, or the provider default for an omitted
    /// one. Expiry is `Timeout`.
    Deadline,
    /// Credential validity cut the call short. Expiry is
    /// `CredentialExpired` — an authority lapse, not a timeout.
    Credential,
}

impl DeadlineBound {
    /// The terminal this bound produces when it fires (§2.1) — the two
    /// are never conflated.
    pub const fn expiry_reason(self) -> StreamTerminalReason {
        match self {
            Self::Deadline => StreamTerminalReason::Timeout,
            Self::Credential => StreamTerminalReason::CredentialExpired,
        }
    }
}

/// Why an opening's effective deadline is unusable (§2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeadlineRefusal {
    /// An explicit deadline beyond `max_live`. Refused, never clamped.
    ExceedsPolicy,
    /// The effective end is already in the past.
    AlreadyElapsed,
    /// Checked arithmetic overflowed (a pre-epoch or absurd clock).
    Overflow,
}

/// §2.1's effective deadline — three distinct bounds, never one `min`:
/// an omitted deadline reaches the provider default ONLY through the
/// `None` arm (it can never cap an explicit request); an explicit
/// request over `now + max_live_ns` is REFUSED, never clamped; and
/// credential validity CLAMPS and records that it clamped — on an
/// exact tie credential expiry wins (at that instant the authority is
/// gone, and a plain timeout would understate it).
pub fn resolve_stream_deadline(
    now_ns: u64,
    requested: Option<u64>,
    credential_ends_ns: &[Option<u64>],
    default_live_ns: u64,
    max_live_ns: u64,
) -> Result<(u64, DeadlineBound), DeadlineRefusal> {
    let (requested_end, mut bound) = match requested {
        Some(end) => {
            let cap = now_ns
                .checked_add(max_live_ns)
                .ok_or(DeadlineRefusal::Overflow)?;
            if end > cap {
                return Err(DeadlineRefusal::ExceedsPolicy);
            }
            (end, DeadlineBound::Deadline)
        }
        None => (
            now_ns
                .checked_add(default_live_ns)
                .ok_or(DeadlineRefusal::Overflow)?,
            DeadlineBound::Deadline,
        ),
    };
    let mut end_ns = requested_end;
    for candidate in credential_ends_ns.iter().flatten() {
        if *candidate <= end_ns {
            end_ns = *candidate;
            bound = DeadlineBound::Credential;
        }
    }
    if end_ns <= now_ns {
        return Err(DeadlineRefusal::AlreadyElapsed);
    }
    Ok((end_ns, bound))
}

/// Why a service could not be registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServeError {
    /// The service name does not derive a valid request channel.
    InvalidService(String),
    /// The service is already served here.
    AlreadyServed,
}

/// The provider-side registry: registrations, live protected calls,
/// opening reservations (§3), and the outbound frame queue the node
/// pumps.
pub struct ServeRegistry {
    origin_hash: u64,
    services: HashMap<String, ServeOptions>,
    handlers: HashMap<String, ServeHandler>,
    /// §3's opening reservations: keys held BEFORE any signature work
    /// and released on every refusal (the replay record, if one was
    /// inserted, stays consumed — Owner Q4).
    openings: HashSet<(NodeId, u64, u64)>,
    calls: HashMap<(NodeId, u64, u64), ServeCallState>,
    /// §6's failed-admission throttle (core's
    /// `MeshNode::admission_rate_limiter`): consulted BEFORE the
    /// signature work and charged on every denial except
    /// `AuthorityChanged` (D7).
    failure_limiter: AdmissionFailureLimiter,
    /// Refusals answered with NO wire frame because their key was live
    /// (see [`Self::key_is_live`]).
    live_key_refusals: u64,
    out: VecDeque<ServeOutFrame>,
}

impl core::fmt::Debug for ServeRegistry {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ServeRegistry")
            .field("services", &self.services.len())
            .field("live_calls", &self.calls.len())
            .finish()
    }
}

type CallKey = (NodeId, u64, u64);

impl ServeRegistry {
    /// A registry stamping `origin_hash` on every frame it emits.
    pub fn new(origin_hash: u64) -> Self {
        Self {
            origin_hash,
            services: HashMap::new(),
            handlers: HashMap::new(),
            openings: HashSet::new(),
            calls: HashMap::new(),
            failure_limiter: AdmissionFailureLimiter::with_defaults(),
            live_key_refusals: 0,
            out: VecDeque::new(),
        }
    }

    /// How many refusals were answered with no wire frame because a
    /// call (or an in-flight opening) already owned their key.
    pub fn live_key_refusals(&self) -> u64 {
        self.live_key_refusals
    }

    /// Register one service. The node pairs this with reserving the
    /// `<service>.requests` carrier (exactly one plane owner per
    /// carrier, the `ensure_reply_subscription` reservation
    /// discipline).
    pub fn serve(
        &mut self,
        service: &str,
        opts: ServeOptions,
        handler: ServeHandler,
    ) -> Result<(), ServeError> {
        crate::channel::request_channel(service)
            .map_err(|e| ServeError::InvalidService(format!("{e}")))?;
        if self.services.contains_key(service) {
            return Err(ServeError::AlreadyServed);
        }
        self.services.insert(service.to_string(), opts);
        self.handlers.insert(service.to_string(), handler);
        Ok(())
    }

    /// Stop serving a service: its live calls retire
    /// [`StreamTerminalReason::ServeHandleDropped`] (their callers see
    /// `Cancelled`), exactly one terminal each.
    pub fn unserve(&mut self, service: &str) -> usize {
        self.services.remove(service);
        self.handlers.remove(service);
        let keys: Vec<CallKey> = self
            .calls
            .iter()
            .filter(|(_, c)| c.service == service)
            .map(|(k, _)| *k)
            .collect();
        self.retire_keys(&keys, StreamTerminalReason::ServeHandleDropped)
    }

    /// The services currently served.
    pub fn services(&self) -> impl Iterator<Item = &str> {
        self.services.keys().map(String::as_str)
    }

    /// How many protected calls are live.
    pub fn live_calls(&self) -> usize {
        self.calls.len()
    }

    /// An inbound opening `REQUEST` on a served carrier. `call_id`
    /// comes from `EventMeta::seq_or_ts` (a REQUEST carries no
    /// in-payload call id). Reserves the active key BEFORE any
    /// signature work (§3), verifies, and releases the reservation on
    /// any refusal while the replay record — if one was inserted —
    /// stays consumed (Owner Q4).
    #[allow(clippy::too_many_arguments)]
    pub fn on_request(
        &mut self,
        from: &ServePeer,
        served_service: &str,
        call_id: u64,
        payload: RpcRequestPayload,
        now_unix_ns: u64,
        admission: &ServeAdmission<'_>,
    ) -> OpenOutcome {
        let Some(opts) = self.services.get(served_service).cloned() else {
            return self.refuse_malformed(from, served_service, call_id, "no registration");
        };
        if payload.service != served_service {
            return self.refuse_malformed(
                from,
                served_service,
                call_id,
                "the request names a different service than its carrier serves",
            );
        }
        let flags = payload.flags;
        let client_streaming = flags & FLAG_RPC_CLIENT_STREAMING_REQUEST != 0;
        let streaming_response = flags & FLAG_RPC_STREAMING_RESPONSE != 0;
        // One supplied clock for the whole admission: the paired
        // sample's monotonic millis (see `verify_org_admission`).
        let now_mono_ms = now_unix_ns / 1_000_000;
        if !request_flags_ok(opts.shape, flags) {
            // The flags claim a shape this registration does not serve.
            // The typed reason is the CORE mapping, exactly (§1.5 step
            // 4 + the folds' contract-4 flag checks): only a UNARY
            // registration's streaming flags read the preserved
            // `StreamingUnsupported`, while a streaming registration's
            // wrong flags are `ShapeMismatch` (coarse `Denied`) — the
            // two are never conflated.
            let reason = if opts.shape == RpcCallShape::Unary {
                AdmissionDenied::StreamingUnsupported
            } else {
                AdmissionDenied::ShapeMismatch
            };
            return self.refuse_denied(from, served_service, call_id, now_mono_ms, reason);
        }
        // Parse the opt-in windows. Malformed values parse as `None`
        // (no flow control), exactly as the core's parsers behave.
        let output_window = parse_window(&payload.headers, HEADER_NRPC_STREAM_WINDOW_INITIAL);
        let request_window = parse_window(&payload.headers, HEADER_NRPC_REQUEST_WINDOW_INITIAL);
        if output_window == Some(0) || request_window == Some(0) {
            // `Some(0)` means the pump would await a credit that can
            // never arrive; refused at admission.
            return self.refuse_denied(
                from,
                served_service,
                call_id,
                now_mono_ms,
                AdmissionDenied::ProviderPolicyRejected,
            );
        }

        // §3 step 1 — RESERVE the active key before any signature
        // work. A live key (or one already opening) is refused
        // `ActiveCallOwned` with NO wire frame (`key_is_live`), so the
        // live call keeps its own single terminal.
        let key = (from.peer, from.incarnation, call_id);
        if self.calls.contains_key(&key) || self.openings.contains(&key) {
            return self.refuse_denied(
                from,
                served_service,
                call_id,
                now_mono_ms,
                AdmissionDenied::ActiveCallOwned,
            );
        }
        let Ok(reply_name) =
            crate::channel::reply_channel(served_service, from.caller.origin_hash())
        else {
            return self.refuse_malformed(from, served_service, call_id, "bad reply channel");
        };
        let reply_route = crate::channel::Channel::from_name(reply_name).canonical();
        self.openings.insert(key);

        // §3 step 2 — VERIFY (credentials → binding → stability →
        // replay insert → provider policy).
        let proof_headers: Vec<&[u8]> = payload
            .headers
            .iter()
            .filter(|(name, _)| name == ORG_ADMISSION_HEADER)
            .map(|(_, value)| value.as_slice())
            .collect();
        let request_digest = match org_request_digest(&payload) {
            Ok(digest) => digest,
            Err(_) => {
                self.openings.remove(&key);
                return self.refuse_denied(
                    from,
                    served_service,
                    call_id,
                    now_mono_ms,
                    AdmissionDenied::BindingInvalid,
                );
            }
        };
        let ctx = AdmissionContext::new(
            opts.access.org_mode(),
            &from.caller,
            admission.provider,
            opts.provider_owner_org,
            CapabilityAuthorityId::for_tag(&format!("nrpc:{served_service}")),
            call_id,
            request_digest,
            opts.shape,
            client_streaming,
            streaming_response,
            from.session_binding,
            admission.facts,
            opts.skew_secs,
        );
        let epoch_at_reserve = admission.facts.epoch;
        let poisoned_at_reserve = admission.facts.poisoned;
        let policy = opts.policy.clone();
        // §6 — throttle BEFORE the signature work, not after (the core
        // bridge's `admit_protected_opening` gate): a peer that has
        // spent its failure budget is refused WITHOUT reaching
        // `verify_org_admission` and its `verify_strict` work.
        if !self.failure_limiter.may_attempt(from.peer, now_mono_ms) {
            self.openings.remove(&key);
            return self.refuse_throttled(from, served_service, call_id);
        }
        let admitted = match verify_org_admission(
            &ctx,
            &proof_headers,
            admission.replay,
            now_unix_ns,
            now_mono_ms,
            // Stability linearization (E1.4 §9.5): the captured
            // `(epoch, poisoned)` must not have moved.
            || {
                admission.facts.epoch == epoch_at_reserve
                    && !poisoned_at_reserve
                    && !admission.facts.poisoned
            },
            |proof| policy.as_ref().is_none_or(|p| p(proof)),
        ) {
            Ok(admitted) => admitted,
            Err(reason) => {
                // §3 step 3 — release the active reservation; the
                // replay record (if inserted) stays consumed.
                self.openings.remove(&key);
                return self.refuse_denied(from, served_service, call_id, now_mono_ms, reason);
            }
        };

        // The proof's member facts (generation + credential validity
        // ends) come from the decoded proof; verification established
        // them, this extracts what the lifecycle layer needs.
        let Some(facts) = extract_proof_facts(opts.shape, proof_headers.first().copied()) else {
            self.openings.remove(&key);
            return self.refuse_denied(
                from,
                served_service,
                call_id,
                now_mono_ms,
                AdmissionDenied::MalformedProof,
            );
        };

        // §2.1 — the effective deadline: three distinct bounds.
        let requested = if payload.deadline_ns == 0 {
            None
        } else {
            Some(payload.deadline_ns)
        };
        let (end_ns, bound) = match resolve_stream_deadline(
            now_unix_ns,
            requested,
            &facts.credential_ends_ns,
            opts.default_live_ns,
            opts.max_live_ns,
        ) {
            Ok(resolved) => resolved,
            Err(_) => {
                self.openings.remove(&key);
                return self.refuse_denied(
                    from,
                    served_service,
                    call_id,
                    now_mono_ms,
                    AdmissionDenied::DeadlineExceedsPolicy,
                );
            }
        };

        // Admitted: build the per-shape call state and invoke the
        // handler hook.
        let (input, preloaded): (InputHalf, Vec<Bytes>) = match opts.shape {
            // Unary and server-streaming: the REQUEST body IS the one
            // request item; the upload half ends with the shape.
            RpcCallShape::Unary | RpcCallShape::ServerStreaming => {
                (InputHalf::Ended, vec![payload.body])
            }
            // CS/DX: the initial body is the first chunk unless the
            // degenerate zero-item opening (empty + FLAG_END).
            RpcCallShape::ClientStreaming | RpcCallShape::Duplex => {
                let end = flags & FLAG_RPC_REQUEST_END != 0;
                let mut items = Vec::new();
                if !payload.body.is_empty() || !end {
                    items.push(payload.body);
                }
                (
                    if end {
                        InputHalf::Ended
                    } else {
                        InputHalf::Open
                    },
                    items,
                )
            }
        };
        let shared = Rc::new(RefCell::new(ServeShared {
            caller: admitted.clone(),
            generation: facts.generation,
            acting_org: facts.acting_org,
            member: facts.member,
            input: preloaded.into_iter().collect(),
            input_ended: matches!(input, InputHalf::Ended | InputHalf::Closed),
            consumed: 0,
            output: VecDeque::new(),
            resp_credit: output_window,
            result: None,
            single_response: matches!(
                opts.shape,
                RpcCallShape::ClientStreaming | RpcCallShape::Unary
            ),
            producer_done: false,
            closed: false,
            retired: None,
            settled: false,
        }));
        self.openings.remove(&key);
        self.calls.insert(
            key,
            ServeCallState {
                key,
                service: served_service.to_string(),
                shape: opts.shape,
                origin_hash: self.origin_hash,
                reply_route,
                end_ns,
                bound,
                request_window_initial: request_window,
                request_granted: 0,
                input,
                terminal: None,
                shared: Rc::clone(&shared),
            },
        );
        if let Some(handler) = self.handlers.get(served_service) {
            handler(ServeCall { inner: shared });
        }
        OpenOutcome::Admitted
    }

    /// An inbound `REQUEST_CHUNK`. Matched on `(peer, incarnation,
    /// call_id)`; a wrong peer/session delivers nothing and cancels
    /// nothing, and a pre-admission chunk is never delivered. EOF is
    /// exactly `FLAG_RPC_REQUEST_END`; a late chunk after END (or
    /// after the handler returned) delivers nothing and cancels
    /// nothing.
    pub fn on_chunk(&mut self, from: &ServePeer, chunk: RpcRequestChunkPayload) -> bool {
        let key = (from.peer, from.incarnation, chunk.call_id);
        let end = chunk.flags & FLAG_RPC_REQUEST_END != 0;
        let mut overflowed = false;
        let delivered = {
            let Some(state) = self.calls.get_mut(&key) else {
                return false;
            };
            if state.terminal.is_some() || !matches!(state.input, InputHalf::Open) {
                // Late upload: delivers nothing, changes nothing.
                false
            } else if !chunk.body.is_empty() || !end {
                let mut sh = state.shared.borrow_mut();
                if sh.input.len() >= MAX_QUEUED_ITEMS {
                    overflowed = true;
                    false
                } else {
                    sh.input.push_back(chunk.body.clone());
                    true
                }
            } else {
                false
            }
        };
        if overflowed {
            // An admitted item that cannot be delivered retires the
            // call — never a silent drop followed by success (§2.7).
            self.retire_keys(&[key], StreamTerminalReason::ResourceExhausted);
            return false;
        }
        if delivered || end {
            if let Some(state) = self.calls.get_mut(&key) {
                if end && matches!(state.input, InputHalf::Open) {
                    // END closes the input half exactly once, and
                    // never touches the output half.
                    state.input = InputHalf::Ended;
                    state.shared.borrow_mut().input_ended = true;
                }
            }
        }
        delivered
    }

    /// An inbound `STREAM_GRANT` (response-direction credit). Ignored
    /// on unknown keys (a wrong-session grant releases nothing), on
    /// non-flow-controlled calls (no window header), and once terminal.
    pub fn on_stream_grant(&mut self, from: &ServePeer, call_id: u64, credits: u32) -> bool {
        let key = (from.peer, from.incarnation, call_id);
        let credited = {
            let Some(state) = self.calls.get_mut(&key) else {
                return false;
            };
            if state.terminal.is_some() {
                return false;
            }
            let mut sh = state.shared.borrow_mut();
            if sh.resp_credit.is_none() {
                // Non-flow-controlled: every GRANT is ignored.
                return false;
            }
            if credits == 0 {
                return false;
            }
            if let Some(credit) = sh.resp_credit.as_mut() {
                *credit = credit.saturating_add(credits);
            }
            true
        };
        // Credit released: parked senders may push again, and the pump
        // runs.
        self.pump_key(key);
        credited
    }

    /// An inbound `CANCEL`. Matched on the key; a wrong peer/session
    /// cancels nothing. Retirement is first-writer-wins, so a CANCEL
    /// after any other terminal is a no-op.
    pub fn on_cancel(&mut self, from: &ServePeer, call_id: u64) -> bool {
        let key = (from.peer, from.incarnation, call_id);
        self.retire_keys(&[key], StreamTerminalReason::Cancelled) > 0
    }

    /// The pull-driven sweep: emit pending `REQUEST_GRANT`s, drain
    /// response output (credit-gated, in order), commit completions,
    /// and retire overdue calls.
    ///
    /// **Suspension contract (§4.5):** deadlines are absolute — a
    /// frozen tab's ticker stopping extends nothing, the first sweep
    /// after wake retires everything overdue with its deadline
    /// terminal, and nothing is transparently re-opened.
    pub fn advance(&mut self, now_unix_ns: u64) {
        let keys: Vec<CallKey> = self.calls.keys().copied().collect();
        let mut overdue = Vec::new();
        for key in keys {
            let Some(state) = self.calls.get_mut(&key) else {
                continue;
            };
            if state.terminal.is_none() && now_unix_ns >= state.end_ns {
                overdue.push((key, state.bound.expiry_reason()));
                continue;
            }
            self.pump_key(key);
        }
        for (key, reason) in overdue {
            self.retire_keys(&[key], reason);
        }
    }

    /// Raise-only floor movement: retire every live call whose
    /// membership generation is now below its floor
    /// ([`StreamTerminalReason::Revoked`] — the frozen `Revoked →
    /// Denied` + `&[0]` byte). Returns the retired call ids so a
    /// caller can observe the two-branch property (a compliant sibling
    /// is untouched).
    pub fn raise_floors(&mut self, facts: &RevocationFacts) -> Vec<u64> {
        let victims: Vec<CallKey> = self
            .calls
            .iter()
            .filter(|(_, state)| {
                let sh = state.shared.borrow();
                sh.generation < facts.floor_for(&sh.acting_org, &sh.member)
            })
            .map(|(k, _)| *k)
            .collect();
        let before = victims.clone();
        self.retire_keys(&victims, StreamTerminalReason::Revoked);
        before.into_iter().map(|key| key.2).collect()
    }

    /// The pinned session was replaced: its calls retire typed (the
    /// default reason maps to [`StreamTerminalReason::SessionReplaced`]).
    pub fn fail_incarnation(&mut self, incarnation: u64, reason: RetireReason) -> usize {
        let keys: Vec<CallKey> = self
            .calls
            .keys()
            .copied()
            .filter(|key| key.1 == incarnation)
            .collect();
        self.retire_keys(&keys, reason.wire_reason())
    }

    /// The peer went away entirely.
    pub fn fail_peer(&mut self, peer: NodeId, reason: RetireReason) -> usize {
        let keys: Vec<CallKey> = self
            .calls
            .keys()
            .copied()
            .filter(|key| key.0 == peer)
            .collect();
        self.retire_keys(&keys, reason.wire_reason())
    }

    /// Node close: every call retires typed.
    pub fn fail_all(&mut self, reason: RetireReason) -> usize {
        let keys: Vec<CallKey> = self.calls.keys().copied().collect();
        self.retire_keys(&keys, reason.wire_reason())
    }

    /// Drain the frames to hand to the node's event plane.
    pub fn take_outbound(&mut self) -> Vec<ServeOutFrame> {
        self.out.drain(..).collect()
    }

    /// Retire each key with `reason`, first writer wins; committed
    /// terminals release their registry entry (the handler's
    /// [`ServeCall`] keeps the shared state and its observables).
    fn retire_keys(&mut self, keys: &[CallKey], reason: StreamTerminalReason) -> usize {
        let mut n = 0;
        for key in keys {
            let retired = {
                let Some(state) = self.calls.get_mut(key) else {
                    continue;
                };
                retire_into(&mut self.out, state, reason.clone())
            };
            if retired {
                n += 1;
            }
            // Whether retired now or earlier, a committed terminal
            // means the entry is no longer live.
            if self.calls.get(key).is_some_and(|s| s.terminal.is_some()) {
                if let Some(state) = self.calls.remove(key) {
                    state.shared.borrow_mut().settled = true;
                }
            }
        }
        n
    }

    /// Run one call's emission pump (grants, items, completion).
    fn pump_key(&mut self, key: CallKey) {
        let committed = {
            let Some(state) = self.calls.get_mut(&key) else {
                return;
            };
            pump(&mut self.out, state);
            state.terminal.is_some()
        };
        if committed {
            if let Some(state) = self.calls.remove(&key) {
                state.shared.borrow_mut().settled = true;
            }
        }
    }

    /// A refusal frame plus its typed outcome: `AdmissionDenied` + the
    /// one-byte coarse reason (the `emit_admission_denial` shape). §6:
    /// every denial charges the peer's failed-admission budget EXCEPT
    /// `AuthorityChanged` (D7) — the core bridge's exact disposition.
    fn refuse_denied(
        &mut self,
        from: &ServePeer,
        service: &str,
        call_id: u64,
        now_mono_ms: u64,
        reason: AdmissionDenied,
    ) -> OpenOutcome {
        if reason != AdmissionDenied::AuthorityChanged {
            self.failure_limiter.on_failure(from.peer, now_mono_ms);
        }
        let payload = denial_payload(reason.coarse());
        self.emit_refusal(from, service, call_id, &payload);
        OpenOutcome::Denied(reason)
    }

    /// Whether a call (or an in-flight opening) already owns this key.
    /// A refusal under a live key sends NO wire frame: exactly one
    /// terminal per call_id on the wire, and the live call's own single
    /// terminal must be the only terminal-shaped frame its id ever
    /// carries (the caller's latch is first-writer-wins, and LATCHING
    /// DISARMS the drop-CANCEL guard). ANY refusal reaching this under a
    /// live key — the `ActiveCallOwned` duplicate, or a crafted REQUEST
    /// reusing the id with wrong flags / a wrong service / a zero window
    /// — would otherwise be consumed as that call's terminal. Nothing is
    /// owed to the sender: the live call already guarantees the caller
    /// its one terminal. (The first repair sent the refusal on
    /// `call_id ^ 2^63` instead; a leaf caller seeds its stream table at
    /// `seed ^ 2^63`, so that id can be the same caller's OTHER live call
    /// — §23 audit.) A refusal for a FRESH key keeps its `call_id`: that
    /// frame IS the caller's latch for the refused opening.
    fn key_is_live(&self, from: &ServePeer, call_id: u64) -> bool {
        let key = (from.peer, from.incarnation, call_id);
        self.calls.contains_key(&key) || self.openings.contains(&key)
    }

    /// Emit a refusal frame for `call_id` unless its key is live (then
    /// count it and send nothing — see [`Self::key_is_live`]).
    fn emit_refusal(
        &mut self,
        from: &ServePeer,
        service: &str,
        call_id: u64,
        payload: &RpcResponsePayload,
    ) {
        if self.key_is_live(from, call_id) {
            self.live_key_refusals += 1;
            return;
        }
        self.emit_terminal(from, service, call_id, payload);
    }

    /// The §6 throttle refusal (core's `OpeningRefusal::Throttled`):
    /// the coarse `Unavailable` byte on the wire, WITHOUT a second
    /// `on_failure` charge (the limiter counted this denial itself).
    fn refuse_throttled(&mut self, from: &ServePeer, service: &str, call_id: u64) -> OpenOutcome {
        let payload = denial_payload(CoarseAdmissionReason::Unavailable);
        self.emit_refusal(from, service, call_id, &payload);
        OpenOutcome::Throttled
    }

    /// A structural refusal: the core's malformed-request shape
    /// (`UnknownVersion` + `end` + diagnostic).
    fn refuse_malformed(
        &mut self,
        from: &ServePeer,
        service: &str,
        call_id: u64,
        what: &str,
    ) -> OpenOutcome {
        let payload = RpcResponsePayload {
            status: RpcStatus::UnknownVersion,
            headers: vec![(
                HEADER_NRPC_STREAMING.to_string(),
                HEADER_NRPC_STREAMING_END.to_vec(),
            )],
            body: Bytes::from(format!("malformed request: {what}")),
        };
        self.emit_refusal(from, service, call_id, &payload);
        OpenOutcome::Malformed(what.to_string())
    }

    /// Emit one terminal-shaped frame on the call's reply route (the
    /// route derives from the AUTHENTICATED caller entity).
    fn emit_terminal(
        &mut self,
        from: &ServePeer,
        service: &str,
        call_id: u64,
        payload: &RpcResponsePayload,
    ) {
        let Ok(name) = crate::channel::reply_channel(service, from.caller.origin_hash()) else {
            return;
        };
        let route = crate::channel::Channel::from_name(name).canonical();
        if let Ok(frame) = encode_response_frame(self.origin_hash, call_id, route, payload) {
            self.out.push_back(ServeOutFrame {
                peer: from.peer,
                route,
                frame,
            });
        }
    }
}

/// §2.2's retirement order, synchronously and in order:
///
/// 1. latch the terminal reason FIRST (first writer wins; a second
///    retirement is a no-op),
/// 2. stop emission (the output half is over),
/// 3. close and discard queued input,
/// 4. drop the handler continuation — its later sends get the typed
///    [`SinkError::Closed`] refusal and its result is DISCARDED,
/// 5. discard the remaining response queue (retirement never drains —
///    only `Completed(_)` does, the F-S1 closure property),
/// 6. emit exactly ONE terminal through
///    [`rpc_wire::stream_terminal_payload`].
///
/// F-S3.1-2, the handler-drop level, exactly: "the retire supervisor
/// may drop the handler future without a final poll — cancellation is
/// observed through the retirement observables (the terminal item, the
/// sink's typed closed refusal, and the retired signal), never assumed
/// as a handler-side event". A detached observer holding the retired
/// signal CAN observe.
fn retire_into(
    out: &mut VecDeque<ServeOutFrame>,
    state: &mut ServeCallState,
    reason: StreamTerminalReason,
) -> bool {
    if state.terminal.is_some() {
        return false;
    }
    state.terminal = Some(reason.clone());
    state.input = InputHalf::Closed;
    {
        let mut sh = state.shared.borrow_mut();
        sh.input.clear();
        sh.input_ended = true;
        sh.output.clear();
        sh.closed = true;
        sh.retired = Some(retire_reason_of(&reason));
    }
    emit(out, state, &stream_terminal_payload(&reason));
    true
}

/// The emission pump: request grants out, response items out (in
/// order, credit-gated), then — once the producer is finished and the
/// queue is drained — the one terminal. Only `Completed(_)` drains
/// queued output before its terminal (the F-S1 closure property).
fn pump(out: &mut VecDeque<ServeOutFrame>, state: &mut ServeCallState) {
    if state.terminal.is_some() {
        return;
    }
    emit_request_grants(out, state);
    let single_response = matches!(
        state.shape,
        RpcCallShape::ClientStreaming | RpcCallShape::Unary
    );
    loop {
        let (has_item, done, result) = {
            let sh = state.shared.borrow();
            (!sh.output.is_empty(), sh.producer_done, sh.result.clone())
        };
        if single_response {
            // The single-response emitter (§2.2): the handler's own
            // response IS the terminal, verbatim — its error is the
            // terminal, never `Ok`. No pump event and no input END is
            // awaited.
            if !done {
                return;
            }
            let body = {
                let mut sh = state.shared.borrow_mut();
                // `ServeCall::send` refuses a second single-response
                // item typed, so at most one is ever queued.
                sh.output.pop_front().unwrap_or_default()
            };
            let outcome = result.unwrap_or(StreamHandlerResult::Ok);
            let payload = match outcome.clone() {
                StreamHandlerResult::Ok => RpcResponsePayload {
                    status: RpcStatus::Ok,
                    headers: Vec::new(),
                    body,
                },
                err @ StreamHandlerResult::Err(_, _) => {
                    stream_terminal_payload(&StreamTerminalReason::Completed(err))
                }
            };
            state.terminal = Some(StreamTerminalReason::Completed(outcome));
            emit(out, state, &payload);
            return;
        }
        // SS/DX: queued items drain as `continue` chunks in order —
        // each was PAID its credit at `ServeCall::send`, so the pump
        // parks only on the queue, never on the window. The terminal
        // follows the drain.
        if has_item {
            let body = state
                .shared
                .borrow_mut()
                .output
                .pop_front()
                .expect("checked");
            let payload = RpcResponsePayload {
                status: RpcStatus::Ok,
                headers: vec![(
                    HEADER_NRPC_STREAMING.to_string(),
                    HEADER_NRPC_STREAMING_CONTINUE.to_vec(),
                )],
                body,
            };
            emit(out, state, &payload);
            continue;
        }
        if !done {
            return;
        }
        // Queue drained and producer finished: commit
        // `Completed(result)` and emit the one terminal (Ok → `end` +
        // empty body; Err → the handler's status verbatim + message).
        let outcome = result.unwrap_or(StreamHandlerResult::Ok);
        let payload = stream_terminal_payload(&StreamTerminalReason::Completed(outcome.clone()));
        state.terminal = Some(StreamTerminalReason::Completed(outcome));
        emit(out, state, &payload);
        return;
    }
}

/// One-per-consumed-chunk `REQUEST_GRANT` pacing for upload-windowed
/// calls (core's `RequestStream::poll_next` auto-grant): every consumed
/// chunk earns exactly one grant, for the LIFE of the call —
/// unbounded, exactly like core, whose
/// [`REQUEST_GRANT_PER_CALL_CAP`] bounds the CALLER's credit BALANCE
/// (`StreamCallRegistry::on_grant`), never this lifetime count. A
/// lifetime cap here would stall every sustained upload in
/// `SinkError::WouldBlock` to its deadline once the cap is spent.
fn emit_request_grants(out: &mut VecDeque<ServeOutFrame>, state: &mut ServeCallState) {
    if state.request_window_initial.is_none() {
        // Not upload-windowed: consumption grants nothing.
        return;
    }
    let consumed = state.shared.borrow().consumed;
    while state.request_granted < consumed {
        state.request_granted += 1;
        let frame =
            encode_request_grant_frame(state.origin_hash, state.key.2, state.reply_route, 1);
        out.push_back(ServeOutFrame {
            peer: state.key.0,
            route: state.reply_route,
            frame,
        });
    }
}

/// The one-byte coarse admission-denial payload (the
/// `emit_admission_denial` shape: `AdmissionDenied` + the coarse byte;
/// "denial is not a credential oracle").
fn denial_payload(coarse: CoarseAdmissionReason) -> RpcResponsePayload {
    RpcResponsePayload {
        status: RpcStatus::AdmissionDenied,
        headers: Vec::new(),
        body: Bytes::copy_from_slice(&[coarse.to_wire()]),
    }
}

/// Queue one provider→caller frame on the call's reply route.
fn emit(out: &mut VecDeque<ServeOutFrame>, state: &ServeCallState, payload: &RpcResponsePayload) {
    if let Ok(frame) =
        encode_response_frame(state.origin_hash, state.key.2, state.reply_route, payload)
    {
        out.push_back(ServeOutFrame {
            peer: state.key.0,
            route: state.reply_route,
            frame,
        });
    }
}

/// The terminal-reason → retire-signal mapping (the frozen string
/// vocabulary of [`RetireReason::as_str`]).
fn retire_reason_of(reason: &StreamTerminalReason) -> RetireReason {
    match reason {
        StreamTerminalReason::Completed(_) => RetireReason::Cancelled,
        StreamTerminalReason::Cancelled => RetireReason::Cancelled,
        StreamTerminalReason::ServeHandleDropped => RetireReason::NodeClosed,
        StreamTerminalReason::Timeout => RetireReason::Timeout,
        StreamTerminalReason::CredentialExpired
        | StreamTerminalReason::Revoked
        | StreamTerminalReason::AuthorityUnavailable => RetireReason::Revoked,
        StreamTerminalReason::ResourceExhausted => RetireReason::ResourceExhausted,
        StreamTerminalReason::SessionReplaced => RetireReason::Replaced,
        StreamTerminalReason::PumpFailed => RetireReason::NodeClosed,
    }
}

/// The `ss_request_flags_ok` / `cs_request_flags_ok` /
/// `dx_request_flags_ok` mirror plus the unary case: the flags must
/// claim EXACTLY the registered shape.
fn request_flags_ok(shape: RpcCallShape, flags: u16) -> bool {
    let streaming_response = flags & FLAG_RPC_STREAMING_RESPONSE != 0;
    let client_streaming = flags & FLAG_RPC_CLIENT_STREAMING_REQUEST != 0;
    match shape {
        RpcCallShape::Unary => !streaming_response && !client_streaming,
        RpcCallShape::ServerStreaming => streaming_response && !client_streaming,
        RpcCallShape::ClientStreaming => !streaming_response && client_streaming,
        RpcCallShape::Duplex => streaming_response && client_streaming,
    }
}

/// Window-header parsing (case-insensitive name, ASCII-decimal `u32`):
/// missing/malformed ⇒ `None` ⇒ no flow control, exactly as the core's
/// parsers behave.
fn parse_window(headers: &[(String, Vec<u8>)], name: &str) -> Option<u32> {
    headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .and_then(|(_, v)| std::str::from_utf8(v).ok())
        .and_then(|v| v.parse::<u32>().ok())
}

/// The member facts the lifecycle layer needs from the admitted proof:
/// the membership generation (revocation sweeps) and every credential
/// validity end in nanoseconds (the §2.1 clamp).
struct ProofFacts {
    generation: u32,
    acting_org: OrgId,
    member: EntityId,
    credential_ends_ns: Vec<Option<u64>>,
}

fn extract_proof_facts(shape: RpcCallShape, proof: Option<&[u8]>) -> Option<ProofFacts> {
    let proof = proof?;
    let (membership, dispatcher, grant) = match shape {
        RpcCallShape::Unary => {
            let p = OrgCallProof::decode(proof).ok()?;
            (p.caller_membership, p.dispatcher_grant, p.capability_grant)
        }
        RpcCallShape::ServerStreaming | RpcCallShape::ClientStreaming | RpcCallShape::Duplex => {
            let p = OrgStreamCallProof::decode(proof).ok()?;
            (p.caller_membership, p.dispatcher_grant, p.capability_grant)
        }
    };
    let secs = |s: u64| Some(s.saturating_mul(1_000_000_000));
    Some(ProofFacts {
        generation: membership.generation,
        acting_org: membership.org_id,
        member: membership.member.clone(),
        credential_ends_ns: vec![
            secs(membership.not_after),
            secs(dispatcher.not_after),
            grant.as_ref().and_then(|g| secs(g.not_after)),
        ],
    })
}
