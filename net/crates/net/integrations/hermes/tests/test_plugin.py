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
    "net_search_capabilities",
    "net_describe_capability",
    "net_invoke_capability",
    "net_list_pinned_capabilities",
    "net_request_pin",
}


def _run(coro):
    return json.loads(asyncio.run(coro))


# --- registration ----------------------------------------------------------


def test_register_wires_five_tools_and_hook(plugin, ctx):
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


def test_invoke_defaults_arguments_to_empty(plugin, node_ready):
    tools = plugin.tools
    # No `arguments` key: still well-formed, only the (unreachable) provider fails.
    result = _run(tools.handle_net_invoke({"cap_id": "42/nope"}))
    assert result["status"] in {"transport_error", "not_found"}


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


def test_bad_identity_seed_is_a_clear_error(plugin, monkeypatch):
    # A malformed NET_MESH_IDENTITY_SEED fails early (before the mesh is built)
    # with a message that names the env var — not a bare ValueError. `_build`
    # doesn't touch the node singleton, so this is safe to call directly.
    monkeypatch.setenv("NET_MESH_IDENTITY_SEED", "not-valid-hex-zz")
    with pytest.raises(RuntimeError, match="NET_MESH_IDENTITY_SEED"):
        plugin.node._build()
