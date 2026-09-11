//! Packet-AEAD backend seam (S0a finding).
//!
//! `crypto.rs` binds the packet cipher directly to `ring::aead`
//! (`crypto.rs:10`). `ring` 0.17 **cannot build for
//! `wasm32-unknown-unknown` without a `clang` that targets wasm32**:
//! its `build.rs` drives `cc`, and on a machine with no clang the
//! build stops with `error occurred in cc-rs: failed to find tool
//! "clang"`. That is a toolchain requirement for everyone who builds
//! the browser leaf, not a property of the Net code, so the spike
//! does what Stage 2 will have to do: put one thin seam in front of
//! ChaCha20-Poly1305 and pick the backend per target.
//!
//! - native: `ring::aead::LessSafeKey` — byte-identical to today.
//! - `wasm32`: the pure-Rust `chacha20poly1305` crate (RustCrypto).
//!
//! Both are RFC 8439 ChaCha20-Poly1305 with a 12-byte nonce and a
//! 16-byte tag, so the wire format is unchanged; the round-trip test
//! runs on the ring side and the `wasm_wire_probe` export runs on the
//! RustCrypto side.
//!
//! The API deliberately mirrors ring's, so `crypto.rs`'s call sites
//! keep their shape: nonce in, AAD in, in-place buffer, detached or
//! appended tag out.

/// Opaque AEAD failure. The caller maps it onto `CryptoError`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AeadError;

/// ChaCha20-Poly1305 tag length.
pub const TAG_LEN: usize = 16;

/// ChaCha20-Poly1305 nonce length.
pub const NONCE_LEN: usize = 12;

#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use super::{AeadError, NONCE_LEN, TAG_LEN};
    use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, CHACHA20_POLY1305};

    /// ring-backed packet key.
    pub struct AeadKey(LessSafeKey);

    impl AeadKey {
        /// Bind a 32-byte ChaCha20-Poly1305 key.
        #[expect(
            clippy::expect_used,
            reason = "UnboundKey::new fails only on key-length mismatch; the [u8; 32] parameter makes that unrepresentable for CHACHA20_POLY1305"
        )]
        pub fn new(key: &[u8; 32]) -> Self {
            Self(LessSafeKey::new(
                UnboundKey::new(&CHACHA20_POLY1305, key).expect("32-byte ChaCha20-Poly1305 key"),
            ))
        }

        /// Encrypt `buffer` in place; return the detached tag.
        #[inline]
        pub fn seal_detached(
            &self,
            nonce: [u8; NONCE_LEN],
            aad: &[u8],
            buffer: &mut [u8],
        ) -> Result<[u8; TAG_LEN], AeadError> {
            let tag = self
                .0
                .seal_in_place_separate_tag(
                    Nonce::assume_unique_for_key(nonce),
                    Aad::from(aad),
                    buffer,
                )
                .map_err(|_| AeadError)?;
            let mut out = [0u8; TAG_LEN];
            out.copy_from_slice(tag.as_ref());
            Ok(out)
        }

        /// Encrypt `buffer` in place and append the tag.
        #[inline]
        pub fn seal_append_tag(
            &self,
            nonce: [u8; NONCE_LEN],
            aad: &[u8],
            buffer: &mut Vec<u8>,
        ) -> Result<(), AeadError> {
            self.0
                .seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), Aad::from(aad), buffer)
                .map_err(|_| AeadError)
        }

        /// Decrypt `buffer` (ciphertext || tag) in place; return the
        /// plaintext length.
        #[inline]
        pub fn open_in_place(
            &self,
            nonce: [u8; NONCE_LEN],
            aad: &[u8],
            buffer: &mut [u8],
        ) -> Result<usize, AeadError> {
            self.0
                .open_in_place(Nonce::assume_unique_for_key(nonce), Aad::from(aad), buffer)
                .map(|plaintext| plaintext.len())
                .map_err(|_| AeadError)
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod imp {
    use super::{AeadError, NONCE_LEN, TAG_LEN};
    use chacha20poly1305::aead::{AeadInPlace, KeyInit};
    use chacha20poly1305::{ChaCha20Poly1305, Tag};

    /// RustCrypto-backed packet key (wasm32).
    pub struct AeadKey(ChaCha20Poly1305);

    impl AeadKey {
        /// Bind a 32-byte ChaCha20-Poly1305 key.
        pub fn new(key: &[u8; 32]) -> Self {
            Self(ChaCha20Poly1305::new(key.into()))
        }

        /// Encrypt `buffer` in place; return the detached tag.
        #[inline]
        pub fn seal_detached(
            &self,
            nonce: [u8; NONCE_LEN],
            aad: &[u8],
            buffer: &mut [u8],
        ) -> Result<[u8; TAG_LEN], AeadError> {
            let tag = self
                .0
                .encrypt_in_place_detached((&nonce).into(), aad, buffer)
                .map_err(|_| AeadError)?;
            let mut out = [0u8; TAG_LEN];
            out.copy_from_slice(tag.as_slice());
            Ok(out)
        }

        /// Encrypt `buffer` in place and append the tag.
        #[inline]
        pub fn seal_append_tag(
            &self,
            nonce: [u8; NONCE_LEN],
            aad: &[u8],
            buffer: &mut Vec<u8>,
        ) -> Result<(), AeadError> {
            let tag = self.seal_detached(nonce, aad, buffer.as_mut_slice())?;
            buffer.extend_from_slice(&tag);
            Ok(())
        }

        /// Decrypt `buffer` (ciphertext || tag) in place; return the
        /// plaintext length.
        #[inline]
        pub fn open_in_place(
            &self,
            nonce: [u8; NONCE_LEN],
            aad: &[u8],
            buffer: &mut [u8],
        ) -> Result<usize, AeadError> {
            if buffer.len() < TAG_LEN {
                return Err(AeadError);
            }
            let split = buffer.len() - TAG_LEN;
            let (ciphertext, tag) = buffer.split_at_mut(split);
            let tag = Tag::clone_from_slice(tag);
            self.0
                .decrypt_in_place_detached((&nonce).into(), aad, ciphertext, &tag)
                .map_err(|_| AeadError)?;
            Ok(split)
        }
    }
}

pub use imp::AeadKey;
