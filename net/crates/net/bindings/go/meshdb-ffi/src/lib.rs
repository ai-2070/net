//! C FFI for the MeshDB query layer.
//!
//! # Scope
//!
//! Slice 1 of the Go SDK roadmap (matches the Python / Node
//! slice 1 scope): atomic factories (`at` / `between` /
//! `latest`), in-memory `ChainReader` populator, synchronous
//! execute returning a result-iterator handle. Composite
//! operators (count / sum / avg / window / join / filter) +
//! the Phase F cache surface + the builder API land in
//! follow-up slices.
//!
//! # Symbol naming
//!
//! `net_meshdb_<noun>_<verb>` mirroring the existing `ffi::cortex`
//! convention. Symbols are unconditionally exported when the
//! crate is built with the `meshdb` Cargo feature.
//!
//! # Error codes
//!
//! Functions return `i32` status codes:
//! - `0` — OK
//! - `1` — End of iterator (specific to `iter_next`)
//! - `2` — Invalid argument (null pointer, out-of-range, etc.)
//! - `3` — Runtime / executor error
//!
//! The detail message for non-OK statuses is fetched via
//! `net_meshdb_last_error_message()` (the latest message
//! populated on the calling thread; the pointer is valid until
//! the next FFI call on the same thread). The structured kind
//! discriminator — one of the [`MeshError`] variants
//! (`"planner_error"`, `"executor_error"`,
//! `"historical_range_unavailable"`, `"query_cancelled"`,
//! `"runtime_panic"`, etc.) — is available via
//! `net_meshdb_last_error_kind()`.

use std::cell::RefCell;
use std::ffi::{c_char, c_int, CString};
use std::panic::AssertUnwindSafe;
use std::ptr;
use std::slice;
use std::sync::Arc;

use tokio::runtime::Runtime;

use net::adapter::net::behavior::meshdb::MeshError;
use net::adapter::net::behavior::meshdb::{
    executor::{ChainReader, ExecuteOptions, LocalMeshQueryExecutor, MeshQueryExecutor},
    planner::{CostEstimate, OperatorNode, OperatorPlan},
    query::ResultRow,
    ExecutionPlan, SeqNum,
};

/// Status code: function returned successfully.
pub const NET_MESHDB_OK: c_int = 0;
/// Status code: iterator has no more rows.
pub const NET_MESHDB_END: c_int = 1;
/// Status code: caller passed an invalid argument.
pub const NET_MESHDB_INVALID_ARG: c_int = 2;
/// Status code: planner or executor surfaced an error.
pub const NET_MESHDB_RUNTIME_ERR: c_int = 3;

// =====================================================================
// Thread-local last-error reporting
// =====================================================================
//
// FFI errors flow through a per-thread "last error" pair (message
// + kind). Callers retrieve both via the `_last_error_*` getters;
// pointers stay valid until the next FFI call on the same thread
// touches the thread-local. Both panics from the async closure and
// `MeshError`s from the executor populate this.

thread_local! {
    static LAST_ERROR_MESSAGE: RefCell<Option<CString>> = const { RefCell::new(None) };
    static LAST_ERROR_KIND: RefCell<Option<CString>> = const { RefCell::new(None) };
}

fn set_last_error_from_mesh(err: &MeshError) {
    let msg = CString::new(err.to_string()).ok();
    let kind = CString::new(mesh_error_kind(err)).ok();
    LAST_ERROR_MESSAGE.with(|c| *c.borrow_mut() = msg);
    LAST_ERROR_KIND.with(|c| *c.borrow_mut() = kind);
}

fn set_last_error_from_panic(payload: &(dyn std::any::Any + Send)) {
    // Best-effort extraction of the panic message; both
    // `&str` and `String` are the common payload shapes.
    let detail = if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic across FFI boundary".to_string()
    };
    let msg = CString::new(format!("runtime panic: {detail}")).ok();
    let kind = CString::new("runtime_panic").ok();
    LAST_ERROR_MESSAGE.with(|c| *c.borrow_mut() = msg);
    LAST_ERROR_KIND.with(|c| *c.borrow_mut() = kind);
}

fn set_last_error_static(message: &str, kind: &str) {
    let msg = CString::new(message).ok();
    let kind = CString::new(kind).ok();
    LAST_ERROR_MESSAGE.with(|c| *c.borrow_mut() = msg);
    LAST_ERROR_KIND.with(|c| *c.borrow_mut() = kind);
}

fn clear_last_error() {
    LAST_ERROR_MESSAGE.with(|c| *c.borrow_mut() = None);
    LAST_ERROR_KIND.with(|c| *c.borrow_mut() = None);
}

/// Map a [`MeshError`] to a stable string discriminator.
/// Delegates to [`MeshError::kind`] in the substrate so all
/// three FFI shims (Python / Node / Go) report the same
/// `kind` string for the same variant.
fn mesh_error_kind(err: &MeshError) -> &'static str {
    err.kind()
}

/// Return the most recent error message recorded on this
/// thread, or NULL if there is none. The pointer is valid
/// until the next FFI call on the same thread that touches
/// the thread-local. Callers must not free the returned
/// pointer.
#[no_mangle]
pub extern "C" fn net_meshdb_last_error_message() -> *const c_char {
    LAST_ERROR_MESSAGE.with(|c| match &*c.borrow() {
        Some(s) => s.as_ptr(),
        None => ptr::null(),
    })
}

/// Return the most recent error kind recorded on this thread,
/// or NULL if there is none. Same lifetime rules as
/// `net_meshdb_last_error_message`.
#[no_mangle]
pub extern "C" fn net_meshdb_last_error_kind() -> *const c_char {
    LAST_ERROR_KIND.with(|c| match &*c.borrow() {
        Some(s) => s.as_ptr(),
        None => ptr::null(),
    })
}

/// Clear the thread-local last-error state.
#[no_mangle]
pub extern "C" fn net_meshdb_clear_last_error() {
    clear_last_error();
}

/// Wrap an FFI entry-point body in `catch_unwind`. Unwinding
/// across `extern "C"` is undefined behaviour; any panic that
/// escapes a guarded body is trapped, recorded as the thread-
/// local last-error pair with kind `"runtime_panic"`, and the
/// supplied `$default` is returned. Use:
///
/// ```ignore
/// pub extern "C" fn net_meshdb_thing() -> *mut Foo {
///     ffi_guard!(ptr::null_mut(), {
///         // body...
///     })
/// }
/// ```
///
/// The two `runner_execute*` paths use a hand-rolled
/// `catch_unwind` because they already need an inner closure
/// around the tokio `block_on`; everywhere else uses this
/// macro for uniformity.
macro_rules! ffi_guard {
    ($default:expr, $body:block) => {{
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| $body)) {
            Ok(v) => v,
            Err(payload) => {
                set_last_error_from_panic(&*payload);
                $default
            }
        }
    }};
}

/// Opaque handle to a Phase F-less local executor over an
/// in-memory chain reader. Holds its own `Arc<InMemoryStore>`
/// clone so the runner survives even if the caller frees the
/// reader handle first.
pub struct MeshDbRunner {
    runtime: Arc<Runtime>,
    executor: Arc<LocalMeshQueryExecutor<InMemoryStore>>,
}

/// Opaque handle to a planned query.
pub struct MeshDbQuery {
    plan: ExecutionPlan,
}

/// Opaque handle to an in-progress query's row stream. Iterate
/// via `net_meshdb_iter_next`; free via `net_meshdb_iter_free`.
pub struct MeshDbIter {
    /// Pre-drained rows. Slice 1 collects all rows eagerly (the
    /// local executor returns finite results); slice 2 may
    /// switch to lazy iteration once we wire continuation
    /// tokens through the FFI.
    rows: Vec<ResultRow>,
    next_idx: usize,
}

/// In-memory chain reader populated via `net_meshdb_reader_append`.
#[derive(Default)]
pub struct InMemoryStore {
    chains: std::sync::Mutex<
        std::collections::BTreeMap<u64, std::collections::BTreeMap<SeqNum, Vec<u8>>>,
    >,
}

impl ChainReader for InMemoryStore {
    fn read_one(&self, origin: u64, seq: SeqNum) -> Option<Vec<u8>> {
        self.chains.lock().unwrap().get(&origin)?.get(&seq).cloned()
    }

