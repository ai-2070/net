"""The organization capability-auth facade (``net_sdk.org``) — the pure-SDK
mirror of ``ORG_SCOPED_STREAMING_PLAN`` §4.4's Python verbs (Stage 4, Q6).

THIN FORWARDING ONLY. Every verb here passes through to the ``net`` wheel's
org surface (``net._net`` / ``net.org``, landed by the S4Python binding lane)
and returns the wheel's EXISTING handle classes — ``RpcStream`` /
``ClientStreamCall`` / ``DuplexCall`` and the ``Async*`` classes — unchanged.
There is no new stream wrapper and no protocol logic in this module: a
``net_sdk`` consumer calls and serves all four shapes without private
binding access (``MeshNode`` or the raw wheel mesh is accepted everywhere;
the wrapper's private handle is unwrapped HERE, never in application code).

Seam contracts — the wheel's, restated at every entry point:

* **Provider pinned per call.** Each call discovers its provider privately,
  issues ONE exact-target call, and pins that provider for the whole stream.
  The call is NEVER retried.
* **``deadline_ms == 0`` is the facade's 300 s default** (the Q1 default
  protected lifetime) — never "no deadline".
* **``cancel_token == 0`` means uncancellable.** Reserve a token with
  :meth:`OrgClient.reserve_cancel_token` before the call and fire it with
  :meth:`OrgClient.cancel`. The ``AsyncOrgClient`` deliberately takes no
  ``cancel_token`` parameter: the bridge mints and owns the token so asyncio
  task cancellation is the one cancellation story.
* **Midstream errors mirror ``org_err_to_py`` at this wrapper level**: any
  terminal or midstream failure on an org-opened handle surfaces as the
  ``org:`` wire vocabulary — the :class:`OrgError` family mirrored below —
  never the ``RpcError`` family. ``org:rpc:``-vocabulary errors surface under
  the base :class:`OrgError` exactly as the wheel's ``org_err_to_py``
  classifies them. :func:`parse_org_error` / :func:`classify_org_error` (the
  wheel's ``net.org`` vocabulary layer) are re-exported below.

The unary verbs are preserved through this facade both raw
(:meth:`OrgClient.call` / :func:`serve_org`) and over the wheel's typed
wrappers (:class:`TypedOrgClient` / :func:`serve_org_typed`, re-exported
below).

Teardown order for every shape: ``client.close()`` -> ``serve_handle.close()``
-> ``mesh.shutdown()``.
"""

from __future__ import annotations

import importlib
from typing import Any, Callable, Optional

# The wheel's native org surface (``net._net`` via ``net/__init__.py``).
from net import (  # type: ignore[attr-defined]
    AsyncOrgClient as _AsyncOrgClient,
    OrgAdmissionDeniedError,
    OrgClient as _OrgClient,
    OrgCredentials,
    OrgCredentialsError,
    OrgDiscoveryError,
    OrgError,
    OrgServeHandle,
    OrgUnclassifiedError,
    install_org_authority as _install_org_authority,
    install_provider_grant_audience as _install_provider_grant_audience,
    serve_org as _serve_org,
    serve_org_client_stream as _serve_org_client_stream,
    serve_org_duplex as _serve_org_duplex,
    serve_org_streaming as _serve_org_streaming,
)

HANDLER_DROP_CONTRACT = """\
**Handler-drop contract (Specification §2.2 — the F-S3.1-2 level).** A protected
call runs under a per-call retire supervisor. On retirement — caller CANCEL or
the caller handle's ``close()``/drop, the call deadline, revocation, session
replacement, ``serve_handle.close()`` against an in-flight call, or node
shutdown — the supervisor drops the handler future **without a final poll**.
Cancellation is observed ONLY through the retirement observables (the request
input fences to EOF where the shape has one; library-controlled sinks stop
admitting output) and NEVER as a handler-side event: a ``def`` handler's
blocking thread cannot be interrupted and runs to whatever point it reaches
(its return value is discarded, its performed effects are not recalled); an
``async def`` handler's coroutine MAY see ``asyncio.CancelledError`` at an
``await`` as best-effort teardown machinery, but it may equally never be
resumed to observe anything — never rely on it."""

_DIAGNOSTICS_LEVEL = """\
Diagnostics level: the ``RequestStreamRecv`` of the streaming org verbs
carries chunks + the retire signal only — its raw-transport diagnostic
getters (``caller_origin``, ``call_id``, ``deadline_ns``, ``headers``) are
unpopulated (0 / empty); attribution rides the verified ``caller`` dict."""


