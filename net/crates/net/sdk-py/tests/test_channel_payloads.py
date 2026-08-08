"""Serialization and parser contract for tagged Python channels.

Three defects are pinned here:

1. `@dataclass(slots=True)` has no instance `__dict__`, so the old
   duck-typed `_to_dict` chain fell through to the `{"_value": event}`
   wrapper and then died inside `json.dumps` — despite the Python guide
   promising that any dataclass works.
2. `NetNode.channel` accepted only `name` and `model`, so the documented
   `parse=` argument raised `TypeError`.
3. Custom `parse=` callables were the one path that still saw the
   `_channel` routing tag, so a strict Pydantic model rejected every
   event on the channel.
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field

import pytest

from net_sdk.channel import CHANNEL_TAG_KEY, TypedChannel, _to_dict
from net_sdk.node import NetNode
from net_sdk.types import Receipt


@dataclass
class _IngestResult:
    """Stand-in for the native `net.IngestResult`."""

    shard_id: int
    timestamp: int


class _RecordingBus:
    """Captures what the channel hands the native bus."""

    def __init__(self) -> None:
        self.ingested: list[str] = []

    def ingest_raw(self, raw: str) -> _IngestResult:
        self.ingested.append(raw)
        return _IngestResult(shard_id=len(self.ingested) % 4, timestamp=1_000)

    def ingest_raw_batch(self, raws: list[str]) -> int:
        self.ingested.extend(raws)
        return len(raws)


def _node(bus: _RecordingBus) -> NetNode:
    node = NetNode.__new__(NetNode)  # bypass native bus construction
    node._bus = bus  # type: ignore[attr-defined]
    return node


# --- 1. Serialization -----------------------------------------------


@dataclass
class Plain:
    sensor_id: str
    celsius: float


@dataclass(slots=True)
class Slotted:
    sensor_id: str
    celsius: float


@dataclass
class Nested:
    inner: Plain
    tags: list[str] = field(default_factory=list)


class Slotless:
    __slots__ = ("sensor_id", "celsius")

    def __init__(self, sensor_id: str, celsius: float) -> None:
        self.sensor_id = sensor_id
        self.celsius = celsius


class DictBased:
    def __init__(self, sensor_id: str) -> None:
        self.sensor_id = sensor_id


class FakePydantic:
    """Anything with `model_dump()` takes the Pydantic path."""

    def model_dump(self) -> dict:
        return {"sensor_id": "a1", "celsius": 22.5}


@pytest.mark.parametrize(
    ("event", "expected"),
    [
        ({"sensor_id": "a1"}, {"sensor_id": "a1"}),
        (Plain("a1", 22.5), {"sensor_id": "a1", "celsius": 22.5}),
        (Slotted("a1", 22.5), {"sensor_id": "a1", "celsius": 22.5}),
        (Slotless("a1", 22.5), {"sensor_id": "a1", "celsius": 22.5}),
        (DictBased("a1"), {"sensor_id": "a1"}),
        (FakePydantic(), {"sensor_id": "a1", "celsius": 22.5}),
    ],
)
def test_to_dict_shapes(event: object, expected: dict) -> None:
    assert _to_dict(event) == expected


def test_slotted_dataclass_publishes() -> None:
    """The reported failure: `@dataclass(slots=True)` used to reach
    `json.dumps({"_value": <object>})` and raise."""
    bus = _RecordingBus()
    ch = TypedChannel(bus, "sensors/temperature")
    ch.publish(Slotted("a1", 22.5))

    assert json.loads(bus.ingested[0]) == {
        "sensor_id": "a1",
        "celsius": 22.5,
        CHANNEL_TAG_KEY: "sensors/temperature",
    }


def test_nested_dataclass_is_recursed() -> None:
    """`dataclasses.asdict` recurses; the old `dict(__dict__)` left the
    inner dataclass as an unserializable object."""
    bus = _RecordingBus()
    ch = TypedChannel(bus, "sensors/temperature")
    ch.publish(Nested(inner=Plain("a1", 22.5), tags=["x"]))

    assert json.loads(bus.ingested[0])["inner"] == {
        "sensor_id": "a1",
        "celsius": 22.5,
    }


def test_batch_publish_serializes_slotted_dataclasses() -> None:
    bus = _RecordingBus()
    ch = TypedChannel(bus, "sensors/temperature")
    assert ch.publish_batch([Slotted("a1", 1.0), Slotted("a2", 2.0)]) == 2
    assert len(bus.ingested) == 2


@pytest.mark.parametrize("event", [object(), b"bytes", {1, 2}])
def test_unserializable_event_fails_at_publish_with_a_clear_error(
    event: object,
) -> None:
    """Better a named TypeError than a `{"_value": <object>}` wrapper
    that only explodes deeper inside `json.dumps`."""
    with pytest.raises(TypeError, match="cannot serialize"):
        _to_dict(event)


def test_empty_slots_class_serializes_to_an_empty_payload() -> None:
    """A slotted class with no fields is a legitimate marker event, not
    a serialization failure."""

    class Ping:
        __slots__ = ()

    assert _to_dict(Ping()) == {}


def test_scalar_events_still_use_the_value_wrapper() -> None:
    assert _to_dict(42) == {"_value": 42}
    assert _to_dict("hello") == {"_value": "hello"}
    assert _to_dict([1, 2]) == {"_value": [1, 2]}


# --- 2. `parse=` reaches through NetNode.channel ---------------------


def test_node_channel_accepts_parse() -> None:
    """`.claude/skills/net-event-bus/payloads.md` documented this call;
    it used to raise `TypeError: unexpected keyword argument 'parse'`."""
    bus = _RecordingBus()
    ch = _node(bus).channel(
        "sensors/temperature",
        parse=lambda raw: {"parsed": json.loads(raw)},
    )
    assert ch._parse is not None


def test_node_channel_still_accepts_model_positionally() -> None:
    bus = _RecordingBus()
    ch = _node(bus).channel("sensors/temperature", Plain)
    assert ch._model is Plain


# --- 3. Parser sees payload-only JSON -------------------------------


def _parse_fn(channel: TypedChannel):
    """Reach the parse callable `subscribe()` installs."""
    stream = channel.subscribe()
    return stream._parse


def test_custom_parser_receives_payload_only_json() -> None:
    seen: list[str] = []
    bus = _RecordingBus()
    ch = TypedChannel(bus, "sensors/temperature", parse=lambda raw: seen.append(raw))

    _parse_fn(ch)(
        json.dumps({"sensor_id": "a1", CHANNEL_TAG_KEY: "sensors/temperature"})
    )

    assert json.loads(seen[0]) == {"sensor_id": "a1"}


def test_strict_model_parser_accepts_channel_events() -> None:
    """A parser that rejects unknown keys — the shape a Pydantic model
    with `extra="forbid"` produces — must not choke on routing metadata."""

    def strict(raw: str) -> Plain:
        data = json.loads(raw)
        unknown = set(data) - {"sensor_id", "celsius"}
        if unknown:
            raise ValueError(f"unrecognized keys: {sorted(unknown)}")
        return Plain(**data)

    bus = _RecordingBus()
    ch = TypedChannel(bus, "sensors/temperature", parse=strict)

    got = _parse_fn(ch)(
        json.dumps(
            {
                "sensor_id": "a1",
                "celsius": 22.5,
                CHANNEL_TAG_KEY: "sensors/temperature",
            }
        )
    )
    assert got == Plain("a1", 22.5)


def test_model_and_default_paths_also_strip_the_tag() -> None:
    raw = json.dumps(
        {"sensor_id": "a1", "celsius": 22.5, CHANNEL_TAG_KEY: "sensors/temperature"}
    )

    bus = _RecordingBus()
    model_ch = TypedChannel(bus, "sensors/temperature", model=Plain)
    assert _parse_fn(model_ch)(raw) == Plain("a1", 22.5)

    default_ch = TypedChannel(bus, "sensors/temperature")
    assert _parse_fn(default_ch)(raw) == {"sensor_id": "a1", "celsius": 22.5}


def test_untagged_payload_passes_through_untouched() -> None:
    """The strip is a no-op — including byte-identical passthrough —
    when there is no tag to remove."""
    seen: list[str] = []
    bus = _RecordingBus()
    ch = TypedChannel(bus, "sensors/temperature", parse=lambda raw: seen.append(raw))

    raw = '{"sensor_id": "a1"}'
    _parse_fn(ch)(raw)
    assert seen == [raw]


# --- 4. Publish return contract -------------------------------------


def test_publish_returns_the_ingest_receipt() -> None:
    """`publish` used to discard the native `ingest_raw` result and
    return `None`, so a Python caller had no way to correlate an event
    with the shard that took it."""
    bus = _RecordingBus()
    ch = TypedChannel(bus, "sensors/temperature")

    receipt = ch.publish({"sensor_id": "a1"})

    assert isinstance(receipt, Receipt)
    assert receipt.timestamp == 1_000


def test_publish_propagates_ingestion_failure() -> None:
    """Failure surfaces as a raise, not a falsy return."""

    class _FailingBus(_RecordingBus):
        def ingest_raw(self, raw: str):  # type: ignore[override]
            raise RuntimeError("EventBus has been shut down")

    ch = TypedChannel(_FailingBus(), "sensors/temperature")
    with pytest.raises(RuntimeError, match="shut down"):
        ch.publish({"sensor_id": "a1"})


def test_publish_batch_reports_a_short_count_without_raising() -> None:
    """A partial batch is not an error — compare against the input
    length to detect drops."""

    class _PartialBus(_RecordingBus):
        def ingest_raw_batch(self, raws: list[str]) -> int:
            self.ingested.extend(raws)
            return len(raws) - 1

    ch = TypedChannel(_PartialBus(), "sensors/temperature")
    assert ch.publish_batch([{"a": 1}, {"a": 2}]) == 1


def test_publish_subscribe_round_trip_is_symmetric() -> None:
    """What a slotted dataclass publishes is what the model path
    reconstructs — the two halves of the contract must agree."""
    bus = _RecordingBus()
    ch = TypedChannel(bus, "sensors/temperature", model=Slotted)
    ch.publish(Slotted("a1", 22.5))

    assert _parse_fn(ch)(bus.ingested[0]) == Slotted("a1", 22.5)
