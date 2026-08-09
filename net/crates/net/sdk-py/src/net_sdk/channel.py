"""Typed channels — strongly typed pub/sub over named channels."""

from __future__ import annotations

import dataclasses
import json
from dataclasses import replace
from typing import Any, Callable, Generic, Iterator, Optional, TypeVar

from net import Net

from net_sdk.stream import EventStream, SubscribeOpts, TypedEventStream
from net_sdk.types import Receipt

T = TypeVar("T")

#: The reserved key `TypedChannel` stamps on every published payload so
#: subscribers can filter on it. Routing metadata, not part of the
#: caller's event type — stripped before typed delivery.
CHANNEL_TAG_KEY = "_channel"

#: Maximum channel-name length in bytes. Mirrors `MAX_NAME_LEN` in
#: `net/crates/net/src/adapter/net/channel/name.rs`.
MAX_CHANNEL_NAME_LEN = 255

_ALLOWED_CHANNEL_CHARS = frozenset(
    "abcdefghijklmnopqrstuvwxyz" "0123456789" "-_./"
)


class ChannelNameError(ValueError):
    """A channel name violated the canonical Net naming grammar.

    Raised by :func:`validate_channel_name` and, transitively, by the
    :class:`TypedChannel` constructor.
    """


def validate_channel_name(name: str) -> str:
    """Validate `name` against the canonical Net channel-name grammar.

    This is the Python mirror of `ChannelName::validate` in
    `net/crates/net/src/adapter/net/channel/name.rs`. The Rust type is
    the only constructor for a distributed mesh channel name and has no
    `From<&str>` escape hatch, but the ergonomic tagged-topic wrapper
    here never reaches it: `TypedChannel.publish` embeds the name in
    generic EventBus JSON as `_channel`. Without this check a name that
    the mesh would reject is accepted locally and only fails — or,
    worse, silently splits a namespace — once the same string is used
    against a real mesh channel.

    Returns `name` unchanged so callers can validate inline.
    Raises `ChannelNameError` on any violation.
    """
    if not isinstance(name, str):
        raise ChannelNameError(
            f"channel name must be a str, got {type(name).__name__}"
        )
    if not name:
        raise ChannelNameError("channel name must not be empty")
    encoded_len = len(name.encode("utf-8"))
    if encoded_len > MAX_CHANNEL_NAME_LEN:
        raise ChannelNameError(
            f"channel name too long: {encoded_len} bytes "
            f"(max {MAX_CHANNEL_NAME_LEN})"
        )
    if name.startswith("/") or name.endswith("/"):
        raise ChannelNameError("channel name must not start or end with '/'")
    if "//" in name:
        raise ChannelNameError("channel name must not contain '//'")
    for ch in name:
        # Uppercase gets its own message: `foo.bar` and `FOO.BAR` would
        # otherwise be distinct channels with distinct hashes, registry
        # entries, and ACL entries — an operator who locked down
        # `prod.deploy` would silently leave `Prod.deploy` open.
        if "A" <= ch <= "Z":
            raise ChannelNameError(
                f"uppercase character {ch!r} not allowed — "
                "channel names are lowercase only"
            )
        if ch not in _ALLOWED_CHANNEL_CHARS:
            raise ChannelNameError(f"invalid character {ch!r} in channel name")
    # Channel names double as on-disk directory segments under the
    # `redex-disk` feature: `..` would escape the persistence root and
    # `.` would alias the current directory.
    for seg in name.split("/"):
        if seg in (".", ".."):
            raise ChannelNameError(f"path segment {seg!r} is reserved")
    return name


#: Types JSON can encode directly. Anything else must be converted to a
#: mapping first, or `json.dumps` fails at publish time.
_JSON_SCALARS = (str, int, float, bool, type(None))


