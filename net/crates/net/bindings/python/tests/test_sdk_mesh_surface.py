"""The rest of ``net_sdk.MeshNode``'s surface, live (`PYTHON_SDK_WRAPPER_PARITY_PLAN.md`
S4): identity, ``rpc()``, capability aggregation, tools, blob and directory
transfer, connectivity. Before S4 each of these was reachable only through
``MeshNode._native``; ``net.tool.list_tools(node)`` failed outright on an
SDK node, because it calls ``node.list_tools()``.

Imports ``net_sdk`` from the in-repo source when the wrapper isn't
installed (CI's main run); it never skips for that.
"""

from __future__ import annotations

import importlib
import importlib.util
import sys
import threading
import time
from pathlib import Path

import pytest

pytest.importorskip("net._net")

from net import Identity, Redex, RpcNoRouteError  # noqa: E402

SDK_SRC = Path(__file__).resolve().parents[3] / "sdk-py" / "src"
PSK = "42" * 32


def _sdk(module: str):
    # Fall back to the checkout only when `net_sdk` is not installed at
    # all. An ImportError raised *inside* an installed package is a real
    # failure and must surface, not be papered over with source.
    if importlib.util.find_spec("net_sdk") is None:
        sys.path.insert(0, str(SDK_SRC))
    return importlib.import_module(module)


@pytest.fixture
def pair():
    """Two connected, started SDK nodes, ``(a, b)``, with the DEFAULT strict
    channel registry. nRPC works without `permissive_channels`, and this
    fixture pins that: the docs once said otherwise (2026-10-01)."""
    net_sdk = _sdk("net_sdk")
    opts = dict(heartbeat_interval_ms=200)
    a = net_sdk.MeshNode("127.0.0.1:0", PSK, **opts)
    b = net_sdk.MeshNode("127.0.0.1:0", PSK, **opts)
    errors: list[Exception] = []

    def _accept() -> None:
        try:
            b.accept(a.node_id)
        except Exception as e:  # noqa: BLE001
            errors.append(e)

    t = threading.Thread(target=_accept, daemon=True)
    t.start()
    time.sleep(0.05)
    a.connect(b.local_addr, b.public_key, b.node_id)
    t.join(timeout=5)
    assert not t.is_alive()
    if errors:
        raise errors[0]
    a.start()
    b.start()
    try:
        yield a, b
    finally:
        a.shutdown()
        b.shutdown()


def test_entity_id_matches_the_seeded_identity() -> None:
    net_sdk = _sdk("net_sdk")
    seed = b"\x07" * 32
    node = net_sdk.MeshNode("127.0.0.1:0", PSK, identity_seed=seed)
    try:
        assert node.entity_id == Identity.from_seed(seed).entity_id
    finally:
        node.shutdown()


def test_rpc_round_trips_a_typed_unary_call(pair) -> None:
    a, b = pair
    server = b.rpc()
    handle = server.serve("sdk.sum", lambda req: {"sum": req["x"] + req["y"]})
    try:
        client = a.rpc()
        deadline = time.time() + 3.0
        while True:
            try:
                reply = client.call(b.node_id, "sdk.sum", {"x": 2, "y": 3})
                break
            except RpcNoRouteError:
                if time.time() >= deadline:
                    raise
                time.sleep(0.3)
        assert reply == {"sum": 5}
    finally:
        handle.close()


def test_capability_aggregation_returns_typed_rows() -> None:
    net_sdk = _sdk("net_sdk")
    agg = _sdk("net_sdk.capability_aggregation")
    node = net_sdk.MeshNode("127.0.0.1:0", PSK)
    try:
        native = node._native
        if not hasattr(native, "_test_inject_synthetic_peer_with_tags"):
            pytest.fail("wheel lacks the groups-feature test hook this test needs")
        native._test_inject_synthetic_peer_with_tags(
            0xA, ["hardware.gpu", "hardware.gpu.count=8", "scope:region:us-east"]
        )
        native._test_inject_synthetic_peer_with_tags(
            0xB, ["hardware.gpu", "hardware.gpu.count=4", "scope:region:us-east"]
        )
        native._test_inject_synthetic_peer_with_tags(
            0xC, ["hardware.gpu", "hardware.gpu.count=2", "scope:region:us-west"]
        )
        rows = node.capability_aggregate(
            None, agg.GroupByCls.region(), agg.AggregationCls.count()
        )
        assert all(isinstance(r, agg.AggregateRow) for r in rows)
        by_bucket = {r.bucket: r.value for r in rows}
        assert by_bucket["us-east"] == 2
        assert by_bucket["us-west"] == 1

        ranking = node.capability_capacity_ranking(
            agg.CapacityQuery(group_by=agg.GroupByCls.region())
        )
        assert ranking and all(isinstance(r, agg.CapacityRow) for r in ranking)
        assert {r.bucket for r in ranking} >= {"us-east", "us-west"}
    finally:
        node.shutdown()


def test_list_tools_works_on_an_sdk_node() -> None:
    net_sdk = _sdk("net_sdk")
    node = net_sdk.MeshNode("127.0.0.1:0", PSK)
    try:
        assert node.list_tools() == []
    finally:
        node.shutdown()


def test_store_dir_then_fetch_dir_across_two_nodes(pair, tmp_path) -> None:
    blob = _sdk("net_sdk.blob")
    a, b = pair
    src = tmp_path / "src"
    (src / "sub").mkdir(parents=True)
    (src / "hello.txt").write_bytes(b"hello")
    (src / "sub" / "data.bin").write_bytes(bytes(range(256)) * 8)

    adapter = blob.MeshBlobAdapter(Redex(), "sdk-dir-test-b")
    b.serve_blob_transfer(adapter)
    # The fetching node needs the transfer engine too (core
    # `transfer_fetch_chunk` refuses without one), as in tests/dir_transfer.rs.
    a.serve_blob_transfer(blob.MeshBlobAdapter(Redex(), "sdk-dir-test-a"))
    manifest = b.store_dir(adapter, str(src))
    assert isinstance(manifest, blob.BlobRef)

    dest = tmp_path / "dest"
    files, nbytes = a.fetch_dir(b.node_id, manifest, str(dest))
    assert files == 2
    assert nbytes == 5 + 256 * 8
    assert (dest / "hello.txt").read_bytes() == b"hello"
    assert (dest / "sub" / "data.bin").read_bytes() == bytes(range(256)) * 8


def test_connectivity_counters(pair) -> None:
    a, _ = pair
    assert isinstance(a.discovered_nodes(), int)
