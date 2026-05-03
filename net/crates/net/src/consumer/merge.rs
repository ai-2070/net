//! Cross-shard poll merge layer.
//!
//! This module handles polling from multiple shards and merging the results
//! into a unified stream with proper cursor management.
//!
//! # Composite Cursor
//!
//! When polling multiple shards, we track position in each shard using a
//! composite cursor encoded as base64 JSON:
//!
//! ```json
//! {"0": "1702123456789-0", "1": "1702123456790-0", ...}
//! ```

use std::cmp::Ordering as CmpOrdering;
use std::collections::HashMap;
use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde::{Deserialize, Serialize};

use crate::adapter::{Adapter, ShardPollResult};
use crate::consumer::filter::Filter;
use crate::error::{AdapterError, ConsumerError};
use crate::event::StoredEvent;

/// Compare two adapter-emitted stream ids using numeric semantics for
/// the formats both built-in adapters produce, with a lex fallback for
/// already-lex-comparable opaque ids (ULID, UUIDv7, fixed-width hex).
///
/// A raw `str::cmp` would invert ordering on the unpadded numeric
/// ids the JetStream adapter emits (`seq.to_string()`) and the
/// Redis Streams server-side `{ms}-{seq}` format — `"9" > "10"`
/// lexicographically would wedge the cursor at every decade
/// boundary. The structured comparator below handles both formats
/// numerically. Mixed-padding comparisons across upgrades still
/// compare correctly because parse-then-compare ignores leading
/// zeros.
///
/// Order of attempts:
/// 1. Both ids parse as `<u64>-<u64>` (Redis Streams).
/// 2. Both ids parse as `<u128>` (raw numeric, padded or unpadded).
/// 3. Lex compare (ULID, UUID, hex digests, etc.).
///
/// We deliberately do not try to mix formats — if one side is `123`
/// and the other is `456-0`, the lex fallback kicks in. In practice
/// a single adapter emits a single format, so this only matters for
/// pathological mixed-source streams.
pub(crate) fn compare_stream_ids(a: &str, b: &str) -> CmpOrdering {
    // Redis Streams `<ms>-<seq>`.
    if let (Some((a_ms, a_seq)), Some((b_ms, b_seq))) = (split_redis_id(a), split_redis_id(b)) {
        return (a_ms, a_seq).cmp(&(b_ms, b_seq));
    }
    // Plain numeric (JetStream `seq.to_string()` or future zero-padded form).
    if let (Ok(an), Ok(bn)) = (a.parse::<u128>(), b.parse::<u128>()) {
        return an.cmp(&bn);
    }
    // Opaque id — assumed already lex-comparable.
    a.cmp(b)
}

fn split_redis_id(s: &str) -> Option<(u64, u64)> {
    let (ms, seq) = s.split_once('-')?;
    Some((ms.parse().ok()?, seq.parse().ok()?))
}

/// Coarse classifier for a stream id. Two ids of the same
/// format compare safely via `compare_stream_ids`; two ids of
/// different formats fall through to a lex compare that may
/// wedge the cursor (see `update_from_events`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IdFormat {
    /// Redis Streams `<ms>-<seq>`.
    Redis,
    /// Plain numeric (JetStream `seq.to_string()`).
    Numeric,
    /// Anything else — assumed lex-comparable.
    Opaque,
}

pub(crate) fn id_format(s: &str) -> IdFormat {
    if split_redis_id(s).is_some() {
        IdFormat::Redis
    } else if s.parse::<u128>().is_ok() {
        IdFormat::Numeric
    } else {
        IdFormat::Opaque
    }
}

/// Backing type for per-shard cursor positions. `Arc<str>` makes
/// cursor clones (and internal copies during poll merging) cheap by
/// reference-counting the id bytes rather than copying them.
type CursorPos = Arc<str>;

/// Ordering mode for consumed events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Ordering {
    /// Return events in arbitrary order (fastest).
    #[default]
    None,
    /// Sort events by insertion timestamp (cross-shard ordering).
    InsertionTs,
}

/// Composite cursor tracking position across multiple shards.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompositeCursor {
    /// Per-shard positions (shard_id -> stream_id).
    ///
    /// Stored as `Arc<str>` so internal copies (e.g. `cursor.clone()`
    /// inside the poll merger) bump a refcount instead of duplicating
    /// each id's bytes.
    #[serde(flatten)]
    pub positions: HashMap<u16, CursorPos>,
}

impl CompositeCursor {
    /// Create an empty cursor.
    pub fn new() -> Self {
        Self::default()
    }

    /// Encode the cursor as a base64 string.
    ///
    /// Pre-fix used `unwrap_or_default()`, which silently
    /// produced an empty string on serialization failure. The
    /// empty cursor then base64-encoded to an empty string and
    /// the consumer's next poll restarted from the beginning of
    /// the stream — silent rewind. For the current `positions`
    /// schema (`HashMap<u16, Arc<str>>`), serialization is
    /// infallible, so the failure path is unreachable.
    ///
    /// We surface that as a `ConsumerError::InvalidCursor` rather
    /// than a panic. `poll()` is an `async fn`, and a panic that
    /// propagates from there can abort the surrounding tokio
    /// runtime worker. Returning `Err` lets `poll()` stay
    /// non-panicking even if a future schema change breaks the
    /// invariant; the caller will see a structured error and can
    /// retry / log instead of taking down a worker.
    pub fn encode(&self) -> Result<String, ConsumerError> {
        let json = serde_json::to_string(&self.positions).map_err(|e| {
            ConsumerError::InvalidCursor(format!(
                "CompositeCursor::encode failed to serialize positions \
                 (HashMap<u16, Arc<str>> should be infallible): {e}"
            ))
        })?;
        Ok(BASE64.encode(json.as_bytes()))
    }

    /// Decode a cursor from a base64 string.
    pub fn decode(s: &str) -> Result<Self, ConsumerError> {
        let bytes = BASE64
            .decode(s)
            .map_err(|e| ConsumerError::InvalidCursor(e.to_string()))?;

        // Two-pass parse so non-canonical shard-id keys (e.g.
        // `"00"` aliasing `"0"`) are rejected explicitly. Pre-fix
        // we deserialized straight into `HashMap<u16, _>`; serde
        // parses each string key as u16, so `"0"` and `"00"`
        // both produce key 0 and the second insert silently
        // overwrites the first. The collision is benign in
        // production (no caller emits non-canonical keys) but
        // a malicious or buggy producer could inject a hostile
        // cursor that ambiguates which shard's position a
        // consumer ended up with on round-trip.
        let raw_positions: HashMap<String, CursorPos> = serde_json::from_slice(&bytes)
            .map_err(|e| ConsumerError::InvalidCursor(e.to_string()))?;
        let mut positions: HashMap<u16, CursorPos> = HashMap::with_capacity(raw_positions.len());
        for (key, val) in raw_positions {
            let id: u16 = key.parse().map_err(|_| {
                ConsumerError::InvalidCursor(format!("shard key {key:?} is not a valid u16"))
            })?;
            // Reject non-canonical stringifications. The
            // round-trip `u16 → String` is the canonical form;
            // any other string that parses to the same u16 is
            // a non-canonical alias.
            if id.to_string() != key {
                return Err(ConsumerError::InvalidCursor(format!(
                    "non-canonical shard key {key:?} (parses to {id}, \
                     canonical form is {id})"
                )));
            }
            if positions.insert(id, val).is_some() {
                // Defensive: if non-canonical detection above
                // missed something (it shouldn't), a duplicate
                // canonical key is also a structural error.
                return Err(ConsumerError::InvalidCursor(format!(
                    "duplicate shard key {id} after canonicalization"
                )));
            }
        }

        Ok(Self { positions })
    }

    /// Get the position for a specific shard.
    pub fn get(&self, shard_id: u16) -> Option<&str> {
        self.positions.get(&shard_id).map(|s| s.as_ref())
    }

    /// Set the position for a specific shard.
    ///
    /// Accepts anything that converts into an `Arc<str>` — notably
    /// `String`, `&str`, and `Arc<str>` itself. This lets adapters
    /// hand us a freshly-allocated `String` (becomes a single boxed
    /// allocation) without forcing a second copy for the cursor.
    pub fn set(&mut self, shard_id: u16, position: impl Into<CursorPos>) {
        self.positions.insert(shard_id, position.into());
    }

    /// Update positions from consumed events.
    ///
    /// Per-shard CAS routed through `compare_stream_ids`, which
    /// understands the Redis (`<ms>-<seq>`) and JetStream (`<u64>`)
    /// formats numerically and falls back to lex for opaque ids
    /// (ULID, UUIDv7, hex digests). The cursor cannot regress and
    /// decade-rollovers cannot freeze it. Unconditional inserts
    /// would let whichever event for a given `shard_id` appeared
    /// *last* in the slice win regardless of stream order; a plain
    /// `str::cmp` CAS would wedge on the unpadded numeric ids both
    /// built-in adapters emit (`"9" > "10"` lexicographically).
    pub fn update_from_events(&mut self, events: &[StoredEvent]) {
        for event in events {
            let new_id = event.id.as_str();
            match self.positions.get(&event.shard_id) {
                Some(existing) => {
                    // Detect a backend-format change before
                    // calling the comparator. Pre-fix a cursor
                    // at `"42"` (JetStream numeric) confronted
                    // with a new `"1700-0"` (Redis) fell through
                    // both structured branches of
                    // `compare_stream_ids`, hit the lex fallback
                    // (`'4' > '1'`), and the CAS guard refused
                    // to update — silent stall requiring manual
                    // cursor reset. Detect the mismatch
                    // explicitly: surface a loud error and
                    // refuse the update so operators see the
                    // backend migration in logs and reset the
                    // cursor deliberately. Keeping the existing
                    // value (rather than blindly accepting the
                    // new one) avoids a potential regression in
                    // the other direction.
                    let existing_fmt = id_format(existing.as_ref());
                    let new_fmt = id_format(new_id);
                    if existing_fmt != new_fmt {
                        tracing::error!(
                            shard_id = event.shard_id,
                            existing = %existing,
                            new = %new_id,
                            existing_format = ?existing_fmt,
                            new_format = ?new_fmt,
                            "stream id format change detected — likely a \
                             backend migration (e.g. JetStream → Redis). \
                             Refusing to advance the cursor; operator must \
                             explicitly reset to consume from the new \
                             backend.",
                        );
                        continue;
                    }
                    if compare_stream_ids(existing.as_ref(), new_id) == CmpOrdering::Less {
                        self.positions.insert(event.shard_id, Arc::from(new_id));
                    }
                    // Existing is >= new_id under the structured
                    // comparator — don't regress.
                }
                None => {
                    self.positions.insert(event.shard_id, Arc::from(new_id));
                }
            }
        }
    }
}

/// Request for consuming events.
#[derive(Debug, Clone, Default)]
pub struct ConsumeRequest {
    /// Start cursor (opaque to caller). None means from the beginning.
    pub from_id: Option<String>,
    /// Maximum number of events to return.
    pub limit: usize,
    /// Optional filter to apply.
    pub filter: Option<Filter>,
    /// Ordering mode.
    pub ordering: Ordering,
    /// Specific shards to poll. None means all shards.
    pub shards: Option<Vec<u16>>,
}

impl ConsumeRequest {
    /// Create a new consume request.
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            ..Default::default()
        }
    }

    /// Set the starting cursor.
    pub fn from(mut self, cursor: impl Into<String>) -> Self {
        self.from_id = Some(cursor.into());
        self
    }

    /// Set the filter.
    pub fn filter(mut self, filter: Filter) -> Self {
        self.filter = Some(filter);
        self
    }

    /// Set the ordering mode.
    pub fn ordering(mut self, ordering: Ordering) -> Self {
        self.ordering = ordering;
        self
    }

    /// Set specific shards to poll.
    pub fn shards(mut self, shards: Vec<u16>) -> Self {
        self.shards = Some(shards);
        self
    }
}

