//! Reassembly of leaf fragments at the RTC ingress.
//!
//! # Why the native side has to do this
//!
//! A browser leaf cannot put more than [`MAX_PAYLOAD_SIZE`] bytes in
//! one Net packet — `NetHeader::validate` refuses it on arrival — so
//! it fragments, stamping `fragment_id`, `fragment_offset` and
//! `frag_flags` on each piece. Those three fields have always been
//! on the wire and have always been authenticated by
//! `NetHeader::aad()`, but nothing in the core ever *read*
//! `frag_flags`. A native node therefore saw a fragmented leaf
//! message as several independent events: a subscriber got partial
//! application payloads, and an nRPC or enrollment frame whose first
//! piece declares a body longer than it carries failed to decode and
//! timed out.
//!
//! That is not a bulk-transfer-only gap. The enrollment writer
//! accepts request bodies up to `MAX_ENROLL_BODY_BYTES`, which
//! exceeds one packet before nRPC framing is added, so the very
//! first thing a leaf says to an anchor can need this.
//!
//! # What is bounded, and by what
//!
//! Nothing here trusts the peer. A group is refused unless its
//! pieces are disjoint, all inside the declared total, and together
//! cover `[0, total)` exactly. Concurrency and memory are bounded
//! **per session**: at most [`MAX_GROUPS_PER_SESSION`] open groups,
//! and at most [`MAX_PROVISIONAL_STREAM_BYTES`] of held bytes — the
//! same whole-session byte budget `ProvisionalBudget` already
//! enforces on receive-stream allocation, reused rather than
//! re-invented so one number governs how much unenrolled receive
//! state a session can pin. A group that sits incomplete past
//! [`GROUP_TTL`] is reaped — on every piece, not only when some
//! unrelated new group needs a slot.
//!
//! # One session, one guard (X9)
//!
//! Every fact about a session's reassembly — whether it is retired,
//! which groups it holds, which groups it has abandoned — lives in
//! ONE [`SessionState`] behind ONE map entry. Retirement publishes
//! its marker and releases the groups under that entry's write
//! guard; ingress checks the marker, the deadline, the abandonment
//! fence and the capacity bound and then inserts, under the same
//! guard. The two operations are therefore serialized on the
//! session, which is what the previous shape — groups in one map,
//! retirement markers in another — could not do: a packet that had
//! already passed its session lookup could check an empty marker
//! map and insert a group after the retirement had swept it
//! (cleanup-before-marker), or pause between its marker check and
//! its insert and resurrect a group after the marker landed
//! (check-before-insert). Neither schedule exists now: there is no
//! window between the check and the insert to interleave with.
//!
//! The tombstone horizon is still bounded — [`GROUP_TTL`] per
//! session and [`MAX_RETIRED_SESSIONS`] sessions, because the mesh
//! outlives every session in it and the map must not grow with the
//! process. Capacity eviction takes an **expired** marker first and
//! only falls back to the oldest live one, so the bound costs a
//! fence only when more than [`MAX_RETIRED_SESSIONS`] sessions end
//! inside one horizon. Leaf fragment ids restart at 1 on every new
//! session, so a reconnect never inherits a predecessor's partial
//! group either way.
//!
//! # Every group is owned (X11)
//!
//! A piece reaches this reassembler only after the ingress has
//! recorded its sequence and returned its credit — that is
//! deliberate (the bytes did cross the wire) but it means every
//! buffered piece is a piece this node has **acknowledged**. So a
//! group that is destroyed for any reason other than completion has
//! lost acknowledged data, and saying nothing about it is the silent
//! abandonment P1 names. Two rules make that impossible:
//!
//! * Every destruction produces an [`AbandonedGroup`] record naming
//!   the group's stream, origin, channel, subprotocol and first
//!   sequence — counted and logged at the point of destruction, on
//!   every path, and queued for the ingress and diagnostics to
//!   inspect. A group either completes or is disposed of, never
//!   neither.
//! * The destroyed group's id is **fenced** for [`GROUP_TTL`], so a
//!   tail arriving after its head was reaped is refused with
//!   [`FragmentOutcome::Abandoned`] instead of opening a headless
//!   group that could never complete.
//!
//! Group identity is fixed by its first piece
//! ([`FragmentProvenance`]): stream, origin, channel, subprotocol
//! and reliability. A later piece that disagrees is not merged — it
//! is [`FragmentOutcome::Inconsistent`] and takes the group with it.
//! That is what lets the completing packet's context be used for
//! dispatch: with mismatches refused, it is provably the group's own
//! context rather than whichever piece happened to finish it.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use bytes::{Bytes, BytesMut};
use dashmap::DashMap;
use parking_lot::Mutex;

use net_wire::protocol::{NetHeader, FRAG_FRAGMENTED, FRAG_LAST, MAX_PAYLOAD_SIZE};

use super::MAX_PROVISIONAL_STREAM_BYTES;

/// Concurrent incomplete groups one session may hold.
///
/// The leaf's own fragmentation ceiling is eight pieces per group;
/// eight groups is the same bound its receive side applies, so an
/// honest peer never meets this and a hostile one cannot open
/// unbounded state.
pub const MAX_GROUPS_PER_SESSION: usize = 8;

/// Pieces one group may hold. The leaf refuses to emit more, so a
/// group claiming more is malformed.
pub const MAX_PIECES_PER_GROUP: usize = 8;

/// Largest payload a group may reassemble to: eight full packets'
/// worth, which is the leaf's `MAX_FRAGMENTED_PAYLOAD`.
pub const MAX_REASSEMBLED_BYTES: usize = MAX_PAYLOAD_SIZE * MAX_PIECES_PER_GROUP;

/// How long an incomplete group may sit before it is reaped, and how
/// long a retirement marker or an abandonment fence is kept.
pub const GROUP_TTL: Duration = Duration::from_secs(2);

/// Sessions whose retirement is remembered at once.
///
/// The marker only has to outlive the packets that were already
/// in flight past their session lookup when the retirement ran, so
/// it expires with [`GROUP_TTL`]. Sixty-four is far above the
/// number of sessions that can end inside one such window, and past
/// it an expired marker is dropped before any live one.
pub const MAX_RETIRED_SESSIONS: usize = 64;

/// Abandoned group ids one session fences at once. A group cannot be
/// abandoned without having held a slot, so the live fence can never
/// need more than the slot bound.
pub const MAX_ABANDONED_GROUPS_PER_SESSION: usize = MAX_GROUPS_PER_SESSION;

/// Abandonment records held for the ingress to drain.
///
/// The totals and the log line are unconditional; this queue is the
/// drainable DETAIL, and it only has to absorb the burst one
/// retirement can produce. Past it the OLDEST record gives way and
/// [`RtcReassembly::abandoned_dropped`] counts that.
pub const MAX_ABANDONMENT_RECORDS: usize = 64;

/// Why a piece did not become a payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FragmentOutcome {
    /// Buffered; the group is not complete yet.
    Buffered,
    /// Byte-identical to a piece the group already holds, at the
    /// same offset, making the same final-piece claim: the
    /// legitimate retransmission a sender builds whenever an
    /// acknowledgement is lost. The group keeps what it has —
    /// treating this as a contradiction and discarding the group is
    /// how one lost ack destroyed a whole message.
    Duplicate,
    /// The group contradicted itself and was discarded.
    Malformed,
    /// The piece disagreed with its group's bound identity — a
    /// different stream, origin, channel, subprotocol or
    /// reliability. The group is discarded rather than mixed.
    Inconsistent,
    /// The session is already holding as much partial state as it
    /// may.
    Refused,
    /// The piece's session has been retired. Its state is gone on
    /// purpose and this packet does not get to bring it back.
    Retired,
    /// The piece's group was abandoned — reaped, refused or
    /// contradicted — and its disposition already reported. A late
    /// piece does not get to open a headless successor.
    Abandoned,
}

/// Everything every piece of one group must agree on.
///
/// Bound by the group's first piece and checked against every later
/// one. Without it, two disjoint pieces that merely share a session
/// and a `fragment_id` — a different stream, a different channel, a
/// different subprotocol — combined into one payload and were
/// dispatched with whichever piece's context happened to complete
/// the group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FragmentProvenance {
    /// The stream the group belongs to, and the stream whose
    /// abandonment is reported when it is destroyed.
    pub stream_id: u64,
    /// Full 64-bit origin identity hash.
    pub origin_hash: u64,
    /// Wire channel-name hint.
    pub channel_hash: u16,
    /// Subprotocol the reassembled payload is dispatched as.
    pub subprotocol_id: u16,
    /// Whether the sender asked for acknowledgement.
    pub reliable: bool,
}

impl FragmentProvenance {
    /// The provenance an arriving packet declares.
    #[inline]
    pub fn from_header(header: &NetHeader) -> Self {
        Self {
            stream_id: header.stream_id,
            origin_hash: header.origin_hash,
            channel_hash: header.channel_hash,
            subprotocol_id: header.subprotocol_id,
            reliable: header.flags.is_reliable(),
        }
    }
}

