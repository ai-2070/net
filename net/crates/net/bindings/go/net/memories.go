// Memories adapter — Go wrapper over the C ABI exported by
// `net::ffi::cortex` (the Memories half).
//
// Mirrors `tasks.go` for the Memories domain. See that file for
// the full lifecycle pattern, non-blocking RYW semantics, and
// context-aware wait helpers.
//
// Domain operations:
//
//   - Store(input)   — emit MEMORY_STORED
//   - Retag(input)   — emit MEMORY_RETAGGED
//   - Pin(id)        — emit MEMORY_PINNED
//   - Unpin(id)      — emit MEMORY_UNPINNED
//   - Delete(id)     — emit MEMORY_DELETED
//   - List(filter)   — query the materialized state
//   - SnapshotAndWatch — atomic snapshot + delta stream cursor

package net

/*
#include <stdint.h>
#include <stdlib.h>

typedef struct RedexHandle RedexHandle;
typedef struct MemoriesAdapterHandle MemoriesAdapterHandle;
typedef struct MemoriesWatchHandle MemoriesWatchHandle;

extern int net_memories_adapter_open(
    RedexHandle* redex,
    uint64_t origin_hash,
    int persistent,
    MemoriesAdapterHandle** out_handle
);
extern int net_memories_adapter_close(MemoriesAdapterHandle* handle);
extern void net_memories_adapter_free(MemoriesAdapterHandle* handle);

extern int net_memories_store(
    MemoriesAdapterHandle* handle,
    const char* input_json,
    uint64_t* out_seq
);
extern int net_memories_retag(
    MemoriesAdapterHandle* handle,
    const char* input_json,
    uint64_t* out_seq
);
extern int net_memories_pin(
    MemoriesAdapterHandle* handle,
    uint64_t id,
    uint64_t now_ns,
    uint64_t* out_seq
);
extern int net_memories_unpin(
    MemoriesAdapterHandle* handle,
    uint64_t id,
    uint64_t now_ns,
    uint64_t* out_seq
);
extern int net_memories_delete(
    MemoriesAdapterHandle* handle,
    uint64_t id,
    uint64_t* out_seq
);
extern int net_memories_wait_for_seq(
    MemoriesAdapterHandle* handle,
    uint64_t seq,
    uint32_t timeout_ms
);
extern int net_memories_wait_for_token(
    MemoriesAdapterHandle* handle,
    uint64_t origin_hash,
    uint64_t seq,
    uint32_t timeout_ms
);
extern int net_memories_list(
    MemoriesAdapterHandle* handle,
    const char* filter_json,
    char** out_json,
    size_t* out_len
);
extern int net_memories_snapshot_and_watch(
    MemoriesAdapterHandle* handle,
    const char* filter_json,
    char** out_snapshot,
    size_t* out_snapshot_len,
    MemoriesWatchHandle** out_cursor
);
extern int net_memories_watch_next(
    MemoriesWatchHandle* cursor,
    uint32_t timeout_ms,
    char** out_json,
    size_t* out_len
);
extern void net_memories_watch_free(MemoriesWatchHandle* cursor);
*/
import "C"

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"runtime"
	"sync"
	"time"
	"unsafe"
)

// =====================================================================
// Errors — Memories-specific aliases over the shared ErrTasks family
// =====================================================================

// ErrMemories is the umbrella error for any Memories-adapter failure.
var ErrMemories = errors.New("memories")

// ErrMemoriesTimeout / WrongOrigin / QueueFull / FoldStopped / Panic
// mirror the Tasks-side errors. The substrate codes are shared, so
// `errors.Is(err, ErrMemoriesTimeout)` and `errors.Is(err, ErrTasksTimeout)`
// would point at the same semantic. We keep separate sentinels for
// the domain-error namespace; callers who want generic timeout
// handling can `errors.Is(err, ErrMemoriesTimeout)` per-adapter.
var ErrMemoriesTimeout = fmt.Errorf("%w: wait timeout", ErrMemories)
var ErrMemoriesWrongOrigin = fmt.Errorf("%w: wrong token origin", ErrMemories)
var ErrMemoriesQueueFull = fmt.Errorf("%w: wait-queue saturated", ErrMemories)
var ErrMemoriesFoldStopped = fmt.Errorf("%w: fold task stopped", ErrMemories)
var ErrMemoriesPanic = fmt.Errorf("%w: substrate panic", ErrMemories)

