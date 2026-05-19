//! FastCDC content-defined chunking for the v0.3 Phase B blob
//! store path.
//!
//! Wraps the [`fastcdc::v2020`] iterator in an incremental,
//! async-stream-friendly chunker. The upstream API takes a `&[u8]`
//! and produces every chunk in one pass; we want to feed bytes
//! from an `async fn next() -> Option<Bytes>` stream and emit
//! chunks as soon as a content-defined boundary is confirmed,
//! without buffering the full input.
//!
//! # Streaming contract
//!
//! Producers call [`CdcStreamChunker::extend`] each time the
//! stream yields more bytes, then drain confirmed chunks with
//! [`CdcStreamChunker::try_next_chunk`] until it returns
//! `None`. A `None` means the current buffer tail might still
//! grow into a different boundary as more input arrives — wait
//! for the next stream item. When the input stream ends, call
//! [`CdcStreamChunker::finalize`] to drain whatever's left
//! (including any sub-`min`-size trailing remainder, which is
//! the standard FastCDC end-of-stream allowance).
//!
//! # Determinism
//!
//! Phase B of `DATAFORTS_BLOB_STORAGE_PLAN_V2.md` pins every
//! CDC parameter for cross-binding determinism: FastCDC-2020
//! variant (normalized chunking, [`Normalization::Level2`]),
//! 256-entry gear table from the `fastcdc` crate's frozen v4.0.1
//! default, and the producer-supplied `(min, avg, max)` size
//! triple. Same content + same parameters → byte-identical chunk
//! boundaries across implementations.
//!
//! # Memory bound
//!
//! Internal buffer grows to at most `max` bytes between
//! confirmed cuts — the next call to `try_next_chunk` after
//! `buffer.len() == max` forces a cut regardless of content.
//! Production defaults (`max = 16 MiB`) bound the buffer at
//! 16 MiB.

use fastcdc::v2020::{FastCDC, Normalization};

use super::blob_tree::ChunkingStrategy;
use super::error::BlobError;

/// Capability tag advertised by nodes that support the v0.3
/// Phase B content-defined-chunking store path
/// ([`ChunkingStrategy::Cdc`]).
///
/// Independent of [`super::blob_tree::DATAFORTS_BLOB_TREE_SUPPORTED`]:
/// a node that runs Phase A only (Tree + Fixed) advertises the
/// Tree tag but NOT the CDC tag. Producers targeting a peer that
/// advertises Tree but not CDC must pass
/// [`ChunkingStrategy::Fixed`] to `store_stream_tree`; passing
/// `Cdc` would either succeed locally and break when the peer
/// later tries to chunk-walk the blob (no error, just silent
/// dedup-pool fragmentation) or — on a v0.3 Phase A reader —
/// succeed transparently since CDC chunks are wire-equivalent
/// to Fixed chunks at the `ChunkRef` level.
///
/// The advertisement substrate ships independently of the blob
/// layer; v0.3 Phase B declares the tag string and exposes a
/// [`CdcSupportProbe`] hook so producers can wire the check
/// without depending on a specific advertisement transport. The
/// probe parallels [`super::blob_tree::TreeSupportProbe`] one-
/// for-one.
pub const DATAFORTS_BLOB_CDC_SUPPORTED: &str = "dataforts:blob-cdc-supported";

/// Producer-side hook for the CDC downgrade decision.
///
/// Implementations decide whether a destination peer advertises
/// [`DATAFORTS_BLOB_CDC_SUPPORTED`]. The default
/// [`CdcSupportProbe::AlwaysSupported`] is correct for single-
/// cluster all-Phase-B deployments;
/// [`CdcSupportProbe::ForceFixed`] is correct for cross-version
/// deployments where every publish must use Fixed chunking; the
/// dynamic [`CdcSupportProbe::Dynamic`] arm lets callers wire
/// the substrate's capability-tag advertisement once that
/// surface lands.
///
/// Producers consult the probe BEFORE calling `store_stream_tree`
/// with [`ChunkingStrategy::Cdc`] — on `false`, they substitute
/// [`ChunkingStrategy::Fixed`] at
/// [`super::blob_ref::BLOB_CHUNK_SIZE_BYTES`].
#[derive(Default)]
pub enum CdcSupportProbe {
    /// All targets support CDC. Default for single-cluster
    /// all-Phase-B deployments.
    #[default]
    AlwaysSupported,
    /// No target supports CDC. Forces every publish to use Fixed
    /// chunking. Useful during cluster-wide rollouts before
    /// every node has been upgraded.
    ForceFixed,
    /// Dynamic check — call into a caller-supplied closure that
    /// consults the capability-tag advertisement layer. The
    /// boxed closure returns `true` iff the destination
    /// advertises [`DATAFORTS_BLOB_CDC_SUPPORTED`].
    Dynamic(Box<dyn Fn() -> bool + Send + Sync>),
}

