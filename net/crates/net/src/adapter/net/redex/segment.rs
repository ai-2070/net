//! Payload storage segments for RedEX.
//!
//! v1 has one segment type: `HeapSegment`, a grow-only `Vec<u8>` payload
//! store returning `(offset, len)` for each append. Range reads return
//! `Bytes` slices over the underlying buffer.
//!
//! Reclamation (for retention) is deferred: truncation of the head of
//! the buffer happens on the retention sweep by rewriting the segment
//! plus adjusting a `base_offset` that callers subtract from stored
//! entry offsets. v1 keeps that machinery inside `RedexFile`; this
//! module just provides the primitive append+read surface.

use bytes::{Bytes, BytesMut};

use super::error::RedexError;

/// Maximum heap segment size before `append` fails with
/// `PayloadTooLarge`. 32-bit offsets imply 4 GB hard max; we stay 1 GB
/// below to leave room for concurrent appends during a retention sweep.
pub(super) const MAX_SEGMENT_BYTES: usize = 3 * 1024 * 1024 * 1024; // 3 GB

/// In-memory payload segment.
///
/// Append-only from the caller's perspective. The retention sweep may
/// rewrite the buffer and advance `base_offset` to drop evicted heads;
/// all live offsets stored in `RedexEntry` records are absolute over
/// the logical seq-space and translated through `base_offset` on read.
///
/// The buffer is held as a [`Bytes`] so `read` returns zero-copy
/// `Bytes::slice` snapshots — refcount bumps only. Pre-fix
/// [perf #51 in `docs/performance/net-perf-analysis.md`] `read`
/// did a full `Bytes::copy_from_slice` on every call, costing one
/// memcpy per materialized event on every `tail` / `read_range` /
/// `read_one` / replication ship / watcher delivery path. For a
/// 4KB-payload watcher at 100K ev/s that was 400 MB/s of pure
/// memory bandwidth wasted on the copy.
///
/// Appends use [`Bytes::try_into_mut`] to extend in place when the
/// segment is the sole owner (the common case — readers consume
/// returned `Bytes` slices and drop them quickly). When outstanding
/// reader slices exist, `try_into_mut` falls back to a single
/// `BytesMut` allocation; existing slices keep their portion of
/// the old `Bytes` alive via refcount and stay valid.
#[derive(Debug)]
pub struct HeapSegment {
    buf: Bytes,
    /// The absolute offset of the first byte currently in `buf`.
    /// Starts at 0 and increases as eviction compacts the head.
    base_offset: u64,
}

impl HeapSegment {
    /// Create an empty segment.
    pub fn new() -> Self {
        Self {
            buf: Bytes::new(),
            base_offset: 0,
        }
    }

    /// Create an empty segment with `capacity` bytes reserved.
    pub fn with_capacity(capacity: usize) -> Self {
        // BytesMut::with_capacity reserves; freezing immediately
        // yields a Bytes whose capacity hint is carried into the
        // first `try_into_mut` round-trip.
        Self {
            buf: BytesMut::with_capacity(capacity).freeze(),
            base_offset: 0,
        }
    }

    /// Build a segment from pre-existing payload bytes (e.g. replayed
    /// from disk). The bytes become the live region starting at
    /// absolute offset 0.
    #[cfg(feature = "redex-disk")]
    pub(super) fn from_existing(buf: Vec<u8>) -> Self {
        Self {
            buf: Bytes::from(buf),
            base_offset: 0,
        }
    }

    /// Acquire the underlying buffer as a mutable [`BytesMut`].
    ///
    /// Fast path: when no reader holds a `Bytes` slice into the
    /// current buf, [`Bytes::try_into_mut`] returns the same
    /// allocation as a `BytesMut` in O(1). Slow path: an outstanding
    /// reader (typical only briefly during watcher delivery) forces
    /// a single `BytesMut::extend_from_slice` copy of the live
    /// region — the next `freeze` yields a fresh `Bytes` and the
    /// reader's stale snapshot stays valid via its own refcount.
    fn take_mut_or_copy(&mut self, additional: usize) -> BytesMut {
        match std::mem::take(&mut self.buf).try_into_mut() {
            Ok(m) => m,
            Err(bytes) => {
                let mut m = BytesMut::with_capacity(bytes.len() + additional);
                m.extend_from_slice(&bytes);
                m
            }
        }
    }

