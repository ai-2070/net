//! C ABI for capability sensing (SENSING_OLB_COMPLETION_PLAN B2). It rides
//! `libnet_org` because that crate already depends on `net-sdk`, and adding a
//! second cdylib is forbidden (the single-cdylib rule).
//!
//! Every behaviour is the Rust SDK's (`net_sdk::sensing`), forwarded, exactly
//! as the Node and Python bindings forward it:
//!
//! - **Incarnation.** [`net_sensing_next_incarnation`] reads, increments and
//!   durably persists the boot counter through the SDK's `FileIncarnation`.
//!   The caller passes the result as the mesh config's `sensing_incarnation`.
//!   Persistence stays in Rust.
//! - **Provider.** [`net_sensing_provide`] is the SDK's value-backed provider.
//!   The caller sets a value; Rust's evaluator reads it on the attestation
//!   path, so no callback crosses the boundary per beat.
//! - **Consumer.** [`net_sensing_watch_changed`] blocks the calling thread with
//!   an optional timeout, which returns [`NET_SENSING_TIMEOUT`] and consumes no
//!   edge. [`net_sensing_watch_close`] may run on ANOTHER thread while a
//!   `changed` is blocked, and that wait returns [`NET_SENSING_ERR_SENSING`]
//!   with the `sensing:watch_closed:` wire (B1-pre).
//! - **Errors.** A sensing refusal is [`NET_SENSING_ERR_SENSING`] with the SDK's
//!   `sensing:<kind>: <message>` wire string in `out_err`.
//!   [`net_sensing_error_class`] returns Rust's kind and terminal flag for it,
//!   so no caller keeps a classification table of its own (C5).
//!
//! # Ownership
//!
//! Handles are `Box`ed and freed through their `_free` with a DOUBLE pointer
//! that is NULLed, as everywhere else in this crate. A watch must not be freed
//! while a `changed` on it is still running; close it first, which ends the
//! wait. Strings written to `out_err`, `out_kind` and `out_json` are freed with
//! `net_org_free_cstring`, and response bodies with `net_org_response_free`.

use std::os::raw::{c_char, c_int};
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::mesh_rpc::CallOptions;
use net::adapter::net::MeshNode;
use net_sdk::sensing::{self as sdk, ReadinessValue, SensedViability, SensingError, SensingQuery};

use super::{block_on, cstr_to_string, write_err, write_response};

/// Operation succeeded (`changed`: something moved).
pub const NET_SENSING_OK: c_int = 0;
/// A required pointer was NULL.
pub const NET_SENSING_ERR_NULL: c_int = -1;
/// A string argument was not UTF-8.
pub const NET_SENSING_ERR_INVALID_UTF8: c_int = -2;
/// A sensing refusal; `out_err` carries the `sensing:<kind>: <message>` wire.
pub const NET_SENSING_ERR_SENSING: c_int = -3;
/// `net_sensing_watch_changed` timed out. Not an error, and no edge was lost.
pub const NET_SENSING_TIMEOUT: c_int = -4;
/// The sensed service call failed; `out_err` carries the nRPC error text.
pub const NET_SENSING_ERR_RPC: c_int = -5;
/// The incarnation counter could not be read or durably written.
pub const NET_SENSING_ERR_INCARNATION: c_int = -6;
/// An argument was out of range (an unknown state, a reason over 16 bits).
pub const NET_SENSING_ERR_INVALID_ARG: c_int = -7;

/// `state`: ready (optionally with an estimated start).
pub const NET_SENSING_STATE_READY: c_int = 0;
/// `state`: not ready, with a provider-defined reason code.
pub const NET_SENSING_STATE_NOT_READY: c_int = 1;
/// `state`: the provider cannot currently say (read as Unknown).
pub const NET_SENSING_STATE_UNEVALUABLE: c_int = 2;

/// The ABI version this build implements, pinned in `include/net_sensing.h`
/// as `NET_SENSING_ABI_VERSION`. Bumped on any incompatible change to a
/// `net_sensing_*` signature or constant.
pub const NET_SENSING_ABI_VERSION: u32 = 1;

