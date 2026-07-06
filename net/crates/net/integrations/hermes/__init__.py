"""``net`` — Net mesh integration for Hermes (``HERMES_INTEGRATION_PLAN.md``
Phase 1).

A first-party, standalone plugin that lets a Hermes agent reach capabilities
running on the user's *other* machines — the ones published there with
``net wrap`` — as ``net_*`` tools, with local consent + pin approval. It
embeds a first-class Net node in-process via ``net-mesh-sdk`` (no daemon, no MCP
shim); the node joins the mesh, and the tools drive the SDK's consent-gated
``AsyncCapabilityGateway`` and the machine-shared pin store.

Doctrine: the plugin sees only neutral capability shapes — no MCP awareness
(H-anti-goals). All consent / validation / pin logic lives once in the Rust SDK
(H2); this plugin is a thin, public-API-only view over it (H6).

Enable it by adding ``net`` to ``plugins.enabled`` in your Hermes config, then
set ``NET_MESH_PSK`` (and ``NET_MESH_PEERS``) to join your mesh — see
``node.py`` for the environment configuration.
"""

from __future__ import annotations

import logging
import os
from typing import Optional

from . import a2a, delegation, federate, node, pins, provider, renewal
from .tools import TOOLS

# `delegation` / `renewal` / `provider` / `federate` / `a2a` are imported for
# package-attribute access (tests, and the node's lazy imports); the node owns
# their lifecycle.
_ = delegation
_ = renewal
_ = provider
_ = federate
_ = a2a

logger = logging.getLogger(__name__)

# The running pin-promotion subscription (Phase 2), if started. Held so the
# session-end hook can stop it.
_promotion: Optional["pins.PinPromotionService"] = None

# The running local-tool publication (V2 Phase 2, provider side), if started —
# this Hermes's OWN tools announced to the mesh. Opt-in; held so the session-end
# hook can withdraw it.
_provider: Optional["provider.LocalToolProvider"] = None

# The running tool-federation service (V2 Phase 2, consumer side), if started —
# surfaces discovered mesh capabilities as machine-namespaced first-class tools.
# Opt-in; held so the session-end hook can stop it.
_federation: Optional["federate.FederationService"] = None

# The running A2A executor service (V2 Phase 3), if started — accepts
# hand-off tasks from sibling in-root agents and runs them through the host
# agent loop. Opt-in; held so the session-end hook can stop it.
_a2a_service: Optional["a2a.A2aService"] = None


def _env_flag(name: str) -> bool:
    return (os.environ.get(name) or "").strip().lower() in ("1", "true", "yes", "on")


def _publish_local_tools_enabled() -> bool:
    """Whether to announce this Hermes's OWN local tools to the mesh. Opt-in —
    exposing a machine's toolset to the operator mesh must be deliberate, so it
    is off unless ``NET_MESH_PUBLISH_LOCAL_TOOLS`` is truthy."""
    return _env_flag("NET_MESH_PUBLISH_LOCAL_TOOLS")


def _federate_tools_enabled() -> bool:
    """Whether to auto-surface discovered mesh capabilities as first-class tools.
    Opt-in via ``NET_MESH_FEDERATE_TOOLS`` — a federation-heavy mesh can add many
    tools, so a deployment turns it on deliberately."""
    return _env_flag("NET_MESH_FEDERATE_TOOLS")


def _a2a_executor_enabled() -> bool:
    """Whether this Hermes accepts hand-off tasks from sibling agents. Opt-in via
    ``NET_MESH_A2A_EXECUTOR`` — running other agents' work must be deliberate.
    (The requester-side ``net_a2a_*`` tools are always available.)"""
    return _env_flag("NET_MESH_A2A_EXECUTOR")


def _on_session_start(**_kwargs) -> None:
    """Start the pin-promotion subscription (Phase 2) for this session. Promotes
    approved pins to first-class tools, driven by the SDK's pin-change
    subscription. Best-effort and idempotent — a failure here must never break a
    session; the meta-tools stand on their own."""
    global _promotion, _provider, _federation, _a2a_service
    if _promotion is None:
        try:
            if node.check_net_available():
                _promotion = pins.start_pin_promotion()
        except Exception as e:  # noqa: BLE001 — a session must not fail on this
            logger.warning("net plugin: pin promotion not started: %s", e)
    # Provider side (opt-in): announce this Hermes's OWN local tools to the mesh,
    # so a sibling machine can invoke them (dangerous ones gated by the
    # provider's approval flow). Best-effort — a publish failure must never break
    # a session; the consume/enroll features stand on their own.
    if _provider is None and _publish_local_tools_enabled():
        try:
            if node.check_net_available():
                _provider = provider.start_local_tool_provider(node.mesh())
        except Exception as e:  # noqa: BLE001 — a session must not fail on this
            logger.warning("net plugin: local-tool publishing not started: %s", e)
    # Consumer side (opt-in): auto-surface discovered mesh capabilities as
    # machine-namespaced first-class tools. Best-effort — a discovery failure
    # must never break a session; the meta-tools still reach the mesh.
    if _federation is None and _federate_tools_enabled():
        try:
            if node.check_net_available():
                _federation = federate.start_federation()
        except Exception as e:  # noqa: BLE001 — a session must not fail on this
            logger.warning("net plugin: tool federation not started: %s", e)
    # A2A executor side (opt-in): accept hand-off tasks from sibling agents and
    # run them through the host agent loop. Best-effort — a failure must never
    # break a session; the requester-side net_a2a_* tools stand on their own.
    if _a2a_service is None and _a2a_executor_enabled():
        try:
            if node.check_net_available():
                _a2a_service = a2a.start_a2a_service(node.mesh())
        except Exception as e:  # noqa: BLE001 — a session must not fail on this
            logger.warning("net plugin: A2A executor not started: %s", e)


def _on_session_end(**_kwargs) -> None:
    """Best-effort teardown when the session ends. Idempotent; swallows errors
    so session end never fails on cleanup."""
    global _promotion, _provider, _federation, _a2a_service
    if _a2a_service is not None:
        _a2a_service.stop()
        _a2a_service = None
    if _federation is not None:
        _federation.stop()
        _federation = None
    if _provider is not None:
        _provider.stop()
        _provider = None
    if _promotion is not None:
        _promotion.stop()
        _promotion = None
    node.shutdown()


def register(ctx) -> None:
    """Register the ``net_*`` mesh tools + the session lifecycle hooks.

    Called once by the plugin loader when ``net`` is in ``plugins.enabled``.
    Every tool shares ``check_fn`` = local node/SDK health (never "peers
    visible"), so a healthy-but-isolated node keeps its tools; only an
    SDK/node failure removes them at the next tools-assembly boundary. The
    pin-promotion subscription starts per-session (``on_session_start``), so
    plugin load itself stays a pure, side-effect-free wiring step.
    """
    for name, schema, handler, emoji in TOOLS:
        ctx.register_tool(
            name=name,
            toolset="net",
            schema=schema,
            handler=handler,
            check_fn=node.check_net_available,
            is_async=True,
            emoji=emoji,
        )
    ctx.register_hook("on_session_start", _on_session_start)
    ctx.register_hook("on_session_end", _on_session_end)
    logger.info("net plugin: registered %d mesh tools (toolset 'net')", len(TOOLS))
