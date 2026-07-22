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
    ClauseStats,
    ClauseTrace,
    MetadataChange,
    MetadataChangeAdded,
    MetadataChangeRemoved,
    MetadataChangeUpdated,
    PlacementCandidate,
    PlacementFilterFn,
    Predicate,
    PredicateDebugReport,
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
    evaluate_predicate_with_trace,
    p,
    placement_filter_from_fn,
    predicate_debug_report,
    predicate_debug_report_from_wire,
    predicate_from_rpc_header,
    predicate_from_wire,
    predicate_to_rpc_header,
    predicate_to_wire,
    redact_metadata_keys,
    require_axis_value,
    require_tag,
    standard_placement,
    starts_with_reserved_prefix,
    tag_from_string,
    tag_from_user_string,
    tag_key,
    tag_to_string,
    where_header,
    with_metadata,
)
from net_sdk.capability_aggregation import (
    AggregateRow,
    Aggregation,
    AggregationCls,
    CapacityQuery,
    CapacityRow,
    GroupBy,
    GroupByCls,
    TagMatcher,
    TagMatcherCls,
    TaxonomyAxis,
)
from net_sdk.capability_schema import (
    AXIS_SCHEMA,
    METADATA_RESERVED_KEYS,
    METADATA_RESERVED_PREFIXES,
    METADATA_SOFT_CAP_BYTES,
    AxisEntry,
    AxisSchema,
    KeyEntry,
    KeyShape,
    KeyShapeIndexedCollection,
    KeyShapeKeyedMap,
    KeyShapeKind,
    SchemaError,
    SchemaErrorIndexMalformed,
    SchemaErrorTypeMismatch,
    SchemaErrorUnknownAxis,
    ValidationReport,
    ValidationWarning,
    ValueType,
    WarningLegacyTag,
    WarningMetadataOversize,
    WarningUnknownKey,
    validate_capabilities,
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
    # Capability-aggregation surface (Phase 6c).
    "Aggregation",
    "AggregationCls",
    "AggregateRow",
    "CapacityQuery",
    "CapacityRow",
    "GroupBy",
    "GroupByCls",
    "TagMatcher",
    "TagMatcherCls",
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
    # Phase 9a — axis schema + validator.
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
    "ValidationReport",
    "validate_capabilities",
]

# AI tool calling — serve_tool / call_tool / streaming variants +
# the four provider format translators (openai / anthropic / mcp /
# gemini). Detailed shape lives in ``net_sdk.tool``, which
# re-exports from ``net.tool`` (the maturin-built wheel). Users who
# want only the tool layer can also import from ``net_sdk.tool``
# directly.
from net_sdk.tool import (  # noqa: E402
    TOOL_METADATA_FETCH_SERVICE,
    ToolCallParseError,
    ToolCallSpec,
    ToolDescriptor,
    ToolEvent,
    ToolEventDelta,
    ToolEventError,
    ToolEventProgress,
    ToolEventResult,
    ToolEventStart,
    ToolListChange,
    ToolListChangeAdded,
    ToolListChangeNodeCountChanged,
    ToolListChangeRemoved,
    ToolServeHandle,
    add_tool_capabilities_to_announce,
    anthropic,
    call_tool,
    call_tool_async,
    call_tool_streaming,
    call_tool_streaming_async,
    descriptor_for,
    fetch_tool_metadata,
    fetch_tool_metadata_async,
    gemini,
    is_terminal_event,
    list_tools,
    mcp,
    openai,
    serve_tool,
    serve_tool_async,
    serve_tool_streaming,
    serve_tool_streaming_async,
    watch_tools,
)

__all__ += [
    "TOOL_METADATA_FETCH_SERVICE",
    "ToolCallParseError",
    "ToolCallSpec",
    "ToolDescriptor",
    "ToolEvent",
    "ToolEventDelta",
    "ToolEventError",
    "ToolEventProgress",
    "ToolEventResult",
    "ToolEventStart",
    "ToolListChange",
    "ToolListChangeAdded",
    "ToolListChangeNodeCountChanged",
    "ToolListChangeRemoved",
    "ToolServeHandle",
    "add_tool_capabilities_to_announce",
    "anthropic",
    "call_tool",
    "call_tool_async",
    "call_tool_streaming",
    "call_tool_streaming_async",
    "descriptor_for",
    "fetch_tool_metadata",
    "fetch_tool_metadata_async",
    "gemini",
    "is_terminal_event",
    "list_tools",
    "mcp",
    "openai",
    "serve_tool",
    "serve_tool_async",
    "serve_tool_streaming",
    "serve_tool_streaming_async",
    "watch_tools",
]

# Consent, pins, and the native consent-gated capability gateway — the bridge's
# demand surface (`HERMES_INTEGRATION_PLAN.md` Phase 1). Re-exported from the
# `net` wheel via `net_sdk.consent`; `CapabilityGateway` is present iff the
# wheel was built with the `net` + `mcp` features (the default one is).
from net_sdk.consent import (  # noqa: E402
    AsyncPinStore,
    AsyncPinWatcher,
    CapabilityId,
    ConsentPolicy,
    PinChange,
    PinsError,
    PinStore,
    credential_requires_consent,
    default_pin_store_path,
)

__all__ += [
    "AsyncPinStore",
    "AsyncPinWatcher",
    "CapabilityId",
    "ConsentPolicy",
    "PinChange",
    "PinsError",
    "PinStore",
    "credential_requires_consent",
    "default_pin_store_path",
]

try:
    from net_sdk.consent import (  # noqa: E402
        AsyncCapabilityGateway,
        CapabilityGateway,
    )
except ImportError:  # pragma: no cover - minimal build
    pass
else:
    __all__ += ["AsyncCapabilityGateway", "CapabilityGateway"]

# Delegated agent identity (`HERMES_INTEGRATION_PLAN.md` Phase 3): the
# DelegationChain (`root -> machine -> gateway -> subagent`) + shared
# RevocationRegistry + child-`Identity` derivation. Present iff the wheel was
# built with the `delegation` feature (the default one is).
try:
    from net_sdk.delegation import (  # noqa: E402
        GATEWAY_DELEGATION_CHANNEL,
        DelegationChain,
        RevocationRegistry,
        default_revocation_store_path,
        derive_child_identity,
    )
except ImportError:  # pragma: no cover - minimal build
    pass
else:
    __all__ += [
        "GATEWAY_DELEGATION_CHANNEL",
        "DelegationChain",
        "RevocationRegistry",
        "default_revocation_store_path",
        "derive_child_identity",
    ]

# Device enrollment (`HERMES_INTEGRATION_PLAN_V2.md` Phase 1): the invite ->
# join -> approve handshake + the operator device-lifecycle facade. Present iff
# the wheel was built with the `delegation` feature (the default one is).
try:
    from net_sdk.enrollment import (  # noqa: E402
        DeviceEnrollment,
        DeviceRecord,
        InviteToken,
        JoinOutcome,
        JoinRequest,
        OperatorEnrollment,
        fingerprint,
    )
except ImportError:  # pragma: no cover - minimal build
    pass
else:
    __all__ += [
        "DeviceEnrollment",
        "DeviceRecord",
        "InviteToken",
        "JoinOutcome",
        "JoinRequest",
        "OperatorEnrollment",
        "fingerprint",
    ]

__version__ = "0.34.0"
