//! Stage 0 slice 0.3 of
//! `docs/internal/plans/ORG_SCOPED_STREAMING_PLAN.md` — the executable
//! lifecycle model for one organization-protected streaming call.
//!
//! This module is a **model**, not production wiring: it is `#[cfg(test)]`
//! only and no fold, bridge or binding reaches it. Its job is to make the
//! plan's §2.1 (effective deadline), §2.2 (bounded supervision) and §2.6
//! (independent halves, preserved handler result, terminal ownership)
//! executable and adversarially schedulable **before** any of it is wired
//! into `cortex::rpc`'s folds, where a mistake costs a whole review round.
//!
//! Three pieces, in dependency order:
//!
//! 1. [`LifetimePolicy`] + [`resolve_deadline`] — §2.1. Three distinct
//!    bounds, never one `min`: the default applies ONLY to an omitted
//!    caller deadline, an explicit request over the provider cap is
//!    REFUSED (never clamped), and credential validity CLAMPS with a
//!    different terminal reason.
//! 2. [`CallLifecycle`] — §2.6. Independent input/output halves, the
//!    handler's result preserved through a `Draining` phase, and one
//!    terminal owner. "Producer finished" is deliberately not terminal:
//!    the pump still needs credit, so grants stay admissible while
//!    draining and deadline/cancel/revocation stay armed.
//! 3. [`run_supervisor`] — §2.2. The `select!` that owns the handler, the
//!    pump, the credit semaphores and the single terminal emission, and
//!    completes within a bound even when the pump is parked on credit
//!    the caller never grants.
//!
//! # Why a model rather than production code
//!
//! The production shape this replaces (`cortex/rpc.rs:2779-2836`) spawns
//! the pump, runs the handler, then `await`s the pump — so a pump parked
//! in `acquire_owned` on a zero-credit semaphore (`:2792`) is retired by
//! nothing. Wrapping only the handler future (what the client-streaming
//! and duplex folds do today at `:3410` / `:3863`) cannot fix that. The
//! ownership structure needed to fix it is what this file pins, with an
//! inverse for every rule.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, Notify, Semaphore};

// ---------------------------------------------------------------------
// §2.1 — effective deadline
// ---------------------------------------------------------------------

/// Provider lifetime policy (plan Q1 defaults: 300 s / 3600 s).
///
/// Both values are wall-clock nanosecond durations so they compose with
/// `ClockSample::wall_ns` without a second unit conversion at the call
/// site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LifetimePolicy {
    /// Used ONLY when the caller supplied no deadline.
    pub default_live_ns: u64,
    /// Ceiling on an explicitly requested deadline. Exceeding it refuses.
    pub max_live_ns: u64,
}

impl LifetimePolicy {
    /// The Q1 initial defaults: 300 s default, 3600 s maximum.
    pub const fn q1_defaults() -> Self {
        Self {
            default_live_ns: 300 * 1_000_000_000,
            max_live_ns: 3_600 * 1_000_000_000,
        }
    }

    /// Startup validation (Q1): both positive, and the default can never
    /// trip the cap. A policy that fails this could refuse every call
    /// that omitted a deadline, which is a configuration bug, not a
    /// denial.
    pub fn validate(&self) -> Result<(), PolicyError> {
        if self.default_live_ns == 0 || self.max_live_ns == 0 {
            return Err(PolicyError::NotPositive);
        }
        if self.default_live_ns > self.max_live_ns {
            return Err(PolicyError::DefaultOverMax);
        }
        Ok(())
    }
}

/// Why a [`LifetimePolicy`] is unusable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyError {
    /// A zero duration: no call could ever run.
    NotPositive,
    /// `default_live > max_live`: the default itself would be refused.
    DefaultOverMax,
}

/// Which bound produced the effective end — it selects the terminal
/// reason, so it is not a diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeadlineBound {
    /// The caller's deadline, or the provider default for an omitted one.
    /// Expiry is an ordinary [`TerminalReason::Timeout`].
    Deadline,
    /// Credential validity cut the call short. Expiry is an authority
    /// lapse, not a timeout.
    Credential,
}

/// The single monotonic-translatable end of a protected call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedDeadline {
    /// Absolute wall-clock end, unix nanoseconds.
    pub end_ns: u64,
    /// Which of the three bounds won.
    pub bound: DeadlineBound,
}

impl ResolvedDeadline {
    /// The terminal this deadline produces when it fires.
    pub fn expiry_reason(&self) -> TerminalReason {
        match self.bound {
            DeadlineBound::Deadline => TerminalReason::Timeout,
            DeadlineBound::Credential => TerminalReason::CredentialExpired,
        }
    }
}

/// Why an opening is refused before any handler effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeadlineRefusal {
    /// An explicit caller deadline beyond `max_live`. Refused, never
    /// clamped: the caller asked for something the provider does not
    /// offer and must learn that, not silently get five minutes.
    ExceedsPolicy,
    /// The effective end is already in the past.
    AlreadyElapsed,
    /// Checked arithmetic overflowed (a pre-epoch or absurd clock).
    Overflow,
}

/// Convert whole seconds (what credentials carry) to the nanosecond unit
/// the deadline math uses, without wrapping.
pub fn secs_to_ns(secs: u64) -> Option<u64> {
    secs.checked_mul(1_000_000_000)
}

/// §2.1. `requested` is `None` when the caller omitted a deadline
/// (`deadline_ns == 0` on the wire). `credential_ends_ns` carries every
/// applicable validity end in nanoseconds — caller membership, dispatcher
/// grant, the optional capability grant, AND the provider's own authority
/// validity; `None` entries contribute no bound.
pub fn resolve_deadline(
    now_ns: u64,
    requested: Option<u64>,
    credential_ends_ns: &[Option<u64>],
    policy: &LifetimePolicy,
) -> Result<ResolvedDeadline, DeadlineRefusal> {
    // 1. Requested end. The default is reached ONLY through the `None`
    //    arm, so it can never cap an explicit request.
    let (requested_end, mut bound) = match requested {
        Some(end) => {
            // 2. Provider cap: refuse, never clamp.
            let cap = now_ns
                .checked_add(policy.max_live_ns)
                .ok_or(DeadlineRefusal::Overflow)?;
            if end > cap {
                return Err(DeadlineRefusal::ExceedsPolicy);
            }
            (end, DeadlineBound::Deadline)
        }
        None => (
            now_ns
                .checked_add(policy.default_live_ns)
                .ok_or(DeadlineRefusal::Overflow)?,
            DeadlineBound::Deadline,
        ),
    };

    // 3. Credential bound: clamp, and record that it clamped. On an exact
    //    tie credential expiry wins — at that instant the authority is
    //    gone, and reporting a plain timeout would understate it.
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
    Ok(ResolvedDeadline { end_ns, bound })
}

// ---------------------------------------------------------------------
// §2.6 — the state model
// ---------------------------------------------------------------------

/// Which protected streaming shape a record was admitted under. The
/// shape fixes the initial half states; it is not re-derived from frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallShape {
    /// One request, many responses: input is closed on arrival.
    ServerStreaming,
    /// Many requests, one response.
    ClientStreaming,
    /// Many both ways.
    Duplex,
}

/// The input (caller → provider) half.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    /// Accepting request chunks.
    Open,
    /// The caller sent END. Legitimate remaining output is unaffected.
    Ended,
    /// The *consumer* is gone (the handler returned). Further chunks are
    /// refused and discarded — distinct from `Ended`, which the caller
    /// chose.
    Closed,
}

/// The output (provider → caller) half.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Output {
    /// The handler may still produce.
    Open,
    /// The handler returned and its result is held here while the pump
    /// drains already-queued items. **Not terminal.**
    Draining(HandlerResult),
    /// The pump has stopped; nothing further can be published.
    Ended,
}

/// What the handler returned. Preserved verbatim so an error can never be
/// reported as success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandlerResult {
    /// Clean return.
    Ok,
    /// Typed application failure: `(status, message)`.
    Err(u16, String),
}

/// The single terminal disposition of a call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalReason {
    /// The handler finished and the pump drained. Carries the handler's
    /// own result: an `Err` handler yields `Completed(Err(..))`, never
    /// `Ok`.
    Completed(HandlerResult),
    /// Caller CANCEL, or the caller handle dropped.
    Cancelled,
    /// The effective deadline fired under [`DeadlineBound::Deadline`].
    Timeout,
    /// The effective deadline fired under [`DeadlineBound::Credential`] —
    /// authority lapsed rather than time running out.
    CredentialExpired,
    /// A revocation floor rose past this call's member generation.
    Revoked,
    /// The authority or revocation store moved, was removed, or is
    /// poisoned: fail closed.
    AuthorityUnavailable,
    /// An admitted item could neither be reserved nor delivered. The call
    /// dies; it never completes `Ok` having silently dropped input.
    ResourceExhausted,
    /// The peer's session was replaced or the peer disconnected.
    SessionReplaced,
    /// The protected registration's `ServeHandle` dropped, or the node
    /// shut down.
    ServeHandleDropped,
    /// The pump stopped without the handler returning — a typed failure,
    /// never successful completion.
    PumpFailed,
}

impl TerminalReason {
    /// Whether already-queued response items are published before the
    /// terminal (§2.2's queued-data table). Only a genuine completion
    /// drains; every retirement discards.
    pub fn drains_queued_output(&self) -> bool {
        matches!(self, TerminalReason::Completed(_))
    }
}

/// What the control path actually did with the terminal (§2.8). The four
/// records are distinct because a `try_send` attempt is **not** peer
/// receipt: `Queued` means the control queue took the job, `Sent` means
/// the transport accepted it (a production seam — this model has no
/// transport), and `Refused`/`Unreachable` are interruption, never
/// synthetic success.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalDisposition {
    /// The control queue accepted the terminal. Not peer receipt.
    Queued,
    /// The transport accepted the terminal job. Recorded at the send
    /// seam; still not endpoint receipt, which is attributed at the peer.
    Sent,
    /// The session or route is gone. The peer will observe interruption
    /// or its own deadline.
    Unreachable,
    /// The control queue refused the terminal. Recorded as interruption;
    /// ownership is still released.
    Refused,
}

