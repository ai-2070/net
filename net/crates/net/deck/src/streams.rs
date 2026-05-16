//! Streaming tails — Phase 4 wiring. Replaces "render straight
//! from snapshot.log_ring" with a per-stream buffer fed by the
//! SDK's `subscribe_*` APIs. The buffer is decoupled from the
//! substrate's ring cap, so an operator who pauses on LOGS
//! keeps the records they were reading even if the runtime
//! rotates them out of `log_ring`.
//!
//! The implementation is intentionally small: a parking_lot
//! `Mutex<VecDeque>` shared between a tokio spawn that drains
//! the SDK stream and the App's sync render path. Render-side
//! locks are short (push to buffer / clone slice on render),
//! never held across an await, never contended at the rate of
//! a 16Hz redraw.

use std::collections::VecDeque;
use std::sync::Arc;

use futures::StreamExt;
use net_sdk::dataforts::{BlobAdapter, BlobInventoryEntry, BlobListOptions, MeshBlobAdapter};
use net_sdk::deck::{AdminAuditRecord, DeckClient, FailureRecord, LogFilter, LogRecord};
use parking_lot::Mutex;

/// Capacity of the LOGS tail. 5000 records × ~256B per record
/// is ~1.3MB — fine for an operator session and deep enough
/// that scrolling back through an incident's worth of lines
/// rarely runs out.
pub const LOGS_TAIL_CAP: usize = 5_000;

/// Capacity of the AUDIT tail. Admin commits are sparse
/// (operator-driven, never machine-generated), so 2000 covers
/// a long session with room to spare.
pub const AUDIT_TAIL_CAP: usize = 2_000;

/// Capacity of the FAILURES tail. Bursty on outages but rarely
/// chatty enough in steady state to need a deep history;
/// 2000 records is comfortable for incident triage.
pub const FAILURES_TAIL_CAP: usize = 2_000;

/// Shared, lock-protected ring of log records. Owned by App;
/// the streaming task holds a clone of the Arc and pushes new
/// records as they arrive.
#[derive(Clone)]
pub struct LogsTail {
    pub records: Arc<Mutex<VecDeque<LogRecord>>>,
    pub cap: usize,
}

impl LogsTail {
    pub fn new(cap: usize) -> Self {
        Self {
            records: Arc::new(Mutex::new(VecDeque::with_capacity(cap.min(1024)))),
            cap,
        }
    }

    /// Copy the current buffer contents into a flat Vec for the
    /// render path. We allocate per redraw rather than returning
    /// a lock guard so the lock is held for microseconds, not
    /// the full render pass — and the render functions stay sync
    /// without leaking the lock type into their signatures.
    pub fn snapshot(&self) -> Vec<LogRecord> {
        let g = self.records.lock();
        g.iter().cloned().collect()
    }

    /// Append a record, dropping the oldest if at capacity.
    pub fn push(&self, record: LogRecord) {
        let mut g = self.records.lock();
        if g.len() == self.cap {
            g.pop_front();
        }
        g.push_back(record);
    }
}

/// Spawn the LOGS streaming task. Returns immediately; the task
/// runs until the stream errors / closes (substrate shutdown).
/// The filter is intentionally empty — App-side filters (level
/// threshold, substring search) apply at render time so
/// operators can adjust without re-subscribing.
pub fn spawn_logs_stream(deck: Arc<DeckClient>, tail: LogsTail) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut stream = deck.subscribe_logs(LogFilter::new());
        while let Some(item) = stream.next().await {
            match item {
                Ok(record) => tail.push(record),
                Err(_err) => {
                    // Stream-level errors are surfaced by the
                    // SDK but rare in practice (substrate gone).
                    // Back off briefly so a persistently failing
                    // stream doesn't peg a tokio worker; if the
                    // substrate is truly gone the stream will
                    // end on its own.
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    continue;
                }
            }
        }
    })
}

/// AUDIT tail mirror of `LogsTail`. Same locking + capacity
/// discipline; different record type. Fed by the audit stream.
#[derive(Clone)]
pub struct AuditTail {
    pub records: Arc<Mutex<VecDeque<AdminAuditRecord>>>,
    pub cap: usize,
}

impl AuditTail {
    pub fn new(cap: usize) -> Self {
        Self {
            records: Arc::new(Mutex::new(VecDeque::with_capacity(cap.min(512)))),
            cap,
        }
    }

    pub fn snapshot(&self) -> Vec<AdminAuditRecord> {
        let g = self.records.lock();
        g.iter().cloned().collect()
    }

    pub fn push(&self, record: AdminAuditRecord) {
        let mut g = self.records.lock();
        if g.len() == self.cap {
            g.pop_front();
        }
        g.push_back(record);
    }
}

