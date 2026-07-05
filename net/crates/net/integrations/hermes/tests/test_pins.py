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


async def _wait_until(cond, timeout: float = 5.0, interval: float = 0.02) -> bool:
    """Poll ``cond`` until true or ``timeout`` elapses, yielding to the loop so
    the promoter task runs. Decouples correctness from OS file-watcher latency
    (fixed sleeps flake under CI load)."""
    import time

    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if cond():
            return True
        await asyncio.sleep(interval)
    return cond()


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


def test_promote_survives_a_registration_error(plugin):
    # A registry rejection (e.g. a name collision) or a bad schema must not
    # propagate out of the promote loop and silently stop all future promotion.
    pins = plugin.pins

    class _RaisingRegistrar:
        def __init__(self):
            self.registered = {}

        def register_pinned(self, *, name, schema, handler, check_fn, description):
            if "boom" in name:
                raise RuntimeError("registry rejected the name")
            self.registered[name] = schema

        def deregister_pinned(self, name):
            self.registered.pop(name, None)

    async def fake_describe(cap_id):
        return {
            "status": "ok",
            "name": cap_id,
            "description": f"desc {cap_id}",
            "input_schema": {"type": "object", "properties": {}},
        }

    reg = _RaisingRegistrar()
    promoter = pins.PinPromoter(reg, fake_describe, lambda: True, "unused")

    async def body():
        # The pinned name for "prov/boom" embeds "boom" → register_pinned raises;
        # the promoter must swallow it, not propagate.
        await promoter._promote("prov/boom")
        # A later good cap still promotes — the loop wasn't poisoned.
        await promoter._promote("prov/ok")

    asyncio.run(body())
    assert not any("boom" in n for n in reg.registered), reg.registered
    assert any("ok" in n for n in reg.registered), reg.registered


def test_pin_promotion_service_stop_is_idempotent(plugin, tmp_path):
    # Regression: after a successful stop the promoter's event loop is closed;
    # a second stop() must NOT call `call_soon_threadsafe` on that closed loop
    # (RuntimeError) — `_on_session_end` documents teardown as idempotent.
    import time

    pins = plugin.pins

    async def fake_describe(cap_id: str) -> dict:
        return {
            "status": "ok",
            "name": cap_id,
            "input_schema": {"type": "object", "properties": {}},
        }

    promoter = pins.PinPromoter(
        _FakeRegistrar(), fake_describe, lambda: True, str(tmp_path / "pins.json")
    )
    service = pins.PinPromotionService(promoter)
    service.start()
    try:
        # Wait until the background loop/task are actually set, so stop()
        # exercises the running-then-closed path (not the never-started one).
        deadline = time.monotonic() + 5.0
        while service._loop is None and time.monotonic() < deadline:
            time.sleep(0.02)
        assert service._loop is not None, "promotion loop never started"
        service.stop()  # first stop: cancels + joins, loop closes
        # A second stop must be a clean no-op, not a RuntimeError on the closed loop.
        service.stop()
        # A third, for good measure — still idempotent.
        service.stop()
    finally:
        service.stop()


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

    name_a = pins.pinned_tool_name("prov/a")
    name_b = pins.pinned_tool_name("prov/b")

    async def body() -> _FakeRegistrar:
        registrar = _FakeRegistrar()
        store = AsyncPinStore(path)
        assert await store.approve("prov/a")  # seed one approved pin
        promoter = pins.PinPromoter(registrar, fake_describe, lambda: True, path)
        task = asyncio.create_task(promoter.run())
        try:
            # Poll for each event (not fixed sleeps): the snapshot promotes
            # prov/a; only THEN mutate again, so the 120ms watcher debounce can't
            # coalesce successive mutations into one delta.
            assert await _wait_until(lambda: name_a in registrar.registered), registrar.registered
            await store.approve("prov/b")  # -> a promote event
            assert await _wait_until(lambda: name_b in registrar.registered), registrar.registered
            await store.reject("prov/a")  # -> a retire event
            assert await _wait_until(lambda: name_a in registrar.deregistered), registrar.deregistered
        finally:
            task.cancel()
            try:
                await task
            except asyncio.CancelledError:
                pass
        return registrar

    registrar = asyncio.run(body())

    # prov/b (approved while watching) is now a registered tool; prov/a (rejected)
    # was retired.
    assert name_b in registrar.registered, registrar.registered
    assert name_a in registrar.deregistered, registrar.deregistered
    assert name_a not in registrar.registered

    # The promoted tool carries the capability's live schema + the shared check_fn.
    entry = registrar.registered[name_b]
    assert entry["schema"]["parameters"]["properties"] == {"m": {"type": "string"}}
    assert entry["check_fn"]() is True
