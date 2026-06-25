// Package net — CortEX (tasks + memories) and RedEX file bindings.
//
// These surfaces are compiled into the Rust cdylib when the core is
// built with `--features "netdb redex-disk"` (see the README).
//
// The Go API is idiomatic:
//
//   - Opaque handles are Go structs wrapping a C pointer, with
//     finalizers installed so a dropped handle releases the native
//     allocation even if the caller forgets `Close` / `Free`.
//
//   - Watch / tail iterators return `<-chan` plus a `context.Context`.
//     The goroutine pumps the cursor until the context is cancelled,
//     the channel is drained by the consumer, or the stream ends.
//
//   - `SnapshotAndWatch` returns both the initial result AND a
//     subscription channel atomically, preserving the v2 race fix
//     (see docs/STORAGE_AND_CORTEX.md).
//
// See `example/cortex/main.go` for an end-to-end walkthrough.

package net

/*
#include "net.h"
#include <stdlib.h>
#include <string.h>
*/
import "C"

import (
	"context"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"runtime"
	"sync"
	"time"
	"unsafe"
)

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

var (
	ErrCortexClosed  = errors.New("cortex adapter closed")
	ErrCortexFold    = errors.New("cortex fold / ingestion failed")
	ErrNetDb         = errors.New("netdb error")
	ErrRedex         = errors.New("redex error")
	ErrStreamEnded   = errors.New("stream ended")
	ErrStreamTimeout = errors.New("stream timed out")
)

// durationToMillisU32 clamps a Go Duration down to the millisecond
// range the C ABI expects (`uint32`). Negative durations are treated
// as zero (=wait indefinitely on the FFI side); durations that would
// overflow u32 are clamped at `math.MaxUint32` (~49.7 days) rather
// than silently wrapping — which would flip a 2-hour timeout into a
// ~50ms poll.
func durationToMillisU32(d time.Duration) C.uint32_t {
	if d <= 0 {
		return 0
	}
	ms := d.Milliseconds()
	if ms > int64(^uint32(0)) {
		return C.uint32_t(^uint32(0))
	}
	return C.uint32_t(ms)
}

func cortexErrorFromCode(code C.int) error {
	switch code {
	case 0:
		return nil
	case -1:
		return ErrNullPointer
	case -2:
		return ErrInvalidUTF8
	case -3:
		return ErrInvalidJSON
	case -100:
		return ErrCortexClosed
	case -101:
		return ErrCortexFold
	case -102:
		return ErrNetDb
	case -103:
		return ErrRedex
	case 1:
		return ErrStreamTimeout
	case 2:
		return ErrStreamEnded
	default:
		return fmt.Errorf("cortex unknown error (code %d)", code)
	}
}

// ---------------------------------------------------------------------------
// Redex manager
// ---------------------------------------------------------------------------

// Redex is a local RedEX manager. One handle per process; shared by
// all adapters on the same storage tree.
type Redex struct {
	mu     sync.RWMutex
	handle *C.net_redex_t
}

// NewRedex creates a Redex manager. Pass an empty `persistentDir` for
// heap-only. With a directory, adapters opened with `persistent=true`
// write to `<dir>/<channel>/{idx,dat}` and replay on reopen.
func NewRedex(persistentDir string) *Redex {
	var cDir *C.char
	if persistentDir != "" {
		cDir = C.CString(persistentDir)
		defer C.free(unsafe.Pointer(cDir))
	}
	handle := C.net_redex_new(cDir)
	r := &Redex{handle: handle}
	runtime.SetFinalizer(r, (*Redex).Free)
	return r
}

// Free releases the Redex and every open file handle bound to it.
// Idempotent.
func (r *Redex) Free() {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.handle != nil {
		C.net_redex_free(r.handle)
		r.handle = nil
		runtime.SetFinalizer(r, nil)
	}
}

// ---------------------------------------------------------------------------
// RedexFile (raw log)
// ---------------------------------------------------------------------------

// RedexFileConfig maps to the Rust `RedexFileConfig`. `FsyncEveryN`
// and `FsyncIntervalMs` are mutually exclusive.
type RedexFileConfig struct {
	Persistent         bool   `json:"persistent,omitempty"`
	FsyncEveryN        uint64 `json:"fsync_every_n,omitempty"`
	FsyncIntervalMs    uint64 `json:"fsync_interval_ms,omitempty"`
	RetentionMaxEvents uint64 `json:"retention_max_events,omitempty"`
	RetentionMaxBytes  uint64 `json:"retention_max_bytes,omitempty"`
	RetentionMaxAgeMs  uint64 `json:"retention_max_age_ms,omitempty"`
}

// RedexEvent is one materialized event yielded by a tail / range read.
type RedexEvent struct {
	Seq      uint64 `json:"seq"`
	Payload  []byte `json:"-"`
	Checksum uint32 `json:"checksum"`
	IsInline bool   `json:"is_inline"`
}

// Intermediate for JSON (hex payload).
type redexEventWire struct {
	Seq        uint64 `json:"seq"`
	PayloadHex string `json:"payload_hex"`
	Checksum   uint32 `json:"checksum"`
	IsInline   bool   `json:"is_inline"`
}

func (e *redexEventWire) toEvent() (RedexEvent, error) {
	payload, err := hex.DecodeString(e.PayloadHex)
	if err != nil {
		return RedexEvent{}, fmt.Errorf("invalid payload hex: %w", err)
	}
	return RedexEvent{
		Seq:      e.Seq,
		Payload:  payload,
		Checksum: e.Checksum,
		IsInline: e.IsInline,
	}, nil
}

// RedexFile is a raw append-only log bound to a channel name.
type RedexFile struct {
	mu     sync.RWMutex
	handle *C.net_redex_file_t
}

// OpenFile opens (or gets) a RedEX file on `redex`. `config` may be
// nil for defaults.
func (r *Redex) OpenFile(name string, config *RedexFileConfig) (*RedexFile, error) {
	cName := C.CString(name)
	defer C.free(unsafe.Pointer(cName))

	var cCfg *C.char
	if config != nil {
		data, err := json.Marshal(config)
		if err != nil {
			return nil, fmt.Errorf("marshal config: %w", err)
		}
		cCfg = C.CString(string(data))
		defer C.free(unsafe.Pointer(cCfg))
	}

	// Hold the Redex read-lock through the C call so a concurrent
	// Free() can't race the native pointer into a use-after-free.
	r.mu.RLock()
	defer r.mu.RUnlock()
	if r.handle == nil {
		return nil, ErrShuttingDown
	}
	var out *C.net_redex_file_t
	code := C.net_redex_open_file(r.handle, cName, cCfg, &out)
	if err := cortexErrorFromCode(code); err != nil {
		return nil, err
	}
	f := &RedexFile{handle: out}
	runtime.SetFinalizer(f, (*RedexFile).free)
	return f, nil
}

