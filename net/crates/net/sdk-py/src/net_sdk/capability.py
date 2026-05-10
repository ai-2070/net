"""
Capability-System Enhancements — Python surface.

Mirrors the substrate's typed-tag taxonomy, predicate IR, and
``CapabilitySet::diff`` exactly, so Python applications produce
byte-equal wire JSON to the Rust SDK and the TypeScript SDK. The
fixtures under ``tests/cross_lang_capability/`` pin the canonical
shapes; ``tests/test_capability_enhancements.py`` drives them.

The wire format is ``{"tags": [...], "metadata": {...}}`` — pure
JSON, no FFI dance required for the predicate IR / diff / tag
taxonomy. The substrate's net-side execution (capability-index
lookup, predicate evaluation against a live index) stays Rust-side;
this surface produces the request shapes.

Usage:

    from net_sdk.capability import (
        TaxonomyAxis, p, predicate_to_wire, predicate_to_rpc_header,
        diff_capabilities, require_tag, require_axis_value,
        with_metadata, standard_placement,
    )

    pred = p.and_(
        p.exists(("hardware", "gpu")),
        p.numeric_at_least(("hardware", "memory_mb"), 65536),
        p.metadata_equals("intent", "ml-training"),
    )
    header_value = predicate_to_rpc_header(pred)
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from typing import Any, Callable, Dict, List, Literal, Mapping, Optional, Sequence, Tuple, Union

# ============================================================================
# Typed taxonomy
# ============================================================================

TaxonomyAxis = Literal["hardware", "software", "devices", "dataforts"]

#: All known taxonomy axes — matches ``TaxonomyAxis`` on the substrate.
TAXONOMY_AXES: Tuple[TaxonomyAxis, ...] = ("hardware", "software", "devices", "dataforts")

#: Reserved cross-axis prefixes. Privileged-path code emits these via
#: substrate APIs (announce-chain / fork-coordination / scope helpers);
#: user code goes through :func:`tag_from_user_string`, which rejects.
RESERVED_PREFIXES: Tuple[str, ...] = ("causal:", "fork-of:", "heat:", "scope:")


AxisSeparator = Literal[":", "="]


# A TagKey is just a (axis, key) pair — keeping it as a tuple stays
# JSON-serializable and matches the substrate's wire shape ``{"axis":
# ..., "key": ...}`` after a single :func:`_tag_key_to_wire` pass.
TagKey = Tuple[TaxonomyAxis, str]


def tag_key(axis: TaxonomyAxis, key: str) -> TagKey:
    """Build a :data:`TagKey`. Raises on empty key."""
    if not key:
        raise ValueError(f"tag_key: key must be non-empty (axis={axis!r})")
    return (axis, key)


def _tag_key_to_wire(tk: TagKey) -> Dict[str, str]:
    return {"axis": tk[0], "key": tk[1]}


def _tag_key_from_wire(d: Mapping[str, Any]) -> TagKey:
    return (d["axis"], d["key"])


# Tag — the substrate distinguishes axisPresent / axisValue / reserved
# / legacy. We use a small dataclass family rather than ``Literal``
# discrimination so callers can ``isinstance``-check.


@dataclass(frozen=True)
class TagAxisPresent:
    """``<axis>.<key>`` — axis tag with no value."""

    axis: TaxonomyAxis
    key: str


@dataclass(frozen=True)
class TagAxisValue:
    """``<axis>.<key>=<value>`` or ``<axis>.<key>:<value>``."""

    axis: TaxonomyAxis
    key: str
    value: str
    separator: AxisSeparator = "="


@dataclass(frozen=True)
class TagReserved:
    """One of the :data:`RESERVED_PREFIXES` cross-axis tags."""

    prefix: str
    body: str


@dataclass(frozen=True)
class TagLegacy:
    """Arbitrary string outside the typed taxonomy."""

    raw: str


Tag = Union[TagAxisPresent, TagAxisValue, TagReserved, TagLegacy]


def starts_with_reserved_prefix(s: str) -> Optional[str]:
    """Return the matched prefix, or ``None``."""
    for p_ in RESERVED_PREFIXES:
        if s.startswith(p_):
            return p_
    return None


def tag_to_string(tag: Tag) -> str:
    """Render to canonical wire string. Matches the substrate's
    ``Display`` impl for ``Tag`` byte-for-byte."""
    if isinstance(tag, TagAxisPresent):
        return f"{tag.axis}.{tag.key}"
    if isinstance(tag, TagAxisValue):
        return f"{tag.axis}.{tag.key}{tag.separator}{tag.value}"
    if isinstance(tag, TagReserved):
        return f"{tag.prefix}{tag.body}"
    if isinstance(tag, TagLegacy):
        return tag.raw
    raise TypeError(f"unknown tag variant: {type(tag).__name__}")


def tag_from_string(s: str) -> Tag:
    """Parse a wire string into a :data:`Tag`. Privileged path —
    accepts reserved prefixes. User code should prefer
    :func:`tag_from_user_string`."""
    if not s:
        raise ValueError("tag_from_string: tag must be non-empty")
    reserved = starts_with_reserved_prefix(s)
    if reserved is not None:
        return TagReserved(prefix=reserved, body=s[len(reserved):])
    dot = s.find(".")
    if dot < 0:
        return TagLegacy(raw=s)
    axis = s[:dot]
    if axis not in TAXONOMY_AXES:
        return TagLegacy(raw=s)
    body = s[dot + 1:]
    if not body:
        return TagLegacy(raw=s)
    eq = body.find("=")
    colon = body.find(":")
    sep_idx = -1
    sep: Optional[AxisSeparator] = None
    if eq >= 0 and colon >= 0:
        if eq < colon:
            sep, sep_idx = "=", eq
        else:
            sep, sep_idx = ":", colon
    elif eq >= 0:
        sep, sep_idx = "=", eq
    elif colon >= 0:
        sep, sep_idx = ":", colon
    if sep is None:
        return TagAxisPresent(axis=axis, key=body)  # type: ignore[arg-type]
    key = body[:sep_idx]
    value = body[sep_idx + 1:]
    if not key or not value:
        return TagLegacy(raw=s)
    return TagAxisValue(axis=axis, key=key, value=value, separator=sep)  # type: ignore[arg-type]


def tag_from_user_string(s: str) -> Tag:
    """Parse a wire string from user code. Rejects the reserved
    prefixes ({"causal:", "fork-of:", "heat:", "scope:"})."""
    if not s:
        raise ValueError("tag_from_user_string: tag must be non-empty")
    reserved = starts_with_reserved_prefix(s)
    if reserved is not None:
        raise ValueError(
            f"tag {s!r} starts with reserved prefix {reserved!r}; "
            f"user code cannot emit reserved-prefix tags"
        )
    return tag_from_string(s)


# ============================================================================
# Predicate IR — flat post-order tree, identical wire shape to the
# substrate's ``PredicateWire`` and the cross-binding fixtures.
# ============================================================================


# A predicate-AST node kind keyed by ``type``. Using ``Dict`` over a
# closed sum type keeps the JSON encoder a one-liner.
@dataclass(frozen=True)
class _PredExists:
    key: TagKey


@dataclass(frozen=True)
class _PredEquals:
    key: TagKey
    value: str


@dataclass(frozen=True)
class _PredNumericAtLeast:
    key: TagKey
    threshold: float


@dataclass(frozen=True)
class _PredNumericAtMost:
    key: TagKey
    threshold: float


@dataclass(frozen=True)
class _PredNumericInRange:
    key: TagKey
    min: float
    max: float


@dataclass(frozen=True)
class _PredSemverAtLeast:
    key: TagKey
    version: str


@dataclass(frozen=True)
class _PredSemverAtMost:
    key: TagKey
    version: str


@dataclass(frozen=True)
class _PredSemverCompatible:
    key: TagKey
    version: str


@dataclass(frozen=True)
class _PredStringPrefix:
    key: TagKey
    prefix: str


@dataclass(frozen=True)
class _PredStringMatches:
    key: TagKey
    pattern: str


@dataclass(frozen=True)
class _PredMetadataExists:
    key: str


@dataclass(frozen=True)
class _PredMetadataEquals:
    key: str
    value: str


@dataclass(frozen=True)
class _PredMetadataMatches:
    key: str
    pattern: str


@dataclass(frozen=True)
class _PredMetadataNumericAtLeast:
    key: str
    threshold: float


@dataclass(frozen=True)
class _PredAnd:
    children: Tuple["Predicate", ...]


@dataclass(frozen=True)
class _PredOr:
    children: Tuple["Predicate", ...]


@dataclass(frozen=True)
class _PredNot:
    child: "Predicate"


Predicate = Union[
    _PredExists,
    _PredEquals,
    _PredNumericAtLeast,
    _PredNumericAtMost,
    _PredNumericInRange,
    _PredSemverAtLeast,
    _PredSemverAtMost,
    _PredSemverCompatible,
    _PredStringPrefix,
    _PredStringMatches,
    _PredMetadataExists,
    _PredMetadataEquals,
    _PredMetadataMatches,
    _PredMetadataNumericAtLeast,
    _PredAnd,
    _PredOr,
    _PredNot,
]


class _PredicateBuilder:
    """Fluent predicate constructors. Snake_case methods to match
    Python conventions; ``and_`` / ``or_`` / ``not_`` use trailing
    underscores so they don't collide with the keywords."""

    @staticmethod
    def exists(key: TagKey) -> Predicate:
        return _PredExists(key)

    @staticmethod
    def equals(key: TagKey, value: str) -> Predicate:
        return _PredEquals(key, value)

    @staticmethod
    def numeric_at_least(key: TagKey, threshold: float) -> Predicate:
        return _PredNumericAtLeast(key, float(threshold))

    @staticmethod
    def numeric_at_most(key: TagKey, threshold: float) -> Predicate:
        return _PredNumericAtMost(key, float(threshold))

    @staticmethod
    def numeric_in_range(key: TagKey, min_: float, max_: float) -> Predicate:
        return _PredNumericInRange(key, float(min_), float(max_))

    @staticmethod
    def semver_at_least(key: TagKey, version: str) -> Predicate:
        return _PredSemverAtLeast(key, version)

    @staticmethod
    def semver_at_most(key: TagKey, version: str) -> Predicate:
        return _PredSemverAtMost(key, version)

    @staticmethod
    def semver_compatible(key: TagKey, version: str) -> Predicate:
        return _PredSemverCompatible(key, version)

    @staticmethod
    def string_prefix(key: TagKey, prefix: str) -> Predicate:
        return _PredStringPrefix(key, prefix)

    @staticmethod
    def string_matches(key: TagKey, pattern: str) -> Predicate:
        return _PredStringMatches(key, pattern)

    @staticmethod
    def metadata_exists(key: str) -> Predicate:
        return _PredMetadataExists(key)

    @staticmethod
    def metadata_equals(key: str, value: str) -> Predicate:
        return _PredMetadataEquals(key, value)

    @staticmethod
    def metadata_matches(key: str, pattern: str) -> Predicate:
        return _PredMetadataMatches(key, pattern)

    @staticmethod
    def metadata_numeric_at_least(key: str, threshold: float) -> Predicate:
        return _PredMetadataNumericAtLeast(key, float(threshold))

    @staticmethod
    def and_(*children: Predicate) -> Predicate:
        return _PredAnd(tuple(children))

    @staticmethod
    def or_(*children: Predicate) -> Predicate:
        return _PredOr(tuple(children))

    @staticmethod
    def not_(child: Predicate) -> Predicate:
        return _PredNot(child)


