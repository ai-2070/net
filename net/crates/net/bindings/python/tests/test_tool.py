"""Pure-function tests for the Python tool layer.

Covers ``descriptor_for``, ``add_tool_capabilities_to_announce``,
and all four provider format translators (both directions).

Live-mesh tests (``serve_tool`` + ``call_tool`` round-trip) are
deferred to an integration test once we're ready to spin up a
two-mesh harness. The cross-language byte-equality fixtures
pinned by T-1 will eventually feed this file, the Node TS
``tool.test.ts``, and the Rust ``formats`` module from the same
golden vectors.
"""

from __future__ import annotations

import json

import pytest

import asyncio
from typing import Any, AsyncIterator, List

from net.tool import (
    TOOL_METADATA_FETCH_SERVICE,
    ToolCallParseError,
    ToolDescriptor,
    add_tool_capabilities_to_announce,
    anthropic,
    call_tool_streaming,
    call_tool_streaming_async,
    descriptor_for,
    gemini,
    is_terminal_event,
    mcp,
    openai,
    serve_tool,
    serve_tool_streaming,
)
from net import tool as _tool_module


def sample_descriptor() -> ToolDescriptor:
    return descriptor_for(
        "web_search",
        description="Search the web.",
        input_schema={
            "type": "object",
            "properties": {"query": {"type": "string"}},
            "required": ["query"],
        },
    )


# ---------------------------------------------------------------------------
# descriptor_for
# ---------------------------------------------------------------------------


def test_descriptor_for_defaults_version_flags_and_lists() -> None:
    desc = descriptor_for("x")
    assert desc.tool_id == "x"
    assert desc.name == "x"
    assert desc.version == "1.0.0"
    assert desc.stateless is True
    assert desc.streaming is False
    assert desc.estimated_time_ms == 0
    assert desc.tags == []
    assert desc.requires == []
    assert desc.node_count == 0
    assert desc.input_schema is None


def test_descriptor_for_serializes_schemas_to_strings() -> None:
    desc = sample_descriptor()
    assert isinstance(desc.input_schema, str)
    parsed = json.loads(desc.input_schema)
    assert parsed["properties"]["query"] == {"type": "string"}


# ---------------------------------------------------------------------------
# is_terminal_event
# ---------------------------------------------------------------------------


def test_terminal_events_flagged() -> None:
    assert is_terminal_event({"type": "result", "data": 1})
    assert is_terminal_event({"type": "error", "code": "x", "message": "y"})


def test_non_terminal_events_flagged() -> None:
    assert not is_terminal_event({"type": "start", "tool_id": "x"})
    assert not is_terminal_event({"type": "progress", "pct": 50})
    assert not is_terminal_event({"type": "delta", "data": 1})


# ---------------------------------------------------------------------------
# add_tool_capabilities_to_announce
# ---------------------------------------------------------------------------


def test_add_tool_capabilities_merges_on_fresh_set() -> None:
    desc = sample_descriptor()
    caps = add_tool_capabilities_to_announce({}, [desc])
    assert "ai-tool:web_search" in caps["tags"]
    assert caps["tools"][0]["tool_id"] == "web_search"


def test_add_tool_capabilities_preserves_existing_tags_and_dedupes() -> None:
    desc = sample_descriptor()
    caps = add_tool_capabilities_to_announce(
        {"tags": ["region.eu", "ai-tool:web_search"]},
        [desc],
    )
    occurrences = sum(1 for t in caps["tags"] if t == "ai-tool:web_search")
    assert occurrences == 1
    assert "region.eu" in caps["tags"]


def test_add_tool_capabilities_no_op_on_empty_descriptors() -> None:
    caps = add_tool_capabilities_to_announce({"tags": ["x"]}, [])
    assert caps["tags"] == ["x"]
    assert "tools" not in caps


# ---------------------------------------------------------------------------
# OpenAI format
# ---------------------------------------------------------------------------


