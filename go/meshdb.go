// Package net — MeshDB query layer.
//
// The MeshDB surface is compiled into the `libnet_meshdb` cdylib (separate
// from `libnet`). Build with `cargo build --release -p net-meshdb-ffi`.
//
// This file exposes the minimal Reader / Runner / Query / Iterator path:
// build an in-memory chain reader, append events, construct a runner,
// execute a query, drain rows. The full surface (window aggregates,
// joins, filter predicates, cache options) lives in the reference Go
// binding at `net/crates/net/bindings/go/net/meshdb.go` — extend this
// file as you need additional operators.

package net

/*
#cgo LDFLAGS: -L${SRCDIR}/../net/crates/net/target/release -lnet_meshdb
#include <stdint.h>
#include <stdlib.h>

typedef struct MeshDbReader MeshDbReader;
typedef struct MeshDbRunner MeshDbRunner;
typedef struct MeshDbQuery  MeshDbQuery;
typedef struct MeshDbIter   MeshDbIter;

extern MeshDbReader* net_meshdb_reader_new(void);
extern void net_meshdb_reader_free(MeshDbReader* reader);
extern int  net_meshdb_reader_append(MeshDbReader* reader,
                                     uint64_t origin, uint64_t seq,
                                     const uint8_t* payload, size_t payload_len);

extern MeshDbQuery* net_meshdb_query_at(uint64_t origin, uint64_t seq);
extern MeshDbQuery* net_meshdb_query_between(uint64_t origin,
                                             uint64_t start, uint64_t end);
extern MeshDbQuery* net_meshdb_query_latest(uint64_t origin);
extern void         net_meshdb_query_free(MeshDbQuery* query);

extern MeshDbRunner* net_meshdb_runner_new(const MeshDbReader* reader);
extern void          net_meshdb_runner_free(MeshDbRunner* runner);
extern MeshDbIter*   net_meshdb_runner_execute(MeshDbRunner* runner,
                                               const MeshDbQuery* query);

extern int  net_meshdb_iter_next(MeshDbIter* iter,
                                 uint64_t* origin_out, uint64_t* seq_out,
                                 uint8_t** payload_out_ptr, size_t* payload_out_len);
extern void net_meshdb_payload_free(uint8_t* ptr, size_t len);
extern void net_meshdb_iter_free(MeshDbIter* iter);
*/
import "C"

import (
	"errors"
	"fmt"
	"runtime"
	"sync"
	"unsafe"
)

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

// ErrMeshDb is the umbrella for any MeshDB-layer failure.
var ErrMeshDb = errors.New("meshdb")

// ErrMeshDbInvalidArg corresponds to NET_MESHDB_INVALID_ARG from the C ABI.
var ErrMeshDbInvalidArg = fmt.Errorf("%w: invalid argument", ErrMeshDb)

// ErrMeshDbEnd indicates the iterator reached end-of-stream (NET_MESHDB_END).
// Not a true error — callers loop on it.
var ErrMeshDbEnd = fmt.Errorf("%w: iterator end", ErrMeshDb)

// ---------------------------------------------------------------------------
// MeshDbReader
// ---------------------------------------------------------------------------

// MeshDbReader is an in-memory chain reader. Append events to it, then
// build a Runner to query over them. Cheap to clone via Arc on the
// substrate side — multiple Runners over the same Reader share state.
type MeshDbReader struct {
	mu     sync.RWMutex
	handle *C.MeshDbReader
}

// NewMeshDbReader allocates a fresh in-memory chain reader.
func NewMeshDbReader() *MeshDbReader {
	h := C.net_meshdb_reader_new()
	r := &MeshDbReader{handle: h}
	runtime.SetFinalizer(r, (*MeshDbReader).Free)
	return r
}

