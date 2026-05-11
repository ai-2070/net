//! C FFI bindings for CortEX + RedEX, behind the `cortex` feature.
//!
//! Surface targeted at the Go SDK: NetDb, TasksAdapter, MemoriesAdapter,
//! and raw RedexFile access. Everything crosses the boundary as:
//!
//! - Opaque handles (`*mut T`) freed via dedicated `_free` functions.
//! - Scalar IDs / timestamps as `u64`.
//! - Everything else as JSON strings allocated with `CString::into_raw`,
//!   freed by the caller via [`crate::ffi::net_free_string`].
//!
//! Watch / tail iterators use a cursor pattern:
//! `net_*_next(cursor, timeout_ms, out_json, out_len) -> c_int` returns
//! `0 = event delivered`, `1 = timeout`, `2 = stream ended cleanly`,
//! or a negative `NetError`. The Go layer wraps the cursor in a
//! goroutine that pumps into a channel, calling `close` on `ctx.Done()`.

use std::ffi::{c_char, c_int, CStr, CString};
use std::mem::ManuallyDrop;
use std::os::raw::c_void;
use std::ptr;
use std::sync::Arc;

use futures::stream::BoxStream;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::runtime::Runtime;
use tokio::sync::Mutex as TokioMutex;

use crate::adapter::net::channel::ChannelName;
use crate::adapter::net::cortex::memories::{
    MemoriesAdapter as InnerMemoriesAdapter, MemoriesFilter, MemoriesWatcher, Memory,
    OrderBy as MemoriesOrderBy,
};
use crate::adapter::net::cortex::tasks::{
    OrderBy as TasksOrderBy, Task, TaskStatus, TasksAdapter as InnerTasksAdapter, TasksFilter,
    TasksWatcher,
};
use crate::adapter::net::redex::{
    FsyncPolicy, Redex as InnerRedex, RedexError, RedexEvent, RedexFile as InnerRedexFile,
    RedexFileConfig,
};

use super::handle_guard::{HandleGuard, FFI_HANDLE_FREE_DEADLINE};
use super::NetError;

// =========================================================================
// Extended error codes for the CortEX surface. Keep numbering below -99
// (NetError::Unknown) so they never collide with the base surface.
// =========================================================================

pub(crate) const NET_ERR_CORTEX_CLOSED: c_int = -100;
pub(crate) const NET_ERR_CORTEX_FOLD: c_int = -101;
// Exported via the Go header (`net.h` / `ErrNetDb`) for forward
// compatibility with future NetDb-layer errors; no current FFI site
// returns it, hence the allow.
#[allow(dead_code)]
pub(crate) const NET_ERR_NETDB: c_int = -102;
pub(crate) const NET_ERR_REDEX: c_int = -103;
pub(crate) const NET_ERR_TIMEOUT: c_int = 1;
pub(crate) const NET_ERR_STREAM_ENDED: c_int = 2;

// =========================================================================
// Shared utilities
// =========================================================================

/// One tokio runtime, lazily initialized, used by every CortEX /
/// RedEX FFI call. The watch / tail cursors rely on a single runtime
/// so the spawned forwarding tasks survive across cursor calls.
/// Uses `eprintln! + std::process::abort()` on builder failure
/// instead of `expect`-panic. See `ffi/mesh.rs::runtime()` for the
/// full rationale.
fn runtime() -> &'static Arc<Runtime> {
    use std::sync::OnceLock;
    static RT: OnceLock<Arc<Runtime>> = OnceLock::new();
    RT.get_or_init(|| {
        match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => Arc::new(rt),
            Err(e) => {
                eprintln!(
                    "FATAL: cortex FFI tokio runtime build failure ({e:?}); aborting to avoid panic across the FFI boundary"
                );
                std::process::abort();
            }
        }
    })
}

/// `block_on(...)` wrapper that aborts on runtime-in-runtime
/// rather than panicking across the FFI boundary. See
/// `ffi/mesh.rs::block_on` for the full rationale; the check is the
/// same `Handle::try_current()` test, the abort message names the
/// cortex surface so the post-mortem is unambiguous.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    if tokio::runtime::Handle::try_current().is_ok() {
        eprintln!(
            "FATAL: cortex FFI called from inside a tokio runtime context; \
             aborting to avoid runtime-in-runtime panic across the FFI boundary"
        );
        std::process::abort();
    }
    runtime().block_on(future)
}

/// Copy a C string into an owned `String`. Returns `None` on null or
/// non-UTF-8 input.
///
/// Returns `String` (not `&str`) by design: a helper that returned a
/// borrow would need a free-choice lifetime like `Option<&'a str>`,
/// which would let callers pick `'static` and silently produce a
/// dangling reference once the caller's `*const c_char` goes out of
/// scope. Owning the copy eliminates the footgun at a small allocation
/// cost per FFI call (these paths already allocate for JSON parsing).
unsafe fn c_str_to_owned(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    CStr::from_ptr(p).to_str().ok().map(|s| s.to_owned())
}

/// Serialize `value` as JSON into a C-owned string + length. On
/// success writes the pointer to `*out_ptr` and the length to
/// `*out_len` (excluding the null terminator) and returns `0`.
/// On non-success the out params are zeroed (`null`, `0`) so a
/// caller that reads them before checking the return code sees
/// "no output" rather than stale stack data. The caller must
/// free the string with `net_free_string` on success.
///
/// Null-checks `out_ptr` and `out_len` before writing through
/// them. Returns `NetError::NullPointer` so the FFI caller can
/// distinguish "I forgot output pointers" from "the operation
/// failed."
fn write_json_out<T: Serialize>(
    value: &T,
    out_ptr: *mut *mut c_char,
    out_len: *mut usize,
) -> c_int {
    if out_ptr.is_null() || out_len.is_null() {
        return NetError::NullPointer.into();
    }
    let Ok(s) = serde_json::to_string(value) else {
        // Pre-zero so the caller can rely on the contract
        // "non-zero return ⇒ out_ptr is null and out_len is 0"
        // rather than reading stale data from before the call.
        unsafe {
            *out_ptr = ptr::null_mut();
            *out_len = 0;
        }
        return NetError::Unknown.into();
    };
    let len = s.len();
    let Ok(cs) = CString::new(s) else {
        unsafe {
            *out_ptr = ptr::null_mut();
            *out_len = 0;
        }
        return NetError::Unknown.into();
    };
    unsafe {
        *out_ptr = cs.into_raw();
        *out_len = len;
    }
    0
}

/// Helper: pre-zero `*out_ptr` and `*out_len` after a null-check.
/// Call at the top of every FFI function that takes
/// `(out_json, out_len)` so subsequent error returns leave the
/// out params as `(null, 0)` rather than stale stack data. The
/// audit (#136) calls this contract "pre-zero" — every error
/// return must satisfy "out_json is null AND out_len is 0,"
/// distinct from the success contract "out_json is heap-allocated
/// and out_len is its length." Pre-fix several functions
/// returned errors without touching the out params, so callers
/// that didn't strictly check the return code dereferenced
/// stale data.
fn zero_out_json(out_ptr: *mut *mut c_char, out_len: *mut usize) {
    if !out_ptr.is_null() {
        unsafe {
            *out_ptr = ptr::null_mut();
        }
    }
    if !out_len.is_null() {
        unsafe {
            *out_len = 0;
        }
    }
}

// =========================================================================
// Compile-time Send + Sync assertions for FFI handle inner types.
//
// These handles are returned to C as `*mut HandleType` and routinely
// shared across goroutines / Python threads — the docstrings on
// every "open" / "watch" function advertise this pattern. Soundness
// rests entirely on the inner type's `Send + Sync` impl; the FFI
// layer doesn't typecheck `Send + Sync` itself, so a future refactor
// that adds a `Cell` / `RefCell` / `Rc` / `*mut` field to one of
// these types would compile cleanly while silently introducing a
// data race that any threaded caller would trigger.
//
// The `const _: fn() = ...` idiom is a compile-time trait check
// without pulling in `static_assertions` as a dep. If any inner
// type loses `Send + Sync`, this block fails to compile.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<InnerRedex>();
    assert_send_sync::<InnerRedexFile>();
    assert_send_sync::<InnerTasksAdapter>();
    assert_send_sync::<InnerMemoriesAdapter>();
    assert_send_sync::<
        TokioMutex<Option<BoxStream<'static, std::result::Result<RedexEvent, RedexError>>>>,
    >();
    assert_send_sync::<TokioMutex<Option<BoxStream<'static, Vec<Task>>>>>();
    assert_send_sync::<TokioMutex<Option<BoxStream<'static, Vec<Memory>>>>>();
};

// =========================================================================
// Redex manager
// =========================================================================

/// FFI handle wrapping an [`InnerRedex`] manager.
///
/// Carries a [`HandleGuard`] so a Go cgo / Python-thread caller
/// racing `net_redex_free` against `net_redex_open_file` /
/// `net_tasks_adapter_open` / `net_memories_adapter_open` doesn't
/// UAF the dropped inner. Box is intentionally leaked on free;
/// inner Arc lives in [`ManuallyDrop`] for take-and-drop after
/// the drain.
pub struct RedexHandle {
    inner: ManuallyDrop<Arc<InnerRedex>>,
    guard: HandleGuard,
}

/// Create a new Redex manager. `persistent_dir` may be NULL for
/// heap-only. Returns a heap-allocated handle the caller must free
/// with `net_redex_free`.
#[unsafe(no_mangle)]
pub extern "C" fn net_redex_new(persistent_dir: *const c_char) -> *mut RedexHandle {
    let dir = if persistent_dir.is_null() {
        None
    } else {
        unsafe { c_str_to_owned(persistent_dir) }
    };
    let inner = match dir {
        Some(d) => InnerRedex::new().with_persistent_dir(d),
        None => InnerRedex::new(),
    };
    Box::into_raw(Box::new(RedexHandle {
        inner: ManuallyDrop::new(Arc::new(inner)),
        guard: HandleGuard::new(),
    }))
}

#[unsafe(no_mangle)]
pub extern "C" fn net_redex_free(handle: *mut RedexHandle) {
    if handle.is_null() {
        return;
    }
    // Quiesce in-flight ops before dropping the inner.
    // Box stays leaked. See `super::handle_guard` for soundness.
    let h: &RedexHandle = unsafe { &*handle };
    if h.guard.begin_free(FFI_HANDLE_FREE_DEADLINE) {
        // SAFETY: `freeing=true` blocks new ops; `active_ops`
        // drained to zero. We hold the unique writable reference.
        unsafe {
            let inner = ManuallyDrop::take(&mut (*handle).inner);
            drop(inner);
        }
    } else {
        tracing::warn!(
            "net_redex_free: in-flight ops did not drain within deadline; \
             leaking inner to avoid use-after-free"
        );
    }
}

// =========================================================================
// Replication operator surface — Phase I Go binding completion
// =========================================================================
//
// These functions extend the existing `net_redex_*` FFI to expose
// the operator surface from `Redex::enable_replication`, plus the
// per-channel metrics view via `replication_prometheus_text`. The
// Go binding consumes them via `net_redex_enable_replication(mesh)`
// followed by `net_redex_open_file` with a `RedexFileConfigJson`
// carrying a populated `replication` field.
//
// Cross-link to the Node + Python bindings: the same surface is
// exposed via `Redex.enableReplication(mesh)` (NAPI) and
// `Redex.enable_replication(mesh)` (PyO3) in
// `bindings/{node,python}/src/cortex.rs`. The Go side has its own
// FFI because there's no shared SDK wrapper — every binding goes
// straight against the core `Redex` types.

/// Install cross-node replication on this `Redex`. Consumes the
/// `*mut Arc<MeshNode>` boxed pointer produced by
/// `net_mesh_arc_clone` — caller MUST NOT free it again
/// **regardless of return code**: success consumes the Arc into
/// the new wiring; error returns drop the Arc before returning.
/// Idempotent — repeated calls return without disturbing the
/// existing router.
///
/// Returns `0` on success, `NetError::NullPointer` (`-1`) when
/// either handle is NULL, `NetError::ShuttingDown` when the
/// `Redex` is in `_free`-quiesce.
#[cfg(feature = "net")]
#[unsafe(no_mangle)]
pub extern "C" fn net_redex_enable_replication(
    redex: *mut RedexHandle,
    mesh_arc: *mut Arc<crate::adapter::net::MeshNode>,
) -> c_int {
    // R-8: free `mesh_arc` on every error path. The Go binding
    // (and every other C consumer) reads the rc + assumes the
    // Arc was consumed regardless; without this drop the boxed
    // Arc leaks on every NullPointer / ShuttingDown return.
    if redex.is_null() || mesh_arc.is_null() {
        if !mesh_arc.is_null() {
            // SAFETY: caller documented `mesh_arc` as produced by
            // `net_mesh_arc_clone`. Drop now to honor the
            // "consumed regardless" contract.
            unsafe {
                drop(Box::from_raw(mesh_arc));
            }
        }
        return NetError::NullPointer.into();
    }
    let redex_ref = unsafe { &*redex };
    let _op = match redex_ref.guard.try_enter() {
        Some(op) => op,
        None => {
            // SAFETY: as above; drop the Arc the caller already
            // gave up ownership of.
            unsafe {
                drop(Box::from_raw(mesh_arc));
            }
            return NetError::ShuttingDown.into();
        }
    };
    // SAFETY: `mesh_arc` is documented as produced by
    // `net_mesh_arc_clone`; consume the Box, take the Arc.
    let mesh = unsafe { *Box::from_raw(mesh_arc) };
    redex_ref.inner.enable_replication(mesh);
    0
}

