"""Subnets — typed helpers for :class:`net_sdk.MeshNode`'s ``subnet`` and
``subnet_policy`` options.

The native constructor takes plain lists and dicts; these helpers build
exactly those shapes, with types, and validate a subnet id the same way
the native parser does (no looser, no stricter).

Example:
    >>> from net_sdk import MeshNode
    >>> from net_sdk.subnets import subnet_id, subnet_policy
    >>> node = MeshNode(
    ...     "127.0.0.1:0", psk,
    ...     subnet=subnet_id(3, 7),
    ...     subnet_policy=subnet_policy(
    ...         {"tag_prefix": "region:", "level": 0, "values": {"eu": 1, "us": 2}},
    ...     ),
    ... )

Not to be confused with the subnet *authority* plane
(``subnet_authorities`` / ``subnet_exports``), which lives in ``net.subnet``.
"""

from __future__ import annotations

from typing import Dict, List, TypedDict

SubnetId = List[int]
"""1–4 hierarchy levels, each in ``[0, 255]``. The form ``MeshNode(subnet=…)``
takes."""

GLOBAL_SUBNET: SubnetId = [0]
"""The global subnet (no restriction). The native default; ``[0]``
encodes to the core's ``SubnetId::GLOBAL``."""


class SubnetRule(TypedDict):
    """Derive one subnet level from a capability tag: a tag
    ``<tag_prefix><value>`` sets level ``level`` to ``values[value]``.
    ``level`` is 0–3; every mapped value is 1–255 (0 is reserved)."""

    tag_prefix: str
    level: int
    values: Dict[str, int]


class SubnetPolicy(TypedDict):
    """The ``MeshNode(subnet_policy=…)`` shape."""

    rules: List[SubnetRule]


def subnet_id(*levels: int) -> SubnetId:
    """A subnet id from 1–4 levels, each an int in ``[0, 255]``.

    Raises ``ValueError`` for exactly the inputs the native constructor
    rejects (it raises ``IdentityError`` for them).
    """
    if not 1 <= len(levels) <= 4:
        raise ValueError(f"subnet: levels must have 1-4 entries, got {len(levels)}")
    for i, value in enumerate(levels):
        if isinstance(value, bool) or not isinstance(value, int) or not 0 <= value <= 255:
            raise ValueError(f"subnet: level {i} value {value!r} must be an int in [0, 255]")
    return list(levels)


def subnet_policy(*rules: SubnetRule) -> SubnetPolicy:
    """A subnet policy from its rules. Validation happens in the native
    constructor."""
    return {"rules": list(rules)}


__all__ = [
    "GLOBAL_SUBNET",
    "SubnetId",
    "SubnetPolicy",
    "SubnetRule",
    "subnet_id",
    "subnet_policy",
]