// =====================================================================
// Types
// =====================================================================

// Memory is the Go-facing record. Matches the JSON shape the FFI
// emits from `MemoryJson`.
type Memory struct {
	ID        uint64   `json:"id"`
	Content   string   `json:"content"`
	Tags      []string `json:"tags"`
	Source    string   `json:"source"`
	CreatedNs uint64   `json:"created_ns"`
	UpdatedNs uint64   `json:"updated_ns"`
	Pinned    bool     `json:"pinned"`
}

// MemoriesOrderBy mirrors the substrate enum.
type MemoriesOrderBy string

const (
	MemoriesOrderByCreatedAsc  MemoriesOrderBy = "created_asc"
	MemoriesOrderByCreatedDesc MemoriesOrderBy = "created_desc"
	MemoriesOrderByUpdatedAsc  MemoriesOrderBy = "updated_asc"
	MemoriesOrderByUpdatedDesc MemoriesOrderBy = "updated_desc"
)

// MemoriesFilter is the Go-side filter passed to List /
// SnapshotAndWatch.
type MemoriesFilter struct {
	Source          *string          `json:"source,omitempty"`
	ContentContains *string          `json:"content_contains,omitempty"`
	Tag             *string          `json:"tag,omitempty"`
	AnyTag          []string         `json:"any_tag,omitempty"`
	AllTags         []string         `json:"all_tags,omitempty"`
	Pinned          *bool            `json:"pinned,omitempty"`
	CreatedAfterNs  *uint64          `json:"created_after_ns,omitempty"`
	CreatedBeforeNs *uint64          `json:"created_before_ns,omitempty"`
	UpdatedAfterNs  *uint64          `json:"updated_after_ns,omitempty"`
	UpdatedBeforeNs *uint64          `json:"updated_before_ns,omitempty"`
	OrderBy         *MemoriesOrderBy `json:"order_by,omitempty"`
	Limit           *uint32          `json:"limit,omitempty"`
}

// MemoryStoreInput is the JSON shape `net_memories_store` accepts.
type MemoryStoreInput struct {
	ID      uint64   `json:"id"`
	Content string   `json:"content"`
	Tags    []string `json:"tags"`
	Source  string   `json:"source"`
	NowNs   uint64   `json:"now_ns"`
}

// MemoryRetagInput is the JSON shape `net_memories_retag` accepts.
type MemoryRetagInput struct {
	ID    uint64   `json:"id"`
	Tags  []string `json:"tags"`
	NowNs uint64   `json:"now_ns"`
}

// =====================================================================
// MemoriesAdapter
// =====================================================================

// MemoriesAdapter wraps the C `*MemoriesAdapterHandle`.
type MemoriesAdapter struct {
	mu         sync.RWMutex
	handle     *C.MemoriesAdapterHandle
	originHash uint64
}

// OpenMemoriesAdapter opens a Memories adapter against the supplied
// Redex. Same lifecycle pattern as TasksAdapter.
func OpenMemoriesAdapter(redex *Redex, originHash uint64, persistent bool) (*MemoriesAdapter, error) {
	if redex == nil {
		return nil, fmt.Errorf("%w: nil redex", ErrMemories)
	}
	rh := redex.Handle()
	if rh == nil {
		return nil, fmt.Errorf("%w: redex closed", ErrMemories)
	}
	var handle *C.MemoriesAdapterHandle
	var pflag C.int
	if persistent {
		pflag = 1
	}
	rc := C.net_memories_adapter_open(
		(*C.RedexHandle)(rh),
		C.uint64_t(originHash),
		pflag,
		&handle,
	)
	if rc != 0 {
		return nil, fmt.Errorf("%w: open failed (rc=%d)", ErrMemories, int(rc))
	}
	a := &MemoriesAdapter{handle: handle, originHash: originHash}
	runtime.SetFinalizer(a, func(a *MemoriesAdapter) { _ = a.Close() })
	return a, nil
}

// newMemoriesAdapterFromRaw wraps a raw `*MemoriesAdapterHandle` (obtained
// from another cgo file in the same package, e.g. `net_netdb_memories`)
// into a `*MemoriesAdapter`. The handle must already be Arc-cloned by the
// caller — freeing the returned adapter calls
// `net_memories_adapter_free` exactly once.
//
// Used by NetDb.Memories() to bridge across the per-file cgo type wall.
func newMemoriesAdapterFromRaw(handle unsafe.Pointer, originHash uint64) *MemoriesAdapter {
	a := &MemoriesAdapter{handle: (*C.MemoriesAdapterHandle)(handle), originHash: originHash}
	runtime.SetFinalizer(a, func(a *MemoriesAdapter) { _ = a.Close() })
	return a
}

