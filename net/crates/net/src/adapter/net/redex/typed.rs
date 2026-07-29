//! Typed wrapper over `RedexFile` — auto-serde via postcard.
//!
//! `TypedRedexFile<T>` is a thin layer over [`RedexFile`] that hides
//! the `&[u8] ↔ T` boundary. Callers pass / receive `T` directly;
//! the wrapper handles serialization and deserialization.
//!
//! Semantics (errors, retention, watcher behavior, disk durability,
//! snapshot) are inherited from the underlying `RedexFile` — this is
//! purely an ergonomic layer.
//!
//! See `docs/internal/plans/REDEX_V2_PLAN.md` §4a.

use std::marker::PhantomData;

use bytes::Bytes;
use futures::{Stream, StreamExt};
use serde::de::DeserializeOwned;
use serde::Serialize;

use super::error::RedexError;
use super::file::RedexFile;

/// Typed wrapper over a `RedexFile`. `T` is the domain event type;
/// it must be `Serialize + DeserializeOwned` for postcard.
pub struct TypedRedexFile<T> {
    inner: RedexFile,
    _marker: PhantomData<fn() -> T>,
}

impl<T> TypedRedexFile<T> {
    /// Wrap an existing `RedexFile`. Cheap — just stores the handle.
    pub fn new(file: RedexFile) -> Self {
        Self {
            inner: file,
            _marker: PhantomData,
        }
    }

    /// Borrow the underlying file for untyped operations (length,
    /// retention sweep, close, etc.).
    pub fn file(&self) -> &RedexFile {
        &self.inner
    }
}

impl<T: Serialize> TypedRedexFile<T> {
    /// Serialize with postcard and append. Returns the assigned seq.
    pub fn append(&self, value: &T) -> Result<u64, RedexError> {
        let bytes = postcard::to_allocvec(value)
            .map_err(|e| RedexError::Encode(format!("typed append serialize: {}", e)))?;
        self.inner.append(&bytes)
    }

    /// Serialize a batch and append atomically (all entries land in
    /// the index contiguously or none do).
    ///
    /// Returns `Some(first_seq)` on non-empty input, `None` on empty
    /// input — propagates the underlying `RedexFile::append_batch`
    /// contract.
    pub fn append_batch(&self, values: &[T]) -> Result<Option<u64>, RedexError> {
        let mut buffers: Vec<Bytes> = Vec::with_capacity(values.len());
        for v in values {
            let bytes = postcard::to_allocvec(v)
                .map_err(|e| RedexError::Encode(format!("typed append_batch serialize: {}", e)))?;
            buffers.push(Bytes::from(bytes));
        }
        self.inner.append_batch(&buffers)
    }
}

impl<T: DeserializeOwned + Send + 'static> TypedRedexFile<T> {
    /// Tail the file from `from_seq` onward, yielding `(seq, value)`
    /// pairs. Deserialization errors surface as
    /// [`RedexError::Encode`] items — the stream continues.
    pub fn tail(
        &self,
        from_seq: u64,
    ) -> impl Stream<Item = Result<(u64, T), RedexError>> + Send + 'static {
        self.inner.tail(from_seq).map(|result| {
            let ev = result?;
            let seq = ev.entry.seq;
            let value: T = postcard::from_bytes(&ev.payload).map_err(|e| {
                RedexError::Encode(format!("typed tail deserialize at seq {}: {}", seq, e))
            })?;
            Ok((seq, value))
        })
    }

    /// Read the half-open range `[start, end)`, returning `(seq, value)`
    /// pairs. Deserialization errors are returned per-entry —
    /// callers decide whether to skip or abort.
    pub fn read_range(&self, start: u64, end: u64) -> Vec<Result<(u64, T), RedexError>> {
        self.inner
            .read_range(start, end)
            .into_iter()
            .map(|ev| {
                let seq = ev.entry.seq;
                let value: T = postcard::from_bytes(&ev.payload).map_err(|e| {
                    RedexError::Encode(format!(
                        "typed read_range deserialize at seq {}: {}",
                        seq, e
                    ))
                })?;
                Ok((seq, value))
            })
            .collect()
    }
}

impl<T> std::fmt::Debug for TypedRedexFile<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TypedRedexFile")
            .field("inner", &self.inner)
            .field("T", &std::any::type_name::<T>())
            .finish()
    }
}

impl<T> Clone for TypedRedexFile<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            _marker: PhantomData,
        }
    }
}