/// ABI version of the sensing surface. A C consumer compares it with the
/// header's `NET_SENSING_ABI_VERSION` at init and refuses a mismatched
/// library.
#[unsafe(no_mangle)]
pub extern "C" fn net_sensing_abi_version() -> u32 {
    NET_SENSING_ABI_VERSION
}

/// An owning handle on one value-backed readiness registration.
pub struct NetSensingProvider {
    inner: sdk::ValueReadiness,
}

/// An owning observation of one capability.
pub struct NetSensingWatch {
    inner: Arc<tokio::sync::Mutex<sdk::SensingWatch>>,
    closer: sdk::SensingWatchCloser,
}

/// One immutable readiness capture.
pub struct NetSensingSnapshot {
    inner: sdk::SensingSnapshot,
}

fn optional_ms(value: i64) -> Option<Duration> {
    (value >= 0).then(|| Duration::from_millis(value as u64))
}

fn readiness_value(state: c_int, estimated_start_ms: i64, reason: u32) -> Option<ReadinessValue> {
    match state {
        NET_SENSING_STATE_READY => Some(ReadinessValue::Ready {
            estimated_start: optional_ms(estimated_start_ms),
        }),
        NET_SENSING_STATE_NOT_READY => u16::try_from(reason)
            .ok()
            .map(|reason| ReadinessValue::NotReady { reason }),
        NET_SENSING_STATE_UNEVALUABLE => Some(ReadinessValue::Unevaluable),
        _ => None,
    }
}

fn sensing_refusal(error: SensingError, out_err: *mut *mut c_char) -> c_int {
    write_err(out_err, error.to_wire());
    NET_SENSING_ERR_SENSING
}

fn client_for(node: Arc<MeshNode>) -> Result<sdk::SensingClient, SensingError> {
    net_sdk::mesh::Mesh::from_node_arc(
        node,
        Arc::new(net::adapter::net::ChannelConfigRegistry::new()),
        None,
    )
    .sensing()
}

/// Read, increment and durably persist the sensing boot counter at `path`,
/// writing the new incarnation to `*out_incarnation`. Pass that value as the
/// mesh config's `sensing_incarnation`. A fresh install starts at 1; a corrupt
/// or unwritable counter fails rather than restarting at 1.
#[unsafe(no_mangle)]
pub extern "C" fn net_sensing_next_incarnation(
    path_ptr: *const c_char,
    path_len: usize,
    out_incarnation: *mut u64,
    out_err: *mut *mut c_char,
) -> c_int {
    ffi_guard!(NET_SENSING_ERR_NULL, {
        if out_incarnation.is_null() {
            return NET_SENSING_ERR_NULL;
        }
        let Some(path) = cstr_to_string(path_ptr, path_len) else {
            return NET_SENSING_ERR_INVALID_UTF8;
        };
        match sdk::next_incarnation(&mut sdk::FileIncarnation::new(&path)) {
            Ok(incarnation) => {
                unsafe { *out_incarnation = incarnation.get() };
                NET_SENSING_OK
            }
            Err(error) => {
                write_err(out_err, format!("sensing incarnation at {path:?}: {error}"));
                NET_SENSING_ERR_INCARNATION
            }
        }
    })
}

/// Rust's classification of a sensing wire string. On success writes the kind
/// to `*out_kind` (free with `net_org_free_cstring`) and 1/0 to
/// `*out_terminal`. Returns [`NET_SENSING_ERR_INVALID_ARG`] when `wire` is not
/// a sensing wire string this build knows.
#[unsafe(no_mangle)]
pub extern "C" fn net_sensing_error_class(
    wire_ptr: *const c_char,
    wire_len: usize,
    out_kind: *mut *mut c_char,
    out_terminal: *mut c_int,
) -> c_int {
    ffi_guard!(NET_SENSING_ERR_NULL, {
        if out_kind.is_null() || out_terminal.is_null() {
            return NET_SENSING_ERR_NULL;
        }
        let Some(wire) = cstr_to_string(wire_ptr, wire_len) else {
            return NET_SENSING_ERR_INVALID_UTF8;
        };
        match sdk::classify_sensing_wire(&wire) {
            Some(class) => {
                write_err(out_kind, class.kind.to_string());
                unsafe { *out_terminal = c_int::from(class.terminal) };
                NET_SENSING_OK
            }
            None => NET_SENSING_ERR_INVALID_ARG,
        }
    })
}

