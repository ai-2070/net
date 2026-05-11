//! `BlobRef` — typed event-payload that points at content stored
//! out-of-band in a [`super::BlobAdapter`] backend.
//!
//! Wire encoding (`DATAFORTS_PLAN.md` § Phase 3 locked decisions):
//!
//! | Byte | Field |
//! |---|---|
//! | `0` | discriminator (`0xB0`) |
//! | `1` | version (`0x01` for v1) |
//! | `2..34` | BLAKE3 hash (32 bytes) |
//! | `34..42` | size (`u64` little-endian) |
//! | `42..` | URI bytes (UTF-8, length = remaining frame length) |
//!
//! No length prefix on the URI — the encoded form lives inside an
//! event payload whose length is already framed by the substrate.
//! Inline event payloads carry no discriminator (back-compat); the
//! substrate distinguishes by peeking at byte 0.

use super::error::BlobError;

/// Discriminator byte at offset 0 of an encoded [`BlobRef`].
/// Distinguishes blob-ref payloads from inline event payloads on
/// every `read_range` / `tail` output. Picked to be statistically
/// rare in user payloads; collisions are not a correctness issue
/// because the version + hash + size + URI all have to decode
/// successfully for a `BlobRef` to materialise.
pub const BLOB_REF_DISCRIMINATOR: u8 = 0xB0;

/// `BlobRef` wire-encoding version. v1 is the only version this
/// build encodes; the version byte is reserved so future migrations
/// (e.g. BLAKE3-256 → BLAKE3-512, or a multi-hash format) can land
/// without breaking the decoder.
pub const BLOB_REF_VERSION_V1: u8 = 0x01;

/// Minimum encoded length: discriminator + version + hash + size.
/// URI may be empty.
pub const BLOB_REF_HEADER_LEN: usize = 1 + 1 + 32 + 8;

/// Pointer to content stored out-of-band. Round-trips through
/// every binding as a typed value via the public fields; the
/// substrate uses [`Self::encode`] / [`Self::decode`] for the wire
/// form.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BlobRef {
    /// Encoding version byte. Always [`BLOB_REF_VERSION_V1`] on
    /// fresh constructions; decode preserves the on-wire value so
    /// upstream code can detect forward-compat scenarios.
    pub version: u8,
    /// Adapter-routed URI — e.g. `s3://bucket/key`, `ipfs://<cid>`,
    /// `file:///abs/path`. The scheme picks the adapter; the rest
    /// is passed through opaque.
    pub uri: String,
    /// BLAKE3-256 hash of the canonical bytes the URI resolves to.
    /// The substrate verifies this on every successful fetch; an
    /// adversarial adapter cannot fake-verify because the check
    /// runs in the substrate, not the adapter.
    pub hash: [u8; 32],
    /// Size of the resolved content in bytes. Range-fetch callers
    /// use this to bound their reads; the verification path uses
    /// it to short-circuit obviously-wrong payloads.
    pub size: u64,
}

impl BlobRef {
    /// Construct a v1 `BlobRef`. The caller is responsible for the
    /// `hash` matching the content at `uri` — the substrate
    /// verifies on fetch, not on construction.
    pub fn new(uri: impl Into<String>, hash: [u8; 32], size: u64) -> Self {
        Self {
            version: BLOB_REF_VERSION_V1,
            uri: uri.into(),
            hash,
            size,
        }
    }

    /// Encoded length: header (42 bytes) + URI byte length.
    pub fn encoded_len(&self) -> usize {
        BLOB_REF_HEADER_LEN + self.uri.len()
    }

