"""Typed nRPC wrappers + retry / hedge / circuit-breaker helpers.

Sits on top of the raw ``net.MeshRpc`` pyclass: translates typed
Python objects to/from JSON bytes, and provides pure-Python
implementations of the resilience policies that mirror the Rust
SDK's defaults.

Usage::

    from net import NetMesh
    from net.mesh_rpc import TypedMeshRpc, RetryPolicy

    mesh = NetMesh("127.0.0.1:0", "00..." * 32)
    rpc = TypedMeshRpc(mesh)

    handle = rpc.serve("echo", lambda req: req)
    reply = rpc.call(target_id, "echo", {"hello": "world"})

    # With retry:
    policy = RetryPolicy(max_attempts=3)
    reply = rpc.call_with_retry(target_id, "echo", req, policy=policy)

This module is feature-complete for synchronous Python; native
``async def`` handler support is a follow-up that would require
pyo3-asyncio integration on the binding side.
"""

from __future__ import annotations

import json
import random
import threading
import time
from dataclasses import dataclass, field
from typing import Any, Callable, Iterator, Optional, Sequence

# Import the native types lazily so this module loads cleanly even
# when the binding was built without the cortex feature (in which
# case TypedMeshRpc raises a clear error on first use). Most users
# will have cortex enabled.
try:
    from net._net import (
        Cancellable,
        ClientStreamCall as _RawClientStreamCall,
        DuplexCall as _RawDuplexCall,
        DuplexSink as _RawDuplexSink,
        DuplexStream as _RawDuplexStream,
        MeshRpc as _RawMeshRpc,
        RequestStreamRecv as _RawRequestStreamRecv,
        ResponseSinkSend as _RawResponseSinkSend,
        RpcAppError,
        RpcCallEvent as _RawRpcCallEvent,
        RpcCancelledError,
        RpcCapabilityDeniedError,
        RpcCodecError,
        RpcError,
        RpcMetricsSnapshot as _RawRpcMetricsSnapshot,
        RpcNoRouteError,
        RpcServerError,
        RpcStream as _RawRpcStream,
        RpcTimeoutError,
        RpcTransportError,
        ServeHandle,
        ServiceMetrics as _RawServiceMetrics,
    )
except ImportError:  # pragma: no cover — feature-flag path
    _RawMeshRpc = None  # type: ignore[assignment]
    _RawRpcStream = None  # type: ignore[assignment]
    _RawClientStreamCall = None  # type: ignore[assignment]
    _RawDuplexCall = None  # type: ignore[assignment]
    _RawDuplexSink = None  # type: ignore[assignment]
    _RawDuplexStream = None  # type: ignore[assignment]
    _RawRequestStreamRecv = None  # type: ignore[assignment]
    _RawResponseSinkSend = None  # type: ignore[assignment]
    _RawRpcCallEvent = None  # type: ignore[assignment]
    _RawRpcMetricsSnapshot = None  # type: ignore[assignment]
    _RawServiceMetrics = None  # type: ignore[assignment]
    RpcError = Exception  # type: ignore[misc,assignment]
    RpcNoRouteError = RpcError  # type: ignore[misc,assignment]
    RpcTimeoutError = RpcError  # type: ignore[misc,assignment]
    RpcServerError = RpcError  # type: ignore[misc,assignment]
    RpcTransportError = RpcError  # type: ignore[misc,assignment]
    RpcCodecError = RpcError  # type: ignore[misc,assignment]
    RpcCancelledError = RpcError  # type: ignore[misc,assignment]
    RpcCapabilityDeniedError = RpcError  # type: ignore[misc,assignment]

    # Fallback `RpcAppError` carries (status, body) on `args` so the
    # cross-binding semantics still hold against test stubs. The
    # native class registered in lib.rs has the same shape; users
    # writing typed handlers raise it identically in both paths.
    class RpcAppError(RpcError):  # type: ignore[no-redef]
        """Application-status signal for typed serve handlers.

        Arguments: ``(status: int, body: bytes | str)``. The Rust
        side translates this to ``RpcStatus::Application(status)``
        with ``body`` as the response body. Pure-Python fallback
        used when the native module isn't available; same shape so
        user code is portable across both paths.
        """

    # Fallback Cancellable for non-native test paths. The native
    # class hooks into the tokio runtime; the fallback just
    # latches the cancel flag so user code that constructs and
    # cancels works without a built wheel — but cancellation has
    # no effect on whatever stub raw layer is in use.
    class Cancellable:  # type: ignore[no-redef]
        """Caller-side cancel token. Pure-Python fallback used
        when the native module isn't available; cancel() latches
        but doesn't reach into a runtime.
        """

        def __init__(self) -> None:
            self._cancelled = False

        def cancel(self) -> None:
            self._cancelled = True

        def is_cancelled(self) -> bool:
            return self._cancelled

    ServeHandle = Any  # type: ignore[misc,assignment]


# ---------------------------------------------------------------------------
# Status codes — parallel to the Rust SDK's NRPC_TYPED_BAD_REQUEST /
# NRPC_TYPED_HANDLER_ERROR and the Node binding's exports.
# ---------------------------------------------------------------------------

