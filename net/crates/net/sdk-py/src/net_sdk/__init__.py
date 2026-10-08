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
from net_sdk.channel import (
    CHANNEL_TAG_KEY,
    MAX_CHANNEL_NAME_LEN,
    ChannelNameError,
    TypedChannel,
    validate_channel_name,
)
from net_sdk.mesh import (
    AsyncMeshNode,
    BackpressureError,
    ChannelAuthError,
    ChannelConfig,
    ChannelError,
    MeshNode,
    MeshStream,
    NotConnectedError,
    OnFailure,
    PublishConfig,
    PublishError,
    PublishReport,
    Reliability,
    SessionSupersededError,
    StreamStats,
    Visibility,
)
from net_sdk.node import NetNode
from net_sdk.org import (
    HANDLER_DROP_CONTRACT,
    AsyncOrgClient,
    OrgAdmissionDeniedError,
    OrgClient,
    OrgCredentials,
    OrgCredentialsError,
    OrgDiscoveryError,
    OrgError,
    OrgServeHandle,
    OrgUnclassifiedError,
    install_org_authority,
    install_provider_grant_audience,
    serve_org,
    serve_org_client_stream,
    serve_org_duplex,
    serve_org_streaming,
)
from net_sdk.types import Receipt
from net_sdk.stream import EventStream, TypedEventStream

__all__ = [
    "NetNode",
    "Receipt",
    "EventStream",
    "TypedEventStream",
    "TypedChannel",
    "ChannelNameError",
    "validate_channel_name",
    "MAX_CHANNEL_NAME_LEN",
    "CHANNEL_TAG_KEY",
    "MeshNode",
    "AsyncMeshNode",
    "MeshStream",
    "StreamStats",
    "Reliability",
    "BackpressureError",
    "NotConnectedError",
    "SessionSupersededError",
    # Mesh channels (distributed pub/sub) on `MeshNode`.
    "ChannelError",
    "ChannelAuthError",
    "ChannelConfig",
    "PublishConfig",
    "PublishError",
    "PublishReport",
    "Visibility",
    "OnFailure",
    # Organization capability auth (the §4.4 facade — thin forwarding over
    # the wheel's org surface; the typed wrappers + the `org:` vocabulary
    # mirror live at `net_sdk.org.TypedOrgClient` / `.serve_org_typed` /
    # `.parse_org_error` / `.classify_org_error` / `.ParsedOrgError`).
    "HANDLER_DROP_CONTRACT",
    "OrgCredentials",
    "OrgClient",
    "AsyncOrgClient",
    "OrgServeHandle",
    "install_org_authority",
    "install_provider_grant_audience",
    "serve_org",
    "serve_org_streaming",
    "serve_org_client_stream",
    "serve_org_duplex",
    "OrgError",
    "OrgCredentialsError",
    "OrgDiscoveryError",
    "OrgAdmissionDeniedError",
    "OrgUnclassifiedError",
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

# Payments + paid A2A over the SDK's MeshNode (NODE_A2A_PAID_ADMISSION_PLAN.md
# WS-G): factories that adapt the mesh handle and return the native objects,
# plus the native classes and refusals, so a paid-A2A program never imports
# `net`. Each native name is present iff the wheel has its feature.
from net_sdk import payments as _payments  # noqa: E402
from net_sdk.payments import (  # noqa: E402
    create_async_capability_gateway,
    create_capability_gateway,
    create_payment_provider,
    set_a2a_org_caller,
)

__all__ += [
    "create_async_capability_gateway",
    "create_capability_gateway",
    "create_payment_provider",
    "set_a2a_org_caller",
]
for _name in ("PaymentProvider", "PaymentRefused", "JournalOwnedElsewhere"):
    if _name in _payments.__all__:
        globals()[_name] = getattr(_payments, _name)
        __all__.append(_name)
del _name

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

# Compute — daemons with snapshot + live migration (`net_sdk.compute`,
# `PYTHON_SDK_WRAPPER_PARITY_PLAN.md` S2). Present iff the wheel was built
# with the `compute` feature (the default one is).
try:
    from net_sdk.compute import (  # noqa: E402
        CausalEvent,
        DaemonError,
        DaemonFactory,
        DaemonHandle,
        DaemonHostConfig,
        DaemonRuntime,
        MeshDaemon,
        MigrationError,
        MigrationErrorKind,
        MigrationHandle,
        MigrationOptions,
        MigrationPhase,
        migration_error_kind,
    )
except ImportError:  # pragma: no cover - minimal build
    pass
else:
    __all__ += [
        "CausalEvent",
        "DaemonError",
        "DaemonFactory",
        "DaemonHandle",
        "DaemonHostConfig",
        "DaemonRuntime",
        "MeshDaemon",
        "MigrationError",
        "MigrationErrorKind",
        "MigrationHandle",
        "MigrationOptions",
        "MigrationPhase",
        "migration_error_kind",
    ]

# Groups — HA / scaling overlays over compute daemons (`net_sdk.groups`,
# `PYTHON_SDK_WRAPPER_PARITY_PLAN.md` S3). Present iff the wheel was built
# with `compute` + `groups` (the default one is).
try:
    from net_sdk.groups import (  # noqa: E402
        ForkGroup,
        ForkRecord,
        GroupError,
        GroupErrorKind,
        GroupHealth,
        GroupMemberInfo,
        GroupStatus,
        LoadBalanceStrategy,
        ReplicaGroup,
        RequestContext,
        StandbyGroup,
        group_error_kind,
    )
except ImportError:  # pragma: no cover - minimal build
    pass
else:
    __all__ += [
        "ForkGroup",
        "ForkRecord",
        "GroupError",
        "GroupErrorKind",
        "GroupHealth",
        "GroupMemberInfo",
        "GroupStatus",
        "LoadBalanceStrategy",
        "ReplicaGroup",
        "RequestContext",
        "StandbyGroup",
        "group_error_kind",
    ]

# Identity + tokens, and subnet helpers (`net_sdk.identity`,
# `net_sdk.subnets`; `PYTHON_SDK_WRAPPER_PARITY_PLAN.md` S5). Identity is in
# every build; the subnet helpers are pure Python.
from net_sdk.identity import (  # noqa: E402
    Identity,
    IdentityError,
    TokenError,
    TokenScope,
    channel_hash,
    delegate_token,
    normalize_gpu_vendor,
    parse_token,
    stream_id_from_label,
    token_is_expired,
    verify_signature,
    verify_token,
)
from net_sdk.subnets import (  # noqa: E402
    GLOBAL_SUBNET,
    SubnetId,
    SubnetPolicy,
    SubnetRule,
    subnet_id,
    subnet_policy,
)

__all__ += [
    "Identity",
    "IdentityError",
    "TokenError",
    "TokenScope",
    "channel_hash",
    "delegate_token",
    "normalize_gpu_vendor",
    "parse_token",
    "stream_id_from_label",
    "token_is_expired",
    "verify_signature",
    "verify_token",
    "GLOBAL_SUBNET",
    "SubnetId",
    "SubnetPolicy",
    "SubnetRule",
    "subnet_id",
    "subnet_policy",
]

__version__ = "0.42.0"
