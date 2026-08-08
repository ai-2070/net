// `RequireToken` without `TokenRoots` is a permanently closed channel.
//
// Core rejects every authorization when token enforcement is on and no
// trusted roots are installed (`channel/config.rs`). The C FFI's
// `ChannelConfigInput` has accepted `token_roots` all along, but Go's
// `ChannelConfig` omitted the field — so an ordinary Go caller could
// reach the fail-closed switch and nothing that makes it usable.
//
// The marshalling tests are pure Go and run without the native library;
// the registration test needs it.

package net

import (
	"encoding/json"
	"strings"
	"testing"
)

func rootHex(b byte) string {
	return strings.Repeat(string("0123456789abcdef"[b&0xf]), 64)
}

func TestChannelConfig_MarshalsTokenRoots(t *testing.T) {
	data, err := json.Marshal(ChannelConfig{
		Name:         "secure/telemetry",
		RequireToken: true,
		TokenRoots:   []string{rootHex(1), rootHex(2)},
	})
	if err != nil {
		t.Fatalf("marshal: %v", err)
	}

	var got map[string]any
	if err := json.Unmarshal(data, &got); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}

	// The key must match the C FFI's `ChannelConfigInput` field name,
	// or the roots are silently dropped by serde and the channel stays
	// closed with no error anywhere.
	roots, ok := got["token_roots"].([]any)
	if !ok {
		t.Fatalf("token_roots missing or wrong type in %s", data)
	}
	if len(roots) != 2 {
		t.Fatalf("want 2 roots, got %d in %s", len(roots), data)
	}
	if roots[0] != rootHex(1) {
		t.Fatalf("root 0 = %v, want %v", roots[0], rootHex(1))
	}
}

func TestChannelConfig_OmitsEmptyTokenRoots(t *testing.T) {
	// `omitempty` keeps the untouched config byte-identical to what it
	// marshalled before the field existed — serde would reject a
	// `"token_roots": null` it does not expect on older cores.
	data, err := json.Marshal(ChannelConfig{Name: "plain"})
	if err != nil {
		t.Fatalf("marshal: %v", err)
	}
	if strings.Contains(string(data), "token_roots") {
		t.Fatalf("empty TokenRoots should be omitted, got %s", data)
	}
}

func TestRegisterChannel_AcceptsTokenRoots(t *testing.T) {
	m := newMeshForCaps(t)
	defer m.Shutdown()

	if err := m.RegisterChannel(ChannelConfig{
		Name:         "secure/telemetry",
		RequireToken: true,
		TokenRoots:   []string{rootHex(3)},
	}); err != nil {
		t.Fatalf("register token-gated channel: %v", err)
	}
}

func TestRegisterChannel_RejectsMalformedTokenRoot(t *testing.T) {
	m := newMeshForCaps(t)
	defer m.Shutdown()

	for _, bad := range []string{
		"not-hex",
		strings.Repeat("ab", 16), // 16 bytes, not 32
		strings.Repeat("ab", 64), // 64 bytes, not 32
	} {
		err := m.RegisterChannel(ChannelConfig{
			Name:         "secure/bad",
			RequireToken: true,
			TokenRoots:   []string{bad},
		})
		if err == nil {
			t.Fatalf("expected rejection for token root %q", bad)
		}
	}
}
