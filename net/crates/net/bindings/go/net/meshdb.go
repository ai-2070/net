// Package net — MeshDB query-layer consumer wrapper for the C ABI
// exported by `net::ffi::meshdb` (compiled as `libnet_meshdb`).
//
// This file is a **reference implementation** documenting the
// expected Go-side surface for consumers of `libnet_meshdb`.
// Downstream binding trees follow the same shape; the upstream
// `net` repo owns the C ABI side and ships this file as the
// canonical contract.
//
// # Channels-based execution
//
// Per the locked Go SDK decision, `(*MeshDBRunner).Execute` and
// its ctx-aware variants return `(<-chan MeshDBResult, error)`.
// The wrapper spawns a goroutine that pumps rows from the FFI
// iterator into the channel; the goroutine closes the channel on
// EOF or on the first error.
//
// # Cancellation
//
// `Execute(query)` is non-cancellable: dropping the channel
// receiver does NOT signal the sender in Go, and the pumping
// goroutine would block on `ch <- row` after the buffered window
// (32) fills, leaking the goroutine and the FFI iterator. For
// cancellable use, call `ExecuteContext(ctx, query)` (or
// `ExecuteWithContext(ctx, query, options)`); the goroutine
// selects on `ctx.Done()` for every send, frees the FFI iterator,
// and closes the channel when the context fires.
//
// `Execute(query)` is preserved as a thin wrapper that calls
// `ExecuteContext(context.Background(), query)`, so existing
// callers keep working; new code should prefer the ctx variant.
//
// # Memory model
//
// Every Rust object that crosses the FFI is wrapped in a
// `runtime.SetFinalizer`–protected Go handle. Manual `.Free()`
// methods are exposed for callers that want deterministic
// teardown (production wrappers; the finalizer is the safety
// net).
//
// # Error model
//
// FFI functions return `c_int` status codes:
//
//   - 0 (`NET_MESHDB_OK`)        — success.
//   - 1 (`NET_MESHDB_END`)       — iterator EOF (poll only).
//   - 2 (`NET_MESHDB_INVALID_ARG`) — null pointer / bad input.
//   - 3 (`NET_MESHDB_RUNTIME_ERR`) — planner / executor failure.
//
// Slice 1 keeps error detail strings stringly-typed; the upstream
// FFI exposes a thread-local last-error message helper that
// downstream wrappers can call when the status is non-zero.
// Structured error access lands when consumers ask for it.
package net

/*
#include <stdint.h>
#include <stdlib.h>

// Forward-declared opaque handle types from `libnet_meshdb`.
typedef struct MeshDbReader MeshDbReader;
typedef struct MeshDbQuery MeshDbQuery;
typedef struct MeshDbRunner MeshDbRunner;
typedef struct MeshDbIter MeshDbIter;

// Status codes mirroring `net::ffi::meshdb`. Keep in lockstep.
#define NET_MESHDB_OK 0
#define NET_MESHDB_END 1
#define NET_MESHDB_INVALID_ARG 2
#define NET_MESHDB_RUNTIME_ERR 3

// Reader.
extern MeshDbReader* net_meshdb_reader_new(void);
extern void net_meshdb_reader_free(MeshDbReader* reader);
extern int net_meshdb_reader_append(
    MeshDbReader* reader,
    uint64_t origin,
    uint64_t seq,
    const uint8_t* payload,
    size_t payload_len
);

// Query factories (slice 1 atomic operators).
extern MeshDbQuery* net_meshdb_query_at(uint64_t origin, uint64_t seq);
extern MeshDbQuery* net_meshdb_query_between(
    uint64_t origin,
    uint64_t start,
    uint64_t end
);
extern MeshDbQuery* net_meshdb_query_latest(uint64_t origin);
extern void net_meshdb_query_free(MeshDbQuery* query);

// Runner + execute.
extern MeshDbRunner* net_meshdb_runner_new(const MeshDbReader* reader);
extern void net_meshdb_runner_free(MeshDbRunner* runner);
extern MeshDbIter* net_meshdb_runner_execute(
    MeshDbRunner* runner,
    const MeshDbQuery* query
);

// Iterator.
extern int net_meshdb_iter_next(
    MeshDbIter* iter,
    uint64_t* origin_out,
    uint64_t* seq_out,
    uint8_t** payload_out_ptr,
    size_t* payload_out_len
);
extern void net_meshdb_payload_free(uint8_t* ptr, size_t len);
extern void net_meshdb_iter_free(MeshDbIter* iter);

// Slice 2: composite factories. `group_by` is a comma-separated
// C-string of row-intrinsic field names (null / empty for no
// grouping; "origin" / "seq" / "origin,seq").
extern MeshDbQuery* net_meshdb_query_window(
    const MeshDbQuery* inner,
    uint64_t size
);
extern MeshDbQuery* net_meshdb_query_count(
    const MeshDbQuery* inner,
    const char* group_by
);
extern MeshDbQuery* net_meshdb_query_numeric_agg(
    const MeshDbQuery* inner,
    const char* kind,
    const char* field,
    const char* group_by
);
extern MeshDbQuery* net_meshdb_query_percentile(
    const MeshDbQuery* inner,
    const char* field,
    double p,
    const char* group_by
);
extern MeshDbQuery* net_meshdb_query_join(
    const MeshDbQuery* left,
    const MeshDbQuery* right,
    const char* kind,
    const char* key,
    const char* strategy,
    double watermark_secs
);

// LineageEmit: pre-walked entries form. `entries_json` is a JSON
// array of {"origin":N,"depth":N,"tip_seq":N|null}; `direction` is
// "back" or "forward". Returns null on parse error or invalid args.
extern MeshDbQuery* net_meshdb_query_lineage_emit(
    uint64_t origin,
    const char* entries_json,
    const char* direction
);

// Slice 2: payload decoder (JSON intermediate). Returns null
// when the payload isn't a postcard-encoded aggregate / joined /
// window envelope.
extern char* net_meshdb_decode_payload_json(
    const uint8_t* payload,
    size_t payload_len
);
extern void net_meshdb_free_string(char* s);

// Slice 3: Filter via JSON-encoded predicate.
extern MeshDbQuery* net_meshdb_query_filter_json(
    const MeshDbQuery* inner,
    const char* predicate_json
);

// Slice 5: Phase F cache options.
#define NET_MESHDB_CACHE_PERMANENT 0
#define NET_MESHDB_CACHE_TIME_BOUND 1

extern MeshDbRunner* net_meshdb_runner_new_cached(const MeshDbReader* reader);
extern MeshDbIter* net_meshdb_runner_execute_with(
    MeshDbRunner* runner,
    const MeshDbQuery* query,
    int bypass_cache,
    int cache_policy_kind,
    double cache_ttl_secs
);

// Thread-local last-error pair populated by every FFI entry
// point on a non-OK status. Pointers are valid until the next
// FFI call on the same thread touches the thread-local; callers
// must not free.
extern const char* net_meshdb_last_error_message(void);
extern const char* net_meshdb_last_error_kind(void);
extern void net_meshdb_clear_last_error(void);
*/
import "C"

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"math"
	"runtime"
	"unsafe"
)