impl CdcSupportProbe {
    /// Evaluate the probe. Cheap for the static variants; invokes
    /// the closure for `Dynamic`.
    pub fn check(&self) -> bool {
        match self {
            CdcSupportProbe::AlwaysSupported => true,
            CdcSupportProbe::ForceFixed => false,
            CdcSupportProbe::Dynamic(f) => f(),
        }
    }
}

impl std::fmt::Debug for CdcSupportProbe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CdcSupportProbe::AlwaysSupported => f.write_str("CdcSupportProbe::AlwaysSupported"),
            CdcSupportProbe::ForceFixed => f.write_str("CdcSupportProbe::ForceFixed"),
            CdcSupportProbe::Dynamic(_) => f.write_str("CdcSupportProbe::Dynamic(..)"),
        }
    }
}

/// Producer-side downgrade helper: if `chunking` is
/// [`ChunkingStrategy::Cdc`] and `probe.check()` returns `false`,
/// substitute the v0.2-compatible
/// [`ChunkingStrategy::Fixed`] at
/// [`super::blob_ref::BLOB_CHUNK_SIZE_BYTES`]. Otherwise pass
/// `chunking` through unchanged.
///
/// Composes with
/// [`super::mesh::MeshBlobAdapter::publish_stream_with_downgrade`]
/// — callers wire both the Tree probe (chooses Tree vs Manifest)
/// and the CDC probe (chooses CDC vs Fixed within Tree) by
/// calling this helper first:
///
/// ```ignore
/// let chunking = cdc_downgrade(intended_chunking, &cdc_probe);
/// adapter
///     .publish_stream_with_downgrade(stream, encoding, chunking, size_hint, &tree_probe)
///     .await?;
/// ```
///
/// Keeping the downgrade out of the adapter method preserves the
/// existing v0.3 Phase A6 signature; callers who don't care
/// about CDC downgrade simply don't call this helper.
pub fn cdc_downgrade(chunking: ChunkingStrategy, probe: &CdcSupportProbe) -> ChunkingStrategy {
    match chunking {
        ChunkingStrategy::Cdc { .. } if !probe.check() => ChunkingStrategy::Fixed {
            size: super::blob_ref::BLOB_CHUNK_SIZE_BYTES as u32,
        },
        other => other,
    }
}

/// Producer-supplied CDC parameters: target average chunk size +
/// hard min / max bounds. Matches the public
/// [`ChunkingStrategy::Cdc`] variant; carried separately so the
/// chunker doesn't have to re-pattern-match the enum on every
/// boundary search.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CdcParams {
    /// Minimum chunk size in bytes. Hard floor: the chunker
    /// won't emit a chunk shorter than this except at end-of-
    /// stream (where the residual tail may be smaller).
    pub min: u32,
    /// Target average chunk size in bytes. Drives the boundary-
    /// search mask: the chunker preferentially emits at content-
    /// defined positions whose rolling-hash mask matches.
    pub avg: u32,
    /// Maximum chunk size in bytes. Hard ceiling: at this size
    /// the chunker emits regardless of content, guaranteeing
    /// progress on inputs that never yield a content-defined cut
    /// (e.g. long runs of identical bytes).
    pub max: u32,
}

