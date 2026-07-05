"""Subscription-driven pin-promotion tests (Phase 2).

Covers the pure helpers and the diff engine (``PinPromoter``) end to end,
driven by a REAL pin-change subscription over a temp store — no Hermes, no live
provider (a fake ``describe`` stands in for the gateway). The engine is
dependency-injected precisely so this is testable in isolation.
"""

from __future__ import annotations

import asyncio

import pytest

pytest.importorskip("net")
pytest.importorskip("net_sdk")


def test_pinned_tool_name_is_stable_and_hermes_safe(plugin):
    pins = plugin.pins
    a = pins.pinned_tool_name("42/github.create_issue")
    assert a == pins.pinned_tool_name("42/github.create_issue")  # stable
    assert a.startswith("net_pinned__")
    assert all(c.isalnum() or c == "_" for c in a)  # registry-safe
    assert pins.pinned_tool_name("42/a") != pins.pinned_tool_name("42/b")


def test_build_pinned_schema_carries_live_schema_and_risk(plugin):
    pins = plugin.pins
    detail = {
        "status": "ok",
        "name": "create_issue",
        "description": "Create a GitHub issue",
        "input_schema": {
            "type": "object",
            "properties": {"title": {"type": "string"}},
            "required": ["title"],
        },
        "credential_status": "credentialed",
    }
    schema = pins.build_pinned_schema("42/github.create_issue", detail)
    assert schema["name"] == pins.pinned_tool_name("42/github.create_issue")
    assert schema["parameters"] == detail["input_schema"]
    assert "42/github.create_issue" in schema["description"]
    assert "risk" in schema["description"]  # credentialed -> a risk note


class _FakeRegistrar:
    def __init__(self) -> None:
        self.registered: dict = {}
        self.deregistered: list = []

    def register_pinned(self, *, name, schema, handler, check_fn, description) -> None:
        self.registered[name] = {"schema": schema, "handler": handler, "check_fn": check_fn}

    def deregister_pinned(self, name) -> None:
        self.deregistered.append(name)
        self.registered.pop(name, None)


def test_promoter_promotes_snapshot_then_applies_deltas(plugin, tmp_path):
    pins = plugin.pins
    from net_sdk import AsyncPinStore

    path = str(tmp_path / "pins.json")

    async def fake_describe(cap_id: str) -> dict:
        return {
            "status": "ok",
            "name": cap_id,
            "description": f"desc {cap_id}",
            "input_schema": {"type": "object", "properties": {"m": {"type": "string"}}},
            "credential_status": "none",
        }

    async def body() -> _FakeRegistrar:
        registrar = _FakeRegistrar()
        store = AsyncPinStore(path)
        assert await store.approve("prov/a")  # seed one approved pin
        promoter = pins.PinPromoter(registrar, fake_describe, lambda: True, path)
        task = asyncio.create_task(promoter.run())
        await asyncio.sleep(0.35)  # the snapshot promotes prov/a
        await store.approve("prov/b")  # -> a promote event
        await asyncio.sleep(0.45)
        await store.reject("prov/a")  # -> a retire event
        await asyncio.sleep(0.45)
        task.cancel()
        try:
            await task
        except asyncio.CancelledError:
            pass
        return registrar

    registrar = asyncio.run(body())
    name_a = pins.pinned_tool_name("prov/a")
    name_b = pins.pinned_tool_name("prov/b")

    # prov/b (approved while watching) is now a registered tool; prov/a (rejected)
    # was retired.
    assert name_b in registrar.registered, registrar.registered
    assert name_a in registrar.deregistered, registrar.deregistered
    assert name_a not in registrar.registered

    # The promoted tool carries the capability's live schema + the shared check_fn.
    entry = registrar.registered[name_b]
    assert entry["schema"]["parameters"]["properties"] == {"m": {"type": "string"}}
    assert entry["check_fn"]() is True
