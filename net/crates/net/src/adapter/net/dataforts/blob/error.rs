//! `BlobError` — typed failure surface for the blob layer.

use std::fmt;

/// Errors surfaced by [`super::BlobAdapter`] implementations and the
/// substrate's blob-fetch path. Variants stay byte-stable across
/// bindings because they appear in error-routing logic on the
/// JS / Python / Go sides.
///
/// `#[non_exhaustive]` so binding-side FFI sites that match
/// exhaustively get a compile-time nudge when new variants land,
/// rather than silently routing unknown errors to a default arm.
#[derive(Debug, PartialEq, Eq, Clone)]
#[non_exhaustive]
pub enum BlobError {
    /// Adapter returned bytes whose BLAKE3 hash did not match the
    /// expected hash carried in the [`super::BlobRef`]. The
    /// substrate enforces verification so an adversarial adapter
    /// cannot fake-verify. `expected` / `actual` are 32-byte
    /// BLAKE3 outputs.
    HashMismatch {
        /// Hash recorded on the `BlobRef`.
        expected: [u8; 32],
        /// Hash computed over the fetched bytes.
        actual: [u8; 32],
    },
    /// `BlobRef::uri` carries a scheme this adapter does not
    /// recognise (`s3://`, `ipfs://`, `file://`, etc.). The
    /// substrate routes per scheme; surface from the routing layer
    /// when no registered adapter claims the scheme.
    UnsupportedScheme(String),
    /// Object did not exist at the adapter's backend.
    NotFound(String),
    /// Adapter-side I/O / network / auth failure. The string is the
    /// adapter's best-effort message; downstream telemetry consumes
    /// the whole `BlobError` Display.
    Backend(String),
    /// Caller cancelled the fetch (e.g. context dropped, future
    /// aborted).
    Cancelled,
    /// `BlobRef` encoded with a version byte this build does not
    /// understand. Reserved for migration headroom; current encoder
    /// only emits [`super::blob_ref::BLOB_REF_VERSION_V1`].
    UnsupportedVersion(u8),
    /// `BlobRef` encoded form failed to decode (truncated /
    /// corrupted bytes, bad postcard frame, etc.).
    Decode(String),
    /// Channel's `RedexFileConfig` did not specify a
    /// `blob_adapter_id` — substrate can't route the BlobRef
    /// resolve. Operator misconfiguration (vs `AdapterNotRegistered`
    /// which is a deploy-ordering issue).
    AdapterNotConfigured,
    /// Channel's configured `blob_adapter_id` is not present in
    /// the registry — either an adapter that hasn't been
    /// registered yet (deploy-ordering race) or one that was
    /// unregistered. Distinct from `AdapterNotConfigured` so
    /// operators can tell apart "you forgot to set it" from
    /// "you didn't register the named adapter yet."
    AdapterNotRegistered(String),
    /// Caller failed an authorization check on the blob op:
    /// AuthGuard rejected the `(origin_hash, ChannelName)` ACL
    /// lookup, or no guard was configured for an op that
    /// requires one. Distinct from `Backend` so callers (and
    /// metrics) can tell apart a 401-style security boundary hit
    /// from a 500-style adapter failure. The string is the
    /// authorization-side context; do not leak channel names or
    /// principal identifiers if they're sensitive.
    Unauthorized(String),
    /// Backend returned a chunk whose length is shorter than the
    /// manifest's recorded chunk size — distinct from
    /// [`Self::HashMismatch`] so retry logic can tell a
    /// truncated tail (where the *content* may still hash
    /// correctly over its visible prefix) from a fundamental
    /// content disagreement. Pre-fix, an over-short chunk
    /// surfaced as `HashMismatch { expected, actual: blake3(short_bytes) }`,
    /// where `actual` could even equal `expected` for a
    /// truncated tail aligned to a block boundary, confusing
    /// retry / divergence-detection callers.
    ShortChunk {
        /// Hash recorded on the `BlobRef::Manifest` chunk entry.
        hash: [u8; 32],
        /// Bytes the request asked the chunk to span past
        /// (`req.start_in_chunk`).
        requested_start: u64,
        /// Bytes the request asked the chunk to span up to
        /// (`req.end_in_chunk`).
        requested_end: u64,
        /// Bytes the backend actually delivered for this chunk.
        actual_len: u64,
    },
}

impl fmt::Display for BlobError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HashMismatch { expected, actual } => write!(
                f,
                "blob hash mismatch (expected {}, got {})",
                hex32(expected),
                hex32(actual)
            ),
            Self::UnsupportedScheme(s) => write!(f, "blob scheme not supported: {}", s),
            Self::NotFound(uri) => write!(f, "blob not found: {}", uri),
            Self::Backend(msg) => write!(f, "blob backend error: {}", msg),
            Self::Cancelled => f.write_str("blob fetch cancelled"),
            Self::UnsupportedVersion(v) => write!(f, "blob ref version {} not supported", v),
            Self::Decode(msg) => write!(f, "blob ref decode failed: {}", msg),
            Self::AdapterNotConfigured => f.write_str(
                "blob adapter not configured: channel's RedexFileConfig has no blob_adapter_id",
            ),
            Self::AdapterNotRegistered(id) => {
                write!(f, "blob adapter \"{}\" not registered", id)
            }
            Self::Unauthorized(msg) => write!(f, "blob op unauthorized: {}", msg),
            Self::ShortChunk {
                hash,
                requested_start,
                requested_end,
                actual_len,
            } => write!(
                f,
                "blob chunk {} too short: requested bytes [{}, {}); backend returned {} bytes",
                hex32(hash),
                requested_start,
                requested_end,
                actual_len
            ),
        }
    }
}

impl std::error::Error for BlobError {}

// Delegate to the shared lookup-table-based `hex32` in `mod.rs`
// (see dataforts perf #171). The local definition used to be an
// independent `write!("{:02x}", b)` loop — same output, ~10× slower.
use super::hex32;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_includes_hash_hex_on_mismatch() {
        let err = BlobError::HashMismatch {
            expected: [0x11; 32],
            actual: [0x22; 32],
        };
        let s = err.to_string();
        assert!(s.contains(&"11".repeat(32)));
        assert!(s.contains(&"22".repeat(32)));
    }

    #[test]
    fn display_carries_uri_on_not_found() {
        let err = BlobError::NotFound("s3://bucket/key".into());
        assert!(err.to_string().contains("s3://bucket/key"));
    }
}
