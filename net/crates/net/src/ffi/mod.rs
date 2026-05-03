//! C FFI bindings for cross-language integration.
//!
//! This module provides a C-compatible API for using Net from
//! other languages (Python, Node.js, Go, etc.).
//!
//! # Safety
//!
//! All public FFI functions in this module accept raw pointers from C code.
//! While they are not marked `unsafe` (to maintain C ABI compatibility),
//! callers must ensure:
//! - Pointers are valid and properly aligned
//! - String pointers point to valid UTF-8 data
//! - Buffer sizes are accurate
//! - Handles are not used after `net_shutdown`
//!
//! # Thread Safety
//!
//! All FFI functions are thread-safe. The event bus handle can be shared
//! across threads.
//!
//! # Tokio runtime restriction
//!
//! Internal FFI ops (`net_poll`, `net_flush`, `net_shutdown`,
//! `net_redex_*`, `net_mesh_new`, the cortex FFI, the mesh FFI)
//! drive the bus's tokio runtime via `Runtime::block_on`. That
//! function panics with "Cannot start a runtime from within a
//! runtime" if the calling thread is already inside a tokio
//! runtime context. The functions are `extern "C"`, so a panic
//! unwinds across the FFI boundary into C / Go-cgo / Python /
//! NAPI — undefined behavior.
//!
//! **The common-case C / Go / Python caller has no Rust tokio
//! runtime, so this is unreachable for them.** The narrow path is:
//!
//! - A **Rust** caller loads the cdylib and calls these
//!   functions from inside its own `#[tokio::main]` (or any
//!   thread that has called `Runtime::enter()`).
//! - A non-Rust caller embeds a Rust library that runs its own
//!   tokio runtime and forwards calls into this cdylib on the
//!   same thread.
//!
//! Both forms are unusual but reachable. **Do not call any FFI
//! op from a thread that already holds a tokio runtime
//! context.** If you must, spawn the FFI call on a fresh OS
//! thread that doesn't carry a runtime guard, or wrap the call
//! with `tokio::task::spawn_blocking(|| net_xxx(...))` to escape
//! the worker pool.
//!
//! `net_init` (`mod.rs:284-316`) hardens against this for runtime
//! *construction*; the steady-state ops do not, since the cost
//! of a `Handle::try_current()` check on every poll would be
//! measurable for the common path that doesn't hit the bug.
//!
//! # Memory Management
//!
//! - Handles returned by `net_init` must be freed with `net_shutdown`
//! - String buffers passed to `net_poll` are owned by the caller
//! - Error codes are returned as integers (0 = success, negative = error)
//!
//! # Example (C)
//!
//! ```c
//! #include "net.h"
//!
//! int main() {
//!     // Initialize with default config
//!     void* bus = net_init("{\"num_shards\": 4}");
//!     if (!bus) return 1;
//!
//!     // Ingest an event
//!     int result = net_ingest(bus, "{\"token\": \"hello\"}", 19);
//!     if (result < 0) { /* handle error */ }
//!
//!     // Poll events
//!     char buffer[65536];
//!     result = net_poll(bus, "{\"limit\": 100}", buffer, sizeof(buffer));
//!
//!     // Shutdown
//!     net_shutdown(bus);
//!     return 0;
//! }
//! ```

// FFI functions accept raw pointers but are not marked `unsafe` to maintain
// C ABI compatibility. Safety is documented in the module-level docs.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::ffi::CStr;
use std::os::raw::{c_char, c_int};
use std::ptr;

use tokio::runtime::Runtime;

use crate::bus::EventBus;
use crate::config::EventBusConfig;
use crate::consumer::ConsumeRequest;
use crate::event::{Event, RawEvent};

/// C FFI for CortEX / NetDb / RedexFile. Requires `netdb` (for the
/// unified facade) and `redex-disk` (for persistent storage paths on
/// `Redex` / `RedexFile`). Go / cgo consumers target this surface.
///
/// `missing_docs` is suppressed on this module: these are extern "C"
/// shims over already-documented Rust adapters, and the per-function
/// contract is documented in the binding-side READMEs (Go / TS / Py).
/// Re-documenting each shim would duplicate with drift risk.
#[cfg(all(feature = "netdb", feature = "redex-disk"))]
#[allow(missing_docs)]
pub mod cortex;

/// C FFI for the encrypted-UDP mesh transport + channels. Requires
/// the `net` feature (which brings in the crypto + transport). Go /
/// cgo consumers target this surface alongside `ffi::cortex`. See
/// the `ffi::cortex` note for why `missing_docs` is suppressed here.
#[cfg(feature = "net")]
#[allow(missing_docs)]
pub mod mesh;

/// C FFI for the Redis Streams consumer-side dedup helper. Mirrors
/// the Rust `net::adapter::RedisStreamDedup` surface for Go / C / Zig
/// consumers. See `ffi::redis_dedup` module docs for the wire
/// shape and the dedup contract.
#[cfg(feature = "redis")]
pub mod redis_dedup;

#[cfg(feature = "net")]
use crate::adapter::net::{NetAdapterConfig, ReliabilityConfig, StaticKeypair};
#[cfg(any(feature = "redis", feature = "jetstream", feature = "net"))]
use crate::config::AdapterConfig;
#[cfg(feature = "jetstream")]
use crate::config::JetStreamAdapterConfig;
#[cfg(feature = "redis")]
use crate::config::RedisAdapterConfig;
#[cfg(feature = "net")]
use std::ffi::CString;

/// Opaque handle to an event bus instance.
///
/// This wraps the EventBus along with a Tokio runtime for async operations.
///
/// # Lifetime / soundness
///
/// The handle storage is *intentionally leaked* on `net_shutdown` rather
/// than freed via `Box::from_raw`. Reasoning: every FFI entry point
/// dereferences the C-side `*mut NetHandle` to access the atomics that
/// gate shutdown. The previous Dekker-style SeqCst handshake between
/// `FfiOpGuard::try_enter` (which calls `fetch_add` on `active_ops`) and
/// `net_shutdown` (which loads `active_ops` then `Box::from_raw`s the
/// handle) was unsound: SeqCst orders the atomic operations only — the
/// non-atomic `Box::from_raw` could deallocate the storage between
/// shutdown's load and a concurrent FFI op's `fetch_add`, producing a
/// use-after-free on the freed atomic. By never freeing the box, the
/// atomic memory backing the handle is always valid; concurrent FFI ops
/// observe `shutting_down=true` after shutdown signals it and bail
/// before touching `bus`/`runtime`.
///
/// `bus` and `runtime` are stored in `ManuallyDrop` so that
/// `net_shutdown` can `take` them out (via `ptr::read`) in order to
/// call `bus.shutdown().await`. Because `shutting_down` is set first
/// and shutdown waits for `active_ops` to drop to zero before reading
/// these fields, no FFI op can be racing the read. If the wait times
/// out, the `ptr::read` is skipped and both fields are leaked along
/// with the box.
pub struct NetHandle {
    /// Owned `EventBus`. Read out via `ManuallyDrop::take` during
    /// shutdown once `active_ops` has drained to zero. After that
    /// point, `shutting_down` is `true` and no FFI op may access this
    /// field.
    bus: std::mem::ManuallyDrop<EventBus>,
    /// Owned tokio runtime. Same lifetime contract as `bus`.
    runtime: std::mem::ManuallyDrop<Runtime>,
    /// Set to `true` once `net_shutdown` begins. All other FFI
    /// functions check this flag and return `ShuttingDown` before
    /// touching `bus` / `runtime`.
    shutting_down: std::sync::atomic::AtomicBool,
    /// Number of in-flight FFI operations (excluding shutdown itself).
    /// `net_shutdown` spins until this drops to zero (with a deadline)
    /// before reading `bus` / `runtime` to call shutdown.
    active_ops: std::sync::atomic::AtomicU32,
    /// Set to `true` after `net_shutdown` has consumed `bus` /
    /// `runtime` via `ManuallyDrop::take`. A second `net_shutdown`
    /// call observes this and returns `Success` without re-taking
    /// (which would be UB). FFI ops also check this before touching
    /// `bus` / `runtime`, defending against a contract-violating
    /// caller that races a post-shutdown call.
    bus_taken: std::sync::atomic::AtomicBool,
    /// Set to `true` after `bus.shutdown()` returns from the
    /// first `net_shutdown` call. A second/third concurrent
    /// `net_shutdown` caller spins until this flips before
    /// returning success — without this gate the second caller
    /// observed `bus_taken == true` and returned `Success` while
    /// the first caller was still mid-`block_on(bus.shutdown())`,
    /// falsely signaling completion of an in-progress shutdown.
    shutdown_completed: std::sync::atomic::AtomicBool,
}

/// Maximum time `net_shutdown` will wait for in-flight FFI operations
/// to complete before giving up. If the deadline expires, the bus is
/// leaked rather than read out — leaking is correct (the box is
/// already leaked permanently for soundness reasons) but means the
/// adapter's `flush()` / `shutdown()` won't run.
const FFI_SHUTDOWN_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

/// RAII guard that increments `active_ops` on creation and decrements on drop.
struct FfiOpGuard<'a> {
    handle: &'a NetHandle,
}

impl<'a> FfiOpGuard<'a> {
    /// Try to enter an FFI operation. Returns `None` if the handle is
    /// shutting down or if `bus` / `runtime` have already been taken.
    ///
    /// Soundness rests on the fact that the box backing `handle` is
    /// never freed (see `NetHandle` doc). The `fetch_add` is therefore
    /// always on valid memory regardless of whether shutdown is in
    /// progress. The subsequent loads decide whether the op is allowed
    /// to proceed; if shutdown was signaled or `bus_taken` flipped
    /// before our increment was visible, we bail without touching
    /// `bus` / `runtime`. The `bus_taken` check defends against a
    /// contract-violating caller that races a post-shutdown call: even
    /// if `shutting_down` was reset somehow, an op that would touch the
    /// already-taken `ManuallyDrop` fields is rejected.
    fn try_enter(handle: &'a NetHandle) -> Option<Self> {
        handle
            .active_ops
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if handle
            .shutting_down
            .load(std::sync::atomic::Ordering::SeqCst)
            || handle.bus_taken.load(std::sync::atomic::Ordering::SeqCst)
        {
            handle
                .active_ops
                .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
            None
        } else {
            Some(Self { handle })
        }
    }
}