/// The committed terminal plus its one-shot emission record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Terminal {
    /// The selected outcome. First writer wins.
    pub reason: TerminalReason,
    /// The control-path disposition, recorded exactly once by the
    /// supervisor after the pump stopped.
    pub emission: Option<TerminalDisposition>,
}

/// A frame arriving for an admitted call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frame {
    /// `DISPATCH_RPC_REQUEST_CHUNK`.
    Chunk,
    /// A request chunk carrying `FLAG_RPC_REQUEST_END`.
    End,
    /// `DISPATCH_RPC_CANCEL`.
    Cancel,
    /// `DISPATCH_RPC_STREAM_GRANT` (response-direction credit).
    Grant,
}

/// What the fold must do with a frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Disposition {
    /// Hand the chunk to the handler's request stream.
    Deliver,
    /// Add the credit to the response flow semaphore.
    Credit,
    /// Drop it: no delivery and no credit. NOT "no state change" — the
    /// first `Frame::End` half-closes the input as a side effect and
    /// reports [`Disposition::InputEnded`] instead; this arm is the
    /// frames whose state effect is genuinely nil (the idempotent
    /// second END, a chunk after the input half closed, a frame after
    /// the terminal).
    Ignored,
    /// The first `Frame::End` half-closed the input: `input` became
    /// `Ended`, and that state change IS the disposition — no delivery,
    /// no credit, but the fold can see the input half just closed (the
    /// production mirror's `end_input() == true`,
    /// `cortex/rpc.rs`'s §2.6 record).
    InputEnded,
    /// Begin retirement with this reason.
    Retire(TerminalReason),
}

/// One protected call's lifecycle state (§2.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallLifecycle {
    shape: CallShape,
    input: Input,
    output: Output,
    terminal: Option<Terminal>,
    incarnation: u64,
}

impl CallLifecycle {
    /// Open a record. Server-streaming starts with its input already
    /// `Ended` — the single request arrived with the opening, so there is
    /// no upload half to wait for. Getting this wrong is how a
    /// "both halves ended" completion rule deadlocks server-streaming.
    pub fn new(shape: CallShape, incarnation: u64) -> Self {
        let input = match shape {
            CallShape::ServerStreaming => Input::Ended,
            CallShape::ClientStreaming | CallShape::Duplex => Input::Open,
        };
        Self {
            shape,
            input,
            output: Output::Open,
            terminal: None,
            incarnation,
        }
    }

    /// The admitted shape.
    pub fn shape(&self) -> CallShape {
        self.shape
    }

    /// The input half.
    pub fn input(&self) -> Input {
        self.input
    }

    /// The output half.
    pub fn output(&self) -> &Output {
        &self.output
    }

    /// The committed terminal, if any.
    pub fn terminal(&self) -> Option<&Terminal> {
        self.terminal.as_ref()
    }

    /// This record's incarnation — every ownership operation is
    /// conditional on it, so a late retire cannot touch a reused key.
    pub fn incarnation(&self) -> u64 {
        self.incarnation
    }

    /// Whether the record still owns a live call (not terminal).
    pub fn is_live(&self) -> bool {
        self.terminal.is_none()
    }

    /// Classify an inbound frame and apply its state effect.
    pub fn on_frame(&mut self, frame: Frame) -> Disposition {
        // Terminal swallows everything, including CANCEL: the outcome is
        // already selected and re-entering retirement would race the
        // emission owner.
        if self.terminal.is_some() {
            return Disposition::Ignored;
        }
        match frame {
            // CANCEL stays admissible while draining — a caller that
            // walked away should not have to wait out a drain it will
            // never credit.
            Frame::Cancel => Disposition::Retire(TerminalReason::Cancelled),
            // Credit is what lets a drain finish, so it must survive the
            // handler's return. Once the pump is gone there is nothing to
            // credit.
            Frame::Grant => match self.output {
                Output::Open | Output::Draining(_) => Disposition::Credit,
                Output::Ended => Disposition::Ignored,
            },
            Frame::Chunk => match self.input {
                Input::Open => Disposition::Deliver,
                Input::Ended | Input::Closed => Disposition::Ignored,
            },
            Frame::End => match self.input {
                Input::Open => {
                    self.input = Input::Ended;
                    Disposition::InputEnded
                }
                // Idempotent, and never touches `output`: half-close
                // independence.
                Input::Ended | Input::Closed => Disposition::Ignored,
            },
        }
    }

    /// The handler returned. Output enters `Draining`, and an open input
    /// half becomes `Closed` — its consumer is gone, so retaining chunks
    /// would be a leak and delivering them impossible.
    ///
    /// Half-close independence protects legitimate *output* from an early
    /// END. It does not oblige a handler to wait for END before
    /// rejecting, which is exactly what an aggregate or a validation
    /// failure needs.
    pub fn handler_returned(&mut self, result: HandlerResult) -> bool {
        if self.terminal.is_some() || !matches!(self.output, Output::Open) {
            return false;
        }
        self.output = Output::Draining(result);
        if self.input == Input::Open {
            self.input = Input::Closed;
        }
        true
    }

    /// The pump (or, for client-streaming, the single-response emitter)
    /// stopped. While `Draining` this commits `Completed(result)`; while
    /// still `Open` the producer died under the handler, which is a typed
    /// failure and never a success.
    pub fn pump_exited(&mut self) -> Option<TerminalReason> {
        if self.terminal.is_some() {
            return None;
        }
        let reason = match std::mem::replace(&mut self.output, Output::Ended) {
            Output::Draining(result) => TerminalReason::Completed(result),
            Output::Open => TerminalReason::PumpFailed,
            Output::Ended => {
                // Already ended and non-terminal is unreachable; restore
                // and report nothing rather than inventing an outcome.
                self.output = Output::Ended;
                return None;
            }
        };
        self.terminal = Some(Terminal {
            reason: reason.clone(),
            emission: None,
        });
        Some(reason)
    }

    /// Retire from ANY state, including `Draining`. First writer wins;
    /// later END, handler return or pump exit are no-ops.
    pub fn retire(&mut self, reason: TerminalReason) -> bool {
        if self.terminal.is_some() {
            return false;
        }
        self.terminal = Some(Terminal {
            reason,
            emission: None,
        });
        true
    }

    /// An admitted input item could not be reserved or delivered
    /// (§2.7). While input is open this kills the call; once the input
    /// half is already `Ended`/`Closed` a late chunk is merely refused and
    /// must not replace the handler's result.
    pub fn input_admission_failed(&mut self) -> Option<TerminalReason> {
        if self.input != Input::Open {
            return None;
        }
        if self.retire(TerminalReason::ResourceExhausted) {
            Some(TerminalReason::ResourceExhausted)
        } else {
            None
        }
    }

    /// A protected **output** item was refused — unsatisfiable against
    /// the configured per-call budget, or non-deliverable (§2.7 response
    /// direction). The refusal latches `ResourceExhausted` and retires
    /// the call: a refused item is never a metric-only drop followed by a
    /// successful completion.
    ///
    /// Refusals once the output half is no longer `Open` are the
    /// producer-gate class and must not come here: a stale sink clone
    /// cannot overwrite the preserved handler result.
    pub fn output_admission_failed(&mut self) -> Option<TerminalReason> {
        if self.output != Output::Open {
            return None;
        }
        if self.retire(TerminalReason::ResourceExhausted) {
            Some(TerminalReason::ResourceExhausted)
        } else {
            None
        }
    }

    /// Record the terminal's control-path disposition. Returns `true`
    /// exactly once, for the supervisor that owns the emission; the first
    /// disposition wins like every other terminal write.
    pub fn record_emission(&mut self, disposition: TerminalDisposition) -> bool {
        match self.terminal.as_mut() {
            Some(terminal) if terminal.emission.is_none() => {
                terminal.emission = Some(disposition);
                true
            }
            _ => false,
        }
    }
}

// ---------------------------------------------------------------------
// §2.2 — the bounded supervisor
// ---------------------------------------------------------------------

/// Abstract response pump: drains queued items, paying one credit per
/// item, and records what it published. Stands in for the fold's pump
/// task (`cortex/rpc.rs:2779`).
#[derive(Debug)]
pub struct PumpHandles {
    /// Item credit, mirroring the fold's `flow_control` semaphore. Closed
    /// by retirement so a parked `acquire` errors out instead of hanging.
    pub credit: Arc<Semaphore>,
    /// Byte permits (§2.7). Closed by the same retirement.
    pub bytes: Arc<Semaphore>,
}

/// What the supervisor observed, for assertions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupervisorOutcome {
    /// The committed terminal.
    pub terminal: TerminalReason,
    /// Response items actually published before the terminal.
    pub published: usize,
    /// The recorded control-path disposition of the terminal (exactly
    /// once). `Some(TerminalDisposition::Queued)` is NOT peer receipt.
    pub emission: Option<TerminalDisposition>,
    /// Response items discarded instead of published: still queued when
    /// the pump's receiver died, taken off the queue but not yet
    /// published when a retirement terminal committed (CORE-2's publish
    /// barrier), or refused at that barrier. Every admitted-but-never-
    /// published item lands here — never a silent drop.
    pub discarded: usize,
}

/// Retirement signal shared by the revocation callback, the session
/// sweep, `ServeHandle::drop` and the CANCEL arm.
#[derive(Debug)]
pub struct RetireSignal {
    notify: Notify,
    reason: parking_lot::Mutex<Option<TerminalReason>>,
}

impl Default for RetireSignal {
    fn default() -> Self {
        Self::new()
    }
}

impl RetireSignal {
    /// A fresh, unsignalled handle.
    pub fn new() -> Self {
        Self {
            notify: Notify::new(),
            reason: parking_lot::Mutex::new(None),
        }
    }

    /// Signal retirement. First reason wins, matching
    /// [`CallLifecycle::retire`].
    pub fn fire(&self, reason: TerminalReason) {
        let mut slot = self.reason.lock();
        if slot.is_none() {
            *slot = Some(reason);
        }
        drop(slot);
        self.notify.notify_waiters();
    }

    fn taken(&self) -> Option<TerminalReason> {
        self.reason.lock().clone()
    }

    async fn wait(&self) -> TerminalReason {
        loop {
            // Register before re-checking: a `fire` between the check and
            // the await would otherwise be missed for good.
            let notified = self.notify.notified();
            if let Some(reason) = self.taken() {
                return reason;
            }
            notified.await;
        }
    }
}

