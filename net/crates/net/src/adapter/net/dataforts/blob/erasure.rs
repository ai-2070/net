//! Reed-Solomon erasure-coding primitives for the v0.3 Phase C
//! blob store path.
//!
//! v0.3 Phase A + B store every chunk in full (the
//! [`Encoding::Replicated`](super::blob_ref::Encoding::Replicated)
//! path); chunk-level redundancy comes from cross-node replication.
//! Phase C adds the
//! [`Encoding::ReedSolomon { k, m }`](super::blob_ref::Encoding::ReedSolomon)
//! path: each stripe of `k` data chunks gets `m` parity chunks
//! computed via systematic Reed-Solomon over `GF(2^8)`, so the
//! stripe survives any `m` chunk losses (data OR parity) and only
//! pays `(k + m) / k` storage overhead — e.g. `(10, 4)` costs 1.4×
//! storage vs 2× for two-copy replication, with the same tolerance
//! to any 4 chunk losses per stripe.
//!
//! # Scope of this module (Phase C1)
//!
//! Phase C ships in several commits; C1 lands the foundational
//! primitives:
//!
//! - The [`RsParams`] `(k, m)` value type with hard validation
//!   (rejects `k == 0`, `m == 0`, `k + m > 255`) + a soft warning
//!   threshold at `k + m > 64`.
//! - [`RsEncoder`] — a thin wrapper around
//!   [`reed_solomon_erasure::ReedSolomon`] with `GF(2^8)`. Exposes
//!   `encode(data) -> parity` for the store path and
//!   `reconstruct_data(shards)` for the fetch path. All shards
//!   MUST be pre-padded to equal length by the caller (the striper
//!   that lands in C2 owns the padding policy).
//! - Striper constants ([`RS_STRIPE_TARGET_BYTES`],
//!   [`RS_STRIPE_MIN_BYTES`]) per the v0.3 plan §6.
//! - The [`DATAFORTS_BLOB_ERASURE_SUPPORTED`] capability tag +
//!   [`ErasureSupportProbe`] hook, mirroring the Tree/CDC probe
//!   pattern from Phase A6 / B2.
//!
//! Subsequent C-phase commits wire these into
//! `MeshBlobAdapter::store_stream_tree` (the striper), the fetch
//! path (optimistic data fetch + on-failure parity reconstruction),
//! the GC stripe-membership index, and the operator-driven repair
//! sweep.

use reed_solomon_erasure::galois_8;
use reed_solomon_erasure::ReedSolomon;

use super::blob_ref::Encoding;
use super::error::BlobError;

// ───────────────────────────────────────────────────────────────────────────
// Striper constants (used by C2)
// ───────────────────────────────────────────────────────────────────────────

/// Target accumulated *data bytes* before a stripe closes. Set to
/// `10 × 4 MiB = 40 MiB`, matching the default
/// `(k = 10, m = 4)` configuration's data-side capacity at the
/// production CDC average chunk size. Striping by bytes (not chunk
/// count) keeps the stripe predictable under CDC where chunks
/// range `[1 MiB, 16 MiB]`; a stripe spans 4-12 CDC chunks
/// depending on boundary distribution.
pub const RS_STRIPE_TARGET_BYTES: u64 = 40 * 1024 * 1024;

/// Minimum accumulated *data bytes* a stripe needs to actually
/// receive RS encoding. A stripe that hasn't reached this size at
/// end-of-stream (i.e., the blob is too small to fill a stripe)
/// falls back to [`Encoding::Replicated`] for that final partial
/// stripe — see the plan §6 small-stripe fallback. Without the
/// fallback, a 1 MiB blob stored under `(10, 4)` would carry 4 MiB
/// of parity overhead (5× storage); the fallback skips parity for
/// the small case.
pub const RS_STRIPE_MIN_BYTES: u64 = 8 * 1024 * 1024;

/// Default data shards per stripe. `(10, 4)` is the v0.3 plan's
/// canonical configuration: 1.4× storage overhead, tolerates any
/// 4 chunk losses per stripe.
pub const DEFAULT_RS_K: u8 = 10;