#: Singleton instance — usage is ``p.exists(...)``, mirroring the TS
#: ``p.exists(...)``. Lets call sites read identically across bindings.
p = _PredicateBuilder()


def _emit(node: Predicate, out: List[Dict[str, Any]]) -> int:
    if isinstance(node, _PredExists):
        out.append({"kind": "exists", "key": _tag_key_to_wire(node.key)})
        return len(out) - 1
    if isinstance(node, _PredEquals):
        out.append(
            {"kind": "equals", "key": _tag_key_to_wire(node.key), "value": node.value}
        )
        return len(out) - 1
    if isinstance(node, _PredNumericAtLeast):
        out.append(
            {
                "kind": "numeric_at_least",
                "key": _tag_key_to_wire(node.key),
                "threshold": node.threshold,
            }
        )
        return len(out) - 1
    if isinstance(node, _PredNumericAtMost):
        out.append(
            {
                "kind": "numeric_at_most",
                "key": _tag_key_to_wire(node.key),
                "threshold": node.threshold,
            }
        )
        return len(out) - 1
    if isinstance(node, _PredNumericInRange):
        out.append(
            {
                "kind": "numeric_in_range",
                "key": _tag_key_to_wire(node.key),
                "min": node.min,
                "max": node.max,
            }
        )
        return len(out) - 1
    if isinstance(node, _PredSemverAtLeast):
        out.append(
            {
                "kind": "semver_at_least",
                "key": _tag_key_to_wire(node.key),
                "version": node.version,
            }
        )
        return len(out) - 1
    if isinstance(node, _PredSemverAtMost):
        out.append(
            {
                "kind": "semver_at_most",
                "key": _tag_key_to_wire(node.key),
                "version": node.version,
            }
        )
        return len(out) - 1
    if isinstance(node, _PredSemverCompatible):
        out.append(
            {
                "kind": "semver_compatible",
                "key": _tag_key_to_wire(node.key),
                "version": node.version,
            }
        )
        return len(out) - 1
    if isinstance(node, _PredStringPrefix):
        out.append(
            {
                "kind": "string_prefix",
                "key": _tag_key_to_wire(node.key),
                "prefix": node.prefix,
            }
        )
        return len(out) - 1
    if isinstance(node, _PredStringMatches):
        out.append(
            {
                "kind": "string_matches",
                "key": _tag_key_to_wire(node.key),
                "pattern": node.pattern,
            }
        )
        return len(out) - 1
    if isinstance(node, _PredMetadataExists):
        out.append({"kind": "metadata_exists", "key": node.key})
        return len(out) - 1
    if isinstance(node, _PredMetadataEquals):
        out.append({"kind": "metadata_equals", "key": node.key, "value": node.value})
        return len(out) - 1
    if isinstance(node, _PredMetadataMatches):
        out.append(
            {"kind": "metadata_matches", "key": node.key, "pattern": node.pattern}
        )
        return len(out) - 1
    if isinstance(node, _PredMetadataNumericAtLeast):
        out.append(
            {
                "kind": "metadata_numeric_at_least",
                "key": node.key,
                "threshold": node.threshold,
            }
        )
        return len(out) - 1
    if isinstance(node, _PredAnd):
        child_idxs = [_emit(c, out) for c in node.children]
        out.append({"kind": "and", "children": child_idxs})
        return len(out) - 1
    if isinstance(node, _PredOr):
        child_idxs = [_emit(c, out) for c in node.children]
        out.append({"kind": "or", "children": child_idxs})
        return len(out) - 1
    if isinstance(node, _PredNot):
        child_idx = _emit(node.child, out)
        out.append({"kind": "not", "child": child_idx})
        return len(out) - 1
    raise TypeError(f"unknown predicate variant: {type(node).__name__}")


