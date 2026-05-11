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
import re
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
RPC_WHERE_HEADER = "net-where"


def predicate_to_rpc_header(pred: Predicate) -> str:
    """Encode a predicate to the canonical request-header value
    (JSON-encoded :func:`predicate_to_wire` output)."""
    # ``separators`` matches serde_json's default, no spaces.
    return json.dumps(predicate_to_wire(pred), separators=(",", ":"))


def predicate_from_rpc_header(value: str) -> Predicate:
    """Decode a ``net-where`` header value into a predicate AST."""
    return predicate_from_wire(json.loads(value))


def where_header(pred: Predicate) -> Tuple[str, bytes]:
    """Build the canonical ``net-where:`` request-header entry
    for Phase 9b predicate-pushdown calls. Drops straight into the
    ``request_headers`` list of a Python ``MeshRpc.call`` opts dict.

    Example::

        from net_sdk import p, tag_key, where_header
        pred = p.exists(tag_key("hardware", "gpu"))
        await mesh_rpc.call(
            target_node_id,
            "filter-svc",
            payload,
            opts={"request_headers": [where_header(pred)]},
        )

    The header value is the canonical JSON-encoded ``PredicateWire``
    pinned by ``predicate_nrpc_envelope.json``.
    """
    return (RPC_WHERE_HEADER, predicate_to_rpc_header(pred).encode("utf-8"))


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


def _try_parse_float(s: str) -> Optional[float]:
    """Parse a tag/metadata value as f64, matching Rust's
    ``value.parse::<f64>()`` semantics. Accepts decimal,
    scientific notation (``1e10`` / ``1.5e-3``), the canonical
    ``inf`` / ``-inf`` literals, and NaN (Rust parses NaN too;
    IEEE-754 comparisons against NaN always return False, which
    naturally yields the right predicate result on both sides).

    Q15: pre-fix the numeric branches matched against ``_NUMERIC_RE``
    (decimal-only) before calling ``float()``. That regex rejected
    scientific notation that Rust accepts, so a predicate against
    ``software.gpu.fp16_tflops_x10=1.5e3`` silently failed in Python
    while passing in Rust.

    R2: Rust's ``f64::from_str`` rejects leading / trailing
    whitespace; Python's ``float("  1.5")`` strips it. A tag value
    like ``"  1.5"`` parsed cleanly in Python and rejected in
    Rust diverged numeric-evaluation semantics — explicitly
    reject any input with surrounding whitespace before delegating
    to ``float()``.
    """
    if not s:
        return None
    if s != s.strip():
        # Rust f64 parse rejects whitespace; mirror that here.
        return None
    # N-14: Python's ``float()`` accepts digit-separator underscores
    # (``float("1_000") == 1000.0``); Rust's ``f64::from_str``
    # rejects them. A peer announcing ``hardware.cpu_cores=1_000``
    # then evaluated NumericAtLeast differently across bindings.
    # Hex floats aren't a Python concern (``float("0x1p3")`` already
    # raises ValueError) but underscores need the explicit gate.
    if "_" in s:
        return None
    try:
        return float(s)
    except (ValueError, OverflowError):
        return None


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
    # N-13: lock the accepted-set to exactly what Rust's
    # `parts.next()?.parse::<u64>()` parses (predicate.rs:1782-1784).
    # `str.isdigit()` accepts Unicode digits like "١٢" (Arabic-Indic)
    # that Rust rejects, AND rejects the `+1` Rust accepts. Mirror
    # the same `^\+?[0-9]+$` regex `capability_schema.py:332` uses
    # for the schema validator (R4) so the predicate-side and
    # schema-side accepted-sets agree.
    if not _SEMVER_COMPONENT.match(parts[0]):
        return None
    try:
        major = int(parts[0])
        if len(parts) > 1:
            if not _SEMVER_COMPONENT.match(parts[1]):
                return None
            minor = int(parts[1])
        else:
            minor = 0
        if len(parts) > 2:
            if not _SEMVER_COMPONENT.match(parts[2]):
                return None
            patch = int(parts[2])
        else:
            patch = 0
    except ValueError:
        return None
    return (major, minor, patch)


