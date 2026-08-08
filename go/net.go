// Package net provides Go bindings for the Net high-performance event bus.
//
// Net is designed for AI runtime workloads, providing high-throughput event
// ingestion and consumption with support for the Net encrypted UDP backend.
//
// # Quick Start
//
//	bus, err := net.New(nil)
//	if err != nil {
//	    log.Fatal(err)
//	}
//	defer bus.Shutdown()
//
//	// Ingest events
//	err = bus.IngestRaw(`{"token": "hello", "index": 0}`)
//
//	// Poll events
//	events, err := bus.Poll(100, "")
package net

/*
#cgo LDFLAGS: -L${SRCDIR}/../net/crates/net/target/release -lnet
#include "net.h"
#include <stdlib.h>
*/
import "C"

import (
	"encoding/json"
	"errors"
	"fmt"
	"runtime"
	"strconv"
	"sync"
	"unsafe"
)

// emptyByte is a single-byte array used as a non-null pointer
// substitute for empty-string FFI arguments. Passing the literal
// `nil` `*C.char` would change behavior — the C side maps a null
// `json` pointer to `ErrNullPointer` even when `len == 0`, whereas
// the previous `C.CString("")` produced a valid pointer to a `\0`
// byte and the C side treats it as a (zero-length, valid) input.
// One package-level byte preserves the previous semantics without
// allocating per call.
var emptyByte [1]byte

// Error codes
var (
	ErrNullPointer     = errors.New("null pointer")
	ErrInvalidUTF8     = errors.New("invalid UTF-8")
	ErrInvalidJSON     = errors.New("invalid JSON")
	ErrInitFailed      = errors.New("initialization failed")
	ErrIngestionFailed = errors.New("ingestion failed")
	ErrPollFailed      = errors.New("poll failed")
	ErrBufferTooSmall  = errors.New("buffer too small")
	ErrShuttingDown    = errors.New("shutting down")
	// ErrInvalidArgument (NET_ERR_INVALID_ARGUMENT, -12) reports input
	// that deserialized cleanly but is semantically unusable — e.g. a
	// ScopeFilter whose Kind is unrecognized, or whose required
	// selector is missing, empty, or an all-empty list.
	//
	// Distinct from ErrInvalidJSON, which means the bytes were not
	// valid JSON at all. These shapes previously resolved to the
	// broadest scope filter, so a query that could not narrow silently
	// widened instead of failing.
	ErrInvalidArgument = errors.New("invalid argument")
	ErrUnknown         = errors.New("unknown error")
)

func errorFromCode(code C.int) error {
	switch code {
	case 0:
		return nil
	case -1:
		return ErrNullPointer
	case -2:
		return ErrInvalidUTF8
	case -3:
		return ErrInvalidJSON
	case -4:
		return ErrInitFailed
	case -5:
		return ErrIngestionFailed
	case -6:
		return ErrPollFailed
	case -7:
		return ErrBufferTooSmall
	case -8:
		return ErrShuttingDown
	default:
		return ErrUnknown
	}
}

// Config represents the event bus configuration.
type Config struct {
	// NumShards is the number of shards for parallel ingestion.
	// Defaults to the number of CPU cores.
	NumShards int `json:"num_shards,omitempty"`

	// RingBufferCapacity is the capacity of each shard's ring buffer.
	// Must be a power of 2. Defaults to 1048576 (1M events).
	RingBufferCapacity int `json:"ring_buffer_capacity,omitempty"`

	// BackpressureMode determines behavior when buffers are full.
	// Options: "DropNewest", "DropOldest", "FailProducer"
	BackpressureMode string `json:"backpressure_mode,omitempty"`

	// Net configuration for Net encrypted UDP backend.
	Net *NetConfig `json:"net,omitempty"`
}

// ReliabilityModes are the values NetConfig.Reliability accepts. The
// vocabulary mirrors the core `ReliabilityConfig` enum; the string
// boundary is the only place a typo can survive.
var ReliabilityModes = []string{"none", "light", "full"}

// validateReliability rejects any reliability spelling outside
// ReliabilityModes. An empty string means "unset" and leaves the core
// default in place.
func (c *Config) validateReliability() error {
	if c == nil || c.Net == nil || c.Net.Reliability == "" {
		return nil
	}
	for _, mode := range ReliabilityModes {
		if c.Net.Reliability == mode {
			return nil
		}
	}
	return fmt.Errorf(
		"invalid reliability %q: use one of %v (values are case-sensitive)",
		c.Net.Reliability, ReliabilityModes,
	)
}