    /// Append `payload` and return the absolute offset it was written
    /// at (offset in the logical seq-space, not in the backing buffer).
    pub fn append(&mut self, payload: &[u8]) -> Result<u64, RedexError> {
        if self.buf.len().saturating_add(payload.len()) > MAX_SEGMENT_BYTES {
            return Err(RedexError::PayloadTooLarge {
                size: payload.len(),
                max: MAX_SEGMENT_BYTES.saturating_sub(self.buf.len()),
            });
        }
        let offset = self.base_offset + self.buf.len() as u64;
        let mut m = self.take_mut_or_copy(payload.len());
        m.extend_from_slice(payload);
        self.buf = m.freeze();
        Ok(offset)
    }

    /// Append every payload in `payloads` in order. Returns the
    /// absolute offset the FIRST payload was written at; subsequent
    /// payloads land at successive offsets.
    ///
    /// Performs one bounds check against the total size and one
    /// `reserve` before extending — equivalent to N `append` calls
    /// but with a single capacity check and a single allocation
    /// when the buffer needs to grow.
    pub fn append_many(&mut self, payloads: &[Bytes]) -> Result<u64, RedexError> {
        let total: usize = payloads.iter().map(|p| p.len()).sum();
        if self.buf.len().saturating_add(total) > MAX_SEGMENT_BYTES {
            return Err(RedexError::PayloadTooLarge {
                size: total,
                max: MAX_SEGMENT_BYTES.saturating_sub(self.buf.len()),
            });
        }
        let first = self.base_offset + self.buf.len() as u64;
        let mut m = self.take_mut_or_copy(total);
        m.reserve(total);
        for p in payloads {
            m.extend_from_slice(p);
        }
        self.buf = m.freeze();
        Ok(first)
    }

    /// Read `len` bytes starting at absolute `offset`. Returns `None`
    /// if the slice is not fully contained in the live region
    /// (evicted or past end).
    ///
    /// Zero-copy: returns a [`Bytes`] slice that shares the
    /// underlying allocation with the segment (refcount bump only).
    pub fn read(&self, offset: u64, len: u32) -> Option<Bytes> {
        let len = len as usize;
        if offset < self.base_offset {
            return None;
        }
        let rel = (offset - self.base_offset) as usize;
        let end = rel.checked_add(len)?;
        if end > self.buf.len() {
            return None;
        }
        Some(self.buf.slice(rel..end))
    }

    /// Number of live bytes currently in the segment.
    pub fn live_bytes(&self) -> usize {
        self.buf.len()
    }

    /// Absolute offset of the first live byte. Anything below this has
    /// been evicted.
    pub fn base_offset(&self) -> u64 {
        self.base_offset
    }

    /// Test-only: forcibly set the absolute base offset without
    /// touching the buffer. Used to simulate a long-lifetime file
    /// where eviction has pushed `base_offset` near `u32::MAX`,
    /// triggering the overflow path in `file.rs::offset_to_u32` and
    /// the pre-validation in `append_batch` / `append_batch_ordered`.
    #[cfg(test)]
    pub(super) fn force_base_offset(&mut self, base: u64) {
        self.base_offset = base;
    }

    /// Reset `base_offset` to zero without touching the buffer.
    ///
    /// Used by `RedexFile::sweep_retention` after a successful
    /// `disk.compact_to`: the on-disk dat is now rewritten to start
    /// at byte 0, so the in-memory segment must follow the same
    /// renormalization or subsequent appends will compute absolute
    /// offsets that index past the end of the new on-disk dat (BUG
    /// #92). Caller is responsible for renormalizing any external
    /// offsets (e.g. `RedexEntry::payload_offset` values stored in
    /// the index) by the prior `base_offset` value before calling
    /// this — otherwise reads through `read_at` will misalign.
    #[cfg(feature = "redex-disk")]
    pub(super) fn rebase_to_zero(&mut self) {
        self.base_offset = 0;
    }

