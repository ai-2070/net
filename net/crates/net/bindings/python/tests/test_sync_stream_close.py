"""Sync stream ``close()`` must stop a reader parked in another thread.

SDK audit 2026-10-11. ``RpcStream.__next__`` takes its stream out of the
mutex for the blocking wait; ``close()`` then found nothing to close, sent no
CANCEL, and the reader put the stream back when the next chunk arrived — the
call came back to life. ``close()`` now latches, wakes the parked reader
(iteration ends) and a closed stream is never put back.
"""

from __future__ import annotations

import threading
import time

import pytest

net = pytest.importorskip("net", reason="net wheel not built")


def test_close_ends_iteration_parked_in_another_thread(mesh_pair) -> None:
    a, b = mesh_pair
    server = net.MeshRpc(b)
    client = net.MeshRpc(a)
    release = threading.Event()

    def silent(_req: bytes, _sink) -> None:
        release.wait(10)  # never sends a chunk while the test runs

    handle = server.serve_streaming("audit.silent.sync", silent, 30_000)
    try:
        time.sleep(0.3)
        stream = client.call_streaming(b.node_id, "audit.silent.sync", b"x")
        outcome: list = []

        def reader() -> None:
            try:
                for chunk in stream:
                    outcome.append(("chunk", chunk))
                outcome.append(("done", None))
            except Exception as e:  # noqa: BLE001 - recorded for the assert
                outcome.append(("error", e))

        t = threading.Thread(target=reader, daemon=True)
        t.start()
        time.sleep(0.5)  # parked in native next()
        stream.close()
        t.join(timeout=5)
        assert not t.is_alive(), "close() did not wake a reader parked in __next__"
        assert outcome == [("done", None)], outcome
    finally:
        release.set()
        handle.close() if hasattr(handle, "close") else None
