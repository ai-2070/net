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
from typing import (
    Any,
    AsyncGenerator,
    AsyncIterator,
    Callable,
    Dict,
    Iterator,
    List,
    Literal,
    Optional,
    TypedDict,
    Union,
)

from .mesh_rpc import AsyncTypedMeshRpc, ServeHandle, TypedMeshRpc

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
    deregister the underlying nRPC handler + remove the descriptor
    from the per-rpc registry that backs ``tool.metadata.fetch``.
    Idempotent; second ``.close()`` is a no-op. Mirror of the Rust
    ``ToolServeHandle``'s Drop semantics.

    When closing the last serve handle against a given rpc, also
    drops the fetch-handler and removes the process-global registry
    entry so a recycled rpc doesn't leak.
    """

    descriptor: ToolDescriptor
    _inner: ServeHandle
    _registry: Optional[Dict[str, "ToolDescriptor"]] = None
    _outer_key: Optional[int] = None
    _closed: bool = False

    def close(self) -> None:
        if self._closed:
            return
        self._closed = True
        if self._registry is not None:
            self._registry.pop(self.descriptor.tool_id, None)
            if not self._registry and self._outer_key is not None:
                entry = _tool_registries.pop(self._outer_key, None)
                if entry is not None:
                    fetch_handle = entry.get("fetch_handle")
                    if fetch_handle is not None:
                        try:
                            fetch_handle.close()
                        except Exception:
                            pass
        self._inner.close()


# Per-rpc descriptor registries keyed by `TypedMeshRpc` id. Each
# entry holds a `dict[tool_id -> ToolDescriptor]` and an optional
# fetch-handler ServeHandle. The fetch handler is lazy-installed on
# the first `serve_tool` call against a given rpc and stays alive
# for the rpc's lifetime — harmless when the registry is empty
# (returns NotFound for every request). Mirrors the Rust SDK's
# `ensure_tool_metadata_fetch_installed` pattern.
_tool_registries: Dict[int, Dict[str, Any]] = {}


def _ensure_fetch_installed(rpc: Any) -> dict:
    key = id(rpc)
    entry = _tool_registries.get(key)
    if entry is not None:
        return entry
    registry: Dict[str, ToolDescriptor] = {}

    def _fetch_handler(req: dict) -> dict:
        name = req.get("name", "")
        desc = registry.get(name)
        if desc is None:
            return {"type": "not_found", "name": name}
        return {
            "type": "found",
            "descriptor": {
                "tool_id": desc.tool_id,
                "name": desc.name,
                "version": desc.version,
                "description": desc.description,
                "input_schema": desc.input_schema,
                "output_schema": desc.output_schema,
                "requires": desc.requires,
                "estimated_time_ms": desc.estimated_time_ms,
                "stateless": desc.stateless,
                "streaming": desc.streaming,
                "tags": desc.tags,
                "node_count": desc.node_count,
            },
        }

    try:
        fetch_handle = rpc.serve(TOOL_METADATA_FETCH_SERVICE, _fetch_handler)
    except Exception:
        # If install fails (service name already taken), leave it
        # null and retry on subsequent serve_tool calls. Failure
        # surfaces to the agent side as NoRoute / NotFound from
        # fetch_tool_metadata.
        fetch_handle = None
    entry = {"registry": registry, "fetch_handle": fetch_handle}
    _tool_registries[key] = entry
    return entry


def _coerce_descriptor(
    options_or_descriptor: Union[ToolDescriptor, dict, str],
    kwargs: dict,
    *,
    context: str,
) -> ToolDescriptor:
    """Normalize the ``options_or_descriptor`` parameter accepted by
    every ``serve_tool*`` variant into a concrete
    :class:`ToolDescriptor`. ``context`` names the calling function
    for the ``TypeError`` message.
    """
    if isinstance(options_or_descriptor, ToolDescriptor):
        return options_or_descriptor
    if isinstance(options_or_descriptor, dict):
        opts = dict(options_or_descriptor)
        name = opts.pop("name")
        return descriptor_for(name, **opts)
    if isinstance(options_or_descriptor, str):
        return descriptor_for(options_or_descriptor, **kwargs)
    raise TypeError(
        f"{context}: options_or_descriptor must be ToolDescriptor, dict, or str"
    )


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
    descriptor = _coerce_descriptor(options_or_descriptor, kwargs, context="serve_tool")
    entry = _ensure_fetch_installed(rpc)
    entry["registry"][descriptor.tool_id] = descriptor
    inner = rpc.serve(descriptor.tool_id, handler)
    return ToolServeHandle(
        descriptor=descriptor,
        _inner=inner,
        _registry=entry["registry"],
        _outer_key=id(rpc),
    )


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


def call_tool_streaming(
    rpc: TypedMeshRpc,
    tool_id: str,
    request: Any,
    opts: Optional[dict] = None,
) -> Iterator[dict]:
    """Capability-routed streaming tool invocation.

    Yields each JSON-decoded :class:`ToolEvent` envelope as the
    handler emits them. Synthesizes a terminal
    ``{"type": "error", "code": "missing_terminal", ...}`` event
    if the stream ends without a ``result`` / ``error`` — matches
    the Rust SDK's ``serve_tool_streaming`` contract and the T-2
    cross-language fixture.

    The returned iterator wraps a :class:`TypedRpcStream`; iterate
    until exhaustion (or break out — the stream's drop emits CANCEL
    to the host).
    """
    stream = rpc.call_service_streaming(tool_id, request, opts)
    saw_terminal = False
    try:
        for event in stream:
            yield event
            if is_terminal_event(event):
                saw_terminal = True
    finally:
        try:
            stream.close()
        except Exception:
            pass
    if not saw_terminal:
        yield {
            "type": "error",
            "code": "missing_terminal",
            "message": (
                "tool stream ended without a terminal result or error envelope"
            ),
        }


#: nRPC service name for the on-demand tool-descriptor pull.
TOOL_METADATA_FETCH_SERVICE = "tool.metadata.fetch"


def fetch_tool_metadata(
    rpc: TypedMeshRpc,
    host_node_id: int,
    tool_id: str,
    opts: Optional[dict] = None,
) -> dict:
    """Pull a tool's full descriptor from a specific host via the
    auto-installed ``tool.metadata.fetch`` nRPC service.

    Useful when the local fold's entry dropped the schema (size-
    budget-exceeded) and the agent needs the full input/output
    schemas for strict-mode provider lowering.

    Returns a dict matching the substrate's
    ``ToolMetadataResponse`` wire shape (JSON-tagged on ``type``,
    snake_case):

    - ``{"type": "found", "descriptor": {...}}`` — descriptor has
      every field the host's tool_registry holds.
    - ``{"type": "not_found", "name": "<tool_id>"}`` — host
      doesn't currently serve this tool.

    Mirror of calling ``mesh.call_typed(host, TOOL_METADATA_FETCH_SERVICE,
    {"name": tool_id})`` in the Rust SDK. The handler is auto-
    installed on the host's first ``serve_tool`` call.
    """
    return rpc.call(host_node_id, TOOL_METADATA_FETCH_SERVICE, {"name": tool_id}, opts)


@dataclass
class ToolListChangeAdded:
    type: Literal["added"]
    descriptor: ToolDescriptor


@dataclass
class ToolListChangeRemoved:
    type: Literal["removed"]
    descriptor: ToolDescriptor


@dataclass
class ToolListChangeNodeCountChanged:
    type: Literal["node_count_changed"]
    descriptor: ToolDescriptor
    prev_node_count: int


#: One change in the set of tools visible to the local capability
#: fold. Mirror of the Rust SDK's ``ToolListChange`` enum.
#: Wire-compatible 1:1 with the Node TS ``ToolListChange`` union.
ToolListChange = Union[
    ToolListChangeAdded,
    ToolListChangeRemoved,
    ToolListChangeNodeCountChanged,
]


async def watch_tools(
    mesh: Any,
    *,
    interval: float = 1.0,
):
    """Async-iterator over [`ToolListChange`] events for every
    dynamic addition / removal / publisher-count change in the
    local capability fold's tool view.

    Polling-backed: every ``interval`` seconds (default ``1.0``),
    the helper re-runs :func:`list_tools` on the mesh and diffs
    against the prior snapshot. The first event fires AFTER the
    initial baseline — call ``list_tools(mesh)`` once before
    consuming the iterator if you need the starting shape.

    Mirror of the Rust SDK's ``Mesh::watch_tools`` and the Node TS
    ``watchTools``. All three are polling-backed at 1s default;
    identical semantics.

    Cancel by calling :meth:`asyncio.CancelledError` on the
    consuming task — the polling loop exits on the next tick.

    Usage::

        async for change in watch_tools(mesh, interval=0.25):
            match change.type:
                case "added":   print(f"+ {change.descriptor.tool_id}")
                case "removed": print(f"- {change.descriptor.tool_id}")
                case "node_count_changed":
                    print(f"~ {change.descriptor.tool_id}: {change.prev_node_count} -> {change.descriptor.node_count}")
    """
    import asyncio

    def snapshot() -> dict:
        return {(d.tool_id, d.version): d for d in list_tools(mesh)}

    prev = snapshot()
    while True:
        await asyncio.sleep(interval)
        next_snap = snapshot()
        for key, desc in next_snap.items():
            if key not in prev:
                yield ToolListChangeAdded(type="added", descriptor=desc)
        for key, desc in prev.items():
            if key not in next_snap:
                yield ToolListChangeRemoved(type="removed", descriptor=desc)
        for key, desc in next_snap.items():
            old = prev.get(key)
            if old is not None and old.node_count != desc.node_count:
                yield ToolListChangeNodeCountChanged(
                    type="node_count_changed",
                    descriptor=desc,
                    prev_node_count=old.node_count,
                )
        prev = next_snap


def list_tools(mesh: Any) -> List[ToolDescriptor]:
    """Walk the local capability fold for every published AI tool.

    Returns a list of :class:`ToolDescriptor` instances, one per
    ``(tool_id, version)`` slot, with ``node_count`` filled in by
    the aggregating walk.

    Pure delegation to the pyo3 binding's ``NetMesh.list_tools()``
    (C-3 of the plan). Requires the pyo3 binding's ``tool`` Cargo
    feature (default-on).

    Schemas come back as JSON-encoded strings on
    ``descriptor.input_schema`` / ``descriptor.output_schema`` —
    call ``json.loads(...)`` for the parsed shape that adapter
    packages consume when lowering into provider-specific tool
    definitions.

    The native binding returns a single JSON-encoded string of all
    descriptors; this wrapper parses it once with ``json.loads`` and
    converts each entry to a :class:`ToolDescriptor` dataclass for
    type safety and IDE autocomplete. Avoids the per-descriptor
    ``PyDict_SetItem`` storm the prior implementation paid on every
    ``watch_tools`` tick.
    """
    out: List[ToolDescriptor] = []
    for d in json.loads(mesh.list_tools()):
        out.append(
            ToolDescriptor(
                tool_id=d["tool_id"],
                name=d["name"],
                version=d["version"],
                description=d.get("description"),
                input_schema=d.get("input_schema"),
                output_schema=d.get("output_schema"),
                requires=list(d.get("requires") or []),
                estimated_time_ms=d.get("estimated_time_ms", 0),
                stateless=d.get("stateless", True),
                streaming=d.get("streaming", False),
                tags=list(d.get("tags") or []),
                node_count=d.get("node_count", 0),
            )
        )
    return out


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


async def call_tool_async(
    rpc: AsyncTypedMeshRpc,
    tool_id: str,
    request: Any,
    opts: Optional[dict] = None,
) -> Any:
    """Async equivalent of :func:`call_tool` for callers using the
    ``AsyncTypedMeshRpc`` surface.

    Capability-routed unary tool invocation; awaits the response
    and decodes the typed reply. Raises an
    :class:`net.mesh_rpc.RpcError` subclass on failure
    (``NoRouteError`` if no host advertises ``nrpc:<tool_id>``;
    bubbled handler errors as ``RpcServerError``).

    Used by asyncio-driven agents — the dominant pattern for
    Python LLM integrations (Anthropic SDK, OpenAI Python SDK,
    httpx, etc. all expose async clients).
    """
    return await rpc.call_service(tool_id, request, opts)


async def call_tool_streaming_async(
    rpc: AsyncTypedMeshRpc,
    tool_id: str,
    request: Any,
    opts: Optional[dict] = None,
) -> AsyncGenerator[dict, None]:
    """Async equivalent of :func:`call_tool_streaming`. Yields each
    :class:`ToolEvent` as the handler emits it; synthesizes a
    terminal ``error`` envelope with ``code='missing_terminal'`` if
    the stream ends without one.

    Uses :meth:`AsyncTypedMeshRpc.call_service_streaming` under the
    hood, so the substrate's asyncio cancel-bridge applies — an
    asyncio task-cancel mid-stream terminates the WHOLE call via
    the substrate cancel-registry.
    """
    stream = await rpc.call_service_streaming(tool_id, request, opts)
    saw_terminal = False
    try:
        async for event in stream:
            yield event
            if isinstance(event, dict) and event.get("type") in ("result", "error"):
                saw_terminal = True
    finally:
        try:
            await stream.aclose()
        except Exception:
            pass
    if not saw_terminal:
        yield {
            "type": "error",
            "code": "missing_terminal",
            "message": (
                "tool stream ended without a terminal result or error envelope"
            ),
        }


async def fetch_tool_metadata_async(
    rpc: AsyncTypedMeshRpc,
    host_node_id: int,
    tool_id: str,
    opts: Optional[dict] = None,
) -> dict:
    """Async equivalent of :func:`fetch_tool_metadata`. Awaits the
    nRPC call to the auto-installed ``tool.metadata.fetch`` service
    on the host, returns the wire-shape ``ToolMetadataResponse``
    dict.
    """
    return await rpc.call(host_node_id, TOOL_METADATA_FETCH_SERVICE, {"name": tool_id}, opts)


def serve_tool_streaming(
    rpc: TypedMeshRpc,
    options_or_descriptor: Union[ToolDescriptor, dict, str],
    handler: Callable[[Any], Any],
    **kwargs: Any,
) -> ToolServeHandle:
    """Register a streaming tool handler. ``handler`` is a regular
    callable taking ``(req)`` and returning an iterator (or
    generator) of :class:`ToolEvent` dicts — each yielded event is
    forwarded to the caller via :func:`call_tool_streaming`.

    Atomic register + lazy auto-install of ``tool.metadata.fetch``
    — same pattern as :func:`serve_tool`. Stamps ``streaming=True``
    on the descriptor so peers can discover the streaming variant.

    Handler exceptions map to a terminal
    ``{"type": "error", "code": "handler_error", "message": str}``
    envelope so callers see a typed error rather than the
    synthesized ``missing_terminal``.
    """
    base = _coerce_descriptor(options_or_descriptor, kwargs, context="serve_tool_streaming")
    descriptor = ToolDescriptor(**{**base.__dict__, "streaming": True})

    def _stream_handler(req: Any, sink: Any) -> None:
        saw_terminal = False
        try:
            for event in handler(req):
                sink.send(event)
                if is_terminal_event(event):
                    saw_terminal = True
            if not saw_terminal:
                sink.send(
                    {
                        "type": "error",
                        "code": "missing_terminal",
                        "message": (
                            "tool stream ended without a terminal result or "
                            "error envelope"
                        ),
                    }
                )
        except Exception as exc:
            sink.send(
                {
                    "type": "error",
                    "code": "handler_error",
                    "message": str(exc),
                }
            )

    entry = _ensure_fetch_installed(rpc)
    entry["registry"][descriptor.tool_id] = descriptor
    inner = rpc.serve_streaming(descriptor.tool_id, _stream_handler)
    return ToolServeHandle(
        descriptor=descriptor,
        _inner=inner,
        _registry=entry["registry"],
        _outer_key=id(rpc),
    )


def serve_tool_streaming_async(
    rpc: AsyncTypedMeshRpc,
    options_or_descriptor: Union[ToolDescriptor, dict, str],
    handler: Callable[[Any], Any],
    **kwargs: Any,
) -> ToolServeHandle:
    """Async variant of :func:`serve_tool_streaming`. ``handler``
    may be an async generator (``async def handler(req): yield
    event``) or a sync generator returning an iterable; both are
    detected via :mod:`inspect` and dispatched on the appropriate
    serve path.

    Handler exceptions map to a terminal ``handler_error``
    envelope, matching the sync wrapper's contract.
    """
    base = _coerce_descriptor(
        options_or_descriptor, kwargs, context="serve_tool_streaming_async"
    )
    descriptor = ToolDescriptor(**{**base.__dict__, "streaming": True})

    import inspect

    if inspect.isasyncgenfunction(handler):

        async def _stream_async(req: Any, sink: Any) -> None:
            saw_terminal = False
            try:
                async for event in handler(req):
                    sink.send(event)
                    if is_terminal_event(event):
                        saw_terminal = True
                if not saw_terminal:
                    sink.send(
                        {
                            "type": "error",
                            "code": "missing_terminal",
                            "message": (
                                "tool stream ended without a terminal result "
                                "or error envelope"
                            ),
                        }
                    )
            except Exception as exc:
                sink.send(
                    {
                        "type": "error",
                        "code": "handler_error",
                        "message": str(exc),
                    }
                )

        wrapped: Callable[[Any, Any], Any] = _stream_async
    else:

        def _stream_sync(req: Any, sink: Any) -> None:
            saw_terminal = False
            try:
                for event in handler(req):
                    sink.send(event)
                    if is_terminal_event(event):
                        saw_terminal = True
                if not saw_terminal:
                    sink.send(
                        {
                            "type": "error",
                            "code": "missing_terminal",
                            "message": (
                                "tool stream ended without a terminal result "
                                "or error envelope"
                            ),
                        }
                    )
            except Exception as exc:
                sink.send(
                    {
                        "type": "error",
                        "code": "handler_error",
                        "message": str(exc),
                    }
                )

        wrapped = _stream_sync

    entry = _ensure_fetch_installed(rpc)
    entry["registry"][descriptor.tool_id] = descriptor
    inner = rpc.serve_streaming(descriptor.tool_id, wrapped)
    return ToolServeHandle(
        descriptor=descriptor,
        _inner=inner,
        _registry=entry["registry"],
        _outer_key=id(rpc),
    )


def serve_tool_async(
    rpc: AsyncTypedMeshRpc,
    options_or_descriptor: Union[ToolDescriptor, dict, str],
    handler: Callable[[Any], Any],
    **kwargs: Any,
) -> ToolServeHandle:
    """Async-rpc equivalent of :func:`serve_tool`.

    Same atomic register + auto-install-fetch behavior as the sync
    version. The ``handler`` may be either ``def handler(req) ->
    resp`` or ``async def handler(req) -> resp`` —
    :meth:`AsyncTypedMeshRpc.serve` detects the coroutine-function
    case and drives the future on the substrate's tokio runtime.

    ``AsyncTypedMeshRpc.serve`` itself is sync (handler-registration
    doesn't need to be async), so this helper is `def` not `async
    def`.
    """
    descriptor = _coerce_descriptor(options_or_descriptor, kwargs, context="serve_tool_async")
    entry = _ensure_fetch_installed(rpc)
    entry["registry"][descriptor.tool_id] = descriptor
    inner = rpc.serve(descriptor.tool_id, handler)
    return ToolServeHandle(
        descriptor=descriptor,
        _inner=inner,
        _registry=entry["registry"],
        _outer_key=id(rpc),
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
    "serve_tool_streaming",
    "serve_tool_async",
    "serve_tool_streaming_async",
    "call_tool",
    "call_tool_streaming",
    "call_tool_async",
    "call_tool_streaming_async",
    "fetch_tool_metadata_async",
    "list_tools",
    "watch_tools",
    "ToolListChange",
    "ToolListChangeAdded",
    "ToolListChangeRemoved",
    "ToolListChangeNodeCountChanged",
    "fetch_tool_metadata",
    "add_tool_capabilities_to_announce",
    "TOOL_METADATA_FETCH_SERVICE",
    "is_terminal_event",
    "openai",
    "anthropic",
    "mcp",
    "gemini",
]
