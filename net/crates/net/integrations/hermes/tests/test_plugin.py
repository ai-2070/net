"""Contract + handler tests for the ``net`` Hermes plugin.

Registration is asserted against a fake ``PluginContext``; the async handlers
run against an isolated embedded node (no peers) with a temp pin store. The
consent-gate *logic* is pinned in Rust (``serve::gated`` + the binding's
gateway tests); here we prove the plugin wires the tools correctly and the
handlers produce the right structured JSON.
"""

from __future__ import annotations

import asyncio
import json

import pytest

pytest.importorskip("net")
pytest.importorskip("net_sdk")

TOOL_NAMES = {
    # Capability meta-tools.
    "net_search_capabilities",
    "net_describe_capability",
    "net_invoke_capability",
    "net_list_pinned_capabilities",
    "net_request_pin",
    # Device-enrollment (mesh admin) tools (V2 Phase 1).
    "net_mesh_invite",
    "net_mesh_devices",
    "net_mesh_revoke",
}


def _run(coro):
    return json.loads(asyncio.run(coro))


# --- registration ----------------------------------------------------------


def test_register_wires_all_tools_and_hook(plugin, ctx):
    plugin.register(ctx)
    assert set(ctx.tools) == TOOL_NAMES
    for name, entry in ctx.tools.items():
        assert entry["toolset"] == "net"
        assert entry["is_async"] is True
        assert entry["check_fn"] is plugin.node.check_net_available
        assert entry["schema"]["name"] == name
        assert entry["schema"]["parameters"]["type"] == "object"
        assert callable(entry["handler"])
    assert "on_session_start" in ctx.hooks
    assert "on_session_end" in ctx.hooks


def test_descriptions_disambiguate_from_local_tool_search(plugin, ctx):
    plugin.register(ctx)
    desc = ctx.tools["net_search_capabilities"]["schema"]["description"]
    # Must explicitly name Hermes's local tool so the model doesn't misroute.
    assert "tool_search" in desc
    assert "MESH" in desc


# --- handlers (isolated node) ----------------------------------------------


def test_search_on_isolated_node_is_ok_and_empty(plugin, node_ready):
    tools = plugin.tools
    result = _run(tools.handle_net_search({"query": "anything"}))
    assert result == {"status": "ok", "capabilities": []}


def test_invoke_unknown_capability_is_structured(plugin, node_ready):
    tools = plugin.tools
    result = _run(tools.handle_net_invoke({"cap_id": "42/nope", "arguments": {"x": 1}}))
    assert result["status"] in {"transport_error", "not_found"}


def test_invoke_and_describe_require_cap_id(plugin, node_ready):
    tools = plugin.tools
    assert _run(tools.handle_net_invoke({}))["status"] == "error"
    assert _run(tools.handle_net_describe({}))["status"] == "error"


def test_invoke_defaults_absent_arguments_to_empty_object(plugin, node_ready, monkeypatch):
    # Prove the handler passes "{}" (an empty object) — NOT "null" — when
    # `arguments` is absent or explicitly None. The previous version invoked an
    # unreachable cap and asserted transport_error/not_found, which held true
    # regardless of the defaulting logic (false confidence). Capture what the
    # gateway actually receives instead.
    tools = plugin.tools

    class _RecordingGateway:
        def __init__(self):
            self.last_args = None

        async def invoke(self, cap_id, arguments_json):
            self.last_args = arguments_json
            return '{"status": "ok", "content": []}'

    fake = _RecordingGateway()
    monkeypatch.setattr(plugin.node, "gateway", lambda: fake)

    # Absent `arguments`.
    _run(tools.handle_net_invoke({"cap_id": "42/x"}))
    assert fake.last_args == "{}"
    # Explicit null also normalizes to an empty object at the handler.
    _run(tools.handle_net_invoke({"cap_id": "42/x", "arguments": None}))
    assert fake.last_args == "{}"


