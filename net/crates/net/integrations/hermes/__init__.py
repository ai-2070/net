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
from typing import Optional

from . import delegation, node, pins
from .tools import TOOLS

# `delegation` is imported for package-attribute access (tests, and the
# node's lazy `from .delegation import GatewayDelegation`); the node owns its
# lifecycle.
_ = delegation

logger = logging.getLogger(__name__)

# The running pin-promotion subscription (Phase 2), if started. Held so the
# session-end hook can stop it.
_promotion: Optional["pins.PinPromotionService"] = None


def _on_session_start(**_kwargs) -> None:
    """Start the pin-promotion subscription (Phase 2) for this session. Promotes
    approved pins to first-class tools, driven by the SDK's pin-change
    subscription. Best-effort and idempotent — a failure here must never break a
    session; the meta-tools stand on their own."""
    global _promotion
    if _promotion is not None:
        return
    try:
        if node.check_net_available():
            _promotion = pins.start_pin_promotion()
    except Exception as e:  # noqa: BLE001 — a session must not fail on this
        logger.warning("net plugin: pin promotion not started: %s", e)


def _on_session_end(**_kwargs) -> None:
    """Best-effort teardown when the session ends. Idempotent; swallows errors
    so session end never fails on cleanup."""
    global _promotion
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
