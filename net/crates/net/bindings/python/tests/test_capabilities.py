"""Tests for the capability-announcement + filter surface (Stage F-2).

Each node self-indexes its own announcement, so the single-node
roundtrip is a full contract test for the dict→core conversion plus
the filter predicate. Multi-node propagation is covered by the Rust
integration suite (`tests/capability_broadcast.rs`).
"""

from __future__ import annotations

import pytest

from net import NetMesh, normalize_gpu_vendor


PSK = "42" * 32


def _port(seed: int) -> str:
    return f"127.0.0.1:{28000 + seed}"


# -------------------------------------------------------------------------
# Self-match round-trip
# -------------------------------------------------------------------------


def test_announce_then_find_self_matches_on_tag() -> None:
    m = NetMesh(_port(1), PSK)
    try:
        m.announce_capabilities({"tags": ["gpu", "prod"]})
        peers = m.find_nodes({"require_tags": ["gpu"]})
        assert m.node_id in peers
    finally:
        m.shutdown()


def test_find_nodes_empty_when_filter_mismatches() -> None:
    m = NetMesh(_port(2), PSK)
    try:
        m.announce_capabilities({"tags": ["cpu"]})
        peers = m.find_nodes({"require_tags": ["gpu"]})
        assert peers == []
    finally:
        m.shutdown()


def test_find_nodes_without_announcement_is_empty() -> None:
    m = NetMesh(_port(3), PSK)
    try:
        peers = m.find_nodes({"require_tags": ["anything"]})
        assert peers == []
    finally:
        m.shutdown()


# -------------------------------------------------------------------------
# Hardware filter round-trip
# -------------------------------------------------------------------------


def test_hardware_and_gpu_filter_matches() -> None:
    m = NetMesh(_port(4), PSK)
    try:
        m.announce_capabilities(
            {
                "hardware": {
                    "cpu_cores": 16,
                    "memory_gb": 64,
                    "gpu": {
                        "vendor": "nvidia",
                        "model": "h100",
                        "vram_gb": 80,
                    },
                },
                "tags": ["gpu"],
            }
        )
        peers = m.find_nodes(
            {
                "require_gpu": True,
                "gpu_vendor": "nvidia",
                "min_vram_gb": 40,
                "min_memory_gb": 32,
            }
        )
        assert m.node_id in peers

        # Too-strict VRAM requirement should reject.
        peers_strict = m.find_nodes({"min_vram_gb": 200})
        assert peers_strict == []
    finally:
        m.shutdown()


def test_model_and_tool_filter_matches() -> None:
    m = NetMesh(_port(5), PSK)
    try:
        m.announce_capabilities(
            {
                "models": [
                    {
                        "model_id": "llama-3.1-70b",
                        "family": "llama",
                        "parameters_b_x10": 700,
                        "context_length": 128_000,
                        "modalities": ["text", "code"],
                    }
                ],
                "tools": [{"tool_id": "sql_exec", "name": "SQL Exec"}],
            }
        )
        assert m.node_id in m.find_nodes(
            {"require_models": ["llama-3.1-70b"]}
        )
        assert m.node_id in m.find_nodes({"require_tools": ["sql_exec"]})
        assert m.node_id in m.find_nodes(
            {"require_modalities": ["code"], "min_context_length": 100_000}
        )
        assert m.find_nodes({"require_models": ["missing"]}) == []
    finally:
        m.shutdown()


def test_empty_announcement_still_self_indexes() -> None:
    m = NetMesh(_port(6), PSK)
    try:
        m.announce_capabilities({})
        # Empty filter matches any announcer in the index.
        peers = m.find_nodes({})
        assert m.node_id in peers
    finally:
        m.shutdown()


# -------------------------------------------------------------------------
# Vendor normalization helper
# -------------------------------------------------------------------------


@pytest.mark.parametrize(
    ("raw", "expected"),
    [
        ("NVIDIA", "nvidia"),
        ("Nvidia", "nvidia"),
        ("amd", "amd"),
        ("Apple", "apple"),
        ("qualcomm", "qualcomm"),
        ("intel", "intel"),
        ("bogus", "unknown"),
        ("", "unknown"),
    ],
)
def test_normalize_gpu_vendor(raw: str, expected: str) -> None:
    assert normalize_gpu_vendor(raw) == expected


# -------------------------------------------------------------------------
# Input validation
# -------------------------------------------------------------------------


def test_announce_rejects_wrong_type_for_hardware() -> None:
    m = NetMesh(_port(7), PSK)
    try:
        with pytest.raises(TypeError):
            m.announce_capabilities({"hardware": "not-a-dict"})
    finally:
        m.shutdown()


def test_find_nodes_rejects_wrong_type_for_require_tags() -> None:
    m = NetMesh(_port(8), PSK)
    try:
        with pytest.raises(TypeError):
            m.find_nodes({"require_tags": "gpu"})  # must be list
    finally:
        m.shutdown()


# -------------------------------------------------------------------------
# Scoped discovery (`scope:*` reserved tags)
# -------------------------------------------------------------------------
#
# The PyO3 layer has unique plumbing — `scope_filter_from_py` parses
# the dict, `with_scope_filter` projects to the borrowed core enum.
# These tests exercise the JS↔Rust boundary end-to-end with a
# single-node self-match; the underlying matching logic is covered
# by the Rust unit + integration suites.


