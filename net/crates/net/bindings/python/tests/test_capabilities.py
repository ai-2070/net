"""Tests for the capability-announcement + filter surface (Stage F-2).

Each node self-indexes its own announcement, so the single-node
roundtrip is a full contract test for the dict→core conversion plus
the filter predicate. Multi-node propagation is covered by the Rust
integration suite (`tests/capability_broadcast.rs`).
"""

from __future__ import annotations

import contextlib
import threading
import time
from collections.abc import Iterator

import pytest

from net import AsyncNetMesh, NetMesh, normalize_gpu_vendor


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


def test_find_nodes_scoped_tenants_with_only_empty_strings_raises() -> None:
    # A `tenants` filter that cleans down to nothing carries no tenant
    # identity to narrow by. It used to collapse to `Any` — the BROADEST
    # filter — so a caller whose tenant id arrived empty silently
    # queried the whole mesh and picked a provider from it. Narrowing
    # filters that cannot narrow now raise.
    m = NetMesh(_port(13), PSK)
    try:
        m.announce_capabilities({"tags": ["gpu", "scope:tenant:oem-123"]})

        for tenants in ([""], [], ["", ""]):
            with pytest.raises(ValueError):
                m.find_nodes_scoped(
                    {"require_tags": ["gpu"]},
                    {"kind": "tenants", "tenants": tenants},
                )
    finally:
        m.shutdown()


def test_find_nodes_scoped_empty_tenant_and_unknown_kind_raise() -> None:
    # Same rule on the single-selector kinds, and on a kind typo — the
    # unknown-`kind` fallthrough was previously documented as
    # "defensive" but resolved to `Any`.
    m = NetMesh(_port(16), PSK)
    try:
        m.announce_capabilities({"tags": ["gpu", "scope:tenant:oem-123"]})

        for scope in (
            {"kind": "tenant", "tenant": ""},
            {"kind": "tenant"},
            {"kind": "region", "region": ""},
            {"kind": "tenat", "tenant": "oem-123"},
        ):
            with pytest.raises(ValueError):
                m.find_nodes_scoped({"require_tags": ["gpu"]}, scope)
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


def test_find_nodes_scoped_regions_with_only_empty_strings_raises() -> None:
    # Regions mirror tenants: an all-empty list cannot narrow, so it
    # raises rather than widening to Any.
    m = NetMesh(_port(15), PSK)
    try:
        m.announce_capabilities(
            {"tags": ["relay-capable", "scope:region:eu-west"]}
        )

        for regions in ([""], []):
            with pytest.raises(ValueError):
                m.find_nodes_scoped(
                    {"require_tags": ["relay-capable"]},
                    {"kind": "regions", "regions": regions},
                )
    finally:
        m.shutdown()


# -------------------------------------------------------------------------
# Single-winner discovery (`find_best_node` / `find_best_node_scoped`)
# -------------------------------------------------------------------------
#
# Scoring semantics live in the substrate and are pinned there by the
# four inverse witnesses in `capability_bridge.rs`. What these tests
# own is the Python side of the boundary: the dict conversion, the
# refusal of weights that cannot be clamped meaningfully, and that a
# weight set from Python still reaches the scorer and moves the winner.


def _gpu_caps(vram_gb: int, model: str) -> dict:
    return {
        "hardware": {"gpu": {"vendor": "nvidia", "model": model, "vram_gb": vram_gb}},
        "tags": ["gpu-pool"],
    }


@contextlib.contextmanager
def _vram_pool(seed: int) -> Iterator[tuple[NetMesh, int, int]]:
    """One querier connected to two announcing peers.

    Yields ``(querier, low_id, high_id)`` where the peer holding the
    HIGHER node id was given the BIGGER GPU. Node ids derive from a
    fresh keypair per node, so which peer sorts first is not knowable
    until runtime — hence the sort. Without it, a run where the strong
    peer happened to hold the lower id would pass even with a dead
    weight, because the lowest id is exactly what an unweighted query
    returns.

    The querier announces nothing, so it never self-indexes and the
    only candidates are the two peers.
    """
    q = NetMesh(_port(seed), PSK, heartbeat_interval_ms=200)
    p1 = NetMesh(_port(seed + 1), PSK, heartbeat_interval_ms=200)
    p2 = NetMesh(_port(seed + 2), PSK, heartbeat_interval_ms=200)
    try:
        for peer, addr in ((p1, _port(seed + 1)), (p2, _port(seed + 2))):
            errors: list[Exception] = []

            def _accept(peer: NetMesh = peer, errors: list = errors) -> None:
                try:
                    peer.accept(q.node_id)
                except Exception as e:  # noqa: BLE001
                    errors.append(e)

            t = threading.Thread(target=_accept, daemon=True)
            t.start()
            time.sleep(0.05)
            q.connect(addr, peer.public_key, peer.node_id)
            t.join(timeout=5)
            if errors:
                raise errors[0]
        q.start()
        p1.start()
        p2.start()

        low, high = sorted((p1, p2), key=lambda m: m.node_id)
        low.announce_capabilities(_gpu_caps(8, "weak"))
        high.announce_capabilities(_gpu_caps(80, "strong"))

        # Both announcements must be visible before a winner means
        # anything: a query that has seen only one peer returns that
        # peer whatever the weights say.
        deadline = time.monotonic() + 5.0
        while time.monotonic() < deadline:
            if len(q.find_nodes({"require_tags": ["gpu-pool"]})) == 2:
                break
            time.sleep(0.025)
        else:
            raise AssertionError("both announcements did not arrive within 5 s")

        yield q, low.node_id, high.node_id
    finally:
        for m in (q, p1, p2):
            m.shutdown()


