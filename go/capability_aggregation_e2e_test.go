// End-to-end smoke for the Phase 6c capability-aggregation surface.
//
// Builds a single MeshNode, primes its capability fold via
// `TestInjectSyntheticPeerWithTags`, then exercises both
// `CapabilityAggregate` and `CapabilityCapacityRanking` through
// the C ABI. Asserts the same bucketed output the Rust E2E suite
// at `tests/capability_aggregation_e2e.rs` pins.
//
// Requires the `test_helpers` Go build tag AND the `test-helpers`
// cargo feature on `net-compute-ffi` so the FFI symbol
// `net_compute_test_inject_synthetic_peer_with_tags` is exported.
//
// Run:
//   go test -tags test_helpers ./...

//go:build test_helpers

package net

import (
	"encoding/hex"
	"fmt"
	"sync/atomic"
	"testing"
)

var aggPortCounter int32 = 31_700

func aggNextPort() string {
	p := atomic.AddInt32(&aggPortCounter, 1)
	return fmt.Sprintf("127.0.0.1:%d", p)
}

// primedAggMesh builds a MeshNode with three synthetic publishers
// covering two regions / two GPU types — matches the Rust E2E
// fixture so assertions stay in sync.
func primedAggMesh(t *testing.T) *MeshNode {
	t.Helper()
	mesh, err := NewMeshNode(MeshConfig{
		BindAddr: aggNextPort(),
		PskHex:   hex.EncodeToString(psk32(0x42)),
	})
	if err != nil {
		t.Fatalf("NewMeshNode: %v", err)
	}
	t.Cleanup(func() { _ = mesh.Shutdown() })

	TestInjectSyntheticPeerWithTags(mesh, 0xA, []string{
		"hardware.gpu",
		"hardware.gpu.h100",
		"hardware.gpu.count=8",
		"software.python=3.11",
		"scope:region:us-east",
	})
	TestInjectSyntheticPeerWithTags(mesh, 0xB, []string{
		"hardware.gpu",
		"hardware.gpu.h100",
		"hardware.gpu.count=4",
		"software.python=3.12",
		"scope:region:us-east",
	})
	TestInjectSyntheticPeerWithTags(mesh, 0xC, []string{
		"hardware.gpu",
		"hardware.gpu.a100",
		"hardware.gpu.count=2",
		"software.python=3.11",
		"scope:region:us-west",
	})
	return mesh
}

// psk32 returns a 32-byte PSK filled with `b`.
func psk32(b byte) []byte {
	out := make([]byte, 32)
	for i := range out {
		out[i] = b
	}
	return out
}

// ──────────────────────────────────────────────────────────────────
// CapabilityAggregate
// ──────────────────────────────────────────────────────────────────

func TestCapabilityAggregateE2E_CountByRegion(t *testing.T) {
	mesh := primedAggMesh(t)
	rows, err := mesh.CapabilityAggregate(nil, GroupByRegion(), AggCount())
	if err != nil {
		t.Fatalf("CapabilityAggregate: %v", err)
	}
	byBucket := map[string]uint64{}
	for _, r := range rows {
		byBucket[r.Bucket] = r.Value
	}
	if byBucket["us-east"] != 2 {
		t.Errorf("us-east: got %d, want 2", byBucket["us-east"])
	}
	if byBucket["us-west"] != 1 {
		t.Errorf("us-west: got %d, want 1", byBucket["us-west"])
	}
}

func TestCapabilityAggregateE2E_TagStemBucketing(t *testing.T) {
	mesh := primedAggMesh(t)
	matcher := MatchPrefix("hardware.gpu")
	rows, err := mesh.CapabilityAggregate(
		&matcher,
		GroupByTagStem("hardware.gpu"),
		AggCount(),
	)
	if err != nil {
		t.Fatalf("CapabilityAggregate: %v", err)
	}
	byBucket := map[string]uint64{}
	for _, r := range rows {
		byBucket[r.Bucket] = r.Value
	}
	if byBucket["h100"] != 2 {
		t.Errorf("h100: got %d, want 2", byBucket["h100"])
	}
	if byBucket["a100"] != 1 {
		t.Errorf("a100: got %d, want 1", byBucket["a100"])
	}
	if byBucket["count"] != 3 {
		t.Errorf("count: got %d, want 3", byBucket["count"])
	}
}

