"""Payments and paid agent-to-agent tasks over the SDK's :class:`MeshNode`.

``docs/internal/plans/NODE_A2A_PAID_ADMISSION_PLAN.md`` WS-G. The native
``PaymentProvider`` and ``CapabilityGateway`` take a native ``net.NetMesh``;
handing them a :class:`net_sdk.MeshNode` is a ``TypeError``, which left a
``net_sdk`` program reaching for the private ``node._native`` or importing the
raw ``net`` wheel. This module **adapts handles and implements nothing**: each
factory resolves the mesh through :func:`net_sdk.mesh._native_mesh` (a
``MeshNode``, an ``AsyncMeshNode`` or a raw ``NetMesh``), passes every keyword
through untouched, and returns the **native** object. A keyword the native
constructor gains later works with no edit here, and the native constructor
stays the single source of truth for validation.

**Paid A2A is sync-gateway-only**, by the native contract: the native
``AsyncCapabilityGateway`` refuses ``a2a_purchase_path`` and has no paid
verbs. From asyncio, build the sync gateway with
:func:`create_capability_gateway` (an ``AsyncMeshNode`` is accepted) and drive
it with ``await asyncio.to_thread(gateway.prepare_task, ...)``.
:func:`create_async_capability_gateway` adapts the handle for the async
``search`` / ``describe`` / ``invoke`` surface only.

Python ints are arbitrary precision, so ``json.loads`` / ``json.dumps`` keep
the u64 fields of these documents exact — unlike JavaScript, no special
reader is needed: ``json.dumps(env["prepared"])`` is the handoff.

Example::

    from net_sdk import MeshNode
    from net_sdk.payments import create_capability_gateway, create_payment_provider

    provider = create_payment_provider(node, "state/engine.json",
                                       facilitator_url="https://facilitator.example.com")
    gateway = create_capability_gateway(caller,
                                        payment_policy_path="state/spend-policy.json",
                                        payment_profile="production",
                                        a2a_purchase_path="state/a2a-purchases.json")
    env = json.loads(gateway.prepare_task(provider_node_id, "summarize", prompt))
    prepared = json.dumps(env["prepared"])
    gateway.purchase_task(prepared)
    gateway.submit_task(prepared)
"""

from __future__ import annotations

from typing import Any, Optional

from net_sdk.mesh import _native_mesh

__all__ = [
    "create_async_capability_gateway",
    "create_capability_gateway",
    "create_payment_provider",
    "set_a2a_org_caller",
]

# The native classes and refusals, re-exported so a paid-A2A program never
# imports `net`. Each guard is its own: a wheel built without `payments` /
# `a2a` still imports `net_sdk`.
try:
    from net import PaymentProvider
except ImportError:  # pragma: no cover - build without payments
    pass
else:
    __all__.append("PaymentProvider")

try:
    from net import PaymentRefused
except ImportError:  # pragma: no cover - build without a2a
    pass
else:
    __all__.append("PaymentRefused")

try:
    from net import JournalOwnedElsewhere
except ImportError:  # pragma: no cover - build without a2a + payments
    pass
else:
    __all__.append("JournalOwnedElsewhere")


def create_payment_provider(mesh: Any, state_path: str, **kwargs: Any) -> Any:
    """A native ``PaymentProvider`` over ``mesh`` — a :class:`MeshNode`, an
    :class:`AsyncMeshNode` or a raw ``NetMesh``. ``kwargs`` go to the native
    constructor unchanged (``billing_log_path``, ``facilitator_url``,
    ``unsafe_dev_mock_facilitator``, ...); it raises exactly what that
    constructor raises."""
    from net import PaymentProvider as _PaymentProvider

    return _PaymentProvider(_native_mesh(mesh), state_path, **kwargs)


def create_capability_gateway(mesh: Any, **kwargs: Any) -> Any:
    """A native sync ``CapabilityGateway`` over ``mesh`` — the paid-A2A
    surface (``prepare_task`` / ``purchase_task`` / ``submit_task`` /
    ``a2a_attempts`` / ``a2a_resolve_attempt``). ``kwargs`` go to the native
    constructor unchanged (``payment_policy_path``, ``payment_profile``,
    ``a2a_purchase_path``, ...)."""
    from net import CapabilityGateway as _CapabilityGateway

    return _CapabilityGateway(_native_mesh(mesh), **kwargs)


def create_async_capability_gateway(mesh: Any, **kwargs: Any) -> Any:
    """A native ``AsyncCapabilityGateway`` over ``mesh``, for the awaitable
    ``search`` / ``describe`` / ``invoke`` surface **only**. It is not the
    async twin of paid A2A: the native class refuses ``a2a_purchase_path``
    and has no paid verbs — use :func:`create_capability_gateway` and
    ``asyncio.to_thread`` for those."""
    from net import AsyncCapabilityGateway as _AsyncCapabilityGateway

    return _AsyncCapabilityGateway(_native_mesh(mesh), **kwargs)


def _native_org_client(org: Any) -> Optional[Any]:
    """The native ``net.OrgClient`` behind ``org``: the SDK's
    :class:`net_sdk.org.OrgClient` (``.raw``), the native client itself, or
    ``None``. The async client is refused by name — the A2A slot takes the
    sync client."""
    if org is None:
        return None
    from net_sdk import org as _org

    if isinstance(org, _org.AsyncOrgClient):
        raise TypeError(
            "set_a2a_org_caller takes the sync OrgClient (net_sdk.org.OrgClient "
            "or net.OrgClient), not AsyncOrgClient — bind a sync client with the "
            "same credentials for the A2A slot"
        )
    if isinstance(org, _org.OrgClient):
        return org.raw
    return org


def set_a2a_org_caller(target: Any, org: Any) -> None:
    """Install (or clear with ``None``) the organization identity paid A2A
    presents to a PROTECTED provider.

    ``target`` picks the slot: a :class:`MeshNode`, :class:`AsyncMeshNode` or
    raw ``NetMesh`` sets the mesh's own (the raw requester verbs —
    ``describe_a2a``, ``submit_task``, ``submit_task_paid``, ``task_status``,
    ``cancel_task``); a sync ``CapabilityGateway`` sets the gateway's (its
    ``prepare_task`` → ``purchase_task`` → ``submit_task`` lifecycle). Setting
    one does not set the other. An ``AsyncCapabilityGateway`` is refused: it
    has no paid-A2A lifecycle to present an identity to.
    """
    # Each class guarded on its own: a wheel built without `payments` / `mcp`
    # has no gateway at all, and the mesh path below must still work there.
    try:
        from net import CapabilityGateway as _CapabilityGateway
    except ImportError:  # pragma: no cover - build without payments
        _CapabilityGateway = None

    native_org = _native_org_client(org)
    if _CapabilityGateway is not None and isinstance(target, _CapabilityGateway):
        target.set_a2a_org_caller(native_org)
        return
    try:
        from net import AsyncCapabilityGateway as _AsyncCapabilityGateway
    except ImportError:  # pragma: no cover - minimal build
        _AsyncCapabilityGateway = None
    if _AsyncCapabilityGateway is not None and isinstance(target, _AsyncCapabilityGateway):
        raise TypeError(
            "set_a2a_org_caller: AsyncCapabilityGateway has no paid-A2A lifecycle "
            "(the native class refuses a2a_purchase_path); the paid surface is the "
            "sync CapabilityGateway from create_capability_gateway"
        )
    _native_mesh(target).set_a2a_org_caller(native_org)
