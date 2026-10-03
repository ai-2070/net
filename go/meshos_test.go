package net

import (
	"errors"
	"testing"
)

// TestMeshOsDefaultDaemonImplementsInterface confirms the embed-this
// base type provides every optional MeshOsDaemon method. A consumer
// defines Name + Process and embeds MeshOsDefaultDaemon — that should
// be a full implementation of the interface.
func TestMeshOsDefaultDaemonImplementsInterface(t *testing.T) {
	type minimalDaemon struct {
		MeshOsDefaultDaemon
	}
	// The interface contract requires Name + Process — minimalDaemon
	// doesn't have them yet, so this should fail to type-check if we
	// asserted it directly. Add them inline below to make the
	// assertion valid.
	type fullDaemon struct {
		MeshOsDefaultDaemon
		name string
	}
	d := &fullDaemonImpl{name: "echo"}
	var _ MeshOsDaemon = d
	if d.Name() != "echo" {
		t.Fatalf("Name() = %q, want %q", d.Name(), "echo")
	}
	// Default methods should return safe zero values.
	if got, present := d.Snapshot(); got != nil || present {
		t.Fatalf("default Snapshot should return (nil, false), got (%v, %v)", got, present)
	}
	if err := d.Restore(nil); err != nil {
		t.Fatalf("default Restore should be nil, got %v", err)
	}
	d.OnControl(MeshOsDaemonControl{}) // no panic
	if h := d.Health(); h != HealthHealthy {
		t.Fatalf("default Health = %v, want HealthHealthy", h)
	}
	if s := d.Saturation(); s != 0 {
		t.Fatalf("default Saturation = %v, want 0", s)
	}
}

type fullDaemonImpl struct {
	MeshOsDefaultDaemon
	name string
}

func (d *fullDaemonImpl) Name() string { return d.name }
func (d *fullDaemonImpl) Process(_ MeshOsCausalEvent) ([][]byte, error) {
	return nil, nil
}

// TestMeshOsSdkErrorFormatting verifies the error envelope renders
// every kind/message combination correctly and Unwrap returns the
// sentinel so errors.Is routing works.
func TestMeshOsSdkErrorFormatting(t *testing.T) {
	cases := []struct {
		name     string
		err      *MeshOsSdkError
		wantStr  string
		wantWrap error
	}{
		{
			name:     "kind + message",
			err:      &MeshOsSdkError{Sentinel: ErrMeshOsCallFailed, Kind: "queue_full", Message: "log ring saturated"},
			wantStr:  "meshos: call failed (kind=queue_full): log ring saturated",
			wantWrap: ErrMeshOsCallFailed,
		},
		{
			name:     "kind only",
			err:      &MeshOsSdkError{Sentinel: ErrMeshOsAlreadyShutdown, Kind: "already_shutdown"},
			wantStr:  "meshos: already shutdown (kind=already_shutdown)",
			wantWrap: ErrMeshOsAlreadyShutdown,
		},
		{
			name:     "message only",
			err:      &MeshOsSdkError{Sentinel: ErrMeshOsInvalidArg, Message: "seed length"},
			wantStr:  "meshos: invalid argument: seed length",
			wantWrap: ErrMeshOsInvalidArg,
		},
		{
			name:     "sentinel only",
			err:      &MeshOsSdkError{Sentinel: ErrMeshOsCallFailed},
			wantStr:  "meshos: call failed",
			wantWrap: ErrMeshOsCallFailed,
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if got := tc.err.Error(); got != tc.wantStr {
				t.Errorf("Error() = %q, want %q", got, tc.wantStr)
			}
			if !errors.Is(tc.err, tc.wantWrap) {
				t.Errorf("errors.Is(err, %v) = false, want true", tc.wantWrap)
			}
		})
	}
}

// TestMeshOsSdkErrorNilSafe — calling Error() / Unwrap() on a nil
// receiver shouldn't panic.
func TestMeshOsSdkErrorNilSafe(t *testing.T) {
	var e *MeshOsSdkError
	if got := e.Error(); got != "<nil meshos error>" {
		t.Errorf("nil Error() = %q", got)
	}
	if got := e.Unwrap(); got != nil {
		t.Errorf("nil Unwrap() = %v", got)
	}
}

// TestRegisterDaemonRejectsBadSeed — pure Go-side validation runs
// before any FFI call, so this test works without the cdylib.
func TestRegisterDaemonRejectsBadSeed(t *testing.T) {
	// Nil SDK is invalid arg — should return early without crashing.
	var s *MeshOsDaemonSdk
	_, err := s.RegisterDaemon("name", make([]byte, 32))
	if !errors.Is(err, ErrMeshOsInvalidArg) {
		t.Errorf("nil SDK should return ErrMeshOsInvalidArg, got %v", err)
	}

	// The seed-length check needs a live SDK; it is covered by
	// TestRegisterDaemonWithCallbacksRejectsBadInputs.
}

