"""JSON-encoder pins for the Phase 6c capability-aggregation surface.

The Rust core's ``serde_json::to_string`` produces specific byte
sequences pinned by ``serde_shapes_match_cross_binding_wire_format``
in ``capability_aggregation.rs``. The Python encoder in
``net_sdk.capability_aggregation`` must produce the same JSON for
the PyO3 boundary to deserialize correctly. This file pins the Python
side; both sides move together when the wire shape changes.
"""

from __future__ import annotations

import json

from net_sdk.capability_aggregation import (
    AggregationCls,
    CapacityQuery,
    GroupByCls,
    TagMatcherCls,
    aggregation_to_json,
    capacity_query_to_json,
    group_by_to_json,
    tag_matcher_to_json,
)


# ──────────────────────────────────────────────────────────────────
# TagMatcher
# ──────────────────────────────────────────────────────────────────


def test_tag_matcher_exact_encodes_value():
    assert tag_matcher_to_json(TagMatcherCls.exact("software.python=3.11")) == (
        '{"value": "software.python=3.11", "kind": "exact"}'
    )


def test_tag_matcher_prefix_encodes_value():
    assert tag_matcher_to_json(TagMatcherCls.prefix("hardware.gpu")) == (
        '{"value": "hardware.gpu", "kind": "prefix"}'
    )


def test_tag_matcher_axis_encodes_axis_only():
    assert tag_matcher_to_json(TagMatcherCls.axis("hardware")) == (
        '{"axis": "hardware", "kind": "axis"}'
    )


def test_tag_matcher_axis_key_encodes_axis_and_key():
    assert tag_matcher_to_json(TagMatcherCls.axis_key("hardware", "gpu.count")) == (
        '{"axis": "hardware", "key": "gpu.count", "kind": "axis_key"}'
    )


def test_tag_matcher_regex_encodes_pattern():
    assert tag_matcher_to_json(TagMatcherCls.regex("^a$")) == (
        '{"pattern": "^a$", "kind": "regex"}'
    )


def test_tag_matcher_version_range_nulls_unset_bounds():
    j = tag_matcher_to_json(
        TagMatcherCls.version_range("software.python", min="3.10.0")
    )
    parsed = json.loads(j)
    assert parsed == {
        "axis_key": "software.python",
        "min": "3.10.0",
        "max": None,
        "kind": "version_range",
    }


# ──────────────────────────────────────────────────────────────────
# GroupBy
# ──────────────────────────────────────────────────────────────────


def test_group_by_class_kind_only():
    assert group_by_to_json(GroupByCls.class_()) == '{"kind": "class"}'


def test_group_by_state_kind_only():
    assert group_by_to_json(GroupByCls.state()) == '{"kind": "state"}'


def test_group_by_region_kind_only():
    assert group_by_to_json(GroupByCls.region()) == '{"kind": "region"}'


def test_group_by_publisher_kind_only():
    assert group_by_to_json(GroupByCls.publisher()) == '{"kind": "publisher"}'


def test_group_by_tag_stem_encodes_prefix():
    assert group_by_to_json(GroupByCls.tag_stem("hardware.gpu")) == (
        '{"prefix": "hardware.gpu", "kind": "tag_stem"}'
    )


def test_group_by_tag_value_encodes_axis_and_key():
    assert group_by_to_json(GroupByCls.tag_value("software", "python")) == (
        '{"axis": "software", "key": "python", "kind": "tag_value"}'
    )


# ──────────────────────────────────────────────────────────────────
# Aggregation
# ──────────────────────────────────────────────────────────────────


def test_aggregation_count_kind_only():
    assert aggregation_to_json(AggregationCls.count()) == '{"kind": "count"}'


def test_aggregation_distinct_publishers_kind_only():
    assert aggregation_to_json(AggregationCls.distinct_publishers()) == (
        '{"kind": "distinct_publishers"}'
    )


def test_aggregation_distinct_values_encodes_axis_and_key():
    assert aggregation_to_json(
        AggregationCls.distinct_values("software", "python")
    ) == ('{"axis": "software", "key": "python", "kind": "distinct_values"}')


def test_aggregation_sum_numeric_tag_encodes_axis_key():
    assert aggregation_to_json(
        AggregationCls.sum_numeric_tag("hardware.gpu.count")
    ) == ('{"axis_key": "hardware.gpu.count", "kind": "sum_numeric_tag"}')


def test_aggregation_min_max_numeric_tag_use_distinct_kinds():
    assert aggregation_to_json(
        AggregationCls.min_numeric_tag("hardware.gpu.count")
    ) == ('{"axis_key": "hardware.gpu.count", "kind": "min_numeric_tag"}')
    assert aggregation_to_json(
        AggregationCls.max_numeric_tag("hardware.gpu.count")
    ) == ('{"axis_key": "hardware.gpu.count", "kind": "max_numeric_tag"}')


# ──────────────────────────────────────────────────────────────────
# CapacityQuery
# ──────────────────────────────────────────────────────────────────


def test_capacity_query_round_trips_into_rust_wire_shape():
    j = capacity_query_to_json(
        CapacityQuery(
            matcher=TagMatcherCls.prefix("hardware.gpu"),
            group_by=GroupByCls.tag_stem("hardware.gpu"),
            max_rtt_ms=50,
            sum_axis_key="hardware.gpu.count",
            limit=5,
        )
    )
    parsed = json.loads(j)
    assert parsed == {
        "matcher": {
            "value": "hardware.gpu",
            "kind": "prefix",
        },
        "group_by": {
            "prefix": "hardware.gpu",
            "kind": "tag_stem",
        },
        "max_rtt_ms": 50,
        "sum_axis_key": "hardware.gpu.count",
        "limit": 5,
    }


def test_capacity_query_nulls_absent_fields():
    j = capacity_query_to_json(
        CapacityQuery(group_by=GroupByCls.region(), limit=0)
    )
    parsed = json.loads(j)
    assert parsed == {
        "matcher": None,
        "group_by": {"kind": "region"},
        "max_rtt_ms": None,
        "sum_axis_key": None,
        "limit": 0,
    }