def test_openai_to_tool_emits_function_envelope_and_strict() -> None:
    tool = openai.to_openai_tool(sample_descriptor())
    assert tool["type"] == "function"
    fn = tool["function"]
    assert fn["name"] == "web_search"
    assert fn["description"] == "Search the web."
    assert fn["strict"] is True
    assert fn["parameters"]["type"] == "object"


def test_openai_lower_tool_call_extracts_name_arguments_and_id() -> None:
    spec = openai.lower_openai_tool_call(
        {
            "id": "call_abc",
            "type": "function",
            "function": {"name": "web_search", "arguments": '{"query":"mesh"}'},
        }
    )
    assert spec.name == "web_search"
    assert spec.arguments_json == '{"query":"mesh"}'
    assert spec.provider_call_id == "call_abc"


def test_openai_lower_tool_call_rejects_malformed_arguments() -> None:
    with pytest.raises(ToolCallParseError):
        openai.lower_openai_tool_call(
            {"function": {"name": "x", "arguments": "not valid json {"}}
        )


# ---------------------------------------------------------------------------
# Anthropic format
# ---------------------------------------------------------------------------


def test_anthropic_to_tool_uses_snake_case_input_schema() -> None:
    tool = anthropic.to_anthropic_tool(sample_descriptor())
    assert tool["name"] == "web_search"
    assert tool["description"] == "Search the web."
    assert tool["input_schema"]["type"] == "object"
    assert "strict" not in tool


def test_anthropic_lower_tool_use_serializes_input_and_carries_id() -> None:
    spec = anthropic.lower_anthropic_tool_use(
        {
            "type": "tool_use",
            "id": "toolu_xyz",
            "name": "web_search",
            "input": {"query": "mesh", "max_results": 5},
        }
    )
    assert spec.name == "web_search"
    parsed = json.loads(spec.arguments_json)
    assert parsed["query"] == "mesh"
    assert parsed["max_results"] == 5
    assert spec.provider_call_id == "toolu_xyz"


# ---------------------------------------------------------------------------
# MCP format
# ---------------------------------------------------------------------------


def test_mcp_to_tool_uses_camel_case_input_schema() -> None:
    tool = mcp.to_mcp_tool(sample_descriptor())
    assert tool["name"] == "web_search"
    assert tool["inputSchema"]["type"] == "object"


def test_mcp_lower_tools_call_leaves_provider_call_id_none() -> None:
    spec = mcp.lower_mcp_tools_call(
        {"name": "web_search", "arguments": {"query": "mesh"}}
    )
    assert spec.name == "web_search"
    parsed = json.loads(spec.arguments_json)
    assert parsed["query"] == "mesh"
    assert spec.provider_call_id is None


# ---------------------------------------------------------------------------
# Gemini format
# ---------------------------------------------------------------------------


def test_gemini_to_function_declaration_uses_parameters_field() -> None:
    decl = gemini.to_gemini_function_declaration(sample_descriptor())
    assert decl["name"] == "web_search"
    assert decl["parameters"]["type"] == "object"


def test_gemini_lower_function_call_reads_args_field() -> None:
    spec = gemini.lower_gemini_function_call(
        {"name": "web_search", "args": {"query": "mesh"}}
    )
    assert spec.name == "web_search"
    parsed = json.loads(spec.arguments_json)
    assert parsed["query"] == "mesh"
    assert spec.provider_call_id is None


# ---------------------------------------------------------------------------
# Empty-schema fallback (covers all four formats)
# ---------------------------------------------------------------------------


def test_formats_fall_back_to_empty_object_schema() -> None:
    desc = descriptor_for("no_args", description="Bare.")
    assert openai.to_openai_tool(desc)["function"]["parameters"]["type"] == "object"
    assert openai.to_openai_tool(desc)["function"]["strict"] is False
    assert anthropic.to_anthropic_tool(desc)["input_schema"]["type"] == "object"
    assert mcp.to_mcp_tool(desc)["inputSchema"]["type"] == "object"
    assert gemini.to_gemini_function_declaration(desc)["parameters"]["type"] == "object"