#: Application status: typed handler couldn't decode the request body.
NRPC_TYPED_BAD_REQUEST = 0x8000

#: Application status: typed handler returned an exception.
NRPC_TYPED_HANDLER_ERROR = 0x8001


# ---------------------------------------------------------------------------
# JSON codec helpers.
#
# The typed wrappers wrap the user's Python value with json.dumps /
# loads on each side of the wire. Encode failure (non-serializable
# value) raises RpcCodecError BEFORE the call hits the wire so the
# diagnostic points at the user's call site rather than at the
# server. Decode failure on the response raises RpcCodecError too.
# ---------------------------------------------------------------------------


def _json_encode(value: Any) -> bytes:
    try:
        text = json.dumps(value, separators=(",", ":"))
    except (TypeError, ValueError) as e:
        raise RpcCodecError(f"client encode: {e}") from e
    return text.encode("utf-8")


def _json_decode(buf: bytes) -> Any:
    try:
        return json.loads(buf.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError, ValueError) as e:
        raise RpcCodecError(f"client decode: {e}") from e


# ---------------------------------------------------------------------------
# TypedRpcStream — typed wrapper around the raw RpcStream iterator.
#
# Yields decoded Python objects; raises RpcCodecError on a chunk
# that fails to decode (and closes the underlying stream so the
# server's handler observes CANCEL).
# ---------------------------------------------------------------------------


class TypedRpcStream:
    """Typed iterator over a streaming RPC. Each iteration yields
    a decoded Python object. Raises ``RpcCodecError`` on a
    malformed chunk (and closes the underlying stream).
    """

    def __init__(self, raw: _RawRpcStream) -> None:
        self._raw = raw
        self._done = False

    def __iter__(self) -> Iterator[Any]:
        return self

    def __next__(self) -> Any:
        if self._done:
            raise StopIteration
        try:
            chunk = next(self._raw)
        except StopIteration:
            self._done = True
            raise
        try:
            return _json_decode(chunk)
        except RpcCodecError:
            self._done = True
            try:
                self._raw.close()
            except Exception:
                # Best-effort — don't mask the original codec error.
                pass
            raise

    def grant(self, n: int) -> None:
        """Grant ``n`` flow-control credits to the server pump."""
        self._raw.grant(n)

    def flow_controlled(self) -> bool:
        """``True`` if the call set ``stream_window_initial``."""
        return bool(self._raw.flow_controlled())

    def close(self) -> None:
        """Close the stream; emits CANCEL to the server."""
        self._done = True
        try:
            self._raw.close()
        except Exception:
            pass


# ---------------------------------------------------------------------------
# TypedClientStreamCall + TypedRequestStream (S2-C1).
#
# Client-streaming: caller pushes typed requests via ``send``,
# then ``finish`` awaits a single terminal response.
#
# Cancellation contract (locked decision #2): v1 ``close()``-only.
# Unifying cancel-token / signal / context propagation across
# streaming shapes is a deliberate post-v1 follow-up.
# ---------------------------------------------------------------------------


class TypedClientStreamCall:
    """Typed client-streaming call handle. Push typed requests via
    :meth:`send`, then :meth:`finish` to await the terminal
    response. Supports the context-manager protocol so a ``with``
    block closes the call on exit.

    Encoding failures on :meth:`send` raise :class:`RpcCodecError`
    BEFORE the chunk hits the wire; decode failure on the
    terminal response surfaces similarly.
    """

    def __init__(self, raw: Any) -> None:
        self._raw = raw

    @property
    def raw(self) -> Any:
        """Underlying raw ``ClientStreamCall`` (bytes-level surface)."""
        return self._raw

    def send(self, value: Any) -> None:
        """Encode ``value`` as JSON and push it as one request
        chunk. Raises :class:`RpcCodecError` on encode failure;
        the chunk is NOT sent in that case.
        """
        self._raw.send(_json_encode(value))

    def finish(self) -> Any:
        """Close the upload direction and await the terminal
        response. Consumes the call. Returns the decoded
        response; raises :class:`RpcCodecError` on a malformed
        terminal body.
        """
        return _json_decode(self._raw.finish())

    def call_id(self) -> int:
        """Server-assigned ``call_id``."""
        return int(self._raw.call_id())

    def flow_controlled(self) -> bool:
        """``True`` if the call was opened with
        ``request_window_initial``.
        """
        return bool(self._raw.flow_controlled())

    def close(self) -> None:
        """Close without finishing. Fires CANCEL via the SDK's
        Drop. Idempotent; concurrent in-flight :meth:`send`
        awaiting credit observes ``RpcError("send aborted by
        close()")``.
        """
        try:
            self._raw.close()
        except Exception:
            # Best-effort — match TypedRpcStream.close semantics.
            pass

    def __enter__(self) -> "TypedClientStreamCall":
        return self

    def __exit__(self, exc_type: Any, exc_value: Any, traceback: Any) -> bool:
        self.close()
        return False