# N-13: same regex shape as `_U64_LITERAL` in `capability_schema.py`.
# Defined here so the predicate-side parser doesn't pull a
# circular import from the schema module.
_SEMVER_COMPONENT = re.compile(r"^\+?[0-9]+$")


def _semver_compatible(lhs: Tuple[int, int, int], rhs: Tuple[int, int, int]) -> bool:
    """``lhs`` is caret-compatible with ``rhs`` per the standard
    semver rule: same major (or same minor for ``0.x.y``, exact for
    ``0.0.x``), and ``lhs >= rhs``. Mirrors cargo's ``^`` operator
    semantics — kept in lockstep with the Rust ``semver_compatible``
    helper in ``predicate.rs``.
    """
    if lhs < rhs:
        return False
    if rhs[0] == 0:
        if rhs[1] == 0:
            # P1-D: 0.0.x — patch is the compatibility band; anything
            # other than the exact tuple is a breaking change.
            # Combined with the lhs >= rhs guard above this collapses
            # to lhs == rhs.
            return lhs == rhs
        # Q1: 0.x.y — minor is the compatibility band, AND the
        # major must also be 0. Pre-fix `rhs[1] == lhs[1]` alone
        # admitted `lhs = (1, 2, 5)` as compatible with
        # `rhs = (0, 2, 3)` (the lhs >= rhs guard passes since
        # 1 > 0, then minors match). 1.x.y against ^0.2.3 is a
        # major-version regression that should fail.
        return lhs[0] == 0 and rhs[1] == lhs[1]
    return rhs[0] == lhs[0]


def _axis_tag_value(tags: Sequence[str], key: TagKey) -> Optional[str]:
    """Return the matched ``AxisValue`` tag's value, or ``None`` if
    no value-bearing tag matches the (axis, key) pair.

    Q2: pre-fix this also returned ``""`` for ``AxisPresent`` tags,
    which let value predicates (Equals, NumericAtLeast, StringPrefix,
    SemverAtLeast, …) match presence-only tags. The Rust substrate
    requires ``Tag::AxisValue`` for those predicates and never
    pretends a presence tag has an empty value. Use
    :func:`_axis_tag_present` for `Exists` semantics.

    A node may carry BOTH an AxisPresent and an AxisValue tag for
    the same (axis, key); the value scan continues past presence
    matches so the value form wins. Mirrors the Rust substrate's
    full-tag-list iteration.
    """
    prefix = f"{key[0]}.{key[1]}"
    for wire in tags:
        if len(wire) <= len(prefix) or not wire.startswith(prefix):
            continue
        sep = wire[len(prefix)]
        if sep == "=" or sep == ":":
            return wire[len(prefix) + 1:]
    return None


def _axis_tag_present(tags: Sequence[str], key: TagKey) -> bool:
    """True if any ``AxisPresent`` or ``AxisValue`` tag matches the
    (axis, key) pair. Used by `Exists` predicates which match either
    form. Q2: split out from ``_axis_tag_value`` so the latter can
    correctly skip AxisPresent for value predicates.
    """
    prefix = f"{key[0]}.{key[1]}"
    for wire in tags:
        if wire == prefix:
            return True
        if len(wire) > len(prefix) and wire.startswith(prefix):
            sep = wire[len(prefix)]
            if sep == "=" or sep == ":":
                return True
    return False


def _eval_leaf(
    pred: Predicate,
    tags: Sequence[str],
    metadata: Mapping[str, str],
) -> bool:
    if isinstance(pred, _PredExists):
        return _axis_tag_present(tags, pred.key)
    if isinstance(pred, _PredEquals):
        v = _axis_tag_value(tags, pred.key)
        return v is not None and v == pred.value
    if isinstance(pred, _PredNumericAtLeast):
        v = _axis_tag_value(tags, pred.key)
        if v is None:
            return False
        n = _try_parse_float(v)
        return n is not None and n >= pred.threshold
    if isinstance(pred, _PredNumericAtMost):
        v = _axis_tag_value(tags, pred.key)
        if v is None:
            return False
        n = _try_parse_float(v)
        return n is not None and n <= pred.threshold
    if isinstance(pred, _PredNumericInRange):
        v = _axis_tag_value(tags, pred.key)
        if v is None:
            return False
        n = _try_parse_float(v)
        return n is not None and pred.min <= n <= pred.max
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
        if v is None:
            return False
        n = _try_parse_float(v)
        return n is not None and n >= pred.threshold
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