func (f *RedexFile) free() {
	f.mu.Lock()
	defer f.mu.Unlock()
	if f.handle != nil {
		C.net_redex_file_free(f.handle)
		f.handle = nil
		runtime.SetFinalizer(f, nil)
	}
}

// Close flushes and closes the file. Subsequent operations error with
// ErrRedex. Idempotent.
func (f *RedexFile) Close() error {
	f.mu.Lock()
	defer f.mu.Unlock()
	if f.handle == nil {
		return nil
	}
	code := C.net_redex_file_close(f.handle)
	C.net_redex_file_free(f.handle)
	f.handle = nil
	runtime.SetFinalizer(f, nil)
	return cortexErrorFromCode(code)
}

// Append appends one payload; returns the assigned sequence number.
func (f *RedexFile) Append(payload []byte) (uint64, error) {
	f.mu.RLock()
	defer f.mu.RUnlock()
	if f.handle == nil {
		return 0, ErrShuttingDown
	}
	var seq C.uint64_t
	var ptr *C.uint8_t
	var ln C.size_t
	if len(payload) > 0 {
		ptr = (*C.uint8_t)(unsafe.Pointer(&payload[0]))
		ln = C.size_t(len(payload))
	}
	code := C.net_redex_file_append(f.handle, ptr, ln, &seq)
	if err := cortexErrorFromCode(code); err != nil {
		return 0, err
	}
	return uint64(seq), nil
}

// Len returns the number of retained events.
func (f *RedexFile) Len() uint64 {
	f.mu.RLock()
	defer f.mu.RUnlock()
	if f.handle == nil {
		return 0
	}
	return uint64(C.net_redex_file_len(f.handle))
}

// ReadRange reads events in [start, end).
func (f *RedexFile) ReadRange(start, end uint64) ([]RedexEvent, error) {
	f.mu.RLock()
	defer f.mu.RUnlock()
	if f.handle == nil {
		return nil, ErrShuttingDown
	}
	var out *C.char
	var outLen C.size_t
	code := C.net_redex_file_read_range(f.handle, C.uint64_t(start), C.uint64_t(end), &out, &outLen)
	if err := cortexErrorFromCode(code); err != nil {
		return nil, err
	}
	defer C.net_free_string(out)
	// `C.GoBytes` copies the C buffer once into a Go-managed byte
	// slice. Previous form (`C.GoStringN` + `[]byte(js)`) copied
	// the payload twice — once into a Go string, then again into a
	// fresh byte slice for `json.Unmarshal`.
	payload := C.GoBytes(unsafe.Pointer(out), C.int(outLen))
	var wire []redexEventWire
	if err := json.Unmarshal(payload, &wire); err != nil {
		return nil, fmt.Errorf("decode read_range: %w", err)
	}
	events := make([]RedexEvent, 0, len(wire))
	for i := range wire {
		ev, err := wire[i].toEvent()
		if err != nil {
			return nil, err
		}
		events = append(events, ev)
	}
	return events, nil
}

// Sync fsyncs the disk segment. No-op on heap-only files.
func (f *RedexFile) Sync() error {
	f.mu.RLock()
	defer f.mu.RUnlock()
	if f.handle == nil {
		return ErrShuttingDown
	}
	return cortexErrorFromCode(C.net_redex_file_sync(f.handle))
}

// Tail returns a channel of RedexEvents starting from `fromSeq`.
// Backfills the retained range atomically, then streams live appends.
// Cancel `ctx` to stop; the channel is closed when the cursor ends.
//
// Hold the read-lock for the duration of the cursor-creation C call
// only. Once the cursor is built, it's self-contained Rust memory
// that survives independent of the file handle — closing the file
// just drives the cursor's tail stream to `RedexError::Closed`, which
// the goroutine maps to a clean channel close.
func (f *RedexFile) Tail(ctx context.Context, fromSeq uint64) (<-chan RedexEvent, <-chan error, error) {
	f.mu.RLock()
	if f.handle == nil {
		f.mu.RUnlock()
		return nil, nil, ErrShuttingDown
	}
	var cursor *C.net_redex_tail_t
	code := C.net_redex_file_tail(f.handle, C.uint64_t(fromSeq), &cursor)
	f.mu.RUnlock()
	if err := cortexErrorFromCode(code); err != nil {
		return nil, nil, err
	}

	events := make(chan RedexEvent, 16)
	errs := make(chan error, 1)
	go func() {
		defer func() {
			C.net_redex_tail_free(cursor)
			close(events)
			close(errs)
		}()
		for {
			if ctx.Err() != nil {
				return
			}
			var out *C.char
			var outLen C.size_t
			// 50ms poll so ctx.Done() is observed even when the
			// underlying stream is idle.
			code := C.net_redex_tail_next(cursor, 50, &out, &outLen)
			switch code {
			case 0:
				payload := C.GoBytes(unsafe.Pointer(out), C.int(outLen))
				C.net_free_string(out)
				var wire redexEventWire
				if err := json.Unmarshal(payload, &wire); err != nil {
					errs <- fmt.Errorf("decode tail event: %w", err)
					return
				}
				ev, err := wire.toEvent()
				if err != nil {
					errs <- err
					return
				}
				select {
				case events <- ev:
				case <-ctx.Done():
					return
				}
			case C.NET_STREAM_TIMEOUT:
				// Keep polling.
			case C.NET_STREAM_ENDED:
				return
			default:
				errs <- cortexErrorFromCode(code)
				return
			}
		}
	}()
	return events, errs, nil
}

// ---------------------------------------------------------------------------
// Tasks adapter
// ---------------------------------------------------------------------------

// Task is a materialized task record.
type Task struct {
	ID        uint64 `json:"id"`
	Title     string `json:"title"`
	Status    string `json:"status"` // "pending" | "completed"
	CreatedNs uint64 `json:"created_ns"`
	UpdatedNs uint64 `json:"updated_ns"`
}

// TasksFilter mirrors the Rust `TasksFilter` via JSON. Unset fields
// are ignored.
type TasksFilter struct {
	Status          string `json:"status,omitempty"` // "pending" | "completed"
	TitleContains   string `json:"title_contains,omitempty"`
	CreatedAfterNs  uint64 `json:"created_after_ns,omitempty"`
	CreatedBeforeNs uint64 `json:"created_before_ns,omitempty"`
	UpdatedAfterNs  uint64 `json:"updated_after_ns,omitempty"`
	UpdatedBeforeNs uint64 `json:"updated_before_ns,omitempty"`
	OrderBy         string `json:"order_by,omitempty"` // "id_asc" | "updated_desc" | ...
	Limit           uint32 `json:"limit,omitempty"`
}

// TasksAdapter is the typed tasks handle.
type TasksAdapter struct {
	mu     sync.RWMutex
	handle *C.net_tasks_adapter_t
}

