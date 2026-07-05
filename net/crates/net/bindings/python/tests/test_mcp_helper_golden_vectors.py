"""Cross-language MCP bridge helper-parity fixture test
(`MCP_BRIDGE_SDK_PLAN.md` P1-P3 conformance).

Loads `crates/net/tests/cross_lang_mcp/helper_vectors.json` — the canonical
fixture the Rust source-of-truth verifier
(`adapters/mcp/tests/helper_golden_vectors.rs`) validates — and asserts the
Python `classify_mcp_server` / `lower_mcp_tool` bindings produce the same
results. A failure here means the Python binding drifted from the core.

Build the extension first:  maturin develop --features mcp
"""

import json
from pathlib import Path

import pytest

pytest.importorskip("net._net")

netmod = pytest.importorskip("net")
if not hasattr(netmod, "classify_mcp_server"):
    pytest.skip("wheel built without the `mcp` feature", allow_module_level=True)

from net import classify_mcp_server, lower_mcp_tool  # noqa: E402


def _fixture() -> dict:
    # tests/ -> python/ -> bindings/ -> net/crates/net
    root = Path(__file__).resolve().parent.parent.parent.parent
    path = root / "tests" / "cross_lang_mcp" / "helper_vectors.json"
    return json.loads(path.read_text())


FIXTURE = _fixture()


def _normalize(result: dict) -> dict:
    """Reshape a lowered DTO into the fixture's comparison shape: the
    descriptor's input_schema / output_schema JSON *strings* become parsed
    *_object values, so the comparison is by value."""
    desc = dict(result["descriptor"])
    desc["input_schema_object"] = json.loads(desc["input_schema"]) if desc.get("input_schema") else None
    desc["output_schema_object"] = json.loads(desc["output_schema"]) if desc.get("output_schema") else None
    desc.pop("input_schema", None)
    desc.pop("output_schema", None)
    return {
        "tool_id": result["tool_id"],
        "mcp_name": result["mcp_name"],
        "bridge_metadata": result["bridge_metadata"],
        "descriptor": desc,
    }


@pytest.mark.parametrize("case", FIXTURE["classify"], ids=lambda c: c["name"])
def test_classify_parity(case: dict) -> None:
    envs = list(case["envs"].items())
    got = classify_mcp_server(
        case["program"],
        case["args"],
        envs,
        case["credential_override"],
        case["force"],
    )
    assert got == case["expected_status"], case["name"]


@pytest.mark.parametrize("case", FIXTURE["lower"], ids=lambda c: c["name"])
def test_lower_parity(case: dict) -> None:
    result = json.loads(
        lower_mcp_tool(
            json.dumps(case["tool"]),
            case["server_version"],
            case["credential_status"],
            case["substitutability"],
        )
    )
    assert _normalize(result) == case["expected"], case["name"]