// =====================================================================
// Errors
// =====================================================================

// ErrMeshDB is the discriminator for MeshDB-side errors. Wrap
// concrete errors with `fmt.Errorf("...: %w", ErrMeshDB)` so
// callers can route via `errors.Is(err, ErrMeshDB)`.
var ErrMeshDB = errors.New("meshdb")

// ErrMeshDBInvalidArg covers null-pointer / out-of-range inputs
// that the FFI rejects synchronously.
var ErrMeshDBInvalidArg = errors.New("meshdb: invalid argument")

// ErrMeshDBRuntime covers planner / executor failures surfaced
// through `NET_MESHDB_RUNTIME_ERR`.
var ErrMeshDBRuntime = errors.New("meshdb: runtime error")

// MeshDBError wraps a sentinel (ErrMeshDBInvalidArg /
// ErrMeshDBRuntime) with the FFI-supplied structured detail. The
// `Kind` field carries one of the `MeshError` variant tags
// (`planner_error`, `executor_error`, `query_cancelled`,
// `historical_range_unavailable`, `runtime_panic`, …) so callers
// can branch without parsing `Message`. `errors.Is(err,
// ErrMeshDBRuntime)` continues to work via the wrapped sentinel.
type MeshDBError struct {
	Sentinel error  // ErrMeshDBInvalidArg or ErrMeshDBRuntime
	Kind     string // FFI kind discriminator; empty when not reported
	Message  string // FFI human-readable detail; empty when not reported
}

// Error renders as "meshdb: <sentinel> (kind=KIND): MSG" — falls
// back to just the sentinel when the FFI didn't populate the
// last-error pair.
func (e *MeshDBError) Error() string {
	if e == nil {
		return "<nil meshdb error>"
	}
	if e.Kind == "" && e.Message == "" {
		return e.Sentinel.Error()
	}
	if e.Kind != "" && e.Message != "" {
		return fmt.Sprintf("%s (kind=%s): %s", e.Sentinel.Error(), e.Kind, e.Message)
	}
	if e.Kind != "" {
		return fmt.Sprintf("%s (kind=%s)", e.Sentinel.Error(), e.Kind)
	}
	return fmt.Sprintf("%s: %s", e.Sentinel.Error(), e.Message)
}

// Unwrap exposes the sentinel for `errors.Is` routing.
func (e *MeshDBError) Unwrap() error {
	if e == nil {
		return nil
	}
	return e.Sentinel
}

// wrapMeshDBError reads the per-thread last-error pair from the
// FFI and wraps a sentinel with the supplied detail. Always
// returns a non-nil `*MeshDBError` so callers don't have to
// nil-check before reading `.Kind`. Clears the thread-local
// last-error state after reading so the next FFI call starts
// fresh.
func wrapMeshDBError(sentinel error) *MeshDBError {
	err := &MeshDBError{Sentinel: sentinel}
	msgPtr := C.net_meshdb_last_error_message()
	if msgPtr != nil {
		err.Message = C.GoString(msgPtr)
	}
	kindPtr := C.net_meshdb_last_error_kind()
	if kindPtr != nil {
		err.Kind = C.GoString(kindPtr)
	}
	C.net_meshdb_clear_last_error()
	return err
}

// =====================================================================
// Result row + result channel envelope
// =====================================================================

// MeshDBResultRow is one row from a query result. `Origin` is the
// chain identifier (`u64`); `Seq` is the per-chain monotonic
// sequence; `Payload` is opaque bytes (event body for plain reads,
// or a postcard-encoded envelope for aggregate / join / window
// sentinel rows — decoders land in slice 2).
type MeshDBResultRow struct {
	Origin  uint64
	Seq     uint64
	Payload []byte
}

// MeshDBResult pairs a row or error onto the channel returned by
// `(*MeshQueryRunner).Execute`. The channel is closed cleanly on
// EOF; on the first error, the goroutine emits a Result with
// `Err != nil` and closes.
type MeshDBResult struct {
	Row MeshDBResultRow
	Err error
}

// =====================================================================
// InMemoryChainReader
// =====================================================================

// MeshDBReader is the Go-side handle for the FFI's in-memory
// `ChainReader`. Slice 1 ships only this reader; Phase B+ adds a
// Redex-backed adapter.
type MeshDBReader struct {
	ptr *C.MeshDbReader
}

// NewMeshDBReader allocates a fresh in-memory chain reader. Free
// via `(*MeshDBReader).Free()` (or rely on the finalizer).
func NewMeshDBReader() *MeshDBReader {
	r := &MeshDBReader{ptr: C.net_meshdb_reader_new()}
	runtime.SetFinalizer(r, func(r *MeshDBReader) { r.Free() })
	return r
}

// Append a single event to the in-memory store. Payload bytes are
// copied into the FFI; the caller retains ownership of `payload`.
func (r *MeshDBReader) Append(origin, seq uint64, payload []byte) error {
	if r == nil || r.ptr == nil {
		return ErrMeshDBInvalidArg
	}
	var (
		ptr *C.uint8_t
		ln  C.size_t
	)
	if len(payload) > 0 {
		ptr = (*C.uint8_t)(unsafe.Pointer(&payload[0]))
		ln = C.size_t(len(payload))
	}
	status := C.net_meshdb_reader_append(r.ptr, C.uint64_t(origin), C.uint64_t(seq), ptr, ln)
	switch status {
	case C.NET_MESHDB_OK:
		return nil
	case C.NET_MESHDB_INVALID_ARG:
		return ErrMeshDBInvalidArg
	default:
		return ErrMeshDBRuntime
	}
}

// Free releases the FFI handle. Idempotent + safe on a nil
// receiver. Subsequent method calls return `ErrMeshDBInvalidArg`.
func (r *MeshDBReader) Free() {
	if r == nil || r.ptr == nil {
		return
	}
	C.net_meshdb_reader_free(r.ptr)
	r.ptr = nil
	runtime.SetFinalizer(r, nil)
}

// =====================================================================
// MeshQuery (factory AST)
// =====================================================================

// MeshDBQuery is the Go-side handle for a planned MeshDB query.
// Construct via the `MeshDBQuery*` factory functions and pass to
// `(*MeshQueryRunner).Execute`.
type MeshDBQuery struct {
	ptr *C.MeshDbQuery
}

// MeshDBQueryAt builds an `At(origin, seq)` query.
func MeshDBQueryAt(origin, seq uint64) *MeshDBQuery {
	q := &MeshDBQuery{ptr: C.net_meshdb_query_at(C.uint64_t(origin), C.uint64_t(seq))}
	runtime.SetFinalizer(q, func(q *MeshDBQuery) { q.Free() })
	return q
}

// MeshDBQueryBetween builds a `Between(origin, start, end)` query
// (half-open). Returns `nil, ErrMeshDBInvalidArg` when `start >=
// end`.
func MeshDBQueryBetween(origin, start, end uint64) (*MeshDBQuery, error) {
	ptr := C.net_meshdb_query_between(C.uint64_t(origin), C.uint64_t(start), C.uint64_t(end))
	if ptr == nil {
		return nil, ErrMeshDBInvalidArg
	}
	q := &MeshDBQuery{ptr: ptr}
	runtime.SetFinalizer(q, func(q *MeshDBQuery) { q.Free() })
	return q, nil
}

