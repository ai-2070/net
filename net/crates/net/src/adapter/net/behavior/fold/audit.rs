//! Audit-event sink trait + plumbing.
//!
//! [`FoldKind::audit_event`](super::FoldKind::audit_event) is
//! called on every applied transition; the returned
//! [`AuditEvent`](super::AuditEvent) is forwarded to the
//! installed [`FoldAuditSink`]. Default install is `None`, in
//! which case audit emission is effectively a no-op; sinks can
//! be installed at any time via
//! [`super::Fold::set_audit_sink`] — `tracing` adapter, the
//! project's signed-audit chain, a Prometheus counter exporter,
//! an in-memory ring for tests, etc.

use super::AuditEvent;

/// Sink that consumes audit events emitted by a fold's
/// transitions.
///
/// Implementors are typically:
/// - A `tracing` adapter that emits via `tracing::info!` /
///   `tracing::warn!` on the appropriate level.
/// - A bridge to the project's existing signed-audit chain
///   (writes to the same `FoldAuditSink` interface used by the
///   safety / replication layers).
/// - A `Vec<AuditEvent>`-backed ring buffer for tests + Deck
///   panel "recent transitions" view.
///
/// The `record` hook is fire-and-forget by contract: a slow
/// sink slows the apply (or expiry) path because the call sites
/// invoke `record` synchronously under the fold's locks. Real
/// implementations push to a channel and drain in a worker.
pub trait FoldAuditSink: Send + Sync {
    /// Record one audit event. The implementor decides where
    /// the event goes; the fold runtime does not introspect
    /// the result.
    fn record(&self, event: AuditEvent);
}

/// No-op audit sink. Constructed implicitly when no sink is
/// installed via [`super::Fold::set_audit_sink`]; surfaced
/// publicly for tests / call sites that want an explicit
/// "discard" sink without an `Option` wrapper.
#[derive(Debug, Default)]
pub struct NoopSink;

impl FoldAuditSink for NoopSink {
    fn record(&self, _event: AuditEvent) {}
}

/// `Vec<AuditEvent>`-backed sink for tests. Records every event
/// in insertion order; tests inspect the stored vec to assert
/// the apply / evict / expiry paths emit the right transitions.
///
/// Thread-safe — the inner storage is wrapped in a
/// `parking_lot::Mutex` so `record` is callable from concurrent
/// fold operations. Use [`Self::snapshot`] to read the recorded
/// events at any point.
#[derive(Default)]
pub struct VecFoldAuditSink {
    events: parking_lot::Mutex<Vec<AuditEvent>>,
}

impl VecFoldAuditSink {
    /// Construct an empty sink.
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot the events recorded so far. Returns a clone so
    /// the caller can inspect without holding the sink's lock.
    pub fn snapshot(&self) -> Vec<AuditEvent> {
        self.events.lock().clone()
    }

    /// Number of events recorded since the sink was constructed.
    pub fn len(&self) -> usize {
        self.events.lock().len()
    }

    /// Whether the sink has recorded any events.
    pub fn is_empty(&self) -> bool {
        self.events.lock().is_empty()
    }
}

impl FoldAuditSink for VecFoldAuditSink {
    fn record(&self, event: AuditEvent) {
        self.events.lock().push(event);
    }
}

/// Bounded ring-buffer audit sink. Keeps the most recent
/// `capacity` events; oldest are dropped first when full. This
/// is the sink the Deck FOLDS panel's "recent transitions" view
/// consumes — operators want the last N events, not a complete
/// history. Bounded capacity also makes this safe to install on
/// a high-throughput fold without unbounded memory growth.
pub struct RingFoldAuditSink {
    ring: super::super::bounded_ring::BoundedRing<AuditEvent>,
}

impl RingFoldAuditSink {
    /// Construct a sink that retains the most recent `capacity`
    /// events. `capacity == 0` is accepted (and useful: an
    /// always-empty sink that still satisfies the trait without
    /// storing anything).
    pub fn new(capacity: usize) -> Self {
        Self {
            ring: super::super::bounded_ring::BoundedRing::new(capacity),
        }
    }

    /// Capacity the sink was constructed with.
    pub fn capacity(&self) -> usize {
        self.ring.capacity()
    }

    /// Current event count (always `<= capacity`).
    pub fn len(&self) -> usize {
        self.ring.len()
    }

    /// Whether the sink currently holds any events.
    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }

    /// Snapshot the retained events in insertion order
    /// (oldest first → newest last).
    pub fn snapshot(&self) -> Vec<AuditEvent> {
        self.ring.snapshot()
    }
}

impl FoldAuditSink for RingFoldAuditSink {
    fn record(&self, event: AuditEvent) {
        self.ring.push(event);
    }
}