/// One inbound piece, as the ingress saw it.
#[derive(Debug, Clone)]
pub struct FragmentPiece {
    /// The wire session the ingress RESOLVED this packet onto — not
    /// whatever the header claims, so reassembly state can never be
    /// keyed by a session the packet merely named.
    pub session_id: u64,
    /// Group id within that session.
    pub fragment_id: u16,
    /// Byte offset of this piece inside the reassembled payload.
    pub offset: u16,
    /// Raw `frag_flags`.
    pub flags: u8,
    /// The piece's own stream sequence — the sequence this node has
    /// acknowledged for it.
    pub sequence: u64,
    /// The group identity this piece claims.
    pub provenance: FragmentProvenance,
    /// The piece's bytes.
    pub data: Bytes,
}

impl FragmentPiece {
    /// The piece an arriving packet carries, on the session the
    /// ingress resolved it onto.
    pub fn from_header(session_id: u64, header: &NetHeader, data: Bytes) -> Self {
        Self {
            session_id,
            fragment_id: header.fragment_id,
            offset: header.fragment_offset,
            flags: header.frag_flags,
            sequence: header.sequence,
            provenance: FragmentProvenance::from_header(header),
            data,
        }
    }
}

/// A completed group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assembled {
    /// The reassembled payload.
    pub payload: Bytes,
    /// The identity bound by the group's FIRST piece — not the
    /// finishing packet's, which is only provably the same because a
    /// disagreeing piece is refused.
    pub provenance: FragmentProvenance,
    /// The lowest sequence the group's pieces carried.
    pub first_sequence: u64,
}

/// Why a group was destroyed without completing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbandonReason {
    /// It sat past [`GROUP_TTL`] without progress.
    Expired,
    /// The session's byte budget could not admit the next piece.
    Refused,
    /// A piece disagreed with the group's bound identity.
    Inconsistent,
    /// The group contradicted itself.
    Malformed,
    /// The session it belonged to ended.
    SessionRetired,
}

impl AbandonReason {
    /// The stable name this disposition is reported under. Shared
    /// verbatim with the leaf's `StreamFailure::ReassemblyAbandoned`
    /// so both sides of the same event read identically.
    #[inline]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Expired => "reassembly_abandoned.expired",
            Self::Refused => "reassembly_abandoned.refused",
            Self::Inconsistent => "reassembly_abandoned.inconsistent",
            Self::Malformed => "reassembly_abandoned.malformed",
            Self::SessionRetired => "reassembly_abandoned.session_retired",
        }
    }
}

/// The terminal disposition of a group that held acknowledged bytes
/// and will never complete.
///
/// Every piece this reassembler buffers was acknowledged by the
/// ingress before it arrived here, so a destroyed group is
/// acknowledged data that is not going to be delivered. This record
/// is what makes that an event rather than a silence: it names the
/// stream it happened to, so the loss is attributable to the
/// conversation that lost it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbandonedGroup {
    /// The session the group belonged to.
    pub session_id: u64,
    /// The group id, now fenced for [`GROUP_TTL`].
    pub fragment_id: u16,
    /// The identity bound by the group's first piece.
    pub provenance: FragmentProvenance,
    /// The lowest sequence the group's pieces carried.
    pub first_sequence: u64,
    /// The highest sequence the group's pieces carried — the
    /// acknowledged range this abandonment covers.
    pub last_sequence: u64,
    /// How many pieces were held.
    pub pieces: usize,
    /// How many acknowledged bytes were held.
    pub held: usize,
    /// Why it was destroyed.
    pub reason: AbandonReason,
}

/// One piece of a group, and what it claimed.
#[derive(Debug)]
struct Piece {
    offset: u16,
    /// Kept so a second piece at the same offset with the same bytes
    /// but a different final-piece claim is a contradiction rather
    /// than a duplicate.
    last: bool,
    sequence: u64,
    data: Bytes,
}

/// One partially reassembled group.
#[derive(Debug)]
struct Partial {
    /// Bound by the first piece; every later piece must match.
    provenance: FragmentProvenance,
    /// `(offset, bytes)`, at most [`MAX_PIECES_PER_GROUP`].
    pieces: Vec<Piece>,
    /// Known once the `FRAG_LAST` piece arrives.
    total: Option<usize>,
    held: usize,
    /// When the group last accepted a piece.
    ///
    /// Last progress rather than opening: a sender whose retransmit
    /// timer is longer than [`GROUP_TTL`] would otherwise have its
    /// group reaped between two pieces it is still legitimately
    /// resending, and the piece that then arrives is refused as
    /// abandoned. The lifetime stays bounded because a group accepts
    /// at most [`MAX_PIECES_PER_GROUP`] pieces.
    touched: Instant,
}

impl Partial {
    fn open(provenance: FragmentProvenance, now: Instant) -> Self {
        Self {
            provenance,
            pieces: Vec::with_capacity(MAX_PIECES_PER_GROUP),
            total: None,
            held: 0,
            touched: now,
        }
    }

    fn sequence_span(&self) -> (u64, u64) {
        let mut lo = u64::MAX;
        let mut hi = 0u64;
        for piece in &self.pieces {
            lo = lo.min(piece.sequence);
            hi = hi.max(piece.sequence);
        }
        if self.pieces.is_empty() {
            (0, 0)
        } else {
            (lo, hi)
        }
    }

    fn abandoned(
        &self,
        session_id: u64,
        fragment_id: u16,
        reason: AbandonReason,
    ) -> AbandonedGroup {
        let (first_sequence, last_sequence) = self.sequence_span();
        AbandonedGroup {
            session_id,
            fragment_id,
            provenance: self.provenance,
            first_sequence,
            last_sequence,
            pieces: self.pieces.len(),
            held: self.held,
            reason,
        }
    }
}

/// Everything one session's reassembly consists of, behind one map
/// entry so retirement and ingress serialize on it (X9).
#[derive(Debug, Default)]
struct SessionState {
    /// `Some(at)` once the session has ended: its groups are gone
    /// and nothing may open another.
    retired: Option<Instant>,
    /// Open groups, at most [`MAX_GROUPS_PER_SESSION`] — a linear
    /// scan over eight entries beats a map's hashing, and the whole
    /// state is already behind one guard.
    groups: Vec<(u16, Partial)>,
    /// Group ids destroyed within the last [`GROUP_TTL`], so a late
    /// piece cannot open a headless successor.
    abandoned: Vec<(u16, Instant)>,
}

impl SessionState {
    fn held(&self) -> u64 {
        self.groups.iter().map(|(_, p)| p.held as u64).sum()
    }

    fn is_empty(&self) -> bool {
        self.retired.is_none() && self.groups.is_empty() && self.abandoned.is_empty()
    }

    /// Reap this session's stale groups, markers and fences.
    ///
    /// A reaped group held acknowledged bytes, so it is abandoned
    /// rather than dropped: the record goes to `out` and the id is
    /// fenced.
    fn expire(&mut self, session_id: u64, now: Instant, out: &mut Vec<AbandonedGroup>) {
        if self
            .retired
            .is_some_and(|at| now.saturating_duration_since(at) >= GROUP_TTL)
        {
            self.retired = None;
        }
        self.abandoned
            .retain(|(_, at)| now.saturating_duration_since(*at) < GROUP_TTL);
        if self.groups.is_empty() {
            return;
        }
        let mut reaped: Vec<(u16, Partial)> = Vec::new();
        self.groups.retain_mut(|(id, partial)| {
            if now.saturating_duration_since(partial.touched) < GROUP_TTL {
                return true;
            }
            reaped.push((
                *id,
                Partial {
                    provenance: partial.provenance,
                    pieces: std::mem::take(&mut partial.pieces),
                    total: partial.total,
                    held: partial.held,
                    touched: partial.touched,
                },
            ));
            false
        });
        for (id, partial) in reaped {
            out.push(partial.abandoned(session_id, id, AbandonReason::Expired));
            self.fence(id, now);
        }
    }

    /// Fence a destroyed group's id for [`GROUP_TTL`].
    fn fence(&mut self, fragment_id: u16, now: Instant) {
        if let Some(slot) = self.abandoned.iter_mut().find(|(id, _)| *id == fragment_id) {
            slot.1 = now;
            return;
        }
        if self.abandoned.len() >= MAX_ABANDONED_GROUPS_PER_SESSION {
            // Oldest first: the newest fence is the one a straggler
            // is most likely to still be racing.
            let oldest = self
                .abandoned
                .iter()
                .enumerate()
                .min_by_key(|(_, (_, at))| *at)
                .map(|(i, _)| i);
            if let Some(i) = oldest {
                self.abandoned.swap_remove(i);
            }
        }
        self.abandoned.push((fragment_id, now));
    }

    /// Destroy group `slot` and account for it.
    fn abandon(
        &mut self,
        slot: usize,
        session_id: u64,
        now: Instant,
        reason: AbandonReason,
        out: &mut Vec<AbandonedGroup>,
    ) {
        let (fragment_id, partial) = self.groups.swap_remove(slot);
        out.push(partial.abandoned(session_id, fragment_id, reason));
        self.fence(fragment_id, now);
    }