// OriginHash returns the adapter's bound origin_hash.
func (a *MemoriesAdapter) OriginHash() uint64 {
	return a.originHash
}

// Close releases the underlying handle. Idempotent.
func (a *MemoriesAdapter) Close() error {
	a.mu.Lock()
	defer a.mu.Unlock()
	if a.handle == nil {
		return nil
	}
	rc := C.net_memories_adapter_close(a.handle)
	C.net_memories_adapter_free(a.handle)
	a.handle = nil
	runtime.SetFinalizer(a, nil)
	if rc != 0 && rc != -100 {
		return fmt.Errorf("%w: close rc=%d", ErrMemories, int(rc))
	}
	return nil
}

func (a *MemoriesAdapter) withHandle(f func(*C.MemoriesAdapterHandle) C.int) (C.int, error) {
	a.mu.RLock()
	defer a.mu.RUnlock()
	if a.handle == nil {
		return 0, fmt.Errorf("%w: adapter closed", ErrMemories)
	}
	return f(a.handle), nil
}

// Store emits a MEMORY_STORED event.
func (a *MemoriesAdapter) Store(input MemoryStoreInput) (WriteToken, error) {
	buf, err := json.Marshal(input)
	if err != nil {
		return WriteToken{}, fmt.Errorf("%w: store encode: %v", ErrMemories, err)
	}
	cInput := C.CString(string(buf))
	defer C.free(unsafe.Pointer(cInput))
	var seq C.uint64_t
	rc, err := a.withHandle(func(h *C.MemoriesAdapterHandle) C.int {
		return C.net_memories_store(h, cInput, &seq)
	})
	if err != nil {
		return WriteToken{}, err
	}
	if rc != 0 {
		return WriteToken{}, fmt.Errorf("%w: store rc=%d", ErrMemories, int(rc))
	}
	return WriteToken{OriginHash: a.originHash, Seq: uint64(seq)}, nil
}

// Retag emits a MEMORY_RETAGGED event.
func (a *MemoriesAdapter) Retag(input MemoryRetagInput) (WriteToken, error) {
	buf, err := json.Marshal(input)
	if err != nil {
		return WriteToken{}, fmt.Errorf("%w: retag encode: %v", ErrMemories, err)
	}
	cInput := C.CString(string(buf))
	defer C.free(unsafe.Pointer(cInput))
	var seq C.uint64_t
	rc, err := a.withHandle(func(h *C.MemoriesAdapterHandle) C.int {
		return C.net_memories_retag(h, cInput, &seq)
	})
	if err != nil {
		return WriteToken{}, err
	}
	if rc != 0 {
		return WriteToken{}, fmt.Errorf("%w: retag rc=%d", ErrMemories, int(rc))
	}
	return WriteToken{OriginHash: a.originHash, Seq: uint64(seq)}, nil
}

// Pin emits a MEMORY_PINNED event.
func (a *MemoriesAdapter) Pin(id, nowNs uint64) (WriteToken, error) {
	var seq C.uint64_t
	rc, err := a.withHandle(func(h *C.MemoriesAdapterHandle) C.int {
		return C.net_memories_pin(h, C.uint64_t(id), C.uint64_t(nowNs), &seq)
	})
	if err != nil {
		return WriteToken{}, err
	}
	if rc != 0 {
		return WriteToken{}, fmt.Errorf("%w: pin rc=%d", ErrMemories, int(rc))
	}
	return WriteToken{OriginHash: a.originHash, Seq: uint64(seq)}, nil
}

// Unpin emits a MEMORY_UNPINNED event.
func (a *MemoriesAdapter) Unpin(id, nowNs uint64) (WriteToken, error) {
	var seq C.uint64_t
	rc, err := a.withHandle(func(h *C.MemoriesAdapterHandle) C.int {
		return C.net_memories_unpin(h, C.uint64_t(id), C.uint64_t(nowNs), &seq)
	})
	if err != nil {
		return WriteToken{}, err
	}
	if rc != 0 {
		return WriteToken{}, fmt.Errorf("%w: unpin rc=%d", ErrMemories, int(rc))
	}
	return WriteToken{OriginHash: a.originHash, Seq: uint64(seq)}, nil
}

