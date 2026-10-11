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

from net_sdk.meshos import MeshOsDaemonHandleWrapper, MeshOsSdkError


class _AlreadyShutdown(MeshOsSdkError):  # type: ignore[misc,valid-type]
    kind = "already_shutdown"


class _FakeRaw:
    """Mirrors the native handle's consumption rule: once
    ``graceful_shutdown`` / ``close`` ran, the control channel is gone
    and every call raises ``already_shutdown`` -- an event queued by the
    one-shot ``graceful_shutdown`` is unreachable (code review
    2026-10-11, PY-4: the previous fake kept the handle readable, so the
    test passed against behaviour the native handle never had)."""

    daemon_id = 1
    daemon_name = "fake"

    def __init__(self) -> None:
        self.events: list[dict] = []
        self.consumed = False
        self.closed_at: float | None = None

    def _live(self) -> None:
        if self.consumed:
            raise _AlreadyShutdown("consumed")

    def next_control(self, timeout_ms=None):
        # Like the native call: blocks (up to timeout) until an event.
        self._live()
        end = None if timeout_ms is None else time.monotonic() + timeout_ms / 1000.0
        while True:
            if self.events:
                return self.events.pop(0)
            if end is not None and time.monotonic() >= end:
                return None
            time.sleep(0.005)

    def try_next_control(self):
        self._live()
        return self.events.pop(0) if self.events else None

    def control_closed(self) -> bool:
        return self.consumed

    def graceful_shutdown(self, grace_ms=None):
        # Native: `inner.take()` first, then route + park.
        self._live()
        self.consumed = True
        time.sleep((grace_ms or 0) / 1000.0)

    def signal_shutdown(self, grace_ms=None):
        self._live()
        self.events.append({"kind": "Shutdown"})

    @property
    def registered(self) -> bool:
        return not self.consumed

    def close(self) -> None:
        if not self.consumed:
            self.consumed = True
            self.closed_at = time.monotonic()


class _LegacyRaw(_FakeRaw):
    """A wheel without the split shutdown."""

    signal_shutdown = None  # type: ignore[assignment]


def test_a_parked_daemon_receives_shutdown_during_the_grace_period() -> None:
    raw = _FakeRaw()
    h = MeshOsDaemonHandleWrapper(raw)  # type: ignore[arg-type]
    got: list = []
    t = threading.Thread(target=lambda: got.append(h.next_control()), daemon=True)
    t.start()
    time.sleep(0.1)  # parked, no event yet
    start = time.monotonic()
    sd = threading.Thread(target=lambda: h.graceful_shutdown(grace_ms=400), daemon=True)
    sd.start()
    t.join(timeout=3)
    assert got == [{"kind": "Shutdown"}]
    # The event arrived while the grace period was still running.
    assert raw.closed_at is None or raw.closed_at - start > 0.3
    sd.join(timeout=3)
    assert not sd.is_alive(), "graceful_shutdown blocked behind next_control()"
    assert raw.consumed


def test_the_daemon_exiting_ends_the_grace_period_early() -> None:
    raw = _FakeRaw()
    h = MeshOsDaemonHandleWrapper(raw)  # type: ignore[arg-type]

    def daemon() -> None:
        with h:  # its own exit while a shutdown is waiting
            assert h.next_control() == {"kind": "Shutdown"}

    t = threading.Thread(target=daemon, daemon=True)
    t.start()
    time.sleep(0.1)
    start = time.monotonic()
    h.graceful_shutdown(grace_ms=5000)
    assert time.monotonic() - start < 2.0, "waited out the full grace period"
    t.join(timeout=3)
    assert not t.is_alive()


def test_a_wheel_without_signal_shutdown_still_shuts_down() -> None:
    raw = _LegacyRaw()
    h = MeshOsDaemonHandleWrapper(raw)  # type: ignore[arg-type]
    h.graceful_shutdown(grace_ms=0)
    assert raw.consumed


def test_next_control_with_zero_timeout_returns_a_queued_event() -> None:
    # PY-3: the slicing loop returned None before any native call.
    raw = _FakeRaw()
    raw.events.append({"kind": "Drain"})
    h = MeshOsDaemonHandleWrapper(raw)  # type: ignore[arg-type]
    assert h.next_control(timeout_ms=0) == {"kind": "Drain"}
    raw.events.append({"kind": "Drain"})
    assert h.next_control(timeout_ms=1) == {"kind": "Drain"}
    assert h.next_control(timeout_ms=0) is None


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