// MeshDBQueryLatest builds a `Latest(origin)` query.
func MeshDBQueryLatest(origin uint64) *MeshDBQuery {
	q := &MeshDBQuery{ptr: C.net_meshdb_query_latest(C.uint64_t(origin))}
	runtime.SetFinalizer(q, func(q *MeshDBQuery) { q.Free() })
	return q
}

// Free releases the FFI handle.
func (q *MeshDBQuery) Free() {
	if q == nil || q.ptr == nil {
		return
	}
	C.net_meshdb_query_free(q.ptr)
	q.ptr = nil
	runtime.SetFinalizer(q, nil)
}

// =====================================================================
// MeshQueryRunner
// =====================================================================

// MeshDBRunner is the Go-side handle for a query runner. Owns a
// shared `Arc<InMemoryStore>` clone from its source reader; the
// reader may be freed independently — the runner outlives it.
type MeshDBRunner struct {
	ptr *C.MeshDbRunner
}

// NewMeshDBRunner constructs a runner over the given reader.
// Returns `nil` when `reader` is nil or already freed.
func NewMeshDBRunner(reader *MeshDBReader) *MeshDBRunner {
	if reader == nil || reader.ptr == nil {
		return nil
	}
	ptr := C.net_meshdb_runner_new(reader.ptr)
	if ptr == nil {
		return nil
	}
	r := &MeshDBRunner{ptr: ptr}
	runtime.SetFinalizer(r, func(r *MeshDBRunner) { r.Free() })
	return r
}

// NewMeshDBRunnerCached constructs a runner with the Phase F
// LRU result cache wired in. Pass `MeshDBExecuteOptions` to
// `ExecuteWith` to control per-query policy (Permanent vs
// TimeBound, bypass for diagnostics).
func NewMeshDBRunnerCached(reader *MeshDBReader) *MeshDBRunner {
	if reader == nil || reader.ptr == nil {
		return nil
	}
	ptr := C.net_meshdb_runner_new_cached(reader.ptr)
	if ptr == nil {
		return nil
	}
	r := &MeshDBRunner{ptr: ptr}
	runtime.SetFinalizer(r, func(r *MeshDBRunner) { r.Free() })
	return r
}

// Execute runs `query` and returns a channel of results. The
// channel is closed on EOF (success) or after the first error.
// Callers stop reading + drop the channel reference to cancel;
// the goroutine notices the channel-send block, gives up, and
// frees the iterator.
//
// Slice 1 ships eager-drain semantics on the Rust side (the FFI's
// iterator pre-collects all rows); the goroutine here streams the
// pre-collected rows onto the channel. Slice 2 may switch to
// lazy iteration once we wire continuation tokens through.
//
// Non-cancellable. Use `ExecuteContext` for ctx-aware variants.
func (r *MeshDBRunner) Execute(query *MeshDBQuery) (<-chan MeshDBResult, error) {
	return r.ExecuteContext(context.Background(), query)
}

// ExecuteContext runs `query` and pumps rows onto the returned
// channel until EOF, the first error, or `ctx.Done()` fires.
// The FFI execute call itself runs inside the spawned goroutine,
// so the caller is never blocked on it — the caller can `select`
// on `ctx.Done()` versus channel receives concurrently with the
// executor. cgo calls are not preemptible mid-call, so an
// already-running FFI execute will run to completion regardless
// of ctx cancellation; the goroutine then frees the resulting
// iterator and exits without pumping rows. Synchronous errors
// from the executor (or the FFI returning null) surface as the
// first `MeshDBResult{Err: ...}` on the channel rather than via
// the returned error — keeps the cancellation surface uniform.
// The returned error is reserved for argument-validation failure
// (nil receiver / nil query).
func (r *MeshDBRunner) ExecuteContext(
	ctx context.Context,
	query *MeshDBQuery,
) (<-chan MeshDBResult, error) {
	if r == nil || r.ptr == nil {
		return nil, ErrMeshDBInvalidArg
	}
	if query == nil || query.ptr == nil {
		return nil, ErrMeshDBInvalidArg
	}
	ch := make(chan MeshDBResult, 32)
	runnerPtr := r.ptr
	queryPtr := query.ptr
	go func() {
		defer close(ch)
		if err := ctx.Err(); err != nil {
			trySend(ctx, ch, MeshDBResult{Err: err})
			return
		}
		iter := C.net_meshdb_runner_execute(runnerPtr, queryPtr)
		if iter == nil {
			trySend(ctx, ch, MeshDBResult{Err: wrapMeshDBError(ErrMeshDBRuntime)})
			return
		}
		defer C.net_meshdb_iter_free(iter)
		pumpIterRowsBody(ctx, iter, ch)
	}()
	return ch, nil
}

// MeshDBCachePolicyKind discriminates the Phase F cache
// policies. Use the constants below — direct construction is
// for advanced callers only.
type MeshDBCachePolicyKind int

const (
	// MeshDBCachePermanent caches until LRU eviction. Use for
	// queries whose result is immutable under substrate
	// semantics (`At(origin, seq)`, closed `Between`).
	MeshDBCachePermanent MeshDBCachePolicyKind = 0
	// MeshDBCacheTimeBound applies a wall-clock TTL. Pair with
	// `TTLSecs` (5.0 is the canonical default — mirrors the
	// locked join watermark).
	MeshDBCacheTimeBound MeshDBCachePolicyKind = 1
)

// MeshDBExecuteOptions is the Phase F per-execute options
// surface. Zero value is the default policy (TimeBound, 5 s)
// with caching active.
type MeshDBExecuteOptions struct {
	// BypassCache skips both lookup and writeback. Use for
	// diagnostics, authoritative reads, or operator catalog
	// churn paths.
	BypassCache bool
	// CachePolicy controls the cache-entry policy (Permanent vs
	// TimeBound). Defaults to TimeBound.
	CachePolicy MeshDBCachePolicyKind
	// TTLSecs is consulted only when `CachePolicy ==
	// MeshDBCacheTimeBound`. Non-finite or negative falls back
	// to 5 s.
	TTLSecs float64
}

// ExecuteWith runs `query` with explicit Phase F options. See
// `Execute` for the channel semantics. The options struct's
// zero value is `{TimeBound, 0 s}` — caller should set TTLSecs
// to 5.0 for the canonical default; the FFI applies a fallback
// when TTLSecs is non-finite or negative.
//
// Non-cancellable; use `ExecuteWithContext` for ctx-aware
// variants.
func (r *MeshDBRunner) ExecuteWith(
	query *MeshDBQuery,
	options MeshDBExecuteOptions,
) (<-chan MeshDBResult, error) {
	return r.ExecuteWithContext(context.Background(), query, options)
}