// Delete emits a MEMORY_DELETED event.
func (a *MemoriesAdapter) Delete(id uint64) (WriteToken, error) {
	var seq C.uint64_t
	rc, err := a.withHandle(func(h *C.MemoriesAdapterHandle) C.int {
		return C.net_memories_delete(h, C.uint64_t(id), &seq)
	})
	if err != nil {
		return WriteToken{}, err
	}
	if rc != 0 {
		return WriteToken{}, fmt.Errorf("%w: delete rc=%d", ErrMemories, int(rc))
	}
	return WriteToken{OriginHash: a.originHash, Seq: uint64(seq)}, nil
}

// List returns every Memory matching `filter`. Pass `nil` for no
// filter.
func (a *MemoriesAdapter) List(filter *MemoriesFilter) ([]Memory, error) {
	cFilter, free, err := encodeFilterJSON(filter)
	if err != nil {
		return nil, err
	}
	defer free()
	var outJSON *C.char
	var outLen C.size_t
	rc, err := a.withHandle(func(h *C.MemoriesAdapterHandle) C.int {
		return C.net_memories_list(h, cFilter, &outJSON, &outLen)
	})
	if err != nil {
		return nil, err
	}
	if rc != 0 {
		return nil, fmt.Errorf("%w: list rc=%d", ErrMemories, int(rc))
	}
	defer C.net_free_string(outJSON)
	buf := C.GoBytes(unsafe.Pointer(outJSON), C.int(outLen))
	var mems []Memory
	if err := json.Unmarshal(buf, &mems); err != nil {
		return nil, fmt.Errorf("%w: list decode: %v", ErrMemories, err)
	}
	return mems, nil
}

// WaitForSeq blocks until fold has processed `seq`.
func (a *MemoriesAdapter) WaitForSeq(seq uint64, timeout time.Duration) error {
	ms := durationToMs(timeout)
	rc, err := a.withHandle(func(h *C.MemoriesAdapterHandle) C.int {
		return C.net_memories_wait_for_seq(h, C.uint64_t(seq), C.uint32_t(ms))
	})
	if err != nil {
		return err
	}
	return mapMemoriesWaitRc(rc, "wait_for_seq")
}

// WaitForToken blocks until the fold has APPLIED `token.Seq`.
// Pins one OS thread for the duration. Use `WaitForTokenContext`
// for cancellable callers.
func (a *MemoriesAdapter) WaitForToken(token WriteToken, timeout time.Duration) error {
	ms := durationToMs(timeout)
	rc, err := a.withHandle(func(h *C.MemoriesAdapterHandle) C.int {
		return C.net_memories_wait_for_token(
			h, C.uint64_t(token.OriginHash), C.uint64_t(token.Seq), C.uint32_t(ms),
		)
	})
	if err != nil {
		return err
	}
	return mapMemoriesWaitRc(rc, "wait_for_token")
}

// PollForToken is a single non-blocking RYW poll. Mirrors
// TasksAdapter.PollForToken.
func (a *MemoriesAdapter) PollForToken(token WriteToken) error {
	rc, err := a.withHandle(func(h *C.MemoriesAdapterHandle) C.int {
		return C.net_memories_wait_for_token(
			h, C.uint64_t(token.OriginHash), C.uint64_t(token.Seq), C.uint32_t(0),
		)
	})
	if err != nil {
		return err
	}
	return mapMemoriesWaitRc(rc, "poll_for_token")
}

// WaitForTokenContext polls until the write is observable or the
// context cancels. Recommended path for cancellable Go callers.
//
// See `TasksAdapter.WaitForTokenContext` for the cancellation
// contract caveat (cgo polls return promptly because each is
// non-blocking; the cancellation gap is sub-millisecond).
func (a *MemoriesAdapter) WaitForTokenContext(ctx context.Context, token WriteToken) error {
	pollInterval := 10 * time.Millisecond
	timer := time.NewTimer(pollInterval)
	defer timer.Stop()
	for {
		err := a.PollForToken(token)
		switch {
		case err == nil:
			return nil
		case errors.Is(err, ErrMemoriesTimeout):
			// Not landed yet — fall through to sleep + retry.
		default:
			return err
		}
		timer.Reset(pollInterval)
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-timer.C:
		}
	}
}

// =====================================================================
// MemoriesWatch
// =====================================================================

