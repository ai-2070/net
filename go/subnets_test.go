// Tests for the subnet / subnet-policy kwargs on MeshConfig
// (Stage G-3). Single-node smoke tests: construct succeeds with
// various subnet shapes, input validation rejects out-of-range level
// bytes and malformed policy dicts. Multi-node subnet-local
// visibility enforcement is covered by the Rust integration suite
// (`tests/subnet_*.rs`).

package net

import (
	"bytes"
	"encoding/hex"
	"testing"
)

// ---------------------------------------------------------------------------
// Subnet id kwarg
// ---------------------------------------------------------------------------

func TestMesh_AcceptsSingleLevelSubnet(t *testing.T) {
	addr := reserveLocalUDPPort(t)
	m, err := NewMeshNode(MeshConfig{BindAddr: addr, PskHex: meshPsk, Subnet: []uint32{3}})
	if err != nil {
		t.Fatalf("new mesh: %v", err)
	}
	defer m.Shutdown()
	if m.NodeID() == 0 {
		t.Fatal("node id unset")
	}
}

func TestMesh_AcceptsFourLevelSubnet(t *testing.T) {
	addr := reserveLocalUDPPort(t)
	m, err := NewMeshNode(MeshConfig{
		BindAddr: addr,
		PskHex:   meshPsk,
		Subnet:   []uint32{1, 2, 3, 4},
	})
	if err != nil {
		t.Fatalf("new mesh: %v", err)
	}
	defer m.Shutdown()
}

func TestMesh_NoSubnetKwarg_DefaultsGlobal(t *testing.T) {
	addr := reserveLocalUDPPort(t)
	m, err := NewMeshNode(MeshConfig{BindAddr: addr, PskHex: meshPsk})
	if err != nil {
		t.Fatalf("new mesh: %v", err)
	}
	defer m.Shutdown()
	if m.NodeID() == 0 {
		t.Fatal("node id unset")
	}
}

func TestMesh_SubnetTooManyLevels_Rejected(t *testing.T) {
	addr := reserveLocalUDPPort(t)
	_, err := NewMeshNode(MeshConfig{
		BindAddr: addr,
		PskHex:   meshPsk,
		Subnet:   []uint32{1, 2, 3, 4, 5},
	})
	if err == nil {
		t.Fatal("expected error for 5-level subnet")
	}
}

func TestMesh_SubnetByteOutOfRange_Rejected(t *testing.T) {
	addr := reserveLocalUDPPort(t)
	_, err := NewMeshNode(MeshConfig{
		BindAddr: addr,
		PskHex:   meshPsk,
		Subnet:   []uint32{256},
	})
	if err == nil {
		t.Fatal("expected error for level byte > 255")
	}
}

// ---------------------------------------------------------------------------
// Subnet policy kwarg
// ---------------------------------------------------------------------------

func TestMesh_AcceptsSubnetPolicy(t *testing.T) {
	addr := reserveLocalUDPPort(t)
	policy := &SubnetPolicy{
		Rules: []SubnetRule{
			{
				TagPrefix: "region:",
				Level:     0,
				Values:    map[string]uint32{"eu": 1, "us": 2, "apac": 3},
			},
			{
				TagPrefix: "zone:",
				Level:     1,
				Values:    map[string]uint32{"a": 1, "b": 2, "c": 3},
			},
		},
	}
	m, err := NewMeshNode(MeshConfig{BindAddr: addr, PskHex: meshPsk, SubnetPolicy: policy})
	if err != nil {
		t.Fatalf("new mesh: %v", err)
	}
	defer m.Shutdown()
}

func TestMesh_SubnetPolicyNoRules(t *testing.T) {
	addr := reserveLocalUDPPort(t)
	m, err := NewMeshNode(MeshConfig{
		BindAddr:     addr,
		PskHex:       meshPsk,
		SubnetPolicy: &SubnetPolicy{},
	})
	if err != nil {
		t.Fatalf("new mesh: %v", err)
	}
	defer m.Shutdown()
}

func TestMesh_SubnetPolicyLevelOutOfRange_Rejected(t *testing.T) {
	addr := reserveLocalUDPPort(t)
	_, err := NewMeshNode(MeshConfig{
		BindAddr: addr,
		PskHex:   meshPsk,
		SubnetPolicy: &SubnetPolicy{
			Rules: []SubnetRule{
				{TagPrefix: "region:", Level: 4, Values: map[string]uint32{}},
			},
		},
	})
	if err == nil {
		t.Fatal("expected error for rule level=4")
	}
}