// OpenTasks opens the tasks adapter against a Redex. `persistent`
// routes writes through the Redex's persistent directory.
func OpenTasks(redex *Redex, originHash uint64, persistent bool) (*TasksAdapter, error) {
	var p C.int
	if persistent {
		p = 1
	}
	// Hold the redex read-lock for the duration of the C call so a
	// concurrent Free() can't race it.
	redex.mu.RLock()
	defer redex.mu.RUnlock()
	if redex.handle == nil {
		return nil, ErrShuttingDown
	}
	var out *C.net_tasks_adapter_t
	code := C.net_tasks_adapter_open(redex.handle, C.uint64_t(originHash), p, &out)
	if err := cortexErrorFromCode(code); err != nil {
		return nil, err
	}
	t := &TasksAdapter{handle: out}
	runtime.SetFinalizer(t, (*TasksAdapter).free)
	return t, nil
}

func (t *TasksAdapter) free() {
	t.mu.Lock()
	defer t.mu.Unlock()
	if t.handle != nil {
		C.net_tasks_adapter_free(t.handle)
		t.handle = nil
		runtime.SetFinalizer(t, nil)
	}
}

// Close stops the fold task. Subsequent CRUD errors with
// ErrCortexClosed. Idempotent.
//
// Two-phase so an indefinite WaitForSeq (which holds an RLock) can't
// hang shutdown: signal the native close under RLock (fast — just an
// atomic swap + notify, wakes pending wait_for_seq waiters), then
// take the writer Lock to free. By the time the writer Lock is
// contested, in-flight waiters have observed `running=false` and
// released their RLocks.
func (t *TasksAdapter) Close() error {
	t.mu.RLock()
	if t.handle == nil {
		t.mu.RUnlock()
		return nil
	}
	code := C.net_tasks_adapter_close(t.handle)
	t.mu.RUnlock()

	t.mu.Lock()
	defer t.mu.Unlock()
	if t.handle == nil {
		return cortexErrorFromCode(code)
	}
	C.net_tasks_adapter_free(t.handle)
	t.handle = nil
	runtime.SetFinalizer(t, nil)
	return cortexErrorFromCode(code)
}

func (t *TasksAdapter) Create(id uint64, title string, nowNs uint64) (uint64, error) {
	cTitle := C.CString(title)
	defer C.free(unsafe.Pointer(cTitle))
	t.mu.RLock()
	defer t.mu.RUnlock()
	if t.handle == nil {
		return 0, ErrShuttingDown
	}
	var seq C.uint64_t
	code := C.net_tasks_create(t.handle, C.uint64_t(id), cTitle, C.uint64_t(nowNs), &seq)
	return uint64(seq), cortexErrorFromCode(code)
}

func (t *TasksAdapter) Rename(id uint64, newTitle string, nowNs uint64) (uint64, error) {
	cNew := C.CString(newTitle)
	defer C.free(unsafe.Pointer(cNew))
	t.mu.RLock()
	defer t.mu.RUnlock()
	if t.handle == nil {
		return 0, ErrShuttingDown
	}
	var seq C.uint64_t
	code := C.net_tasks_rename(t.handle, C.uint64_t(id), cNew, C.uint64_t(nowNs), &seq)
	return uint64(seq), cortexErrorFromCode(code)
}

func (t *TasksAdapter) Complete(id uint64, nowNs uint64) (uint64, error) {
	t.mu.RLock()
	defer t.mu.RUnlock()
	if t.handle == nil {
		return 0, ErrShuttingDown
	}
	var seq C.uint64_t
	code := C.net_tasks_complete(t.handle, C.uint64_t(id), C.uint64_t(nowNs), &seq)
	return uint64(seq), cortexErrorFromCode(code)
}

func (t *TasksAdapter) Delete(id uint64) (uint64, error) {
	t.mu.RLock()
	defer t.mu.RUnlock()
	if t.handle == nil {
		return 0, ErrShuttingDown
	}
	var seq C.uint64_t
	code := C.net_tasks_delete(t.handle, C.uint64_t(id), &seq)
	return uint64(seq), cortexErrorFromCode(code)
}

// WaitForSeq blocks until the fold has applied every event up through
// `seq`. Pass `timeout = 0` to wait indefinitely.
func (t *TasksAdapter) WaitForSeq(seq uint64, timeout time.Duration) error {
	timeoutMs := durationToMillisU32(timeout)
	t.mu.RLock()
	defer t.mu.RUnlock()
	if t.handle == nil {
		return ErrShuttingDown
	}
	code := C.net_tasks_wait_for_seq(t.handle, C.uint64_t(seq), timeoutMs)
	return cortexErrorFromCode(code)
}

// List returns a snapshot query over the materialized state. Pass
// `nil` filter for "all tasks."
func (t *TasksAdapter) List(filter *TasksFilter) ([]Task, error) {
	cFilter, err := marshalFilter(filter)
	if err != nil {
		return nil, err
	}
	if cFilter != nil {
		defer C.free(unsafe.Pointer(cFilter))
	}
	t.mu.RLock()
	defer t.mu.RUnlock()
	if t.handle == nil {
		return nil, ErrShuttingDown
	}
	var out *C.char
	var outLen C.size_t
	code := C.net_tasks_list(t.handle, cFilter, &out, &outLen)
	if err := cortexErrorFromCode(code); err != nil {
		return nil, err
	}
	defer C.net_free_string(out)
	payload := C.GoBytes(unsafe.Pointer(out), C.int(outLen))
	var tasks []Task
	if err := json.Unmarshal(payload, &tasks); err != nil {
		return nil, fmt.Errorf("decode list: %w", err)
	}
	return tasks, nil
}

// SnapshotAndWatch returns the current filter result AND a channel of
// subsequent filter results, atomically. `ctx.Done()` terminates the
// watch; the channel is closed either on ctx cancel or stream end.
// The `errs` channel carries at most one error (stream / decode
// failure) before being closed alongside the data channel.
//
// This is the v2 race-fix primitive — prefer it to calling `List` +
// building your own watch, which would race and lose updates.
func (t *TasksAdapter) SnapshotAndWatch(
	ctx context.Context,
	filter *TasksFilter,
) ([]Task, <-chan []Task, <-chan error, error) {
	cFilter, err := marshalFilter(filter)
	if err != nil {
		return nil, nil, nil, err
	}
	if cFilter != nil {
		defer C.free(unsafe.Pointer(cFilter))
	}
	var snap *C.char
	var snapLen C.size_t
	var cursor *C.net_tasks_watch_t
	// Hold the read-lock only for the duration of the
	// snapshot-and-cursor-creation C call. The cursor itself lives
	// in independent Rust memory; the goroutine below uses the
	// cursor pointer, not the adapter handle.
	t.mu.RLock()
	if t.handle == nil {
		t.mu.RUnlock()
		return nil, nil, nil, ErrShuttingDown
	}
	code := C.net_tasks_snapshot_and_watch(t.handle, cFilter, &snap, &snapLen, &cursor)
	t.mu.RUnlock()
	if err := cortexErrorFromCode(code); err != nil {
		return nil, nil, nil, err
	}
	defer C.net_free_string(snap)
	snapPayload := C.GoBytes(unsafe.Pointer(snap), C.int(snapLen))
	var snapshot []Task
	if err := json.Unmarshal(snapPayload, &snapshot); err != nil {
		C.net_tasks_watch_free(cursor)
		return nil, nil, nil, fmt.Errorf("decode snapshot: %w", err)
	}
	updates, errs := pumpTasksWatch(ctx, cursor)
	return snapshot, updates, errs, nil
}