// ExecuteWithContext is the cancellable variant of `ExecuteWith`.
// Same channel-and-EOF semantics as `ExecuteContext`: the FFI
// execute call runs inside the spawned goroutine, never on the
// caller's stack, so ctx.Done() races the FFI call rather than
// being blocked by it. Runtime errors and ctx cancellation
// surface as `MeshDBResult{Err: ...}` on the channel; the
// returned error is argument-validation only.
func (r *MeshDBRunner) ExecuteWithContext(
	ctx context.Context,
	query *MeshDBQuery,
	options MeshDBExecuteOptions,
) (<-chan MeshDBResult, error) {
	if r == nil || r.ptr == nil {
		return nil, ErrMeshDBInvalidArg
	}
	if query == nil || query.ptr == nil {
		return nil, ErrMeshDBInvalidArg
	}
	bypass := C.int(0)
	if options.BypassCache {
		bypass = C.int(1)
	}
	ch := make(chan MeshDBResult, 32)
	runnerPtr := r.ptr
	queryPtr := query.ptr
	policyKind := C.int(options.CachePolicy)
	ttl := C.double(options.TTLSecs)
	go func() {
		defer close(ch)
		if err := ctx.Err(); err != nil {
			trySend(ctx, ch, MeshDBResult{Err: err})
			return
		}
		iter := C.net_meshdb_runner_execute_with(
			runnerPtr,
			queryPtr,
			bypass,
			policyKind,
			ttl,
		)
		if iter == nil {
			trySend(ctx, ch, MeshDBResult{Err: wrapMeshDBError(ErrMeshDBRuntime)})
			return
		}
		defer C.net_meshdb_iter_free(iter)
		pumpIterRowsBody(ctx, iter, ch)
	}()
	return ch, nil
}

// pumpIterRowsBody drives the row-pumping loop for an
// already-allocated FFI iterator. It does NOT free the iterator
// or close the channel — the caller's deferred close + free
// own that. Splitting the body out lets `ExecuteContext` /
// `ExecuteWithContext` run the FFI execute call from inside a
// goroutine (so ctx.Done() races the executor) without
// double-closing the channel from nested defers.
func pumpIterRowsBody(ctx context.Context, iter *C.MeshDbIter, ch chan<- MeshDBResult) {
	for {
		// Cheap ctx check before reaching for the next FFI row;
		// avoids one unused decode + free on cancellation.
		select {
		case <-ctx.Done():
			trySend(ctx, ch, MeshDBResult{Err: ctx.Err()})
			return
		default:
		}
		var (
			origin     C.uint64_t
			seq        C.uint64_t
			payloadPtr *C.uint8_t
			payloadLen C.size_t
		)
		status := C.net_meshdb_iter_next(iter, &origin, &seq, &payloadPtr, &payloadLen)
		switch status {
		case C.NET_MESHDB_OK:
			payload, copyErr := copyFFIPayload(payloadPtr, payloadLen)
			C.net_meshdb_payload_free(payloadPtr, payloadLen)
			if copyErr != nil {
				trySend(ctx, ch, MeshDBResult{Err: copyErr})
				return
			}
			row := MeshDBResultRow{
				Origin:  uint64(origin),
				Seq:     uint64(seq),
				Payload: payload,
			}
			if !trySend(ctx, ch, MeshDBResult{Row: row}) {
				return
			}
		case C.NET_MESHDB_END:
			return
		case C.NET_MESHDB_INVALID_ARG:
			trySend(ctx, ch, MeshDBResult{Err: wrapMeshDBError(ErrMeshDBInvalidArg)})
			return
		default:
			trySend(ctx, ch, MeshDBResult{Err: wrapMeshDBError(ErrMeshDBRuntime)})
			return
		}
	}
}

// copyFFIPayload turns a `(ptr, size_t len)` pair from the FFI
// into an owned `[]byte`. `C.size_t` is 64-bit on a 64-bit host,
// `C.int` is 32-bit signed — `C.GoBytes` silently truncates /
// sign-flips lengths past `math.MaxInt32`. We refuse oversized
// payloads explicitly with `ErrMeshDBRuntime` rather than risk a
// truncated Go-side buffer.
//
// Empty payloads (`len == 0` and / or null `ptr`) yield a nil
// slice, matching the FFI's "no body" semantics.
func copyFFIPayload(ptr *C.uint8_t, length C.size_t) ([]byte, error) {
	b, ok := goBytesChecked(ptr, length)
	if !ok {
		return nil, ErrMeshDBRuntime
	}
	return b, nil
}

// goBytesChecked turns a `(ptr, size_t len)` pair from the FFI into an
// owned `[]byte`, refusing lengths that don't fit a Go `int`.
//
// It exists because `C.GoBytes(ptr, C.int(len))` casts the length to
// `C.int`, which is 32-bit signed even on 64-bit hosts: a length with
// bit 31 set sign-flips negative (cgo then panics with "negative
// length", crashing the callback before any recover runs), and a
// length >= 4 GiB mod 2^32 yields a short copy that desyncs framing.
// Both are reachable from an inbound mesh peer's request / event body.
// `unsafe.Slice` takes a platform-`int` (64-bit) length and
// `bytes.Clone` copies into a Go-owned slice, sidestepping the
// truncation entirely.
//
// Returns `(nil, true)` for an empty/null payload (the FFI "no body"
// shape) and `(nil, false)` when the length is out of range; callers
// map `false` onto their own error / status convention.
func goBytesChecked(ptr *C.uint8_t, length C.size_t) ([]byte, bool) {
	if length == 0 || ptr == nil {
		return nil, true
	}
	if uint64(length) > uint64(math.MaxInt) {
		return nil, false
	}
	view := unsafe.Slice((*byte)(unsafe.Pointer(ptr)), int(length))
	return bytes.Clone(view), true
}

// trySend forwards `res` to `ch`, but selects on `ctx.Done()` so
// the goroutine can exit if the consumer cancelled. Returns
// `false` when ctx fired before the send completed.
func trySend(ctx context.Context, ch chan<- MeshDBResult, res MeshDBResult) bool {
	select {
	case ch <- res:
		return true
	case <-ctx.Done():
		// Best-effort: signal the cancellation to the consumer
		// without blocking forever if the buffer is full.
		select {
		case ch <- MeshDBResult{Err: ctx.Err()}:
		default:
		}
		return false
	}
}

// Free releases the FFI handle.
func (r *MeshDBRunner) Free() {
	if r == nil || r.ptr == nil {
		return
	}
	C.net_meshdb_runner_free(r.ptr)
	r.ptr = nil
	runtime.SetFinalizer(r, nil)
}

// =====================================================================
// Slice 2: composite factories
// =====================================================================
//
// Each factory takes an inner *MeshDBQuery, planning the outer
// operator on top. The inner is NOT consumed — callers retain
// ownership and should Free it separately. Returns
// ErrMeshDBInvalidArg when the factory returns null (caller
// passed invalid args).

// MeshDBQueryWindow constructs a tumbling-on-seq window with
// the given bucket size. Errors when size == 0.
func MeshDBQueryWindow(inner *MeshDBQuery, size uint64) (*MeshDBQuery, error) {
	if inner == nil || inner.ptr == nil {
		return nil, fmt.Errorf("window: inner is nil: %w", ErrMeshDBInvalidArg)
	}
	if size == 0 {
		return nil, fmt.Errorf("window: size must be >= 1: %w", ErrMeshDBInvalidArg)
	}
	ptr := C.net_meshdb_query_window(inner.ptr, C.uint64_t(size))
	if ptr == nil {
		return nil, fmt.Errorf("window: factory returned null: %w", ErrMeshDBInvalidArg)
	}
	q := &MeshDBQuery{ptr: ptr}
	runtime.SetFinalizer(q, func(q *MeshDBQuery) { q.Free() })
	return q, nil
}

