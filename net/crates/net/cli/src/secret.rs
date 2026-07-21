//! RAII scrub wrappers for in-process secret material (org root seeds,
//! audience discovery keys, node identity seeds).
//!
//! These live at crate level rather than inside `commands::org` because the
//! secret they protect crosses module boundaries: `net org keygen` serializes
//! the org root seed in `commands::org` and hands it to
//! `commands::identity::write_identity_atomically` to persist. A scrub
//! ceremony that stops at the module edge is not a scrub ceremony — §10 was
//! exactly that, a `ScrubbedString` in `org.rs` whose contents were copied
//! into a plain `Vec<u8>` by the writer it delegated to.
//!
//! Both wrappers zero on `Drop`, so EVERY exit path scrubs — early return,
//! `?`, or unwind — not only a success tail reached after all the fallible
//! steps.

/// Volatile, non-elidable zeroing of a byte buffer.
///
/// `write_volatile` per byte rather than a plain loop or `fill(0)`: the
/// compiler is free to delete a write it can prove is never read, which is
/// precisely the situation for a buffer about to be dropped.
pub(crate) fn zeroize_slice(bytes: &mut [u8]) {
    for byte in bytes.iter_mut() {
        // SAFETY: `byte` is a valid, aligned, uniquely-borrowed `u8` for the
        // duration of this write.
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
    // Keep the writes from being reordered past the end of the scrub.
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

/// Volatile-scrub a string's backing bytes in place.
///
/// Takes `&mut str` rather than `&mut String` so a caller can scrub any string
/// slice; a `&mut String` coerces automatically.
pub(crate) fn zeroize_string(s: &mut str) {
    // SAFETY: NUL is valid UTF-8, so zeroing preserves the `str` invariant,
    // and the buffer is not read as text again after this point.
    let bytes = unsafe { s.as_bytes_mut() };
    zeroize_slice(bytes);
}

/// RAII volatile-scrub for a byte buffer holding secret material: zeroes on
/// drop so EVERY exit path scrubs — not only a success tail reached after all
/// fallible operations (Kyra OA2-F). Non-secret payloads may use it too; the
/// extra memset is harmless and keeps staging uniform.
pub(crate) struct ScrubbedBytes(Vec<u8>);

impl ScrubbedBytes {
    pub(crate) fn new(bytes: Vec<u8>) -> Self {
        ScrubbedBytes(bytes)
    }
    pub(crate) fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

impl Drop for ScrubbedBytes {
    fn drop(&mut self) {
        zeroize_slice(&mut self.0);
    }
}

/// RAII volatile-scrub for a `String` holding secret material (e.g. the
/// serialized org root seed): scrubs on EVERY exit, including error returns
/// from the atomic write or the permission-enforcement step, not only the
/// success tail (Kyra OA2-F).
pub(crate) struct ScrubbedString(String);

impl ScrubbedString {
    pub(crate) fn new(s: String) -> Self {
        ScrubbedString(s)
    }
    pub(crate) fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

impl Drop for ScrubbedString {
    fn drop(&mut self) {
        zeroize_string(&mut self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wrappers must not change what the caller reads — the scrub happens
    /// on drop, not before.
    #[test]
    fn wrappers_expose_their_payload_until_dropped() {
        let b = ScrubbedBytes::new(b"seedbytes".to_vec());
        assert_eq!(b.as_slice(), b"seedbytes");
        let s = ScrubbedString::new("seedtext".to_string());
        assert_eq!(s.as_bytes(), b"seedtext");
    }

    #[test]
    fn zeroize_clears_every_byte() {
        let mut buf = vec![0xAAu8; 64];
        zeroize_slice(&mut buf);
        assert!(buf.iter().all(|b| *b == 0), "every byte must be zeroed");

        let mut text = "sensitive".to_string();
        zeroize_string(&mut text);
        assert!(
            text.as_bytes().iter().all(|b| *b == 0),
            "every byte of the string must be zeroed",
        );
    }
}
