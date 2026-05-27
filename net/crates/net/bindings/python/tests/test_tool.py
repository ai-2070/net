"""Pure-function tests for the Python tool layer.

Covers ``descriptor_for``, ``add_tool_capabilities_to_announce``,
and all four provider format translators (both directions).

Live-mesh tests (``serve_tool`` + ``call_tool`` round-trip) are
deferred to an integration test once we're ready to spin up a
two-mesh harness. The cross-language byte-equality fixtures
pinned by T-1 will eventually feed this file, the Node TS
``tool.test.ts``, and the Rust ``formats`` module from the same
golden vectors.
"""

from __future__ import annotations

import json

import pytest

from net.tool import (
    ToolCallParseError,
    ToolDescriptor,
    add_tool_capabilities_to_announce,
    anthropic,
    descriptor_for,
    gemini,
    is_terminal_event,
    mcp,
    openai,
)


def sample_descriptor() -> ToolDescriptor:
    return descriptor_for(
        "web_search",
        description="Search the web.",
        input_schema={
            "type": "object",
            "properties": {"query": {"type": "string"}},
            "required": ["query"],
        },
    )


# ---------------------------------------------------------------------------
# descriptor_for
# ---------------------------------------------------------------------------


def test_descriptor_for_defaults_version_flags_and_lists() -> None:
    desc = descriptor_for("x")
    assert desc.tool_id == "x"
    assert desc.name == "x"
    assert desc.version == "1.0.0"
    assert desc.stateless is True
    assert desc.streaming is False
    assert desc.estimated_time_ms == 0
    assert desc.tags == []
    assert desc.requires == []
    assert desc.node_count == 0
    assert desc.input_schema is None


def test_descriptor_for_serializes_schemas_to_strings() -> None:
    desc = sample_descriptor()
    assert isinstance(desc.input_schema, str)
    parsed = json.loads(desc.input_schema)
    assert parsed["properties"]["query"] == {"type": "string"}


# ---------------------------------------------------------------------------
# is_terminal_event
# ---------------------------------------------------------------------------


def test_terminal_events_flagged() -> None:
    assert is_terminal_event({"type": "result", "data": 1})
    assert is_terminal_event({"type": "error", "code": "x", "message": "y"})


def test_non_terminal_events_flagged() -> None:
    assert not is_terminal_event({"type": "start", "tool_id": "x"})
    assert not is_terminal_event({"type": "progress", "pct": 50})
    assert not is_terminal_event({"type": "delta", "data": 1})


# ---------------------------------------------------------------------------
# add_tool_capabilities_to_announce
# ---------------------------------------------------------------------------


def test_add_tool_capabilities_merges_on_fresh_set() -> None:
    desc = sample_descriptor()
    caps = add_tool_capabilities_to_announce({}, [desc])
    assert "ai-tool:web_search" in caps["tags"]
    assert caps["tools"][0]["tool_id"] == "web_search"


def test_add_tool_capabilities_preserves_existing_tags_and_dedupes() -> None:
    desc = sample_descriptor()
    caps = add_tool_capabilities_to_announce(
        {"tags": ["region.eu", "ai-tool:web_search"]},
        [desc],
    )
    occurrences = sum(1 for t in caps["tags"] if t == "ai-tool:web_search")
    assert occurrences == 1
    assert "region.eu" in caps["tags"]


def test_add_tool_capabilities_no_op_on_empty_descriptors() -> None:
    caps = add_tool_capabilities_to_announce({"tags": ["x"]}, [])
    assert caps["tags"] == ["x"]
    assert "tools" not in caps


# ---------------------------------------------------------------------------
# OpenAI format
# ---------------------------------------------------------------------------


def test_openai_to_tool_emits_function_envelope_and_strict() -> None:
    tool = openai.to_openai_tool(sample_descriptor())
    assert tool["type"] == "function"
    fn = tool["function"]
    assert fn["name"] == "web_search"
    assert fn["description"] == "Search the web."
    assert fn["strict"] is True
    assert fn["parameters"]["type"] == "object"


