"""SDK audit 2026-10-11 — Python SDK lows."""

from __future__ import annotations

import dataclasses
import json

from net_sdk.capability import PlacementCandidate, placement_filter_from_fn
from net_sdk.node import _emit_payload
from net_sdk.stream import DEFAULT_MAX_BACKOFF, DEFAULT_POLL_INTERVAL


def test_placement_filter_receives_a_candidate_not_a_dict() -> None:
    seen: list = []
    f = placement_filter_from_fn(lambda c: seen.append(c) or "gpu" in c.tags)
    # The native layer calls with a plain dict.
    assert f.fn({"node_id": 7, "tags": ["gpu"], "metadata": {"k": "v"}}) is True
    assert isinstance(seen[0], PlacementCandidate)
    assert seen[0].node_id == 7 and seen[0].metadata == {"k": "v"}


@dataclasses.dataclass(slots=True)
class _Inner:
    x: int


@dataclasses.dataclass(slots=True)
class _Reading:
    sensor: str
    inner: _Inner


def test_emit_serializes_slotted_and_nested_dataclasses() -> None:
    payload = _emit_payload(_Reading("a", _Inner(1)))
    assert json.loads(json.dumps(payload)) == {"sensor": "a", "inner": {"x": 1}}
    assert _emit_payload([1, 2]) == [1, 2]
    assert _emit_payload("s") == "s"


def test_net_node_subscribe_uses_the_stream_defaults() -> None:
    import inspect

    from net_sdk.node import NetNode

    params = inspect.signature(NetNode.subscribe).parameters
    assert params["poll_interval"].default == DEFAULT_POLL_INTERVAL
    assert params["max_backoff"].default == DEFAULT_MAX_BACKOFF