/// Count of per-channel replication runtimes registered on this
/// `Redex`. Returns `0` when replication isn't enabled or on a
/// NULL handle (defensive — the Go side typically validates the
/// handle is non-NULL before calling).
#[unsafe(no_mangle)]
pub extern "C" fn net_redex_replication_runtime_count(redex: *const RedexHandle) -> u32 {
    let Some(h) = (unsafe { redex.as_ref() }) else {
        return 0;
    };
    let _op = match h.guard.try_enter() {
        Some(op) => op,
        None => return 0,
    };
    h.inner.replication_runtime_count() as u32
}

/// Render the per-channel replication metrics as Prometheus text.
/// Returns a heap-allocated, NUL-terminated string the caller frees
/// with [`crate::ffi::net_free_string`]. Returns the empty string
/// (heap-allocated + NUL-terminated) when replication isn't
/// enabled — the call site can pipe straight into an HTTP scrape
/// body without branching. Returns NULL only on a NULL input
/// handle or when the `Redex` is in `_free`-quiesce.
///
/// Covers the seven per-channel shapes from
/// `docs/CONFIG_REPLICATION.md`: `*_lag_seconds`,
/// `*_sync_bytes_total`, `*_leader_changes_total`,
/// `*_under_capacity_total`, `*_skip_ahead_total`,
/// `*_election_thrash_total`, `*_witness_withdrawals_total`.
#[unsafe(no_mangle)]
pub extern "C" fn net_redex_replication_prometheus_text(redex: *const RedexHandle) -> *mut c_char {
    let Some(h) = (unsafe { redex.as_ref() }) else {
        return std::ptr::null_mut();
    };
    let _op = match h.guard.try_enter() {
        Some(op) => op,
        None => return std::ptr::null_mut(),
    };
    let text = h.inner.replication_prometheus_text();
    // CString::new rejects strings with interior NULs. Prometheus
    // text shouldn't contain any, but use the fallback just in
    // case (replication channel names with embedded NULs would
    // have been rejected at `ChannelName::new` long before this
    // path runs).
    match CString::new(text) {
        Ok(c) => c.into_raw(),
        Err(_) => CString::new("").unwrap().into_raw(),
    }
}

// =========================================================================
// Greedy-LRU dataforts operator surface (DATAFORTS_PLAN § Phase 1)
// =========================================================================
//
// Same shape as the replication FFI above. The Go binding consumes
// `net_redex_enable_greedy_dataforts(mesh, config_json)`; the config
// rides as a JSON-encoded `RedexGreedyConfigJson` so binding-side
// validation surfaces typed errors before the install lands.

/// JSON shape the Go (and any C-ABI) consumer encodes for
/// `net_redex_enable_greedy_dataforts`. All fields optional —
/// missing fields keep the substrate Phase-1 defaults.
#[cfg(feature = "dataforts-greedy")]
#[derive(serde::Deserialize, Default)]
struct RedexGreedyConfigJson {
    /// Scope filter (`scope:<label>` body matches admit). Empty /
    /// missing admits regardless.
    scopes: Option<Vec<String>>,
    /// Maximum acceptable RTT to the chain's home node, in
    /// milliseconds. Default `200`.
    proximity_max_rtt_ms: Option<u64>,
    /// Per-channel byte cap (floor 1 MiB, default 100 MiB).
    per_channel_cap_bytes: Option<u64>,
    /// Cluster-wide byte cap (default 10 GiB; must be ≥
    /// `per_channel_cap_bytes`).
    total_cap_bytes: Option<u64>,
    /// I/O budget as a fraction of measured NIC peak. Range
    /// `(0.0, 1.0]`. Default `0.25`.
    bandwidth_budget_fraction: Option<f32>,
    /// `"disabled"` / `"any_of_local_capabilities"` (default) /
    /// `"strict"`.
    intent_match: Option<String>,
    /// `"ignore"` / `"soft_preference"` (default) /
    /// `"strict_required"`.
    colocation_policy: Option<String>,
}

#[cfg(feature = "dataforts-greedy")]
impl RedexGreedyConfigJson {
    fn into_config(
        self,
    ) -> Result<crate::adapter::net::dataforts::GreedyConfig, &'static str> {
        use crate::adapter::net::dataforts::{
            ColocationPolicy, GreedyConfig, IntentMatchPolicy, ScopeLabel,
        };
        let mut cfg = GreedyConfig::new();
        if let Some(scopes) = self.scopes {
            cfg = cfg.with_scopes(scopes.into_iter().map(ScopeLabel::new).collect());
        }
        if let Some(ms) = self.proximity_max_rtt_ms {
            cfg = cfg.with_proximity_max_rtt(std::time::Duration::from_millis(ms));
        }
        if let Some(b) = self.per_channel_cap_bytes {
            cfg = cfg.with_per_channel_cap_bytes(b);
        }
        if let Some(b) = self.total_cap_bytes {
            cfg = cfg.with_total_cap_bytes(b);
        }
        if let Some(f) = self.bandwidth_budget_fraction {
            cfg = cfg.with_bandwidth_budget_fraction(f);
        }
        if let Some(policy) = self.intent_match {
            let parsed = match policy.as_str() {
                "disabled" => IntentMatchPolicy::Disabled,
                "any_of_local_capabilities" => IntentMatchPolicy::AnyOfLocalCapabilities,
                "strict" => IntentMatchPolicy::Strict,
                _ => return Err("unknown intent_match"),
            };
            cfg = cfg.with_intent_match(parsed);
        }
        if let Some(policy) = self.colocation_policy {
            let parsed = match policy.as_str() {
                "ignore" => ColocationPolicy::Ignore,
                "soft_preference" => ColocationPolicy::SoftPreference,
                "strict_required" => ColocationPolicy::StrictRequired,
                _ => return Err("unknown colocation_policy"),
            };
            cfg = cfg.with_colocation_policy(parsed);
        }
        Ok(cfg)
    }
}

/// Install greedy-LRU dataforts wiring on this `Redex`. Same
/// Arc-consumption contract as `net_redex_enable_replication`:
/// `mesh_arc` is consumed regardless of return code.
///
/// `config_json` is optional — pass NULL or empty to use the
/// locked Phase-1 defaults. JSON parse errors and validation
/// errors surface as `NET_ERR_REDEX`.
///
/// Returns `0` on success; `NetError::NullPointer` (`-1`) when
/// either redex or mesh_arc is NULL; `NetError::ShuttingDown`
/// when the Redex is in `_free`-quiesce;
/// `NetError::InvalidUtf8` / `NetError::InvalidJson` for malformed
/// config; `NET_ERR_REDEX` for validation errors.
#[cfg(all(feature = "net", feature = "dataforts-greedy"))]
#[unsafe(no_mangle)]
pub extern "C" fn net_redex_enable_greedy_dataforts(
    redex: *mut RedexHandle,
    mesh_arc: *mut Arc<crate::adapter::net::MeshNode>,
    config_json: *const c_char,
) -> c_int {
    if redex.is_null() || mesh_arc.is_null() {
        if !mesh_arc.is_null() {
            unsafe {
                drop(Box::from_raw(mesh_arc));
            }
        }
        return NetError::NullPointer.into();
    }
    let redex_ref = unsafe { &*redex };
    let _op = match redex_ref.guard.try_enter() {
        Some(op) => op,
        None => {
            unsafe {
                drop(Box::from_raw(mesh_arc));
            }
            return NetError::ShuttingDown.into();
        }
    };
    let cfg_json: RedexGreedyConfigJson = if config_json.is_null() {
        RedexGreedyConfigJson::default()
    } else {
        let Some(s) = (unsafe { c_str_to_owned(config_json) }) else {
            unsafe {
                drop(Box::from_raw(mesh_arc));
            }
            return NetError::InvalidUtf8.into();
        };
        if s.is_empty() {
            RedexGreedyConfigJson::default()
        } else {
            match serde_json::from_str(&s) {
                Ok(v) => v,
                Err(_) => {
                    unsafe {
                        drop(Box::from_raw(mesh_arc));
                    }
                    return NetError::InvalidJson.into();
                }
            }
        }
    };
    let cfg = match cfg_json.into_config() {
        Ok(c) => c,
        Err(_) => {
            unsafe {
                drop(Box::from_raw(mesh_arc));
            }
            return NET_ERR_REDEX;
        }
    };
    let mesh = unsafe { *Box::from_raw(mesh_arc) };
    let local_caps = Arc::new(
        crate::adapter::net::behavior::capability::CapabilitySet::default(),
    );
    let registry = crate::adapter::net::behavior::placement::IntentRegistry::defaults();
    match redex_ref
        .inner
        .enable_greedy_dataforts(mesh, cfg, local_caps, registry)
    {
        Ok(()) => 0,
        Err(_) => NET_ERR_REDEX,
    }
}

/// Uninstall greedy wiring. Idempotent.
#[cfg(feature = "dataforts-greedy")]
#[unsafe(no_mangle)]
pub extern "C" fn net_redex_disable_greedy_dataforts(redex: *mut RedexHandle) -> c_int {
    let Some(h) = (unsafe { redex.as_ref() }) else {
        return NetError::NullPointer.into();
    };
    let _op = match h.guard.try_enter() {
        Some(op) => op,
        None => return NetError::ShuttingDown.into(),
    };
    h.inner.disable_greedy_dataforts();
    0
}

/// Count of channels currently in the greedy cache. Returns `0`
/// when greedy isn't enabled or on a NULL handle.
#[cfg(feature = "dataforts-greedy")]
#[unsafe(no_mangle)]
pub extern "C" fn net_redex_greedy_cached_channel_count(
    redex: *const RedexHandle,
) -> u32 {
    let Some(h) = (unsafe { redex.as_ref() }) else {
        return 0;
    };
    let _op = match h.guard.try_enter() {
        Some(op) => op,
        None => return 0,
    };
    h.inner
        .greedy_runtime()
        .map(|r| r.cached_channel_count() as u32)
        .unwrap_or(0)
}

/// Render greedy metrics as Prometheus text. Caller frees via
/// [`crate::ffi::net_free_string`]. Empty string when greedy
/// isn't enabled; NULL on a NULL handle or shutting-down Redex.
#[cfg(feature = "dataforts-greedy")]
#[unsafe(no_mangle)]
pub extern "C" fn net_redex_greedy_prometheus_text(
    redex: *const RedexHandle,
) -> *mut c_char {
    let Some(h) = (unsafe { redex.as_ref() }) else {
        return std::ptr::null_mut();
    };
    let _op = match h.guard.try_enter() {
        Some(op) => op,
        None => return std::ptr::null_mut(),
    };
    let text = h
        .inner
        .greedy_runtime()
        .map(|r| r.metrics().snapshot().prometheus_text())
        .unwrap_or_default();
    match CString::new(text) {
        Ok(c) => c.into_raw(),
        Err(_) => CString::new("").unwrap().into_raw(),
    }
}

// =========================================================================
// RedexFile
// =========================================================================

#[derive(Deserialize, Default)]
struct RedexFileConfigJson {
    #[serde(default)]
    persistent: bool,
    fsync_every_n: Option<u64>,
    fsync_interval_ms: Option<u64>,
    retention_max_events: Option<u64>,
    retention_max_bytes: Option<u64>,
    retention_max_age_ms: Option<u64>,
    /// Cross-node replication opt-in. `None` (default) keeps the
    /// channel single-node; `Some(cfg)` opts into replication and
    /// requires `net_redex_enable_replication` to have been called
    /// first — otherwise `net_redex_open_file` returns
    /// `NET_ERR_REDEX` with the typed error from
    /// `Redex::open_file`.
    replication: Option<RedexReplicationConfigJson>,
}

/// Replication-config JSON shape. Mirrors `ReplicationConfig` from
/// the core. All fields are optional — omitted ones fall back to
/// the core's defaults (`factor=3`, `heartbeat_ms=500`,
/// `placement=Standard`, `on_under_capacity=Withdraw`,
/// `replication_budget_fraction=0.5`).
///
/// `placement` rides as a tagged enum so the Go side serializes a
/// flat JSON object rather than choosing between a nested form and
/// an enum string per-strategy. `on_under_capacity` is a flat
/// string.
#[derive(Deserialize, Default)]
struct RedexReplicationConfigJson {
    factor: Option<u8>,
    heartbeat_ms: Option<u64>,
    /// `"standard"` (default), `"pinned"`, `"colocation_strict"`.
    /// With `"pinned"`, `pinned_nodes` is required.
    placement: Option<String>,
    pinned_nodes: Option<Vec<u64>>,
    leader_pinned: Option<u64>,
    /// `"withdraw"` (default), `"evict_oldest"`.
    on_under_capacity: Option<String>,
    replication_budget_fraction: Option<f32>,
}