/// The producer-finished gate (§2.2). Once the handler returns, the
/// call's sink is logically closed: new sends are refused even from a
/// clone the handler retained or handed to a detached task, and the pump
/// drains only what was already admitted.
///
/// Without this gate a retained clone keeps the queue open, the pump
/// parks in `recv`, and the call runs to its deadline instead of
/// completing — which is exactly what the first run of
/// `retained_sink_clone_cannot_extend_drain` demonstrated.
#[derive(Debug, Default)]
pub struct ProducerGate {
    finished: std::sync::atomic::AtomicBool,
    woken: Notify,
}

impl ProducerGate {
    /// A gate that is still admitting.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the producer half is closed.
    pub fn is_finished(&self) -> bool {
        self.finished.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Close the producer half and wake a pump parked on `recv`.
    pub fn finish(&self) {
        self.finished
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.woken.notify_waiters();
    }
}

/// The sink half a protected handler holds, with §2.7's two refusal
/// classes kept apart:
///
/// 1. **Producer-gate closed** — refused WITHOUT latching, so a retained
///    clone cannot extend the drain or change the result the call already
///    holds (§2.2).
/// 2. **A refused protected item** — unsatisfiable against `budget`, or
///    non-deliverable: this LATCHES `ResourceExhausted` and retires the
///    call (§2.7 response direction). The item is never merely dropped
///    and counted.
pub async fn sink_send(
    state: &parking_lot::Mutex<CallLifecycle>,
    gate: &ProducerGate,
    tx: &mpsc::Sender<usize>,
    budget: usize,
    len: usize,
) -> Result<(), SinkClosed> {
    if gate.is_finished() {
        return Err(SinkClosed);
    }
    if len > budget {
        state.lock().output_admission_failed();
        return Err(SinkClosed);
    }
    if tx.send(len).await.is_err() {
        state.lock().output_admission_failed();
        return Err(SinkClosed);
    }
    Ok(())
}

/// The protected sink's refusal — the model's `RpcSinkClosed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SinkClosed;

/// One response item the pump took off the queue but has not published
/// yet. Dropping it unpublished IS a discard (CORE-2's publish barrier
/// refusing it, a permit acquisition that errors out, an aborted pump's
/// parked in-hand item) and must be counted — an item may never vanish
/// uncounted between `admitted` and `published`.
struct InFlightItem {
    len: Option<usize>,
    discarded: Arc<std::sync::atomic::AtomicUsize>,
}

impl InFlightItem {
    fn new(len: usize, discarded: &Arc<std::sync::atomic::AtomicUsize>) -> Self {
        Self {
            len: Some(len),
            discarded: Arc::clone(discarded),
        }
    }

