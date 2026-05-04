//! `CortexAdapter<State>` — one RedEX file, one fold, one materialized
//! state.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use futures::{Stream, StreamExt};
use parking_lot::RwLock;
use tokio::sync::{broadcast, Notify};
use tokio_stream::wrappers::BroadcastStream;

use serde::de::DeserializeOwned;
use serde::Serialize;

use super::super::channel::ChannelName;
use super::super::redex::{Redex, RedexError, RedexEvent, RedexFile, RedexFileConfig, RedexFold};
use super::config::{CortexAdapterConfig, FoldErrorPolicy, StartPosition};
use super::envelope::IntoRedexPayload;
use super::error::CortexAdapterError;
use super::meta::EVENT_META_SIZE;

/// One-file CortEX adapter: projects envelopes into RedEX payloads,
/// tails the same file, drives a [`RedexFold`] implementation, and
/// exposes the materialized state as a read handle.
///
/// Created via [`Self::open`].
pub struct CortexAdapter<State> {
    inner: Arc<AdapterInner<State>>,
}

/// Capacity of the post-fold change-notification broadcast channel.
/// A slow subscriber that falls more than this many events behind
/// gets a `Lagged` signal and should re-read state fresh.
const CHANGES_BROADCAST_CAP: usize = 64;

/// Item type yielded by [`CortexAdapter::changes_with_lag`].
///
/// The plain `changes()` stream uses `filter_map(|r| r.ok())`
/// which silently drops `BroadcastStream::Lagged(n)` errors —
/// downstream telemetry consumers have no way to surface "you
/// missed N changes." This enum exposes both shapes; subscribers
/// who need only the latest sequence can stay on
/// [`CortexAdapter::changes`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeEvent {
    /// A successful fold apply produced this RedEX sequence.
    Seq(u64),
    /// The subscriber fell `n` events behind the broadcast channel
    /// and `n` change notifications were dropped. By the time the
    /// subscriber sees this, `state()` already reflects past those
    /// events — the lag value is purely observability.
    Lagged(u64),
}

struct AdapterInner<State> {
    file: RedexFile,
    state: Arc<RwLock<State>>,
    /// Highest RedEX seq applied to state, as a signed i64 so we can
    /// sentinel "nothing folded yet" with `start_seq - 1` (can be
    /// negative when `start_seq == 0`).
    /// Highest RedEX seq the fold task has *processed* — applied
    /// successfully OR skipped via `recoverable_decode` under
    /// `Stop` policy. The skip-and-continue path advances this
    /// watermark so live consumers waiting via `wait_for_seq`
    /// don't deadlock on a single permanently-bad event (the
    /// DoS-resistance contract documented on
    /// `RedexError::is_recoverable_decode`).
    ///
    /// **Use `applied_through_seq` instead** if you need
    /// "state actually reflects this seq" semantics — that
    /// watermark only advances on `Ok(())` folds, and is what
    /// `snapshot` persists so restore tails from a position
    /// consistent with the in-memory state.
    ///
    /// `u64::MAX` is the "nothing folded yet" sentinel; safe
    /// because the `open_from_snapshot` overflow guard rejects
    /// `last_seq == u64::MAX`, so no real event can ever occupy
    /// that slot.
    folded_through_seq: AtomicU64,
    /// Highest RedEX seq K such that **every** seq in
    /// `start_seq..=K` was applied to state via a successful
    /// `RedexFold::apply`. A *strict-prefix* watermark — any
    /// skip (`recoverable_decode` under `Stop`, or any error
    /// under `LogAndContinue`) breaks the prefix at the skipped
    /// seq, and `applied_through_seq` cannot advance past it
    /// even when later seqs apply successfully. Distinct from
    /// `folded_through_seq`, which is the highest *processed*
    /// seq and advances over skips so live consumers don't
    /// deadlock.
    ///
    /// Snapshot persists this value, so restore tails from
    /// `applied_through_seq + 1` with the guarantee that **every
    /// prior seq is reflected in state**. Without strict-prefix
    /// semantics, a skip at seq M followed by successful applies
    /// at M+1, M+2 would advance `applied` to M+2 (highest
    /// individual apply); snapshot persists M+2; restore tails
    /// from M+3; seq M is never re-attempted on restore — its
    /// mutations are permanently lost from durable state, even
    /// though the log still carries the event. That's the
    /// "violates log is the source of truth" failure audited as
    /// #6+#7.
    ///
    /// Same `u64::MAX` "nothing applied yet" sentinel as
    /// `folded_through_seq`. `start_seq == 0` is the only
    /// configuration that initializes to sentinel; an
    /// `open_from_snapshot` with `last_seq = Some(K)` initializes
    /// to `K` (the snapshot's contract is that `start_seq..=K`
    /// is already in the rehydrated state).
    applied_through_seq: AtomicU64,
    /// First RedEX seq this adapter began folding from. Any seq
    /// strictly below this is conceptually behind us — `wait_for_seq`
    /// short-circuits without blocking on the watermark, even when
    /// `start_seq == 0` puts the watermark at the `u64::MAX`
    /// sentinel. Stored on inner so the wait predicate doesn't
    /// need to reach back into the open-time `start_seq` local.
    start_seq: u64,
    fold_errors: AtomicU64,
    running: AtomicBool,
    closed: AtomicBool,
    notify: Notify,
    shutdown: Notify,
    /// Broadcast of RedEX seqs after each successful (or LogAndContinue-skipped)
    /// fold apply. Subscribers: see [`CortexAdapter::changes`].
    changes_tx: broadcast::Sender<u64>,
}

impl<State> CortexAdapter<State> {
    /// Read-only access to the materialized state. The returned `Arc`
    /// is cheap to clone; all readers and the fold task share the
    /// same `RwLock`.
    pub fn state(&self) -> Arc<RwLock<State>> {
        self.inner.state.clone()
    }

    /// Highest RedEX sequence the fold task has *processed* —
    /// applied successfully OR skipped via `recoverable_decode`
    /// under `Stop` policy.
    ///
    /// Use [`Self::applied_through_seq`] if you need "state
    /// actually reflects this seq" semantics; the difference
    /// matters under `Stop`+recoverable-decode where the
    /// skip-and-continue path advances *this* watermark (so
    /// `wait_for_seq` doesn't deadlock on a bad event) but
    /// leaves `applied_through_seq` behind.
    ///
    /// `None` if no event has been folded yet since open.
    pub fn folded_through_seq(&self) -> Option<u64> {
        let v = self.inner.folded_through_seq.load(Ordering::Acquire);
        if v == u64::MAX {
            None
        } else {
            Some(v)
        }
    }

    /// Highest RedEX sequence K such that *every* seq in
    /// `start_seq..=K` was successfully applied to state via
    /// `Ok(()) RedexFold::apply`. A *strict-prefix* watermark:
    /// any skip (`recoverable_decode` under `Stop`, or any error
    /// under `LogAndContinue`) at seq M permanently caps this
    /// watermark at M-1, even if subsequent seqs apply
    /// successfully.
    ///
    /// `snapshot` persists this value, so a restore tails from
    /// `applied_through_seq + 1` and re-attempts every seq from
    /// the first skip onwards — preserving "log is the source
    /// of truth" even across the skip-and-continue path.
    ///
    /// Distinct from [`Self::folded_through_seq`], which is the
    /// highest *processed* seq and advances over skips so live
    /// consumers don't deadlock.
    ///
    /// `None` if no event has been applied yet since open
    /// (start_seq=0 with no successful applies, or start_seq=0
    /// with the very first event having been skipped).
    pub fn applied_through_seq(&self) -> Option<u64> {
        let v = self.inner.applied_through_seq.load(Ordering::Acquire);
        if v == u64::MAX {
            None
        } else {
            Some(v)
        }
    }

    /// Cumulative count of fold errors (only ever increases under
    /// [`FoldErrorPolicy::LogAndContinue`]; under `Stop` it is 0 or
    /// 1, with the task exiting after the first error).
    pub fn fold_errors(&self) -> u64 {
        self.inner.fold_errors.load(Ordering::Acquire)
    }