class TypedRequestStream:
    """Typed inbound request stream surfaced to client-streaming +
    duplex server handlers. Iterates over decoded chunks until
    EOF (``StopIteration``). Decode failure on a chunk raises
    :class:`RpcCodecError` and marks the stream done so subsequent
    ``next`` returns ``StopIteration``.

    Diagnostic getters (``caller_origin``, ``call_id``,
    ``deadline_ns``, ``headers``) are stable for the lifetime of
    the stream.
    """

    def __init__(self, raw: Any) -> None:
        self._raw = raw
        self._done = False

    @property
    def raw(self) -> Any:
        """Underlying raw ``RequestStreamRecv``."""
        return self._raw

    @property
    def caller_origin(self) -> int:
        """Caller's peer origin hash (``0`` on loopback)."""
        return int(self._raw.caller_origin)

    @property
    def call_id(self) -> int:
        """Server-assigned ``call_id``."""
        return int(self._raw.call_id)

    @property
    def deadline_ns(self) -> int:
        """Caller's declared deadline as a Unix-nanoseconds absolute
        timestamp; ``0`` means no deadline.
        """
        return int(self._raw.deadline_ns)

    @property
    def headers(self) -> list:
        """Initial-REQUEST headers as ``[(name, bytes)]``."""
        return list(self._raw.headers)

    def __iter__(self) -> Iterator[Any]:
        return self

    def __next__(self) -> Any:
        if self._done:
            raise StopIteration
        try:
            chunk = next(self._raw)
        except StopIteration:
            self._done = True
            raise
        try:
            return _json_decode(chunk)
        except RpcCodecError:
            # Mark done so subsequent next() returns StopIteration
            # — refuse to continue draining a stream whose framing
            # is broken. Mirrors TypedRpcStream's behavior.
            self._done = True
            raise


# ---------------------------------------------------------------------------
# TypedDuplexCall + TypedDuplexSink + TypedDuplexStream +
# TypedResponseSink (S2-C2).
#
# Duplex: caller pushes Reqs and pulls Resps concurrently on a
# single call. ``into_split`` separates the halves for the
# encoder-thread / decoder-thread pattern.
#
# Cancellation contract (locked decision #2): v1 ``close()``-only,
# same as client-streaming.
# ---------------------------------------------------------------------------


class TypedDuplexCall:
    """Typed duplex call handle. Push typed requests via
    :meth:`send`, pull typed responses via :meth:`__next__` /
    iteration, or call :meth:`into_split` to peel off independent
    sink + stream halves.

    After :meth:`into_split` returns, the call is consumed —
    subsequent :meth:`send` / :meth:`finish_sending` /
    :meth:`__next__` raise.
    """

    def __init__(self, raw: Any) -> None:
        self._raw = raw
        self._done = False

    @property
    def raw(self) -> Any:
        """Underlying raw ``DuplexCall``."""
        return self._raw

    def send(self, value: Any) -> None:
        """Encode + push one request chunk."""
        self._raw.send(_json_encode(value))

    def finish_sending(self) -> None:
        """Close the upload direction (emit REQUEST_END)."""
        self._raw.finish_sending()

    def __iter__(self) -> Iterator[Any]:
        return self

    def __next__(self) -> Any:
        """Pull the next decoded response. Raises
        ``StopIteration`` on clean EOF. Decode failure raises
        :class:`RpcCodecError` after closing the underlying
        duplex call.
        """
        if self._done:
            raise StopIteration
        try:
            chunk = next(self._raw)
        except StopIteration:
            self._done = True
            raise
        try:
            return _json_decode(chunk)
        except RpcCodecError:
            self._done = True
            try:
                self._raw.close()
            except Exception:
                pass
            raise

    def into_split(self) -> tuple:
        """Split the duplex into independent typed sink + stream
        halves. After return, this call is consumed — subsequent
        :meth:`send` / :meth:`__next__` raise. CANCEL fires only
        when BOTH split halves drop without observing the
        response stream's terminal frame.

        Returns ``(TypedDuplexSink, TypedDuplexStream)``.
        """
        raw_sink, raw_stream = self._raw.into_split()
        self._done = True
        return TypedDuplexSink(raw_sink), TypedDuplexStream(raw_stream)

    def call_id(self) -> int:
        """Server-assigned ``call_id``."""
        return int(self._raw.call_id())

    def flow_controlled(self) -> bool:
        """``True`` if the call was opened with non-``None``
        ``request_window_initial``. Reports the upload-direction
        flow-control state.
        """
        return bool(self._raw.flow_controlled())

    def close(self) -> None:
        """Close without observing the response terminator. Fires
        CANCEL on the wire. Idempotent.
        """
        self._done = True
        try:
            self._raw.close()
        except Exception:
            pass

    def __enter__(self) -> "TypedDuplexCall":
        return self

    def __exit__(self, exc_type: Any, exc_value: Any, traceback: Any) -> bool:
        self.close()
        return False


class TypedDuplexSink:
    """Send-half of a typed duplex call after
    :meth:`TypedDuplexCall.into_split`.
    """

    def __init__(self, raw: Any) -> None:
        self._raw = raw

    @property
    def raw(self) -> Any:
        """Underlying raw ``DuplexSink``."""
        return self._raw

    def send(self, value: Any) -> None:
        """Encode + push one request chunk."""
        self._raw.send(_json_encode(value))

    def finish(self) -> None:
        """Close the upload direction (emit REQUEST_END)."""
        self._raw.finish()

    def call_id(self) -> int:
        return int(self._raw.call_id())

    def flow_controlled(self) -> bool:
        return bool(self._raw.flow_controlled())

    def close(self) -> None:
        """Close without emitting REQUEST_END. Idempotent."""
        try:
            self._raw.close()
        except Exception:
            pass