// MeshDBQueryCount counts the rows produced by `inner`. `groupBy`
// is a slice of row-intrinsic field names: empty / nil for a
// single-bucket count, ["origin"], ["seq"], or ["origin","seq"]
// for grouped counts.
func MeshDBQueryCount(inner *MeshDBQuery, groupBy []string) (*MeshDBQuery, error) {
	if inner == nil || inner.ptr == nil {
		return nil, fmt.Errorf("count: inner is nil: %w", ErrMeshDBInvalidArg)
	}
	gbStr, free := buildGroupByCStr(groupBy)
	defer free()
	ptr := C.net_meshdb_query_count(inner.ptr, gbStr)
	if ptr == nil {
		return nil, fmt.Errorf("count: factory returned null: %w", ErrMeshDBInvalidArg)
	}
	q := &MeshDBQuery{ptr: ptr}
	runtime.SetFinalizer(q, func(q *MeshDBQuery) { q.Free() })
	return q, nil
}

// MeshDBQuerySum / Avg / Min / Max / DistinctCount: numeric
// aggregates over `field`. `kind` is one of: "sum", "avg",
// "min", "max", "distinct_count".
func MeshDBQueryNumericAgg(
	inner *MeshDBQuery,
	kind, field string,
	groupBy []string,
) (*MeshDBQuery, error) {
	if inner == nil || inner.ptr == nil {
		return nil, fmt.Errorf("%s: inner is nil: %w", kind, ErrMeshDBInvalidArg)
	}
	kindC := C.CString(kind)
	defer C.free(unsafe.Pointer(kindC))
	fieldC := C.CString(field)
	defer C.free(unsafe.Pointer(fieldC))
	gbStr, free := buildGroupByCStr(groupBy)
	defer free()
	ptr := C.net_meshdb_query_numeric_agg(inner.ptr, kindC, fieldC, gbStr)
	if ptr == nil {
		return nil, fmt.Errorf("%s: factory returned null: %w", kind, ErrMeshDBInvalidArg)
	}
	q := &MeshDBQuery{ptr: ptr}
	runtime.SetFinalizer(q, func(q *MeshDBQuery) { q.Free() })
	return q, nil
}

// MeshDBQueryPercentile: nearest-rank exact percentile. `p` is
// clamped at the FFI boundary — must be finite in [0, 1].
func MeshDBQueryPercentile(
	inner *MeshDBQuery,
	field string,
	p float64,
	groupBy []string,
) (*MeshDBQuery, error) {
	if inner == nil || inner.ptr == nil {
		return nil, fmt.Errorf("percentile: inner is nil: %w", ErrMeshDBInvalidArg)
	}
	fieldC := C.CString(field)
	defer C.free(unsafe.Pointer(fieldC))
	gbStr, free := buildGroupByCStr(groupBy)
	defer free()
	ptr := C.net_meshdb_query_percentile(inner.ptr, fieldC, C.double(p), gbStr)
	if ptr == nil {
		return nil, fmt.Errorf(
			"percentile: factory returned null (check p in [0,1]): %w",
			ErrMeshDBInvalidArg,
		)
	}
	q := &MeshDBQuery{ptr: ptr}
	runtime.SetFinalizer(q, func(q *MeshDBQuery) { q.Free() })
	return q, nil
}

// MeshDBQueryJoin: hash-join two queries. `kind` is one of
// "inner" / "left_outer" / "right_outer" / "full_outer".
// `key` is "origin", "seq", "origin,seq", or a JSON payload
// path. `strategy` is "hash_broadcast" (default) or
// "sort_merge"; empty string = default. `watermarkSecs` is
// informational under snapshot semantics; pass 5.0 to match
// the locked join watermark.
func MeshDBQueryJoin(
	left, right *MeshDBQuery,
	kind, key, strategy string,
	watermarkSecs float64,
) (*MeshDBQuery, error) {
	if left == nil || left.ptr == nil || right == nil || right.ptr == nil {
		return nil, fmt.Errorf("join: left or right is nil: %w", ErrMeshDBInvalidArg)
	}
	kindC := C.CString(kind)
	defer C.free(unsafe.Pointer(kindC))
	keyC := C.CString(key)
	defer C.free(unsafe.Pointer(keyC))
	var stratC *C.char
	if strategy != "" {
		stratC = C.CString(strategy)
		defer C.free(unsafe.Pointer(stratC))
	}
	ptr := C.net_meshdb_query_join(
		left.ptr, right.ptr, kindC, keyC, stratC, C.double(watermarkSecs),
	)
	if ptr == nil {
		return nil, fmt.Errorf(
			"join: factory returned null (check kind / key / strategy): %w",
			ErrMeshDBInvalidArg,
		)
	}
	q := &MeshDBQuery{ptr: ptr}
	runtime.SetFinalizer(q, func(q *MeshDBQuery) { q.Free() })
	return q, nil
}

// MeshDBLineageEntry describes one chain reached during a
// lineage walk. Pre-walked by the caller and handed to
// `MeshDBQueryLineageEmit(...)`. The SDK doesn't itself walk
// the fork-of: graph; callers maintain their own graph view
// and emit entries in walk order: index 0 is the start origin
// with Depth = 0; ancestors / descendants follow.
//
// `TipSeq == nil` means "no known tip" — the emitted row's
// Seq defaults to 0 in that case.
type MeshDBLineageEntry struct {
	Origin uint64
	Depth  uint32
	TipSeq *uint64
}

// MeshDBQueryLineageEmit constructs a `LineageEmit(origin,
// entries, direction)` query. `direction` is "back" or
// "forward". Each entry produces one ResultRow with origin =
// entry.Origin, seq = entry.TipSeq (or 0), payload empty.
// Compose with At / Between to fetch event content per chain.
func MeshDBQueryLineageEmit(
	origin uint64,
	entries []MeshDBLineageEntry,
	direction string,
) (*MeshDBQuery, error) {
	if direction != "back" && direction != "forward" {
		return nil, fmt.Errorf(
			"lineage_emit: direction %q not recognised (want 'back' or 'forward'): %w",
			direction, ErrMeshDBInvalidArg,
		)
	}
	type wireEntry struct {
		Origin uint64  `json:"origin"`
		Depth  uint32  `json:"depth"`
		TipSeq *uint64 `json:"tip_seq"`
	}
	wire := make([]wireEntry, 0, len(entries))
	for _, e := range entries {
		wire = append(wire, wireEntry{Origin: e.Origin, Depth: e.Depth, TipSeq: e.TipSeq})
	}
	jsonBytes, err := json.Marshal(wire)
	if err != nil {
		return nil, fmt.Errorf("lineage_emit: marshal entries: %w", ErrMeshDBInvalidArg)
	}
	entriesC := C.CString(string(jsonBytes))
	defer C.free(unsafe.Pointer(entriesC))
	dirC := C.CString(direction)
	defer C.free(unsafe.Pointer(dirC))
	ptr := C.net_meshdb_query_lineage_emit(C.uint64_t(origin), entriesC, dirC)
	if ptr == nil {
		return nil, fmt.Errorf(
			"lineage_emit: factory returned null (check entries JSON): %w",
			ErrMeshDBInvalidArg,
		)
	}
	q := &MeshDBQuery{ptr: ptr}
	runtime.SetFinalizer(q, func(q *MeshDBQuery) { q.Free() })
	return q, nil
}

