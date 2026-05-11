// Tests for the capability announcement + filter surface (Stage G-2).
//
// Each node self-indexes its own announcement, so single-node
// round-trip covers the dict→core conversion plus filter predicate
// end-to-end. Multi-node propagation is covered by the Rust
// integration suite (`tests/capability_broadcast.rs`).

package net

import (
	"slices"
	"testing"
)

func newMeshForCaps(t *testing.T) *MeshNode {
	t.Helper()
	addr := reserveLocalUDPPort(t)
	m, err := NewMeshNode(MeshConfig{BindAddr: addr, PskHex: meshPsk})
	if err != nil {
		t.Fatalf("new mesh: %v", err)
	}
	return m
}

// ---------------------------------------------------------------------------
// Self-match round-trip
// ---------------------------------------------------------------------------

func TestAnnounce_ThenFind_SelfMatchesOnTag(t *testing.T) {
	m := newMeshForCaps(t)
	defer m.Shutdown()

	if err := m.AnnounceCapabilities(CapabilitySet{Tags: []string{"gpu", "prod"}}); err != nil {
		t.Fatalf("announce: %v", err)
	}
	peers, err := m.FindNodes(CapabilityFilter{RequireTags: []string{"gpu"}})
	if err != nil {
		t.Fatalf("find_nodes: %v", err)
	}
	if !slices.Contains(peers, m.NodeID()) {
		t.Fatalf("own node id missing from find_nodes result: %v", peers)
	}
}

func TestFindNodes_EmptyWhenFilterMismatches(t *testing.T) {
	m := newMeshForCaps(t)
	defer m.Shutdown()

	if err := m.AnnounceCapabilities(CapabilitySet{Tags: []string{"cpu"}}); err != nil {
		t.Fatalf("announce: %v", err)
	}
	peers, err := m.FindNodes(CapabilityFilter{RequireTags: []string{"gpu"}})
	if err != nil {
		t.Fatalf("find_nodes: %v", err)
	}
	if len(peers) != 0 {
		t.Fatalf("expected empty peer list, got %v", peers)
	}
}

func TestFindNodes_WithoutAnnouncement_IsEmpty(t *testing.T) {
	m := newMeshForCaps(t)
	defer m.Shutdown()

	peers, err := m.FindNodes(CapabilityFilter{RequireTags: []string{"anything"}})
	if err != nil {
		t.Fatalf("find_nodes: %v", err)
	}
	if len(peers) != 0 {
		t.Fatalf("expected empty peer list, got %v", peers)
	}
}

// ---------------------------------------------------------------------------
// Hardware / model / tool filters
// ---------------------------------------------------------------------------

func TestHardwareAndGpuFilter_Matches(t *testing.T) {
	m := newMeshForCaps(t)
	defer m.Shutdown()

	err := m.AnnounceCapabilities(CapabilitySet{
		Hardware: &HardwareCaps{
			CPUCores: 16,
			MemoryGB: 64,
			GPU:      &GPUInfo{Vendor: "nvidia", Model: "h100", VRAMGB: 80},
		},
		Tags: []string{"gpu"},
	})
	if err != nil {
		t.Fatalf("announce: %v", err)
	}

	peers, err := m.FindNodes(CapabilityFilter{
		RequireGPU:  true,
		GPUVendor:   "nvidia",
		MinVRAMGB:   40,
		MinMemoryGB: 32,
	})
	if err != nil {
		t.Fatalf("find_nodes: %v", err)
	}
	if !slices.Contains(peers, m.NodeID()) {
		t.Fatalf("own id missing: %v", peers)
	}

	strict, err := m.FindNodes(CapabilityFilter{MinVRAMGB: 200})
	if err != nil {
		t.Fatalf("find_nodes strict: %v", err)
	}
	if len(strict) != 0 {
		t.Fatalf("expected strict filter to miss, got %v", strict)
	}
}

func TestModelAndToolFilter_Matches(t *testing.T) {
	m := newMeshForCaps(t)
	defer m.Shutdown()

	err := m.AnnounceCapabilities(CapabilitySet{
		Models: []ModelCaps{{
			ModelID:       "llama-3.1-70b",
			Family:        "llama",
			ParametersBx10: 700,
			ContextLength: 128_000,
			Modalities:    []string{"text", "code"},
		}},
		Tools: []ToolCaps{{ToolID: "sql_exec", Name: "SQL Exec"}},
	})
	if err != nil {
		t.Fatalf("announce: %v", err)
	}

	peers, err := m.FindNodes(CapabilityFilter{RequireModels: []string{"llama-3.1-70b"}})
	if err != nil || !slices.Contains(peers, m.NodeID()) {
		t.Fatalf("model filter missed own node: err=%v peers=%v", err, peers)
	}
	peers, err = m.FindNodes(CapabilityFilter{RequireTools: []string{"sql_exec"}})
	if err != nil || !slices.Contains(peers, m.NodeID()) {
		t.Fatalf("tool filter missed own node: err=%v peers=%v", err, peers)
	}
	peers, err = m.FindNodes(CapabilityFilter{
		RequireModalities: []string{"code"},
		MinContextLength:  100_000,
	})
	if err != nil || !slices.Contains(peers, m.NodeID()) {
		t.Fatalf("modality filter missed own node: err=%v peers=%v", err, peers)
	}

	missing, err := m.FindNodes(CapabilityFilter{RequireModels: []string{"missing"}})
	if err != nil {
		t.Fatalf("find_nodes missing-model query: %v", err)
	}
	if len(missing) != 0 {
		t.Fatalf("expected no match, got %v", missing)
	}
}