class TypedDuplexStream:
    """Receive-half of a typed duplex call after
    :meth:`TypedDuplexCall.into_split`. Iterates over decoded
    responses; decode failure raises :class:`RpcCodecError` and
    closes the underlying stream.
    """

    def __init__(self, raw: Any) -> None:
        self._raw = raw
        self._done = False

    @property
    def raw(self) -> Any:
        """Underlying raw ``DuplexStream``."""
        return self._raw

    def __iter__(self) -> Iterator[Any]:
        return self

    def __next__(self) -> Any:
        if self._done:
            raise StopIteration
        try:
            chunk = next(self._raw)
        except StopIteration:
            self._done = True
            raise
        try:
            return _json_decode(chunk)
        except RpcCodecError:
            self._done = True
            try:
                self._raw.close()
            except Exception:
                pass
            raise

    def call_id(self) -> int:
        return int(self._raw.call_id())

    def close(self) -> None:
        """Close the stream. Idempotent."""
        self._done = True
        try:
            self._raw.close()
        except Exception:
            pass


class TypedResponseSink:
    """Typed outbound response sink for duplex server handlers.

    Non-async (mirrors the raw ``ResponseSinkSend.send``). Returns
    ``True`` when the chunk was enqueued; ``False`` if the
    underlying sink is closed. Encode failure raises
    :class:`RpcCodecError` and the chunk is NOT sent.

    Flow control: the underlying sink ``try_send``\\ s into a
    bounded 1024-chunk mpsc. Bursts past the credit window are
    dropped (counted by ``streaming_chunks_dropped_total``). Pace
    your :meth:`send` calls to the protocol's REQUEST_GRANT
    cadence for lossless flow control.
    """

    def __init__(self, raw: Any) -> None:
        self._raw = raw

    @property
    def raw(self) -> Any:
        """Underlying raw ``ResponseSinkSend``."""
        return self._raw

    def send(self, value: Any) -> bool:
        """Encode + emit one response chunk. Returns ``True`` on
        successful enqueue; ``False`` if the sink has been closed.
        Raises :class:`RpcCodecError` on encode failure.
        """
        return bool(self._raw.send(_json_encode(value)))


# ---------------------------------------------------------------------------
# TypedMeshRpc — public typed wrapper.
# ---------------------------------------------------------------------------


