"""``net`` — Net mesh integration for Hermes (``HERMES_INTEGRATION_PLAN.md``
Phase 1).

A first-party, standalone plugin that lets a Hermes agent reach capabilities
running on the user's *other* machines — the ones published there with
``net wrap`` — as five ``net_*`` tools, with local consent + pin approval. It
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

from . import node
from .tools import TOOLS

logger = logging.getLogger(__name__)


def _on_session_end(**_kwargs) -> None:
    """Best-effort node teardown when the session ends. Idempotent; swallows
    errors so session end never fails on cleanup."""
    node.shutdown()


def register(ctx) -> None:
    """Register the five ``net_*`` mesh tools + the shutdown hook.

    Called once by the plugin loader when ``net`` is in ``plugins.enabled``.
    Every tool shares ``check_fn`` = local node/SDK health (never "peers
    visible"), so a healthy-but-isolated node keeps its tools; only an
    SDK/node failure removes them at the next tools-assembly boundary.
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
    ctx.register_hook("on_session_end", _on_session_end)
    logger.info("net plugin: registered %d mesh tools (toolset 'net')", len(TOOLS))