    /// Release every group of `stream_id`'s receive lifetime and
    /// fence their ids, returning how many were released.
    ///
    /// **NR6.** A RESET ends that stream's receive lifetime: the
    /// peer's send half gave up and may restart from sequence zero,
    /// so the receive cursor and the reliability ranges are dropped.
    /// A partial group retained from before the reset is not part of
    /// the lifetime that follows it — leave it and an old delayed
    /// tail completes it afterwards, and a payload from before the
    /// reset is dispatched against the fresh cursor. Session
    /// retirement does not cover this: the session is still live.
    ///
    /// Nothing is reported as abandoned, exactly as the leaf's
    /// `Reassembler::retire_stream` reports nothing: the reset **is**
    /// that stream's terminal disposition and its owner has already
    /// been told. Reporting again would ask the ingress to reset a
    /// stream because it was reset.
    fn retire_stream_groups(&mut self, stream_id: u64, now: Instant) -> usize {
        let mut fence = Vec::new();
        self.groups.retain(|(id, partial)| {
            if partial.provenance.stream_id != stream_id {
                return true;
            }
            fence.push(*id);
            false
        });
        for id in &fence {
            self.fence(*id, now);
        }
        fence.len()
    }
}

/// A pause the ingress takes **inside** a session's guard, between
/// the retirement check and the insert (test/fixture builds only).
///
/// The X9 hazard is a gap between those two operations, so the only
/// way to witness that there is no gap is to stop the ingress there
/// and observe that a retirement cannot get past it. A hook that
/// fires outside the guard would prove nothing: the interval it
/// names is precisely the one under the lock.
#[cfg(any(test, feature = "fixtures"))]
#[derive(Clone)]
pub struct IngressPause(std::sync::Arc<dyn Fn() + Send + Sync>);

#[cfg(any(test, feature = "fixtures"))]
impl IngressPause {
    /// Run `f` once per accepted piece, under the session's guard.
    pub fn new<F: Fn() + Send + Sync + 'static>(f: F) -> Self {
        Self(std::sync::Arc::new(f))
    }
}

#[cfg(any(test, feature = "fixtures"))]
impl std::fmt::Debug for IngressPause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("IngressPause")
    }
}

/// Per-session leaf-fragment reassembly for the RTC ingress.
#[derive(Debug, Default)]
pub struct RtcReassembly {
    sessions: DashMap<u64, SessionState>,
    /// The DIAGNOSTIC record of what was lost: drained by operators
    /// and by witnesses, and deliberately independent of whether the
    /// ingress has carried the loss into the conversation yet.
    abandoned: Mutex<VecDeque<AbandonedGroup>>,
    /// The PRODUCTION queue: terminal dispositions the RTC ingress
    /// has not yet turned into a receive-half reset on the stream
    /// that lost the bytes (NR2). Consumed exactly once, by
    /// `MeshNode::dispose_abandoned_rtc_groups`, so one destroyed
    /// group produces one terminal — which is why it cannot be the
    /// same queue a diagnostic reader drains.
    terminals: Mutex<VecDeque<AbandonedGroup>>,
    abandoned_total: AtomicU64,
    abandoned_bytes_total: AtomicU64,
    abandoned_dropped: AtomicU64,
    /// Groups released because their stream's receive lifetime ended
    /// in a RESET. Counted rather than reported: the reset is the
    /// terminal (NR6).
    reset_retired_total: AtomicU64,
    #[cfg(any(test, feature = "fixtures"))]
    pause: Mutex<Option<IngressPause>>,
    /// The seam the NR3 schedule needs, which
    /// [`RtcReassembly::pause`] cannot provide: it fires in the
    /// ingress BEFORE the admission decision, where a resolved
    /// session handle has been captured and the packet has not yet
    /// reached reassembly. That is the interval a retirement's
    /// bounded marker used to have to outlive.
    #[cfg(any(test, feature = "fixtures"))]
    dispatch_pause: Mutex<Option<IngressPause>>,
}

impl RtcReassembly {
    /// An empty reassembler.
    pub fn new() -> Self {
        Self::default()
    }

    /// Install (or clear) the ingress pause: a fixtures-only seam
    /// that fires INSIDE the session guard, between the admission
    /// decision and the write, so a retirement racing an in-flight
    /// accept can be scheduled deterministically instead of hoped
    /// for.
    #[cfg(any(test, feature = "fixtures"))]
    pub fn set_ingress_pause(&self, pause: Option<IngressPause>) {
        *self.pause.lock() = pause;
    }

    /// Install (or clear) the DISPATCH pause: a fixtures-only seam
    /// the RTC ingress runs before it asks whether the resolved
    /// session is still live, so the NR3 schedule — a frame captured
    /// under an incarnation that is retired while the frame waits —
    /// is scheduled rather than hoped for. No lock is held while it
    /// runs.
    #[cfg(any(test, feature = "fixtures"))]
    pub fn set_dispatch_pause(&self, pause: Option<IngressPause>) {
        *self.dispatch_pause.lock() = pause;
    }

    /// Run the dispatch pause, if one is installed. Cloned out from
    /// under its lock first: the hook blocks by design, and blocking
    /// with the lock held would serialize the retirement this seam
    /// exists to let through.
    #[cfg(any(test, feature = "fixtures"))]
    pub fn run_dispatch_pause(&self) {
        let hook = self.dispatch_pause.lock().clone();
        if let Some(hook) = hook {
            (hook.0)();
        }
    }

    /// How many groups are open. Observable so a test asserts the
    /// bound rather than inferring it.
    pub fn outstanding(&self) -> usize {
        self.sessions.iter().map(|e| e.value().groups.len()).sum()
    }

    /// Bytes currently held for `session_id`.
    pub fn held_bytes(&self, session_id: u64) -> u64 {
        self.sessions
            .get(&session_id)
            .map(|e| e.value().held())
            .unwrap_or(0)
    }

    /// Groups abandoned since this reassembler was created — the
    /// monotone total, which no drain and no queue bound can lose.
    pub fn abandoned_total(&self) -> u64 {
        self.abandoned_total.load(Ordering::Relaxed)
    }

    /// Acknowledged bytes those groups were holding when they were
    /// destroyed — the size of the loss, not just its count.
    pub fn abandoned_bytes_total(&self) -> u64 {
        self.abandoned_bytes_total.load(Ordering::Relaxed)
    }

    /// Abandonment records that fell off the drainable queue because
    /// nothing drained it in time. Their totals and log lines were
    /// still emitted.
    pub fn abandoned_dropped(&self) -> u64 {
        self.abandoned_dropped.load(Ordering::Relaxed)
    }

    /// Take the terminal dispositions produced since the last drain.
    pub fn take_abandoned(&self) -> Vec<AbandonedGroup> {
        let mut queue = self.abandoned.lock();
        queue.drain(..).collect()
    }

    /// Take the terminal dispositions the ingress owes the
    /// conversation (NR2).
    ///
    /// Separate from [`Self::take_abandoned`] on purpose: the
    /// diagnostic drain must not be able to swallow a production
    /// terminal, and a production drain must not erase the record an
    /// operator is about to read.
    pub fn take_terminals(&self) -> Vec<AbandonedGroup> {
        let mut queue = self.terminals.lock();
        queue.drain(..).collect()
    }

    /// Groups released because a RESET ended their stream's receive
    /// lifetime. The reset itself is the terminal, so these are
    /// counted rather than reported (NR6).
    pub fn reset_retired_total(&self) -> u64 {
        self.reset_retired_total.load(Ordering::Relaxed)
    }

    /// Release every group belonging to `session_id`, and fence it.
    ///
    /// The map outlives any one session, so a session that ends has
    /// to say so: otherwise its incomplete groups hold their bytes
    /// for as long as the mesh lives, and only the TTL — which
    /// nothing guarantees will be reached while the process is
    /// quiet — ever releases them.
    ///
    /// The fence is the other half. Ingress clones the session Arc
    /// before the packet reaches reassembly, so a packet that was
    /// already past that lookup when this ran would otherwise insert
    /// a fresh group under the very key just released. For
    /// [`GROUP_TTL`] after retirement, pieces for `session_id` are
    /// refused with [`FragmentOutcome::Retired`] instead.
    ///
    /// **X9:** the marker is published and the groups released under
    /// ONE entry guard, which is the same guard [`Self::accept`]
    /// takes to decide and insert. There is no interleaving in which
    /// an admitted packet observes "not retired" and then inserts
    /// into a swept session.
    ///
    /// **NR3:** the marker is a *bounded* fence — it expires with
    /// [`GROUP_TTL`] and gives way to [`MAX_RETIRED_SESSIONS`]
    /// churn — so it cannot be the whole retirement authority. The
    /// authority is the session handle itself: every caller of this
    /// function deactivates the `NetSession` it is retiring, and the
    /// ingress refuses a frame whose session is no longer active
    /// before it ever reaches here. That flag is captured with the
    /// frame, is one-way, and expires with nothing. The marker
    /// remains as the cheap in-window refusal.
    pub fn retire_session(&self, session_id: u64, now: Instant) {
        let mut abandoned = Vec::new();
        {
            let mut entry = self.sessions.entry(session_id).or_default();
            let state = entry.value_mut();
            // Marker FIRST, release SECOND, one guard around both.
            state.retired = Some(now);
            let released = std::mem::take(&mut state.groups);
            state.abandoned.clear();
            for (fragment_id, partial) in released {
                abandoned.push(partial.abandoned(
                    session_id,
                    fragment_id,
                    AbandonReason::SessionRetired,
                ));
            }
        }
        self.record_abandoned(abandoned);
        self.bound_retired(now);
    }

