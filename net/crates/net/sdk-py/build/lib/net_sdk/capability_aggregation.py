"""Capability-aggregation surface — Phase 6c of
``MULTIFOLD_PHASE_6C_CAPACITY_AGGREGATION.md``.

Three composable primitives map onto the Rust core's
``Fold::aggregate`` and ``Fold::capacity_ranking``:

- :class:`TagMatcher` — picks which entries the aggregation walks.
- :class:`GroupBy` — buckets matching entries.
- :class:`Aggregation` — reduces each bucket to a number.

:class:`CapacityQuery` composes a matcher + groupBy + optional RTT
filter + optional summed-capacity axis into a single
:func:`MeshNode.capability_capacity_ranking` call; the materialized
view returns per-bucket state breakdown sorted by available capacity.

The Rust core takes JSON-encoded tagged unions over the PyO3
boundary; the helpers below handle the conversion so Python callers
work with idiomatic dataclasses.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from typing import Literal, Optional, Union

# ──────────────────────────────────────────────────────────────────
# TagMatcher
# ──────────────────────────────────────────────────────────────────

TaxonomyAxis = Literal["hardware", "software", "devices", "dataforts"]
"""Taxonomy axis name — matches the Rust core's ``TaxonomyAxis``."""


@dataclass(frozen=True)
class _MatcherExact:
    """Exact tag match. ``value`` is the literal canonical tag."""

    value: str
    kind: Literal["exact"] = "exact"


@dataclass(frozen=True)
class _MatcherPrefix:
    """Prefix match. Matches any tag starting with ``value``."""

    value: str
    kind: Literal["prefix"] = "prefix"


@dataclass(frozen=True)
class _MatcherAxis:
    """Match any tag in the given taxonomy axis."""

    axis: TaxonomyAxis
    kind: Literal["axis"] = "axis"


@dataclass(frozen=True)
class _MatcherAxisKey:
    """Match tags with the given ``(axis, key)`` regardless of value."""

    axis: TaxonomyAxis
    key: str
    kind: Literal["axis_key"] = "axis_key"


@dataclass(frozen=True)
class _MatcherRegex:
    """Regex match against the canonical tag string."""

    pattern: str
    kind: Literal["regex"] = "regex"


@dataclass(frozen=True)
class _MatcherVersionRange:
    """Semver range match against an ``AxisValue`` tag's value."""

    axis_key: str
    min: Optional[str] = None
    max: Optional[str] = None
    kind: Literal["version_range"] = "version_range"


TagMatcher = Union[
    _MatcherExact,
    _MatcherPrefix,
    _MatcherAxis,
    _MatcherAxisKey,
    _MatcherRegex,
    _MatcherVersionRange,
]
"""Pre-grouping filter. Use the :class:`TagMatcherCls` factory."""


class TagMatcherCls:
    """Factory constructors for :class:`TagMatcher` variants."""

    @staticmethod
    def exact(value: str) -> TagMatcher:
        return _MatcherExact(value=value)

    @staticmethod
    def prefix(value: str) -> TagMatcher:
        return _MatcherPrefix(value=value)

    @staticmethod
    def axis(axis: TaxonomyAxis) -> TagMatcher:
        return _MatcherAxis(axis=axis)

    @staticmethod
    def axis_key(axis: TaxonomyAxis, key: str) -> TagMatcher:
        return _MatcherAxisKey(axis=axis, key=key)

    @staticmethod
    def regex(pattern: str) -> TagMatcher:
        return _MatcherRegex(pattern=pattern)

    @staticmethod
    def version_range(
        axis_key: str,
        *,
        min: Optional[str] = None,
        max: Optional[str] = None,
    ) -> TagMatcher:
        return _MatcherVersionRange(axis_key=axis_key, min=min, max=max)


# ──────────────────────────────────────────────────────────────────
# GroupBy
# ──────────────────────────────────────────────────────────────────


@dataclass(frozen=True)
class _GroupClass:
    kind: Literal["class"] = "class"


@dataclass(frozen=True)
class _GroupState:
    kind: Literal["state"] = "state"


@dataclass(frozen=True)
class _GroupRegion:
    kind: Literal["region"] = "region"


@dataclass(frozen=True)
class _GroupPublisher:
    kind: Literal["publisher"] = "publisher"


@dataclass(frozen=True)
class _GroupTagStem:
    prefix: str
    kind: Literal["tag_stem"] = "tag_stem"


@dataclass(frozen=True)
class _GroupTagValue:
    axis: TaxonomyAxis
    key: str
    kind: Literal["tag_value"] = "tag_value"


GroupBy = Union[
    _GroupClass,
    _GroupState,
    _GroupRegion,
    _GroupPublisher,
    _GroupTagStem,
    _GroupTagValue,
]
"""Bucket-key derivation. Use the :class:`GroupByCls` factory."""


class GroupByCls:
    """Factory constructors for :class:`GroupBy` variants."""

    @staticmethod
    def class_() -> GroupBy:
        return _GroupClass()

    @staticmethod
    def state() -> GroupBy:
        return _GroupState()

    @staticmethod
    def region() -> GroupBy:
        return _GroupRegion()

    @staticmethod
    def publisher() -> GroupBy:
        return _GroupPublisher()

    @staticmethod
    def tag_stem(prefix: str) -> GroupBy:
        return _GroupTagStem(prefix=prefix)

    @staticmethod
    def tag_value(axis: TaxonomyAxis, key: str) -> GroupBy:
        return _GroupTagValue(axis=axis, key=key)