# ---------------------------------------------------------------------------
# call_tool_streaming — missing_terminal synthesis + happy paths
# ---------------------------------------------------------------------------


class _FakeStream:
    """Sync iterator surface that mirrors TypedRpcStream just enough
    for call_tool_streaming to drain. Tracks whether close() fired."""

    def __init__(self, events: List[Any]) -> None:
        self._events = list(events)
        self.closed = False

    def __iter__(self) -> "Any":
        for e in self._events:
            yield e

    def close(self) -> None:
        self.closed = True


class _FakeRpc:
    """Mock TypedMeshRpc with just enough surface for
    call_tool_streaming(rpc, ...)."""

    def __init__(self, events: List[Any]) -> None:
        self._events = events
        self.last_stream: _FakeStream | None = None

    def call_service_streaming(
        self, tool_id: str, request: Any, opts: Any = None
    ) -> _FakeStream:
        self.last_stream = _FakeStream(self._events)
        return self.last_stream


def test_call_tool_streaming_passes_terminal_event_through() -> None:
    rpc = _FakeRpc(
        [
            {"type": "start", "tool_id": "web_search", "call_id": 1},
            {"type": "delta", "data": {"partial": "a"}},
            {"type": "result", "data": {"final": "ok"}},
        ]
    )
    events = list(call_tool_streaming(rpc, "web_search", {}))
    assert len(events) == 3
    assert events[-1]["type"] == "result"
    # No synthesized missing_terminal envelope.
    assert not any(
        e.get("type") == "error" and e.get("code") == "missing_terminal" for e in events
    )


def test_call_tool_streaming_synthesizes_missing_terminal_on_clean_eof() -> None:
    rpc = _FakeRpc(
        [
            {"type": "start", "tool_id": "web_search", "call_id": 2},
            {"type": "delta", "data": {"partial": "no-terminal"}},
        ]
    )
    events = list(call_tool_streaming(rpc, "web_search", {}))
    # start + delta + synthesized error envelope.
    assert len(events) == 3
    final = events[-1]
    # Exact byte shape pinned by T-2 fixture.
    assert final["type"] == "error"
    assert final["code"] == "missing_terminal"
    assert "terminal" in final["message"].lower()


def test_call_tool_streaming_empty_stream_emits_single_synthesized_error() -> None:
    rpc = _FakeRpc([])
    events = list(call_tool_streaming(rpc, "noop_tool", {}))
    assert len(events) == 1
    assert events[0]["type"] == "error"
    assert events[0]["code"] == "missing_terminal"


def test_call_tool_streaming_error_terminal_suppresses_synthesis() -> None:
    rpc = _FakeRpc(
        [
            {"type": "start", "tool_id": "web_search", "call_id": 3},
            {"type": "error", "code": "handler_panicked", "message": "boom"},
        ]
    )
    events = list(call_tool_streaming(rpc, "web_search", {}))
    assert len(events) == 2
    # Handler's original error survives — wrapper does NOT paper over.
    assert events[-1]["code"] == "handler_panicked"


# ---------------------------------------------------------------------------
# call_tool_streaming_async — same contract on the asyncio surface
# ---------------------------------------------------------------------------


class _FakeAsyncStream:
    def __init__(self, events: List[Any]) -> None:
        self._events = list(events)
        self.closed = False

    def __aiter__(self) -> "AsyncIterator[Any]":
        async def gen() -> AsyncIterator[Any]:
            for e in self._events:
                yield e

        return gen()

    async def aclose(self) -> None:
        self.closed = True


