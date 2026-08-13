"""Shared test fixtures for the Python bindings test suite.

Consolidates the `_next_port` / `PSK` / `_mesh_pair` helpers that
were copy-pasted across test_compute.py, test_groups.py,
test_capability_aggregation_e2e.py, and test_async_interop.py (P12).

Existing per-file helpers keep working — the fixtures are additive.
New tests should use the fixtures here; old tests can migrate
opportunistically.
"""

from __future__ import annotations

import threading
import time

import pytest

# 32-byte hex pre-shared key shared across the test suite — same
# value the per-file helpers used.
PSK = "42" * 32

@pytest.fixture
def next_port() -> "function":  # type: ignore[name-defined]
    """Yields a bind address the OS picks a port for.

    Use as::

        def test_thing(next_port):
            addr = next_port()
            ...

    Port ``0`` asks the kernel for a free ephemeral port. This used to
    hand out fixed numbers from ``itertools.count(30_000)``, with the
    offset chosen to sit above the per-file counters in
    ``test_compute.py`` and ``test_async_interop.py`` — which kept the
    suite from colliding with *itself*, and did nothing about anything
    else on the machine. CI hit exactly that: ``EADDRINUSE`` on a port
    no test in this repository claims.

    A caller that has to dial the resulting node reads ``local_addr``
    back after construction; ``0`` is not a connect target.
    """

    def _allocator() -> str:
        return "127.0.0.1:0"

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
    # 200 ms heartbeat — matches the Rust integration tests'
    # `with_heartbeat_interval`. The default (5 s) is fine for
    # production but makes short tests race on every gossip-bound
    # state-sync (channel membership, cap-index, etc.).
    #
    # permissive_channels=True — matches the Rust default of NOT
    # installing a `ChannelConfigRegistry` on the MeshNode. With
    # the strict registry (the Python default), nrpc tests fail
    # with UnknownChannel because reply-channel names are dynamic
    # per-caller-origin and can't be pre-registered.
    a = NetMesh(
        bind_addr=a_addr,
        psk=PSK,
        heartbeat_interval_ms=200,
        permissive_channels=True,
    )
    b = NetMesh(
        bind_addr=b_addr,
        psk=PSK,
        heartbeat_interval_ms=200,
        permissive_channels=True,
    )

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
    # `b_addr` is `127.0.0.1:0`; the port the kernel actually chose is
    # only knowable after construction.
    a.connect(b.local_addr, b.public_key, b.node_id)
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
