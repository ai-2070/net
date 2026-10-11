"""``NetMesh.shutdown()`` while another thread is parked in a blocking call.

SDK audit 2026-10-11. ``shutdown`` / ``__exit__`` took ``&mut self`` on a
non-frozen pyclass, and a blocking ``&self`` method holds its ``PyRef``
across the GIL release — so shutting down a mesh another thread was blocked
in raised ``RuntimeError: Already borrowed``, and a ``with`` block raised on
exit. The node now lives behind a lock and both are ``&self``.
"""

from __future__ import annotations

import threading
import time

import pytest

net = pytest.importorskip("net", reason="net wheel not built")

PSK = "42" * 32


@pytest.mark.parametrize("via_exit", [False, True])
def test_shutdown_while_another_thread_blocks_in_accept(next_port, via_exit) -> None:
    mesh = net.NetMesh(bind_addr=next_port(), psk=PSK, permissive_channels=True)
    parked = threading.Event()

    def blocked() -> None:
        parked.set()
        try:
            mesh.accept(0xDEAD_BEEF)  # nobody will ever handshake
        except Exception:  # noqa: BLE001 - shutdown may fail the accept
            pass

    t = threading.Thread(target=blocked, daemon=True)
    t.start()
    parked.wait(5)
    time.sleep(0.3)  # inside the native accept, GIL released

    if via_exit:
        assert mesh.__exit__(None, None, None) is False
    else:
        mesh.shutdown()
    assert repr(mesh) == "NetMesh(shutdown)"
    mesh.shutdown()  # idempotent