impl RedexReplicationConfigJson {
    fn into_config(self) -> Result<crate::adapter::net::redex::ReplicationConfig, &'static str> {
        use crate::adapter::net::redex::{PlacementStrategy, ReplicationConfig, UnderCapacity};
        let mut cfg = ReplicationConfig::new();
        if let Some(f) = self.factor {
            cfg = cfg.with_factor(f);
        }
        if let Some(hb) = self.heartbeat_ms {
            cfg = cfg.with_heartbeat_ms(hb);
        }
        let placement = match self.placement.as_deref() {
            None | Some("standard") => PlacementStrategy::Standard,
            Some("colocation_strict") | Some("colocation-strict") => {
                PlacementStrategy::ColocationStrict
            }
            Some("pinned") => {
                let nodes = self
                    .pinned_nodes
                    .ok_or("pinned placement requires pinned_nodes")?;
                if nodes.is_empty() {
                    return Err("pinned placement requires non-empty pinned_nodes");
                }
                PlacementStrategy::Pinned(nodes)
            }
            Some(_) => return Err("unknown placement strategy"),
        };
        cfg = cfg.with_placement(placement);
        if let Some(leader) = self.leader_pinned {
            cfg = cfg.with_leader_pinned(Some(leader));
        }
        let policy = match self.on_under_capacity.as_deref() {
            None | Some("withdraw") => UnderCapacity::Withdraw,
            Some("evict_oldest") | Some("evict-oldest") => UnderCapacity::EvictOldest,
            Some(_) => return Err("unknown on_under_capacity policy"),
        };
        cfg = cfg.with_on_under_capacity(policy);
        if let Some(fr) = self.replication_budget_fraction {
            cfg = cfg.with_replication_budget_fraction(fr);
        }
        cfg.validate().map_err(|_| "replication config invalid")?;
        Ok(cfg)
    }
}

/// FFI handle wrapping a [`InnerRedexFile`].
///
/// Carries a [`HandleGuard`] to close the audit-#23 use-after-free:
/// pre-fix `net_redex_file_free` was an unconditional
/// `Box::from_raw`, so a Go cgo / Python-thread caller racing
/// `net_redex_file_append` against `_free` would have its
/// concurrent `&*handle` deref read freed memory.
///
/// `inner` lives in [`ManuallyDrop`] so `_free` can take it out
/// after quiescing in-flight ops; the outer `Box` is intentionally
/// leaked (the handle box must outlive `try_enter`'s `fetch_add`
/// — see [`super::handle_guard`] for the full soundness story).
pub struct RedexFileHandle {
    inner: ManuallyDrop<Arc<InnerRedexFile>>,
    guard: HandleGuard,
}

