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


def test_validate_handles_non_string_metadata_without_crashing() -> None:
    """Regression: the metadata size accounting used to call
    ``len(v)`` directly. Non-string values (``int`` / ``bool`` /
    ``None``) raised ``TypeError`` before any report could ship —
    a malformed-input case escalated from a warning to an
    uncaught exception. Coerce both halves to ``str`` so the
    oversize check stays robust against whatever Python type
    smuggles through an untyped ``dict``.
    """
    # Mix of types that all need ``str()`` to survive ``len()``.
    caps = {
        "tags": [],
        "metadata": {
            "int_value": 42,        # type: ignore[dict-item]
            "bool_value": True,     # type: ignore[dict-item]
            "none_value": None,     # type: ignore[dict-item]
            "str_value": "hello",
        },
    }
    # The substrate's contract is that this returns a report; it
    # must NOT raise. Pre-fix this raised
    # ``TypeError: object of type 'int' has no len()``.
    report = validate_capabilities(caps)
    # No oversize warning at this size, regardless of types.
    assert all(w.to_wire()["kind"] != "metadata_oversize" for w in report.warnings)


def test_metadata_reserved_exact_match_warns() -> None:
    """P2-L: mirror substrate CR-14 — schema's
    ``metadata_reserved`` (exact-match: ``intent``,
    ``colocate-with``, ``priority``, ``owner``,
    ``colocate-with-strict``) must be flagged in the report.
    Pre-fix the validator never consulted the list, so user
    code shadowing scheduler hints emitted no warning.
    """
    from net_sdk.capability_schema import WarningMetadataReservedKey

    caps = {"tags": [], "metadata": {"intent": "ml-training", "benign": "ok"}}
    report = validate_capabilities(caps)
    reserved = [
        w for w in report.warnings if isinstance(w, WarningMetadataReservedKey)
    ]
    assert len(reserved) == 1
    assert reserved[0].key == "intent"


def test_metadata_reserved_prefix_warns() -> None:
    """P2-L: prefix-match reservations (``tool::*``)."""
    from net_sdk.capability_schema import WarningMetadataReservedPrefix

    caps = {
        "tags": [],
        "metadata": {"tool::evil::input_schema": "spoof"},
    }
    report = validate_capabilities(caps)
    matches = [
        w for w in report.warnings if isinstance(w, WarningMetadataReservedPrefix)
    ]
    assert len(matches) == 1
    assert matches[0].key == "tool::evil::input_schema"
    assert matches[0].prefix == "tool::"


def test_number_value_rejects_negative() -> None:
    """P1-C: substrate ``Number`` is unsigned (u64-only) — see CR-15
    and ``schema.rs::ValueType::Number``. Negative values surface
    as ``TypeMismatch`` errors on the substrate side; the Python
    validator must mirror that decision so client-side checks
    don't pass a CapabilitySet the substrate would later reject.
    """
    caps = {"tags": ["hardware.memory_mb=-1"], "metadata": {}}
    report = validate_capabilities(caps)
    mismatch = [
        e
        for e in report.errors
        if e.to_wire()["kind"] == "type_mismatch"
        and e.to_wire()["axis"] == "hardware"
        and e.to_wire()["key"] == "memory_mb"
    ]
    assert len(mismatch) == 1
    assert mismatch[0].to_wire()["actual"] == "-1"


def test_number_value_accepts_unsigned() -> None:
    """Sanity check that the negative-rejection didn't break the
    happy path — unsigned u64 values still parse cleanly.
    """
    caps = {"tags": ["hardware.memory_mb=65536"], "metadata": {}}
    report = validate_capabilities(caps)
    assert report.errors == ()


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