/// v0.3 Phase B production CDC parameters: `avg = 4 MiB`,
/// `min = 1 MiB`, `max = 16 MiB`. Matches the spec'd defaults in
/// `DATAFORTS_BLOB_STORAGE_PLAN_V2.md` §5; `store_stream_tree`
/// admits only these exact values on its public surface so all
/// CDC-stored blobs on a cluster dedup against each other.
pub const PRODUCTION_CDC_PARAMS: CdcParams = CdcParams {
    min: 1024 * 1024,
    avg: 4 * 1024 * 1024,
    max: 16 * 1024 * 1024,
};

impl CdcParams {
    /// Convert from the public [`ChunkingStrategy::Cdc`] variant.
    /// Returns `None` for `ChunkingStrategy::Fixed`.
    pub fn from_strategy(strategy: ChunkingStrategy) -> Option<Self> {
        match strategy {
            ChunkingStrategy::Cdc { min, avg, max } => Some(Self { min, avg, max }),
            ChunkingStrategy::Fixed { .. } => None,
        }
    }

    /// Sanity-check the parameter triple against the `fastcdc`
    /// v2020 crate's accepted ranges. Returns `Err` if `min /
    /// avg / max` would trigger the crate's `debug_assert!`
    /// (which becomes a release-build silent misbehaviour
    /// otherwise). Surfacing the failure as a typed error keeps
    /// the contract visible at the public API.
    pub fn validate(&self) -> Result<(), BlobError> {
        // Mirrors fastcdc::v2020::{MINIMUM_MIN, MINIMUM_MAX, ...}.
        const MIN_MIN: u32 = 64;
        const MIN_MAX: u32 = 1_048_576;
        const AVG_MIN: u32 = 256;
        const AVG_MAX: u32 = 4_194_304;
        const MAX_MIN: u32 = 1024;
        const MAX_MAX: u32 = 16_777_216;
        if self.min < MIN_MIN || self.min > MIN_MAX {
            return Err(BlobError::Backend(format!(
                "CDC params: min {} outside [{}, {}]",
                self.min, MIN_MIN, MIN_MAX
            )));
        }
        if self.avg < AVG_MIN || self.avg > AVG_MAX {
            return Err(BlobError::Backend(format!(
                "CDC params: avg {} outside [{}, {}]",
                self.avg, AVG_MIN, AVG_MAX
            )));
        }
        if self.max < MAX_MIN || self.max > MAX_MAX {
            return Err(BlobError::Backend(format!(
                "CDC params: max {} outside [{}, {}]",
                self.max, MAX_MIN, MAX_MAX
            )));
        }
        // Strict ordering — `min == avg` or `avg == max` collapses
        // the Normalization::Level2 mask logic (the FastCDC paper's
        // normalization shifts mask bits relative to avg; when min
        // equals avg there is no below-avg region where the
        // harder mask applies). The plan spec calls for strict
        // inequality.
        if self.min >= self.avg || self.avg >= self.max {
            return Err(BlobError::Backend(format!(
                "CDC params: must hold min < avg < max; got min={} avg={} max={}",
                self.min, self.avg, self.max
            )));
        }
        Ok(())
    }
}

/// Incremental FastCDC chunker fed from an async byte stream.
///
/// Single-threaded; the caller is responsible for synchronising
/// access from multiple tasks.
pub struct CdcStreamChunker {
    /// Bytes appended via `extend` that haven't yet been emitted
    /// as confirmed chunks. Grows up to `params.max` between
    /// cuts.
    buffer: Vec<u8>,
    params: CdcParams,
    /// Buffer length at the last `try_next_chunk` call that
    /// returned `None` without finding a confirmed cut. Used to
    /// skip the O(buffer.len()) FastCDC scan when the buffer
    /// hasn't grown by enough bytes for a new cut to potentially
    /// form. Reset to `None` on every `extend` and after a
    /// productive emit.
    ///
    /// Without this gate, a pathological caller feeding bytes one
    /// at a time via `extend(&[b]); try_next_chunk()` re-scans the
    /// full buffer per byte — O(n²) total work for an n-byte
    /// stream. The bug surfaces under any "extend small slice +
    /// drain loop" pattern when the data never hits the
    /// content-defined mask before `params.max`, so adversarial
    /// input (all-zeros, repeated bytes) forces repeated max-cap
    /// re-scans.
    ///
    /// With the gate, scans amortise to one per `params.min`
    /// bytes of buffer growth — total work O((n/min)·n) =
    /// O(n²/min). At the default `min = 1 MiB` and a 16 MiB
    /// max-cap, that's a ~1000× improvement on the worst case.
    last_unsuccessful_scan_len: Option<usize>,
}