impl Drop for FfiOpGuard<'_> {
    fn drop(&mut self) {
        self.handle
            .active_ops
            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

/// Returns `true` when `handle` is non-null and aligned for
/// `NetHandle`. Every `extern "C"` entry point that derefs the
/// raw handle must gate on this — a misaligned pointer produced
/// by an over-eager `void *` cast in a foreign caller would be
/// immediate UB on `&*handle`, even before the `is_null` check.
#[inline]
fn handle_is_valid(handle: *const NetHandle) -> bool {
    !handle.is_null() && (handle as usize).is_multiple_of(std::mem::align_of::<NetHandle>())
}

/// Error codes returned by FFI functions.
#[repr(C)]
pub enum NetError {
    /// Success (no error).
    Success = 0,
    /// Null pointer passed.
    NullPointer = -1,
    /// Invalid UTF-8 string.
    InvalidUtf8 = -2,
    /// Invalid JSON.
    InvalidJson = -3,
    /// Initialization failed.
    InitFailed = -4,
    /// Ingestion failed (backpressure).
    IngestionFailed = -5,
    /// Poll failed.
    PollFailed = -6,
    /// Buffer too small.
    BufferTooSmall = -7,
    /// Shutting down.
    ShuttingDown = -8,
    /// Integer overflow: result does not fit in `c_int`.
    IntOverflow = -9,
    /// Stream handle does not belong to the supplied node handle.
    /// Previously the send-family FFIs accepted any (stream, node)
    /// pair without verifying they were created from the same node,
    /// allowing silent cross-session traffic.
    MismatchedHandles = -10,
    /// `CString::new` failure: the input bytes are valid UTF-8 by
    /// Rust's `String` invariant but contain an interior NUL byte
    /// — and the C ABI cannot represent that, since C strings are
    /// NUL-terminated. Pre-fix this was reported as
    /// `InvalidUtf8`, which was wrong: the input is UTF-8-valid;
    /// it just has a NUL where C expects it not to. A binding
    /// reading the typed error and seeing "invalid UTF-8" would
    /// chase the wrong cause.
    InteriorNul = -11,
    /// Unknown error.
    Unknown = -99,
}

impl From<NetError> for c_int {
    fn from(e: NetError) -> Self {
        e as c_int
    }
}

/// Enter an FFI operation with lifetime protection. Returns an `FfiOpGuard`
/// that prevents `net_shutdown` from deallocating the handle until the guard
/// is dropped. Returns `Err` with the error code if shutdown is in progress.
#[inline]
fn enter_ffi_op(handle: &NetHandle) -> Result<FfiOpGuard<'_>, c_int> {
    FfiOpGuard::try_enter(handle).ok_or(NetError::ShuttingDown.into())
}

/// Initialize a new event bus.
///
/// # Parameters
///
/// - `config_json`: JSON configuration string (UTF-8, null-terminated).
///   Pass NULL or empty string for default configuration.
///
/// # Returns
///
/// Opaque handle to the event bus, or NULL on failure.
/// The handle must be freed with `net_shutdown`.
///
/// # Example Configuration
///
/// ```json
/// {
///   "num_shards": 8,
///   "ring_buffer_capacity": 1048576,
///   "backpressure_mode": "DropOldest",
///   "batch": {
///     "min_size": 1000,
///     "max_size": 10000,
///     "max_delay_ms": 10
///   }
/// }
/// ```
#[unsafe(no_mangle)]
pub extern "C" fn net_init(config_json: *const c_char) -> *mut NetHandle {
    // Parse and validate the config BEFORE constructing the tokio
    // runtime. Building the runtime first would let any subsequent
    // early-return path (`CStr::to_str` Err, `parse_config_json`
    // returning None, `EventBus::new` returning Err) drop the
    // local `Runtime` on function return. Dropping a multi-thread
    // tokio runtime from inside ANOTHER tokio runtime's worker
    // thread panics with "Cannot drop a runtime in a context where
    // blocking is not allowed", unwinding across this `extern "C"`
    // boundary into a Python / Go-cgo / NAPI / PyO3 caller —
    // undefined behaviour. By validating inputs first, the runtime
    // is only built once we know it will be installed into the
    // `NetHandle` and survive the call.
    let config = if config_json.is_null() {
        EventBusConfig::default()
    } else {
        let config_str = match unsafe { CStr::from_ptr(config_json) }.to_str() {
            Ok("") => EventBusConfig::default(),
            Ok(s) => match parse_config_json(s) {
                Some(cfg) => cfg,
                None => return ptr::null_mut(),
            },
            Err(_) => return ptr::null_mut(),
        };
        config_str
    };

    // Now construct the runtime — its lifetime is tied to the
    // returned `NetHandle` (via `create_with_config`), so the only
    // remaining drop is on `net_shutdown`, which already handles
    // it via `runtime.block_on(...)` (see #74) outside any other
    // tokio context.
    let runtime = match Runtime::new() {
        Ok(rt) => rt,
        Err(_) => return ptr::null_mut(),
    };

    create_with_config(runtime, config)
}

/// Parse JSON configuration into EventBusConfig.
///
/// Supports:
/// - `num_shards`: number of shards
/// - `ring_buffer_capacity`: ring buffer size per shard
/// - `backpressure_mode`: "DropNewest", "DropOldest", "FailProducer"
fn parse_config_json(json_str: &str) -> Option<EventBusConfig> {
    let value: serde_json::Value = serde_json::from_str(json_str).ok()?;

    let mut builder = EventBusConfig::builder();

    if let Some(num_shards) = value.get("num_shards").and_then(|v| v.as_u64()) {
        let num_shards = u16::try_from(num_shards).ok()?;
        builder = builder.num_shards(num_shards);
    }

    if let Some(capacity) = value.get("ring_buffer_capacity").and_then(|v| v.as_u64()) {
        let capacity = usize::try_from(capacity).ok()?;
        builder = builder.ring_buffer_capacity(capacity);
    }

    if let Some(bp_value) = value.get("backpressure_mode") {
        let bp_mode = if let Some(mode) = bp_value.as_str() {
            match mode {
                "DropNewest" | "drop_newest" => crate::config::BackpressureMode::DropNewest,
                "DropOldest" | "drop_oldest" => crate::config::BackpressureMode::DropOldest,
                "FailProducer" | "fail_producer" => crate::config::BackpressureMode::FailProducer,
                // Pre-fix every other string silently fell back to
                // `DropNewest`. A typo (`"DropOldset"`) thus
                // changed durability profile at deploy time with
                // no error. Reject unknowns to match the contract
                // already enforced by `parse_poll_request_json`.
                _ => return None,
            }
        } else if let Some(obj) = bp_value.as_object() {
            // Object form: `{"Sample": {"rate": N}}` for the
            // sampling mode that has an associated value.
            if let Some(sample) = obj.get("Sample").or_else(|| obj.get("sample")) {
                let rate = sample.get("rate").and_then(|v| v.as_u64())?;
                let rate = u32::try_from(rate).ok()?;
                if rate == 0 {
                    // Validated again by `EventBusConfig::validate`,
                    // but reject earlier so the parser surface
                    // matches the validator surface.
                    return None;
                }
                crate::config::BackpressureMode::Sample { rate }
            } else {
                return None;
            }
        } else {
            return None;
        };
        builder = builder.backpressure_mode(bp_mode);
    }

    // Parse Redis config
    #[cfg(feature = "redis")]
    if let Some(redis) = value.get("redis") {
        if let Some(url) = redis.get("url").and_then(|v| v.as_str()) {
            let mut redis_config = RedisAdapterConfig::new(url);

            if let Some(prefix) = redis.get("prefix").and_then(|v| v.as_str()) {
                redis_config = redis_config.with_prefix(prefix);
            }
            if let Some(max_len) = redis.get("max_stream_len").and_then(|v| v.as_u64()) {
                let max_len = usize::try_from(max_len).ok()?;
                redis_config = redis_config.with_max_stream_len(max_len);
            }
            if let Some(pipeline_size) = redis.get("pipeline_size").and_then(|v| v.as_u64()) {
                let pipeline_size = usize::try_from(pipeline_size).ok()?;
                redis_config = redis_config.with_pipeline_size(pipeline_size);
            }

            builder = builder.adapter(AdapterConfig::Redis(redis_config));
        }
    }

    // Parse JetStream config
    #[cfg(feature = "jetstream")]
    if let Some(jetstream) = value.get("jetstream") {
        if let Some(url) = jetstream.get("url").and_then(|v| v.as_str()) {
            let mut js_config = JetStreamAdapterConfig::new(url);

            if let Some(prefix) = jetstream.get("prefix").and_then(|v| v.as_str()) {
                js_config = js_config.with_prefix(prefix);
            }
            if let Some(max_messages) = jetstream.get("max_messages").and_then(|v| v.as_i64()) {
                js_config = js_config.with_max_messages(max_messages);
            }
            if let Some(replicas) = jetstream.get("replicas").and_then(|v| v.as_u64()) {
                let replicas = usize::try_from(replicas).ok()?;
                js_config = js_config.with_replicas(replicas);
            }

            builder = builder.adapter(AdapterConfig::JetStream(js_config));
        }
    }

    // Parse Net config
    #[cfg(feature = "net")]
    if let Some(net) = value.get("net") {
        let bind_addr: std::net::SocketAddr = net
            .get("bind_addr")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())?;

        let peer_addr: std::net::SocketAddr = net
            .get("peer_addr")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())?;

        let psk: [u8; 32] = net
            .get("psk")
            .and_then(|v| v.as_str())
            .and_then(|s| hex::decode(s).ok())
            .and_then(|v| v.try_into().ok())?;

        let role = net
            .get("role")
            .and_then(|v| v.as_str())
            .unwrap_or("initiator");

        let mut net_config = match role {
            "initiator" => {
                let peer_pubkey: [u8; 32] = net
                    .get("peer_public_key")
                    .and_then(|v| v.as_str())
                    .and_then(|s| hex::decode(s).ok())
                    .and_then(|v| v.try_into().ok())?;
                NetAdapterConfig::initiator(bind_addr, peer_addr, psk, peer_pubkey)
            }
            "responder" => {
                let secret_key: [u8; 32] = net
                    .get("secret_key")
                    .and_then(|v| v.as_str())
                    .and_then(|s| hex::decode(s).ok())
                    .and_then(|v| v.try_into().ok())?;
                let public_key: [u8; 32] = net
                    .get("public_key")
                    .and_then(|v| v.as_str())
                    .and_then(|s| hex::decode(s).ok())
                    .and_then(|v| v.try_into().ok())?;
                let keypair = StaticKeypair::from_keys(secret_key, public_key);
                NetAdapterConfig::responder(bind_addr, peer_addr, psk, keypair)
            }
            _ => return None,
        };

        // Apply optional settings
        if let Some(reliability) = net.get("reliability").and_then(|v| v.as_str()) {
            net_config = net_config.with_reliability(match reliability {
                "light" => ReliabilityConfig::Light,
                "full" => ReliabilityConfig::Full,
                _ => ReliabilityConfig::None,
            });
        }

        if let Some(pool_size) = net.get("packet_pool_size").and_then(|v| v.as_u64()) {
            if let Ok(size) = usize::try_from(pool_size) {
                net_config = net_config.with_pool_size(size);
            }
        }

        // Reject `0` for `heartbeat_interval_ms` and
        // `session_timeout_ms`. `EventBusConfig::validate` rejects
        // zero `Duration`s for `cooldown`, `metrics_window`, etc.,
        // but the Net adapter's JSON parser had no equivalent guard
        // — a `0` here flowed through to `Duration::from_millis(0)`,
        // which on the heartbeat path busy-loops the heartbeat task
        // and saturates a CPU. Treat zero as a misconfig and refuse
        // to build the bus, surfacing as `InvalidJson` so the FFI
        // caller sees a typed failure rather than a hung daemon.
        if let Some(interval_ms) = net.get("heartbeat_interval_ms").and_then(|v| v.as_u64()) {
            if interval_ms == 0 {
                return None;
            }
            net_config =
                net_config.with_heartbeat_interval(std::time::Duration::from_millis(interval_ms));
        }

        if let Some(timeout_ms) = net.get("session_timeout_ms").and_then(|v| v.as_u64()) {
            if timeout_ms == 0 {
                return None;
            }
            net_config =
                net_config.with_session_timeout(std::time::Duration::from_millis(timeout_ms));
        }

        if let Some(batched) = net.get("batched_io").and_then(|v| v.as_bool()) {
            net_config = net_config.with_batched_io(batched);
        }

        builder = builder.adapter(AdapterConfig::Net(Box::new(net_config)));
    }

    builder.build().ok()
}

