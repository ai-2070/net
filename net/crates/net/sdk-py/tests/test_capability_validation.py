"""Cross-binding wire-format compat for ``validate_capabilities``."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any, Dict, List

import pytest

from net_sdk.capability_schema import (
    AXIS_SCHEMA,
    METADATA_SOFT_CAP_BYTES,
    AxisEntry,
    SchemaErrorTypeMismatch,
    ValidationReport,
    WarningLegacyTag,
    validate_capabilities,
)


# ---------------------------------------------------------------------------
# Fixture loader
# ---------------------------------------------------------------------------

_NET_CRATE_ROOT = Path(__file__).resolve().parents[2]
VALIDATION_FIXTURE = (
    _NET_CRATE_ROOT
    / "tests"
    / "cross_lang_capability"
    / "capability_validation.json"
)


def _load_fixture() -> Dict[str, Any]:
    if not VALIDATION_FIXTURE.exists():
        raise FileNotFoundError(
            f"validation fixture missing at {VALIDATION_FIXTURE}; "
            f"cross-binding test cannot run"
        )
    return json.loads(VALIDATION_FIXTURE.read_text(encoding="utf-8"))


def _validation_cases() -> List[Dict[str, Any]]:
    return _load_fixture()["cases"]


def _sorted_by_json(arr: List[Any]) -> List[Any]:
    return sorted(arr, key=lambda x: json.dumps(x, sort_keys=True))


# ---------------------------------------------------------------------------
# Cross-binding fixture cases
# ---------------------------------------------------------------------------


def test_validation_fixture_soft_cap_matches_constant() -> None:
    fx = _load_fixture()
    assert fx["schema_metadata_soft_cap_bytes"] == METADATA_SOFT_CAP_BYTES


@pytest.mark.parametrize("case", _validation_cases(), ids=lambda c: c["name"])
def test_validation_fixture(case: Dict[str, Any]) -> None:
    report = validate_capabilities(case["caps"])
    wire = report.to_wire()
    assert _sorted_by_json(wire["errors"]) == _sorted_by_json(case["expected_errors"])
    assert _sorted_by_json(wire["warnings"]) == _sorted_by_json(
        case["expected_warnings"]
    )


# ---------------------------------------------------------------------------
# Local unit tests
# ---------------------------------------------------------------------------


def test_axis_schema_hardware_keys_present() -> None:
    keys = [e.key for e in AXIS_SCHEMA.hardware.keys]
    for required in ("cpu_cores", "memory_mb", "gpu", "gpu.vendor"):
        assert required in keys


def test_axis_schema_software_shapes_present() -> None:
    prefixes = [s.prefix for s in AXIS_SCHEMA.software.shapes]
    assert sorted(prefixes) == sorted(
        ["runtime.", "framework.", "driver.", "model.", "tool."]
    )


def test_devices_and_dataforts_axes_are_reserved_empty() -> None:
    for axis in (AXIS_SCHEMA.devices, AXIS_SCHEMA.dataforts):
        assert axis.keys == ()
        assert axis.shapes == ()


def test_metadata_oversize_warning_fires() -> None:
    big = "x" * (METADATA_SOFT_CAP_BYTES + 100)
    caps = {"tags": [], "metadata": {"padding": big}}
    report = validate_capabilities(caps)
    assert report.errors == ()
    matching = [w for w in report.warnings if w.to_wire()["kind"] == "metadata_oversize"]
    assert len(matching) == 1
    assert matching[0].to_wire()["soft_cap_bytes"] == METADATA_SOFT_CAP_BYTES
    assert matching[0].to_wire()["actual_bytes"] == len("padding") + len(big)


def test_metadata_oversize_does_not_fire_at_cap() -> None:
    value = "x" * (METADATA_SOFT_CAP_BYTES - len("k"))
    caps = {"tags": [], "metadata": {"k": value}}
    report = validate_capabilities(caps)
    assert report.warnings == ()


def test_report_is_clean_helpers() -> None:
    clean = ValidationReport()
    assert clean.is_clean()
    assert clean.is_valid()

    warned = ValidationReport(warnings=(WarningLegacyTag(tag="foo"),))
    assert not warned.is_clean()
    assert warned.is_valid()

    errored = ValidationReport(
        errors=(
            SchemaErrorTypeMismatch(
                axis="hardware", key="memory_mb", expected="number", actual="lots"
            ),
        )
    )
    assert not errored.is_clean()
    assert not errored.is_valid()