// NetConfig represents Net encrypted UDP adapter configuration.
type NetConfig struct {
	// BindAddr is the local bind address (e.g., "127.0.0.1:9000").
	BindAddr string `json:"bind_addr"`

	// PeerAddr is the remote peer address (e.g., "127.0.0.1:9001").
	PeerAddr string `json:"peer_addr"`

	// PSK is the hex-encoded 32-byte pre-shared key.
	PSK string `json:"psk"`

	// Role is the connection role: "initiator" or "responder".
	Role string `json:"role"`

	// PeerPublicKey is the hex-encoded peer's public key (required for initiator).
	PeerPublicKey string `json:"peer_public_key,omitempty"`

	// SecretKey is the hex-encoded secret key (required for responder).
	SecretKey string `json:"secret_key,omitempty"`

	// PublicKey is the hex-encoded public key (required for responder).
	PublicKey string `json:"public_key,omitempty"`

	// Reliability is the reliability mode: "none" (default), "light",
	// or "full". Any other value — including a case variant like
	// "FULL" — is rejected by New; it is not silently treated as
	// "none", which used to turn a typo into fire-and-forget delivery.
	Reliability string `json:"reliability,omitempty"`

	// HeartbeatIntervalMs is the heartbeat interval in milliseconds (default: 5000).
	HeartbeatIntervalMs int64 `json:"heartbeat_interval_ms,omitempty"`

	// SessionTimeoutMs is the session timeout in milliseconds (default: 30000).
	SessionTimeoutMs int64 `json:"session_timeout_ms,omitempty"`

	// BatchedIO enables batched I/O for Linux (default: false).
	BatchedIO bool `json:"batched_io,omitempty"`

	// PacketPoolSize is the packet pool size (default: 64).
	PacketPoolSize int `json:"packet_pool_size,omitempty"`
}

// NetKeypair holds a generated keypair for Net.
type NetKeypair struct {
	// PublicKey is the hex-encoded 32-byte public key.
	PublicKey string `json:"public_key"`

	// SecretKey is the hex-encoded 32-byte secret key.
	SecretKey string `json:"secret_key"`
}

// Event represents a stored event returned from polling.
type Event struct {
	// Raw is the raw JSON payload.
	Raw json.RawMessage `json:"raw"`
}

// PollResponse contains the results of a poll operation.
type PollResponse struct {
	// Events is the list of events.
	Events []json.RawMessage `json:"events"`

	// NextID is the cursor for the next poll.
	NextID string `json:"next_id,omitempty"`

	// HasMore indicates if there are more events available.
	HasMore bool `json:"has_more"`

	// Count is the number of events returned.
	Count int `json:"count"`
}

// Stats contains event bus statistics.
type Stats struct {
	EventsIngested   uint64 `json:"events_ingested"`
	EventsDropped    uint64 `json:"events_dropped"`
	BatchesDispathed uint64 `json:"batches_dispatched"`
}

// Net is a high-performance event bus handle.
//
// All methods are thread-safe and can be called from multiple goroutines.
type Net struct {
	mu     sync.RWMutex
	handle C.net_handle_t
}

// New creates a new Net event bus with the given configuration.
// Pass nil for default configuration.
func New(config *Config) (*Net, error) {
	var configJSON *C.char
	if config != nil {
		// The C parser rejects an unrecognized reliability value, but
		// it can only answer with a generic init failure. Check here so
		// a typo names itself: `Reliability` is a plain string field
		// with no compiler check behind it, and mapping an unknown
		// value to "none" used to downgrade delivery from
		// acknowledged/retransmitted to fire-and-forget in silence.
		if err := config.validateReliability(); err != nil {
			return nil, err
		}
		data, err := json.Marshal(config)
		if err != nil {
			return nil, fmt.Errorf("failed to marshal config: %w", err)
		}
		configJSON = C.CString(string(data))
		defer C.free(unsafe.Pointer(configJSON))
	}

	handle := C.net_init(configJSON)
	if handle == nil {
		return nil, ErrInitFailed
	}

	bs := &Net{handle: handle}
	runtime.SetFinalizer(bs, (*Net).Shutdown)
	return bs, nil
}

// IngestRaw ingests a raw JSON string (fastest path).
//
// The JSON string is stored directly without parsing.
// This is the recommended method for high-throughput ingestion.
func (bs *Net) IngestRaw(json string) error {
	bs.mu.RLock()
	defer bs.mu.RUnlock()

	if bs.handle == nil {
		return ErrShuttingDown
	}

	// Zero-copy: the C side accepts (ptr, size_t) pairs and treats
	// the bytes as immutable, so we can hand it Go's own string
	// backing store via unsafe.StringData. C.CString would malloc +
	// copy; the (ptr, len) pattern matches what compute.go already
	// uses for short-lived args. For empty input we hand it a
	// non-null sentinel so the C side doesn't reject the call as
	// `ErrNullPointer` — preserves the previous `C.CString("")`
	// shape without per-call malloc.
	var ptr *C.char
	if len(json) > 0 {
		ptr = (*C.char)(unsafe.Pointer(unsafe.StringData(json)))
	} else {
		ptr = (*C.char)(unsafe.Pointer(&emptyByte[0]))
	}
	result := C.net_ingest_raw(bs.handle, ptr, C.size_t(len(json)))
	runtime.KeepAlive(json)
	return errorFromCode(result)
}

