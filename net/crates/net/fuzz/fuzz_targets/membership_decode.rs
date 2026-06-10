//! Fuzz target: `channel::membership::decode`.
//!
//! Channel membership subprotocol decoder — subscribe /
//! unsubscribe / ack frames. A peer can send arbitrary bytes for
//! this subprotocol; the membership handler decodes before the
//! frame is trusted, so a panic or unbounded allocation here is a
//! live remote-DoS.
//!
//! Invariants asserted:
//!
//! - No panic / no unbounded allocation on any byte sequence.
//! - If `decode` returns `Ok(msg)`, `encode(&msg)` round-trips
//!   back to an equal `msg` — guards against canonicalization
//!   drift that would silently corrupt membership state.

#![no_main]

use libfuzzer_sys::fuzz_target;
use net::adapter::net::channel::membership::{decode, encode};

fuzz_target!(|data: &[u8]| {
    let Ok(msg) = decode(data) else {
        return;
    };

    // Round-trip: encode is infallible; re-decode must succeed and
    // yield an equal message.
    let bytes = encode(&msg);
    let redecoded = decode(&bytes).unwrap_or_else(|e| {
        panic!(
            "membership::decode failed on encode output: {:?} / {} bytes",
            e,
            bytes.len()
        )
    });
    assert_eq!(
        msg, redecoded,
        "membership canonicalization drift: decode(encode(x)) != x",
    );
});
