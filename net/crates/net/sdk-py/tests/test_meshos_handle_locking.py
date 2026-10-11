"""MeshOS handle wrapper: the borrow lock must not be held across an
unbounded native wait, nor be waited for on the event loop.

SDK audit 2026-10-11:
- ``next_control()`` held the lock across one unbounded native wait, so a
  ``graceful_shutdown()`` from another thread — the call that would deliver
  the ``Shutdown`` event — blocked behind it forever.
- the async paths took the same ``threading.Lock`` on the event-loop thread,
  freezing the loop while another thread held it (``graceful_shutdown``
  holds it for the whole grace period).
- ``anext_control(timeout_ms=None)`` returned ``None`` after 100 ms although
  documented to behave like the sync variant (wait for an event).
"""

from __future__ import annotations

import asyncio
import threading
import time

from net_sdk.meshos import MeshOsDaemonHandleWrapper


class _FakeRaw:
    daemon_id = 1
    daemon_name = "fake"

    def __init__(self) -> None:
        self.events: list[dict] = []
        self.shut = threading.Event()

    def next_control(self, timeout_ms=None):
        # Like the native call: blocks (up to timeout) until an event.
        end = None if timeout_ms is None else time.monotonic() + timeout_ms / 1000.0
        while end is None or time.monotonic() < end:
            if self.events:
                return self.events.pop(0)
            time.sleep(0.005)
        return None

    def try_next_control(self):
        return self.events.pop(0) if self.events else None

    def graceful_shutdown(self, grace_ms=None):
        self.events.append({"kind": "Shutdown"})
        time.sleep((grace_ms or 0) / 1000.0)
        self.shut.set()


def test_graceful_shutdown_is_not_blocked_by_a_parked_next_control() -> None:
    raw = _FakeRaw()
    h = MeshOsDaemonHandleWrapper(raw)  # type: ignore[arg-type]
    got: list = []
    t = threading.Thread(target=lambda: got.append(h.next_control()), daemon=True)
    t.start()
    time.sleep(0.1)  # parked, no event yet
    sd = threading.Thread(target=lambda: h.graceful_shutdown(grace_ms=0), daemon=True)
    sd.start()
    sd.join(timeout=3)
    assert not sd.is_alive(), "graceful_shutdown blocked behind next_control()"
    t.join(timeout=3)
    assert got == [{"kind": "Shutdown"}]


def test_the_event_loop_never_waits_for_the_borrow_lock() -> None:
    raw = _FakeRaw()
    h = MeshOsDaemonHandleWrapper(raw)  # type: ignore[arg-type]

    async def main() -> float:
        # Another thread holds the lock for 800 ms (like a grace period).
        holder = threading.Thread(
            target=lambda: (h._borrow_lock.acquire(), time.sleep(0.8), h._borrow_lock.release()),
            daemon=True,
        )
        holder.start()
        await asyncio.sleep(0.05)
        start = time.monotonic()
        await h.anext_control(timeout_ms=200)
        elapsed = time.monotonic() - start
        holder.join()
        return elapsed

    elapsed = asyncio.run(main())
    # Pre-fix the loop thread blocked on the lock until the holder let go.
    assert elapsed < 0.5, f"anext_control waited {elapsed:.2f}s for another thread's lock"


def test_anext_control_without_timeout_waits_for_an_event() -> None:
    raw = _FakeRaw()
    h = MeshOsDaemonHandleWrapper(raw)  # type: ignore[arg-type]

    async def main():
        async def later() -> None:
            await asyncio.sleep(0.3)
            raw.events.append({"kind": "Drain"})

        asyncio.create_task(later())
        return await asyncio.wait_for(h.anext_control(), timeout=3)

    assert asyncio.run(main()) == {"kind": "Drain"}