class TypedMeshRpc:
    """Typed wrapper around the raw ``MeshRpc`` pyclass. JSON
    encode / decode happens at the binding boundary so user code
    works with plain Python objects (dicts, lists, strings, etc.).

    Constructed via :meth:`from_mesh` (or directly via
    ``TypedMeshRpc(MeshRpc(net_mesh))``). Resilience helpers
    (``call_with_retry``, ``call_with_hedge_to``) are methods on
    this class.
    """

    def __init__(self, raw: Any) -> None:
        # `raw` is duck-typed: it must expose `serve` / `call` /
        # `call_service` / `call_streaming` / `find_service_nodes`.
        # The native `_RawMeshRpc` satisfies this; tests can pass a
        # stub. We deliberately don't gate on `_RawMeshRpc is None`
        # here so a non-cortex build can still drive the wrapper
        # against test stubs.
        self._raw = raw

    @classmethod
    def from_mesh(cls, mesh: Any) -> "TypedMeshRpc":
        """Build a TypedMeshRpc against an existing ``NetMesh``."""
        if _RawMeshRpc is None:
            raise RuntimeError(
                "net._net.MeshRpc unavailable — did the wheel get built "
                "without the cortex feature?"
            )
        return cls(_RawMeshRpc(mesh))

    @property
    def raw(self) -> _RawMeshRpc:
        """Underlying raw ``MeshRpc`` (bytes-level surface)."""
        return self._raw

    # ---- serve --------------------------------------------------------------

    def serve(
        self,
        service: str,
        handler: Callable[[Any], Any],
    ) -> ServeHandle:
        """Register a typed handler. ``handler`` receives a
        decoded request and returns a response (which gets JSON-
        encoded back to the wire).

        Decode failures on the request surface to the caller as a
        canonical typed-bad-request: status
        ``NRPC_TYPED_BAD_REQUEST`` (``0x8000``), JSON body
        ``{"error": "invalid_request", "detail": ...}``. This
        matches the Rust integration test contract pinned in
        ``tests/integration_nrpc_cross_lang.rs`` and the cross-
        binding fixture under
        ``tests/cross_lang_nrpc/golden_vectors.json``. Other
        handler-raised exceptions still map to
        ``RpcStatus::Internal``; raise ``RpcAppError(status, body)``
        explicitly to surface a custom application status.
        """

        def _wrapped(req_bytes: bytes) -> bytes:
            try:
                req = _json_decode(req_bytes)
            except RpcCodecError as e:
                body = json.dumps(
                    {"error": "invalid_request", "detail": str(e)},
                    separators=(",", ":"),
                ).encode("utf-8")
                raise RpcAppError(NRPC_TYPED_BAD_REQUEST, body) from e
            resp = handler(req)
            return _json_encode(resp)

        return self._raw.serve(service, _wrapped)

    # ---- call ---------------------------------------------------------------

    def call(
        self,
        target_node_id: int,
        service: str,
        request: Any,
        opts: Optional[dict] = None,
    ) -> Any:
        """Direct-addressed typed call. Returns the decoded
        response; raises an ``RpcError`` subclass on failure.
        """
        req_bytes = _json_encode(request)
        resp_bytes = self._raw.call(target_node_id, service, req_bytes, opts)
        return _json_decode(resp_bytes)

    def call_service(
        self,
        service: str,
        request: Any,
        opts: Optional[dict] = None,
    ) -> Any:
        """Service-discovery typed call."""
        req_bytes = _json_encode(request)
        resp_bytes = self._raw.call_service(service, req_bytes, opts)
        return _json_decode(resp_bytes)

    def call_streaming(
        self,
        target_node_id: int,
        service: str,
        request: Any,
        opts: Optional[dict] = None,
    ) -> TypedRpcStream:
        """Open a typed streaming call. Returns a
        :class:`TypedRpcStream` that yields decoded values until
        EOF (StopIteration) or a terminal error.
        """
        req_bytes = _json_encode(request)
        raw = self._raw.call_streaming(target_node_id, service, req_bytes, opts)
        return TypedRpcStream(raw)

    def find_service_nodes(self, service: str) -> list[int]:
        """All node ids advertising ``nrpc:<service>``."""
        return list(self._raw.find_service_nodes(service))

    # ---- client-streaming (S2-C1) -------------------------------------------

    def call_client_stream(
        self,
        target_node_id: int,
        service: str,
        opts: Optional[dict] = None,
    ) -> "TypedClientStreamCall":
        """Open a typed client-streaming call. Returns a
        :class:`TypedClientStreamCall` — push typed requests via
        :meth:`send`, then :meth:`finish` to drain the terminal
        response.

        Cancellation contract (locked decision #2): v1 supports
        ``close()`` only. ``opts['cancel']`` is **not** wired into
        the raw streaming layer; invoke ``typed_call.close()`` to
        abort an in-flight stream.
        """
        raw_call = self._raw.call_client_stream(target_node_id, service, opts)
        return TypedClientStreamCall(raw_call)

    def serve_client_stream(
        self,
        service: str,
        handler: Callable[["TypedRequestStream"], Any],
    ) -> ServeHandle:
        """Register a typed client-streaming handler. ``handler``
        receives a :class:`TypedRequestStream` (auto-decodes each
        inbound chunk) and returns the terminal response
        ``Resp`` (which gets JSON-encoded back to the wire).

        Decode failure on a chunk surfaces from
        ``stream.__next__`` as :class:`RpcCodecError`. The
        handler MAY catch and skip; letting the exception
        propagate surfaces to the caller as
        ``RpcStatus::Internal``. Raise
        ``RpcAppError(NRPC_TYPED_BAD_REQUEST, body)`` to surface
        a typed bad-request status code instead.
        """

        def _wrapped(raw_stream: Any) -> bytes:
            typed_stream = TypedRequestStream(raw_stream)
            resp = handler(typed_stream)
            return _json_encode(resp)

        return self._raw.serve_client_stream(service, _wrapped)

    # ---- duplex (S2-C2) -----------------------------------------------------

    def call_duplex(
        self,
        target_node_id: int,
        service: str,
        opts: Optional[dict] = None,
    ) -> "TypedDuplexCall":
        """Open a typed duplex call. Returns a
        :class:`TypedDuplexCall` — push typed requests via
        :meth:`TypedDuplexCall.send`, pull typed responses via
        iteration, or :meth:`TypedDuplexCall.into_split` to
        separate the halves.

        Cancellation contract (locked decision #2): v1 supports
        ``close()`` only. ``opts['cancel']`` is **not** wired into
        the raw streaming layer; invoke ``typed_call.close()`` to
        abort an in-flight duplex.
        """
        raw_call = self._raw.call_duplex(target_node_id, service, opts)
        return TypedDuplexCall(raw_call)

    def serve_duplex(
        self,
        service: str,
        handler: Callable[["TypedRequestStream", "TypedResponseSink"], None],
    ) -> ServeHandle:
        """Register a typed duplex handler. ``handler`` signature
        is ``(stream: TypedRequestStream, sink: TypedResponseSink) -> None``:
        drain inbound chunks from ``stream``, emit response chunks
        via ``sink.send(value)``. Handler return is ``None``; the
        substrate emits the terminal frame automatically.

        Raise ``RpcAppError(code, body)`` to surface a typed
        Application status.
        """

        def _wrapped(raw_stream: Any, raw_sink: Any) -> None:
            typed_stream = TypedRequestStream(raw_stream)
            typed_sink = TypedResponseSink(raw_sink)
            handler(typed_stream, typed_sink)

        return self._raw.serve_duplex(service, _wrapped)

    # ---- resilience ---------------------------------------------------------

    def call_with_retry(
        self,
        target_node_id: int,
        service: str,
        request: Any,
        policy: "RetryPolicy",
        opts: Optional[dict] = None,
    ) -> Any:
        """Direct-addressed typed call with retry. Encodes the
        request once and reuses the bytes across attempts.
        """
        req_bytes = _json_encode(request)
        resp_bytes = run_retry(
            policy,
            lambda: self._raw.call(target_node_id, service, req_bytes, opts),
        )
        return _json_decode(resp_bytes)

    def call_with_hedge_to(
        self,
        targets: Sequence[int],
        service: str,
        request: Any,
        policy: "HedgePolicy",
        opts: Optional[dict] = None,
    ) -> Any:
        """Hedge typed call across the listed targets. First reply
        wins; if every target fails, the surfaced error is the
        primary's (target index 0) for stable diagnostics.
        """
        req_bytes = _json_encode(request)
        resp_bytes = run_hedge(
            policy,
            list(targets),
            lambda t: self._raw.call(t, service, req_bytes, opts),
        )
        return _json_decode(resp_bytes)


