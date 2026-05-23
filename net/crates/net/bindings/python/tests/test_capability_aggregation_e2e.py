"""End-to-end smoke for the Phase 6c capability-aggregation surface.

Builds a single NetMesh, primes its capability fold via
`_test_inject_synthetic_peer_with_tags`, then exercises both
`capability_aggregate` and `capability_capacity_ranking` through the
PyO3 boundary. Asserts the same bucketed output the Rust E2E suite
at `tests/capability_aggregation_e2e.rs` pins.

Requires the `groups` cargo feature for the synthetic-peer staging
helper (CI's `maturin develop` flags already include it).
"""

from __future__ import annotations

import itertools
import json

import pytest

try:
    from net import NetMesh
except ImportError as err:
    pytestmark = pytest.mark.skip(
        reason=f"net bindings not importable ({err})"
    )

PSK = "42" * 32

_port_counter = itertools.count(31_500)


def _next_port() -> str:
    return f"127.0.0.1:{next(_port_counter)}"


def _primed_mesh() -> NetMesh:
    """Build a NetMesh with three synthetic publishers covering two
    regions / two GPU types, matching the Rust E2E fixture so the
    assertions stay in sync."""
    mesh = NetMesh(bind_addr=_next_port(), psk=PSK)
    # Synthetic publishers; the inject helper takes raw canonical
    # tag strings including reserved-prefix `scope:region:*`.
    if not hasattr(mesh, "_test_inject_synthetic_peer_with_tags"):
        pytest.skip(
            "net bindings built without the `groups` feature; the "
            "_test_inject_synthetic_peer_with_tags helper is not "
            "exported. Rebuild with `maturin develop --features "
            "net,cortex,compute,groups`."
        )
    mesh._test_inject_synthetic_peer_with_tags(
        0xA,
        [
            "hardware.gpu",
            "hardware.gpu.h100",
            "hardware.gpu.count=8",
            "software.python=3.11",
            "scope:region:us-east",
        ],
    )
    mesh._test_inject_synthetic_peer_with_tags(
        0xB,
        [
            "hardware.gpu",
            "hardware.gpu.h100",
            "hardware.gpu.count=4",
            "software.python=3.12",
            "scope:region:us-east",
        ],
    )
    mesh._test_inject_synthetic_peer_with_tags(
        0xC,
        [
            "hardware.gpu",
            "hardware.gpu.a100",
            "hardware.gpu.count=2",
            "software.python=3.11",
            "scope:region:us-west",
        ],
    )
    return mesh


# ──────────────────────────────────────────────────────────────────
# capability_aggregate
# ──────────────────────────────────────────────────────────────────


def test_aggregate_counts_publishers_per_region():
    mesh = _primed_mesh()
    rows = mesh.capability_aggregate(
        None,
        json.dumps({"kind": "region"}),
        json.dumps({"kind": "count"}),
    )
    by_bucket = {r["bucket"]: r["value"] for r in rows}
    assert by_bucket["us-east"] == 2
    assert by_bucket["us-west"] == 1


def test_aggregate_buckets_by_gpu_tag_stem():
    mesh = _primed_mesh()
    rows = mesh.capability_aggregate(
        json.dumps({"kind": "prefix", "value": "hardware.gpu"}),
        json.dumps({"kind": "tag_stem", "prefix": "hardware.gpu"}),
        json.dumps({"kind": "count"}),
    )
    by_bucket = {r["bucket"]: r["value"] for r in rows}
    assert by_bucket["h100"] == 2
    assert by_bucket["a100"] == 1
    assert by_bucket["count"] == 3


def test_aggregate_sums_numeric_tag_per_region():
    mesh = _primed_mesh()
    rows = mesh.capability_aggregate(
        None,
        json.dumps({"kind": "region"}),
        json.dumps(
            {"kind": "sum_numeric_tag", "axis_key": "hardware.gpu.count"}
        ),
    )
    by_bucket = {r["bucket"]: r["value"] for r in rows}
    assert by_bucket["us-east"] == 12
    assert by_bucket["us-west"] == 2


# ──────────────────────────────────────────────────────────────────
# capability_capacity_ranking
# ──────────────────────────────────────────────────────────────────


def test_capacity_ranking_breaks_down_state_with_summed_capacity():
    mesh = _primed_mesh()
    query = json.dumps(
        {
            "matcher": None,
            "group_by": {"kind": "region"},
            "max_rtt_ms": None,
            "sum_axis_key": "hardware.gpu.count",
            "limit": 0,
        }
    )
    rows = mesh.capability_capacity_ranking(query, None)
    assert len(rows) == 2
    # Sorted by `available` desc.
    assert rows[0]["bucket"] == "us-east"
    assert rows[0]["available"] == 2
    assert rows[0]["summed_capacity"] == 12
    assert rows[1]["bucket"] == "us-west"
    assert rows[1]["available"] == 1
    assert rows[1]["summed_capacity"] == 2


def test_capacity_ranking_rtt_filter_drops_unknown_publishers():
    mesh = _primed_mesh()
    query = json.dumps(
        {
            "matcher": None,
            "group_by": {"kind": "region"},
            "max_rtt_ms": 50,
            "sum_axis_key": None,
            "limit": 0,
        }
    )
    # Only 0xA has a known RTT under the threshold; 0xB and 0xC
    # resolve to None and drop.
    rtt_map = {0xA: 10}
    rows = mesh.capability_capacity_ranking(query, rtt_map)
    assert len(rows) == 1
    assert rows[0]["bucket"] == "us-east"
    assert rows[0]["available"] == 1


def test_capacity_ranking_limit_truncates():
    mesh = _primed_mesh()
    query = json.dumps(
        {
            "matcher": None,
            "group_by": {"kind": "region"},
            "max_rtt_ms": None,
            "sum_axis_key": None,
            "limit": 1,
        }
    )
    rows = mesh.capability_capacity_ranking(query, None)
    assert len(rows) == 1
    assert rows[0]["bucket"] == "us-east"