class _FakeAsyncRpc:
    def __init__(self, events: List[Any]) -> None:
        self._events = events
        self.last_stream: _FakeAsyncStream | None = None

    async def call_service_streaming(
        self, tool_id: str, request: Any, opts: Any = None
    ) -> _FakeAsyncStream:
        self.last_stream = _FakeAsyncStream(self._events)
        return self.last_stream


def _drain_async(coro_factory: Any) -> List[Any]:
    """Run an async generator factory under the asyncio runtime and
    collect all yielded events."""
    async def driver() -> List[Any]:
        out: List[Any] = []
        async for e in coro_factory():
            out.append(e)
        return out

    return asyncio.run(driver())


def test_call_tool_streaming_async_passes_terminal_through() -> None:
    rpc = _FakeAsyncRpc(
        [
            {"type": "start", "tool_id": "web_search", "call_id": 1},
            {"type": "result", "data": {"final": "ok"}},
        ]
    )
    events = _drain_async(lambda: call_tool_streaming_async(rpc, "web_search", {}))
    assert events[-1]["type"] == "result"
    assert not any(
        e.get("type") == "error" and e.get("code") == "missing_terminal" for e in events
    )


def test_call_tool_streaming_async_synthesizes_missing_terminal() -> None:
    rpc = _FakeAsyncRpc(
        [
            {"type": "start", "tool_id": "web_search", "call_id": 2},
            {"type": "delta", "data": {"partial": "no-terminal"}},
        ]
    )
    events = _drain_async(lambda: call_tool_streaming_async(rpc, "web_search", {}))
    assert events[-1]["type"] == "error"
    assert events[-1]["code"] == "missing_terminal"
    assert rpc.last_stream is not None
    # aclose() must have fired on the underlying stream — the wrapper
    # ALWAYS closes in the `finally` block, even when the host's
    # handler exits without a terminal.
    assert rpc.last_stream.closed is True


def test_call_tool_streaming_async_empty_stream_emits_synthesized_error() -> None:
    rpc = _FakeAsyncRpc([])
    events = _drain_async(lambda: call_tool_streaming_async(rpc, "noop_tool", {}))
    assert len(events) == 1
    assert events[0]["code"] == "missing_terminal"


# ---------------------------------------------------------------------------
# serve_tool / serve_tool_streaming — registry cleanup + server-side
# missing_terminal regression tests (C-3, E-9, E-8).
# ---------------------------------------------------------------------------


class _ServeHandle:
    """Minimal stand-in for the real ServeHandle the napi/pyo3
    layers return — just an idempotent close() flag."""

    def __init__(self) -> None:
        self.closed = False

    def close(self) -> None:
        self.closed = True


class _Sink:
    """Captures sink.send(event) calls from a streaming handler."""

    def __init__(self) -> None:
        self.events: List[Any] = []

    def send(self, event: Any) -> None:
        self.events.append(event)


class _ServeRpc:
    """Mock TypedMeshRpc with enough surface for serve_tool +
    serve_tool_streaming. Tracks the wrapped streaming handler so
    tests can drive it directly with a _Sink."""

    def __init__(self) -> None:
        self.served: List[str] = []
        self.streaming_handlers: dict = {}

    def serve(self, service: str, handler: Any) -> _ServeHandle:
        self.served.append(service)
        return _ServeHandle()

    def serve_streaming(self, service: str, handler: Any) -> _ServeHandle:
        self.served.append(service)
        self.streaming_handlers[service] = handler
        return _ServeHandle()


def _reset_registries() -> None:
    """Clear the process-global registry so tests don't bleed into
    each other."""
    _tool_module._tool_registries.clear()