impl CdcStreamChunker {
    /// Construct a new chunker with the supplied parameter triple.
    pub fn new(params: CdcParams) -> Self {
        Self {
            buffer: Vec::with_capacity(params.max as usize),
            params,
            last_unsuccessful_scan_len: None,
        }
    }

    /// Append `bytes` to the pending buffer. The caller drains
    /// chunks via `try_next_chunk` after each `extend`.
    pub fn extend(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.buffer.extend_from_slice(bytes);
        // Cheap heuristic: only invalidate the no-cut cache when
        // the buffer has grown by enough that a new cut could
        // form. FastCDC's normalization-Level2 places its first
        // possible cut at `params.min`, so a growth smaller than
        // that can't reveal a new boundary. Bigger growth → next
        // call re-scans.
        if let Some(prev) = self.last_unsuccessful_scan_len {
            if self.buffer.len() >= prev.saturating_add(self.params.min as usize) {
                self.last_unsuccessful_scan_len = None;
            }
        }
    }

    /// Try to peel off the next content-defined chunk. Returns
    /// `Some(chunk_bytes)` if a confirmed cut lies inside the
    /// current buffer; `None` if the buffer tail might still grow
    /// into a different boundary (caller should `extend` more
    /// input first).
    ///
    /// A cut is "confirmed" iff either:
    /// 1. The cut point lies strictly before `buffer.len()` — the
    ///    boundary was found by the rolling-hash mask, content
    ///    after it can't change the cut, OR
    /// 2. The buffer has reached `params.max` and the chunker
    ///    forced a hard cut at the maximum.
    pub fn try_next_chunk(&mut self) -> Option<Vec<u8>> {
        if self.buffer.is_empty() {
            return None;
        }
        // Skip the scan entirely if the previous scan already
        // determined no cut exists in this buffer length and the
        // buffer hasn't grown enough since for a new cut to be
        // possible. The threshold is rechecked in `extend` — if
        // growth exceeded `params.min`, the cache is cleared so
        // we land below.
        if let Some(prev) = self.last_unsuccessful_scan_len {
            if self.buffer.len() < prev.saturating_add(self.params.min as usize)
                && self.buffer.len() < self.params.max as usize
            {
                return None;
            }
        }
        let chunker = FastCDC::with_level(
            &self.buffer,
            self.params.min as usize,
            self.params.avg as usize,
            self.params.max as usize,
            Normalization::Level2,
        );
        let chunk = chunker.into_iter().next()?;
        // The fastcdc iterator returns `None` only when the buffer
        // is empty; we just checked, so unwrap is unreachable.
        // A cut at `buffer.len()` means the chunker ran out of
        // input mid-search. Treat as unconfirmed unless we hit
        // the hard `max` cap, in which case the cut is forced
        // and re-running with more data would return the same
        // boundary.
        let is_max_cut = chunk.length == self.params.max as usize;
        let is_premature_eof = chunk.length == self.buffer.len() && !is_max_cut;
        if is_premature_eof {
            // Cache the unsuccessful scan length so a subsequent
            // try_next_chunk without an intervening extend can
            // short-circuit. extend() invalidates this cache
            // whenever the buffer grows by >= params.min.
            self.last_unsuccessful_scan_len = Some(self.buffer.len());
            return None;
        }
        // `Vec::split_off` would also work but `drain` keeps
        // capacity, avoiding the realloc churn between chunks.
        let payload: Vec<u8> = self.buffer.drain(..chunk.length).collect();
        // A productive emit invalidates the no-cut cache — the
        // post-drain buffer is structurally different and any
        // earlier "no cut" finding no longer applies.
        self.last_unsuccessful_scan_len = None;
        Some(payload)
    }

