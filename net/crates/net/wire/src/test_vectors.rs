//! Golden vectors this crate owns, as `&'static str` constants.
//!
//! Package-safety, not convenience. The wasm test used to
//! `include_str!("../../tests/cross_lang_wire/aead_vector.json")` —
//! a path outside the package, so an unpacked `cargo package`
//! tarball of `net-mesh-wire` could not compile its own test target.
//! The JSON now lives under `src/`, which `cargo package` always
//! carries, and both consumers read the same constant: the wasm test
//! here and the core's `tests/cross_lang_wire.rs`, which additionally
//! asserts the repository copy under
//! `net/crates/net/tests/cross_lang_wire/` is byte-identical to it.
//!
//! Behind the `test-vectors` feature: production builds of a browser
//! leaf should not carry test data.

/// The cross-backend packet-AEAD golden vector (RFC 8439
/// ChaCha20-Poly1305, 12-byte nonce, 16-byte tag).
///
/// The `ring` backend seals it natively and the `chacha20poly1305`
/// backend seals it on wasm32; both must produce the pinned
/// ciphertext, or the AEAD seam has split the wire format between
/// native and browser nodes.
pub const AEAD_VECTOR: &str = include_str!("test_vectors/aead_vector.json");