// Append adds a `(origin, seq, payload)` row to the reader. New rows are
// visible to every Runner built from this reader.
func (r *MeshDbReader) Append(origin, seq uint64, payload []byte) error {
	r.mu.RLock()
	defer r.mu.RUnlock()
	if r.handle == nil {
		return fmt.Errorf("%w: reader closed", ErrMeshDb)
	}
	var ptr *C.uint8_t
	if len(payload) > 0 {
		ptr = (*C.uint8_t)(unsafe.Pointer(&payload[0]))
	}
	rc := C.net_meshdb_reader_append(
		r.handle,
		C.uint64_t(origin),
		C.uint64_t(seq),
		ptr,
		C.size_t(len(payload)),
	)
	if rc != 0 {
		return fmt.Errorf("%w: append rc=%d", ErrMeshDb, int(rc))
	}
	return nil
}

// Free releases the reader. Idempotent. Outstanding Runners built from
// this reader stay valid — they hold their own Arc clone.
func (r *MeshDbReader) Free() {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.handle != nil {
		C.net_meshdb_reader_free(r.handle)
		r.handle = nil
		runtime.SetFinalizer(r, nil)
	}
}

// ---------------------------------------------------------------------------
// MeshDbQuery factories
// ---------------------------------------------------------------------------

// MeshDbQuery is a planned query AST. Construct via QueryAt / QueryBetween
// / QueryLatest, pass to (*MeshDbRunner).Execute, then Free.
type MeshDbQuery struct {
	mu     sync.Mutex
	handle *C.MeshDbQuery
}

func wrapQuery(h *C.MeshDbQuery) (*MeshDbQuery, error) {
	if h == nil {
		return nil, fmt.Errorf("%w: query constructor returned NULL", ErrMeshDb)
	}
	q := &MeshDbQuery{handle: h}
	runtime.SetFinalizer(q, (*MeshDbQuery).Free)
	return q, nil
}

// QueryAt — read a single event by `(origin, seq)`.
func QueryAt(origin, seq uint64) (*MeshDbQuery, error) {
	return wrapQuery(C.net_meshdb_query_at(C.uint64_t(origin), C.uint64_t(seq)))
}

// QueryBetween — half-open seq range `[start, end)` on a chain.
func QueryBetween(origin, start, end uint64) (*MeshDbQuery, error) {
	if start >= end {
		return nil, fmt.Errorf("%w: start (%d) must be < end (%d)", ErrMeshDbInvalidArg, start, end)
	}
	return wrapQuery(C.net_meshdb_query_between(
		C.uint64_t(origin), C.uint64_t(start), C.uint64_t(end),
	))
}

// QueryLatest — tip event for a chain.
func QueryLatest(origin uint64) (*MeshDbQuery, error) {
	return wrapQuery(C.net_meshdb_query_latest(C.uint64_t(origin)))
}

// Free releases the query handle. Idempotent.
func (q *MeshDbQuery) Free() {
	q.mu.Lock()
	defer q.mu.Unlock()
	if q.handle != nil {
		C.net_meshdb_query_free(q.handle)
		q.handle = nil
		runtime.SetFinalizer(q, nil)
	}
}

// ---------------------------------------------------------------------------
// MeshDbRunner
// ---------------------------------------------------------------------------

// MeshDbRunner drives query execution against a MeshDbReader. The runner
// clones the reader's underlying Arc<InMemoryStore> on construction;
// freeing the reader before the runner is sound.
type MeshDbRunner struct {
	mu     sync.RWMutex
	handle *C.MeshDbRunner
}

// NewMeshDbRunner builds a runner over `reader`. Returns nil and an
// error if the reader has already been freed.
func NewMeshDbRunner(reader *MeshDbReader) (*MeshDbRunner, error) {
	if reader == nil {
		return nil, fmt.Errorf("%w: nil reader", ErrMeshDbInvalidArg)
	}
	reader.mu.RLock()
	defer reader.mu.RUnlock()
	if reader.handle == nil {
		return nil, fmt.Errorf("%w: reader freed", ErrMeshDb)
	}
	h := C.net_meshdb_runner_new(reader.handle)
	if h == nil {
		return nil, fmt.Errorf("%w: runner constructor returned NULL", ErrMeshDb)
	}
	r := &MeshDbRunner{handle: h}
	runtime.SetFinalizer(r, (*MeshDbRunner).Free)
	return r, nil
}