def test_find_best_node_prefers_more_vram_over_lower_node_id() -> None:
    with _vram_pool(40) as (q, low_id, high_id):
        pool = {"filter": {"require_tags": ["gpu-pool"]}}
        # Unweighted: every match scores the same, so the tie-break
        # decides. This half is what makes the next assertion mean
        # something.
        assert q.find_best_node(pool) == low_id
        # Same fold, same candidates — only the weight changes.
        assert q.find_best_node({**pool, "prefer_more_vram": 1.0}) == high_id


def test_find_best_node_clamps_finite_out_of_range_weights() -> None:
    with _vram_pool(45) as (q, low_id, high_id):
        pool = {"filter": {"require_tags": ["gpu-pool"]}}
        # 5.0 clamps to 1.0 in the substrate and selects like a full
        # weight rather than raising — one clamp contract shared with
        # Rust, Go, C and Node. -1.0 clamps to 0.0, "don't consult".
        assert q.find_best_node({**pool, "prefer_more_vram": 5.0}) == high_id
        assert q.find_best_node({**pool, "prefer_more_vram": -1.0}) == low_id
        # An int is as natural a literal here as a float.
        assert q.find_best_node({**pool, "prefer_more_vram": 1}) == high_id


def test_find_best_node_scoped_filters_before_scoring() -> None:
    m = NetMesh(_port(50), PSK)
    try:
        m.announce_capabilities(
            {
                "hardware": {
                    "gpu": {"vendor": "nvidia", "model": "h100", "vram_gb": 80}
                },
                "tags": ["gpu-pool", "scope:tenant:oem-123"],
            }
        )
        req = {"filter": {"require_tags": ["gpu-pool"]}, "prefer_more_vram": 1.0}
        assert (
            m.find_best_node_scoped(req, {"kind": "tenant", "tenant": "oem-123"})
            == m.node_id
        )
        # Out of scope: excluded before scoring, so its 80 GB GPU
        # cannot buy it back in.
        assert (
            m.find_best_node_scoped(req, {"kind": "tenant", "tenant": "corp-acme"})
            is None
        )
    finally:
        m.shutdown()


def test_find_best_node_returns_none_when_nothing_matches() -> None:
    m = NetMesh(_port(52), PSK)
    try:
        # `None` means no match. A node id of 0 is a real id, so
        # callers must test against `None` rather than truthiness —
        # which is why this returns `None` and not 0.
        assert m.find_best_node({"filter": {"require_tags": ["gpu"]}}) is None
        m.announce_capabilities({"tags": ["gpu"]})
        assert m.find_best_node({"filter": {"require_tags": ["gpu"]}}) == m.node_id
    finally:
        m.shutdown()


def test_find_best_node_defaults_missing_filter_and_weights() -> None:
    m = NetMesh(_port(54), PSK)
    try:
        m.announce_capabilities({"tags": ["gpu"]})
        # An empty requirement is valid: match everything, prefer
        # nothing. Same defaults the C ABI's JSON DTO applies.
        assert m.find_best_node({}) == m.node_id
    finally:
        m.shutdown()


def test_find_best_node_rejects_non_finite_weights() -> None:
    m = NetMesh(_port(56), PSK)
    try:
        m.announce_capabilities({"tags": ["gpu"]})
        req = {"filter": {"require_tags": ["gpu"]}}
        axes = (
            "prefer_more_memory",
            "prefer_more_vram",
            "prefer_faster_inference",
            "prefer_loaded_models",
        )
        # `nan` survives clamping and then loses every score
        # comparison, so a weighted requirement would silently select
        # as if unweighted. `inf` clamps to a value the caller never
        # wrote. Both are the wrong VALUE, hence ValueError.
        for axis in axes:
            for bad in (float("nan"), float("inf"), float("-inf")):
                with pytest.raises(ValueError):
                    m.find_best_node({**req, axis: bad})
    finally:
        m.shutdown()


def test_find_best_node_rejects_wrong_types() -> None:
    m = NetMesh(_port(58), PSK)
    try:
        # A non-numeric weight and a non-dict filter are the wrong
        # TYPE, distinct from the ValueError a non-finite number
        # raises. Neither may be silently ignored: a dropped filter
        # would widen the query to the whole mesh.
        with pytest.raises(TypeError):
            m.find_best_node({"prefer_more_vram": "1.0"})
        with pytest.raises(TypeError):
            m.find_best_node({"filter": "require_tags"})
    finally:
        m.shutdown()


def test_async_mesh_exposes_synchronous_local_discovery() -> None:
    m = NetMesh(_port(60), PSK)
    try:
        am = AsyncNetMesh(m)
        m.announce_capabilities(
            {
                "hardware": {
                    "gpu": {"vendor": "nvidia", "model": "h100", "vram_gb": 80}
                },
                "tags": ["gpu-pool", "scope:tenant:oem-123"],
            }
        )
        tags = {"require_tags": ["gpu-pool"]}
        tenant = {"kind": "tenant", "tenant": "oem-123"}
        req = {"filter": tags, "prefer_more_vram": 1.0}

        # All four read the local fold and return the value directly.
        # They are NOT awaitable — awaiting a plain list or int raises
        # TypeError, so returning the value IS the contract.
        assert am.find_nodes(tags) == [m.node_id]
        assert am.find_nodes_scoped(tags, tenant) == [m.node_id]
        assert am.find_best_node(req) == m.node_id
        assert am.find_best_node_scoped(req, tenant) == m.node_id
        assert am.find_best_node_scoped(req, {"kind": "tenant", "tenant": "x"}) is None
    finally:
        m.shutdown()
