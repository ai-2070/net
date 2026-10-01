"""``net_sdk.compute`` live: daemons built from a ``net_sdk.MeshNode``.

`PYTHON_SDK_WRAPPER_PARITY_PLAN.md` S2. Until S2 there was no SDK compute
module, and the wheel's ``DaemonRuntime`` would not take an SDK node, so
callers passed ``node._native``. Behaviour of the runtime itself is
covered by ``test_compute.py``; this file proves the SDK path reaches it.

Imports ``net_sdk`` from the in-repo source when the wrapper isn't
installed (CI's main run); it never skips for that.
"""

from __future__ import annotations

import importlib
import sys
from pathlib import Path

import pytest

pytest.importorskip("net._net")

from net import Identity, NetMesh  # noqa: E402

SDK_SRC = Path(__file__).resolve().parents[3] / "sdk-py" / "src"
PSK = "42" * 32


def _sdk(module: str = "net_sdk"):
    try:
        return importlib.import_module(module)
    except ImportError:
        sys.path.insert(0, str(SDK_SRC))
        return importlib.import_module(module)


class Echo:
    """Stateless: echoes each payload. Written against the
    ``MeshDaemon`` protocol — nothing to inherit."""

    def process(self, event) -> list[bytes]:
        return [event.payload]


class Counter:
    def __init__(self) -> None:
        self.count = 0

    def process(self, event) -> list[bytes]:
        self.count += 1
        return [self.count.to_bytes(4, "little")]

    def snapshot(self) -> bytes:
        return self.count.to_bytes(4, "little")

    def restore(self, state: bytes) -> None:
        self.count = int.from_bytes(state, "little")


class Boom:
    def process(self, event) -> list[bytes]:
        raise ValueError("daemon exploded")


@pytest.fixture
def runtime():
    compute = _sdk("net_sdk.compute")
    node = _sdk().MeshNode("127.0.0.1:0", PSK)
    rt = compute.DaemonRuntime(node)
    rt.start()
    try:
        yield compute, rt
    finally:
        rt.shutdown()
        node.shutdown()


def test_an_echo_daemon_spawns_from_an_sdk_node_and_delivers(runtime) -> None:
    compute, rt = runtime
    assert rt.is_ready()
    rt.register_factory("echo", Echo)
    ident = Identity.generate()
    handle = rt.spawn("echo", ident, {"max_log_entries": 64})
    assert rt.daemon_count() == 1
    event = compute.CausalEvent(ident.origin_hash, 1, b"hello")
    assert rt.deliver(handle.origin_hash, event) == [b"hello"]
    rt.stop(handle.origin_hash)
    assert rt.daemon_count() == 0


def test_snapshot_then_spawn_from_snapshot_restores_state(runtime) -> None:
    compute, rt = runtime
    rt.register_factory("counter", Counter)
    ident = Identity.generate()
    handle = rt.spawn("counter", ident)
    for seq in (1, 2, 3):
        rt.deliver(handle.origin_hash, compute.CausalEvent(ident.origin_hash, seq, b""))
    state = rt.snapshot(handle.origin_hash)
    # Opaque core `StateSnapshot` bytes (not the daemon's own encoding);
    # only `spawn_from_snapshot` reads them.
    assert isinstance(state, bytes) and state
    rt.stop(handle.origin_hash)

    restored = rt.spawn_from_snapshot("counter", ident, state)
    out = rt.deliver(restored.origin_hash, compute.CausalEvent(ident.origin_hash, 4, b""))
    assert out == [(4).to_bytes(4, "little")]


def test_a_raising_daemon_surfaces_as_daemon_error(runtime) -> None:
    compute, rt = runtime
    rt.register_factory("boom", Boom)
    ident = Identity.generate()
    handle = rt.spawn("boom", ident)
    with pytest.raises(compute.DaemonError):
        rt.deliver(handle.origin_hash, compute.CausalEvent(ident.origin_hash, 1, b""))
    # The host survived: the runtime still answers.
    assert rt.is_ready()


def test_runtime_takes_a_raw_net_mesh_and_rejects_anything_else() -> None:
    compute = _sdk("net_sdk.compute")
    mesh = NetMesh("127.0.0.1:0", PSK)
    try:
        rt = compute.DaemonRuntime(mesh)
        assert not rt.is_ready()
        rt.shutdown()
    finally:
        mesh.shutdown()
    with pytest.raises(TypeError, match="MeshNode"):
        compute.DaemonRuntime("not a mesh")


def test_the_wrapper_exposes_the_native_runtime_for_async_and_groups(runtime) -> None:
    import net

    _, rt = runtime
    assert isinstance(rt.native, net.DaemonRuntime)
    async_rt = net.AsyncDaemonRuntime(rt.native)
    assert async_rt.is_ready()