fn create_with_config(runtime: Runtime, config: EventBusConfig) -> *mut NetHandle {
    let bus = match runtime.block_on(EventBus::new(config)) {
        Ok(bus) => bus,
        Err(_) => {
            // Send the runtime off to a fresh OS thread for
            // dropping. Dropping a multi-thread tokio `Runtime`
            // from inside another tokio runtime's worker thread
            // panics ("Cannot drop a runtime in a context where
            // blocking is not allowed"); a panic here would unwind
            // across this `extern "C"` frame. The fresh thread
            // guarantees a non-tokio context, so the drop is sound
            // regardless of the caller's runtime environment. We
            // don't `join()` the thread — the drop completes on
            // its own and the caller has already been told
            // `net_init` failed (returning null).
            std::thread::spawn(move || drop(runtime));
            return ptr::null_mut();
        }
    };

    let handle = Box::new(NetHandle {
        bus: std::mem::ManuallyDrop::new(bus),
        runtime: std::mem::ManuallyDrop::new(runtime),
        shutting_down: std::sync::atomic::AtomicBool::new(false),
        active_ops: std::sync::atomic::AtomicU32::new(0),
        bus_taken: std::sync::atomic::AtomicBool::new(false),
        shutdown_completed: std::sync::atomic::AtomicBool::new(false),
    });

    Box::into_raw(handle)
}

/// Ingest a single event.
///
/// # Parameters
///
/// - `handle`: Event bus handle from `net_init`.
/// - `event_json`: JSON event string (UTF-8).
/// - `len`: Length of the event string in bytes.
///
/// # Returns
///
/// - `0` on success
/// - Negative error code on failure
#[unsafe(no_mangle)]
pub extern "C" fn net_ingest(
    handle: *mut NetHandle,
    event_json: *const c_char,
    len: usize,
) -> c_int {
    if !handle_is_valid(handle) || event_json.is_null() {
        return NetError::NullPointer.into();
    }

    let handle = unsafe { &*handle };
    let _guard = match enter_ffi_op(handle) {
        Ok(g) => g,
        Err(err) => return err,
    };

    // `slice::from_raw_parts` requires `len <= isize::MAX`. A
    // C caller passing a sign-extended `-1` (or any
    // `len > isize::MAX as usize`) triggers immediate UB before
    // any other validation runs. Reject such inputs explicitly
    // — caller should never see this in practice; surfacing a
    // typed error is safer than UB.
    if len > isize::MAX as usize {
        return NetError::InvalidJson.into();
    }
    // Parse event JSON
    let json_bytes = unsafe { std::slice::from_raw_parts(event_json as *const u8, len) };
    let json_str = match std::str::from_utf8(json_bytes) {
        Ok(s) => s,
        Err(_) => return NetError::InvalidUtf8.into(),
    };

    let event = match Event::from_str(json_str) {
        Ok(e) => e,
        Err(_) => return NetError::InvalidJson.into(),
    };

    // Ingest
    match handle.bus.ingest(event) {
        Ok(_) => NetError::Success.into(),
        Err(_) => NetError::IngestionFailed.into(),
    }
}

/// Ingest a raw JSON string (fastest path).
///
/// The JSON string is stored directly without parsing.
/// This is the recommended method for high-throughput ingestion.
///
/// # Parameters
///
/// - `handle`: Event bus handle from `net_init`.
/// - `json`: JSON string (UTF-8).
/// - `len`: Length of the JSON string in bytes.
///
/// # Returns
///
/// - `0` on success
/// - Negative error code on failure
#[unsafe(no_mangle)]
pub extern "C" fn net_ingest_raw(handle: *mut NetHandle, json: *const c_char, len: usize) -> c_int {
    if !handle_is_valid(handle) || json.is_null() {
        return NetError::NullPointer.into();
    }

    let handle = unsafe { &*handle };
    let _guard = match enter_ffi_op(handle) {
        Ok(g) => g,
        Err(err) => return err,
    };

    // `slice::from_raw_parts` requires `len <= isize::MAX`.
    if len > isize::MAX as usize {
        return NetError::InvalidJson.into();
    }
    let json_bytes = unsafe { std::slice::from_raw_parts(json as *const u8, len) };
    let json_str = match std::str::from_utf8(json_bytes) {
        Ok(s) => s,
        Err(_) => return NetError::InvalidUtf8.into(),
    };

    let raw = RawEvent::from_str(json_str);

    match handle.bus.ingest_raw(raw) {
        Ok(_) => NetError::Success.into(),
        Err(_) => NetError::IngestionFailed.into(),
    }
}

/// Ingest multiple raw JSON strings (fastest batch path).
///
/// # Parameters
///
/// - `handle`: Event bus handle.
/// - `jsons`: Array of pointers to JSON strings.
/// - `lens`: Array of lengths for each JSON string.
/// - `count`: Number of events in the arrays.
///
/// # Returns
///
/// Number of successfully ingested events, or negative error code.
#[unsafe(no_mangle)]
pub extern "C" fn net_ingest_raw_batch(
    handle: *mut NetHandle,
    jsons: *const *const c_char,
    lens: *const usize,
    count: usize,
) -> c_int {
    if !handle_is_valid(handle) || jsons.is_null() || lens.is_null() {
        return NetError::NullPointer.into();
    }
    if count == 0 {
        return 0;
    }

    let handle = unsafe { &*handle };
    let _guard = match enter_ffi_op(handle) {
        Ok(g) => g,
        Err(err) => return err,
    };
    let mut events = Vec::with_capacity(count);
    // Track per-entry drops so the caller's accounting can
    // reconcile the returned count against the input count.
    // Pre-fix per-entry rejects (null pointer, oversized length,
    // invalid UTF-8) were silently `continue`-d and the caller
    // saw `count - drops` accepted events without any signal as
    // to which input indices were dropped. A binding that
    // attributed the drop to back-pressure and retried got the
    // wrong indices and double-published the good ones.
    //
    // The C-API contract is "returns count of accepted events";
    // expanding it to take an out-param of dropped indices is
    // an API addition, not a fix-in-place. Emit `tracing::warn!`
    // with the offending index AND reason so operators
    // observing the bus can correlate drop counts to specific
    // inputs without changing the C surface. For high-volume
    // bindings this should still be sized at one log line per
    // dropped entry; if that ever matters in practice the
    // `*_ex` follow-up can return the indices structurally.
    let mut dropped_null = 0usize;
    let mut dropped_oversize = 0usize;
    let mut dropped_invalid_utf8 = 0usize;

    for i in 0..count {
        let json_ptr = unsafe { *jsons.add(i) };
        let len = unsafe { *lens.add(i) };

        if json_ptr.is_null() {
            tracing::warn!(
                index = i,
                "net_ingest_raw_batch: dropping entry with null pointer"
            );
            dropped_null += 1;
            continue;
        }

        // `slice::from_raw_parts` requires `len <= isize::MAX`.
        // Skip pathological per-entry lengths rather than UB.
        if len > isize::MAX as usize {
            tracing::warn!(
                index = i,
                len,
                "net_ingest_raw_batch: dropping entry with len > isize::MAX"
            );
            dropped_oversize += 1;
            continue;
        }
        let json_bytes = unsafe { std::slice::from_raw_parts(json_ptr as *const u8, len) };
        match std::str::from_utf8(json_bytes) {
            Ok(json_str) => events.push(RawEvent::from_str(json_str)),
            Err(_) => {
                tracing::warn!(
                    index = i,
                    "net_ingest_raw_batch: dropping entry with invalid UTF-8"
                );
                dropped_invalid_utf8 += 1;
            }
        }
    }
    let total_dropped = dropped_null + dropped_oversize + dropped_invalid_utf8;
    if total_dropped > 0 {
        // Aggregate summary for log-pipeline filters that fold
        // per-index lines.
        tracing::warn!(
            input_count = count,
            dropped_null,
            dropped_oversize,
            dropped_invalid_utf8,
            "net_ingest_raw_batch: {} of {} entries dropped before ingest",
            total_dropped,
            count,
        );
    }

    let count = handle.bus.ingest_raw_batch(events);
    // Returning `c_int::MAX` on overflow would be ambiguous with a real
    // `INT_MAX` ingest. Signal overflow explicitly so callers doing
    // accounting in high-throughput paths do not silently miscount.
    c_int::try_from(count).unwrap_or_else(|_| NetError::IntOverflow.into())
}