def predicate_to_wire(pred: Predicate) -> Dict[str, Any]:
    """Flatten an AST into wire form. Children always at strictly
    lower indices than their parent (post-order)."""
    nodes: List[Dict[str, Any]] = []
    root_idx = _emit(pred, nodes)
    return {"nodes": nodes, "root_idx": root_idx}


def _node_from_wire(
    n: Mapping[str, Any], prior: List[Predicate], self_idx: int
) -> Predicate:
    def check_child(idx: int) -> Predicate:
        if idx < 0 or idx >= self_idx:
            raise ValueError(
                f"predicate_from_wire: child index {idx} not strictly less than self {self_idx}"
            )
        return prior[idx]

    kind = n["kind"]
    if kind == "exists":
        return _PredExists(_tag_key_from_wire(n["key"]))
    if kind == "equals":
        return _PredEquals(_tag_key_from_wire(n["key"]), n["value"])
    if kind == "numeric_at_least":
        return _PredNumericAtLeast(_tag_key_from_wire(n["key"]), float(n["threshold"]))
    if kind == "numeric_at_most":
        return _PredNumericAtMost(_tag_key_from_wire(n["key"]), float(n["threshold"]))
    if kind == "numeric_in_range":
        return _PredNumericInRange(
            _tag_key_from_wire(n["key"]), float(n["min"]), float(n["max"])
        )
    if kind == "semver_at_least":
        return _PredSemverAtLeast(_tag_key_from_wire(n["key"]), n["version"])
    if kind == "semver_at_most":
        return _PredSemverAtMost(_tag_key_from_wire(n["key"]), n["version"])
    if kind == "semver_compatible":
        return _PredSemverCompatible(_tag_key_from_wire(n["key"]), n["version"])
    if kind == "string_prefix":
        return _PredStringPrefix(_tag_key_from_wire(n["key"]), n["prefix"])
    if kind == "string_matches":
        return _PredStringMatches(_tag_key_from_wire(n["key"]), n["pattern"])
    if kind == "metadata_exists":
        return _PredMetadataExists(n["key"])
    if kind == "metadata_equals":
        return _PredMetadataEquals(n["key"], n["value"])
    if kind == "metadata_matches":
        return _PredMetadataMatches(n["key"], n["pattern"])
    if kind == "metadata_numeric_at_least":
        return _PredMetadataNumericAtLeast(n["key"], float(n["threshold"]))
    if kind == "and":
        return _PredAnd(tuple(check_child(c) for c in n["children"]))
    if kind == "or":
        return _PredOr(tuple(check_child(c) for c in n["children"]))
    if kind == "not":
        return _PredNot(check_child(n["child"]))
    raise ValueError(f"unknown predicate kind: {kind!r}")