func TestEmptyAnnouncement_StillSelfIndexes(t *testing.T) {
	m := newMeshForCaps(t)
	defer m.Shutdown()

	if err := m.AnnounceCapabilities(CapabilitySet{}); err != nil {
		t.Fatalf("announce: %v", err)
	}
	peers, err := m.FindNodes(CapabilityFilter{})
	if err != nil {
		t.Fatalf("find_nodes: %v", err)
	}
	if !slices.Contains(peers, m.NodeID()) {
		t.Fatalf("expected own node indexed, got %v", peers)
	}
}

// ---------------------------------------------------------------------------
// Vendor normalization helper
// ---------------------------------------------------------------------------

func TestNormalizeGPUVendor(t *testing.T) {
	cases := []struct {
		raw, want string
	}{
		{"NVIDIA", "nvidia"},
		{"Nvidia", "nvidia"},
		{"amd", "amd"},
		{"Apple", "apple"},
		{"qualcomm", "qualcomm"},
		{"intel", "intel"},
		{"bogus", "unknown"},
		{"", "unknown"},
	}
	for _, c := range cases {
		got, err := NormalizeGPUVendor(c.raw)
		if err != nil {
			t.Fatalf("normalize %q: %v", c.raw, err)
		}
		if got != c.want {
			t.Fatalf("normalize %q: want %q, got %q", c.raw, c.want, got)
		}
	}
}

// ---------------------------------------------------------------------------
// Scoped discovery (reserved scope:* tags)
// ---------------------------------------------------------------------------

func TestFindNodesScoped_TenantTagFiltersOutOtherTenants(t *testing.T) {
	m := newMeshForCaps(t)
	defer m.Shutdown()

	// Announce ourselves under tenant `oem-123`.
	caps := CapabilitySet{
		Tags: []string{"model:llama3-70b", "scope:tenant:oem-123"},
	}
	if err := m.AnnounceCapabilities(caps); err != nil {
		t.Fatalf("announce: %v", err)
	}
	filter := CapabilityFilter{RequireTags: []string{"model:llama3-70b"}}

	// Tenant("oem-123"): includes us.
	peers, err := m.FindNodesScoped(filter, ScopeFilter{Kind: "tenant", Tenant: "oem-123"})
	if err != nil {
		t.Fatalf("find_nodes_scoped tenant=oem-123: %v", err)
	}
	if !slices.Contains(peers, m.NodeID()) {
		t.Fatalf("own node missing under tenant=oem-123: %v", peers)
	}

	// Tenant("corp-acme"): excludes us — different tenant.
	peers, err = m.FindNodesScoped(filter, ScopeFilter{Kind: "tenant", Tenant: "corp-acme"})
	if err != nil {
		t.Fatalf("find_nodes_scoped tenant=corp-acme: %v", err)
	}
	if slices.Contains(peers, m.NodeID()) {
		t.Fatalf("own node leaked into tenant=corp-acme: %v", peers)
	}

	// Any: includes us (no SubnetLocal exclusion applies).
	peers, err = m.FindNodesScoped(filter, ScopeFilter{Kind: "any"})
	if err != nil {
		t.Fatalf("find_nodes_scoped any: %v", err)
	}
	if !slices.Contains(peers, m.NodeID()) {
		t.Fatalf("own node missing under any: %v", peers)
	}

	// GlobalOnly: excludes us — we have a tenant tag.
	peers, err = m.FindNodesScoped(filter, ScopeFilter{Kind: "global_only"})
	if err != nil {
		t.Fatalf("find_nodes_scoped global_only: %v", err)
	}
	if slices.Contains(peers, m.NodeID()) {
		t.Fatalf("tenant-tagged node leaked into global_only: %v", peers)
	}
}

