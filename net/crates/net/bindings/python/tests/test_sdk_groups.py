"""``net_sdk.groups`` live: groups built on an SDK runtime over an SDK node.

`PYTHON_SDK_WRAPPER_PARITY_PLAN.md` S3. The wheel's group constructors
take only the wheel's ``DaemonRuntime``; the SDK classes accept the
``net_sdk.compute.DaemonRuntime`` wrapper (or the raw one). Group
behaviour itself is covered by ``test_groups.py``.

Imports ``net_sdk`` from the in-repo source when the wrapper isn't
installed (CI's main run); it never skips for that.
"""

from __future__ import annotations

import importlib
import sys
from pathlib import Path

import pytest

pytest.importorskip("net._net")

SDK_SRC = Path(__file__).resolve().parents[3] / "sdk-py" / "src"
PSK = "42" * 32


def _sdk(module: str):
    try:
        return importlib.import_module(module)
    except ImportError:
        sys.path.insert(0, str(SDK_SRC))
        return importlib.import_module(module)


class Echo:
    def process(self, event) -> list[bytes]:
        return [event.payload]


class Counter:
    """Stateful: a standby group syncs its members by snapshot."""

    def __init__(self) -> None:
        self.count = 0

    def process(self, event) -> list[bytes]:
        self.count += 1
        return []

    def snapshot(self) -> bytes:
        return self.count.to_bytes(4, "little")

    def restore(self, state: bytes) -> None:
        self.count = int.from_bytes(state, "little")


@pytest.fixture
def env():
    net_sdk = _sdk("net_sdk")
    compute = _sdk("net_sdk.compute")
    groups = _sdk("net_sdk.groups")
    node = net_sdk.MeshNode("127.0.0.1:0", PSK)
    # Placement spreads members across nodes; give it candidates. Test-only
    # hook, reached through the native handle on purpose.
    for i in range(1, 5):
        node._native._test_inject_synthetic_peer(0x1000_0000_0000_0000 + i)
    rt = compute.DaemonRuntime(node)
    rt.register_factory("echo", Echo)
    rt.register_factory("counter", Counter)
    rt.start()
    try:
        yield compute, groups, rt
    finally:
        rt.shutdown()
        node.shutdown()


def test_a_replica_group_routes_to_replicas_that_deliver(env) -> None:
    compute, groups, rt = env
    group = groups.ReplicaGroup.spawn(rt, "echo", 3, b"\x11" * 32, "consistent-hash")
    assert group.replica_count == 3
    assert group.health["status"] == "healthy"
    live = {m["origin_hash"] for m in group.replicas}
    assert len(live) == 3

    for i in range(10):
        target = group.route_event({"routing_key": f"user-{i}"})
        assert target in live
        out = rt.deliver(target, compute.CausalEvent(target, i + 1, b"ping"))
        assert out == [b"ping"]


def test_a_fork_group_has_verifiable_lineage(env) -> None:
    _, groups, rt = env
    group = groups.ForkGroup.fork(rt, "echo", 0xABCD_EF01, 42, 3, "round-robin")
    assert group.verify_lineage() is True
    assert group.fork_count == 3
    assert len(group.fork_records) == 3
    assert {r["fork_seq"] for r in group.fork_records} == {42}


def test_a_standby_group_promotes_a_new_active(env) -> None:
    _, groups, rt = env
    group = groups.StandbyGroup.spawn(rt, "counter", 3, b"\x22" * 32)
    first = group.active_origin
    assert group.member_role(0) == "active"
    group.sync_standbys()
    assert group.promote() != first
    assert group.active_origin != first


def test_an_unknown_kind_raises_a_classified_group_error(env) -> None:
    _, groups, rt = env
    with pytest.raises(groups.GroupError) as exc_info:
        groups.ReplicaGroup.spawn(rt, "never-registered", 2, b"\x33" * 32, "random")
    assert groups.group_error_kind(exc_info.value) == "factory-not-found"


def test_groups_accept_the_raw_native_runtime_too(env) -> None:
    _, groups, rt = env
    group = groups.ReplicaGroup.spawn(rt.native, "echo", 2, b"\x44" * 32, "round-robin")
    assert group.replica_count == 2
    with pytest.raises(TypeError):
        groups.ReplicaGroup.spawn("not a runtime", "echo", 2, b"\x44" * 32, "random")