/// Serve readiness for `capability` from a stored value. `mesh_arc` (from
/// `net_mesh_arc_clone`) is CONSUMED on every path. `estimated_start_ms < 0`
/// means no estimate; `reason` applies to `NET_SENSING_STATE_NOT_READY`.
#[unsafe(no_mangle)]
pub extern "C" fn net_sensing_provide(
    mesh_arc: *mut Arc<MeshNode>,
    capability_ptr: *const c_char,
    capability_len: usize,
    state: c_int,
    estimated_start_ms: i64,
    reason: u32,
    out_provider: *mut *mut NetSensingProvider,
    out_err: *mut *mut c_char,
) -> c_int {
    ffi_guard!(NET_SENSING_ERR_NULL, {
        if mesh_arc.is_null() || out_provider.is_null() {
            return NET_SENSING_ERR_NULL;
        }
        let node: Arc<MeshNode> = unsafe { *Box::from_raw(mesh_arc) };
        let Some(capability) = cstr_to_string(capability_ptr, capability_len) else {
            return NET_SENSING_ERR_INVALID_UTF8;
        };
        let Some(value) = readiness_value(state, estimated_start_ms, reason) else {
            return NET_SENSING_ERR_INVALID_ARG;
        };
        let _enter = super::runtime().enter();
        match client_for(node).and_then(|client| client.provide_value(capability, value)) {
            Ok(inner) => {
                unsafe {
                    *out_provider = Box::into_raw(Box::new(NetSensingProvider { inner }));
                }
                NET_SENSING_OK
            }
            Err(error) => sensing_refusal(error, out_err),
        }
    })
}

/// Publish a new value, then wake every live observation path. Writes 1 to
/// `*out_moved` (if non-NULL) when an observation moved. A set after close
/// stores nothing.
#[unsafe(no_mangle)]
pub extern "C" fn net_sensing_provider_set(
    provider: *const NetSensingProvider,
    state: c_int,
    estimated_start_ms: i64,
    reason: u32,
    out_moved: *mut c_int,
) -> c_int {
    ffi_guard!(NET_SENSING_ERR_NULL, {
        let Some(provider) = (unsafe { provider.as_ref() }) else {
            return NET_SENSING_ERR_NULL;
        };
        let Some(value) = readiness_value(state, estimated_start_ms, reason) else {
            return NET_SENSING_ERR_INVALID_ARG;
        };
        let moved = provider.inner.set(value);
        if !out_moved.is_null() {
            unsafe { *out_moved = c_int::from(moved) };
        }
        NET_SENSING_OK
    })
}

/// Stop serving readiness. Returns 1 only for the call that removed it.
#[unsafe(no_mangle)]
pub extern "C" fn net_sensing_provider_close(provider: *const NetSensingProvider) -> c_int {
    ffi_guard!(0, {
        match unsafe { provider.as_ref() } {
            Some(provider) => c_int::from(provider.inner.close()),
            None => 0,
        }
    })
}

/// Free a provider handle (closing it) and NULL `*provider`. Idempotent on
/// NULL.
#[unsafe(no_mangle)]
pub extern "C" fn net_sensing_provider_free(provider: *mut *mut NetSensingProvider) {
    ffi_guard!((), {
        if provider.is_null() {
            return;
        }
        let ptr = unsafe { *provider };
        if !ptr.is_null() {
            drop(unsafe { Box::from_raw(ptr) });
            unsafe { *provider = std::ptr::null_mut() };
        }
    })
}