// IngestRawBatch ingests multiple raw JSON strings in a batch.
//
// Returns the number of successfully ingested events.
func (bs *Net) IngestRawBatch(jsons []string) int {
	if len(jsons) == 0 {
		return 0
	}

	bs.mu.RLock()
	defer bs.mu.RUnlock()

	if bs.handle == nil {
		return 0
	}

	// Zero-copy per element: each entry points at the Go string's
	// own backing store, so the only per-batch allocation is the
	// pointer + length tables themselves. Previously each event
	// paid a full malloc + memcpy through C.CString.
	//
	// Cgo rule note: passing `&ptrs[0]` to C means we're passing Go
	// memory that contains pointers into other Go memory (the string
	// backing stores). The cgo rules forbid that unless every such
	// pointer is pinned via `runtime.Pinner`. Without pinning, the
	// Go runtime's `cgocheck=2` would flag this and a future GC
	// could move a string's backing store out from under the C call.
	// `IngestRaw` doesn't have this problem because it passes a flat
	// `*C.char` (single Go pointer, not a Go slice of Go pointers).
	var pinner runtime.Pinner
	defer pinner.Unpin()
	ptrs := make([]*C.char, len(jsons))
	lens := make([]C.size_t, len(jsons))
	for i, j := range jsons {
		if len(j) > 0 {
			data := unsafe.StringData(j)
			pinner.Pin(data)
			ptrs[i] = (*C.char)(unsafe.Pointer(data))
		} else {
			// `emptyByte` is a package-level Go variable; pin its
			// element so the same rule applies uniformly.
			pinner.Pin(&emptyByte[0])
			ptrs[i] = (*C.char)(unsafe.Pointer(&emptyByte[0]))
		}
		lens[i] = C.size_t(len(j))
	}

	result := C.net_ingest_raw_batch(
		bs.handle,
		(**C.char)(unsafe.Pointer(&ptrs[0])),
		(*C.size_t)(unsafe.Pointer(&lens[0])),
		C.size_t(len(jsons)),
	)
	// Keep the slice (and transitively the strings it references)
	// alive across the cgo call. Go's escape analysis won't see the
	// reachability through the raw pointers we passed; the pinner
	// itself does not extend lifetimes, only relocation/freeing.
	runtime.KeepAlive(jsons)
	runtime.KeepAlive(ptrs)
	runtime.KeepAlive(lens)

	return int(result)
}

// Ingest ingests an event by marshaling the given value to JSON.
func (bs *Net) Ingest(event interface{}) error {
	data, err := json.Marshal(event)
	if err != nil {
		return fmt.Errorf("failed to marshal event: %w", err)
	}
	return bs.IngestRaw(string(data))
}

// IngestBatch ingests multiple events by marshaling them to JSON.
//
// Returns the number of successfully ingested events.
func (bs *Net) IngestBatch(events []interface{}) int {
	jsons := make([]string, 0, len(events))
	for _, e := range events {
		data, err := json.Marshal(e)
		if err != nil {
			continue
		}
		jsons = append(jsons, string(data))
	}
	return bs.IngestRawBatch(jsons)
}

// Poll retrieves events from the bus.
//
// Parameters:
//   - limit: Maximum number of events to return.
//   - cursor: Optional cursor from a previous poll for pagination.
//
// Returns the poll response containing events and pagination info.
func (bs *Net) Poll(limit int, cursor string) (*PollResponse, error) {
	bs.mu.RLock()
	defer bs.mu.RUnlock()

	if bs.handle == nil {
		return nil, ErrShuttingDown
	}

	// Build the request JSON inline. The previous map +
	// `json.Marshal` path allocated a `map[string]interface{}`,
	// boxed the int, ran the reflect-based marshaler, *and* paid
	// a `C.CString` malloc + copy to hand it across the FFI
	// boundary. Hand-rolling produces a single null-terminated
	// byte slice and passes it directly via the Go-managed
	// pointer — kept alive across the cgo call by KeepAlive.
	cRequestBuf := buildPollRequest(limit, cursor)
	cRequest := (*C.char)(unsafe.Pointer(&cRequestBuf[0]))

	// Allocate output buffer (start with 64KB, grow if needed)
	bufferSize := 65536
	for {
		buffer := make([]byte, bufferSize)
		result := C.net_poll(
			bs.handle,
			cRequest,
			(*C.char)(unsafe.Pointer(&buffer[0])),
			C.size_t(bufferSize),
		)
		// Keep both the request and response buffers alive across the
		// cgo call. The request side was already pinned via
		// KeepAlive(cRequestBuf); the response side relied on Go's
		// escape analysis (the `buffer[:result]` slice below keeps it
		// reachable today) which is fragile and inconsistent with the
		// adjacent KeepAlive on the request. Be explicit so the C side
		// can never write into a buffer the GC reclaimed mid-call.
		runtime.KeepAlive(cRequestBuf)
		runtime.KeepAlive(buffer)

		if result == -7 { // NET_ERR_BUFFER_TOO_SMALL
			bufferSize *= 2
			if bufferSize > 64*1024*1024 { // 64MB max
				return nil, ErrBufferTooSmall
			}
			continue
		}

		if result < 0 {
			return nil, errorFromCode(result)
		}

		// Parse response
		var response PollResponse
		if err := json.Unmarshal(buffer[:result], &response); err != nil {
			return nil, fmt.Errorf("failed to parse response: %w", err)
		}

		return &response, nil
	}
}

