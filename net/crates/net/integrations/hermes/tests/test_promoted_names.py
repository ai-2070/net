"""Promoted tool-name allocation + ownership tests (HERMES-04) that run
WITHOUT the native ``net`` wheel.

``test_pins.py`` / ``test_federate.py`` skip wholesale when ``net`` is not
importable; the naming and the promoters' register/retire bookkeeping are pure
Python, so they are pinned here against recording registrars.
"""

from __future__ import annotations

import asyncio
import re

import pytest

# Valid tool ids (`[a-z0-9._/-]`) that the old lossy sanitizer folded together.
_ALIASING_PAIRS = [
    ("42/foo-bar", "42/foo_bar"),  # the audited pair
    ("42/foo.bar", "42/foo_bar"),
    ("42/foo-bar", "42/foo.bar"),
    ("42/a/b", "42/a_b"),
    ("42/a.b", "42/a/b"),
    ("42/x", "42/x-"),  # trailing run stripped
    ("42/X", "42/x"),  # case folded
    ("4_2/x", "4/2_x"),
    ("42/" + "a" * 120, "42/" + "a" * 121),  # truncation
]


def _names(plugin):
    return {
        "pinned": plugin.pins.pinned_tool_name,
        "federated": plugin.federate.federated_tool_name,
    }


@pytest.mark.parametrize("kind", ["pinned", "federated"])
@pytest.mark.parametrize("a,b", _ALIASING_PAIRS)
def test_distinct_capabilities_get_distinct_names(plugin, kind, a, b):
    name = _names(plugin)[kind]
    assert name(a) != name(b), (kind, a, b, name(a))


@pytest.mark.parametrize("kind", ["pinned", "federated"])
def test_names_are_stable_and_host_safe(plugin, kind):
    name = _names(plugin)[kind]
    for cap_id in {c for pair in _ALIASING_PAIRS for c in pair}:
        n = name(cap_id)
        assert n == name(cap_id)
        assert re.fullmatch(r"[a-z0-9_]+", n), n
        assert len(n) <= 110, n


def test_simple_ids_keep_their_readable_names(plugin):
    # Ids the readable form encodes losslessly (`provider/snake_case`, the
    # common MCP shape) keep exactly the names they always had.
    f, p = plugin.federate.federated_tool_name, plugin.pins.pinned_tool_name
    assert f("pc/terminal_run") == "net_mesh__pc__terminal_run"
    assert f("42/echo") == "net_mesh__42__echo"
    assert f("42/a/b_c") == "net_mesh__42__a__b_c"
    assert p("42/echo") == "net_pinned__42_echo"
    assert p("1234/create_issue") == "net_pinned__1234_create_issue"
    # An id the readable form would fold keeps a readable prefix + a digest.
    assert re.fullmatch(r"net_mesh__pc__terminal_run___[0-9a-f]{16}", f("pc/terminal.run"))
    assert re.fullmatch(r"net_pinned__42_foo_bar___[0-9a-f]{16}", p("42/foo-bar"))


# ---------------------------------------------------------------------------
# Promoters: two capabilities never share one host registration, and a retire
# only removes the registration its capability owns.
# ---------------------------------------------------------------------------


class _PinRegistrar:
    def __init__(self):
        self.registered = {}
        self.deregistered = []

    def register_pinned(self, *, name, schema, handler, check_fn, description):
        self.registered[name] = handler  # the host replaces same-name entries

    def deregister_pinned(self, name):
        self.deregistered.append(name)
        self.registered.pop(name, None)


class _FedRegistrar:
    def __init__(self):
        self.registered = {}
        self.deregistered = []

    def register_federated(self, *, name, schema, handler, check_fn, description):
        self.registered[name] = handler

    def deregister_federated(self, name):
        self.deregistered.append(name)
        self.registered.pop(name, None)


async def _describe(cap_id):
    return {"status": "ok", "name": cap_id, "input_schema": {"type": "object", "properties": {}}}


async def _invoke(cap_id, args):
    return cap_id


def _pin_promoter(plugin, reg):
    return plugin.pins.PinPromoter(reg, _describe, lambda: True, "unused")


def _fed_promoter(plugin, reg):
    return plugin.federate.FederationPromoter(reg, _describe, _invoke, lambda: True)


def test_pin_promotion_of_aliasing_ids_keeps_both(plugin):
    reg = _PinRegistrar()
    prom = _pin_promoter(plugin, reg)
    asyncio.run(prom._promote("42/foo-bar"))
    asyncio.run(prom._promote("42/foo_bar"))
    assert len(reg.registered) == 2, reg.registered
    # Retiring one leaves the other's registration in place.
    prom._retire("42/foo-bar")
    assert list(reg.registered) == [plugin.pins.pinned_tool_name("42/foo_bar")]


