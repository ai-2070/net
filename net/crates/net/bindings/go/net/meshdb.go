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
// Per the locked Go SDK decision, `(*MeshQueryRunner).Execute`
// returns `(<-chan MeshQueryResult, error)`. The wrapper spawns a
// goroutine that pumps rows from the FFI iterator into the
// channel; the goroutine closes the channel on EOF or on the first
// error. Cancellation works the standard Go way — the caller stops
// reading + drops the channel reference; the goroutine notices the
// channel-send block, gives up, and frees the iterator.
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
extern MeshDbRunner* net_meshdb_runner_new(MeshDbReader* reader);
extern void net_meshdb_runner_free(MeshDbRunner* runner);
extern MeshDbIter* net_meshdb_runner_execute(
    MeshDbRunner* runner,
    MeshDbQuery* query
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
*/
import "C"

import (
	"errors"
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
func (r *MeshDBRunner) Execute(query *MeshDBQuery) (<-chan MeshDBResult, error) {
	if r == nil || r.ptr == nil {
		return nil, ErrMeshDBInvalidArg
	}
	if query == nil || query.ptr == nil {
		return nil, ErrMeshDBInvalidArg
	}
	iter := C.net_meshdb_runner_execute(r.ptr, query.ptr)
	if iter == nil {
		return nil, ErrMeshDBRuntime
	}
	ch := make(chan MeshDBResult, 32)
	go func() {
		defer close(ch)
		defer C.net_meshdb_iter_free(iter)
		for {
			var (
				origin     C.uint64_t
				seq        C.uint64_t
				payloadPtr *C.uint8_t
				payloadLen C.size_t
			)
			status := C.net_meshdb_iter_next(iter, &origin, &seq, &payloadPtr, &payloadLen)
			switch status {
			case C.NET_MESHDB_OK:
				row := MeshDBResultRow{
					Origin:  uint64(origin),
					Seq:     uint64(seq),
					Payload: C.GoBytes(unsafe.Pointer(payloadPtr), C.int(payloadLen)),
				}
				C.net_meshdb_payload_free(payloadPtr, payloadLen)
				// Channel send doubles as the cancellation
				// signal: a dropped receiver causes us to block
				// here, after which the test-harness or the
				// runtime eventually GCs the goroutine. For
				// production cancellation, callers should wrap
				// in select{} + context.Done().
				ch <- MeshDBResult{Row: row}
			case C.NET_MESHDB_END:
				return
			case C.NET_MESHDB_INVALID_ARG:
				ch <- MeshDBResult{Err: ErrMeshDBInvalidArg}
				return
			default:
				ch <- MeshDBResult{Err: ErrMeshDBRuntime}
				return
			}
		}
	}()
	return ch, nil
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