def predicate_from_wire(wire: Mapping[str, Any]) -> Predicate:
    """Inverse of :func:`predicate_to_wire`. Throws on out-of-range
    indices or unknown kinds."""
    nodes = wire["nodes"]
    root_idx = wire["root_idx"]
    built: List[Predicate] = []
    for i, n in enumerate(nodes):
        built.append(_node_from_wire(n, built, i))
    if root_idx < 0 or root_idx >= len(built):
        raise ValueError(
            f"predicate_from_wire: root_idx {root_idx} out of range [0, {len(built)})"
        )
    return built[root_idx]


# nRPC envelope — header + helpers ------------------------------------------

#: nRPC header carrying a predicate. Matches ``RPC_WHERE_HEADER``.
RPC_WHERE_HEADER = "cyberdeck-where"


def predicate_to_rpc_header(pred: Predicate) -> str:
    """Encode a predicate to the canonical request-header value
    (JSON-encoded :func:`predicate_to_wire` output)."""
    # ``separators`` matches serde_json's default, no spaces.
    return json.dumps(predicate_to_wire(pred), separators=(",", ":"))


def predicate_from_rpc_header(value: str) -> Predicate:
    """Decode a ``cyberdeck-where`` header value into a predicate AST."""
    return predicate_from_wire(json.loads(value))