# ============================================================================
# Predicate trace evaluator — Phase 9d slice. Mirrors the substrate's
# ``Predicate::evaluate_with_trace``: children of ``and`` / ``or``
# evaluate in cost-ascending order; short-circuited siblings dropped
# from the trace. Pinned across bindings by ``predicate_trace.json``.
# ============================================================================


@dataclass(frozen=True)
class ClauseTrace:
    """Per-clause trace entry. Mirrors the substrate's ``ClauseTrace``."""

    label: str
    result: bool
    children: Tuple["ClauseTrace", ...] = ()

    def to_wire(self) -> Dict[str, Any]:
        return {
            "label": self.label,
            "result": self.result,
            "children": [c.to_wire() for c in self.children],
        }


def _pred_static_cost(p: Predicate) -> int:
    if isinstance(p, _PredMetadataExists):
        return 10
    if isinstance(p, _PredMetadataEquals):
        return 11
    if isinstance(p, _PredExists):
        return 20
    if isinstance(p, _PredEquals):
        return 21
    if isinstance(p, _PredMetadataNumericAtLeast):
        return 25
    if isinstance(p, (_PredNumericAtLeast, _PredNumericAtMost, _PredNumericInRange)):
        return 30
    if isinstance(p, _PredStringPrefix):
        return 40
    if isinstance(p, _PredMetadataMatches):
        return 45
    if isinstance(p, _PredStringMatches):
        return 50
    if isinstance(p, (_PredSemverAtLeast, _PredSemverAtMost, _PredSemverCompatible)):
        return 60
    if isinstance(p, (_PredAnd, _PredOr)):
        # Saturating sum at 0xFFFFFFFF mirrors the substrate.
        s = 0
        for c in p.children:
            s = min(s + _pred_static_cost(c), 0xFFFFFFFF)
        return s
    if isinstance(p, _PredNot):
        return _pred_static_cost(p.child)
    raise TypeError(f"_pred_static_cost: unknown variant {type(p).__name__}")


def _format_float(n: float) -> str:
    """Match Rust's ``{}`` Display for f64: integers print without
    decimal, fractional values include their digits.

    Magnitude check runs FIRST. ``int(n)`` raises ``ValueError`` on
    NaN and ``OverflowError`` on infinity; the original
    ``n == int(n) and abs(n) < 1e16`` short-circuited in the wrong
    order (Python's ``and`` evaluates left to right), so a NaN
    threshold reached the predicate-debug-report path as a
    runtime exception rather than just falling through to ``repr``.
    """
    import math
    if not math.isfinite(n) or abs(n) >= 1e16:
        return repr(n)
    if n == int(n):
        return str(int(n))
    return repr(n)


def _rust_dbg_string(s: str) -> str:
    """Match Rust's ``{:?}`` debug-format for &str: double-quoted with
    standard escape sequences (matches ``json.dumps`` for plain
    strings)."""
    return __import__("json").dumps(s)