def test_serve_tool_close_removes_global_registry_entry() -> None:
    """C-3 + E-9: closing the last serve handle for an rpc must drop
    the process-global entry AND close the fetch handler. Otherwise
    long-lived processes recycling rpc instances leak."""
    _reset_registries()
    rpc = _ServeRpc()

    handle = serve_tool(rpc, "web_search", lambda req: {"ok": True})
    key = id(rpc)
    assert key in _tool_module._tool_registries
    entry = _tool_module._tool_registries[key]
    fetch_handle = entry["fetch_handle"]
    assert fetch_handle is not None
    assert fetch_handle.closed is False
    # Fetch handler installed alongside the user's tool.
    assert TOOL_METADATA_FETCH_SERVICE in rpc.served
    assert "web_search" in rpc.served

    handle.close()

    assert key not in _tool_module._tool_registries, "registry entry leaked"
    assert fetch_handle.closed is True, "fetch handler must close on last serve close"


def test_serve_tool_close_keeps_entry_if_other_serves_active() -> None:
    """Closing ONE of two serve handles must leave the entry intact
    so the surviving handle's fetch handler still answers."""
    _reset_registries()
    rpc = _ServeRpc()

    h1 = serve_tool(rpc, "web_search", lambda req: 1)
    h2 = serve_tool(rpc, "summarize", lambda req: 2)
    key = id(rpc)
    assert key in _tool_module._tool_registries
    fetch_handle = _tool_module._tool_registries[key]["fetch_handle"]

    h1.close()
    assert key in _tool_module._tool_registries, "entry dropped while h2 still active"
    assert fetch_handle.closed is False

    h2.close()
    assert key not in _tool_module._tool_registries
    assert fetch_handle.closed is True


def test_serve_tool_streaming_synthesizes_missing_terminal_when_handler_clean_returns() -> None:
    """E-8 (server-side): a handler that yields events WITHOUT a
    terminal frame must have the wrapper emit a synthesized
    missing_terminal envelope before returning. Otherwise raw
    clients (other languages, direct drain) see broken streams."""
    _reset_registries()
    rpc = _ServeRpc()

    def handler(req: Any):
        yield {"type": "start", "tool_id": "web_search"}
        yield {"type": "delta", "data": {"partial": "no terminal here"}}

    serve_tool_streaming(rpc, "web_search", handler)
    wrapped = rpc.streaming_handlers["web_search"]
    sink = _Sink()
    wrapped({"any": "request"}, sink)

    # start + delta + synth missing_terminal.
    assert len(sink.events) == 3
    final = sink.events[-1]
    assert final["type"] == "error"
    assert final["code"] == "missing_terminal"


def test_serve_tool_streaming_no_synth_when_handler_yields_terminal() -> None:
    """E-8: when the user's handler yields a terminal `result` or
    `error`, the wrapper must NOT inject a duplicate."""
    _reset_registries()
    rpc = _ServeRpc()

    def handler(req: Any):
        yield {"type": "start", "tool_id": "web_search"}
        yield {"type": "result", "data": {"final": "ok"}}

    serve_tool_streaming(rpc, "web_search", handler)
    wrapped = rpc.streaming_handlers["web_search"]
    sink = _Sink()
    wrapped({}, sink)

    assert len(sink.events) == 2
    assert sink.events[-1]["type"] == "result"
    assert not any(
        e.get("code") == "missing_terminal" for e in sink.events
    ), "wrapper double-emitted a synth after user's terminal"


def test_serve_tool_streaming_handler_exception_maps_to_handler_error() -> None:
    """E-8: exceptions from the user handler convert to a terminal
    handler_error envelope (no missing_terminal synth)."""
    _reset_registries()
    rpc = _ServeRpc()

    def handler(req: Any):
        yield {"type": "start", "tool_id": "web_search"}
        raise RuntimeError("boom")

    serve_tool_streaming(rpc, "web_search", handler)
    wrapped = rpc.streaming_handlers["web_search"]
    sink = _Sink()
    wrapped({}, sink)

    # start + handler_error
    assert len(sink.events) == 2
    assert sink.events[-1]["type"] == "error"
    assert sink.events[-1]["code"] == "handler_error"
    assert "boom" in sink.events[-1]["message"]
