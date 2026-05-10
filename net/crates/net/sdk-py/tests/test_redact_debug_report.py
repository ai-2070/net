"""Cross-binding wire-format compat for redact_metadata_keys + JSON
round-trip on PredicateDebugReport."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any, Dict, List

import pytest

from net_sdk.capability import (
    ClauseStats,
    PredicateDebugReport,
    predicate_debug_report_from_wire,
    redact_metadata_keys,
)


_NET_CRATE_ROOT = Path(__file__).resolve().parents[2]
REDACT_FIXTURE = (
    _NET_CRATE_ROOT
    / "tests"
    / "cross_lang_capability"
    / "predicate_debug_report_redacted.json"
)


def _load_fixture() -> Dict[str, Any]:
    if not REDACT_FIXTURE.exists():
        raise FileNotFoundError(
            f"redaction fixture missing at {REDACT_FIXTURE}; cross-binding test cannot run"
        )
    return json.loads(REDACT_FIXTURE.read_text(encoding="utf-8"))


def _redact_cases() -> List[Dict[str, Any]]:
    return _load_fixture()["cases"]


@pytest.mark.parametrize("case", _redact_cases(), ids=lambda c: c["name"])
def test_redact_metadata_keys_fixture(case: Dict[str, Any]) -> None:
    report = predicate_debug_report_from_wire(case["report"])
    redacted = redact_metadata_keys(report, case["redact_keys"])
    assert redacted.to_wire() == case["redacted_report"]


@pytest.mark.parametrize("case", _redact_cases(), ids=lambda c: c["name"])
def test_predicate_debug_report_from_wire_round_trips(case: Dict[str, Any]) -> None:
    report = predicate_debug_report_from_wire(case["report"])
    again = predicate_debug_report_from_wire(report.to_wire())
    assert again.to_wire() == case["report"]


def test_redaction_is_idempotent() -> None:
    report = PredicateDebugReport(
        total_candidates=4,
        matched=2,
        clause_stats=(
            ClauseStats("MetadataEquals(intent=ml-training)", 4, 2),
            ClauseStats("Exists(hardware.gpu)", 4, 3),
        ),
    )
    once = redact_metadata_keys(report, ["intent"])
    twice = redact_metadata_keys(once, ["intent"])
    assert once.to_wire() == twice.to_wire()


def test_redaction_preserves_total_candidates_and_matched() -> None:
    report = PredicateDebugReport(
        total_candidates=100,
        matched=42,
        clause_stats=(ClauseStats("MetadataEquals(intent=ml-training)", 100, 42),),
    )
    out = redact_metadata_keys(report, ["intent"])
    assert out.total_candidates == 100
    assert out.matched == 42


def test_predicate_debug_report_from_wire_rejects_missing_fields() -> None:
    with pytest.raises(ValueError):
        predicate_debug_report_from_wire({})
    with pytest.raises(ValueError):
        predicate_debug_report_from_wire(
            {"total_candidates": 1, "matched": 0, "clause_stats": [{"label": "X"}]}
        )


def test_predicate_debug_report_from_wire_rejects_non_mapping() -> None:
    with pytest.raises(TypeError):
        predicate_debug_report_from_wire(None)  # type: ignore[arg-type]
    with pytest.raises(TypeError):
        predicate_debug_report_from_wire(42)  # type: ignore[arg-type]