/// Response from consuming events.
#[derive(Debug, Clone)]
pub struct ConsumeResponse {
    /// Events matching the request.
    pub events: Vec<StoredEvent>,
    /// Cursor for the next poll. None if no events returned.
    pub next_id: Option<String>,
    /// True if there are more events available.
    pub has_more: bool,
    /// `true` if the per-shard fetch was clamped by the internal
    /// `PER_SHARD_FETCH_CAP` (10 000). Callers requesting very
    /// large `limit` values across few shards may receive fewer
    /// events than `limit` per `poll()` even when the underlying
    /// streams have more — pagination via `next_id` still works.
    ///
    /// Pre-fix this clamp was silent. The default is
    /// `false`; tools building observability around large polls
    /// can detect under-delivery via this flag.
    pub truncated_at_per_shard_cap: bool,
    /// Shards that reported `has_more=true` but contributed no
    /// events and no cursor advance to this poll. The merger
    /// suppresses the aggregate `has_more` flag for caller-
    /// protection (preventing infinite loops), but operators
    /// monitoring adapter health should know which shards are
    /// stuck.
    ///
    /// Pre-fix the suppression was logged at warn but
    /// invisible to callers. Empty on the happy path; populated
    /// only when a stall was detected and suppressed.
    pub stalled_shards: Vec<u16>,
    /// Shards whose adapter call returned an error during this
    /// poll. The merger logs each error at WARN and continues
    /// with the surviving shards, so the response's `events`
    /// can come from a strict subset of the configured shards
    /// and silently miss data the operator expected to see.
    /// Operators monitoring adapter health need to know WHICH
    /// shards failed (not just that something logged a warn) so
    /// they can correlate alerts with specific Redis / JetStream
    /// nodes.
    ///
    /// Pre-fix this signal lived only in the warn log; an
    /// observer parsing `ConsumeResponse` saw a clean partial-
    /// shards response with no field indicating *which* shards
    /// were missing, in contrast to `stalled_shards` which IS
    /// surfaced. Empty on the happy path; populated only when at
    /// least one shard's poll errored.
    pub failed_shards: Vec<u16>,
}

impl ConsumeResponse {
    /// Create an empty response.
    pub fn empty() -> Self {
        Self {
            events: Vec::new(),
            next_id: None,
            has_more: false,
            truncated_at_per_shard_cap: false,
            stalled_shards: Vec::new(),
            failed_shards: Vec::new(),
        }
    }
}

/// Internal cap on per-shard `direct_get` / `XRANGE` fetch sizes,
/// applied in `PollMerger::poll`. Bounds the adapter's per-call
/// memory pressure for a single poll. Callers needing larger
/// effective limits should paginate.
///
/// Marked `#[doc(hidden)]` because the value is an internal
/// tuning knob, not part of the consumer's public API. Surfacing
/// it on the docs would invite downstreams to match against it
/// and turn a silent tuning change into a breaking-change
/// negotiation. Callers that need to know whether a poll was
/// truncated should read `ConsumeResponse::truncated_at_per_shard_cap`
/// rather than comparing against this constant directly.
#[doc(hidden)]
pub const PER_SHARD_FETCH_CAP: usize = 10_000;

/// Match a `StoredEvent` against a filter, surfacing parse failures.
///
/// Returns `true` iff the event parses as JSON AND the filter matches
/// the parsed value. A parse failure is logged at WARN with the event's
/// id and shard so on-disk corruption or framing bugs in upstream
/// adapters are observable from the filtered-poll path; without this,
/// corrupt events were silently dropped from filtered results while the
/// unfiltered path still returned them — a confusing inconsistency.
fn event_matches_filter(event: &StoredEvent, filter: &Filter) -> bool {
    match event.parse() {
        Ok(value) => filter.matches(&value),
        Err(e) => {
            tracing::warn!(
                event_id = %event.id,
                shard_id = event.shard_id,
                error = %e,
                "dropping unparseable event from filtered poll result"
            );
            false
        }
    }
}

/// Poll merger for cross-shard aggregation.
pub struct PollMerger {
    /// Adapter for polling shards.
    adapter: Arc<dyn Adapter>,
    /// Active shard IDs to poll when the request omits an explicit
    /// `shards` list.
    ///
    /// Previously stored only `num_shards: u16` and generated
    /// `(0..num_shards)` on every default-shards poll. After a dynamic
    /// scale-down (`ShardMapper::scale_down` evicts the lowest-weight
    /// shard, not necessarily the highest id), the active id set can
    /// become sparse — e.g. `{1, 2}` after id 0 was drained — but
    /// `num_shards == 2` still produces `[0, 1]`, polling a stale or
    /// nonexistent shard 0 and skipping the live shard 2 entirely.
    /// Captured at construction; the bus replaces the merger via
    /// `ArcSwap` whenever topology changes (`add_shard`,
    /// `remove_shard_internal`).
    shard_ids: Vec<u16>,
}

impl PollMerger {
    /// Create a new poll merger.
    ///
    /// `shard_ids` should be the snapshot of currently-active shard IDs
    /// (e.g. `ShardManager::shard_ids()`). Passing `0..num_shards` is
    /// only correct when ids are guaranteed dense from 0 — i.e. the
    /// static-shards path with no scaling.
    pub fn new(adapter: Arc<dyn Adapter>, shard_ids: Vec<u16>) -> Self {
        Self { adapter, shard_ids }
    }

