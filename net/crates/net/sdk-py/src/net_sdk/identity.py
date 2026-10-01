"""Identity and permission tokens.

Re-exports the wheel's identity surface so callers import from
``net_sdk``, and adds the typed :data:`TokenScope` vocabulary the
TypeScript SDK ships. Tokens cross the Python boundary as ``bytes``
(serialized ``PermissionToken``s); there is no token class.

Example:
    >>> from net_sdk.identity import Identity, channel_hash
    >>> me = Identity.generate()
    >>> token = me.issue_token(peer_entity_id, ["subscribe"], "sensors/temp", 3600)
    >>> node.subscribe_channel(publisher_id, "sensors/temp", token)

Present iff the wheel was built with the ``net`` feature (every build is).
"""

from __future__ import annotations

from typing import Literal

from net import (  # type: ignore[attr-defined]
    Identity,
    IdentityError,
    TokenError,
    channel_hash,
    delegate_token,
    normalize_gpu_vendor,
    parse_token,
    stream_id_from_label,
    token_is_expired,
    verify_signature,
    verify_token,
)

TokenScope = Literal["publish", "subscribe", "admin", "delegate", "wildcard"]
"""A permission a token grants. Exactly the strings the wheel's
``parse_scope`` accepts; anything else raises ``IdentityError``.
``"wildcard"`` authorizes the token's actions on every channel,
regardless of its channel hash."""

__all__ = [
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
]
