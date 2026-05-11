// Tests for the channel-auth surface on MeshNode (Stage G-4).
//
// Single-mesh tests mirror the discipline of `test_channel_auth.py`
// / `test_channels.py`. Full two-mesh handshake coverage lives in
// the Rust integration suite (`tests/channel_auth.rs`) and the TS
// SDK. What we verify here, single-mesh:
//
//   1. RegisterChannel accepts PublishCaps / SubscribeCaps.
//   2. Publisher-side publish denial (own caps don't satisfy
//      PublishCaps).
//   3. SubscribeChannelWithToken rejects malformed bytes up-front
//      with ErrTokenInvalidFormat.
//   4. A well-formed token reaches the transport — the failure we
//      observe is ErrChannel (missing peer), not a token error.

package net

import (
	"errors"
	"testing"
)

// ---------------------------------------------------------------------------
// Channel config shape
// ---------------------------------------------------------------------------

func TestRegisterChannel_AcceptsPublishAndSubscribeCaps(t *testing.T) {
	m := newMeshForCaps(t)
	defer m.Shutdown()

	err := m.RegisterChannel(ChannelConfig{
		Name:          "auth/both",
		PublishCaps:   &CapabilityFilter{RequireTags: []string{"admin"}},
		SubscribeCaps: &CapabilityFilter{RequireTags: []string{"reader"}},
	})
	if err != nil {
		t.Fatalf("register_channel w/ caps: %v", err)
	}
}

func TestRegisterChannel_AcceptsGPUFilterForSubscribe(t *testing.T) {
	m := newMeshForCaps(t)
	defer m.Shutdown()

	err := m.RegisterChannel(ChannelConfig{
		Name: "gpu/only",
		SubscribeCaps: &CapabilityFilter{
			RequireGPU: true,
			GPUVendor:  "nvidia",
			MinVRAMGB:  16,
		},
	})
	if err != nil {
		t.Fatalf("register_channel w/ gpu subscribe caps: %v", err)
	}
}

// ---------------------------------------------------------------------------
// Publisher-side publish denial (single-mesh; enforced pre fan-out)
// ---------------------------------------------------------------------------

func TestPublish_DeniedByOwnPublishCaps(t *testing.T) {
	m := newMeshForCaps(t)
	defer m.Shutdown()

	// Node has no announced caps; channel requires `admin` tag.
	if err := m.AnnounceCapabilities(CapabilitySet{}); err != nil {
		t.Fatalf("announce: %v", err)
	}
	if err := m.RegisterChannel(ChannelConfig{
		Name:        "admin/only",
		PublishCaps: &CapabilityFilter{RequireTags: []string{"admin"}},
	}); err != nil {
		t.Fatalf("register_channel: %v", err)
	}

	_, err := m.Publish("admin/only", []byte("x"), PublishConfig{Reliability: "fire_and_forget"})
	if err == nil {
		t.Fatal("expected publish to be denied by own cap filter")
	}
	if !errors.Is(err, ErrChannel) && !errors.Is(err, ErrChannelAuth) {
		t.Fatalf("want ErrChannel/ErrChannelAuth, got %v", err)
	}
}

func TestPublish_AllowedWhenOwnCapsMatch(t *testing.T) {
	m := newMeshForCaps(t)
	defer m.Shutdown()

	if err := m.AnnounceCapabilities(CapabilitySet{Tags: []string{"admin"}}); err != nil {
		t.Fatalf("announce: %v", err)
	}
	if err := m.RegisterChannel(ChannelConfig{
		Name:        "admin/only",
		PublishCaps: &CapabilityFilter{RequireTags: []string{"admin"}},
	}); err != nil {
		t.Fatalf("register_channel: %v", err)
	}
	report, err := m.Publish(
		"admin/only", []byte("x"), PublishConfig{Reliability: "fire_and_forget"},
	)
	if err != nil {
		t.Fatalf("publish: %v", err)
	}
	if report.Attempted != 0 {
		t.Fatalf("no subscribers — want attempted=0, got %d", report.Attempted)
	}
}

func TestPublish_OpenChannelNoCapsEnforced(t *testing.T) {
	m := newMeshForCaps(t)
	defer m.Shutdown()

	// Regression: no publish_caps + no require_token ⇒ open.
	if err := m.RegisterChannel(ChannelConfig{Name: "open/anyone"}); err != nil {
		t.Fatalf("register_channel: %v", err)
	}
	report, err := m.Publish(
		"open/anyone", []byte("x"), PublishConfig{Reliability: "fire_and_forget"},
	)
	if err != nil {
		t.Fatalf("publish open: %v", err)
	}
	if report.Attempted != 0 {
		t.Fatalf("want attempted=0, got %d", report.Attempted)
	}
}

// ---------------------------------------------------------------------------
// SubscribeChannelWithToken parsing
// ---------------------------------------------------------------------------

func TestSubscribeWithToken_RejectsMalformedBytes(t *testing.T) {
	m := newMeshForCaps(t)
	defer m.Shutdown()

	// 16 bytes is far too short for a 159-byte PermissionToken —
	// `from_bytes` must reject with `ErrTokenInvalidFormat` *before*
	// any network I/O, so there's no peer-missing timeout.
	err := m.SubscribeChannelWithToken(0, "some/channel", make([]byte, 16))
	if !errors.Is(err, ErrTokenInvalidFormat) {
		t.Fatalf("want ErrTokenInvalidFormat, got %v", err)
	}
}

func TestSubscribeWithToken_AcceptsStructurallyValid(t *testing.T) {
	// A well-formed, signed 159-byte token reaches the transport —
	// structural parse succeeds client-side, so the failure we see
	// is ErrChannel for the missing peer, NOT a token error.
	issuer, _ := GenerateIdentity()
	defer issuer.Close()
	subject, _ := GenerateIdentity()
	defer subject.Close()
	subID, _ := subject.EntityID()
	token := issueTestToken(
		t, issuer, subID, []string{"subscribe"}, "c", 60, 0,
	)

	m := newMeshForCaps(t)
	defer m.Shutdown()

	err := m.SubscribeChannelWithToken(0, "c", token)
	if err == nil {
		t.Fatal("expected ErrChannel (no peer), got nil")
	}
	if errors.Is(err, ErrTokenInvalidFormat) ||
		errors.Is(err, ErrTokenInvalidSignature) {
		t.Fatalf("token parsed fine client-side; want non-token error, got %v", err)
	}
}

func TestSubscribeWithoutToken_ErrorsAtTransport(t *testing.T) {
	// A dangling subscribe with no peer should fail as an
	// ErrChannel / transport failure, not crash.
	m := newMeshForCaps(t)
	defer m.Shutdown()

	err := m.SubscribeChannel(12345, "anywhere")
	if err == nil {
		t.Fatal("expected transport error, got nil")
	}
}
