//! Fuzz target: `compute::bindings::DaemonBindings::from_bytes`.
//!
//! Subscription-ledger decoder used on the migration *target*
//! side: when a daemon migrates, the target reconstructs the
//! source's binding set from this buffer. The bytes originate from
//! the peer being migrated, so a panic or unbounded allocation on
//! a malformed ledger is a remote-DoS on the migration path.
//!
//! `from_bytes` declares a `count` up front; the decoder must
//! reject a count larger than the remaining buffer can supply
//! rather than `with_capacity`-allocating on the attacker's word.
//!
//! Invariants asserted:
//!
//! - No panic / no unbounded allocation on any byte sequence.
//! - If `from_bytes` returns `Some(b)`, `b.to_bytes()` round-trips
//!   back to an equal `b`.

#![no_main]

use libfuzzer_sys::fuzz_target;
use net::adapter::net::compute::bindings::DaemonBindings;

fuzz_target!(|data: &[u8]| {
    let Some(bindings) = DaemonBindings::from_bytes(data) else {
        return;
    };

    let bytes = bindings.to_bytes();
    let Some(round) = DaemonBindings::from_bytes(&bytes) else {
        panic!(
            "DaemonBindings round-trip failed: original {} bytes, \
             serialized {} bytes",
            data.len(),
            bytes.len()
        );
    };
    assert_eq!(
        bindings, round,
        "DaemonBindings canonicalization drift: from_bytes(to_bytes(x)) != x",
    );
});