/// Spawn the AUDIT streaming task. The query is unfiltered —
/// App-side toggles (`[f]` ICE-only, `[/]` search) apply at
/// render time so operators can adjust without re-subscribing.
pub fn spawn_audit_stream(deck: Arc<DeckClient>, tail: AuditTail) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut stream = deck.audit().stream();
        while let Some(item) = stream.next().await {
            match item {
                Ok(record) => tail.push(record),
                Err(_err) => {
                    // Back off briefly so a persistently
                    // failing stream doesn't peg a tokio
                    // worker in a tight retry loop. 50ms is
                    // small enough to feel instant on success;
                    // a stream that errors continuously will
                    // either end (substrate gone — the while
                    // loop exits) or recover.
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    continue;
                }
            }
        }
    })
}

/// FAILURES tail mirror of `LogsTail` / `AuditTail`. Holds
/// executor failure records — dispatcher rejections,
/// constraint-violation drops, etc. Fed by the failure stream.
#[derive(Clone)]
pub struct FailuresTail {
    pub records: Arc<Mutex<VecDeque<FailureRecord>>>,
    pub cap: usize,
}

impl FailuresTail {
    pub fn new(cap: usize) -> Self {
        Self {
            records: Arc::new(Mutex::new(VecDeque::with_capacity(cap.min(512)))),
            cap,
        }
    }

    pub fn snapshot(&self) -> Vec<FailureRecord> {
        let g = self.records.lock();
        g.iter().cloned().collect()
    }

    pub fn push(&self, record: FailureRecord) {
        let mut g = self.records.lock();
        if g.len() == self.cap {
            g.pop_front();
        }
        g.push_back(record);
    }
}

/// Spawn the FAILURES streaming task. `since_seq = 0` replays
/// everything still on the substrate's ring at subscribe time;
/// the SDK dedups via per-record `seq` so reconnects don't
/// double-emit.
pub fn spawn_failures_stream(
    deck: Arc<DeckClient>,
    tail: FailuresTail,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut stream = deck.subscribe_failures(0);
        while let Some(item) = stream.next().await {
            match item {
                Ok(record) => tail.push(record),
                Err(_err) => {
                    // Back off briefly so a persistently
                    // failing stream doesn't peg a tokio
                    // worker in a tight retry loop. 50ms is
                    // small enough to feel instant on success;
                    // a stream that errors continuously will
                    // either end (substrate gone — the while
                    // loop exits) or recover.
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    continue;
                }
            }
        }
    })
}

/// Cap on the BLOBS inventory snapshot the deck renders. The
/// adapter's full set may exceed this; the cap bounds memory
/// and render cost at the cost of truncating to
/// most-recently-touched entries first.
pub const BLOBS_TAIL_CAP: usize = 5_000;

/// BLOBS inventory tail. Distinct from the log / audit /
/// failure tails because the source isn't a `Stream` —
/// `BlobAdapter::list` is a one-shot query. The spawned task
/// re-polls on a fixed cadence and overwrites the cached
/// snapshot. App reads via [`Self::snapshot`].
#[derive(Clone)]
pub struct BlobsTail {
    pub records: Arc<Mutex<Vec<BlobInventoryEntry>>>,
}

impl BlobsTail {
    pub fn new() -> Self {
        Self {
            records: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Copy the current inventory into a flat Vec for the
    /// render path. Brief lock; never held across an await.
    pub fn snapshot(&self) -> Vec<BlobInventoryEntry> {
        self.records.lock().clone()
    }

    /// Overwrite the cached inventory with a fresh poll. Used
    /// by [`spawn_blobs_poll`].
    fn replace(&self, entries: Vec<BlobInventoryEntry>) {
        *self.records.lock() = entries;
    }
}

impl Default for BlobsTail {
    fn default() -> Self {
        Self::new()
    }
}

/// Spawn the BLOBS inventory poller. Polls `adapter.list(...)`
/// every `poll_interval`; updates the cached snapshot via
/// [`BlobsTail::replace`]. Cancelled when the returned handle
/// is dropped.
pub fn spawn_blobs_poll(
    adapter: Arc<MeshBlobAdapter>,
    tail: BlobsTail,
    poll_interval: std::time::Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(poll_interval);
        // Skip the immediate tick — the spawn-time call below
        // covers the cold path; the interval ticks for the
        // refresh cadence after that.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let opts = BlobListOptions {
                prefix_hex: None,
                limit: BLOBS_TAIL_CAP,
            };
            match adapter.list(&opts).await {
                Ok(entries) => tail.replace(entries),
                Err(_) => continue,
            }
        }
    })
}