func TestMesh_SubnetPolicyValueOutOfRange_Rejected(t *testing.T) {
	addr := reserveLocalUDPPort(t)
	_, err := NewMeshNode(MeshConfig{
		BindAddr: addr,
		PskHex:   meshPsk,
		SubnetPolicy: &SubnetPolicy{
			Rules: []SubnetRule{
				{TagPrefix: "region:", Level: 0, Values: map[string]uint32{"eu": 512}},
			},
		},
	})
	if err == nil {
		t.Fatal("expected error for rule value > 255")
	}
}

// Regression for a cubic-flagged P1: `SubnetRule::map` in the core
// panics if any value is 0 (0 is reserved for "unmatched / no
// restriction"). A Go caller passing `{"eu": 0}` used to abort
// the cdylib with a Rust panic. The FFI layer now returns a clean
// mesh-init error.
func TestMesh_SubnetPolicyValueZero_Rejected(t *testing.T) {
	addr := reserveLocalUDPPort(t)
	_, err := NewMeshNode(MeshConfig{
		BindAddr: addr,
		PskHex:   meshPsk,
		SubnetPolicy: &SubnetPolicy{
			Rules: []SubnetRule{
				{TagPrefix: "region:", Level: 0, Values: map[string]uint32{"eu": 0}},
			},
		},
	})
	if err == nil {
		t.Fatal("expected error for rule value = 0 (reserved)")
	}
}

// ---------------------------------------------------------------------------
// identity_seed_hex — verifies EntityID round-trip across mesh + identity
// ---------------------------------------------------------------------------

func TestMesh_IdentitySeed_MatchesIdentityFromSeed(t *testing.T) {
	// Deterministic seed so the mesh's entity_id equals what
	// IdentityFromSeed would produce given the same bytes.
	seed := bytes.Repeat([]byte{0x42}, 32)
	expectedID, err := IdentityFromSeed(seed)
	if err != nil {
		t.Fatalf("identity from seed: %v", err)
	}
	defer expectedID.Close()
	wantEID, err := expectedID.EntityID()
	if err != nil {
		t.Fatalf("identity entity id: %v", err)
	}

	addr := reserveLocalUDPPort(t)
	m, err := NewMeshNode(MeshConfig{
		BindAddr:        addr,
		PskHex:          meshPsk,
		IdentitySeedHex: hex.EncodeToString(seed),
	})
	if err != nil {
		t.Fatalf("new mesh w/ seed: %v", err)
	}
	defer m.Shutdown()

	gotEID, err := m.EntityID()
	if err != nil {
		t.Fatalf("mesh entity id: %v", err)
	}
	if !bytes.Equal(gotEID, wantEID) {
		t.Fatalf("mesh entity id mismatch:\n  want %x\n  got  %x", wantEID, gotEID)
	}
	if m.NodeID() != expectedID.NodeID() {
		t.Fatalf("node id mismatch: want %d, got %d", expectedID.NodeID(), m.NodeID())
	}
}

func TestMesh_IdentitySeedHex_BadLength_Rejected(t *testing.T) {
	addr := reserveLocalUDPPort(t)
	_, err := NewMeshNode(MeshConfig{
		BindAddr:        addr,
		PskHex:          meshPsk,
		IdentitySeedHex: "deadbeef", // too short
	})
	if err == nil {
		t.Fatal("expected error for too-short identity seed hex")
	}
}

// ---------------------------------------------------------------------------
// Combined
// ---------------------------------------------------------------------------

func TestMesh_AcceptsFullSecurityConfig(t *testing.T) {
	addr := reserveLocalUDPPort(t)
	seed := bytes.Repeat([]byte{0x01}, 32)
	m, err := NewMeshNode(MeshConfig{
		BindAddr:                  addr,
		PskHex:                    meshPsk,
		IdentitySeedHex:           hex.EncodeToString(seed),
		Subnet:                    []uint32{7, 3},
		SubnetPolicy: &SubnetPolicy{
			Rules: []SubnetRule{{
				TagPrefix: "tier:", Level: 2,
				Values: map[string]uint32{"prod": 1, "dev": 2},
			}},
		},
		RequireSignedCapabilities: &[]bool{true}[0],
		CapabilityGCIntervalMs:    120_000,
	})
	if err != nil {
		t.Fatalf("new mesh with full security cfg: %v", err)
	}
	defer m.Shutdown()

	eid, err := m.EntityID()
	if err != nil {
		t.Fatalf("entity id: %v", err)
	}
	if len(eid) != 32 {
		t.Fatalf("want 32-byte entity id, got %d", len(eid))
	}
}