// buildGroupByCStr renders a Go []string as a comma-separated
// C-string. Returns the *C.char + a free closure. nil/empty
// slice => nil *C.char (FFI treats null as "no grouping").
func buildGroupByCStr(groupBy []string) (*C.char, func()) {
	if len(groupBy) == 0 {
		return nil, func() {}
	}
	joined := groupBy[0]
	for _, s := range groupBy[1:] {
		joined += "," + s
	}
	c := C.CString(joined)
	return c, func() { C.free(unsafe.Pointer(c)) }
}

// =====================================================================
// Slice 2: payload decoder
// =====================================================================
//
// Sentinel rows from aggregate / join / window operators carry
// postcard-encoded envelopes in their payload. `DecodePayload`
// calls the FFI's JSON decoder, then unmarshals into a tagged
// Go struct. Plain at/between/latest rows (event payloads)
// return (nil, nil) — `Decoded == nil` means "not a sentinel".

// DecodedPayload is a tagged union over the three sentinel
// envelope shapes. Exactly one of Aggregate / Joined / Window
// is non-nil based on Kind.
type DecodedPayload struct {
	Kind      string                // "aggregate" | "joined" | "window"
	Aggregate *DecodedAggregate     // populated when Kind == "aggregate"
	Joined    *DecodedJoined        // populated when Kind == "joined"
	Window    *DecodedWindowBoundary // populated when Kind == "window"
}

// DecodedAggregate is the decoded form of an aggregate sentinel
// row. `Group` is nil for single-bucket (no group_by) results.
type DecodedAggregate struct {
	Group *DecodedGroupKey
	Value DecodedAggregateValue
}

// DecodedGroupKey identifies which group an aggregate row
// belongs to. Kind is "origin" / "seq" / "origin_seq".
type DecodedGroupKey struct {
	Kind   string
	Origin uint64 // populated when Kind contains "origin"
	Seq    uint64 // populated when Kind contains "seq"
}

// DecodedAggregateValue carries the numeric output of an
// aggregate. Kind is "count" / "sum" / "avg" / "min" / "max" /
// "distinct_count" / "percentile". `Value` is the float64 result
// (nil for empty groups under avg/min/max/percentile). `Count`
// mirrors Value as a uint64 for count / distinct_count kinds.
type DecodedAggregateValue struct {
	Kind  string
	Value *float64
	Count *uint64
}

// DecodedJoined holds the (left, right) pair from a join
// sentinel row. Either side may be nil for outer-join unmatched
// rows.
type DecodedJoined struct {
	Left  *MeshDBResultRow
	Right *MeshDBResultRow
}

// DecodedWindowBoundary holds a window bucket: half-open
// `[Start, End)` over seq, plus the rows that landed in it.
type DecodedWindowBoundary struct {
	Start uint64
	End   uint64
	Rows  []MeshDBResultRow
}

// DecodePayload parses a result-row's payload as a postcard-
// encoded sentinel envelope. Returns (nil, nil) for plain
// event-payload rows; (nil, err) on malformed FFI output.
func DecodePayload(row MeshDBResultRow) (*DecodedPayload, error) {
	if len(row.Payload) == 0 {
		return nil, nil
	}
	cstr := C.net_meshdb_decode_payload_json(
		(*C.uint8_t)(unsafe.Pointer(&row.Payload[0])),
		C.size_t(len(row.Payload)),
	)
	if cstr == nil {
		return nil, nil
	}
	defer C.net_meshdb_free_string(cstr)
	jsonBytes := []byte(C.GoString(cstr))
	return parseDecodedJSON(jsonBytes)
}

// parseDecodedJSON converts the FFI's JSON intermediate into a
// typed DecodedPayload. Extracted so unit tests can exercise
// the parser without an FFI round-trip.
func parseDecodedJSON(jsonBytes []byte) (*DecodedPayload, error) {
	var head struct {
		Kind string `json:"kind"`
	}
	if err := json.Unmarshal(jsonBytes, &head); err != nil {
		return nil, fmt.Errorf("decode payload: malformed FFI JSON: %w", err)
	}
	switch head.Kind {
	case "aggregate":
		return decodeAggregate(jsonBytes)
	case "joined":
		return decodeJoined(jsonBytes)
	case "window":
		return decodeWindow(jsonBytes)
	default:
		return nil, fmt.Errorf("decode payload: unknown kind %q", head.Kind)
	}
}

func decodeAggregate(b []byte) (*DecodedPayload, error) {
	var raw struct {
		Group *struct {
			Kind   string `json:"kind"`
			Origin uint64 `json:"origin"`
			Seq    uint64 `json:"seq"`
		} `json:"group"`
		Value struct {
			Kind  string   `json:"kind"`
			Value *float64 `json:"value"`
			Count *uint64  `json:"count"`
		} `json:"value"`
	}
	if err := json.Unmarshal(b, &raw); err != nil {
		return nil, fmt.Errorf("decode aggregate: %w", err)
	}
	out := &DecodedAggregate{
		Value: DecodedAggregateValue{
			Kind:  raw.Value.Kind,
			Value: raw.Value.Value,
			Count: raw.Value.Count,
		},
	}
	if raw.Group != nil {
		out.Group = &DecodedGroupKey{
			Kind:   raw.Group.Kind,
			Origin: raw.Group.Origin,
			Seq:    raw.Group.Seq,
		}
	}
	return &DecodedPayload{Kind: "aggregate", Aggregate: out}, nil
}

func decodeJoined(b []byte) (*DecodedPayload, error) {
	var raw struct {
		Left  *jsonRow `json:"left"`
		Right *jsonRow `json:"right"`
	}
	if err := json.Unmarshal(b, &raw); err != nil {
		return nil, fmt.Errorf("decode joined: %w", err)
	}
	out := &DecodedJoined{}
	if raw.Left != nil {
		row := raw.Left.toRow()
		out.Left = &row
	}
	if raw.Right != nil {
		row := raw.Right.toRow()
		out.Right = &row
	}
	return &DecodedPayload{Kind: "joined", Joined: out}, nil
}

func decodeWindow(b []byte) (*DecodedPayload, error) {
	var raw struct {
		Start uint64    `json:"start"`
		End   uint64    `json:"end"`
		Rows  []jsonRow `json:"rows"`
	}
	if err := json.Unmarshal(b, &raw); err != nil {
		return nil, fmt.Errorf("decode window: %w", err)
	}
	rows := make([]MeshDBResultRow, 0, len(raw.Rows))
	for _, jr := range raw.Rows {
		rows = append(rows, jr.toRow())
	}
	return &DecodedPayload{
		Kind:   "window",
		Window: &DecodedWindowBoundary{Start: raw.Start, End: raw.End, Rows: rows},
	}, nil
}