def test_find_nodes_scoped_tenant_self_matches_under_matching_tenant() -> None:
    m = NetMesh(_port(9), PSK)
    try:
        m.announce_capabilities(
            {"tags": ["model:llama3-70b", "scope:tenant:oem-123"]}
        )

        # Matching tenant — self appears.
        peers = m.find_nodes_scoped(
            {"require_tags": ["model:llama3-70b"]},
            {"kind": "tenant", "tenant": "oem-123"},
        )
        assert m.node_id in peers

        # Non-matching tenant — self excluded.
        peers = m.find_nodes_scoped(
            {"require_tags": ["model:llama3-70b"]},
            {"kind": "tenant", "tenant": "corp-acme"},
        )
        assert m.node_id not in peers

        # GlobalOnly — tenant-tagged node also excluded.
        peers = m.find_nodes_scoped(
            {"require_tags": ["model:llama3-70b"]},
            {"kind": "global_only"},
        )
        assert m.node_id not in peers
    finally:
        m.shutdown()


def test_find_nodes_scoped_global_node_visible_to_tenant_query() -> None:
    # Permissive default: an untagged ("Global") node stays
    # discoverable under tenant-scoped queries. Locks in v1
    # backwards-compat through the dict→Rust scope-filter path.
    m = NetMesh(_port(10), PSK)
    try:
        m.announce_capabilities({"tags": ["gpu"]})
        peers = m.find_nodes_scoped(
            {"require_tags": ["gpu"]},
            {"kind": "tenant", "tenant": "oem-123"},
        )
        assert m.node_id in peers
    finally:
        m.shutdown()


def test_find_nodes_scoped_regions_list_marshals_through_pyo3() -> None:
    # Multi-element variants (`tenants` / `regions`) take a separate
    # path in `with_scope_filter` — they need an intermediate
    # `Vec<&str>` whose lifetime outlives the borrow. This test
    # exercises that borrow trampoline end-to-end.
    m = NetMesh(_port(11), PSK)
    try:
        m.announce_capabilities(
            {"tags": ["relay-capable", "scope:region:eu-west"]}
        )

        # Multi-region list including ours — match.
        peers = m.find_nodes_scoped(
            {"require_tags": ["relay-capable"]},
            {"kind": "regions", "regions": ["us-east", "eu-west"]},
        )
        assert m.node_id in peers

        # Multi-region list excluding ours — no match.
        peers = m.find_nodes_scoped(
            {"require_tags": ["relay-capable"]},
            {"kind": "regions", "regions": ["us-east", "ap-south"]},
        )
        assert m.node_id not in peers
    finally:
        m.shutdown()


def test_find_nodes_scoped_camelcase_kinds_accepted() -> None:
    # The PyO3 converter accepts both snake_case (`global_only`,
    # `same_subnet`) and camelCase (`globalOnly`, `sameSubnet`) so
    # cross-binding fixtures (TS uses camelCase) round-trip.
    m = NetMesh(_port(12), PSK)
    try:
        m.announce_capabilities({"tags": ["gpu"]})
        # Untagged node is Global → globalOnly returns it.
        peers = m.find_nodes_scoped(
            {"require_tags": ["gpu"]},
            {"kind": "globalOnly"},
        )
        assert m.node_id in peers
    finally:
        m.shutdown()


# Regression: P2 (Cubic) — empty-string sanitization on `tenants` /
# `regions` lists. Unsanitized input like `[""]` used to flow through
# to a `Tenants([""])` filter, which matches no real tenant and
# silently narrows results to Global candidates. Fix: drop empties;
# fall back to Any when the cleaned list is empty.


def test_find_nodes_scoped_tenants_with_only_empty_strings_falls_back_to_any() -> None:
    m = NetMesh(_port(13), PSK)
    try:
        # Tenant-tagged provider — without sanitization, a
        # `tenants: [""]` query would not return this node and
        # would not return any Global node either.
        m.announce_capabilities({"tags": ["gpu", "scope:tenant:oem-123"]})

        # After sanitization, `tenants: [""]` collapses to Any.
        peers = m.find_nodes_scoped(
            {"require_tags": ["gpu"]},
            {"kind": "tenants", "tenants": [""]},
        )
        assert m.node_id in peers

        # Empty list also falls back to Any.
        peers = m.find_nodes_scoped(
            {"require_tags": ["gpu"]},
            {"kind": "tenants", "tenants": []},
        )
        assert m.node_id in peers
    finally:
        m.shutdown()


def test_find_nodes_scoped_tenants_partial_clean_drops_empties() -> None:
    m = NetMesh(_port(14), PSK)
    try:
        m.announce_capabilities({"tags": ["gpu", "scope:tenant:oem-123"]})

        # `["", "oem-123"]` sanitizes to `Tenants(["oem-123"])`
        # — real tenant semantics preserved, empty silently
        # dropped.
        peers = m.find_nodes_scoped(
            {"require_tags": ["gpu"]},
            {"kind": "tenants", "tenants": ["", "oem-123"]},
        )
        assert m.node_id in peers

        # `["", "corp-acme"]` excludes us (not our tenant).
        peers = m.find_nodes_scoped(
            {"require_tags": ["gpu"]},
            {"kind": "tenants", "tenants": ["", "corp-acme"]},
        )
        assert m.node_id not in peers
    finally:
        m.shutdown()


def test_find_nodes_scoped_regions_with_only_empty_strings_falls_back_to_any() -> None:
    m = NetMesh(_port(15), PSK)
    try:
        m.announce_capabilities(
            {"tags": ["relay-capable", "scope:region:eu-west"]}
        )

        peers = m.find_nodes_scoped(
            {"require_tags": ["relay-capable"]},
            {"kind": "regions", "regions": [""]},
        )
        assert m.node_id in peers

        peers = m.find_nodes_scoped(
            {"require_tags": ["relay-capable"]},
            {"kind": "regions", "regions": []},
        )
        assert m.node_id in peers
    finally:
        m.shutdown()