/// Observe the readiness of this node's authorized own-organization providers
/// of `capability`. `mesh_arc` is CONSUMED. `within_ms` / `start_within_ms`
/// below 0 mean "not set".
#[unsafe(no_mangle)]
pub extern "C" fn net_sensing_watch(
    mesh_arc: *mut Arc<MeshNode>,
    capability_ptr: *const c_char,
    capability_len: usize,
    within_ms: i64,
    start_within_ms: i64,
    out_watch: *mut *mut NetSensingWatch,
    out_err: *mut *mut c_char,
) -> c_int {
    ffi_guard!(NET_SENSING_ERR_NULL, {
        if mesh_arc.is_null() || out_watch.is_null() {
            return NET_SENSING_ERR_NULL;
        }
        let node: Arc<MeshNode> = unsafe { *Box::from_raw(mesh_arc) };
        let Some(capability) = cstr_to_string(capability_ptr, capability_len) else {
            return NET_SENSING_ERR_INVALID_UTF8;
        };
        let mut query = SensingQuery::new(capability);
        if let Some(within) = optional_ms(within_ms) {
            query = query.within(within);
        }
        if let Some(start) = optional_ms(start_within_ms) {
            query = query.start_within(start);
        }
        let _enter = super::runtime().enter();
        match client_for(node).and_then(|client| client.watch(query)) {
            Ok(watch) => {
                let closer = watch.closer();
                unsafe {
                    *out_watch = Box::into_raw(Box::new(NetSensingWatch {
                        inner: Arc::new(tokio::sync::Mutex::new(watch)),
                        closer,
                    }));
                }
                NET_SENSING_OK
            }
            Err(error) => sensing_refusal(error, out_err),
        }
    })
}

/// Take a snapshot into a new `*out_snapshot` handle (free with
/// [`net_sensing_snapshot_free`]).
#[unsafe(no_mangle)]
pub extern "C" fn net_sensing_watch_snapshot(
    watch: *const NetSensingWatch,
    out_snapshot: *mut *mut NetSensingSnapshot,
    out_err: *mut *mut c_char,
) -> c_int {
    ffi_guard!(NET_SENSING_ERR_NULL, {
        let Some(watch) = (unsafe { watch.as_ref() }) else {
            return NET_SENSING_ERR_NULL;
        };
        if out_snapshot.is_null() {
            return NET_SENSING_ERR_NULL;
        }
        if watch.closer.is_closed() {
            return sensing_refusal(SensingError::WatchClosed, out_err);
        }
        let inner = watch.inner.clone();
        match block_on(async move { inner.lock().await.snapshot() }) {
            Ok(inner) => {
                unsafe {
                    *out_snapshot = Box::into_raw(Box::new(NetSensingSnapshot { inner }));
                }
                NET_SENSING_OK
            }
            Err(error) => sensing_refusal(error, out_err),
        }
    })
}

/// Block until something that can change the answer moved
/// ([`NET_SENSING_OK`]), `timeout_ms` elapsed ([`NET_SENSING_TIMEOUT`]; no edge
/// is consumed; `timeout_ms < 0` waits without a bound), or the watch closed
/// ([`NET_SENSING_ERR_SENSING`], `sensing:watch_closed:`). The close may run on
/// another thread while this call is blocked.
#[unsafe(no_mangle)]
pub extern "C" fn net_sensing_watch_changed(
    watch: *const NetSensingWatch,
    timeout_ms: i64,
    out_err: *mut *mut c_char,
) -> c_int {
    ffi_guard!(NET_SENSING_ERR_NULL, {
        let Some(watch) = (unsafe { watch.as_ref() }) else {
            return NET_SENSING_ERR_NULL;
        };
        if watch.closer.is_closed() {
            return sensing_refusal(SensingError::WatchClosed, out_err);
        }
        let inner = watch.inner.clone();
        let wait = async move {
            let mut guard = inner.lock().await;
            guard.changed().await
        };
        let outcome = block_on(async move {
            match optional_ms(timeout_ms) {
                Some(limit) => tokio::time::timeout(limit, wait).await.ok(),
                None => Some(wait.await),
            }
        });
        match outcome {
            None => NET_SENSING_TIMEOUT,
            Some(Ok(())) => NET_SENSING_OK,
            Some(Err(error)) => sensing_refusal(error, out_err),
        }
    })
}

/// Close the watch from any thread; ends a blocked `changed`. Returns 1 only
/// for the call that closed it.
#[unsafe(no_mangle)]
pub extern "C" fn net_sensing_watch_close(watch: *const NetSensingWatch) -> c_int {
    ffi_guard!(0, {
        match unsafe { watch.as_ref() } {
            Some(watch) => c_int::from(watch.closer.close()),
            None => 0,
        }
    })
}

