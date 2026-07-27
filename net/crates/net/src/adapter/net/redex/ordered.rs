//! Single-threaded ordered-append helper for deterministic replay.
//!
//! `OrderedAppender` wraps a [`RedexFile`] and routes all appends
//! through the `_ordered` variants, which hold the state lock across
//! seq allocation. Under concurrent writers, that guarantees the
//! index records entries in strict seq order (no interleaving at
//! the `state.lock()` site).
//!
//! # Determinism
//!
//! For fully deterministic output — byte-identical logs across runs
//! given the same input sequence — use ONE `OrderedAppender` from
//! ONE thread. Multiple ordered appenders against the same file
//! still get correct seq ordering, but the mapping from input call
//! order to seq assignment depends on which thread wins each lock
//! acquisition and is not deterministic.
//!
//! See `docs/internal/plans/REDEX_V2_PLAN.md` §7.

use bytes::Bytes;

use super::entry::INLINE_PAYLOAD_SIZE;
use super::error::RedexError;
use super::file::RedexFile;

/// Single-threaded deterministic appender over a `RedexFile`.
///
/// Cheap to construct; holds a cloned `RedexFile` handle internally.
#[derive(Clone)]
pub struct OrderedAppender {
    file: RedexFile,
}

impl OrderedAppender {
    /// Wrap a `RedexFile`. The appender doesn't hold any exclusive
    /// resource; callers who care about determinism should ensure
    /// they use only one `OrderedAppender` per file.
    pub fn new(file: RedexFile) -> Self {
        Self { file }
    }

    /// The underlying file. Useful for tail / read_range operations
    /// from the same handle.
    pub fn file(&self) -> &RedexFile {
        &self.file
    }

    /// Append one payload (heap segment). Returns the assigned seq.
    /// Holds the file's state lock across seq allocation.
    pub fn append(&self, payload: &[u8]) -> Result<u64, RedexError> {
        self.file.append_ordered(payload)
    }

    /// Append a fixed 8-byte inline payload. Same ordering contract
    /// as [`Self::append`].
    pub fn append_inline(&self, payload: &[u8; INLINE_PAYLOAD_SIZE]) -> Result<u64, RedexError> {
        self.file.append_inline_ordered(payload)
    }

    /// Append a batch. The whole batch lands under one lock hold —
    /// seqs are strictly contiguous within the batch AND strictly
    /// ordered relative to other ordered writers.
    ///
    /// Returns `Some(first_seq)` on non-empty input, `None` on empty.
    pub fn append_batch(&self, payloads: &[Bytes]) -> Result<Option<u64>, RedexError> {
        self.file.append_batch_ordered(payloads)
    }
}

impl std::fmt::Debug for OrderedAppender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OrderedAppender")
            .field("file", &self.file)
            .finish()
    }
}