    /// Release every group of one stream's receive lifetime on
    /// `session_id`, and fence their ids (NR6).
    ///
    /// The session survives — only this stream's receive half ended.
    /// See [`SessionState::retire_stream_groups`] for why the
    /// released groups are counted rather than reported.
    pub fn retire_stream(&self, session_id: u64, stream_id: u64, now: Instant) {
        let released = {
            let Some(mut entry) = self.sessions.get_mut(&session_id) else {
                return;
            };
            entry.value_mut().retire_stream_groups(stream_id, now)
        };
        if released > 0 {
            self.reset_retired_total
                .fetch_add(released as u64, Ordering::Relaxed);
            tracing::debug!(
                session_id,
                stream_id = format!("{stream_id:#x}"),
                released,
                "rtc: reset retired the stream's partial fragment groups"
            );
        }
    }

    /// Retire EVERY session's reassembly (NR3, shutdown).
    ///
    /// A shut-down node's reassembly map is mesh-owned, so nothing
    /// else releases it: the close notifications that would have
    /// retired each session are produced by the driver's teardown,
    /// which happens while this node's consumers are being joined,
    /// and a retained `MeshNode` therefore kept every partial group
    /// it had buffered. Shutdown owns those bytes and says so here
    /// instead of depending on a notification arriving in time.
    ///
    /// Returns how many groups were released.
    pub fn retire_all(&self, now: Instant) -> usize {
        let mut abandoned = Vec::new();
        for mut entry in self.sessions.iter_mut() {
            let session_id = *entry.key();
            let state = entry.value_mut();
            state.retired = Some(now);
            let released = std::mem::take(&mut state.groups);
            state.abandoned.clear();
            for (fragment_id, partial) in released {
                abandoned.push(partial.abandoned(
                    session_id,
                    fragment_id,
                    AbandonReason::SessionRetired,
                ));
            }
        }
        let released = abandoned.len();
        self.record_abandoned(abandoned);
        released
    }

    /// Offer one inbound piece.
    ///
    /// `Ok(Some(assembled))` when it completed its group, `Ok(None)`
    /// when the packet was not a fragment at all (the caller keeps
    /// its event as-is), and `Err(outcome)` when the piece was
    /// buffered, duplicated, refused, retired, fenced or discarded.
    pub fn accept(
        &self,
        piece: FragmentPiece,
        now: Instant,
    ) -> Result<Option<Assembled>, FragmentOutcome> {
        if piece.flags & FRAG_FRAGMENTED == 0 {
            return Ok(None);
        }
        let end = piece.offset as usize + piece.data.len();
        if end > MAX_REASSEMBLED_BYTES {
            return Err(FragmentOutcome::Malformed);
        }
        let last = piece.flags & FRAG_LAST != 0;

        // Age every session's groups out on every piece, not only
        // when a new group wants a slot. A deadline applied when
        // unrelated traffic happens to arrive is not a deadline.
        // This runs BEFORE the entry guard below, never under it: it
        // walks the whole map, and walking a map while holding one of
        // its entries is how a shard deadlocks against itself.
        self.expire(now);

        // Cloned BEFORE the guard: reading it under the guard would
        // be a second lock taken while an entry is held.
        #[cfg(any(test, feature = "fixtures"))]
        let pause = self.pause.lock().clone();
        let mut abandoned = Vec::new();
        let outcome = {
            let mut entry = self.sessions.entry(piece.session_id).or_default();
            let state = entry.value_mut();
            let outcome = Self::accept_locked(
                state,
                &piece,
                end,
                last,
                now,
                &mut abandoned,
                #[cfg(any(test, feature = "fixtures"))]
                pause.as_ref(),
            );
            // An entry that holds nothing must not linger: the map
            // outlives every session in it.
            let empty = state.is_empty();
            drop(entry);
            if empty {
                self.sessions
                    .remove_if(&piece.session_id, |_, state| state.is_empty());
            }
            outcome
        };
        self.record_abandoned(abandoned);
        outcome
    }

    /// The whole decision, under this session's write guard.
    ///
    /// An associated function, not a method: it cannot reach `self`,
    /// so it cannot take a second lock on the map whose entry it is
    /// already holding.
    fn accept_locked(
        state: &mut SessionState,
        piece: &FragmentPiece,
        end: usize,
        last: bool,
        now: Instant,
        abandoned: &mut Vec<AbandonedGroup>,
        #[cfg(any(test, feature = "fixtures"))] pause: Option<&IngressPause>,
    ) -> Result<Option<Assembled>, FragmentOutcome> {
        let session_id = piece.session_id;
        if state.retired.is_some() {
            return Err(FragmentOutcome::Retired);
        }
        // The deadline is applied before the admission decision, so
        // a piece can never be admitted into a group that has just
        // aged out from under it.
        state.expire(session_id, now, abandoned);
        if state.retired.is_some() {
            return Err(FragmentOutcome::Retired);
        }
        if state
            .abandoned
            .iter()
            .any(|(id, _)| *id == piece.fragment_id)
        {
            return Err(FragmentOutcome::Abandoned);
        }
        // X9: the interval the hazard lives in. Everything above is
        // the "may this piece be admitted" decision and everything
        // below writes the group, so a schedule that resurrects
        // retired state has to interleave HERE. It cannot: this runs
        // with the session's write guard held, so a retirement
        // waiting to publish its marker is waiting on us.
        #[cfg(any(test, feature = "fixtures"))]
        if let Some(pause) = pause {
            (pause.0)();
        }

        let held = state.held();
        let slot = match state
            .groups
            .iter()
            .position(|(id, _)| *id == piece.fragment_id)
        {
            Some(slot) => {
                // X11: the group's identity is its first piece's.
                if state.groups[slot].1.provenance != piece.provenance {
                    state.abandon(
                        slot,
                        session_id,
                        now,
                        AbandonReason::Inconsistent,
                        abandoned,
                    );
                    return Err(FragmentOutcome::Inconsistent);
                }
                // The duplicate check comes before the budget because
                // a duplicate adds no bytes: charging it against the
                // session budget would let an ordinary retransmission
                // refuse the group it is trying to complete.
                //
                // NR6: the SEQUENCE is part of what makes it a
                // duplicate. A sender rebuilding a lost piece resends
                // it with the sequence it was stamped with, so the
                // legitimate retransmission still matches; a second
                // packet carrying the same bytes at the same offset
                // on a DIFFERENT sequence is a second acknowledged
                // sequence claiming one piece's place, which is
                // exactly what the span rule below forbids.
                if state.groups[slot].1.pieces.iter().any(|p| {
                    p.offset == piece.offset
                        && p.last == last
                        && p.sequence == piece.sequence
                        && p.data == piece.data
                }) {
                    return Err(FragmentOutcome::Duplicate);
                }
                if held.saturating_add(piece.data.len() as u64) > MAX_PROVISIONAL_STREAM_BYTES {
                    state.abandon(slot, session_id, now, AbandonReason::Refused, abandoned);
                    return Err(FragmentOutcome::Refused);
                }
                slot
            }
            None => {
                if state.groups.len() >= MAX_GROUPS_PER_SESSION
                    || held.saturating_add(piece.data.len() as u64) > MAX_PROVISIONAL_STREAM_BYTES
                {
                    // **NR2.** This piece's sequence was recorded and
                    // its credit returned before reassembly ever saw
                    // it, so refusing to open its group loses
                    // acknowledged bytes exactly as destroying a live
                    // group does — and pre-fix this branch produced
                    // neither a record nor a fence. The consequence
                    // was worse than the silence: once the capacity
                    // pressure eased, a TAIL of the refused group
                    // opened a fresh headless group that could never
                    // complete. The record names the loss and the
                    // fence refuses the rest of the group.
                    abandoned.push(AbandonedGroup {
                        session_id,
                        fragment_id: piece.fragment_id,
                        provenance: piece.provenance,
                        first_sequence: piece.sequence,
                        last_sequence: piece.sequence,
                        pieces: 1,
                        held: piece.data.len(),
                        reason: AbandonReason::Refused,
                    });
                    state.fence(piece.fragment_id, now);
                    return Err(FragmentOutcome::Refused);
                }
                state
                    .groups
                    .push((piece.fragment_id, Partial::open(piece.provenance, now)));
                state.groups.len() - 1
            }
        };

        let partial = &mut state.groups[slot].1;
        let contradicts = partial.pieces.iter().any(|p| {
            let (s, e) = (p.offset as usize, p.offset as usize + p.data.len());
            // Any overlap that is not the exact duplicate handled
            // above: a group is written once. NR6: or a sequence the
            // group already holds a piece for — one acknowledged
            // sequence carries one piece, and two pieces sharing a
            // sequence would let a group claim coverage it never
            // received.
            (s < end && (piece.offset as usize) < e) || p.sequence == piece.sequence
        }) || partial.pieces.len() >= MAX_PIECES_PER_GROUP
            || partial.total.is_some_and(|t| end > t || (last && end != t))
            // The declared end binds the pieces that arrived before
            // it, not only the ones that arrive after.
            || (last
                && partial
                    .pieces
                    .iter()
                    .any(|p| p.offset as usize + p.data.len() > end));
        if contradicts {
            state.abandon(slot, session_id, now, AbandonReason::Malformed, abandoned);
            return Err(FragmentOutcome::Malformed);
        }
        partial.held += piece.data.len();
        partial.pieces.push(Piece {
            offset: piece.offset,
            last,
            sequence: piece.sequence,
            data: piece.data.clone(),
        });
        partial.touched = now;
        if last {
            partial.total = Some(end);
        }
        if !partial.total.is_some_and(|t| partial.held == t) {
            return Err(FragmentOutcome::Buffered);
        }

        let (fragment_id, mut partial) = state.groups.swap_remove(slot);
        let total = partial.total.unwrap_or(0);
        partial.pieces.sort_unstable_by_key(|p| p.offset);
        // **NR6: sequence-to-offset provenance.** Byte coverage
        // alone cannot tell one message's pieces from two. A head on
        // sequence 0 and a tail on sequence 2 cover their bytes
        // exactly while the group silently claims sequence 1 — a
        // packet that belonged to whatever arrived in between and has
        // already advanced this stream's FIFO. The leaf's sender
        // allocates the pieces of one payload contiguous sequences in
        // offset order, synchronously, inside one `build_packets`
        // call, so that is the rule the receiver holds it to: sorted
        // by offset, the sequences ascend by exactly one. The span
        // needs no separate bound — it is the piece count, which
        // `MAX_PIECES_PER_GROUP` already caps.
        let contiguous = partial
            .pieces
            .windows(2)
            .all(|w| w[0].sequence.checked_add(1) == Some(w[1].sequence));
        if !contiguous {
            abandoned.push(partial.abandoned(session_id, fragment_id, AbandonReason::Malformed));
            state.fence(fragment_id, now);
            return Err(FragmentOutcome::Malformed);
        }
        // Prove the coverage rather than infer it from the byte
        // count: every piece must start exactly where the previous
        // one ended, from zero to the declared total.
        let mut covered = 0usize;
        let mut whole = BytesMut::with_capacity(total);
        for p in &partial.pieces {
            if p.offset as usize != covered {
                abandoned.push(partial.abandoned(
                    session_id,
                    fragment_id,
                    AbandonReason::Malformed,
                ));
                state.fence(fragment_id, now);
                return Err(FragmentOutcome::Malformed);
            }
            covered += p.data.len();
            whole.extend_from_slice(&p.data);
        }
        if covered != total {
            abandoned.push(partial.abandoned(session_id, fragment_id, AbandonReason::Malformed));
            state.fence(fragment_id, now);
            return Err(FragmentOutcome::Malformed);
        }
        let (first_sequence, _) = partial.sequence_span();
        Ok(Some(Assembled {
            payload: whole.freeze(),
            provenance: partial.provenance,
            first_sequence,
        }))
    }

