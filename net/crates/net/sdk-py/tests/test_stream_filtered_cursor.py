"""Regression: a filtered subscription must advance past filtered windows.

SDK audit 2026-10-11. When every event a poll fetched was filtered out, the
core still returns a ``next_id`` past that window (and ``has_more=True``).
`EventStream` only adopted ``next_id`` when events came back, so it re-read
the same window forever and a filtered subscription behind a run of
non-matching events never delivered. `TypedChannel.subscribe` always
filters, so a quiet channel on a busy bus was silent.
"""

from __future__ import annotations

import asyncio
from dataclasses import dataclass, field
from typing import List, Optional

from net_sdk.stream import EventStream, SubscribeOpts


@dataclass
class _Resp:
    events: List[str]
    next_id: Optional[str]
    has_more: bool

    def __len__(self) -> int:
        return len(self.events)

    def __iter__(self):
        return iter(self.events)


@dataclass
class _FilteringBus:
    """Three windows of non-matching events, then one match."""

    calls: List[Optional[str]] = field(default_factory=list)

    def poll(self, limit, cursor, filter, ordering):  # noqa: A002 - mirrors the native kwarg
        self.calls.append(cursor)
        if len(self.calls) > 20:
            raise AssertionError(f"stream is stuck re-polling: {self.calls}")
        step = {None: 0, "c1": 1, "c2": 2}.get(cursor)
        if step is None:
            return _Resp(["match"], "c4", False)
        return _Resp([], f"c{step + 1}", True)


def _opts() -> SubscribeOpts:
    return SubscribeOpts(limit=10, filter='{"x":1}', poll_interval=0.0, max_backoff=0.0)


def test_sync_iter_advances_past_filtered_windows() -> None:
    bus = _FilteringBus()
    stream = EventStream(bus, _opts())  # type: ignore[arg-type]
    first = next(iter(stream))
    assert first == "match"
    assert bus.calls == [None, "c1", "c2", "c3"]


def test_async_iter_advances_past_filtered_windows() -> None:
    bus = _FilteringBus()
    stream = EventStream(bus, _opts())  # type: ignore[arg-type]

    async def first():
        async for ev in stream:
            return ev

    assert asyncio.run(first()) == "match"
    assert bus.calls == [None, "c1", "c2", "c3"]


def test_a_stalled_poll_keeps_the_cursor() -> None:
    """``next_id=None`` must not reset pagination to the start."""

    class _Bus:
        def __init__(self) -> None:
            self.calls: List[Optional[str]] = []

        def poll(self, limit, cursor, filter, ordering):  # noqa: A002
            self.calls.append(cursor)
            if len(self.calls) == 1:
                return _Resp(["a"], "k1", False)
            if len(self.calls) == 2:
                return _Resp([], None, False)
            return _Resp(["b"], "k2", False)

    bus = _Bus()
    it = iter(EventStream(bus, _opts()))  # type: ignore[arg-type]
    assert next(it) == "a"
    assert next(it) == "b"
    assert bus.calls == [None, "k1", "k1"]
