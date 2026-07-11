package net

import "testing"

// The stage-5 stats parity pin: TraversalStats returns the full
// snapshot shape via the v2 FFI call, and every field boots to
// zero / false / empty on a fresh node. Field-by-name assertions
// mirror the Rust SDK's pre_classification_state_is_unknown and
// the Node / Python shape tests — a core-snapshot field that
// stops being forwarded fails here.
func TestTraversalStatsFullShapeBootsZero(t *testing.T) {
	aAddr, _ := allocPortPair(t)
	m, err := NewMeshNode(MeshConfig{BindAddr: aAddr, PskHex: meshPsk})
	if err != nil {
		t.Fatalf("new: %v", err)
	}
	defer func() {
		if err := m.Shutdown(); err != nil {
			t.Fatalf("shutdown: %v", err)
		}
	}()

	stats, err := m.TraversalStats()
	if err != nil {
		t.Fatalf("traversal_stats: %v", err)
	}
	if stats.PunchesAttempted != 0 {
		t.Errorf("PunchesAttempted = %d, want 0", stats.PunchesAttempted)
	}
	if stats.PunchesSucceeded != 0 {
		t.Errorf("PunchesSucceeded = %d, want 0", stats.PunchesSucceeded)
	}
	if stats.PunchesFailed != 0 {
		t.Errorf("PunchesFailed = %d, want 0", stats.PunchesFailed)
	}
	if stats.RelayFallbacks != 0 {
		t.Errorf("RelayFallbacks = %d, want 0", stats.RelayFallbacks)
	}
	if stats.PunchTimeouts != 0 {
		t.Errorf("PunchTimeouts = %d, want 0", stats.PunchTimeouts)
	}
	if stats.PunchRejections != 0 {
		t.Errorf("PunchRejections = %d, want 0", stats.PunchRejections)
	}
	if stats.RendezvousNoRelay != 0 {
		t.Errorf("RendezvousNoRelay = %d, want 0", stats.RendezvousNoRelay)
	}
	if stats.UpgradesAttempted != 0 {
		t.Errorf("UpgradesAttempted = %d, want 0", stats.UpgradesAttempted)
	}
	if stats.UpgradesSucceeded != 0 {
		t.Errorf("UpgradesSucceeded = %d, want 0", stats.UpgradesSucceeded)
	}
	if stats.UpgradesDeferredBusy != 0 {
		t.Errorf("UpgradesDeferredBusy = %d, want 0", stats.UpgradesDeferredBusy)
	}
	if stats.PortMappingActive {
		t.Errorf("PortMappingActive = true, want false")
	}
	if stats.PortMappingExternal != "" {
		t.Errorf("PortMappingExternal = %q, want empty", stats.PortMappingExternal)
	}
	if stats.PortMappingRenewals != 0 {
		t.Errorf("PortMappingRenewals = %d, want 0", stats.PortMappingRenewals)
	}
}

// The AutoDirectUpgrade config flag round-trips through the JSON
// config into the cdylib without erroring (behavioral coverage of
// the upgrade itself lives in the Rust integration suite — this
// pins the binding-surface plumbing).
func TestAutoDirectUpgradeFlagAccepted(t *testing.T) {
	aAddr, _ := allocPortPair(t)
	m, err := NewMeshNode(MeshConfig{
		BindAddr:          aAddr,
		PskHex:            meshPsk,
		AutoDirectUpgrade: true,
	})
	if err != nil {
		t.Fatalf("new with AutoDirectUpgrade: %v", err)
	}
	if err := m.Shutdown(); err != nil {
		t.Fatalf("shutdown: %v", err)
	}
}
