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

#: Upper bound for a parked `accept` to return: the core handshake budget
#: (3 retries x 5s) plus slack for a loaded CI runner.
_ACCEPT_BUDGET_S = 30


@pytest.mark.parametrize("via_exit", [False, True])
def test_shutdown_while_another_thread_blocks_in_accept(next_port, via_exit) -> None:
    mesh = net.NetMesh(bind_addr=next_port(), psk=PSK, permissive_channels=True)
    entering = threading.Event()
    returned = threading.Event()

    def blocked() -> None:
        entering.set()
        try:
            mesh.accept(0xDEAD_BEEF)  # nobody will ever handshake
        except Exception:  # noqa: BLE001 - shutdown may fail the accept
            pass
        finally:
            returned.set()

    t = threading.Thread(target=blocked, daemon=True)
    t.start()
    assert entering.wait(5), "accept thread never started"
    time.sleep(0.3)  # let it get inside the native accept, GIL released
    # Proof it is parked in the blocking call (a fast-failing accept would
    # make the rest of the test vacuous): nobody handshakes, so it must
    # still be inside `accept`.
    assert not returned.is_set(), "accept returned before shutdown; nothing was blocked"
    assert t.is_alive()

    if via_exit:
        assert mesh.__exit__(None, None, None) is False
    else:
        mesh.shutdown()
    assert repr(mesh) == "NetMesh(shutdown)"
    mesh.shutdown()  # idempotent

    # The parked caller must come back after shutdown, not hang forever.
    # NOTE: core `MeshNode::accept` does not observe shutdown today — it
    # returns only when its handshake budget runs out (default
    # `handshake_retries` 3 x `handshake_timeout` 5s = ~15s, not
    # configurable from Python), so the bound is that budget plus slack
    # rather than "promptly".
    t.join(timeout=_ACCEPT_BUDGET_S)
    assert not t.is_alive(), "accept stayed blocked after NetMesh.shutdown()"
    assert returned.is_set()