    /// Reap groups whose last progress is older than [`GROUP_TTL`],
    /// retirement markers and abandonment fences past the same
    /// horizon, and the session entries left holding nothing.
    pub fn expire(&self, now: Instant) {
        if self.sessions.is_empty() {
            return;
        }
        let mut abandoned = Vec::new();
        self.sessions.retain(|session_id, state| {
            state.expire(*session_id, now, &mut abandoned);
            !state.is_empty()
        });
        self.record_abandoned(abandoned);
    }

    /// Keep the retirement fence bounded.
    ///
    /// An expired marker goes before any live one, so the bound
    /// costs a fence only when more than [`MAX_RETIRED_SESSIONS`]
    /// sessions end inside one [`GROUP_TTL`].
    fn bound_retired(&self, now: Instant) {
        if self.sessions.len() <= MAX_RETIRED_SESSIONS {
            return;
        }
        let mut markers: Vec<(u64, Instant)> = self
            .sessions
            .iter()
            .filter_map(|e| e.value().retired.map(|at| (*e.key(), at)))
            .collect();
        if markers.len() <= MAX_RETIRED_SESSIONS {
            return;
        }
        // Oldest first — and an expired marker is by definition
        // older than every live one, so this drops those first.
        markers.sort_unstable_by_key(|(_, at)| *at);
        let excess = markers.len() - MAX_RETIRED_SESSIONS;
        for (session_id, _) in markers.into_iter().take(excess) {
            self.sessions.remove_if(&session_id, |_, state| {
                state.retired.is_some() && state.groups.is_empty()
            });
        }
        let _ = now;
    }

