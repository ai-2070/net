//! `BlobError` — typed failure surface for the blob layer.

use std::fmt;

/// Errors surfaced by [`super::BlobAdapter`] implementations and the
/// substrate's blob-fetch path. Variants stay byte-stable across
/// bindings because they appear in error-routing logic on the
/// JS / Python / Go sides.
#[derive(Debug, PartialEq, Eq, Clone)]
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
        }
    }
}

impl std::error::Error for BlobError {}

fn hex32(bytes: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(s, "{:02x}", b);
    }
    s
}

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