func TestFindNodesScoped_GlobalNodeVisibleToTenantQuery(t *testing.T) {
	// A node without any `scope:*` tag is `Global` — visible to
	// tenant queries by design (permissive default; matches the v1
	// behaviour for nodes that don't opt in to scope tagging).
	m := newMeshForCaps(t)
	defer m.Shutdown()

	if err := m.AnnounceCapabilities(CapabilitySet{Tags: []string{"gpu"}}); err != nil {
		t.Fatalf("announce: %v", err)
	}
	peers, err := m.FindNodesScoped(
		CapabilityFilter{RequireTags: []string{"gpu"}},
		ScopeFilter{Kind: "tenant", Tenant: "oem-123"},
	)
	if err != nil {
		t.Fatalf("find_nodes_scoped: %v", err)
	}
	if !slices.Contains(peers, m.NodeID()) {
		t.Fatalf("untagged (Global) node should match tenant query, got %v", peers)
	}
}

// ---------------------------------------------------------------------------
// FindBestNode — scored placement
// ---------------------------------------------------------------------------

func TestFindBestNode_SelfMatchesOnFilter(t *testing.T) {
	m := newMeshForCaps(t)
	defer m.Shutdown()

	if err := m.AnnounceCapabilities(CapabilitySet{
		Hardware: &HardwareCaps{
			MemoryGB: 64,
			GPU:      &GPUInfo{Vendor: "nvidia", Model: "h100", VRAMGB: 80},
		},
		Tags: []string{"gpu"},
	}); err != nil {
		t.Fatalf("announce: %v", err)
	}
	// Filter that matches us; weights aren't load-bearing for a
	// single-candidate set but exercise the scoring path.
	req := CapabilityRequirement{
		Filter:           CapabilityFilter{RequireGPU: true, MinVRAMGB: 40},
		PreferMoreVRAM:   1.0,
		PreferMoreMemory: 0.5,
	}
	nodeID, ok, err := m.FindBestNode(req)
	if err != nil {
		t.Fatalf("find_best_node: %v", err)
	}
	if !ok {
		t.Fatalf("expected a match, got none")
	}
	if nodeID != m.NodeID() {
		t.Fatalf("expected own node id %d, got %d", m.NodeID(), nodeID)
	}
}

func TestFindBestNode_NoMatchReturnsFalseNotError(t *testing.T) {
	m := newMeshForCaps(t)
	defer m.Shutdown()

	if err := m.AnnounceCapabilities(CapabilitySet{Tags: []string{"cpu"}}); err != nil {
		t.Fatalf("announce: %v", err)
	}
	req := CapabilityRequirement{
		Filter: CapabilityFilter{RequireGPU: true},
	}
	nodeID, ok, err := m.FindBestNode(req)
	if err != nil {
		t.Fatalf("find_best_node: %v", err)
	}
	if ok {
		t.Fatalf("expected no match, got node %d", nodeID)
	}
	if nodeID != 0 {
		t.Fatalf("expected nodeID=0 on miss, got %d", nodeID)
	}
}

// Regression: P2 (Cubic) — empty-string sanitization on
// `Tenants` / `Regions` lists. Unsanitized input like `[""]` used
// to flow through to a `Tenants([""])` filter, which matches no
// real tenant and silently narrows results to Global candidates.
// Fix: drop empties; fall back to Any when cleaned list is empty.

func TestFindNodesScoped_TenantsEmptyListFallsBackToAny(t *testing.T) {
	m := newMeshForCaps(t)
	defer m.Shutdown()

	// Tenant-tagged provider — without sanitization, a
	// `tenants: [""]` query would NOT return this node, and
	// would NOT return any Global node either.
	if err := m.AnnounceCapabilities(CapabilitySet{
		Tags: []string{"gpu", "scope:tenant:oem-123"},
	}); err != nil {
		t.Fatalf("announce: %v", err)
	}
	filter := CapabilityFilter{RequireTags: []string{"gpu"}}

	// `[""]` sanitizes to Any → matches own node.
	peers, err := m.FindNodesScoped(filter, ScopeFilter{Kind: "tenants", Tenants: []string{""}})
	if err != nil {
		t.Fatalf("find_nodes_scoped tenants=[\"\"]: %v", err)
	}
	if !slices.Contains(peers, m.NodeID()) {
		t.Fatalf("tenants=[\"\"] must fall back to Any (P2 regression); got %v", peers)
	}

	// `nil` / empty list also falls back to Any.
	peers, err = m.FindNodesScoped(filter, ScopeFilter{Kind: "tenants", Tenants: []string{}})
	if err != nil {
		t.Fatalf("find_nodes_scoped tenants=[]: %v", err)
	}
	if !slices.Contains(peers, m.NodeID()) {
		t.Fatalf("tenants=[] must fall back to Any; got %v", peers)
	}
}

