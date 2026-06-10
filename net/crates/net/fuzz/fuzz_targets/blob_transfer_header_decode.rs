//! Fuzz target: `dataforts::blob::transfer::TransferHeader` decode.
//!
//! First data-plane event of a blob-transfer stream (holder →
//! requester). The requester's reassembler postcard-decodes this
//! header to learn the declared `total_len` before reading chunk
//! events. The bytes come from the serving peer, so a panic in the
//! header decode — or honoring an over-cap `total_len` — is a
//! remote-DoS / OOM vector on every blob fetch.
//!
//! Invariants asserted:
//!
//! - No panic / no unbounded allocation decoding any byte sequence
//!   as a `TransferHeader`.
//! - If decode succeeds, re-encoding and decoding again yields an
//!   equal header (postcard canonicalization is stable).

#![no_main]

use libfuzzer_sys::fuzz_target;
use net::adapter::net::dataforts::blob::transfer::TransferHeader;

fuzz_target!(|data: &[u8]| {
    let Ok(header) = postcard::from_bytes::<TransferHeader>(data) else {
        return;
    };

    let bytes = postcard::to_allocvec(&header).expect("encode of a decoded header must succeed");
    let round = postcard::from_bytes::<TransferHeader>(&bytes)
        .expect("re-decode of encoded header must succeed");
    assert_eq!(
        header, round,
        "TransferHeader canonicalization drift across postcard round-trip",
    );
});