/// Default parity shards per stripe. See [`DEFAULT_RS_K`].
pub const DEFAULT_RS_M: u8 = 4;

/// Hard ceiling on `k + m`. The `Encoding::ReedSolomon { k, m }`
/// wire field is two `u8`s so a sum > 255 cannot encode validly;
/// the validator rejects at the producer surface so the failure
/// surfaces synchronously, not as a wire-decode error on the
/// receiver.
pub const RS_MAX_KM_SUM: u16 = 255;

/// Soft threshold on `k + m` above which a warning is emitted at
/// validation time. Most RS implementations are tuned for sums
/// below this; reconstruction performance degrades non-linearly
/// past the threshold. Configurations like `(20, 4)` (sum 24) or
/// `(10, 4)` (sum 14) stay well clear.
pub const RS_WARN_KM_SUM: u16 = 64;

// ───────────────────────────────────────────────────────────────────────────
// Capability tag + downgrade probe
// ───────────────────────────────────────────────────────────────────────────

/// Capability tag a node advertises when it supports the v0.3
/// Phase C Reed-Solomon store path
/// ([`Encoding::ReedSolomon { k, m }`](super::blob_ref::Encoding::ReedSolomon)).
///
/// Independent of [`super::blob_tree::DATAFORTS_BLOB_TREE_SUPPORTED`]
/// and [`super::cdc::DATAFORTS_BLOB_CDC_SUPPORTED`]: a node can
/// support Tree + CDC without RS (Phase A + B without C). Producers
/// targeting a peer that does NOT advertise this tag must downgrade
/// the blob's encoding to [`Encoding::Replicated`] — the substrate
/// has no transparent fallback at fetch time because the receiver
/// must already hold a copy of the parity-computing code to
/// reconstruct missing chunks.
pub const DATAFORTS_BLOB_ERASURE_SUPPORTED: &str = "dataforts:blob-erasure-supported";

/// Producer-side hook for the RS downgrade decision.
///
/// Mirrors [`super::cdc::CdcSupportProbe`] and
/// [`super::blob_tree::TreeSupportProbe`] one-for-one. Default
/// [`ErasureSupportProbe::AlwaysSupported`] is correct for
/// single-cluster all-Phase-C deployments;
/// [`ErasureSupportProbe::ForceReplicated`] is correct for cross-
/// version rollouts; [`ErasureSupportProbe::Dynamic`] lets callers
/// wire a runtime capability-tag check.
///
/// Producers consult the probe BEFORE passing
/// [`Encoding::ReedSolomon`] to `store_stream_tree` — on `false`,
/// they substitute [`Encoding::Replicated`].
pub enum ErasureSupportProbe {
    /// All targets support RS. Default for single-cluster
    /// all-Phase-C deployments.
    AlwaysSupported,
    /// No target supports RS. Forces every publish to use
    /// Replicated encoding. Useful during cluster-wide rollouts
    /// before every node has been upgraded.
    ForceReplicated,
    /// Dynamic check — caller-supplied closure consults the
    /// capability-tag advertisement layer at decision time.
    /// Returns `true` iff the destination advertises
    /// [`DATAFORTS_BLOB_ERASURE_SUPPORTED`].
    Dynamic(Box<dyn Fn() -> bool + Send + Sync>),
}

impl ErasureSupportProbe {
    /// Evaluate the probe. Cheap for the static variants; invokes
    /// the closure for `Dynamic`.
    pub fn check(&self) -> bool {
        match self {
            ErasureSupportProbe::AlwaysSupported => true,
            ErasureSupportProbe::ForceReplicated => false,
            ErasureSupportProbe::Dynamic(f) => f(),
        }
    }
}

impl std::fmt::Debug for ErasureSupportProbe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ErasureSupportProbe::AlwaysSupported => {
                f.write_str("ErasureSupportProbe::AlwaysSupported")
            }
            ErasureSupportProbe::ForceReplicated => {
                f.write_str("ErasureSupportProbe::ForceReplicated")
            }
            ErasureSupportProbe::Dynamic(_) => f.write_str("ErasureSupportProbe::Dynamic(..)"),
        }
    }
}