func pumpTasksWatch(ctx context.Context, cursor *C.net_tasks_watch_t) (<-chan []Task, <-chan error) {
	data := make(chan []Task, 4)
	errs := make(chan error, 1)
	go func() {
		defer func() {
			C.net_tasks_watch_free(cursor)
			close(data)
			close(errs)
		}()
		for {
			if ctx.Err() != nil {
				return
			}
			var out *C.char
			var outLen C.size_t
			code := C.net_tasks_watch_next(cursor, 50, &out, &outLen)
			switch code {
			case 0:
				payload := C.GoBytes(unsafe.Pointer(out), C.int(outLen))
				C.net_free_string(out)
				var batch []Task
				if err := json.Unmarshal(payload, &batch); err != nil {
					errs <- fmt.Errorf("decode watch batch: %w", err)
					return
				}
				select {
				case data <- batch:
				case <-ctx.Done():
					return
				}
			case C.NET_STREAM_TIMEOUT:
			case C.NET_STREAM_ENDED:
				return
			default:
				errs <- cortexErrorFromCode(code)
				return
			}
		}
	}()
	return data, errs
}

// ---------------------------------------------------------------------------
// Memories adapter
// ---------------------------------------------------------------------------

// Memory is a materialized memory record.
type Memory struct {
	ID        uint64   `json:"id"`
	Content   string   `json:"content"`
	Tags      []string `json:"tags"`
	Source    string   `json:"source"`
	CreatedNs uint64   `json:"created_ns"`
	UpdatedNs uint64   `json:"updated_ns"`
	Pinned    bool     `json:"pinned"`
}

// MemoriesFilter mirrors the Rust `MemoriesFilter` via JSON.
type MemoriesFilter struct {
	Source          string   `json:"source,omitempty"`
	ContentContains string   `json:"content_contains,omitempty"`
	Tag             string   `json:"tag,omitempty"`
	AnyTag          []string `json:"any_tag,omitempty"`
	AllTags         []string `json:"all_tags,omitempty"`
	Pinned          *bool    `json:"pinned,omitempty"`
	CreatedAfterNs  uint64   `json:"created_after_ns,omitempty"`
	CreatedBeforeNs uint64   `json:"created_before_ns,omitempty"`
	UpdatedAfterNs  uint64   `json:"updated_after_ns,omitempty"`
	UpdatedBeforeNs uint64   `json:"updated_before_ns,omitempty"`
	OrderBy         string   `json:"order_by,omitempty"`
	Limit           uint32   `json:"limit,omitempty"`
}

// MemoriesAdapter is the typed memories handle.
type MemoriesAdapter struct {
	mu     sync.RWMutex
	handle *C.net_memories_adapter_t
}

func OpenMemories(redex *Redex, originHash uint64, persistent bool) (*MemoriesAdapter, error) {
	var p C.int
	if persistent {
		p = 1
	}
	redex.mu.RLock()
	defer redex.mu.RUnlock()
	if redex.handle == nil {
		return nil, ErrShuttingDown
	}
	var out *C.net_memories_adapter_t
	code := C.net_memories_adapter_open(redex.handle, C.uint64_t(originHash), p, &out)
	if err := cortexErrorFromCode(code); err != nil {
		return nil, err
	}
	m := &MemoriesAdapter{handle: out}
	runtime.SetFinalizer(m, (*MemoriesAdapter).free)
	return m, nil
}

func (m *MemoriesAdapter) free() {
	m.mu.Lock()
	defer m.mu.Unlock()
	if m.handle != nil {
		C.net_memories_adapter_free(m.handle)
		m.handle = nil
		runtime.SetFinalizer(m, nil)
	}
}

// Close stops the fold task. Subsequent ops error with
// ErrCortexClosed. Idempotent. Two-phase — see the doc on
// `(*TasksAdapter).Close` for why.
func (m *MemoriesAdapter) Close() error {
	m.mu.RLock()
	if m.handle == nil {
		m.mu.RUnlock()
		return nil
	}
	code := C.net_memories_adapter_close(m.handle)
	m.mu.RUnlock()

	m.mu.Lock()
	defer m.mu.Unlock()
	if m.handle == nil {
		return cortexErrorFromCode(code)
	}
	C.net_memories_adapter_free(m.handle)
	m.handle = nil
	runtime.SetFinalizer(m, nil)
	return cortexErrorFromCode(code)
}

type memoryStoreInput struct {
	ID      uint64   `json:"id"`
	Content string   `json:"content"`
	Tags    []string `json:"tags"`
	Source  string   `json:"source"`
	NowNs   uint64   `json:"now_ns"`
}

type memoryRetagInput struct {
	ID    uint64   `json:"id"`
	Tags  []string `json:"tags"`
	NowNs uint64   `json:"now_ns"`
}

func (m *MemoriesAdapter) Store(id uint64, content string, tags []string, source string, nowNs uint64) (uint64, error) {
	if tags == nil {
		tags = []string{}
	}
	data, err := json.Marshal(memoryStoreInput{
		ID: id, Content: content, Tags: tags, Source: source, NowNs: nowNs,
	})
	if err != nil {
		return 0, fmt.Errorf("marshal store: %w", err)
	}
	c := C.CString(string(data))
	defer C.free(unsafe.Pointer(c))
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return 0, ErrShuttingDown
	}
	var seq C.uint64_t
	code := C.net_memories_store(m.handle, c, &seq)
	return uint64(seq), cortexErrorFromCode(code)
}

func (m *MemoriesAdapter) Retag(id uint64, tags []string, nowNs uint64) (uint64, error) {
	if tags == nil {
		tags = []string{}
	}
	data, err := json.Marshal(memoryRetagInput{ID: id, Tags: tags, NowNs: nowNs})
	if err != nil {
		return 0, fmt.Errorf("marshal retag: %w", err)
	}
	c := C.CString(string(data))
	defer C.free(unsafe.Pointer(c))
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return 0, ErrShuttingDown
	}
	var seq C.uint64_t
	code := C.net_memories_retag(m.handle, c, &seq)
	return uint64(seq), cortexErrorFromCode(code)
}

func (m *MemoriesAdapter) Pin(id uint64, nowNs uint64) (uint64, error) {
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return 0, ErrShuttingDown
	}
	var seq C.uint64_t
	code := C.net_memories_pin(m.handle, C.uint64_t(id), C.uint64_t(nowNs), &seq)
	return uint64(seq), cortexErrorFromCode(code)
}

