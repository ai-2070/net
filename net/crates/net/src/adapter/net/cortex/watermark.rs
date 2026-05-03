//! `WatermarkingFold<S, F>` — wraps a user fold and piggybacks
//! `app_seq` discovery onto its event-traversal pass.
//!
//! Background. Both `TasksAdapter` and `MemoriesAdapter` keep a
//! per-origin monotonic counter (`app_seq`) that gets stamped on every
//! `EventMeta::seq_or_ts`. After `open_from_snapshot` the counter must
//! satisfy `app_seq > max(seq_or_ts of any in-log event for our
//! origin)` before the first `ingest_typed`, otherwise the next ingest
//! can stamp a duplicate `seq_or_ts` (data corruption — two distinct
//! events with the same per-origin sequence number).
//!
//! The typed adapters install this wrapper around the user fold so
//! discovery piggybacks on the fold task's traversal. The fold task
//! reads each event exactly once; on every successful inner-fold
//! `apply` we parse the leading [`EventMeta`] and, if the event
//! matches our `origin_hash`, advance the shared `Arc<AtomicU64>` via
//! `fetch_max(meta.seq_or_ts + 1)`. The typed constructors then
//! `wait_for_seq(replay_end - 1).await` before returning so callers
//! see a fully-ready adapter — `app_seq` is correct synchronously
//! from the caller's perspective even though it was assembled
//! asynchronously by the fold task.
//!
//! A naïve alternative — a separate synchronous `read_range` walk
//! that re-materializes every event after the inner fold task has
//! already done so — costs N redundant payload reads + N redundant
//! checksum verifications + 2N `Bytes` copies on an N-event log, and
//! is deliberately avoided here.

use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use super::super::redex::{RedexError, RedexEvent, RedexFold};
use super::meta::{EventMeta, EVENT_META_SIZE};

/// Wraps `inner: F` and updates a shared `Arc<AtomicU64>` watermark
/// from each event's [`EventMeta`] header. Only events whose
/// `origin_hash` matches `self.origin_hash` advance the counter;
/// events from other origins are folded but ignored for the
/// watermark.
///
/// `S` is the user state type passed through to the inner fold.
pub(super) struct WatermarkingFold<S, F> {
    inner: F,
    app_seq: Arc<AtomicU64>,
    origin_hash: u32,
    _state: PhantomData<fn(&mut S)>,
}

impl<S, F> WatermarkingFold<S, F> {
    pub(super) fn new(inner: F, app_seq: Arc<AtomicU64>, origin_hash: u32) -> Self {
        Self {
            inner,
            app_seq,
            origin_hash,
            _state: PhantomData,
        }
    }
}

