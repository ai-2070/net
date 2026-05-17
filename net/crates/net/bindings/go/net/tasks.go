// Tasks adapter — Go wrapper over the C ABI exported by
// `net::ffi::cortex` (the Tasks half).
//
// # Surface scope
//
// Mirrors the existing TasksAdapter shape in Python (`PyTasksAdapter`)
// and Node (`TasksAdapter`):
//
//   - Open(redex, originHash, persistent) -> *TasksAdapter
//   - Close()
//   - Create(id, title, nowNs) -> seq
//   - Rename(id, newTitle, nowNs) -> seq
//   - Complete(id, nowNs) -> seq
//   - Delete(id) -> seq
//   - List(filter) -> []Task
//   - SnapshotAndWatch(filter) -> ([]Task, *TasksWatch)
//   - WaitForSeq(seq, timeout) error
//   - WaitForToken(token, timeout) error           // single blocking wait
//   - PollForToken(token) error                    // non-blocking single poll
//   - WaitForTokenContext(ctx, token) error        // context-aware loop
//
// # Non-blocking RYW semantics
//
// `WaitForToken` calls into cgo with the supplied timeout; the OS
// thread is parked for the duration. Go's scheduler spins up other M's
// so this doesn't starve other goroutines, but the thread cost is real.
//
// `PollForToken` uses the substrate's `timeout_ms == 0` non-blocking
// shape — checks the applied watermark + origin binding and returns
// immediately. Returns `ErrTasksTimeout` when the write hasn't landed
// yet, `nil` when it has.
//
// `WaitForTokenContext` is the recommended path for cancellable Go
// callers: loops poll-then-sleep until either the wait succeeds, the
// context cancels, or a substrate-level error surfaces. No OS thread
// is parked for more than ~10ms.

package net