    /// Test-only: mutate the underlying byte buffer in place.
    /// Used by checksum-on-read regression tests to simulate
    /// on-disk corruption without going through a real I/O path.
    ///
    /// Takes a closure rather than returning `&mut [u8]` because
    /// the new `Bytes`-backed buffer needs a `try_into_mut` /
    /// `freeze` round-trip to give the test a mutable view, and a
    /// returned `&mut [u8]` would dangle past the temporary
    /// `BytesMut`. The closure scopes the borrow correctly.
    #[cfg(test)]
    pub(super) fn with_bytes_for_test_mut<F>(&mut self, f: F)
    where
        F: FnOnce(&mut [u8]),
    {
        let mut m = self.take_mut_or_copy(0);
        f(&mut m);
        self.buf = m.freeze();
    }

    /// Evict the prefix of the segment strictly below `new_base` in
    /// the absolute offset space.
    ///
    /// Returns the number of bytes evicted. The retained tail
    /// becomes a fresh `Bytes` slice — refcount bump only.
    /// Existing reader slices into the evicted prefix stay valid
    /// via their own refcounts to the prior allocation.
    pub fn evict_prefix_to(&mut self, new_base: u64) -> u64 {
        if new_base <= self.base_offset {
            return 0;
        }
        let delta = (new_base - self.base_offset) as usize;
        let delta = delta.min(self.buf.len());
        // `Bytes::slice` is zero-copy; the old buf is dropped as
        // soon as the last outstanding reader releases its slice.
        // Materialize the tail into a fresh BytesMut so subsequent
        // appends can `try_into_mut` cheaply — otherwise the
        // sub-Bytes returned by `slice` still references the
        // original allocation and `try_into_mut` would have to
        // copy on every append.
        let mut m = BytesMut::with_capacity(self.buf.len() - delta);
        m.extend_from_slice(&self.buf[delta..]);
        self.buf = m.freeze();
        self.base_offset += delta as u64;
        delta as u64
    }
}

impl Default for HeapSegment {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_append_and_read() {
        let mut seg = HeapSegment::new();
        let o1 = seg.append(b"hello").unwrap();
        let o2 = seg.append(b"world").unwrap();
        assert_eq!(o1, 0);
        assert_eq!(o2, 5);

        assert_eq!(seg.read(o1, 5).unwrap().as_ref(), b"hello");
        assert_eq!(seg.read(o2, 5).unwrap().as_ref(), b"world");
    }

    #[test]
    fn test_read_out_of_range_returns_none() {
        let mut seg = HeapSegment::new();
        seg.append(b"abc").unwrap();
        assert!(seg.read(10, 3).is_none()); // offset past end
        assert!(seg.read(0, 10).is_none()); // len overruns
    }

    #[test]
    fn test_evict_prefix() {
        let mut seg = HeapSegment::new();
        seg.append(b"aaaa").unwrap();
        let o2 = seg.append(b"bbbb").unwrap();
        assert_eq!(o2, 4);

        let evicted = seg.evict_prefix_to(4);
        assert_eq!(evicted, 4);
        assert_eq!(seg.base_offset(), 4);
        assert_eq!(seg.live_bytes(), 4);

        // Old offset 0 is now below base
        assert!(seg.read(0, 4).is_none());
        // New read from o2 still works
        assert_eq!(seg.read(o2, 4).unwrap().as_ref(), b"bbbb");
    }

    #[test]
    fn test_evict_below_base_is_noop() {
        let mut seg = HeapSegment::new();
        seg.append(b"abc").unwrap();
        seg.evict_prefix_to(10);
        assert_eq!(seg.base_offset(), 3);
        // Further eviction below current base does nothing.
        assert_eq!(seg.evict_prefix_to(1), 0);
    }