# ============================================================================
# CapabilitySet diff — wire-format input, sorted output.
# ============================================================================


@dataclass(frozen=True)
class CapabilitySetWire:
    """Wire-format capability set — string tags + str→str metadata."""

    tags: Tuple[str, ...] = ()
    metadata: Mapping[str, str] = field(default_factory=dict)


@dataclass(frozen=True)
class MetadataChangeAdded:
    key: str
    value: str

    @property
    def kind(self) -> str:
        return "added"


@dataclass(frozen=True)
class MetadataChangeRemoved:
    key: str
    prev_value: str

    @property
    def kind(self) -> str:
        return "removed"


@dataclass(frozen=True)
class MetadataChangeUpdated:
    key: str
    prev_value: str
    new_value: str

    @property
    def kind(self) -> str:
        return "updated"


MetadataChange = Union[MetadataChangeAdded, MetadataChangeRemoved, MetadataChangeUpdated]


def _metadata_change_to_dict(c: MetadataChange) -> Dict[str, Any]:
    if isinstance(c, MetadataChangeAdded):
        return {"kind": "added", "key": c.key, "value": c.value}
    if isinstance(c, MetadataChangeRemoved):
        return {"kind": "removed", "key": c.key, "prev_value": c.prev_value}
    if isinstance(c, MetadataChangeUpdated):
        return {
            "kind": "updated",
            "key": c.key,
            "prev_value": c.prev_value,
            "new_value": c.new_value,
        }
    raise TypeError(f"unknown change variant: {type(c).__name__}")


@dataclass(frozen=True)
class CapabilitySetDiff:
    added_tags: Tuple[str, ...]
    removed_tags: Tuple[str, ...]
    metadata_changes: Tuple[MetadataChange, ...]

    def to_wire(self) -> Dict[str, Any]:
        """Encode as the cross-binding wire JSON shape."""
        return {
            "added_tags": list(self.added_tags),
            "removed_tags": list(self.removed_tags),
            "metadata_changes": [_metadata_change_to_dict(c) for c in self.metadata_changes],
        }


def _coerce_caps(
    v: Union[CapabilitySetWire, Mapping[str, Any]]
) -> CapabilitySetWire:
    if isinstance(v, CapabilitySetWire):
        return v
    return CapabilitySetWire(
        tags=tuple(v.get("tags", ())),
        metadata=dict(v.get("metadata", {})),
    )


def diff_capabilities(
    prev: Union[CapabilitySetWire, Mapping[str, Any]],
    curr: Union[CapabilitySetWire, Mapping[str, Any]],
) -> CapabilitySetDiff:
    """Compute ``curr.diff(prev)``. Pinned by the
    ``capability_set_diff.json`` cross-binding fixture.

    - Tag arrays are sorted by wire string.
    - Metadata changes are sorted by key (BTreeMap semantics).
    - A key rename surfaces as Removed + Added (NOT Updated). Only a
      value change for the same key is Updated.
    """
    prev_w = _coerce_caps(prev)
    curr_w = _coerce_caps(curr)
    prev_tags = set(prev_w.tags)
    curr_tags = set(curr_w.tags)
    added = sorted(curr_tags - prev_tags)
    removed = sorted(prev_tags - curr_tags)

    changes: List[MetadataChange] = []
    all_keys = sorted(set(prev_w.metadata) | set(curr_w.metadata))
    for key in all_keys:
        in_prev = key in prev_w.metadata
        in_curr = key in curr_w.metadata
        if in_prev and in_curr:
            pv = prev_w.metadata[key]
            nv = curr_w.metadata[key]
            if pv != nv:
                changes.append(MetadataChangeUpdated(key=key, prev_value=pv, new_value=nv))
        elif in_curr:
            changes.append(MetadataChangeAdded(key=key, value=curr_w.metadata[key]))
        else:
            changes.append(MetadataChangeRemoved(key=key, prev_value=prev_w.metadata[key]))

    return CapabilitySetDiff(
        added_tags=tuple(added),
        removed_tags=tuple(removed),
        metadata_changes=tuple(changes),
    )


