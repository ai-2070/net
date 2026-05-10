// Package net — placement-filter bridge for the C ABI exported by
// `bindings/go/compute-ffi` (Phase 7 slice 4 of
// `CAPABILITY_SYSTEM_SDK_PLAN.md`).
//
// This file is a **reference implementation** documenting the
// expected Go-side surface for consumers of `libnet_compute`. The
// upstream net repo owns the C ABI side (see
// `bindings/go/compute-ffi/src/lib.rs`); downstream Go consumers
// either copy this file or import a published Go module that
// mirrors it. Pattern matches `mesh_rpc.go`.
//
// # Build prerequisites
//
//	#cgo LDFLAGS: -L/path/to/target/release -lnet_compute
//
// # Lifecycle
//
//	f := PlacementFilterFromFn(myPredicate, "")        // SDK helper, capability.go
//	if err := RegisterPlacementFilter(mesh, f); err != nil { ... }
//	// daemon spec references CustomFilterID = f.ID; substrate
//	// resolves through the registry on each placement decision.
//	UnregisterPlacementFilter(f.ID)                    // when done
//
// # Wire shape
//
// Rust marshals the candidate as a single JSON string per scoring
// call:
//
//	{ "node_id": uint64, "tags": [string], "metadata": map[string]string }
//
// Go decodes inside the trampoline before invoking the user's
// `PlacementFilterFn`. JSON keeps the C ABI tight (one byte
// buffer per call) at the cost of a per-call serde round-trip —
// bounded by per-node metadata cardinality, acceptable for the
// placement hot path. Node + Python bindings marshal natively
// because their FFIs already speak structured types; cgo doesn't.
//
// # Cross-binding compat contract
//
// Downstream Go test trees are expected to mirror the Rust /
// TS / Python cross-binding compat tests:
//
//   - Rust:   `tests/cross_lang_capability_fixtures.rs`,
//             test `predicate_eval_fixture_matches_via_placement_filter_callback`
//   - TS:     `sdk-ts/test/capability_enhancements.test.ts`,
//             describe `placementFilterFromFn (cross-binding fixture)`
//   - Python: `sdk-py/tests/test_capability_enhancements.py`,
//             test `test_predicate_eval_fixture_via_placement_filter_callback`
//
// Each test loads `tests/cross_lang_capability/predicate_eval.json`,
// wraps each case's predicate as a `PlacementFilterFn` callback,
// and asserts the wrapped-callback boolean output equals direct
// `Predicate::evaluate_unplanned` (the fixture's `expected` field).
// The Go equivalent — using `PlacementFilterFromFn` and
// `EvaluatePredicate(pred, candidate.Tags, candidate.Metadata)` —
// is mechanical and lives in the downstream Go binding tree's
// `capability_test.go`. Failures across bindings show up as a
// fixture-driven CI failure on the drifting binding.
package net

/*
#include <stdint.h>
#include <stdlib.h>

// Trampoline signature — Rust calls back into Go via this function
// pointer to score a placement candidate. Return:
//   - 1: keep candidate (Rust translates to placement-score 1.0)
//   - 0: drop candidate (Rust returns None from `placement_score`)
//   - any negative value: Go-side error; treated as veto
typedef int (*PlacementFilterFn)(
    const char* filter_id_ptr, size_t filter_id_len,
    uint64_t node_id,
    const char* candidate_json_ptr, size_t candidate_json_len
);

// Imported FFI surface from `net-compute-ffi`.
extern int net_compute_set_placement_filter_dispatcher(PlacementFilterFn dispatcher);
extern int net_compute_register_placement_filter(
    net_compute_mesh_arc_t* mesh_arc,
    const char* id_ptr, size_t id_len
);
extern int net_compute_unregister_placement_filter(
    const char* id_ptr, size_t id_len
);
extern int net_compute_has_placement_filter(
    const char* id_ptr, size_t id_len
);

// Forward-declared Go trampoline. Defined below as a `//export`
// function and registered via
// `net_compute_set_placement_filter_dispatcher` on first use.
int go_net_placement_filter_trampoline(
    const char* filter_id_ptr, size_t filter_id_len,
    uint64_t node_id,
    const char* candidate_json_ptr, size_t candidate_json_len
);
*/
import "C"

import (
	"encoding/json"
	"fmt"
	"sync"
	"unsafe"
)

// =====================================================================
// Filter registry — Go side
// =====================================================================