def test_federation_of_aliasing_ids_keeps_both(plugin):
    reg = _FedRegistrar()
    prom = _fed_promoter(plugin, reg)
    rows = [{"cap_id": "42/foo-bar"}, {"cap_id": "42/foo_bar"}]
    asyncio.run(prom.reconcile(rows))
    assert len(reg.registered) == 2, reg.registered
    asyncio.run(prom.reconcile([{"cap_id": "42/foo_bar"}]))
    assert list(reg.registered) == [plugin.federate.federated_tool_name("42/foo_bar")]


class _FlakyPinRegistrar(_PinRegistrar):
    """Deregistration fails the first ``fail`` times."""

    def __init__(self, fail=1):
        super().__init__()
        self.fail = fail

    def deregister_pinned(self, name):
        if self.fail:
            self.fail -= 1
            raise RuntimeError("registry busy")
        super().deregister_pinned(name)


class _FlakyFedRegistrar(_FedRegistrar):
    def __init__(self, fail=1):
        super().__init__()
        self.fail = fail

    def deregister_federated(self, name):
        if self.fail:
            self.fail -= 1
            raise RuntimeError("registry busy")
        super().deregister_federated(name)


def test_a_failed_pin_retire_is_retried(plugin):
    # The pin is gone (`removed` fired once); no second event will come. The
    # removal must still happen, and the name must then be free again.
    reg = _FlakyPinRegistrar()
    prom = _pin_promoter(plugin, reg)
    asyncio.run(prom._promote("42/a"))
    name = plugin.pins.pinned_tool_name("42/a")
    prom._retire("42/a")
    assert name in reg.registered, "the first deregistration failed"
    asyncio.run(prom._retry_pending())
    assert reg.registered == {}
    assert prom._owners == {} and prom._retiring == {}


def test_a_re_promoted_pin_is_not_removed_by_its_stale_retire(plugin):
    reg = _FlakyPinRegistrar()
    prom = _pin_promoter(plugin, reg)
    asyncio.run(prom._promote("42/a"))
    prom._retire("42/a")  # fails; queued for retry
    asyncio.run(prom._promote("42/a"))  # approved again before the retry
    asyncio.run(prom._retry_pending())
    assert list(reg.registered) == [plugin.pins.pinned_tool_name("42/a")]


def test_a_collision_refused_pin_is_promoted_once_the_name_frees(plugin, monkeypatch):
    monkeypatch.setattr(plugin.pins, "pinned_tool_name", lambda _c: "net_pinned__same")
    reg = _PinRegistrar()
    prom = _pin_promoter(plugin, reg)
    asyncio.run(prom._promote("42/a"))
    asyncio.run(prom._promote("42/b"))  # refused: the name is owned
    prom._retire("42/a")
    # No new `added` event for 42/b: the retry after the change promotes it.
    asyncio.run(prom._retry_pending())
    assert prom._registered == {"42/b": "net_pinned__same"}


def test_a_failed_federation_retire_is_retried_on_the_next_reconcile(plugin):
    reg = _FlakyFedRegistrar()
    prom = _fed_promoter(plugin, reg)
    asyncio.run(prom.reconcile([{"cap_id": "42/a"}]))
    asyncio.run(prom.reconcile([]))  # vanished; deregistration fails
    assert len(reg.registered) == 1
    asyncio.run(prom.reconcile([]))  # retried
    assert reg.registered == {}
    assert prom._owners == {} and prom._registered == {}


@pytest.mark.parametrize("which", ["pins", "federate"])
def test_name_collision_is_refused_not_replaced(plugin, monkeypatch, which):
    # Defence in depth: even if two ids ever produced one name, the second
    # must not take over (or later delete) the first one's registration.
    if which == "pins":
        monkeypatch.setattr(plugin.pins, "pinned_tool_name", lambda _c: "net_pinned__same")
        reg = _PinRegistrar()
        prom = _pin_promoter(plugin, reg)
        asyncio.run(prom._promote("42/a"))
        first = reg.registered["net_pinned__same"]
        asyncio.run(prom._promote("42/b"))
        assert reg.registered == {"net_pinned__same": first}
        prom._retire("42/b")  # never registered: must not remove 42/a's tool
        assert reg.registered == {"net_pinned__same": first}
        prom._retire("42/a")
        assert reg.registered == {}
        # The loser is retried once the name is free again.
        asyncio.run(prom._promote("42/b"))
        assert list(reg.registered) == ["net_pinned__same"]
    else:
        monkeypatch.setattr(plugin.federate, "federated_tool_name", lambda _c: "net_mesh__same")
        reg = _FedRegistrar()
        prom = _fed_promoter(plugin, reg)
        asyncio.run(prom.reconcile([{"cap_id": "42/a"}, {"cap_id": "42/b"}]))
        first = reg.registered["net_mesh__same"]
        assert reg.registered == {"net_mesh__same": first}
        # 42/b vanishing must not retire 42/a's tool.
        asyncio.run(prom.reconcile([{"cap_id": "42/a"}]))
        assert reg.registered == {"net_mesh__same": first}
        assert reg.deregistered == []