    /// True if the fold task is currently running (has not observed
    /// shutdown, an error under `Stop`, or a tail-end signal).
    pub fn is_running(&self) -> bool {
        self.inner.running.load(Ordering::Acquire)
    }

    /// Block until the fold task has *processed* every event up
    /// through `seq` (applied successfully OR skipped via
    /// `recoverable_decode` under `Stop` policy), or until the
    /// fold task stops (e.g. close, non-recoverable fold error
    /// under `Stop`). Returning after the task has stopped is
    /// correct behavior — callers should re-check
    /// [`Self::is_running`] if they need to distinguish.
    ///
    /// Use pattern:
    /// ```ignore
    /// let seq = adapter.ingest(envelope)?;
    /// adapter.wait_for_seq(seq).await;
    /// let state = adapter.state().read();
    /// // state reflects the ingest, UNLESS the event at `seq`
    /// // was skipped via recoverable_decode under `Stop`.
    /// ```
    ///
    /// **Subtle point.** This method waits on
    /// [`Self::folded_through_seq`], which advances over
    /// events the fold task processed but skipped via
    /// `RedexError::is_recoverable_decode`. The skip-and-
    /// continue path is the documented DoS-resistance contract
    /// (a single corrupt event must not wedge the task forever).
    /// If you need to confirm `state` actually reflects the
    /// ingest at `seq`, follow up with
    /// `adapter.applied_through_seq() >= Some(seq)`.
    pub async fn wait_for_seq(&self, seq: u64) {
        // Any seq strictly below `start_seq` is conceptually behind
        // us — those events were applied before we opened the
        // adapter (or are explicitly past the LiveOnly cutoff).
        // Short-circuit returning immediately so a caller that
        // passes a stale seq cannot hang. This also covers the
        // `start_seq == 0 && seq == 0` blocked-forever-on-empty-
        // file case for adapters opened with `FromBeginning` on a
        // freshly-created log: `seq < start_seq` is false, but the
        // sentinel check below correctly waits for the first event.
        if seq < self.inner.start_seq {
            return;
        }
        loop {
            let notified = self.inner.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            let watermark = self.inner.folded_through_seq.load(Ordering::Acquire);
            // `u64::MAX` is the "nothing folded yet" sentinel — any
            // other value is a real applied seq, so `watermark >= seq`
            // after the sentinel check returns exactly when seq has
            // been applied.
            if watermark != u64::MAX && watermark >= seq {
                return;
            }
            if !self.inner.running.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }

    /// Close the adapter. Stops the fold task (after it finishes any
    /// in-progress apply), leaves the RedEX file open so other
    /// adapters / callers can continue using it, and leaves the
    /// state handle readable. Idempotent.
    pub fn close(&self) -> Result<(), CortexAdapterError> {
        if self.inner.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        // `notify_one()` stores a permit if the fold task hasn't yet
        // reached its `shutdown.notified()` poll, so a close that
        // races the spawn → first-select window is still observed.
        // `notify_waiters()` would drop the signal in that window.
        self.inner.shutdown.notify_one();
        Ok(())
    }

    /// Stream of RedEX sequences, one per successful (or
    /// `LogAndContinue`-skipped) fold application. Used by reactive
    /// queries: on each emission, the caller re-reads
    /// [`Self::state`] to compute its current view.
    ///
    /// Lag semantics: if a subscriber falls more than 64 events
    /// behind (the internal broadcast channel capacity), the channel
    /// drops intermediate events. This implementation filters lag
    /// errors out silently — by the time the subscriber catches up,
    /// `state()` reflects the latest applied events regardless of
    /// how many signals were missed. Subscribers that need to
    /// observe lag (e.g. for telemetry or reactive-backpressure)
    /// should use [`Self::changes_with_lag`] instead.
    ///
    /// The stream ends when all adapter handles have been dropped
    /// and the fold task has exited.
    pub fn changes(&self) -> impl Stream<Item = u64> + Send + 'static {
        BroadcastStream::new(self.inner.changes_tx.subscribe())
            .filter_map(|r| async move { r.ok() })
    }

    /// Stream of changes that surfaces broadcast-channel lag as a
    /// `Lagged(n)` event interleaved with the sequence emissions.
    ///
    /// The yielded items are [`ChangeEvent`]s — `Seq(u64)` for a
    /// successful fold-apply notification, and `Lagged(n)` when the
    /// subscriber fell `n` events behind the broadcast channel
    /// (capacity 64). Pre-fix [`Self::changes`] silently dropped
    /// `Lagged` errors via `filter_map(|r| r.ok())`; downstream
    /// telemetry consumers had no way to surface "you missed N
    /// changes." This method is the lossless counterpart — by the
    /// time a subscriber sees `Lagged(n)`, `state()` already
    /// reflects past those n events, so the subscriber can react
    /// (re-read state, log lag, apply backpressure) without
    /// missing data.
    pub fn changes_with_lag(&self) -> impl Stream<Item = ChangeEvent> + Send + 'static {
        use futures::StreamExt;
        BroadcastStream::new(self.inner.changes_tx.subscribe()).map(|r| match r {
            Ok(seq) => ChangeEvent::Seq(seq),
            Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(n)) => {
                ChangeEvent::Lagged(n)
            }
        })
    }

    /// Append an envelope. Projects to `(EventMeta, tail)`, builds the
    /// concatenated payload, calls [`RedexFile::append`], and returns
    /// the assigned RedEX sequence.
    pub fn ingest<E: IntoRedexPayload>(&self, envelope: E) -> Result<u64, CortexAdapterError> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(CortexAdapterError::Closed);
        }
        let (meta, tail) = envelope.into_redex_payload();
        let mut buf = Vec::with_capacity(EVENT_META_SIZE + tail.len());
        buf.extend_from_slice(&meta.to_bytes());
        buf.extend_from_slice(&tail);
        Ok(self.inner.file.append(&buf)?)
    }
}

