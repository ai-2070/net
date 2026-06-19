//! Error type for RedEX operations.

use thiserror::Error;

/// Errors produced by RedEX operations.
#[derive(Debug, Error)]
pub enum RedexError {
    /// Payload is larger than the configured segment supports.
    #[error("payload too large: {size} bytes (max {max})")]
    PayloadTooLarge {
        /// Attempted payload size.
        size: usize,
        /// Maximum accepted payload size.
        max: usize,
    },

    /// Segment offset exceeded `u32::MAX`.
    ///
    /// Fires on long-running persistent files whose lifetime heap
    /// bytes have crossed the 4 GB `payload_offset` field width.
    /// Recoverable by reopening the file; disk recovery resets the
    /// base offset. Sweep-time offset renormalization lands in v2.
    #[error("segment offset overflow: offset {offset} exceeds u32::MAX")]
    SegmentOffsetOverflow {
        /// The overflowing absolute offset.
        offset: u64,
    },

    /// Requested sequence is outside the file's retained range.
    #[error("sequence {seq} is outside the retained range [{lo}, {hi})")]
    SeqOutOfRange {
        /// The requested sequence.
        seq: u64,
        /// Lowest retained sequence (inclusive).
        lo: u64,
        /// Next sequence to be assigned (exclusive upper bound).
        hi: u64,
    },

    /// A seq-guarded append (`append_batch_if_next_seq`) found the
    /// file's `next_seq` no longer equal to the caller-supplied
    /// `expected_first_seq` at the moment the append would run.
    ///
    /// #5: replication catch-up validates `first_seq == local_next`
    /// and then appends; a concurrent writer that advanced
    /// `next_seq` between the two would misalign the leader↔replica
    /// seqs. The guarded append rejects with this error instead of
    /// silently landing the batch at the wrong seqs. Recoverable —
    /// the caller (replica runtime) re-issues a `SYNC_REQUEST` from
    /// the new tail.
    #[error("seq guard failed: expected next_seq {expected}, found {actual}")]
    SeqMismatch {
        /// The `next_seq` the caller required (the chunk's
        /// `first_seq`).
        expected: u64,
        /// The `next_seq` actually observed under the append lock.
        actual: u64,
    },

    /// A channel name was rejected (e.g. invalid format, collision on open).
    #[error("channel error: {0}")]
    Channel(String),

    /// An encoding helper (e.g. `append_postcard`) failed to serialize.
    #[error("encode failed: {0}")]
    Encode(String),

    /// A decode helper (postcard, EventMeta shape, checksum) rejected
    /// a per-event payload. Distinct from [`Self::Encode`] so the
    /// fold-error-policy interpreter can treat per-event decode
    /// failures as skip-and-continue even under the `Stop` policy
    /// — otherwise a single corrupt or attacker-crafted event
    /// could wedge the fold task forever.
    #[error("decode failed: {0}")]
    Decode(String),

    /// Caller is not authorized to append or tail this file.
    #[error("unauthorized")]
    Unauthorized,

    /// Underlying I/O failure (disk segment only).
    #[error("io: {0}")]
    Io(String),

    /// The file has been closed.
    #[error("file closed")]
    Closed,

    /// A tail subscriber fell behind the per-subscription buffer and
    /// was disconnected. Delivery is best-effort: under saturation
    /// the buffer may be too full to accept this signal itself, in
    /// which case the subscriber observes only a plain stream end.
    #[error("tail subscriber lagged; stream disconnected")]
    Lagged,
}

impl RedexError {
    /// Construct from any `std::io::Error` with its message preserved.
    pub fn io(err: std::io::Error) -> Self {
        Self::Io(err.to_string())
    }

    /// Returns `true` when this error represents a per-event
    /// recoverable failure (a single bad event, NOT an
    /// underlying-storage failure that affects every subsequent
    /// event). The cortex fold-error-policy interpreter treats
    /// these as "always skip-and-continue" even under the `Stop`
    /// policy — otherwise a single corrupt postcard tail (or a
    /// 32-bit checksum collision) would wedge the fold task
    /// permanently and DoS a multi-tenant cortex instance via one
    /// bad event.
    ///
    /// Only `Decode` qualifies — it's stamped by the cortex fold
    /// implementations specifically on postcard / EventMeta /
    /// checksum failures. `Encode` is reserved for user-fold-level
    /// errors and storage-side encode failures, which legitimately
    /// halt under `Stop`. `Io` / `Closed` / `Lagged` are
    /// stream-level. `PayloadTooLarge` / `SegmentOffsetOverflow` /
    /// `SeqOutOfRange` / `SeqMismatch` / `Channel` / `Unauthorized`
    /// are configuration / concurrency / authorization issues.
    pub fn is_recoverable_decode(&self) -> bool {
        matches!(self, Self::Decode(_))
    }
}
