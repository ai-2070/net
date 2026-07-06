"""Tests for consume-side tool federation (`HERMES_INTEGRATION_PLAN_V2.md`
Phase 2, Slice D): the :class:`federate.FederationPromoter` surfaces discovered
mesh capabilities as machine-namespaced first-class Hermes tools, reconciled
against the gateway's discovery.

The discovery + dedup + invoke machinery is the SDK's (the gateway groups
``provider_equivalent`` capabilities and consent-gates invoke); here we prove the
plugin's promotion diff (promote / retire / namespacing) — including a live
2-node test where a real gateway discovers a remote tool and it becomes a
first-class ``net_mesh__<provider>__<tool>`` entry.
"""

from __future__ import annotations

import asyncio
import json
import threading
import time

import pytest

pytest.importorskip("net")
pytest.importorskip("net._net")

import net  # noqa: E402

PSK = "7d" * 32


class RecordingRegistrar:
    """A ``federate.Registrar`` double that records (de)registrations."""

    def __init__(self) -> None:
        self.registered: dict = {}
        self.deregistered: list = []

    def register_federated(self, *, name, schema, handler, check_fn, description) -> None:
        self.registered[name] = {
            "schema": schema,
            "handler": handler,
            "check_fn": check_fn,
            "description": description,
        }

    def deregister_federated(self, name) -> None:
        self.deregistered.append(name)
        self.registered.pop(name, None)


def _promoter(plugin, reg, *, describe=None, invoke=None):
    F = plugin.federate

    async def _describe(cap_id):
        if describe is not None:
            return await describe(cap_id)
        return {
            "status": "ok",
            "name": cap_id,
            "description": f"desc {cap_id}",
            "input_schema": {"type": "object", "properties": {}},
        }

    invoked = []

    async def _invoke(cap_id, args):
        invoked.append((cap_id, args))
        if invoke is not None:
            return await invoke(cap_id, args)
        return json.dumps({"status": "ok", "cap_id": cap_id})

    prom = F.FederationPromoter(reg, _describe, _invoke, lambda: True)
    return prom, invoked


# ---------------------------------------------------------------------------
# Namespacing + reconcile (deterministic, no network).
# ---------------------------------------------------------------------------


def test_federated_tool_name_is_namespaced_and_stable(plugin):
    f = plugin.federate.federated_tool_name
    assert f("pc/terminal.run") == "net_mesh__pc__terminal_run"
    assert f("42/echo") == f("42/echo")  # stable
    assert f("42/echo") != f("99/echo")  # per-provider namespacing


def test_reconcile_promotes_dedups_providers_and_retires(plugin):
    reg = RecordingRegistrar()
    prom, _ = _promoter(plugin, reg)
    caps = [
        {"cap_id": "42/echo", "providers": [42]},
        {"cap_id": "42/add", "providers": [42, 99]},  # a deduped group
    ]
    asyncio.run(prom.reconcile(caps))
    assert set(reg.registered) == {"net_mesh__42__echo", "net_mesh__42__add"}
    # The dedup group's providers ride in the description.
    assert "providers: 42, 99" in reg.registered["net_mesh__42__add"]["description"]

    # Idempotent — re-reconciling the same set registers nothing new.
    asyncio.run(prom.reconcile(caps))
    assert len(reg.registered) == 2
    assert reg.deregistered == []

    # A vanished capability is retired.
    asyncio.run(prom.reconcile([{"cap_id": "42/echo", "providers": [42]}]))
    assert "net_mesh__42__add" in reg.deregistered
    assert set(reg.registered) == {"net_mesh__42__echo"}


def test_promoted_handler_invokes_the_capability(plugin):
    reg = RecordingRegistrar()
    prom, invoked = _promoter(plugin, reg)
    asyncio.run(prom.reconcile([{"cap_id": "42/echo", "providers": [42]}]))
    handler = reg.registered["net_mesh__42__echo"]["handler"]
    out = asyncio.run(handler({"message": "hi"}))
    assert json.loads(out)["status"] == "ok"
    assert invoked == [("42/echo", {"message": "hi"})]


def test_bad_describe_skips_promotion_without_crashing(plugin):
    reg = RecordingRegistrar()

    async def _describe(cap_id):
        return {"status": "transport_error", "error": "unreachable"}

    prom, _ = _promoter(plugin, reg, describe=_describe)
    asyncio.run(prom.reconcile([{"cap_id": "42/echo", "providers": [42]}]))
    assert reg.registered == {}  # not promoted, and the reconcile didn't raise


def test_service_start_stop_is_idempotent(plugin):
    reg = RecordingRegistrar()
    prom, _ = _promoter(plugin, reg)

    async def _search():
        return []  # nothing discovered → reconcile is a no-op

    svc = plugin.federate.FederationService(prom, _search, interval_seconds=100)
    svc.start()
    svc.start()  # idempotent — no second thread
    svc.stop()
    svc.stop()  # idempotent


# ---------------------------------------------------------------------------
# Live 2-node: a real gateway discovers a published remote tool, and the
# promoter federates it as a machine-namespaced first-class tool.
# ---------------------------------------------------------------------------


def _handshake(connector, acceptor):
    errs = []

    def _accept():
        try:
            acceptor.accept(connector.node_id)
        except Exception as e:  # noqa: BLE001
            errs.append(e)

    t = threading.Thread(target=_accept, daemon=True)
    t.start()
    time.sleep(0.05)
    connector.connect(acceptor.local_addr, acceptor.public_key, acceptor.node_id)
    t.join(timeout=5)
    if errs:
        raise errs[0]


def _wait_discover(gw, needle, timeout=8.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        raw = json.loads(gw.search(""))
        if raw.get("status") == "ok":
            caps = raw.get("capabilities", [])
            if any(needle in (c.get("cap_id") or "") for c in caps):
                return caps
        time.sleep(0.2)
    return []


def test_discovers_and_federates_a_remote_tool_over_the_wire(plugin):
    provider_mesh = net.NetMesh("127.0.0.1:0", PSK, permissive_channels=True)
    consumer = net.NetMesh("127.0.0.1:0", PSK, permissive_channels=True)
    schema_json = json.dumps(
        {"type": "object", "properties": {"message": {"type": "string"}}}
    )

    async def _cb(name, args_json):
        return "ok"

    handle = None
    try:
        _handshake(consumer, provider_mesh)
        provider_mesh.start()
        consumer.start()
        handle = provider_mesh.publish_tools(
            [("echo", "echo it", schema_json)], _cb, allow_any_caller=True
        )

        # The consumer's real gateway discovers the remote echo through the fold.
        gw = net.CapabilityGateway(consumer)
        caps = _wait_discover(gw, "echo")
        assert caps, "consumer discovers the remote echo capability"

        # Federate what the gateway discovered.
        reg = RecordingRegistrar()
        F = plugin.federate

        async def _describe(cap_id):
            return json.loads(gw.describe(cap_id))

        async def _invoke(cap_id, args):
            return gw.invoke(cap_id, json.dumps(args))

        prom = F.FederationPromoter(reg, _describe, _invoke, lambda: True)
        asyncio.run(prom.reconcile(caps))

        # echo is now a machine-namespaced, first-class tool.
        assert any(
            n.startswith("net_mesh__") and n.endswith("__echo") for n in reg.registered
        ), list(reg.registered)
    finally:
        if handle is not None:
            handle.stop()
        for m in (consumer, provider_mesh):
            try:
                m.shutdown()
            except Exception:  # noqa: BLE001
                pass
