"""MCP bridge pure-helper tests (`MCP_BRIDGE_SDK_PLAN.md` P1).

Build the extension first::

    maturin develop --features mcp

The helpers are the bridge's one Rust implementation — these tests pin the
classification parity vectors (same inputs -> same status in every
binding) and the secret-negative rule (no env value ever crosses back).
"""

import json

import pytest

pytest.importorskip("net._net")

netmod = pytest.importorskip("net")
if not hasattr(netmod, "classify_mcp_server"):
    pytest.skip("wheel built without the `mcp` feature", allow_module_level=True)

from net import classify_mcp_server, lower_mcp_tool  # noqa: E402


SECRET = "ghp_this-value-must-never-cross"


def test_classify_parity_vectors() -> None:
    # The cross-binding parity vectors: same inputs -> same status/tags.
    assert (
        classify_mcp_server("npx", ["-y", "some-server"], [("GITHUB_TOKEN", SECRET)])
        == "credentialed"
    )
    assert (
        classify_mcp_server("npx", ["-y", "@modelcontextprotocol/server-github"], [])
        == "external_api"
    )
    # Unsure => spicy: gated exactly like credentialed.
    assert classify_mcp_server("uvx", ["mcp-server-time"], [("TZ", "UTC")]) == "unknown"


def test_downgrade_requires_force_and_upgrade_does_not() -> None:
    with pytest.raises(ValueError, match="force"):
        classify_mcp_server("uvx", ["mcp-server-time"], [], "no-credentials")
    assert (
        classify_mcp_server("uvx", ["mcp-server-time"], [], "no-credentials", True)
        == "none"
    )
    assert classify_mcp_server("uvx", ["t"], [], "credentialed") == "credentialed"
    with pytest.raises(ValueError, match="credential_override"):
        classify_mcp_server("uvx", ["t"], [], "bogus")


def test_lower_tool_produces_descriptor_and_bridge_metadata() -> None:
    tool = {
        "name": "echo",
        "description": "echo it back",
        "inputSchema": {"type": "object", "properties": {"message": {"type": "string"}}},
    }
    lowered = json.loads(
        lower_mcp_tool(json.dumps(tool), "2.0.0", "credentialed", "provider_local")
    )
    assert lowered["tool_id"] == "echo"
    assert lowered["mcp_name"] == "echo"
    meta = lowered["bridge_metadata"]
    assert meta["tool::echo::compat_tier"] == "mcp_bridge"
    assert meta["tool::echo::credential_status"] == "credentialed"
    desc = lowered["descriptor"]
    assert desc["tool_id"] == "echo"
    assert "message" in desc["input_schema"]


def test_lower_sanitizes_non_channel_safe_names() -> None:
    # A camelCase name is bridged under a sanitized id; the original name
    # rides along as mcp_name for the eventual tools/call.
    tool = {"name": "getCaps", "inputSchema": {"type": "object"}}
    lowered = json.loads(lower_mcp_tool(json.dumps(tool), "1.0.0", "none"))
    assert lowered["mcp_name"] == "getCaps"
    assert lowered["tool_id"] != "getCaps"
    assert lowered["tool_id"].startswith("getcaps")


def test_lower_rejects_wire_style_garbage_status() -> None:
    # `credential_status` is trusted LOCAL input (the classifier's own
    # label) — an unknown label is an error, never silently gated/guessed.
    tool = {"name": "echo", "inputSchema": {"type": "object"}}
    with pytest.raises(ValueError, match="credential_status"):
        lower_mcp_tool(json.dumps(tool), "1.0.0", "totally-fine-trust-me")
    with pytest.raises(ValueError, match="substitutability"):
        lower_mcp_tool(json.dumps(tool), "1.0.0", "none", "anything")


def test_secret_negative_no_env_value_ever_crosses() -> None:
    # The secret-negative rule from the SDK matrix: classification takes
    # env pairs but only a fixed label comes back, and nothing lowered
    # ever contains an env value.
    status = classify_mcp_server("npx", ["srv"], [("API_KEY", SECRET)])
    assert status == "credentialed"
    assert SECRET not in status

    tool = {"name": "srv.call", "description": "calls things", "inputSchema": {"type": "object"}}
    lowered = lower_mcp_tool(json.dumps(tool), "1.0.0", status)
    assert SECRET not in lowered