func TestFindNodesScoped_TenantsPartialCleanDropsEmpties(t *testing.T) {
	m := newMeshForCaps(t)
	defer m.Shutdown()

	if err := m.AnnounceCapabilities(CapabilitySet{
		Tags: []string{"gpu", "scope:tenant:oem-123"},
	}); err != nil {
		t.Fatalf("announce: %v", err)
	}
	filter := CapabilityFilter{RequireTags: []string{"gpu"}}

	// `["", "oem-123"]` sanitizes to `Tenants(["oem-123"])`.
	peers, err := m.FindNodesScoped(filter, ScopeFilter{
		Kind:    "tenants",
		Tenants: []string{"", "oem-123"},
	})
	if err != nil {
		t.Fatalf("find_nodes_scoped: %v", err)
	}
	if !slices.Contains(peers, m.NodeID()) {
		t.Fatalf("partial-clean must keep \"oem-123\" filter; got %v", peers)
	}

	// `["", "corp-acme"]` excludes us (different tenant).
	peers, err = m.FindNodesScoped(filter, ScopeFilter{
		Kind:    "tenants",
		Tenants: []string{"", "corp-acme"},
	})
	if err != nil {
		t.Fatalf("find_nodes_scoped: %v", err)
	}
	if slices.Contains(peers, m.NodeID()) {
		t.Fatalf("partial-clean with non-matching tenant must exclude us; got %v", peers)
	}
}

func TestFindNodesScoped_RegionsEmptyListFallsBackToAny(t *testing.T) {
	m := newMeshForCaps(t)
	defer m.Shutdown()

	if err := m.AnnounceCapabilities(CapabilitySet{
		Tags: []string{"relay-capable", "scope:region:eu-west"},
	}); err != nil {
		t.Fatalf("announce: %v", err)
	}
	filter := CapabilityFilter{RequireTags: []string{"relay-capable"}}

	peers, err := m.FindNodesScoped(filter, ScopeFilter{Kind: "regions", Regions: []string{""}})
	if err != nil {
		t.Fatalf("find_nodes_scoped regions=[\"\"]: %v", err)
	}
	if !slices.Contains(peers, m.NodeID()) {
		t.Fatalf("regions=[\"\"] must fall back to Any (P2 regression); got %v", peers)
	}

	peers, err = m.FindNodesScoped(filter, ScopeFilter{Kind: "regions", Regions: []string{}})
	if err != nil {
		t.Fatalf("find_nodes_scoped regions=[]: %v", err)
	}
	if !slices.Contains(peers, m.NodeID()) {
		t.Fatalf("regions=[] must fall back to Any; got %v", peers)
	}
}

func TestFindBestNodeScoped_SelfMatchesUnderTenantScope(t *testing.T) {
	m := newMeshForCaps(t)
	defer m.Shutdown()

	if err := m.AnnounceCapabilities(CapabilitySet{
		Tags: []string{"model:llama3-70b", "scope:tenant:oem-123"},
	}); err != nil {
		t.Fatalf("announce: %v", err)
	}

	req := CapabilityRequirement{
		Filter: CapabilityFilter{RequireTags: []string{"model:llama3-70b"}},
	}

	// Tenant("oem-123") — own node matches.
	nodeID, ok, err := m.FindBestNodeScoped(req, ScopeFilter{Kind: "tenant", Tenant: "oem-123"})
	if err != nil {
		t.Fatalf("find_best_node_scoped tenant=oem-123: %v", err)
	}
	if !ok || nodeID != m.NodeID() {
		t.Fatalf("expected own node id %d under matching tenant, got (%d, %v)",
			m.NodeID(), nodeID, ok)
	}

	// Tenant("corp-acme") — no match (different tenant; tenant-tagged
	// nodes are excluded from non-matching tenant queries).
	nodeID, ok, err = m.FindBestNodeScoped(req, ScopeFilter{Kind: "tenant", Tenant: "corp-acme"})
	if err != nil {
		t.Fatalf("find_best_node_scoped tenant=corp-acme: %v", err)
	}
	if ok {
		t.Fatalf("expected no match under non-matching tenant, got node %d", nodeID)
	}
}

// ---------------------------------------------------------------------------
// Constructor kwargs (capability_gc + signed)
// ---------------------------------------------------------------------------

func TestMeshConfig_AcceptsCapabilityKwargs(t *testing.T) {
	addr := reserveLocalUDPPort(t)
	m, err := NewMeshNode(MeshConfig{
		BindAddr:                  addr,
		PskHex:                    meshPsk,
		CapabilityGCIntervalMs:    120_000,
		RequireSignedCapabilities: true,
	})
	if err != nil {
		t.Fatalf("new mesh with cap kwargs: %v", err)
	}
	defer m.Shutdown()
	if m.NodeID() == 0 {
		t.Fatal("node id unset")
	}
}
