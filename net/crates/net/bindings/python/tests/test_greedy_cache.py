"""Greedy Dataforts read path and data gravity: Python side.

`Redex.greedy_cache_for(channel)` returns the greedy cache's copy of a peer's
channel (or None). A hit is a served read, which data gravity turns into heat
announcements counted by `dataforts_greedy_gravity_heat_emissions_total`.

Plan: docs/internal/plans/GO_BINDING_CONSOLIDATION_AND_BLOBS_PLAN.md, gap G-A.
"""

from __future__ import annotations

import re
import time

import pytest

net = pytest.importorskip("net")
if not hasattr(net, "Redex") or not hasattr(net.Redex, "greedy_cache_for"):
    pytest.skip("net was built without the greedy read path", allow_module_level=True)

from net import Redex, RedexError  # noqa: E402

_EMISSIONS = re.compile(r"^dataforts_greedy_gravity_heat_emissions_total (\d+)$", re.M)


def _emissions(redex: Redex) -> int:
    m = _EMISSIONS.search(redex.greedy_prometheus_text())
    assert m, "greedy metrics have no gravity emissions counter"
    return int(m.group(1))


def _wait(what: str, cond, timeout: float = 10.0) -> None:
    deadline = time.monotonic() + timeout
    while not cond():
        if time.monotonic() > deadline:
            pytest.fail(f"timed out: {what}")
        time.sleep(0.05)


def _cached_peer_channel(mesh_pair, channel: str, **gravity) -> Redex:
    a, b = mesh_pair
    redex = Redex()
    redex.enable_greedy_dataforts(b, intent_match="disabled")
    redex.enable_gravity_for_greedy(b, **gravity)
    a.register_channel(channel, visibility="global", reliable=True)
    b.subscribe_channel(a.node_id, channel)

    def admitted() -> bool:
        a.publish(channel, b"observed", reliability="reliable", on_failure="best_effort")
        # The cached-channel count rises at admission, before the event is
        # appended, so wait for the cached file to hold an event as well.
        if redex.greedy_cached_channel_count() < 1:
            return False
        f = redex.greedy_cache_for(channel)
        return f is not None and len(f) > 0

    _wait("greedy cached A's channel", admitted)
    return redex


def _read_cached(redex: Redex, channel: str):
    f = redex.greedy_cache_for(channel)
    assert f is not None, "a cached channel came back as a miss"
    return f.read_range(0, len(f))


def test_greedy_cache_for_without_greedy() -> None:
    redex = Redex()
    assert redex.greedy_cache_for("py/greedy/off") is None
    with pytest.raises(RedexError):
        redex.greedy_cache_for("")


def test_greedy_cache_for_reads_a_peers_channel(mesh_pair) -> None:
    channel = "py/greedy/observed"
    redex = _cached_peer_channel(mesh_pair, channel, tick_interval_ms=50, emit_threshold_ratio=1.01)

    events = _read_cached(redex, channel)
    assert events and bytes(events[0].payload) == b"observed"
    assert redex.greedy_cache_for("py/greedy/never-published") is None

    def heated() -> bool:
        _read_cached(redex, channel)
        return _emissions(redex) > 0

    _wait("gravity announced heat from served reads", heated)


def test_greedy_cache_view_close_keeps_the_cache_live(mesh_pair) -> None:
    # Closing a cache view must not close the greedy runtime's file, or the
    # cache stops admitting the peer's later events (cubic review, #1165).
    a, _ = mesh_pair
    channel = "py/greedy/close-view"
    redex = _cached_peer_channel(mesh_pair, channel)
    view = redex.greedy_cache_for(channel)
    assert view is not None
    view.close()
    view.close()

    def after_close_cached() -> bool:
        a.publish(channel, b"after-close", reliability="reliable", on_failure="best_effort")
        return any(bytes(e.payload) == b"after-close" for e in _read_cached(redex, channel))

    _wait("the cache admitted an event published after a view was closed", after_close_cached)


def test_gravity_config_is_forwarded(mesh_pair) -> None:
    channel = "py/greedy/forwarded"
    redex = _cached_peer_channel(
        mesh_pair, channel, tick_interval_ms=50, enabled=False, emit_threshold_ratio=1.01
    )
    for _ in range(20):
        _read_cached(redex, channel)
        time.sleep(0.025)
    assert _emissions(redex) == 0, "gravity with enabled=False announced heat"