def test_openai_lower_tool_call_extracts_name_arguments_and_id() -> None:
    spec = openai.lower_openai_tool_call(
        {
            "id": "call_abc",
            "type": "function",
            "function": {"name": "web_search", "arguments": '{"query":"mesh"}'},
        }
    )
    assert spec.name == "web_search"
    assert spec.arguments_json == '{"query":"mesh"}'
    assert spec.provider_call_id == "call_abc"


def test_openai_lower_tool_call_rejects_malformed_arguments() -> None:
    with pytest.raises(ToolCallParseError):
        openai.lower_openai_tool_call(
            {"function": {"name": "x", "arguments": "not valid json {"}}
        )


# ---------------------------------------------------------------------------
# Anthropic format
# ---------------------------------------------------------------------------


def test_anthropic_to_tool_uses_snake_case_input_schema() -> None:
    tool = anthropic.to_anthropic_tool(sample_descriptor())
    assert tool["name"] == "web_search"
    assert tool["description"] == "Search the web."
    assert tool["input_schema"]["type"] == "object"
    assert "strict" not in tool


def test_anthropic_lower_tool_use_serializes_input_and_carries_id() -> None:
    spec = anthropic.lower_anthropic_tool_use(
        {
            "type": "tool_use",
            "id": "toolu_xyz",
            "name": "web_search",
            "input": {"query": "mesh", "max_results": 5},
        }
    )
    assert spec.name == "web_search"
    parsed = json.loads(spec.arguments_json)
    assert parsed["query"] == "mesh"
    assert parsed["max_results"] == 5
    assert spec.provider_call_id == "toolu_xyz"


# ---------------------------------------------------------------------------
# MCP format
# ---------------------------------------------------------------------------


def test_mcp_to_tool_uses_camel_case_input_schema() -> None:
    tool = mcp.to_mcp_tool(sample_descriptor())
    assert tool["name"] == "web_search"
    assert tool["inputSchema"]["type"] == "object"


def test_mcp_lower_tools_call_leaves_provider_call_id_none() -> None:
    spec = mcp.lower_mcp_tools_call(
        {"name": "web_search", "arguments": {"query": "mesh"}}
    )
    assert spec.name == "web_search"
    parsed = json.loads(spec.arguments_json)
    assert parsed["query"] == "mesh"
    assert spec.provider_call_id is None


# ---------------------------------------------------------------------------
# Gemini format
# ---------------------------------------------------------------------------


def test_gemini_to_function_declaration_uses_parameters_field() -> None:
    decl = gemini.to_gemini_function_declaration(sample_descriptor())
    assert decl["name"] == "web_search"
    assert decl["parameters"]["type"] == "object"


def test_gemini_lower_function_call_reads_args_field() -> None:
    spec = gemini.lower_gemini_function_call(
        {"name": "web_search", "args": {"query": "mesh"}}
    )
    assert spec.name == "web_search"
    parsed = json.loads(spec.arguments_json)
    assert parsed["query"] == "mesh"
    assert spec.provider_call_id is None


# ---------------------------------------------------------------------------
# Empty-schema fallback (covers all four formats)
# ---------------------------------------------------------------------------


def test_formats_fall_back_to_empty_object_schema() -> None:
    desc = descriptor_for("no_args", description="Bare.")
    assert openai.to_openai_tool(desc)["function"]["parameters"]["type"] == "object"
    assert openai.to_openai_tool(desc)["function"]["strict"] is False
    assert anthropic.to_anthropic_tool(desc)["input_schema"]["type"] == "object"
    assert mcp.to_mcp_tool(desc)["inputSchema"]["type"] == "object"
    assert gemini.to_gemini_function_declaration(desc)["parameters"]["type"] == "object"
