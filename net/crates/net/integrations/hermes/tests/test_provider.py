"""Tests for provider-side local-tool federation (`HERMES_INTEGRATION_PLAN_V2.md`
Phase 2, Slice C): the :class:`provider.LocalToolProvider` publishes this node's
OWN tools and routes each mesh invoke back into dispatch, gated by provider-side
approval for dangerous tools (fail-closed).

The publish/serve/invoke machinery is the Rust SDK's (proven in the binding's
``test_publish.py``); here we prove the plugin's approval gating + publish
orchestration — including that a dangerous tool is gated **over the wire** and
never runs when approval is declined / unreachable.
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

PSK = "6c" * 32
OBJ_SCHEMA = {"type": "object", "properties": {"message": {"type": "string"}}}


def _make(plugin, *, mesh=None, tools=None, dispatch=None, approve=False, dangerous=None):
    """Build a LocalToolProvider with recording doubles. ``approve`` is the fixed
    decision (True/False/None); ``dangerous`` is the set of dangerous names."""
    P = plugin.provider
    rec = {"dispatched": [], "approved": []}

    def _list():
        return tools if tools is not None else []

    async def _dispatch(name, args):
        rec["dispatched"].append((name, args))
        if dispatch is not None:
            return await dispatch(name, args)
        return f"ran:{name}"

    async def _approve(name, args):
        rec["approved"].append((name, args))
        return approve

    def _dangerous(name):
        return bool(dangerous) and name in dangerous

    prov = P.LocalToolProvider(mesh, _list, _dispatch, _approve, _dangerous)
    return prov, rec


# ---------------------------------------------------------------------------
# Approval gating (deterministic, no network).
# ---------------------------------------------------------------------------


def test_safe_tool_dispatches_without_approval(plugin):
    prov, rec = _make(plugin, dangerous=set())
    out = asyncio.run(prov._callback("read_file", json.dumps({"path": "/x"})))
    assert out == "ran:read_file"
    assert rec["dispatched"] == [("read_file", {"path": "/x"})]
    assert rec["approved"] == []  # a safe tool is never sent to approval


def test_dangerous_tool_runs_only_when_approved(plugin):
    prov, rec = _make(plugin, dangerous={"rm"}, approve=True)
    out = asyncio.run(prov._callback("rm", json.dumps({"path": "/x"})))
    assert out == "ran:rm"
    assert rec["approved"] == [("rm", {"path": "/x"})]
    assert rec["dispatched"] == [("rm", {"path": "/x"})]


def test_denied_dangerous_tool_never_dispatches(plugin):
    prov, rec = _make(plugin, dangerous={"rm"}, approve=False)
    body, is_error = asyncio.run(prov._callback("rm", "{}"))
    assert is_error is True
    assert json.loads(body)["status"] == "denied"
    assert rec["dispatched"] == []  # fail-closed


def test_unreachable_approval_fails_closed(plugin):
    prov, rec = _make(plugin, dangerous={"rm"}, approve=None)
    body, is_error = asyncio.run(prov._callback("rm", "{}"))
    assert is_error is True
    assert json.loads(body)["status"] == "approval_unreachable"
    assert rec["dispatched"] == []


def test_dispatch_failure_is_surfaced_in_band(plugin):
    async def _boom(name, args):
        raise RuntimeError("kaboom")

    prov, _rec = _make(plugin, dangerous=set(), dispatch=_boom)
    body, is_error = asyncio.run(prov._callback("read_file", "{}"))
    assert is_error is True
    payload = json.loads(body)
    assert payload["status"] == "error"
    assert "kaboom" in payload["message"]


def test_name_heuristic_classifier(plugin):
    d = plugin.provider.name_looks_dangerous
    for danger in ("terminal_run", "shell", "write_file", "desktop_click", "browser_open"):
        assert d(danger) is True, danger
    for safe in ("read_file", "get_status", "list_dir", "search_web", "describe_x"):
        assert d(safe) is False, safe
    assert d("frobnicate") is True  # unknown → gated (fail-safe)


# ---------------------------------------------------------------------------
# Publish orchestration (real isolated node, no peer).
# ---------------------------------------------------------------------------


def test_start_publishes_and_stop_withdraws(plugin):
    mesh = net.NetMesh("127.0.0.1:0", PSK, permissive_channels=True)
    mesh.start()
    prov, _ = _make(plugin, mesh=mesh, tools=[("echo", "echo it", OBJ_SCHEMA)], dangerous=set())
    try:
        published = prov.start()
        assert "echo" in published
        assert "echo" in prov.published
        assert prov.start() == published  # idempotent
        prov.stop()
        assert prov.published == []
        prov.stop()  # idempotent
    finally:
        try:
            mesh.shutdown()
        except Exception:  # noqa: BLE001
            pass


def test_empty_toolset_publishes_nothing(plugin):
    mesh = net.NetMesh("127.0.0.1:0", PSK, permissive_channels=True)
    mesh.start()
    prov, _ = _make(plugin, mesh=mesh, tools=[])
    try:
        assert prov.start() == []
        assert prov.published == []
    finally:
        try:
            mesh.shutdown()
        except Exception:  # noqa: BLE001
            pass


# ---------------------------------------------------------------------------
# Live 2-node: a safe tool runs over the wire; a dangerous tool is gated and
# never runs when approval is unreachable.
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


def _call_retry(rpc, target, service, body, attempts=6):
    last = None
    for _ in range(attempts):
        try:
            return rpc.call(target, service, body)
        except Exception as e:  # noqa: BLE001
            last = e
            time.sleep(0.1)
    raise last


def test_dangerous_tool_is_gated_over_the_wire(plugin):
    # accept/connect BEFORE start (a started node's auto-accept races a manual
    # accept — see test_publish.py).
    provider_mesh = net.NetMesh("127.0.0.1:0", PSK, permissive_channels=True)
    consumer = net.NetMesh("127.0.0.1:0", PSK, permissive_channels=True)

    dispatched = []

    async def _dispatch(name, args):
        dispatched.append(name)
        return f"ran:{name}"

    async def _approve(name, args):
        return None  # no operator surface reachable → fail closed

    prov = plugin.provider.LocalToolProvider(
        provider_mesh,
        lambda: [("echo", "safe", OBJ_SCHEMA), ("rm", "danger", OBJ_SCHEMA)],
        _dispatch,
        _approve,
        lambda n: n == "rm",
    )
    try:
        _handshake(consumer, provider_mesh)
        provider_mesh.start()
        consumer.start()
        prov.start()
        assert set(prov.published) == {"echo", "rm"}

        rpc = net.MeshRpc(consumer)

        # Safe tool: runs and round-trips.
        echo_out = _call_retry(rpc, provider_mesh.node_id, "echo", b"{}")
        assert json.loads(echo_out.decode("utf-8"))["isError"] is False
        assert "echo" in dispatched

        # Dangerous tool: the callback returns an is_error result, which the wrap
        # handler surfaces as ERR_TOOL — the raw call raises with the denial body
        # in the message. Warm up the reply channel first (a fast rejection can
        # outrace the subscription), then assert the structured denial.
        try:
            _call_retry(rpc, provider_mesh.node_id, "rm", b"{}", attempts=2)
        except Exception:  # noqa: BLE001 — expected; establishing the reply channel
            pass
        with pytest.raises(Exception) as exc:
            rpc.call(provider_mesh.node_id, "rm", b"{}")
        assert "approval_unreachable" in str(exc.value)
        assert "rm" not in dispatched  # the dangerous tool never ran
    finally:
        prov.stop()
        for m in (consumer, provider_mesh):
            try:
                m.shutdown()
            except Exception:  # noqa: BLE001
                pass