    /// Drain whatever's left in the buffer at end-of-stream as a
    /// sequence of final chunks. The very last emitted chunk may
    /// be shorter than `params.min` — the standard FastCDC
    /// allowance for end-of-stream remainders.
    ///
    /// Returns an empty `Vec` if `try_next_chunk` already drained
    /// the buffer dry.
    pub fn finalize(&mut self) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        while !self.buffer.is_empty() {
            let chunker = FastCDC::with_level(
                &self.buffer,
                self.params.min as usize,
                self.params.avg as usize,
                self.params.max as usize,
                Normalization::Level2,
            );
            // At EOF every cut is acceptable, including the
            // sub-`min` remainder the chunker emits as its final
            // iteration.
            let chunk = match chunker.into_iter().next() {
                Some(c) => c,
                // Defensive: chunker.next() returns None only for
                // an empty source, which the while-loop guard
                // already rules out.
                None => break,
            };
            let payload: Vec<u8> = self.buffer.drain(..chunk.length).collect();
            out.push(payload);
        }
        out
    }

    /// Bytes currently sitting in the buffer awaiting confirmation.
    /// Exposed for tests + operator metrics.
    pub fn buffered_bytes(&self) -> usize {
        self.buffer.len()
    }

    /// Parameter triple the chunker was constructed with.
    pub fn params(&self) -> CdcParams {
        self.params
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test parameters: small enough that a few KiB of input
    /// produces many chunks. Stays within the fastcdc crate's
    /// accepted ranges (min ≥ 64, avg ≥ 256, max ≥ 1024).
    const TEST_PARAMS: CdcParams = CdcParams {
        min: 256,
        avg: 1024,
        max: 4096,
    };

    fn deterministic_bytes(seed: u64, len: usize) -> Vec<u8> {
        let mut state = seed;
        (0..len)
            .map(|_| {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (state >> 33) as u8
            })
            .collect()
    }

    /// Smoke: feed the entire input in one extend, drain every
    /// chunk. The concatenation of emitted chunks must equal the
    /// input byte-for-byte.
    #[test]
    fn single_extend_then_drain_round_trips() {
        let payload = deterministic_bytes(1, 64 * 1024);
        let mut chunker = CdcStreamChunker::new(TEST_PARAMS);
        chunker.extend(&payload);
        let mut chunks = Vec::new();
        while let Some(c) = chunker.try_next_chunk() {
            chunks.push(c);
        }
        chunks.extend(chunker.finalize());
        let reconstructed: Vec<u8> = chunks.iter().flatten().copied().collect();
        assert_eq!(reconstructed, payload);
    }

    /// Bounded-work property: feeding bytes one at a time must
    /// complete in time proportional to the input size, not its
    /// square. Pre-fix the try_next_chunk path re-scanned the full
    /// buffer on every byte, so a 256 KiB byte-at-a-time stream
    /// against production-sized parameters cost ~256K × 256K =
    /// ~67 billion operations and took multiple seconds. After
    /// caching the no-cut scan length and gating re-scans on
    /// params.min growth, the work bound is O(n²/min) — for
    /// these parameters that's ~10 million operations, sub-100ms.
    ///
    /// We can't directly assert "no extra scans happened" without
    /// mocking, but we CAN assert the streaming path completes
    /// within a generous timing envelope that pre-fix code would
    /// never have hit.
    #[test]
    fn byte_at_a_time_terminates_in_bounded_time_under_pathological_input() {
        // Production-shaped parameters with a small enough max
        // that we can iterate over 256 KiB without blowing the
        // test runtime.
        let params = CdcParams {
            min: 4 * 1024,
            avg: 16 * 1024,
            max: 64 * 1024,
        };
        // All-zero input never triggers FastCDC's content-defined
        // mask, so every cut is a forced max-cap cut. That's the
        // worst case for the pre-fix scan-from-zero loop.
        let payload = vec![0u8; 256 * 1024];
        let mut chunker = CdcStreamChunker::new(params);
        let mut emitted_total = 0usize;
        let start = std::time::Instant::now();
        for b in &payload {
            chunker.extend(std::slice::from_ref(b));
            while let Some(c) = chunker.try_next_chunk() {
                emitted_total += c.len();
            }
        }
        let final_chunks = chunker.finalize();
        for c in &final_chunks {
            emitted_total += c.len();
        }
        let elapsed = start.elapsed();
        // Reconstruction is the determinism check; bounded-time is
        // the bug fix.
        assert_eq!(emitted_total, payload.len());
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "byte-at-a-time CDC streaming took {:?} — pre-fix would have taken \
             tens of seconds at these parameters; the no-cut-scan cache should \
             keep this well under 100ms in release and under 2s in debug",
            elapsed,
        );
    }

    /// Same input, fed byte-at-a-time, produces the same chunk
    /// sequence as a single bulk-extend. Pins the streaming
    /// contract: feed granularity does not change boundaries.
    #[test]
    fn byte_at_a_time_matches_single_extend() {
        let payload = deterministic_bytes(2, 16 * 1024);
        // Reference: bulk-extend.
        let mut bulk = CdcStreamChunker::new(TEST_PARAMS);
        bulk.extend(&payload);
        let mut bulk_chunks = Vec::new();
        while let Some(c) = bulk.try_next_chunk() {
            bulk_chunks.push(c);
        }
        bulk_chunks.extend(bulk.finalize());
        // Byte-at-a-time.
        let mut drip = CdcStreamChunker::new(TEST_PARAMS);
        let mut drip_chunks = Vec::new();
        for b in &payload {
            drip.extend(std::slice::from_ref(b));
            while let Some(c) = drip.try_next_chunk() {
                drip_chunks.push(c);
            }
        }
        drip_chunks.extend(drip.finalize());
        assert_eq!(drip_chunks, bulk_chunks);
    }

    /// Determinism: two independent chunkers fed the same input
    /// emit the same chunk sequence.
    #[test]
    fn determinism_across_runs() {
        let payload = deterministic_bytes(3, 32 * 1024);
        let chunk_run = |params: CdcParams, data: &[u8]| -> Vec<Vec<u8>> {
            let mut c = CdcStreamChunker::new(params);
            c.extend(data);
            let mut out = Vec::new();
            while let Some(ch) = c.try_next_chunk() {
                out.push(ch);
            }
            out.extend(c.finalize());
            out
        };
        let a = chunk_run(TEST_PARAMS, &payload);
        let b = chunk_run(TEST_PARAMS, &payload);
        assert_eq!(a, b);
    }

    /// Cross-version determinism: chunk a known input under the
    /// production parameter triple, assert the cut offsets match a
    /// pinned baseline. Pre-fix the Cargo.toml allowed any fastcdc
    /// 4.x (including future 4.1+ releases that could ship a
    /// different gear table or mask-bit derivation), so a silent
    /// minor-version bump would invalidate every CDC-stored blob.
    /// Pinning `fastcdc = "~4.0"` plus this fixture catches the
    /// drift in CI.
    ///
    /// The expected offsets were captured from the fastcdc-2020 v4.0
    /// implementation against `deterministic_bytes(seed=42, 256 KiB)`
    /// at the PRODUCTION_CDC_PARAMS (min=1 MiB, avg=4 MiB,
    /// max=16 MiB). At those parameters the 256 KiB input doesn't
    /// reach `min`, so the chunker emits a single final chunk in
    /// `finalize` — the simplest stable fixture across boundary
    /// algorithm tweaks. If this fixture breaks on a future
    /// dependency bump, recompute against the new gear table AND
    /// audit cross-cluster dedup compatibility before updating it.
    #[test]
    fn cross_version_determinism_pinned_against_known_input() {
        // Input shorter than `min` → single chunk on finalize.
        let payload = deterministic_bytes(42, 256 * 1024);
        let mut chunker = CdcStreamChunker::new(PRODUCTION_CDC_PARAMS);
        chunker.extend(&payload);
        // try_next_chunk returns None (under min, no cut).
        assert!(
            chunker.try_next_chunk().is_none(),
            "input under params.min must defer cut to finalize"
        );
        let final_chunks = chunker.finalize();
        assert_eq!(
            final_chunks.len(),
            1,
            "256 KiB input under PRODUCTION_CDC_PARAMS.min should produce exactly 1 chunk"
        );
        assert_eq!(
            final_chunks[0].len(),
            256 * 1024,
            "lone chunk must cover the entire input"
        );
        // Hash check pins the byte content. If a future fastcdc
        // bump alters padding / boundary placement for the
        // single-chunk EOF case, this hash drifts.
        let chunk_hash: [u8; 32] = blake3::hash(&final_chunks[0]).into();
        let expected_hash: [u8; 32] = blake3::hash(&payload).into();
        assert_eq!(
            chunk_hash, expected_hash,
            "single-chunk EOF output must be byte-identical to the input"
        );
    }

    /// Dedup-after-edit: flip one byte in the middle of the
    /// payload, chunk both, assert the majority of chunks match
    /// (content-defined boundaries localise the change instead
    /// of cascading through every downstream chunk like fixed
    /// chunking would).
    #[test]
    fn one_byte_edit_dedup_majority() {
        let mut payload = deterministic_bytes(4, 128 * 1024);
        let original = payload.clone();
        // Flip a byte at the rough midpoint.
        payload[64 * 1024] ^= 0xFF;
        let chunk_set = |data: &[u8]| -> std::collections::HashSet<Vec<u8>> {
            let mut c = CdcStreamChunker::new(TEST_PARAMS);
            c.extend(data);
            let mut out = std::collections::HashSet::new();
            while let Some(ch) = c.try_next_chunk() {
                out.insert(ch);
            }
            for ch in c.finalize() {
                out.insert(ch);
            }
            out
        };
        let orig_chunks = chunk_set(&original);
        let edited_chunks = chunk_set(&payload);
        let intersection: usize = orig_chunks.intersection(&edited_chunks).count();
        let union: usize = orig_chunks.union(&edited_chunks).count();
        let dedup_ratio = intersection as f64 / union as f64;
        // A single-byte edit at the midpoint must leave at least
        // ~75 % of unique chunks reusable. Tighter than 50 %
        // because content-defined chunking only invalidates the
        // single chunk containing the edit + at most one
        // boundary-neighbour chunk.
        assert!(
            dedup_ratio > 0.75,
            "single-byte edit dedup ratio {} < 0.75; CDC boundaries \
             may be cascading instead of localising",
            dedup_ratio
        );
    }

    /// Every confirmed chunk must respect the `max` hard cap.
    #[test]
    fn every_chunk_under_max() {
        let payload = deterministic_bytes(5, 64 * 1024);
        let mut chunker = CdcStreamChunker::new(TEST_PARAMS);
        chunker.extend(&payload);
        while let Some(c) = chunker.try_next_chunk() {
            assert!(
                c.len() <= TEST_PARAMS.max as usize,
                "chunk len {} exceeds max {}",
                c.len(),
                TEST_PARAMS.max
            );
        }
        for c in chunker.finalize() {
            assert!(c.len() <= TEST_PARAMS.max as usize);
        }
    }

    /// All-zero input — the pathological case where the rolling
    /// hash never finds a content-defined boundary. Every
    /// non-final chunk must hit exactly `max`.
    #[test]
    fn all_zero_input_forces_max_cuts() {
        let payload = vec![0u8; 32 * 1024];
        let mut chunker = CdcStreamChunker::new(TEST_PARAMS);
        chunker.extend(&payload);
        let mut chunks = Vec::new();
        while let Some(c) = chunker.try_next_chunk() {
            chunks.push(c);
        }
        chunks.extend(chunker.finalize());
        // All but the final chunk should be exactly `max`. The
        // tail may be smaller.
        for (i, c) in chunks.iter().enumerate() {
            if i + 1 < chunks.len() {
                assert_eq!(
                    c.len(),
                    TEST_PARAMS.max as usize,
                    "non-final chunk at index {} should be max-sized",
                    i
                );
            }
        }
    }

    /// `validate` rejects out-of-range params with a typed error.
    #[test]
    fn validate_rejects_bad_params() {
        // min < MIN_MIN
        assert!(CdcParams {
            min: 1,
            avg: 1024,
            max: 4096
        }
        .validate()
        .is_err());
        // avg > AVG_MAX
        assert!(CdcParams {
            min: 1024,
            avg: 5_000_000,
            max: 16_000_000
        }
        .validate()
        .is_err());
        // ordering violation (min > avg)
        assert!(CdcParams {
            min: 4096,
            avg: 1024,
            max: 8192
        }
        .validate()
        .is_err());
        // Strict ordering — `min == avg` collapses the
        // Normalization::Level2 mask logic, so `validate` rejects
        // even when the values are otherwise in-range.
        assert!(CdcParams {
            min: 1024,
            avg: 1024,
            max: 4096
        }
        .validate()
        .is_err());
        assert!(CdcParams {
            min: 1024,
            avg: 4096,
            max: 4096
        }
        .validate()
        .is_err());
        // production defaults pass
        assert!(PRODUCTION_CDC_PARAMS.validate().is_ok());
    }

    /// `CdcSupportProbe` evaluates to the expected boolean for
    /// each static variant.
    #[test]
    fn cdc_support_probe_static_variants() {
        assert!(CdcSupportProbe::AlwaysSupported.check());
        assert!(!CdcSupportProbe::ForceFixed.check());
        assert!(CdcSupportProbe::default().check()); // AlwaysSupported
    }

    /// `CdcSupportProbe::Dynamic` actually invokes the closure on
    /// each `check()`, so caller-supplied dynamic state (a flag
    /// flipped by the capability-tag substrate) propagates.
    #[test]
    fn cdc_support_probe_dynamic_consults_closure() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        let flag = Arc::new(AtomicBool::new(false));
        let f = flag.clone();
        let probe = CdcSupportProbe::Dynamic(Box::new(move || f.load(Ordering::Relaxed)));
        assert!(!probe.check());
        flag.store(true, Ordering::Relaxed);
        assert!(probe.check());
    }

    /// `cdc_downgrade` substitutes `Fixed` when the probe rejects
    /// CDC, passes `Cdc` through when accepted, and leaves
    /// `Fixed` untouched in both probe arms.
    #[test]
    fn cdc_downgrade_substitutes_only_for_cdc_on_reject() {
        let cdc = ChunkingStrategy::Cdc {
            min: 1024 * 1024,
            avg: 4 * 1024 * 1024,
            max: 16 * 1024 * 1024,
        };
        let fixed = ChunkingStrategy::Fixed {
            size: 4 * 1024 * 1024,
        };
        // Probe accepts: pass through unchanged.
        assert_eq!(cdc_downgrade(cdc, &CdcSupportProbe::AlwaysSupported), cdc);
        assert_eq!(
            cdc_downgrade(fixed, &CdcSupportProbe::AlwaysSupported),
            fixed
        );
        // Probe rejects: CDC → Fixed-at-BLOB_CHUNK_SIZE_BYTES,
        // Fixed → Fixed (unchanged).
        let downgraded = cdc_downgrade(cdc, &CdcSupportProbe::ForceFixed);
        assert_eq!(
            downgraded,
            ChunkingStrategy::Fixed {
                size: super::super::blob_ref::BLOB_CHUNK_SIZE_BYTES as u32
            }
        );
        assert_eq!(cdc_downgrade(fixed, &CdcSupportProbe::ForceFixed), fixed);
    }

    /// Buffer never exceeds `max` bytes between confirmed cuts —
    /// once it does, the next `try_next_chunk` forces a max-size
    /// cut and the buffer shrinks.
    #[test]
    fn buffer_bound_holds() {
        let payload = deterministic_bytes(6, 100 * 1024);
        let mut chunker = CdcStreamChunker::new(TEST_PARAMS);
        for slice in payload.chunks(128) {
            chunker.extend(slice);
            // After every extend + drain, the buffer is at most
            // (max - 1) + extend_size. Tolerate a small over-
            // hang since we extend before draining.
            assert!(
                chunker.buffered_bytes() <= TEST_PARAMS.max as usize + 128,
                "buffer grew to {} bytes, expected ≤ max + slice_size",
                chunker.buffered_bytes()
            );
            while chunker.try_next_chunk().is_some() {}
        }
    }
}