# ──────────────────────────────────────────────────────────────────
# Aggregation
# ──────────────────────────────────────────────────────────────────


@dataclass(frozen=True)
class _AggCount:
    kind: Literal["count"] = "count"


@dataclass(frozen=True)
class _AggDistinctPublishers:
    kind: Literal["distinct_publishers"] = "distinct_publishers"


@dataclass(frozen=True)
class _AggDistinctValues:
    axis: TaxonomyAxis
    key: str
    kind: Literal["distinct_values"] = "distinct_values"


@dataclass(frozen=True)
class _AggSumNumericTag:
    axis_key: str
    kind: Literal["sum_numeric_tag"] = "sum_numeric_tag"


@dataclass(frozen=True)
class _AggMinNumericTag:
    axis_key: str
    kind: Literal["min_numeric_tag"] = "min_numeric_tag"


@dataclass(frozen=True)
class _AggMaxNumericTag:
    axis_key: str
    kind: Literal["max_numeric_tag"] = "max_numeric_tag"


Aggregation = Union[
    _AggCount,
    _AggDistinctPublishers,
    _AggDistinctValues,
    _AggSumNumericTag,
    _AggMinNumericTag,
    _AggMaxNumericTag,
]
"""Per-bucket reduction. Use the :class:`AggregationCls` factory."""


class AggregationCls:
    """Factory constructors for :class:`Aggregation` variants."""

    @staticmethod
    def count() -> Aggregation:
        return _AggCount()

    @staticmethod
    def distinct_publishers() -> Aggregation:
        return _AggDistinctPublishers()

    @staticmethod
    def distinct_values(axis: TaxonomyAxis, key: str) -> Aggregation:
        return _AggDistinctValues(axis=axis, key=key)

    @staticmethod
    def sum_numeric_tag(axis_key: str) -> Aggregation:
        return _AggSumNumericTag(axis_key=axis_key)

    @staticmethod
    def min_numeric_tag(axis_key: str) -> Aggregation:
        return _AggMinNumericTag(axis_key=axis_key)

    @staticmethod
    def max_numeric_tag(axis_key: str) -> Aggregation:
        return _AggMaxNumericTag(axis_key=axis_key)


# ──────────────────────────────────────────────────────────────────
# CapacityQuery + CapacityRow
# ──────────────────────────────────────────────────────────────────


@dataclass
class CapacityQuery:
    """Composed capacity-ranking query — passed to
    :meth:`MeshNode.capability_capacity_ranking`.

    Attributes:
        group_by: Bucket-derivation strategy. Required.
        matcher: Optional pre-filter. ``None`` walks every entry.
        max_rtt_ms: Drop entries whose publisher's RTT exceeds this
            (the closure consults the ``rtt_map`` argument to
            ``capability_capacity_ranking``). ``None`` disables the
            RTT filter regardless of the map.
        sum_axis_key: Canonical ``<axis>.<key>`` of a numeric tag
            to sum across each bucket (e.g.
            ``"hardware.gpu.count"``).
        limit: Top-N buckets by ``available`` descending. ``0`` =
            no truncation.
    """

    group_by: GroupBy
    matcher: Optional[TagMatcher] = None
    max_rtt_ms: Optional[int] = None
    sum_axis_key: Optional[str] = None
    limit: int = 0


@dataclass
class CapacityRow:
    """One row of a capacity-ranking result."""

    bucket: str
    idle: int
    busy: int
    reserved: int
    available: int
    summed_capacity: Optional[int] = None


@dataclass
class AggregateRow:
    """One row of an aggregate result."""

    bucket: str
    value: int


# ──────────────────────────────────────────────────────────────────
# JSON encoding for the napi boundary
# ──────────────────────────────────────────────────────────────────


def _to_dict(obj: object) -> dict:
    """Project a frozen-dataclass tagged-union variant into the
    JSON shape the Rust core expects. Drops Python-side
    ``kind`` defaults and unset ``Optional`` fields (rendered as
    ``null`` in serialized JSON, not omitted)."""
    from dataclasses import asdict

    return asdict(obj)


def tag_matcher_to_json(matcher: TagMatcher) -> str:
    """Encode a :class:`TagMatcher` to the wire-format JSON string."""
    return json.dumps(_to_dict(matcher))


def group_by_to_json(group_by: GroupBy) -> str:
    """Encode a :class:`GroupBy` to the wire-format JSON string."""
    return json.dumps(_to_dict(group_by))


def aggregation_to_json(aggregation: Aggregation) -> str:
    """Encode an :class:`Aggregation` to the wire-format JSON string."""
    return json.dumps(_to_dict(aggregation))


def capacity_query_to_json(query: CapacityQuery) -> str:
    """Encode a :class:`CapacityQuery` to the wire-format JSON string."""
    matcher_dict = _to_dict(query.matcher) if query.matcher is not None else None
    payload = {
        "matcher": matcher_dict,
        "group_by": _to_dict(query.group_by),
        "max_rtt_ms": query.max_rtt_ms,
        "sum_axis_key": query.sum_axis_key,
        "limit": query.limit,
    }
    return json.dumps(payload)


__all__ = [
    "Aggregation",
    "AggregationCls",
    "AggregateRow",
    "CapacityQuery",
    "CapacityRow",
    "GroupBy",
    "GroupByCls",
    "TagMatcher",
    "TagMatcherCls",
    "TaxonomyAxis",
    "tag_matcher_to_json",
    "group_by_to_json",
    "aggregation_to_json",
    "capacity_query_to_json",
]