def _pred_debug_label(p: Predicate) -> str:
    def tk(k: TagKey) -> str:
        return f"{k[0]}.{k[1]}"

    if isinstance(p, _PredExists):
        return f"Exists({tk(p.key)})"
    if isinstance(p, _PredEquals):
        return f"Equals({tk(p.key)}={p.value})"
    if isinstance(p, _PredNumericAtLeast):
        return f"NumericAtLeast({tk(p.key)} >= {_format_float(p.threshold)})"
    if isinstance(p, _PredNumericAtMost):
        return f"NumericAtMost({tk(p.key)} <= {_format_float(p.threshold)})"
    if isinstance(p, _PredNumericInRange):
        return (
            f"NumericInRange({tk(p.key)} in "
            f"[{_format_float(p.min)}, {_format_float(p.max)}])"
        )
    if isinstance(p, _PredSemverAtLeast):
        return f"SemverAtLeast({tk(p.key)} >= {p.version})"
    if isinstance(p, _PredSemverAtMost):
        return f"SemverAtMost({tk(p.key)} <= {p.version})"
    if isinstance(p, _PredSemverCompatible):
        return f"SemverCompatible({tk(p.key)} ~= {p.version})"
    if isinstance(p, _PredStringPrefix):
        return f"StringPrefix({tk(p.key)} starts with {_rust_dbg_string(p.prefix)})"
    if isinstance(p, _PredStringMatches):
        return f"StringMatches({tk(p.key)} contains {_rust_dbg_string(p.pattern)})"
    if isinstance(p, _PredMetadataExists):
        return f"MetadataExists({p.key})"
    if isinstance(p, _PredMetadataEquals):
        return f"MetadataEquals({p.key}={p.value})"
    if isinstance(p, _PredMetadataMatches):
        return f"MetadataMatches({p.key} contains {_rust_dbg_string(p.pattern)})"
    if isinstance(p, _PredMetadataNumericAtLeast):
        return (
            f"MetadataNumericAtLeast({p.key} >= {_format_float(p.threshold)})"
        )
    if isinstance(p, _PredAnd):
        return f"And({len(p.children)} clauses)"
    if isinstance(p, _PredOr):
        return f"Or({len(p.children)} clauses)"
    if isinstance(p, _PredNot):
        return "Not"
    raise TypeError(f"_pred_debug_label: unknown variant {type(p).__name__}")


def _plan_children(children: Sequence[Predicate]) -> List[Predicate]:
    """Stable sort by static_cost ascending."""
    indexed = list(enumerate(children))
    # Python's sort is stable; sorting by cost preserves declaration
    # order for ties (matches Rust's `sort_by_key`).
    indexed.sort(key=lambda it: _pred_static_cost(it[1]))
    return [c for _i, c in indexed]


def evaluate_predicate_with_trace(
    pred: Predicate,
    tags: Sequence[str],
    metadata: Mapping[str, str],
) -> Tuple[bool, ClauseTrace]:
    """Evaluate + produce a trace tree. Mirrors the substrate's
    ``Predicate::evaluate_with_trace``: cost-ordered, short-circuiting,
    drops siblings that didn't run from the trace. Pinned across
    bindings by ``predicate_trace.json``."""
    label = _pred_debug_label(pred)
    if isinstance(pred, _PredAnd):
        ordered = _plan_children(pred.children)
        traces: List[ClauseTrace] = []
        result = True
        for c in ordered:
            r, t = evaluate_predicate_with_trace(c, tags, metadata)
            traces.append(t)
            if not r:
                result = False
                break
        return result, ClauseTrace(label=label, result=result, children=tuple(traces))
    if isinstance(pred, _PredOr):
        ordered = _plan_children(pred.children)
        traces = []
        result = False
        for c in ordered:
            r, t = evaluate_predicate_with_trace(c, tags, metadata)
            traces.append(t)
            if r:
                result = True
                break
        return result, ClauseTrace(label=label, result=result, children=tuple(traces))
    if isinstance(pred, _PredNot):
        r, t = evaluate_predicate_with_trace(pred.child, tags, metadata)
        return not r, ClauseTrace(label=label, result=not r, children=(t,))
    r = _eval_leaf(pred, tags, metadata)
    return r, ClauseTrace(label=label, result=r, children=())


# ============================================================================
# PredicateDebugReport — aggregate per-clause stats over a corpus.
#
# Mirrors the substrate's ``PredicateDebugReport::from_evaluations``.
# Pinned across bindings by ``predicate_debug_report.json``.
# ============================================================================


@dataclass(frozen=True)
class ClauseStats:
    """Per-clause aggregated stats. Mirrors the substrate."""

    label: str
    evaluated: int
    matched: int

    def to_wire(self) -> Dict[str, Any]:
        return {
            "label": self.label,
            "evaluated": self.evaluated,
            "matched": self.matched,
        }


