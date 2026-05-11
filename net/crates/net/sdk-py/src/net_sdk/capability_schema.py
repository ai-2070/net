"""
Capability axis schema + ``validate_capabilities`` — Phase 9a of
``CAPABILITY_SYSTEM_SDK_PLAN.md``.

Mirrors the substrate's ``AXIS_SCHEMA`` const + the canonical
``validate_capabilities`` validator. The wire shape of the
``ValidationReport`` (lowercase ``kind`` discriminator, axis as
lowercase string, value type as lowercase string) is pinned by the
cross-binding fixture
``tests/cross_lang_capability/capability_validation.json``.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from typing import Any, Dict, List, Literal, Mapping, Optional, Tuple, Union

from net_sdk.capability import (
    CapabilitySetWire,
    TagAxisPresent,
    TagAxisValue,
    TagLegacy,
    TagReserved,
    TaxonomyAxis,
    tag_from_string,
)

# ============================================================================
# Schema vocabulary
# ============================================================================

ValueType = Literal["presence", "number", "string", "enumeration", "bool", "csv"]


@dataclass(frozen=True)
class KeyEntry:
    key: str
    value_type: ValueType


@dataclass(frozen=True)
class KeyShapeIndexedCollection:
    pass


@dataclass(frozen=True)
class KeyShapeKeyedMap:
    value_type: ValueType


KeyShapeKind = Union[KeyShapeIndexedCollection, KeyShapeKeyedMap]


@dataclass(frozen=True)
class KeyShape:
    prefix: str
    shape: KeyShapeKind
    sub_keys: Tuple[KeyEntry, ...] = ()


@dataclass(frozen=True)
class AxisEntry:
    keys: Tuple[KeyEntry, ...] = ()
    shapes: Tuple[KeyShape, ...] = ()


@dataclass(frozen=True)
class AxisSchema:
    hardware: AxisEntry
    software: AxisEntry
    devices: AxisEntry
    dataforts: AxisEntry
    metadata_reserved: Tuple[str, ...]
    metadata_reserved_prefixes: Tuple[str, ...]


# ============================================================================
# AXIS_SCHEMA — mirrors `behavior::schema::AXIS_SCHEMA`.
# ============================================================================

_HARDWARE_KEYS: Tuple[KeyEntry, ...] = (
    KeyEntry("cpu_cores", "number"),
    KeyEntry("cpu_threads", "number"),
    KeyEntry("memory_gb", "number"),
    KeyEntry("gpu", "presence"),
    KeyEntry("gpu.vendor", "enumeration"),
    KeyEntry("gpu.model", "string"),
    KeyEntry("gpu.vram_gb", "number"),
    KeyEntry("gpu.compute_units", "number"),
    KeyEntry("gpu.tensor_cores", "number"),
    KeyEntry("gpu.fp16_tflops_x10", "number"),
    KeyEntry("storage_gb", "number"),
    KeyEntry("network_mbps", "number"),
    KeyEntry("limits.max_concurrent_requests", "number"),
    KeyEntry("limits.max_tokens_per_request", "number"),
    KeyEntry("limits.rate_limit_rpm", "number"),
    KeyEntry("limits.max_batch_size", "number"),
    KeyEntry("limits.max_input_bytes", "number"),
    KeyEntry("limits.max_output_bytes", "number"),
)

_SOFTWARE_KEYS: Tuple[KeyEntry, ...] = (
    KeyEntry("os", "string"),
    KeyEntry("os_version", "string"),
    KeyEntry("cuda_version", "string"),
)

_SOFTWARE_SHAPES: Tuple[KeyShape, ...] = (
    KeyShape("runtime.", KeyShapeKeyedMap(value_type="string"), ()),
    KeyShape("framework.", KeyShapeKeyedMap(value_type="string"), ()),
    KeyShape("driver.", KeyShapeKeyedMap(value_type="string"), ()),
    KeyShape(
        "model.",
        KeyShapeIndexedCollection(),
        (
            KeyEntry("id", "string"),
            KeyEntry("family", "string"),
            KeyEntry("parameters_b_x10", "number"),
            KeyEntry("context_length", "number"),
            KeyEntry("quantization", "string"),
            KeyEntry("modalities", "csv"),
            KeyEntry("tokens_per_sec", "number"),
            KeyEntry("loaded", "bool"),
        ),
    ),
    KeyShape(
        "tool.",
        KeyShapeIndexedCollection(),
        (
            KeyEntry("tool_id", "string"),
            KeyEntry("name", "string"),
            KeyEntry("version", "string"),
            KeyEntry("requires", "csv"),
            KeyEntry("estimated_time_ms", "number"),
            KeyEntry("stateless", "bool"),
        ),
    ),
)

#: Reserved metadata keys (substrate-defined).
METADATA_RESERVED_KEYS: Tuple[str, ...] = (
    "intent",
    "colocate-with",
    "colocate-with-strict",
    "priority",
    "owner",
)

#: Reserved metadata-key prefixes.
METADATA_RESERVED_PREFIXES: Tuple[str, ...] = ("tool::",)

#: Default soft cap for ``metadata`` total size.
METADATA_SOFT_CAP_BYTES: int = 4 * 1024

#: The canonical axis schema. Mirrors ``behavior::schema::AXIS_SCHEMA``.
AXIS_SCHEMA: AxisSchema = AxisSchema(
    hardware=AxisEntry(keys=_HARDWARE_KEYS, shapes=()),
    software=AxisEntry(keys=_SOFTWARE_KEYS, shapes=_SOFTWARE_SHAPES),
    devices=AxisEntry(),
    dataforts=AxisEntry(),
    metadata_reserved=METADATA_RESERVED_KEYS,
    metadata_reserved_prefixes=METADATA_RESERVED_PREFIXES,
)


# ============================================================================
# ValidationReport wire types
# ============================================================================


@dataclass(frozen=True)
class SchemaErrorUnknownAxis:
    axis_prefix: str
    tag: str

    def to_wire(self) -> Dict[str, Any]:
        return {"kind": "unknown_axis", "axis_prefix": self.axis_prefix, "tag": self.tag}


@dataclass(frozen=True)
class SchemaErrorTypeMismatch:
    axis: TaxonomyAxis
    key: str
    expected: ValueType
    actual: str

    def to_wire(self) -> Dict[str, Any]:
        return {
            "kind": "type_mismatch",
            "axis": self.axis,
            "key": self.key,
            "expected": self.expected,
            "actual": self.actual,
        }


@dataclass(frozen=True)
class SchemaErrorIndexMalformed:
    axis: TaxonomyAxis
    prefix: str
    index: str
    tag: str

    def to_wire(self) -> Dict[str, Any]:
        return {
            "kind": "index_malformed",
            "axis": self.axis,
            "prefix": self.prefix,
            "index": self.index,
            "tag": self.tag,
        }


SchemaError = Union[
    SchemaErrorUnknownAxis, SchemaErrorTypeMismatch, SchemaErrorIndexMalformed
]


@dataclass(frozen=True)
class WarningUnknownKey:
    axis: TaxonomyAxis
    key: str

    def to_wire(self) -> Dict[str, Any]:
        return {"kind": "unknown_key", "axis": self.axis, "key": self.key}


@dataclass(frozen=True)
class WarningMetadataOversize:
    soft_cap_bytes: int
    actual_bytes: int

    def to_wire(self) -> Dict[str, Any]:
        return {
            "kind": "metadata_oversize",
            "soft_cap_bytes": self.soft_cap_bytes,
            "actual_bytes": self.actual_bytes,
        }


@dataclass(frozen=True)
class WarningLegacyTag:
    tag: str

    def to_wire(self) -> Dict[str, Any]:
        return {"kind": "legacy_tag", "tag": self.tag}


# P2-L: mirror substrate CR-14 reserved-metadata warnings.
@dataclass(frozen=True)
class WarningMetadataReservedKey:
    key: str

    def to_wire(self) -> Dict[str, Any]:
        return {"kind": "metadata_reserved_key", "key": self.key}


@dataclass(frozen=True)
class WarningMetadataReservedPrefix:
    key: str
    prefix: str

    def to_wire(self) -> Dict[str, Any]:
        return {
            "kind": "metadata_reserved_prefix",
            "key": self.key,
            "prefix": self.prefix,
        }


ValidationWarning = Union[
    WarningUnknownKey,
    WarningMetadataOversize,
    WarningLegacyTag,
    WarningMetadataReservedKey,
    WarningMetadataReservedPrefix,
]


@dataclass(frozen=True)
class ValidationReport:
    errors: Tuple[SchemaError, ...] = ()
    warnings: Tuple[ValidationWarning, ...] = ()

    def is_clean(self) -> bool:
        """True iff there are zero errors and zero warnings."""
        return not self.errors and not self.warnings

    def is_valid(self) -> bool:
        """True iff there are zero errors. Warnings are allowed."""
        return not self.errors

    def to_wire(self) -> Dict[str, Any]:
        """Project onto the canonical wire shape. Lists are NOT
        sorted here; callers needing canonical comparison sort by
        ``json.dumps`` themselves."""
        return {
            "errors": [e.to_wire() for e in self.errors],
            "warnings": [w.to_wire() for w in self.warnings],
        }


# ============================================================================
# Validator
# ============================================================================


def _axis_entry(schema: AxisSchema, axis: TaxonomyAxis) -> AxisEntry:
    if axis == "hardware":
        return schema.hardware
    if axis == "software":
        return schema.software
    if axis == "devices":
        return schema.devices
    if axis == "dataforts":
        return schema.dataforts
    raise ValueError(f"unknown axis: {axis!r}")


# R4: ASCII-only digits + optional leading `+`. Mirrors Rust's
# `u64::from_str` accepted-set:
#
#   * Rust accepts ``"42"`` and ``"+42"`` (one optional leading
#     ``+``); rejects ``"-1"`` (covered separately by P1-C),
#     leading whitespace, and non-ASCII digits like ``"١٢"``
#     (Arabic-Indic 1, 2).
#   * Python's ``str.isdigit()`` accepts the Unicode digits and
#     rejects the leading ``+``, so the two diverge in opposite
#     directions if we rely on it. The regex locks the accepted
#     set to exactly what Rust parses.
_U64_LITERAL = re.compile(r"^\+?[0-9]+$")


def _check_value(
    entry: KeyEntry,
    observed_type: ValueType,
    observed_value: Optional[str],
    axis: TaxonomyAxis,
    errors: List[SchemaError],
) -> None:
    if entry.value_type == "presence":
        if observed_type != "presence":
            errors.append(
                SchemaErrorTypeMismatch(
                    axis=axis,
                    key=entry.key,
                    expected="presence",
                    actual=observed_value if observed_value is not None else "",
                )
            )
        return
    if observed_value is None:
        errors.append(
            SchemaErrorTypeMismatch(
                axis=axis,
                key=entry.key,
                expected=entry.value_type,
                actual="<no value>",
            )
        )
        return
    parses = False
    if entry.value_type == "number":
        # Substrate `Number` is unsigned u64 — see CR-15 in the
        # capability-system-2 review and `schema.rs::ValueType::Number`.
        # Negative values AND values that exceed u64::MAX surface
        # as TypeMismatch on the substrate side; mirror both
        # conditions here so client-side validation doesn't pass
        # a CapabilitySet the substrate would later reject.
        #
        # Q4: pre-fix `bool(observed_value) and observed_value.isdigit()`
        # only ruled out negatives + non-digits, but admitted
        # values like "18446744073709551616" (u64::MAX + 1) which
        # the Rust `value.parse::<u64>()` rejects.
        #
        # R4: `str.isdigit()` diverged from Rust's `u64::from_str`
        # in two opposite directions:
        #   - Python rejects "+1" (the `+` isn't a digit) while
        #     Rust accepts an optional leading `+`.
        #   - Python accepts Unicode digits like "١٢" (Arabic-
        #     Indic) which Rust rejects (ASCII-only).
        # Use an ASCII-digit + optional-`+` regex to lock the
        # accepted-set to exactly what Rust parses.
        if observed_value and _U64_LITERAL.match(observed_value):
            try:
                parsed = int(observed_value)
                parses = 0 <= parsed <= 0xFFFF_FFFF_FFFF_FFFF
            except ValueError:
                parses = False
    elif entry.value_type in ("string", "enumeration", "csv"):
        parses = bool(observed_value)
    elif entry.value_type == "bool":
        parses = observed_value in ("true", "false")
    if not parses:
        errors.append(
            SchemaErrorTypeMismatch(
                axis=axis,
                key=entry.key,
                expected=entry.value_type,
                actual=observed_value,
            )
        )


def _validate_axis_key(
    axis: TaxonomyAxis,
    key: str,
    observed_type: ValueType,
    observed_value: Optional[str],
    schema: AxisSchema,
    errors: List[SchemaError],
    warnings: List[ValidationWarning],
    tag_wire: str,
) -> None:
    entry = _axis_entry(schema, axis)
    fixed = next((e for e in entry.keys if e.key == key), None)
    if fixed is not None:
        _check_value(fixed, observed_type, observed_value, axis, errors)
        return
    for shape in entry.shapes:
        if not key.startswith(shape.prefix):
            continue
        rest = key[len(shape.prefix):]
        if isinstance(shape.shape, KeyShapeIndexedCollection):
            dot = rest.find(".")
            if dot < 0:
                continue
            idx = rest[:dot]
            sub = rest[dot + 1:]
            # N-5: substrate parses the index as `u32` (schema.rs:616).
            # `str.isdigit()` accepts Unicode digits and unbounded
            # digit runs; both diverge from Rust. Mirror the
            # `_U64_LITERAL`-style accepted-set with an explicit
            # `u32::MAX` ceiling so cross-binding fixture rows agree.
            if not _U64_LITERAL.match(idx) or int(idx) > 0xFFFF_FFFF:
                errors.append(
                    SchemaErrorIndexMalformed(
                        axis=axis, prefix=shape.prefix, index=idx, tag=tag_wire
                    )
                )
                return
            sub_entry = next((e for e in shape.sub_keys if e.key == sub), None)
            if sub_entry is not None:
                _check_value(sub_entry, observed_type, observed_value, axis, errors)
                return
            warnings.append(WarningUnknownKey(axis=axis, key=key))
            return
        # KeyedMap: rest IS the user-defined name.
        if isinstance(shape.shape, KeyShapeKeyedMap):
            if rest:
                synth = KeyEntry(shape.prefix, shape.shape.value_type)
                _check_value(synth, observed_type, observed_value, axis, errors)
                return
    warnings.append(WarningUnknownKey(axis=axis, key=key))


def validate_capabilities(
    caps: Union[CapabilitySetWire, Mapping[str, Any]],
    schema: AxisSchema = AXIS_SCHEMA,
) -> ValidationReport:
    """Validate a wire-format capability set against a schema. Mirrors
    the substrate's ``validate_capabilities``."""
    if isinstance(caps, CapabilitySetWire):
        tags = list(caps.tags)
        metadata = dict(caps.metadata)
    else:
        tags = list(caps.get("tags", ()))
        metadata = dict(caps.get("metadata", {}))

    errors: List[SchemaError] = []
    warnings: List[ValidationWarning] = []

    for wire in tags:
        tag = tag_from_string(wire)
        if isinstance(tag, TagAxisPresent):
            _validate_axis_key(
                tag.axis, tag.key, "presence", None, schema, errors, warnings, wire
            )
        elif isinstance(tag, TagAxisValue):
            _validate_axis_key(
                tag.axis, tag.key, "string", tag.value, schema, errors, warnings, wire
            )
        elif isinstance(tag, TagReserved):
            # Reserved-prefix tags pass through unchecked.
            pass
        elif isinstance(tag, TagLegacy):
            warnings.append(WarningLegacyTag(tag=tag.raw))

    # P2-L: metadata-key reservation check. The schema declares
    # `metadata_reserved` (exact-match) and
    # `metadata_reserved_prefixes` (prefix-match) but pre-fix the
    # Python validator never consulted them — a user's
    # `with_metadata("intent", …)` smuggling onto a scheduler-
    # reserved key emitted no warning. Mirrors the substrate's
    # CR-14 fix.
    for key in metadata.keys():
        key_str = str(key)
        if key_str in schema.metadata_reserved:
            warnings.append(WarningMetadataReservedKey(key=key_str))
            continue
        for prefix in schema.metadata_reserved_prefixes:
            if key_str.startswith(prefix):
                warnings.append(
                    WarningMetadataReservedPrefix(key=key_str, prefix=prefix)
                )
                break

    # The wire shape says metadata is `{str: str}`, but Python's
    # untyped dicts let callers smuggle through `int` / `bool` /
    # `None` etc. Coerce both halves to `str` for the size
    # accounting so an oversize report ships instead of a `TypeError`
    # inside `len()`. The stringified form is what would actually
    # land on the wire after JSON-encoding, so this is also the
    # correct bound for the `METADATA_SOFT_CAP_BYTES` check.
    #
    # Q3: count UTF-8 bytes, not Python characters. Pre-fix
    # `len(str(k))` returned the code-point count, which under-
    # counts non-ASCII content (e.g. CJK characters take 3 UTF-8
    # bytes each but `len()` returns 1). The substrate's
    # `METADATA_SOFT_CAP_BYTES` is a byte budget; the wire format
    # is JSON-encoded UTF-8, so the right comparison is byte length.
    metadata_bytes = sum(
        len(str(k).encode("utf-8")) + len(str(v).encode("utf-8"))
        for k, v in metadata.items()
    )
    if metadata_bytes > METADATA_SOFT_CAP_BYTES:
        warnings.append(
            WarningMetadataOversize(
                soft_cap_bytes=METADATA_SOFT_CAP_BYTES,
                actual_bytes=metadata_bytes,
            )
        )

    return ValidationReport(errors=tuple(errors), warnings=tuple(warnings))


__all__ = [
    "ValueType",
    "KeyEntry",
    "KeyShape",
    "KeyShapeKind",
    "KeyShapeIndexedCollection",
    "KeyShapeKeyedMap",
    "AxisEntry",
    "AxisSchema",
    "AXIS_SCHEMA",
    "METADATA_RESERVED_KEYS",
    "METADATA_RESERVED_PREFIXES",
    "METADATA_SOFT_CAP_BYTES",
    "SchemaError",
    "SchemaErrorUnknownAxis",
    "SchemaErrorTypeMismatch",
    "SchemaErrorIndexMalformed",
    "ValidationWarning",
    "WarningUnknownKey",
    "WarningMetadataOversize",
    "WarningLegacyTag",
    "WarningMetadataReservedKey",
    "WarningMetadataReservedPrefix",
    "ValidationReport",
    "validate_capabilities",
]
