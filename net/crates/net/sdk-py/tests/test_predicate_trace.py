"""Cross-binding wire-format compat for the Python trace evaluator."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any, Dict, List

import pytest

from net_sdk.capability import (
    ClauseTrace,
    evaluate_predicate_with_trace,
    p,
    predicate_from_wire,
    tag_key,
)


_NET_CRATE_ROOT = Path(__file__).resolve().parents[2]
TRACE_FIXTURE = (
    _NET_CRATE_ROOT / "tests" / "cross_lang_capability" / "predicate_trace.json"
)


def _load_fixture() -> Dict[str, Any]:
    if not TRACE_FIXTURE.exists():
        raise FileNotFoundError(
            f"trace fixture missing at {TRACE_FIXTURE}; cross-binding test cannot run"
        )
    return json.loads(TRACE_FIXTURE.read_text(encoding="utf-8"))


def _trace_cases() -> List[Dict[str, Any]]:
    return _load_fixture()["cases"]


@pytest.mark.parametrize("case", _trace_cases(), ids=lambda c: c["name"])
def test_predicate_trace_fixture(case: Dict[str, Any]) -> None:
    pred = predicate_from_wire(case["wire"])
    result, trace = evaluate_predicate_with_trace(pred, case["tags"], case["metadata"])
    assert result is case["expected_result"]
    assert trace.to_wire() == case["expected_trace"]


def test_and_short_circuits_on_first_false() -> None:
    pred = p.and_(
        p.semver_compatible(tag_key("software", "runtime.python"), "3.11.0"),
        p.metadata_equals("intent", "no-match"),
    )
    result, trace = evaluate_predicate_with_trace(
        pred, ["software.runtime.python=3.11.5"], {}
    )
    assert result is False
    # Cost-ordered: metadata_equals (cost 11) wins; semver (60) skipped.
    assert len(trace.children) == 1
    assert trace.children[0].label.startswith("MetadataEquals")


def test_not_keeps_inner_as_single_child() -> None:
    pred = p.not_(p.exists(tag_key("hardware", "gpu")))
    result, trace = evaluate_predicate_with_trace(pred, [], {})
    assert result is True
    assert trace.label == "Not"
    assert len(trace.children) == 1
    assert trace.children[0].label == "Exists(hardware.gpu)"
    assert trace.children[0].result is False


def test_label_format_for_string_prefix_uses_rust_dbg_quoting() -> None:
    pred = p.string_prefix(tag_key("software", "os"), "linux")
    _result, trace = evaluate_predicate_with_trace(pred, [], {})
    assert trace.label == 'StringPrefix(software.os starts with "linux")'


def test_clause_trace_to_wire_round_trips() -> None:
    pred = p.and_(
        p.exists(tag_key("hardware", "gpu")),
        p.metadata_equals("intent", "ml-training"),
    )
    _result, trace = evaluate_predicate_with_trace(
        pred, ["hardware.gpu"], {"intent": "ml-training"}
    )
    wire = trace.to_wire()
    assert wire["label"].startswith("And(")
    assert wire["result"] is True
    assert isinstance(wire["children"], list)
