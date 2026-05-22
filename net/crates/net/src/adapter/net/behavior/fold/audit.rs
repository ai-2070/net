//! Phase 1B audit-event sink trait + plumbing.
//!
//! Phase 1's [`FoldKind::audit_event`](super::FoldKind::audit_event)
//! is called on every applied transition but the returned
//! [`AuditEvent`](super::AuditEvent) is currently dropped at the
//! call site (it has no destination). Phase 1B wires a sink
//! trait so callers can install their own destination —
//! `tracing` adapter, the project's signed-audit chain, a
//! Prometheus counter exporter, an in-memory ring for tests.
//!
//! Default install: nothing. Folds constructed via
//! [`super::Fold::new`] start with `audit_sink == None` so the
//! `K::audit_event` calls remain effectively no-ops for any
//! fold that didn't opt into audit emission. The sink can be
//! installed at any time via
//! [`super::Fold::set_audit_sink`].

use super::AuditEvent;

/// Sink that consumes audit events emitted by a fold's
/// transitions.
///
/// Implementors are typically:
/// - A `tracing` adapter that emits via `tracing::info!` /
///   `tracing::warn!` on the appropriate level.
/// - A bridge to the project's existing signed-audit chain
///   (writes to the same `AuditSink` interface used by the
///   safety / replication layers).
/// - A `Vec<AuditEvent>`-backed ring buffer for tests + Deck
///   panel "recent transitions" view.
///
/// The `record` hook is fire-and-forget by contract: a slow
/// sink slows the apply (or expiry) path because the call sites
/// invoke `record` synchronously under the fold's locks. Real
/// implementations push to a channel and drain in a worker.
pub trait AuditSink: Send + Sync {
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

impl AuditSink for NoopSink {
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
pub struct VecAuditSink {
    events: parking_lot::Mutex<Vec<AuditEvent>>,
}

impl VecAuditSink {
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

impl AuditSink for VecAuditSink {
    fn record(&self, event: AuditEvent) {
        self.events.lock().push(event);
    }
}

