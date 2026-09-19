//! Application-facing stream API over `NetSession`.
//!
//! A `Stream` is a typed handle to one logical channel in an encrypted
//! session to a single peer. Multiple streams share one session, one
//! Noise cipher, and one UDP socket — but have **independent**:
//!
//! - Sequence numbers (per-stream `tx_seq` / `rx_seq`).
//! - Reliability mode (`FireAndForget` or `Reliable`), chosen at `open_stream`.
//! - Fairness weight in the forwarding router's `FairScheduler`.
//! - Statistics.
//!
//! # Contract
//!
//! - **Ordering within a stream:** FIFO for `Reliable` streams (per-stream
//!   sequence + NACK-driven in-order delivery), best-effort monotonic-seq
//!   for `FireAndForget`.
//! - **No ordering across streams.** Fair scheduling prevents starvation
//!   but timing is not synchronized.
//! - **Stream IDs are opaque `u64`s.** No range has reserved meaning at
//!   the transport layer. Callers derive IDs however they want —
//!   `stream_id_from_key(&str)` is the canonical helper for a
//!   deterministic derivation from a name.
//! - **Not multicast.** A stream is one logical flow to one peer. Sending
//!   the same content to multiple peers is an app / daemon / channel
//!   concern that sits a layer above the transport.

use std::fmt;

/// Reliability mode chosen per stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reliability {
    /// Send packets and forget. No retransmission, no ordering recovery,
    /// no ACK/NACK tracking. Monotonic sequence numbers on the wire so
    /// callers that care can detect gaps / reorder themselves.
    FireAndForget,
    /// Retransmit lost packets (receiver NACKs + a sender timeout
    /// backstop; the retransmit window is sized to the tx-window so
    /// nothing in flight is unrecoverable). Guarantees **gap-free
    /// eventual delivery** of every byte.
    ///
    /// **Ordering contract (H-8):** the substrate does NOT reorder for
    /// you. Accepted packets — including out-of-order arrivals and
    /// retransmits — are delivered to the inbound queue in **arrival
    /// order**, each tagged with its monotonic `seq`. A consumer that
    /// needs strict in-order bytes must reassemble by `seq` itself (the
    /// blob-transfer engine's per-stream reorder buffer is the reference
    /// example). Consumers that frame their own ordering (nRPC streaming
    /// keys on `EventMeta`/`call_id`) or tolerate reordering need do
    /// nothing. Reliable here means "no loss", not "delivered in order".
    Reliable,
}

impl Reliability {
    /// Whether this mode needs reliability state tracking.
    #[inline]
    pub fn is_reliable(self) -> bool {
        matches!(self, Reliability::Reliable)
    }
}

/// What to do with pending outbound packets when a stream is closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseBehavior {
    /// Wait for the stream's pending outbound packets to leave the
    /// scheduler before tearing down state. "Durable close."
    DrainThenClose,
    /// Drop pending outbound packets immediately. "Fast close."
    DropAndClose,
}

/// Default initial credit window for newly-opened streams, in
/// **on-wire bytes** (Net header + AEAD tag + payload). 64 KB is
/// a reasonable starting point for LAN / typical mesh deployments;
/// each packet costs ~80 B of fixed overhead plus its payload, so
/// the window comfortably fits hundreds of small packets or a few
/// MTU-sized ones in flight. Callers who want the v1-style
/// "unbounded" escape hatch can explicitly set
/// `with_window_bytes(0)`.
pub const DEFAULT_STREAM_WINDOW_BYTES: u32 = 65_536;

/// Per-stream configuration supplied at `open_stream` time.
///
/// Configuration is immutable for the lifetime of a stream. Re-opening
/// the same `(peer, stream_id)` with different config is a no-op with
/// a warning log — the original config wins.
#[derive(Debug, Clone, Copy)]
pub struct StreamConfig {
    /// Reliability mode. Defaults to `FireAndForget`.
    pub reliability: Reliability,
    /// Initial credit window for the stream's send path, in bytes.
    /// The sender starts with `tx_credit_remaining = window_bytes` and
    /// decrements on each socket send; receiver-driven `StreamWindow`
    /// grants replenish the counter. `0` disables backpressure
    /// (unbounded — v1 escape hatch). Defaults to
    /// [`DEFAULT_STREAM_WINDOW_BYTES`].
    pub window_bytes: u32,
    /// Fair-scheduler quantum multiplier. `1` is equal-share; higher
    /// means this stream gets proportionally more packets per round.
    pub fairness_weight: u8,
    /// Route this stream's *originating* sends through the router's
    /// `FairScheduler` (in the core's `adapter::net::router`)
    /// rather than straight to the socket. Default `false` — every
    /// existing caller (nRPC streaming, control traffic) keeps the
    /// direct `socket.send_to` path. Set `true` for bulk transfers so
    /// their sends participate in per-stream weighted fairness and
    /// can't monopolize the link against other scheduled streams. The
    /// scheduler's queue depth becomes an additional backpressure
    /// source (surfaced as [`StreamError::Backpressure`]).
    pub scheduled: bool,
    /// What to do with pending outbound packets on close.
    pub close_behavior: CloseBehavior,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            reliability: Reliability::FireAndForget,
            window_bytes: DEFAULT_STREAM_WINDOW_BYTES,
            fairness_weight: 1,
            scheduled: false,
            close_behavior: CloseBehavior::DropAndClose,
        }
    }
}