    /// Poll events according to the request.
    pub async fn poll(&self, request: ConsumeRequest) -> Result<ConsumeResponse, ConsumerError> {
        if request.limit == 0 {
            return Ok(ConsumeResponse::empty());
        }

        // Decode cursor
        let cursor = match &request.from_id {
            Some(s) => CompositeCursor::decode(s)?,
            None => CompositeCursor::new(),
        };

        // Determine which shards to poll
        let shards: Vec<u16> = request
            .shards
            .clone()
            .unwrap_or_else(|| self.shard_ids.clone());

        if shards.is_empty() {
            return Ok(ConsumeResponse::empty());
        }

        // Calculate per-shard limit (over-fetch to account for filtering)
        // Use ceiling division to avoid truncating to 0 when limit < shard count.
        //
        // Pre-fix this `min(10_000)` clamp was silent —
        // a caller with `limit=200_000` over 10 shards expected
        // 20 000/shard plus over-fetch but got 10 000/shard with
        // no diagnostic. Track whether the clamp triggered and
        // surface it on the response so callers building
        // observability around large polls can detect under-
        // delivery.
        let over_fetch_factor = if request.filter.is_some() { 3 } else { 2 };
        let unclamped_per_shard = request
            .limit
            .div_ceil(shards.len())
            .max(1)
            .saturating_mul(over_fetch_factor);
        let per_shard_limit = unclamped_per_shard.min(PER_SHARD_FETCH_CAP);
        let truncated_at_per_shard_cap = unclamped_per_shard > PER_SHARD_FETCH_CAP;

        // Poll all shards in parallel. Each future borrows its start
        // position directly from `cursor` (which outlives `join_all` below),
        // avoiding a per-shard `String` allocation on every poll.
        let poll_futures: Vec<_> = shards
            .iter()
            .map(|&shard_id| {
                let adapter = self.adapter.clone();
                let from: Option<&str> = cursor.get(shard_id);
                async move {
                    let result = adapter.poll_shard(shard_id, from, per_shard_limit).await;
                    (shard_id, result)
                }
            })
            .collect();

        let shard_results: Vec<(u16, Result<ShardPollResult, AdapterError>)> =
            futures::future::join_all(poll_futures).await;

        // Collect results, tracking errors. Pre-allocate to the exact total
        // event count so extend() below never reallocates.
        let total_events: usize = shard_results
            .iter()
            .filter_map(|(_, r)| r.as_ref().ok().map(|sr| sr.events.len()))
            .sum();
        let mut all_events = Vec::with_capacity(total_events);
        let mut any_has_more = false;
        // Track which shards reported `has_more=true` so
        // we can surface them on the response when the merger
        // suppresses has_more for caller-protection. Pre-fix, an
        // adapter stuck reporting `has_more=true` with no events
        // and no cursor advance was logged at warn but invisible
        // to callers — they saw a clean "no more events" and
        // exited.
        let mut shards_reporting_has_more: Vec<u16> = Vec::new();
        // Per-shard adapter errors. Pre-fix these were logged at
        // warn and then dropped on the floor; the response's
        // `events` was a strict subset of the configured shards
        // with no field indicating WHICH shards were missing.
        // Surface the failed shard ids so observers can correlate
        // alerts with specific Redis / JetStream nodes (parallel
        // to the existing `stalled_shards` field).
        let mut failed_shards: Vec<u16> = Vec::new();
        // `new_cursor` (fetched-position tracking) is only consulted on the
        // filter path — building it for unfiltered polls wastes a full
        // HashMap clone plus a `set()` per shard every poll.
        let mut new_cursor = if request.filter.is_some() {
            Some(cursor.clone())
        } else {
            None
        };

        for (shard_id, result) in shard_results {
            match result {
                Ok(shard_result) => {
                    // Destructure to move `next_id` out without cloning the
                    // String that the adapter already allocated for us.
                    let ShardPollResult {
                        events,
                        next_id,
                        has_more,
                    } = shard_result;
                    if let (Some(nc), Some(next_id)) = (new_cursor.as_mut(), next_id) {
                        nc.set(shard_id, next_id);
                    }
                    if has_more {
                        any_has_more = true;
                        shards_reporting_has_more.push(shard_id);
                    }
                    all_events.extend(events);
                }
                Err(e) => {
                    tracing::warn!(
                        shard_id = shard_id,
                        error = %e,
                        "Failed to poll shard, skipping"
                    );
                    failed_shards.push(shard_id);
                    // Continue with other shards
                }
            }
        }

        // Apply filter.
        //
        // IMPORTANT: Use `new_cursor` (which tracks fetched positions) as
        // the base cursor so that shards whose events are entirely filtered
        // out still advance past those events. Without this, filtered-out
        // events would be re-fetched on every subsequent poll, causing an
        // infinite loop.
        //
        // Parse failures: a `StoredEvent` whose `raw` bytes don't
        // deserialize as JSON cannot match a filter, so it is dropped
        // from the filtered result. Previously this drop was silent
        // (`unwrap_or(false)`), making on-disk corruption or
        // adapter-side framing bugs invisible to operators — only
        // *unfiltered* polls would surface the bad event.
        //
        // The previous `Ordering::None` path had a `break` once
        // `kept.len() >= limit + 1`, which discarded events from later
        // shards without ever filtering them. Combined with the cursor
        // advancing past every fetched event, that meant matching
        // events on un-inspected shards were silently lost. The fix
        // uses a single full `retain` pass for both ordering modes;
        // the `lazy parse` micro-optimization is gone, but per-event
        // filter-matching is cheap and consistent semantics with the
        // sort path is worth more than parse-skip on over-fetches.
        if let Some(filter) = &request.filter {
            all_events.retain(|e| event_matches_filter(e, filter));
        }

        // Apply ordering
        match request.ordering {
            Ordering::None => {
                // Keep arbitrary order
            }
            Ordering::InsertionTs => {
                // `insertion_ts` is monotonic *per shard*, not
                // globally (see `event.rs:233`), so two events from
                // different shards can carry the same timestamp. With
                // a stable sort on `insertion_ts` alone, ties were
                // broken by the input order — which depends on
                // `futures::future::join_all`'s completion ordering
                // and is non-deterministic across polls. Combined
                // with `truncate(limit)` and the cursor-rollback step,
                // the same logical event could be returned twice or
                // skipped at the limit boundary across consecutive
                // polls.
                //
                // Add `(shard_id, id)` as deterministic
                // tiebreakers. `id` is the storage backend's
                // identifier and is unique within a shard, so the
                // composite is a strict total order.
                //
                // The id tiebreak routes through
                // `compare_stream_ids`, which understands the
                // unpadded numeric formats both built-in adapters
                // emit (Redis `<ms>-<seq>`, JetStream `<u64>`) and
                // falls back to lex for opaque ids (ULID, UUIDv7,
                // hex digests). The `(insertion_ts, shard_id)`
                // chain resolves the common cases first; when two
                // events from the same shard land at the same
                // `insertion_ts` (rare but legal at millisecond
                // granularity), the structured id compare is
                // correct on every adapter format.
                all_events.sort_by(|a, b| {
                    a.insertion_ts
                        .cmp(&b.insertion_ts)
                        .then(a.shard_id.cmp(&b.shard_id))
                        .then(compare_stream_ids(&a.id, &b.id))
                });
            }
        }

        // Track per-shard match counts *before* truncate. After
        // truncation, any shard whose match count shrank means matches
        // were dropped — and those matches must be re-fetched on the
        // next poll, otherwise they are silently lost (the cursor
        // would otherwise advance past them via `new_cursor`).
        let mut matched_per_shard: std::collections::HashMap<u16, usize> =
            std::collections::HashMap::new();
        if request.filter.is_some() {
            for e in &all_events {
                *matched_per_shard.entry(e.shard_id).or_insert(0) += 1;
            }
        }

        // Truncate to requested limit
        let had_extra = all_events.len() > request.limit;
        all_events.truncate(request.limit);

        // Build the final cursor.
        //
        // With filtering: start from `new_cursor` (fetched positions) so
        // shards whose events were entirely filtered out advance past
        // them. Then:
        //   1. For shards that had matches truncated (returned <
        //      total_matched), roll the cursor *back* to the original
        //      pre-poll position. The override step then bumps it
        //      forward to the last returned match for that shard, so
        //      the unreturned matches re-appear on the next poll.
        //   2. Override with the last *returned* event id per shard —
        //      this also prevents skipping matching events that were
        //      fetched but truncated by the limit on shards that did
        //      land in the returned set.
        //
        // Without filtering: start from the original `cursor` so shards
        // with no returned events (due to limit truncation) don't skip
        // ahead.
        let mut final_cursor = match new_cursor {
            Some(nc) => nc,
            None => cursor.clone(),
        };

        // Step 1: rollback for shards with truncated matches. We
        // track which shards we rolled back here so Step 2 only
        // overrides those shards (rather than every shard with a
        // returned event, which would throw away the adapter's
        // `next_id` advance for shards that returned all their
        // matches).
        let mut rolled_back: std::collections::HashSet<u16> = std::collections::HashSet::new();
        if request.filter.is_some() && had_extra {
            let mut returned_per_shard: std::collections::HashMap<u16, usize> =
                std::collections::HashMap::new();
            for e in &all_events {
                *returned_per_shard.entry(e.shard_id).or_insert(0) += 1;
            }
            for (shard_id, &total_matched) in &matched_per_shard {
                let returned = returned_per_shard.get(shard_id).copied().unwrap_or(0);
                if returned < total_matched {
                    // Some matches for this shard were truncated. Roll
                    // back to the original cursor so they're re-fetched.
                    // The override below will move us forward to the
                    // last *returned* match (if any), so we still make
                    // progress per poll.
                    match cursor.positions.get(shard_id) {
                        Some(orig) => final_cursor.set(*shard_id, orig.clone()),
                        None => {
                            final_cursor.positions.remove(shard_id);
                        }
                    }
                    rolled_back.insert(*shard_id);
                }
            }
        }

        // Step 2: override to last returned event id per shard.
        // Only the last returned event per shard matters for the
        // cursor, so iterate in reverse and skip shards already seen.
        // This reduces id clones from O(all_events.len()) to
        // O(shards.len()).
        //
        // When the filter path is active, Step 1 has already
        // populated `final_cursor` with the adapter's `next_id`
        // (a position past the last *fetched* event for each
        // shard). A blanket Step 2 override here would move the
        // cursor back to the last *matched* event id, which is
        // BEHIND the last fetched event for any shard with
        // non-matched events — subsequent polls would re-fetch and
        // re-filter those non-matches, wasting work proportional
        // to `over_fetch_factor` on low-match-rate streams. So we
        // only Step 2-override for shards that were actually
        // rolled back in Step 1 (those need a forward push past
        // the last *returned* match), OR when the filter path
        // wasn't used at all (filter is None).
        //
        // For filter=None, the previous Step-2 behavior was correct
        // — `final_cursor` started as `cursor.clone()` (no
        // `new_cursor` advance) and the only progress signal is
        // the last returned event id.
        let mut seen_shards: std::collections::HashSet<u16> =
            std::collections::HashSet::with_capacity(shards.len());
        for event in all_events.iter().rev() {
            if seen_shards.insert(event.shard_id) {
                let should_override =
                    request.filter.is_none() || rolled_back.contains(&event.shard_id);
                if should_override {
                    final_cursor.set(event.shard_id, event.id.clone());
                }
                // Else: the adapter's `next_id` (already in
                // `final_cursor` via the `new_cursor` initial
                // value) is more advanced than the last matched
                // event — preserve it.
            }
        }

        let cursor_advanced = final_cursor.positions != cursor.positions;
        // When filtering removed everything but we did advance past fetched
        // events, signal has_more so the caller keeps polling forward.
        let all_filtered = request.filter.is_some() && all_events.is_empty() && cursor_advanced;
        // Previously `has_more = any_has_more || had_extra ||
        // all_filtered`. If a single adapter returned
        // `ShardPollResult { events: [], next_id: None,
        // has_more: true }` (legal under the trait contract — nothing
        // forbids it), then `any_has_more=true` propagated even
        // though we made *no* progress. The caller observed
        // `(has_more=true, next_id=None)` and re-polled from the
        // same starting cursor indefinitely.
        //
        // Suppress `has_more` when the merger itself made no progress
        // at all (no events returned AND the cursor didn't advance).
        // The caller then sees a clean "nothing to do right now"
        // response and must back off rather than spin.
        let we_made_progress = !all_events.is_empty() || cursor_advanced;
        let has_more = (any_has_more || had_extra || all_filtered) && we_made_progress;
        // When we suppress has_more for caller-protection
        // (an adapter stuck at has_more=true with no progress),
        // surface the offending shard ids on the response so
        // operators can alert. Pre-fix the suppression was warn-
        // logged only.
        let stalled_shards: Vec<u16> = if any_has_more && !we_made_progress {
            tracing::warn!(
                stalled_shards = ?shards_reporting_has_more,
                "PollMerger: an adapter reported has_more=true with no events \
                 and no cursor advance — suppressing to avoid caller infinite-loop"
            );
            shards_reporting_has_more
        } else {
            Vec::new()
        };
        // Return the cursor even when all events were filtered out, so the
        // caller advances past the filtered region instead of re-fetching
        // the same events forever. When the poll made no progress at
        // all, echo back the caller's input cursor (if any) instead of
        // returning `None` — pre-fix a stalled poll dropped the cursor,
        // so a caller that interpreted `next_id == None` as "no events,
        // restart from the beginning" silently regressed pagination
        // across the stall. Echoing preserves the cursor across stalls.
        let next_id = if we_made_progress {
            Some(final_cursor.encode()?)
        } else {
            request.from_id.clone()
        };

        Ok(ConsumeResponse {
            events: all_events,
            next_id,
            has_more,
            truncated_at_per_shard_cap,
            stalled_shards,
            failed_shards,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_cursor_encode_decode() {
        let mut cursor = CompositeCursor::new();
        cursor.set(0, "1702123456789-0".to_string());
        cursor.set(1, "1702123456790-0".to_string());
        cursor.set(5, "1702123456795-0".to_string());

        let encoded = cursor.encode().unwrap();
        let decoded = CompositeCursor::decode(&encoded).unwrap();

        assert_eq!(decoded.get(0), Some("1702123456789-0"));
        assert_eq!(decoded.get(1), Some("1702123456790-0"));
        assert_eq!(decoded.get(5), Some("1702123456795-0"));
        assert_eq!(decoded.get(2), None);
    }

    #[test]
    fn test_cursor_update_from_events() {
        let mut cursor = CompositeCursor::new();

        let events = vec![
            StoredEvent::from_value("100-0".to_string(), json!({}), 100, 0),
            StoredEvent::from_value("200-0".to_string(), json!({}), 200, 1),
            // This used to be "later event in shard 0" by
            // virtue of being LAST in the input slice, but its id
            // (150-0) is stream-order BEFORE 200-0 — wait, this is
            // for shard 0 not shard 1. shard 0 only had 100-0
            // before this; 150-0 > 100-0, so it advances normally.
            StoredEvent::from_value("150-0".to_string(), json!({}), 150, 0),
        ];

        cursor.update_from_events(&events);

        // Should have the highest id seen for each shard.
        assert_eq!(cursor.get(0), Some("150-0"));
        assert_eq!(cursor.get(1), Some("200-0"));
    }

    /// Cursor must NOT regress when events arrive in a
    /// non-ascending order for the same shard. Pre-fix the cursor
    /// for shard 0 would land on `100-0` (the last item in the
    /// slice), regressing past `200-0`.
    #[test]
    fn cursor_does_not_regress_on_unsorted_per_shard_events() {
        let mut cursor = CompositeCursor::new();
        // For shard 0: stream-order 100-0 → 200-0, but the slice
        // has them reversed (consumer received 200-0 first,
        // then 100-0 because of merge ordering).
        let events = vec![
            StoredEvent::from_value("200-0".to_string(), json!({}), 200, 0),
            StoredEvent::from_value("100-0".to_string(), json!({}), 100, 0),
        ];
        cursor.update_from_events(&events);
        assert_eq!(
            cursor.get(0),
            Some("200-0"),
            "cursor must hold the highest id, not the last-in-slice id",
        );
    }

    /// A partial overlap (advance for one shard, regression
    /// attempt for another shard) must keep both cursors at their
    /// respective max.
    #[test]
    fn cursor_compare_and_set_is_per_shard() {
        let mut cursor = CompositeCursor::new();
        cursor.update_from_events(&[
            StoredEvent::from_value("500-0".to_string(), json!({}), 500, 0),
            StoredEvent::from_value("500-0".to_string(), json!({}), 500, 1),
        ]);
        // Now "advance" with one regress attempt for shard 0 + a
        // legitimate advance for shard 1.
        cursor.update_from_events(&[
            StoredEvent::from_value("100-0".to_string(), json!({}), 100, 0), // regress
            StoredEvent::from_value("700-0".to_string(), json!({}), 700, 1), // advance
        ]);
        assert_eq!(cursor.get(0), Some("500-0"), "shard 0 must not regress");
        assert_eq!(cursor.get(1), Some("700-0"), "shard 1 must advance");
    }

    /// CR-1: pre-fix the cursor used `str::cmp` which inverts at
    /// every decade rollover for unpadded numeric ids. JetStream
    /// emits `seq.to_string()` (unpadded) — once seq=10 lands, lex
    /// compare says `"10" < "9"` and the cursor freezes at "9".
    #[test]
    fn cursor_does_not_wedge_on_jetstream_decade_rollover() {
        let mut cursor = CompositeCursor::new();
        for seq in 1u64..=20 {
            let ev = StoredEvent::from_value(seq.to_string(), json!({}), seq, 0);
            cursor.update_from_events(&[ev]);
        }
        assert_eq!(
            cursor.get(0),
            Some("20"),
            "cursor must reach 20; lex compare would wedge at \"9\""
        );
    }

    /// CR-1: same hazard for Redis's `<ms>-<seq>` format when seq
    /// rolls past a decade within a single ms.
    #[test]
    fn cursor_does_not_wedge_on_redis_seq_decade_rollover() {
        let mut cursor = CompositeCursor::new();
        // All within one ms — Redis collides on seq when many events
        // hit in the same millisecond.
        for seq in 1u64..=20 {
            let id = format!("1700000000000-{}", seq);
            let ev = StoredEvent::from_value(id, json!({}), 1700000000000, 0);
            cursor.update_from_events(&[ev]);
        }
        assert_eq!(
            cursor.get(0),
            Some("1700000000000-20"),
            "cursor must reach -20; lex compare would wedge at -9"
        );
    }

    /// Regression: a backend migration (e.g. JetStream → Redis)
    /// that lands an id of a different format than the existing
    /// cursor must NOT silently advance OR silently stall. Pre-
    /// fix `compare_stream_ids` fell through both structured
    /// branches and hit the lex fallback: `"42" > "1700-0"` (because
    /// `'4' > '1'`), so the CAS guard refused to update the
    /// cursor. Result: the consumer kept seeing `"42"` forever
    /// while the new Redis backend kept emitting Redis-formatted
    /// ids, with no surfaced error.
    ///
    /// Post-fix: format-mismatch is detected explicitly and
    /// surfaced via `tracing::error!`. The cursor stays at its
    /// current value (so we don't regress), and an operator must
    /// reset the cursor deliberately to consume from the new
    /// backend. This test pins the "stays at existing value"
    /// half of the contract; the loud error is observability,
    /// not behavior, so it isn't asserted here.
    #[test]
    fn cursor_refuses_to_advance_across_backend_format_change() {
        let mut cursor = CompositeCursor::new();
        // Cursor starts at JetStream-style numeric "42".
        cursor.update_from_events(&[StoredEvent::from_value("42".to_string(), json!({}), 42, 0)]);
        assert_eq!(cursor.get(0), Some("42"));

        // A new event arrives in Redis format. Pre-fix this would
        // hit the lex fallback and the cursor would refuse to
        // advance silently; post-fix the format mismatch is
        // detected and the cursor STILL refuses to advance, but
        // a `tracing::error!` is emitted so operators see the
        // backend migration.
        cursor.update_from_events(&[StoredEvent::from_value(
            "1700000000000-0".to_string(),
            json!({}),
            1700000000000,
            0,
        )]);

        // Cursor must still be at the original numeric id (not
        // the new Redis id, which would be a regression-like
        // jump back in time, AND not unset, which would lose
        // progress).
        assert_eq!(
            cursor.get(0),
            Some("42"),
            "regression: cursor must not silently advance through a \
             backend-format change. The pre-fix lex fallback also \
             happened to keep the existing value (by `'4' > '1'`), \
             but only by accident; this test pins the explicit \
             format-mismatch refusal."
        );

        // And the reverse direction: a Redis cursor confronted
        // with a new numeric id must also stay put.
        let mut cursor = CompositeCursor::new();
        cursor.update_from_events(&[StoredEvent::from_value(
            "1700000000000-0".to_string(),
            json!({}),
            1700000000000,
            0,
        )]);
        cursor.update_from_events(&[StoredEvent::from_value(
            "9000".to_string(),
            json!({}),
            9000,
            0,
        )]);
        assert_eq!(
            cursor.get(0),
            Some("1700000000000-0"),
            "regression (reverse direction): Redis cursor must not be \
             silently overwritten by an incoming numeric id"
        );
    }

    /// CR-1: cross-decade compare on JetStream-style ids.
    #[test]
    fn cursor_advances_from_unpadded_9_to_unpadded_10() {
        let mut cursor = CompositeCursor::new();
        cursor.update_from_events(&[StoredEvent::from_value("9".to_string(), json!({}), 9, 0)]);
        cursor.update_from_events(&[StoredEvent::from_value("10".to_string(), json!({}), 10, 0)]);
        assert_eq!(cursor.get(0), Some("10"));
    }

    /// CR-1: ULID / opaque ids must still work via lex fallback.
    /// ULIDs are designed to be lex-sortable and we should NOT route
    /// them through the numeric parsers.
    #[test]
    fn cursor_advances_on_ulid_ids_via_lex_fallback() {
        let mut cursor = CompositeCursor::new();
        // Two real ULIDs in ascending stream order. Different
        // timestamp prefixes ensure lex order matches stream order.
        let earlier = "01HZ0000000000000000000000";
        let later = "01HZ0000010000000000000000";
        cursor.update_from_events(&[StoredEvent::from_value(
            earlier.to_string(),
            json!({}),
            1,
            0,
        )]);
        cursor.update_from_events(&[StoredEvent::from_value(later.to_string(), json!({}), 2, 0)]);
        assert_eq!(cursor.get(0), Some(later));
        // Now feed the earlier id again — must NOT regress.
        cursor.update_from_events(&[StoredEvent::from_value(
            earlier.to_string(),
            json!({}),
            1,
            0,
        )]);
        assert_eq!(cursor.get(0), Some(later));
    }

    /// CR-1: direct `compare_stream_ids` unit coverage.
    #[test]
    fn compare_stream_ids_handles_known_formats() {
        // JetStream-style: numeric compare, padded-or-not.
        assert_eq!(compare_stream_ids("9", "10"), CmpOrdering::Less);
        assert_eq!(compare_stream_ids("10", "9"), CmpOrdering::Greater);
        assert_eq!(compare_stream_ids("100", "100"), CmpOrdering::Equal);
        // Padding-insensitive: leading zeros do not change numeric value.
        assert_eq!(compare_stream_ids("00000010", "9"), CmpOrdering::Greater);

        // Redis-style: tuple compare on (ms, seq).
        assert_eq!(
            compare_stream_ids("1700-9", "1700-10"),
            CmpOrdering::Less,
            "seq must compare numerically, not lex"
        );
        assert_eq!(
            compare_stream_ids("1700-9", "1701-0"),
            CmpOrdering::Less,
            "ms wins over seq"
        );
        assert_eq!(
            compare_stream_ids("1700-100", "1700-9"),
            CmpOrdering::Greater
        );

        // ULID-shaped ids fall through to lex compare.
        let ulid_a = "01HZ0000000000000000000000";
        let ulid_b = "01HZ0000010000000000000000";
        assert_eq!(compare_stream_ids(ulid_a, ulid_b), CmpOrdering::Less);

        // Mixed format (one Redis, one numeric) — neither structured
        // path applies, falls through to lex. Documented limitation.
        // (Adapters emit a single format, so this is pathological.)
        let _ = compare_stream_ids("1700-0", "9999");
    }

    /// CR-1: in `Ordering::InsertionTs` mode, two events from the
    /// same shard with the same `insertion_ts` and unpadded numeric
    /// ids must sort by numeric id, not lex id.
    #[test]
    fn insertion_ts_sort_breaks_tie_on_id_numerically() {
        // Same shard, same ts — only the id tiebreak fires.
        let mut events = [
            StoredEvent::from_value("10".to_string(), json!({}), 1000, 0),
            StoredEvent::from_value("9".to_string(), json!({}), 1000, 0),
            StoredEvent::from_value("11".to_string(), json!({}), 1000, 0),
        ];
        events.sort_by(|a, b| {
            a.insertion_ts
                .cmp(&b.insertion_ts)
                .then(a.shard_id.cmp(&b.shard_id))
                .then(compare_stream_ids(&a.id, &b.id))
        });
        let ordered: Vec<&str> = events.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(
            ordered,
            vec!["9", "10", "11"],
            "id tiebreak must be numeric, not lex"
        );
    }

    #[test]
    fn test_consume_request_builder() {
        let request = ConsumeRequest::new(100)
            .from("some_cursor")
            .ordering(Ordering::InsertionTs)
            .shards(vec![0, 1, 2])
            .filter(Filter::eq("type", json!("token")));

        assert_eq!(request.limit, 100);
        assert_eq!(request.from_id, Some("some_cursor".to_string()));
        assert_eq!(request.ordering, Ordering::InsertionTs);
        assert_eq!(request.shards, Some(vec![0, 1, 2]));
        assert!(request.filter.is_some());
    }

    #[test]
    fn test_invalid_cursor() {
        let result = CompositeCursor::decode("not_valid_base64!!!");
        assert!(result.is_err());

        // Valid base64 but not valid JSON
        let result = CompositeCursor::decode(&BASE64.encode(b"not json"));
        assert!(result.is_err());
    }

    /// Regression: non-canonical shard-id keys must be rejected
    /// at decode time. Pre-fix `serde_json::from_slice::<HashMap<u16,
    /// _>>` parsed `"00"` and `"0"` both as u16 0; the second
    /// insert silently overwrote the first, leaving the consumer
    /// with whichever entry happened to come later in the JSON.
    /// Two distinct stringifications collapsed to one shard
    /// position with no surfaced error.
    #[test]
    fn cursor_decode_rejects_non_canonical_shard_keys() {
        // Construct a JSON cursor with `"00"` aliasing `"0"`.
        // Both round-trip to u16 0 under standard parsing.
        let hostile = br#"{"00":"id_a","1":"id_b"}"#;
        let encoded = BASE64.encode(hostile);
        let result = CompositeCursor::decode(&encoded);
        assert!(
            result.is_err(),
            "non-canonical shard key `\"00\"` must reject; \
             pre-fix this silently parsed as shard 0"
        );

        // Boundary: the canonical "0" (no leading zero) decodes
        // normally.
        let canonical = br#"{"0":"id_a","1":"id_b"}"#;
        let encoded_ok = BASE64.encode(canonical);
        let cursor =
            CompositeCursor::decode(&encoded_ok).expect("canonical shard keys must decode cleanly");
        assert_eq!(cursor.get(0), Some("id_a"));
        assert_eq!(cursor.get(1), Some("id_b"));
    }

    #[test]
    fn test_composite_cursor_new() {
        let cursor = CompositeCursor::new();
        assert!(cursor.positions.is_empty());
    }

    #[test]
    fn test_composite_cursor_default() {
        let cursor = CompositeCursor::default();
        assert!(cursor.positions.is_empty());
    }

    #[test]
    fn test_composite_cursor_get_nonexistent() {
        let cursor = CompositeCursor::new();
        assert!(cursor.get(0).is_none());
        assert!(cursor.get(100).is_none());
    }

    #[test]
    fn test_composite_cursor_set_overwrites() {
        let mut cursor = CompositeCursor::new();
        cursor.set(0, "first".to_string());
        assert_eq!(cursor.get(0), Some("first"));

        cursor.set(0, "second".to_string());
        assert_eq!(cursor.get(0), Some("second"));
    }

    #[test]
    fn test_composite_cursor_empty_encode() {
        let cursor = CompositeCursor::new();
        let encoded = cursor.encode().unwrap();
        let decoded = CompositeCursor::decode(&encoded).unwrap();
        assert!(decoded.positions.is_empty());
    }

    #[test]
    fn test_composite_cursor_clone() {
        let mut cursor = CompositeCursor::new();
        cursor.set(0, "pos-0".to_string());
        cursor.set(1, "pos-1".to_string());

        let cloned = cursor.clone();
        assert_eq!(cloned.get(0), Some("pos-0"));
        assert_eq!(cloned.get(1), Some("pos-1"));
    }

    #[test]
    fn test_composite_cursor_debug() {
        let mut cursor = CompositeCursor::new();
        cursor.set(0, "test".to_string());
        let debug = format!("{:?}", cursor);
        assert!(debug.contains("CompositeCursor"));
        assert!(debug.contains("positions"));
    }

    #[test]
    fn test_ordering_default() {
        let ordering = Ordering::default();
        assert_eq!(ordering, Ordering::None);
    }

    #[test]
    fn test_ordering_clone_copy() {
        let ordering = Ordering::InsertionTs;
        let cloned = ordering;
        assert_eq!(cloned, Ordering::InsertionTs);
    }

    #[test]
    fn test_ordering_debug() {
        assert!(format!("{:?}", Ordering::None).contains("None"));
        assert!(format!("{:?}", Ordering::InsertionTs).contains("InsertionTs"));
    }

    #[test]
    fn test_consume_request_new() {
        let request = ConsumeRequest::new(50);
        assert_eq!(request.limit, 50);
        assert!(request.from_id.is_none());
        assert!(request.filter.is_none());
        assert_eq!(request.ordering, Ordering::None);
        assert!(request.shards.is_none());
    }

    #[test]
    fn test_consume_request_default() {
        let request = ConsumeRequest::default();
        assert_eq!(request.limit, 0);
        assert!(request.from_id.is_none());
        assert!(request.filter.is_none());
        assert_eq!(request.ordering, Ordering::None);
        assert!(request.shards.is_none());
    }

    #[test]
    fn test_consume_request_from_string() {
        let request = ConsumeRequest::new(10).from(String::from("cursor123"));
        assert_eq!(request.from_id, Some("cursor123".to_string()));
    }

    #[test]
    fn test_consume_request_clone() {
        let request = ConsumeRequest::new(100)
            .from("cursor")
            .ordering(Ordering::InsertionTs)
            .shards(vec![0, 1]);

        let cloned = request.clone();
        assert_eq!(cloned.limit, 100);
        assert_eq!(cloned.from_id, Some("cursor".to_string()));
        assert_eq!(cloned.ordering, Ordering::InsertionTs);
        assert_eq!(cloned.shards, Some(vec![0, 1]));
    }

    #[test]
    fn test_consume_request_debug() {
        let request = ConsumeRequest::new(10);
        let debug = format!("{:?}", request);
        assert!(debug.contains("ConsumeRequest"));
        assert!(debug.contains("limit"));
    }

    #[test]
    fn test_consume_response_empty() {
        let response = ConsumeResponse::empty();
        assert!(response.events.is_empty());
        assert!(response.next_id.is_none());
        assert!(!response.has_more);
    }

    #[test]
    fn test_consume_response_clone() {
        let mut response = ConsumeResponse::empty();
        response.next_id = Some("cursor".to_string());
        response.has_more = true;

        let cloned = response.clone();
        assert_eq!(cloned.next_id, Some("cursor".to_string()));
        assert!(cloned.has_more);
    }

    #[test]
    fn test_consume_response_debug() {
        let response = ConsumeResponse::empty();
        let debug = format!("{:?}", response);
        assert!(debug.contains("ConsumeResponse"));
        assert!(debug.contains("events"));
    }

    #[test]
    fn test_cursor_update_from_empty_events() {
        let mut cursor = CompositeCursor::new();
        cursor.set(0, "original".to_string());

        let events: Vec<StoredEvent> = vec![];
        cursor.update_from_events(&events);

        // Cursor should be unchanged
        assert_eq!(cursor.get(0), Some("original"));
    }

    #[test]
    fn test_cursor_many_shards() {
        let mut cursor = CompositeCursor::new();
        for i in 0..100u16 {
            cursor.set(i, format!("pos-{}", i));
        }

        let encoded = cursor.encode().unwrap();
        let decoded = CompositeCursor::decode(&encoded).unwrap();

        for i in 0..100u16 {
            assert_eq!(decoded.get(i), Some(format!("pos-{}", i).as_str()));
        }
    }

    #[test]
    fn test_consume_request_empty_shards() {
        let request = ConsumeRequest::new(100).shards(vec![]);
        assert_eq!(request.shards, Some(vec![]));
    }

    #[test]
    fn test_consume_request_ordering_none() {
        let request = ConsumeRequest::new(100).ordering(Ordering::None);
        assert_eq!(request.ordering, Ordering::None);
    }

    #[test]
    fn test_ordering_equality() {
        assert_eq!(Ordering::None, Ordering::None);
        assert_eq!(Ordering::InsertionTs, Ordering::InsertionTs);
        assert_ne!(Ordering::None, Ordering::InsertionTs);
    }

    // Mock adapter for testing PollMerger
    use crate::adapter::{Adapter, ShardPollResult};
    use crate::error::AdapterError;
    use crate::event::Batch;
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::RwLock;

    struct MockAdapter {
        events: RwLock<HashMap<u16, Vec<StoredEvent>>>,
    }

    impl MockAdapter {
        fn new() -> Self {
            Self {
                events: RwLock::new(HashMap::new()),
            }
        }

        fn add_events(&self, shard_id: u16, events: Vec<StoredEvent>) {
            let mut map = self.events.write().unwrap();
            map.entry(shard_id).or_default().extend(events);
        }
    }

    #[async_trait]
    impl Adapter for MockAdapter {
        async fn init(&mut self) -> Result<(), AdapterError> {
            Ok(())
        }

        async fn on_batch(&self, _batch: Batch) -> Result<(), AdapterError> {
            Ok(())
        }

        async fn flush(&self) -> Result<(), AdapterError> {
            Ok(())
        }

        async fn shutdown(&self) -> Result<(), AdapterError> {
            Ok(())
        }

        async fn poll_shard(
            &self,
            shard_id: u16,
            from_id: Option<&str>,
            limit: usize,
        ) -> Result<ShardPollResult, AdapterError> {
            let map = self.events.read().unwrap();
            let events = map.get(&shard_id).cloned().unwrap_or_default();

            // Filter by from_id if provided
            let filtered: Vec<_> = if let Some(from) = from_id {
                events
                    .into_iter()
                    .skip_while(|e| e.id != from)
                    .skip(1) // Skip the from_id itself
                    .collect()
            } else {
                events
            };

            let has_more = filtered.len() > limit;
            let events: Vec<_> = filtered.into_iter().take(limit).collect();
            let next_id = events.last().map(|e| e.id.clone());

            Ok(ShardPollResult {
                events,
                next_id,
                has_more,
            })
        }

        fn name(&self) -> &'static str {
            "mock"
        }
    }

    #[tokio::test]
    async fn test_poll_merger_new() {
        let adapter = Arc::new(MockAdapter::new());
        let merger = PollMerger::new(adapter, vec![0, 1, 2, 3]);
        assert_eq!(merger.shard_ids, vec![0, 1, 2, 3]);
    }

    /// When the active shard id set is sparse (e.g. shard 0
    /// was scaled down, leaving `{1, 2}`), a poll with no explicit
    /// `request.shards` must hit shards 1 and 2 — not generate
    /// `[0, 1]` from a stale count and miss the live shard 2.
    ///
    /// Pre-fix, `PollMerger` stored only `num_shards: u16` and the
    /// default branch generated `(0..num_shards).collect()`, so
    /// shard 2's events were silently invisible to default-shards
    /// consumers after a scale-down.
    #[tokio::test]
    async fn poll_merger_default_shards_uses_active_id_set_after_scale_down() {
        let adapter = Arc::new(MockAdapter::new());

        // Shard 0 (drained / no longer active): would mis-poll
        // pre-fix and could return stale data on adapters that
        // recreate streams on demand. We don't add events here.
        // Shard 1 + Shard 2: active set after a scale-down that
        // evicted shard 0.
        adapter.add_events(
            1,
            vec![StoredEvent::from_value(
                "1-a".to_string(),
                json!({"shard": 1}),
                100,
                1,
            )],
        );
        adapter.add_events(
            2,
            vec![StoredEvent::from_value(
                "2-a".to_string(),
                json!({"shard": 2}),
                200,
                2,
            )],
        );

        // Sparse id set — shard 0 is NOT in the active list.
        let merger = PollMerger::new(adapter, vec![1, 2]);

        // Default-shards request (no `shards` override).
        let request = ConsumeRequest::new(100);
        let response = merger.poll(request).await.unwrap();

        let returned: std::collections::HashSet<u16> =
            response.events.iter().map(|e| e.shard_id).collect();
        assert!(
            returned.contains(&1),
            "default-shards poll must include shard 1 (active)",
        );
        assert!(
            returned.contains(&2),
            "default-shards poll must include shard 2 — pre-fix this was silently \
             skipped because the merger generated `0..num_shards` = `[0, 1]`",
        );
        assert!(
            !returned.contains(&0),
            "default-shards poll must NOT touch shard 0 — it was evicted",
        );
        assert_eq!(response.events.len(), 2);
    }

    #[tokio::test]
    async fn test_poll_merger_empty_limit() {
        let adapter = Arc::new(MockAdapter::new());
        let merger = PollMerger::new(adapter, vec![0, 1, 2, 3]);

        let request = ConsumeRequest::new(0);
        let response = merger.poll(request).await.unwrap();

        assert!(response.events.is_empty());
        assert!(response.next_id.is_none());
        assert!(!response.has_more);
    }

    #[tokio::test]
    async fn test_poll_merger_empty_shards() {
        let adapter = Arc::new(MockAdapter::new());
        let merger = PollMerger::new(adapter, vec![0, 1, 2, 3]);

        let request = ConsumeRequest::new(100).shards(vec![]);
        let response = merger.poll(request).await.unwrap();

        assert!(response.events.is_empty());
        assert!(response.next_id.is_none());
        assert!(!response.has_more);
    }

    /// Regression: per-shard adapter errors must surface on
    /// `ConsumeResponse.failed_shards`, not silently disappear
    /// after a `tracing::warn!`. Pre-fix the merger logged the
    /// error and continued; the response carried events from
    /// the surviving shards with no field indicating WHICH
    /// shards failed (in contrast to `stalled_shards` which IS
    /// surfaced). Operators correlating alerts with specific
    /// Redis / JetStream nodes had to grep logs instead of
    /// reading a structured response field.
    #[tokio::test]
    async fn poll_response_surfaces_failed_shard_ids() {
        // Mock that fails on a specific shard id.
        struct FailingShardMock {
            inner: MockAdapter,
            fail_shard: u16,
        }

        #[async_trait]
        impl Adapter for FailingShardMock {
            async fn init(&mut self) -> Result<(), AdapterError> {
                Ok(())
            }
            async fn on_batch(&self, _b: Batch) -> Result<(), AdapterError> {
                Ok(())
            }
            async fn flush(&self) -> Result<(), AdapterError> {
                Ok(())
            }
            async fn shutdown(&self) -> Result<(), AdapterError> {
                Ok(())
            }
            async fn poll_shard(
                &self,
                shard_id: u16,
                from_id: Option<&str>,
                limit: usize,
            ) -> Result<ShardPollResult, AdapterError> {
                if shard_id == self.fail_shard {
                    return Err(AdapterError::Transient(format!(
                        "synthetic failure on shard {shard_id}"
                    )));
                }
                self.inner.poll_shard(shard_id, from_id, limit).await
            }
            fn name(&self) -> &'static str {
                "failing-mock"
            }
        }

        let inner = MockAdapter::new();
        // Shard 0 has events, shard 1 will fail, shard 2 has events.
        inner.add_events(
            0,
            vec![StoredEvent::from_value(
                "0-1".to_string(),
                json!({"shard": 0}),
                100,
                0,
            )],
        );
        inner.add_events(
            2,
            vec![StoredEvent::from_value(
                "2-1".to_string(),
                json!({"shard": 2}),
                100,
                2,
            )],
        );

        let adapter = Arc::new(FailingShardMock {
            inner,
            fail_shard: 1,
        });
        let merger = PollMerger::new(adapter, vec![0, 1, 2]);

        let response = merger.poll(ConsumeRequest::new(100)).await.unwrap();

        // Surviving shards' events still come through.
        assert_eq!(
            response.events.len(),
            2,
            "events from non-failing shards must still be returned"
        );

        // The failed shard id is surfaced on the response.
        assert_eq!(
            response.failed_shards,
            vec![1],
            "regression: failed_shards must list the shard whose adapter \
             errored. Pre-fix this list didn't exist; observers couldn't \
             tell which shard was missing without log scraping."
        );
    }

    #[tokio::test]
    async fn test_poll_merger_with_events() {
        let adapter = Arc::new(MockAdapter::new());

        // Add events to shard 0
        adapter.add_events(
            0,
            vec![
                StoredEvent::from_value("0-1".to_string(), json!({"type": "a"}), 100, 0),
                StoredEvent::from_value("0-2".to_string(), json!({"type": "b"}), 200, 0),
            ],
        );

        // Add events to shard 1
        adapter.add_events(
            1,
            vec![StoredEvent::from_value(
                "1-1".to_string(),
                json!({"type": "c"}),
                150,
                1,
            )],
        );

        let merger = PollMerger::new(adapter, vec![0, 1]);

        let request = ConsumeRequest::new(100);
        let response = merger.poll(request).await.unwrap();

        assert_eq!(response.events.len(), 3);
        assert!(response.next_id.is_some());
    }

    #[tokio::test]
    async fn test_poll_merger_with_ordering() {
        let adapter = Arc::new(MockAdapter::new());

        // Add events with different timestamps
        adapter.add_events(
            0,
            vec![
                StoredEvent::from_value("0-1".to_string(), json!({}), 300, 0),
                StoredEvent::from_value("0-2".to_string(), json!({}), 100, 0),
            ],
        );
        adapter.add_events(
            1,
            vec![StoredEvent::from_value(
                "1-1".to_string(),
                json!({}),
                200,
                1,
            )],
        );

        let merger = PollMerger::new(adapter, vec![0, 1]);

        let request = ConsumeRequest::new(100).ordering(Ordering::InsertionTs);
        let response = merger.poll(request).await.unwrap();

        // Events should be sorted by insertion_ts
        assert_eq!(response.events.len(), 3);
        assert_eq!(response.events[0].insertion_ts, 100);
        assert_eq!(response.events[1].insertion_ts, 200);
        assert_eq!(response.events[2].insertion_ts, 300);
    }

    #[tokio::test]
    async fn test_poll_merger_with_filter() {
        let adapter = Arc::new(MockAdapter::new());

        adapter.add_events(
            0,
            vec![
                StoredEvent::from_value("0-1".to_string(), json!({"type": "token"}), 100, 0),
                StoredEvent::from_value("0-2".to_string(), json!({"type": "message"}), 200, 0),
                StoredEvent::from_value("0-3".to_string(), json!({"type": "token"}), 300, 0),
            ],
        );

        let merger = PollMerger::new(adapter, vec![0]);

        let request = ConsumeRequest::new(100).filter(Filter::eq("type", json!("token")));
        let response = merger.poll(request).await.unwrap();

        assert_eq!(response.events.len(), 2);
        for event in &response.events {
            assert!(event.raw_str().unwrap().contains("token"));
        }
    }

    #[tokio::test]
    async fn test_poll_merger_with_limit() {
        let adapter = Arc::new(MockAdapter::new());

        adapter.add_events(
            0,
            vec![
                StoredEvent::from_value("0-1".to_string(), json!({}), 100, 0),
                StoredEvent::from_value("0-2".to_string(), json!({}), 200, 0),
                StoredEvent::from_value("0-3".to_string(), json!({}), 300, 0),
            ],
        );

        let merger = PollMerger::new(adapter, vec![0]);

        let request = ConsumeRequest::new(2);
        let response = merger.poll(request).await.unwrap();

        assert_eq!(response.events.len(), 2);
        assert!(response.has_more);
    }

    #[tokio::test]
    async fn test_poll_merger_specific_shards() {
        let adapter = Arc::new(MockAdapter::new());

        adapter.add_events(
            0,
            vec![StoredEvent::from_value(
                "0-1".to_string(),
                json!({"shard": 0}),
                100,
                0,
            )],
        );
        adapter.add_events(
            1,
            vec![StoredEvent::from_value(
                "1-1".to_string(),
                json!({"shard": 1}),
                100,
                1,
            )],
        );
        adapter.add_events(
            2,
            vec![StoredEvent::from_value(
                "2-1".to_string(),
                json!({"shard": 2}),
                100,
                2,
            )],
        );

        let merger = PollMerger::new(adapter, vec![0, 1, 2]);

        // Only poll shard 0 and 2
        let request = ConsumeRequest::new(100).shards(vec![0, 2]);
        let response = merger.poll(request).await.unwrap();

        assert_eq!(response.events.len(), 2);
        let shard_ids: Vec<_> = response.events.iter().map(|e| e.shard_id).collect();
        assert!(shard_ids.contains(&0));
        assert!(shard_ids.contains(&2));
        assert!(!shard_ids.contains(&1));
    }

    #[tokio::test]
    async fn test_poll_merger_with_cursor() {
        let adapter = Arc::new(MockAdapter::new());

        adapter.add_events(
            0,
            vec![
                StoredEvent::from_value("0-1".to_string(), json!({}), 100, 0),
                StoredEvent::from_value("0-2".to_string(), json!({}), 200, 0),
                StoredEvent::from_value("0-3".to_string(), json!({}), 300, 0),
            ],
        );

        let merger = PollMerger::new(adapter, vec![0]);

        // First poll
        let request = ConsumeRequest::new(2);
        let response1 = merger.poll(request).await.unwrap();
        assert_eq!(response1.events.len(), 2);

        // Second poll with cursor
        let cursor = response1.next_id.unwrap();
        let request2 = ConsumeRequest::new(10).from(cursor);
        let response2 = merger.poll(request2).await.unwrap();

        assert_eq!(response2.events.len(), 1);
        assert_eq!(response2.events[0].id, "0-3");
    }

    #[tokio::test]
    async fn test_poll_merger_pagination_multi_shard() {
        // Test that pagination across multiple shards doesn't skip events
        let adapter = Arc::new(MockAdapter::new());

        // Shard 0: 10 events
        let shard0_events: Vec<_> = (1..=10)
            .map(|i| {
                StoredEvent::from_value(
                    format!("0-{}", i),
                    json!({"shard": 0, "idx": i}),
                    i as u64 * 10,
                    0,
                )
            })
            .collect();
        adapter.add_events(0, shard0_events);

        // Shard 1: 15 events
        let shard1_events: Vec<_> = (1..=15)
            .map(|i| {
                StoredEvent::from_value(
                    format!("1-{}", i),
                    json!({"shard": 1, "idx": i}),
                    i as u64 * 10 + 5,
                    1,
                )
            })
            .collect();
        adapter.add_events(1, shard1_events);

        let merger = PollMerger::new(adapter, vec![0, 1]);

        // Poll in pages of 10 and collect all events
        let mut all_events = Vec::new();
        let mut cursor: Option<String> = None;
        let mut iterations = 0;

        loop {
            iterations += 1;
            let request = match &cursor {
                Some(c) => ConsumeRequest::new(10).from(c.clone()),
                None => ConsumeRequest::new(10),
            };

            let response = merger.poll(request).await.unwrap();
            all_events.extend(response.events);

            if !response.has_more {
                break;
            }
            cursor = response.next_id;

            // Safety: prevent infinite loop
            if iterations > 10 {
                panic!("Too many iterations");
            }
        }

        // Should get all 25 events (10 from shard 0 + 15 from shard 1)
        assert_eq!(
            all_events.len(),
            25,
            "Expected 25 events, got {}. Iterations: {}",
            all_events.len(),
            iterations
        );

        // Verify we got events from both shards
        let shard0_count = all_events.iter().filter(|e| e.shard_id == 0).count();
        let shard1_count = all_events.iter().filter(|e| e.shard_id == 1).count();
        assert_eq!(shard0_count, 10, "Expected 10 events from shard 0");
        assert_eq!(shard1_count, 15, "Expected 15 events from shard 1");
    }

    #[tokio::test]
    async fn test_poll_merger_pagination_no_duplicates() {
        // Test that pagination doesn't return duplicate events
        let adapter = Arc::new(MockAdapter::new());

        // Add events to both shards
        for shard_id in 0..2u16 {
            let events: Vec<_> = (1..=20)
                .map(|i| {
                    StoredEvent::from_value(
                        format!("{}-{}", shard_id, i),
                        json!({"shard": shard_id, "idx": i}),
                        i as u64 * 10,
                        shard_id,
                    )
                })
                .collect();
            adapter.add_events(shard_id, events);
        }

        let merger = PollMerger::new(adapter, vec![0, 1]);

        // Poll in small pages
        let mut all_event_ids = Vec::new();
        let mut cursor: Option<String> = None;

        for _ in 0..20 {
            let request = match &cursor {
                Some(c) => ConsumeRequest::new(5).from(c.clone()),
                None => ConsumeRequest::new(5),
            };

            let response = merger.poll(request).await.unwrap();
            all_event_ids.extend(response.events.iter().map(|e| e.id.clone()));

            if !response.has_more {
                break;
            }
            cursor = response.next_id;
        }

        // Check for duplicates
        let unique_count = {
            let mut ids = all_event_ids.clone();
            ids.sort();
            ids.dedup();
            ids.len()
        };

        assert_eq!(
            unique_count,
            all_event_ids.len(),
            "Found duplicate events! Total: {}, Unique: {}",
            all_event_ids.len(),
            unique_count
        );

        // Should have all 40 events
        assert_eq!(all_event_ids.len(), 40);
    }

    #[tokio::test]
    async fn test_poll_merger_pagination_with_ordering() {
        // Test pagination with timestamp ordering
        let adapter = Arc::new(MockAdapter::new());

        // Add events with interleaved timestamps across shards
        adapter.add_events(
            0,
            vec![
                StoredEvent::from_value("0-1".to_string(), json!({}), 100, 0),
                StoredEvent::from_value("0-2".to_string(), json!({}), 300, 0),
                StoredEvent::from_value("0-3".to_string(), json!({}), 500, 0),
            ],
        );
        adapter.add_events(
            1,
            vec![
                StoredEvent::from_value("1-1".to_string(), json!({}), 200, 1),
                StoredEvent::from_value("1-2".to_string(), json!({}), 400, 1),
            ],
        );

        let merger = PollMerger::new(adapter, vec![0, 1]);

        // Poll with ordering, page size 2
        let mut all_events = Vec::new();
        let mut cursor: Option<String> = None;

        for _ in 0..5 {
            let mut request = ConsumeRequest::new(2).ordering(Ordering::InsertionTs);
            if let Some(c) = &cursor {
                request = request.from(c.clone());
            }

            let response = merger.poll(request).await.unwrap();
            all_events.extend(response.events);

            if !response.has_more {
                break;
            }
            cursor = response.next_id;
        }

        // Should get all 5 events
        assert_eq!(all_events.len(), 5);

        // Verify ordering is maintained
        let timestamps: Vec<_> = all_events.iter().map(|e| e.insertion_ts).collect();
        let mut sorted = timestamps.clone();
        sorted.sort();
        assert_eq!(timestamps, sorted, "Events should be sorted by timestamp");
    }

    #[tokio::test]
    async fn test_poll_merger_cursor_tracks_returned_events_only() {
        // Test that cursor tracks position based on returned events, not fetched events
        let adapter = Arc::new(MockAdapter::new());

        // Shard 0: 3 events
        adapter.add_events(
            0,
            vec![
                StoredEvent::from_value("0-1".to_string(), json!({}), 100, 0),
                StoredEvent::from_value("0-2".to_string(), json!({}), 200, 0),
                StoredEvent::from_value("0-3".to_string(), json!({}), 300, 0),
            ],
        );

        // Shard 1: 3 events
        adapter.add_events(
            1,
            vec![
                StoredEvent::from_value("1-1".to_string(), json!({}), 150, 1),
                StoredEvent::from_value("1-2".to_string(), json!({}), 250, 1),
                StoredEvent::from_value("1-3".to_string(), json!({}), 350, 1),
            ],
        );

        let merger = PollMerger::new(adapter, vec![0, 1]);

        // First poll with limit 2 - should get 2 events and cursor should reflect only those 2
        let response1 = merger.poll(ConsumeRequest::new(2)).await.unwrap();
        assert_eq!(response1.events.len(), 2);
        assert!(response1.has_more);

        // Decode cursor to verify it tracks returned events
        let next_id = response1.next_id.clone().unwrap();
        let cursor = CompositeCursor::decode(&next_id).unwrap();

        // Cursor should only have positions for shards that had events in the returned set
        let returned_shard_ids: std::collections::HashSet<_> =
            response1.events.iter().map(|e| e.shard_id).collect();

        for shard_id in 0..2u16 {
            if returned_shard_ids.contains(&shard_id) {
                // Shard had returned events, cursor should have position
                assert!(
                    cursor.get(shard_id).is_some(),
                    "Cursor should have position for shard {} which had returned events",
                    shard_id
                );
            }
        }

        // Second poll should continue from where we left off
        let response2 = merger
            .poll(ConsumeRequest::new(10).from(next_id))
            .await
            .unwrap();

        // Should get remaining 4 events
        assert_eq!(response2.events.len(), 4, "Should get remaining 4 events");
    }

    /// When the per-shard fetch hits the
    /// PER_SHARD_FETCH_CAP clamp, the response must surface
    /// `truncated_at_per_shard_cap = true` so callers can detect
    /// the silent under-delivery.
    #[tokio::test]
    async fn poll_merger_surfaces_per_shard_cap_truncation() {
        let adapter = Arc::new(MockAdapter::new());
        // Single shard — over-fetch factor 2 — request limit
        // 50 000 → unclamped per_shard would be 100 000 → clamped
        // to PER_SHARD_FETCH_CAP (10 000).
        adapter.add_events(
            0,
            (0..1)
                .map(|i| StoredEvent::from_value(format!("0-{}", i), json!({}), 100, 0))
                .collect(),
        );

        let merger = PollMerger::new(adapter, vec![0]);
        let response = merger.poll(ConsumeRequest::new(50_000)).await.unwrap();
        assert!(
            response.truncated_at_per_shard_cap,
            "large limit must flag the per-shard cap clamp",
        );
    }

    /// Corollary: a small request that fits well below
    /// the cap must NOT flag truncation.
    #[tokio::test]
    async fn poll_merger_does_not_flag_truncation_on_small_limit() {
        let adapter = Arc::new(MockAdapter::new());
        adapter.add_events(
            0,
            vec![StoredEvent::from_value(
                "0-1".to_string(),
                json!({}),
                100,
                0,
            )],
        );
        let merger = PollMerger::new(adapter, vec![0]);
        let response = merger.poll(ConsumeRequest::new(100)).await.unwrap();
        assert!(
            !response.truncated_at_per_shard_cap,
            "small limits must not flag the cap",
        );
    }

    #[tokio::test]
    async fn test_poll_merger_small_limit_many_shards() {
        // Regression: limit < shard count caused integer division truncation to 0,
        // making per-shard fetch too small. Now uses ceiling division.
        let adapter = Arc::new(MockAdapter::new());
        let num_shards = 8u16;

        for shard_id in 0..num_shards {
            adapter.add_events(
                shard_id,
                vec![StoredEvent::from_value(
                    format!("{}-1", shard_id),
                    json!({"shard": shard_id}),
                    100,
                    shard_id,
                )],
            );
        }

        let merger = PollMerger::new(adapter, (0..num_shards).collect());

        // Request fewer events than shards — should still work
        let request = ConsumeRequest::new(3);
        let response = merger.poll(request).await.unwrap();

        assert_eq!(response.events.len(), 3);
        assert!(response.has_more);
    }

    #[tokio::test]
    async fn test_regression_filtered_shards_cursor_advances() {
        // Bug 3: "Cursor never advances for filtered-out shards"
        //
        // When shard 1's events are entirely filtered out, the cursor for shard 1
        // must still advance past those events. Otherwise, subsequent polls will
        // re-fetch the same filtered-out events forever.
        let adapter = Arc::new(MockAdapter::new());

        // Shard 0: events matching the filter
        adapter.add_events(
            0,
            vec![
                StoredEvent::from_value("0-1".to_string(), json!({"type": "token"}), 100, 0),
                StoredEvent::from_value("0-2".to_string(), json!({"type": "token"}), 200, 0),
            ],
        );

        // Shard 1: events that will be filtered out
        adapter.add_events(
            1,
            vec![
                StoredEvent::from_value("1-1".to_string(), json!({"type": "message"}), 150, 1),
                StoredEvent::from_value("1-2".to_string(), json!({"type": "message"}), 250, 1),
            ],
        );

        let merger = PollMerger::new(adapter, vec![0, 1]);
        let filter = Filter::eq("type", json!("token"));

        // First poll: should return only the "token" events from shard 0
        let response1 = merger
            .poll(ConsumeRequest::new(100).filter(filter.clone()))
            .await
            .unwrap();

        assert_eq!(response1.events.len(), 2, "Should get 2 token events");
        for event in &response1.events {
            assert_eq!(
                event.shard_id, 0,
                "All returned events should be from shard 0"
            );
        }

        let cursor1 = response1
            .next_id
            .expect("Should have a cursor after first poll");

        // Verify the cursor advanced for shard 1 even though its events were filtered out
        let decoded = CompositeCursor::decode(&cursor1).unwrap();
        assert!(
            decoded.get(1).is_some(),
            "Cursor must advance for shard 1 even though all its events were filtered out"
        );
        assert_eq!(
            decoded.get(1),
            Some("1-2"),
            "Shard 1 cursor should point to its last fetched event"
        );

        // Second poll with the cursor: should NOT re-fetch shard 1's events
        let response2 = merger
            .poll(ConsumeRequest::new(100).filter(filter).from(cursor1))
            .await
            .unwrap();

        assert!(
            response2.events.is_empty(),
            "Second poll should return no events (all events already consumed or filtered)"
        );
    }

    #[tokio::test]
    async fn test_regression_poll_merger_filter_does_not_infinite_loop() {
        // Regression: when one shard has events matching the filter and another
        // shard has events that are all filtered out, polling in pages must
        // terminate and return all matching events without looping forever.
        let adapter = Arc::new(MockAdapter::new());

        // Shard 0: 100 events all matching filter
        let shard0_events: Vec<_> = (1..=100)
            .map(|i| {
                StoredEvent::from_value(
                    format!("0-{}", i),
                    json!({"type": "token", "idx": i}),
                    i as u64 * 10,
                    0,
                )
            })
            .collect();
        adapter.add_events(0, shard0_events);

        // Shard 1: 100 events none matching filter
        let shard1_events: Vec<_> = (1..=100)
            .map(|i| {
                StoredEvent::from_value(
                    format!("1-{}", i),
                    json!({"type": "message", "idx": i}),
                    i as u64 * 10 + 5,
                    1,
                )
            })
            .collect();
        adapter.add_events(1, shard1_events);

        let merger = PollMerger::new(adapter, vec![0, 1]);
        let filter = Filter::eq("type", json!("token"));

        let mut all_events = Vec::new();
        let mut cursor: Option<String> = None;
        let max_iterations = 50;
        let mut iterations = 0;

        loop {
            iterations += 1;
            if iterations > max_iterations {
                panic!(
                    "Infinite loop detected after {} iterations! Collected {} events so far.",
                    max_iterations,
                    all_events.len()
                );
            }

            let mut request = ConsumeRequest::new(50).filter(filter.clone());
            if let Some(c) = &cursor {
                request = request.from(c.clone());
            }

            let response = merger.poll(request).await.unwrap();
            all_events.extend(response.events);

            if !response.has_more {
                break;
            }
            cursor = response.next_id;
        }

        // Should have collected exactly 100 matching events from shard 0
        assert_eq!(
            all_events.len(),
            100,
            "Expected 100 matching events, got {}. Iterations: {}",
            all_events.len(),
            iterations
        );

        // All events should be from shard 0 (the "token" shard)
        for event in &all_events {
            assert_eq!(
                event.shard_id, 0,
                "All matching events should come from shard 0"
            );
        }

        // Verify no duplicates
        let mut ids: Vec<_> = all_events.iter().map(|e| e.id.clone()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 100, "Should have no duplicate events");
    }

    #[tokio::test]
    async fn test_regression_all_events_filtered_returns_cursor() {
        // Regression: when every fetched event was filtered out, next_id was
        // None, leaving the caller stuck re-fetching the same events forever.
        let adapter = Arc::new(MockAdapter::new());

        // Only non-matching events
        adapter.add_events(
            0,
            vec![
                StoredEvent::from_value("0-1".to_string(), json!({"type": "noise"}), 100, 0),
                StoredEvent::from_value("0-2".to_string(), json!({"type": "noise"}), 200, 0),
            ],
        );

        let merger = PollMerger::new(adapter, vec![0]);
        let filter = Filter::eq("type", json!("signal"));

        let response = merger
            .poll(ConsumeRequest::new(100).filter(filter))
            .await
            .unwrap();

        // No events match, but cursor must still advance
        assert!(response.events.is_empty());
        assert!(
            response.next_id.is_some(),
            "cursor must advance past filtered events even when none match"
        );
    }

    /// Exercises the non-lazy filter branch: when `Ordering::InsertionTs`
    /// is requested we can't short-circuit at `limit + 1` matches (the
    /// sort needs every event first), so the code falls through to
    /// `retain` → sort → truncate. This test pins:
    /// - Results are globally sorted by `insertion_ts` (not input order).
    /// - Only filter-matching events come through.
    /// - Truncation picks the `limit` *earliest* matches by ts.
    /// - `has_more` is set when matches exceed `limit`.
    #[tokio::test]
    async fn test_poll_merger_filter_insertion_ts_truncates_after_sort() {
        let adapter = Arc::new(MockAdapter::new());

        // Interleave shards with out-of-order timestamps and a mix of
        // matching / non-matching events. Matching timestamps: 120, 200,
        // 260, 400. Non-matching timestamps: 100, 300.
        adapter.add_events(
            0,
            vec![
                StoredEvent::from_value("0-1".to_string(), json!({"type": "token"}), 400, 0),
                StoredEvent::from_value("0-2".to_string(), json!({"type": "noise"}), 100, 0),
                StoredEvent::from_value("0-3".to_string(), json!({"type": "token"}), 200, 0),
            ],
        );
        adapter.add_events(
            1,
            vec![
                StoredEvent::from_value("1-1".to_string(), json!({"type": "token"}), 260, 1),
                StoredEvent::from_value("1-2".to_string(), json!({"type": "noise"}), 300, 1),
                StoredEvent::from_value("1-3".to_string(), json!({"type": "token"}), 120, 1),
            ],
        );

        let merger = PollMerger::new(adapter, vec![0, 1]);
        let filter = Filter::eq("type", json!("token"));

        // 4 matches exist; asking for 2 must yield the two earliest
        // after a full sort (120, 200) and signal has_more.
        let response = merger
            .poll(
                ConsumeRequest::new(2)
                    .filter(filter)
                    .ordering(Ordering::InsertionTs),
            )
            .await
            .unwrap();

        assert_eq!(response.events.len(), 2);
        assert_eq!(
            response.events[0].insertion_ts, 120,
            "earliest match must come first"
        );
        assert_eq!(response.events[1].insertion_ts, 200);
        assert!(
            response.has_more,
            "two more matching events remain past the limit"
        );
    }

    #[tokio::test]
    async fn test_regression_corrupt_event_filter_drop_is_consistent_and_logged() {
        // Regression: corrupt events (raw bytes that don't deserialize as
        // JSON) used to be silently dropped from the filtered poll path
        // via `event.parse().map(...).unwrap_or(false)`, while the
        // unfiltered path returned them as-is. That inconsistency hid
        // upstream framing/storage corruption from anyone running with a
        // filter (i.e. most consumers).
        //
        // The fix routes parse failures through `event_matches_filter`,
        // which emits `tracing::warn!` per dropped event. We don't have
        // a tracing-test subscriber wired up so we don't assert on the
        // log line itself; instead we pin the behavioral surface so a
        // future regression that re-silences corruption (e.g., dropping
        // the helper) shows up in code review:
        //   - filtered poll: corrupt event is dropped, valid event kept
        //   - unfiltered poll: corrupt event flows through unchanged
        //
        // If the helper is removed or the warn! is downgraded to debug!,
        // this test still passes — but the helper's doc-comment names
        // the inconsistency and is the artifact that protects the
        // observability requirement.
        let adapter = Arc::new(MockAdapter::new());
        adapter.add_events(
            0,
            vec![
                StoredEvent::from_value("0-1".to_string(), json!({"type": "ok"}), 100, 0),
                // Raw bytes that don't parse as JSON — a torn write or
                // upstream framing bug surface.
                StoredEvent::new(
                    "0-2".to_string(),
                    bytes::Bytes::from_static(b"\xff\xff not json \xff"),
                    200,
                    0,
                ),
            ],
        );

        let merger = PollMerger::new(adapter, vec![0]);

        // Filtered: corrupt event must be dropped, valid event kept.
        let filtered = merger
            .poll(ConsumeRequest::new(100).filter(Filter::eq("type", json!("ok"))))
            .await
            .unwrap();
        assert_eq!(
            filtered.events.len(),
            1,
            "filtered poll must drop the corrupt event"
        );
        assert_eq!(filtered.events[0].id, "0-1");

        // Unfiltered: corrupt event flows through. Documenting that the
        // unfiltered path is the *only* way an operator currently sees
        // the corrupt bytes — without the warn! the filtered path is
        // a black hole.
        let unfiltered = merger.poll(ConsumeRequest::new(100)).await.unwrap();
        assert_eq!(
            unfiltered.events.len(),
            2,
            "unfiltered poll must surface the corrupt event verbatim"
        );
        let ids: Vec<_> = unfiltered.events.iter().map(|e| e.id.as_str()).collect();
        assert!(ids.contains(&"0-1"));
        assert!(ids.contains(&"0-2"));
    }

    /// Regression: BUG_REPORT.md #2 — `Ordering::None` filter previously
    /// broke out of the drain loop once `kept.len() >= limit + 1`,
    /// which silently discarded events from later shards without
    /// checking the filter. Combined with `new_cursor` advancing for
    /// every polled shard, that meant matching events on un-inspected
    /// shards were lost forever.
    ///
    /// Setup: shard 0 has matches followed by shard 1 with matches.
    /// With `limit=2`, shard 0's first three events (two matches plus
    /// one extra to trigger has_more) used to satisfy the early break,
    /// silently dropping shard 1's matches AND advancing past them.
    /// The fix runs a full `retain` pass over every fetched event,
    /// then rolls back the cursor for shards whose matches were
    /// truncated so they're re-fetched on the next poll.
    #[tokio::test]
    async fn test_regression_ordering_none_filter_does_not_strand_later_shards() {
        let adapter = Arc::new(MockAdapter::new());

        // Shard 0: 3 matching events.
        adapter.add_events(
            0,
            vec![
                StoredEvent::from_value("0-1".to_string(), json!({"type": "token"}), 100, 0),
                StoredEvent::from_value("0-2".to_string(), json!({"type": "token"}), 110, 0),
                StoredEvent::from_value("0-3".to_string(), json!({"type": "token"}), 120, 0),
            ],
        );
        // Shard 1: 3 matching events.
        adapter.add_events(
            1,
            vec![
                StoredEvent::from_value("1-1".to_string(), json!({"type": "token"}), 200, 1),
                StoredEvent::from_value("1-2".to_string(), json!({"type": "token"}), 210, 1),
                StoredEvent::from_value("1-3".to_string(), json!({"type": "token"}), 220, 1),
            ],
        );

        let merger = PollMerger::new(adapter, vec![0, 1]);
        let filter = Filter::eq("type", json!("token"));

        // Page through with a small limit — over many polls every
        // matching event must surface exactly once. Bound iterations
        // to detect either a stall or an explosion.
        let mut all_returned: Vec<String> = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..20 {
            let mut req = ConsumeRequest::new(2).filter(filter.clone());
            if let Some(c) = &cursor {
                req = req.from(c.clone());
            }
            let resp = merger.poll(req).await.unwrap();
            for e in &resp.events {
                all_returned.push(e.id.clone());
            }
            if !resp.has_more {
                break;
            }
            cursor = resp.next_id;
        }

        all_returned.sort();
        all_returned.dedup();
        assert_eq!(
            all_returned,
            vec!["0-1", "0-2", "0-3", "1-1", "1-2", "1-3"],
            "every matching event from every shard must be returned exactly once"
        );
    }

    /// Regression: BUG_REPORT.md #23 — `Ordering::InsertionTs` filter
    /// previously stranded matches on shards whose matching events
    /// all sorted later than `limit` matches from other shards. The
    /// global sort+truncate dropped them, the cursor-override only
    /// fired for shards present in the *returned* set, and so the
    /// cursor for the un-returned shard advanced to its fetched
    /// position via `new_cursor` — silently skipping the matches.
    ///
    /// Setup: shard 0 has 3 early-ts matches and shard 1 has 3
    /// late-ts matches. With `limit=2` and `InsertionTs` ordering,
    /// the first poll returns the two earliest from shard 0;
    /// shard 1's matches must NOT be lost. The fix detects that
    /// shard 1 had matches truncated and rolls its cursor back so
    /// they're re-fetched on the next poll.
    #[tokio::test]
    async fn test_regression_insertion_ts_filter_does_not_strand_late_shard() {
        let adapter = Arc::new(MockAdapter::new());

        adapter.add_events(
            0,
            vec![
                StoredEvent::from_value("0-1".to_string(), json!({"type": "token"}), 100, 0),
                StoredEvent::from_value("0-2".to_string(), json!({"type": "token"}), 110, 0),
                StoredEvent::from_value("0-3".to_string(), json!({"type": "token"}), 120, 0),
            ],
        );
        adapter.add_events(
            1,
            vec![
                StoredEvent::from_value("1-1".to_string(), json!({"type": "token"}), 1000, 1),
                StoredEvent::from_value("1-2".to_string(), json!({"type": "token"}), 1010, 1),
                StoredEvent::from_value("1-3".to_string(), json!({"type": "token"}), 1020, 1),
            ],
        );

        let merger = PollMerger::new(adapter, vec![0, 1]);
        let filter = Filter::eq("type", json!("token"));

        let mut all_returned: Vec<String> = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..20 {
            let mut req = ConsumeRequest::new(2)
                .filter(filter.clone())
                .ordering(Ordering::InsertionTs);
            if let Some(c) = &cursor {
                req = req.from(c.clone());
            }
            let resp = merger.poll(req).await.unwrap();
            for e in &resp.events {
                all_returned.push(e.id.clone());
            }
            if !resp.has_more {
                break;
            }
            cursor = resp.next_id;
        }

        all_returned.sort();
        all_returned.dedup();
        assert_eq!(
            all_returned,
            vec!["0-1", "0-2", "0-3", "1-1", "1-2", "1-3"],
            "matches from the late-ts shard must not be lost to truncation"
        );
    }

    /// Regression: BUG_REPORT.md #50 — if any adapter returns
    /// `has_more: true` with no events and no `next_id`, the merger
    /// previously forwarded that as `(has_more=true, next_id=None)`,
    /// causing the caller to re-poll from the same starting cursor
    /// indefinitely. The fix suppresses `has_more` whenever the
    /// merger itself made no observable progress (no events AND no
    /// cursor advance).
    #[tokio::test]
    async fn has_more_is_suppressed_when_no_progress() {
        struct LiarAdapter;

        #[async_trait]
        impl Adapter for LiarAdapter {
            async fn init(&mut self) -> Result<(), AdapterError> {
                Ok(())
            }
            async fn on_batch(&self, _batch: Batch) -> Result<(), AdapterError> {
                Ok(())
            }
            async fn flush(&self) -> Result<(), AdapterError> {
                Ok(())
            }
            async fn shutdown(&self) -> Result<(), AdapterError> {
                Ok(())
            }
            async fn poll_shard(
                &self,
                _shard_id: u16,
                _from_id: Option<&str>,
                _limit: usize,
            ) -> Result<ShardPollResult, AdapterError> {
                // The pathological case: claim has_more without
                // returning events or advancing the cursor.
                Ok(ShardPollResult {
                    events: Vec::new(),
                    next_id: None,
                    has_more: true,
                })
            }
            fn name(&self) -> &'static str {
                "liar"
            }
        }

        let adapter: Arc<dyn Adapter> = Arc::new(LiarAdapter);
        let merger = PollMerger::new(adapter, vec![0, 1]);
        let response = merger.poll(ConsumeRequest::new(100)).await.unwrap();

        assert!(
            response.events.is_empty(),
            "no events were emitted, but merger returned {}",
            response.events.len()
        );
        // The whole point of this fix: don't let a misbehaving
        // adapter trick the caller into an infinite re-poll.
        assert!(
            !response.has_more,
            "has_more must be suppressed when merger made no progress (#50)"
        );
        assert!(
            response.next_id.is_none(),
            "next_id must remain None when no progress was made (#50)"
        );
    }