/// Ingest multiple events.
///
/// # Parameters
///
/// - `handle`: Event bus handle.
/// - `events_json`: JSON array of events (UTF-8, null-terminated).
///
/// # Returns
///
/// Number of successfully ingested events, or negative error code.
#[unsafe(no_mangle)]
pub extern "C" fn net_ingest_batch(handle: *mut NetHandle, events_json: *const c_char) -> c_int {
    if !handle_is_valid(handle) || events_json.is_null() {
        return NetError::NullPointer.into();
    }

    let handle = unsafe { &*handle };
    let _guard = match enter_ffi_op(handle) {
        Ok(g) => g,
        Err(err) => return err,
    };

    let json_str = match unsafe { CStr::from_ptr(events_json) }.to_str() {
        Ok(s) => s,
        Err(_) => return NetError::InvalidUtf8.into(),
    };

    // Parse as JSON array
    let array: Vec<serde_json::Value> = match serde_json::from_str(json_str) {
        Ok(a) => a,
        Err(_) => return NetError::InvalidJson.into(),
    };

    let events: Vec<Event> = array.into_iter().map(Event::new).collect();
    let count = handle.bus.ingest_batch(events);

    // Returning `c_int::MAX` on overflow would be ambiguous with a real
    // `INT_MAX` ingest. Signal overflow explicitly — matches the
    // `net_ingest_raw_batch` contract.
    c_int::try_from(count).unwrap_or_else(|_| NetError::IntOverflow.into())
}

/// Parse the JSON request body passed to `net_poll` into a
/// `ConsumeRequest`. Returns the negative `NetError` code on parse
/// failure so the caller can surface it back across FFI. Both `limit`
/// and `cursor` are optional, but if either key is present with the
/// wrong JSON type it is an explicit error — silently falling back to
/// the default would hide caller bugs (e.g. the Go binding that
/// previously serialized `cursor` but had it dropped server-side).
fn parse_poll_request_json(json_str: &str) -> Result<ConsumeRequest, c_int> {
    let value: serde_json::Value =
        serde_json::from_str(json_str).map_err(|_| c_int::from(NetError::InvalidJson))?;

    let limit = match value.get("limit") {
        None | Some(serde_json::Value::Null) => 100usize,
        Some(v) => match v.as_u64() {
            // `as usize` would silently truncate on 32-bit targets for
            // values above `usize::MAX`. Reject such inputs explicitly
            // so a caller asking for e.g. 2^33 events on a wasm32
            // build gets `InvalidJson` instead of a tiny wrap-around.
            Some(n) => usize::try_from(n).map_err(|_| c_int::from(NetError::InvalidJson))?,
            None => return Err(NetError::InvalidJson.into()),
        },
    };
    let cursor = match value.get("cursor") {
        None | Some(serde_json::Value::Null) => None,
        Some(v) => match v.as_str() {
            Some(s) => Some(s.to_owned()),
            None => return Err(NetError::InvalidJson.into()),
        },
    };
    let mut req = ConsumeRequest::new(limit);
    req.from_id = cursor;
    Ok(req)
}

/// Poll events from the bus.
///
/// # Parameters
///
/// - `handle`: Event bus handle.
/// - `request_json`: JSON request string (UTF-8, null-terminated).
///   Example: `{"limit": 100, "ordering": "InsertionTs"}`
/// - `out_buffer`: Output buffer for JSON response.
/// - `buffer_len`: Size of the output buffer.
///
/// # Returns
///
/// - Number of bytes written to buffer on success
/// - Negative error code on failure
#[unsafe(no_mangle)]
pub extern "C" fn net_poll(
    handle: *mut NetHandle,
    request_json: *const c_char,
    out_buffer: *mut c_char,
    buffer_len: usize,
) -> c_int {
    if !handle_is_valid(handle) || out_buffer.is_null() {
        return NetError::NullPointer.into();
    }

    let handle = unsafe { &*handle };
    let _guard = match enter_ffi_op(handle) {
        Ok(g) => g,
        Err(err) => return err,
    };

    // Parse request
    let request = if request_json.is_null() {
        ConsumeRequest::new(100)
    } else {
        let json_str = match unsafe { CStr::from_ptr(request_json) }.to_str() {
            Ok(s) => s,
            Err(_) => return NetError::InvalidUtf8.into(),
        };
        match parse_poll_request_json(json_str) {
            Ok(req) => req,
            Err(code) => return code,
        }
    };

    // Reject buffers too small to even hold an empty-response
    // JSON envelope. This catches the degenerate "tiny buffer"
    // case before we hit the adapter — `BufferTooSmall` returned
    // here means "no work was done, caller's cursor is unchanged."
    // 256 bytes comfortably fits the empty-response JSON below
    // even with a long echoed `next_id` cursor.
    const MIN_RESPONSE_BUFFER: usize = 256;
    if buffer_len < MIN_RESPONSE_BUFFER {
        return NetError::BufferTooSmall.into();
    }

    // Stash the cursor before moving `request` into `poll()` so
    // the post-poll fallback can echo it back to the caller. On
    // overflow we write a minimal "no events delivered, cursor
    // unchanged" response so the caller's next poll re-fetches
    // the same range — events are not lost on idempotent
    // adapters (Redis XRANGE, JetStream direct_get).
    let cursor_snapshot = request.from_id.clone();

    // Poll
    let response = match handle.runtime.block_on(handle.bus.poll(request)) {
        Ok(r) => r,
        Err(_) => return NetError::PollFailed.into(),
    };

    // Serialize response. Events that fail to parse are included as raw
    // strings so the caller can see all events and detect parse failures.
    let total_events = response.events.len();
    let mut parsed_events: Vec<serde_json::Value> = Vec::with_capacity(total_events);
    let mut parse_errors: usize = 0;
    for e in &response.events {
        match e.parse() {
            Ok(v) => parsed_events.push(v),
            Err(_) => {
                parse_errors += 1;
                // Include the raw bytes as a string so the caller doesn't silently lose events
                if let Ok(raw) = e.raw_str() {
                    parsed_events.push(serde_json::Value::String(raw.to_string()));
                }
            }
        }
    }
    let response_json = match serde_json::to_string(&serde_json::json!({
        "events": parsed_events,
        "next_id": response.next_id,
        "has_more": response.has_more,
        "count": parsed_events.len(),
        "parse_errors": parse_errors,
    })) {
        Ok(s) => s,
        Err(_) => return NetError::Unknown.into(),
    };

    // Buffer overflow: emit a minimal fallback response that echoes
    // the caller's original cursor as `next_id`. The caller's next
    // poll runs against the same range and re-delivers the events
    // (idempotent on Redis XRANGE / JetStream direct_get). Without
    // this, a caller that trusts `next_id` blindly would advance
    // past the unread batch.
    if response_json.len() + 1 > buffer_len {
        let fallback = serde_json::to_string(&serde_json::json!({
            "events": [],
            "next_id": cursor_snapshot,
            "has_more": true,
            "count": 0,
            "parse_errors": 0,
            "buffer_too_small": true,
            "events_dropped": total_events,
        }))
        .unwrap_or_else(|_| String::from(
            r#"{"events":[],"next_id":null,"has_more":true,"count":0,"parse_errors":0,"buffer_too_small":true}"#
        ));
        if fallback.len() < buffer_len {
            unsafe {
                ptr::copy_nonoverlapping(
                    fallback.as_ptr() as *const c_char,
                    out_buffer,
                    fallback.len(),
                );
                *out_buffer.add(fallback.len()) = 0;
            }
        }
        return NetError::BufferTooSmall.into();
    }

    // Copy to output buffer
    unsafe {
        ptr::copy_nonoverlapping(
            response_json.as_ptr() as *const c_char,
            out_buffer,
            response_json.len(),
        );
        *out_buffer.add(response_json.len()) = 0; // Null terminate
    }

    // Data was already copied into the caller's buffer; a
    // `c_int` overflow here means the byte count exceeds c_int's
    // range, NOT that the buffer was too small. Returning
    // `BufferTooSmall` would tell the caller to "resize and retry"
    // when retrying can't fix the actual condition. `IntOverflow`
    // is the documented variant for this case.
    match c_int::try_from(response_json.len()) {
        Ok(n) => n,
        Err(_) => NetError::IntOverflow.into(),
    }
}