def _to_dict(event: Any) -> dict:
    """Convert an event to a dict copy, never mutating the original."""
    if hasattr(event, "model_dump"):
        # Pydantic v2 — honours the model's own field serializers.
        return event.model_dump()
    elif isinstance(event, dict):
        return dict(event)
    elif dataclasses.is_dataclass(event) and not isinstance(event, type):
        # Checked before `__dict__` because `@dataclass(slots=True)` has
        # no instance `__dict__` at all. The old duck-typed chain fell
        # through to the `_value` wrapper for slotted dataclasses, which
        # then died inside `json.dumps` — "any dataclass works" was only
        # true without `slots=True`. `asdict` also recurses into nested
        # dataclasses, which `dict(__dict__)` did not.
        return dataclasses.asdict(event)
    elif hasattr(event, "__dict__"):
        return dict(event.__dict__)
    elif isinstance(event, _JSON_SCALARS) or isinstance(event, (list, tuple)):
        return {"_value": list(event) if isinstance(event, tuple) else event}
    elif hasattr(type(event), "__slots__"):
        # Plain (non-dataclass) slotted class. Same failure mode as
        # above; gather the declared slots instead of guessing.
        slots: dict[str, Any] = {}
        for cls in type(event).__mro__:
            for slot in getattr(cls, "__slots__", ()):
                if slot not in slots and hasattr(event, slot):
                    slots[slot] = getattr(event, slot)
        return slots
    else:
        raise TypeError(
            f"cannot serialize {type(event).__name__} to a channel event: "
            "pass a dict, a dataclass, a Pydantic model, or an object with "
            "__dict__/__slots__"
        )


def _strip_channel_tag(raw: str) -> str:
    """Remove the `_channel` routing tag from a raw JSON event.

    Custom `parse=` callables receive payload-only JSON, the same shape
    the model and default paths hand back. Otherwise a strict Pydantic
    model (`extra="forbid"`) passed through
    `parse=lambda raw: Model.model_validate_json(raw)` would reject
    every event on the channel purely because of routing metadata it
    never declared.

    The substring guard keeps the common case to one scan instead of a
    parse/re-serialize round trip. Any standard JSON serializer writes
    the key literally, so a miss means the key is genuinely absent.
    """
    if f'"{CHANNEL_TAG_KEY}"' not in raw:
        return raw
    data = json.loads(raw)
    if not isinstance(data, dict) or CHANNEL_TAG_KEY not in data:
        return raw
    data.pop(CHANNEL_TAG_KEY)
    return json.dumps(data)


class TypedChannel(Generic[T]):
    """
    A strongly typed channel for publishing and subscribing to events.

    Deserialization picks one of three paths, in order: an explicit
    `parse` callable, a `model` (constructed as `model(**payload)`), or
    a plain dict. All three receive payload-only JSON — the `_channel`
    routing tag `publish()` stamps on the wire is stripped first. Use
    `subscribe_raw()` if you want the tag.

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
        self._name = validate_channel_name(name)
        self._model = model
        self._parse = parse
        # Filter is a constant for the lifetime of the channel; build
        # the JSON string once instead of regenerating it on every
        # subscribe / subscribe_raw call.
        self._filter = json.dumps({"path": CHANNEL_TAG_KEY, "value": name})

    @property
    def name(self) -> str:
        """The channel name."""
        return self._name

    def publish(self, event: T) -> Receipt:
        """Publish a typed event to this channel.

        This is local EventBus ingestion tagged with `_channel`, not
        distributed mesh fan-out — there is no roster and no per-peer
        `PublishReport`. Returns the same `Receipt` as `NetNode.emit`
        (the native `ingest_raw` result, which this used to discard and
        return `None`), and raises on ingestion failure.
        """
        data = _to_dict(event)
        data[CHANNEL_TAG_KEY] = self._name
        result = self._bus.ingest_raw(json.dumps(data))
        return Receipt(shard_id=result.shard_id, timestamp=result.timestamp)

    def publish_batch(self, events: list[T]) -> int:
        """Publish a batch of typed events. Returns number ingested.

        The count can be lower than `len(events)` — a partial batch is
        not an error and does not raise.
        """
        payloads = []
        for event in events:
            data = _to_dict(event)
            data[CHANNEL_TAG_KEY] = self._name
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
            user_parse = self._parse

            def parse_fn(raw: str) -> T:
                # Payload-only JSON, matching the model and default
                # paths below. A custom parser used to be the one path
                # that still saw `_channel`, so a strict Pydantic model
                # rejected every event on the channel.
                return user_parse(_strip_channel_tag(raw))
        elif self._model is not None:
            model = self._model

            def parse_fn(raw: str) -> T:
                data = json.loads(raw)
                data.pop(CHANNEL_TAG_KEY, None)
                return model(**data)  # type: ignore[return-value]
        else:

            def parse_fn(raw: str) -> T:
                data = json.loads(raw)
                data.pop(CHANNEL_TAG_KEY, None)
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
