"""Typed channels — strongly typed pub/sub over named channels."""

from __future__ import annotations

import json
from dataclasses import replace
from typing import Any, Callable, Generic, Iterator, Optional, TypeVar

from net import Net

from net_sdk.stream import EventStream, SubscribeOpts, TypedEventStream

T = TypeVar("T")


def _to_dict(event: Any) -> dict:
    """Convert an event to a dict copy, never mutating the original."""
    if hasattr(event, "model_dump"):
        return event.model_dump()
    elif isinstance(event, dict):
        return dict(event)
    elif hasattr(event, "__dict__"):
        return dict(event.__dict__)
    else:
        return {"_value": event}


class TypedChannel(Generic[T]):
    """
    A strongly typed channel for publishing and subscribing to events.

    Example:
        >>> temps = node.channel('sensors/temperature', TemperatureReading)
        >>> temps.publish(TemperatureReading(sensor_id='A1', celsius=22.5))
        >>> for reading in temps.subscribe():
        ...     print(f'{reading.sensor_id}: {reading.celsius}°C')
    """

    def __init__(
        self,
        bus: Net,
        name: str,
        model: Optional[type] = None,
        parse: Optional[Callable[[str], T]] = None,
    ) -> None:
        self._bus = bus
        self._name = name
        self._model = model
        self._parse = parse
        # Filter is a constant for the lifetime of the channel; build
        # the JSON string once instead of regenerating it on every
        # subscribe / subscribe_raw call.
        self._filter = json.dumps({"path": "_channel", "value": name})

    @property
    def name(self) -> str:
        """The channel name."""
        return self._name

    def publish(self, event: T) -> None:
        """Publish a typed event to this channel."""
        data = _to_dict(event)
        data["_channel"] = self._name
        self._bus.ingest_raw(json.dumps(data))

    def publish_batch(self, events: list[T]) -> int:
        """Publish a batch of typed events. Returns number ingested."""
        payloads = []
        for event in events:
            data = _to_dict(event)
            data["_channel"] = self._name
            payloads.append(json.dumps(data))
        return self._bus.ingest_raw_batch(payloads)

    def subscribe(self, opts: Optional[SubscribeOpts] = None) -> TypedEventStream[T]:
        """Subscribe to typed events on this channel.

        `opts` is treated as read-only: a copy is made before defaulting
        the filter to this channel's filter. The previous code aliased
        the caller's `SubscribeOpts` (`merged = opts or SubscribeOpts()`)
        and then mutated `merged.filter` in place, so reusing one
        `SubscribeOpts` across two channels silently delivered the
        first channel's events on the second subscription.
        """
        merged = SubscribeOpts() if opts is None else replace(opts)
        if merged.filter is None:
            merged.filter = self._filter

        if self._parse is not None:
            parse_fn = self._parse
        elif self._model is not None:
            model = self._model

            def parse_fn(raw: str) -> T:
                data = json.loads(raw)
                data.pop("_channel", None)
                return model(**data)  # type: ignore[return-value]
        else:

            def parse_fn(raw: str) -> T:
                data = json.loads(raw)
                data.pop("_channel", None)
                return data  # type: ignore[return-value]

        return TypedEventStream(self._bus, parse_fn, merged)

    def subscribe_raw(self, opts: Optional[SubscribeOpts] = None) -> EventStream:
        """Subscribe to raw events on this channel.

        See `subscribe()` — `opts` is copied before mutation for the
        same reason.
        """
        merged = SubscribeOpts() if opts is None else replace(opts)
        if merged.filter is None:
            merged.filter = self._filter
        return EventStream(self._bus, merged)