// buildPollRequest produces a null-terminated UTF-8 byte slice of
// the form `{"limit":<N>}` or `{"limit":<N>,"cursor":<JSON>}`. The
// trailing `0x00` lets us hand the slice's data pointer to a C
// function expecting `const char*` without paying for a C-side
// malloc + copy via `C.CString`.
//
// Cursor escaping deliberately routes through `json.Marshal` so any
// quotes / control bytes / backslashes in a future cursor format
// stay correctly encoded. The simple-limit-only path skips Marshal
// entirely.
func buildPollRequest(limit int, cursor string) []byte {
	// Capacity guess: prefix + ~20 digits for limit + cursor block + close + null.
	buf := make([]byte, 0, 32+len(cursor)+8)
	buf = append(buf, `{"limit":`...)
	buf = strconv.AppendInt(buf, int64(limit), 10)
	if cursor != "" {
		buf = append(buf, `,"cursor":`...)
		cursorJSON, _ := json.Marshal(cursor)
		buf = append(buf, cursorJSON...)
	}
	buf = append(buf, '}', 0)
	return buf
}

// Stats returns event bus statistics.
func (bs *Net) Stats() (*Stats, error) {
	bs.mu.RLock()
	defer bs.mu.RUnlock()

	if bs.handle == nil {
		return nil, ErrShuttingDown
	}

	buffer := make([]byte, 4096)
	result := C.net_stats(
		bs.handle,
		(*C.char)(unsafe.Pointer(&buffer[0])),
		C.size_t(len(buffer)),
	)

	if result < 0 {
		return nil, errorFromCode(result)
	}

	var stats Stats
	if err := json.Unmarshal(buffer[:result], &stats); err != nil {
		return nil, fmt.Errorf("failed to parse stats: %w", err)
	}

	return &stats, nil
}

// NumShards returns the number of shards.
func (bs *Net) NumShards() int {
	bs.mu.RLock()
	defer bs.mu.RUnlock()

	if bs.handle == nil {
		return 0
	}

	return int(C.net_num_shards(bs.handle))
}

// Flush flushes all pending batches to the adapter.
func (bs *Net) Flush() error {
	bs.mu.RLock()
	defer bs.mu.RUnlock()

	if bs.handle == nil {
		return ErrShuttingDown
	}

	result := C.net_flush(bs.handle)
	return errorFromCode(result)
}

// Shutdown gracefully shuts down the event bus and frees resources.
//
// After calling Shutdown, the Net instance is no longer usable.
func (bs *Net) Shutdown() error {
	bs.mu.Lock()
	defer bs.mu.Unlock()

	if bs.handle == nil {
		return nil
	}

	result := C.net_shutdown(bs.handle)
	bs.handle = nil
	runtime.SetFinalizer(bs, nil)
	return errorFromCode(result)
}

// Version returns the library version.
func Version() string {
	return C.GoString(C.net_version())
}

// GenerateNetKeypair generates a new X25519 keypair for Net.
//
// Returns a keypair with hex-encoded public and secret keys.
// Use this to generate keys for a responder, then share the public key
// with the initiator.
func GenerateNetKeypair() (*NetKeypair, error) {
	result := C.net_generate_keypair()
	if result == nil {
		return nil, errors.New("failed to generate keypair (Net feature may not be enabled)")
	}
	defer C.net_free_string(result)

	jsonStr := C.GoString(result)
	var keypair NetKeypair
	if err := json.Unmarshal([]byte(jsonStr), &keypair); err != nil {
		return nil, fmt.Errorf("failed to parse keypair: %w", err)
	}
	return &keypair, nil
}
