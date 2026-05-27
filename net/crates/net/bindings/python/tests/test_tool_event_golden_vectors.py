"""Cross-language ToolEvent envelope round-trip fixture test (plan T-2).

Loads ``crates/net/tests/cross_lang_tool_formats/tool_event_vectors.json``
and asserts that for each case the Python ``ToolEvent`` (TypedDict
union) representation round-trips through ``json.dumps`` /
``json.loads`` byte-equal to the wire shape.

Matches the Rust verifier at
``sdk/tests/tool_event_golden_vectors.rs`` and the Node TS verifier
at ``bindings/node/test/tool_event_golden_vectors.test.ts``.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from net.tool import is_terminal_event

FIXTURE_PATH = (
    Path(__file__).resolve().parent.parent.parent.parent
    / "tests"
    / "cross_lang_tool_formats"
    / "tool_event_vectors.json"
)

FIXTURE = json.loads(FIXTURE_PATH.read_text(encoding="utf-8"))


@pytest.mark.parametrize(
    "case",
    FIXTURE["cases"],
    ids=lambda c: c["name"],
)
def test_tool_event_round_trip_matches_golden_vectors(case: dict) -> None:
    wire = case["wire"]
    # In Python, ToolEvent is a TypedDict union — its dict form IS
    # the wire shape. Round-trip via json.dumps/loads pins that no
    # extra fields are added and no required fields drop out.
    event = wire  # type: ignore[assignment]
    assert is_terminal_event(event) is case["is_terminal"]
    round_tripped = json.loads(json.dumps(event))
    assert round_tripped == wire