# ---------------------------------------------------------------------------
# Default retry / breaker predicates — mirror Rust SDK's
# default_retryable. Detection uses ``type(err).__name__`` (a
# runtime string) so it survives any future class-identity edge
# cases between the binding and user code.
# ---------------------------------------------------------------------------

_STATUS_INTERNAL = 0x0006
_STATUS_BACKPRESSURE = 0x0004
_STATUS_TIMEOUT = 0x0003


# Tolerates both ``status=0x...`` (the canonical Rust binding form
# emitted by ``rpc_error_to_pyerr``) and the legacy ``status 0x...``
# / ``status0x...`` shapes a future formatter change might produce.
# Matched in the cross-binding compat suite — see
# ``tests/test_cross_lang_compat.py::_parse_status``.
_STATUS_PATTERN = "status\\s*=?\\s*0x([0-9a-fA-F]+)"


def _parse_status_from_message(msg: str) -> Optional[int]:
    """Best-effort parse of ``status=0xNNNN`` from an
    ``RpcServerError`` message. Returns ``None`` if no match."""
    import re

    m = re.search(_STATUS_PATTERN, msg)
    return int(m.group(1), 16) if m else None


# Stable nRPC error-message prefix shared with the Node and Go
# bindings: every error message produced by the Rust binding starts
# with ``nrpc:<kind>:`` (see ``bindings/python/src/mesh_rpc.rs::
# rpc_error_to_pyerr``). The set of kinds is fixed by the cross-
# binding contract.
_NRPC_PREFIX = "nrpc:"
_NRPC_KINDS = frozenset({
    "no_route",
    "timeout",
    "server_error",
    "transport",
    "codec_encode",
    "codec_decode",
    "breaker_open",
    "cancelled",
    "capability_denied",
})


def classify_error(exc: BaseException) -> Optional[str]:
    """Extract the nRPC error kind from a caught exception's message.

    Returns one of the canonical kind strings (``"no_route"``,
    ``"timeout"``, ``"server_error"``, ``"transport"``,
    ``"codec_encode"``, ``"codec_decode"``, ``"breaker_open"``) or
    ``None`` when the message doesn't carry an ``nrpc:`` prefix.

    Useful for fallback paths where ``isinstance`` discrimination
    is awkward — e.g. when the native module wasn't built and every
    typed exception alias collapses to plain ``Exception``::

        try:
            rpc.call(...)
        except Exception as e:
            kind = classify_error(e)
            if kind == "no_route":
                ...
            elif kind == "timeout":
                ...

    Mirrors the Node binding's ``classifyError`` in
    ``bindings/node/errors.js``.
    """
    msg = str(exc)
    if not msg.startswith(_NRPC_PREFIX):
        return None
    body = msg[len(_NRPC_PREFIX) :]
    colon = body.find(":")
    if colon == -1:
        return None
    kind = body[:colon].strip()
    return kind if kind in _NRPC_KINDS else None


def default_retryable(err: BaseException) -> bool:
    """Default predicate for ``RetryPolicy``. Retries Timeout,
    Transport, and ServerError(Internal/Backpressure/Timeout).
    Skips NoRoute and Codec failures (caller-fixable / terminal),
    plus application errors (status >= 0x8000).
    """
    if err is None:
        return False
    name = type(err).__name__
    # CapabilityDeniedError is a signed policy verdict from the
    # target — retry can't change it until the target publishes a
    # more permissive announcement. Treat as terminal.
    if name in (
        "RpcNoRouteError",
        "RpcCodecError",
        "RpcCancelledError",
        "RpcCapabilityDeniedError",
    ):
        return False
    if name in ("RpcTimeoutError", "RpcTransportError"):
        return True
    if name == "RpcServerError":
        status = _parse_status_from_message(str(err))
        return status in (_STATUS_INTERNAL, _STATUS_BACKPRESSURE, _STATUS_TIMEOUT)
    return False


def default_breaker_failure(err: BaseException) -> bool:
    """Default predicate for ``CircuitBreaker``. Same shape as
    ``default_retryable`` — counts transient infrastructure
    failures, skips application errors and codec/no-route faults.
    """
    return default_retryable(err)