@dataclass(frozen=True)
class PredicateDebugReport:
    """Aggregate report from running a predicate across a corpus."""

    total_candidates: int = 0
    matched: int = 0
    clause_stats: Tuple[ClauseStats, ...] = ()

    def to_wire(self) -> Dict[str, Any]:
        return {
            "total_candidates": self.total_candidates,
            "matched": self.matched,
            "clause_stats": [s.to_wire() for s in self.clause_stats],
        }

    def render(self) -> str:
        """One-line-per-clause text summary suitable for CLI output."""
        def pct(num: int, denom: int) -> str:
            if denom == 0:
                return "0.0%"
            return f"{(100 * num / denom):.1f}%"

        lines: List[str] = []
        lines.append("Predicate evaluation report")
        lines.append("─────────────────────────────────────────")
        lines.append(f"Total candidates: {self.total_candidates}")
        lines.append(
            f"Matched:          {self.matched} ({pct(self.matched, self.total_candidates)})"
        )
        lines.append("")
        lines.append("Per-clause stats (alphabetical):")
        for s in self.clause_stats:
            lines.append(
                f"  {s.label:<60} evaluated {s.evaluated:>5}, "
                f"matched {s.matched:>5} ({pct(s.matched, s.evaluated)})"
            )
        return "\n".join(lines) + "\n"


def _accumulate_trace(
    trace: ClauseTrace, acc: Dict[str, List[int]]
) -> None:
    """Walk trace post-order, updating acc[label] = [evaluated, matched]."""
    entry = acc.setdefault(trace.label, [0, 0])
    entry[0] += 1
    if trace.result:
        entry[1] += 1
    for child in trace.children:
        _accumulate_trace(child, acc)


def predicate_debug_report(
    pred: Predicate,
    contexts: Sequence[Mapping[str, Any]],
) -> PredicateDebugReport:
    """Run ``pred`` against each context in ``contexts``, accumulating
    per-clause hit / miss stats. Mirrors the substrate's
    ``PredicateDebugReport::from_evaluations``.

    Each context is a mapping with ``tags`` (sequence of wire strings)
    and ``metadata`` (str → str map). Returns a :class:`PredicateDebugReport`
    with ``clause_stats`` sorted by label (BTreeMap semantics)."""
    acc: Dict[str, List[int]] = {}
    matched = 0
    for ctx in contexts:
        tags = list(ctx.get("tags", ()))
        metadata = dict(ctx.get("metadata", {}))
        r, trace = evaluate_predicate_with_trace(pred, tags, metadata)
        if r:
            matched += 1
        _accumulate_trace(trace, acc)

    sorted_labels = sorted(acc.keys())
    stats = tuple(
        ClauseStats(label=lbl, evaluated=acc[lbl][0], matched=acc[lbl][1])
        for lbl in sorted_labels
    )
    return PredicateDebugReport(
        total_candidates=len(contexts),
        matched=matched,
        clause_stats=stats,
    )


# ============================================================================
# Redaction + JSON round-trip — Phase 9d redaction.
#
# `redact_metadata_keys` rewrites metadata-clause labels to hide
# sensitive predicate values before persistence. `predicate_debug_report_from_wire`
# is the symmetric inverse of `report.to_wire()` for save/replay.
# Pinned across bindings by `predicate_debug_report_redacted.json`.
# ============================================================================


def _redact_label(label: str, keys: frozenset) -> str:
    """Rewrite a metadata-clause label to hide its value, when
    the clause's metadata key is in ``keys``.

    P2-O: pre-fix this used regexes like
    ``r"^MetadataEquals\\(([^=]+)=(.+)\\)$"`` for the
    ``MetadataEquals`` form. The ``[^=]+`` group explicitly
    forbids the key from containing ``=``, so a redact-key like
    ``"k=v"`` or any user-emitted metadata key with a literal
    ``=`` never matched and the secret stayed in the label.
    Substrate metadata is ``BTreeMap<String, String>`` and accepts
    arbitrary keys; the redaction must too. Mirrors the Rust
    ``redact_label`` fix in CR-19.

    Strategy: try the redact keys longest-first against the label
    interior. The first key that matches (as the literal prefix
    before the separator) wins. Keeps the shape of the substrate's
    ``redact_label`` helper.
    """
    if not keys:
        return label
    sorted_keys = sorted(keys, key=len, reverse=True)

    for prefix, suffix, sep, replacement in (
        ("MetadataEquals(", ")", "=", "MetadataEquals({key}=<redacted>)"),
        (
            "MetadataMatches(",
            ")",
            ' contains "',
            'MetadataMatches({key} contains "<redacted>")',
        ),
        (
            "MetadataNumericAtLeast(",
            ")",
            " >= ",
            "MetadataNumericAtLeast({key} >= <redacted>)",
        ),
    ):
        if not label.startswith(prefix) or not label.endswith(suffix):
            continue
        inner = label[len(prefix) : -len(suffix)]
        # MetadataMatches's suffix is `")` — the inner piece is
        # `<key> contains "<pattern>` with the trailing `"` already
        # absorbed by the suffix, so the key+sep prefix check is
        # `<key> contains "`. Same shape; nothing special.
        for key in sorted_keys:
            if inner.startswith(f"{key}{sep}"):
                return replacement.format(key=key)
        return label
    return label


