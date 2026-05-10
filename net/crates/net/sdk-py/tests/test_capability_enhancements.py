"""
Cross-binding wire-format compat for the Python Capability-System
Enhancements surface. Drives the same JSON fixtures the Rust + TS
tests consume (under ``net/crates/net/tests/cross_lang_capability``)
so all bindings agree byte-for-byte on the predicate envelope and
``CapabilitySet::diff`` output.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any, Dict, List

import pytest

from net_sdk.capability import (
    RESERVED_PREFIXES,
    RPC_WHERE_HEADER,
    PlacementCandidate,
    diff_capabilities,
    empty_capabilities,
    evaluate_predicate,
    p,
    placement_filter_from_fn,
    predicate_from_rpc_header,
    predicate_from_wire,
    predicate_to_rpc_header,
    predicate_to_wire,
    require_axis_value,
    require_tag,
    standard_placement,
    tag_from_string,
    tag_from_user_string,
    tag_key,
    tag_to_string,
    with_metadata,
)
from net_sdk.capability import (  # noqa: E501 — split for clarity
    TagAxisPresent,
    TagAxisValue,
    TagLegacy,
    TagReserved,
)


# ---------------------------------------------------------------------------
# Fixture loaders
# ---------------------------------------------------------------------------

# tests/test_capability_enhancements.py → up 2 levels to the
# `crates/net/` directory which hosts the cross-binding fixtures.
_NET_CRATE_ROOT = Path(__file__).resolve().parents[2]
PREDICATE_FIXTURE = (
    _NET_CRATE_ROOT / "tests" / "cross_lang_capability" / "predicate_nrpc_envelope.json"
)
DIFF_FIXTURE = (
    _NET_CRATE_ROOT / "tests" / "cross_lang_capability" / "capability_set_diff.json"
)
EVAL_FIXTURE = (
    _NET_CRATE_ROOT / "tests" / "cross_lang_capability" / "predicate_eval.json"
)


def _load_json(path: Path, label: str) -> Dict[str, Any]:
    if not path.exists():
        raise FileNotFoundError(
            f"{label} fixture missing at {path}; cross-binding tests cannot run"
        )
    return json.loads(path.read_text(encoding="utf-8"))


# ---------------------------------------------------------------------------
# Predicate envelope round-trip
# ---------------------------------------------------------------------------


def _predicate_cases() -> List[Dict[str, Any]]:
    return _load_json(PREDICATE_FIXTURE, "predicate envelope")["cases"]


def test_predicate_fixture_header_matches() -> None:
    fx = _load_json(PREDICATE_FIXTURE, "predicate envelope")
    assert fx["header_name"] == RPC_WHERE_HEADER


@pytest.mark.parametrize("case", _predicate_cases(), ids=lambda c: c["name"])
def test_predicate_fixture_round_trip(case: Dict[str, Any]) -> None:
    wire = case["wire"]
    ast = predicate_from_wire(wire)
    re_emitted = predicate_to_wire(ast)
    assert re_emitted == wire

    header_val = json.dumps(wire)
    from_header = predicate_from_rpc_header(header_val)
    assert predicate_to_wire(from_header) == wire


# ---------------------------------------------------------------------------
# CapabilitySet diff
# ---------------------------------------------------------------------------


def _diff_cases() -> List[Dict[str, Any]]:
    return _load_json(DIFF_FIXTURE, "capability-set diff")["cases"]


def _eval_cases() -> List[Dict[str, Any]]:
    return _load_json(EVAL_FIXTURE, "predicate eval")["cases"]


@pytest.mark.parametrize("case", _eval_cases(), ids=lambda c: c["name"])
def test_predicate_eval_fixture(case: Dict[str, Any]) -> None:
    pred = predicate_from_wire(case["wire"])
    got = evaluate_predicate(pred, case["tags"], case["metadata"])
    assert got is case["expected"]


@pytest.mark.parametrize("case", _diff_cases(), ids=lambda c: c["name"])
def test_capability_set_diff_fixture(case: Dict[str, Any]) -> None:
    got = diff_capabilities(case["prev"], case["curr"]).to_wire()
    assert got["added_tags"] == case["expected_added_tags"]
    assert got["removed_tags"] == case["expected_removed_tags"]
    assert got["metadata_changes"] == case["expected_metadata_changes"]


# ---------------------------------------------------------------------------
# Typed taxonomy
# ---------------------------------------------------------------------------


def test_axis_present_round_trip() -> None:
    tag = tag_from_string("hardware.gpu")
    assert isinstance(tag, TagAxisPresent)
    assert tag.axis == "hardware"
    assert tag.key == "gpu"
    assert tag_to_string(tag) == "hardware.gpu"


def test_axis_value_round_trip_eq() -> None:
    tag = tag_from_string("software.os=linux")
    assert isinstance(tag, TagAxisValue)
    assert tag.axis == "software"
    assert tag.key == "os"
    assert tag.value == "linux"
    assert tag.separator == "="
    assert tag_to_string(tag) == "software.os=linux"


def test_axis_value_round_trip_colon() -> None:
    tag = tag_from_string("dataforts.region:us-east")
    assert isinstance(tag, TagAxisValue)
    assert tag.axis == "dataforts"
    assert tag.separator == ":"
    assert tag_to_string(tag) == "dataforts.region:us-east"


def test_reserved_prefix_routes() -> None:
    for prefix in RESERVED_PREFIXES:
        wire = f"{prefix}value"
        tag = tag_from_string(wire)
        assert isinstance(tag, TagReserved)
        assert tag.prefix == prefix
        assert tag.body == "value"
        assert tag_to_string(tag) == wire


def test_legacy_fallback() -> None:
    assert isinstance(tag_from_string("myteam-tag"), TagLegacy)
    assert isinstance(tag_from_string("unknown-axis.key"), TagLegacy)


def test_tag_from_user_string_rejects_reserved() -> None:
    for prefix in RESERVED_PREFIXES:
        with pytest.raises(ValueError, match="reserved prefix"):
            tag_from_user_string(f"{prefix}value")


def test_tag_key_rejects_empty() -> None:
    with pytest.raises(ValueError):
        tag_key("hardware", "")


# ---------------------------------------------------------------------------
# Chain composition
# ---------------------------------------------------------------------------


def test_require_tag_idempotent() -> None:
    caps = empty_capabilities()
    caps = require_tag(caps, "hardware", "gpu")
    caps = require_tag(caps, "hardware", "gpu")
    assert caps.tags == ("hardware.gpu",)


def test_require_axis_value_idempotent() -> None:
    caps = empty_capabilities()
    caps = require_axis_value(caps, "software", "os", "linux")
    caps = require_axis_value(caps, "software", "os", "linux")
    assert caps.tags == ("software.os=linux",)


def test_require_axis_value_colon() -> None:
    caps = require_axis_value(
        empty_capabilities(), "dataforts", "region", "us-east", ":"
    )
    assert caps.tags == ("dataforts.region:us-east",)


def test_with_metadata_does_not_mutate_input() -> None:
    a = empty_capabilities()
    b = with_metadata(a, "intent", "ml-training")
    assert a.metadata == {}
    assert b.metadata == {"intent": "ml-training"}


def test_chain_compose_left_to_right() -> None:
    caps = with_metadata(
        require_axis_value(
            require_tag(empty_capabilities(), "hardware", "gpu"),
            "software",
            "os",
            "linux",
        ),
        "intent",
        "ml-training",
    )
    assert sorted(caps.tags) == ["hardware.gpu", "software.os=linux"]
    assert caps.metadata == {"intent": "ml-training"}


# ---------------------------------------------------------------------------
# Predicate fluent builder
# ---------------------------------------------------------------------------


def test_predicate_complex_round_trip() -> None:
    pred = p.and_(
        p.or_(
            p.exists(tag_key("hardware", "gpu")),
            p.and_(
                p.numeric_at_least(tag_key("hardware", "memory_mb"), 65536),
                p.metadata_exists("intent"),
            ),
        ),
        p.not_(p.metadata_equals("decommissioning", "true")),
        p.semver_at_least(tag_key("software", "runtime.python"), "3.10.0"),
    )
    wire = predicate_to_wire(pred)
    assert predicate_to_wire(predicate_from_wire(wire)) == wire
    assert wire["nodes"][wire["root_idx"]]["kind"] == "and"


def test_predicate_from_wire_rejects_forward_child() -> None:
    bad = {
        "nodes": [
            {"kind": "and", "children": [1]},
            {"kind": "metadata_exists", "key": "x"},
        ],
        "root_idx": 0,
    }
    with pytest.raises(ValueError, match="strictly less"):
        predicate_from_wire(bad)


def test_predicate_from_wire_rejects_out_of_range_root() -> None:
    bad = {"nodes": [{"kind": "metadata_exists", "key": "x"}], "root_idx": 99}
    with pytest.raises(ValueError, match="root_idx"):
        predicate_from_wire(bad)


def test_predicate_to_rpc_header_canonical_json() -> None:
    pred = p.exists(tag_key("hardware", "gpu"))
    header = predicate_to_rpc_header(pred)
    assert json.loads(header) == {
        "nodes": [{"kind": "exists", "key": {"axis": "hardware", "key": "gpu"}}],
        "root_idx": 0,
    }


def test_where_header_builds_canonical_entry() -> None:
    """Phase 9b: ``where_header(pred)`` returns ``(name, bytes)``
    suitable for the ``request_headers`` opts list."""
    from net_sdk import RPC_WHERE_HEADER, where_header

    pred = p.exists(tag_key("hardware", "gpu"))
    name, value = where_header(pred)
    assert name == RPC_WHERE_HEADER
    assert isinstance(value, bytes)
    assert value.decode("utf-8") == predicate_to_rpc_header(pred)


# ---------------------------------------------------------------------------
# P1-D: semver_compatible 0.0.x exact-only.
#
# Cargo's caret rule treats `^0.0.x` as exact-only — every patch
# is a breaking-change boundary. Pre-fix the Python helper applied
# the 0.x.y minor-band rule even when the major was 0 AND the
# minor was 0, so 0.0.4 satisfied a 0.0.3 requirement (it
# shouldn't). Mirrors the Rust CR pinned in
# `predicate.rs::semver_compatible_zero_zero_patch_is_exact_only`.
# ---------------------------------------------------------------------------


def test_semver_compatible_zero_zero_patch_is_exact_only() -> None:
    from net_sdk.capability import _semver_compatible

    # 0.0.x band: every patch is a breaking change.
    assert _semver_compatible((0, 0, 3), (0, 0, 3)) is True
    assert _semver_compatible((0, 0, 4), (0, 0, 3)) is False
    assert _semver_compatible((0, 0, 2), (0, 0, 3)) is False
    # 0.x.y (x > 0) band: minor is the compatibility band.
    assert _semver_compatible((0, 2, 5), (0, 2, 3)) is True
    assert _semver_compatible((0, 2, 3), (0, 3, 0)) is False
    # x.y.z (x > 0) band: major is the compatibility band.
    assert _semver_compatible((1, 4, 5), (1, 2, 3)) is True
    assert _semver_compatible((2, 0, 0), (1, 9, 9)) is False


def test_numeric_predicate_rejects_whitespace_padded_values() -> None:
    """R2: Rust's ``f64::from_str`` rejects leading / trailing
    whitespace; Python's ``float("  1.5")`` strips it. A value
    like ``"  1500"`` parsed cleanly in Python (passing the
    predicate) but failed parse in Rust (predicate returns
    False). Cross-binding evaluation must agree.
    """
    metadata: Dict[str, str] = {}
    # Leading whitespace.
    tags = ["software.runtime.python=  1500"]
    assert evaluate_predicate(
        p.numeric_at_least(tag_key("software", "runtime.python"), 1000.0),
        tags,
        metadata,
    ) is False
    # Trailing whitespace.
    tags = ["software.runtime.python=1500  "]
    assert evaluate_predicate(
        p.numeric_at_least(tag_key("software", "runtime.python"), 1000.0),
        tags,
        metadata,
    ) is False
    # Sanity: clean value still parses.
    tags = ["software.runtime.python=1500"]
    assert evaluate_predicate(
        p.numeric_at_least(tag_key("software", "runtime.python"), 1000.0),
        tags,
        metadata,
    ) is True


def test_numeric_predicate_accepts_scientific_notation() -> None:
    """Q15: Rust's `value.parse::<f64>()` accepts scientific
    notation (`1e10`, `1.5e-3`); pre-fix the Python regex
    `r"^-?\\d+(\\.\\d+)?$"` rejected them, diverging numeric-
    evaluation semantics. A predicate against a tag value of
    `"1.5e3"` (= 1500) silently failed in Python while passing
    in Rust.
    """
    metadata: Dict[str, str] = {}
    # NumericAtLeast against 1.5e3 (= 1500) tag value.
    tags = ["software.runtime.python=1.5e3"]
    assert evaluate_predicate(
        p.numeric_at_least(tag_key("software", "runtime.python"), 1500.0),
        tags,
        metadata,
    ) is True
    assert evaluate_predicate(
        p.numeric_at_least(tag_key("software", "runtime.python"), 1501.0),
        tags,
        metadata,
    ) is False
    # NumericInRange with scientific notation.
    tags = ["hardware.memory_mb=2.5e4"]  # 25000
    assert evaluate_predicate(
        p.numeric_in_range(tag_key("hardware", "memory_mb"), 20000.0, 30000.0),
        tags,
        metadata,
    ) is True


def test_axis_present_tag_does_not_match_value_predicates() -> None:
    """Q2: ``Tag::AxisPresent`` (e.g. ``hardware.gpu``) must NOT
    match value-bearing predicates like ``Equals``, ``StringPrefix``,
    ``NumericAtLeast``. Pre-fix ``_axis_tag_value`` returned ``""``
    for AxisPresent, so ``Equals(_, "")`` / ``StringPrefix(_, "")``
    spuriously matched any presence tag — diverged from the Rust
    substrate which requires `Tag::AxisValue` for those predicates.

    `Exists`, on the other hand, must continue to match AxisPresent.
    """
    tags = ["hardware.gpu"]  # AxisPresent
    metadata: Dict[str, str] = {}

    # Exists matches AxisPresent (sanity).
    assert evaluate_predicate(
        p.exists(tag_key("hardware", "gpu")), tags, metadata
    ) is True

    # Equals against the empty string must NOT match AxisPresent.
    assert evaluate_predicate(
        p.equals(tag_key("hardware", "gpu"), ""), tags, metadata
    ) is False

    # StringPrefix with empty string must NOT match AxisPresent.
    assert evaluate_predicate(
        p.string_prefix(tag_key("hardware", "gpu"), ""), tags, metadata
    ) is False

    # AxisValue still works for value predicates.
    tags_v = ["hardware.gpu.vram_mb=80000"]
    assert evaluate_predicate(
        p.equals(tag_key("hardware", "gpu.vram_mb"), "80000"), tags_v, metadata
    ) is True


def test_axis_value_wins_over_presence_when_both_present() -> None:
    """Q2: when a node carries both ``hardware.gpu`` (AxisPresent)
    AND ``hardware.gpu=h100`` (AxisValue), value predicates must
    consult the AxisValue tag — pre-fix `_axis_tag_value` returned
    on the first match (the AxisPresent's empty-string), short-
    circuiting and missing the value tag.
    """
    # Both tags present. Predicate looking for value 'h100' should
    # match the AxisValue tag.
    tags = ["hardware.gpu", "hardware.gpu=h100"]
    metadata: Dict[str, str] = {}
    assert evaluate_predicate(
        p.equals(tag_key("hardware", "gpu"), "h100"), tags, metadata
    ) is True


def test_semver_compatible_zero_x_band_requires_lhs_major_zero() -> None:
    """Q1: a non-zero major lhs is NOT compatible with a 0.x.y rhs.
    Pre-fix `rhs[1] == lhs[1]` alone passed for `lhs = (1, 2, 5)`
    against `rhs = (0, 2, 3)` (lhs >= rhs since 1 > 0; minors
    match). Cargo's caret rule treats 0.x.y as the band IFF the
    band itself is 0.x.y — 1.x.y running against ^0.2.3 is a
    major-version regression.
    """
    from net_sdk.capability import _semver_compatible

    # 0.2.x band: same-major-zero, same-minor matches.
    assert _semver_compatible((0, 2, 5), (0, 2, 3)) is True
    # 0.2.x band: lhs major == 1 must NOT match (was admitted pre-fix).
    assert _semver_compatible((1, 2, 5), (0, 2, 3)) is False
    # Sanity: lhs major == 2 also fails.
    assert _semver_compatible((2, 2, 5), (0, 2, 3)) is False


# ---------------------------------------------------------------------------
# StandardPlacement builder + custom placement filter
# ---------------------------------------------------------------------------


def test_standard_placement_builder() -> None:
    cfg = (
        standard_placement()
        .require_tag("hardware", "gpu")
        .require_axis_value("software", "os", "linux")
        .forbid_tag("hardware", "decommissioned")
        .require_metadata("intent", "ml-training")
        .with_predicate(p.metadata_exists("owner"))
        .with_limit(3)
        .with_custom_filter_id("placement-foo")
        .build()
    )
    assert cfg.require_tags == ("hardware.gpu", "software.os=linux")
    assert cfg.forbid_tags == ("hardware.decommissioned",)
    assert cfg.require_metadata == {"intent": "ml-training"}
    assert cfg.predicate is not None
    assert cfg.predicate["nodes"][0] == {"kind": "metadata_exists", "key": "owner"}
    assert cfg.limit == 3
    assert cfg.custom_filter_id == "placement-foo"


def test_standard_placement_rejects_negative_limit() -> None:
    with pytest.raises(ValueError):
        standard_placement().with_limit(-1)


def test_standard_placement_accepts_pre_built_wire() -> None:
    wire = predicate_to_wire(p.exists(tag_key("hardware", "gpu")))
    cfg = standard_placement().with_predicate(wire).build()
    assert cfg.predicate == wire


def test_placement_filter_from_fn_auto_id() -> None:
    a = placement_filter_from_fn(lambda c: True)
    b = placement_filter_from_fn(lambda c: False)
    assert a.id != b.id
    candidate = PlacementCandidate(node_id=1, tags=(), metadata={})
    assert a.fn(candidate) is True
    assert b.fn(candidate) is False


def test_placement_filter_from_fn_explicit_id() -> None:
    f = placement_filter_from_fn(lambda c: True, "my-filter")
    assert f.id == "my-filter"


# =====================================================================
# SDK Phase 7 cross-binding compat — wrap a predicate as a
# placement-filter callback and run it against the same
# `predicate_eval.json` fixture every binding consumes. Pins that
# the Python SDK's `placement_filter_from_fn` correctly delivers
# each candidate's `(tags, metadata)` to the user closure such
# that direct `evaluate_predicate(pred, tags, metadata)` and the
# wrapped-callback path produce identical booleans.
#
# Mirror of the Rust-side
# `predicate_eval_fixture_matches_via_placement_filter_callback`
# test in `tests/cross_lang_capability_fixtures.rs`. Failures here
# vs there indicate cross-binding drift in either the predicate
# evaluator or the placement-filter helper.
# =====================================================================


@pytest.mark.parametrize("case", _eval_cases(), ids=lambda c: c["name"])
def test_predicate_eval_fixture_via_placement_filter_callback(
    case: Dict[str, Any],
) -> None:
    pred = predicate_from_wire(case["wire"])
    # Wrap the predicate evaluator as a `PlacementFilterFn`. The
    # candidate carries the case's `(tags, metadata)`; node_id is
    # arbitrary because the predicate doesn't read it.
    filt = placement_filter_from_fn(
        lambda cand: evaluate_predicate(pred, cand.tags, cand.metadata)
    )
    candidate = PlacementCandidate(
        node_id=0x1234_5678,
        tags=tuple(case["tags"]),
        metadata=dict(case["metadata"]),
    )
    assert filt.fn(candidate) is case["expected"]