def test_request_pin_records_pending_and_list_reflects_it(plugin, node_ready):
    from net_sdk import PinStore

    tools = plugin.tools
    cap = "42/plugin-req-test"

    res = _run(tools.handle_net_request_pin({"cap_id": cap}))
    assert res["status"] == "pending_approval"
    assert res["cap_id"] == cap
    # H9: operator-surface-first. The CLI is a *fallback* channel, listed last
    # and never the lead of the model-facing message.
    assert res["approval_channels"] == ["telegram", "desktop", "cli_fallback"]
    assert res["approval_channels"][-1] == "cli_fallback"
    assert "operator surface" in res["message"].lower()
    assert res["message"].lower().index("hermes") < res["message"].lower().index("cli")
    assert f"net mcp pin approve {cap}" in res["message"]  # still present as the fallback

    listed = _run(tools.handle_net_list_pinned({}))
    assert listed["status"] == "ok"
    assert cap in listed["pending"]
    assert cap not in listed["approved"]

    # An operator approve on the SAME shared store flips the list — the plugin
    # and `net mcp pin` are one store, one lock.
    PinStore(node_ready.pin_store_path()).approve(cap)
    listed2 = _run(tools.handle_net_list_pinned({}))
    assert cap in listed2["approved"]
    assert cap not in listed2["pending"]


def test_request_pin_requires_cap_id(plugin, node_ready):
    tools = plugin.tools
    assert _run(tools.handle_net_request_pin({}))["status"] == "error"


def test_list_pinned_surfaces_store_errors_as_data(plugin, node_ready, monkeypatch):
    # A pin-store read failure must come back as structured JSON, never raise
    # out of the tool call.
    tools = plugin.tools

    def _boom():
        raise RuntimeError("store unreadable")

    monkeypatch.setattr(plugin.node, "pin_store_path", _boom)
    res = _run(tools.handle_net_list_pinned({}))
    assert res["status"] == "error"
    assert "store unreadable" in res["error"]


def test_request_pin_on_an_already_approved_cap_says_so(plugin, node_ready):
    # `request` never downgrades an approved pin, so the response must not claim
    # approval is still required — the message reflects the real state.
    from net_sdk import PinStore

    tools = plugin.tools
    cap = "42/already-approved-req-test"
    PinStore(node_ready.pin_store_path()).approve(cap)

    res = _run(tools.handle_net_request_pin({"cap_id": cap}))
    assert res["status"] == "approved"
    assert "already approved" in res["message"].lower()
    assert "approval required" not in res["message"].lower()


def test_malformed_peer_entries_do_not_crash_startup(plugin, monkeypatch, tmp_path):
    # Non-dict peer entries must be skipped (logged), not crash the node build:
    # without the isinstance guard the logging path re-raises on `p.get(...)`.
    monkeypatch.setenv("NET_MESH_PEERS", '[42, "nope"]')
    monkeypatch.setenv("NET_MESH_PIN_STORE", str(tmp_path / "pins.json"))
    monkeypatch.delenv("NET_MESH_PSK", raising=False)
    monkeypatch.delenv("NET_MESH_IDENTITY_SEED", raising=False)
    mesh, _gateway, _pin_store, _delegation = plugin.node._build()
    try:
        assert mesh is not None  # built despite the malformed peers
    finally:
        mesh.shutdown()


def test_init_failure_after_start_rolls_back_the_mesh(plugin, monkeypatch):
    # If setup fails AFTER mesh.start(), the started mesh must be shut down so
    # init leaves no live loop / socket behind. Drive a post-start failure (no
    # resolvable pin store) against a fake mesh and assert shutdown ran.
    import net

    class _FakeMesh:
        def __init__(self, *a, **kw):
            self.started = False
            self.shut = 0

        def start(self):
            self.started = True

        def connect(self, *a, **kw):
            pass

        def shutdown(self):
            self.shut += 1

    made = []

    def factory(*a, **kw):
        m = _FakeMesh()
        made.append(m)
        return m

    monkeypatch.setattr(net, "NetMesh", factory)
    monkeypatch.setattr("net_sdk.default_pin_store_path", lambda: None)
    monkeypatch.delenv("NET_MESH_PIN_STORE", raising=False)
    monkeypatch.delenv("NET_MESH_PSK", raising=False)
    monkeypatch.delenv("NET_MESH_IDENTITY_SEED", raising=False)

    with pytest.raises(RuntimeError, match="pin-store"):
        plugin.node._build()

    assert made, "the fake mesh should have been constructed"
    assert made[0].started, "the mesh was started before the failure"
    assert made[0].shut == 1, "the started mesh must be shut down on init failure"


def test_bad_identity_seed_is_a_clear_error(plugin, monkeypatch):
    # A malformed NET_MESH_IDENTITY_SEED fails early (before the mesh is built)
    # with a message that names the env var — not a bare ValueError. `_build`
    # doesn't touch the node singleton, so this is safe to call directly.
    monkeypatch.setenv("NET_MESH_IDENTITY_SEED", "not-valid-hex-zz")
    with pytest.raises(RuntimeError, match="NET_MESH_IDENTITY_SEED"):
        plugin.node._build()