# ============================================================================
# Chain composition helpers
# ============================================================================


def empty_capabilities() -> CapabilitySetWire:
    """Empty wire-format capability set."""
    return CapabilitySetWire(tags=(), metadata={})


def _fresh_tags(caps: CapabilitySetWire) -> List[str]:
    seen: List[str] = []
    for t in caps.tags:
        if t not in seen:
            seen.append(t)
    return seen


def require_tag(caps: CapabilitySetWire, axis: TaxonomyAxis, key: str) -> CapabilitySetWire:
    """Add an axis-tag (no value). Idempotent."""
    if not key:
        raise ValueError("require_tag: key must be non-empty")
    wire = tag_to_string(TagAxisPresent(axis=axis, key=key))
    tags = _fresh_tags(caps)
    if wire not in tags:
        tags.append(wire)
    return CapabilitySetWire(tags=tuple(tags), metadata=dict(caps.metadata))


def require_axis_value(
    caps: CapabilitySetWire,
    axis: TaxonomyAxis,
    key: str,
    value: str,
    separator: AxisSeparator = "=",
) -> CapabilitySetWire:
    """Add ``<axis>.<key><sep><value>``. Idempotent for the exact tuple."""
    if not key:
        raise ValueError("require_axis_value: key must be non-empty")
    if not value:
        raise ValueError("require_axis_value: value must be non-empty")
    wire = tag_to_string(
        TagAxisValue(axis=axis, key=key, value=value, separator=separator)
    )
    tags = _fresh_tags(caps)
    if wire not in tags:
        tags.append(wire)
    return CapabilitySetWire(tags=tuple(tags), metadata=dict(caps.metadata))


def with_metadata(
    caps: CapabilitySetWire, key: str, value: str
) -> CapabilitySetWire:
    """Set / overwrite a metadata entry."""
    if not key:
        raise ValueError("with_metadata: key must be non-empty")
    md = dict(caps.metadata)
    md[key] = value
    return CapabilitySetWire(tags=tuple(caps.tags), metadata=md)


# ============================================================================
# StandardPlacement config + builder
# ============================================================================


@dataclass(frozen=True)
class StandardPlacement:
    """Placement filter declared by a daemon. All fields optional —
    an empty config matches every node.

    The substrate side runs the actual filter; this is the
    JSON-serializable configuration shape.
    """

    require_tags: Tuple[str, ...] = ()
    forbid_tags: Tuple[str, ...] = ()
    require_metadata: Mapping[str, str] = field(default_factory=dict)
    predicate: Optional[Mapping[str, Any]] = None
    limit: Optional[int] = None
    custom_filter_id: Optional[str] = None

    def to_wire(self) -> Dict[str, Any]:
        out: Dict[str, Any] = {}
        if self.require_tags:
            out["require_tags"] = list(self.require_tags)
        if self.forbid_tags:
            out["forbid_tags"] = list(self.forbid_tags)
        if self.require_metadata:
            out["require_metadata"] = dict(self.require_metadata)
        if self.predicate is not None:
            out["predicate"] = dict(self.predicate)
        if self.limit is not None:
            out["limit"] = self.limit
        if self.custom_filter_id is not None:
            out["custom_filter_id"] = self.custom_filter_id
        return out


class StandardPlacementBuilder:
    """Fluent builder for :class:`StandardPlacement`."""

    def __init__(self) -> None:
        self._require_tags: List[str] = []
        self._forbid_tags: List[str] = []
        self._require_metadata: Dict[str, str] = {}
        self._predicate: Optional[Mapping[str, Any]] = None
        self._limit: Optional[int] = None
        self._custom_filter_id: Optional[str] = None

    def require_tag(self, axis: TaxonomyAxis, key: str) -> "StandardPlacementBuilder":
        self._require_tags.append(tag_to_string(TagAxisPresent(axis=axis, key=key)))
        return self

    def require_axis_value(
        self,
        axis: TaxonomyAxis,
        key: str,
        value: str,
        separator: AxisSeparator = "=",
    ) -> "StandardPlacementBuilder":
        self._require_tags.append(
            tag_to_string(
                TagAxisValue(axis=axis, key=key, value=value, separator=separator)
            )
        )
        return self

    def forbid_tag(self, axis: TaxonomyAxis, key: str) -> "StandardPlacementBuilder":
        self._forbid_tags.append(tag_to_string(TagAxisPresent(axis=axis, key=key)))
        return self

    def require_metadata(self, key: str, value: str) -> "StandardPlacementBuilder":
        self._require_metadata[key] = value
        return self

    def with_predicate(
        self, pred: Union[Predicate, Mapping[str, Any]]
    ) -> "StandardPlacementBuilder":
        if isinstance(pred, Mapping) and "nodes" in pred and "root_idx" in pred:
            self._predicate = dict(pred)
        else:
            self._predicate = predicate_to_wire(pred)  # type: ignore[arg-type]
        return self

    def with_limit(self, n: int) -> "StandardPlacementBuilder":
        if n < 0:
            raise ValueError("with_limit: n must be non-negative")
        self._limit = int(n)
        return self

    def with_custom_filter_id(self, id_: str) -> "StandardPlacementBuilder":
        if not id_:
            raise ValueError("with_custom_filter_id: id must be non-empty")
        self._custom_filter_id = id_
        return self

    def build(self) -> StandardPlacement:
        return StandardPlacement(
            require_tags=tuple(self._require_tags),
            forbid_tags=tuple(self._forbid_tags),
            require_metadata=dict(self._require_metadata),
            predicate=dict(self._predicate) if self._predicate is not None else None,
            limit=self._limit,
            custom_filter_id=self._custom_filter_id,
        )


