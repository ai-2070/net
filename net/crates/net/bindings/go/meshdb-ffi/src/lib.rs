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
//! The detail message for non-OK statuses is fetched via the
//! existing `net_last_error_message()` helper (which the cortex
//! FFI already populates from a thread-local). Slice 1 keeps
//! errors stringly-typed; structured access lands when consumers
//! ask for it.

use std::ffi::c_int;
use std::ptr;
use std::slice;
use std::sync::Arc;

use tokio::runtime::Runtime;

use net::adapter::net::behavior::meshdb::{
    executor::{ChainReader, ExecuteOptions, LocalMeshQueryExecutor, MeshQueryExecutor},
    planner::{CostEstimate, OperatorNode, OperatorPlan},
    query::ResultRow,
    ExecutionPlan, SeqNum,
};
use net::adapter::net::behavior::meshdb::MeshError;

/// Status code: function returned successfully.
pub const NET_MESHDB_OK: c_int = 0;
/// Status code: iterator has no more rows.
pub const NET_MESHDB_END: c_int = 1;
/// Status code: caller passed an invalid argument.
pub const NET_MESHDB_INVALID_ARG: c_int = 2;
/// Status code: planner or executor surfaced an error.
pub const NET_MESHDB_RUNTIME_ERR: c_int = 3;

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
    Box::into_raw(Box::new(MeshDbReader {
        store: Arc::new(InMemoryStore::default()),
    }))
}

/// Free a reader handle. No-op on null. Safe to call even
/// while a `MeshDbRunner` built from this reader is still
/// live — the runner holds its own Arc clone of the store.
///
/// # Safety
/// `reader` must be a pointer returned by
/// `net_meshdb_reader_new`, or null. Must not have been freed
/// already.
#[no_mangle]
pub unsafe extern "C" fn net_meshdb_reader_free(reader: *mut MeshDbReader) {
    if reader.is_null() {
        return;
    }
    drop(Box::from_raw(reader));
}

/// Append `(origin, seq, payload)` to the reader.
///
/// # Safety
/// `reader` must be a valid pointer returned by
/// `net_meshdb_reader_new`. `payload` must be a valid pointer
/// to `payload_len` bytes, or null when `payload_len == 0`.
#[no_mangle]
pub unsafe extern "C" fn net_meshdb_reader_append(
    reader: *mut MeshDbReader,
    origin: u64,
    seq: u64,
    payload: *const u8,
    payload_len: usize,
) -> c_int {
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
    let plan = plan_of(OperatorPlan::AtRead {
        origin,
        seq: SeqNum(seq),
    });
    Box::into_raw(Box::new(MeshDbQuery { plan }))
}

/// Construct a `Between(origin, start, end)` query (half-open).
/// Returns null when `start >= end`.
#[no_mangle]
pub extern "C" fn net_meshdb_query_between(
    origin: u64,
    start: u64,
    end: u64,
) -> *mut MeshDbQuery {
    if start >= end {
        return ptr::null_mut();
    }
    let plan = plan_of(OperatorPlan::BetweenRead {
        origin,
        start: SeqNum(start),
        end: SeqNum(end),
    });
    Box::into_raw(Box::new(MeshDbQuery { plan }))
}

/// Construct a `Latest(origin)` query.
#[no_mangle]
pub extern "C" fn net_meshdb_query_latest(origin: u64) -> *mut MeshDbQuery {
    let plan = plan_of(OperatorPlan::LatestRead { origin });
    Box::into_raw(Box::new(MeshDbQuery { plan }))
}

/// Free a query handle. No-op on null.
///
/// # Safety
/// `query` must be a pointer returned by `net_meshdb_query_*`
/// or null. Must not have been freed already.
#[no_mangle]
pub unsafe extern "C" fn net_meshdb_query_free(query: *mut MeshDbQuery) {
    if query.is_null() {
        return;
    }
    drop(Box::from_raw(query));
}

// =====================================================================
// Runner + execute
// =====================================================================

/// Construct a runner that shares the given reader's
/// underlying store via `Arc` clone. The caller may safely
/// free the reader pointer afterwards — the runner keeps the
/// store alive until itself is freed. Subsequent
/// `net_meshdb_reader_append` calls on the original reader
/// pointer are visible to this runner because they target the
/// same `Arc<InMemoryStore>`.
///
/// # Safety
/// `reader` must be a valid pointer returned by
/// `net_meshdb_reader_new`, or null (which yields null).
#[no_mangle]
pub unsafe extern "C" fn net_meshdb_runner_new(
    reader: *mut MeshDbReader,
) -> *mut MeshDbRunner {
    if reader.is_null() {
        return ptr::null_mut();
    }
    let store = (&*reader).store.clone();
    let runtime = match Runtime::new() {
        Ok(rt) => Arc::new(rt),
        Err(_) => return ptr::null_mut(),
    };
    let executor: LocalMeshQueryExecutor<InMemoryStore> =
        LocalMeshQueryExecutor::new(store);
    let runner = MeshDbRunner {
        runtime,
        executor: Arc::new(executor),
    };
    Box::into_raw(Box::new(runner))
}

/// Free a runner handle.
///
/// # Safety
/// `runner` must be a pointer returned by `net_meshdb_runner_new`
/// or null.
#[no_mangle]
pub unsafe extern "C" fn net_meshdb_runner_free(runner: *mut MeshDbRunner) {
    if runner.is_null() {
        return;
    }
    drop(Box::from_raw(runner));
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
        return ptr::null_mut();
    }
    let runner_ref = &*runner;
    let plan = (&*query).plan.clone();
    let executor = runner_ref.executor.clone();
    let runtime = runner_ref.runtime.clone();
    let rows: Result<Vec<ResultRow>, MeshError> = runtime.block_on(async move {
        use futures::StreamExt;
        let running = executor
            .execute_with(plan, ExecuteOptions::default())
            .await?;
        let mut stream = running.rows;
        let mut out = Vec::new();
        while let Some(item) = stream.next().await {
            out.push(item?);
        }
        Ok(out)
    });
    match rows {
        Ok(rows) => Box::into_raw(Box::new(MeshDbIter { rows, next_idx: 0 })),
        Err(_) => ptr::null_mut(),
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
}

/// Free a payload buffer returned by `net_meshdb_iter_next`.
///
/// # Safety
/// `ptr` must be a buffer returned by `net_meshdb_iter_next` (or
/// null). `len` must equal the length originally written by
/// `net_meshdb_iter_next` for that pointer.
#[no_mangle]
pub unsafe extern "C" fn net_meshdb_payload_free(ptr: *mut u8, len: usize) {
    if ptr.is_null() || len == 0 {
        return;
    }
    let boxed: Box<[u8]> = Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, len));
    drop(boxed);
}

/// Free an iterator handle.
///
/// # Safety
/// `iter` must be a pointer returned by `net_meshdb_runner_execute`
/// or null.
#[no_mangle]
pub unsafe extern "C" fn net_meshdb_iter_free(iter: *mut MeshDbIter) {
    if iter.is_null() {
        return;
    }
    drop(Box::from_raw(iter));
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
}
