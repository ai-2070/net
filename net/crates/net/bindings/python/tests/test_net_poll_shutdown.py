"""``Net.poll`` / ``Net.flush`` racing ``Net.shutdown`` from another thread
must not deadlock (code review 2026-10-11, PY-1).

``poll`` / ``flush`` released the GIL while holding the bus read guard;
``shutdown`` took the write guard while holding the GIL. A poll returning
from its wait then needed the GIL that the waiting shutdown held, while the
shutdown needed the read guard the poll held.
"""

from __future__ import annotations

import threading
import time

import pytest

net = pytest.importorskip("net", reason="net wheel not built")


@pytest.mark.parametrize("op", ["poll", "flush"])
def test_shutdown_while_another_thread_polls(op: str) -> None:
    bus = net.Net(num_shards=2)
    for i in range(64):
        bus.ingest_raw('{"i": %d}' % i)
    stop = threading.Event()
    errors: list[BaseException] = []

    def hammer() -> None:
        while not stop.is_set():
            try:
                if op == "poll":
                    bus.poll(limit=16)
                else:
                    bus.flush()
            except RuntimeError:
                return  # "has been shut down" — the expected ending
            except BaseException as e:  # noqa: BLE001
                errors.append(e)
                return

    workers = [threading.Thread(target=hammer, daemon=True) for _ in range(4)]
    for w in workers:
        w.start()
    time.sleep(0.05)
    shutdown_errors: list[BaseException] = []

    def do_shutdown() -> None:
        try:
            bus.shutdown()
        except BaseException as e:  # noqa: BLE001 - surfaced by the assert below
            shutdown_errors.append(e)

    sd = threading.Thread(target=do_shutdown, daemon=True)
    sd.start()
    sd.join(timeout=10)
    stop.set()
    assert not sd.is_alive(), "Net.shutdown deadlocked against a concurrent " + op
    for w in workers:
        w.join(timeout=10)
        assert not w.is_alive(), op + " deadlocked against Net.shutdown"
    assert not shutdown_errors, shutdown_errors
    assert not errors, errors
    assert repr(bus) == "Net(shutdown)"