impl Default for ErasureSupportProbe {
    fn default() -> Self {
        Self::AlwaysSupported
    }
}

/// Producer-side downgrade helper: if `encoding` is
/// [`Encoding::ReedSolomon`] and `probe.check()` returns `false`,
/// substitute [`Encoding::Replicated`]. Passes other encodings
/// through unchanged.
///
/// Composes with the [`super::cdc::cdc_downgrade`] helper —
/// callers consult Tree, CDC, and erasure probes independently
/// before invoking `store_stream_tree`.
pub fn erasure_downgrade(encoding: Encoding, probe: &ErasureSupportProbe) -> Encoding {
    match encoding {
        Encoding::ReedSolomon { .. } if !probe.check() => Encoding::Replicated,
        other => other,
    }
}

// ───────────────────────────────────────────────────────────────────────────
// RsParams + RsEncoder
// ───────────────────────────────────────────────────────────────────────────

/// `(k, m)` parameter pair for a Reed-Solomon stripe configuration.
/// Mirrors the [`Encoding::ReedSolomon { k, m }`](super::blob_ref::Encoding::ReedSolomon)
/// fields; carried separately so the encoder doesn't have to
/// re-pattern-match the enum on every stripe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RsParams {
    /// Number of data shards per stripe. Must be `>= 1`.
    pub k: u8,
    /// Number of parity shards per stripe. Must be `>= 1`. The
    /// stripe survives any `m` chunk losses (data OR parity).
    pub m: u8,
}

impl RsParams {
    /// Convenience constructor. Validation is deferred to
    /// [`Self::validate`] — construction is infallible so test
    /// fixtures can build deliberately-malformed params.
    pub const fn new(k: u8, m: u8) -> Self {
        Self { k, m }
    }

    /// v0.3 Phase C default: `(10, 4)` — 1.4× storage, 4-loss
    /// tolerance. Matches [`DEFAULT_RS_K`] / [`DEFAULT_RS_M`].
    pub const fn default_production() -> Self {
        Self {
            k: DEFAULT_RS_K,
            m: DEFAULT_RS_M,
        }
    }

    /// Reject the malformed cases:
    /// - `k == 0` — a stripe with no data shards is meaningless.
    /// - `m == 0` — no parity means nothing to reconstruct from.
    /// - `k as u16 + m as u16 > 255` — wire fields are `u8` each;
    ///   the substrate uses `k + m` to size the underlying matrix.
    ///
    /// Caller is expected to honour the [`RS_WARN_KM_SUM`] soft
    /// threshold via its own logging; the validator stays quiet
    /// about it so test fixtures can use whatever shape they need.
    pub fn validate(&self) -> Result<(), BlobError> {
        if self.k == 0 {
            return Err(BlobError::Backend(
                "RS params: k must be >= 1; zero-data stripe is invalid".to_owned(),
            ));
        }
        if self.m == 0 {
            return Err(BlobError::Backend(
                "RS params: m must be >= 1; zero-parity stripe cannot reconstruct losses"
                    .to_owned(),
            ));
        }
        if self.k as u16 + self.m as u16 > RS_MAX_KM_SUM {
            return Err(BlobError::Backend(format!(
                "RS params: k + m = {} exceeds the wire-format maximum {}",
                self.k as u16 + self.m as u16,
                RS_MAX_KM_SUM
            )));
        }
        Ok(())
    }

    /// Pull the params out of an [`Encoding::ReedSolomon { k, m }`]
    /// enum variant. Returns `None` for [`Encoding::Replicated`].
    pub fn from_encoding(encoding: Encoding) -> Option<Self> {
        match encoding {
            Encoding::ReedSolomon { k, m } => Some(Self { k, m }),
            Encoding::Replicated => None,
        }
    }
}

impl Default for RsParams {
    fn default() -> Self {
        Self::default_production()
    }
}

/// Reed-Solomon encoder/decoder wrapper over `GF(2^8)`. Construct
/// once per [`RsParams`] configuration and reuse across many
/// stripes — the underlying matrix construction is the expensive
/// part, the per-stripe `encode` / `reconstruct_data` calls are
/// data-throughput-bound.
pub struct RsEncoder {
    rs: ReedSolomon<galois_8::Field>,
    params: RsParams,
}