func (m *MemoriesAdapter) Unpin(id uint64, nowNs uint64) (uint64, error) {
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return 0, ErrShuttingDown
	}
	var seq C.uint64_t
	code := C.net_memories_unpin(m.handle, C.uint64_t(id), C.uint64_t(nowNs), &seq)
	return uint64(seq), cortexErrorFromCode(code)
}

func (m *MemoriesAdapter) Delete(id uint64) (uint64, error) {
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return 0, ErrShuttingDown
	}
	var seq C.uint64_t
	code := C.net_memories_delete(m.handle, C.uint64_t(id), &seq)
	return uint64(seq), cortexErrorFromCode(code)
}

func (m *MemoriesAdapter) WaitForSeq(seq uint64, timeout time.Duration) error {
	timeoutMs := durationToMillisU32(timeout)
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return ErrShuttingDown
	}
	code := C.net_memories_wait_for_seq(m.handle, C.uint64_t(seq), timeoutMs)
	return cortexErrorFromCode(code)
}

func (m *MemoriesAdapter) List(filter *MemoriesFilter) ([]Memory, error) {
	cFilter, err := marshalFilter(filter)
	if err != nil {
		return nil, err
	}
	if cFilter != nil {
		defer C.free(unsafe.Pointer(cFilter))
	}
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return nil, ErrShuttingDown
	}
	var out *C.char
	var outLen C.size_t
	code := C.net_memories_list(m.handle, cFilter, &out, &outLen)
	if err := cortexErrorFromCode(code); err != nil {
		return nil, err
	}
	defer C.net_free_string(out)
	payload := C.GoBytes(unsafe.Pointer(out), C.int(outLen))
	var memories []Memory
	if err := json.Unmarshal(payload, &memories); err != nil {
		return nil, fmt.Errorf("decode list: %w", err)
	}
	return memories, nil
}

// SnapshotAndWatch mirrors TasksAdapter.SnapshotAndWatch for memories.
func (m *MemoriesAdapter) SnapshotAndWatch(
	ctx context.Context,
	filter *MemoriesFilter,
) ([]Memory, <-chan []Memory, <-chan error, error) {
	cFilter, err := marshalFilter(filter)
	if err != nil {
		return nil, nil, nil, err
	}
	if cFilter != nil {
		defer C.free(unsafe.Pointer(cFilter))
	}
	var snap *C.char
	var snapLen C.size_t
	var cursor *C.net_memories_watch_t
	m.mu.RLock()
	if m.handle == nil {
		m.mu.RUnlock()
		return nil, nil, nil, ErrShuttingDown
	}
	code := C.net_memories_snapshot_and_watch(m.handle, cFilter, &snap, &snapLen, &cursor)
	m.mu.RUnlock()
	if err := cortexErrorFromCode(code); err != nil {
		return nil, nil, nil, err
	}
	defer C.net_free_string(snap)
	snapPayload := C.GoBytes(unsafe.Pointer(snap), C.int(snapLen))
	var snapshot []Memory
	if err := json.Unmarshal(snapPayload, &snapshot); err != nil {
		C.net_memories_watch_free(cursor)
		return nil, nil, nil, fmt.Errorf("decode snapshot: %w", err)
	}
	updates, errs := pumpMemoriesWatch(ctx, cursor)
	return snapshot, updates, errs, nil
}