impl<S, F> RedexFold<S> for WatermarkingFold<S, F>
where
    F: RedexFold<S>,
{
    fn apply(&mut self, ev: &RedexEvent, state: &mut S) -> Result<(), RedexError> {
        // Inner fold owns the user-visible state-update semantics. If
        // it errors we surface that verbatim — the watermark only
        // advances on a successful apply, so a fold-error policy of
        // `Continue` skips this event for both state AND watermark
        // accounting (matching the pre-fix behavior where the
        // synchronous `read_range` loop would have included the
        // event but the fold would have skipped it).
        self.inner.apply(ev, state)?;

        // Defensive payload-length guard — a payload shorter than
        // `EVENT_META_SIZE` cannot have come through `ingest_typed`
        // (which always writes a `EventMeta` prefix). Still possible
        // if a third party appends raw bytes to the same channel
        // file, in which case we silently skip rather than corrupt
        // the watermark with a bogus parse.
        if ev.payload.len() < EVENT_META_SIZE {
            return Ok(());
        }
        let Some(meta) = EventMeta::from_bytes(&ev.payload[..EVENT_META_SIZE]) else {
            return Ok(());
        };
        if meta.origin_hash != self.origin_hash {
            return Ok(());
        }

        // `fetch_max` is the right primitive: events arrive in
        // RedEX-seq order (which is NOT the same as `seq_or_ts` order
        // — two adapters writing to the same channel can interleave
        // their per-origin counters), so we want monotonic-up
        // semantics regardless of arrival order.
        //
        // Skip the watermark update if `seq_or_ts == u64::MAX`.
        // Pre-fix `saturating_add(1)` pinned `app_seq` at `u64::MAX`
        // when a peer (legitimately compromised, deliberately hostile,
        // or carrying a malformed payload that survived checksum) wrote
        // an event with `seq_or_ts == u64::MAX`. The next legitimate
        // `ingest_typed` then ran `app_seq.fetch_add(1)` on `u64::MAX`,
        // which panics in debug builds and wraps to 0 in release —
        // breaking per-origin monotonicity (two distinct events
        // stamped with the same `seq_or_ts == 0`, the canonical data
        // corruption this counter exists to prevent). Reaching
        // `seq_or_ts == u64::MAX` legitimately would require 2^64
        // ingests under one origin (unreachable in practice), so
        // every observation of it is necessarily a hostile or
        // malformed event; refusing to advance keeps `app_seq <
        // u64::MAX` and preserves the monotonicity invariant.
        if meta.seq_or_ts == u64::MAX {
            return Ok(());
        }
        let next = meta.seq_or_ts.saturating_add(1);
        self.app_seq.fetch_max(next, Ordering::AcqRel);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    //! Unit-level coverage for the `WatermarkingFold` wrapper. The
    //! integration tests in `tests/integration_cortex_{tasks,memories}.rs`
    //! exercise the wrapper end-to-end through the typed adapters; these
    //! tests pin its individual behaviors without spinning up a Redex
    //! file or fold task.
    //!
    //! We construct synthetic `RedexEvent`s directly. The entry's
    //! `seq` / `flags_and_checksum` fields don't matter for the
    //! wrapper — it only inspects `ev.payload[..EVENT_META_SIZE]`.
    use super::*;

    use bytes::Bytes;

    use super::super::super::redex::{RedexEntry, RedexEvent};

    /// Inner fold that just records every (seq, dispatch) pair the
    /// wrapper hands it, and optionally fails on a specific seq to
    /// exercise the error-propagation path.
    struct MockFold {
        seen: Vec<(u64, u8)>,
        fail_at_seq: Option<u64>,
    }

    impl MockFold {
        fn new() -> Self {
            Self {
                seen: Vec::new(),
                fail_at_seq: None,
            }
        }
        fn fail_on(seq: u64) -> Self {
            Self {
                seen: Vec::new(),
                fail_at_seq: Some(seq),
            }
        }
    }

    impl RedexFold<Vec<(u64, u8)>> for MockFold {
        fn apply(&mut self, ev: &RedexEvent, state: &mut Vec<(u64, u8)>) -> Result<(), RedexError> {
            if Some(ev.entry.seq) == self.fail_at_seq {
                return Err(RedexError::Decode("forced failure".into()));
            }
            let dispatch = ev.payload.first().copied().unwrap_or(0);
            self.seen.push((ev.entry.seq, dispatch));
            state.push((ev.entry.seq, dispatch));
            Ok(())
        }
    }

    /// Build a synthetic `RedexEvent` whose payload is `EventMeta` (with
    /// the given origin/seq_or_ts) followed by `tail`.
    fn ev_with_meta(seq: u64, origin_hash: u32, seq_or_ts: u64, tail: &[u8]) -> RedexEvent {
        let meta = EventMeta::new(0xAB, 0, origin_hash, seq_or_ts, 0);
        let mut payload = Vec::with_capacity(EVENT_META_SIZE + tail.len());
        payload.extend_from_slice(&meta.to_bytes());
        payload.extend_from_slice(tail);
        RedexEvent {
            entry: RedexEntry::new_heap(seq, 0, payload.len() as u32, 0, 0),
            payload: Bytes::from(payload),
        }
    }

    /// Build a `RedexEvent` whose payload is shorter than
    /// `EVENT_META_SIZE` — exercises the defensive guard.
    fn ev_short(seq: u64, len: usize) -> RedexEvent {
        let payload = vec![0u8; len];
        RedexEvent {
            entry: RedexEntry::new_heap(seq, 0, len as u32, 0, 0),
            payload: Bytes::from(payload),
        }
    }

    const ORIGIN_US: u32 = 0xAAAA_BBBB;
    const ORIGIN_OTHER: u32 = 0xCCCC_DDDD;

    #[test]
    fn matching_origin_advances_app_seq_via_fetch_max() {
        let app_seq = Arc::new(AtomicU64::new(0));
        let mut wf = WatermarkingFold::new(MockFold::new(), app_seq.clone(), ORIGIN_US);
        let mut state = Vec::new();

        wf.apply(&ev_with_meta(0, ORIGIN_US, 5, b""), &mut state)
            .unwrap();
        assert_eq!(app_seq.load(Ordering::Acquire), 6);
    }

    #[test]
    fn other_origin_does_not_advance_app_seq() {
        let app_seq = Arc::new(AtomicU64::new(0));
        let mut wf = WatermarkingFold::new(MockFold::new(), app_seq.clone(), ORIGIN_US);
        let mut state = Vec::new();

        wf.apply(&ev_with_meta(0, ORIGIN_OTHER, 999, b""), &mut state)
            .unwrap();
        assert_eq!(
            app_seq.load(Ordering::Acquire),
            0,
            "events from another origin must not move our watermark",
        );
        // Inner fold still saw the event — wrapper does NOT filter
        // delivery, only the watermark update.
        assert_eq!(state.len(), 1);
        assert_eq!(state[0].0, 0);
    }

    #[test]
    fn fetch_max_keeps_watermark_monotonic_under_out_of_order_seq_or_ts() {
        // Two adapters writing to the same channel can interleave their
        // per-origin counters, so a single matching-origin tail can
        // legitimately arrive in non-monotonic seq_or_ts order. The
        // watermark must track the MAX, not the latest.
        let app_seq = Arc::new(AtomicU64::new(0));
        let mut wf = WatermarkingFold::new(MockFold::new(), app_seq.clone(), ORIGIN_US);
        let mut state = Vec::new();

        wf.apply(&ev_with_meta(0, ORIGIN_US, 10, b""), &mut state)
            .unwrap();
        assert_eq!(app_seq.load(Ordering::Acquire), 11);

        // A later RedEX seq with a SMALLER seq_or_ts must NOT pull the
        // watermark back down.
        wf.apply(&ev_with_meta(1, ORIGIN_US, 3, b""), &mut state)
            .unwrap();
        assert_eq!(
            app_seq.load(Ordering::Acquire),
            11,
            "fetch_max must keep the watermark from regressing",
        );
    }

    #[test]
    fn short_payload_is_silently_skipped() {
        // A third-party writer that appended raw bytes (no `EventMeta`
        // prefix) would produce a payload < EVENT_META_SIZE. The
        // wrapper must defensively skip rather than corrupt the
        // watermark with a bogus parse.
        let app_seq = Arc::new(AtomicU64::new(7));
        let mut wf = WatermarkingFold::new(MockFold::new(), app_seq.clone(), ORIGIN_US);
        let mut state = Vec::new();

        // 19 bytes — one short of EVENT_META_SIZE (20).
        wf.apply(&ev_short(0, 19), &mut state).unwrap();
        assert_eq!(
            app_seq.load(Ordering::Acquire),
            7,
            "watermark must be untouched when payload is too short to parse",
        );
    }

    #[test]
    fn inner_fold_error_propagates_and_does_not_advance_watermark() {
        // The watermark only advances on a *successful*
        // inner-fold apply. If the user fold rejects the event, the
        // wrapper must surface the error AND leave app_seq alone — the
        // event was effectively skipped (Continue policy) or halted
        // the task (Stop policy), and either way the per-origin
        // counter must not include it.
        let app_seq = Arc::new(AtomicU64::new(0));
        let mut wf = WatermarkingFold::new(MockFold::fail_on(0), app_seq.clone(), ORIGIN_US);
        let mut state = Vec::new();

        let r = wf.apply(&ev_with_meta(0, ORIGIN_US, 42, b""), &mut state);
        assert!(matches!(r, Err(RedexError::Decode(_))));
        assert_eq!(
            app_seq.load(Ordering::Acquire),
            0,
            "watermark must NOT advance for an event the inner fold rejected",
        );
    }

    #[test]
    fn watermark_holds_when_pre_set_value_already_exceeds_observed_seq_or_ts() {
        // `open_from_snapshot` pre-loads `app_seq` from the snapshot
        // payload. If the snapshot value already covers every same-
        // origin event in the replay tail, the wrapper's fetch_max is
        // a no-op. Pin that semantics so `open_from_snapshot` doesn't
        // accidentally regress the counter when the tail is empty
        // for our origin.
        let app_seq = Arc::new(AtomicU64::new(100));
        let mut wf = WatermarkingFold::new(MockFold::new(), app_seq.clone(), ORIGIN_US);
        let mut state = Vec::new();

        wf.apply(&ev_with_meta(0, ORIGIN_US, 5, b""), &mut state)
            .unwrap();
        assert_eq!(app_seq.load(Ordering::Acquire), 100);
    }

    #[test]
    fn mixed_origin_stream_only_advances_for_matching_origin() {
        // Realistic shape: a channel shared by us and another origin,
        // events interleaved in the log.
        let app_seq = Arc::new(AtomicU64::new(0));
        let mut wf = WatermarkingFold::new(MockFold::new(), app_seq.clone(), ORIGIN_US);
        let mut state = Vec::new();

        let stream = [
            (0, ORIGIN_OTHER, 100),
            (1, ORIGIN_US, 0),
            (2, ORIGIN_OTHER, 200),
            (3, ORIGIN_US, 1),
            (4, ORIGIN_OTHER, 300),
            (5, ORIGIN_US, 2),
        ];
        for (seq, origin, seq_or_ts) in stream {
            wf.apply(&ev_with_meta(seq, origin, seq_or_ts, b""), &mut state)
                .unwrap();
        }

        assert_eq!(
            app_seq.load(Ordering::Acquire),
            3,
            "watermark must reflect only our origin's max+1 (saw seq_or_ts 0,1,2)",
        );
        // Inner fold saw every event (delivery is not filtered).
        assert_eq!(state.len(), 6);
    }

    #[test]
    fn watermark_ignores_seq_or_ts_at_u64_max_to_preserve_monotonicity() {
        // Pre-fix: `saturating_add(1)` pinned `app_seq` at
        // `u64::MAX` when a peer wrote an event with
        // `seq_or_ts == u64::MAX`. The next legitimate ingest's
        // `fetch_add(1)` on `u64::MAX` then panics (debug) or
        // wraps to 0 (release), breaking the per-origin
        // monotonicity invariant. A hostile or malformed peer
        // could thus poison our adapter with one bad event.
        //
        // Post-fix: `seq_or_ts == u64::MAX` is treated as
        // malformed and ignored. The watermark stays at
        // whatever it was, the inner fold still receives the
        // event (delivery is not filtered), and the next
        // ingest's `fetch_add(1)` is always safe.
        let app_seq = Arc::new(AtomicU64::new(42));
        let mut wf = WatermarkingFold::new(MockFold::new(), app_seq.clone(), ORIGIN_US);
        let mut state = Vec::new();

        // Inner fold runs (delivery is not filtered), but the
        // watermark must NOT advance to u64::MAX.
        wf.apply(&ev_with_meta(0, ORIGIN_US, u64::MAX, b""), &mut state)
            .unwrap();
        assert_eq!(
            app_seq.load(Ordering::Acquire),
            42,
            "watermark must NOT advance on a u64::MAX seq_or_ts \
             — a subsequent fetch_add(1) on u64::MAX panics in debug \
             or wraps to 0 in release, breaking per-origin monotonicity"
        );
        assert_eq!(state.len(), 1, "inner fold must still see the event");

        // Confirm normal operation still works after the
        // poisoning attempt.
        wf.apply(&ev_with_meta(1, ORIGIN_US, 100, b""), &mut state)
            .unwrap();
        assert_eq!(
            app_seq.load(Ordering::Acquire),
            101,
            "subsequent legitimate seq_or_ts must still advance the watermark"
        );

        // Boundary: seq_or_ts = u64::MAX - 1 still advances (it's
        // legitimate, even if astronomical). The next state is
        // app_seq = u64::MAX, which is the highest value an
        // adapter can ever observe — but that's only a problem
        // if the NEXT ingest is allowed; the audit's invariant
        // here is just that hostile u64::MAX inputs don't
        // accelerate exhaustion.
        let app_seq2 = Arc::new(AtomicU64::new(0));
        let mut wf2 = WatermarkingFold::new(MockFold::new(), app_seq2.clone(), ORIGIN_US);
        let mut state2 = Vec::new();
        wf2.apply(&ev_with_meta(0, ORIGIN_US, u64::MAX - 1, b""), &mut state2)
            .unwrap();
        assert_eq!(
            app_seq2.load(Ordering::Acquire),
            u64::MAX,
            "seq_or_ts = u64::MAX - 1 is legitimate (saturating_add(1) = u64::MAX)"
        );
    }
}