    /// Emit the wire form. See the module-level table for the
    /// byte layout.
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(self.encoded_len());
        buf.push(BLOB_REF_DISCRIMINATOR);
        buf.push(self.version);
        buf.extend_from_slice(&self.hash);
        buf.extend_from_slice(&self.size.to_le_bytes());
        buf.extend_from_slice(self.uri.as_bytes());
        buf
    }

    /// Decode a wire form. Returns `Ok(None)` when `bytes[0]` is
    /// not the discriminator (caller should treat the payload as
    /// inline). Returns `Err` only when the discriminator matches
    /// but the rest of the frame is malformed.
    pub fn decode(bytes: &[u8]) -> Result<Option<Self>, BlobError> {
        if bytes.first().copied() != Some(BLOB_REF_DISCRIMINATOR) {
            return Ok(None);
        }
        if bytes.len() < BLOB_REF_HEADER_LEN {
            return Err(BlobError::Decode(format!(
                "frame too short: {} bytes, need at least {}",
                bytes.len(),
                BLOB_REF_HEADER_LEN
            )));
        }
        let version = bytes[1];
        if version != BLOB_REF_VERSION_V1 {
            return Err(BlobError::UnsupportedVersion(version));
        }
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&bytes[2..34]);
        let mut size_bytes = [0u8; 8];
        size_bytes.copy_from_slice(&bytes[34..42]);
        let size = u64::from_le_bytes(size_bytes);
        let uri_bytes = &bytes[42..];
        let uri = std::str::from_utf8(uri_bytes)
            .map_err(|e| BlobError::Decode(format!("URI not UTF-8: {}", e)))?
            .to_owned();
        Ok(Some(Self {
            version,
            uri,
            hash,
            size,
        }))
    }

    /// Verify `bytes` resolves to this `BlobRef`'s `hash`. Returns
    /// `Ok(())` on match, `Err(BlobError::HashMismatch)` otherwise.
    /// Runs inside the substrate, not the adapter, so an
    /// adversarial adapter cannot fake-verify.
    pub fn verify(&self, bytes: &[u8]) -> Result<(), BlobError> {
        let actual: [u8; 32] = blake3::hash(bytes).into();
        if actual == self.hash {
            Ok(())
        } else {
            Err(BlobError::HashMismatch {
                expected: self.hash,
                actual,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> BlobRef {
        BlobRef::new("s3://bucket/key", [0xAB; 32], 12345)
    }

    #[test]
    fn round_trip_encode_decode() {
        let original = fixture();
        let bytes = original.encode();
        let decoded = BlobRef::decode(&bytes).unwrap().unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn decode_returns_none_when_discriminator_missing() {
        // First byte is not the discriminator → inline payload.
        let bytes = vec![0x00, 0x01, 0x02];
        assert!(BlobRef::decode(&bytes).unwrap().is_none());
    }

    #[test]
    fn decode_rejects_short_frame() {
        let bytes = vec![BLOB_REF_DISCRIMINATOR, BLOB_REF_VERSION_V1, 0x00];
        let err = BlobRef::decode(&bytes).unwrap_err();
        assert!(matches!(err, BlobError::Decode(_)));
    }

    #[test]
    fn decode_rejects_unknown_version() {
        let blob = fixture();
        let mut bytes = blob.encode();
        bytes[1] = 0xFE;
        let err = BlobRef::decode(&bytes).unwrap_err();
        assert!(matches!(err, BlobError::UnsupportedVersion(0xFE)));
    }

    #[test]
    fn encoded_len_matches_real_encoding() {
        let blob = fixture();
        assert_eq!(blob.encoded_len(), blob.encode().len());
    }

    #[test]
    fn verify_accepts_matching_bytes() {
        let payload = b"the lazy dog";
        let hash: [u8; 32] = blake3::hash(payload).into();
        let blob = BlobRef::new("file:///x", hash, payload.len() as u64);
        blob.verify(payload).unwrap();
    }

    #[test]
    fn verify_rejects_mismatching_bytes() {
        let blob = BlobRef::new("file:///x", [0xCC; 32], 0);
        let err = blob.verify(b"different content").unwrap_err();
        match err {
            BlobError::HashMismatch { expected, actual } => {
                assert_eq!(expected, [0xCC; 32]);
                assert_ne!(actual, expected);
            }
            other => panic!("expected HashMismatch, got {:?}", other),
        }
    }

    #[test]
    fn empty_uri_round_trips() {
        let blob = BlobRef::new("", [0x00; 32], 0);
        let bytes = blob.encode();
        let decoded = BlobRef::decode(&bytes).unwrap().unwrap();
        assert_eq!(decoded.uri, "");
        assert_eq!(decoded.size, 0);
    }
}
