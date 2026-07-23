"""Streaming event consumption — generators and async generators."""

from __future__ import annotations

import asyncio
import time
from dataclasses import dataclass
from typing import AsyncIterator, Callable, Generic, Iterator, Optional, TypeVar

from net import Net, StoredEvent

T = TypeVar("T")

DEFAULT_LIMIT = 100
# Starting backoff for idle polls and the inter-poll wait on
# partial-batch responses. `0.001` (1 ms) had us re-issue an FFI poll
# hundreds of times per second on a near-drained stream that returns
# 1-2 events per call; `0.005` (5 ms) keeps us at a 200/s ceiling on
# that path while still feeling instant. Saturated streams (full
# `limit` batches) skip the sleep entirely and continue to drain at
# full speed.
DEFAULT_POLL_INTERVAL = 0.005  # 5ms
DEFAULT_MAX_BACKOFF = 0.1  # 100ms


@dataclass
class SubscribeOpts:
    """Options for subscribing to events."""

    limit: int = DEFAULT_LIMIT
    filter: Optional[str] = None
    ordering: Optional[str] = None
    poll_interval: float = DEFAULT_POLL_INTERVAL
    max_backoff: float = DEFAULT_MAX_BACKOFF
    timeout: Optional[float] = None


class EventStream:
    """
    A polled event stream — supports both synchronous and asynchronous
    iteration over the same instance API.

    Polls the bus with adaptive backoff — tight loop when events flow
    in saturated batches, exponential backoff when idle, and a small
    inter-poll sleep on partial-batch (drained) responses to avoid
    spamming FFI calls on trickle workloads.

    Sync example:
        >>> for event in node.subscribe(limit=100):
        ...     print(event.raw)

    Async example:
        >>> async for event in node.subscribe(limit=100):
        ...     print(event.raw)

    Pick one mode per stream instance — interleaving sync and async
    iteration on the same instance is undefined.
    """

    def __init__(self, bus: Net, opts: Optional[SubscribeOpts] = None) -> None:
        self._bus = bus
        self._opts = opts or SubscribeOpts()
        self._cursor: Optional[str] = None
        self._stopped = False

    def stop(self) -> None:
        """Stop the stream."""
        self._stopped = True

    def _poll(self):
        return self._bus.poll(
            limit=self._opts.limit,
            cursor=self._cursor,
            filter=self._opts.filter,
            ordering=self._opts.ordering,
        )

    def __iter__(self) -> Iterator[StoredEvent]:
        # Clamp the user-provided knobs to non-negative locals once,
        # so every `time.sleep` site below is safe regardless of what
        # the caller put in `SubscribeOpts`. `time.sleep` raises
        # `ValueError` on negative input; clamping at the boundary
        # keeps the loop correct against misconfiguration.
        poll_interval = max(0.0, self._opts.poll_interval)
        max_backoff = max(0.0, self._opts.max_backoff)
        backoff = poll_interval
        start = time.monotonic()

        while not self._stopped:
            if self._opts.timeout is not None:
                elapsed = time.monotonic() - start
                if elapsed >= self._opts.timeout:
                    return

            response = self._poll()

            if len(response) > 0:
                backoff = poll_interval
                self._cursor = response.next_id

                for event in response:
                    yield event

                # Partial-batch sleep: a poll that returned fewer than
                # `limit` events has drained (or nearly drained) the
                # bus; re-issuing immediately would just spam FFI
                # calls for trickle streams. A full-batch response
                # means more events are queued, so we skip the sleep
                # and loop tight to keep up.
                if len(response) < self._opts.limit:
                    time.sleep(poll_interval)
            else:
                time.sleep(backoff)
                backoff = min(backoff * 2, max_backoff)

    async def __aiter__(self) -> AsyncIterator[StoredEvent]:
        # Same clamp rationale as `__iter__`. `asyncio.sleep` raises
        # `ValueError` on negative input.
        poll_interval = max(0.0, self._opts.poll_interval)
        max_backoff = max(0.0, self._opts.max_backoff)
        backoff = poll_interval
        start = time.monotonic()

        while not self._stopped:
            if self._opts.timeout is not None:
                elapsed = time.monotonic() - start
                if elapsed >= self._opts.timeout:
                    return

            response = self._poll()

            if len(response) > 0:
                backoff = poll_interval
                self._cursor = response.next_id

                for event in response:
                    yield event

                if len(response) < self._opts.limit:
                    await asyncio.sleep(poll_interval)
            else:
                await asyncio.sleep(backoff)
                backoff = min(backoff * 2, max_backoff)


class TypedEventStream(Generic[T]):
    """
    A typed event stream that deserializes events. Supports both sync
    and async iteration; same constraints as `EventStream`.

    Example:
        >>> for reading in node.subscribe_typed(model=TemperatureReading):
        ...     print(f'{reading.sensor_id}: {reading.celsius}°C')
    """

    def __init__(
        self,
        bus: Net,
        parse: Callable[[str], T],
        opts: Optional[SubscribeOpts] = None,
    ) -> None:
        self._inner = EventStream(bus, opts)
        self._parse = parse

    def stop(self) -> None:
        """Stop the stream."""
        self._inner.stop()

    def __iter__(self) -> Iterator[T]:
        for event in self._inner:
            yield self._parse(event.raw)

    async def __aiter__(self) -> AsyncIterator[T]:
        async for event in self._inner:
            yield self._parse(event.raw)
