//! Fuzz target: `cortex::rpc::RpcRequestPayload::decode`.
//!
//! nRPC request decoder — the payload after the 24-byte
//! `EventMeta` prefix of a `DISPATCH_RPC_REQUEST` event. A peer
//! issues these to invoke a capability-gated service; the inbound
//! dispatcher decodes the request before the handler runs, so a
//! panic or unbounded allocation here is a live remote-DoS on any
//! node serving an nRPC service.
//!
//! The decoder carries length-prefixed headers and a body, each
//! bounded by the `MAX_RPC_*` constants; over-cap inputs must
//! error rather than allocate. This target exercises that the caps
//! actually hold across the whole malformed-input space.
//!
//! Invariants asserted:
//!
//! - No panic / no unbounded allocation on any byte sequence.
//! - If `decode` returns `Ok(req)`, `req.encode()` round-trips
//!   back to an equal request.

#![no_main]

use bytes::Bytes;
use libfuzzer_sys::fuzz_target;
use net::adapter::net::cortex::rpc::RpcRequestPayload;

fuzz_target!(|data: &[u8]| {
    let Ok(req) = RpcRequestPayload::decode(Bytes::copy_from_slice(data)) else {
        return;
    };

    let bytes = req.encode();
    let round = RpcRequestPayload::decode(Bytes::from(bytes.clone())).unwrap_or_else(|e| {
        panic!(
            "RpcRequestPayload::decode failed on encode output: {:?} / {} bytes",
            e,
            bytes.len()
        )
    });
    assert_eq!(
        req, round,
        "nRPC request canonicalization drift: decode(encode(x)) != x",
    );
});