// TestRegisterDaemonWithCallbacksRejectsBadInputs covers the
// validation gauntlet against a live SDK: nil daemon, wrong seed length,
// empty name. It used to skip for want of an SDK pointer, and no live test
// covered these paths either (cubic review, PR #1165).
func TestRegisterDaemonWithCallbacksRejectsBadInputs(t *testing.T) {
	sdk, err := StartMeshOsDaemonSdk(MeshOsConfig{
		ThisNode:            1,
		TickIntervalMs:      50,
		EventQueueCapacity:  16,
		ActionQueueCapacity: 16,
		ControlCapacity:     16,
	})
	if err != nil {
		t.Fatalf("StartMeshOsDaemonSdk: %v", err)
	}
	defer sdk.Free()
	defer sdk.Shutdown()

	seed := make([]byte, 32)
	if _, err := sdk.RegisterDaemonWithCallbacks(nil, seed); !errors.Is(err, ErrMeshOsInvalidArg) {
		t.Errorf("nil daemon: want ErrMeshOsInvalidArg, got %v", err)
	}
	if _, err := sdk.RegisterDaemonWithCallbacks(namedTestDaemon{name: "short-seed"}, seed[:31]); !errors.Is(err, ErrMeshOsInvalidArg) {
		t.Errorf("31-byte seed: want ErrMeshOsInvalidArg, got %v", err)
	}
	if _, err := sdk.RegisterDaemonWithCallbacks(namedTestDaemon{name: ""}, seed); !errors.Is(err, ErrMeshOsInvalidArg) {
		t.Errorf("empty Name(): want ErrMeshOsInvalidArg, got %v", err)
	}
	if _, err := sdk.RegisterDaemon("short-seed", seed[:31]); !errors.Is(err, ErrMeshOsInvalidArg) {
		t.Errorf("RegisterDaemon with a 31-byte seed: want ErrMeshOsInvalidArg, got %v", err)
	}
	// The same SDK still accepts a valid registration afterwards.
	h, err := sdk.RegisterDaemonWithCallbacks(namedTestDaemon{name: "valid"}, seed)
	if err != nil {
		t.Fatalf("valid registration after the refusals: %v", err)
	}
	h.Free()
}

type namedTestDaemon struct {
	MeshOsDefaultDaemon
	name string
}

func (d namedTestDaemon) Name() string { return d.name }

func (namedTestDaemon) Process(MeshOsCausalEvent) ([][]byte, error) { return nil, nil }

// TestMeshosControlFromC sanity-checks the C → Go projection.
func TestMeshosControlFromC(t *testing.T) {
	// We can't easily construct a C.NetMeshOsDaemonControl from pure
	// Go test code, but the function is a trivial field copy. Sanity
	// check that the discriminators line up with the FFI constants.
	for _, kind := range []DaemonControlKind{
		ControlNone, ControlShutdown, ControlDrainStart, ControlDrainFinish,
		ControlBackpressureOn, ControlBackpressureOff, ControlUnknown,
	} {
		// Just verify the constants are unique + non-negative.
		if kind < 0 {
			t.Errorf("DaemonControlKind %d should be non-negative", kind)
		}
	}
	kinds := []DaemonControlKind{
		ControlNone, ControlShutdown, ControlDrainStart, ControlDrainFinish,
		ControlBackpressureOn, ControlBackpressureOff, ControlUnknown,
	}
	seen := map[DaemonControlKind]bool{}
	for _, k := range kinds {
		if seen[k] {
			t.Errorf("DaemonControlKind %d not unique", k)
		}
		seen[k] = true
	}
}

// TestMeshOsLogLevelsAreOrdered confirms the log-level constants are
// monotone — a regression that flipped two values would be caught here.
func TestMeshOsLogLevelsAreOrdered(t *testing.T) {
	if !(LogTrace < LogDebug && LogDebug < LogInfo && LogInfo < LogWarn && LogWarn < LogError) {
		t.Errorf("log level ordering broken: trace=%d debug=%d info=%d warn=%d error=%d",
			LogTrace, LogDebug, LogInfo, LogWarn, LogError)
	}
}

// TestHandleNilSafety — every public method on a nil *MeshOsDaemonHandle
// should return cleanly (no panic) so consumers don't have to defensively
// check the pointer at every call site.
func TestHandleNilSafety(t *testing.T) {
	var h *MeshOsDaemonHandle
	if id := h.DaemonID(); id != 0 {
		t.Errorf("nil DaemonID() = %d", id)
	}
	if name := h.DaemonName(); name != "" {
		t.Errorf("nil DaemonName() = %q", name)
	}
	if _, err := h.TryNextControl(); err == nil {
		t.Errorf("nil TryNextControl should error")
	}
	if _, err := h.NextControl(0); err == nil {
		t.Errorf("nil NextControl should error")
	}
	if err := h.PublishLog(LogInfo, "x"); err == nil {
		t.Errorf("nil PublishLog should error")
	}
	if err := h.GracefulShutdown(0); err == nil {
		t.Errorf("nil GracefulShutdown should error")
	}
	if _, err := h.Metadata(); err == nil {
		t.Errorf("nil Metadata should error")
	}
	if _, err := h.RefreshMetadata(); err == nil {
		t.Errorf("nil RefreshMetadata should error")
	}
	if err := h.PublishCapabilities(nil); err == nil {
		t.Errorf("nil PublishCapabilities should error")
	}
	// Free on nil should be a no-op (not panic).
	h.Free()
}