def _native_mesh(mesh: Any) -> Any:
    """The wheel-level mesh handle for ``mesh``.

    A :class:`net_sdk.mesh.MeshNode` is unwrapped to its private native
    handle HERE — this facade exists so application code never names
    ``_native`` — and anything else (the raw wheel mesh) passes through.
    """
    return getattr(mesh, "_native", mesh)


class OrgClient:
    """The sync caller half — thin forwarding over the wheel's ``OrgClient``.

    :meth:`call_streaming` / :meth:`call_client_stream` / :meth:`call_duplex`
    are §4.4's call trio over the frozen ``*_bytes_deadline`` seams; they
    return the wheel's existing handle classes unchanged (no new stream
    wrapper). :meth:`call` / :meth:`call_exported` preserve the unary verbs.

    Seam contracts (the wheel's, load-bearing here too): each call pins its
    provider for the whole stream and is never retried; ``deadline_ms == 0``
    is the facade's 300 s default, never "none"; ``cancel_token == 0`` means
    uncancellable (reserve with :meth:`reserve_cancel_token`, fire with
    :meth:`cancel`). Midstream errors classify through the ``org:`` vocabulary
    (the :class:`OrgError` family mirrored in this module — the
    ``org_err_to_py`` classification), never the ``RpcError`` family.

    Close it when done (context-manager supported): ``close()`` drops the
    audience lease and the node reference. Teardown order:
    ``client.close()`` -> ``serve_handle.close()`` -> ``mesh.shutdown()``.
    """

    __slots__ = ("raw",)

    def __init__(self, raw: Any) -> None:
        self.raw = raw

    @staticmethod
    def bind(mesh: Any, credentials: OrgCredentials) -> "OrgClient":
        """Bind a validated credential set to ``mesh`` (a ``MeshNode`` or the
        raw wheel mesh). Consumes ``credentials``."""
        return OrgClient(_OrgClient.bind(_native_mesh(mesh), credentials))

    def call(self, service: str, request: bytes) -> bytes:
        """The preserved unary call: one request in, one response out.

        Discovers privately, issues ONE exact-target call, never retries."""
        return self.raw.call(service, request)

    def call_exported(self, service: str, request: bytes) -> bytes:
        """The preserved unary call through a subnet export."""
        return self.raw.call_exported(service, request)

    def call_streaming(
        self,
        service: str,
        request: bytes,
        deadline_ms: int = 0,
        cancel_token: int = 0,
    ) -> Any:
        """Call a protected service whose response is a STREAM — one request
        in, byte chunks out, over the wheel's :class:`RpcStream` (§4.4). The
        provider is pinned for the whole stream and the call is never
        retried; dropping / ``close()``-ing the stream emits exactly one
        CANCEL. Midstream errors classify through the ``org:`` vocabulary
        (the :class:`OrgError` family), not the ``RpcError`` family.
        ``deadline_ms == 0`` is the facade's 300 s default, never "none";
        ``cancel_token == 0`` means uncancellable (see
        :meth:`reserve_cancel_token`)."""
        return self.raw.call_streaming(
            service, request, deadline_ms, cancel_token
        )

    def call_client_stream(
        self, service: str, deadline_ms: int = 0, cancel_token: int = 0
    ) -> Any:
        """Call a protected service with a STREAM OF REQUESTS and one terminal
        response, over the wheel's :class:`ClientStreamCall` (§4.4). ``send``
        pushes items (the signed opening rides the first), ``finish`` awaits
        the terminal response. Same provider-pinned / deadline / cancel-token
        / ``org:`` error semantics as :meth:`call_streaming`."""
        return self.raw.call_client_stream(service, deadline_ms, cancel_token)

    def call_duplex(
        self, service: str, deadline_ms: int = 0, cancel_token: int = 0
    ) -> Any:
        """Call a protected service BIDIRECTIONALLY, over the wheel's
        :class:`DuplexCall` (§4.4). Same provider-pinned / deadline /
        cancel-token / ``org:`` error semantics as :meth:`call_streaming`."""
        return self.raw.call_duplex(service, deadline_ms, cancel_token)

    def reserve_cancel_token(self) -> int:
        """Reserve a cancel token for a subsequent streaming call — reserve
        BEFORE the call, pass it as ``cancel_token=...``, fire it with
        :meth:`cancel`."""
        return self.raw.reserve_cancel_token()

    def cancel(self, token: int) -> None:
        """Cancel the one in-flight call bound to ``token``. Idempotent; a
        no-op for ``0`` or an unused token. Never retries the call."""
        return self.raw.cancel(token)

    @property
    def acting_org(self) -> bytes:
        return self.raw.acting_org

    @property
    def caller(self) -> bytes:
        return self.raw.caller

    @property
    def is_closed(self) -> bool:
        return self.raw.is_closed

    def close(self) -> None:
        """Drop the audience lease and the node reference. Idempotent."""
        self.raw.close()

    def __enter__(self) -> "OrgClient":
        return self

    def __exit__(self, *exc: object) -> bool:
        self.close()
        return False