/// Get event bus statistics.
///
/// # Parameters
///
/// - `handle`: Event bus handle.
/// - `out_buffer`: Output buffer for JSON statistics.
/// - `buffer_len`: Size of the output buffer.
///
/// # Returns
///
/// Number of bytes written, or negative error code.
#[unsafe(no_mangle)]
pub extern "C" fn net_stats(
    handle: *mut NetHandle,
    out_buffer: *mut c_char,
    buffer_len: usize,
) -> c_int {
    if !handle_is_valid(handle) || out_buffer.is_null() {
        return NetError::NullPointer.into();
    }

    let handle = unsafe { &*handle };
    let _guard = match enter_ffi_op(handle) {
        Ok(g) => g,
        Err(err) => return err,
    };
    let stats = handle.bus.stats();
    let shard_stats = handle.bus.shard_stats();

    let stats_json = match serde_json::to_string(&serde_json::json!({
        "events_ingested": stats.events_ingested.load(std::sync::atomic::Ordering::Relaxed),
        "events_dropped": stats.events_dropped.load(std::sync::atomic::Ordering::Relaxed),
        "batches_dispatched": stats.batches_dispatched.load(std::sync::atomic::Ordering::Relaxed),
        "shard_events_ingested": shard_stats.events_ingested,
        "shard_events_dropped": shard_stats.events_dropped,
        "shard_batches_dispatched": shard_stats.batches_dispatched,
    })) {
        Ok(s) => s,
        Err(_) => return NetError::Unknown.into(),
    };

    if stats_json.len() + 1 > buffer_len {
        return NetError::BufferTooSmall.into();
    }

    unsafe {
        ptr::copy_nonoverlapping(
            stats_json.as_ptr() as *const c_char,
            out_buffer,
            stats_json.len(),
        );
        *out_buffer.add(stats_json.len()) = 0;
    }

    // See net_poll above — the data was already copied, so an
    // overflowing length is `IntOverflow`, not `BufferTooSmall`.
    match c_int::try_from(stats_json.len()) {
        Ok(n) => n,
        Err(_) => NetError::IntOverflow.into(),
    }
}

/// Flush all pending batches to the adapter.
///
/// # Parameters
///
/// - `handle`: Event bus handle.
///
/// # Returns
///
/// - `0` on success
/// - Negative error code on failure
#[unsafe(no_mangle)]
pub extern "C" fn net_flush(handle: *mut NetHandle) -> c_int {
    if !handle_is_valid(handle) {
        return NetError::NullPointer.into();
    }

    let handle = unsafe { &*handle };
    let _guard = match enter_ffi_op(handle) {
        Ok(g) => g,
        Err(err) => return err,
    };

    match handle.runtime.block_on(handle.bus.flush()) {
        Ok(_) => NetError::Success.into(),
        Err(_) => NetError::Unknown.into(),
    }
}