impl RsEncoder {
    /// Construct an encoder for the supplied parameters. Returns
    /// `BlobError::Backend` if `params.validate()` fails or if the
    /// underlying RS-library constructor rejects the shape
    /// (currently identical to our validator but kept as a separate
    /// error to surface library-side changes if any).
    pub fn new(params: RsParams) -> Result<Self, BlobError> {
        params.validate()?;
        let rs = ReedSolomon::<galois_8::Field>::new(params.k as usize, params.m as usize)
            .map_err(|e| {
                BlobError::Backend(format!(
                    "RS encoder construction failed for (k={}, m={}): {:?}",
                    params.k, params.m, e
                ))
            })?;
        Ok(Self { rs, params })
    }

    /// `(k, m)` the encoder was constructed with.
    pub fn params(&self) -> RsParams {
        self.params
    }

    /// Compute `m` parity shards from `k` equal-length data shards.
    ///
    /// All data shards MUST be the same length; the caller (the
    /// striper that lands in C2) is responsible for zero-padding
    /// short chunks. Returns a `Vec<Vec<u8>>` of length `m`, each
    /// inner `Vec` sized to the data-shard length. Errors:
    ///
    /// - `data.len() != self.params.k` → `BlobError::Backend`.
    /// - Inner-vec lengths differ → `BlobError::Backend`.
    /// - Inner-vec length is zero → `BlobError::Backend`.
    pub fn encode(&self, data: &[Vec<u8>]) -> Result<Vec<Vec<u8>>, BlobError> {
        if data.len() != self.params.k as usize {
            return Err(BlobError::Backend(format!(
                "RS encode: expected {} data shards, got {}",
                self.params.k,
                data.len()
            )));
        }
        let shard_len = match data.first() {
            Some(first) => first.len(),
            None => 0,
        };
        if shard_len == 0 {
            return Err(BlobError::Backend(
                "RS encode: data shards must be non-empty".to_owned(),
            ));
        }
        if data.iter().any(|d| d.len() != shard_len) {
            return Err(BlobError::Backend(
                "RS encode: all data shards must be the same length (caller is responsible \
                 for zero-padding short chunks)"
                    .to_owned(),
            ));
        }
        let mut parity: Vec<Vec<u8>> = (0..self.params.m).map(|_| vec![0u8; shard_len]).collect();
        // The crate's `encode_sep` takes `data: &[T]` and writes
        // into `parity: &mut [U]`; both must be slice-of-slices.
        let data_refs: Vec<&[u8]> = data.iter().map(|d| d.as_slice()).collect();
        let mut parity_refs: Vec<&mut [u8]> = parity.iter_mut().map(|p| p.as_mut_slice()).collect();
        self.rs
            .encode_sep(&data_refs, &mut parity_refs)
            .map_err(|e| BlobError::Backend(format!("RS encode_sep failed: {:?}", e)))?;
        Ok(parity)
    }

