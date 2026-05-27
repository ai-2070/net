"""Python layer for AI tool calling on net.

Wraps the existing ``TypedMeshRpc`` Python surface with the
``serve_tool`` / ``call_tool`` ergonomic helpers + format
translators that lower :class:`ToolDescriptor` instances to
OpenAI / Anthropic / MCP / Gemini tool shapes and parse provider
tool-call replies back into nRPC dispatches.

This is the Wave 3 / C-1 + C-4 starting point. v1 covers unary
register + invoke + format conversion. Streaming (C-2) and
discovery (C-3 ``list_tools`` / ``watch_tools``) follow once the
underlying pyo3 surface exposes them.

Plan: see
``crates/net/docs/plans/NRPC_AI_TOOL_CALLING_AND_AGENT_DX.md``,
slices C-1 / C-2 / C-4. Mirror of the Rust SDK's ``net_sdk::tool``
+ ``net_sdk::tool::formats`` modules and the Node TS ``tool.ts``
shim — cross-language tests (T-1) will pin byte equality across
all three.
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from typing import Any, Callable, Dict, List, Literal, Optional, TypedDict, Union

from .mesh_rpc import ServeHandle, TypedMeshRpc

# =============================================================================
# Wire types — mirror of the Rust ``ToolDescriptor`` + ``ToolEvent``.
# =============================================================================


@dataclass
class ToolDescriptor:
    """Discovery shape for an AI tool, as advertised on the
    capability fold. One row per ``(tool_id, version)``;
    ``node_count`` is filled by the aggregating walk
    (``list_tools`` once it lands).

    Wire-compatible 1:1 with the Rust substrate's ``ToolDescriptor``
    and the Node TS ``ToolDescriptor`` interface.

    Schemas are stored as JSON-encoded strings (matching the wire
    shape); use ``json.loads(desc.input_schema)`` to get the
    parsed object for lowering into a provider tool definition.
    """

    tool_id: str
    name: str
    version: str = "1.0.0"
    description: Optional[str] = None
    input_schema: Optional[str] = None
    output_schema: Optional[str] = None
    requires: List[str] = field(default_factory=list)
    estimated_time_ms: int = 0
    stateless: bool = True
    streaming: bool = False
    tags: List[str] = field(default_factory=list)
    node_count: int = 0


# ToolEvent is encoded on the wire as a tagged union (``{"type":
# "start", …}`` shape). The Python form uses TypedDicts so callers
# can pattern-match on ``event["type"]`` without instantiating a
# class hierarchy — keeps the JSON round-trip lossless.


class ToolEventStart(TypedDict, total=False):
    type: Literal["start"]
    tool_id: str
    call_id: int
    metadata: Any


class ToolEventProgress(TypedDict, total=False):
    type: Literal["progress"]
    pct: float
    message: str


class ToolEventDelta(TypedDict):
    type: Literal["delta"]
    data: Any


class ToolEventResult(TypedDict):
    type: Literal["result"]
    data: Any


class ToolEventError(TypedDict, total=False):
    type: Literal["error"]
    code: str
    message: str
    details: Any


ToolEvent = Union[
    ToolEventStart,
    ToolEventProgress,
    ToolEventDelta,
    ToolEventResult,
    ToolEventError,
]


def is_terminal_event(event: ToolEvent) -> bool:
    """True if ``event`` is a terminal envelope (``result`` or
    ``error``).
    """
    return event.get("type") in ("result", "error")


# =============================================================================
# Descriptor construction
# =============================================================================


def descriptor_for(
    name: str,
    *,
    description: Optional[str] = None,
    version: str = "1.0.0",
    input_schema: Optional[dict] = None,
    output_schema: Optional[dict] = None,
    requires: Optional[List[str]] = None,
    estimated_time_ms: int = 0,
    stateless: bool = True,
    tags: Optional[List[str]] = None,
) -> ToolDescriptor:
    """Build a :class:`ToolDescriptor` from a name + per-field
    overrides. Mirror of the Rust ``metadata_for(name)`` +
    ``ToolMetadataBuilder`` surface and the Node TS
    ``descriptorFrom({...})`` helper.

    Callers supply JSON Schema as a Python ``dict`` (commonly the
    output of ``pydantic.BaseModel.model_json_schema()`` or a
    hand-written schema literal); the helper serializes to a string
    for storage on the descriptor.

    ``streaming`` defaults to ``False`` — for streaming tools, call
    :func:`serve_tool_streaming` (once it lands) which forces the
    flag to ``True`` on register.
    """
    return ToolDescriptor(
        tool_id=name,
        name=name,
        version=version,
        description=description,
        input_schema=json.dumps(input_schema) if input_schema is not None else None,
        output_schema=json.dumps(output_schema) if output_schema is not None else None,
        requires=list(requires) if requires else [],
        estimated_time_ms=estimated_time_ms,
        stateless=stateless,
        streaming=False,
        tags=list(tags) if tags else [],
        node_count=0,
    )


# =============================================================================
# Register / invoke
# =============================================================================


@dataclass
class ToolServeHandle:
    """Handle returned by :func:`serve_tool`. Call ``.close()`` to
    deregister the underlying nRPC handler. Idempotent; second
    ``.close()`` is a no-op. Mirror of the Rust
    ``ToolServeHandle``'s Drop semantics.

    NOTE: v1 does NOT integrate with the substrate-side
    ``tool_registry``, so the ``ai-tool:<tool_id>`` capability tag
    must be added to the caller's announce explicitly. See
    :func:`add_tool_capabilities_to_announce`. Once pyo3 exposes
    ``tool_registry()`` (Wave 3 follow-up), this handle will
    atomically reverse both the registry insert and the handler
    registration on ``.close()``.
    """

    descriptor: ToolDescriptor
    _inner: ServeHandle
    _closed: bool = False

    def close(self) -> None:
        if self._closed:
            return
        self._closed = True
        self._inner.close()


def serve_tool(
    rpc: TypedMeshRpc,
    options_or_descriptor: Union[ToolDescriptor, dict, str],
    handler: Callable[[Any], Any],
    **kwargs: Any,
) -> ToolServeHandle:
    """Register a handler as an AI tool against ``rpc``.

    ``options_or_descriptor`` is one of:

    - A pre-built :class:`ToolDescriptor`.
    - A ``dict`` of options forwarded to :func:`descriptor_for`
      (must include ``name``).
    - A bare ``name`` string — additional fields can be supplied as
      keyword arguments (``description``, ``input_schema``, etc.).

    The handler is registered as an nRPC service at
    ``descriptor.tool_id`` with JSON codec. ``rpc.serve(...)``
    accepts a sync callable; async handlers (``async def``) work
    transparently when registered against ``AsyncTypedMeshRpc``
    (Python's async TypedMeshRpc — see ``net.async_mesh_rpc``).

    The caller is responsible for announcing the tool to peers —
    use :func:`add_tool_capabilities_to_announce` on the
    capability set passed to ``mesh.announce_capabilities(...)``
    so the ``ai-tool:<tool_id>`` tag + the ToolJs entry land on
    the wire.

    Wave 3 follow-up: once pyo3 exposes ``tool_registry()``, this
    helper will atomically insert there too, making the announce-
    time merge automatic (matching the Rust SDK's contract).
    """
    if isinstance(options_or_descriptor, ToolDescriptor):
        descriptor = options_or_descriptor
    elif isinstance(options_or_descriptor, dict):
        opts = dict(options_or_descriptor)
        name = opts.pop("name")
        descriptor = descriptor_for(name, **opts)
    elif isinstance(options_or_descriptor, str):
        descriptor = descriptor_for(options_or_descriptor, **kwargs)
    else:
        raise TypeError(
            "serve_tool: options_or_descriptor must be ToolDescriptor, dict, or str"
        )
    inner = rpc.serve(descriptor.tool_id, handler)
    return ToolServeHandle(descriptor=descriptor, _inner=inner)


def call_tool(
    rpc: TypedMeshRpc,
    tool_id: str,
    request: Any,
    opts: Optional[dict] = None,
) -> Any:
    """Capability-routed unary tool invocation. Encodes ``request``
    as JSON, dispatches via :meth:`TypedMeshRpc.call_service`.

    Raises :class:`net.mesh_rpc.RpcError` subclasses on failure
    (``NoRouteError`` if no host advertises ``nrpc:<tool_id>``;
    bubbled handler errors as ``RpcServerError``).
    """
    return rpc.call_service(tool_id, request, opts)


def add_tool_capabilities_to_announce(
    caps: Dict[str, Any],
    descriptors: List[ToolDescriptor],
) -> Dict[str, Any]:
    """Merge tool descriptors into a capability-set dict so the
    next ``mesh.announce_capabilities(caps)`` carries:

    - ``ai-tool:<tool_id>`` tag — peer fold's tag-prefix lookup
      hits.
    - A ``tools[]`` entry — peer's ``list_tools`` walk sees the
      tool's tag-encoded fields.

    Caller still owns ``caps`` — pass it through
    ``mesh.announce_capabilities(caps)`` to publish. Returns the
    same dict for chaining.

    This is a v1 convenience; once pyo3 exposes
    ``tool_registry()``, the announce-time merge happens
    automatically and this helper becomes optional.
    """
    if not descriptors:
        return caps
    existing_tags = list(caps.get("tags") or [])
    tag_set = set(existing_tags)
    tools = list(caps.get("tools") or [])
    for desc in descriptors:
        tag = f"ai-tool:{desc.tool_id}"
        if tag not in tag_set:
            existing_tags.append(tag)
            tag_set.add(tag)
        tools.append(
            {
                "tool_id": desc.tool_id,
                "name": desc.name,
                "version": desc.version,
                "input_schema": desc.input_schema,
                "output_schema": desc.output_schema,
                "requires": desc.requires,
                "estimated_time_ms": desc.estimated_time_ms,
                "stateless": desc.stateless,
            }
        )
    caps["tags"] = existing_tags
    caps["tools"] = tools
    return caps


# =============================================================================
# Format translators — mirror of ``net_sdk::tool::formats``
# =============================================================================
#
# Each provider submodule exports two directions:
#
# 1. ``to_<provider>_tool(desc) -> dict`` — descriptor → provider's
#    tool-definition shape for the ``tools`` array on the provider's
#    HTTP request.
# 2. ``lower_<provider>_tool_call(reply) -> ToolCallSpec`` —
#    provider's reply → ToolCallSpec the caller hands to
#    :func:`call_tool`.
#
# Empty-input-schema fallback matches the Rust + Node impls
# (``{"type": "object", "properties": {}}``).


@dataclass
class ToolCallSpec:
    """Canonical hand-off between an LLM-provider adapter and
    :func:`call_tool`. ``arguments_json`` is a string so the
    boundary is provider-agnostic (OpenAI's arguments arrive as a
    string anyway; Anthropic/MCP/Gemini's parsed objects re-
    serialize once).
    """

    name: str
    arguments_json: str
    provider_call_id: Optional[str] = None


class ToolCallParseError(Exception):
    """Raised when a provider's tool-call reply doesn't match the
    expected shape (missing ``name``, malformed arguments, etc.).
    """


def _input_schema_value(desc: ToolDescriptor) -> dict:
    if desc.input_schema is None:
        return {"type": "object", "properties": {}}
    try:
        return json.loads(desc.input_schema)
    except (json.JSONDecodeError, ValueError):
        # Malformed schema string (shouldn't happen for descriptors
        # built via descriptor_for). Empty-object fallback keeps
        # provider validators happy.
        return {"type": "object", "properties": {}}


class openai:  # noqa: N801 — namespace, not a real class
    """Translators for the OpenAI Chat Completions / Responses API
    ``tools`` array shape.
    """

    @staticmethod
    def to_openai_tool(desc: ToolDescriptor) -> dict:
        """Lower a :class:`ToolDescriptor` to an OpenAI tool
        definition. Shape::

            {
              "type": "function",
              "function": {
                "name": <tool_id>,
                "description": <description>,
                "parameters": <input_schema>,
                "strict": <bool>
              }
            }

        ``strict`` is set to ``True`` when the descriptor carried
        an ``input_schema`` (i.e. publishable on the fold).
        """
        return {
            "type": "function",
            "function": {
                "name": desc.tool_id,
                "description": desc.description or "",
                "parameters": _input_schema_value(desc),
                "strict": desc.input_schema is not None,
            },
        }

    @staticmethod
    def lower_openai_tool_call(call: dict) -> ToolCallSpec:
        """Parse one OpenAI ``tool_calls[]`` entry into a
        :class:`ToolCallSpec`. OpenAI's ``function.arguments`` is a
        JSON-encoded STRING; the helper validates it parses up
        front so malformed payloads fail fast instead of riding
        through :func:`call_tool`.
        """
        function = call.get("function")
        if not isinstance(function, dict):
            raise ToolCallParseError(
                "tool-call reply missing field `function`"
            )
        name = function.get("name")
        if not isinstance(name, str):
            raise ToolCallParseError(
                "tool-call reply field `function.name` must be a string"
            )
        arguments = function.get("arguments")
        if not isinstance(arguments, str):
            raise ToolCallParseError(
                "tool-call reply field `function.arguments` must be a "
                "JSON-encoded string"
            )
        try:
            json.loads(arguments)
        except json.JSONDecodeError as e:
            raise ToolCallParseError(
                f"tool-call arguments were not valid JSON: {e.msg}"
            ) from e
        call_id = call.get("id")
        return ToolCallSpec(
            name=name,
            arguments_json=arguments,
            provider_call_id=call_id if isinstance(call_id, str) else None,
        )


class anthropic:  # noqa: N801 — namespace, not a real class
    """Translators for the Anthropic Messages API ``tools`` array
    + ``tool_use`` content blocks.
    """

    @staticmethod
    def to_anthropic_tool(desc: ToolDescriptor) -> dict:
        """Lower a descriptor to an Anthropic tool definition.
        Shape: ``{name, description, input_schema}`` (snake_case
        ``input_schema``). No tool-level ``strict`` flag.
        """
        return {
            "name": desc.tool_id,
            "description": desc.description or "",
            "input_schema": _input_schema_value(desc),
        }

    @staticmethod
    def lower_anthropic_tool_use(block: dict) -> ToolCallSpec:
        """Parse one Anthropic ``tool_use`` content block. ``input``
        is already a parsed object; the helper re-serializes once
        to preserve the ``arguments_json: str`` invariant.
        """
        name = block.get("name")
        if not isinstance(name, str):
            raise ToolCallParseError(
                "tool_use block field `name` must be a string"
            )
        if "input" not in block:
            raise ToolCallParseError(
                "tool_use block missing field `input`"
            )
        arguments_json = json.dumps(block["input"])
        call_id = block.get("id")
        return ToolCallSpec(
            name=name,
            arguments_json=arguments_json,
            provider_call_id=call_id if isinstance(call_id, str) else None,
        )


class mcp:  # noqa: N801 — namespace, not a real class
    """Translators for the Model Context Protocol ``tools/list``
    + ``tools/call`` shape.
    """

    @staticmethod
    def to_mcp_tool(desc: ToolDescriptor) -> dict:
        """Lower a descriptor to an MCP tool definition. Shape:
        ``{name, description, inputSchema}`` (camelCase
        ``inputSchema``).
        """
        return {
            "name": desc.tool_id,
            "description": desc.description or "",
            "inputSchema": _input_schema_value(desc),
        }

    @staticmethod
    def lower_mcp_tools_call(params: dict) -> ToolCallSpec:
        """Parse an MCP ``tools/call`` request's ``params`` into a
        :class:`ToolCallSpec`. ``provider_call_id`` is left
        ``None`` — MCP's JSON-RPC ``id`` lives one envelope layer
        up, threaded independently.
        """
        name = params.get("name")
        if not isinstance(name, str):
            raise ToolCallParseError(
                "tools/call params field `name` must be a string"
            )
        if "arguments" not in params:
            raise ToolCallParseError(
                "tools/call params missing field `arguments`"
            )
        return ToolCallSpec(
            name=name,
            arguments_json=json.dumps(params["arguments"]),
            provider_call_id=None,
        )


class gemini:  # noqa: N801 — namespace, not a real class
    """Translators for the Google Gemini ``generateContent`` API
    function-calling shape.
    """

    @staticmethod
    def to_gemini_function_declaration(desc: ToolDescriptor) -> dict:
        """Lower a descriptor to one Gemini ``FunctionDeclaration``.
        Shape: ``{name, description, parameters}``. Caller wraps
        these into the outer
        ``tools: [{ function_declarations: [...] }]`` array.
        """
        return {
            "name": desc.tool_id,
            "description": desc.description or "",
            "parameters": _input_schema_value(desc),
        }

    @staticmethod
    def lower_gemini_function_call(call: dict) -> ToolCallSpec:
        """Parse one Gemini ``functionCall`` part. Gemini has no
        per-call id; ``provider_call_id`` is ``None`` (multi-call
        sequences are positional).
        """
        name = call.get("name")
        if not isinstance(name, str):
            raise ToolCallParseError(
                "functionCall field `name` must be a string"
            )
        if "args" not in call:
            raise ToolCallParseError(
                "functionCall missing field `args`"
            )
        return ToolCallSpec(
            name=name,
            arguments_json=json.dumps(call["args"]),
            provider_call_id=None,
        )


__all__ = [
    "ToolDescriptor",
    "ToolEvent",
    "ToolEventStart",
    "ToolEventProgress",
    "ToolEventDelta",
    "ToolEventResult",
    "ToolEventError",
    "ToolServeHandle",
    "ToolCallSpec",
    "ToolCallParseError",
    "descriptor_for",
    "serve_tool",
    "call_tool",
    "add_tool_capabilities_to_announce",
    "is_terminal_event",
    "openai",
    "anthropic",
    "mcp",
    "gemini",
]