// MemoriesWatch is a cursor over the filtered Memories delta
// stream. Mirrors TasksWatch.
type MemoriesWatch struct {
	mu     sync.Mutex
	cursor *C.MemoriesWatchHandle
}

// SnapshotAndWatch returns an atomic snapshot + a delta cursor.
func (a *MemoriesAdapter) SnapshotAndWatch(filter *MemoriesFilter) ([]Memory, *MemoriesWatch, error) {
	cFilter, free, err := encodeFilterJSON(filter)
	if err != nil {
		return nil, nil, err
	}
	defer free()
	var outSnap *C.char
	var outLen C.size_t
	var cursor *C.MemoriesWatchHandle
	rc, err := a.withHandle(func(h *C.MemoriesAdapterHandle) C.int {
		return C.net_memories_snapshot_and_watch(h, cFilter, &outSnap, &outLen, &cursor)
	})
	if err != nil {
		return nil, nil, err
	}
	if rc != 0 {
		return nil, nil, fmt.Errorf("%w: snapshot_and_watch rc=%d", ErrMemories, int(rc))
	}
	defer C.net_free_string(outSnap)
	buf := C.GoBytes(unsafe.Pointer(outSnap), C.int(outLen))
	var mems []Memory
	if err := json.Unmarshal(buf, &mems); err != nil {
		C.net_memories_watch_free(cursor)
		return nil, nil, fmt.Errorf("%w: snapshot decode: %v", ErrMemories, err)
	}
	watch := &MemoriesWatch{cursor: cursor}
	runtime.SetFinalizer(watch, func(w *MemoriesWatch) { _ = w.Close() })
	return mems, watch, nil
}

// Next pulls the next delta batch.
func (w *MemoriesWatch) Next(timeout time.Duration) ([]Memory, error) {
	w.mu.Lock()
	defer w.mu.Unlock()
	if w.cursor == nil {
		return nil, fmt.Errorf("%w: watch closed", ErrMemories)
	}
	var outJSON *C.char
	var outLen C.size_t
	rc := C.net_memories_watch_next(w.cursor, C.uint32_t(durationToMs(timeout)), &outJSON, &outLen)
	switch rc {
	case 0:
		defer C.net_free_string(outJSON)
		buf := C.GoBytes(unsafe.Pointer(outJSON), C.int(outLen))
		var batch []Memory
		if err := json.Unmarshal(buf, &batch); err != nil {
			return nil, fmt.Errorf("%w: watch_next decode: %v", ErrMemories, err)
		}
		return batch, nil
	case 1:
		return nil, ErrMemoriesTimeout
	case 2:
		return nil, fmt.Errorf("%w: watch stream ended", ErrMemories)
	default:
		return nil, fmt.Errorf("%w: watch_next rc=%d", ErrMemories, int(rc))
	}
}

// NextContext is the cancellable variant of `Next`.
func (w *MemoriesWatch) NextContext(ctx context.Context) ([]Memory, error) {
	pollInterval := 50 * time.Millisecond
	for {
		batch, err := w.Next(pollInterval)
		if err == nil {
			return batch, nil
		}
		if !errors.Is(err, ErrMemoriesTimeout) {
			return nil, err
		}
		select {
		case <-ctx.Done():
			return nil, ctx.Err()
		default:
		}
	}
}

// Close releases the cursor. Idempotent.
func (w *MemoriesWatch) Close() error {
	w.mu.Lock()
	defer w.mu.Unlock()
	if w.cursor == nil {
		return nil
	}
	C.net_memories_watch_free(w.cursor)
	w.cursor = nil
	runtime.SetFinalizer(w, nil)
	return nil
}

// mapMemoriesWaitRc translates wait-family FFI return codes.
// Mirrors mapWaitRc but routes through the Memories error
// sentinels so `errors.Is(err, ErrMemoriesTimeout)` works for
// callers using the Memories adapter.
func mapMemoriesWaitRc(rc C.int, label string) error {
	switch rc {
	case 0:
		return nil
	case 1:
		return ErrMemoriesTimeout
	case -104:
		return ErrMemoriesWrongOrigin
	case -105:
		return ErrMemoriesQueueFull
	case -106:
		return ErrMemoriesFoldStopped
	case -108:
		return ErrMemoriesPanic
	default:
		return fmt.Errorf("%w: %s rc=%d", ErrMemories, label, int(rc))
	}
}
