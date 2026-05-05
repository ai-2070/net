//! Re-export of the consumer-side Redis Streams dedup helper.
//!
//! The canonical implementation lives in the core crate
//! ([`net::adapter::RedisStreamDedup`]) so the C FFI layer in
//! `net/src/ffi/redis_dedup.rs` can use it without depending on
//! the SDK. This module re-exports it under
//! `net_sdk::RedisStreamDedup` for convenience — SDK callers can
//! import either path.

pub use net::adapter::RedisStreamDedup;