    fn read_range(&self, origin: u64, start: SeqNum, end: SeqNum) -> Vec<(SeqNum, Vec<u8>)> {
        self.chains
            .lock()
            .unwrap()
            .get(&origin)
            .map(|chain| {
                chain
                    .range(start..end)
                    .map(|(s, p)| (*s, p.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn latest_seq(&self, origin: u64) -> Option<SeqNum> {
        self.chains
            .lock()
            .unwrap()
            .get(&origin)?
            .keys()
            .next_back()
            .copied()
    }
}

// =====================================================================
// Reader
// =====================================================================

/// Reader handle type. Wraps an `Arc<InMemoryStore>` so the
/// reader can outlive the original pointer when a
/// `MeshDbRunner` holds a clone of the Arc.
pub struct MeshDbReader {
    store: Arc<InMemoryStore>,
}

/// Allocate an in-memory `ChainReader`. Free with
/// `net_meshdb_reader_free`.
///
/// # Safety
/// Allocates a heap object; caller owns the returned pointer.
#[no_mangle]
pub extern "C" fn net_meshdb_reader_new() -> *mut MeshDbReader {
    ffi_guard!(ptr::null_mut(), {
        Box::into_raw(Box::new(MeshDbReader {
            store: Arc::new(InMemoryStore::default()),
        }))
    })
}

/// Free a reader handle. No-op on null.
///
/// Freeing the reader does NOT tear down a `MeshDbRunner`
/// built from it — the runner holds its own
/// `Arc<InMemoryStore>` clone and stays usable. But once the
/// reader is freed, calling `net_meshdb_reader_append` on
/// that pointer is undefined behaviour (use-after-free). If
/// you intend to keep appending after a runner is built,
/// keep the reader alive too; otherwise free it after your
/// last append.
///
/// # Safety
/// `reader` must be a pointer returned by
/// `net_meshdb_reader_new`, or null. Must not have been freed
/// already.
#[no_mangle]
pub unsafe extern "C" fn net_meshdb_reader_free(reader: *mut MeshDbReader) {
    ffi_guard!((), {
        if reader.is_null() {
            return;
        }
        drop(Box::from_raw(reader));
    })
}

/// Append `(origin, seq, payload)` to the reader.
///
/// New rows are visible to every `MeshDbRunner` that was
/// constructed from this reader before or after the append —
/// they share the same underlying `Arc<InMemoryStore>`.
///
/// # Safety
/// `reader` must be a valid pointer returned by
/// `net_meshdb_reader_new` and not yet freed — calling this
/// after `net_meshdb_reader_free(reader)` is a use-after-free.
/// `payload` must be a valid pointer to `payload_len` bytes,
/// or null when `payload_len == 0`.
#[no_mangle]
pub unsafe extern "C" fn net_meshdb_reader_append(
    reader: *mut MeshDbReader,
    origin: u64,
    seq: u64,
    payload: *const u8,
    payload_len: usize,
) -> c_int {
    ffi_guard!(NET_MESHDB_RUNTIME_ERR, {
        if reader.is_null() {
            return NET_MESHDB_INVALID_ARG;
        }
        let payload_vec = if payload_len == 0 {
            Vec::new()
        } else if payload.is_null() {
            return NET_MESHDB_INVALID_ARG;
        } else {
            slice::from_raw_parts(payload, payload_len).to_vec()
        };
        let reader_ref = &*reader;
        reader_ref
            .store
            .chains
            .lock()
            .unwrap()
            .entry(origin)
            .or_default()
            .insert(SeqNum(seq), payload_vec);
        NET_MESHDB_OK
    })
}

// =====================================================================
// Query
// =====================================================================

/// Construct an `At(origin, seq)` query.
///
/// # Safety
/// Allocates a heap object; caller owns the returned pointer.
/// Free with `net_meshdb_query_free`.
#[no_mangle]
pub extern "C" fn net_meshdb_query_at(origin: u64, seq: u64) -> *mut MeshDbQuery {
    ffi_guard!(ptr::null_mut(), {
        let plan = plan_of(OperatorPlan::AtRead {
            origin,
            seq: SeqNum(seq),
        });
        Box::into_raw(Box::new(MeshDbQuery { plan }))
    })
}

/// Construct a `Between(origin, start, end)` query (half-open).
/// Returns null when `start >= end`.
#[no_mangle]
pub extern "C" fn net_meshdb_query_between(origin: u64, start: u64, end: u64) -> *mut MeshDbQuery {
    ffi_guard!(ptr::null_mut(), {
        if start >= end {
            return ptr::null_mut();
        }
        let plan = plan_of(OperatorPlan::BetweenRead {
            origin,
            start: SeqNum(start),
            end: SeqNum(end),
        });
        Box::into_raw(Box::new(MeshDbQuery { plan }))
    })
}

/// Construct a `Latest(origin)` query.
#[no_mangle]
pub extern "C" fn net_meshdb_query_latest(origin: u64) -> *mut MeshDbQuery {
    ffi_guard!(ptr::null_mut(), {
        let plan = plan_of(OperatorPlan::LatestRead { origin });
        Box::into_raw(Box::new(MeshDbQuery { plan }))
    })
}

// =====================================================================
// Slice 2: composite factories
// =====================================================================
//
// Slice 2 group-by encoding: a C-string with comma-separated row-
// intrinsic field names. `""` / null → no grouping; `"origin"`,
// `"seq"`, `"origin,seq"` map to the typed `JoinKeyMode`. Other
// values surface as null (caller treats as invalid args).

use net::adapter::net::behavior::meshdb::planner::{
    JoinKeyMode, JoinStrategy, LineageDirection, LineageEntry,
};
use net::adapter::net::behavior::meshdb::query::{
    JoinKind, NumericAggregateKind, NumericReductionKind, WindowSpec,
};
use net::adapter::net::behavior::predicate::Predicate;
use net::adapter::net::behavior::tag::{TagKey, TaxonomyAxis};

unsafe fn parse_group_by_cstr(
    group_by: *const std::ffi::c_char,
) -> std::result::Result<Option<JoinKeyMode>, ()> {
    if group_by.is_null() {
        return Ok(None);
    }
    let s = std::ffi::CStr::from_ptr(group_by)
        .to_str()
        .map_err(|_| ())?;
    if s.is_empty() {
        return Ok(None);
    }
    // Canonical group_by tokens across all shims:
    // "origin", "seq", "origin,seq". The variant "seq,origin"
    // was tolerated in earlier slices but is now rejected —
    // cross-language conformance tests need one canonical
    // encoding.
    match s {
        "origin" => Ok(Some(JoinKeyMode::Origin)),
        "seq" => Ok(Some(JoinKeyMode::Seq)),
        "origin,seq" => Ok(Some(JoinKeyMode::OriginSeq)),
        _ => Err(()),
    }
}

unsafe fn cstr_to_string(s: *const std::ffi::c_char) -> Option<String> {
    if s.is_null() {
        return None;
    }
    std::ffi::CStr::from_ptr(s)
        .to_str()
        .ok()
        .map(|x| x.to_string())
}

/// `Window(inner, size)` — tumbling on seq. Returns null when
/// `size == 0`. `inner` is NOT consumed (caller still owns).
///
/// # Safety
/// `inner` must be a valid `MeshDbQuery*` or null.
#[no_mangle]
pub unsafe extern "C" fn net_meshdb_query_window(
    inner: *const MeshDbQuery,
    size: u64,
) -> *mut MeshDbQuery {
    ffi_guard!(ptr::null_mut(), {
        if inner.is_null() || size == 0 {
            return ptr::null_mut();
        }
        let inner_node = (&*inner).plan.root.clone();
        let plan = plan_of(OperatorPlan::Window {
            input: Box::new(inner_node),
            spec: WindowSpec::TumblingSeq { size },
        });
        Box::into_raw(Box::new(MeshDbQuery { plan }))
    })
}

/// `Count(inner, group_by)`. `group_by` is a comma-separated
/// C-string of row-intrinsic field names; null / empty for a
/// single bucket.
///
/// # Safety
/// `inner` valid pointer; `group_by` null or valid C-string.
#[no_mangle]
pub unsafe extern "C" fn net_meshdb_query_count(
    inner: *const MeshDbQuery,
    group_by: *const std::ffi::c_char,
) -> *mut MeshDbQuery {
    ffi_guard!(ptr::null_mut(), {
        if inner.is_null() {
            return ptr::null_mut();
        }
        let Ok(group_by_mode) = parse_group_by_cstr(group_by) else {
            return ptr::null_mut();
        };
        let inner_node = (&*inner).plan.root.clone();
        let plan = plan_of(OperatorPlan::AggregateCount {
            input: Box::new(inner_node),
            group_by: group_by_mode,
        });
        Box::into_raw(Box::new(MeshDbQuery { plan }))
    })
}

/// `Sum / Avg / Min / Max / DistinctCount` aggregates. `kind` is
/// one of: `"sum"`, `"avg"`, `"min"`, `"max"`, `"distinct_count"`.
/// `field` is a row-intrinsic name (`"origin"` / `"seq"`) or a
/// dotted JSON path. Returns null for unknown `kind` or invalid
/// args.
///
/// # Safety
/// `inner` valid pointer; `kind` + `field` valid C-strings.
#[no_mangle]
pub unsafe extern "C" fn net_meshdb_query_numeric_agg(
    inner: *const MeshDbQuery,
    kind: *const std::ffi::c_char,
    field: *const std::ffi::c_char,
    group_by: *const std::ffi::c_char,
) -> *mut MeshDbQuery {
    ffi_guard!(ptr::null_mut(), {
        if inner.is_null() {
            return ptr::null_mut();
        }
        let (Some(kind), Some(field)) = (cstr_to_string(kind), cstr_to_string(field)) else {
            return ptr::null_mut();
        };
        let Ok(group_by_mode) = parse_group_by_cstr(group_by) else {
            return ptr::null_mut();
        };
        let inner_node = (&*inner).plan.root.clone();
        let op = match kind.as_str() {
            "sum" => OperatorPlan::AggregateNumeric {
                input: Box::new(inner_node),
                group_by: group_by_mode,
                field_path: field,
                kind: NumericAggregateKind::Sum,
            },
            "avg" => OperatorPlan::AggregateNumeric {
                input: Box::new(inner_node),
                group_by: group_by_mode,
                field_path: field,
                kind: NumericAggregateKind::Avg,
            },
            "min" => OperatorPlan::AggregateReduction {
                input: Box::new(inner_node),
                group_by: group_by_mode,
                field_path: field,
                kind: NumericReductionKind::Min,
            },
            "max" => OperatorPlan::AggregateReduction {
                input: Box::new(inner_node),
                group_by: group_by_mode,
                field_path: field,
                kind: NumericReductionKind::Max,
            },
            "distinct_count" => OperatorPlan::AggregateDistinct {
                input: Box::new(inner_node),
                group_by: group_by_mode,
                field_path: field,
            },
            _ => return ptr::null_mut(),
        };
        Box::into_raw(Box::new(MeshDbQuery { plan: plan_of(op) }))
    })
}

/// `Percentile(inner, field, p)`. `p` must be finite in
/// `[0.0, 1.0]`. Returns null otherwise.
///
/// # Safety
/// `inner` valid pointer; `field` valid C-string.
#[no_mangle]
pub unsafe extern "C" fn net_meshdb_query_percentile(
    inner: *const MeshDbQuery,
    field: *const std::ffi::c_char,
    p: f64,
    group_by: *const std::ffi::c_char,
) -> *mut MeshDbQuery {
    ffi_guard!(ptr::null_mut(), {
        if inner.is_null() || !p.is_finite() || !(0.0..=1.0).contains(&p) {
            return ptr::null_mut();
        }
        let Some(field) = cstr_to_string(field) else {
            return ptr::null_mut();
        };
        let Ok(group_by_mode) = parse_group_by_cstr(group_by) else {
            return ptr::null_mut();
        };
        let inner_node = (&*inner).plan.root.clone();
        let plan = plan_of(OperatorPlan::AggregateReduction {
            input: Box::new(inner_node),
            group_by: group_by_mode,
            field_path: field,
            kind: NumericReductionKind::Percentile { p },
        });
        Box::into_raw(Box::new(MeshDbQuery { plan }))
    })
}

/// Hash-join two queries. `kind` is one of `"inner"` /
/// `"left_outer"` / `"right_outer"` / `"full_outer"`. `key` is
/// the field name shared by both sides — `"origin"` / `"seq"` /
/// `"origin,seq"` map to the typed enum, anything else is a
/// JSON payload path. `strategy` is `"hash_broadcast"` (default)
/// or `"sort_merge"`. `watermark_secs` ≥ 0 (informational under
/// snapshot semantics; 5.0 is the canonical default).
///
/// # Safety
/// All pointers valid (left/right `MeshDbQuery*`; kind/key
/// non-null; strategy nullable).
#[no_mangle]
pub unsafe extern "C" fn net_meshdb_query_join(
    left: *const MeshDbQuery,
    right: *const MeshDbQuery,
    kind: *const std::ffi::c_char,
    key: *const std::ffi::c_char,
    strategy: *const std::ffi::c_char,
    watermark_secs: f64,
) -> *mut MeshDbQuery {
    ffi_guard!(ptr::null_mut(), {
        if left.is_null() || right.is_null() {
            return ptr::null_mut();
        }
        let (Some(kind), Some(key)) = (cstr_to_string(kind), cstr_to_string(key)) else {
            return ptr::null_mut();
        };
        let kind = match kind.as_str() {
            "inner" => JoinKind::Inner,
            "left_outer" => JoinKind::LeftOuter,
            "right_outer" => JoinKind::RightOuter,
            "full_outer" => JoinKind::FullOuter,
            _ => return ptr::null_mut(),
        };
        let strategy_str = cstr_to_string(strategy);
        let strategy = match strategy_str.as_deref() {
            None | Some("") | Some("hash_broadcast") => JoinStrategy::HashBroadcast,
            Some("sort_merge") => JoinStrategy::SortMerge,
            _ => return ptr::null_mut(),
        };
        // Canonical join key tokens across all shims:
        // "origin", "seq", "origin,seq". Anything else is treated
        // as a dotted JSON field path. "seq,origin" was tolerated
        // in earlier slices but is now rejected.
        let key_mode = match key.as_str() {
            "origin" => JoinKeyMode::Origin,
            "seq" => JoinKeyMode::Seq,
            "origin,seq" => JoinKeyMode::OriginSeq,
            other => JoinKeyMode::Field(other.to_string()),
        };
        let watermark_secs = if watermark_secs.is_finite() && watermark_secs >= 0.0 {
            watermark_secs
        } else {
            5.0
        };
        let plan = plan_of(OperatorPlan::HashJoin {
            left: Box::new((&*left).plan.root.clone()),
            right: Box::new((&*right).plan.root.clone()),
            key_mode,
            kind,
            strategy,
            watermark: std::time::Duration::from_secs_f64(watermark_secs),
        });
        Box::into_raw(Box::new(MeshDbQuery { plan }))
    })
}

/// `LineageEmit(origin, entries, direction)`. Emits one row per
/// pre-walked entry; the SDK doesn't itself walk the fork-of:
/// graph. `entries_json` is a JSON array of
/// `{"origin":N,"depth":N,"tip_seq":N|null}` objects in walk
/// order. `direction` is `"back"` or `"forward"`. Returns null
/// on JSON parse error or invalid direction.
///
/// # Safety
/// `entries_json` and `direction` must be valid UTF-8 C-strings.
#[no_mangle]
pub unsafe extern "C" fn net_meshdb_query_lineage_emit(
    origin: u64,
    entries_json: *const std::ffi::c_char,
    direction: *const std::ffi::c_char,
) -> *mut MeshDbQuery {
    ffi_guard!(ptr::null_mut(), {
        if entries_json.is_null() || direction.is_null() {
            return ptr::null_mut();
        }
        let Ok(json) = std::ffi::CStr::from_ptr(entries_json).to_str() else {
            return ptr::null_mut();
        };
        let Ok(dir_str) = std::ffi::CStr::from_ptr(direction).to_str() else {
            return ptr::null_mut();
        };
        let direction = match dir_str {
            "back" => LineageDirection::Back,
            "forward" => LineageDirection::Forward,
            _ => return ptr::null_mut(),
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
            return ptr::null_mut();
        };
        let Some(arr) = value.as_array() else {
            return ptr::null_mut();
        };
        let mut entries = Vec::with_capacity(arr.len());
        for e in arr {
            let Some(obj) = e.as_object() else {
                return ptr::null_mut();
            };
            let Some(entry_origin) = obj.get("origin").and_then(|x| x.as_u64()) else {
                return ptr::null_mut();
            };
            let Some(depth) = obj.get("depth").and_then(|x| x.as_u64()) else {
                return ptr::null_mut();
            };
            let Ok(depth) = u32::try_from(depth) else {
                return ptr::null_mut();
            };
            let tip_seq = match obj.get("tip_seq") {
                None | Some(serde_json::Value::Null) => None,
                Some(v) => match v.as_u64() {
                    Some(s) => Some(SeqNum(s)),
                    None => return ptr::null_mut(),
                },
            };
            entries.push(LineageEntry {
                origin: entry_origin,
                depth,
                tip_seq,
            });
        }
        let plan = plan_of(OperatorPlan::LineageEmit {
            origin,
            direction,
            entries,
        });
        Box::into_raw(Box::new(MeshDbQuery { plan }))
    })
}

// =====================================================================
// Slice 3: Filter + Predicate (JSON-encoded predicate)
// =====================================================================
//
// The FFI accepts the predicate as a JSON object — same shape
// the Python / Node SDKs build internally. Parsed here into the
// typed `Predicate`, converted to PredicateWire, wrapped in a
// Filter operator.
//
// JSON shape (mirrors Python `Predicate` factories):
// - `{"kind":"exists","field":"<name>"}`
// - `{"kind":"equals","field":"<name>","value":"<str>"}`
// - `{"kind":"numeric_at_least","field":"<name>","threshold":N}`
// - `{"kind":"numeric_at_most","field":"<name>","threshold":N}`
// - `{"kind":"numeric_in_range","field":"<name>","min":N,"max":N}`
// - `{"kind":"string_prefix","field":"<name>","prefix":"<str>"}`
// - `{"kind":"string_matches","field":"<name>","pattern":"<str>"}`
// - `{"kind":"semver_at_least","field":"<name>","version":"<str>"}`
// - `{"kind":"and","children":[<pred>,...]}`
// - `{"kind":"or","children":[<pred>,...]}`
// - `{"kind":"not","child":<pred>}`
//
// Field names are row-intrinsic (`origin` / `seq`) or JSON
// payload paths; matching is done against the synthetic per-row
// tag view (see `row::synthetic_row_view`).

/// Construct a `Filter(inner, predicate)` query. The predicate
/// is a JSON object describing one of the shapes above; see the
/// module comment for the schema. Returns null on parse error
/// or any invalid argument.
///
/// # Safety
/// `inner` valid pointer; `predicate_json` valid C-string of
/// UTF-8 JSON.
#[no_mangle]
pub unsafe extern "C" fn net_meshdb_query_filter_json(
    inner: *const MeshDbQuery,
    predicate_json: *const std::ffi::c_char,
) -> *mut MeshDbQuery {
    ffi_guard!(ptr::null_mut(), {
        if inner.is_null() || predicate_json.is_null() {
            return ptr::null_mut();
        }
        let Ok(json) = std::ffi::CStr::from_ptr(predicate_json).to_str() else {
            return ptr::null_mut();
        };
        let Ok(predicate) = parse_predicate_json(json) else {
            return ptr::null_mut();
        };
        let inner_node = (&*inner).plan.root.clone();
        let plan = plan_of(OperatorPlan::Filter {
            input: Box::new(inner_node),
            predicate: predicate.to_wire(),
        });
        Box::into_raw(Box::new(MeshDbQuery { plan }))
    })
}

fn parse_predicate_json(json: &str) -> std::result::Result<Predicate, ()> {
    let value: serde_json::Value = serde_json::from_str(json).map_err(|_| ())?;
    parse_predicate_value(&value)
}

fn parse_predicate_value(v: &serde_json::Value) -> std::result::Result<Predicate, ()> {
    let obj = v.as_object().ok_or(())?;
    let kind = obj.get("kind").and_then(|k| k.as_str()).ok_or(())?;
    let field = |k: &str| -> std::result::Result<TagKey, ()> {
        let f = obj.get(k).and_then(|x| x.as_str()).ok_or(())?;
        Ok(TagKey {
            axis: TaxonomyAxis::Dataforts,
            key: f.to_string(),
        })
    };
    match kind {
        "exists" => Ok(Predicate::Exists {
            key: field("field")?,
        }),
        "equals" => {
            let value = obj.get("value").and_then(|x| x.as_str()).ok_or(())?;
            Ok(Predicate::Equals {
                key: field("field")?,
                value: value.to_string(),
            })
        }
        "numeric_at_least" => {
            let threshold = obj.get("threshold").and_then(|x| x.as_f64()).ok_or(())?;
            Ok(Predicate::NumericAtLeast {
                key: field("field")?,
                threshold,
            })
        }
        "numeric_at_most" => {
            let threshold = obj.get("threshold").and_then(|x| x.as_f64()).ok_or(())?;
            Ok(Predicate::NumericAtMost {
                key: field("field")?,
                threshold,
            })
        }
        "numeric_in_range" => {
            let min = obj.get("min").and_then(|x| x.as_f64()).ok_or(())?;
            let max = obj.get("max").and_then(|x| x.as_f64()).ok_or(())?;
            if min > max {
                return Err(());
            }
            Ok(Predicate::NumericInRange {
                key: field("field")?,
                min,
                max,
            })
        }
        "string_prefix" => {
            let prefix = obj.get("prefix").and_then(|x| x.as_str()).ok_or(())?;
            Ok(Predicate::StringPrefix {
                key: field("field")?,
                prefix: prefix.to_string(),
            })
        }
        "string_matches" => {
            let pattern = obj.get("pattern").and_then(|x| x.as_str()).ok_or(())?;
            Ok(Predicate::StringMatches {
                key: field("field")?,
                pattern: pattern.to_string(),
            })
        }
        "semver_at_least" => {
            let version = obj.get("version").and_then(|x| x.as_str()).ok_or(())?;
            Ok(Predicate::SemverAtLeast {
                key: field("field")?,
                version: version.to_string(),
            })
        }
        "and" | "or" => {
            let children_v = obj.get("children").and_then(|x| x.as_array()).ok_or(())?;
            let mut children = Vec::with_capacity(children_v.len());
            for c in children_v {
                children.push(parse_predicate_value(c)?);
            }
            if kind == "and" {
                Ok(Predicate::And(children))
            } else {
                Ok(Predicate::Or(children))
            }
        }
        "not" => {
            let child = obj.get("child").ok_or(())?;
            let inner = parse_predicate_value(child)?;
            Ok(Predicate::Not(Box::new(inner)))
        }
        _ => Err(()),
    }
}

// =====================================================================
// Slice 2: payload decoder (JSON intermediate)
// =====================================================================

/// Try to decode a result-row payload into a JSON description.
/// Returns a heap-allocated C-string on success, or null when
/// the payload doesn't deserialize as any known sentinel
/// envelope (atomic-operator rows return null — their payload
/// is event data, not a postcard envelope).
///
/// JSON shape per variant:
/// - Aggregate: `{"kind":"aggregate","group":{...|null},"value":{"kind":"count","count":42,"value":42.0}}`
/// - Joined: `{"kind":"joined","left":{...|null},"right":{...|null}}`
/// - Window: `{"kind":"window","start":N,"end":N,"rows":[{...},...]}`
///
/// Each row inside the JSON is `{"origin":N,"seq":N,"payload":"<base64>"}`.
/// Free the returned C-string with `net_meshdb_free_string`.
///
/// # Safety
/// `payload` must be a valid pointer to `payload_len` bytes
/// (or null when `payload_len == 0`).
#[no_mangle]
pub unsafe extern "C" fn net_meshdb_decode_payload_json(
    payload: *const u8,
    payload_len: usize,
) -> *mut std::ffi::c_char {
    ffi_guard!(ptr::null_mut(), {
        if payload_len == 0 || payload.is_null() {
            return ptr::null_mut();
        }
        let bytes = slice::from_raw_parts(payload, payload_len);
        let json = match decode_to_json(bytes) {
            Some(s) => s,
            None => return ptr::null_mut(),
        };
        match std::ffi::CString::new(json) {
            Ok(c) => c.into_raw(),
            Err(_) => ptr::null_mut(),
        }
    })
}

/// Free a string returned by `net_meshdb_decode_payload_json`.
///
/// # Safety
/// `s` must be a pointer returned by the decoder or null.
#[no_mangle]
pub unsafe extern "C" fn net_meshdb_free_string(s: *mut std::ffi::c_char) {
    ffi_guard!((), {
        if s.is_null() {
            return;
        }
        drop(std::ffi::CString::from_raw(s));
    })
}

fn decode_to_json(bytes: &[u8]) -> Option<String> {
    use net::adapter::net::behavior::meshdb::query::{
        AggregateRowPayload, JoinedRowPayload, WindowBoundary,
    };
    if let Ok(p) = postcard::from_bytes::<AggregateRowPayload>(bytes) {
        return Some(aggregate_to_json(&p));
    }
    if let Ok(p) = postcard::from_bytes::<JoinedRowPayload>(bytes) {
        return Some(joined_to_json(&p));
    }
    if let Ok(p) = postcard::from_bytes::<WindowBoundary>(bytes) {
        return Some(window_to_json(&p));
    }
    None
}

fn row_to_json_value(r: &ResultRow) -> String {
    // Payload is encoded as a JSON array of byte integers. Avoids
    // a base64 dep and lets Go decode trivially via the json
    // package. Higher-level wrappers can re-pack into []byte.
    let payload_bytes: Vec<String> = r.payload.iter().map(|b| b.to_string()).collect();
    format!(
        r#"{{"origin":{},"seq":{},"payload":[{}]}}"#,
        r.origin,
        r.seq.0,
        payload_bytes.join(",")
    )
}

fn group_key_to_json(g: &Option<net::adapter::net::behavior::meshdb::query::GroupKey>) -> String {
    use net::adapter::net::behavior::meshdb::query::GroupKey as GK;
    match g {
        None => "null".to_string(),
        Some(GK::Origin(o)) => format!(r#"{{"kind":"origin","origin":{o}}}"#),
        Some(GK::Seq(s)) => format!(r#"{{"kind":"seq","seq":{}}}"#, s.0),
        Some(GK::OriginSeq { origin, seq }) => format!(
            r#"{{"kind":"origin_seq","origin":{origin},"seq":{}}}"#,
            seq.0
        ),
    }
}

fn aggregate_value_to_json(
    v: &net::adapter::net::behavior::meshdb::query::AggregateValue,
) -> String {
    use net::adapter::net::behavior::meshdb::query::AggregateValue as AV;
    let (kind, value, count) = match v {
        AV::Count(c) => ("count", Some(*c as f64), Some(*c)),
        AV::Sum(s) => ("sum", Some(*s), None),
        AV::Avg(opt) => ("avg", *opt, None),
        AV::Min(opt) => ("min", *opt, None),
        AV::Max(opt) => ("max", *opt, None),
        AV::DistinctCount(c) => ("distinct_count", Some(*c as f64), Some(*c)),
        AV::Percentile(opt) => ("percentile", *opt, None),
        _ => ("unknown", None, None),
    };
    let value_json = value
        .map(|v| v.to_string())
        .unwrap_or_else(|| "null".to_string());
    let count_json = count
        .map(|c| c.to_string())
        .unwrap_or_else(|| "null".to_string());
    format!(r#"{{"kind":"{kind}","value":{value_json},"count":{count_json}}}"#)
}

fn aggregate_to_json(
    p: &net::adapter::net::behavior::meshdb::query::AggregateRowPayload,
) -> String {
    format!(
        r#"{{"kind":"aggregate","group":{},"value":{}}}"#,
        group_key_to_json(&p.group),
        aggregate_value_to_json(&p.value)
    )
}

fn joined_to_json(p: &net::adapter::net::behavior::meshdb::query::JoinedRowPayload) -> String {
    let left = p
        .left
        .as_ref()
        .map(row_to_json_value)
        .unwrap_or_else(|| "null".to_string());
    let right = p
        .right
        .as_ref()
        .map(row_to_json_value)
        .unwrap_or_else(|| "null".to_string());
    format!(r#"{{"kind":"joined","left":{left},"right":{right}}}"#)
}

fn window_to_json(b: &net::adapter::net::behavior::meshdb::query::WindowBoundary) -> String {
    let rows: Vec<String> = b.rows.iter().map(row_to_json_value).collect();
    format!(
        r#"{{"kind":"window","start":{},"end":{},"rows":[{}]}}"#,
        b.start.0,
        b.end.0,
        rows.join(",")
    )
}

/// Free a query handle. No-op on null.
///
/// # Safety
/// `query` must be a pointer returned by `net_meshdb_query_*`
/// or null. Must not have been freed already.
#[no_mangle]
pub unsafe extern "C" fn net_meshdb_query_free(query: *mut MeshDbQuery) {
    ffi_guard!((), {
        if query.is_null() {
            return;
        }
        drop(Box::from_raw(query));
    })
}

// =====================================================================
// Runner + execute
// =====================================================================

/// Shared Tokio runtime — one per loaded cdylib, not one per
/// runner. Spinning up a multi-thread runtime per runner was
/// meaningful overhead for test harnesses that construct many
/// runners; a single shared runtime suffices because each
/// `runner_execute` blocks the calling thread until the row
/// stream is drained anyway.
fn shared_runtime() -> std::io::Result<Arc<Runtime>> {
    use std::sync::OnceLock;
    static RT: OnceLock<Arc<Runtime>> = OnceLock::new();
    if let Some(rt) = RT.get() {
        return Ok(rt.clone());
    }
    let rt = Arc::new(Runtime::new()?);
    let _ = RT.set(rt.clone());
    Ok(RT.get().cloned().unwrap_or(rt))
}

/// Construct a runner that shares the given reader's
/// underlying store via `Arc` clone.
///
/// Ownership / lifetime — TWO valid patterns:
///
/// 1. **Snapshot then free**: append everything you need on
///    the reader, build the runner, then call
///    `net_meshdb_reader_free(reader)`. The runner stays
///    usable; further `reader_append` calls on the freed
///    pointer are UB.
/// 2. **Keep the reader alive**: do not free the reader
///    while you still want to call `reader_append` against
///    it. New appends are visible to the runner (same
///    underlying `Arc<InMemoryStore>`). Free the reader after
///    the last append (the runner is still usable).
///
/// What you must NOT do: free the reader and then continue
/// to `reader_append` against the freed pointer. The runner
/// alone is not sufficient to keep the reader-handle struct
/// alive.
///
/// # Safety
/// `reader` must be a valid pointer returned by
/// `net_meshdb_reader_new`, or null (which yields null).
#[no_mangle]
pub unsafe extern "C" fn net_meshdb_runner_new(reader: *mut MeshDbReader) -> *mut MeshDbRunner {
    ffi_guard!(ptr::null_mut(), {
        if reader.is_null() {
            return ptr::null_mut();
        }
        let store = (&*reader).store.clone();
        let runtime = match shared_runtime() {
            Ok(rt) => rt,
            Err(_) => return ptr::null_mut(),
        };
        let executor: LocalMeshQueryExecutor<InMemoryStore> = LocalMeshQueryExecutor::new(store);
        let runner = MeshDbRunner {
            runtime,
            executor: Arc::new(executor),
        };
        Box::into_raw(Box::new(runner))
    })
}

/// Construct a runner with the Phase F LRU result cache wired
/// in. Otherwise identical to `net_meshdb_runner_new`. The
/// capability-version closure is fixed at `0` because no
/// `CapabilityIndex` is plumbed through the Go FFI yet — the
/// cache is single-node-LRU only. Pull-invalidation across
/// version changes lands when the Go FFI grows a Phase B+
/// transport / federated-executor path.
///
/// # Safety
/// `reader` must be a valid pointer returned by
/// `net_meshdb_reader_new`, or null.
#[no_mangle]
pub unsafe extern "C" fn net_meshdb_runner_new_cached(
    reader: *mut MeshDbReader,
) -> *mut MeshDbRunner {
    ffi_guard!(ptr::null_mut(), {
        if reader.is_null() {
            return ptr::null_mut();
        }
        let store = (&*reader).store.clone();
        let runtime = match shared_runtime() {
            Ok(rt) => rt,
            Err(_) => return ptr::null_mut(),
        };
        let cache: Arc<dyn net::adapter::net::behavior::meshdb::cache::ResultCache> =
            Arc::new(net::adapter::net::behavior::meshdb::cache::LruResultCache::default());
        let version_fn: Arc<dyn Fn() -> u64 + Send + Sync> = Arc::new(|| 0);
        let executor: LocalMeshQueryExecutor<InMemoryStore> =
            LocalMeshQueryExecutor::with_cache(store, cache, version_fn);
        Box::into_raw(Box::new(MeshDbRunner {
            runtime,
            executor: Arc::new(executor),
        }))
    })
}

/// Free a runner handle.
///
/// # Safety
/// `runner` must be a pointer returned by `net_meshdb_runner_new`
/// or null.
#[no_mangle]
pub unsafe extern "C" fn net_meshdb_runner_free(runner: *mut MeshDbRunner) {
    ffi_guard!((), {
        if runner.is_null() {
            return;
        }
        drop(Box::from_raw(runner));
    })
}

/// Execute `query` on `runner`. Returns a heap-allocated
/// iterator handle on success, or null on error. The iterator
/// is drained eagerly; `net_meshdb_iter_next` then walks the
/// collected rows.
///
/// # Safety
/// Both `runner` and `query` must be valid pointers returned by
/// their respective `_new` functions, or null (which yields
/// null).
#[no_mangle]
pub unsafe extern "C" fn net_meshdb_runner_execute(
    runner: *mut MeshDbRunner,
    query: *mut MeshDbQuery,
) -> *mut MeshDbIter {
    if runner.is_null() || query.is_null() {
        set_last_error_static("null runner or query handle", "invalid_arg");
        return ptr::null_mut();
    }
    clear_last_error();
    let runner_ref = &*runner;
    let plan = (&*query).plan.clone();
    let executor = runner_ref.executor.clone();
    let runtime = runner_ref.runtime.clone();
    // catch_unwind: user-controlled operators (aggregate
    // div-by-zero, OOM hash-join) can panic inside the async
    // block. Unwinding across the C ABI is UB, so we trap
    // here and map to NET_MESHDB_RUNTIME_ERR with the panic
    // payload surfaced via net_meshdb_last_error_*.
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        runtime.block_on(async move {
            use futures::StreamExt;
            let running = executor
                .execute_with(plan, ExecuteOptions::default())
                .await?;
            let mut stream = running.rows;
            let mut out = Vec::new();
            while let Some(item) = stream.next().await {
                out.push(item?);
            }
            Ok::<Vec<ResultRow>, MeshError>(out)
        })
    }));
    match result {
        Ok(Ok(rows)) => Box::into_raw(Box::new(MeshDbIter { rows, next_idx: 0 })),
        Ok(Err(err)) => {
            set_last_error_from_mesh(&err);
            ptr::null_mut()
        }
        Err(panic_payload) => {
            set_last_error_from_panic(&*panic_payload);
            ptr::null_mut()
        }
    }
}

/// Phase F cache-policy discriminator for
/// `net_meshdb_runner_execute_with`. Permanent = 0,
/// TimeBound = 1.
pub const NET_MESHDB_CACHE_PERMANENT: c_int = 0;
/// TimeBound cache policy — `ttl_secs` field is consulted.
pub const NET_MESHDB_CACHE_TIME_BOUND: c_int = 1;

/// Execute `query` with explicit Phase F options. Matches the
/// Python / Node `execute_with` surface.
///
/// `bypass_cache` skips both lookup and writeback when non-zero.
/// `cache_policy_kind` is `0` for Permanent, `1` for TimeBound.
/// `cache_ttl_secs` is consulted only when policy is TimeBound;
/// pass `5.0` to match the default. Caller-side may pass any
/// non-finite / negative value to fall back to 5.0.
///
/// # Safety
/// Both `runner` and `query` must be valid pointers, or null.
#[no_mangle]
pub unsafe extern "C" fn net_meshdb_runner_execute_with(
    runner: *mut MeshDbRunner,
    query: *mut MeshDbQuery,
    bypass_cache: c_int,
    cache_policy_kind: c_int,
    cache_ttl_secs: f64,
) -> *mut MeshDbIter {
    if runner.is_null() || query.is_null() {
        set_last_error_static("null runner or query handle", "invalid_arg");
        return ptr::null_mut();
    }
    clear_last_error();
    let runner_ref = &*runner;
    let plan = (&*query).plan.clone();
    let executor = runner_ref.executor.clone();
    let runtime = runner_ref.runtime.clone();
    let cache_policy = match cache_policy_kind {
        NET_MESHDB_CACHE_PERMANENT => {
            net::adapter::net::behavior::meshdb::cache::CachePolicy::Permanent
        }
        _ => {
            // Default to TimeBound for any non-recognized kind;
            // the `ttl_secs` argument falls through to 5 s when
            // not finite / non-negative.
            let ttl_secs = if cache_ttl_secs.is_finite() && cache_ttl_secs >= 0.0 {
                cache_ttl_secs
            } else {
                5.0
            };
            net::adapter::net::behavior::meshdb::cache::CachePolicy::TimeBound {
                ttl: std::time::Duration::from_secs_f64(ttl_secs),
            }
        }
    };
    let options = ExecuteOptions {
        bypass_cache: bypass_cache != 0,
        cache_policy,
    };
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        runtime.block_on(async move {
            use futures::StreamExt;
            let running = executor.execute_with(plan, options).await?;
            let mut stream = running.rows;
            let mut out = Vec::new();
            while let Some(item) = stream.next().await {
                out.push(item?);
            }
            Ok::<Vec<ResultRow>, MeshError>(out)
        })
    }));
    match result {
        Ok(Ok(rows)) => Box::into_raw(Box::new(MeshDbIter { rows, next_idx: 0 })),
        Ok(Err(err)) => {
            set_last_error_from_mesh(&err);
            ptr::null_mut()
        }
        Err(panic_payload) => {
            set_last_error_from_panic(&*panic_payload);
            ptr::null_mut()
        }
    }
}

// =====================================================================
// Result iterator
// =====================================================================

/// Pull the next row from `iter`.
///
/// On `NET_MESHDB_OK`, populates `*origin_out`, `*seq_out`, and
/// `*payload_out_ptr` / `*payload_out_len`. The payload buffer
/// is heap-owned by libnet; callers MUST free it via
/// `net_meshdb_payload_free` (or simply let it leak — for the
/// short-lived test pattern, leaking is acceptable; production
/// consumers should free).
///
/// On `NET_MESHDB_END`, no out params are written.
///
/// # Safety
/// `iter` and the four out-pointer args must all be valid
/// (non-null where the function reads / writes them).
#[no_mangle]
pub unsafe extern "C" fn net_meshdb_iter_next(
    iter: *mut MeshDbIter,
    origin_out: *mut u64,
    seq_out: *mut u64,
    payload_out_ptr: *mut *mut u8,
    payload_out_len: *mut usize,
) -> c_int {
    ffi_guard!(NET_MESHDB_RUNTIME_ERR, {
        if iter.is_null()
            || origin_out.is_null()
            || seq_out.is_null()
            || payload_out_ptr.is_null()
            || payload_out_len.is_null()
        {
            return NET_MESHDB_INVALID_ARG;
        }
        let iter_ref = &mut *iter;
        if iter_ref.next_idx >= iter_ref.rows.len() {
            return NET_MESHDB_END;
        }
        let row = &iter_ref.rows[iter_ref.next_idx];
        iter_ref.next_idx += 1;
        *origin_out = row.origin;
        *seq_out = row.seq.0;
        // Copy the payload into a fresh heap allocation owned by
        // the caller — the rows Vec keeps its own copy intact for
        // iterator-state purposes, but transferring a borrowed
        // pointer would be unsound across the FFI boundary if the
        // iter is later freed.
        let payload = row.payload.clone();
        let mut boxed = payload.into_boxed_slice();
        let len = boxed.len();
        let ptr = boxed.as_mut_ptr();
        std::mem::forget(boxed);
        *payload_out_ptr = ptr;
        *payload_out_len = len;
        NET_MESHDB_OK
    })
}

/// Free a payload buffer returned by `net_meshdb_iter_next`.
///
/// # Safety
/// `ptr` must be a buffer returned by `net_meshdb_iter_next` (or
/// null). `len` must equal the length originally written by
/// `net_meshdb_iter_next` for that pointer.
#[no_mangle]
pub unsafe extern "C" fn net_meshdb_payload_free(ptr: *mut u8, len: usize) {
    ffi_guard!((), {
        if ptr.is_null() || len == 0 {
            return;
        }
        let boxed: Box<[u8]> = Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, len));
        drop(boxed);
    })
}

/// Free an iterator handle.
///
/// # Safety
/// `iter` must be a pointer returned by `net_meshdb_runner_execute`
/// or null.
#[no_mangle]
pub unsafe extern "C" fn net_meshdb_iter_free(iter: *mut MeshDbIter) {
    ffi_guard!((), {
        if iter.is_null() {
            return;
        }
        drop(Box::from_raw(iter));
    })
}

fn plan_of(op: OperatorPlan) -> ExecutionPlan {
    ExecutionPlan {
        root: OperatorNode {
            operator: op,
            target_nodes: vec![],
            cost: CostEstimate::default(),
        },
        total_cost: CostEstimate::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end: create a reader, append rows, build a Latest
    /// query, execute, drain via iter_next, verify the row.
    #[test]
    fn ffi_latest_end_to_end() {
        unsafe {
            let reader = net_meshdb_reader_new();
            assert!(!reader.is_null());

            let payload = b"v1".to_vec();
            assert_eq!(
                net_meshdb_reader_append(reader, 0xAB, 1, payload.as_ptr(), payload.len()),
                NET_MESHDB_OK
            );
            let payload2 = b"v2".to_vec();
            assert_eq!(
                net_meshdb_reader_append(reader, 0xAB, 2, payload2.as_ptr(), payload2.len()),
                NET_MESHDB_OK
            );

            let runner = net_meshdb_runner_new(reader);
            assert!(!runner.is_null());

            let query = net_meshdb_query_latest(0xAB);
            assert!(!query.is_null());

            let iter = net_meshdb_runner_execute(runner, query);
            assert!(!iter.is_null());

            // First call returns OK with the tip row.
            let mut origin: u64 = 0;
            let mut seq: u64 = 0;
            let mut payload_ptr: *mut u8 = ptr::null_mut();
            let mut payload_len: usize = 0;
            let status = net_meshdb_iter_next(
                iter,
                &mut origin,
                &mut seq,
                &mut payload_ptr,
                &mut payload_len,
            );
            assert_eq!(status, NET_MESHDB_OK);
            assert_eq!(origin, 0xAB);
            assert_eq!(seq, 2);
            let payload_slice = slice::from_raw_parts(payload_ptr, payload_len);
            assert_eq!(payload_slice, b"v2");
            net_meshdb_payload_free(payload_ptr, payload_len);

            // Second call returns END.
            let status = net_meshdb_iter_next(
                iter,
                &mut origin,
                &mut seq,
                &mut payload_ptr,
                &mut payload_len,
            );
            assert_eq!(status, NET_MESHDB_END);

            net_meshdb_iter_free(iter);
            net_meshdb_query_free(query);
            net_meshdb_runner_free(runner);
            net_meshdb_reader_free(reader);
        }
    }

    #[test]
    fn ffi_between_emits_rows_in_seq_order() {
        unsafe {
            let reader = net_meshdb_reader_new();
            for s in 1u64..=5 {
                let p = format!("p-{s}");
                net_meshdb_reader_append(reader, 0xCD, s, p.as_ptr(), p.len());
            }
            let runner = net_meshdb_runner_new(reader);
            let query = net_meshdb_query_between(0xCD, 2, 5);
            assert!(!query.is_null());
            let iter = net_meshdb_runner_execute(runner, query);

            let mut seqs: Vec<u64> = Vec::new();
            for _ in 0..10 {
                let mut origin: u64 = 0;
                let mut seq: u64 = 0;
                let mut p_ptr: *mut u8 = ptr::null_mut();
                let mut p_len: usize = 0;
                let status =
                    net_meshdb_iter_next(iter, &mut origin, &mut seq, &mut p_ptr, &mut p_len);
                if status == NET_MESHDB_END {
                    break;
                }
                assert_eq!(status, NET_MESHDB_OK);
                seqs.push(seq);
                net_meshdb_payload_free(p_ptr, p_len);
            }
            assert_eq!(seqs, vec![2, 3, 4]);

            net_meshdb_iter_free(iter);
            net_meshdb_query_free(query);
            net_meshdb_runner_free(runner);
            net_meshdb_reader_free(reader);
        }
    }

    #[test]
    fn between_with_inverted_range_returns_null() {
        let q = net_meshdb_query_between(0xAA, 5, 5);
        assert!(q.is_null());
    }

    /// Slice 2: count factory + JSON decoder end-to-end.
    /// Aggregate sentinel rows carry a postcard-encoded
    /// `AggregateRowPayload`; the JSON decoder turns it into a
    /// `{"kind":"aggregate", "group":..., "value":...}` shape.
    #[test]
    fn ffi_count_factory_and_json_decoder() {
        unsafe {
            let reader = net_meshdb_reader_new();
            for s in 1u64..=4 {
                net_meshdb_reader_append(reader, 0x01, s, ptr::null(), 0);
            }
            let runner = net_meshdb_runner_new(reader);
            let between = net_meshdb_query_between(0x01, 1, 10);
            let count = net_meshdb_query_count(between, ptr::null());
            assert!(!count.is_null());
            let iter = net_meshdb_runner_execute(runner, count);
            let mut origin: u64 = 0;
            let mut seq: u64 = 0;
            let mut p_ptr: *mut u8 = ptr::null_mut();
            let mut p_len: usize = 0;
            assert_eq!(
                net_meshdb_iter_next(iter, &mut origin, &mut seq, &mut p_ptr, &mut p_len),
                NET_MESHDB_OK
            );
            // Decode the sentinel-row payload to JSON.
            let json_ptr = net_meshdb_decode_payload_json(p_ptr, p_len);
            assert!(!json_ptr.is_null(), "expected a JSON-decodable payload");
            let json = std::ffi::CStr::from_ptr(json_ptr).to_str().unwrap();
            assert!(json.contains(r#""kind":"aggregate""#), "got: {json}");
            assert!(json.contains(r#""kind":"count""#), "got: {json}");
            assert!(json.contains(r#""count":4"#), "got: {json}");
            net_meshdb_free_string(json_ptr);
            net_meshdb_payload_free(p_ptr, p_len);
            net_meshdb_iter_free(iter);
            net_meshdb_query_free(count);
            net_meshdb_query_free(between);
            net_meshdb_runner_free(runner);
            net_meshdb_reader_free(reader);
        }
    }

    /// Slice 2: plain at-row payloads aren't postcard-encoded
    /// sentinel envelopes — the decoder returns null.
    #[test]
    fn decode_plain_row_returns_null() {
        unsafe {
            let bytes = b"plain event bytes";
            let json_ptr = net_meshdb_decode_payload_json(bytes.as_ptr(), bytes.len());
            assert!(json_ptr.is_null());
        }
    }

    /// Slice 3: filter via JSON predicate end-to-end.
    /// `equals` on synthetic `seq` keeps the matching row only.
    #[test]
    fn ffi_filter_equals_via_json_predicate() {
        unsafe {
            let reader = net_meshdb_reader_new();
            for s in 1u64..=3 {
                let p = format!("p-{s}");
                net_meshdb_reader_append(reader, 0xAB, s, p.as_ptr(), p.len());
            }
            let runner = net_meshdb_runner_new(reader);
            let between = net_meshdb_query_between(0xAB, 1, 10);
            let predicate_json =
                std::ffi::CString::new(r#"{"kind":"equals","field":"seq","value":"2"}"#).unwrap();
            let filter = net_meshdb_query_filter_json(between, predicate_json.as_ptr());
            assert!(!filter.is_null());
            let iter = net_meshdb_runner_execute(runner, filter);

            let mut seqs: Vec<u64> = Vec::new();
            for _ in 0..10 {
                let mut origin: u64 = 0;
                let mut seq: u64 = 0;
                let mut p_ptr: *mut u8 = ptr::null_mut();
                let mut p_len: usize = 0;
                let status =
                    net_meshdb_iter_next(iter, &mut origin, &mut seq, &mut p_ptr, &mut p_len);
                if status == NET_MESHDB_END {
                    break;
                }
                assert_eq!(status, NET_MESHDB_OK);
                seqs.push(seq);
                net_meshdb_payload_free(p_ptr, p_len);
            }
            assert_eq!(seqs, vec![2]);

            net_meshdb_iter_free(iter);
            net_meshdb_query_free(filter);
            net_meshdb_query_free(between);
            net_meshdb_runner_free(runner);
            net_meshdb_reader_free(reader);
        }
    }

    /// Slice 3: malformed predicate JSON returns null.
    #[test]
    fn ffi_filter_with_bad_json_returns_null() {
        unsafe {
            let inner = net_meshdb_query_latest(0x01);
            let bad = std::ffi::CString::new(r#"{"kind":"unknown","field":"x"}"#).unwrap();
            let filter = net_meshdb_query_filter_json(inner, bad.as_ptr());
            assert!(filter.is_null());
            net_meshdb_query_free(inner);
        }
    }

    /// Slice 3: composite predicate (and / numeric_at_least).
    #[test]
    fn ffi_filter_with_and_composition() {
        unsafe {
            let reader = net_meshdb_reader_new();
            for s in 1u64..=5 {
                let p = format!("p-{s}");
                net_meshdb_reader_append(reader, 0xAB, s, p.as_ptr(), p.len());
            }
            let runner = net_meshdb_runner_new(reader);
            let between = net_meshdb_query_between(0xAB, 1, 10);
            // seq >= 3 AND seq <= 4 → only seqs 3 and 4
            let predicate = std::ffi::CString::new(
                r#"{"kind":"and","children":[
                    {"kind":"numeric_at_least","field":"seq","threshold":3.0},
                    {"kind":"numeric_at_most","field":"seq","threshold":4.0}
                ]}"#,
            )
            .unwrap();
            let filter = net_meshdb_query_filter_json(between, predicate.as_ptr());
            assert!(!filter.is_null());
            let iter = net_meshdb_runner_execute(runner, filter);

            let mut seqs: Vec<u64> = Vec::new();
            for _ in 0..10 {
                let mut origin: u64 = 0;
                let mut seq: u64 = 0;
                let mut p_ptr: *mut u8 = ptr::null_mut();
                let mut p_len: usize = 0;
                let status =
                    net_meshdb_iter_next(iter, &mut origin, &mut seq, &mut p_ptr, &mut p_len);
                if status == NET_MESHDB_END {
                    break;
                }
                assert_eq!(status, NET_MESHDB_OK);
                seqs.push(seq);
                net_meshdb_payload_free(p_ptr, p_len);
            }
            assert_eq!(seqs, vec![3, 4]);

            net_meshdb_iter_free(iter);
            net_meshdb_query_free(filter);
            net_meshdb_query_free(between);
            net_meshdb_runner_free(runner);
            net_meshdb_reader_free(reader);
        }
    }

    /// Slice 5: Phase F cache-aware runner serves identical rows
    /// on the second call without re-hitting the reader. Smoke
    /// test only — cache observability requires deeper hooks.
    #[test]
    fn ffi_cached_runner_round_trips() {
        unsafe {
            // Verify the cache is actually consulted by mutating
            // the underlying store between calls and asserting the
            // cached path returns the *first* observed value (stale),
            // while bypass_cache returns the *current* value.
            let reader = net_meshdb_reader_new();
            net_meshdb_reader_append(reader, 0x01, 1, b"v1".as_ptr(), 2);
            let runner = net_meshdb_runner_new_cached(reader);
            let query = net_meshdb_query_at(0x01, 1);

            let drain = |iter: *mut MeshDbIter| -> Vec<u8> {
                let mut out = Vec::new();
                loop {
                    let mut origin: u64 = 0;
                    let mut seq: u64 = 0;
                    let mut payload_ptr: *mut u8 = ptr::null_mut();
                    let mut payload_len: usize = 0;
                    let rc = net_meshdb_iter_next(
                        iter,
                        &mut origin,
                        &mut seq,
                        &mut payload_ptr,
                        &mut payload_len,
                    );
                    if rc != NET_MESHDB_OK {
                        break;
                    }
                    out.extend_from_slice(slice::from_raw_parts(payload_ptr, payload_len));
                    net_meshdb_payload_free(payload_ptr, payload_len);
                }
                out
            };

            // 1) Prime the cache with the initial payload.
            let iter1 =
                net_meshdb_runner_execute_with(runner, query, 0, NET_MESHDB_CACHE_PERMANENT, 0.0);
            assert!(!iter1.is_null());
            assert_eq!(drain(iter1), b"v1");
            net_meshdb_iter_free(iter1);

            // 2) Mutate the underlying store (the runner shares
            //    the reader's Arc<InMemoryStore>).
            net_meshdb_reader_append(reader, 0x01, 1, b"v2".as_ptr(), 2);

            // 3) Re-execute — Permanent cache returns stale "v1".
            let iter2 =
                net_meshdb_runner_execute_with(runner, query, 0, NET_MESHDB_CACHE_PERMANENT, 0.0);
            assert!(!iter2.is_null());
            assert_eq!(
                drain(iter2),
                b"v1",
                "cached read must return the pre-mutation payload"
            );
            net_meshdb_iter_free(iter2);

            // 4) bypass_cache reads through to the live store.
            let iter3 = net_meshdb_runner_execute_with(
                runner,
                query,
                1, // bypass_cache
                NET_MESHDB_CACHE_TIME_BOUND,
                5.0,
            );
            assert!(!iter3.is_null());
            assert_eq!(
                drain(iter3),
                b"v2",
                "bypass_cache must return the post-mutation payload"
            );
            net_meshdb_iter_free(iter3);

            net_meshdb_query_free(query);
            net_meshdb_runner_free(runner);
            net_meshdb_reader_free(reader);
        }
    }

    /// Slice 2: window factory plus the JSON decoder produces
    /// `{"kind":"window", "start":..., "end":..., "rows":[...]}`.
    #[test]
    fn ffi_window_factory_and_json_decoder() {
        unsafe {
            let reader = net_meshdb_reader_new();
            for s in 1u64..=5 {
                let p = format!("p-{s}");
                net_meshdb_reader_append(reader, 0x01, s, p.as_ptr(), p.len());
            }
            let runner = net_meshdb_runner_new(reader);
            let between = net_meshdb_query_between(0x01, 1, 20);
            let window = net_meshdb_query_window(between, 3);
            assert!(!window.is_null());
            let iter = net_meshdb_runner_execute(runner, window);
            // First bucket [0, 3) — seqs 1, 2.
            let mut origin: u64 = 0;
            let mut seq: u64 = 0;
            let mut p_ptr: *mut u8 = ptr::null_mut();
            let mut p_len: usize = 0;
            assert_eq!(
                net_meshdb_iter_next(iter, &mut origin, &mut seq, &mut p_ptr, &mut p_len),
                NET_MESHDB_OK
            );
            let json_ptr = net_meshdb_decode_payload_json(p_ptr, p_len);
            assert!(!json_ptr.is_null());
            let json = std::ffi::CStr::from_ptr(json_ptr).to_str().unwrap();
            assert!(json.contains(r#""kind":"window""#), "got: {json}");
            assert!(json.contains(r#""start":0"#), "got: {json}");
            assert!(json.contains(r#""end":3"#), "got: {json}");
            net_meshdb_free_string(json_ptr);
            net_meshdb_payload_free(p_ptr, p_len);
            net_meshdb_iter_free(iter);
            net_meshdb_query_free(window);
            net_meshdb_query_free(between);
            net_meshdb_runner_free(runner);
            net_meshdb_reader_free(reader);
        }
    }

    #[test]
    fn ffi_lineage_emit_yields_one_row_per_entry() {
        unsafe {
            let reader = net_meshdb_reader_new();
            let runner = net_meshdb_runner_new(reader);
            let entries =
                std::ffi::CString::new(r#"[{"origin":170,"depth":0,"tip_seq":5},{"origin":187,"depth":1,"tip_seq":3},{"origin":204,"depth":2,"tip_seq":null}]"#)
                    .unwrap();
            let direction = std::ffi::CString::new("back").unwrap();
            let query = net_meshdb_query_lineage_emit(0xAA, entries.as_ptr(), direction.as_ptr());
            assert!(!query.is_null());
            let iter = net_meshdb_runner_execute(runner, query);

            let mut seen: Vec<(u64, u64)> = Vec::new();
            loop {
                let mut origin: u64 = 0;
                let mut seq: u64 = 0;
                let mut p_ptr: *mut u8 = ptr::null_mut();
                let mut p_len: usize = 0;
                let status =
                    net_meshdb_iter_next(iter, &mut origin, &mut seq, &mut p_ptr, &mut p_len);
                if status == NET_MESHDB_END {
                    break;
                }
                assert_eq!(status, NET_MESHDB_OK);
                seen.push((origin, seq));
                assert_eq!(p_len, 0);
                if !p_ptr.is_null() {
                    net_meshdb_payload_free(p_ptr, p_len);
                }
            }
            assert_eq!(seen, vec![(170, 5), (187, 3), (204, 0)]);

            net_meshdb_iter_free(iter);
            net_meshdb_query_free(query);
            net_meshdb_runner_free(runner);
            net_meshdb_reader_free(reader);
        }
    }

    #[test]
    fn ffi_lineage_emit_rejects_unknown_direction() {
        unsafe {
            let entries = std::ffi::CString::new("[]").unwrap();
            let bad = std::ffi::CString::new("sideways").unwrap();
            let q = net_meshdb_query_lineage_emit(0xAA, entries.as_ptr(), bad.as_ptr());
            assert!(q.is_null());
        }
    }

    #[test]
    fn ffi_lineage_emit_rejects_malformed_json() {
        unsafe {
            let bad = std::ffi::CString::new("not json").unwrap();
            let dir = std::ffi::CString::new("back").unwrap();
            let q = net_meshdb_query_lineage_emit(0xAA, bad.as_ptr(), dir.as_ptr());
            assert!(q.is_null());
        }
    }

    #[test]
    fn ffi_last_error_starts_null_and_clears_correctly() {
        unsafe {
            net_meshdb_clear_last_error();
            assert!(net_meshdb_last_error_message().is_null());
            assert!(net_meshdb_last_error_kind().is_null());
        }
    }

    #[test]
    fn ffi_null_handle_populates_last_error() {
        unsafe {
            net_meshdb_clear_last_error();
            // Trigger the null-handle branch: passing a null
            // runner should set the last-error to the
            // invalid-arg kind and return a null iterator.
            let q = net_meshdb_query_latest(0xAB);
            let iter = net_meshdb_runner_execute(ptr::null_mut(), q);
            assert!(iter.is_null());

            let msg_ptr = net_meshdb_last_error_message();
            let kind_ptr = net_meshdb_last_error_kind();
            assert!(!msg_ptr.is_null());
            assert!(!kind_ptr.is_null());
            let kind = std::ffi::CStr::from_ptr(kind_ptr).to_str().unwrap();
            assert_eq!(kind, "invalid_arg");

            net_meshdb_query_free(q);
        }
    }

    #[test]
    fn ffi_mesh_error_kind_round_trip_covers_known_variants() {
        // Pin the variant→string mapping so SDK consumers can
        // branch on `kind` strings without grepping the substrate.
        use net::adapter::net::behavior::meshdb::BudgetMetric;
        assert_eq!(
            mesh_error_kind(&MeshError::QueryCancelled),
            "query_cancelled"
        );
        assert_eq!(
            mesh_error_kind(&MeshError::PlannerError { detail: "x".into() }),
            "planner_error"
        );
        assert_eq!(
            mesh_error_kind(&MeshError::ExecutorError {
                node: 0,
                detail: "x".into()
            }),
            "executor_error"
        );
        assert_eq!(
            mesh_error_kind(&MeshError::JoinMemoryExceeded {
                strategy: "x".into(),
                threshold_bytes: 0
            }),
            "join_memory_exceeded"
        );
        assert_eq!(
            mesh_error_kind(&MeshError::QueryBudgetExceeded {
                metric: BudgetMetric::MaxRows,
                used: 0,
                limit: 0
            }),
            "query_budget_exceeded"
        );
    }

    #[test]
    fn ffi_guard_traps_panics_and_records_last_error() {
        // Direct exercise of the ffi_guard! macro: a panic inside
        // the wrapped body must NOT escape the closure, must
        // populate the thread-local last-error pair with kind
        // "runtime_panic", and must return the declared default.
        // Pin this so future entry points can rely on it.
        clear_last_error();
        let out: *mut MeshDbQuery =
            ffi_guard!(ptr::null_mut(), { panic!("simulated FFI body panic") });
        assert!(out.is_null(), "ffi_guard must return its default on panic");
        unsafe {
            let kind = net_meshdb_last_error_kind();
            assert!(!kind.is_null(), "last-error kind must be populated");
            let kind_s = std::ffi::CStr::from_ptr(kind).to_str().unwrap();
            assert_eq!(kind_s, "runtime_panic");
            let msg = net_meshdb_last_error_message();
            assert!(!msg.is_null());
            let msg_s = std::ffi::CStr::from_ptr(msg).to_str().unwrap();
            assert!(
                msg_s.contains("simulated FFI body panic"),
                "panic message should be preserved; got {msg_s:?}",
            );
        }
        clear_last_error();
    }
}