    /// Report terminal dispositions: count them, log them, queue
    /// them for the ingress to carry into the conversation (NR2),
    /// and keep the diagnostic detail an operator can read.
    fn record_abandoned(&self, groups: Vec<AbandonedGroup>) {
        if groups.is_empty() {
            return;
        }
        self.abandoned_total
            .fetch_add(groups.len() as u64, Ordering::Relaxed);
        self.abandoned_bytes_total.fetch_add(
            groups.iter().map(|g| g.held as u64).sum::<u64>(),
            Ordering::Relaxed,
        );
        let mut queue = self.abandoned.lock();
        let mut terminals = self.terminals.lock();
        for group in groups {
            tracing::warn!(
                session_id = group.session_id,
                fragment_id = group.fragment_id,
                stream_id = group.provenance.stream_id,
                subprotocol_id = group.provenance.subprotocol_id,
                channel_hash = group.provenance.channel_hash,
                origin_hash = format!("{:#x}", group.provenance.origin_hash),
                reliable = group.provenance.reliable,
                first_sequence = group.first_sequence,
                last_sequence = group.last_sequence,
                pieces = group.pieces,
                held = group.held,
                reason = group.reason.as_str(),
                "rtc: reassembly abandoned acknowledged fragment bytes"
            );
            // The production queue bounds the same way, and for the
            // same reason: a consumer that stopped draining must not
            // be able to grow this map. The OLDEST terminal gives
            // way, because the newest loss is the one whose stream
            // is most likely still waiting on a disposition.
            if terminals.len() >= MAX_ABANDONMENT_RECORDS {
                terminals.pop_front();
            }
            terminals.push_back(group.clone());
            if queue.len() >= MAX_ABANDONMENT_RECORDS {
                queue.pop_front();
                self.abandoned_dropped.fetch_add(1, Ordering::Relaxed);
            }
            queue.push_back(group);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: u64 = 0xABCD;
    const STREAM: u64 = 0x5EE1;

    fn piece(n: usize) -> Bytes {
        Bytes::from(vec![0x5A; n])
    }

    fn provenance() -> FragmentProvenance {
        FragmentProvenance {
            stream_id: STREAM,
            origin_hash: 0x1111_2222_3333_4444,
            channel_hash: 0x77,
            subprotocol_id: 0,
            reliable: true,
        }
    }

    /// One piece, with the group identity every piece of a group
    /// shares, on an explicit stream sequence.
    fn part_seq(
        session_id: u64,
        fragment_id: u16,
        offset: u16,
        flags: u8,
        sequence: u64,
        data: Bytes,
    ) -> FragmentPiece {
        FragmentPiece {
            session_id,
            fragment_id,
            offset,
            flags,
            sequence,
            provenance: provenance(),
            data,
        }
    }

    /// The same, stamping the piece's own offset as its sequence.
    ///
    /// Honest for the single-piece cases and for the contradiction
    /// cases below, which never complete a group; a group that
    /// COMPLETES must use [`part_seq`], because a real sender
    /// allocates its pieces contiguous sequences rather than
    /// offset-shaped ones.
    fn part(
        session_id: u64,
        fragment_id: u16,
        offset: u16,
        flags: u8,
        data: Bytes,
    ) -> FragmentPiece {
        part_seq(session_id, fragment_id, offset, flags, offset as u64, data)
    }

    #[test]
    fn a_two_piece_group_reassembles_byte_exactly() {
        let r = RtcReassembly::new();
        let now = Instant::now();
        let head = Bytes::from_static(b"native ");
        let tail = Bytes::from_static(b"reassembly");
        assert_eq!(
            r.accept(
                part_seq(SESSION, 1, 0, FRAG_FRAGMENTED, 0, head.clone()),
                now
            ),
            Err(FragmentOutcome::Buffered)
        );
        let whole = r
            .accept(
                part_seq(
                    SESSION,
                    1,
                    head.len() as u16,
                    FRAG_FRAGMENTED | FRAG_LAST,
                    1,
                    tail,
                ),
                now,
            )
            .expect("completes")
            .expect("payload");
        assert_eq!(whole.payload.as_ref(), b"native reassembly");
        assert_eq!(
            whole.provenance,
            provenance(),
            "the payload carries the group's bound identity"
        );
        assert_eq!(whole.first_sequence, 0);
        assert_eq!(r.outstanding(), 0, "a completed group frees its slot");
        assert_eq!(r.abandoned_total(), 0, "completion is not abandonment");
    }

    #[test]
    fn an_unfragmented_packet_is_not_touched() {
        let r = RtcReassembly::new();
        assert_eq!(
            r.accept(part(SESSION, 0, 0, 0, piece(8)), Instant::now()),
            Ok(None)
        );
        assert_eq!(r.outstanding(), 0);
    }

    /// The coverage rule, which a held-byte count alone does not
    /// prove: a hole at the start plus bytes past the declared end
    /// can sum to exactly the total.
    #[test]
    fn a_group_with_a_missing_prefix_and_data_past_its_end_is_refused() {
        let r = RtcReassembly::new();
        let now = Instant::now();
        assert_eq!(
            r.accept(
                part(
                    SESSION,
                    1,
                    10,
                    FRAG_FRAGMENTED,
                    Bytes::from_static(b"later")
                ),
                now
            ),
            Err(FragmentOutcome::Buffered)
        );
        assert_eq!(
            r.accept(
                part(
                    SESSION,
                    1,
                    5,
                    FRAG_FRAGMENTED | FRAG_LAST,
                    Bytes::from_static(b"early")
                ),
                now
            ),
            Err(FragmentOutcome::Malformed)
        );
        assert_eq!(r.outstanding(), 0, "a contradicted group is discarded");
    }

    #[test]
    fn overlapping_pieces_are_refused() {
        let r = RtcReassembly::new();
        let now = Instant::now();
        let _ = r.accept(part(SESSION, 2, 0, FRAG_FRAGMENTED, piece(16)), now);
        assert_eq!(
            r.accept(part(SESSION, 2, 8, FRAG_FRAGMENTED, piece(16)), now),
            Err(FragmentOutcome::Malformed)
        );
    }

    #[test]
    fn a_session_cannot_hold_more_than_the_byte_budget() {
        let r = RtcReassembly::new();
        let now = Instant::now();
        // Each group holds one near-full packet and never completes.
        let chunk = MAX_PAYLOAD_SIZE - 1;
        let mut opened = 0u64;
        for id in 1..=MAX_GROUPS_PER_SESSION as u16 {
            match r.accept(part(SESSION, id, 0, FRAG_FRAGMENTED, piece(chunk)), now) {
                Err(FragmentOutcome::Buffered) => opened += 1,
                Err(FragmentOutcome::Refused) => break,
                other => panic!("unexpected {other:?}"),
            }
        }
        assert!(
            r.held_bytes(SESSION) <= MAX_PROVISIONAL_STREAM_BYTES,
            "held {} bytes past the session budget",
            r.held_bytes(SESSION)
        );
        assert!(opened > 0, "the budget must admit real traffic");
        assert_eq!(
            r.accept(part(SESSION, 99, 0, FRAG_FRAGMENTED, piece(chunk)), now),
            Err(FragmentOutcome::Refused),
            "past the budget every new group is refused"
        );
    }

    /// A reconnect cannot inherit a predecessor's partial group:
    /// the key carries the wire session id, which a new handshake
    /// never reproduces, and the leftover reaps on its TTL.
    #[test]
    fn two_sessions_with_the_same_fragment_id_do_not_mix() {
        let r = RtcReassembly::new();
        let now = Instant::now();
        assert_eq!(
            r.accept(
                part(SESSION, 1, 0, FRAG_FRAGMENTED, Bytes::from_static(b"old-")),
                now
            ),
            Err(FragmentOutcome::Buffered)
        );
        assert_eq!(
            r.accept(
                part(
                    SESSION + 1,
                    1,
                    0,
                    FRAG_FRAGMENTED,
                    Bytes::from_static(b"new-")
                ),
                now
            ),
            Err(FragmentOutcome::Buffered),
            "the successor opens its OWN group, it does not join the old one"
        );
        let whole = r
            .accept(
                part_seq(
                    SESSION + 1,
                    1,
                    4,
                    FRAG_FRAGMENTED | FRAG_LAST,
                    1,
                    Bytes::from_static(b"tail"),
                ),
                now,
            )
            .expect("completes")
            .expect("payload");
        assert_eq!(
            whole.payload.as_ref(),
            b"new-tail",
            "the successor's payload must not contain the predecessor's bytes"
        );
        assert_eq!(r.held_bytes(SESSION), 4, "the old partial is still its own");
    }

    #[test]
    fn an_incomplete_group_is_reaped_after_its_ttl() {
        let r = RtcReassembly::new();
        let now = Instant::now();
        let _ = r.accept(part(SESSION, 1, 0, FRAG_FRAGMENTED, piece(64)), now);
        assert_eq!(r.outstanding(), 1);
        r.expire(now + GROUP_TTL + Duration::from_millis(1));
        assert_eq!(r.outstanding(), 0);
    }

    /// The retransmission a sender builds whenever an ack is lost:
    /// the same piece arrives twice, and the retained head has to
    /// survive it so the tail can still complete the message.
    #[test]
    fn an_exact_duplicate_piece_leaves_the_group_intact() {
        let r = RtcReassembly::new();
        let now = Instant::now();
        let head = Bytes::from_static(b"native ");
        assert_eq!(
            r.accept(part(SESSION, 1, 0, FRAG_FRAGMENTED, head.clone()), now),
            Err(FragmentOutcome::Buffered)
        );
        assert_eq!(
            r.accept(part(SESSION, 1, 0, FRAG_FRAGMENTED, head.clone()), now),
            Err(FragmentOutcome::Duplicate),
            "a repeated piece is recovery, not a contradiction"
        );
        assert_eq!(r.held_bytes(SESSION), head.len() as u64, "nothing doubled");
        let whole = r
            .accept(
                part_seq(
                    SESSION,
                    1,
                    head.len() as u16,
                    FRAG_FRAGMENTED | FRAG_LAST,
                    1,
                    Bytes::from_static(b"reassembly"),
                ),
                now,
            )
            .expect("completes after the duplicate")
            .expect("payload");
        assert_eq!(whole.payload.as_ref(), b"native reassembly");
    }

    /// Same offset, same length, different bytes: not a
    /// retransmission of anything.
    #[test]
    fn a_conflicting_piece_at_a_held_offset_is_malformed() {
        let r = RtcReassembly::new();
        let now = Instant::now();
        let _ = r.accept(part(SESSION, 1, 0, FRAG_FRAGMENTED, piece(16)), now);
        assert_eq!(
            r.accept(
                part(
                    SESSION,
                    1,
                    0,
                    FRAG_FRAGMENTED,
                    Bytes::from(vec![0xFFu8; 16])
                ),
                now
            ),
            Err(FragmentOutcome::Malformed)
        );
        assert_eq!(r.outstanding(), 0);
    }

    /// Same offset, same bytes, and now the FINAL piece: the two
    /// claims cannot both be true, so this is not the duplicate a
    /// lost ack produces.
    #[test]
    fn a_repeat_that_changes_the_final_piece_claim_is_malformed() {
        let r = RtcReassembly::new();
        let now = Instant::now();
        let head = Bytes::from_static(b"head");
        assert_eq!(
            r.accept(part(SESSION, 1, 0, FRAG_FRAGMENTED, head.clone()), now),
            Err(FragmentOutcome::Buffered)
        );
        assert_eq!(
            r.accept(
                part(SESSION, 1, 0, FRAG_FRAGMENTED | FRAG_LAST, head.clone()),
                now
            ),
            Err(FragmentOutcome::Malformed),
            "a piece that redefines the group's length is a contradiction"
        );
        assert_eq!(r.outstanding(), 0);
    }

    /// X11: two disjoint pieces that share a session and a group id
    /// but belong to different streams are not one message.
    #[test]
    fn a_piece_from_another_stream_cannot_join_the_group() {
        let r = RtcReassembly::new();
        let now = Instant::now();
        assert_eq!(
            r.accept(
                part(SESSION, 1, 0, FRAG_FRAGMENTED, Bytes::from_static(b"head")),
                now
            ),
            Err(FragmentOutcome::Buffered)
        );
        let mut alien = part(
            SESSION,
            1,
            4,
            FRAG_FRAGMENTED | FRAG_LAST,
            Bytes::from_static(b"tail"),
        );
        alien.provenance.stream_id = STREAM + 1;
        assert_eq!(
            r.accept(alien, now),
            Err(FragmentOutcome::Inconsistent),
            "a different stream's piece must not be merged in"
        );
        assert_eq!(r.outstanding(), 0, "the mixed group is destroyed, not kept");
        let records = r.take_abandoned();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].reason, AbandonReason::Inconsistent);
        assert_eq!(
            records[0].provenance.stream_id, STREAM,
            "the disposition names the stream that OWNED the group"
        );
    }