// jsonRow mirrors the FFI's per-row JSON shape:
// `{"origin": N, "seq": N, "payload": [B0, B1, ...]}`.
// Payload is decoded as `[]uint16` (rather than `[]byte`)
// because Go's json package treats `[]byte` as a base64 string;
// the FFI emits raw byte integers as a JSON array. Each value
// in the array fits in u8 by construction so the cast back to
// byte is lossless.
type jsonRow struct {
	Origin  uint64   `json:"origin"`
	Seq     uint64   `json:"seq"`
	Payload []uint16 `json:"payload"`
}

func (j jsonRow) toRow() MeshDBResultRow {
	payload := make([]byte, len(j.Payload))
	for i, b := range j.Payload {
		payload[i] = byte(b)
	}
	return MeshDBResultRow{
		Origin:  j.Origin,
		Seq:     j.Seq,
		Payload: payload,
	}
}

// =====================================================================
// Slice 3: Filter + Predicate
// =====================================================================
//
// The Go Predicate type is JSON-serialized then passed across
// the FFI; the Rust side parses it into a typed `Predicate` and
// converts to PredicateWire. Field names are row-intrinsic
// (`origin` / `seq`) or JSON payload paths matched against the
// synthetic per-row tag view.

// MeshDBPredicate is the Go-side predicate builder. It marshals
// to JSON in the shape the FFI parser expects. Construct via
// the package-level factory functions
// (`MeshDBPredicateEquals`, `MeshDBPredicateAnd`, ...). The
// `Kind` discriminator is the JSON field tag.
type MeshDBPredicate struct {
	Kind      string             `json:"kind"`
	Field     string             `json:"field,omitempty"`
	Value     string             `json:"value,omitempty"`
	Threshold *float64           `json:"threshold,omitempty"`
	Min       *float64           `json:"min,omitempty"`
	Max       *float64           `json:"max,omitempty"`
	Prefix    string             `json:"prefix,omitempty"`
	Pattern   string             `json:"pattern,omitempty"`
	Version   string             `json:"version,omitempty"`
	Children  []MeshDBPredicate  `json:"children,omitempty"`
	Child     *MeshDBPredicate   `json:"child,omitempty"`
}

// MeshDBPredicateExists matches rows where `field` is present.
func MeshDBPredicateExists(field string) MeshDBPredicate {
	return MeshDBPredicate{Kind: "exists", Field: field}
}

// MeshDBPredicateEquals matches rows where `field == value` (string equality).
func MeshDBPredicateEquals(field, value string) MeshDBPredicate {
	return MeshDBPredicate{Kind: "equals", Field: field, Value: value}
}

// MeshDBPredicateNumericAtLeast: `field >= threshold`.
func MeshDBPredicateNumericAtLeast(field string, threshold float64) MeshDBPredicate {
	return MeshDBPredicate{Kind: "numeric_at_least", Field: field, Threshold: &threshold}
}

// MeshDBPredicateNumericAtMost: `field <= threshold`.
func MeshDBPredicateNumericAtMost(field string, threshold float64) MeshDBPredicate {
	return MeshDBPredicate{Kind: "numeric_at_most", Field: field, Threshold: &threshold}
}

// MeshDBPredicateNumericInRange: `min <= field <= max`.
func MeshDBPredicateNumericInRange(field string, min, max float64) MeshDBPredicate {
	return MeshDBPredicate{Kind: "numeric_in_range", Field: field, Min: &min, Max: &max}
}

// MeshDBPredicateStringPrefix: `field.startsWith(prefix)`.
func MeshDBPredicateStringPrefix(field, prefix string) MeshDBPredicate {
	return MeshDBPredicate{Kind: "string_prefix", Field: field, Prefix: prefix}
}

// MeshDBPredicateStringMatches: substring match (regex behind a
// feature flag in the substrate; not exposed at the FFI yet).
func MeshDBPredicateStringMatches(field, pattern string) MeshDBPredicate {
	return MeshDBPredicate{Kind: "string_matches", Field: field, Pattern: pattern}
}

// MeshDBPredicateSemverAtLeast: `field >= version` (semver).
func MeshDBPredicateSemverAtLeast(field, version string) MeshDBPredicate {
	return MeshDBPredicate{Kind: "semver_at_least", Field: field, Version: version}
}

// MeshDBPredicateAnd: conjunction. Empty list is vacuously true
// (substrate semantics).
func MeshDBPredicateAnd(children ...MeshDBPredicate) MeshDBPredicate {
	return MeshDBPredicate{Kind: "and", Children: children}
}

// MeshDBPredicateOr: disjunction. Empty list is vacuously false.
func MeshDBPredicateOr(children ...MeshDBPredicate) MeshDBPredicate {
	return MeshDBPredicate{Kind: "or", Children: children}
}

// MeshDBPredicateNot: negation.
func MeshDBPredicateNot(child MeshDBPredicate) MeshDBPredicate {
	return MeshDBPredicate{Kind: "not", Child: &child}
}

// MeshDBQueryFilter wraps `inner` in a Filter operator over
// `predicate`. The predicate is JSON-encoded and passed across
// the FFI boundary.
func MeshDBQueryFilter(
	inner *MeshDBQuery,
	predicate MeshDBPredicate,
) (*MeshDBQuery, error) {
	if inner == nil || inner.ptr == nil {
		return nil, fmt.Errorf("filter: inner is nil: %w", ErrMeshDBInvalidArg)
	}
	predJSON, err := json.Marshal(predicate)
	if err != nil {
		return nil, fmt.Errorf("filter: predicate marshal failed: %w", err)
	}
	predC := C.CString(string(predJSON))
	defer C.free(unsafe.Pointer(predC))
	ptr := C.net_meshdb_query_filter_json(inner.ptr, predC)
	if ptr == nil {
		return nil, fmt.Errorf(
			"filter: factory returned null (check predicate shape): %w",
			ErrMeshDBInvalidArg,
		)
	}
	q := &MeshDBQuery{ptr: ptr}
	runtime.SetFinalizer(q, func(q *MeshDBQuery) { q.Free() })
	return q, nil
}

// =====================================================================
// Slice 4: fluent QueryBuilder
// =====================================================================
//
// Idiomatic Go-side composition over the package-level factory
// functions. Each chain step returns a fresh builder so aliased
// intermediates stay valid for parallel pipelines. Source
// methods (.At / .Between / .Latest) reset prior state. Build
// returns the accumulated MeshDBQuery (with finalizer
// installed); calling Build on an empty builder is an error.
//
// Per locked scope: builder covers the common ops (At /
// Between / Latest / Filter / Count / Sum / Avg / Min / Max /
// Percentile / DistinctCount / Window / Join). Rarer
// operators (lineage walks, payload-keyed group_by) still go
// through the package-level factory funcs.
//
// Each builder step accumulates errors lazily: if any
// intermediate factory returns an error, the builder records
// it and Build surfaces the first error encountered. This
// keeps chaining ergonomic — callers check error only at the
// terminal Build call.

// MeshDBQueryBuilder is the fluent builder handle.
type MeshDBQueryBuilder struct {
	state *MeshDBQuery
	err   error
}