// Execute runs `query` and returns a drained iterator. The query is not
// consumed — pass it to Execute again or call its Free when done.
func (r *MeshDbRunner) Execute(query *MeshDbQuery) (*MeshDbIter, error) {
	r.mu.RLock()
	defer r.mu.RUnlock()
	if r.handle == nil {
		return nil, fmt.Errorf("%w: runner freed", ErrMeshDb)
	}
	if query == nil {
		return nil, fmt.Errorf("%w: nil query", ErrMeshDbInvalidArg)
	}
	query.mu.Lock()
	defer query.mu.Unlock()
	if query.handle == nil {
		return nil, fmt.Errorf("%w: query freed", ErrMeshDb)
	}
	h := C.net_meshdb_runner_execute(r.handle, query.handle)
	if h == nil {
		return nil, fmt.Errorf("%w: planner / executor failure", ErrMeshDb)
	}
	it := &MeshDbIter{handle: h}
	runtime.SetFinalizer(it, (*MeshDbIter).Free)
	return it, nil
}

// Free releases the runner. Idempotent. Outstanding iterators stay
// valid — they own their drained rows independently.
func (r *MeshDbRunner) Free() {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.handle != nil {
		C.net_meshdb_runner_free(r.handle)
		r.handle = nil
		runtime.SetFinalizer(r, nil)
	}
}

// ---------------------------------------------------------------------------
// MeshDbIter
// ---------------------------------------------------------------------------

// MeshDbRow is one row pulled from an iterator.
type MeshDbRow struct {
	Origin  uint64
	Seq     uint64
	Payload []byte
}

// MeshDbIter is a result-row stream. Call Next until ErrMeshDbEnd, then
// Free. The iterator owns its rows independently of the parent runner.
type MeshDbIter struct {
	mu     sync.Mutex
	handle *C.MeshDbIter
}

// Next pulls the next row. Returns ErrMeshDbEnd on end-of-stream.
func (it *MeshDbIter) Next() (MeshDbRow, error) {
	it.mu.Lock()
	defer it.mu.Unlock()
	if it.handle == nil {
		return MeshDbRow{}, fmt.Errorf("%w: iterator freed", ErrMeshDb)
	}
	var origin, seq C.uint64_t
	var payloadPtr *C.uint8_t
	var payloadLen C.size_t
	rc := C.net_meshdb_iter_next(it.handle, &origin, &seq, &payloadPtr, &payloadLen)
	switch rc {
	case 0:
		// Copy payload into Go memory and free the substrate-side buffer.
		var payload []byte
		if payloadPtr != nil && payloadLen > 0 {
			payload = C.GoBytes(unsafe.Pointer(payloadPtr), C.int(payloadLen))
			C.net_meshdb_payload_free(payloadPtr, payloadLen)
		}
		return MeshDbRow{
			Origin:  uint64(origin),
			Seq:     uint64(seq),
			Payload: payload,
		}, nil
	case 1: // NET_MESHDB_END
		return MeshDbRow{}, ErrMeshDbEnd
	case 2: // NET_MESHDB_INVALID_ARG
		return MeshDbRow{}, ErrMeshDbInvalidArg
	default:
		return MeshDbRow{}, fmt.Errorf("%w: iter_next rc=%d", ErrMeshDb, int(rc))
	}
}

// Drain pulls every remaining row into a slice. Convenience for callers
// who know the result set is small.
func (it *MeshDbIter) Drain() ([]MeshDbRow, error) {
	var rows []MeshDbRow
	for {
		row, err := it.Next()
		if errors.Is(err, ErrMeshDbEnd) {
			return rows, nil
		}
		if err != nil {
			return rows, err
		}
		rows = append(rows, row)
	}
}

// Free releases the iterator. Idempotent. Safe to call before fully
// draining — pending rows are dropped.
func (it *MeshDbIter) Free() {
	it.mu.Lock()
	defer it.mu.Unlock()
	if it.handle != nil {
		C.net_meshdb_iter_free(it.handle)
		it.handle = nil
		runtime.SetFinalizer(it, nil)
	}
}