    /// Reconstruct any missing data shards from a partial set of
    /// data + parity. `shards` is indexed `[0..k)` for data and
    /// `[k..k+m)` for parity; each slot is `Some(bytes)` if present
    /// and `None` if missing. On success, every previously-`None`
    /// data slot is replaced with the reconstructed bytes; parity
    /// slots may remain `None` (we only need `reconstruct_data`,
    /// not full reconstruction). Errors:
    ///
    /// - `shards.len() != k + m` → `BlobError::Backend`.
    /// - Fewer than `k` total survivors (data + parity present)
    ///   → underlying RS library returns
    ///   [`reed_solomon_erasure::Error::TooFewShardsPresent`],
    ///   mapped to `BlobError::Backend`.
    /// - All present shards must have the same length.
    pub fn reconstruct_data(
        &self,
        shards: &mut [Option<Vec<u8>>],
    ) -> Result<(), BlobError> {
        let expected = self.params.k as usize + self.params.m as usize;
        if shards.len() != expected {
            return Err(BlobError::Backend(format!(
                "RS reconstruct_data: expected {} shard slots (k={} + m={}), got {}",
                expected, self.params.k, self.params.m, shards.len()
            )));
        }
        self.rs
            .reconstruct_data(shards)
            .map_err(|e| BlobError::Backend(format!("RS reconstruct_data failed: {:?}", e)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trip: encode k data → m parity, drop m of the
    /// combined (k + m) shards, reconstruct, assert byte-equality.
    #[test]
    fn encode_then_drop_m_shards_then_reconstruct_round_trips() {
        let params = RsParams { k: 4, m: 2 };
        let encoder = RsEncoder::new(params).unwrap();
        // 4 data shards, 1024 bytes each — distinct content per
        // shard so a reconstruction error would be loud.
        let data: Vec<Vec<u8>> = (0..4u8)
            .map(|i| (0..1024).map(|j| i.wrapping_add(j as u8)).collect())
            .collect();
        let parity = encoder.encode(&data).unwrap();
        assert_eq!(parity.len(), 2);
        assert_eq!(parity[0].len(), 1024);

        // Build the shard set, then drop 2 (one data, one parity)
        // and reconstruct. With k=4, m=2, dropping 2 of 6 is the
        // hard tolerance — recovery should succeed.
        let mut shards: Vec<Option<Vec<u8>>> = data
            .iter()
            .cloned()
            .chain(parity.iter().cloned())
            .map(Some)
            .collect();
        shards[1] = None; // drop data shard 1
        shards[5] = None; // drop parity shard 1

        encoder.reconstruct_data(&mut shards).unwrap();
        assert_eq!(
            shards[1].as_ref().unwrap(),
            &data[1],
            "reconstructed data shard 1 must equal the original"
        );
        // Data shards 0, 2, 3 untouched (already present).
        assert_eq!(shards[0].as_ref().unwrap(), &data[0]);
        assert_eq!(shards[2].as_ref().unwrap(), &data[2]);
        assert_eq!(shards[3].as_ref().unwrap(), &data[3]);
    }

    /// Dropping `m + 1` shards must fail reconstruction (the RS
    /// tolerance is exactly `m` losses per stripe).
    #[test]
    fn dropping_more_than_m_shards_fails_reconstruction() {
        let params = RsParams { k: 4, m: 2 };
        let encoder = RsEncoder::new(params).unwrap();
        let data: Vec<Vec<u8>> =
            (0..4u8).map(|i| vec![i; 512]).collect();
        let parity = encoder.encode(&data).unwrap();
        let mut shards: Vec<Option<Vec<u8>>> = data
            .iter()
            .cloned()
            .chain(parity.iter().cloned())
            .map(Some)
            .collect();
        // Drop 3 of the 6 — exceeds m=2 tolerance.
        shards[0] = None;
        shards[1] = None;
        shards[2] = None;
        let err = encoder.reconstruct_data(&mut shards).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("reconstruct_data") || msg.contains("TooFew"),
            "expected an RS-library failure, got: {}",
            msg
        );
    }

    /// All m parity shards lost, all k data shards present: the
    /// "no reconstruction needed" path. `reconstruct_data`
    /// succeeds without touching the data and leaves parity
    /// slots `None` (the call only restores data).
    #[test]
    fn parity_loss_with_full_data_set_succeeds_without_touching_data() {
        let params = RsParams { k: 4, m: 2 };
        let encoder = RsEncoder::new(params).unwrap();
        let data: Vec<Vec<u8>> = (0..4u8).map(|i| vec![i.wrapping_mul(7); 256]).collect();
        let parity = encoder.encode(&data).unwrap();
        let mut shards: Vec<Option<Vec<u8>>> = data
            .iter()
            .cloned()
            .chain(parity.iter().cloned())
            .map(Some)
            .collect();
        // Drop both parity shards.
        shards[4] = None;
        shards[5] = None;
        encoder.reconstruct_data(&mut shards).unwrap();
        for i in 0..4 {
            assert_eq!(shards[i].as_ref().unwrap(), &data[i]);
        }
    }

    /// `validate` rejects the malformed cases.
    #[test]
    fn validate_rejects_malformed_params() {
        assert!(RsParams { k: 0, m: 4 }.validate().is_err());
        assert!(RsParams { k: 10, m: 0 }.validate().is_err());
        // k + m > 255 is the hard ceiling. u8 + u8 max sum is 510,
        // so 200 + 200 = 400 is rejected.
        assert!(RsParams { k: 200, m: 200 }.validate().is_err());
        // Production default is valid.
        assert!(RsParams::default_production().validate().is_ok());
    }

    /// `from_encoding` extracts the params from the wire variant.
    #[test]
    fn from_encoding_extracts_params() {
        assert_eq!(
            RsParams::from_encoding(Encoding::ReedSolomon { k: 6, m: 3 }),
            Some(RsParams { k: 6, m: 3 })
        );
        assert_eq!(RsParams::from_encoding(Encoding::Replicated), None);
    }

    /// `encode` rejects mismatched shard lengths — the caller (the
    /// future striper) must pad to equal length before calling.
    #[test]
    fn encode_rejects_uneven_data_shard_lengths() {
        let encoder = RsEncoder::new(RsParams { k: 3, m: 2 }).unwrap();
        let data = vec![vec![0u8; 100], vec![1u8; 50], vec![2u8; 100]];
        assert!(encoder.encode(&data).is_err());
    }

    /// `encode` rejects the wrong number of data shards.
    #[test]
    fn encode_rejects_wrong_data_shard_count() {
        let encoder = RsEncoder::new(RsParams { k: 4, m: 2 }).unwrap();
        let data = vec![vec![0u8; 100], vec![1u8; 100]];
        assert!(encoder.encode(&data).is_err());
    }

    /// `ErasureSupportProbe` static variants resolve as expected.
    #[test]
    fn erasure_support_probe_static_variants() {
        assert!(ErasureSupportProbe::AlwaysSupported.check());
        assert!(!ErasureSupportProbe::ForceReplicated.check());
        assert!(ErasureSupportProbe::default().check());
    }

    /// `ErasureSupportProbe::Dynamic` consults the closure on each
    /// `check()`.
    #[test]
    fn erasure_support_probe_dynamic_consults_closure() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        let flag = Arc::new(AtomicBool::new(false));
        let f = flag.clone();
        let probe = ErasureSupportProbe::Dynamic(Box::new(move || f.load(Ordering::Relaxed)));
        assert!(!probe.check());
        flag.store(true, Ordering::Relaxed);
        assert!(probe.check());
    }

    /// `erasure_downgrade` substitutes Replicated when the probe
    /// rejects RS, passes RS through when accepted, and leaves
    /// Replicated untouched in both probe arms.
    #[test]
    fn erasure_downgrade_substitutes_only_for_rs_on_reject() {
        let rs = Encoding::ReedSolomon { k: 10, m: 4 };
        let rep = Encoding::Replicated;
        assert_eq!(
            erasure_downgrade(rs, &ErasureSupportProbe::AlwaysSupported),
            rs
        );
        assert_eq!(
            erasure_downgrade(rep, &ErasureSupportProbe::AlwaysSupported),
            rep
        );
        assert_eq!(
            erasure_downgrade(rs, &ErasureSupportProbe::ForceReplicated),
            Encoding::Replicated
        );
        assert_eq!(
            erasure_downgrade(rep, &ErasureSupportProbe::ForceReplicated),
            rep
        );
    }

    /// Production constants match the v0.3 plan §6: `(10, 4)`
    /// default, 40 MiB stripe target, 8 MiB stripe minimum.
    #[test]
    fn striper_constants_match_plan_defaults() {
        assert_eq!(DEFAULT_RS_K, 10);
        assert_eq!(DEFAULT_RS_M, 4);
        assert_eq!(RS_STRIPE_TARGET_BYTES, 40 * 1024 * 1024);
        assert_eq!(RS_STRIPE_MIN_BYTES, 8 * 1024 * 1024);
        assert_eq!(
            RsParams::default_production(),
            RsParams { k: 10, m: 4 }
        );
    }
}