impl StreamConfig {
    /// Start from defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the reliability mode.
    pub fn with_reliability(mut self, reliability: Reliability) -> Self {
        self.reliability = reliability;
        self
    }

    /// Set the per-stream window (queue depth cap).
    pub fn with_window_bytes(mut self, bytes: u32) -> Self {
        self.window_bytes = bytes;
        self
    }

    /// Set the fair-scheduler weight (1 = equal share; higher = more).
    pub fn with_fairness_weight(mut self, weight: u8) -> Self {
        // 0 would starve this stream; clamp up to 1.
        self.fairness_weight = weight.max(1);
        self
    }

    /// Route this stream's originating sends through the router's
    /// fair scheduler (see [`Self::scheduled`]). Use for bulk transfers.
    pub fn with_scheduled(mut self, scheduled: bool) -> Self {
        self.scheduled = scheduled;
        self
    }

    /// Set the close behavior.
    pub fn with_close_behavior(mut self, behavior: CloseBehavior) -> Self {
        self.close_behavior = behavior;
        self
    }
}

/// Errors a `Stream::send` call can surface to the caller.
///
/// `#[non_exhaustive]`: a meaningful new failure mode must be able to
/// arrive without breaking every downstream `match`. That is not a
/// licence to collapse the cases — handle the ones you know
/// explicitly and PROPAGATE the rest. A wildcard arm is not
/// permission to retry: [`Self::SessionSuperseded`] in particular can
/// never succeed on the same handle, and treating an unrecognised
/// variant as "retryable" turns a terminal condition into a loop.
#[derive(Debug)]
#[non_exhaustive]
pub enum StreamError {
    /// The stream's outbound queue is full. No packets were enqueued.
    /// Caller decides whether to retry, drop, or surface further.
    Backpressure,
    /// The underlying session is gone (peer disconnected, never
    /// connected, or the stream was closed).
    NotConnected,
    /// The handle belongs to a session that has since been replaced.
    ///
    /// Distinct from [`Self::NotConnected`] on purpose. `NotConnected`
    /// says "this stream id is not open on the session you addressed";
    /// this says "the session you addressed is no longer the peer's
    /// session at all" — a successor incarnation is installed, and the
    /// stream id may well be open *on it*. The stream epoch cannot
    /// tell the two apart: it restarts at 1 for every session, so a
    /// successor's first stream carries the same epoch as the
    /// predecessor's first stream (see
    /// `NetSession::next_stream_epoch`). Only the session incarnation
    /// recorded on the handle can.
    ///
    /// A caller that sees this must re-open against the current
    /// session; retrying the same handle can never succeed.
    SessionSuperseded,
    /// Underlying transport failure (socket error, encryption error).
    /// Wraps the originating adapter-level error's message.
    Transport(String),
    /// One event in the batch is larger than a single packet can
    /// carry, so no send path could ever deliver it. Nothing was
    /// enqueued — the refusal happens before any packet of the call
    /// reaches the wire.
    ///
    /// Names the limit on purpose. The bound is
    /// [`MAX_EVENT_SIZE`](crate::protocol::MAX_EVENT_SIZE) —
    /// `MAX_PAYLOAD_SIZE` minus the event frame's 4-byte length
    /// prefix — and a caller that cannot see it in the error has no
    /// way to discover it except by losing data. Retrying is
    /// pointless; the payload has to be split by the application, or
    /// carried by a producer that fragments (the browser leaf's
    /// `frame` module is the only one that does).
    EventTooLarge {
        /// The offending event's length in bytes.
        size: usize,
        /// The largest event this transport can carry, in bytes.
        limit: usize,
    },
}

impl fmt::Display for StreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StreamError::Backpressure => write!(f, "stream would block (queue full)"),
            StreamError::NotConnected => write!(f, "stream not connected"),
            StreamError::SessionSuperseded => {
                write!(f, "stream's session has been replaced by a successor")
            }
            StreamError::Transport(msg) => write!(f, "stream transport error: {}", msg),
            StreamError::EventTooLarge { size, limit } => write!(
                f,
                "stream event of {} bytes exceeds the {}-byte per-event limit \
                 (MAX_EVENT_SIZE); nothing was sent — split the payload at the \
                 application layer",
                size, limit
            ),
        }
    }
}

impl std::error::Error for StreamError {}