/// Shut down the event bus and free resources.
///
/// # Parameters
///
/// - `handle`: Event bus handle. After this call, the handle is invalid.
///
/// # Returns
///
/// - `0` on success
/// - Negative error code on failure (including `Unknown` if the
///   bounded wait for in-flight FFI operations expired before the bus
///   could be shut down cleanly)
///
/// # Notes
///
/// The handle's storage is intentionally leaked: the box is never
/// returned to the allocator. See `NetHandle`'s docs for why. This is
/// a one-time cost per shutdown — typically per-process, since most C
/// callers initialize the bus once and shut down once.
#[unsafe(no_mangle)]
pub extern "C" fn net_shutdown(handle: *mut NetHandle) -> c_int {
    if !handle_is_valid(handle) {
        return NetError::NullPointer.into();
    }

    // Scope the `&NetHandle` borrow into an inner block so it is
    // verifiably out of scope before the
    // `ManuallyDrop::take(&mut (*handle).bus)` calls below.
    // Holding an `&NetHandle` in scope for the whole function
    // while taking a raw `&mut (*handle).bus` later would rely on
    // NLL ending the immutable borrow before the mutable take —
    // a pattern fragile under stacked/tree borrow models. The
    // block-scoped borrow makes the lifetime constraint explicit
    // and obvious to both the compiler and any future maintainer.
    let drained_and_taken = {
        // SAFETY: The C contract guarantees `handle` is valid here and that
        // `net_shutdown` is not called concurrently with itself. Future
        // dereferences of the box from concurrent FFI ops on other threads
        // are also sound because we never free the box (see below).
        let handle_ref = unsafe { &*handle };

        // Signal shutdown so concurrent FFI calls bail before touching
        // `bus`/`runtime`. SeqCst pairs with `FfiOpGuard::try_enter`.
        handle_ref
            .shutting_down
            .store(true, std::sync::atomic::Ordering::SeqCst);

        // Bounded wait for in-flight ops to drain. Without a deadline, a
        // hung concurrent operation (e.g. `net_flush` against a stalled
        // adapter) would pin a CPU at 100% inside this loop forever.
        //
        // `std::hint::spin_loop()` is a CPU pause hint, not a yield. On
        // a single-threaded executor (or any configuration where the FFI
        // caller's thread is the same one that needs to make progress on
        // the in-flight async work) the tight spin starves the very tokio
        // worker we're waiting for, *causing* the deadline to expire when
        // it otherwise wouldn't. `thread::yield_now` lets the OS schedule
        // whatever's blocked, and a 1ms `thread::sleep` between yields
        // prevents the loop from saturating a CPU on platforms where
        // `yield_now` is a
        // near-no-op under low contention. The drain we expect to take
        // milliseconds, so a millisecond-granularity poll is fine.
        let deadline = std::time::Instant::now() + FFI_SHUTDOWN_DEADLINE;
        let mut drained = false;
        loop {
            if handle_ref
                .active_ops
                .load(std::sync::atomic::Ordering::SeqCst)
                == 0
            {
                drained = true;
                break;
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::yield_now();
            std::thread::sleep(std::time::Duration::from_millis(1));
        }

        if !drained {
            // In-flight ops may still be reading `bus`/`runtime`; reading
            // them out via `ManuallyDrop::take` would race those readers.
            // Leak both fields along with the box. Future ops still see
            // `shutting_down=true` and bail before touching either field,
            // so the leaked memory is never read again.
            return NetError::Unknown.into();
        }

        // Idempotent shutdown: if a previous `net_shutdown` already
        // moved out the bus/runtime, do not call `ManuallyDrop::take`
        // a second time (that would be UB). The first call may still
        // be inside `runtime.block_on(bus.shutdown())` though — pre-
        // fix the second caller observed `bus_taken == true` and
        // returned `Success` immediately, falsely signaling
        // completion of an in-progress shutdown. Spin on
        // `shutdown_completed` (set by the first caller AFTER
        // `bus.shutdown()` returns) so subsequent callers wait for
        // the actual completion.
        if handle_ref
            .bus_taken
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            // Wait for the first caller to actually finish.
            // Bounded by the same FFI_SHUTDOWN_DEADLINE as the
            // `active_ops` drain — if the first caller is wedged
            // longer than that, we surface a Transient error rather
            // than block forever.
            let inner_deadline = std::time::Instant::now() + FFI_SHUTDOWN_DEADLINE;
            while !handle_ref
                .shutdown_completed
                .load(std::sync::atomic::Ordering::Acquire)
            {
                if std::time::Instant::now() >= inner_deadline {
                    return NetError::Unknown.into();
                }
                std::thread::yield_now();
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            return NetError::Success.into();
        }
        drained
    };
    let _ = drained_and_taken;

    // SAFETY: `active_ops` reached zero with `shutting_down=true`, so:
    //   - Every FFI op that started before shutdown has fully
    //     completed (decremented `active_ops` on guard drop).
    //   - Any future FFI op will observe `shutting_down=true` and
    //     bail in `try_enter` before touching `bus` / `runtime`.
    // Plus, `bus_taken` was just CAS'd from false → true, so no other
    // shutdown is concurrently moving the same fields out. The
    // immutable `handle_ref` borrow above has been dropped (block
    // scope ended), so the `&mut`-via-raw-pointer below is the
    // only live access — no stacked/tree-borrow race.
    //
    // We deliberately do NOT call `Box::from_raw` here. The box's
    // `shutting_down` / `active_ops` / `bus_taken` atomics must remain
    // valid memory because future FFI ops still dereference the
    // C-side pointer to check them. Leaking the box is the
    // correctness fix for the previous use-after-free; the per-handle
    // storage cost is a one-time overhead.
    let bus = unsafe { std::mem::ManuallyDrop::take(&mut (*handle).bus) };
    let runtime = unsafe { std::mem::ManuallyDrop::take(&mut (*handle).runtime) };

    // Flush pending batches and gracefully shut down the adapter
    // before dropping the runtime. Without this, pending events in
    // ring buffers and batch workers would be silently lost.
    let result = runtime.block_on(bus.shutdown());

    // `bus` and `runtime` go out of scope here and are dropped.
    // The leaked box keeps the atomics alive for any straggler ops.

    // Signal completion to any second/third caller spinning on
    // `shutdown_completed` in the idempotent path above. Done
    // AFTER `bus.shutdown()` returns and AFTER the bus / runtime
    // drop, so subsequent callers can rely on this flag as a
    // hard "shutdown is fully done" barrier.
    unsafe { &*handle }
        .shutdown_completed
        .store(true, std::sync::atomic::Ordering::Release);

    match result {
        Ok(()) => NetError::Success.into(),
        Err(_) => NetError::Unknown.into(),
    }
}

/// Get the number of shards.
///
/// # Parameters
///
/// - `handle`: Event bus handle.
///
/// # Returns
///
/// Number of shards, or 0 if handle is null.
#[unsafe(no_mangle)]
pub extern "C" fn net_num_shards(handle: *mut NetHandle) -> u16 {
    if !handle_is_valid(handle) {
        return 0;
    }
    let handle = unsafe { &*handle };
    let _guard = match enter_ffi_op(handle) {
        Ok(g) => g,
        Err(_) => return 0,
    };
    handle.bus.num_shards()
}

/// Get the library version.
///
/// # Returns
///
/// Version string (static, do not free).
#[unsafe(no_mangle)]
pub extern "C" fn net_version() -> *const c_char {
    static VERSION: &[u8] = b"0.8.0\0";
    VERSION.as_ptr() as *const c_char
}

/// Generate a new Net keypair.
///
/// # Returns
///
/// JSON string with hex-encoded public_key and secret_key.
/// The caller must free the returned string with `net_free_string`.
/// Returns NULL if Net feature is not enabled.
#[cfg(feature = "net")]
#[unsafe(no_mangle)]
pub extern "C" fn net_generate_keypair() -> *mut c_char {
    let keypair = StaticKeypair::generate();
    let json = serde_json::json!({
        "public_key": hex::encode(keypair.public_key()),
        "secret_key": hex::encode(keypair.secret_key()),
    });

    match CString::new(json.to_string()) {
        Ok(s) => s.into_raw(),
        Err(_) => ptr::null_mut(),
    }
}

/// Free a string returned by Net functions.
///
/// # Parameters
///
/// - `s`: String pointer returned by `net_generate_keypair` or similar.
#[cfg(feature = "net")]
#[unsafe(no_mangle)]
pub extern "C" fn net_free_string(s: *mut c_char) {
    if !s.is_null() {
        unsafe {
            drop(CString::from_raw(s));
        }
    }
}

// `net.h` declares both `net_generate_keypair` and
// `net_free_string` unconditionally — a consumer linking against
// a cdylib built without the `net` feature would otherwise hit
// a load-time missing-symbol error despite the header advertising
// the symbol. Provide always-empty stubs so the symbol is
// resolvable on every build configuration. Mirrors the
// `nat-traversal` cfg pattern in `mesh.rs`.

/// Stub for builds without the `net` feature.
///
/// `net.h` declares `net_generate_keypair` unconditionally, so
/// the symbol must be resolvable on every build configuration.
/// Returns NULL since keypair generation requires the net feature.
#[cfg(not(feature = "net"))]
#[unsafe(no_mangle)]
pub extern "C" fn net_generate_keypair() -> *mut c_char {
    ptr::null_mut()
}

/// Stub for builds without the `net` feature.
///
/// Mirrors the always-on signature in `net.h`. Reclaims a
/// CString-allocated pointer if non-null.
#[cfg(not(feature = "net"))]
#[unsafe(no_mangle)]
pub extern "C" fn net_free_string(s: *mut c_char) {
    if !s.is_null() {
        unsafe {
            drop(std::ffi::CString::from_raw(s));
        }
    }
}

// =========================================================================
// Structured (non-JSON) API — _ex variants
// =========================================================================

/// Ingestion receipt for C consumers.
#[repr(C)]
pub struct NetReceipt {
    /// Shard the event was assigned to.
    pub shard_id: u16,
    /// Insertion timestamp (nanoseconds).
    pub timestamp: u64,
}

// Pin layout invariants for `NetReceipt`. `#[repr(C)]` already
// gives C ABI compatibility per platform, but doesn't catch a
// future field-reorder or field-add — both would silently break
// any C/Go/Python binding that hard-codes the struct layout.
// Static asserts on 64-bit targets (the production deployment
// shape) trip CI before such a change reaches a binary release.
//
// 64-bit: `u16 (2) + 6 pad + u64 (8)` = 16 bytes, alignment 8.
#[cfg(target_pointer_width = "64")]
const _: () = assert!(
    std::mem::size_of::<NetReceipt>() == 16,
    "NetReceipt size changed on 64-bit; bindings hard-code 16. \
     If the change is intentional, bump the binding versions and \
     update this assertion."
);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(
    std::mem::align_of::<NetReceipt>() == 8,
    "NetReceipt alignment changed on 64-bit; bindings expect 8."
);

/// A single stored event for C consumers.
///
/// # Safety contract for callers
///
/// `id`/`id_len` and `raw`/`raw_len` are produced by Rust as a
/// `Box<[u8]>` whose fat-pointer length is reconstructed at free
/// time from `id_len` / `raw_len`. The fields are `pub` because
/// `#[repr(C)]` exposes them to C, **but they must be treated as
/// read-only** between the `net_poll_*` call that produced them
/// and the `net_free_poll_result` that consumes them.
///
/// Mutating `id_len` or `raw_len` (or copying the struct, replacing
/// the pointer, and then freeing) causes
/// `Box::from_raw(slice_from_raw_parts_mut(ptr, wrong_len))` to be
/// undefined behavior on free — the allocator records the
/// allocation size and any mismatch is UB.
#[repr(C)]
pub struct NetEvent {
    /// Event ID (not null-terminated, use `id_len`).
    /// Read-only after `net_poll_*`; do not mutate.
    pub id: *const c_char,
    /// Length of the event ID. Read-only after `net_poll_*`; do not
    /// mutate (mutation causes UB on free).
    pub id_len: usize,
    /// Raw JSON payload (not null-terminated, use `raw_len`).
    /// Read-only after `net_poll_*`; do not mutate.
    pub raw: *const c_char,
    /// Length of the raw JSON payload. Read-only after
    /// `net_poll_*`; do not mutate (mutation causes UB on free).
    pub raw_len: usize,
    /// Insertion timestamp (nanoseconds).
    pub insertion_ts: u64,
    /// Shard ID.
    pub shard_id: u16,
}

// Pin layout invariants for `NetEvent`. See `NetReceipt`'s
// asserts for rationale. Bindings (C, Go, Python, Node) hard-
// code 48 bytes on 64-bit; an accidental reorder or new field
// would silently shift every offset.
//
// 64-bit: `4 × 8 (ptrs/usize) + u64 (8) + u16 (2) + 6 trail` = 48.
#[cfg(target_pointer_width = "64")]
const _: () = assert!(
    std::mem::size_of::<NetEvent>() == 48,
    "NetEvent size changed on 64-bit; bindings hard-code 48. \
     If the change is intentional, bump the binding versions and \
     update this assertion."
);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(
    std::mem::align_of::<NetEvent>() == 8,
    "NetEvent alignment changed on 64-bit; bindings expect 8."
);

/// Poll result for C consumers.
#[repr(C)]
pub struct NetPollResult {
    /// Array of events. Free with `net_free_poll_result`.
    pub events: *mut NetEvent,
    /// Number of events in the array.
    pub count: usize,
    /// Cursor for the next poll (null-terminated). NULL if no more.
    pub next_id: *mut c_char,
    /// 1 if more events are available, 0 otherwise.
    pub has_more: c_int,
}

/// Stats for C consumers.
#[repr(C)]
pub struct NetStats {
    /// Total events ingested.
    pub events_ingested: u64,
    /// Events dropped due to backpressure.
    pub events_dropped: u64,
    /// Batches dispatched to the adapter.
    pub batches_dispatched: u64,
}

/// Ingest raw JSON with structured receipt.
#[unsafe(no_mangle)]
pub extern "C" fn net_ingest_raw_ex(
    handle: *mut NetHandle,
    json: *const c_char,
    len: usize,
    out: *mut NetReceipt,
) -> c_int {
    if !handle_is_valid(handle) || json.is_null() {
        return NetError::NullPointer.into();
    }

    let handle = unsafe { &*handle };
    let _guard = match enter_ffi_op(handle) {
        Ok(g) => g,
        Err(err) => return err,
    };

    // `slice::from_raw_parts` requires `len <= isize::MAX`.
    if len > isize::MAX as usize {
        return NetError::InvalidJson.into();
    }
    let json_bytes = unsafe { std::slice::from_raw_parts(json as *const u8, len) };
    let json_str = match std::str::from_utf8(json_bytes) {
        Ok(s) => s,
        Err(_) => return NetError::InvalidUtf8.into(),
    };

    let raw = RawEvent::from_str(json_str);

    match handle.bus.ingest_raw(raw) {
        Ok((shard_id, timestamp)) => {
            if !out.is_null() {
                unsafe {
                    (*out).shard_id = shard_id;
                    (*out).timestamp = timestamp;
                }
            }
            NetError::Success.into()
        }
        Err(_) => NetError::IngestionFailed.into(),
    }
}

/// Poll events with structured result (no JSON overhead).
///
/// The caller must free the result with `net_free_poll_result`.
#[unsafe(no_mangle)]
pub extern "C" fn net_poll_ex(
    handle: *mut NetHandle,
    limit: usize,
    cursor: *const c_char,
    out: *mut NetPollResult,
) -> c_int {
    if !handle_is_valid(handle) || out.is_null() {
        return NetError::NullPointer.into();
    }

    // Pre-validate `limit` BEFORE calling `bus.poll` — the bus
    // advances the consumer cursor before returning, so any
    // post-poll allocation failure (e.g. `Layout::array::<NetEvent>`
    // overflow on a pathological `count`, or `std::alloc::alloc`
    // returning null under memory pressure) would drop the response
    // and lose every event the cursor just stepped past. Reject
    // requests whose `count * size_of::<NetEvent>` would overflow
    // `isize::MAX` (the `Layout::array` cap) up front, so the
    // failure happens before the cursor moves.
    if limit > 0
        && (std::mem::size_of::<NetEvent>())
            .checked_mul(limit)
            .is_none_or(|v| v > isize::MAX as usize)
    {
        return NetError::IntOverflow.into();
    }

    let handle = unsafe { &*handle };
    let _guard = match enter_ffi_op(handle) {
        Ok(g) => g,
        Err(err) => return err,
    };

    let mut request = ConsumeRequest::new(limit);
    if !cursor.is_null() {
        if let Ok(s) = unsafe { CStr::from_ptr(cursor) }.to_str() {
            if !s.is_empty() {
                request = request.from(s);
            }
        }
    }

    let response = match handle.runtime.block_on(handle.bus.poll(request)) {
        Ok(r) => r,
        Err(_) => return NetError::PollFailed.into(),
    };

    let count = response.events.len();

    // Allocate events array.
    //
    // Each iteration allocates two boxed byte slices via
    // `Vec::to_vec().into_boxed_slice()`, which panic on OOM in
    // the global allocator. A panic across this `extern "C"`
    // body is UB — under the cgo/N-API/cffi unwind model the
    // panic propagates into a frame that doesn't expect it. Wrap
    // the per-event build in `catch_unwind`, track how many
    // events we've fully written, and on panic / mid-loop
    // failure free the partial array via `free_events_array`
    // so neither UB nor the partial allocations leak.
    let events_ptr = if count > 0 {
        let layout = match std::alloc::Layout::array::<NetEvent>(count) {
            Ok(l) => l,
            Err(_) => return NetError::Unknown.into(),
        };
        let ptr = unsafe { std::alloc::alloc(layout) as *mut NetEvent };
        if ptr.is_null() {
            return NetError::Unknown.into();
        }

        // Shared counter so the outer scope can clean up partial
        // writes if any iteration panics.
        let completed = std::cell::Cell::new(0usize);
        let build_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            for (i, event) in response.events.iter().enumerate() {
                let id_bytes = event.id.as_bytes().to_vec().into_boxed_slice();
                let id_len = id_bytes.len();
                let id_ptr = Box::into_raw(id_bytes) as *const c_char;

                let raw_bytes = event.raw.to_vec().into_boxed_slice();
                let raw_len = raw_bytes.len();
                let raw_ptr = Box::into_raw(raw_bytes) as *const c_char;

                unsafe {
                    ptr.add(i).write(NetEvent {
                        id: id_ptr,
                        id_len,
                        raw: raw_ptr,
                        raw_len,
                        insertion_ts: event.insertion_ts,
                        shard_id: event.shard_id,
                    });
                }
                completed.set(i + 1);
            }
        }));
        if build_result.is_err() {
            // A panic landed mid-loop. Free fully-written events
            // (those past `completed.get()` were never written, so
            // the inner `id`/`raw` pointers aren't valid). The
            // events array was allocated for `count` NetEvent
            // slots, so the dealloc must use that same layout.
            free_events_array_partial(ptr, completed.get(), count);
            return NetError::Unknown.into();
        }
        ptr
    } else {
        ptr::null_mut()
    };

    // Leak next_id if present.
    let next_id_ptr = match response.next_id {
        Some(ref s) => match std::ffi::CString::new(s.as_str()) {
            Ok(c) => c.into_raw(),
            Err(_) => {
                // Free already-allocated events before returning
                // error. `s.as_str()` is valid UTF-8 by `String`
                // invariant, so this is the interior-NUL path —
                // an upstream cursor id that contains `\0` cannot
                // round-trip through a C string. Pre-fix this
                // returned `InvalidUtf8`, which mis-described
                // the cause; bindings now see the more accurate
                // `InteriorNul`.
                free_events_array(events_ptr, count);
                return NetError::InteriorNul.into();
            }
        },
        None => ptr::null_mut(),
    };

    unsafe {
        (*out).events = events_ptr;
        (*out).count = count;
        (*out).next_id = next_id_ptr;
        (*out).has_more = if response.has_more { 1 } else { 0 };
    }

    NetError::Success.into()
}

