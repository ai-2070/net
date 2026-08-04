"""Subnet authority for Python (SSDK S4b, ``SUBNET_AUTH_SDK_PLAN.md`` §6.2).

The native pieces (``serve_subnet_exported``, ``install_subnet_gateway_credentials``,
``declare_subnet_boundaries``, ``apply_subnet_control_fact``, and the
``SubnetProvisionError`` class) live in the ``net._net`` extension. This module
adds the pure-Python parts: the ``subnet:`` wire classifier, the ``admin``
namespace, and the JSON-typed provider wrapper — mirroring how ``org.py`` sits
over the native org surface, and the Node binding's ``subnet.ts`` layer.

Two layers, deliberately separated:

- **Ordinary application** — the provider verb ``serve_subnet_exported_typed``
  (plus the caller verb ``OrgClient.call_exported`` on the org client). The
  provider names a service and a locally configured export; it constructs no
  roots, credentials, boundaries, epochs, or refs.
- **Administration** under :data:`admin` — installing gateway credential-set
  bytes (wholesale replace), declaring boundaries (also wholesale), and applying
  signed control facts (the one door, floors included). Signed artifacts are
  minted by ``net-mesh subnet …`` and cross as opaque canonical wire ``bytes``.

The ``subnet:`` kind vocabulary is single-sourced in Rust and pinned by
``tests/cross_lang_subnet/stable_kinds.json``.
"""

from __future__ import annotations

import json
from typing import Any, Callable, Optional

_ERR_SUBNET_PREFIX = "subnet:"


def parse_subnet_kind(exc_or_message: Any) -> Optional[str]:
    """Recover the stable kind token from a ``subnet:`` wire string.

    Returns the kind (the token after the ``subnet:`` prefix, up to the next
    colon) or ``None`` when the message is not a ``subnet:`` envelope. An
    unrecognized kind still returns verbatim — the kind is data, and inventing a
    substitute for one this build does not know would be the counterfeit the org
    taxonomy's ``unknown`` rule exists to prevent.
    """
    message = exc_or_message if isinstance(exc_or_message, str) else str(exc_or_message)
    if not message.startswith(_ERR_SUBNET_PREFIX):
        return None
    rest = message[len(_ERR_SUBNET_PREFIX) :]
    colon = rest.find(":")
    kind = rest if colon == -1 else rest[:colon]
    kind = kind.strip()
    return kind or None


def classify_subnet_error(exc_or_message: Any) -> Any:
    """Return a :class:`SubnetProvisionError` for a ``subnet:`` wire string.

    Returns the input unchanged when it is not a ``subnet:`` envelope, so it can
    wrap a broad ``except`` without swallowing unrelated errors. A live native
    call already raises ``SubnetProvisionError`` directly; this is for
    classifying a message you caught yourself.
    """
    from ._net import SubnetProvisionError  # local import: native module

    kind = parse_subnet_kind(exc_or_message)
    if kind is None:
        return exc_or_message
    message = (
        exc_or_message if isinstance(exc_or_message, str) else str(exc_or_message)
    )
    err = SubnetProvisionError(message)
    # Expose the parsed kind as an attribute, matching the Node binding's
    # `SubnetProvisionError.kind`. The native class carries only the message.
    err.kind = kind
    return err


def _encode(value: Any) -> bytes:
    return json.dumps(value, separators=(",", ":")).encode("utf-8")


def _decode(data: bytes) -> Any:
    return json.loads(data.decode("utf-8"))


class _Admin:
    """Runtime subnet administration — the operator surface, deliberately not
    beside ordinary service calls. The ordinary application never uses these."""

    __slots__ = ()

    def install_gateway_credentials(self, mesh: Any, credential_sets: "list[bytes]") -> None:
        """Decode and install this node's own gateway credential sets —
        WHOLESALE REPLACE: pass every currently held set, not a delta. Every
        artifact decodes before anything installs, so one malformed ``bytes`` in
        the batch mutates no node state at all."""
        from ._net import install_subnet_gateway_credentials

        install_subnet_gateway_credentials(mesh, list(credential_sets))

    def declare_boundaries(self, mesh: Any, declaration: dict) -> None:
        """Declare this node's protected boundary inventory — also wholesale.
        ``declaration`` is
        ``{"authority_hex": str, "topology_epoch": int, "boundaries": [[int]]}``."""
        from ._net import declare_subnet_boundaries

        declare_subnet_boundaries(mesh, declaration)

    def apply_control_fact(self, mesh: Any, fact: bytes) -> dict:
        """Apply one signed control fact from its outer wire frame — the ONE
        door for floors and descriptive facts alike. Returns
        ``{"kind": str, "applied": bool}``; ``applied=False`` is an authenticated
        stale/idempotent outcome, not a failure."""
        from ._net import apply_subnet_control_fact

        return apply_subnet_control_fact(mesh, fact)


#: The advanced administration surface (``net.subnet.admin.*``).
admin = _Admin()


def serve_subnet_exported_typed(
    mesh: Any,
    service: str,
    export_name: str,
    handler: Callable[[dict, Any], Any],
    handler_timeout_ms: Optional[int] = None,
) -> Any:
    """Serve a subnet-exported service against a NAMED export with a JSON codec.

    ``export_name`` is a provider-local label configured at mesh construction; an
    unknown name raises :class:`SubnetProvisionError` (kind ``unknown_export_name``)
    before anything is registered or announced. The handler is
    ``handler(caller: dict, request) -> response``; ``caller`` carries the same
    verified fields as ``serve_org``. Announcement visibility is always public,
    and the external caller never joins this node's subnet.
    """
    from ._net import serve_subnet_exported  # local import: native module

    def _wrapped(caller: dict, request_bytes: bytes) -> bytes:
        return _encode(handler(caller, _decode(request_bytes)))

    return serve_subnet_exported(mesh, service, export_name, _wrapped, handler_timeout_ms)