/// Free a watch handle (closing it) and NULL `*watch`. Idempotent on NULL.
/// Must not run concurrently with a `changed` or `snapshot` on the same
/// handle: close first.
#[unsafe(no_mangle)]
pub extern "C" fn net_sensing_watch_free(watch: *mut *mut NetSensingWatch) {
    ffi_guard!((), {
        if watch.is_null() {
            return;
        }
        let ptr = unsafe { *watch };
        if !ptr.is_null() {
            let boxed = unsafe { Box::from_raw(ptr) };
            boxed.closer.close();
            drop(boxed);
            unsafe { *watch = std::ptr::null_mut() };
        }
    })
}

/// Render a snapshot as JSON into `*out_json` (free with
/// `net_org_free_cstring`):
///
/// ```json
/// {"capability": "...", "ranked": [<node_id>...], "preferred": <node_id>|null,
///  "providers": [{"node_id": ..., "readiness": "ready|not_ready|unknown",
///                 "viability": "viable|potential|not_viable",
///                 "estimated_start_ms": <f64>|null, "route_estimate_ms": <f64>|null}]}
/// ```
#[unsafe(no_mangle)]
pub extern "C" fn net_sensing_snapshot_json(
    snapshot: *const NetSensingSnapshot,
    out_json: *mut *mut c_char,
) -> c_int {
    ffi_guard!(NET_SENSING_ERR_NULL, {
        let Some(snapshot) = (unsafe { snapshot.as_ref() }) else {
            return NET_SENSING_ERR_NULL;
        };
        if out_json.is_null() {
            return NET_SENSING_ERR_NULL;
        }
        let ms = |d: Option<Duration>| d.map(|d| d.as_secs_f64() * 1000.0);
        let providers: Vec<serde_json::Value> = snapshot
            .inner
            .providers()
            .iter()
            .map(|row| {
                serde_json::json!({
                    "node_id": row.node_id(),
                    "readiness": match row.readiness() {
                        sdk::ProjectedReadiness::Ready => "ready",
                        sdk::ProjectedReadiness::NotReady => "not_ready",
                        sdk::ProjectedReadiness::Unknown => "unknown",
                    },
                    "viability": match row.viability() {
                        SensedViability::Viable => "viable",
                        SensedViability::Potential => "potential",
                        SensedViability::NotViable => "not_viable",
                    },
                    "estimated_start_ms": ms(row.estimated_start()),
                    "route_estimate_ms": ms(row.route_estimate()),
                })
            })
            .collect();
        let doc = serde_json::json!({
            "capability": snapshot.inner.capability(),
            "providers": providers,
            "ranked": snapshot.inner.ranked(),
            "preferred": snapshot.inner.preferred(),
        });
        write_err(out_json, doc.to_string());
        NET_SENSING_OK
    })
}

/// Free a snapshot handle and NULL `*snapshot`. Idempotent on NULL.
#[unsafe(no_mangle)]
pub extern "C" fn net_sensing_snapshot_free(snapshot: *mut *mut NetSensingSnapshot) {
    ffi_guard!((), {
        if snapshot.is_null() {
            return;
        }
        let ptr = unsafe { *snapshot };
        if !ptr.is_null() {
            drop(unsafe { Box::from_raw(ptr) });
            unsafe { *snapshot = std::ptr::null_mut() };
        }
    })
}