// NewMeshDBQueryBuilder returns an empty builder. Use one of
// the source methods (At / Between / Latest) to seed it, then
// chain transformations and call Build.
func NewMeshDBQueryBuilder() *MeshDBQueryBuilder {
	return &MeshDBQueryBuilder{}
}

// At resets the builder to a fresh source: read seq at origin.
//
// Any prior chain step's state on the receiver is explicitly
// freed (Python / Node get away with GC; Go's FFI handle is
// not GC-managed and finalizers run at unknown later times, so
// we free deterministically here to avoid accumulating handles
// in long-lived test harnesses). Any accumulated builder error
// is preserved on the new builder so a Build() further down the
// chain still surfaces the first error.
//
// Aliasing caveat: the receiver's state is freed in place, so
// alias references to THIS builder (`base := q.Between(...);
// other := base.Count(); base.At(...)`) are not safe to
// continue using as a builder. Construct a new builder via
// `MeshQuery.builder()` per pipeline instead of resetting an
// aliased one. (Result queries produced by `Count` / `Sum` /
// etc. are independent: each factory clones the inner plan on
// the FFI side, so freeing the input builder's state does not
// affect a downstream pipeline that already consumed it.)
func (b *MeshDBQueryBuilder) At(origin, seq uint64) *MeshDBQueryBuilder {
	prevErr := b.consumeErr()
	b.resetState()
	return &MeshDBQueryBuilder{state: MeshDBQueryAt(origin, seq), err: prevErr}
}

// Between resets the builder to a fresh source: read events in
// the half-open seq range. Same lifetime / aliasing semantics
// as `At`. Errors from `MeshDBQueryBetween` are combined with
// any prior accumulated error (first error wins).
func (b *MeshDBQueryBuilder) Between(origin, start, end uint64) *MeshDBQueryBuilder {
	prevErr := b.consumeErr()
	b.resetState()
	q, err := MeshDBQueryBetween(origin, start, end)
	if prevErr != nil {
		err = prevErr
	}
	return &MeshDBQueryBuilder{state: q, err: err}
}

// Latest resets the builder to a fresh source: read the tip
// event of origin. Same lifetime / aliasing semantics as `At`.
func (b *MeshDBQueryBuilder) Latest(origin uint64) *MeshDBQueryBuilder {
	prevErr := b.consumeErr()
	b.resetState()
	return &MeshDBQueryBuilder{state: MeshDBQueryLatest(origin), err: prevErr}
}

// resetState frees any handle currently held in `b.state`. Safe
// to call when `b` is nil or `b.state` is nil. Idempotent.
func (b *MeshDBQueryBuilder) resetState() {
	if b == nil || b.state == nil {
		return
	}
	b.state.Free()
	b.state = nil
}

// consumeErr returns the builder's accumulated error and clears
// it on the receiver. Safe to call on a nil receiver.
func (b *MeshDBQueryBuilder) consumeErr() error {
	if b == nil {
		return nil
	}
	e := b.err
	b.err = nil
	return e
}

// Filter wraps the current pipeline in a row filter.
func (b *MeshDBQueryBuilder) Filter(predicate MeshDBPredicate) *MeshDBQueryBuilder {
	if b.err != nil {
		return b
	}
	if b.state == nil {
		return &MeshDBQueryBuilder{err: fmt.Errorf(
			"filter: builder has no source — call At/Between/Latest first: %w",
			ErrMeshDBInvalidArg,
		)}
	}
	q, err := MeshDBQueryFilter(b.state, predicate)
	return &MeshDBQueryBuilder{state: q, err: err}
}

// Count over the current pipeline. `groupBy` is the same
// row-intrinsic field-list as the factory.
func (b *MeshDBQueryBuilder) Count(groupBy []string) *MeshDBQueryBuilder {
	if b.err != nil {
		return b
	}
	if b.state == nil {
		return &MeshDBQueryBuilder{err: fmt.Errorf(
			"count: builder has no source — call At/Between/Latest first: %w",
			ErrMeshDBInvalidArg,
		)}
	}
	q, err := MeshDBQueryCount(b.state, groupBy)
	return &MeshDBQueryBuilder{state: q, err: err}
}

// Sum / Avg / Min / Max over the current pipeline. `kind` is
// one of "sum"/"avg"/"min"/"max"/"distinct_count".
func (b *MeshDBQueryBuilder) NumericAgg(
	kind, field string,
	groupBy []string,
) *MeshDBQueryBuilder {
	if b.err != nil {
		return b
	}
	if b.state == nil {
		return &MeshDBQueryBuilder{err: fmt.Errorf(
			"%s: builder has no source — call At/Between/Latest first: %w",
			kind, ErrMeshDBInvalidArg,
		)}
	}
	q, err := MeshDBQueryNumericAgg(b.state, kind, field, groupBy)
	return &MeshDBQueryBuilder{state: q, err: err}
}

// Percentile over the current pipeline.
func (b *MeshDBQueryBuilder) Percentile(
	field string,
	p float64,
	groupBy []string,
) *MeshDBQueryBuilder {
	if b.err != nil {
		return b
	}
	if b.state == nil {
		return &MeshDBQueryBuilder{err: fmt.Errorf(
			"percentile: builder has no source — call At/Between/Latest first: %w",
			ErrMeshDBInvalidArg,
		)}
	}
	q, err := MeshDBQueryPercentile(b.state, field, p, groupBy)
	return &MeshDBQueryBuilder{state: q, err: err}
}

// Window over the current pipeline.
func (b *MeshDBQueryBuilder) Window(size uint64) *MeshDBQueryBuilder {
	if b.err != nil {
		return b
	}
	if b.state == nil {
		return &MeshDBQueryBuilder{err: fmt.Errorf(
			"window: builder has no source — call At/Between/Latest first: %w",
			ErrMeshDBInvalidArg,
		)}
	}
	q, err := MeshDBQueryWindow(b.state, size)
	return &MeshDBQueryBuilder{state: q, err: err}
}

// Join the current pipeline (left) with `right`. See
// `MeshDBQueryJoin` for the parameter docs.
func (b *MeshDBQueryBuilder) Join(
	right *MeshDBQuery,
	kind, key, strategy string,
	watermarkSecs float64,
) *MeshDBQueryBuilder {
	if b.err != nil {
		return b
	}
	if b.state == nil {
		return &MeshDBQueryBuilder{err: fmt.Errorf(
			"join: builder has no source — call At/Between/Latest first: %w",
			ErrMeshDBInvalidArg,
		)}
	}
	q, err := MeshDBQueryJoin(b.state, right, kind, key, strategy, watermarkSecs)
	return &MeshDBQueryBuilder{state: q, err: err}
}

// Build returns the accumulated MeshDBQuery. Returns the first
// error encountered during chaining, or an error if no source
// was seeded.
func (b *MeshDBQueryBuilder) Build() (*MeshDBQuery, error) {
	if b.err != nil {
		return nil, b.err
	}
	if b.state == nil {
		return nil, fmt.Errorf(
			"build: builder has no source — call At/Between/Latest first: %w",
			ErrMeshDBInvalidArg,
		)
	}
	return b.state, nil
}