/// Free an events array and all its id/raw allocations.
///
/// `count` is the number of fully-written events (those whose
/// inner `id` / `raw` boxed slices were initialized). It must
/// also match the `Layout::array::<NetEvent>` used at allocation
/// time — every existing caller writes exactly `count` events
/// before invoking this function. For partial-cleanup paths
/// (e.g. panic mid-build), use [`free_events_array_partial`].
fn free_events_array(events: *mut NetEvent, count: usize) {
    free_events_array_partial(events, count, count);
}

/// Free an events array where only `walk_count` entries have
/// fully-initialized `id`/`raw` allocations, but the array
/// itself was allocated for `alloc_count` slots. Per-event
/// boxes are freed for `0..walk_count`; the array is then
/// deallocated with the original `Layout::array::<NetEvent>(alloc_count)`
/// to match the allocation. Used by `net_poll_ex`'s panic-mid-loop
/// recovery path.
fn free_events_array_partial(events: *mut NetEvent, walk_count: usize, alloc_count: usize) {
    if events.is_null() || alloc_count == 0 {
        return;
    }
    for i in 0..walk_count {
        let event = unsafe { &*events.add(i) };
        if !event.id.is_null() {
            unsafe {
                let _ = Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                    event.id as *mut u8,
                    event.id_len,
                ));
            }
        }
        if !event.raw.is_null() {
            unsafe {
                let _ = Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                    event.raw as *mut u8,
                    event.raw_len,
                ));
            }
        }
    }
    if let Ok(layout) = std::alloc::Layout::array::<NetEvent>(alloc_count) {
        unsafe {
            std::alloc::dealloc(events as *mut u8, layout);
        }
    }
}

/// Free the internal allocations of a poll result returned by `net_poll_ex`.
///
/// This frees the events array (including each event's `id` and `raw` buffers)
/// and the `next_id` string. It does **not** free the `NetPollResult` struct
/// itself, which is caller-provided (typically stack-allocated or managed by
/// the caller).
#[unsafe(no_mangle)]
pub extern "C" fn net_free_poll_result(result: *mut NetPollResult) {
    if result.is_null() {
        return;
    }

    let result = unsafe { &mut *result };

    // Free events array and all id/raw allocations.
    free_events_array(result.events, result.count);

    // Free next_id.
    if !result.next_id.is_null() {
        unsafe {
            drop(std::ffi::CString::from_raw(result.next_id));
        }
    }

    // Null the fields so a second `net_free_poll_result` on the
    // same struct is a safe no-op rather than a double-free. The
    // C header's contract just says "free a poll result"; without
    // this clear, a defensive caller calling free twice (or two
    // wrappers each calling free in their destructor) would
    // re-`Box::from_raw` an already-freed pointer.
    result.events = std::ptr::null_mut();
    result.count = 0;
    result.next_id = std::ptr::null_mut();
    result.has_more = 0;
}