/// Open (or get) a RedEX file for raw append / tail / read-range.
/// `config_json` may be NULL for defaults. Writes the file handle to
/// `*out_handle` on success. Caller frees with `net_redex_file_free`.
#[unsafe(no_mangle)]
// Field-by-field reassignment after `default()` is clearer here than
// a struct literal because several fields need conditional logic
// (fsync policy validation) that inlines awkwardly.
#[allow(clippy::field_reassign_with_default)]
pub extern "C" fn net_redex_open_file(
    redex: *mut RedexHandle,
    name: *const c_char,
    config_json: *const c_char,
    out_handle: *mut *mut RedexFileHandle,
) -> c_int {
    if redex.is_null() || name.is_null() || out_handle.is_null() {
        return NetError::NullPointer.into();
    }
    // Pre-zero the out-pointer so a cgo / C consumer reading the
    // slot after a non-zero return sees null rather than stale stack
    // data. The success path overwrites this with the boxed handle.
    unsafe {
        *out_handle = std::ptr::null_mut();
    }
    let redex = unsafe { &*redex };
    let _op = match redex.guard.try_enter() {
        Some(op) => op,
        None => return NetError::ShuttingDown.into(),
    };
    let Some(name_str) = (unsafe { c_str_to_owned(name) }) else {
        return NetError::InvalidUtf8.into();
    };
    let Ok(channel) = ChannelName::new(&name_str) else {
        return NET_ERR_REDEX;
    };
    let cfg_json: RedexFileConfigJson = if config_json.is_null() {
        RedexFileConfigJson::default()
    } else {
        let Some(s) = (unsafe { c_str_to_owned(config_json) }) else {
            return NetError::InvalidUtf8.into();
        };
        match serde_json::from_str(&s) {
            Ok(v) => v,
            Err(_) => return NetError::InvalidJson.into(),
        }
    };
    let mut cfg = RedexFileConfig::default();
    cfg.persistent = cfg_json.persistent;
    match (cfg_json.fsync_every_n, cfg_json.fsync_interval_ms) {
        (Some(_), Some(_)) | (Some(0), _) | (_, Some(0)) => return NET_ERR_REDEX,
        (Some(n), None) => cfg.fsync_policy = FsyncPolicy::EveryN(n),
        (None, Some(ms)) => {
            cfg.fsync_policy = FsyncPolicy::Interval(std::time::Duration::from_millis(ms))
        }
        _ => {}
    }
    // Reject `Some(0)` for every retention dimension at the same
    // gate that rejects fsync zeros above. Setting
    // `retention_max_events = 0` (or _bytes / _age_ms) means
    // "evict everything immediately on first append" — almost
    // certainly a config mistake intended as "no limit", which in
    // every JSON schema this crate accepts is expressed as `null`
    // / omission. Pre-fix `Some(0)` was propagated unchecked,
    // turning a config typo into silent total data loss on every
    // write.
    if matches!(cfg_json.retention_max_events, Some(0))
        || matches!(cfg_json.retention_max_bytes, Some(0))
        || matches!(cfg_json.retention_max_age_ms, Some(0))
    {
        return NET_ERR_REDEX;
    }
    cfg.retention_max_events = cfg_json.retention_max_events;
    cfg.retention_max_bytes = cfg_json.retention_max_bytes;
    if let Some(ms) = cfg_json.retention_max_age_ms {
        cfg.retention_max_age_ns = Some(ms.saturating_mul(1_000_000));
    }
    if let Some(rep_json) = cfg_json.replication {
        match rep_json.into_config() {
            Ok(rep) => cfg.replication = Some(rep),
            Err(_) => return NET_ERR_REDEX,
        }
    }
    match redex.inner.open_file(&channel, cfg) {
        Ok(file) => {
            let handle = Box::new(RedexFileHandle {
                inner: ManuallyDrop::new(Arc::new(file)),
                guard: HandleGuard::new(),
            });
            unsafe {
                *out_handle = Box::into_raw(handle);
            }
            0
        }
        Err(_) => NET_ERR_REDEX,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn net_redex_file_free(handle: *mut RedexFileHandle) {
    if handle.is_null() {
        return;
    }
    // Quiesce in-flight ops before dropping the inner.
    // The outer Box is intentionally leaked — see
    // `super::handle_guard` for the soundness story (concurrent
    // ops doing `try_enter`'s `fetch_add` on a deallocated atomic
    // would UAF).
    //
    // SAFETY: `handle` is non-null per the early return above; the
    // caller's contract pins it to a previously-returned
    // `*mut RedexFileHandle`. The guard reference outlives this
    // function (the box stays leaked).
    let h: &RedexFileHandle = unsafe { &*handle };
    if h.guard.begin_free(FFI_HANDLE_FREE_DEADLINE) {
        // No in-flight ops; future try_enter calls bail. Safe to
        // take the inner Arc and drop it (which drops InnerRedexFile
        // when no other Arc clones exist).
        // SAFETY: we hold the unique writable reference at this
        // point — `freeing=true` blocks all new ops, and active_ops
        // has drained to zero. Take goes through a `*mut` because
        // `&` doesn't permit `ManuallyDrop::take` (consumes by
        // ownership).
        unsafe {
            let inner = ManuallyDrop::take(&mut (*handle).inner);
            drop(inner);
        }
    } else {
        // Timeout: in-flight ops still running past the deadline.
        // Leak the inner along with the box rather than risk a UAF.
        // The bus-level `tracing` infra surfaces the wedge for
        // operators; here we degrade silently rather than panic
        // across `extern "C"`.
        tracing::warn!(
            "net_redex_file_free: in-flight ops did not drain within deadline; \
             leaking inner to avoid use-after-free"
        );
    }
}

/// Append one payload. Writes the assigned seq to `*out_seq`.
#[unsafe(no_mangle)]
pub extern "C" fn net_redex_file_append(
    handle: *mut RedexFileHandle,
    payload: *const u8,
    len: usize,
    out_seq: *mut u64,
) -> c_int {
    if handle.is_null() || payload.is_null() || out_seq.is_null() {
        return NetError::NullPointer.into();
    }
    let file = unsafe { &*handle };
    // Refuse to touch `inner` if `_free` has begun. Without this
    // gate, a Go cgo / Python-thread caller racing `_free`
    // against this function reads freed memory after `_free`
    // drops the inner.
    let _op = match file.guard.try_enter() {
        Some(op) => op,
        None => return NetError::ShuttingDown.into(),
    };
    let slice = unsafe { std::slice::from_raw_parts(payload, len) };
    match file.inner.append(slice) {
        Ok(seq) => {
            unsafe {
                *out_seq = seq;
            }
            0
        }
        Err(_) => NET_ERR_REDEX,
    }
}

#[derive(Serialize)]
struct RedexEventJson {
    seq: u64,
    /// Hex-encoded payload so JSON transport is safe for binary data.
    payload_hex: String,
    checksum: u32,
    is_inline: bool,
}

impl From<RedexEvent> for RedexEventJson {
    fn from(ev: RedexEvent) -> Self {
        RedexEventJson {
            seq: ev.entry.seq,
            payload_hex: hex_encode(&ev.payload),
            checksum: ev.entry.checksum(),
            is_inline: ev.entry.is_inline(),
        }
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

#[unsafe(no_mangle)]
pub extern "C" fn net_redex_file_len(handle: *mut RedexFileHandle) -> u64 {
    if handle.is_null() {
        return 0;
    }
    let file = unsafe { &*handle };
    let _op = match file.guard.try_enter() {
        Some(op) => op,
        // 0 is a valid `len`; can't distinguish from "freed" via the
        // return value alone. Caller racing free against `_len`
        // already accepts the post-free 0 result; this path makes
        // the read sound (no UAF on `inner`).
        None => return 0,
    };
    file.inner.len() as u64
}

/// Read the half-open range `[start, end)` into a JSON array.
#[unsafe(no_mangle)]
pub extern "C" fn net_redex_file_read_range(
    handle: *mut RedexFileHandle,
    start: u64,
    end: u64,
    out_json: *mut *mut c_char,
    out_len: *mut usize,
) -> c_int {
    if handle.is_null() || out_json.is_null() || out_len.is_null() {
        return NetError::NullPointer.into();
    }
    let file = unsafe { &*handle };
    let _op = match file.guard.try_enter() {
        Some(op) => op,
        None => return NetError::ShuttingDown.into(),
    };
    let events: Vec<RedexEventJson> = file
        .inner
        .read_range(start, end)
        .into_iter()
        .map(RedexEventJson::from)
        .collect();
    write_json_out(&events, out_json, out_len)
}

#[unsafe(no_mangle)]
pub extern "C" fn net_redex_file_sync(handle: *mut RedexFileHandle) -> c_int {
    if handle.is_null() {
        return NetError::NullPointer.into();
    }
    let file = unsafe { &*handle };
    let _op = match file.guard.try_enter() {
        Some(op) => op,
        None => return NetError::ShuttingDown.into(),
    };
    match file.inner.sync() {
        Ok(()) => 0,
        Err(_) => NET_ERR_REDEX,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn net_redex_file_close(handle: *mut RedexFileHandle) -> c_int {
    if handle.is_null() {
        return NetError::NullPointer.into();
    }
    let file = unsafe { &*handle };
    let _op = match file.guard.try_enter() {
        Some(op) => op,
        None => return NetError::ShuttingDown.into(),
    };
    match file.inner.close() {
        Ok(()) => 0,
        Err(_) => NET_ERR_REDEX,
    }
}

// RedEX tail cursor

/// Type alias to keep the [`RedexTailHandle`] field type from
/// tripping clippy's `type_complexity` lint without `#[allow]`.
type RedexTailStream = ManuallyDrop<
    TokioMutex<Option<BoxStream<'static, std::result::Result<RedexEvent, RedexError>>>>,
>;

/// FFI handle for a tail cursor over a [`RedexFileHandle`].
///
/// Same `HandleGuard` recipe applies. The inner is a
/// `TokioMutex<Option<BoxStream<...>>>`; on free we drain
/// in-flight `next` calls before taking the inner via
/// `ManuallyDrop`. Box stays leaked.
pub struct RedexTailHandle {
    stream: RedexTailStream,
    guard: HandleGuard,
}

#[unsafe(no_mangle)]
pub extern "C" fn net_redex_file_tail(
    handle: *mut RedexFileHandle,
    from_seq: u64,
    out_cursor: *mut *mut RedexTailHandle,
) -> c_int {
    if handle.is_null() || out_cursor.is_null() {
        return NetError::NullPointer.into();
    }
    // Pre-zero the out-pointer so a non-zero return leaves the
    // caller with a null cursor rather than stale stack data.
    unsafe {
        *out_cursor = std::ptr::null_mut();
    }
    let file = unsafe { &*handle };
    let _op = match file.guard.try_enter() {
        Some(op) => op,
        None => return NetError::ShuttingDown.into(),
    };
    let stream = file.inner.tail(from_seq);
    let boxed: BoxStream<'static, std::result::Result<RedexEvent, RedexError>> = stream.boxed();
    let cursor = Box::new(RedexTailHandle {
        stream: ManuallyDrop::new(TokioMutex::new(Some(boxed))),
        guard: HandleGuard::new(),
    });
    unsafe {
        *out_cursor = Box::into_raw(cursor);
    }
    0
}

/// Pull the next tail event. `timeout_ms == 0` blocks indefinitely.
/// Returns:
/// * `0`  — event delivered; JSON written to `*out_json` (caller frees
///   via `net_free_string`).
/// * `1`  — timeout (no event available within `timeout_ms`).
/// * `2`  — stream ended (file closed or dropped).
/// * negative — error.
#[unsafe(no_mangle)]
pub extern "C" fn net_redex_tail_next(
    cursor: *mut RedexTailHandle,
    timeout_ms: u32,
    out_json: *mut *mut c_char,
    out_len: *mut usize,
) -> c_int {
    if cursor.is_null() || out_json.is_null() || out_len.is_null() {
        return NetError::NullPointer.into();
    }
    // Pre-zero out params so timeout / stream-end / error
    // returns leave the caller with `(null, 0)` rather than
    // stale stack data. The doc-comment establishes this
    // contract ("non-zero return ⇒ no JSON written"), but pre-
    // fix the function returned NET_ERR_TIMEOUT and
    // NET_ERR_STREAM_ENDED without touching the out params.
    zero_out_json(out_json, out_len);
    let cursor = unsafe { &*cursor };
    let _op = match cursor.guard.try_enter() {
        Some(op) => op,
        None => return NetError::ShuttingDown.into(),
    };
    block_on(async move {
        let mut guard = cursor.stream.lock().await;
        let Some(stream) = guard.as_mut() else {
            return NET_ERR_STREAM_ENDED;
        };
        let next_fut = stream.next();
        let outcome = if timeout_ms == 0 {
            next_fut.await
        } else {
            match tokio::time::timeout(
                std::time::Duration::from_millis(timeout_ms as u64),
                next_fut,
            )
            .await
            {
                Ok(v) => v,
                Err(_) => return NET_ERR_TIMEOUT,
            }
        };
        match outcome {
            Some(Ok(ev)) => {
                // Drop the cursor guard BEFORE the JSON
                // serialization so concurrent callers on the
                // same cursor don't stall waiting for our
                // write_json_out to finish. Pre-fix the
                // serialization ran inside the TokioMutex
                // critical section, so a fast event arrival on
                // a shared cursor under contention serialized
                // calls behind whichever caller was building
                // the JSON. The event is owned at this point;
                // the mutex was only protecting the stream
                // poll, not the event itself.
                drop(guard);
                let js = RedexEventJson::from(ev);
                write_json_out(&js, out_json, out_len)
            }
            Some(Err(RedexError::Closed)) | None => {
                *guard = None;
                NET_ERR_STREAM_ENDED
            }
            Some(Err(_)) => {
                *guard = None;
                NET_ERR_REDEX
            }
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn net_redex_tail_free(cursor: *mut RedexTailHandle) {
    if cursor.is_null() {
        return;
    }
    // Quiesce in-flight `_next` ops before dropping the inner
    // stream. Box stays leaked.
    let h: &RedexTailHandle = unsafe { &*cursor };
    if h.guard.begin_free(FFI_HANDLE_FREE_DEADLINE) {
        // SAFETY: drained; sole writable reference.
        unsafe {
            let stream = ManuallyDrop::take(&mut (*cursor).stream);
            drop(stream);
        }
    } else {
        tracing::warn!(
            "net_redex_tail_free: in-flight ops did not drain within deadline; \
             leaking inner to avoid use-after-free"
        );
    }
}

// =========================================================================
// Tasks adapter — standalone open. Go-side `NetDb` struct composes
// Redex + Tasks + Memories without a dedicated FFI handle.
// =========================================================================

/// FFI handle wrapping an [`InnerTasksAdapter`].
///
/// Same `HandleGuard` recipe as `RedexHandle` / `RedexFileHandle`.
/// Box leaked on free; inner Arc lives in `ManuallyDrop` for
/// take-and-drop after drain.
pub struct TasksAdapterHandle {
    inner: ManuallyDrop<Arc<InnerTasksAdapter>>,
    guard: HandleGuard,
}

/// Open a tasks adapter against a Redex. `persistent != 0` routes
/// writes through the Redex's persistent directory (requires the
/// Redex to have been created with a `persistent_dir`).
#[unsafe(no_mangle)]
pub extern "C" fn net_tasks_adapter_open(
    redex: *mut RedexHandle,
    origin_hash: u64,
    persistent: c_int,
    out_handle: *mut *mut TasksAdapterHandle,
) -> c_int {
    if redex.is_null() || out_handle.is_null() {
        return NetError::NullPointer.into();
    }
    let redex = unsafe { &*redex };
    let _op = match redex.guard.try_enter() {
        Some(op) => op,
        None => return NetError::ShuttingDown.into(),
    };
    let cfg = if persistent != 0 {
        RedexFileConfig::default().with_persistent(true)
    } else {
        RedexFileConfig::default()
    };
    // `open_with_config` spawns the fold task via `tokio::spawn` and
    // needs a live reactor; run under our runtime.
    let redex_inner: Arc<InnerRedex> = Arc::clone(&redex.inner);
    let result = block_on(async move {
        InnerTasksAdapter::open_with_config(&redex_inner, origin_hash, cfg).await
    });
    match result {
        Ok(adapter) => {
            let handle = Box::new(TasksAdapterHandle {
                inner: ManuallyDrop::new(Arc::new(adapter)),
                guard: HandleGuard::new(),
            });
            unsafe {
                *out_handle = Box::into_raw(handle);
            }
            0
        }
        Err(_) => NET_ERR_CORTEX_FOLD,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn net_tasks_adapter_close(handle: *mut TasksAdapterHandle) -> c_int {
    if handle.is_null() {
        return NetError::NullPointer.into();
    }
    let tasks = unsafe { &*handle };
    let _op = match tasks.guard.try_enter() {
        Some(op) => op,
        None => return NetError::ShuttingDown.into(),
    };
    match tasks.inner.close() {
        Ok(()) => 0,
        Err(_) => NET_ERR_CORTEX_CLOSED,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn net_tasks_adapter_free(handle: *mut TasksAdapterHandle) {
    if handle.is_null() {
        return;
    }
    // Quiesce in-flight ops before dropping inner; box leaked.
    let h: &TasksAdapterHandle = unsafe { &*handle };
    if h.guard.begin_free(FFI_HANDLE_FREE_DEADLINE) {
        // SAFETY: drained; sole writable reference.
        unsafe {
            let inner = ManuallyDrop::take(&mut (*handle).inner);
            drop(inner);
        }
    } else {
        tracing::warn!(
            "net_tasks_adapter_free: in-flight ops did not drain within deadline; \
             leaking inner to avoid use-after-free"
        );
    }
}

#[derive(Serialize)]
struct TaskJson {
    id: u64,
    title: String,
    status: &'static str,
    created_ns: u64,
    updated_ns: u64,
}

impl From<Task> for TaskJson {
    fn from(t: Task) -> Self {
        TaskJson {
            id: t.id,
            title: t.title,
            status: match t.status {
                TaskStatus::Pending => "pending",
                TaskStatus::Completed => "completed",
            },
            created_ns: t.created_ns,
            updated_ns: t.updated_ns,
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn net_tasks_create(
    handle: *mut TasksAdapterHandle,
    id: u64,
    title: *const c_char,
    now_ns: u64,
    out_seq: *mut u64,
) -> c_int {
    if handle.is_null() || title.is_null() || out_seq.is_null() {
        return NetError::NullPointer.into();
    }
    let tasks = unsafe { &*handle };
    let _op = match tasks.guard.try_enter() {
        Some(op) => op,
        None => return NetError::ShuttingDown.into(),
    };
    let Some(title) = (unsafe { c_str_to_owned(title) }) else {
        return NetError::InvalidUtf8.into();
    };
    match tasks.inner.create(id, title, now_ns) {
        Ok(seq) => {
            unsafe {
                *out_seq = seq;
            }
            0
        }
        Err(_) => NET_ERR_CORTEX_FOLD,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn net_tasks_rename(
    handle: *mut TasksAdapterHandle,
    id: u64,
    new_title: *const c_char,
    now_ns: u64,
    out_seq: *mut u64,
) -> c_int {
    if handle.is_null() || new_title.is_null() || out_seq.is_null() {
        return NetError::NullPointer.into();
    }
    let tasks = unsafe { &*handle };
    let _op = match tasks.guard.try_enter() {
        Some(op) => op,
        None => return NetError::ShuttingDown.into(),
    };
    let Some(nt) = (unsafe { c_str_to_owned(new_title) }) else {
        return NetError::InvalidUtf8.into();
    };
    match tasks.inner.rename(id, nt, now_ns) {
        Ok(seq) => {
            unsafe {
                *out_seq = seq;
            }
            0
        }
        Err(_) => NET_ERR_CORTEX_FOLD,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn net_tasks_complete(
    handle: *mut TasksAdapterHandle,
    id: u64,
    now_ns: u64,
    out_seq: *mut u64,
) -> c_int {
    if handle.is_null() || out_seq.is_null() {
        return NetError::NullPointer.into();
    }
    let tasks = unsafe { &*handle };
    let _op = match tasks.guard.try_enter() {
        Some(op) => op,
        None => return NetError::ShuttingDown.into(),
    };
    match tasks.inner.complete(id, now_ns) {
        Ok(seq) => {
            unsafe {
                *out_seq = seq;
            }
            0
        }
        Err(_) => NET_ERR_CORTEX_FOLD,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn net_tasks_delete(
    handle: *mut TasksAdapterHandle,
    id: u64,
    out_seq: *mut u64,
) -> c_int {
    if handle.is_null() || out_seq.is_null() {
        return NetError::NullPointer.into();
    }
    let tasks = unsafe { &*handle };
    let _op = match tasks.guard.try_enter() {
        Some(op) => op,
        None => return NetError::ShuttingDown.into(),
    };
    match tasks.inner.delete(id) {
        Ok(seq) => {
            unsafe {
                *out_seq = seq;
            }
            0
        }
        Err(_) => NET_ERR_CORTEX_FOLD,
    }
}

/// Block until fold has applied every event up through `seq`. Pass
/// `timeout_ms == 0` to wait indefinitely. Returns `0` on success,
/// `1` on timeout, or negative on error.
#[unsafe(no_mangle)]
pub extern "C" fn net_tasks_wait_for_seq(
    handle: *mut TasksAdapterHandle,
    seq: u64,
    timeout_ms: u32,
) -> c_int {
    if handle.is_null() {
        return NetError::NullPointer.into();
    }
    let tasks = unsafe { &*handle };
    let _op = match tasks.guard.try_enter() {
        Some(op) => op,
        None => return NetError::ShuttingDown.into(),
    };
    let adapter: Arc<InnerTasksAdapter> = Arc::clone(&tasks.inner);
    block_on(async move {
        let fut = adapter.wait_for_seq(seq);
        if timeout_ms == 0 {
            fut.await;
            0
        } else {
            match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms as u64), fut)
                .await
            {
                Ok(_) => 0,
                Err(_) => NET_ERR_TIMEOUT,
            }
        }
    })
}

#[derive(Deserialize, Default)]
struct TasksFilterJson {
    status: Option<String>,
    title_contains: Option<String>,
    created_after_ns: Option<u64>,
    created_before_ns: Option<u64>,
    updated_after_ns: Option<u64>,
    updated_before_ns: Option<u64>,
    order_by: Option<String>,
    limit: Option<u32>,
}

fn build_tasks_watcher(
    adapter: &InnerTasksAdapter,
    filter_json: *const c_char,
) -> Result<TasksWatcher, c_int> {
    let mut w = adapter.watch();
    if filter_json.is_null() {
        return Ok(w);
    }
    let Some(s) = (unsafe { c_str_to_owned(filter_json) }) else {
        return Err(NetError::InvalidUtf8.into());
    };
    let f: TasksFilterJson = match serde_json::from_str(&s) {
        Ok(v) => v,
        Err(_) => return Err(NetError::InvalidJson.into()),
    };
    w = match f.status.as_deref() {
        Some("pending") => w.where_status(TaskStatus::Pending),
        Some("completed") => w.where_status(TaskStatus::Completed),
        Some(_) => return Err(NetError::InvalidJson.into()),
        None => w,
    };
    if let Some(s) = f.title_contains {
        w = w.title_contains(s);
    }
    if let Some(ns) = f.created_after_ns {
        w = w.created_after(ns);
    }
    if let Some(ns) = f.created_before_ns {
        w = w.created_before(ns);
    }
    if let Some(ns) = f.updated_after_ns {
        w = w.updated_after(ns);
    }
    if let Some(ns) = f.updated_before_ns {
        w = w.updated_before(ns);
    }
    if let Some(o) = f.order_by.as_deref() {
        w = match o {
            "id_asc" => w.order_by(TasksOrderBy::IdAsc),
            "id_desc" => w.order_by(TasksOrderBy::IdDesc),
            "created_asc" => w.order_by(TasksOrderBy::CreatedAsc),
            "created_desc" => w.order_by(TasksOrderBy::CreatedDesc),
            "updated_asc" => w.order_by(TasksOrderBy::UpdatedAsc),
            "updated_desc" => w.order_by(TasksOrderBy::UpdatedDesc),
            _ => return Err(NetError::InvalidJson.into()),
        };
    }
    if let Some(l) = f.limit {
        w = w.limit(l as usize);
    }
    Ok(w)
}

/// Apply JSON filter to a query-side filter (used by `list_tasks`).
#[allow(clippy::field_reassign_with_default)]
fn build_tasks_list_filter(filter_json: *const c_char) -> Result<TasksFilter, c_int> {
    if filter_json.is_null() {
        return Ok(TasksFilter::default());
    }
    let Some(s) = (unsafe { c_str_to_owned(filter_json) }) else {
        return Err(NetError::InvalidUtf8.into());
    };
    let f: TasksFilterJson = match serde_json::from_str(&s) {
        Ok(v) => v,
        Err(_) => return Err(NetError::InvalidJson.into()),
    };
    let mut out = TasksFilter::default();
    out.status = match f.status.as_deref() {
        Some("pending") => Some(TaskStatus::Pending),
        Some("completed") => Some(TaskStatus::Completed),
        Some(_) => return Err(NetError::InvalidJson.into()),
        None => None,
    };
    out.title_contains = f.title_contains;
    out.created_after_ns = f.created_after_ns;
    out.created_before_ns = f.created_before_ns;
    out.updated_after_ns = f.updated_after_ns;
    out.updated_before_ns = f.updated_before_ns;
    out.order_by = match f.order_by.as_deref() {
        None => None,
        Some("id_asc") => Some(TasksOrderBy::IdAsc),
        Some("id_desc") => Some(TasksOrderBy::IdDesc),
        Some("created_asc") => Some(TasksOrderBy::CreatedAsc),
        Some("created_desc") => Some(TasksOrderBy::CreatedDesc),
        Some("updated_asc") => Some(TasksOrderBy::UpdatedAsc),
        Some("updated_desc") => Some(TasksOrderBy::UpdatedDesc),
        // Reject unknown order_by instead of silently falling back —
        // a misspelling ("createdasc") would otherwise return a
        // successful but misordered result.
        Some(_) => return Err(NetError::InvalidJson.into()),
    };
    out.limit = f.limit.map(|l| l as usize);
    Ok(out)
}

fn run_tasks_list(tasks: &InnerTasksAdapter, filter: &TasksFilter) -> Vec<Task> {
    let state = tasks.state();
    let guard = state.read();
    let mut q = guard.query();
    if let Some(s) = filter.status {
        q = q.where_status(s);
    }
    if let Some(s) = &filter.title_contains {
        q = q.title_contains(s.clone());
    }
    if let Some(ns) = filter.created_after_ns {
        q = q.created_after(ns);
    }
    if let Some(ns) = filter.created_before_ns {
        q = q.created_before(ns);
    }
    if let Some(ns) = filter.updated_after_ns {
        q = q.updated_after(ns);
    }
    if let Some(ns) = filter.updated_before_ns {
        q = q.updated_before(ns);
    }
    if let Some(o) = filter.order_by {
        q = q.order_by(o);
    }
    if let Some(l) = filter.limit {
        q = q.limit(l);
    }
    q.collect()
}

/// List tasks matching `filter_json` (may be NULL). Writes a JSON
/// array of tasks to `*out_json`; caller frees via `net_free_string`.
#[unsafe(no_mangle)]
pub extern "C" fn net_tasks_list(
    handle: *mut TasksAdapterHandle,
    filter_json: *const c_char,
    out_json: *mut *mut c_char,
    out_len: *mut usize,
) -> c_int {
    if handle.is_null() || out_json.is_null() || out_len.is_null() {
        return NetError::NullPointer.into();
    }
    // Pre-zero so a filter-build error return leaves the out
    // params at (null, 0) rather than stale stack data — matches
    // the contract documented on `write_json_out`.
    zero_out_json(out_json, out_len);
    let tasks = unsafe { &*handle };
    let _op = match tasks.guard.try_enter() {
        Some(op) => op,
        None => return NetError::ShuttingDown.into(),
    };
    let filter = match build_tasks_list_filter(filter_json) {
        Ok(f) => f,
        Err(code) => return code,
    };
    let items: Vec<TaskJson> = run_tasks_list(&tasks.inner, &filter)
        .into_iter()
        .map(TaskJson::from)
        .collect();
    write_json_out(&items, out_json, out_len)
}

/// FFI handle for a tasks-watch cursor.
///
/// Same `HandleGuard` recipe. Box leaked on free; inner stream
/// lives in `ManuallyDrop`.
pub struct TasksWatchHandle {
    stream: ManuallyDrop<TokioMutex<Option<BoxStream<'static, Vec<Task>>>>>,
    guard: HandleGuard,
}

/// Atomic snapshot + watch. Writes:
/// * `*out_snapshot` — JSON array of tasks in the current filter result.
/// * `*out_cursor` — watch cursor; iterate via `net_tasks_watch_next`
///   and free via `net_tasks_watch_free`.
#[unsafe(no_mangle)]
pub extern "C" fn net_tasks_snapshot_and_watch(
    handle: *mut TasksAdapterHandle,
    filter_json: *const c_char,
    out_snapshot: *mut *mut c_char,
    out_snapshot_len: *mut usize,
    out_cursor: *mut *mut TasksWatchHandle,
) -> c_int {
    if handle.is_null()
        || out_snapshot.is_null()
        || out_snapshot_len.is_null()
        || out_cursor.is_null()
    {
        return NetError::NullPointer.into();
    }
    let tasks = unsafe { &*handle };
    let _op = match tasks.guard.try_enter() {
        Some(op) => op,
        None => return NetError::ShuttingDown.into(),
    };
    let watcher = match build_tasks_watcher(&tasks.inner, filter_json) {
        Ok(w) => w,
        Err(code) => return code,
    };
    // `watcher.stream()` spawns a forwarding task — needs a live
    // reactor.
    let adapter: Arc<InnerTasksAdapter> = Arc::clone(&tasks.inner);
    let (snapshot, stream) = block_on(async move { adapter.snapshot_and_watch(watcher) });
    let snapshot_json: Vec<TaskJson> = snapshot.into_iter().map(TaskJson::from).collect();
    let code = write_json_out(&snapshot_json, out_snapshot, out_snapshot_len);
    if code != 0 {
        return code;
    }
    let handle = Box::new(TasksWatchHandle {
        stream: ManuallyDrop::new(TokioMutex::new(Some(stream))),
        guard: HandleGuard::new(),
    });
    unsafe {
        *out_cursor = Box::into_raw(handle);
    }
    0
}

/// Pull the next tasks-watch batch. Semantics match
/// [`net_redex_tail_next`] — `0` on event (JSON array written),
/// `1` on timeout, `2` on stream end, negative on error.
#[unsafe(no_mangle)]
pub extern "C" fn net_tasks_watch_next(
    cursor: *mut TasksWatchHandle,
    timeout_ms: u32,
    out_json: *mut *mut c_char,
    out_len: *mut usize,
) -> c_int {
    if cursor.is_null() || out_json.is_null() || out_len.is_null() {
        return NetError::NullPointer.into();
    }
    let cursor = unsafe { &*cursor };
    let _op = match cursor.guard.try_enter() {
        Some(op) => op,
        None => return NetError::ShuttingDown.into(),
    };
    block_on(async move {
        let mut guard = cursor.stream.lock().await;
        let Some(stream) = guard.as_mut() else {
            return NET_ERR_STREAM_ENDED;
        };
        let next_fut = stream.next();
        let outcome = if timeout_ms == 0 {
            next_fut.await
        } else {
            match tokio::time::timeout(
                std::time::Duration::from_millis(timeout_ms as u64),
                next_fut,
            )
            .await
            {
                Ok(v) => v,
                Err(_) => return NET_ERR_TIMEOUT,
            }
        };
        match outcome {
            Some(batch) => {
                let js: Vec<TaskJson> = batch.into_iter().map(TaskJson::from).collect();
                write_json_out(&js, out_json, out_len)
            }
            None => {
                *guard = None;
                NET_ERR_STREAM_ENDED
            }
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn net_tasks_watch_free(cursor: *mut TasksWatchHandle) {
    if cursor.is_null() {
        return;
    }
    let h: &TasksWatchHandle = unsafe { &*cursor };
    if h.guard.begin_free(FFI_HANDLE_FREE_DEADLINE) {
        unsafe {
            let stream = ManuallyDrop::take(&mut (*cursor).stream);
            drop(stream);
        }
    } else {
        tracing::warn!(
            "net_tasks_watch_free: in-flight ops did not drain within deadline; \
             leaking inner to avoid use-after-free"
        );
    }
}

// =========================================================================
// Memories adapter (same shape as tasks)
// =========================================================================

/// FFI handle wrapping an [`InnerMemoriesAdapter`].
///
/// Same `HandleGuard` recipe as the other cortex handles. Box
/// leaked on free; inner Arc lives in `ManuallyDrop` for
/// take-and-drop after drain.
pub struct MemoriesAdapterHandle {
    inner: ManuallyDrop<Arc<InnerMemoriesAdapter>>,
    guard: HandleGuard,
}

#[unsafe(no_mangle)]
pub extern "C" fn net_memories_adapter_open(
    redex: *mut RedexHandle,
    origin_hash: u64,
    persistent: c_int,
    out_handle: *mut *mut MemoriesAdapterHandle,
) -> c_int {
    if redex.is_null() || out_handle.is_null() {
        return NetError::NullPointer.into();
    }
    let redex = unsafe { &*redex };
    let _op = match redex.guard.try_enter() {
        Some(op) => op,
        None => return NetError::ShuttingDown.into(),
    };
    let cfg = if persistent != 0 {
        RedexFileConfig::default().with_persistent(true)
    } else {
        RedexFileConfig::default()
    };
    let redex_inner: Arc<InnerRedex> = Arc::clone(&redex.inner);
    let result = block_on(async move {
        InnerMemoriesAdapter::open_with_config(&redex_inner, origin_hash, cfg).await
    });
    match result {
        Ok(adapter) => {
            let handle = Box::new(MemoriesAdapterHandle {
                inner: ManuallyDrop::new(Arc::new(adapter)),
                guard: HandleGuard::new(),
            });
            unsafe {
                *out_handle = Box::into_raw(handle);
            }
            0
        }
        Err(_) => NET_ERR_CORTEX_FOLD,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn net_memories_adapter_close(handle: *mut MemoriesAdapterHandle) -> c_int {
    if handle.is_null() {
        return NetError::NullPointer.into();
    }
    let mem = unsafe { &*handle };
    let _op = match mem.guard.try_enter() {
        Some(op) => op,
        None => return NetError::ShuttingDown.into(),
    };
    match mem.inner.close() {
        Ok(()) => 0,
        Err(_) => NET_ERR_CORTEX_CLOSED,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn net_memories_adapter_free(handle: *mut MemoriesAdapterHandle) {
    if handle.is_null() {
        return;
    }
    // Quiesce in-flight ops before dropping inner; box leaked.
    let h: &MemoriesAdapterHandle = unsafe { &*handle };
    if h.guard.begin_free(FFI_HANDLE_FREE_DEADLINE) {
        // SAFETY: drained; sole writable reference.
        unsafe {
            let inner = ManuallyDrop::take(&mut (*handle).inner);
            drop(inner);
        }
    } else {
        tracing::warn!(
            "net_memories_adapter_free: in-flight ops did not drain within deadline; \
             leaking inner to avoid use-after-free"
        );
    }
}

#[derive(Serialize)]
struct MemoryJson {
    id: u64,
    content: String,
    tags: Vec<String>,
    source: String,
    created_ns: u64,
    updated_ns: u64,
    pinned: bool,
}

impl From<Memory> for MemoryJson {
    fn from(m: Memory) -> Self {
        MemoryJson {
            id: m.id,
            content: m.content,
            tags: m.tags,
            source: m.source,
            created_ns: m.created_ns,
            updated_ns: m.updated_ns,
            pinned: m.pinned,
        }
    }
}

#[derive(Deserialize)]
struct MemoryStoreInput {
    id: u64,
    content: String,
    tags: Vec<String>,
    source: String,
    now_ns: u64,
}

/// Store a memory. Input is a JSON object
/// `{id, content, tags, source, now_ns}`.
#[unsafe(no_mangle)]
pub extern "C" fn net_memories_store(
    handle: *mut MemoriesAdapterHandle,
    input_json: *const c_char,
    out_seq: *mut u64,
) -> c_int {
    if handle.is_null() || input_json.is_null() || out_seq.is_null() {
        return NetError::NullPointer.into();
    }
    let mem = unsafe { &*handle };
    let _op = match mem.guard.try_enter() {
        Some(op) => op,
        None => return NetError::ShuttingDown.into(),
    };
    let Some(s) = (unsafe { c_str_to_owned(input_json) }) else {
        return NetError::InvalidUtf8.into();
    };
    let input: MemoryStoreInput = match serde_json::from_str(&s) {
        Ok(v) => v,
        Err(_) => return NetError::InvalidJson.into(),
    };
    match mem.inner.store(
        input.id,
        input.content,
        input.tags,
        input.source,
        input.now_ns,
    ) {
        Ok(seq) => {
            unsafe {
                *out_seq = seq;
            }
            0
        }
        Err(_) => NET_ERR_CORTEX_FOLD,
    }
}

#[derive(Deserialize)]
struct MemoryRetagInput {
    id: u64,
    tags: Vec<String>,
    now_ns: u64,
}

#[unsafe(no_mangle)]
pub extern "C" fn net_memories_retag(
    handle: *mut MemoriesAdapterHandle,
    input_json: *const c_char,
    out_seq: *mut u64,
) -> c_int {
    if handle.is_null() || input_json.is_null() || out_seq.is_null() {
        return NetError::NullPointer.into();
    }
    let mem = unsafe { &*handle };
    let _op = match mem.guard.try_enter() {
        Some(op) => op,
        None => return NetError::ShuttingDown.into(),
    };
    let Some(s) = (unsafe { c_str_to_owned(input_json) }) else {
        return NetError::InvalidUtf8.into();
    };
    let input: MemoryRetagInput = match serde_json::from_str(&s) {
        Ok(v) => v,
        Err(_) => return NetError::InvalidJson.into(),
    };
    match mem.inner.retag(input.id, input.tags, input.now_ns) {
        Ok(seq) => {
            unsafe {
                *out_seq = seq;
            }
            0
        }
        Err(_) => NET_ERR_CORTEX_FOLD,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn net_memories_pin(
    handle: *mut MemoriesAdapterHandle,
    id: u64,
    now_ns: u64,
    out_seq: *mut u64,
) -> c_int {
    if handle.is_null() || out_seq.is_null() {
        return NetError::NullPointer.into();
    }
    let mem = unsafe { &*handle };
    let _op = match mem.guard.try_enter() {
        Some(op) => op,
        None => return NetError::ShuttingDown.into(),
    };
    match mem.inner.pin(id, now_ns) {
        Ok(seq) => {
            unsafe {
                *out_seq = seq;
            }
            0
        }
        Err(_) => NET_ERR_CORTEX_FOLD,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn net_memories_unpin(
    handle: *mut MemoriesAdapterHandle,
    id: u64,
    now_ns: u64,
    out_seq: *mut u64,
) -> c_int {
    if handle.is_null() || out_seq.is_null() {
        return NetError::NullPointer.into();
    }
    let mem = unsafe { &*handle };
    let _op = match mem.guard.try_enter() {
        Some(op) => op,
        None => return NetError::ShuttingDown.into(),
    };
    match mem.inner.unpin(id, now_ns) {
        Ok(seq) => {
            unsafe {
                *out_seq = seq;
            }
            0
        }
        Err(_) => NET_ERR_CORTEX_FOLD,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn net_memories_delete(
    handle: *mut MemoriesAdapterHandle,
    id: u64,
    out_seq: *mut u64,
) -> c_int {
    if handle.is_null() || out_seq.is_null() {
        return NetError::NullPointer.into();
    }
    let mem = unsafe { &*handle };
    let _op = match mem.guard.try_enter() {
        Some(op) => op,
        None => return NetError::ShuttingDown.into(),
    };
    match mem.inner.delete(id) {
        Ok(seq) => {
            unsafe {
                *out_seq = seq;
            }
            0
        }
        Err(_) => NET_ERR_CORTEX_FOLD,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn net_memories_wait_for_seq(
    handle: *mut MemoriesAdapterHandle,
    seq: u64,
    timeout_ms: u32,
) -> c_int {
    if handle.is_null() {
        return NetError::NullPointer.into();
    }
    let mem = unsafe { &*handle };
    let _op = match mem.guard.try_enter() {
        Some(op) => op,
        None => return NetError::ShuttingDown.into(),
    };
    let adapter: Arc<InnerMemoriesAdapter> = Arc::clone(&mem.inner);
    block_on(async move {
        let fut = adapter.wait_for_seq(seq);
        if timeout_ms == 0 {
            fut.await;
            0
        } else {
            match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms as u64), fut)
                .await
            {
                Ok(_) => 0,
                Err(_) => NET_ERR_TIMEOUT,
            }
        }
    })
}

#[derive(Deserialize, Default)]
struct MemoriesFilterJson {
    source: Option<String>,
    content_contains: Option<String>,
    tag: Option<String>,
    any_tag: Option<Vec<String>>,
    all_tags: Option<Vec<String>>,
    pinned: Option<bool>,
    created_after_ns: Option<u64>,
    created_before_ns: Option<u64>,
    updated_after_ns: Option<u64>,
    updated_before_ns: Option<u64>,
    order_by: Option<String>,
    limit: Option<u32>,
}

fn parse_memories_order_by(s: &str) -> Option<MemoriesOrderBy> {
    match s {
        "id_asc" => Some(MemoriesOrderBy::IdAsc),
        "id_desc" => Some(MemoriesOrderBy::IdDesc),
        "created_asc" => Some(MemoriesOrderBy::CreatedAsc),
        "created_desc" => Some(MemoriesOrderBy::CreatedDesc),
        "updated_asc" => Some(MemoriesOrderBy::UpdatedAsc),
        "updated_desc" => Some(MemoriesOrderBy::UpdatedDesc),
        _ => None,
    }
}

fn build_memories_watcher(
    adapter: &InnerMemoriesAdapter,
    filter_json: *const c_char,
) -> Result<MemoriesWatcher, c_int> {
    let mut w = adapter.watch();
    if filter_json.is_null() {
        return Ok(w);
    }
    let Some(s) = (unsafe { c_str_to_owned(filter_json) }) else {
        return Err(NetError::InvalidUtf8.into());
    };
    let f: MemoriesFilterJson = match serde_json::from_str(&s) {
        Ok(v) => v,
        Err(_) => return Err(NetError::InvalidJson.into()),
    };
    if let Some(s) = f.source {
        w = w.where_source(s);
    }
    if let Some(s) = f.content_contains {
        w = w.content_contains(s);
    }
    if let Some(t) = f.tag {
        w = w.where_tag(t);
    }
    if let Some(tags) = f.any_tag {
        w = w.where_any_tag(tags);
    }
    if let Some(tags) = f.all_tags {
        w = w.where_all_tags(tags);
    }
    if let Some(p) = f.pinned {
        w = w.where_pinned(p);
    }
    if let Some(ns) = f.created_after_ns {
        w = w.created_after(ns);
    }
    if let Some(ns) = f.created_before_ns {
        w = w.created_before(ns);
    }
    if let Some(ns) = f.updated_after_ns {
        w = w.updated_after(ns);
    }
    if let Some(ns) = f.updated_before_ns {
        w = w.updated_before(ns);
    }
    if let Some(o) = f.order_by.as_deref() {
        if let Some(ob) = parse_memories_order_by(o) {
            w = w.order_by(ob);
        } else {
            return Err(NetError::InvalidJson.into());
        }
    }
    if let Some(l) = f.limit {
        w = w.limit(l as usize);
    }
    Ok(w)
}

#[allow(clippy::field_reassign_with_default)]
fn build_memories_list_filter(filter_json: *const c_char) -> Result<MemoriesFilter, c_int> {
    if filter_json.is_null() {
        return Ok(MemoriesFilter::default());
    }
    let Some(s) = (unsafe { c_str_to_owned(filter_json) }) else {
        return Err(NetError::InvalidUtf8.into());
    };
    let f: MemoriesFilterJson = match serde_json::from_str(&s) {
        Ok(v) => v,
        Err(_) => return Err(NetError::InvalidJson.into()),
    };
    let mut out = MemoriesFilter::default();
    out.source = f.source;
    out.content_contains = f.content_contains;
    out.tag = f.tag;
    out.any_tag = f.any_tag;
    out.all_tags = f.all_tags;
    out.pinned = f.pinned;
    out.created_after_ns = f.created_after_ns;
    out.created_before_ns = f.created_before_ns;
    out.updated_after_ns = f.updated_after_ns;
    out.updated_before_ns = f.updated_before_ns;
    // Reject unknown order_by instead of silently falling back —
    // keep parity with build_memories_watcher above.
    out.order_by = match f.order_by.as_deref() {
        None => None,
        Some(o) => match parse_memories_order_by(o) {
            Some(ob) => Some(ob),
            None => return Err(NetError::InvalidJson.into()),
        },
    };
    out.limit = f.limit.map(|l| l as usize);
    Ok(out)
}

fn run_memories_list(mem: &InnerMemoriesAdapter, filter: &MemoriesFilter) -> Vec<Memory> {
    let state = mem.state();
    let guard = state.read();
    let mut q = guard.query();
    if let Some(s) = &filter.source {
        q = q.where_source(s.clone());
    }
    if let Some(s) = &filter.content_contains {
        q = q.content_contains(s.clone());
    }
    if let Some(t) = &filter.tag {
        q = q.where_tag(t.clone());
    }
    if let Some(tags) = &filter.any_tag {
        q = q.where_any_tag(tags.clone());
    }
    if let Some(tags) = &filter.all_tags {
        q = q.where_all_tags(tags.clone());
    }
    if let Some(p) = filter.pinned {
        q = q.where_pinned(p);
    }
    if let Some(ns) = filter.created_after_ns {
        q = q.created_after(ns);
    }
    if let Some(ns) = filter.created_before_ns {
        q = q.created_before(ns);
    }
    if let Some(ns) = filter.updated_after_ns {
        q = q.updated_after(ns);
    }
    if let Some(ns) = filter.updated_before_ns {
        q = q.updated_before(ns);
    }
    if let Some(o) = filter.order_by {
        q = q.order_by(o);
    }
    if let Some(l) = filter.limit {
        q = q.limit(l);
    }
    q.collect()
}

#[unsafe(no_mangle)]
pub extern "C" fn net_memories_list(
    handle: *mut MemoriesAdapterHandle,
    filter_json: *const c_char,
    out_json: *mut *mut c_char,
    out_len: *mut usize,
) -> c_int {
    if handle.is_null() || out_json.is_null() || out_len.is_null() {
        return NetError::NullPointer.into();
    }
    let mem = unsafe { &*handle };
    let _op = match mem.guard.try_enter() {
        Some(op) => op,
        None => return NetError::ShuttingDown.into(),
    };
    let filter = match build_memories_list_filter(filter_json) {
        Ok(f) => f,
        Err(code) => return code,
    };
    let items: Vec<MemoryJson> = run_memories_list(&mem.inner, &filter)
        .into_iter()
        .map(MemoryJson::from)
        .collect();
    write_json_out(&items, out_json, out_len)
}

/// FFI handle for a memories-watch cursor. Same `HandleGuard`
/// recipe as `TasksWatchHandle`.
pub struct MemoriesWatchHandle {
    stream: ManuallyDrop<TokioMutex<Option<BoxStream<'static, Vec<Memory>>>>>,
    guard: HandleGuard,
}

#[unsafe(no_mangle)]
pub extern "C" fn net_memories_snapshot_and_watch(
    handle: *mut MemoriesAdapterHandle,
    filter_json: *const c_char,
    out_snapshot: *mut *mut c_char,
    out_snapshot_len: *mut usize,
    out_cursor: *mut *mut MemoriesWatchHandle,
) -> c_int {
    if handle.is_null()
        || out_snapshot.is_null()
        || out_snapshot_len.is_null()
        || out_cursor.is_null()
    {
        return NetError::NullPointer.into();
    }
    let mem = unsafe { &*handle };
    let _op = match mem.guard.try_enter() {
        Some(op) => op,
        None => return NetError::ShuttingDown.into(),
    };
    let watcher = match build_memories_watcher(&mem.inner, filter_json) {
        Ok(w) => w,
        Err(code) => return code,
    };
    let adapter: Arc<InnerMemoriesAdapter> = Arc::clone(&mem.inner);
    let (snapshot, stream) = block_on(async move { adapter.snapshot_and_watch(watcher) });
    let snapshot_json: Vec<MemoryJson> = snapshot.into_iter().map(MemoryJson::from).collect();
    let code = write_json_out(&snapshot_json, out_snapshot, out_snapshot_len);
    if code != 0 {
        return code;
    }
    let handle = Box::new(MemoriesWatchHandle {
        stream: ManuallyDrop::new(TokioMutex::new(Some(stream))),
        guard: HandleGuard::new(),
    });
    unsafe {
        *out_cursor = Box::into_raw(handle);
    }
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn net_memories_watch_next(
    cursor: *mut MemoriesWatchHandle,
    timeout_ms: u32,
    out_json: *mut *mut c_char,
    out_len: *mut usize,
) -> c_int {
    if cursor.is_null() || out_json.is_null() || out_len.is_null() {
        return NetError::NullPointer.into();
    }
    let cursor = unsafe { &*cursor };
    let _op = match cursor.guard.try_enter() {
        Some(op) => op,
        None => return NetError::ShuttingDown.into(),
    };
    block_on(async move {
        let mut guard = cursor.stream.lock().await;
        let Some(stream) = guard.as_mut() else {
            return NET_ERR_STREAM_ENDED;
        };
        let next_fut = stream.next();
        let outcome = if timeout_ms == 0 {
            next_fut.await
        } else {
            match tokio::time::timeout(
                std::time::Duration::from_millis(timeout_ms as u64),
                next_fut,
            )
            .await
            {
                Ok(v) => v,
                Err(_) => return NET_ERR_TIMEOUT,
            }
        };
        match outcome {
            Some(batch) => {
                let js: Vec<MemoryJson> = batch.into_iter().map(MemoryJson::from).collect();
                write_json_out(&js, out_json, out_len)
            }
            None => {
                *guard = None;
                NET_ERR_STREAM_ENDED
            }
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn net_memories_watch_free(cursor: *mut MemoriesWatchHandle) {
    if cursor.is_null() {
        return;
    }
    let h: &MemoriesWatchHandle = unsafe { &*cursor };
    if h.guard.begin_free(FFI_HANDLE_FREE_DEADLINE) {
        unsafe {
            let stream = ManuallyDrop::take(&mut (*cursor).stream);
            drop(stream);
        }
    } else {
        tracing::warn!(
            "net_memories_watch_free: in-flight ops did not drain within deadline; \
             leaking inner to avoid use-after-free"
        );
    }
}

// ABI-visible no-op to force the linker to keep `c_void` happy on
// some older linkers; harmless otherwise.
#[doc(hidden)]
pub fn _ffi_cortex_keep_alive() -> *mut c_void {
    ptr::null_mut()
}

#[cfg(test)]
mod tests {
    //! Direct Rust-side coverage for the C FFI shims. The Go / Node
    //! / Python binding tests cover happy-path round-trips; these
    //! pin the corner cases that those tests don't exercise:
    //! invalid config rejection, watch-cursor lifetime, and the
    //! shared-runtime contract.

    use super::*;
    use std::ffi::CString;
    use std::ptr;
    use std::sync::Arc;
    use std::sync::Barrier;
    use std::thread;

    fn redex() -> *mut RedexHandle {
        net_redex_new(ptr::null())
    }

    fn open_file(redex: *mut RedexHandle, name: &str, cfg_json: Option<&str>) -> c_int {
        let name_c = CString::new(name).unwrap();
        let cfg_c = cfg_json.map(|s| CString::new(s).unwrap());
        let cfg_ptr = cfg_c.as_ref().map(|c| c.as_ptr()).unwrap_or(ptr::null());
        let mut handle: *mut RedexFileHandle = ptr::null_mut();
        let rc = net_redex_open_file(redex, name_c.as_ptr(), cfg_ptr, &mut handle);
        if rc == 0 && !handle.is_null() {
            net_redex_file_free(handle);
        }
        rc
    }

    /// Conflicting `fsync_every_n` AND `fsync_interval_ms`, as well
    /// as either set to 0, must be rejected with `NET_ERR_REDEX`.
    /// Go-side configs come straight from JSON without further
    /// validation; if these slip past the FFI, the file opens with
    /// silently-default fsync behavior and durability claims become
    /// untrue.
    #[test]
    fn redex_open_file_rejects_conflicting_or_zero_fsync_config() {
        let r = redex();
        // Pre-checks: defaults and each individual setting succeed.
        assert_eq!(open_file(r, "ok-default", None), 0);
        assert_eq!(open_file(r, "ok-everyn", Some(r#"{"fsync_every_n":4}"#)), 0);
        assert_eq!(
            open_file(r, "ok-interval", Some(r#"{"fsync_interval_ms":50}"#),),
            0
        );

        // Rejected combinations. Each row tests one invalid config.
        let invalid = [
            ("both-set", r#"{"fsync_every_n":4,"fsync_interval_ms":50}"#),
            ("zero-everyn", r#"{"fsync_every_n":0}"#),
            ("zero-interval", r#"{"fsync_interval_ms":0}"#),
            ("both-zero", r#"{"fsync_every_n":0,"fsync_interval_ms":0}"#),
            (
                "everyn-set-interval-zero",
                r#"{"fsync_every_n":4,"fsync_interval_ms":0}"#,
            ),
        ];
        for (name, cfg) in invalid {
            let rc = open_file(r, name, Some(cfg));
            assert_eq!(
                rc, NET_ERR_REDEX,
                "config {name:?} ({cfg}) should be rejected with NET_ERR_REDEX (got {rc})"
            );
        }

        net_redex_free(r);
    }

    /// Pin: `net_redex_open_file` rejects `Some(0)` for any
    /// retention dimension at the same gate that rejects fsync
    /// zeros. Pre-fix the retention triple was propagated
    /// unchecked, so a config typo
    /// (`{"retention_max_events": 0}` instead of `null`) silently
    /// configured "evict everything immediately" and lost every
    /// write to the file.
    #[test]
    fn redex_open_file_rejects_zero_retention() {
        let r = redex();
        let invalid = [
            ("zero-events", r#"{"retention_max_events":0}"#),
            ("zero-bytes", r#"{"retention_max_bytes":0}"#),
            ("zero-age", r#"{"retention_max_age_ms":0}"#),
            (
                "any-zero-among-many",
                r#"{"retention_max_events":1000,"retention_max_bytes":0}"#,
            ),
        ];
        for (name, cfg) in invalid {
            let rc = open_file(r, name, Some(cfg));
            assert_eq!(
                rc, NET_ERR_REDEX,
                "config {name:?} ({cfg}) must be rejected with NET_ERR_REDEX (got {rc})"
            );
        }

        // Non-zero retention still parses.
        let valid = [
            ("non-zero-events", r#"{"retention_max_events":10000}"#),
            ("non-zero-bytes", r#"{"retention_max_bytes":1048576}"#),
            ("non-zero-age", r#"{"retention_max_age_ms":60000}"#),
            ("null-retention", r#"{"retention_max_events":null}"#),
        ];
        for (name, cfg) in valid {
            let rc = open_file(r, name, Some(cfg));
            assert_eq!(
                rc, 0,
                "valid config {name:?} ({cfg}) should succeed (got {rc})"
            );
        }

        net_redex_free(r);
    }

    /// `net_redex_open_file` must pre-zero `*out_handle` on entry so
    /// any non-zero return leaves the caller observing a null
    /// pointer rather than stale stack data. Cgo / C consumers that
    /// read `*out_handle` after `rc != 0` would otherwise see a
    /// random bit pattern from the caller's stack frame and may
    /// attempt to free it.
    #[test]
    fn redex_open_file_zeroes_out_handle_on_error() {
        let r = redex();
        let name = CString::new("bad-json").unwrap();
        let cfg = CString::new("not-json {").unwrap();
        // Seed the out-pointer with a non-null sentinel that
        // resembles a leaked handle. A regression would leave this
        // sentinel in place after the InvalidJson return.
        let sentinel = 0xDEAD_BEEF_usize as *mut RedexFileHandle;
        let mut handle: *mut RedexFileHandle = sentinel;
        let rc = net_redex_open_file(r, name.as_ptr(), cfg.as_ptr(), &mut handle);
        assert_eq!(rc, NetError::InvalidJson as c_int);
        assert!(
            handle.is_null(),
            "out_handle must be null after rc != 0; got {handle:?}"
        );
        net_redex_free(r);
    }

    /// `net_redex_file_tail` must pre-zero `*out_cursor` on entry
    /// for the same reason as `net_redex_open_file`. Free the file
    /// to flip the handle guard into the freeing state so the
    /// subsequent tail call's `try_enter` bails with ShuttingDown
    /// after the pre-zero has run.
    #[test]
    fn redex_file_tail_zeroes_out_cursor_on_error() {
        let r = redex();
        let name = CString::new("tail-zero").unwrap();
        let mut file: *mut RedexFileHandle = ptr::null_mut();
        assert_eq!(
            net_redex_open_file(r, name.as_ptr(), ptr::null(), &mut file),
            0
        );
        // Free the file. begin_free flips freeing=true; the outer
        // box stays leaked, so subsequent calls go through but
        // try_enter bails with ShuttingDown.
        net_redex_file_free(file);

        let sentinel = 0xDEAD_BEEF_usize as *mut RedexTailHandle;
        let mut cursor: *mut RedexTailHandle = sentinel;
        let rc = net_redex_file_tail(file, 0, &mut cursor);
        assert_eq!(rc, NetError::ShuttingDown as c_int);
        assert!(
            cursor.is_null(),
            "out_cursor must be null after rc != 0; got {cursor:?}"
        );
        net_redex_free(r);
    }

    /// `net_redex_open_file` with non-JSON config must return
    /// `InvalidJson`, not silently default. Pinned because the Go
    /// SDK relies on this distinction to surface a useful error.
    #[test]
    fn redex_open_file_rejects_non_json_config() {
        let r = redex();
        let name = CString::new("bad-json").unwrap();
        let cfg = CString::new("not-json {").unwrap();
        let mut handle: *mut RedexFileHandle = ptr::null_mut();
        let rc = net_redex_open_file(r, name.as_ptr(), cfg.as_ptr(), &mut handle);
        assert_eq!(rc, NetError::InvalidJson as c_int);
        assert!(handle.is_null());
        net_redex_free(r);
    }

    /// Once the underlying RedexFile is closed, an outstanding tail
    /// cursor's next `tail_next` call must observe `STREAM_ENDED`
    /// cleanly. This is the load-bearing lifetime contract for any
    /// language binding that pumps the cursor into a goroutine /
    /// task — without it, the consumer would block on a closed
    /// stream forever.
    #[test]
    fn redex_tail_cursor_observes_close_with_stream_ended() {
        let r = redex();
        let name = CString::new("tail-close").unwrap();
        let mut file: *mut RedexFileHandle = ptr::null_mut();
        assert_eq!(
            net_redex_open_file(r, name.as_ptr(), ptr::null(), &mut file),
            0
        );

        let mut cursor: *mut RedexTailHandle = ptr::null_mut();
        assert_eq!(net_redex_file_tail(file, 0, &mut cursor), 0);

        // Close the file while the cursor is live.
        assert_eq!(net_redex_file_close(file), 0);

        // Next call on the cursor must return STREAM_ENDED, not
        // block, not panic, not return an error code.
        let mut out_json: *mut c_char = ptr::null_mut();
        let mut out_len: usize = 0;
        let rc = net_redex_tail_next(cursor, 1_000, &mut out_json, &mut out_len);
        assert_eq!(
            rc, NET_ERR_STREAM_ENDED,
            "expected STREAM_ENDED after file close (got {rc})"
        );
        assert!(out_json.is_null(), "no event payload should be written");

        net_redex_tail_free(cursor);
        net_redex_file_free(file);
        net_redex_free(r);
    }

    /// A Go cgo / Python-thread caller racing
    /// `net_redex_file_free` against a concurrent
    /// `net_redex_file_append` (or any RedexFile op) must not
    /// produce a use-after-free. Without the guard, `_free` would
    /// be an unconditional `Box::from_raw` and the concurrent
    /// op's `&*handle` deref would read freed memory.
    ///
    /// We can't deterministically inject the race in a unit test
    /// without race-injection scaffolding, but we CAN pin the two
    /// load-bearing invariants:
    ///   1. After `_free`, future ops bail with `ShuttingDown`
    ///      rather than touching the (taken-out) inner.
    ///   2. `_free` is idempotent — a second call returns
    ///      immediately without touching the already-taken inner.
    ///      The leaked outer Box stays valid; the second
    ///      `begin_free` observes `freeing=true` (set by the
    ///      first caller's `compare_exchange`) and returns
    ///      `false`, skipping the `ManuallyDrop::take` branch
    ///      entirely.
    #[test]
    fn redex_file_free_blocks_subsequent_ops_with_shutting_down() {
        let r = redex();
        let name = CString::new("free-then-op").unwrap();
        let mut file: *mut RedexFileHandle = ptr::null_mut();
        assert_eq!(
            net_redex_open_file(r, name.as_ptr(), ptr::null(), &mut file),
            0
        );
        assert!(!file.is_null());

        // Free the file. begin_free drains immediately (no in-flight
        // ops), takes the inner, leaks the outer box.
        net_redex_file_free(file);

        // Subsequent ops via the same handle must bail with
        // ShuttingDown — try_enter sees freeing=true, decrements,
        // returns None. They must NOT touch the taken inner.
        let payload = b"x";
        let mut out_seq: u64 = 0;
        let rc = net_redex_file_append(file, payload.as_ptr(), payload.len(), &mut out_seq);
        assert_eq!(
            rc,
            NetError::ShuttingDown as c_int,
            "post-free append must surface ShuttingDown (got {rc})",
        );
        assert_eq!(out_seq, 0, "no seq must be assigned to a post-free append");

        // _len takes the silent path (returns 0 — same as the absent
        // case) per its contract.
        assert_eq!(net_redex_file_len(file), 0);

        // _read_range / _sync / _close also bail with ShuttingDown.
        let mut out_json: *mut c_char = ptr::null_mut();
        let mut out_len: usize = 0;
        let rc = net_redex_file_read_range(file, 0, 1, &mut out_json, &mut out_len);
        assert_eq!(rc, NetError::ShuttingDown as c_int);
        assert_eq!(net_redex_file_sync(file), NetError::ShuttingDown as c_int);
        assert_eq!(net_redex_file_close(file), NetError::ShuttingDown as c_int);

        net_redex_free(r);
    }

    /// Pin: `net_redex_file_free` is idempotent under the post-fix
    /// protocol. Pre-fix a second call after the first
    /// `Box::from_raw` was a double-free; post-fix the second call
    /// observes `freeing=true` and returns without touching the
    /// already-taken inner. The handle box is leaked (intentional
    /// — see handle_guard module docs) so the second call's
    /// `&*handle` deref is on still-valid memory.
    #[test]
    fn redex_file_free_is_idempotent() {
        let r = redex();
        let name = CString::new("free-twice").unwrap();
        let mut file: *mut RedexFileHandle = ptr::null_mut();
        assert_eq!(
            net_redex_open_file(r, name.as_ptr(), ptr::null(), &mut file),
            0
        );
        net_redex_file_free(file);
        // Second free: must not panic, must not double-take the
        // ManuallyDrop, must not deallocate the outer box.
        net_redex_file_free(file);
        net_redex_free(r);
    }

    /// A `net_redex_file_free` racing an in-flight
    /// `net_redex_file_append` from another thread must wait for
    /// the append to finish before taking the inner. Without the
    /// guard, free would proceed immediately and the append's
    /// subsequent `&*handle` deref would UAF the dropped inner.
    ///
    /// We use a long-running append (large payload + sync after)
    /// on a background thread and call `_free` from the main
    /// thread once the append has been observed to start. `_free`
    /// blocks until the append's `try_enter` guard drops.
    #[test]
    fn redex_file_free_waits_for_inflight_append() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let r = redex();
        let name = CString::new("free-races-append").unwrap();
        let mut file: *mut RedexFileHandle = ptr::null_mut();
        assert_eq!(
            net_redex_open_file(r, name.as_ptr(), ptr::null(), &mut file),
            0
        );

        // Smuggle the raw pointer across threads via usize. The
        // contract: pre- and during-the-append, no `_free` runs;
        // the worker signals `started` once it's inside append's
        // try_enter; main waits for that signal then calls free.
        let file_addr = file as usize;
        let started = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let started_w = started.clone();
        let done_w = done.clone();
        let worker = std::thread::spawn(move || {
            // Append a chunk — the inner work is fast, so we wrap
            // the call in a brief sleep AFTER signaling started so
            // the test can race _free against an in-flight op.
            // Doing this without a hook in append itself means we
            // can only approximate the race; the timing window is
            // ~30ms which is enough to catch a missing guard.
            started_w.store(true, Ordering::SeqCst);
            let payload = b"hello";
            let mut out_seq: u64 = 0;
            let h = file_addr as *mut RedexFileHandle;
            // The append itself completes fast; sleep simulates a
            // longer-running op. In production a long op is e.g.
            // a large read_range with serialization.
            std::thread::sleep(std::time::Duration::from_millis(30));
            let rc = net_redex_file_append(h, payload.as_ptr(), payload.len(), &mut out_seq);
            done_w.store(true, Ordering::SeqCst);
            // The append should succeed if it ran before _free's
            // begin_free flipped freeing. If it ran after, it
            // should bail with ShuttingDown — both outcomes are
            // sound; the bug pre-fix was a UAF panic / corruption,
            // not a Stale return.
            assert!(
                rc == 0 || rc == NetError::ShuttingDown as c_int,
                "post-fix append after begin_free must EITHER succeed (op got there first) \
                 OR return ShuttingDown — never UAF. Got rc={rc}, out_seq={out_seq}",
            );
        });

        while !started.load(Ordering::SeqCst) {
            std::thread::yield_now();
        }
        // Main: free the file. Post-fix this blocks until the
        // worker's append (which the worker holds via try_enter)
        // releases; pre-fix it would proceed immediately and the
        // worker's subsequent inner-deref would UAF.
        net_redex_file_free(file);

        worker.join().unwrap();
        assert!(
            done.load(Ordering::SeqCst),
            "worker must have completed; the test would otherwise hang \
             past the watchdog if free's begin_free deadlocked",
        );

        net_redex_free(r);
    }

    /// `runtime()` is a process-wide `OnceLock<Arc<Runtime>>`. Many
    /// FFI entry points call it on first use. We assert that
    /// concurrent first-callers from N threads all observe the
    /// same runtime instance — i.e. that `OnceLock` initialization
    /// is correctly atomic and no thread sees a half-built
    /// runtime. (`OnceLock` guarantees this; the test pins the
    /// guarantee against an accidental refactor to a non-atomic
    /// alternative.)
    #[test]
    fn runtime_first_call_returns_same_instance_under_concurrency() {
        const THREADS: usize = 16;
        let barrier = Arc::new(Barrier::new(THREADS));
        let mut handles = Vec::with_capacity(THREADS);
        for _ in 0..THREADS {
            let b = barrier.clone();
            handles.push(thread::spawn(move || {
                b.wait();
                let rt = runtime();
                Arc::as_ptr(rt) as usize
            }));
        }
        let mut ptrs: Vec<usize> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        ptrs.sort();
        ptrs.dedup();
        assert_eq!(
            ptrs.len(),
            1,
            "concurrent first-callers observed {} distinct runtimes (must be exactly 1)",
            ptrs.len()
        );
    }

    // ────────────────────────────────────────────────────────────────
    // Replication FFI — Phase I Go binding surface
    // ────────────────────────────────────────────────────────────────

    /// `replication_runtime_count` reads 0 on an empty `Redex` and
    /// stays 0 when no `enable_replication` was called.
    #[test]
    fn replication_runtime_count_zero_when_not_enabled() {
        let r = redex();
        assert_eq!(net_redex_replication_runtime_count(r), 0);
        net_redex_free(r);
    }

    /// `replication_prometheus_text` returns the empty string
    /// (heap-allocated + NUL-terminated, NOT NULL) when replication
    /// isn't enabled — call sites pipe straight into an HTTP body
    /// without branching.
    #[test]
    fn replication_prometheus_text_empty_when_not_enabled() {
        let r = redex();
        let p = net_redex_replication_prometheus_text(r);
        assert!(!p.is_null());
        let s = unsafe { CStr::from_ptr(p) }.to_str().unwrap();
        assert_eq!(s, "");
        crate::ffi::net_free_string(p);
        net_redex_free(r);
    }

    /// `replication_prometheus_text` returns NULL on a NULL handle —
    /// defensive; the Go side typically guards before calling.
    #[test]
    fn replication_prometheus_text_null_handle_returns_null() {
        let p = net_redex_replication_prometheus_text(ptr::null());
        assert!(p.is_null());
    }

    /// Opening a channel with `replication: { ... }` BEFORE
    /// `enable_replication` was called must fail with
    /// `NET_ERR_REDEX` — the typed error from `Redex::open_file`.
    #[test]
    fn open_file_with_replication_without_enable_fails() {
        let r = redex();
        let cfg = r#"{"replication":{"factor":3,"heartbeat_ms":500}}"#;
        let rc = open_file(r, "ffi/repl_unconfigured", Some(cfg));
        assert_eq!(rc, NET_ERR_REDEX);
        net_redex_free(r);
    }

    /// Invalid replication config (factor below MIN, unknown
    /// placement, etc.) surfaces `NET_ERR_REDEX` without opening
    /// the file.
    #[test]
    fn open_file_with_invalid_replication_config_rejected() {
        let r = redex();
        // Unknown placement strategy.
        let cfg = r#"{"replication":{"placement":"impossible"}}"#;
        let rc = open_file(r, "ffi/repl_invalid_placement", Some(cfg));
        assert_eq!(rc, NET_ERR_REDEX);

        // Pinned without pinned_nodes.
        let cfg = r#"{"replication":{"placement":"pinned"}}"#;
        let rc = open_file(r, "ffi/repl_pinned_no_nodes", Some(cfg));
        assert_eq!(rc, NET_ERR_REDEX);

        // Unknown on_under_capacity.
        let cfg = r#"{"replication":{"on_under_capacity":"impossible"}}"#;
        let rc = open_file(r, "ffi/repl_invalid_policy", Some(cfg));
        assert_eq!(rc, NET_ERR_REDEX);

        net_redex_free(r);
    }

    /// NULL `redex` to the replication functions surfaces 0 /
    /// NULL respectively (the documented defensive shape).
    #[test]
    fn replication_functions_idempotent_on_null_redex() {
        assert_eq!(net_redex_replication_runtime_count(ptr::null()), 0);
        let p = net_redex_replication_prometheus_text(ptr::null());
        assert!(p.is_null());
    }

    /// R-8 regression: `net_redex_enable_replication` must drop
    /// the boxed `Arc<MeshNode>` regardless of return code so the
    /// Go binding's "consumed on call" contract holds even on
    /// NullPointer / ShuttingDown errors. We exercise the NULL-
    /// redex path with a real (but minimal) MeshNode and verify
    /// the Arc strong count drops after the FFI call returns.
    ///
    /// We use an async-Tokio block to satisfy `MeshNode::new`'s
    /// async signature without making the test runtime async.
    #[cfg(feature = "net")]
    #[test]
    fn enable_replication_drops_mesh_arc_on_null_redex() {
        use crate::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig};
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};
        use std::sync::Arc;

        let rt = tokio::runtime::Runtime::new().unwrap();
        let mesh = rt.block_on(async {
            let identity = EntityKeypair::generate();
            let cfg = MeshNodeConfig::new(
                SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
                [0u8; 32],
            );
            Arc::new(MeshNode::new(identity, cfg).await.unwrap())
        });
        let pre_count = Arc::strong_count(&mesh);
        let boxed_arc: *mut Arc<MeshNode> = Box::into_raw(Box::new(mesh.clone()));
        assert_eq!(Arc::strong_count(&mesh), pre_count + 1);

        // NULL redex, valid mesh_arc — must drop boxed_arc and
        // surface NullPointer.
        let rc = net_redex_enable_replication(ptr::null_mut(), boxed_arc);
        let expected: c_int = NetError::NullPointer.into();
        assert_eq!(rc, expected);
        assert_eq!(
            Arc::strong_count(&mesh),
            pre_count,
            "net_redex_enable_replication must drop the boxed Arc on error paths"
        );
    }

    /// Parallel coverage for the greedy FFI surface — pin the
    /// Arc-consumption contract on the NULL-redex error path
    /// (same shape as the replication test above).
    #[cfg(all(feature = "net", feature = "dataforts-greedy"))]
    #[test]
    fn enable_greedy_drops_mesh_arc_on_null_redex() {
        use crate::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig};
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};
        use std::sync::Arc;

        let rt = tokio::runtime::Runtime::new().unwrap();
        let mesh = rt.block_on(async {
            let identity = EntityKeypair::generate();
            let cfg = MeshNodeConfig::new(
                SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
                [0u8; 32],
            );
            Arc::new(MeshNode::new(identity, cfg).await.unwrap())
        });
        let pre_count = Arc::strong_count(&mesh);
        let boxed_arc: *mut Arc<MeshNode> = Box::into_raw(Box::new(mesh.clone()));
        assert_eq!(Arc::strong_count(&mesh), pre_count + 1);

        let rc = net_redex_enable_greedy_dataforts(
            ptr::null_mut(),
            boxed_arc,
            ptr::null(),
        );
        let expected: c_int = NetError::NullPointer.into();
        assert_eq!(rc, expected);
        assert_eq!(
            Arc::strong_count(&mesh),
            pre_count,
            "net_redex_enable_greedy_dataforts must drop the boxed Arc on error paths"
        );
    }

    /// Smoke test: install greedy on a real Redex + mesh, observe
    /// the channel-count + Prometheus text shape, then uninstall.
    #[cfg(all(feature = "net", feature = "dataforts-greedy"))]
    #[test]
    fn greedy_enable_disable_round_trip() {
        use crate::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig};
        use std::ffi::CString;
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};
        use std::sync::Arc;

        let rt = tokio::runtime::Runtime::new().unwrap();
        let mesh = rt.block_on(async {
            let identity = EntityKeypair::generate();
            let cfg = MeshNodeConfig::new(
                SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
                [0u8; 32],
            );
            Arc::new(MeshNode::new(identity, cfg).await.unwrap())
        });

        let r = redex();
        let boxed_arc: *mut Arc<MeshNode> = Box::into_raw(Box::new(mesh.clone()));
        // Minimal config — just disable intent matching so the
        // empty-registry path doesn't gate us.
        let cfg_json = CString::new(r#"{"intent_match":"disabled"}"#).unwrap();
        let rc = net_redex_enable_greedy_dataforts(r, boxed_arc, cfg_json.as_ptr());
        assert_eq!(rc, 0, "enable must succeed");

        // No channels yet — count is 0.
        assert_eq!(net_redex_greedy_cached_channel_count(r), 0);

        // Prometheus text is non-null and contains the metric
        // family header.
        let p = net_redex_greedy_prometheus_text(r);
        assert!(!p.is_null());
        let text = unsafe { std::ffi::CStr::from_ptr(p) }.to_string_lossy().into_owned();
        unsafe { super::super::net_free_string(p); }
        assert!(
            text.contains("dataforts_greedy_admit_rejected_total"),
            "Prometheus text must include the admit-rejected metric family"
        );

        // Uninstall + verify.
        assert_eq!(net_redex_disable_greedy_dataforts(r), 0);
        let p_after = net_redex_greedy_prometheus_text(r);
        assert!(!p_after.is_null());
        let after_text = unsafe { std::ffi::CStr::from_ptr(p_after) }
            .to_string_lossy()
            .into_owned();
        unsafe {
            super::super::net_free_string(p_after);
        }
        assert!(
            after_text.is_empty(),
            "post-disable Prometheus text must be empty; got {after_text:?}"
        );

        net_redex_free(r);
    }
}