    /// A different channel, same stream: still not one message.
    #[test]
    fn a_piece_from_another_channel_cannot_join_the_group() {
        let r = RtcReassembly::new();
        let now = Instant::now();
        let _ = r.accept(part(SESSION, 3, 0, FRAG_FRAGMENTED, piece(8)), now);
        let mut alien = part(SESSION, 3, 8, FRAG_FRAGMENTED | FRAG_LAST, piece(8));
        alien.provenance.channel_hash = 0x99;
        assert_eq!(r.accept(alien, now), Err(FragmentOutcome::Inconsistent));
        assert_eq!(r.outstanding(), 0);
    }

    /// A session that ends releases its bytes, and a packet that was
    /// already in flight past its session lookup cannot bring them
    /// back. The successor is untouched by either.
    #[test]
    fn retiring_a_session_releases_its_groups_and_fences_late_packets() {
        let r = RtcReassembly::new();
        let now = Instant::now();
        let _ = r.accept(part(SESSION, 1, 0, FRAG_FRAGMENTED, piece(64)), now);
        let _ = r.accept(part(SESSION + 1, 1, 0, FRAG_FRAGMENTED, piece(32)), now);

        r.retire_session(SESSION, now);
        assert_eq!(r.held_bytes(SESSION), 0, "the old session held nothing now");
        assert_eq!(
            r.held_bytes(SESSION + 1),
            32,
            "retirement is exact: the successor keeps its own group"
        );
        let records = r.take_abandoned();
        assert_eq!(
            records.len(),
            1,
            "the released group is disposed of, not dropped"
        );
        assert_eq!(records[0].reason, AbandonReason::SessionRetired);
        assert_eq!(records[0].held, 64);

        assert_eq!(
            r.accept(part(SESSION, 2, 0, FRAG_FRAGMENTED, piece(64)), now),
            Err(FragmentOutcome::Retired),
            "an already-admitted packet must not recreate retired state"
        );
        assert_eq!(r.held_bytes(SESSION), 0);

        // Past the horizon the marker is gone; nothing of the old
        // session survives it either way.
        let later = now + GROUP_TTL + Duration::from_millis(1);
        assert_eq!(
            r.accept(part(SESSION, 2, 0, FRAG_FRAGMENTED, piece(64)), later),
            Err(FragmentOutcome::Buffered)
        );
    }