/// The SDK's sensed service call (S2): `call_service` over the candidates
/// authorized exactly as an unsensed call authorizes them, with the target
/// chosen from `snapshot` (a capture of `nrpc:<service>`; any other capability
/// is no evidence). The target is invoked once and never retried.
/// `mesh_arc` is CONSUMED. `timeout_ms == 0` means no deadline, exactly as
/// `net_rpc_call_service`. A timeout the platform clock cannot represent is
/// [`NET_SENSING_ERR_INVALID_ARG`], never a panic.
/// On success writes the body to `*out_resp`/`*out_len` (free with
/// `net_org_response_free`).
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn net_sensing_call_service(
    mesh_arc: *mut Arc<MeshNode>,
    service_ptr: *const c_char,
    service_len: usize,
    req_ptr: *const u8,
    req_len: usize,
    snapshot: *const NetSensingSnapshot,
    timeout_ms: u64,
    out_resp: *mut *mut u8,
    out_len: *mut usize,
    out_err: *mut *mut c_char,
) -> c_int {
    ffi_guard!(NET_SENSING_ERR_NULL, {
        if mesh_arc.is_null() || out_resp.is_null() || out_len.is_null() {
            return NET_SENSING_ERR_NULL;
        }
        let node: Arc<MeshNode> = unsafe { *Box::from_raw(mesh_arc) };
        let Some(snapshot) = (unsafe { snapshot.as_ref() }) else {
            return NET_SENSING_ERR_NULL;
        };
        let Some(service) = cstr_to_string(service_ptr, service_len) else {
            return NET_SENSING_ERR_INVALID_UTF8;
        };
        let Some(body) = (unsafe { super::copy_body(req_ptr, req_len) }) else {
            return NET_SENSING_ERR_NULL;
        };
        let mut opts = CallOptions::default();
        if timeout_ms > 0 {
            // Checked: an unchecked `Instant + Duration` panics on overflow,
            // which the guard would report as the unrelated NULL code.
            match std::time::Instant::now().checked_add(Duration::from_millis(timeout_ms)) {
                Some(deadline) => opts.deadline = Some(deadline),
                None => {
                    write_err(out_err, format!("timeout_ms {timeout_ms} is out of range"));
                    return NET_SENSING_ERR_INVALID_ARG;
                }
            }
        }
        let readiness = snapshot.inner.clone();
        let outcome = block_on(async move {
            net_sdk::mesh_rpc::call_service_sensed_on_node(&node, &service, body, opts, &readiness)
                .await
        });
        match outcome {
            Ok(reply) => {
                write_response(reply.body.to_vec(), out_resp, out_len);
                NET_SENSING_OK
            }
            Err(error) => {
                write_err(out_err, error.to_string());
                NET_SENSING_ERR_RPC
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every fixture vector classifies through the C entry point exactly as
    /// Rust classifies it (C5 at the C boundary).
    #[test]
    fn the_c_classifier_reproduces_every_fixture_vector() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../tests/cross_lang_sensing/error_vectors.json");
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("json");
        for v in doc["vectors"].as_array().expect("vectors") {
            let wire = v["wire"].as_str().unwrap();
            let mut kind: *mut c_char = std::ptr::null_mut();
            let mut terminal: c_int = -1;
            let code = net_sensing_error_class(
                wire.as_ptr() as *const c_char,
                wire.len(),
                &mut kind,
                &mut terminal,
            );
            assert_eq!(code, NET_SENSING_OK, "{wire}");
            let kind_str = unsafe { std::ffi::CStr::from_ptr(kind) }
                .to_str()
                .unwrap()
                .to_owned();
            super::super::net_org_free_cstring(kind);
            assert_eq!(kind_str, v["kind"].as_str().unwrap());
            assert_eq!(terminal != 0, v["terminal"].as_bool().unwrap());
        }
        let mut kind: *mut c_char = std::ptr::null_mut();
        let mut terminal: c_int = 0;
        let bogus = "org:rpc:timeout: x";
        assert_eq!(
            net_sensing_error_class(
                bogus.as_ptr() as *const c_char,
                bogus.len(),
                &mut kind,
                &mut terminal
            ),
            NET_SENSING_ERR_INVALID_ARG
        );
    }

    /// The incarnation entry point persists before answering, and is
    /// monotonic across calls on one file.
    #[test]
    fn next_incarnation_is_persisted_and_monotonic() {
        let dir = std::env::temp_dir().join(format!(
            "net-sensing-ffi-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("incarnation").to_string_lossy().into_owned();
        let mut first = 0u64;
        let mut second = 0u64;
        let mut err: *mut c_char = std::ptr::null_mut();
        assert_eq!(
            net_sensing_next_incarnation(
                path.as_ptr() as *const c_char,
                path.len(),
                &mut first,
                &mut err
            ),
            NET_SENSING_OK
        );
        assert_eq!(
            net_sensing_next_incarnation(
                path.as_ptr() as *const c_char,
                path.len(),
                &mut second,
                &mut err
            ),
            NET_SENSING_OK
        );
        assert_eq!((first, second), (1, 2));
        assert_eq!(
            std::fs::read_to_string(dir.join("incarnation"))
                .unwrap()
                .trim(),
            "2"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