class AsyncOrgClient:
    """The async caller half (§4.4): the async forms of the call trio,
    awaiting into the wheel's existing ``Async*`` handle classes unchanged
    (no new stream wrapper).

    Same authority semantics as :class:`OrgClient` — one plan per call, the
    provider pinned, never retried, the facade's finite 300 s default lifetime
    when ``deadline_ms == 0``, and midstream errors through the ``org:``
    vocabulary (the :class:`OrgError` family). Cancellation is the asyncio
    story, at its exact levels: ``task.cancel()`` (or ``asyncio.wait_for``
    expiry) on any await of the construction or of the returned handle fires
    the substrate's cancel token — the per-stream cancel watcher tears the
    call down locally at once (the response side EOFs while the handle lives)
    — and the WIRE CANCEL that retires the provider-side call rides the
    handle's ``close()``/drop (the per-shape Drop contract). Cancellation is
    never a handler-side event (see :data:`HANDLER_DROP_CONTRACT`).
    Deliberately no ``cancel_token`` parameter: the bridge mints and owns the
    token. Teardown order: ``client.close()`` -> ``serve_handle.close()`` ->
    ``mesh.shutdown()``.
    """

    __slots__ = ("raw",)

    def __init__(self, raw: Any) -> None:
        self.raw = raw

    @staticmethod
    def bind(mesh: Any, credentials: OrgCredentials) -> "AsyncOrgClient":
        """Bind a validated credential set to ``mesh`` (a ``MeshNode`` or the
        raw wheel mesh). Consumes ``credentials``."""
        return AsyncOrgClient(_AsyncOrgClient.bind(_native_mesh(mesh), credentials))

    async def call_streaming(
        self, service: str, request: bytes, deadline_ms: int = 0
    ) -> Any:
        """Await to open a protected streaming-response call over the wheel's
        ``AsyncRpcStream``. Provider pinned per call; ``deadline_ms == 0`` is
        the facade's 300 s default; errors through the ``org:`` vocabulary."""
        return await self.raw.call_streaming(service, request, deadline_ms)

    async def call_client_stream(
        self, service: str, deadline_ms: int = 0
    ) -> Any:
        """Await to open a protected client-streaming call over the wheel's
        ``AsyncClientStreamCall``. Same seam contracts as
        :meth:`call_streaming`."""
        return await self.raw.call_client_stream(service, deadline_ms)

    async def call_duplex(self, service: str, deadline_ms: int = 0) -> Any:
        """Await to open a protected duplex call over the wheel's
        ``AsyncDuplexCall``. Same seam contracts as :meth:`call_streaming`."""
        return await self.raw.call_duplex(service, deadline_ms)

    @property
    def acting_org(self) -> bytes:
        return self.raw.acting_org

    @property
    def caller(self) -> bytes:
        return self.raw.caller

    @property
    def is_closed(self) -> bool:
        return self.raw.is_closed

    def close(self) -> None:
        """Drop the audience lease and the node reference. Idempotent."""
        self.raw.close()

    def __enter__(self) -> "AsyncOrgClient":
        return self

    def __exit__(self, *exc: object) -> bool:
        self.close()
        return False


def install_org_authority(mesh: Any, authority_dir: str) -> None:
    """Install an adopted node authority from the directory ``net node adopt``
    wrote (``mesh`` is a ``MeshNode`` or the raw wheel mesh). Required before
    :meth:`OrgClient.bind` or a ``"granted"`` :func:`serve_org`."""
    _install_org_authority(_native_mesh(mesh), authority_dir)


def install_provider_grant_audience(
    mesh: Any, grant: bytes, audience_secret_path: str
) -> None:
    """Install the provider half of a grant: its signed grant bytes and the
    path to its audience secret (secrets cross as PATHS, never bytes)."""
    _install_provider_grant_audience(
        _native_mesh(mesh), grant, audience_secret_path
    )