    /// The CORE-2 publish barrier. A retirement terminal commits under
    /// `state`'s lock (`CallLifecycle::retire` is invoked with that lock
    /// held), so re-checking liveness AND counting the publish under the
    /// same lock linearizes every publish before any terminal commit:
    /// **zero items publish after a retirement terminal commits**. Once
    /// a terminal has committed the item is a counted discard (§2.2:
    /// every retirement discards), never a metric-only drop — and the
    /// pump stops publishing entirely (`false`).
    fn publish(
        mut self,
        state: &parking_lot::Mutex<CallLifecycle>,
        published: &std::sync::atomic::AtomicUsize,
    ) -> bool {
        let live = state.lock().is_live();
        if live {
            self.len = None;
            published.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        // Unarmed: `Drop` counts nothing. Still armed: `Drop` counts the
        // discard.
        live
    }
}

impl Drop for InFlightItem {
    fn drop(&mut self) {
        if self.len.take().is_some() {
            self.discarded
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

/// The pump's queue, counted on death: items still queued when the
/// receiver dies — retirement, deadline, a producer that died mid-send —
/// are discards (§2.2: every retirement discards), never silent drops.
struct CountedQueue {
    rx: mpsc::Receiver<usize>,
    discarded: Arc<std::sync::atomic::AtomicUsize>,
}

impl Drop for CountedQueue {
    fn drop(&mut self) {
        let left = self.rx.len();
        if left > 0 {
            self.discarded
                .fetch_add(left, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

/// Run one protected call to a bounded terminal (§2.2).
///
/// The supervisor stays in its `select!` after the handler returns:
/// **producer finished is not terminal**. It exits only when the pump
/// has stopped — because it drained, or because retirement closed its
/// semaphores. Retirement therefore bounds a drain that the caller never
/// credits; the drain does not bound retirement.
///
/// `handler` is the user future; `queued` is the pump's inbound queue
/// (the handler's sink side is the matching `Sender`); `deadline` is the
/// already-resolved §2.1 end.
#[expect(
    clippy::too_many_arguments,
    reason = "the §2.2 ctl parameter pushed this one over the arg-count lint; the model names \
              every supervisor-owned piece one-for-one (handler, pump queue, gate, semaphores, \
              retire signal, deadline, emission counter, control path) and a params struct \
              would only rename the arguments and hide that mapping"
)]
pub async fn run_supervisor(
    state: Arc<parking_lot::Mutex<CallLifecycle>>,
    handler: impl Future<Output = HandlerResult> + Send,
    queued: mpsc::Receiver<usize>,
    gate: Arc<ProducerGate>,
    pump: PumpHandles,
    retire: Arc<RetireSignal>,
    deadline: Option<(tokio::time::Instant, TerminalReason)>,
    published: Arc<std::sync::atomic::AtomicUsize>,
    ctl: mpsc::Sender<TerminalReason>,
) -> SupervisorOutcome {
    use std::sync::atomic::Ordering;

    let pump_published = Arc::clone(&published);
    let discarded = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let pump_credit = Arc::clone(&pump.credit);
    let pump_bytes = Arc::clone(&pump.bytes);

    // The pump: one credit and `len` byte permits per item. Both
    // acquisitions are cancellation points — closing either semaphore is
    // what unparks a pump the caller stopped crediting. Every item is
    // either published (through CORE-2's barrier) or counted discarded:
    // `SupervisorOutcome` accounts for each admitted item exactly once.
    let pump_gate = Arc::clone(&gate);
    let pump_state = Arc::clone(&state);
    let pump_discarded = Arc::clone(&discarded);
    let pump_task = tokio::spawn(async move {
        let mut queued = CountedQueue {
            rx: queued,
            discarded: Arc::clone(&pump_discarded),
        };
        loop {
            // After the producer half closes, drain what is already
            // admitted and stop — never wait for another send that the
            // gate has just made impossible.
            let next = if pump_gate.is_finished() {
                queued.rx.try_recv().ok()
            } else {
                tokio::select! {
                    item = queued.rx.recv() => item,
                    () = pump_gate.woken.notified() => continue,
                }
            };
            let Some(len) = next else { break };
            let item = InFlightItem::new(len, &pump_discarded);
            let Ok(permit) = pump_credit.clone().acquire_owned().await else {
                break;
            };
            permit.forget();
            let Ok(bytes) = pump_bytes
                .clone()
                .acquire_many_owned(u32::try_from(len).unwrap_or(u32::MAX))
                .await
            else {
                break;
            };
            // Publishing releases the item's byte reservation: the bytes
            // left the queue, they were not merely counted. A barrier
            // refusal (the terminal already committed) discards this
            // item AND everything still queued — nothing may publish.
            drop(bytes);
            if !item.publish(&pump_state, &pump_published) {
                break;
            }
        }
    });
    tokio::pin!(pump_task);

    let mut handler = std::pin::pin!(handler);
    let mut handler_done = false;
    let mut pump_done = false;

    let sleep = async {
        match deadline {
            Some((at, reason)) => {
                tokio::time::sleep_until(at).await;
                reason
            }
            // No deadline arm: park forever rather than fire immediately.
            None => std::future::pending().await,
        }
    };
    let mut sleep = std::pin::pin!(sleep);
    let expiry_armed = true;

    let forced: Option<TerminalReason> = loop {
        if pump_done {
            break None;
        }
        tokio::select! {
            biased;

            reason = retire.wait() => break Some(reason),

            reason = &mut sleep, if expiry_armed => break Some(reason),

            result = &mut handler, if !handler_done => {
                handler_done = true;
                // Producer finished: Draining, NOT terminal. The gate
                // closes the sink so a retained clone cannot extend the
                // drain, while grants stay creditable and expiry stays
                // armed.
                state.lock().handler_returned(result);
                gate.finish();
            }

            joined = &mut pump_task, if !pump_done => {
                pump_done = true;
                let _ = joined;
            }
        }
    };

    // CORE-1 (the R4COREFIX-9 mechanism): a pump exit must not shadow a
    // handler whose result has landed but has not been polled yet. The
    // handler's sink sender drops at handler end, BEFORE the result
    // deposit does (the nRPC blocking-bridge window), so the pump exit
    // and the ready handler race exactly here — and a `pump_done` break
    // that never re-polls the handler classifies the call `PumpFailed`
    // (wire 0x0006) over an already-complete handler. Poll the handler
    // ONE more time before classifying a pump exit; its result wins.
    if forced.is_none() && !handler_done {
        let deposited = tokio::select! {
            biased;
            result = &mut handler => Some(result),
            () = std::future::ready(()) => None,
        };
        if let Some(result) = deposited {
            state.lock().handler_returned(result);
            gate.finish();
        }
    }

    // Retirement path: close both semaphores so a parked pump errors out,
    // then abort and join it. `abort` is cooperative with the scheduler,
    // so the join is what establishes "no chunk is published after the
    // terminal" — not the abort call. (CORE-2's publish barrier is the
    // backstop for a pump that reaches its publish point anyway.)
    if let Some(reason) = forced.clone() {
        state.lock().retire(reason);
        pump.credit.close();
        pump.bytes.close();
        if !pump_done {
            pump_task.as_mut().abort();
            let _ = pump_task.as_mut().await;
        }
    } else {
        // A pump exit — clean or failed — is classified by the state
        // machine: `Completed(result)` once the handler has returned
        // (CORE-1's re-poll above included), and `PumpFailed` only for a
        // pump that stopped WITHOUT the handler returning (its documented
        // meaning). A blanket `PumpFailed` here is how a pump exit
        // shadows a completed handler.
        state.lock().pump_exited();
    }

    let guard = state.lock();
    let terminal = guard
        .terminal()
        .map(|t| t.reason.clone())
        .unwrap_or(TerminalReason::PumpFailed);
    drop(guard);

    // §2.8 — record what the control path DID with the terminal, not
    // that an attempt was made. `Queued` is the bounded control queue
    // accepting the job; `Sent` (transport accept) has no model transport
    // and is recorded at that seam in production; a full queue is
    // `Refused` and a gone session is `Unreachable` — both interruption,
    // both still release ownership, which is this function returning.
    let disposition = match ctl.try_send(terminal.clone()) {
        Ok(()) => TerminalDisposition::Queued,
        Err(mpsc::error::TrySendError::Full(_)) => TerminalDisposition::Refused,
        Err(mpsc::error::TrySendError::Closed(_)) => TerminalDisposition::Unreachable,
    };
    let mut guard = state.lock();
    guard.record_emission(disposition);
    let emission = guard.terminal().and_then(|t| t.emission);
    drop(guard);

    SupervisorOutcome {
        terminal,
        published: published.load(Ordering::SeqCst),
        emission,
        discarded: discarded.load(Ordering::SeqCst),
    }
}

/// Convenience: the semaphores for a call with `credit` item permits and
/// `bytes` byte permits.
pub fn pump_handles(credit: usize, bytes: usize) -> PumpHandles {
    PumpHandles {
        credit: Arc::new(Semaphore::new(credit)),
        bytes: Arc::new(Semaphore::new(bytes)),
    }
}

/// A short bound for model waits: long enough that a correct
/// implementation always finishes, short enough that a hang is a failure
/// rather than a suite timeout.
pub const MODEL_BOUND: Duration = Duration::from_secs(5);

/// A runtime-free driver for the same lifecycle.
///
/// [`run_supervisor`] above is tokio-shaped: tasks, semaphores,
/// `select!`. The browser/leaf runtime the owner ruled into the release
/// matrix (Q5) has **no executor at all** — `net/crates/net/leaf/` is
/// single-threaded `Rc<RefCell<_>>` over `futures-channel`, with no
/// `spawn` equivalent. A model whose only progress mechanism is `spawn`
/// therefore could not be reused by a runtime that must implement this
/// contract, and retrofitting that later is expensive.
///
/// So the decision logic lives in [`CallLifecycle`], which is
/// runtime-free by construction, and this module drives it with an
/// explicit `advance(now)` pull step: same states, same terminals, no
/// tasks and no semaphores. The two drivers are checked against each
/// other by the witnesses below.
pub mod pull {
    use std::collections::VecDeque;

    use super::{
        CallLifecycle, CallShape, HandlerResult, SinkClosed, TerminalDisposition, TerminalReason,
    };

    /// What one [`PullCall::advance`] step accomplished.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Progress {
        /// One queued item was published.
        Published,
        /// Nothing to do: the producer is still running and the queue is
        /// empty.
        Idle,
        /// Items are queued but credit or byte capacity is missing. The
        /// caller must grant; the call is NOT terminal.
        Blocked,
        /// The call reached its terminal on this step.
        Terminal(TerminalReason),
        /// Already terminal and emitted; nothing further happens.
        Done,
    }

    /// One protected call driven without any runtime.
    #[derive(Debug)]
    pub struct PullCall {
        state: CallLifecycle,
        queue: VecDeque<usize>,
        credit: usize,
        bytes_free: usize,
        bytes_total: usize,
        published: usize,
        producer_finished: bool,
        deadline: Option<(u64, TerminalReason)>,
        /// The control path's one reserved slot (§2.8). The runtime-free
        /// form of "bounded control-path capacity at admission": the
        /// terminal goes here, and the peer's read of it is receipt.
        control: Option<TerminalReason>,
    }

    impl PullCall {
        /// `deadline` is `(absolute_ns, reason)` from
        /// [`super::resolve_deadline`].
        pub fn new(
            shape: CallShape,
            credit: usize,
            bytes: usize,
            deadline: Option<(u64, TerminalReason)>,
        ) -> Self {
            Self {
                state: CallLifecycle::new(shape, 1),
                queue: VecDeque::new(),
                credit,
                bytes_free: bytes,
                bytes_total: bytes,
                published: 0,
                producer_finished: false,
                deadline,
                control: None,
            }
        }

        /// The terminal the control path accepted, for the peer side to
        /// observe. Presence here is `Queued`, not peer receipt.
        pub fn control(&self) -> Option<&TerminalReason> {
            self.control.as_ref()
        }

        /// The handler's sink. Refuses once the producer half is closed
        /// (the gate) or the call is terminal, and reserves bytes before
        /// accepting. Two refusal classes, as in [`super::sink_send`]:
        ///
        /// - unsatisfiable (`len > bytes_total`): prompt refusal that
        ///   LATCHES `ResourceExhausted` (§2.7) — the item can never be
        ///   queued, and its drop must not end in success;
        /// - transient saturation (`len > bytes_free`, within total): the
        ///   sync driver cannot `send_wait`, so this is a caller-visible
        ///   backpressure refusal (retry after [`Self::advance`]), not a
        ///   latch. The async form waits here instead.
        pub fn submit(&mut self, len: usize) -> Result<(), SinkClosed> {
            if self.producer_finished || !self.state.is_live() {
                return Err(SinkClosed);
            }
            if len > self.bytes_total {
                self.state.output_admission_failed();
                return Err(SinkClosed);
            }
            if len > self.bytes_free {
                return Err(SinkClosed);
            }
            self.bytes_free -= len;
            self.queue.push_back(len);
            Ok(())
        }

        /// A `STREAM_GRANT` from the caller.
        pub fn grant(&mut self, items: usize) {
            self.credit = self.credit.saturating_add(items);
        }

        /// The handler returned: `Draining`, gate closed, input closed.
        pub fn handler_returned(&mut self, result: HandlerResult) {
            if self.state.handler_returned(result) {
                self.producer_finished = true;
            }
        }

        /// Retirement from any source.
        pub fn retire(&mut self, reason: TerminalReason) -> bool {
            self.state.retire(reason)
        }

        /// Items actually published.
        pub fn published(&self) -> usize {
            self.published
        }

        /// Items still queued (discarded on any retirement).
        pub fn queued(&self) -> usize {
            self.queue.len()
        }

        /// The underlying state, for assertions.
        pub fn state(&self) -> &CallLifecycle {
            &self.state
        }

        /// One step. `now_ns` is the caller's clock — the leaf has no
        /// timer either, so expiry is evaluated here rather than fired
        /// by a runtime.
        pub fn advance(&mut self, now_ns: u64) -> Progress {
            if let Some(terminal) = self.state.terminal() {
                if terminal.emission.is_some() {
                    return Progress::Done;
                }
                let reason = terminal.reason.clone();
                // Every retirement discards; only a completion drained,
                // and it drained before the terminal was committed.
                if !reason.drains_queued_output() {
                    self.release_queue();
                }
                // §2.8 — the one reserved control slot takes the terminal
                // (`Queued`) or refuses it (`Refused`, interruption).
                let disposition = match self.control {
                    None => {
                        self.control = Some(reason.clone());
                        TerminalDisposition::Queued
                    }
                    Some(_) => TerminalDisposition::Refused,
                };
                self.state.record_emission(disposition);
                return Progress::Terminal(reason);
            }

            // Expiry is checked before any commit, so a call cannot
            // publish an item after its end.
            if let Some((end, reason)) = self.deadline.clone() {
                if now_ns >= end {
                    self.state.retire(reason);
                    return self.advance(now_ns);
                }
            }

            match self.queue.front().copied() {
                Some(len) if self.credit > 0 => {
                    self.credit -= 1;
                    self.queue.pop_front();
                    // Publishing releases the item's reservation exactly
                    // once, at the point the bytes leave the queue.
                    self.bytes_free += len;
                    self.published += 1;
                    Progress::Published
                }
                Some(_) => Progress::Blocked,
                None if self.producer_finished => {
                    // Pump exit: the state machine decides completion.
                    self.state.pump_exited();
                    self.advance(now_ns)
                }
                None => Progress::Idle,
            }
        }

        /// Drive to a terminal, bounded by `max_steps` so a model bug is
        /// a failure rather than a hang.
        pub fn run_to_terminal(&mut self, now_ns: u64, max_steps: usize) -> Option<TerminalReason> {
            for _ in 0..max_steps {
                match self.advance(now_ns) {
                    Progress::Terminal(reason) => return Some(reason),
                    Progress::Blocked | Progress::Idle => return None,
                    Progress::Done => return None,
                    Progress::Published => {}
                }
            }
            None
        }

        fn release_queue(&mut self) {
            while let Some(len) = self.queue.pop_front() {
                self.bytes_free += len;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const SEC: u64 = 1_000_000_000;

    fn ss() -> CallLifecycle {
        CallLifecycle::new(CallShape::ServerStreaming, 1)
    }
    fn cs() -> CallLifecycle {
        CallLifecycle::new(CallShape::ClientStreaming, 1)
    }
    fn dx() -> CallLifecycle {
        CallLifecycle::new(CallShape::Duplex, 1)
    }

    // ---------------- §2.1 deadline ----------------

    #[test]
    fn omitted_deadline_takes_the_provider_default() {
        let policy = LifetimePolicy::q1_defaults();
        let got = resolve_deadline(1_000 * SEC, None, &[], &policy).expect("resolves");
        assert_eq!(got.end_ns, 1_300 * SEC, "300 s default from now");
        assert_eq!(got.bound, DeadlineBound::Deadline);
    }

    #[test]
    fn explicit_deadline_within_the_cap_is_honoured_not_clamped_to_the_default() {
        // The defect this pins: a single `min(requested, now+default)`
        // silently turns every 900 s request into 300 s.
        let policy = LifetimePolicy::q1_defaults();
        let got = resolve_deadline(1_000 * SEC, Some(1_900 * SEC), &[], &policy).expect("resolves");
        assert_eq!(got.end_ns, 1_900 * SEC);
        assert_eq!(got.bound, DeadlineBound::Deadline);
    }

    #[test]
    fn explicit_deadline_over_the_cap_is_refused_not_clamped() {
        // And this pins the other half: under a `min` the refusal branch
        // is unreachable, because the default always wins first.
        let policy = LifetimePolicy::q1_defaults();
        let got = resolve_deadline(1_000 * SEC, Some(8_200 * SEC), &[], &policy);
        assert_eq!(got, Err(DeadlineRefusal::ExceedsPolicy));
    }

    #[test]
    fn credential_validity_clamps_and_changes_the_terminal_reason() {
        let policy = LifetimePolicy::q1_defaults();
        let got = resolve_deadline(
            1_000 * SEC,
            Some(1_900 * SEC),
            &[Some(1_100 * SEC), None],
            &policy,
        )
        .expect("resolves");
        assert_eq!(got.end_ns, 1_100 * SEC);
        assert_eq!(got.bound, DeadlineBound::Credential);
        assert_eq!(got.expiry_reason(), TerminalReason::CredentialExpired);
    }

    #[test]
    fn provider_authority_validity_bounds_the_call_too() {
        // Not only caller credentials: the provider's own authority
        // validity is in the same list.
        let policy = LifetimePolicy::q1_defaults();
        let got = resolve_deadline(1_000 * SEC, None, &[None, None, Some(1_010 * SEC)], &policy)
            .expect("resolves");
        assert_eq!(got.end_ns, 1_010 * SEC);
        assert_eq!(got.bound, DeadlineBound::Credential);
    }

    #[test]
    fn an_exact_tie_reports_credential_expiry_not_timeout() {
        let policy = LifetimePolicy::q1_defaults();
        let got = resolve_deadline(
            1_000 * SEC,
            Some(1_500 * SEC),
            &[Some(1_500 * SEC)],
            &policy,
        )
        .expect("resolves");
        assert_eq!(got.bound, DeadlineBound::Credential);
        assert_eq!(got.expiry_reason(), TerminalReason::CredentialExpired);
    }

    #[test]
    fn an_already_elapsed_effective_end_is_refused_before_any_effect() {
        let policy = LifetimePolicy::q1_defaults();
        assert_eq!(
            resolve_deadline(1_000 * SEC, Some(1_900 * SEC), &[Some(999 * SEC)], &policy),
            Err(DeadlineRefusal::AlreadyElapsed),
        );
    }

    #[test]
    fn clock_overflow_refuses_rather_than_wrapping_into_the_past() {
        let policy = LifetimePolicy::q1_defaults();
        assert_eq!(
            resolve_deadline(u64::MAX - 5, None, &[], &policy),
            Err(DeadlineRefusal::Overflow),
        );
        assert_eq!(secs_to_ns(u64::MAX), None);
        assert_eq!(secs_to_ns(3), Some(3 * SEC));
    }

    #[test]
    fn policy_validation_rejects_a_default_over_the_maximum() {
        assert_eq!(LifetimePolicy::q1_defaults().validate(), Ok(()));
        assert_eq!(
            LifetimePolicy {
                default_live_ns: 2 * SEC,
                max_live_ns: SEC,
            }
            .validate(),
            Err(PolicyError::DefaultOverMax),
        );
        assert_eq!(
            LifetimePolicy {
                default_live_ns: 0,
                max_live_ns: SEC,
            }
            .validate(),
            Err(PolicyError::NotPositive),
        );
    }

    // ---------------- §2.6 state model ----------------

    #[test]
    fn server_streaming_starts_with_its_input_half_already_ended() {
        assert_eq!(ss().input(), Input::Ended);
        assert_eq!(cs().input(), Input::Open);
        assert_eq!(dx().input(), Input::Open);
    }

    #[test]
    fn end_is_idempotent_and_never_touches_the_output_half() {
        let mut call = dx();
        assert_eq!(
            call.on_frame(Frame::End),
            Disposition::InputEnded,
            "the first END half-closes the input, and says so",
        );
        assert_eq!(call.input(), Input::Ended);
        assert_eq!(*call.output(), Output::Open);
        // Second END changes nothing and does not end output.
        assert_eq!(call.on_frame(Frame::End), Disposition::Ignored);
        assert_eq!(call.input(), Input::Ended);
        assert_eq!(*call.output(), Output::Open);
        assert!(call.is_live());
    }

    #[test]
    fn handler_return_closes_an_open_input_half_and_preserves_the_result() {
        let mut call = cs();
        assert!(call.handler_returned(HandlerResult::Err(0x8001, "bad".into())));
        assert_eq!(call.input(), Input::Closed);
        assert_eq!(
            *call.output(),
            Output::Draining(HandlerResult::Err(0x8001, "bad".into()))
        );
        // A late chunk is refused, not delivered, and does not become a
        // resource failure that would overwrite the handler's verdict.
        assert_eq!(call.on_frame(Frame::Chunk), Disposition::Ignored);
        assert_eq!(call.input_admission_failed(), None);
        assert!(call.is_live(), "draining is not terminal");
    }

    #[test]
    fn a_handler_error_survives_the_drain_as_the_terminal() {
        let mut call = ss();
        call.handler_returned(HandlerResult::Err(0x8001, "nope".into()));
        let terminal = call.pump_exited().expect("terminal");
        assert_eq!(
            terminal,
            TerminalReason::Completed(HandlerResult::Err(0x8001, "nope".into())),
        );
    }

    #[test]
    fn grants_stay_admissible_while_draining_and_stop_once_output_ended() {
        let mut call = ss();
        assert_eq!(call.on_frame(Frame::Grant), Disposition::Credit);
        call.handler_returned(HandlerResult::Ok);
        assert_eq!(
            call.on_frame(Frame::Grant),
            Disposition::Credit,
            "a drain that needs credit must be able to receive it",
        );
        assert_eq!(call.on_frame(Frame::Chunk), Disposition::Ignored);
        call.pump_exited();
        assert_eq!(call.on_frame(Frame::Grant), Disposition::Ignored);
    }

    #[test]
    fn cancel_is_admissible_while_draining_and_preempts_completion() {
        let mut call = dx();
        call.handler_returned(HandlerResult::Ok);
        assert_eq!(
            call.on_frame(Frame::Cancel),
            Disposition::Retire(TerminalReason::Cancelled),
        );
        assert!(call.retire(TerminalReason::Cancelled));
        // The drain's completion loses the race it started second.
        assert_eq!(call.pump_exited(), None);
        assert_eq!(
            call.terminal().map(|t| t.reason.clone()),
            Some(TerminalReason::Cancelled),
        );
    }

    #[test]
    fn retire_is_first_writer_wins_from_every_state() {
        for mut call in [ss(), cs(), dx()] {
            assert!(call.retire(TerminalReason::Revoked));
            assert!(!call.retire(TerminalReason::Cancelled));
            assert!(!call.handler_returned(HandlerResult::Ok));
            assert_eq!(call.pump_exited(), None);
            assert_eq!(
                call.terminal().map(|t| t.reason.clone()),
                Some(TerminalReason::Revoked),
            );
        }
        // …including from Draining.
        let mut call = ss();
        call.handler_returned(HandlerResult::Ok);
        assert!(call.retire(TerminalReason::Timeout));
        assert_eq!(
            call.terminal().map(|t| t.reason.clone()),
            Some(TerminalReason::Timeout),
        );
    }

    #[test]
    fn every_frame_after_the_terminal_is_dropped() {
        let mut call = dx();
        call.retire(TerminalReason::AuthorityUnavailable);
        for frame in [Frame::Chunk, Frame::End, Frame::Grant, Frame::Cancel] {
            assert_eq!(call.on_frame(frame), Disposition::Ignored, "{frame:?}");
        }
    }

    #[test]
    fn a_pump_that_dies_under_an_open_handler_is_a_typed_failure() {
        let mut call = ss();
        assert_eq!(call.pump_exited(), Some(TerminalReason::PumpFailed));
        assert!(!matches!(
            call.terminal().map(|t| t.reason.clone()),
            Some(TerminalReason::Completed(_)),
        ));
    }

    #[test]
    fn an_undeliverable_admitted_input_item_kills_the_call() {
        let mut call = cs();
        assert_eq!(
            call.input_admission_failed(),
            Some(TerminalReason::ResourceExhausted),
        );
        // No Ok completion is reachable afterwards: the drain cannot
        // overwrite the terminal.
        assert!(!call.handler_returned(HandlerResult::Ok));
        assert_eq!(call.pump_exited(), None);
        assert_eq!(
            call.terminal().map(|t| t.reason.clone()),
            Some(TerminalReason::ResourceExhausted),
        );
    }

    #[test]
    fn the_terminal_is_emitted_exactly_once() {
        let mut call = ss();
        assert!(
            !call.record_emission(TerminalDisposition::Queued),
            "nothing to emit before a terminal"
        );
        call.retire(TerminalReason::Cancelled);
        assert!(call.record_emission(TerminalDisposition::Queued));
        assert!(
            !call.record_emission(TerminalDisposition::Refused),
            "the first disposition wins like every other terminal write"
        );
        assert_eq!(
            call.terminal().expect("terminal").emission,
            Some(TerminalDisposition::Queued)
        );
    }

    #[test]
    fn only_a_completion_drains_queued_output() {
        assert!(TerminalReason::Completed(HandlerResult::Ok).drains_queued_output());
        assert!(
            TerminalReason::Completed(HandlerResult::Err(1, String::new())).drains_queued_output()
        );
        for reason in [
            TerminalReason::Cancelled,
            TerminalReason::Timeout,
            TerminalReason::CredentialExpired,
            TerminalReason::Revoked,
            TerminalReason::AuthorityUnavailable,
            TerminalReason::ResourceExhausted,
            TerminalReason::SessionReplaced,
            TerminalReason::ServeHandleDropped,
            TerminalReason::PumpFailed,
        ] {
            assert!(!reason.drains_queued_output(), "{reason:?}");
        }
    }

    #[test]
    fn two_calls_are_independent() {
        let mut a = CallLifecycle::new(CallShape::Duplex, 7);
        let mut b = CallLifecycle::new(CallShape::Duplex, 8);
        a.retire(TerminalReason::Revoked);
        assert!(b.is_live());
        assert_eq!(b.on_frame(Frame::Chunk), Disposition::Deliver);
        assert_eq!(a.incarnation(), 7);
        assert_eq!(b.incarnation(), 8);
    }

    // ---------------- §2.2 supervisor ----------------

    struct Rig {
        state: Arc<parking_lot::Mutex<CallLifecycle>>,
        tx: mpsc::Sender<usize>,
        gate: Arc<ProducerGate>,
        pump: PumpHandles,
        retire: Arc<RetireSignal>,
        published: Arc<AtomicUsize>,
        ctl: mpsc::Sender<TerminalReason>,
        ctl_rx: mpsc::Receiver<TerminalReason>,
    }

    impl Rig {
        /// Clone the pieces `run_supervisor` consumes, leaving the rig's
        /// own handles usable for driving the call from the test.
        fn pump(&self) -> PumpHandles {
            PumpHandles {
                credit: Arc::clone(&self.pump.credit),
                bytes: Arc::clone(&self.pump.bytes),
            }
        }
    }

    fn rig(shape: CallShape, credit: usize, bytes: usize) -> (Rig, mpsc::Receiver<usize>) {
        let (tx, rx) = mpsc::channel(16);
        // The control path's capacity reserved at admission: one terminal
        // slot per call (§2.8).
        let (ctl, ctl_rx) = mpsc::channel(1);
        (
            Rig {
                state: Arc::new(parking_lot::Mutex::new(CallLifecycle::new(shape, 1))),
                tx,
                gate: Arc::new(ProducerGate::new()),
                pump: pump_handles(credit, bytes),
                retire: Arc::new(RetireSignal::new()),
                published: Arc::new(AtomicUsize::new(0)),
                ctl,
                ctl_rx,
            },
            rx,
        )
    }

    #[tokio::test(start_paused = true)]
    async fn a_drain_blocked_on_zero_credit_is_not_terminal_and_a_later_grant_completes_it() {
        // The obligation the production shape fails: the handler is done,
        // items are queued, credit is zero. This must NOT be
        // `Completed` yet, and a grant arriving afterwards must finish it.
        let (r, rx) = rig(CallShape::ServerStreaming, 0, 1024);
        let credit = Arc::clone(&r.pump.credit);
        let state = Arc::clone(&r.state);
        let tx = r.tx.clone();

        let sup = tokio::spawn(run_supervisor(
            Arc::clone(&r.state),
            async move {
                tx.send(8).await.expect("queue");
                tx.send(8).await.expect("queue");
                HandlerResult::Ok
            },
            rx,
            Arc::clone(&r.gate),
            r.pump(),
            Arc::clone(&r.retire),
            None,
            Arc::clone(&r.published),
            r.ctl.clone(),
        ));

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            matches!(*state.lock().output(), Output::Draining(_)),
            "handler finished, so output is draining",
        );
        assert!(state.lock().is_live(), "producer finished is not terminal");
        assert_eq!(r.published.load(Ordering::SeqCst), 0);

        // The grant the caller finally sends.
        assert_eq!(state.lock().on_frame(Frame::Grant), Disposition::Credit);
        credit.add_permits(2);
        drop(r.tx);

        let out = tokio::time::timeout(MODEL_BOUND, sup)
            .await
            .expect("supervisor must finish once credit arrives")
            .expect("no panic");
        assert_eq!(out.terminal, TerminalReason::Completed(HandlerResult::Ok));
        assert_eq!(out.published, 2);
        assert_eq!(out.emission, Some(TerminalDisposition::Queued));
    }

    #[tokio::test(start_paused = true)]
    async fn the_deadline_fires_while_draining_and_discards_the_remainder() {
        let (r, rx) = rig(CallShape::ServerStreaming, 0, 1024);
        let tx = r.tx.clone();
        // Inside `MODEL_BOUND` so the assertion below observes the
        // call's own deadline, not the harness's patience running out.
        let at = tokio::time::Instant::now() + Duration::from_secs(1);

        let sup = tokio::spawn(run_supervisor(
            Arc::clone(&r.state),
            async move {
                tx.send(8).await.expect("queue");
                HandlerResult::Ok
            },
            rx,
            Arc::clone(&r.gate),
            r.pump(),
            Arc::clone(&r.retire),
            Some((at, TerminalReason::Timeout)),
            Arc::clone(&r.published),
            r.ctl.clone(),
        ));
        drop(r.tx);

        let out = tokio::time::timeout(MODEL_BOUND, sup)
            .await
            .expect("a drain nobody credits must still be bounded by the deadline")
            .expect("no panic");
        assert_eq!(out.terminal, TerminalReason::Timeout);
        assert_eq!(out.published, 0, "queued items are discarded, not drained");
        assert_eq!(out.discarded, 1, "the discard is counted, not silent");
        assert_eq!(out.emission, Some(TerminalDisposition::Queued));
    }

    #[tokio::test(start_paused = true)]
    async fn revocation_while_parked_on_credit_retires_within_the_bound() {
        let (r, rx) = rig(CallShape::Duplex, 0, 1024);
        let tx = r.tx.clone();
        let retire = Arc::clone(&r.retire);

        let sup = tokio::spawn(run_supervisor(
            Arc::clone(&r.state),
            async move {
                tx.send(8).await.expect("queue");
                // A handler that never returns: only retirement can end
                // this call.
                std::future::pending::<()>().await;
                HandlerResult::Ok
            },
            rx,
            Arc::clone(&r.gate),
            r.pump(),
            Arc::clone(&r.retire),
            None,
            Arc::clone(&r.published),
            r.ctl.clone(),
        ));

        tokio::time::sleep(Duration::from_millis(10)).await;
        retire.fire(TerminalReason::Revoked);

        let out = tokio::time::timeout(MODEL_BOUND, sup)
            .await
            .expect("retirement must not wait for the handler or the pump")
            .expect("no panic");
        assert_eq!(out.terminal, TerminalReason::Revoked);
        assert_eq!(out.published, 0);
        assert_eq!(out.emission, Some(TerminalDisposition::Queued));
    }

    #[tokio::test(start_paused = true)]
    async fn a_credited_drain_publishes_everything_then_completes() {
        // The positive control for the three negatives above: without it
        // "nothing was published" would pass vacuously.
        let (r, rx) = rig(CallShape::ServerStreaming, 8, 1024);
        let tx = r.tx.clone();

        let sup = tokio::spawn(run_supervisor(
            Arc::clone(&r.state),
            async move {
                for _ in 0..3 {
                    tx.send(16).await.expect("queue");
                }
                HandlerResult::Ok
            },
            rx,
            Arc::clone(&r.gate),
            r.pump(),
            Arc::clone(&r.retire),
            None,
            Arc::clone(&r.published),
            r.ctl.clone(),
        ));
        drop(r.tx);

        let out = tokio::time::timeout(MODEL_BOUND, sup)
            .await
            .expect("finishes")
            .expect("no panic");
        assert_eq!(out.terminal, TerminalReason::Completed(HandlerResult::Ok));
        assert_eq!(out.published, 3);
    }

    #[tokio::test(start_paused = true)]
    async fn retained_sink_clone_cannot_extend_drain() {
        // The handler returns but a clone of its sender is still alive,
        // so the queue never closes on its own. The producer-finished
        // gate is what must end the drain: already-admitted items are
        // published, the clone's later sends are refused, and the call
        // completes without waiting for the clone to drop or for the
        // deadline to save it.
        //
        // This test failed on its first run before `ProducerGate`
        // existed — the call ran to its deadline instead of completing.
        let (r, rx) = rig(CallShape::ServerStreaming, 8, 1024);
        let tx = r.tx.clone();
        let gate = Arc::clone(&r.gate);
        let retained = r.tx.clone();
        let handler_gate = Arc::clone(&r.gate);
        let handler_state = Arc::clone(&r.state);

        let sup = tokio::spawn(run_supervisor(
            Arc::clone(&r.state),
            async move {
                sink_send(&handler_state, &handler_gate, &tx, 1024, 4)
                    .await
                    .expect("admitted");
                HandlerResult::Ok
            },
            rx,
            Arc::clone(&r.gate),
            r.pump(),
            Arc::clone(&r.retire),
            // A deadline far beyond the model bound: if the call only
            // finishes because of it, the timeout below fires instead.
            Some((
                tokio::time::Instant::now() + Duration::from_secs(600),
                TerminalReason::Timeout,
            )),
            Arc::clone(&r.published),
            r.ctl.clone(),
        ));
        drop(r.tx);

        let out = tokio::time::timeout(MODEL_BOUND, sup)
            .await
            .expect("a live clone must not keep the drain alive")
            .expect("no panic");
        assert_eq!(out.published, 1, "the admitted item still drained");
        assert_eq!(
            out.terminal,
            TerminalReason::Completed(HandlerResult::Ok),
            "the gate, not the deadline, ended the call",
        );
        assert!(gate.is_finished());
        assert_eq!(
            sink_send(&r.state, &gate, &retained, 1024, 4).await,
            Err(SinkClosed),
            "a retained clone's later send must be refused",
        );
        assert_eq!(
            r.state.lock().terminal().map(|t| t.reason.clone()),
            Some(TerminalReason::Completed(HandlerResult::Ok)),
            "a stale clone's refusal must not latch over the preserved handler result",
        );
    }

    #[tokio::test(start_paused = true)]
    async fn client_stream_single_response_completes_without_pump() {
        // A valid early unary result on the upload shape: no pump, input
        // still open, no END from the caller. It must complete anyway.
        let mut call = CallLifecycle::new(CallShape::ClientStreaming, 1);
        assert_eq!(call.input(), Input::Open);
        call.handler_returned(HandlerResult::Ok);
        assert_eq!(call.input(), Input::Closed);
        let terminal = call
            .pump_exited()
            .expect("the single-response emitter supplies the drain-complete event");
        assert_eq!(terminal, TerminalReason::Completed(HandlerResult::Ok));
        assert!(call.record_emission(TerminalDisposition::Queued));
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_during_a_drain_preempts_the_completion_end_to_end() {
        let (r, rx) = rig(CallShape::Duplex, 0, 1024);
        let tx = r.tx.clone();
        let state = Arc::clone(&r.state);
        let retire = Arc::clone(&r.retire);

        let sup = tokio::spawn(run_supervisor(
            Arc::clone(&r.state),
            async move {
                tx.send(8).await.expect("queue");
                HandlerResult::Ok
            },
            rx,
            Arc::clone(&r.gate),
            r.pump(),
            Arc::clone(&r.retire),
            None,
            Arc::clone(&r.published),
            r.ctl.clone(),
        ));
        drop(r.tx);
        tokio::time::sleep(Duration::from_millis(10)).await;

        let disposition = state.lock().on_frame(Frame::Cancel);
        assert_eq!(
            disposition,
            Disposition::Retire(TerminalReason::Cancelled),
            "CANCEL must remain admissible while draining",
        );
        retire.fire(TerminalReason::Cancelled);

        let out = tokio::time::timeout(MODEL_BOUND, sup)
            .await
            .expect("finishes")
            .expect("no panic");
        assert_eq!(out.terminal, TerminalReason::Cancelled);
        assert_eq!(out.published, 0);
    }

    /// The R4COREFIX-9 window as a deterministic future: the handler's
    /// result deposit lands AFTER the supervisor's last `select!` poll of
    /// the handler — `call1` returns (dropping the sink's sender, which
    /// ends the pump) before the `spawn_blocking` result deposit does —
    /// and is observable only from a LATER poll. Poll by poll:
    ///
    /// 1. the supervisor's first `select!` poll: the deposit has not
    ///    landed;
    /// 2. the poll in the very evaluation whose pump arm fires (the
    ///    dropped sink has ended the pump): still not landed;
    /// 3. the re-poll before classification (CORE-1): landed.
    ///
    /// A supervisor that breaks on `pump_done` without that last poll
    /// classifies the call `PumpFailed` (wire 0x0006) over an
    /// already-complete handler.
    struct DepositLandsLate {
        polled: usize,
    }

    impl Future for DepositLandsLate {
        type Output = HandlerResult;

        fn poll(
            mut self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<HandlerResult> {
            self.polled += 1;
            if self.polled >= 3 {
                std::task::Poll::Ready(HandlerResult::Ok)
            } else {
                std::task::Poll::Pending
            }
        }
    }

    /// CORE-1 — a pump exit must not shadow a completed handler into
    /// `PumpFailed`. The sink is held OUTSIDE the handler future (a
    /// detached sink): the handler never touches the queue, and dropping
    /// that sink closes the queue and DRIVES THE PUMP EXIT with the
    /// handler already complete (its result deposit landed after its
    /// last poll). The outcome is the handler's, not `PumpFailed`.
    #[tokio::test(start_paused = true)]
    async fn a_pump_exit_never_shadows_a_completed_handler_into_pump_failed() {
        let (r, rx) = rig(CallShape::ServerStreaming, 8, 1024);
        let detached_sink = r.tx.clone();

        let sup = tokio::spawn(run_supervisor(
            Arc::clone(&r.state),
            DepositLandsLate { polled: 0 },
            rx,
            Arc::clone(&r.gate),
            r.pump(),
            Arc::clone(&r.retire),
            None,
            Arc::clone(&r.published),
            r.ctl.clone(),
        ));

        // Let the supervisor park in its `select!` — the handler has had
        // its first poll and is still pending.
        tokio::time::sleep(Duration::from_millis(10)).await;

        // The detached sink vanishes: the queue closes and the pump
        // exits, racing the handler's late deposit exactly as the
        // blocking bridge does.
        drop(detached_sink);
        drop(r.tx);

        let out = tokio::time::timeout(MODEL_BOUND, sup)
            .await
            .expect("the call ends at the pump exit, not the deadline")
            .expect("no panic");
        assert_eq!(
            out.terminal,
            TerminalReason::Completed(HandlerResult::Ok),
            "the handler was complete; a pump exit may not shadow it into PumpFailed: {out:?}",
        );
    }

    /// CORE-2 — an in-flight item at retire time is discarded, never
    /// published after the terminal. Two drives of the same obligation:
    ///
    /// (a) the forced path (a `RetireSignal` retirement): the terminal
    ///     commits and the pump is stopped mid-flight — the in-flight
    ///     item and the queued item die as counted DISCARDS, never
    ///     silent drops (`discarded` is real, CORE-3);
    /// (b) the race the forced path's own ordering opens — its
    ///     `retire(reason)` commits BEFORE the pump is stopped: a pump
    ///     that is still live when the terminal commits must discard at
    ///     the publish barrier, and ZERO items publish afterwards.
    #[tokio::test(start_paused = true)]
    async fn an_in_flight_item_at_retirement_is_discarded_never_published_after_the_terminal() {
        // (a) the forced path.
        let (r, rx) = rig(CallShape::Duplex, 0, 1024);
        let tx = r.tx.clone();
        let retire = Arc::clone(&r.retire);
        let sup = tokio::spawn(run_supervisor(
            Arc::clone(&r.state),
            async move {
                tx.send(8).await.expect("queue");
                tx.send(8).await.expect("queue");
                // A handler that never returns: only retirement can end
                // this call.
                std::future::pending::<()>().await;
                HandlerResult::Ok
            },
            rx,
            Arc::clone(&r.gate),
            r.pump(),
            Arc::clone(&r.retire),
            None,
            Arc::clone(&r.published),
            r.ctl.clone(),
        ));
        // The pump takes the first item and parks on zero credit; the
        // second stays queued. Both are in flight at retirement.
        tokio::time::sleep(Duration::from_millis(10)).await;
        retire.fire(TerminalReason::Revoked);
        let out = tokio::time::timeout(MODEL_BOUND, sup)
            .await
            .expect("the retirement bounds the call")
            .expect("no panic");
        assert_eq!(out.terminal, TerminalReason::Revoked);
        assert_eq!(out.published, 0);
        assert_eq!(
            out.discarded, 2,
            "the in-flight and the queued item are counted discards, not silent drops: {out:?}",
        );

        // (b) the race: the terminal commits — the same `retire(reason)`
        //     the forced path runs — while the pump is still live with an
        //     in-flight item and a queued item.
        let (r, rx) = rig(CallShape::ServerStreaming, 0, 1024);
        let tx = r.tx.clone();
        let handler_state = Arc::clone(&r.state);
        let handler_gate = Arc::clone(&r.gate);
        let sup = tokio::spawn(run_supervisor(
            Arc::clone(&r.state),
            async move {
                sink_send(&handler_state, &handler_gate, &tx, 1024, 8)
                    .await
                    .expect("admitted");
                sink_send(&handler_state, &handler_gate, &tx, 1024, 8)
                    .await
                    .expect("admitted");
                HandlerResult::Ok
            },
            rx,
            Arc::clone(&r.gate),
            r.pump(),
            Arc::clone(&r.retire),
            None,
            Arc::clone(&r.published),
            r.ctl.clone(),
        ));
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(
            r.state.lock().retire(TerminalReason::Revoked),
            "the terminal was still free at the retirement commit",
        );
        // The credit the caller never granted now arrives: the pump
        // reaches its publish point AFTER the terminal committed.
        r.pump.credit.add_permits(2);
        let out = tokio::time::timeout(MODEL_BOUND, sup)
            .await
            .expect("the call ends at the pump exit")
            .expect("no panic");
        assert_eq!(
            out.terminal,
            TerminalReason::Revoked,
            "the terminal is the retirement's: {out:?}",
        );
        assert_eq!(
            out.published, 0,
            "ZERO items publish after a retirement terminal commits: {out:?}",
        );
        assert_eq!(out.discarded, 2, "{out:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn an_oversized_item_never_waits_for_permits_it_cannot_get() {
        // A 4 MiB item against a 1 KiB per-call budget can never acquire
        // enough capacity; it must fail promptly rather than park. The
        // witness drives the REAL admission seam — `sink_send`, the §2.7
        // output direction — not `input_admission_failed()` directly: the
        // named claim is about an item waiting for permits it cannot get,
        // and only the send path can park (or refuse) there.
        let budget = 1024usize;
        let item = 4 * 1024 * 1024usize;
        assert!(
            item > budget,
            "the model's point is that this is unsatisfiable",
        );
        let (r, rx) = rig(CallShape::ServerStreaming, 8, budget);
        let tx = r.tx.clone();
        let handler_state = Arc::clone(&r.state);
        let handler_gate = Arc::clone(&r.gate);

        let sup = tokio::spawn(run_supervisor(
            Arc::clone(&r.state),
            async move {
                assert_eq!(
                    sink_send(&handler_state, &handler_gate, &tx, budget, item).await,
                    Err(SinkClosed),
                    "an unsatisfiable reservation is refused promptly, not awaited",
                );
                HandlerResult::Ok
            },
            rx,
            Arc::clone(&r.gate),
            r.pump(),
            Arc::clone(&r.retire),
            None,
            Arc::clone(&r.published),
            r.ctl.clone(),
        ));
        drop(r.tx);

        let out = tokio::time::timeout(MODEL_BOUND, sup)
            .await
            .expect("the oversized send must not park the call for permits it cannot get")
            .expect("no panic");
        assert_eq!(
            out.terminal,
            TerminalReason::ResourceExhausted,
            "the refusal latches (§2.7): {out:?}",
        );
        assert_eq!(out.published, 0, "nothing was admitted: {out:?}");
    }

    /// The plan's `protected_output_refusal_cannot_complete_ok`
    /// composition check (§2.7 response direction). Leg (a) is the same
    /// path succeeding — the positive control that keeps a broken harness
    /// from passing the refusal. Leg (b): an over-budget output item is a
    /// refused protected item, and the handler returning `Ok` afterwards
    /// must NOT turn the call into a successful completion.
    #[tokio::test(start_paused = true)]
    async fn protected_output_refusal_cannot_complete_ok() {
        // (a) control: an in-budget item completes and publishes.
        let (r, rx) = rig(CallShape::ServerStreaming, 8, 8);
        let tx = r.tx.clone();
        let handler_state = Arc::clone(&r.state);
        let handler_gate = Arc::clone(&r.gate);
        let sup = tokio::spawn(run_supervisor(
            Arc::clone(&r.state),
            async move {
                sink_send(&handler_state, &handler_gate, &tx, 8, 8)
                    .await
                    .expect("in-budget item is admitted");
                HandlerResult::Ok
            },
            rx,
            Arc::clone(&r.gate),
            r.pump(),
            Arc::clone(&r.retire),
            None,
            Arc::clone(&r.published),
            r.ctl.clone(),
        ));
        drop(r.tx);
        let control = tokio::time::timeout(MODEL_BOUND, sup)
            .await
            .expect("control finishes")
            .expect("no panic");
        assert_eq!(
            control.terminal,
            TerminalReason::Completed(HandlerResult::Ok),
            "control leg: {control:?}",
        );
        assert_eq!(control.published, 1, "control leg: {control:?}");

        // (b) the refusal: one byte over the per-call budget.
        let (r, rx) = rig(CallShape::ServerStreaming, 8, 8);
        let tx = r.tx.clone();
        let handler_state = Arc::clone(&r.state);
        let handler_gate = Arc::clone(&r.gate);
        let sup = tokio::spawn(run_supervisor(
            Arc::clone(&r.state),
            async move {
                assert_eq!(
                    sink_send(&handler_state, &handler_gate, &tx, 8, 9).await,
                    Err(SinkClosed),
                    "an unsatisfiable item is refused promptly",
                );
                // The handler shrugs and reports success anyway.
                HandlerResult::Ok
            },
            rx,
            Arc::clone(&r.gate),
            r.pump(),
            Arc::clone(&r.retire),
            None,
            Arc::clone(&r.published),
            r.ctl.clone(),
        ));
        drop(r.tx);
        let out = tokio::time::timeout(MODEL_BOUND, sup)
            .await
            .expect("the refusal retires the call within the bound")
            .expect("no panic");
        assert_eq!(
            out.terminal,
            TerminalReason::ResourceExhausted,
            "the refusal latches; `Completed(Ok)` after a dropped item is the defect: {out:?}",
        );
        assert_eq!(out.published, 0, "nothing was admitted: {out:?}");
    }

    /// The plan's `terminal_queue_refusal_is_not_peer_receipt`
    /// composition check (§2.8). Leg (a) is the positive control: the
    /// reserved control slot takes the terminal and the RECEIVER observes
    /// it. Legs (b) and (c): the control queue is full / the session is
    /// gone — the disposition records interruption (`Refused` /
    /// `Unreachable`), ownership still completes within the bound, and
    /// the receiver never observes this call's terminal. A `try_send`
    /// attempt is not peer receipt.
    #[tokio::test(start_paused = true)]
    async fn terminal_queue_refusal_is_not_peer_receipt() {
        // (a) control: accepted by the control path, observed at the receiver.
        let (r, rx) = rig(CallShape::ServerStreaming, 8, 1024);
        let sup = tokio::spawn(run_supervisor(
            Arc::clone(&r.state),
            async { HandlerResult::Ok },
            rx,
            Arc::clone(&r.gate),
            r.pump(),
            Arc::clone(&r.retire),
            None,
            Arc::clone(&r.published),
            r.ctl.clone(),
        ));
        drop(r.tx);
        let mut ctl_rx = r.ctl_rx;
        let control = tokio::time::timeout(MODEL_BOUND, sup)
            .await
            .expect("control finishes")
            .expect("no panic");
        assert_eq!(
            control.emission,
            Some(TerminalDisposition::Queued),
            "control leg: {control:?}",
        );
        assert_eq!(
            ctl_rx.try_recv().ok(),
            Some(control.terminal.clone()),
            "receipt is attributed at the receiver: {control:?}",
        );

        // (b) the reserved slot is occupied: refusal is interruption.
        let (r, rx) = rig(CallShape::ServerStreaming, 8, 1024);
        r.ctl
            .try_send(TerminalReason::Cancelled)
            .expect("the control path is full");
        let sup = tokio::spawn(run_supervisor(
            Arc::clone(&r.state),
            async { HandlerResult::Ok },
            rx,
            Arc::clone(&r.gate),
            r.pump(),
            Arc::clone(&r.retire),
            None,
            Arc::clone(&r.published),
            r.ctl.clone(),
        ));
        drop(r.tx);
        let mut ctl_rx = r.ctl_rx;
        let out = tokio::time::timeout(MODEL_BOUND, sup)
            .await
            .expect("a refused terminal still completes ownership")
            .expect("no panic");
        assert_eq!(
            out.emission,
            Some(TerminalDisposition::Refused),
            "a full control queue is interruption, not receipt: {out:?}",
        );
        assert_eq!(
            ctl_rx.try_recv().ok(),
            Some(TerminalReason::Cancelled),
            "the occupant, not this call's terminal: {out:?}",
        );
        assert!(
            ctl_rx.try_recv().is_err(),
            "this call's terminal never reached the peer: {out:?}",
        );

        // (c) the session is gone: unreachable is interruption too.
        let (r, rx) = rig(CallShape::ServerStreaming, 8, 1024);
        let sup = tokio::spawn(run_supervisor(
            Arc::clone(&r.state),
            async { HandlerResult::Ok },
            rx,
            Arc::clone(&r.gate),
            r.pump(),
            Arc::clone(&r.retire),
            None,
            Arc::clone(&r.published),
            r.ctl.clone(),
        ));
        drop(r.tx);
        drop(r.ctl_rx);
        let out = tokio::time::timeout(MODEL_BOUND, sup)
            .await
            .expect("cleanup completes without a route")
            .expect("no panic");
        assert_eq!(
            out.emission,
            Some(TerminalDisposition::Unreachable),
            "a gone session is interruption, not synthetic success: {out:?}",
        );
        assert_eq!(
            out.terminal,
            TerminalReason::Completed(HandlerResult::Ok),
            "{out:?}"
        );
    }

    // ------- the runtime-free driver (leaf/browser parity) -------

    mod runtime_free {
        use super::super::pull::{Progress, PullCall};
        use super::*;

        #[test]
        fn a_blocked_drain_is_not_terminal_and_a_grant_completes_it_without_any_runtime() {
            // The same obligation as the tokio witness, driven with no
            // executor, no task and no semaphore — the shape the leaf
            // (which has no runtime at all) must be able to implement.
            let mut call = PullCall::new(CallShape::ServerStreaming, 0, 1024, None);
            call.submit(8).expect("admitted");
            call.submit(8).expect("admitted");
            call.handler_returned(HandlerResult::Ok);

            assert_eq!(call.advance(0), Progress::Blocked);
            assert!(call.state().is_live(), "producer finished is not terminal");
            assert_eq!(call.published(), 0);

            call.grant(2);
            assert_eq!(call.advance(0), Progress::Published);
            assert_eq!(call.advance(0), Progress::Published);
            assert_eq!(
                call.advance(0),
                Progress::Terminal(TerminalReason::Completed(HandlerResult::Ok)),
            );
            assert_eq!(call.published(), 2);
            assert_eq!(call.advance(0), Progress::Done);
        }

        #[test]
        fn the_deadline_preempts_a_blocked_drain_and_discards_the_remainder() {
            let mut call = PullCall::new(
                CallShape::ServerStreaming,
                0,
                1024,
                Some((100, TerminalReason::Timeout)),
            );
            call.submit(8).expect("admitted");
            call.handler_returned(HandlerResult::Ok);
            assert_eq!(call.advance(99), Progress::Blocked);
            assert_eq!(
                call.advance(100),
                Progress::Terminal(TerminalReason::Timeout)
            );
            assert_eq!(call.published(), 0);
            assert_eq!(call.queued(), 0, "queued items are discarded");
        }

        #[test]
        fn retirement_preempts_a_blocked_drain() {
            let mut call = PullCall::new(CallShape::Duplex, 0, 1024, None);
            call.submit(8).expect("admitted");
            call.handler_returned(HandlerResult::Ok);
            assert!(call.retire(TerminalReason::Revoked));
            assert_eq!(call.advance(0), Progress::Terminal(TerminalReason::Revoked));
            assert_eq!(call.published(), 0);
        }

        #[test]
        fn a_retained_sink_cannot_submit_after_the_producer_finished() {
            let mut call = PullCall::new(CallShape::ServerStreaming, 8, 1024, None);
            call.submit(4).expect("admitted");
            call.handler_returned(HandlerResult::Ok);
            assert_eq!(call.submit(4), Err(SinkClosed));
            assert_eq!(
                call.run_to_terminal(0, 8),
                Some(TerminalReason::Completed(HandlerResult::Ok)),
            );
            assert_eq!(call.published(), 1);
            assert_eq!(
                call.control().cloned(),
                Some(TerminalReason::Completed(HandlerResult::Ok)),
                "the reserved control slot delivered the terminal (§2.8)",
            );
        }

        #[test]
        fn an_item_larger_than_the_budget_is_refused_instead_of_queued() {
            let mut call = PullCall::new(CallShape::ClientStreaming, 8, 1024, None);
            assert_eq!(call.submit(4 * 1024 * 1024), Err(SinkClosed));
            assert_eq!(call.queued(), 0, "it never waits for permits it cannot get");
            // The refusal is not a metric-only drop: it latches
            // `ResourceExhausted` and retires the call (§2.7 response
            // direction), so no later step can complete it `Ok`.
            assert_eq!(
                call.run_to_terminal(0, 8),
                Some(TerminalReason::ResourceExhausted),
            );
        }

        #[test]
        fn byte_reservations_are_released_exactly_once_on_publish_and_on_discard() {
            let mut call = PullCall::new(CallShape::ServerStreaming, 1, 16, None);
            call.submit(8).expect("admitted");
            call.submit(8).expect("admitted");
            // Budget is now exhausted: a third item cannot be admitted.
            assert_eq!(call.submit(8), Err(SinkClosed));
            // One publish frees exactly one item's bytes.
            assert_eq!(call.advance(0), Progress::Published);
            call.submit(8).expect("freed by the publish");
            assert_eq!(call.submit(1), Err(SinkClosed), "no double release");
        }

        #[test]
        fn a_handler_error_is_the_terminal_here_too() {
            let mut call = PullCall::new(CallShape::ClientStreaming, 4, 64, None);
            call.handler_returned(HandlerResult::Err(0x8001, "nope".into()));
            assert_eq!(
                call.run_to_terminal(0, 4),
                Some(TerminalReason::Completed(HandlerResult::Err(
                    0x8001,
                    "nope".into()
                ))),
            );
        }
    }
}