func pumpMemoriesWatch(ctx context.Context, cursor *C.net_memories_watch_t) (<-chan []Memory, <-chan error) {
	data := make(chan []Memory, 4)
	errs := make(chan error, 1)
	go func() {
		defer func() {
			C.net_memories_watch_free(cursor)
			close(data)
			close(errs)
		}()
		for {
			if ctx.Err() != nil {
				return
			}
			var out *C.char
			var outLen C.size_t
			code := C.net_memories_watch_next(cursor, 50, &out, &outLen)
			switch code {
			case 0:
				payload := C.GoBytes(unsafe.Pointer(out), C.int(outLen))
				C.net_free_string(out)
				var batch []Memory
				if err := json.Unmarshal(payload, &batch); err != nil {
					errs <- fmt.Errorf("decode watch batch: %w", err)
					return
				}
				select {
				case data <- batch:
				case <-ctx.Done():
					return
				}
			case C.NET_STREAM_TIMEOUT:
			case C.NET_STREAM_ENDED:
				return
			default:
				errs <- cortexErrorFromCode(code)
				return
			}
		}
	}()
	return data, errs
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

// marshalFilter serializes any filter struct (TasksFilter /
// MemoriesFilter) to a C-owned string. Returns `nil, nil` when filter
// is nil.
func marshalFilter(filter any) (*C.char, error) {
	if filter == nil {
		return nil, nil
	}
	// Detect a typed-nil filter pointer (e.g., `(*TasksFilter)(nil)`).
	switch v := filter.(type) {
	case *TasksFilter:
		if v == nil {
			return nil, nil
		}
	case *MemoriesFilter:
		if v == nil {
			return nil, nil
		}
	}
	data, err := json.Marshal(filter)
	if err != nil {
		return nil, fmt.Errorf("marshal filter: %w", err)
	}
	if string(data) == "{}" {
		// Empty filter — don't bother marshalling.
		return nil, nil
	}
	return C.CString(string(data)), nil
}

// ---------------------------------------------------------------------------
// Task lifecycle (WorkflowAdapter)
// ---------------------------------------------------------------------------

// WorkflowTaskState is the materialized lifecycle state of one task.
type WorkflowTaskState struct {
	Step     uint32
	Status   string // submitted|running|waiting|blocked|done|failed
	Attempts uint32
}

// WorkflowStatusCounts rolls up task counts per status.
type WorkflowStatusCounts struct {
	Submitted uint64
	Running   uint64
	Waiting   uint64
	Blocked   uint64
	Done      uint64
	Failed    uint64
}

func wfStatusString(code int) string {
	switch code {
	case 0:
		return "submitted"
	case 1:
		return "running"
	case 2:
		return "waiting"
	case 3:
		return "blocked"
	case 4:
		return "done"
	case 5:
		return "failed"
	default:
		return "unknown"
	}
}

// WorkflowAdapter is the typed task-lifecycle handle — a single-writer
// RedEX chain folded into per-task { step, status, attempts }.
type WorkflowAdapter struct {
	mu     sync.RWMutex
	handle *C.net_workflow_adapter_t
}

// OpenWorkflow opens the workflow adapter against a Redex. `persistent`
// routes writes through the Redex's persistent directory.
func OpenWorkflow(redex *Redex, originHash uint64, persistent bool) (*WorkflowAdapter, error) {
	var p C.int
	if persistent {
		p = 1
	}
	redex.mu.RLock()
	defer redex.mu.RUnlock()
	if redex.handle == nil {
		return nil, ErrShuttingDown
	}
	var out *C.net_workflow_adapter_t
	code := C.net_workflow_adapter_open(redex.handle, C.uint64_t(originHash), p, &out)
	if err := cortexErrorFromCode(code); err != nil {
		return nil, err
	}
	w := &WorkflowAdapter{handle: out}
	runtime.SetFinalizer(w, (*WorkflowAdapter).free)
	return w, nil
}

func (w *WorkflowAdapter) free() {
	w.mu.Lock()
	defer w.mu.Unlock()
	if w.handle != nil {
		C.net_workflow_adapter_free(w.handle)
		w.handle = nil
		runtime.SetFinalizer(w, nil)
	}
}

// Free releases the adapter. Idempotent.
func (w *WorkflowAdapter) Free() { w.free() }

func (w *WorkflowAdapter) seqOp(
	call func(*C.net_workflow_adapter_t, *C.uint64_t) C.int,
) (uint64, error) {
	w.mu.RLock()
	defer w.mu.RUnlock()
	if w.handle == nil {
		return 0, ErrShuttingDown
	}
	var seq C.uint64_t
	code := call(w.handle, &seq)
	if err := cortexErrorFromCode(code); err != nil {
		return 0, err
	}
	return uint64(seq), nil
}

// Submit a new task (enters at step 0, submitted).
func (w *WorkflowAdapter) Submit(id uint64) (uint64, error) {
	return w.seqOp(func(h *C.net_workflow_adapter_t, s *C.uint64_t) C.int {
		return C.net_workflow_submit(h, C.uint64_t(id), s)
	})
}

// Start marks the task running.
func (w *WorkflowAdapter) Start(id uint64) (uint64, error) {
	return w.seqOp(func(h *C.net_workflow_adapter_t, s *C.uint64_t) C.int {
		return C.net_workflow_start(h, C.uint64_t(id), s)
	})
}

// Wait parks the task waiting.
func (w *WorkflowAdapter) Wait(id uint64) (uint64, error) {
	return w.seqOp(func(h *C.net_workflow_adapter_t, s *C.uint64_t) C.int {
		return C.net_workflow_wait(h, C.uint64_t(id), s)
	})
}

// Block parks the task blocked.
func (w *WorkflowAdapter) Block(id uint64) (uint64, error) {
	return w.seqOp(func(h *C.net_workflow_adapter_t, s *C.uint64_t) C.int {
		return C.net_workflow_block(h, C.uint64_t(id), s)
	})
}

// Complete marks the task done (terminal success).
func (w *WorkflowAdapter) Complete(id uint64) (uint64, error) {
	return w.seqOp(func(h *C.net_workflow_adapter_t, s *C.uint64_t) C.int {
		return C.net_workflow_complete(h, C.uint64_t(id), s)
	})
}

// Fail marks the task failed (terminal failure).
func (w *WorkflowAdapter) Fail(id uint64) (uint64, error) {
	return w.seqOp(func(h *C.net_workflow_adapter_t, s *C.uint64_t) C.int {
		return C.net_workflow_fail(h, C.uint64_t(id), s)
	})
}

// Advance bumps the step cursor (resets attempts).
func (w *WorkflowAdapter) Advance(id uint64) (uint64, error) {
	return w.seqOp(func(h *C.net_workflow_adapter_t, s *C.uint64_t) C.int {
		return C.net_workflow_advance(h, C.uint64_t(id), s)
	})
}

// Retry re-runs the current step (never resurrects a done task).
func (w *WorkflowAdapter) Retry(id uint64) (uint64, error) {
	return w.seqOp(func(h *C.net_workflow_adapter_t, s *C.uint64_t) C.int {
		return C.net_workflow_retry(h, C.uint64_t(id), s)
	})
}

// Delete removes a task and its whole linked subtree.
func (w *WorkflowAdapter) Delete(id uint64) (uint64, error) {
	return w.seqOp(func(h *C.net_workflow_adapter_t, s *C.uint64_t) C.int {
		return C.net_workflow_delete(h, C.uint64_t(id), s)
	})
}

// RequestCancel records a cancellation signal for the worker to observe.
func (w *WorkflowAdapter) RequestCancel(id uint64) (uint64, error) {
	return w.seqOp(func(h *C.net_workflow_adapter_t, s *C.uint64_t) C.int {
		return C.net_workflow_request_cancel(h, C.uint64_t(id), s)
	})
}

// Link records a parent->child lineage edge (idempotent).
func (w *WorkflowAdapter) Link(parent, child uint64) (uint64, error) {
	w.mu.RLock()
	defer w.mu.RUnlock()
	if w.handle == nil {
		return 0, ErrShuttingDown
	}
	var seq C.uint64_t
	code := C.net_workflow_link(w.handle, C.uint64_t(parent), C.uint64_t(child), &seq)
	if err := cortexErrorFromCode(code); err != nil {
		return 0, err
	}
	return uint64(seq), nil
}

// Get returns the current state of id, or nil if unknown.
func (w *WorkflowAdapter) Get(id uint64) (*WorkflowTaskState, error) {
	w.mu.RLock()
	defer w.mu.RUnlock()
	if w.handle == nil {
		return nil, ErrShuttingDown
	}
	var found, status C.int
	var step, attempts C.uint32_t
	code := C.net_workflow_get(w.handle, C.uint64_t(id), &found, &step, &status, &attempts)
	if err := cortexErrorFromCode(code); err != nil {
		return nil, err
	}
	if found == 0 {
		return nil, nil
	}
	return &WorkflowTaskState{
		Step:     uint32(step),
		Status:   wfStatusString(int(status)),
		Attempts: uint32(attempts),
	}, nil
}

// IsCancelRequested reports whether cancellation was requested for id.
func (w *WorkflowAdapter) IsCancelRequested(id uint64) (bool, error) {
	w.mu.RLock()
	defer w.mu.RUnlock()
	if w.handle == nil {
		return false, ErrShuttingDown
	}
	var b C.int
	code := C.net_workflow_is_cancel_requested(w.handle, C.uint64_t(id), &b)
	if err := cortexErrorFromCode(code); err != nil {
		return false, err
	}
	return b != 0, nil
}

// StatusCounts rolls up task counts per status.
func (w *WorkflowAdapter) StatusCounts() (WorkflowStatusCounts, error) {
	w.mu.RLock()
	defer w.mu.RUnlock()
	if w.handle == nil {
		return WorkflowStatusCounts{}, ErrShuttingDown
	}
	var c C.net_workflow_status_counts_t
	code := C.net_workflow_status_counts(w.handle, &c)
	if err := cortexErrorFromCode(code); err != nil {
		return WorkflowStatusCounts{}, err
	}
	return WorkflowStatusCounts{
		Submitted: uint64(c.submitted),
		Running:   uint64(c.running),
		Waiting:   uint64(c.waiting),
		Blocked:   uint64(c.blocked),
		Done:      uint64(c.done),
		Failed:    uint64(c.failed),
	}, nil
}

// WaitForSeq blocks until every event up through seq has folded.
// `timeoutMs` of 0 waits indefinitely.
func (w *WorkflowAdapter) WaitForSeq(seq uint64, timeoutMs uint32) error {
	w.mu.RLock()
	defer w.mu.RUnlock()
	if w.handle == nil {
		return ErrShuttingDown
	}
	code := C.net_workflow_wait_for_seq(w.handle, C.uint64_t(seq), C.uint32_t(timeoutMs))
	return cortexErrorFromCode(code)
}

// Subtree returns `id` plus all its transitive descendants (the delete
// subtree).
func (w *WorkflowAdapter) Subtree(id uint64) ([]uint64, error) {
	w.mu.RLock()
	defer w.mu.RUnlock()
	if w.handle == nil {
		return nil, ErrShuttingDown
	}
	var count C.size_t
	code := C.net_workflow_subtree(w.handle, C.uint64_t(id), nil, 0, &count)
	if err := cortexErrorFromCode(code); err != nil {
		return nil, err
	}
	if count == 0 {
		return nil, nil
	}
	ids := make([]uint64, int(count))
	code = C.net_workflow_subtree(
		w.handle, C.uint64_t(id), (*C.uint64_t)(unsafe.Pointer(&ids[0])), count, &count)
	if err := cortexErrorFromCode(code); err != nil {
		return nil, err
	}
	if int(count) < len(ids) {
		ids = ids[:int(count)]
	}
	return ids, nil
}

// Snapshot captures a state snapshot: the bytes plus the snapshot's last
// seq (hasLastSeq is false when the chain was empty). Restore both
// together via OpenWorkflowFromSnapshot.
func (w *WorkflowAdapter) Snapshot() (bytes []byte, lastSeq uint64, hasLastSeq bool, err error) {
	w.mu.RLock()
	defer w.mu.RUnlock()
	if w.handle == nil {
		return nil, 0, false, ErrShuttingDown
	}
	var length C.size_t
	var seq C.uint64_t
	var has C.int
	code := C.net_workflow_snapshot(w.handle, nil, 0, &length, &seq, &has)
	if e := cortexErrorFromCode(code); e != nil {
		return nil, 0, false, e
	}
	buf := make([]byte, int(length))
	if length > 0 {
		code = C.net_workflow_snapshot(
			w.handle, (*C.uint8_t)(unsafe.Pointer(&buf[0])), length, &length, &seq, &has)
		if e := cortexErrorFromCode(code); e != nil {
			return nil, 0, false, e
		}
		if int(length) < len(buf) {
			buf = buf[:int(length)]
		}
	}
	return buf, uint64(seq), has != 0, nil
}

// OpenWorkflowFromSnapshot opens a workflow adapter from a snapshot,
// skipping replay up through lastSeq when hasLastSeq is true.
func OpenWorkflowFromSnapshot(
	redex *Redex, originHash uint64, persistent bool,
	snapshot []byte, lastSeq uint64, hasLastSeq bool,
) (*WorkflowAdapter, error) {
	var p C.int
	if persistent {
		p = 1
	}
	var has C.int
	if hasLastSeq {
		has = 1
	}
	redex.mu.RLock()
	defer redex.mu.RUnlock()
	if redex.handle == nil {
		return nil, ErrShuttingDown
	}
	var ptr *C.uint8_t
	if len(snapshot) > 0 {
		ptr = (*C.uint8_t)(unsafe.Pointer(&snapshot[0]))
	}
	var out *C.net_workflow_adapter_t
	code := C.net_workflow_open_from_snapshot(
		redex.handle, C.uint64_t(originHash), p, ptr, C.size_t(len(snapshot)),
		C.uint64_t(lastSeq), has, &out)
	if err := cortexErrorFromCode(code); err != nil {
		return nil, err
	}
	wf := &WorkflowAdapter{handle: out}
	runtime.SetFinalizer(wf, (*WorkflowAdapter).free)
	return wf, nil
}

// ---------------------------------------------------------------------------
// Tier 2: shards (fan-out / fan-in)
// ---------------------------------------------------------------------------

// ShardGroup is a map-reduce shard group: the shard task ids + reduce id.
type ShardGroup struct {
	handle *C.net_shard_group_t
}

// NewShardGroup builds a shard group.
func NewShardGroup(shards []uint64, reduce uint64) (*ShardGroup, error) {
	var ptr *C.uint64_t
	if len(shards) > 0 {
		ptr = (*C.uint64_t)(unsafe.Pointer(&shards[0]))
	}
	var out *C.net_shard_group_t
	code := C.net_shard_group_new(ptr, C.size_t(len(shards)), C.uint64_t(reduce), &out)
	if err := cortexErrorFromCode(code); err != nil {
		return nil, err
	}
	g := &ShardGroup{handle: out}
	runtime.SetFinalizer(g, (*ShardGroup).free)
	return g, nil
}

func (g *ShardGroup) free() {
	if g.handle != nil {
		C.net_shard_group_free(g.handle)
		g.handle = nil
		runtime.SetFinalizer(g, nil)
	}
}

// JoinResult is the outcome of TryJoin.
type JoinResult struct {
	Kind   string   // submitted | already_submitted | pending | failed
	Seq    uint64   // valid when Kind == "submitted"
	Failed []uint64 // valid when Kind == "failed"
}

func joinKindString(k int) string {
	switch k {
	case 0:
		return "submitted"
	case 1:
		return "already_submitted"
	case 2:
		return "pending"
	case 3:
		return "failed"
	default:
		return "unknown"
	}
}

// FanOut submits every shard task. Returns the last append seq.
func (w *WorkflowAdapter) FanOut(group *ShardGroup) (uint64, error) {
	w.mu.RLock()
	defer w.mu.RUnlock()
	if w.handle == nil {
		return 0, ErrShuttingDown
	}
	var seq C.uint64_t
	code := C.net_workflow_fan_out(w.handle, group.handle, &seq)
	if err := cortexErrorFromCode(code); err != nil {
		return 0, err
	}
	return uint64(seq), nil
}

// TryJoin submits the reduce once every shard is done, surfaces failed
// shards, or reports pending. Idempotent.
func (w *WorkflowAdapter) TryJoin(group *ShardGroup) (JoinResult, error) {
	w.mu.RLock()
	defer w.mu.RUnlock()
	if w.handle == nil {
		return JoinResult{}, ErrShuttingDown
	}
	var kind C.int
	var seq C.uint64_t
	var count C.size_t
	// First pass: learn the failed-id count.
	code := C.net_workflow_try_join(w.handle, group.handle, &kind, &seq, nil, 0, &count)
	if err := cortexErrorFromCode(code); err != nil {
		return JoinResult{}, err
	}
	res := JoinResult{Kind: joinKindString(int(kind)), Seq: uint64(seq)}
	if int(kind) == 3 && count > 0 {
		ids := make([]uint64, int(count))
		code = C.net_workflow_try_join(
			w.handle, group.handle, &kind, &seq,
			(*C.uint64_t)(unsafe.Pointer(&ids[0])), count, &count)
		if err := cortexErrorFromCode(code); err != nil {
			return JoinResult{}, err
		}
		if int(count) < len(ids) {
			ids = ids[:int(count)]
		}
		res.Failed = ids
	}
	return res, nil
}

// ---------------------------------------------------------------------------
// Tier 2: triggers (bound to a WorkflowAdapter; reads its state internally)
// ---------------------------------------------------------------------------

// TriggerAction is a fired trigger's action.
type TriggerAction struct {
	Kind string // submit | start
	ID   uint64
}

func actionKindCode(kind string) C.int {
	if kind == "start" {
		return 1
	}
	return 0
}

// TriggerEngine is the pure trigger engine, bound to a WorkflowAdapter.
type TriggerEngine struct {
	handle *C.net_trigger_engine_t
}

// NewTriggerEngine builds a trigger engine bound to `wf`.
func NewTriggerEngine(wf *WorkflowAdapter) (*TriggerEngine, error) {
	wf.mu.RLock()
	defer wf.mu.RUnlock()
	if wf.handle == nil {
		return nil, ErrShuttingDown
	}
	var out *C.net_trigger_engine_t
	code := C.net_trigger_engine_new(wf.handle, &out)
	if err := cortexErrorFromCode(code); err != nil {
		return nil, err
	}
	e := &TriggerEngine{handle: out}
	runtime.SetFinalizer(e, (*TriggerEngine).free)
	return e, nil
}

func (e *TriggerEngine) free() {
	if e.handle != nil {
		C.net_trigger_engine_free(e.handle)
		e.handle = nil
		runtime.SetFinalizer(e, nil)
	}
}

// ArmAfterTask arms AfterTask(task) -> action (fires when task is done).
func (e *TriggerEngine) ArmAfterTask(task uint64, action TriggerAction) error {
	code := C.net_trigger_arm_after_task(
		e.handle, C.uint64_t(task), actionKindCode(action.Kind), C.uint64_t(action.ID))
	return cortexErrorFromCode(code)
}

// ArmAfterTerminal arms AfterTerminal(task) -> action (done OR failed).
func (e *TriggerEngine) ArmAfterTerminal(task uint64, action TriggerAction) error {
	code := C.net_trigger_arm_after_terminal(
		e.handle, C.uint64_t(task), actionKindCode(action.Kind), C.uint64_t(action.ID))
	return cortexErrorFromCode(code)
}

// ArmIfResult arms IfResult(task, key, value) -> action: fires when
// `task` is done AND its recorded result `key` equals `value`. Record the
// result first via RecordResult.
func (e *TriggerEngine) ArmIfResult(task uint64, key, value string, action TriggerAction) error {
	ckey := C.CString(key)
	defer C.free(unsafe.Pointer(ckey))
	cval := C.CString(value)
	defer C.free(unsafe.Pointer(cval))
	code := C.net_trigger_arm_if_result(
		e.handle, C.uint64_t(task), ckey, cval,
		actionKindCode(action.Kind), C.uint64_t(action.ID))
	return cortexErrorFromCode(code)
}

// RecordResult records `task`'s result key = value for IfResult evaluation.
func (e *TriggerEngine) RecordResult(task uint64, key, value string) error {
	ckey := C.CString(key)
	defer C.free(unsafe.Pointer(ckey))
	cval := C.CString(value)
	defer C.free(unsafe.Pointer(cval))
	code := C.net_trigger_record_result(e.handle, C.uint64_t(task), ckey, cval)
	return cortexErrorFromCode(code)
}

// OnTaskChange returns the actions fired by `task`'s change, evaluated
// against the bound adapter's current state.
func (e *TriggerEngine) OnTaskChange(task, tick uint64) ([]TriggerAction, error) {
	var count C.size_t
	code := C.net_trigger_on_task_change(
		e.handle, C.uint64_t(task), C.uint64_t(tick), nil, nil, 0, &count)
	if err := cortexErrorFromCode(code); err != nil {
		return nil, err
	}
	if count == 0 {
		return nil, nil
	}
	kinds := make([]C.int, int(count))
	ids := make([]uint64, int(count))
	code = C.net_trigger_on_task_change(
		e.handle, C.uint64_t(task), C.uint64_t(tick),
		&kinds[0], (*C.uint64_t)(unsafe.Pointer(&ids[0])), count, &count)
	if err := cortexErrorFromCode(code); err != nil {
		return nil, err
	}
	n := int(count)
	if n > len(ids) {
		n = len(ids)
	}
	out := make([]TriggerAction, n)
	for i := 0; i < n; i++ {
		kind := "submit"
		if kinds[i] == 1 {
			kind = "start"
		}
		out[i] = TriggerAction{Kind: kind, ID: ids[i]}
	}
	return out, nil
}

// ArmAtTick arms AtTick(tick) -> action (fires once the clock reaches tick).
func (e *TriggerEngine) ArmAtTick(tick uint64, action TriggerAction) error {
	code := C.net_trigger_arm_at_tick(
		e.handle, C.uint64_t(tick), actionKindCode(action.Kind), C.uint64_t(action.ID))
	return cortexErrorFromCode(code)
}

// OnTick fires + disarms every AtTick trigger due at `now`; returns them.
func (e *TriggerEngine) OnTick(now uint64) ([]TriggerAction, error) {
	var count C.size_t
	code := C.net_trigger_on_tick(e.handle, C.uint64_t(now), nil, nil, 0, &count)
	if err := cortexErrorFromCode(code); err != nil {
		return nil, err
	}
	if count == 0 {
		return nil, nil
	}
	kinds := make([]C.int, int(count))
	ids := make([]uint64, int(count))
	code = C.net_trigger_on_tick(
		e.handle, C.uint64_t(now),
		&kinds[0], (*C.uint64_t)(unsafe.Pointer(&ids[0])), count, &count)
	if err := cortexErrorFromCode(code); err != nil {
		return nil, err
	}
	n := int(count)
	if n > len(ids) {
		n = len(ids)
	}
	out := make([]TriggerAction, n)
	for i := 0; i < n; i++ {
		kind := "submit"
		if kinds[i] == 1 {
			kind = "start"
		}
		out[i] = TriggerAction{Kind: kind, ID: ids[i]}
	}
	return out, nil
}

// ArmedCount returns the number of armed (not-yet-fired) triggers.
func (e *TriggerEngine) ArmedCount() (int, error) {
	var c C.size_t
	code := C.net_trigger_armed_count(e.handle, &c)
	if err := cortexErrorFromCode(code); err != nil {
		return 0, err
	}
	return int(c), nil
}