# ---------------------------------------------------------------------------
# RetryPolicy — mirrors net_sdk::mesh_rpc_resilience::RetryPolicy.
#
# Defaults match the Rust SDK: 3 attempts, 50ms→1s exponential,
# full-half jitter on, retryable predicate matches default_retryable.
# ---------------------------------------------------------------------------


@dataclass
class RetryPolicy:
    """Backoff + retry policy. Defaults: 3 attempts,
    50ms initial → 1s cap, doubling per attempt, jitter on.
    """

    max_attempts: int = 3
    initial_backoff_ms: int = 50
    max_backoff_ms: int = 1000
    backoff_multiplier: float = 2.0
    jitter: bool = True
    retryable: Callable[[BaseException], bool] = field(default=default_retryable)

    def __post_init__(self) -> None:
        # Clamp to sane defaults.
        self.max_attempts = max(1, int(self.max_attempts))
        self.initial_backoff_ms = max(0, int(self.initial_backoff_ms))
        self.max_backoff_ms = max(self.initial_backoff_ms, int(self.max_backoff_ms))
        self.backoff_multiplier = max(1.0, float(self.backoff_multiplier))

    def compute_backoff_ms(self, attempt: int) -> float:
        """Backoff for ``attempt`` (1-indexed). True ceiling at
        ``max_backoff_ms`` AFTER jitter."""
        exp = max(0, attempt - 1)
        scaled = self.initial_backoff_ms * (self.backoff_multiplier**exp)
        pre_cap = min(self.max_backoff_ms, scaled)
        if self.jitter:
            pre_cap = pre_cap * (0.5 + 0.5 * random.random())
        return min(self.max_backoff_ms, max(0.0, pre_cap))


def run_retry(policy: RetryPolicy, op: Callable[[], Any]) -> Any:
    """Run ``op`` under ``policy``. On retryable failure, sleep
    the computed backoff and re-issue. On exhaustion or non-
    retryable, re-raise the last exception.
    """
    last_exc: Optional[BaseException] = None
    for attempt in range(1, policy.max_attempts + 1):
        try:
            return op()
        except BaseException as e:
            last_exc = e
            if attempt == policy.max_attempts or not policy.retryable(e):
                raise
            ms = policy.compute_backoff_ms(attempt)
            if ms > 0:
                time.sleep(ms / 1000.0)
    # Defensive — loop returns or raises in every iteration.
    assert last_exc is not None
    raise last_exc  # pragma: no cover


# ---------------------------------------------------------------------------
# HedgePolicy + run_hedge.
#
# Uses threads (one per target) because the underlying raw call
# is synchronous from Python's perspective. First successful
# completion wins; if all fail, surface the PRIMARY's error
# (index 0) deterministically.
# ---------------------------------------------------------------------------


@dataclass
class HedgePolicy:
    """Hedge policy: primary at t=0, hedges at delay_ms * idx.
    Defaults: 50ms delay, 1 hedge.
    """

    delay_ms: int = 50
    hedges: int = 1

    def __post_init__(self) -> None:
        self.delay_ms = max(0, int(self.delay_ms))
        self.hedges = max(0, int(self.hedges))


def run_hedge(
    policy: HedgePolicy,
    targets: Sequence[int],
    op: Callable[[int], Any],
) -> Any:
    """Race ``op(target)`` across the listed targets. First
    successful return wins; if every call fails, raises the
    primary's exception (lowest target index with a recorded
    error) for deterministic diagnostics.
    """
    if not targets:
        # Match the Rust SDK + Node binding's NoRoute on empty.
        raise RpcNoRouteError("hedge: empty targets list")
    if policy.hedges == 0 or len(targets) == 1:
        return op(targets[0])

    fanout = min(len(targets), 1 + policy.hedges)
    selected = list(targets[:fanout])
    results: list[tuple[bool, Any]] = [(False, None)] * fanout
    done_event = threading.Event()
    lock = threading.Lock()

    def worker(idx: int, target: int) -> None:
        if idx > 0:
            # Stagger the hedge fires.
            if done_event.wait(timeout=(policy.delay_ms * idx) / 1000.0):
                return  # Winner already resolved before our hedge fires.
        try:
            value = op(target)
            with lock:
                if not done_event.is_set():
                    results[idx] = (True, value)
                    done_event.set()
        except BaseException as e:
            with lock:
                results[idx] = (False, e)

    threads = [
        threading.Thread(target=worker, args=(i, t), daemon=True)
        for i, t in enumerate(selected)
    ]
    for t in threads:
        t.start()

    # Wait for either the winner OR every thread to settle.
    for t in threads:
        t.join()

    # First successful result wins (we set done_event under lock).
    for ok, value in results:
        if ok:
            return value
    # All failed — surface the primary's error deterministically.
    for ok, value in results:
        if not ok and isinstance(value, BaseException):
            raise value
    # Defensive — `targets` was non-empty so at least one slot
    # must hold an exception by here.
    raise RpcError("hedge: drained with no error captured (bug)")  # pragma: no cover


# ---------------------------------------------------------------------------
# CircuitBreaker — mirrors net_sdk::mesh_rpc_resilience::CircuitBreaker.
#
# Three-state machine (closed → open → half-open → closed/open).
# Long-lived; instantiate once per logical downstream and share
# (it's thread-safe via an internal lock).
# ---------------------------------------------------------------------------


