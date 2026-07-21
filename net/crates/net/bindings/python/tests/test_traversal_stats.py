"""Stage-5 stats-shape parity pin for the Python binding.

``NetMesh.traversal_stats()`` must return the full core snapshot —
punch outcomes, the derived failure count, the three failure-cause
counters, background-upgrade activity, and port-mapping state —
with every key present and booting to zero / False / None on a
fresh node. Mirrors the Rust SDK's
``pre_classification_state_is_unknown`` and the Go / Node shape
tests; a core field that stops being forwarded fails here.

Skips gracefully when the wheel wasn't built with nat-traversal.
"""

from __future__ import annotations

import pytest

net = pytest.importorskip("net", reason="net wheel not built locally")

EXPECTED_ZERO_COUNTERS = [
    "punches_attempted",
    "punches_succeeded",
    "punches_failed",
    "relay_fallbacks",
    "punch_timeouts",
    "punch_rejections",
    "rendezvous_no_relay",
    "upgrades_attempted",
    "upgrades_succeeded",
    "upgrades_deferred_busy",
    "port_mapping_renewals",
]

PSK = "42" * 32


@pytest.fixture()
def mesh():
    m = net.NetMesh("127.0.0.1:0", PSK)
    if not hasattr(m, "traversal_stats"):
        pytest.skip("wheel built without nat-traversal")
    yield m
    m.shutdown()


def test_traversal_stats_full_shape_boots_zero(mesh) -> None:
    stats = mesh.traversal_stats()
    for key in EXPECTED_ZERO_COUNTERS:
        assert key in stats, f"missing counter key {key!r}"
        assert stats[key] == 0, f"{key} = {stats[key]}, want 0"
    assert stats["port_mapping_active"] is False
    assert stats["port_mapping_external"] is None
    # Exactly the 13 documented keys — an extra key means the core
    # snapshot grew without this parity pin (and the other three
    # bindings) being updated.
    assert len(stats) == 13, f"unexpected stats shape: {sorted(stats)}"


def test_connect_direct_auto_is_exposed(mesh) -> None:
    assert callable(getattr(mesh, "connect_direct_auto", None))


@pytest.mark.parametrize("enabled", [True, False])
def test_auto_direct_upgrade_kwarg_accepted(enabled: bool) -> None:
    # Both poles: the flag defaults on, so False is the arm that would
    # silently regress if the binding ever collapsed back to
    # opt-in-only plumbing (`== Some(true)` instead of `if let`).
    m = net.NetMesh("127.0.0.1:0", PSK, auto_direct_upgrade=enabled)
    m.shutdown()
