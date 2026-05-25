"""Shared test fixtures for the Python bindings test suite.

Consolidates the `_next_port` / `PSK` / `_mesh_pair` helpers that
were copy-pasted across test_compute.py, test_groups.py,
test_capability_aggregation_e2e.py, and test_async_interop.py (P12).

Existing per-file helpers keep working — the fixtures are additive.
New tests should use the fixtures here; old tests can migrate
opportunistically.
"""

from __future__ import annotations

import itertools
import threading
import time

import pytest

# 32-byte hex pre-shared key shared across the test suite — same
# value the per-file helpers used.
PSK = "42" * 32

# Per-test unique ports so repeated runs don't collide on localhost.
# Starting offset is deliberately above the per-file counters used
# in test_compute.py (29_400) and test_async_interop.py (29_700) so
# tests running side-by-side never pick the same port.
_port_counter = itertools.count(30_000)


@pytest.fixture
def next_port() -> "function":  # type: ignore[name-defined]
    """Yields a fresh ``127.0.0.1:N`` address per call.

    Use as::

        def test_thing(next_port):
            addr = next_port()
            ...
    """

    def _allocator() -> str:
        return f"127.0.0.1:{next(_port_counter)}"

    return _allocator


@pytest.fixture
def mesh_pair(next_port):
    """Build two connected NetMesh instances and yield ``(a, b)``.

    Performs the handshake (b.accept on a thread while a.connect
    fires from the main thread), starts both meshes, and yields
    the pair. The fixture's teardown shuts down both meshes.

    Mirrors the pattern that was duplicated across test_compute.py
    (`_mesh_pair`), test_groups.py, test_capability_aggregation_e2e.py,
    and tests/test_async_interop.py. Tests adopting this fixture
    can delete their per-file copies.

    Requires the `net` feature compiled into the wheel; tests using
    this fixture should also `pytest.importorskip("net._net")` if
    they care about gracefully skipping on a thin wheel.
    """
    from net import NetMesh

    a_addr = next_port()
    b_addr = next_port()
    a = NetMesh(bind_addr=a_addr, psk=PSK)
    b = NetMesh(bind_addr=b_addr, psk=PSK)

    errors: list[Exception] = []

    def _accept() -> None:
        try:
            b.accept(a.node_id)
        except Exception as e:  # noqa: BLE001
            errors.append(e)

    t = threading.Thread(target=_accept, daemon=True)
    t.start()
    # Small beat so the accept-side is primed before connect fires.
    time.sleep(0.05)
    a.connect(b_addr, b.public_key, b.node_id)
    t.join(timeout=5)
    if t.is_alive():
        raise RuntimeError(
            "mesh-pair handshake: accept thread still alive after 5 s timeout"
        )
    if errors:
        raise errors[0]
    a.start()
    b.start()

    try:
        yield a, b
    finally:
        a.shutdown()
        b.shutdown()