var (
	// placementFilters keyed by SDK-supplied string id (the
	// `RegisteredPlacementFilter.ID` produced by
	// `PlacementFilterFromFn` in capability.go). Value is
	// `PlacementFilterFn` (the user predicate).
	placementFilters sync.Map

	// First-call-wins dispatcher registration. Mirrors
	// `dispatcherOnce` in mesh_rpc.go.
	placementDispatcherOnce sync.Once
)

// registerPlacementDispatcher tells the Rust side which Go function
// to invoke per placement-scoring call. Idempotent — only the first
// call from any goroutine takes effect (matches the `OnceLock`
// semantics on the Rust side).
func registerPlacementDispatcher() {
	placementDispatcherOnce.Do(func() {
		C.net_compute_set_placement_filter_dispatcher(
			(C.PlacementFilterFn)(C.go_net_placement_filter_trampoline),
		)
	})
}

//export go_net_placement_filter_trampoline
func go_net_placement_filter_trampoline(
	filterIDPtr *C.char, filterIDLen C.size_t,
	nodeID C.uint64_t,
	candidateJSONPtr *C.char, candidateJSONLen C.size_t,
) C.int {
	// Look up the registered Go closure.
	id := C.GoStringN(filterIDPtr, C.int(filterIDLen))
	val, ok := placementFilters.Load(id)
	if !ok {
		// Unknown id — Rust shouldn't dispatch here unless the
		// Go side raced an Unregister with an in-flight scoring
		// call. Defensive veto.
		return 0
	}
	fn, ok := val.(PlacementFilterFn)
	if !ok {
		fmt.Printf(
			"net: placement filter %q has wrong type %T; vetoing\n",
			id, val,
		)
		return -1
	}

	// Decode candidate JSON. Buffer is owned by Rust for the
	// duration of this call; copy into a Go string immediately so
	// the user's predicate can hold the slice freely.
	candidateJSON := C.GoStringN(candidateJSONPtr, C.int(candidateJSONLen))
	var candidate PlacementCandidate
	if err := json.Unmarshal([]byte(candidateJSON), &candidate); err != nil {
		fmt.Printf(
			"net: placement filter %q candidate JSON decode failed for node %#x: %v; vetoing\n",
			id, uint64(nodeID), err,
		)
		return -2
	}

	// Recover from user panics — a buggy predicate must not crash
	// the whole process. Mirrors `safeCallHandler` in mesh_rpc.go.
	keep := safeCallPlacementFilter(fn, candidate)
	if keep {
		return 1
	}
	return 0
}

func safeCallPlacementFilter(fn PlacementFilterFn, candidate PlacementCandidate) (keep bool) {
	defer func() {
		if r := recover(); r != nil {
			fmt.Printf("net: placement filter panicked: %v; vetoing candidate %#x\n",
				r, candidate.NodeID)
			keep = false
		}
	}()
	return fn(candidate)
}

// =====================================================================
// Public API — register / unregister
// =====================================================================

// MeshArcPtr is an opaque handle obtained from `net_mesh_arc_clone`
// (defined in `net::ffi::mesh`, exposed by upstream consumers). The
// pointer is NOT consumed by `RegisterPlacementFilter`; callers
// remain responsible for freeing it via `net_mesh_arc_free` when
// they no longer need the mesh handle.
type MeshArcPtr unsafe.Pointer

// CR-26: serializes Register/Unregister for the same id to close
// a race where a concurrent unregister between the Go-map insert
// and the Rust register call deleted the Go entry just as Rust
// took ownership — the substrate ended up with a registration
// pointing at no Go callable. Register/unregister are config-
// time operations; a single global mutex is sufficient.
var placementRegisterMutex sync.Mutex

// PlacementFilterError categorizes register-side failures.
type PlacementFilterError struct {
	Code int
	Msg  string
}

func (e *PlacementFilterError) Error() string {
	return fmt.Sprintf("placement-filter: %s (code %d)", e.Msg, e.Code)
}

