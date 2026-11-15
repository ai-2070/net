"""Stable, collision-resistant Hermes tool names for promoted capabilities.

Pinned (``pins.py``) and federated (``federate.py``) capabilities are surfaced
as first-class host tools whose names must be ``[a-z0-9_]`` while capability ids
are ``provider/tool`` over ``[a-z0-9._/-]`` (and the provider half is free-form
on the wire). Folding every unsafe character to ``_`` is lossy: valid
``42/foo-bar`` and ``42/foo_bar`` used to get the SAME name, so the host's
same-name replacement handed one capability's registration to the other, and
retiring either removed both (HERMES-04).

The rule here: an id the readable form encodes **losslessly** keeps exactly the
readable name it always had (the common case — MCP tool names are usually
``snake_case``); any other id gets a readable prefix plus a 64-bit digest of
the ORIGINAL id. Readable names never contain ``___`` and digested names always
do, so the two families cannot collide, and two digested names collide only
if their digests do. Every name is a pure function of the id, so it is stable
across polls, sessions and promotion order.
"""

from __future__ import annotations

import hashlib
import re
from typing import Optional

#: Upper bound on a promoted tool name (unchanged from the previous scheme).
MAX_TOOL_NAME_LEN = 110

_DIGEST_SEP = "___"
_DIGEST_HEX = 16  # 64-bit digest of the original id

#: One readable segment: alphanumeric runs joined by SINGLE underscores — no
#: leading, trailing or doubled ``_``, so ``__`` / ``___`` stay unambiguous.
_SEGMENT = r"[a-z0-9]+(?:_[a-z0-9]+)*"
_UNSAFE = re.compile(r"[^a-z0-9]+")


def _digest(cap_id: str) -> str:
    return hashlib.blake2b(
        cap_id.encode("utf-8"), digest_size=_DIGEST_HEX // 2, person=b"net-hermes-tool"
    ).hexdigest()


def tool_name(prefix: str, cap_id: str, readable: Optional[str], slash: str = "_") -> str:
    """The host tool name for ``cap_id`` under ``prefix``.

    ``readable`` is the caller's lossless readable body for ``cap_id``, or
    ``None`` when ``cap_id`` is outside the set the readable form encodes
    without loss. A readable body that would exceed the length cap also falls
    back to the digested form (truncation is lossy too). ``slash`` is what a
    ``/`` becomes in the digested form's (cosmetic) readable prefix.
    """
    if readable is not None:
        name = prefix + readable
        if len(name) <= MAX_TOOL_NAME_LEN and _DIGEST_SEP not in name[len(prefix) :]:
            return name
    room = MAX_TOOL_NAME_LEN - len(prefix) - len(_DIGEST_SEP) - _DIGEST_HEX
    segments = (_UNSAFE.sub("_", seg.lower()).strip("_") for seg in cap_id.split("/"))
    body = slash.join(segments)[:room].rstrip("_")
    return f"{prefix}{body}{_DIGEST_SEP}{_digest(cap_id)}"


#: ``provider/tool`` where the provider has no ``_`` (canonical providers are
#: decimal node ids) — so the first ``_`` after the prefix is the ``/``.
PINNED_READABLE = re.compile(rf"[a-z0-9]+/{_SEGMENT}")

#: One or more ``/``-separated segments; each ``/`` becomes ``__``.
FEDERATED_READABLE = re.compile(rf"{_SEGMENT}(?:/{_SEGMENT})+")
