"""
net-sdk — Ergonomic Python SDK for the Net mesh network.

Example:
    >>> from net_sdk import NetNode
    >>> node = NetNode(shards=4)
    >>> node.emit({'token': 'hello', 'index': 0})
    >>> for event in node.subscribe():
    ...     print(event.raw)
    >>> node.shutdown()
"""

from net_sdk.capability import (
    RESERVED_PREFIXES,
    RPC_WHERE_HEADER,
    TAXONOMY_AXES,
    AxisSeparator,
    CapabilitySetDiff,
    CapabilitySetWire,
    MetadataChange,
    MetadataChangeAdded,
    MetadataChangeRemoved,
    MetadataChangeUpdated,
    PlacementCandidate,
    PlacementFilterFn,
    Predicate,
    RegisteredPlacementFilter,
    StandardPlacement,
    StandardPlacementBuilder,
    Tag,
    TagAxisPresent,
    TagAxisValue,
    TagKey,
    TagLegacy,
    TagReserved,
    TaxonomyAxis,
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
    starts_with_reserved_prefix,
    tag_from_string,
    tag_from_user_string,
    tag_key,
    tag_to_string,
    with_metadata,
)
from net_sdk.channel import TypedChannel
from net_sdk.mesh import (
    BackpressureError,
    MeshNode,
    MeshStream,
    NotConnectedError,
    Reliability,
    StreamStats,
)
from net_sdk.node import NetNode
from net_sdk.stream import EventStream, TypedEventStream

__all__ = [
    "NetNode",
    "EventStream",
    "TypedEventStream",
    "TypedChannel",
    "MeshNode",
    "MeshStream",
    "StreamStats",
    "Reliability",
    "BackpressureError",
    "NotConnectedError",
    # Capability-System Enhancements.
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

__version__ = "0.12.0"