/*
#include <stdint.h>
#include <stdlib.h>

typedef struct RedexHandle RedexHandle;
typedef struct TasksAdapterHandle TasksAdapterHandle;
typedef struct TasksWatchHandle TasksWatchHandle;

extern int net_tasks_adapter_open(
    RedexHandle* redex,
    uint64_t origin_hash,
    int persistent,
    TasksAdapterHandle** out_handle
);
extern int net_tasks_adapter_close(TasksAdapterHandle* handle);
extern void net_tasks_adapter_free(TasksAdapterHandle* handle);

extern int net_tasks_create(
    TasksAdapterHandle* handle,
    uint64_t id,
    const char* title,
    uint64_t now_ns,
    uint64_t* out_seq
);
extern int net_tasks_rename(
    TasksAdapterHandle* handle,
    uint64_t id,
    const char* new_title,
    uint64_t now_ns,
    uint64_t* out_seq
);
extern int net_tasks_complete(
    TasksAdapterHandle* handle,
    uint64_t id,
    uint64_t now_ns,
    uint64_t* out_seq
);
extern int net_tasks_delete(
    TasksAdapterHandle* handle,
    uint64_t id,
    uint64_t* out_seq
);
extern int net_tasks_wait_for_seq(
    TasksAdapterHandle* handle,
    uint64_t seq,
    uint32_t timeout_ms
);
extern int net_tasks_wait_for_token(
    TasksAdapterHandle* handle,
    uint64_t origin_hash,
    uint64_t seq,
    uint32_t timeout_ms
);
extern int net_tasks_list(
    TasksAdapterHandle* handle,
    const char* filter_json,
    char** out_json,
    size_t* out_len
);
extern int net_tasks_snapshot_and_watch(
    TasksAdapterHandle* handle,
    const char* filter_json,
    char** out_snapshot,
    size_t* out_snapshot_len,
    TasksWatchHandle** out_cursor
);
extern int net_tasks_watch_next(
    TasksWatchHandle* cursor,
    uint32_t timeout_ms,
    char** out_json,
    size_t* out_len
);
extern void net_tasks_watch_free(TasksWatchHandle* cursor);

extern void net_free_string(char* s);
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
// Errors
// =====================================================================

// ErrTasks is the umbrella error for any Tasks-adapter failure.
var ErrTasks = errors.New("tasks")

// ErrTasksTimeout indicates a `WaitFor*` or `PollForToken` did not
// observe the target seq before the deadline.
var ErrTasksTimeout = fmt.Errorf("%w: wait timeout", ErrTasks)

// ErrTasksWrongOrigin indicates the supplied token's origin_hash
// does not match the adapter's bound origin.
var ErrTasksWrongOrigin = fmt.Errorf("%w: wrong token origin", ErrTasks)

// ErrTasksQueueFull indicates the per-adapter or process-wide RYW
// in-flight cap is saturated. Caller should shed load and retry.
var ErrTasksQueueFull = fmt.Errorf("%w: wait-queue saturated", ErrTasks)

// ErrTasksFoldStopped indicates the adapter's fold task stopped
// before reaching the wait target. The adapter is no longer making
// progress; callers should not retry.
var ErrTasksFoldStopped = fmt.Errorf("%w: fold task stopped", ErrTasks)

// ErrTasksPanic indicates a panic surfaced from the substrate
// across the FFI boundary (caught by `catch_unwind`).
var ErrTasksPanic = fmt.Errorf("%w: substrate panic", ErrTasks)

// =====================================================================
// Types
// =====================================================================

// TaskStatus mirrors the substrate's enum.
type TaskStatus string

const (
	TaskStatusPending   TaskStatus = "pending"
	TaskStatusCompleted TaskStatus = "completed"
)

// Task is the Go-facing record. Matches the JSON shape the FFI
// emits from `TaskJson`.
type Task struct {
	ID        uint64     `json:"id"`
	Title     string     `json:"title"`
	Status    TaskStatus `json:"status"`
	CreatedNs uint64     `json:"created_ns"`
	UpdatedNs uint64     `json:"updated_ns"`
}

// TasksOrderBy is the wire-string for sort orders. Match the
// substrate's `TasksOrderBy` enum lowercase.
type TasksOrderBy string

const (
	TasksOrderByCreatedAsc  TasksOrderBy = "created_asc"
	TasksOrderByCreatedDesc TasksOrderBy = "created_desc"
	TasksOrderByUpdatedAsc  TasksOrderBy = "updated_asc"
	TasksOrderByUpdatedDesc TasksOrderBy = "updated_desc"
	TasksOrderByTitleAsc    TasksOrderBy = "title_asc"
)

// TasksFilter is the Go-side filter passed to List /
// SnapshotAndWatch. Marshaled to JSON via standard tags.
type TasksFilter struct {
	Status          *TaskStatus   `json:"status,omitempty"`
	TitleContains   *string       `json:"title_contains,omitempty"`
	CreatedAfterNs  *uint64       `json:"created_after_ns,omitempty"`
	CreatedBeforeNs *uint64       `json:"created_before_ns,omitempty"`
	UpdatedAfterNs  *uint64       `json:"updated_after_ns,omitempty"`
	UpdatedBeforeNs *uint64       `json:"updated_before_ns,omitempty"`
	OrderBy         *TasksOrderBy `json:"order_by,omitempty"`
	Limit           *uint32       `json:"limit,omitempty"`
}

// WriteToken addresses a single write on a specific origin's chain.
// Returned by `Create` / `Rename` / `Complete` / `Delete` via the
// returned seq + the adapter's origin_hash. Round-trips through
// `WaitForToken` / `PollForToken`.
type WriteToken struct {
	OriginHash uint64
	Seq        uint64
}

// =====================================================================
// TasksAdapter
// =====================================================================

// TasksAdapter wraps the C `*TasksAdapterHandle`. Calls take a Go
// RWMutex around `Close()` to serialize the FFI `_free` against
// concurrent in-flight ops; the substrate's HandleGuard handles
// the cross-thread quiesce.
type TasksAdapter struct {
	mu         sync.RWMutex
	handle     *C.TasksAdapterHandle
	originHash uint64
}

// OpenTasksAdapter opens a Tasks adapter against the supplied Redex.
// `persistent = true` routes writes through the Redex's persistent
// directory (the Redex must have been created with
// `NewRedexWithPersistentDir`).
//
// Same lifecycle pattern as the rest of the Go binding: runtime
// finalizer installed; callers should pair every Open with a
// `defer adapter.Close()` for deterministic cleanup.
func OpenTasksAdapter(redex *Redex, originHash uint64, persistent bool) (*TasksAdapter, error) {
	if redex == nil {
		return nil, fmt.Errorf("%w: nil redex", ErrTasks)
	}
	rh := redex.Handle()
	if rh == nil {
		return nil, fmt.Errorf("%w: redex closed", ErrTasks)
	}
	var handle *C.TasksAdapterHandle
	var pflag C.int
	if persistent {
		pflag = 1
	}
	rc := C.net_tasks_adapter_open(
		(*C.RedexHandle)(rh),
		C.uint64_t(originHash),
		pflag,
		&handle,
	)
	if rc != 0 {
		return nil, fmt.Errorf("%w: open failed (rc=%d)", ErrTasks, int(rc))
	}
	a := &TasksAdapter{handle: handle, originHash: originHash}
	runtime.SetFinalizer(a, func(a *TasksAdapter) { _ = a.Close() })
	return a, nil
}

// newTasksAdapterFromRaw wraps a raw `*TasksAdapterHandle` (obtained from
// another cgo file in the same package, e.g. `net_netdb_tasks`) into a
// `*TasksAdapter` with the same finalizer + close semantics as
// `OpenTasksAdapter`. The handle must already be Arc-cloned by the
// caller — freeing the returned adapter calls `net_tasks_adapter_free`
// exactly once on the supplied pointer.
//
// Used by NetDb.Tasks() to bridge across the per-file cgo type wall
// (each cgo file has its own `C.TasksAdapterHandle` Go type, so handles
// must transit through `unsafe.Pointer`).
func newTasksAdapterFromRaw(handle unsafe.Pointer, originHash uint64) *TasksAdapter {
	a := &TasksAdapter{handle: (*C.TasksAdapterHandle)(handle), originHash: originHash}
	runtime.SetFinalizer(a, func(a *TasksAdapter) { _ = a.Close() })
	return a
}

// OriginHash returns the origin_hash the adapter is bound to.
// Tokens issued by `Create` / `Rename` / `Complete` / `Delete`
// carry this value as their `OriginHash`.
func (a *TasksAdapter) OriginHash() uint64 {
	return a.originHash
}

// Close releases the underlying handle. Idempotent. Quiesces
// in-flight ops via the substrate's HandleGuard.
func (a *TasksAdapter) Close() error {
	a.mu.Lock()
	defer a.mu.Unlock()
	if a.handle == nil {
		return nil
	}
	rc := C.net_tasks_adapter_close(a.handle)
	C.net_tasks_adapter_free(a.handle)
	a.handle = nil
	runtime.SetFinalizer(a, nil)
	if rc != 0 && rc != -100 { // NET_ERR_CORTEX_CLOSED on idempotent close
		return fmt.Errorf("%w: close rc=%d", ErrTasks, int(rc))
	}
	return nil
}

func (a *TasksAdapter) withHandle(f func(*C.TasksAdapterHandle) C.int) (C.int, error) {
	a.mu.RLock()
	defer a.mu.RUnlock()
	if a.handle == nil {
		return 0, fmt.Errorf("%w: adapter closed", ErrTasks)
	}
	return f(a.handle), nil
}

// Create issues a TASK_CREATED event. Returns the WriteToken
// addressing the write.
func (a *TasksAdapter) Create(id uint64, title string, nowNs uint64) (WriteToken, error) {
	cTitle := C.CString(title)
	defer C.free(unsafe.Pointer(cTitle))
	var seq C.uint64_t
	rc, err := a.withHandle(func(h *C.TasksAdapterHandle) C.int {
		return C.net_tasks_create(h, C.uint64_t(id), cTitle, C.uint64_t(nowNs), &seq)
	})
	if err != nil {
		return WriteToken{}, err
	}
	if rc != 0 {
		return WriteToken{}, fmt.Errorf("%w: create rc=%d", ErrTasks, int(rc))
	}
	return WriteToken{OriginHash: a.originHash, Seq: uint64(seq)}, nil
}

// Rename emits a TASK_RENAMED event.
func (a *TasksAdapter) Rename(id uint64, newTitle string, nowNs uint64) (WriteToken, error) {
	cTitle := C.CString(newTitle)
	defer C.free(unsafe.Pointer(cTitle))
	var seq C.uint64_t
	rc, err := a.withHandle(func(h *C.TasksAdapterHandle) C.int {
		return C.net_tasks_rename(h, C.uint64_t(id), cTitle, C.uint64_t(nowNs), &seq)
	})
	if err != nil {
		return WriteToken{}, err
	}
	if rc != 0 {
		return WriteToken{}, fmt.Errorf("%w: rename rc=%d", ErrTasks, int(rc))
	}
	return WriteToken{OriginHash: a.originHash, Seq: uint64(seq)}, nil
}

// Complete emits a TASK_COMPLETED event.
func (a *TasksAdapter) Complete(id uint64, nowNs uint64) (WriteToken, error) {
	var seq C.uint64_t
	rc, err := a.withHandle(func(h *C.TasksAdapterHandle) C.int {
		return C.net_tasks_complete(h, C.uint64_t(id), C.uint64_t(nowNs), &seq)
	})
	if err != nil {
		return WriteToken{}, err
	}
	if rc != 0 {
		return WriteToken{}, fmt.Errorf("%w: complete rc=%d", ErrTasks, int(rc))
	}
	return WriteToken{OriginHash: a.originHash, Seq: uint64(seq)}, nil
}

// Delete emits a TASK_DELETED event.
func (a *TasksAdapter) Delete(id uint64) (WriteToken, error) {
	var seq C.uint64_t
	rc, err := a.withHandle(func(h *C.TasksAdapterHandle) C.int {
		return C.net_tasks_delete(h, C.uint64_t(id), &seq)
	})
	if err != nil {
		return WriteToken{}, err
	}
	if rc != 0 {
		return WriteToken{}, fmt.Errorf("%w: delete rc=%d", ErrTasks, int(rc))
	}
	return WriteToken{OriginHash: a.originHash, Seq: uint64(seq)}, nil
}

// List returns every Task matching `filter`. Pass `nil` for no
// filter (returns every Task in deterministic order).
func (a *TasksAdapter) List(filter *TasksFilter) ([]Task, error) {
	cFilter, free, err := encodeFilterJSON(filter)
	if err != nil {
		return nil, err
	}
	defer free()
	var outJSON *C.char
	var outLen C.size_t
	rc, err := a.withHandle(func(h *C.TasksAdapterHandle) C.int {
		return C.net_tasks_list(h, cFilter, &outJSON, &outLen)
	})
	if err != nil {
		return nil, err
	}
	if rc != 0 {
		return nil, fmt.Errorf("%w: list rc=%d", ErrTasks, int(rc))
	}
	defer C.net_free_string(outJSON)
	buf := C.GoBytes(unsafe.Pointer(outJSON), C.int(outLen))
	var tasks []Task
	if err := json.Unmarshal(buf, &tasks); err != nil {
		return nil, fmt.Errorf("%w: list decode: %v", ErrTasks, err)
	}
	return tasks, nil
}

// WaitForSeq blocks until the adapter's fold has processed every
// event up through `seq`, or `timeout` elapses. `timeout == 0`
// blocks indefinitely.
//
// Pins one OS thread for the duration via cgo. For cancellable
// callers use `WaitForTokenContext` (built on the non-blocking
// poll variant).
func (a *TasksAdapter) WaitForSeq(seq uint64, timeout time.Duration) error {
	ms := durationToMs(timeout)
	rc, err := a.withHandle(func(h *C.TasksAdapterHandle) C.int {
		return C.net_tasks_wait_for_seq(h, C.uint64_t(seq), C.uint32_t(ms))
	})
	if err != nil {
		return err
	}
	return mapWaitRc(rc, "wait_for_seq")
}

// WaitForToken blocks until the fold has APPLIED every event up
// through `token.Seq`, or `timeout` elapses. The token's
// `OriginHash` must match the adapter's bound origin —
// `ErrTasksWrongOrigin` otherwise.
//
// Pins one OS thread for the duration via cgo. For cancellable
// callers use `WaitForTokenContext` instead.
func (a *TasksAdapter) WaitForToken(token WriteToken, timeout time.Duration) error {
	ms := durationToMs(timeout)
	rc, err := a.withHandle(func(h *C.TasksAdapterHandle) C.int {
		return C.net_tasks_wait_for_token(
			h, C.uint64_t(token.OriginHash), C.uint64_t(token.Seq), C.uint32_t(ms),
		)
	})
	if err != nil {
		return err
	}
	return mapWaitRc(rc, "wait_for_token")
}

// PollForToken is a single non-blocking RYW poll. Checks the
// adapter's applied watermark + origin binding and returns
// immediately. `nil` means the write is observable; `ErrTasksTimeout`
// means it isn't (yet).
//
// Use this in a loop with `time.Sleep` between calls when you
// need cancellable waits — or use `WaitForTokenContext` which
// wraps that pattern.
func (a *TasksAdapter) PollForToken(token WriteToken) error {
	rc, err := a.withHandle(func(h *C.TasksAdapterHandle) C.int {
		return C.net_tasks_wait_for_token(
			h, C.uint64_t(token.OriginHash), C.uint64_t(token.Seq), C.uint32_t(0),
		)
	})
	if err != nil {
		return err
	}
	return mapWaitRc(rc, "poll_for_token")
}

// WaitForTokenContext blocks until `token`'s write is observable
// OR `ctx` cancels. Internally polls via `PollForToken` with a
// short sleep between attempts so the underlying OS thread isn't
// parked for more than ~10 ms.
//
// Returns:
//   - `nil` on successful observation,
//   - `ctx.Err()` when the context cancels first,
//   - a typed Tasks error on substrate-level failure (wrong origin,
//     queue full, fold stopped, panic).
//
// Recommended path for cancellable Go callers — `WaitForToken`
// pins a thread for the supplied timeout.
//
// # Cancellation contract caveat
//
// `ctx.Done()` cancellation aborts the Go-side polling loop but
// does NOT abort any in-flight `PollForToken` cgo call. Each
// poll is non-blocking (returns immediately via the FFI's
// `timeout_ms == 0` shape), so the worst-case window between
// cancel and return is the duration of one poll — typically
// sub-millisecond. The Rust substrate side has no cancellation
// state to roll back; the next ingest/wait on the adapter is
// unaffected.
//
// If you switch to `WaitForToken(token, timeout)` (single
// blocking cgo call), `ctx.Done()` cannot interrupt the cgo
// blocking phase — the call will return after `timeout` regardless
// of context state. Use `WaitForTokenContext` for cancellable
// waits.
func (a *TasksAdapter) WaitForTokenContext(ctx context.Context, token WriteToken) error {
	pollInterval := 10 * time.Millisecond
	timer := time.NewTimer(pollInterval)
	defer timer.Stop()
	for {
		err := a.PollForToken(token)
		switch {
		case err == nil:
			return nil
		case errors.Is(err, ErrTasksTimeout):
			// Not landed yet — fall through to sleep + retry.
		default:
			// Wrong origin / queue full / fold stopped / panic —
			// terminal, no retry.
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
// TasksWatch — snapshot + delta stream
// =====================================================================

// TasksWatch is a cursor over the filtered Tasks delta stream.
// Returned by `SnapshotAndWatch`; `Next` yields each delta batch;
// `Close` releases the cursor (idempotent; the finalizer is a
// safety net only — prefer explicit Close).
type TasksWatch struct {
	mu     sync.Mutex
	cursor *C.TasksWatchHandle
}

// SnapshotAndWatch returns an atomic snapshot of Tasks matching
// `filter` PLUS a cursor that emits subsequent change batches.
// The snapshot is the initial state; deltas flow on `Next`.
func (a *TasksAdapter) SnapshotAndWatch(filter *TasksFilter) ([]Task, *TasksWatch, error) {
	cFilter, free, err := encodeFilterJSON(filter)
	if err != nil {
		return nil, nil, err
	}
	defer free()
	var outSnap *C.char
	var outLen C.size_t
	var cursor *C.TasksWatchHandle
	rc, err := a.withHandle(func(h *C.TasksAdapterHandle) C.int {
		return C.net_tasks_snapshot_and_watch(h, cFilter, &outSnap, &outLen, &cursor)
	})
	if err != nil {
		return nil, nil, err
	}
	if rc != 0 {
		return nil, nil, fmt.Errorf("%w: snapshot_and_watch rc=%d", ErrTasks, int(rc))
	}
	defer C.net_free_string(outSnap)
	buf := C.GoBytes(unsafe.Pointer(outSnap), C.int(outLen))
	var tasks []Task
	if err := json.Unmarshal(buf, &tasks); err != nil {
		C.net_tasks_watch_free(cursor)
		return nil, nil, fmt.Errorf("%w: snapshot decode: %v", ErrTasks, err)
	}
	watch := &TasksWatch{cursor: cursor}
	runtime.SetFinalizer(watch, func(w *TasksWatch) { _ = w.Close() })
	return tasks, watch, nil
}

// Next pulls the next change batch from the watch cursor.
// `timeout == 0` blocks indefinitely. Returns `(batch, nil)` on
// event, `(nil, ErrTasksTimeout)` on timeout, `(nil, io.EOF`-style
// stream-ended sentinel) when the stream closes.
func (w *TasksWatch) Next(timeout time.Duration) ([]Task, error) {
	w.mu.Lock()
	defer w.mu.Unlock()
	if w.cursor == nil {
		return nil, fmt.Errorf("%w: watch closed", ErrTasks)
	}
	var outJSON *C.char
	var outLen C.size_t
	rc := C.net_tasks_watch_next(w.cursor, C.uint32_t(durationToMs(timeout)), &outJSON, &outLen)
	switch rc {
	case 0:
		defer C.net_free_string(outJSON)
		buf := C.GoBytes(unsafe.Pointer(outJSON), C.int(outLen))
		var batch []Task
		if err := json.Unmarshal(buf, &batch); err != nil {
			return nil, fmt.Errorf("%w: watch_next decode: %v", ErrTasks, err)
		}
		return batch, nil
	case 1: // NET_ERR_TIMEOUT
		return nil, ErrTasksTimeout
	case 2: // NET_ERR_STREAM_ENDED
		return nil, fmt.Errorf("%w: watch stream ended", ErrTasks)
	default:
		return nil, fmt.Errorf("%w: watch_next rc=%d", ErrTasks, int(rc))
	}
}

// NextContext is the cancellable variant of `Next`. Polls with a
// short FFI timeout in a loop; checks `ctx.Done()` between polls
// so a long-cancelled wait doesn't pin a thread.
func (w *TasksWatch) NextContext(ctx context.Context) ([]Task, error) {
	pollInterval := 50 * time.Millisecond
	for {
		batch, err := w.Next(pollInterval)
		if err == nil {
			return batch, nil
		}
		if !errors.Is(err, ErrTasksTimeout) {
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
func (w *TasksWatch) Close() error {
	w.mu.Lock()
	defer w.mu.Unlock()
	if w.cursor == nil {
		return nil
	}
	C.net_tasks_watch_free(w.cursor)
	w.cursor = nil
	runtime.SetFinalizer(w, nil)
	return nil
}

// =====================================================================
// Internal helpers
// =====================================================================

// durationToMs converts a Go Duration to the FFI's u32 ms.
// A zero Duration means "block forever" (the FFI's `timeout_ms == 0`
// shape for `wait_for_seq` / `watch_next`); for `wait_for_token`
// it means "poll, don't wait" — callers route through `PollForToken`
// directly for that semantic so this helper consistently treats
// zero as "no timeout" across all sites.
//
// Durations beyond u32::MAX milliseconds (~49 days) clamp to the
// maximum; substrate-side timeouts above ~49 days are silly.
func durationToMs(d time.Duration) uint32 {
	if d <= 0 {
		return 0
	}
	ms := d / time.Millisecond
	if ms > 0xFFFFFFFF {
		return 0xFFFFFFFF
	}
	return uint32(ms)
}

// mapWaitRc translates the FFI's `c_int` return codes from the
// wait family (`wait_for_seq`, `wait_for_token`, polling) to typed
// Go errors. `rc == 0` is success → `nil`.
func mapWaitRc(rc C.int, label string) error {
	switch rc {
	case 0:
		return nil
	case 1: // NET_ERR_TIMEOUT
		return ErrTasksTimeout
	case -104: // NET_ERR_WRONG_ORIGIN
		return ErrTasksWrongOrigin
	case -105: // NET_ERR_QUEUE_FULL
		return ErrTasksQueueFull
	case -106: // NET_ERR_FOLD_STOPPED
		return ErrTasksFoldStopped
	case -108: // NET_ERR_PANIC
		return ErrTasksPanic
	default:
		return fmt.Errorf("%w: %s rc=%d", ErrTasks, label, int(rc))
	}
}

// encodeFilterJSON renders a filter (or `nil`) into a CString
// suitable for the FFI's `filter_json` parameter. Returns the
// allocated CString and a `free` function the caller must defer.
// `nil` filter → NULL CString (the FFI treats NULL as "no filter").
func encodeFilterJSON(filter interface{}) (*C.char, func(), error) {
	// `interface{}` for Tasks/Memories filter parity — the JSON
	// encoder marshals each shape against its own tags. A typed
	// generic helper would force a trait-like bound that doesn't
	// exist in Go; pass-as-empty-interface is the idiomatic shape.
	if filter == nil {
		return nil, func() {}, nil
	}
	// Reflectively check for nil-pointer interface — Go's
	// `*TasksFilter` zero-value masquerades as `!= nil` here.
	switch v := filter.(type) {
	case *TasksFilter:
		if v == nil {
			return nil, func() {}, nil
		}
	case *MemoriesFilter:
		if v == nil {
			return nil, func() {}, nil
		}
	}
	buf, err := json.Marshal(filter)
	if err != nil {
		return nil, func() {}, fmt.Errorf("%w: filter encode: %v", ErrTasks, err)
	}
	c := C.CString(string(buf))
	return c, func() { C.free(unsafe.Pointer(c)) }, nil
}
