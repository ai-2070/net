package net

// TestABIStabilityRemovedReferenceConstantsSurvive is the witness for S8 of
// docs/internal/plans/GO_BINDING_CONSOLIDATION_AND_BLOBS_PLAN.md, which
// deleted the uncompiled Go reference tree (net/crates/net/bindings/go/net/).
// That tree's cgo preambles carried local copies of 56 NET_*
// constants. Deleting them changes no ABI only if every one is still
// defined, with the same value, where real consumers read it: a header in
// net/crates/net/include/ or a file of this module. This pins that, so the
// deletion cannot have quietly taken the only definition with it.
//
// The table is the set those files defined at 610cd4e, extracted
// mechanically from the deleted sources.

import (
	"os"
	"path/filepath"
	"regexp"
	"testing"
)

func TestABIStabilityRemovedReferenceConstantsSurvive(t *testing.T) {
	removed := []struct{ name, value string }{
		{"NET_RPC_DIRECTION_INBOUND_C", "1"},
		{"NET_RPC_DIRECTION_OUTBOUND_C", "0"},
		{"NET_RPC_STATUS_CANCELED_C", "3"},
		{"NET_RPC_STATUS_ERROR_C", "1"},
		{"NET_RPC_STATUS_OK_C", "0"},
		{"NET_RPC_STATUS_TIMEOUT_C", "2"},
		{"NET_DECK_AVOID_SCOPE_GLOBAL", "0"},
		{"NET_DECK_AVOID_SCOPE_LOCAL", "1"},
		{"NET_DECK_AVOID_SCOPE_ON_PEER", "2"},
		{"NET_DECK_ERR_ALREADY_SHUTDOWN", "-4"},
		{"NET_DECK_ERR_CALL_FAILED", "-2"},
		{"NET_DECK_ERR_END_OF_STREAM", "-5"},
		{"NET_DECK_ERR_INVALID_ARG", "-3"},
		{"NET_DECK_ERR_NULL", "-1"},
		{"NET_DECK_EVENT_KIND_CLEAR_AVOID_LIST", "9"},
		{"NET_DECK_EVENT_KIND_CORDON", "4"},
		{"NET_DECK_EVENT_KIND_DRAIN", "1"},
		{"NET_DECK_EVENT_KIND_DROP_REPLICAS", "6"},
		{"NET_DECK_EVENT_KIND_ENTER_MAINTENANCE", "2"},
		{"NET_DECK_EVENT_KIND_EXIT_MAINTENANCE", "3"},
		{"NET_DECK_EVENT_KIND_INVALIDATE_PLACEMENT", "7"},
		{"NET_DECK_EVENT_KIND_RESTART_ALL_DAEMONS", "8"},
		{"NET_DECK_EVENT_KIND_UNCORDON", "5"},
		{"NET_DECK_EVENT_KIND_UNKNOWN", "0"},
		{"NET_DECK_LOG_DEBUG", "1"},
		{"NET_DECK_LOG_ERROR", "4"},
		{"NET_DECK_LOG_INFO", "2"},
		{"NET_DECK_LOG_TRACE", "0"},
		{"NET_DECK_LOG_WARN", "3"},
		{"NET_DECK_OK", "0"},
		{"NET_MESHDB_CACHE_PERMANENT", "0"},
		{"NET_MESHDB_CACHE_TIME_BOUND", "1"},
		{"NET_MESHDB_END", "1"},
		{"NET_MESHDB_INVALID_ARG", "2"},
		{"NET_MESHDB_OK", "0"},
		{"NET_MESHDB_RUNTIME_ERR", "3"},
		{"NET_MESHOS_CONTROL_BACKPRESSURE_OFF", "5"},
		{"NET_MESHOS_CONTROL_BACKPRESSURE_ON", "4"},
		{"NET_MESHOS_CONTROL_DRAIN_FINISH", "3"},
		{"NET_MESHOS_CONTROL_DRAIN_START", "2"},
		{"NET_MESHOS_CONTROL_NONE", "0"},
		{"NET_MESHOS_CONTROL_SHUTDOWN", "1"},
		{"NET_MESHOS_CONTROL_UNKNOWN", "99"},
		{"NET_MESHOS_ERR_ALREADY_SHUTDOWN", "-4"},
		{"NET_MESHOS_ERR_CALL_FAILED", "-2"},
		{"NET_MESHOS_ERR_INVALID_ARG", "-3"},
		{"NET_MESHOS_ERR_NULL", "-1"},
		{"NET_MESHOS_HEALTH_DEGRADED", "1"},
		{"NET_MESHOS_HEALTH_HEALTHY", "0"},
		{"NET_MESHOS_HEALTH_UNHEALTHY", "2"},
		{"NET_MESHOS_LOG_DEBUG", "1"},
		{"NET_MESHOS_LOG_ERROR", "4"},
		{"NET_MESHOS_LOG_INFO", "2"},
		{"NET_MESHOS_LOG_TRACE", "0"},
		{"NET_MESHOS_LOG_WARN", "3"},
		{"NET_MESHOS_OK", "0"},
	}
	defined := map[string]map[string]string{} // name -> value -> file
	re := regexp.MustCompile(`(?m)^[ 	]*#[ 	]*define[ 	]+(NET_[A-Z0-9_]+)[ 	]+(-?[0-9A-Za-z_]+)|^[ 	]*(NET_[A-Z0-9_]+)[ 	]*=[ 	]*(-?[0-9A-Za-z_]+)`)
	var files []string
	for _, glob := range []string{"../net/crates/net/include/*.h", "*.h", "*.go"} {
		m, err := filepath.Glob(glob)
		if err != nil {
			t.Fatal(err)
		}
		files = append(files, m...)
	}
	for _, f := range files {
		src, err := os.ReadFile(f)
		if err != nil {
			t.Fatalf("read %s: %v", f, err)
		}
		for _, m := range re.FindAllStringSubmatch(string(src), -1) {
			name, value := m[1], m[2]
			if name == "" {
				name, value = m[3], m[4]
			}
			if defined[name] == nil {
				defined[name] = map[string]string{}
			}
			defined[name][value] = f
		}
	}
	for _, c := range removed {
		values, ok := defined[c.name]
		if !ok {
			t.Errorf("%s is no longer defined anywhere a consumer reads", c.name)
			continue
		}
		if _, ok := values[c.value]; !ok {
			t.Errorf("%s survives with a different value: had %s, now %v", c.name, c.value, values)
		}
	}
}