def standard_placement() -> StandardPlacementBuilder:
    """Convenience constructor for :class:`StandardPlacementBuilder`."""
    return StandardPlacementBuilder()


# ============================================================================
# Custom placement-filter callback
# ============================================================================


@dataclass(frozen=True)
class PlacementCandidate:
    """Candidate handed to a custom placement-filter callback."""

    node_id: int
    tags: Tuple[str, ...]
    metadata: Mapping[str, str]


PlacementFilterFn = Callable[[PlacementCandidate], bool]


@dataclass(frozen=True)
class RegisteredPlacementFilter:
    id: str
    fn: PlacementFilterFn


_placement_filter_counter = 0


def placement_filter_from_fn(
    fn: PlacementFilterFn, explicit_id: Optional[str] = None
) -> RegisteredPlacementFilter:
    """Wrap a user predicate as a registered placement filter. Returns
    ``(id, fn)``; the runtime registers them by id, and
    :attr:`StandardPlacement.custom_filter_id` references that id."""
    global _placement_filter_counter
    if explicit_id is None:
        _placement_filter_counter += 1
        id_ = f"pf-{_placement_filter_counter}"
    else:
        id_ = explicit_id
    return RegisteredPlacementFilter(id=id_, fn=fn)


# ============================================================================
# Predicate evaluation — pure local evaluator over (tags, metadata).
#
# Mirrors the substrate's ``Predicate::evaluate_unplanned``. Pinned
# across bindings by ``predicate_eval.json``.
# ============================================================================


_NUMERIC_RE = __import__("re").compile(r"^-?\d+(\.\d+)?$")


def _parse_semver(s: str) -> Optional[Tuple[int, int, int]]:
    """Drop pre-release / build suffix; parse 1-3 dot-separated ints."""
    dash = s.find("-")
    plus = s.find("+")
    if dash >= 0 and plus >= 0:
        core = s[: min(dash, plus)]
    elif dash >= 0:
        core = s[:dash]
    elif plus >= 0:
        core = s[:plus]
    else:
        core = s
    parts = [p.strip() for p in core.split(".")]
    if not parts or len(parts) > 3:
        return None
    try:
        major = int(parts[0]) if parts[0].isdigit() else None
        if major is None:
            return None
        minor = int(parts[1]) if len(parts) > 1 and parts[1].isdigit() else (0 if len(parts) <= 1 else None)
        if minor is None:
            return None
        patch = int(parts[2]) if len(parts) > 2 and parts[2].isdigit() else (0 if len(parts) <= 2 else None)
        if patch is None:
            return None
    except ValueError:
        return None
    return (major, minor, patch)


def _semver_compatible(lhs: Tuple[int, int, int], rhs: Tuple[int, int, int]) -> bool:
    if lhs < rhs:
        return False
    if rhs[0] == 0:
        return rhs[1] == lhs[1]
    return rhs[0] == lhs[0]


def _axis_tag_value(tags: Sequence[str], key: TagKey) -> Optional[str]:
    """Return the matched axis-tag value, or empty string for AxisPresent,
    or ``None`` if no tag matches the (axis, key) pair."""
    prefix = f"{key[0]}.{key[1]}"
    for wire in tags:
        if wire == prefix:
            return ""
        if len(wire) <= len(prefix) or not wire.startswith(prefix):
            continue
        sep = wire[len(prefix)]
        if sep == "=" or sep == ":":
            return wire[len(prefix) + 1:]
    return None