class BreakerOpenError(Exception):
    """Raised by :meth:`CircuitBreaker.call` when state is Open.

    Message carries the canonical ``nrpc:breaker_open:`` prefix so
    ``classify_error`` can dispatch on it the same way it does for
    binding-side errors.
    """

    def __init__(self) -> None:
        super().__init__("nrpc:breaker_open: circuit breaker is open")


_STATE_CLOSED = "closed"
_STATE_OPEN = "open"
_STATE_HALF_OPEN = "half-open"


class CircuitBreaker:
    """Three-state circuit breaker. Long-lived; instantiate once
    per logical downstream and share across calls (thread-safe).
    """

    def __init__(
        self,
        failure_threshold: int = 5,
        reset_after_ms: int = 30_000,
        success_threshold: int = 1,
        failure_predicate: Callable[[BaseException], bool] = default_breaker_failure,
    ) -> None:
        self.failure_threshold = max(1, int(failure_threshold))
        self.reset_after_ms = max(0, int(reset_after_ms))
        self.success_threshold = max(1, int(success_threshold))
        self.failure_predicate = failure_predicate
        self._lock = threading.Lock()
        self._state = _STATE_CLOSED
        self._consecutive_failures = 0
        self._consecutive_successes = 0
        self._opened_at: float = 0.0
        self._probe_in_flight = False

    def state(self) -> str:
        with self._lock:
            return self._state

    def consecutive_failures(self) -> int:
        with self._lock:
            return self._consecutive_failures

    def reset(self) -> None:
        """Operator override — force the breaker back to Closed
        and zero all counters."""
        with self._lock:
            self._state = _STATE_CLOSED
            self._consecutive_failures = 0
            self._consecutive_successes = 0
            self._opened_at = 0.0
            self._probe_in_flight = False

    def call(self, op: Callable[[], Any]) -> Any:
        """Wrap ``op``. Returns the result on success; raises
        :class:`BreakerOpenError` when state is Open within the
        cooldown window; re-raises ``op``'s exception on failure
        (after recording per the failure predicate).
        """
        admission = self._try_admit()
        if admission == "reject":
            raise BreakerOpenError()
        try:
            value = op()
        except BaseException as e:
            self._record_outcome(admission, ok=False, err=e)
            raise
        else:
            self._record_outcome(admission, ok=True, err=None)
            return value

    def _try_admit(self) -> str:
        with self._lock:
            if self._state == _STATE_CLOSED:
                return "closed"
            if self._state == _STATE_OPEN:
                elapsed_ms = (time.monotonic() - self._opened_at) * 1000.0
                if elapsed_ms >= self.reset_after_ms:
                    self._state = _STATE_HALF_OPEN
                    self._consecutive_successes = 0
                    self._probe_in_flight = True
                    return "half-open-probe"
                return "reject"
            # half-open
            if self._probe_in_flight:
                return "reject"
            self._probe_in_flight = True
            return "half-open-probe"

    def _record_outcome(
        self,
        admission: str,
        *,
        ok: bool,
        err: Optional[BaseException],
    ) -> None:
        with self._lock:
            if admission == "closed":
                if ok:
                    self._consecutive_failures = 0
                elif err is not None and self.failure_predicate(err):
                    self._consecutive_failures += 1
                    if self._consecutive_failures >= self.failure_threshold:
                        self._state = _STATE_OPEN
                        self._opened_at = time.monotonic()
                        self._consecutive_successes = 0
                # Predicate said "not a failure" → leave counters.
                return
            # half-open-probe
            self._probe_in_flight = False
            if ok:
                self._consecutive_successes += 1
                if self._consecutive_successes >= self.success_threshold:
                    self._state = _STATE_CLOSED
                    self._consecutive_failures = 0
                    self._consecutive_successes = 0
                    self._opened_at = 0.0
            elif err is not None and self.failure_predicate(err):
                # Failed probe → re-open with fresh cooldown.
                self._state = _STATE_OPEN
                self._opened_at = time.monotonic()
                self._consecutive_failures = 0
                self._consecutive_successes = 0


__all__ = [
    # Resilience classes + helpers
    "BreakerOpenError",
    "CircuitBreaker",
    "HedgePolicy",
    "RetryPolicy",
    "TypedMeshRpc",
    "TypedRpcStream",
    "classify_error",
    "default_breaker_failure",
    "default_retryable",
    "run_hedge",
    "run_retry",
    # Status code constants
    "NRPC_TYPED_BAD_REQUEST",
    "NRPC_TYPED_HANDLER_ERROR",
    # Error classes — re-exported so users who need both the
    # wrapper AND the typed exceptions can `from net.mesh_rpc
    # import RpcCodecError` from one place. These are the SAME
    # objects exposed under `net.RpcError` etc.
    "Cancellable",
    "RpcAppError",
    "RpcCancelledError",
    "RpcCapabilityDeniedError",
    "RpcCodecError",
    "RpcError",
    "RpcNoRouteError",
    "RpcServerError",
    "RpcTimeoutError",
    "RpcTransportError",
    "ServeHandle",
]