// RegisterPlacementFilter wires a `RegisteredPlacementFilter` (from
// capability.go's `PlacementFilterFromFn`) to the substrate, so any
// subsequent placement decision whose
// `StandardPlacement.CustomFilterID` equals `f.ID` routes through
// `f.Fn` for per-candidate scoring.
//
// Errors:
//
//   - NULL `meshHandle` or empty id → `PlacementFilterError{-1}`
//   - Duplicate id (already registered) → `PlacementFilterError{-3}`
//   - Dispatcher not yet installed → `PlacementFilterError{-4}` (only
//     possible if a prior `registerPlacementDispatcher` call panicked
//     before completing)
//   - Non-UTF-8 id → `PlacementFilterError{-5}`
func RegisterPlacementFilter(meshHandle MeshArcPtr, f RegisteredPlacementFilter) error {
	if meshHandle == nil {
		return &PlacementFilterError{Code: -1, Msg: "nil mesh handle"}
	}
	if f.ID == "" {
		return &PlacementFilterError{Code: -1, Msg: "empty filter id"}
	}
	if f.Fn == nil {
		return &PlacementFilterError{Code: -1, Msg: "nil filter function"}
	}

	// CR-26: serialize against UnregisterPlacementFilter for the
	// same id to close the gap where a concurrent unregister
	// between the Go-map LoadOrStore and the Rust register call
	// would delete the Go entry just before Rust took ownership.
	placementRegisterMutex.Lock()
	defer placementRegisterMutex.Unlock()

	registerPlacementDispatcher()

	// Insert into Go-side map BEFORE telling Rust — if Rust
	// dispatched between the C call and the Store, the map miss
	// would surface as a defensive veto. Insert-first is safer.
	if _, loaded := placementFilters.LoadOrStore(f.ID, f.Fn); loaded {
		return &PlacementFilterError{Code: -3, Msg: fmt.Sprintf("id %q already registered", f.ID)}
	}

	cID := C.CString(f.ID)
	defer C.free(unsafe.Pointer(cID))

	rc := C.net_compute_register_placement_filter(
		unsafe.Pointer(meshHandle),
		cID, C.size_t(len(f.ID)),
	)
	if rc != 0 {
		// Roll back so the maps stay consistent. The Rust-side
		// registry didn't take ownership, so a re-register attempt
		// will succeed.
		placementFilters.Delete(f.ID)
		return &PlacementFilterError{
			Code: int(rc),
			Msg:  fmt.Sprintf("substrate rejected registration of id %q", f.ID),
		}
	}
	return nil
}

// UnregisterPlacementFilter drops the Go-side and substrate-side
// registrations under `id`. Returns `true` if the substrate had a
// matching registration (Rust returns `1`); `false` otherwise. Any
// in-flight scheduler call holding an `Arc<dyn PlacementFilter>`
// clone keeps the predicate alive until that call completes — see
// the substrate registry docs.
//
// Substrate-first ordering. Pre-fix this deleted the Go-side
// callback BEFORE the Rust unregister call returned, opening a
// race window: a concurrent dispatcher invocation that had
// already resolved the substrate handle but hadn't reached the
// Go trampoline would find the callback missing and a defensive
// "not registered" veto would ship in place of the real
// predicate result. Unregister substrate-side first — the
// substrate registry's own ref-counting keeps in-flight callers
// alive on the Rust side until their `Arc<dyn PlacementFilter>`
// clones drop — and only then drop the Go callback.
func UnregisterPlacementFilter(id string) bool {
	if id == "" {
		// Match the registry's empty-id rejection so Go state
		// stays consistent with substrate state.
		return false
	}
	// CR-26: serialize against RegisterPlacementFilter for the
	// same id (see comment on `placementRegisterMutex`).
	placementRegisterMutex.Lock()
	defer placementRegisterMutex.Unlock()
	cID := C.CString(id)
	defer C.free(unsafe.Pointer(cID))
	rc := C.net_compute_unregister_placement_filter(cID, C.size_t(len(id)))
	// Drop the Go-side callback only after the substrate has
	// dropped its registry entry. Any in-flight dispatch that
	// already resolved the substrate handle was holding a
	// lock-free clone; substrate's drop semantics keep the
	// predicate alive until that clone is released, but the Go
	// trampoline only reads `placementFilters` synchronously
	// from the dispatcher entry point — so deleting after the
	// substrate-side drop is the moment no new dispatches can
	// race in.
	placementFilters.Delete(id)
	return rc == 1
}

// HasPlacementFilter reports whether the substrate has a
// registration for `id`. Mainly diagnostic.
func HasPlacementFilter(id string) bool {
	if id == "" {
		return false
	}
	cID := C.CString(id)
	defer C.free(unsafe.Pointer(cID))
	rc := C.net_compute_has_placement_filter(cID, C.size_t(len(id)))
	return rc == 1
}
