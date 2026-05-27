"""Cross-language tool-format compatibility fixture test (plan T-1).

Loads ``crates/net/tests/cross_lang_tool_formats/golden_vectors.json``
— the canonical fixture pinning byte-equality across all four
tool-format translators (Rust / Node TS / Python / Go). Failure of
any case here signals cross-binding wire-format drift.

Matches the Rust verifier at
``sdk/tests/tool_format_golden_vectors.rs`` and the Node TS verifier
at ``bindings/node/test/tool_format_golden_vectors.test.ts``.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest

from net.tool import (
    ToolCallParseError,
    ToolDescriptor,
    anthropic,
    gemini,
    mcp,
    openai,
)

FIXTURE_PATH = (
    Path(__file__).resolve().parent.parent.parent.parent
    / "tests"
    / "cross_lang_tool_formats"
    / "golden_vectors.json"
)


def load_fixture() -> dict:
    return json.loads(FIXTURE_PATH.read_text(encoding="utf-8"))


def descriptor_from_fixture(input: dict) -> ToolDescriptor:
    input_schema_obj = input.get("input_schema_object")
    output_schema_obj = input.get("output_schema_object")
    return ToolDescriptor(
        tool_id=input["tool_id"],
        name=input["name"],
        version=input["version"],
        description=input.get("description"),
        input_schema=json.dumps(input_schema_obj) if input_schema_obj is not None else None,
        output_schema=json.dumps(output_schema_obj) if output_schema_obj is not None else None,
        requires=list(input["requires"]),
        estimated_time_ms=input["estimated_time_ms"],
        stateless=input["stateless"],
        streaming=input["streaming"],
        tags=list(input["tags"]),
        node_count=input["node_count"],
    )


FIXTURE = load_fixture()


@pytest.mark.parametrize(
    "case",
    FIXTURE["descriptors"],
    ids=lambda c: c["name"],
)
def test_descriptor_lowerings_match_golden_vectors(case: dict) -> None:
    desc = descriptor_from_fixture(case["input"])
    assert openai.to_openai_tool(desc) == case["lowered_openai"]
    assert anthropic.to_anthropic_tool(desc) == case["lowered_anthropic"]
    assert mcp.to_mcp_tool(desc) == case["lowered_mcp"]
    assert gemini.to_gemini_function_declaration(desc) == case["lowered_gemini"]


def assert_lower_spec(spec: Any, expected: dict) -> None:
    assert spec.name == expected["name"]
    if "arguments_json" in expected:
        assert spec.arguments_json == expected["arguments_json"]
    if "arguments_parsed" in expected:
        assert json.loads(spec.arguments_json) == expected["arguments_parsed"]
    want_id = expected.get("provider_call_id")
    if want_id is None:
        assert spec.provider_call_id is None
    else:
        assert spec.provider_call_id == want_id


@pytest.mark.parametrize("case", FIXTURE["lower_openai_cases"], ids=lambda c: c["name"])
def test_lower_openai_matches_golden_vectors(case: dict) -> None:
    spec = openai.lower_openai_tool_call(case["reply_json"])
    assert_lower_spec(spec, case["expected_spec"])


@pytest.mark.parametrize("case", FIXTURE["lower_anthropic_cases"], ids=lambda c: c["name"])
def test_lower_anthropic_matches_golden_vectors(case: dict) -> None:
    spec = anthropic.lower_anthropic_tool_use(case["reply_json"])
    assert_lower_spec(spec, case["expected_spec"])


@pytest.mark.parametrize("case", FIXTURE["lower_mcp_cases"], ids=lambda c: c["name"])
def test_lower_mcp_matches_golden_vectors(case: dict) -> None:
    spec = mcp.lower_mcp_tools_call(case["reply_json"])
    assert_lower_spec(spec, case["expected_spec"])


@pytest.mark.parametrize("case", FIXTURE["lower_gemini_cases"], ids=lambda c: c["name"])
def test_lower_gemini_matches_golden_vectors(case: dict) -> None:
    spec = gemini.lower_gemini_function_call(case["reply_json"])
    assert_lower_spec(spec, case["expected_spec"])


@pytest.mark.parametrize("case", FIXTURE["error_cases"], ids=lambda c: c["name"])
def test_error_cases_all_reject(case: dict) -> None:
    provider = case["provider"]
    reply = case["reply_json"]
    dispatchers = {
        "openai": openai.lower_openai_tool_call,
        "anthropic": anthropic.lower_anthropic_tool_use,
        "mcp": mcp.lower_mcp_tools_call,
        "gemini": gemini.lower_gemini_function_call,
    }
    with pytest.raises(ToolCallParseError):
        dispatchers[provider](reply)