    #[test]
    fn test_evict_beyond_live_clamps() {
        let mut seg = HeapSegment::new();
        seg.append(b"xyz").unwrap();
        // Eviction beyond the tail should clamp to tail without panic.
        let evicted = seg.evict_prefix_to(100);
        assert_eq!(evicted, 3);
        assert_eq!(seg.base_offset(), 3);
        assert_eq!(seg.live_bytes(), 0);
    }

    #[test]
    fn test_append_many_basic() {
        let mut seg = HeapSegment::new();
        // Pre-fill so the returned offset isn't 0 — pins that
        // `append_many` honors the existing buffer length.
        let pre = seg.append(b"prefix").unwrap();
        assert_eq!(pre, 0);

        let payloads: Vec<Bytes> = vec![
            Bytes::from_static(b"alpha"),
            Bytes::from_static(b"beta"),
            Bytes::from_static(b"gamma"),
        ];
        let first = seg.append_many(&payloads).unwrap();
        assert_eq!(first, 6, "first payload offset == prefix len");

        // Each payload must be readable at first + sum(prev lens).
        assert_eq!(seg.read(6, 5).unwrap().as_ref(), b"alpha");
        assert_eq!(seg.read(11, 4).unwrap().as_ref(), b"beta");
        assert_eq!(seg.read(15, 5).unwrap().as_ref(), b"gamma");
        assert_eq!(seg.live_bytes(), 6 + 5 + 4 + 5);
    }

    #[test]
    fn test_append_many_capacity_exceeded() {
        let mut seg = HeapSegment::new();
        // One huge payload (1 GiB) so the first append succeeds, then
        // a batch whose total pushes us past `MAX_SEGMENT_BYTES` (3
        // GiB). This is the multi-payload bounds-check path that a
        // per-payload loop would not catch until partway through.
        let big = vec![0u8; 1024 * 1024 * 1024];
        seg.append(&big).unwrap();
        seg.append(&big).unwrap();
        seg.append(&big).unwrap();
        // Now at MAX exactly. A two-payload batch totaling 2 bytes
        // must still be rejected.
        let payloads: Vec<Bytes> = vec![Bytes::from_static(b"x"), Bytes::from_static(b"y")];
        assert!(matches!(
            seg.append_many(&payloads),
            Err(RedexError::PayloadTooLarge { .. })
        ));
        // And the buffer state stayed clean — no partial extension.
        assert_eq!(seg.live_bytes(), MAX_SEGMENT_BYTES);
    }

    /// Pin perf #51: `read` returns a zero-copy `Bytes` slice
    /// over the underlying buffer, not a copy. We observe this
    /// by computing the buffer's allocation identity (the raw
    /// pointer) and asserting `read` returns a slice into the
    /// same allocation.
    ///
    /// `Bytes::as_ptr()` points into the buffer's data; for a
    /// non-empty Bytes that's the address of byte 0. For a
    /// slice produced by `Bytes::slice(r)` that's the address
    /// of byte `r.start` IN THE SAME allocation — i.e., we can
    /// compute `slice.as_ptr() - r.start` and it must equal the
    /// original buffer's `as_ptr()`.
    ///
    /// A regression that re-introduces `Bytes::copy_from_slice`
    /// allocates a fresh buffer; the pointer arithmetic above
    /// would yield an unrelated address.
    #[test]
    fn read_returns_zero_copy_slice_of_underlying_buffer() {
        let mut seg = HeapSegment::new();
        let payload = b"the quick brown fox jumps over the lazy dog";
        seg.append(payload).unwrap();
        let buf_ptr = seg.buf.as_ptr();

        let slice = seg.read(0, payload.len() as u32).unwrap();
        // Full-range read returns a Bytes whose data pointer
        // matches the buffer's data pointer exactly.
        assert_eq!(
            slice.as_ptr(),
            buf_ptr,
            "read(0, len) must return a zero-copy slice of the segment buffer",
        );
        assert_eq!(slice.as_ref(), payload);

        // Sub-range read: data pointer is offset by the start of
        // the range within the SAME allocation. Pre-fix
        // `Bytes::copy_from_slice` would put `sub` in a fresh
        // allocation completely unrelated to `buf_ptr`.
        let sub = seg.read(4, 5).unwrap();
        // Compute the address delta via `usize::wrapping_sub`,
        // not `<*const u8>::offset_from`. `offset_from` is
        // documented UB when the two pointers are NOT from the
        // same allocation — which is exactly the regression case
        // this test is trying to detect (a re-introduced
        // `Bytes::copy_from_slice` would place `sub` in a fresh
        // allocation unrelated to `buf_ptr`). The integer-cast
        // form is well-defined for any pointer values: in the
        // zero-copy case it yields exactly 4; in the regression
        // case it yields some large unrelated number that fails
        // the equality assertion cleanly without invoking UB.
        let sub_offset = (sub.as_ptr() as usize).wrapping_sub(buf_ptr as usize);
        assert_eq!(
            sub_offset, 4,
            "sub-range read must be a slice into the original buffer at offset 4; \
             got offset {sub_offset} (a regression here means read deep-copies)",
        );
        assert_eq!(sub.as_ref(), b"quick");
    }

