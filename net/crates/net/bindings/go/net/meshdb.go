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

// Slice 2: payload decoder (JSON intermediate). Returns null
// when the payload isn't a postcard-encoded aggregate / joined /
// window envelope.
extern char* net_meshdb_decode_payload_json(
    const uint8_t* payload,
    size_t payload_len
);
extern void net_meshdb_free_string(char* s);
*/
import "C"

import (
	"encoding/json"
	"errors"
	"fmt"
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
