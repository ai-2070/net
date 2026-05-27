"""AI tool calling surface for the Net mesh SDK.

Re-exports the canonical implementation from ``net.tool`` so users
can ``from net_sdk.tool import serve_tool, call_tool, openai, ...``
without reaching into the raw maturin-built ``net`` package.

The underlying ``net.tool`` module is pure Python (it sits on top
of the typed ``TypedMeshRpc`` / ``AsyncTypedMeshRpc`` wrappers in
``net.mesh_rpc``) — there is no SDK-specific ergonomics to layer on
top, so this module is a pure re-export. Layout mirrors ``net.tool``
exactly: types, descriptor builder, register/invoke (sync + async),
discovery, format translators (``openai`` / ``anthropic`` / ``mcp``
/ ``gemini``), and the ``tool.metadata.fetch`` helpers.

The four provider translators are namespaced objects:
``openai.to_openai_tool(...)`` / ``anthropic.lower_anthropic_tool_use(...)`` /
etc. — the wire shape is pinned by the cross-language T-1 + T-2
golden vectors so the names are stable across the Node TS, Python,
and Go bindings.
"""

from net.tool import (
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

__all__ = [
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