def serve_org(
    mesh: Any,
    service: str,
    access: str,
    handler: Callable[[dict, bytes], bytes],
    handler_timeout_ms: Optional[int] = None,
) -> OrgServeHandle:
    """Serve a protected, privately-discoverable unary service (the preserved
    unary serve verb). ``access`` is ``"same_org"`` or ``"granted"``. The
    handler is ``handler(caller: dict, request: bytes) -> bytes`` and must be
    callable (checked at registration). ``caller`` carries the five verified
    fields plus ``is_same_org``. Raising surfaces as an application error,
    never as an admission denial.

    ``handler_timeout_ms`` bounds how long the provider WAITS for the
    handler's reply before returning an internal error to the caller; ``0``
    means effectively infinite (no bound). The handler runs on a blocking
    thread and is not interrupted when the wait elapses."""
    return _serve_org(
        _native_mesh(mesh), service, access, handler, handler_timeout_ms
    )


def serve_org_streaming(
    mesh: Any,
    service: str,
    access: str,
    handler: Callable[[dict, bytes, Any], Any],
    handler_timeout_ms: Optional[int] = None,
) -> OrgServeHandle:
    """Serve a protected, privately-discoverable service whose response is a
    STREAM (§4.4) — thin forwarding over the wheel's ``serve_org_streaming``.
    ``access`` is ``"same_org"`` or ``"granted"``. The handler is
    ``handler(caller: dict, request: bytes, sink: ResponseSinkSend) -> None``
    (a ``def`` or an ``async def``): ``caller`` carries the five verified
    fields plus ``is_same_org``, chunks go out through ``sink.send(bytes)``,
    and the substrate emits the terminal frame at handler return. Raising
    surfaces as an application error, never as an admission denial. A
    non-callable handler is refused at registration.

    ``handler_timeout_ms`` is the bounded wait (``0`` = effectively infinite);
    the handler is not interrupted when it elapses.

    **Handler-drop contract (Specification §2.2 — the F-S3.1-2 level).** A protected
    call runs under a per-call retire supervisor. On retirement — caller CANCEL or
    the caller handle's ``close()``/drop, the call deadline, revocation, session
    replacement, ``serve_handle.close()`` against an in-flight call, or node
    shutdown — the supervisor drops the handler future **without a final poll**.
    Cancellation is observed ONLY through the retirement observables (the request
    input fences to EOF where the shape has one; library-controlled sinks stop
    admitting output) and NEVER as a handler-side event: a ``def`` handler's
    blocking thread cannot be interrupted and runs to whatever point it reaches
    (its return value is discarded, its performed effects are not recalled); an
    ``async def`` handler's coroutine MAY see ``asyncio.CancelledError`` at an
    ``await`` as best-effort teardown machinery, but it may equally never be
    resumed to observe anything — never rely on it.

    Diagnostics level: the ``RequestStreamRecv`` of the streaming org verbs
    carries chunks + the retire signal only — its raw-transport diagnostic
    getters (``caller_origin``, ``call_id``, ``deadline_ns``, ``headers``) are
    unpopulated (0 / empty); attribution rides the verified ``caller`` dict.
    """
    return _serve_org_streaming(
        _native_mesh(mesh), service, access, handler, handler_timeout_ms
    )


def serve_org_client_stream(
    mesh: Any,
    service: str,
    access: str,
    handler: Callable[[dict, Any], bytes],
    handler_timeout_ms: Optional[int] = None,
) -> OrgServeHandle:
    """Serve a protected, privately-discoverable service with a STREAM OF
    REQUESTS and one terminal response (§4.4) — thin forwarding over the
    wheel's ``serve_org_client_stream``. The handler is ``handler(caller:
    dict, stream: RequestStreamRecv) -> bytes`` (a ``def`` or an ``async
    def``): iterate ``stream`` to drain the upload — it fences to EOF on
    retirement — and return the terminal response as ``bytes``. Same
    registration / timeout / handler-drop contract and diagnostics level as
    :func:`serve_org_streaming` (see :data:`HANDLER_DROP_CONTRACT`)."""
    return _serve_org_client_stream(
        _native_mesh(mesh), service, access, handler, handler_timeout_ms
    )


