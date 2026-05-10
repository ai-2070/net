"""Cross-binding wire-format compat for the Python debug-report aggregator."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any, Dict, List

import pytest

from net_sdk.capability import (
    p,
    predicate_debug_report,
    predicate_from_wire,
    tag_key,
)


_NET_CRATE_ROOT = Path(__file__).resolve().parents[2]
REPORT_FIXTURE = (
    _NET_CRATE_ROOT
    / "tests"
    / "cross_lang_capability"
    / "predicate_debug_report.json"
)


def _load_fixture() -> Dict[str, Any]:
    if not REPORT_FIXTURE.exists():
        raise FileNotFoundError(
            f"report fixture missing at {REPORT_FIXTURE}; cross-binding test cannot run"
        )
    return json.loads(REPORT_FIXTURE.read_text(encoding="utf-8"))


def _report_cases() -> List[Dict[str, Any]]:
    return _load_fixture()["cases"]


@pytest.mark.parametrize("case", _report_cases(), ids=lambda c: c["name"])
def test_predicate_debug_report_fixture(case: Dict[str, Any]) -> None:
    pred = predicate_from_wire(case["wire"])
    report = predicate_debug_report(pred, case["contexts"])
    wire = report.to_wire()
    assert wire["total_candidates"] == case["expected_total_candidates"]
    assert wire["matched"] == case["expected_matched"]
    assert wire["clause_stats"] == case["expected_clause_stats"]


def test_clause_stats_sorted_alphabetically() -> None:
    pred = p.and_(
        p.exists(tag_key("hardware", "gpu")),
        p.metadata_equals("intent", "ml-training"),
    )
    report = predicate_debug_report(
        pred,
        [
            {"tags": ["hardware.gpu"], "metadata": {"intent": "ml-training"}},
            {"tags": [], "metadata": {}},
        ],
    )
    labels = [s.label for s in report.clause_stats]
    assert labels == sorted(labels)


def test_structurally_equal_clauses_merge_by_label() -> None:
    pred = p.or_(
        p.exists(tag_key("hardware", "gpu")),
        p.exists(tag_key("hardware", "gpu")),
    )
    report = predicate_debug_report(pred, [{"tags": [], "metadata": {}}])
    matches = [s for s in report.clause_stats if s.label == "Exists(hardware.gpu)"]
    assert len(matches) == 1
    assert matches[0].evaluated == 2
    assert matches[0].matched == 0


def test_render_is_multi_line_summary() -> None:
    pred = p.exists(tag_key("hardware", "gpu"))
    report = predicate_debug_report(
        pred,
        [
            {"tags": ["hardware.gpu"], "metadata": {}},
            {"tags": [], "metadata": {}},
        ],
    )
    text = report.render()
    assert "Predicate evaluation report" in text
    assert "Total candidates: 2" in text
    assert "Matched:          1" in text
    assert "Exists(hardware.gpu)" in text


def test_empty_corpus_yields_zeros() -> None:
    pred = p.exists(tag_key("hardware", "gpu"))
    report = predicate_debug_report(pred, [])
    assert report.total_candidates == 0
    assert report.matched == 0
    assert report.clause_stats == ()


def test_debug_label_handles_nan_and_infinity_thresholds() -> None:
    """Regression: ``_format_float`` used to call ``int(n)`` before
    the ``abs(n) < 1e16`` magnitude guard. ``int(NaN)`` raises
    ``ValueError`` and ``int(inf)`` raises ``OverflowError``, so a
    predicate with a non-finite numeric threshold made the
    debug-report path crash on label generation. The fix runs the
    finiteness/magnitude check first; non-finite or huge values
    fall through to ``repr``.
    """
    import math

    # NaN threshold — pre-fix: ``int(nan)`` → ``ValueError``.
    pred = p.numeric_at_least(tag_key("hardware", "vram_gb"), float("nan"))
    report = predicate_debug_report(pred, [{"tags": [], "metadata": {}}])
    assert len(report.clause_stats) == 1
    assert "nan" in report.clause_stats[0].label.lower()

    # Positive infinity — pre-fix: ``int(inf)`` → ``OverflowError``.
    pred = p.numeric_at_most(tag_key("hardware", "vram_gb"), math.inf)
    report = predicate_debug_report(pred, [{"tags": [], "metadata": {}}])
    assert len(report.clause_stats) == 1
    assert "inf" in report.clause_stats[0].label.lower()