/// Per-stream statistics snapshot. Cheap to produce (reads a handful of
/// atomics) and safe to poll at arbitrary frequency.
///
/// `#[non_exhaustive]`: this is an evolving DIAGNOSTIC snapshot, and
/// adding a counter should not break downstream construction or
/// exhaustive destructuring every time. Fields stay public and
/// readable; only literal construction from outside the crate is
/// closed, and [`StreamStats::empty`] is the stable way in.
#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub struct StreamStats {
    /// Next TX sequence number. Reflects "how many packets this stream
    /// has enqueued since open" because sequences start at 0.
    pub tx_seq: u64,
    /// Highest RX sequence number observed so far.
    pub rx_seq: u64,
    /// Events currently buffered on the inbound queue (waiting for the
    /// caller to poll).
    pub inbound_pending: u64,
    /// Nanoseconds since Unix epoch of the last inbound or outbound
    /// activity. Used internally for idle eviction; surfaced for
    /// diagnostics.
    pub last_activity_ns: u64,
    /// Whether the stream is active (not closed).
    pub active: bool,
    /// Cumulative count of `send_on_stream` calls that returned
    /// `StreamError::Backpressure` because the stream ran out of
    /// credit. Monotonically increasing; reset only by close + reopen.
    pub backpressure_events: u64,
    /// Bytes of send credit still available. For bounded streams
    /// (`tx_window > 0`), `0` means the next send of any size will
    /// be rejected as Backpressure; near `tx_window` means plenty of
    /// headroom. Receiver-driven `StreamWindow` grants replenish
    /// this counter.
    pub tx_credit_remaining: u32,
    /// Configured initial credit window in bytes. Informational —
    /// `0` disables backpressure entirely on this stream.
    pub tx_window: u32,
    /// Cumulative `StreamWindow` grants received from the peer since
    /// the stream opened (sender side).
    pub credit_grants_received: u64,
    /// Cumulative `StreamWindow` grants emitted to the peer since the
    /// stream opened (receiver side).
    pub credit_grants_sent: u64,
    /// Cumulative wire bytes this side has committed to the wire on
    /// this stream — the sender-side half of the byte ledger. Moved
    /// both by credit admission (`try_acquire_tx_credit`) and by the
    /// control-plane debit (`note_tx_bytes_sent`).
    ///
    /// Not by *every* producer: the event-batch producers that
    /// predate the byte ledger put packets on a stream without
    /// debiting it, so a stream shared between one of those and an
    /// admitted producer still sees the receiver's total run ahead of
    /// this watermark. Byte conservation below is a property of the
    /// admitted and control-debited producers, not of the stream map.
    pub tx_bytes_sent: u64,
    /// Highest cumulative-consumed total the receiver has reported
    /// for this stream — the receiver-side half of the same ledger.
    ///
    /// Conservation, over the producers that move `tx_bytes_sent`:
    /// `tx_credit_remaining + (tx_bytes_sent - max_consumed_seen)
    /// == tx_window + overdraft` at every settled point, and
    /// `tx_bytes_sent - max_consumed_seen` is exactly the bytes in
    /// flight or lost.
    ///
    /// The `overdraft` term is control debt. A control-plane producer
    /// has no caller to refuse, so it debits unconditionally and
    /// `tx_credit_remaining` floors at zero; the part of the debit
    /// the window could not pay for is carried as debt rather than
    /// forgiven. It is retired by the next authoritative grant —
    /// which pays debt down before it reopens application credit —
    /// or given back if the transport never admitted the send.
    /// Without the term, a grant reporting only a control frame's own
    /// bytes as consumed reopened the window for application bytes
    /// still outstanding. `overdraft` is internal to `StreamState`;
    /// it is the difference between this identity and the one a
    /// stream with no control traffic satisfies.
    ///
    /// A producer that skips its debit breaks the identity by letting
    /// the receiver's total run past `tx_bytes_sent`, whereupon the
    /// grant clamp refunds window for data that never arrived.
    pub max_consumed_seen: u64,
}

impl StreamStats {
    /// An all-zero snapshot, for building a synthetic one field by
    /// field.
    ///
    /// Deliberately NOT a constructor taking every counter: that
    /// merely relocates the compatibility break to the next counter
    /// added, which is the problem `#[non_exhaustive]` is here to
    /// solve. Assign the fields you mean and leave the rest at zero.
    ///
    /// This is for tests, fixtures and adapters that must present a
    /// stats-shaped value. LIVE statistics come from the stream —
    /// `MeshNode::stream_stats` / `all_stream_stats` — never from a
    /// value assembled by hand.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }
}

// The `Stream` **handle** deliberately does NOT live here.
//
// It is an application-facing handle whose fields (`epoch`, `config`)
// must stay private: `MeshNode::send_on_stream` reads the flags off
// the handle while the retransmit bookkeeping reads the live
// `StreamState`, so a writable `config` lets an application put
// `wire_reliable=true` on the wire with no retransmit entry retained.
// A crate boundary cannot express "private outside the core", so the
// handle stays in the core (`adapter::net::stream_handle`) and only
// the config/error/stats vocabulary lives here.