def serve_org_duplex(
    mesh: Any,
    service: str,
    access: str,
    handler: Callable[[dict, Any, Any], Any],
    handler_timeout_ms: Optional[int] = None,
) -> OrgServeHandle:
    """Serve a protected, privately-discoverable service BIDIRECTIONALLY
    (§4.4) — thin forwarding over the wheel's ``serve_org_duplex``. The
    handler is ``handler(caller: dict, stream: RequestStreamRecv, sink:
    ResponseSinkSend) -> None`` (a ``def`` or an ``async def``): drain
    ``stream``, emit through ``sink.send(bytes)``; the substrate emits the
    terminal frame at handler return. Same registration / timeout /
    handler-drop contract and diagnostics level as
    :func:`serve_org_streaming` (see :data:`HANDLER_DROP_CONTRACT`)."""
    return _serve_org_duplex(
        _native_mesh(mesh), service, access, handler, handler_timeout_ms
    )


# The wheel's ``net.org`` typed unary wrappers, passed through with the same
# ``MeshNode``-or-raw-mesh entry contract as every other facade verb (the
# wheel's own ``bind``/``serve_org_typed`` name the native mesh directly).
class TypedOrgClient:
    """JSON-typed unary caller — pass-through over the wheel's
    ``net.org.TypedOrgClient`` (the typed wrapper). The codec is JSON,
    hard-coded, matching every other typed layer in the SDK. Use
    ``client.raw.call`` for bytes if you marshal yourself.

    ``bind`` accepts a ``MeshNode`` or the raw wheel mesh, like every other
    facade entry. Supports the context-manager protocol; ``close()``
    releases the audience lease and the node reference."""

    __slots__ = ("raw",)

    def __init__(self, raw: Any) -> None:
        self.raw = raw

    @staticmethod
    def bind(mesh: Any, credentials: OrgCredentials) -> "TypedOrgClient":
        """Bind a validated credential set to ``mesh``. Consumes
        ``credentials``."""
        typed = importlib.import_module("net.org").TypedOrgClient
        return TypedOrgClient(typed.bind(_native_mesh(mesh), credentials))

    def call(self, service: str, request: Any) -> Any:
        """Call a protected service. Discovers privately, issues ONE
        exact-target call, never retries. Provider pinned per call."""
        return self.raw.call(service, request)

    @property
    def acting_org(self) -> bytes:
        return self.raw.acting_org

    @property
    def caller(self) -> bytes:
        return self.raw.caller

    @property
    def is_closed(self) -> bool:
        return self.raw.is_closed

    def close(self) -> None:
        """Drop the audience lease and the node reference. Idempotent."""
        self.raw.close()

    def __enter__(self) -> "TypedOrgClient":
        return self

    def __exit__(self, *exc: object) -> bool:
        self.close()
        return False


def serve_org_typed(
    mesh: Any,
    service: str,
    access: str,
    handler: Callable[[dict, Any], Any],
    handler_timeout_ms: Optional[int] = None,
) -> OrgServeHandle:
    """Serve a protected unary service with a JSON codec — pass-through over
    the wheel's ``net.org.serve_org_typed`` (the typed wrapper). ``access``
    is ``"same_org"`` or ``"granted"``. The handler is ``handler(caller:
    dict, request) -> response``; ``caller`` carries the five verified fields
    plus ``is_same_org``. Raising surfaces as an application error, never as
    an admission denial. Same registration / timeout contract as
    :func:`serve_org`."""
    serve_typed = importlib.import_module("net.org").serve_org_typed
    return serve_typed(
        _native_mesh(mesh), service, access, handler, handler_timeout_ms
    )


# The wheel's ``net.org`` ``org:`` wire-vocabulary parser/classifier (the
# ``org_err_to_py`` mirror). Resolved lazily because ``net.org`` is a
# submodule of the native package — importing it at module load would break
# this package's import against the sdk-py test-suite's ``net`` stub (which
# is not a package). First access resolves and caches the real name.
_NET_ORG_NAMES = (
    "parse_org_error",
    "classify_org_error",
    "ParsedOrgError",
)


def __getattr__(name: str) -> Any:
    if name in _NET_ORG_NAMES:
        value = getattr(importlib.import_module("net.org"), name)
        globals()[name] = value
        return value
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")


__all__ = [
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
    # The org_err_to_py mirror at the wrapper level: the ``org:`` wire
    # vocabulary (mirrored typed family + the parser/classifier).
    "OrgError",
    "OrgCredentialsError",
    "OrgDiscoveryError",
    "OrgAdmissionDeniedError",
    "OrgUnclassifiedError",
    "parse_org_error",
    "classify_org_error",
    "ParsedOrgError",
    # The wheel's typed unary wrappers (pass-through).
    "TypedOrgClient",
    "serve_org_typed",
]