    /// Companion to the zero-copy pin above: prove the
    /// `wrapping_sub` comparison correctly detects the
    /// regression-case shape (a `Bytes` from a fresh allocation
    /// unrelated to the segment buffer). Pre-fix this test
    /// would have used `<*const u8>::offset_from`, which is
    /// documented UB across allocations — the very case the
    /// regression test is meant to detect. The integer-cast
    /// form (`(p as usize).wrapping_sub(q as usize)`) is
    /// well-defined for any pointer pair.
    ///
    /// We construct what a deep-copy regression WOULD return:
    /// a fresh `Bytes::copy_from_slice` of the bytes at offset
    /// 4, holding the same content as the sub-slice but in a
    /// new allocation. The wrapping_sub against the segment's
    /// `buf_ptr` must yield some non-4 value (with overwhelming
    /// probability — distinct heap allocations don't land at a
    /// fixed offset of each other).
    #[test]
    fn read_zero_copy_pin_detects_deep_copy_via_wrapping_sub() {
        let mut seg = HeapSegment::new();
        let payload = b"the quick brown fox jumps over the lazy dog";
        seg.append(payload).unwrap();
        let buf_ptr = seg.buf.as_ptr();

        // Simulate the regression: a fresh allocation carrying
        // the same five bytes the zero-copy `read(4, 5)` would
        // produce. `Bytes::copy_from_slice` allocates a brand
        // new buffer; its data pointer is unrelated to
        // `buf_ptr`.
        let fake_deep_copy = bytes::Bytes::copy_from_slice(&payload[4..9]);
        assert_eq!(fake_deep_copy.as_ref(), b"quick");
        let fake_offset = (fake_deep_copy.as_ptr() as usize).wrapping_sub(buf_ptr as usize);
        // The two addresses live in different allocations. The
        // wrapping_sub is some arbitrary non-4 value — we can't
        // predict it, but we can assert it isn't 4 (the rare
        // collision case where the allocator happens to lay the
        // fresh buffer exactly 4 bytes past the segment buffer
        // is vanishingly improbable on any real allocator, and
        // even a deliberate adversary couldn't arrange it
        // through public API).
        assert_ne!(
            fake_offset, 4,
            "wrapping_sub form must distinguish a fresh allocation \
             from a same-allocation sub-slice — if these collide \
             the zero-copy pin would no longer detect a deep-copy \
             regression",
        );

        // And confirm the wrapping_sub form is non-UB by running
        // it across the two unrelated pointers without exploding.
        let _well_defined: usize = fake_offset;
    }

    #[test]
    fn test_append_many_empty_returns_current_end() {
        let mut seg = HeapSegment::new();
        seg.append(b"xyz").unwrap();
        // Empty batch is a no-op that returns the current end offset.
        let off = seg.append_many(&[]).unwrap();
        assert_eq!(off, 3);
        assert_eq!(seg.live_bytes(), 3);
    }
}
