"""Mesh channels through ``net_sdk.MeshNode``, live, two real nodes.

`PYTHON_SDK_WRAPPER_PARITY_PLAN.md` S1. Until S1 the SDK node had no
channel methods at all; callers reached through ``MeshNode._native``.
This file is also the first two-node channel test in the Python suite
(``test_channels.py`` covers single-node registration only).

Lives with the wheel tests because it needs the real extension. When the
sdk-py wrapper isn't installed (CI's main run), ``net_sdk`` is imported
from the in-repo source; the test never skips for that.
"""

from __future__ import annotations

import importlib
import sys
import threading
import time
from pathlib import Path

import pytest

pytest.importorskip("net._net")

SDK_SRC = Path(__file__).resolve().parents[3] / "sdk-py" / "src"
PSK = "42" * 32


def _net_sdk():
    try:
        return importlib.import_module("net_sdk")
    except ImportError:
        sys.path.insert(0, str(SDK_SRC))
        return importlib.import_module("net_sdk")


@pytest.fixture
def sdk_pair():
    """Two connected, started ``net_sdk.MeshNode``s: ``(subscriber, publisher)``."""
    net_sdk = _net_sdk()
    sub = net_sdk.MeshNode("127.0.0.1:0", PSK, heartbeat_interval_ms=200)
    pub = net_sdk.MeshNode("127.0.0.1:0", PSK, heartbeat_interval_ms=200)
    errors: list[Exception] = []

    def _accept() -> None:
        try:
            pub.accept(sub.node_id)
        except Exception as e:  # noqa: BLE001
            errors.append(e)

    t = threading.Thread(target=_accept, daemon=True)
    t.start()
    time.sleep(0.05)
    sub.connect(pub.local_addr, pub.public_key, pub.node_id)
    t.join(timeout=5)
    assert not t.is_alive(), "accept thread still alive after 5 s"
    if errors:
        raise errors[0]
    sub.start()
    pub.start()
    try:
        yield sub, pub
    finally:
        sub.shutdown()
        pub.shutdown()


def _recv_payloads(node, want: int, timeout_s: float = 5.0) -> list[bytes]:
    got: list[bytes] = []
    deadline = time.monotonic() + timeout_s
    while len(got) < want and time.monotonic() < deadline:
        for event in node.recv(64):
            raw = event.raw
            got.append(raw.encode() if isinstance(raw, str) else bytes(raw))
        if len(got) < want:
            time.sleep(0.02)
    return got


def test_register_subscribe_publish_recv_round_trip(sdk_pair) -> None:
    sub, pub = sdk_pair
    pub.register_channel("sdk/round-trip", visibility="global", reliable=True)
    sub.subscribe_channel(pub.node_id, "sdk/round-trip")

    report = pub.publish("sdk/round-trip", b'{"n": 1}', reliability="reliable")
    assert report["attempted"] == 1, report
    assert report["delivered"] == 1, report
    assert report["errors"] == []

    got = _recv_payloads(sub, 1)
    assert got == [b'{"n": 1}']

    sub.unsubscribe_channel(pub.node_id, "sdk/round-trip")
    sub.unsubscribe_channel(pub.node_id, "sdk/round-trip")  # idempotent


def test_a_subscriber_failing_subscribe_caps_gets_channel_auth_error(sdk_pair) -> None:
    net_sdk = _net_sdk()
    sub, pub = sdk_pair
    pub.register_channel(
        "sdk/gpu-only",
        subscribe_caps={"require_tags": ["sdk-test:needs-this-tag"]},
    )
    with pytest.raises(net_sdk.ChannelAuthError):
        sub.subscribe_channel(pub.node_id, "sdk/gpu-only")
    # The SDK's classes ARE the wheel's, so `except ChannelError` holds.
    assert issubclass(net_sdk.ChannelAuthError, net_sdk.ChannelError)


def test_publish_with_no_subscribers_reports_zero_attempted(sdk_pair) -> None:
    _, pub = sdk_pair
    pub.register_channel("sdk/quiet")
    report = pub.publish("sdk/quiet", b"x", on_failure="collect", max_inflight=4)
    assert report == {"attempted": 0, "delivered": 0, "errors": []}


def test_an_invalid_option_raises_the_sdk_channel_error(sdk_pair) -> None:
    net_sdk = _net_sdk()
    _, pub = sdk_pair
    with pytest.raises(net_sdk.ChannelError):
        pub.register_channel("sdk/bad", visibility="nonsense")  # type: ignore[arg-type]
    pub.register_channel("sdk/ok")
    with pytest.raises(net_sdk.ChannelError):
        pub.publish("sdk/ok", b"x", on_failure="nonsense")  # type: ignore[arg-type]


def test_shard_routing_and_the_stream_inbox_report_the_sender(sdk_pair) -> None:
    sub, pub = sdk_pair
    n = pub.num_shards()
    assert n >= 1
    sid = 0x5EED_0042
    assert pub.shard_for_stream(sid) == sid % n

    with pub.open_stream_inbox(sid, capacity=8) as inbox:
        stream = sub.open_stream(pub.node_id, sid, reliability="reliable")
        sub.send_with_retry(stream, [b"hello"])
        event = inbox.recv(timeout_ms=5000)
        assert event is not None
        assert (event.peer_node_id, event.payload) == (sub.node_id, b"hello")
        # Delivered to the inbox INSTEAD of the shard queue.
        assert pub.poll_shard(pub.shard_for_stream(sid), 64) == []