    /// X11/P1: the deadline is a deadline, AND the head it reaped was
    /// acknowledged — so the tail cannot open a headless group and
    /// the loss is reported against the stream that suffered it.
    #[test]
    fn a_tail_past_the_ttl_is_refused_and_its_group_reported_abandoned() {
        let r = RtcReassembly::new();
        let now = Instant::now();
        assert_eq!(
            r.accept(
                part(SESSION, 1, 0, FRAG_FRAGMENTED, Bytes::from_static(b"head")),
                now
            ),
            Err(FragmentOutcome::Buffered)
        );
        let late = now + GROUP_TTL + Duration::from_millis(1);
        assert_eq!(
            r.accept(
                part(
                    SESSION,
                    1,
                    4,
                    FRAG_FRAGMENTED | FRAG_LAST,
                    Bytes::from_static(b"tail")
                ),
                late
            ),
            Err(FragmentOutcome::Abandoned),
            "the head was reaped, so the tail must not open a group that \
             can never complete"
        );
        assert_eq!(
            r.held_bytes(SESSION),
            0,
            "and it holds nothing: no payload was assembled and none will be"
        );
        assert_eq!(r.abandoned_total(), 1);
        let records = r.take_abandoned();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].reason, AbandonReason::Expired);
        assert_eq!(records[0].provenance.stream_id, STREAM);
        assert_eq!(records[0].held, 4);
    }

    /// The fence is bounded, like everything else here: past the
    /// horizon the group id is usable again.
    #[test]
    fn an_abandonment_fence_expires_with_the_group_ttl() {
        let r = RtcReassembly::new();
        let now = Instant::now();
        let _ = r.accept(part(SESSION, 1, 0, FRAG_FRAGMENTED, piece(16)), now);
        let late = now + GROUP_TTL + Duration::from_millis(1);
        assert_eq!(
            r.accept(part(SESSION, 1, 16, FRAG_FRAGMENTED, piece(16)), late),
            Err(FragmentOutcome::Abandoned)
        );
        let much_later = late + GROUP_TTL + Duration::from_millis(1);
        assert_eq!(
            r.accept(part(SESSION, 1, 0, FRAG_FRAGMENTED, piece(16)), much_later),
            Err(FragmentOutcome::Buffered),
            "the fence is a bounded horizon, not a permanent ban"
        );
    }

    /// Capacity refusal destroys a group holding acknowledged bytes,
    /// so it reports the same terminal disposition expiry does.
    #[test]
    fn a_capacity_refusal_reports_the_group_it_destroyed() {
        let r = RtcReassembly::new();
        let now = Instant::now();
        let chunk = MAX_PAYLOAD_SIZE - 1;
        let mut last_open = 0u16;
        for id in 1..=MAX_GROUPS_PER_SESSION as u16 {
            match r.accept(part(SESSION, id, 0, FRAG_FRAGMENTED, piece(chunk)), now) {
                Err(FragmentOutcome::Buffered) => last_open = id,
                Err(FragmentOutcome::Refused) => break,
                other => panic!("unexpected {other:?}"),
            }
        }
        assert!(last_open > 0, "the budget must admit real traffic");
        let _ = r.take_abandoned();
        // A second piece for an OPEN group, with the session's byte
        // budget exhausted: the group cannot grow and cannot ever
        // complete, so it is abandoned rather than silently dropped.
        assert_eq!(
            r.accept(
                part(
                    SESSION,
                    last_open,
                    chunk as u16,
                    FRAG_FRAGMENTED,
                    piece(chunk)
                ),
                now
            ),
            Err(FragmentOutcome::Refused)
        );
        let records = r.take_abandoned();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].reason, AbandonReason::Refused);
        assert_eq!(records[0].fragment_id, last_open);
    }

    /// X9: retirement and ingress are serialized on the session, so
    /// the interleaving that resurrected retired state does not
    /// exist.
    ///
    /// This is Kyra's schedule, made deterministic instead of raced:
    /// hold an already-admitted ingress at the point *after* it has
    /// decided the session is live and *before* it writes its group,
    /// run the retirement on another thread, then release it. The
    /// two facts that follow are the whole property:
    ///
    /// * while the ingress is held, the retirement **cannot
    ///   complete** — it is waiting on the same guard;
    /// * when both have finished, the session holds **nothing**,
    ///   whichever order they ran in.
    ///
    /// Inverse: take the retirement check and the insert under two
    /// separate entry guards (release between them, which is what
    /// the two-map shape did). The retirement then completes while
    /// the ingress is held, the first assertion goes red, and the
    /// resumed ingress re-creates a group under a retired session so
    /// the second goes red too.
    #[test]
    fn a_held_ingress_and_a_retirement_cannot_interleave() {
        use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
        use std::sync::mpsc;
        use std::sync::Arc;

        let r = Arc::new(RtcReassembly::new());
        // `entered` says the ingress is inside the guarded interval;
        // `release` lets it out.
        let (entered_tx, entered_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let release_rx = Mutex::new(release_rx);
        r.set_ingress_pause(Some(IngressPause::new(move || {
            entered_tx.send(()).expect("the test is waiting for this");
            release_rx
                .lock()
                .recv()
                .expect("the test releases the ingress");
        })));

        let ingress = {
            let r = Arc::clone(&r);
            std::thread::spawn(move || {
                r.accept(
                    part(SESSION, 1, 0, FRAG_FRAGMENTED, piece(32)),
                    Instant::now(),
                )
            })
        };
        entered_rx
            .recv()
            .expect("the ingress must reach the guarded interval");

        let retired = Arc::new(AtomicBool::new(false));
        let retirement = {
            let r = Arc::clone(&r);
            let retired = Arc::clone(&retired);
            std::thread::spawn(move || {
                r.retire_session(SESSION, Instant::now());
                retired.store(true, AtomicOrdering::Release);
            })
        };
        // Long enough that a retirement which is NOT serialized
        // against this ingress would have finished many times over.
        std::thread::sleep(Duration::from_millis(150));
        assert!(
            !retired.load(AtomicOrdering::Acquire),
            "the retirement completed while an ingress was inside the \
             check-then-insert interval: the two are not serialized, so a \
             packet admitted against a live session can land in a swept one"
        );

        release_tx.send(()).expect("release the ingress");
        let outcome = ingress.join().expect("ingress thread");
        retirement.join().expect("retirement thread");
        assert_eq!(
            outcome,
            Err(FragmentOutcome::Buffered),
            "the ingress ran first and against a live session, so its piece \
             was legitimately buffered"
        );
        assert_eq!(
            r.held_bytes(SESSION),
            0,
            "and the retirement that followed released it: a retired \
             session holds nothing, whichever order the two ran in"
        );
        let released = r.take_abandoned();
        assert_eq!(released.len(), 1, "and said so");
        assert_eq!(released[0].reason, AbandonReason::SessionRetired);
    }

    /// **NR6.** Two pieces that cover their bytes exactly but arrived
    /// on sequences 0 and 2 are not one message: the group would
    /// silently claim sequence 1, which belonged to whatever arrived
    /// between them and has already advanced this stream's FIFO.
    ///
    /// Inverse: delete the `contiguous` check in `accept_locked` —
    /// the payload assembles and is dispatched as if the sender had
    /// sent it in one run.
    #[test]
    fn a_group_cannot_claim_a_sequence_none_of_its_pieces_arrived_on() {
        let r = RtcReassembly::new();
        let now = Instant::now();
        assert_eq!(
            r.accept(
                part_seq(
                    SESSION,
                    1,
                    0,
                    FRAG_FRAGMENTED,
                    0,
                    Bytes::from_static(b"head")
                ),
                now
            ),
            Err(FragmentOutcome::Buffered)
        );
        assert_eq!(
            r.accept(
                part_seq(
                    SESSION,
                    1,
                    4,
                    FRAG_FRAGMENTED | FRAG_LAST,
                    2,
                    Bytes::from_static(b"tail")
                ),
                now
            ),
            Err(FragmentOutcome::Malformed),
            "a gap in the group's sequences is not a group"
        );
        assert_eq!(r.outstanding(), 0, "and the group goes with it");
        let records = r.take_abandoned();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].reason, AbandonReason::Malformed);
        assert_eq!(
            r.accept(
                part_seq(
                    SESSION,
                    1,
                    4,
                    FRAG_FRAGMENTED | FRAG_LAST,
                    1,
                    Bytes::from_static(b"tail")
                ),
                now
            ),
            Err(FragmentOutcome::Abandoned),
            "and the id is fenced, so the honest tail cannot open a \
             headless successor either"
        );
    }

    /// **NR6.** One acknowledged sequence carries one piece: a second
    /// piece on a sequence the group already holds is a contradiction,
    /// not a second piece.
    #[test]
    fn two_pieces_cannot_share_one_acknowledged_sequence() {
        let r = RtcReassembly::new();
        let now = Instant::now();
        let _ = r.accept(
            part_seq(
                SESSION,
                1,
                0,
                FRAG_FRAGMENTED,
                7,
                Bytes::from_static(b"head"),
            ),
            now,
        );
        assert_eq!(
            r.accept(
                part_seq(
                    SESSION,
                    1,
                    4,
                    FRAG_FRAGMENTED | FRAG_LAST,
                    7,
                    Bytes::from_static(b"tail")
                ),
                now
            ),
            Err(FragmentOutcome::Malformed)
        );
        assert_eq!(r.outstanding(), 0);
    }

    /// **NR2.** A NEW group refused by the capacity bound has already
    /// had its piece acknowledged, so the refusal is a loss: it is
    /// reported, and the group id is fenced so the rest of the group
    /// cannot open a headless successor once the pressure eases.
    ///
    /// Inverse: return a bare `Refused` from the `None` arm of
    /// `accept_locked`'s slot match — `abandoned_total` stays at the
    /// pre-refusal value and the tail opens a group of its own.
    #[test]
    fn a_refused_first_piece_is_reported_and_fences_its_group() {
        let r = RtcReassembly::new();
        let now = Instant::now();
        let chunk = MAX_PAYLOAD_SIZE - 1;
        for id in 1..=MAX_GROUPS_PER_SESSION as u16 {
            if r.accept(part(SESSION, id, 0, FRAG_FRAGMENTED, piece(chunk)), now)
                == Err(FragmentOutcome::Refused)
            {
                break;
            }
        }
        let before = r.abandoned_total();
        let _ = r.take_abandoned();
        let _ = r.take_terminals();
        // A group the session has never seen, whose first piece the
        // byte budget cannot admit.
        assert_eq!(
            r.accept(
                part_seq(SESSION, 400, 0, FRAG_FRAGMENTED, 90, piece(64)),
                now
            ),
            Err(FragmentOutcome::Refused)
        );
        assert_eq!(
            r.abandoned_total(),
            before + 1,
            "refusing a first piece loses acknowledged bytes, so it is \
             reported like every other destruction"
        );
        let records = r.take_abandoned();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].reason, AbandonReason::Refused);
        assert_eq!(records[0].fragment_id, 400);
        assert_eq!(records[0].held, 64, "the piece's acknowledged bytes");
        assert_eq!(records[0].first_sequence, 90);
        assert_eq!(
            r.accept(
                part_seq(SESSION, 400, 64, FRAG_FRAGMENTED | FRAG_LAST, 91, piece(8)),
                now
            ),
            Err(FragmentOutcome::Abandoned),
            "and the refused group is fenced: its tail must not open a \
             group whose head was never admitted"
        );
    }

    /// **NR6.** A RESET ends one stream's receive lifetime, so that
    /// stream's partial groups go with it — and only that stream's.
    /// Nothing is reported: the reset is the terminal.
    ///
    /// Inverse: make `retire_stream` a no-op — the pre-reset head
    /// survives and a delayed old tail completes it against the
    /// lifetime that followed the reset.
    #[test]
    fn a_stream_reset_retires_only_that_streams_groups() {
        let r = RtcReassembly::new();
        let now = Instant::now();
        let mut other = part_seq(SESSION, 2, 0, FRAG_FRAGMENTED, 5, piece(16));
        other.provenance.stream_id = STREAM + 1;
        let _ = r.accept(part_seq(SESSION, 1, 0, FRAG_FRAGMENTED, 4, piece(32)), now);
        let _ = r.accept(other, now);
        assert_eq!(r.held_bytes(SESSION), 48);

        r.retire_stream(SESSION, STREAM, now);
        assert_eq!(
            r.held_bytes(SESSION),
            16,
            "the reset stream's group is released; the other stream's is not"
        );
        assert_eq!(r.reset_retired_total(), 1);
        assert!(
            r.take_abandoned().is_empty(),
            "the reset IS the terminal disposition — reporting it again \
             would ask the ingress to reset a stream because it was reset"
        );
        assert_eq!(
            r.accept(
                part_seq(SESSION, 1, 32, FRAG_FRAGMENTED | FRAG_LAST, 5, piece(8)),
                now
            ),
            Err(FragmentOutcome::Abandoned),
            "and the released group is fenced, so an old delayed tail \
             cannot complete it against the lifetime after the reset"
        );
    }

    /// **NR3.** Shutdown owns the mesh's reassembly: every session's
    /// partial groups are released and every session is fenced,
    /// without waiting for a close notification that arrives after
    /// its consumer has been joined.
    ///
    /// Inverse: drop the `retire_all` call from `MeshNode::shutdown`
    /// — a retained shut-down node keeps every buffered group.
    #[test]
    fn shutting_down_retires_every_sessions_groups() {
        let r = RtcReassembly::new();
        let now = Instant::now();
        let _ = r.accept(part_seq(SESSION, 1, 0, FRAG_FRAGMENTED, 0, piece(32)), now);
        let _ = r.accept(
            part_seq(SESSION + 1, 1, 0, FRAG_FRAGMENTED, 0, piece(8)),
            now,
        );
        assert_eq!(r.outstanding(), 2);

        assert_eq!(r.retire_all(now), 2);
        assert_eq!(r.outstanding(), 0);
        assert_eq!(r.held_bytes(SESSION), 0);
        assert_eq!(r.held_bytes(SESSION + 1), 0);
        let released = r.take_abandoned();
        assert_eq!(released.len(), 2, "and both losses are named");
        assert!(released
            .iter()
            .all(|g| g.reason == AbandonReason::SessionRetired));
        assert_eq!(
            r.accept(part_seq(SESSION, 1, 32, FRAG_FRAGMENTED, 1, piece(8)), now),
            Err(FragmentOutcome::Retired),
            "and a frame already past its session lookup cannot rebuild \
             state on a node that has shut down"
        );
    }

    /// **NR2.** The production terminal queue and the diagnostic
    /// record are independent: draining one does not consume the
    /// other, so an operator reading `take_abandoned` cannot swallow
    /// the ingress's obligation to end the stream — which is the
    /// shape that let abandonment be diagnostic-only.
    #[test]
    fn a_destroyed_group_owes_a_terminal_and_records_a_diagnostic() {
        let r = RtcReassembly::new();
        let now = Instant::now();
        let _ = r.accept(part_seq(SESSION, 1, 0, FRAG_FRAGMENTED, 0, piece(32)), now);
        r.retire_session(SESSION, now);

        let diagnostics = r.take_abandoned();
        assert_eq!(diagnostics.len(), 1);
        let terminals = r.take_terminals();
        assert_eq!(
            terminals.len(),
            1,
            "the diagnostic drain must not consume the terminal the \
             ingress owes this stream"
        );
        assert_eq!(terminals[0].provenance.stream_id, STREAM);
        assert!(
            r.take_terminals().is_empty(),
            "and a terminal is consumed exactly once: one destroyed group \
             is one reset, not one per drainer"
        );
    }
}