    /// Pin: a stalled poll (no events, no cursor advance) that
    /// was given an input cursor must echo the cursor back to the
    /// caller. Pre-fix the merger returned `next_id = None` on no
    /// progress, so a caller that interpreted None as "no events
    /// — restart from the beginning" silently re-fetched from the
    /// stream's start across the stall, losing pagination
    /// continuity.
    #[tokio::test]
    async fn stalled_poll_echoes_caller_cursor_back() {
        struct EmptyAdapter;

        #[async_trait]
        impl Adapter for EmptyAdapter {
            async fn init(&mut self) -> Result<(), AdapterError> {
                Ok(())
            }
            async fn on_batch(&self, _batch: Batch) -> Result<(), AdapterError> {
                Ok(())
            }
            async fn flush(&self) -> Result<(), AdapterError> {
                Ok(())
            }
            async fn shutdown(&self) -> Result<(), AdapterError> {
                Ok(())
            }
            async fn poll_shard(
                &self,
                _shard_id: u16,
                _from_id: Option<&str>,
                _limit: usize,
            ) -> Result<ShardPollResult, AdapterError> {
                Ok(ShardPollResult {
                    events: Vec::new(),
                    next_id: None,
                    has_more: false,
                })
            }
            fn name(&self) -> &'static str {
                "empty"
            }
        }