impl<State: Send + Sync + 'static> CortexAdapter<State> {
    /// Open an adapter against a RedEX file.
    ///
    /// Opens (or reuses) `<redex>/<name>` via
    /// [`Redex::open_file`](super::super::redex::Redex::open_file),
    /// spawns a background task that tails the file and drives
    /// `fold`, and returns the handle.
    pub fn open<F>(
        redex: &Redex,
        name: &ChannelName,
        redex_config: RedexFileConfig,
        adapter_config: CortexAdapterConfig,
        fold: F,
        initial_state: State,
    ) -> Result<Self, CortexAdapterError>
    where
        F: RedexFold<State> + Send + 'static,
    {
        // Positions that skip a non-empty event prefix require
        // externally-rehydrated state — the watermark would
        // otherwise advance past events the adapter never saw,
        // making `wait_for_seq(k)` return immediately for skipped
        // k while `state` has never observed those events.
        // Callers using these positions must use
        // `open_from_snapshot` (which carries the matching
        // `last_seq` + serialized state) and routes through
        // `open_unchecked` below.
        match adapter_config.start {
            StartPosition::FromBeginning => {}
            StartPosition::LiveOnly => {
                return Err(CortexAdapterError::InvalidStartPosition("LiveOnly"));
            }
            StartPosition::FromSeq(n) if n > 0 => {
                return Err(CortexAdapterError::InvalidStartPosition("FromSeq(n>0)"));
            }
            StartPosition::FromSeq(_) => {} // FromSeq(0) is equivalent to FromBeginning
        }
        Self::open_unchecked(
            redex,
            name,
            redex_config,
            adapter_config,
            fold,
            initial_state,
        )
    }

    /// Internal open path that bypasses the start-position
    /// guard. Used by `open_from_snapshot`, where the externally-
    /// rehydrated state is the legitimate reason to skip the
    /// event prefix.
    fn open_unchecked<F>(
        redex: &Redex,
        name: &ChannelName,
        redex_config: RedexFileConfig,
        adapter_config: CortexAdapterConfig,
        mut fold: F,
        initial_state: State,
    ) -> Result<Self, CortexAdapterError>
    where
        F: RedexFold<State> + Send + 'static,
    {
        let file = redex.open_file(name, redex_config)?;

        let start_seq = match adapter_config.start {
            StartPosition::FromBeginning => 0,
            StartPosition::LiveOnly => file.next_seq(),
            StartPosition::FromSeq(n) => n,
        };

        let state = Arc::new(RwLock::new(initial_state));
        // Initial watermark encodes "applied through start_seq-1", so
        // `wait_for_seq(start_seq-1)` returns immediately after open
        // (those seqs are conceptually behind us) while
        // `wait_for_seq(start_seq)` blocks until the first event
        // actually folds. `start_seq == 0` encodes the "nothing
        // folded yet" state with the `u64::MAX` sentinel.
        let initial_watermark: u64 = if start_seq == 0 {
            u64::MAX
        } else {
            start_seq - 1
        };
        let (changes_tx, _) = broadcast::channel(CHANGES_BROADCAST_CAP);
        let inner = Arc::new(AdapterInner {
            file: file.clone(),
            state: state.clone(),
            folded_through_seq: AtomicU64::new(initial_watermark),
            // Mirror folded's initial watermark: the snapshot/restore
            // contract treats the sentinel as "nothing applied yet"
            // and the (start_seq - 1) seed as "applied through the
            // pre-snapshot prefix" — same semantics, applied to the
            // strict watermark.
            applied_through_seq: AtomicU64::new(initial_watermark),
            start_seq,
            fold_errors: AtomicU64::new(0),
            running: AtomicBool::new(true),
            closed: AtomicBool::new(false),
            notify: Notify::new(),
            shutdown: Notify::new(),
            changes_tx,
        });

        let policy = adapter_config.on_fold_error;
        let inner_task = inner.clone();
        let mut stream = Box::pin(file.tail(start_seq));

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = inner_task.shutdown.notified() => {
                        break;
                    }
                    next = stream.next() => {
                        match next {
                            None => break,
                            Some(Err(_)) => {
                                // Tail yielded an error (e.g. file
                                // closed). Stop cleanly.
                                break;
                            }
                            Some(Ok(event)) => {
                                if handle_event(
                                    &inner_task,
                                    &mut fold,
                                    &event,
                                    policy,
                                ) {
                                    break;
                                }
                            }
                        }
                    }
                }
            }
            inner_task.running.store(false, Ordering::Release);
            inner_task.notify.notify_waiters();
        });

        Ok(Self { inner })
    }
}

impl<State> CortexAdapter<State>
where
    State: Serialize + Send + Sync + 'static,
{
    /// Capture a point-in-time snapshot of the materialized state.
    ///
    /// Returns `(state_bytes, last_seq)` where `state_bytes` is the
    /// postcard-serialized state and `last_seq` is the highest RedEX
    /// sequence successfully *applied* to it as a strict prefix
    /// (i.e. [`Self::applied_through_seq`]). Persist both together —
    /// they form a consistent pair, guaranteed by the adapter
    /// holding the state write lock while advancing the watermark.
    ///
    /// Restore via [`Self::open_from_snapshot`] on a State that
    /// also implements `DeserializeOwned`. Restore tails from
    /// `last_seq + 1`, so any events that were processed but
    /// *skipped* via `recoverable_decode` between snapshots are
    /// re-attempted on restore — preserving "log is the source of
    /// truth" even across the skip-and-continue path. (Pre-fix,
    /// `snapshot` read `folded_through_seq`, which advances over
    /// skipped events; restore tailed from past them and the gap
    /// became permanent in durable state.)
    ///
    /// **Re-apply double-counting.** `state_bytes` reflects
    /// in-memory state at snapshot time, which includes the
    /// effects of every successful fold *including* applies past
    /// any prior skip. Restore tails from the strict-prefix
    /// `last_seq + 1`, so seqs past the skip are re-fed to the
    /// fold function — fold functions that are not idempotent
    /// against re-application will produce divergent state on
    /// restore. The mitigations: (1) make the fold idempotent
    /// (the standard event-sourcing recommendation); (2)
    /// snapshot only when `applied_through_seq() ==
    /// folded_through_seq()` (no gap → no re-apply); or (3)
    /// accept best-effort restore semantics for adapters that
    /// have ever observed a recoverable_decode skip. The
    /// trade-off vs. the pre-fix behavior is asymmetric:
    /// pre-fix, the skipped seq was permanently lost; post-fix,
    /// the skipped seq is re-attempted, at the cost of
    /// double-applying intervening successful seqs for
    /// non-idempotent folds.
    ///
    /// `last_seq` is `None` if no event has been applied yet
    /// since open (the snapshot is still meaningful — it
    /// represents the initial State — but callers typically wait
    /// until [`Self::wait_for_seq`] has returned and
    /// [`Self::applied_through_seq`] has advanced before
    /// snapshotting).
    pub fn snapshot(&self) -> Result<(Vec<u8>, Option<u64>), CortexAdapterError> {
        let state = self.inner.state.read();
        let bytes = postcard::to_allocvec(&*state).map_err(|e| {
            CortexAdapterError::Redex(RedexError::Encode(format!("snapshot serialize: {}", e)))
        })?;
        let watermark = self.inner.applied_through_seq.load(Ordering::Acquire);
        let last_seq = if watermark == u64::MAX {
            None
        } else {
            Some(watermark)
        };
        Ok((bytes, last_seq))
    }
}

impl<State> CortexAdapter<State>
where
    State: DeserializeOwned + Send + Sync + 'static,
{
    /// Open an adapter from a previously-captured snapshot, skipping
    /// the `[0, last_seq]` replay.
    ///
    /// `state_bytes` is the blob returned from [`Self::snapshot`].
    /// `last_seq` is its companion sequence. The tail starts at
    /// `last_seq + 1`; the initial state is deserialized from the
    /// blob; the fold task is spawned as usual.
    ///
    /// If `last_seq` is `None` (no events had been folded at
    /// snapshot time), the tail starts at seq 0 — equivalent to
    /// `StartPosition::FromBeginning` with the deserialized initial
    /// state.
    pub fn open_from_snapshot<F>(
        redex: &Redex,
        name: &ChannelName,
        redex_config: RedexFileConfig,
        adapter_config: CortexAdapterConfig,
        fold: F,
        state_bytes: &[u8],
        last_seq: Option<u64>,
    ) -> Result<Self, CortexAdapterError>
    where
        F: RedexFold<State> + Send + 'static,
    {
        let initial_state: State = postcard::from_bytes(state_bytes).map_err(|e| {
            CortexAdapterError::Redex(RedexError::Encode(format!("deserialize snapshot: {}", e)))
        })?;
        let start = match last_seq {
            Some(n) => {
                let next = n.checked_add(1).ok_or_else(|| {
                    CortexAdapterError::Redex(RedexError::Encode(
                        "snapshot last_seq at u64::MAX; cannot resume".to_string(),
                    ))
                })?;
                StartPosition::FromSeq(next)
            }
            None => StartPosition::FromBeginning,
        };
        let config = CortexAdapterConfig {
            start,
            on_fold_error: adapter_config.on_fold_error,
        };
        // Route through `open_unchecked` so the externally-
        // rehydrated state can skip its event prefix.
        Self::open_unchecked(redex, name, redex_config, config, fold, initial_state)
    }
}

impl<State> Clone for CortexAdapter<State> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<State> std::fmt::Debug for CortexAdapter<State> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CortexAdapter")
            .field("folded_through_seq", &self.folded_through_seq())
            .field("applied_through_seq", &self.applied_through_seq())
            .field("fold_errors", &self.fold_errors())
            .field("running", &self.is_running())
            .field("closed", &self.inner.closed.load(Ordering::Acquire))
            .finish()
    }
}