def _eval_leaf(
    pred: Predicate,
    tags: Sequence[str],
    metadata: Mapping[str, str],
) -> bool:
    if isinstance(pred, _PredExists):
        return _axis_tag_value(tags, pred.key) is not None
    if isinstance(pred, _PredEquals):
        v = _axis_tag_value(tags, pred.key)
        return v is not None and v == pred.value
    if isinstance(pred, _PredNumericAtLeast):
        v = _axis_tag_value(tags, pred.key)
        if v is None or not _NUMERIC_RE.match(v):
            return False
        return float(v) >= pred.threshold
    if isinstance(pred, _PredNumericAtMost):
        v = _axis_tag_value(tags, pred.key)
        if v is None or not _NUMERIC_RE.match(v):
            return False
        return float(v) <= pred.threshold
    if isinstance(pred, _PredNumericInRange):
        v = _axis_tag_value(tags, pred.key)
        if v is None or not _NUMERIC_RE.match(v):
            return False
        n = float(v)
        return pred.min <= n <= pred.max
    if isinstance(pred, _PredSemverAtLeast):
        rhs = _parse_semver(pred.version)
        if rhs is None:
            return False
        v = _axis_tag_value(tags, pred.key)
        if v is None:
            return False
        lhs = _parse_semver(v)
        return lhs is not None and lhs >= rhs
    if isinstance(pred, _PredSemverAtMost):
        rhs = _parse_semver(pred.version)
        if rhs is None:
            return False
        v = _axis_tag_value(tags, pred.key)
        if v is None:
            return False
        lhs = _parse_semver(v)
        return lhs is not None and lhs <= rhs
    if isinstance(pred, _PredSemverCompatible):
        rhs = _parse_semver(pred.version)
        if rhs is None:
            return False
        v = _axis_tag_value(tags, pred.key)
        if v is None:
            return False
        lhs = _parse_semver(v)
        return lhs is not None and _semver_compatible(lhs, rhs)
    if isinstance(pred, _PredStringPrefix):
        v = _axis_tag_value(tags, pred.key)
        return v is not None and v.startswith(pred.prefix)
    if isinstance(pred, _PredStringMatches):
        v = _axis_tag_value(tags, pred.key)
        return v is not None and pred.pattern in v
    if isinstance(pred, _PredMetadataExists):
        return pred.key in metadata
    if isinstance(pred, _PredMetadataEquals):
        return metadata.get(pred.key) == pred.value
    if isinstance(pred, _PredMetadataMatches):
        v = metadata.get(pred.key)
        return v is not None and pred.pattern in v
    if isinstance(pred, _PredMetadataNumericAtLeast):
        v = metadata.get(pred.key)
        if v is None or not _NUMERIC_RE.match(v):
            return False
        return float(v) >= pred.threshold
    raise TypeError(f"_eval_leaf: composite predicate {type(pred).__name__} routed through leaf evaluator")


def evaluate_predicate(
    pred: Predicate,
    tags: Sequence[str],
    metadata: Mapping[str, str],
) -> bool:
    """Evaluate a predicate against a wire-format ``(tags, metadata)``
    context. Mirrors the substrate's ``Predicate::evaluate_unplanned``;
    children of ``and`` / ``or`` evaluate in declaration order with
    short-circuit semantics. Pinned across bindings by
    ``predicate_eval.json``."""
    if isinstance(pred, _PredAnd):
        return all(evaluate_predicate(c, tags, metadata) for c in pred.children)
    if isinstance(pred, _PredOr):
        return any(evaluate_predicate(c, tags, metadata) for c in pred.children)
    if isinstance(pred, _PredNot):
        return not evaluate_predicate(pred.child, tags, metadata)
    return _eval_leaf(pred, tags, metadata)


__all__ = [
    "TaxonomyAxis",
    "TAXONOMY_AXES",
    "RESERVED_PREFIXES",
    "AxisSeparator",
    "TagKey",
    "tag_key",
    "Tag",
    "TagAxisPresent",
    "TagAxisValue",
    "TagReserved",
    "TagLegacy",
    "starts_with_reserved_prefix",
    "tag_to_string",
    "tag_from_string",
    "tag_from_user_string",
    "Predicate",
    "p",
    "predicate_to_wire",
    "predicate_from_wire",
    "RPC_WHERE_HEADER",
    "predicate_to_rpc_header",
    "predicate_from_rpc_header",
    "CapabilitySetWire",
    "CapabilitySetDiff",
    "MetadataChange",
    "MetadataChangeAdded",
    "MetadataChangeRemoved",
    "MetadataChangeUpdated",
    "diff_capabilities",
    "empty_capabilities",
    "require_tag",
    "require_axis_value",
    "with_metadata",
    "StandardPlacement",
    "StandardPlacementBuilder",
    "standard_placement",
    "PlacementCandidate",
    "PlacementFilterFn",
    "RegisteredPlacementFilter",
    "placement_filter_from_fn",
    "evaluate_predicate",
]