/// Get stats without JSON serialization.
#[unsafe(no_mangle)]
pub extern "C" fn net_stats_ex(handle: *mut NetHandle, out: *mut NetStats) -> c_int {
    if !handle_is_valid(handle) || out.is_null() {
        return NetError::NullPointer.into();
    }

    let handle = unsafe { &*handle };
    let _guard = match enter_ffi_op(handle) {
        Ok(g) => g,
        Err(err) => return err,
    };
    let stats = handle.bus.stats();

    unsafe {
        (*out).events_ingested = stats
            .events_ingested
            .load(std::sync::atomic::Ordering::Relaxed);
        (*out).events_dropped = stats
            .events_dropped
            .load(std::sync::atomic::Ordering::Relaxed);
        (*out).batches_dispatched = stats
            .batches_dispatched
            .load(std::sync::atomic::Ordering::Relaxed);
    }

    NetError::Success.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_config_valid() {
        let config = parse_config_json(r#"{"num_shards": 8}"#);
        assert!(config.is_some());
    }

    #[test]
    fn test_parse_config_num_shards_overflow() {
        // u16::MAX is 65535, so 65536 should fail
        let config = parse_config_json(r#"{"num_shards": 65536}"#);
        assert!(
            config.is_none(),
            "num_shards exceeding u16::MAX should fail"
        );

        // Much larger value should also fail
        let config = parse_config_json(r#"{"num_shards": 100000}"#);
        assert!(
            config.is_none(),
            "num_shards exceeding u16::MAX should fail"
        );
    }

    #[test]
    fn test_parse_config_num_shards_max_valid() {
        // u16::MAX (65535) should be valid
        let config = parse_config_json(r#"{"num_shards": 65535}"#);
        assert!(config.is_some(), "num_shards at u16::MAX should be valid");
    }

    #[test]
    fn test_parse_config_invalid_json() {
        let config = parse_config_json(r#"{"num_shards": invalid}"#);
        assert!(config.is_none());
    }

    #[test]
    fn test_parse_config_empty() {
        let config = parse_config_json(r#"{}"#);
        assert!(config.is_some(), "empty config should use defaults");
    }

    /// Pin: known `backpressure_mode` strings round-trip; an
    /// unknown value (typo) is rejected with `None`, not silently
    /// downgraded to `DropNewest`. Pre-fix a deploy-time typo
    /// like `"DropOldset"` swapped the operator's intended
    /// durability for `DropNewest` with no diagnostic.
    #[test]
    fn parse_config_rejects_unknown_backpressure_mode() {
        // Known values still parse.
        for s in [
            "DropNewest",
            "drop_newest",
            "DropOldest",
            "drop_oldest",
            "FailProducer",
            "fail_producer",
        ] {
            let cfg = parse_config_json(&format!(r#"{{"backpressure_mode": "{}"}}"#, s));
            assert!(cfg.is_some(), "known mode `{}` must parse", s);
        }

        // Typos must fail.
        for s in ["DropOldset", "FailProduce", "drop_oldst", "garbage", ""] {
            let cfg = parse_config_json(&format!(r#"{{"backpressure_mode": "{}"}}"#, s));
            assert!(
                cfg.is_none(),
                "unknown mode `{}` must reject (pre-fix this silently \
                 fell through to DropNewest)",
                s,
            );
        }

        // Wrong JSON type also fails — pre-fix this hit the
        // `and_then(|v| v.as_str())` short-circuit and was
        // ignored entirely.
        let cfg = parse_config_json(r#"{"backpressure_mode": 42}"#);
        assert!(
            cfg.is_none(),
            "non-string non-object backpressure_mode must reject"
        );
        let cfg = parse_config_json(r#"{"backpressure_mode": true}"#);
        assert!(cfg.is_none(), "boolean backpressure_mode must reject");
    }

    /// Pin: the `Sample { rate }` mode is reachable from JSON
    /// via `{"backpressure_mode": {"Sample": {"rate": N}}}`,
    /// and a zero rate is rejected (validator already rejects
    /// it; the parser must too, so the surface is consistent).
    #[test]
    fn parse_config_supports_sample_mode_with_validation() {
        let cfg = parse_config_json(r#"{"backpressure_mode": {"Sample": {"rate": 10}}}"#);
        assert!(cfg.is_some(), "Sample with non-zero rate must parse");

        let cfg = parse_config_json(r#"{"backpressure_mode": {"Sample": {"rate": 0}}}"#);
        assert!(cfg.is_none(), "Sample with rate=0 must reject");

        let cfg = parse_config_json(r#"{"backpressure_mode": {"Sample": {}}}"#);
        assert!(cfg.is_none(), "Sample missing rate must reject");
    }

    // Regression: the Go binding's `Poll(limit, cursor)` serializes a
    // `"cursor"` field that the FFI JSON path previously ignored —
    // cross-shard pagination silently broke. `parse_poll_request_json`
    // must round-trip the cursor into `ConsumeRequest.from_id`.
    #[test]
    fn test_parse_poll_request_preserves_cursor() {
        let req = parse_poll_request_json(r#"{"limit": 50, "cursor": "abc:123"}"#).unwrap();
        assert_eq!(req.limit, 50);
        assert_eq!(req.from_id.as_deref(), Some("abc:123"));
    }

    #[test]
    fn test_parse_poll_request_no_cursor_defaults_to_none() {
        let req = parse_poll_request_json(r#"{"limit": 10}"#).unwrap();
        assert_eq!(req.limit, 10);
        assert_eq!(req.from_id, None);
    }

    #[test]
    fn test_parse_poll_request_empty_uses_default_limit() {
        let req = parse_poll_request_json(r#"{}"#).unwrap();
        assert_eq!(req.limit, 100);
        assert_eq!(req.from_id, None);
    }

    // Regression: a wrong-typed `"limit"` previously hit
    // `.as_u64().unwrap_or(100)` and silently defaulted. Caller bugs
    // (e.g. sending a string or a negative number) must surface as
    // `InvalidJson` instead.
    #[test]
    fn test_parse_poll_request_wrong_type_limit_errors() {
        let err = parse_poll_request_json(r#"{"limit": "50"}"#).unwrap_err();
        assert_eq!(err, c_int::from(NetError::InvalidJson));
        let err = parse_poll_request_json(r#"{"limit": -1}"#).unwrap_err();
        assert_eq!(err, c_int::from(NetError::InvalidJson));
    }

    #[test]
    fn test_parse_poll_request_wrong_type_cursor_errors() {
        let err = parse_poll_request_json(r#"{"cursor": 123}"#).unwrap_err();
        assert_eq!(err, c_int::from(NetError::InvalidJson));
    }

    #[test]
    fn test_parse_poll_request_null_fields_use_defaults() {
        let req = parse_poll_request_json(r#"{"limit": null, "cursor": null}"#).unwrap();
        assert_eq!(req.limit, 100);
        assert_eq!(req.from_id, None);
    }

    /// `usize::MAX` is always a valid usize regardless of target
    /// pointer width, so it must parse successfully on both 32- and
    /// 64-bit builds. This pins the boundary case.
    #[test]
    fn test_parse_poll_request_limit_at_usize_max() {
        let json = format!(r#"{{"limit": {}}}"#, usize::MAX);
        let req = parse_poll_request_json(&json).unwrap();
        assert_eq!(req.limit, usize::MAX);
    }

    /// Regression: `as usize` silently truncates on 32-bit targets
    /// for `u64` values above `usize::MAX`. The parser must return
    /// `InvalidJson` instead of wrapping. We only run this on 32-bit
    /// targets because on 64-bit `usize::MAX == u64::MAX`, leaving
    /// nothing that fits in u64 but not usize.
    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_parse_poll_request_limit_overflows_usize_on_32bit() {
        // 2^33 — fits in u64, but exceeds usize::MAX on a 32-bit build.
        let err = parse_poll_request_json(r#"{"limit": 8589934592}"#).unwrap_err();
        assert_eq!(err, c_int::from(NetError::InvalidJson));
    }

    /// CR-22: pin parity between the Rust-side `NetError` enum and
    /// the two C-header copies. The Rust enum is the source of
    /// truth; C / Go consumers `errors.Is` against the named codes.
    /// Pre-CR-22 the headers were missing `-9` (IntOverflow) and
    /// `-10` (MismatchedHandles); a consumer receiving those values
    /// would fall into the unknown-code branch and lose actionable
    /// distinction.
    ///
    /// We extract every integer literal that appears as the
    /// right-hand side of an `= ` token in the file and check
    /// that each Rust-side value is present in BOTH headers. The
    /// test does NOT verify symbolic names; the sealing
    /// constraint is the numeric value alone.
    ///
    /// Both `include_str!` paths point inside `net/crates/net/`.
    /// `include/net.go.h` is a manually-synced mirror of the
    /// repo-root `go/net.h`. Reaching outside the crate root
    /// (`include_str!("../../../../../go/net.h")`) breaks
    /// `cargo publish` and any out-of-repo vendoring of this
    /// crate, so the in-crate copy is the supported source. A
    /// drift between the two surfaces here as a parity-test
    /// failure: one of them will be missing the new value.
    #[test]
    fn cr22_c_header_parity_with_rust_neterror() {
        let primary = include_str!("../../include/net.h");
        let go_copy = include_str!("../../include/net.go.h");

        // The Rust enum's full set of values (mirrors `pub enum
        // NetError` above). When a new variant is added in the
        // Rust source, this list — AND both headers — must be
        // updated together. The asserts that follow then catch a
        // missing header update at the next CI run.
        let rust_values: &[i32] = &[0, -1, -2, -3, -4, -5, -6, -7, -8, -9, -10, -11, -99];

        // Pull every numeric literal that looks like an enum-value
        // assignment (`= <number>` followed by `,` or whitespace).
        // Whitespace-tolerant: skips `= 0`, `=  0`, `= -10`, etc.
        fn extract_assigned_values(src: &str) -> Vec<i32> {
            let mut out = Vec::new();
            let mut chars = src.chars().peekable();
            while let Some(c) = chars.next() {
                if c != '=' {
                    continue;
                }
                // Skip whitespace.
                while let Some(&peek) = chars.peek() {
                    if peek == ' ' || peek == '\t' {
                        chars.next();
                    } else {
                        break;
                    }
                }
                // Optional sign.
                let mut buf = String::new();
                if let Some(&peek) = chars.peek() {
                    if peek == '-' || peek == '+' {
                        buf.push(peek);
                        chars.next();
                    }
                }
                // Digits.
                let mut had_digit = false;
                while let Some(&peek) = chars.peek() {
                    if peek.is_ascii_digit() {
                        buf.push(peek);
                        chars.next();
                        had_digit = true;
                    } else {
                        break;
                    }
                }
                if had_digit {
                    if let Ok(v) = buf.parse::<i32>() {
                        out.push(v);
                    }
                }
            }
            out
        }

        let primary_vals = extract_assigned_values(primary);
        let go_vals = extract_assigned_values(go_copy);

        for &v in rust_values {
            assert!(
                primary_vals.contains(&v),
                "CR-22 regression: include/net.h is missing the value {} \
                 (Rust NetError defines it). Add the matching `NET_ERR_*` \
                 enumerator before merging.",
                v
            );
            assert!(
                go_vals.contains(&v),
                "CR-22 regression: bindings/go/net/net.h is missing the value {} \
                 (Rust NetError defines it).",
                v
            );
        }
    }

    /// `handle_is_valid` rejects null and any pointer not aligned for
    /// `NetHandle`. A foreign caller producing a misaligned pointer
    /// (e.g. via an over-eager `void *` cast on a packed struct) hits
    /// `&*handle` UB before any other check fires; this gate is the
    /// pre-deref discriminator.
    #[test]
    fn handle_is_valid_rejects_null_and_misaligned() {
        // Null is rejected.
        assert!(
            !handle_is_valid(std::ptr::null::<NetHandle>()),
            "null pointer must not be considered a valid handle"
        );

        // Aligned but non-null is accepted (we use a small backing
        // buffer to materialize a pointer without dereferencing it).
        // `align_of::<NetHandle>()` is the alignment we must match.
        let align = std::mem::align_of::<NetHandle>();
        let buf = vec![0u8; align * 2];
        let base = buf.as_ptr() as usize;
        let aligned = (base + align - 1) & !(align - 1);
        let aligned_ptr = aligned as *const NetHandle;
        assert!(
            handle_is_valid(aligned_ptr),
            "aligned non-null pointer must validate (align={align}, ptr={aligned_ptr:p})"
        );

        // A pointer one byte past `aligned_ptr` is misaligned for any
        // type with align > 1, and `NetHandle` (containing `AtomicU32`,
        // `AtomicBool`, ManuallyDrop'd EventBus + Runtime) easily
        // exceeds 1.
        if align > 1 {
            let misaligned_ptr = (aligned + 1) as *const NetHandle;
            assert!(
                !handle_is_valid(misaligned_ptr),
                "misaligned pointer must be rejected (align={align}, ptr={misaligned_ptr:p})"
            );
        }
    }

    /// Pin: zero values for `heartbeat_interval_ms` and
    /// `session_timeout_ms` must reject the entire config (parser
    /// returns `None`). Pre-fix the parser threaded `0` through
    /// to `Duration::from_millis(0)`, which on the Net adapter's
    /// heartbeat path results in a busy-loop that pegs a CPU and
    /// produces no diagnostic — the FFI caller saw a successful
    /// `net_init` followed by a hung daemon. The validator-level
    /// guard for cooldown / metrics_window has no equivalent on
    /// the Net-adapter side, so the parser is the only place that
    /// can refuse the build.
    #[cfg(feature = "net")]
    #[test]
    fn parse_config_rejects_zero_heartbeat_and_session_timeout() {
        // 32-byte hex strings (64 chars) so `hex::decode` produces
        // exactly the [u8; 32] the parser requires for `psk` and
        // `peer_public_key`.
        let psk = "0".repeat(64);
        let peer_pk = "1".repeat(64);

        // Sanity: a config with both fields *non-zero* must parse
        // successfully — proves the rejection in the negative
        // cases below is caused by the zero, not a missing
        // required field on the surrounding `net` block.
        let baseline = format!(
            r#"{{"net":{{"bind_addr":"127.0.0.1:9000","peer_addr":"127.0.0.1:9001",
                "psk":"{psk}","peer_public_key":"{peer_pk}",
                "heartbeat_interval_ms":1000,"session_timeout_ms":30000}}}}"#
        );
        assert!(
            parse_config_json(&baseline).is_some(),
            "baseline net config with non-zero heartbeat/session_timeout must parse"
        );

        // heartbeat_interval_ms = 0 → reject.
        let zero_hb = format!(
            r#"{{"net":{{"bind_addr":"127.0.0.1:9000","peer_addr":"127.0.0.1:9001",
                "psk":"{psk}","peer_public_key":"{peer_pk}",
                "heartbeat_interval_ms":0,"session_timeout_ms":30000}}}}"#
        );
        assert!(
            parse_config_json(&zero_hb).is_none(),
            "heartbeat_interval_ms=0 must reject (pre-fix this produced a CPU-pegging busy loop)"
        );

        // session_timeout_ms = 0 → reject.
        let zero_to = format!(
            r#"{{"net":{{"bind_addr":"127.0.0.1:9000","peer_addr":"127.0.0.1:9001",
                "psk":"{psk}","peer_public_key":"{peer_pk}",
                "heartbeat_interval_ms":1000,"session_timeout_ms":0}}}}"#
        );
        assert!(
            parse_config_json(&zero_to).is_none(),
            "session_timeout_ms=0 must reject"
        );
    }
}
