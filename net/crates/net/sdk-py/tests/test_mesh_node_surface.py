"""Forwarding tests for the S4 ``MeshNode`` surface
(`PYTHON_SDK_WRAPPER_PARITY_PLAN.md` S4), against the stubbed extension
(``conftest.py``). Behaviour is witnessed live in
``bindings/python/tests/test_sdk_mesh_surface.py``.
"""

from __future__ import annotations

from unittest.mock import MagicMock

import pytest

import net_sdk.mesh as mesh_mod


@pytest.fixture
def node(monkeypatch: pytest.MonkeyPatch):
    native = MagicMock(name="NetMesh-instance")
    monkeypatch.setattr(mesh_mod, "_NetMesh", MagicMock(return_value=native))
    return mesh_mod.MeshNode("127.0.0.1:0", "00" * 32), native


# (method, args, native method, expected native args) — one row per
# straight forward. The blob/tool/rpc/aggregation methods go through
# module-level helpers and are covered below.
ROWS = [
    ("discovered_nodes", (), "discovered_nodes", ()),
    ("traversal_stats", (), "traversal_stats", ()),
    ("connect_direct", (5, "ab" * 32, 9), "connect_direct", (5, "ab" * 32, 9)),
    ("connect_direct_auto", (5, "ab" * 32), "connect_direct_auto", (5, "ab" * 32)),
]


@pytest.mark.parametrize("method,args,native_method,native_args", ROWS)
def test_direct_forwards(node, method, args, native_method, native_args) -> None:
    mesh, native = node
    getattr(mesh, method)(*args)
    getattr(native, native_method).assert_called_once_with(*native_args)


def test_entity_id_reads_the_native_property(node) -> None:
    mesh, native = node
    native.entity_id = b"\x01" * 32
    assert mesh.entity_id == b"\x01" * 32


def test_aggregation_encodes_dataclasses_and_types_rows(node) -> None:
    from net_sdk import capability_aggregation as agg

    mesh, native = node
    native.capability_aggregate.return_value = [{"bucket": "us-east", "value": 2}]
    rows = mesh.capability_aggregate(None, agg.GroupByCls.region(), agg.AggregationCls.count())
    args = native.capability_aggregate.call_args.args
    assert args[0] is None
    assert args[1] == agg.group_by_to_json(agg.GroupByCls.region())
    assert args[2] == agg.aggregation_to_json(agg.AggregationCls.count())
    assert rows == [agg.AggregateRow(bucket="us-east", value=2)]

    native.capability_capacity_ranking.return_value = [
        {"bucket": "b", "idle": 1, "busy": 0, "reserved": 0, "available": 1,
         "summed_capacity": None}
    ]
    query = agg.CapacityQuery(group_by=agg.GroupByCls.region())
    ranking = mesh.capability_capacity_ranking(query, {7: 12})
    native.capability_capacity_ranking.assert_called_once_with(
        agg.capacity_query_to_json(query), {7: 12}
    )
    assert ranking[0].available == 1


def test_tools_go_through_net_sdk_tool_with_the_native_mesh(node, monkeypatch) -> None:
    from net_sdk import tool

    mesh, native = node
    seen = {}
    monkeypatch.setattr(tool, "list_tools", lambda m: seen.setdefault("list", m) and [])
    monkeypatch.setattr(
        tool, "watch_tools", lambda m, interval=None: seen.setdefault("watch", (m, interval))
    )
    mesh.list_tools()
    mesh.watch_tools(interval=0.5)
    assert seen["list"] is native
    assert seen["watch"] == (native, 0.5)