/// Apply one event. Returns `true` if the task should exit
/// (Stop policy + error).
fn handle_event<State, F>(
    inner: &Arc<AdapterInner<State>>,
    fold: &mut F,
    event: &RedexEvent,
    policy: FoldErrorPolicy,
) -> bool
where
    F: RedexFold<State>,
{
    let seq = event.entry.seq;
    // Hold the write lock across both the fold and the watermark
    // update so that a `snapshot()` holding `state.read()` observes
    // a consistent `(state, folded_through_seq)` pair — otherwise
    // the state could reflect seq N while the watermark still reads
    // N-1, causing restore to double-apply event N.
    let result = {
        let mut state = inner.state.write();
        let r = fold.apply(event, &mut state);
        // Under `Stop` policy, a per-event recoverable decode
        // failure (postcard error, EventMeta shape mismatch —
        // anything `RedexError::is_recoverable_decode` flags) is
        // treated as skip-and-continue rather than halting. Halting
        // on every such failure would let a single bad event (disk
        // corruption past the 32-bit checksum, or a
        // deliberately-crafted matching-collision tail) wedge the
        // fold task permanently — a DoS vector against multi-tenant
        // cortex instances via one bad event. Stream-level errors
        // (`Io`, `Closed`, `Lagged`) and authorization /
        // configuration errors still halt under `Stop` as
        // documented.
        //
        // Two watermarks; both written under the state write lock
        // so a concurrent `snapshot` reading either one observes a
        // value consistent with the state it sees:
        //   - `applied_through_seq` is a STRICT-PREFIX watermark:
        //     it advances by exactly one only when this seq is
        //     the immediate successor of the current value (or
        //     equals `start_seq` from the sentinel state). Any
        //     skip — `recoverable_decode` under `Stop`, OR any
        //     error under `LogAndContinue` — breaks the prefix
        //     at the skipped seq; further successful applies
        //     cannot heal it. `snapshot` persists this watermark
        //     so restore tails from a position guaranteed to
        //     have every prior seq reflected in state.
        //   - `folded_through_seq` is the HIGHEST PROCESSED
        //     watermark: advances on `Ok(())` AND on every skip
        //     path. Live consumers waiting via `wait_for_seq` use
        //     this watermark so a single bad event doesn't
        //     deadlock them — the DoS-resistance contract.
        // Why two watermarks, not one strict-prefix that lives
        // consumers also wait on: collapsing them re-introduces
        // the deadlock — a consumer waiting for the skipped seq
        // would block forever even though the fold task has
        // moved on.
        // Order: `applied` is stored first so any reader that
        // observes the `folded` Release also synchronizes-with
        // the `applied` Release that preceded it — `applied <=
        // folded` is therefore an invariant at every observable
        // point.
        let applied = matches!(&r, Ok(()));
        let recoverable_decode = matches!(&r, Err(e) if e.is_recoverable_decode());
        let advance = applied
            || matches!((&r, policy), (Err(_), FoldErrorPolicy::LogAndContinue))
            || recoverable_decode;
        if applied {
            // Strict-prefix advance: bump by exactly one if this
            // seq is the immediate successor of the current
            // applied watermark (or this is the very first event
            // and matches `start_seq`). Otherwise the prefix is
            // broken by a prior skip; leave the watermark alone
            // and let restore re-attempt from the gap.
            let prev = inner.applied_through_seq.load(Ordering::Acquire);
            let advance_applied = if prev == u64::MAX {
                seq == inner.start_seq
            } else {
                seq == prev.wrapping_add(1)
            };
            if advance_applied {
                inner.applied_through_seq.store(seq, Ordering::Release);
            }
        }
        if advance {
            inner.folded_through_seq.store(seq, Ordering::Release);
        }
        r
    };

    match result {
        Ok(()) => {
            inner.notify.notify_waiters();
            let _ = inner.changes_tx.send(seq);
            false
        }
        Err(err) => {
            inner.fold_errors.fetch_add(1, Ordering::AcqRel);
            tracing::warn!(seq = seq, error = %err, "cortex fold error");
            // Per-event decode errors always skip-and-continue;
            // only stream-level / configuration errors halt under
            // `Stop`.
            let recoverable_decode = err.is_recoverable_decode();
            match policy {
                FoldErrorPolicy::Stop if !recoverable_decode => {
                    // Wake subscribers via `notify_waiters` so
                    // anything parked on `inner.notify` unblocks
                    // and can observe the halt via
                    // `is_running()`. Do NOT broadcast `seq` on
                    // `changes_tx`: this branch did not advance
                    // `folded_through_seq` (the `advance` gate
                    // above is false for `Stop + non-recoverable`),
                    // and `changes_tx` is documented as carrying
                    // *successful fold-apply* notifications. A
                    // `ChangeEvent::Seq(seq)` for an unapplied
                    // sequence would mislead consumers into
                    // thinking the watermark advanced past the
                    // failure — the very mis-routing the broadcast
                    // contract was designed to avoid.
                    //
                    // The trade-off: subscribers using
                    // `changes_with_lag()` won't see a terminal
                    // event in the stream on halt; they need to
                    // poll `is_running()` separately (or rely on
                    // the broadcast channel ending when all adapter
                    // handles are dropped). That's the documented
                    // failure mode for non-recoverable halts —
                    // surfacing a phantom seq was not.
                    inner.notify.notify_waiters();
                    true
                }
                FoldErrorPolicy::Stop | FoldErrorPolicy::LogAndContinue => {
                    // Watermark was already advanced inside the lock
                    // above; just notify waiters.
                    inner.notify.notify_waiters();
                    let _ = inner.changes_tx.send(seq);
                    false
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::channel::ChannelName;
    use super::super::super::redex::{RedexError, RedexFold};
    use super::super::envelope::EventEnvelope;
    use super::super::meta::EventMeta;
    use super::*;
    use bytes::Bytes;

    fn cn(s: &str) -> ChannelName {
        ChannelName::new(s).unwrap()
    }

    struct CountFold;
    impl RedexFold<u64> for CountFold {
        fn apply(&mut self, _ev: &RedexEvent, state: &mut u64) -> Result<(), RedexError> {
            *state += 1;
            Ok(())
        }
    }

    /// Pin: `wait_for_seq(seq)` short-circuits without blocking
    /// when `seq < start_seq` — those events were applied before
    /// the adapter opened (e.g. they're folded into the snapshot
    /// the caller passed to `open_from_snapshot`). Pre-fix the
    /// function used only the `watermark >= seq` check; until at
    /// least one event landed under the new adapter, the
    /// `u64::MAX` "nothing folded yet" sentinel kept the
    /// comparison false and a caller waiting for a stale seq
    /// would block forever.
    #[tokio::test]
    async fn wait_for_seq_short_circuits_below_start_seq() {
        let redex = Redex::new();
        // Pre-populate the file with 5 events via a temporary
        // FromBeginning adapter, then snapshot.
        let bytes;
        let last_seq;
        {
            let pre = CortexAdapter::<u64>::open(
                &redex,
                &cn("cortex/short-circuit"),
                RedexFileConfig::default(),
                CortexAdapterConfig::default(),
                CountFold,
                0u64,
            )
            .unwrap();
            for i in 0..5u64 {
                let meta = EventMeta::new(1, 0, 1, i, 0);
                let env = EventEnvelope::new(meta, Bytes::from_static(b""));
                let seq = pre.ingest(env).unwrap();
                pre.wait_for_seq(seq).await;
            }
            let (b, ls) = pre.snapshot().unwrap();
            bytes = b;
            last_seq = ls;
            pre.close().unwrap();
        }

        // Restore from snapshot: `start_seq` is `last_seq + 1 =
        // 5` (the snapshot already absorbed seqs 0..=4). Any
        // wait_for_seq below 5 is conceptually behind us.
        let adapter = CortexAdapter::<u64>::open_from_snapshot(
            &redex,
            &cn("cortex/short-circuit"),
            RedexFileConfig::default(),
            CortexAdapterConfig::default(),
            CountFold,
            &bytes,
            last_seq,
        )
        .unwrap();

        // wait_for_seq(0..5) must all return immediately.
        // Wrap each in a tight timeout — pre-fix behavior was
        // an indefinite block.
        for seq in 0..5u64 {
            tokio::time::timeout(std::time::Duration::from_secs(2), adapter.wait_for_seq(seq))
                .await
                .unwrap_or_else(|_| {
                    panic!(
                        "wait_for_seq({}) blocked past start_seq=5 — \
                     short-circuit regressed",
                        seq
                    )
                });
        }
    }

    #[tokio::test]
    async fn test_open_ingest_wait_query() {
        let redex = Redex::new();
        let adapter = CortexAdapter::<u64>::open(
            &redex,
            &cn("cortex/counts"),
            RedexFileConfig::default(),
            CortexAdapterConfig::default(),
            CountFold,
            0u64,
        )
        .unwrap();

        for i in 0..10u64 {
            let meta = EventMeta::new(1, 0, 1, i, 0);
            let env = EventEnvelope::new(meta, Bytes::from_static(b""));
            let seq = adapter.ingest(env).unwrap();
            adapter.wait_for_seq(seq).await;
        }

        assert_eq!(*adapter.state().read(), 10);
        assert_eq!(adapter.fold_errors(), 0);
        assert!(adapter.is_running());
    }

    #[tokio::test]
    async fn test_close_stops_fold_task() {
        let redex = Redex::new();
        let adapter = CortexAdapter::<u64>::open(
            &redex,
            &cn("cortex/close"),
            RedexFileConfig::default(),
            CortexAdapterConfig::default(),
            CountFold,
            0u64,
        )
        .unwrap();

        adapter.close().unwrap();
        // Close is idempotent.
        adapter.close().unwrap();

        // Ingest after close returns Closed.
        let meta = EventMeta::new(0, 0, 0, 0, 0);
        let env = EventEnvelope::new(meta, Bytes::from_static(b""));
        let err = adapter.ingest(env).unwrap_err();
        assert!(matches!(err, CortexAdapterError::Closed));

        // State handle still readable.
        assert_eq!(*adapter.state().read(), 0);
    }

    struct FailAtSeq(u64);
    impl RedexFold<u64> for FailAtSeq {
        fn apply(&mut self, ev: &RedexEvent, state: &mut u64) -> Result<(), RedexError> {
            if ev.entry.seq == self.0 {
                Err(RedexError::Encode(format!(
                    "deliberate failure at seq {}",
                    ev.entry.seq
                )))
            } else {
                *state += 1;
                Ok(())
            }
        }
    }

    /// Returns `RedexError::Decode` (recoverable) at the
    /// configured seqs; counts the attempt so tests can assert
    /// re-attempt after restore. Apply on any other seq bumps
    /// state.
    struct FailDecodeAtSeqs {
        skip: std::collections::HashSet<u64>,
        attempts: Arc<AtomicU64>,
    }
    impl RedexFold<u64> for FailDecodeAtSeqs {
        fn apply(&mut self, ev: &RedexEvent, state: &mut u64) -> Result<(), RedexError> {
            self.attempts.fetch_add(1, Ordering::AcqRel);
            if self.skip.contains(&ev.entry.seq) {
                Err(RedexError::Decode(format!(
                    "deliberate decode skip at seq {}",
                    ev.entry.seq
                )))
            } else {
                *state += 1;
                Ok(())
            }
        }
    }

    #[tokio::test]
    async fn test_stop_policy_halts_on_first_error() {
        let redex = Redex::new();
        let adapter = CortexAdapter::<u64>::open(
            &redex,
            &cn("cortex/stop"),
            RedexFileConfig::default(),
            CortexAdapterConfig::default(), // Stop is default
            FailAtSeq(3),
            0u64,
        )
        .unwrap();

        for i in 0..10u64 {
            let meta = EventMeta::new(0, 0, 0, i, 0);
            let env = EventEnvelope::new(meta, Bytes::from_static(b""));
            adapter.ingest(env).unwrap();
        }

        // Wait until fold task stops. wait_for_seq returns on stop
        // even if the seq isn't reached.
        adapter.wait_for_seq(10).await;
        assert!(!adapter.is_running());
        assert_eq!(adapter.fold_errors(), 1);
        // Seqs 0..=2 folded; seq 3 errored; seqs 4..=9 never folded.
        assert_eq!(*adapter.state().read(), 3);
    }

    /// `changes_tx` is the broadcast channel `changes_with_lag`
    /// surfaces as `ChangeEvent::Seq(u64)` — documented as
    /// "successful fold-apply notification". On the
    /// Stop+non-recoverable halt path, the watermark
    /// (`folded_through_seq`) is NOT advanced, so emitting the
    /// failing seq on `changes_tx` would mis-represent an
    /// unapplied event as if it were folded. Subscribers reading
    /// the broadcast and trusting the contract would advance
    /// their own state machines past the failure.
    ///
    /// Pin: after a Stop-policy halt, the broadcast must contain
    /// the prefix that *did* apply (seqs 0..=2), and must NOT
    /// contain the failing seq (3) or any later seq.
    #[tokio::test]
    async fn stop_policy_does_not_broadcast_failing_seq() {
        use futures::StreamExt;

        let redex = Redex::new();
        let adapter = CortexAdapter::<u64>::open(
            &redex,
            &cn("cortex/stop-no-phantom-seq"),
            RedexFileConfig::default(),
            CortexAdapterConfig::default(), // Stop is default
            FailAtSeq(3),
            0u64,
        )
        .unwrap();

        // Subscribe BEFORE ingesting so we capture every seq.
        let mut changes = adapter.changes_with_lag();

        for i in 0..10u64 {
            let meta = EventMeta::new(0, 0, 0, i, 0);
            let env = EventEnvelope::new(meta, Bytes::from_static(b""));
            adapter.ingest(env).unwrap();
        }

        // Wait for halt.
        adapter.wait_for_seq(10).await;
        assert!(!adapter.is_running(), "Stop policy must halt the task");
        assert_eq!(adapter.fold_errors(), 1);

        // Drain the broadcast with a short bound so a regression
        // that re-emits the phantom seq doesn't hang the test.
        let mut received: Vec<u64> = Vec::new();
        loop {
            match tokio::time::timeout(std::time::Duration::from_millis(50), changes.next()).await {
                Ok(Some(ChangeEvent::Seq(s))) => received.push(s),
                Ok(Some(ChangeEvent::Lagged(_))) => continue,
                Ok(None) | Err(_) => break,
            }
        }

        // Successful prefix (0, 1, 2) must be visible. The failing
        // seq (3) and any later seq must NOT.
        assert_eq!(
            received,
            vec![0, 1, 2],
            "broadcast must carry only successfully-folded seqs; \
             pre-fix this would include 3 (the failing seq) as a \
             phantom Seq(3) event, mis-routing subscribers' state"
        );
    }

    // ========================================================================
    // applied_through_seq vs folded_through_seq
    // ========================================================================

    /// Under `Stop` policy, a per-event recoverable decode error
    /// advances `folded_through_seq` (so live consumers don't
    /// deadlock on a permanently-bad event) but must NOT advance
    /// `applied_through_seq` (state at the skipped seq was never
    /// written). When the two were the same atomic,
    /// `wait_for_seq(seq)` returned claiming "state reflects seq"
    /// for events whose mutation never landed.
    #[tokio::test]
    async fn recoverable_decode_skip_advances_folded_but_not_applied() {
        let redex = Redex::new();
        let attempts = Arc::new(AtomicU64::new(0));
        let fold = FailDecodeAtSeqs {
            skip: [3u64].into_iter().collect(),
            attempts: attempts.clone(),
        };
        let adapter = CortexAdapter::<u64>::open(
            &redex,
            &cn("cortex/audit-6-skip"),
            RedexFileConfig::default(),
            CortexAdapterConfig::default(), // Stop is default
            fold,
            0u64,
        )
        .unwrap();

        for i in 0..6u64 {
            let meta = EventMeta::new(0, 0, 0, i, 0);
            let env = EventEnvelope::new(meta, Bytes::from_static(b""));
            adapter.ingest(env).unwrap();
        }
        adapter.wait_for_seq(5).await;

        // Live consumer didn't deadlock — folded reached the tail.
        assert_eq!(
            adapter.folded_through_seq(),
            Some(5),
            "folded must advance over the recoverable_decode skip",
        );
        // STRICT-PREFIX: applied caps at 2 (the last consecutive
        // successful seq before the gap at seq 3). Subsequent
        // successful applies at 4 and 5 do NOT heal the prefix.
        assert_eq!(
            adapter.applied_through_seq(),
            Some(2),
            "strict-prefix watermark caps at the last consecutive Ok before the skip",
        );
        // State count = 5 (seqs 0,1,2,4,5 applied; 3 skipped).
        // The fold function still mutated state for the seqs it
        // saw — only the WATERMARK is strict-prefix.
        assert_eq!(*adapter.state().read(), 5);
        assert_eq!(adapter.fold_errors(), 1);
        // Task still running — recoverable decode does not halt
        // under Stop.
        assert!(adapter.is_running());
        // Six attempts (one per event). The `attempts` counter
        // pins that the fold function was invoked for the
        // skipped seq (skip is detected inside the fold, not
        // upstream).
        assert_eq!(attempts.load(Ordering::Acquire), 6);
    }

    /// When the FIRST event is skipped via
    /// recoverable_decode, `applied_through_seq`
    /// stays at the "nothing applied yet" sentinel until a
    /// subsequent successful fold. This is the strict shape: an
    /// adapter freshly opened that has only seen skipped events
    /// reports `None` from `applied_through_seq`, even though
    /// `folded_through_seq` reports the highest skipped seq.
    #[tokio::test]
    async fn applied_stays_at_sentinel_when_only_skipped_events_processed() {
        let redex = Redex::new();
        let attempts = Arc::new(AtomicU64::new(0));
        let fold = FailDecodeAtSeqs {
            skip: [0u64, 1, 2].into_iter().collect(),
            attempts: attempts.clone(),
        };
        let adapter = CortexAdapter::<u64>::open(
            &redex,
            &cn("cortex/audit-6-only-skips"),
            RedexFileConfig::default(),
            CortexAdapterConfig::default(),
            fold,
            0u64,
        )
        .unwrap();

        for i in 0..3u64 {
            let meta = EventMeta::new(0, 0, 0, i, 0);
            let env = EventEnvelope::new(meta, Bytes::from_static(b""));
            adapter.ingest(env).unwrap();
        }
        adapter.wait_for_seq(2).await;

        assert_eq!(adapter.folded_through_seq(), Some(2));
        assert_eq!(
            adapter.applied_through_seq(),
            None,
            "no successful apply ⇒ applied_through_seq stays at sentinel",
        );
        assert_eq!(*adapter.state().read(), 0);
    }

    /// `snapshot()` must use `applied_through_seq`, not
    /// `folded_through_seq`. When the two were the same atomic,
    /// snapshot persisted the highest folded seq — including
    /// skipped events — so a restore tailed from past the skip
    /// and the skipped seq became permanently lost from durable
    /// state, even though the in-memory state at snapshot time
    /// never reflected it.
    #[tokio::test]
    async fn snapshot_last_seq_reflects_applied_not_folded_after_skip() {
        let redex = Redex::new();
        let attempts = Arc::new(AtomicU64::new(0));
        let fold = FailDecodeAtSeqs {
            skip: [5u64].into_iter().collect(),
            attempts: attempts.clone(),
        };
        let adapter = CortexAdapter::<u64>::open(
            &redex,
            &cn("cortex/audit-7-snapshot"),
            RedexFileConfig::default(),
            CortexAdapterConfig::default(),
            fold,
            0u64,
        )
        .unwrap();

        // 8 events; seq 5 will be skipped via recoverable_decode.
        for i in 0..8u64 {
            let meta = EventMeta::new(0, 0, 0, i, 0);
            let env = EventEnvelope::new(meta, Bytes::from_static(b""));
            adapter.ingest(env).unwrap();
        }
        adapter.wait_for_seq(7).await;

        let folded = adapter.folded_through_seq();
        let applied = adapter.applied_through_seq();
        assert_eq!(folded, Some(7), "folded includes the skipped seq's slot");
        assert_eq!(
            applied,
            Some(4),
            "strict-prefix applied caps at the last consecutive Ok (seq 4) before the gap at seq 5",
        );

        let (_bytes, last_seq) = adapter.snapshot().unwrap();
        // snapshot reflects applied (strict-prefix), not folded.
        // Pre-fix snapshot returned Some(7) — restore would tail
        // from seq 8 and seq 5 would be permanently lost from
        // durable state.
        assert_eq!(last_seq, applied);
        assert_eq!(last_seq, Some(4));
    }

    /// Strict shape: when the LAST processed event is skipped,
    /// snapshot's `last_seq` must stay at the highest *applied*
    /// seq — not advance to the skipped one. Without the split,
    /// snapshot returned the skipped seq; restore tailed from
    /// past it; skipped event was lost.
    #[tokio::test]
    async fn snapshot_last_seq_does_not_advance_to_a_skipped_tail() {
        let redex = Redex::new();
        let attempts = Arc::new(AtomicU64::new(0));
        let fold = FailDecodeAtSeqs {
            // Skip the LAST event so applied < folded at snapshot time.
            skip: [4u64].into_iter().collect(),
            attempts: attempts.clone(),
        };
        let adapter = CortexAdapter::<u64>::open(
            &redex,
            &cn("cortex/audit-7-tail-skip"),
            RedexFileConfig::default(),
            CortexAdapterConfig::default(),
            fold,
            0u64,
        )
        .unwrap();

        for i in 0..5u64 {
            let meta = EventMeta::new(0, 0, 0, i, 0);
            let env = EventEnvelope::new(meta, Bytes::from_static(b""));
            adapter.ingest(env).unwrap();
        }
        adapter.wait_for_seq(4).await;

        assert_eq!(adapter.folded_through_seq(), Some(4));
        assert_eq!(
            adapter.applied_through_seq(),
            Some(3),
            "tail apply was skipped ⇒ applied stays at the prior successful seq",
        );

        let (_bytes, last_seq) = adapter.snapshot().unwrap();
        assert_eq!(
            last_seq,
            Some(3),
            "pre-fix this would be Some(4) and a restore would tail from 5, \
             dropping seq 4 permanently from durable state",
        );
    }

    /// End-to-end: an adapter whose fold previously skipped seq
    /// N via recoverable_decode is snapshotted, closed, and
    /// reopened via `open_from_snapshot` with a fold that NOW
    /// handles seq N successfully (e.g. a software upgrade fixed
    /// the decoder). On restore, seq N must be re-fed to the
    /// fold function — without the strict-prefix watermark,
    /// snapshot's `last_seq` would be the highest folded
    /// (advanced over skips), restore would tail from past the
    /// skipped seq, and the previously-skipped event would be
    /// permanently lost.
    ///
    /// Asserts the strict-prefix `last_seq` and that the skipped
    /// seq is re-fed on restore. State-count after restore is
    /// fold-dependent (see `snapshot` doc on the re-apply
    /// double-counting trade-off) and is not pinned here.
    #[tokio::test]
    async fn restore_after_skip_re_attempts_skipped_event() {
        let redex = Redex::new();
        let pre_attempts = Arc::new(AtomicU64::new(0));
        let pre_fold = FailDecodeAtSeqs {
            skip: [2u64].into_iter().collect(),
            attempts: pre_attempts.clone(),
        };
        let adapter = CortexAdapter::<u64>::open(
            &redex,
            &cn("cortex/audit-6-7-restore-reattempt"),
            RedexFileConfig::default(),
            CortexAdapterConfig::default(),
            pre_fold,
            0u64,
        )
        .unwrap();

        for i in 0..5u64 {
            let meta = EventMeta::new(0, 0, 0, i, 0);
            let env = EventEnvelope::new(meta, Bytes::from_static(b""));
            adapter.ingest(env).unwrap();
        }
        adapter.wait_for_seq(4).await;

        // Pre-snapshot state: STRICT-PREFIX applied caps at 1
        // (seq 2 was skipped; the prefix breaks there). Folded
        // reached 4. The fold function still wrote state for
        // every Ok (0,1,3,4) so state count is 4.
        assert_eq!(adapter.applied_through_seq(), Some(1));
        assert_eq!(adapter.folded_through_seq(), Some(4));
        assert_eq!(*adapter.state().read(), 4);
        let (bytes, last_seq) = adapter.snapshot().unwrap();
        // snapshot must reflect applied (strict-prefix), so
        // restore tails from seq 2 and re-attempts the skipped
        // event plus everything after.
        assert_eq!(last_seq, Some(1));
        adapter.close().unwrap();

        // Reopen with a fold that handles seq 2 successfully
        // this time (no entries in `skip`). The previously-
        // skipped event must be re-fed, AND so must seqs 3 and
        // 4 (because their prior application is not part of the
        // snapshot's strict-prefix watermark — only seqs 0..=1
        // are guaranteed reflected in the rehydrated state).
        let post_attempts = Arc::new(AtomicU64::new(0));
        let post_fold = FailDecodeAtSeqs {
            skip: std::collections::HashSet::new(),
            attempts: post_attempts.clone(),
        };
        let restored = CortexAdapter::<u64>::open_from_snapshot(
            &redex,
            &cn("cortex/audit-6-7-restore-reattempt"),
            RedexFileConfig::default(),
            CortexAdapterConfig::default(),
            post_fold,
            &bytes,
            last_seq,
        )
        .unwrap();

        restored.wait_for_seq(4).await;

        // Pre-fix this assertion would FAIL: snapshot would have
        // returned Some(4) (folded), restore would tail from
        // seq 5, the post_fold would never see seq 2, and the
        // skipped event would be permanently lost.
        let post_invocations = post_attempts.load(Ordering::Acquire);
        assert_eq!(
            post_invocations, 3,
            "post-restore fold must be re-fed seqs 2, 3, 4 (everything past the \
             snapshot's strict-prefix watermark)",
        );
        // After restore, the strict-prefix watermark heals all
        // the way to seq 4 because every event from start_seq=2
        // onwards applied successfully on the second pass.
        assert_eq!(restored.applied_through_seq(), Some(4));
        // State count is fold-dependent on idempotency. CountFold
        // is *not* idempotent (each apply increments), so seqs
        // 3 and 4 are double-counted: pre-restore in-memory
        // state was 4 (seqs 0,1,3,4 applied), snapshot bytes
        // serialized that 4, restore deserializes 4 + applies
        // seqs 2,3,4 = 7. The audit-fix's *correctness* claim is
        // not "state matches what a re-fold from scratch would
        // produce" — that requires either an idempotent fold or
        // snapshotting only when there's no gap (see `snapshot`
        // doc). The claim is "the skipped seq is re-fed instead
        // of being permanently lost from durable state" — pinned
        // by `post_invocations == 3` above.
        assert_eq!(
            *restored.state().read(),
            7,
            "non-idempotent CountFold double-counts seqs past the gap; this is the \
             documented re-apply trade-off, not a bug",
        );
    }

    /// Pin: `wait_for_seq(seq)` returns promptly even when seq
    /// was skipped via recoverable_decode. The DoS-resistance
    /// contract (one bad event must not deadlock live consumers)
    /// is preserved by the `applied`/`folded` split — `wait_for_seq`
    /// still waits on `folded`, which advances over skips.
    #[tokio::test]
    async fn wait_for_seq_returns_after_recoverable_decode_skip() {
        let redex = Redex::new();
        let attempts = Arc::new(AtomicU64::new(0));
        let fold = FailDecodeAtSeqs {
            skip: [0u64].into_iter().collect(),
            attempts: attempts.clone(),
        };
        let adapter = CortexAdapter::<u64>::open(
            &redex,
            &cn("cortex/audit-6-no-deadlock"),
            RedexFileConfig::default(),
            CortexAdapterConfig::default(),
            fold,
            0u64,
        )
        .unwrap();

        let meta = EventMeta::new(0, 0, 0, 0, 0);
        let env = EventEnvelope::new(meta, Bytes::from_static(b""));
        let seq = adapter.ingest(env).unwrap();

        // wait_for_seq must NOT block on the skipped seq.
        // Tight timeout — pre-fix shape never had this hazard;
        // the post-fix split must preserve it. (If `wait_for_seq`
        // ever silently switches to `applied_through_seq`, this
        // regresses with an indefinite hang.)
        tokio::time::timeout(std::time::Duration::from_secs(2), adapter.wait_for_seq(seq))
            .await
            .expect("wait_for_seq must return promptly even for a recoverable-decode-skipped seq");

        // Confirm what was actually observable at return:
        // folded reached seq, applied did not.
        assert_eq!(adapter.folded_through_seq(), Some(0));
        assert_eq!(adapter.applied_through_seq(), None);
    }

    // ========================================================================
    // open must reject FromSeq(n>0) / LiveOnly
    // ========================================================================

    /// `open` rejects `StartPosition::FromSeq(n)` for n > 0
    /// because the watermark would advance past events the adapter
    /// never folded, leaving `wait_for_seq` lying about applied
    /// state. Callers that intentionally skip a prefix must use
    /// `open_from_snapshot`.
    #[test]
    fn open_rejects_from_seq_n_greater_than_zero() {
        let redex = Redex::new();
        let cfg = CortexAdapterConfig::new().with_start(StartPosition::FromSeq(5));
        let result = CortexAdapter::<u64>::open(
            &redex,
            &cn("cortex/from-seq-guard"),
            RedexFileConfig::default(),
            cfg,
            CountFold,
            0u64,
        );
        assert!(
            matches!(result, Err(CortexAdapterError::InvalidStartPosition(_))),
            "open must reject FromSeq(n>0), got {:?}",
            result.map(|_| "Ok"),
        );
    }

    /// `open` rejects `StartPosition::LiveOnly` for the same
    /// reason — the start_seq is `file.next_seq()`, so any prior
    /// events go un-folded but the watermark advances past them.
    #[test]
    fn open_rejects_live_only_start_position() {
        let redex = Redex::new();
        let cfg = CortexAdapterConfig::new().with_start(StartPosition::LiveOnly);
        let result = CortexAdapter::<u64>::open(
            &redex,
            &cn("cortex/live-only-guard"),
            RedexFileConfig::default(),
            cfg,
            CountFold,
            0u64,
        );
        assert!(
            matches!(result, Err(CortexAdapterError::InvalidStartPosition(_))),
            "open must reject LiveOnly, got {:?}",
            result.map(|_| "Ok"),
        );
    }

    // ========================================================================
    // Stop policy must skip-and-continue on per-event Decode errors
    // ========================================================================

    struct FailDecodeAtSeq(u64);
    impl RedexFold<u64> for FailDecodeAtSeq {
        fn apply(&mut self, ev: &RedexEvent, state: &mut u64) -> Result<(), RedexError> {
            if ev.entry.seq == self.0 {
                // Decode-class error: simulates a corrupt postcard
                // tail / EventMeta shape mismatch / checksum miss
                // — exactly what the cortex fold paths surface as
                // RedexError::Decode.
                Err(RedexError::Decode(format!(
                    "deliberate decode failure at seq {}",
                    ev.entry.seq
                )))
            } else {
                *state += 1;
                Ok(())
            }
        }
    }

    /// Under `Stop` policy, a `RedexError::Decode` MUST NOT halt
    /// the fold task — it's a per-event recoverable failure
    /// (corrupt event payload past the 32-bit checksum, or an
    /// attacker-crafted matching collision). Pre-fix this hung
    /// the task on the first bad event, DoSing the cortex via one
    /// payload. Post-fix: the bad event is logged + skipped, the
    /// watermark advances, and subsequent events still fold.
    #[tokio::test]
    async fn stop_policy_skips_recoverable_decode_error() {
        let redex = Redex::new();
        let adapter = CortexAdapter::<u64>::open(
            &redex,
            &cn("cortex/decode-skip"),
            RedexFileConfig::default(),
            CortexAdapterConfig::default(), // Stop is default
            FailDecodeAtSeq(3),
            0u64,
        )
        .unwrap();

        for i in 0..10u64 {
            let meta = EventMeta::new(0, 0, 0, i, 0);
            let env = EventEnvelope::new(meta, Bytes::from_static(b""));
            let seq = adapter.ingest(env).unwrap();
            adapter.wait_for_seq(seq).await;
        }

        // Fold task is still running — the decode error didn't
        // halt it. fold_errors counts the one bad event.
        assert!(
            adapter.is_running(),
            "Stop policy must NOT halt on RedexError::Decode"
        );
        assert_eq!(adapter.fold_errors(), 1);
        // Seqs 0,1,2,4,5,6,7,8,9 folded; seq 3 skipped.
        assert_eq!(*adapter.state().read(), 9);
    }

    /// `Encode` errors (storage / user-fold-level) STILL halt
    /// under `Stop` — pins the conservative boundary so the
    /// recoverable-decode carve-out is strictly limited to per-event
    /// decode failures. The pre-existing `test_stop_policy_halts_on_first_error`
    /// already exercises this with `RedexError::Encode`, but we
    /// pin the contract explicitly here so a future expansion of
    /// `is_recoverable_decode` (e.g. accidentally including
    /// `Encode`) is caught.
    #[test]
    fn redex_error_recoverable_decode_classification_is_decode_only() {
        assert!(RedexError::Decode("x".into()).is_recoverable_decode());
        assert!(!RedexError::Encode("x".into()).is_recoverable_decode());
        assert!(!RedexError::Closed.is_recoverable_decode());
        assert!(!RedexError::Io("x".into()).is_recoverable_decode());
        assert!(!RedexError::Lagged.is_recoverable_decode());
        assert!(!RedexError::Unauthorized.is_recoverable_decode());
    }

    /// `FromSeq(0)` is equivalent to `FromBeginning` (no events
    /// skipped) and must still be accepted — pins the boundary so
    /// the start-position guard doesn't accidentally lock out the
    /// degenerate-but-valid `FromSeq(0)` form.
    #[tokio::test]
    async fn open_accepts_from_seq_zero() {
        let redex = Redex::new();
        let cfg = CortexAdapterConfig::new().with_start(StartPosition::FromSeq(0));
        CortexAdapter::<u64>::open(
            &redex,
            &cn("cortex/from-seq-zero"),
            RedexFileConfig::default(),
            cfg,
            CountFold,
            0u64,
        )
        .expect("FromSeq(0) is equivalent to FromBeginning and must be accepted");
    }

    // ========================================================================
    // changes_with_lag must surface BroadcastStream::Lagged
    // ========================================================================

    /// `changes_with_lag` yields a `ChangeEvent::Lagged(n)` when a
    /// subscriber falls behind the broadcast channel capacity. Pre-
    /// fix `changes()` silently dropped these events via
    /// `filter_map(|r| r.ok())`; downstream telemetry consumers had
    /// no way to detect or count missed change notifications.
    ///
    /// Setup: subscribe, then ingest more than CHANGES_BROADCAST_CAP
    /// (64) events without polling the stream. The broadcast channel
    /// drops the oldest, and the next stream poll surfaces a
    /// `Lagged(n)` for the dropped count.
    #[tokio::test]
    async fn changes_with_lag_yields_lagged_when_subscriber_falls_behind() {
        let redex = Redex::new();
        let adapter = CortexAdapter::<u64>::open(
            &redex,
            &cn("cortex/lag"),
            RedexFileConfig::default(),
            CortexAdapterConfig::default(),
            CountFold,
            0u64,
        )
        .unwrap();

        let stream = adapter.changes_with_lag();
        tokio::pin!(stream);

        // Ingest CHANGES_BROADCAST_CAP + 16 events without polling
        // the stream. The broadcast channel will drop the oldest 16
        // (or thereabouts — the exact count depends on broadcast
        // semantics; we just need at least one Lagged emission).
        let total = (CHANGES_BROADCAST_CAP + 16) as u64;
        for i in 0..total {
            let meta = EventMeta::new(0, 0, 0, i, 0);
            let env = EventEnvelope::new(meta, Bytes::from_static(b""));
            let seq = adapter.ingest(env).unwrap();
            adapter.wait_for_seq(seq).await;
        }

        // First poll should see a Lagged event (the broadcast channel
        // has overflowed). Drain the stream up to a reasonable cap and
        // assert at least one Lagged event was observed.
        let mut saw_lagged = false;
        let mut saw_seq = false;
        for _ in 0..(total as usize + 4) {
            match tokio::time::timeout(std::time::Duration::from_millis(50), stream.next()).await {
                Ok(Some(ChangeEvent::Lagged(n))) => {
                    saw_lagged = true;
                    assert!(n > 0, "Lagged count must be positive");
                }
                Ok(Some(ChangeEvent::Seq(_))) => {
                    saw_seq = true;
                }
                Ok(None) | Err(_) => break,
            }
        }
        assert!(
            saw_lagged,
            "subscriber that fell behind {} events must observe Lagged",
            CHANGES_BROADCAST_CAP + 16,
        );
        assert!(
            saw_seq,
            "the stream should still emit Seq events after the lag",
        );
    }

    /// `changes()` continues to silently drop lag (the documented
    /// best-effort behavior) — pins the contract so a future
    /// refactor doesn't accidentally surface `Lagged` through the
    /// simple stream and break consumers that don't want it.
    #[tokio::test]
    async fn changes_filters_out_lag_silently() {
        let redex = Redex::new();
        let adapter = CortexAdapter::<u64>::open(
            &redex,
            &cn("cortex/lag-silent"),
            RedexFileConfig::default(),
            CortexAdapterConfig::default(),
            CountFold,
            0u64,
        )
        .unwrap();

        let stream = adapter.changes();
        tokio::pin!(stream);

        // Same overflow setup.
        let total = (CHANGES_BROADCAST_CAP + 16) as u64;
        for i in 0..total {
            let meta = EventMeta::new(0, 0, 0, i, 0);
            let env = EventEnvelope::new(meta, Bytes::from_static(b""));
            let seq = adapter.ingest(env).unwrap();
            adapter.wait_for_seq(seq).await;
        }

        // Drain everything we can from the stream. Item type is `u64`
        // (not Result), so we can't observe Lagged in any form. Just
        // verify the stream still produces some seqs without errors.
        let mut got_seq = false;
        for _ in 0..(total as usize + 4) {
            match tokio::time::timeout(std::time::Duration::from_millis(50), stream.next()).await {
                Ok(Some(_seq)) => {
                    got_seq = true;
                }
                Ok(None) | Err(_) => break,
            }
        }
        assert!(got_seq, "changes() must still emit seqs after lag");
    }

    #[tokio::test]
    async fn test_log_and_continue_skips_errors() {
        let redex = Redex::new();
        let cfg =
            CortexAdapterConfig::new().with_fold_error_policy(FoldErrorPolicy::LogAndContinue);
        let adapter = CortexAdapter::<u64>::open(
            &redex,
            &cn("cortex/lc"),
            RedexFileConfig::default(),
            cfg,
            FailAtSeq(3),
            0u64,
        )
        .unwrap();

        for i in 0..10u64 {
            let meta = EventMeta::new(0, 0, 0, i, 0);
            let env = EventEnvelope::new(meta, Bytes::from_static(b""));
            let seq = adapter.ingest(env).unwrap();
            adapter.wait_for_seq(seq).await;
        }

        assert!(adapter.is_running());
        assert_eq!(adapter.fold_errors(), 1);
        // All seqs except 3 were folded → state == 9.
        assert_eq!(*adapter.state().read(), 9);
    }
}
