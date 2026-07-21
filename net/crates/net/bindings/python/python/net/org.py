"""Organization capability auth for Python (OSDK-L Workstream P).

The native classes (``OrgCredentials``, ``OrgClient``, ``serve_org``, and the
``OrgError`` family) live in the ``net._net`` extension. This module adds the
pure-Python pieces the native layer cannot: parsing the ``org:`` wire
vocabulary and the typed convenience wrappers, mirroring how ``mesh_rpc.py``
sits over the raw nRPC surface.

The wire vocabulary is single-sourced in Rust (``OrgSdkError::to_wire``) and
pinned by ``tests/cross_lang_org/error_vectors.json``; the classifier here must
recover exactly what that fixture declares.
"""

from __future__ import annotations

import json
from typing import Any, Callable, Optional

_ERR_ORG_PREFIX = "org:"

# The domains whose refusal means the request never left this process.
_LOCAL_DOMAINS = frozenset({"credentials", "discovery"})


class ParsedOrgError:
    """The domain and kind recovered from an ``org:`` wire string.

    ``domain`` is the load-bearing fact — it says WHERE the refusal happened.
    Use :attr:`is_local` rather than re-parsing the message. ``kind`` is the
    finer token within the domain, ``None`` when it could not be parsed.
    """

    __slots__ = ("domain", "kind", "message")

    def __init__(self, domain: str, kind: Optional[str], message: str) -> None:
        self.domain = domain
        self.kind = kind
        self.message = message

    @property
    def is_local(self) -> bool:
        """Whether nothing was sent — True only for credential and discovery
        failures. ``unknown`` is NOT local: it claims nothing either way."""
        return self.domain in _LOCAL_DOMAINS

    def __repr__(self) -> str:
        return f"ParsedOrgError(domain={self.domain!r}, kind={self.kind!r})"


def parse_org_error(exc_or_message: Any) -> ParsedOrgError:
    """Parse the ``org:`` vocabulary out of an exception or message string.

    Mirrors Rust's ``parse_org_wire``. Anything that is not an ``org:`` string
    with a domain this build knows classifies as ``unknown`` with no kind —
    deliberately, because reporting a canonical domain for a string we could
    not parse would assert a refusal location we cannot establish. In
    particular, reporting ``admission_denied`` would falsely claim a request
    reached a provider and its admission engine evaluated it.
    """
    message = exc_or_message if isinstance(exc_or_message, str) else str(exc_or_message)

    if not message.startswith(_ERR_ORG_PREFIX):
        return ParsedOrgError("unknown", None, message)

    rest = message[len(_ERR_ORG_PREFIX) :]
    first = rest.find(":")
    if first <= 0:
        return ParsedOrgError("unknown", None, message)

    domain = rest[:first]
    after = rest[first + 1 :]
    second = after.find(":")
    kind = after if second == -1 else after[:second]
    if not kind:
        return ParsedOrgError("unknown", None, message)

    # A literal `unknown` domain is a fallback classification, never something a
    # peer asserts, so it recovers no kind.
    if domain in ("credentials", "discovery", "admission_denied", "rpc"):
        return ParsedOrgError(domain, kind, message)
    return ParsedOrgError("unknown", None, message)


def _encode(value: Any) -> bytes:
    return json.dumps(value, separators=(",", ":")).encode("utf-8")


def _decode(data: bytes) -> Any:
    return json.loads(data.decode("utf-8"))


class TypedOrgClient:
    """JSON-typed wrapper over the native ``OrgClient``.

    The codec is JSON, hard-coded, matching every other typed layer in the SDK.
    Use ``client.raw.call`` for bytes if you marshal yourself. Supports the
    context-manager protocol; ``close()`` releases the audience lease and the
    node reference.
    """

    __slots__ = ("raw",)

    def __init__(self, raw: Any) -> None:
        self.raw = raw

    @staticmethod
    def bind(mesh: Any, credentials: Any) -> "TypedOrgClient":
        """Bind credentials to a mesh. Consumes ``credentials``."""
        from ._net import OrgClient  # local import: native module

        return TypedOrgClient(OrgClient.bind(mesh, credentials))

    def call(self, service: str, request: Any) -> Any:
        """Call a protected service. Discovers privately, issues ONE
        exact-target call, never retries."""
        return _decode(self.raw.call(service, _encode(request)))

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
) -> Any:
    """Serve a protected service with a JSON codec.

    ``access`` is ``"same_org"`` or ``"granted"``. The handler is
    ``handler(caller: dict, request) -> response``; ``caller`` carries the five
    verified fields (``entity``, ``acting_org``, ``provider_org``, ``provider``,
    ``capability``) plus ``is_same_org``. Raising surfaces as an application
    error, never as an admission denial.
    """
    from ._net import serve_org  # local import: native module

    def _wrapped(caller: dict, request_bytes: bytes) -> bytes:
        return _encode(handler(caller, _decode(request_bytes)))

    return serve_org(mesh, service, access, _wrapped, handler_timeout_ms)
