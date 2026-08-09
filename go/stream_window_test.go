// An explicit zero window must survive the trip to Rust.
//
// `WindowBytes uint32` with `omitempty` erased the documented
// unbounded mode: the C parser saw no key, `window_bytes.unwrap_or(
// DEFAULT_STREAM_WINDOW_BYTES)` applied 64 KiB, and a Go caller asking
// for no backpressure quietly got bounded backpressure. Nothing on
// either side reported it.
//
// These pin the marshalled bytes and are pure Go — no native library.

package net

import (
	"encoding/json"
	"testing"
)

func marshalStreamConfig(t *testing.T, cfg StreamConfig) map[string]any {
	t.Helper()
	data, err := json.Marshal(cfg)
	if err != nil {
		t.Fatalf("marshal: %v", err)
	}
	var got map[string]any
	if err := json.Unmarshal(data, &got); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}
	return got
}

func TestStreamConfig_UnboundedWindowReachesTheWire(t *testing.T) {
	got := marshalStreamConfig(t, StreamConfig{WindowBytes: UnboundedWindow()})

	value, present := got["window_bytes"]
	if !present {
		t.Fatal("explicit zero window must be present in the JSON — " +
			"omitting it makes Rust apply the 64 KiB default, which is " +
			"the opposite of what the caller asked for")
	}
	if value != float64(0) {
		t.Fatalf("want window_bytes 0, got %v", value)
	}
}

func TestStreamConfig_UnsetWindowIsOmitted(t *testing.T) {
	// Absent must still mean "inherit the default", so a config that
	// never mentions the window marshals exactly as it did before.
	got := marshalStreamConfig(t, StreamConfig{})
	if _, present := got["window_bytes"]; present {
		t.Fatalf("unset window must be omitted so Rust applies its "+
			"default, got %v", got)
	}
}

func TestStreamConfig_ExplicitWindowReachesTheWire(t *testing.T) {
	got := marshalStreamConfig(t, StreamConfig{WindowBytes: WindowBytesOf(16384)})
	if got["window_bytes"] != float64(16384) {
		t.Fatalf("want window_bytes 16384, got %v", got)
	}
}

func TestStreamConfig_ZeroAndUnsetAreDistinguishable(t *testing.T) {
	// The whole point: these two configs must not produce the same JSON.
	unbounded := marshalStreamConfig(t, StreamConfig{WindowBytes: UnboundedWindow()})
	unset := marshalStreamConfig(t, StreamConfig{})

	_, unboundedHas := unbounded["window_bytes"]
	_, unsetHas := unset["window_bytes"]
	if unboundedHas == unsetHas {
		t.Fatal("explicit-zero and unset window must marshal differently")
	}
}

func TestUnboundedWindow_ReturnsDistinctPointers(t *testing.T) {
	// Two configs must not alias one another's window.
	a, b := UnboundedWindow(), UnboundedWindow()
	if a == b {
		t.Fatal("UnboundedWindow must return a fresh pointer per call")
	}
	*a = 99
	if *b != 0 {
		t.Fatal("mutating one window changed another")
	}
}