func TestCapabilityAggregateE2E_SumNumericTag(t *testing.T) {
	mesh := primedAggMesh(t)
	rows, err := mesh.CapabilityAggregate(
		nil,
		GroupByRegion(),
		AggSumNumericTag("hardware.gpu.count"),
	)
	if err != nil {
		t.Fatalf("CapabilityAggregate: %v", err)
	}
	byBucket := map[string]uint64{}
	for _, r := range rows {
		byBucket[r.Bucket] = r.Value
	}
	if byBucket["us-east"] != 12 {
		t.Errorf("us-east summed: got %d, want 12", byBucket["us-east"])
	}
	if byBucket["us-west"] != 2 {
		t.Errorf("us-west summed: got %d, want 2", byBucket["us-west"])
	}
}

// ──────────────────────────────────────────────────────────────────
// CapabilityCapacityRanking
// ──────────────────────────────────────────────────────────────────

func TestCapabilityCapacityRankingE2E_StateBreakdownWithSummedCapacity(t *testing.T) {
	mesh := primedAggMesh(t)
	rows, err := mesh.CapabilityCapacityRanking(
		CapacityQuery{
			GroupBy:    GroupByRegion(),
			SumAxisKey: "hardware.gpu.count",
		},
		nil,
	)
	if err != nil {
		t.Fatalf("CapabilityCapacityRanking: %v", err)
	}
	if len(rows) != 2 {
		t.Fatalf("rows: got %d, want 2", len(rows))
	}
	// Sorted by Available desc.
	if rows[0].Bucket != "us-east" {
		t.Errorf("rows[0].Bucket: got %s, want us-east", rows[0].Bucket)
	}
	if rows[0].Available != 2 {
		t.Errorf("us-east available: got %d, want 2", rows[0].Available)
	}
	if rows[0].SummedCapacity == nil || *rows[0].SummedCapacity != 12 {
		t.Errorf("us-east summed_capacity: got %v, want 12",
			rows[0].SummedCapacity)
	}
	if rows[1].Bucket != "us-west" {
		t.Errorf("rows[1].Bucket: got %s, want us-west", rows[1].Bucket)
	}
	if rows[1].Available != 1 {
		t.Errorf("us-west available: got %d, want 1", rows[1].Available)
	}
}

func TestCapabilityCapacityRankingE2E_RTTFilterDropsUnknownPublishers(t *testing.T) {
	mesh := primedAggMesh(t)
	maxRtt := uint32(50)
	rttMap := map[uint64]uint32{0xA: 10}
	rows, err := mesh.CapabilityCapacityRanking(
		CapacityQuery{
			GroupBy:  GroupByRegion(),
			MaxRTTMs: &maxRtt,
		},
		rttMap,
	)
	if err != nil {
		t.Fatalf("CapabilityCapacityRanking: %v", err)
	}
	// Only 0xA (us-east) has a known RTT; 0xB + 0xC drop fail-closed.
	if len(rows) != 1 {
		t.Fatalf("rows: got %d, want 1", len(rows))
	}
	if rows[0].Bucket != "us-east" {
		t.Errorf("bucket: got %s, want us-east", rows[0].Bucket)
	}
	if rows[0].Available != 1 {
		t.Errorf("available: got %d, want 1", rows[0].Available)
	}
}

func TestCapabilityCapacityRankingE2E_LimitTruncates(t *testing.T) {
	mesh := primedAggMesh(t)
	rows, err := mesh.CapabilityCapacityRanking(
		CapacityQuery{
			GroupBy: GroupByRegion(),
			Limit:   1,
		},
		nil,
	)
	if err != nil {
		t.Fatalf("CapabilityCapacityRanking: %v", err)
	}
	if len(rows) != 1 {
		t.Fatalf("rows: got %d, want 1", len(rows))
	}
	if rows[0].Bucket != "us-east" {
		t.Errorf("bucket: got %s, want us-east", rows[0].Bucket)
	}
}