        let adapter: Arc<dyn Adapter> = Arc::new(EmptyAdapter);
        let merger = PollMerger::new(adapter, vec![0, 1]);

        // Build a real composite cursor for the request to echo.
        let mut cursor = CompositeCursor::new();
        cursor.set(0, "1702-0".to_string());
        cursor.set(1, "1703-0".to_string());
        let encoded = cursor.encode().unwrap();

        let mut req = ConsumeRequest::new(100);
        req.from_id = Some(encoded.clone());

        let response = merger.poll(req).await.unwrap();

        assert!(response.events.is_empty());
        assert_eq!(
            response.next_id.as_deref(),
            Some(encoded.as_str()),
            "stalled poll with input cursor must echo cursor back \
             (got {:?}); pre-fix this was None and callers paged \
             back to the stream's start",
            response.next_id,
        );

        // Without an input cursor, no progress + no input → still
        // None (preserving the prior behavior of #50).
        let response_no_cursor = merger.poll(ConsumeRequest::new(100)).await.unwrap();
        assert!(response_no_cursor.next_id.is_none());
    }

    /// Regression: BUG_REPORT.md #52 — `sort_by_key(|e| e.insertion_ts)`
    /// is stable but ties across shards depend on `join_all`'s
    /// completion order, which is non-deterministic. Combined with
    /// `truncate(limit)`, this could drop or duplicate events at the
    /// limit boundary across consecutive polls. The fix breaks ties
    /// deterministically on `(shard_id, id)`.
    #[tokio::test]
    async fn sort_breaks_ties_deterministically_across_shards() {
        // Two shards with events that share `insertion_ts` so the
        // tiebreaker controls the order.
        let adapter = Arc::new(MockAdapter::new());
        adapter.add_events(
            0,
            vec![
                StoredEvent::from_value("0-a".to_string(), json!({}), 100, 0),
                StoredEvent::from_value("0-b".to_string(), json!({}), 100, 0),
            ],
        );
        adapter.add_events(
            1,
            vec![
                StoredEvent::from_value("1-a".to_string(), json!({}), 100, 1),
                StoredEvent::from_value("1-b".to_string(), json!({}), 100, 1),
            ],
        );

        // Poll many times; the order must be stable.
        let merger = PollMerger::new(adapter, vec![0, 1]);
        let mut prior_order: Option<Vec<String>> = None;
        for iter in 0..20 {
            let r = merger
                .poll(ConsumeRequest::new(10).ordering(Ordering::InsertionTs))
                .await
                .unwrap();
            let ids: Vec<String> = r.events.iter().map(|e| e.id.clone()).collect();
            if let Some(prev) = &prior_order {
                assert_eq!(
                    &ids, prev,
                    "iter {iter}: order is non-deterministic — sort tie-break failed (#52)"
                );
            }
            prior_order = Some(ids);
        }

        // And the order must match `(shard_id, id)`.
        let r = merger
            .poll(ConsumeRequest::new(10).ordering(Ordering::InsertionTs))
            .await
            .unwrap();
        let ids: Vec<String> = r.events.iter().map(|e| e.id.clone()).collect();
        assert_eq!(ids, vec!["0-a", "0-b", "1-a", "1-b"]);
    }
}