def redact_metadata_keys(
    report: PredicateDebugReport, keys: Sequence[str]
) -> PredicateDebugReport:
    """Rewrite metadata-clause values in a debug report to hide
    sensitive values before persistence / sharing.

    Walks the report's ``clause_stats`` and rewrites any label whose
    metadata key is in ``keys``:

    - ``MetadataEquals(<key>=<value>)`` → ``MetadataEquals(<key>=<redacted>)``
    - ``MetadataMatches(<key> contains "<pattern>")`` → ``MetadataMatches(<key> contains "<redacted>")``
    - ``MetadataNumericAtLeast(<key> >= <threshold>)`` → ``MetadataNumericAtLeast(<key> >= <redacted>)``
    - ``MetadataExists(<key>)`` — unchanged (no value to redact).
    - All non-metadata labels unchanged.

    After rewriting, stats with the same redacted label are merged
    (``evaluated`` and ``matched`` summed). Output is sorted by label.

    Idempotent: ``redact(redact(r, k), k) == redact(r, k)``."""
    key_set = frozenset(keys)
    merged: Dict[str, List[int]] = {}
    for stat in report.clause_stats:
        new_label = _redact_label(stat.label, key_set)
        entry = merged.setdefault(new_label, [0, 0])
        entry[0] += stat.evaluated
        entry[1] += stat.matched
    sorted_labels = sorted(merged.keys())
    new_stats = tuple(
        ClauseStats(label=lbl, evaluated=merged[lbl][0], matched=merged[lbl][1])
        for lbl in sorted_labels
    )
    return PredicateDebugReport(
        total_candidates=report.total_candidates,
        matched=report.matched,
        clause_stats=new_stats,
    )


def predicate_debug_report_from_wire(
    wire: Mapping[str, Any]
) -> PredicateDebugReport:
    """Reconstruct a :class:`PredicateDebugReport` from its wire JSON
    form. Symmetric inverse of ``report.to_wire()``."""
    if not isinstance(wire, Mapping):
        raise TypeError(
            f"predicate_debug_report_from_wire: expected mapping, got {type(wire).__name__}"
        )
    if (
        "total_candidates" not in wire
        or "matched" not in wire
        or "clause_stats" not in wire
    ):
        raise ValueError(
            "predicate_debug_report_from_wire: missing required field "
            "(total_candidates / matched / clause_stats)"
        )
    total = int(wire["total_candidates"])
    matched_n = int(wire["matched"])
    raw_stats = wire["clause_stats"]
    if not isinstance(raw_stats, list):
        raise TypeError("predicate_debug_report_from_wire: clause_stats must be a list")
    stats: List[ClauseStats] = []
    for s in raw_stats:
        if not isinstance(s, Mapping):
            raise ValueError(
                f"predicate_debug_report_from_wire: bad clause_stats entry {s!r}"
            )
        if "label" not in s or "evaluated" not in s or "matched" not in s:
            raise ValueError(
                f"predicate_debug_report_from_wire: missing field in clause_stats entry {dict(s)!r}"
            )
        stats.append(
            ClauseStats(
                label=str(s["label"]),
                evaluated=int(s["evaluated"]),
                matched=int(s["matched"]),
            )
        )
    return PredicateDebugReport(
        total_candidates=total,
        matched=matched_n,
        clause_stats=tuple(stats),
    )


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
    "where_header",
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
    "ClauseTrace",
    "evaluate_predicate_with_trace",
    "ClauseStats",
    "PredicateDebugReport",
    "predicate_debug_report",
    "redact_metadata_keys",
    "predicate_debug_report_from_wire",
]
