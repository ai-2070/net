"""Provider-side local-tool federation for the ``net`` Hermes plugin
(``HERMES_INTEGRATION_PLAN_V2.md`` Phase 2, Slice C).

The *provider* half of "federate everything, in-root": a Hermes announces its
OWN local tools (terminal, files, browser, …) to the operator mesh so a sibling
machine can invoke them, backed by a callback that routes each invoke back into
this Hermes's own tool dispatch. **The mesh adds reach, not authority** — a
mesh-originated call to a *dangerous* tool runs the provider's existing approval
flow (routed to the operator surface), and **fails closed** with
``approval_unreachable`` when no operator surface can be reached, never leaking
the decision back into the calling agent's loop.

The publish + serve machinery lives in the Rust SDK
(``NetMesh.publish_tools`` → ``net_mcp::wrap``, H2); this module is the thin,
dependency-injected orchestration over it, so the approval gating is testable
without a running Hermes. ``start_local_tool_provider`` wires the production
seams to Hermes's registry / dispatch / approval surfaces (best-effort, guarded;
validated under a real Hermes).
"""

from __future__ import annotations

import json
import logging
import os
from typing import Any, Awaitable, Callable, Dict, List, Optional, Sequence, Tuple

logger = logging.getLogger(__name__)

# A local tool descriptor: (name, description|None, input_schema as a dict or a
# JSON string). The provider serializes a dict to the JSON string the binding
# wants.
ToolSpec = Tuple[str, Optional[str], Any]

# Injected seams — production wires them to Hermes; tests pass doubles.
ListTools = Callable[[], Sequence[ToolSpec]]
Dispatch = Callable[[str, Dict[str, Any]], Awaitable[Any]]
# Approve returns True (run) / False (operator declined) / None (no operator
# surface reachable → fail closed).
Approve = Callable[[str, Dict[str, Any]], Awaitable[Optional[bool]]]
IsDangerous = Callable[[str], bool]


class LocalToolProvider:
    """Publishes this node's local tools and routes each mesh invoke back into
    Hermes's dispatch, gated by provider-side approval for dangerous tools.

    Dependency-injected (``list_tools`` / ``dispatch`` / ``approve`` /
    ``is_dangerous``) so the approval gating is unit-testable without Hermes or
    a live provider.
    """

    def __init__(
        self,
        mesh: Any,
        list_tools: ListTools,
        dispatch: Dispatch,
        approve: Approve,
        is_dangerous: IsDangerous,
    ) -> None:
        self._mesh = mesh
        self._list_tools = list_tools
        self._dispatch = dispatch
        self._approve = approve
        self._is_dangerous = is_dangerous
        self._handle: Any = None

    def start(self) -> List[str]:
        """Publish the current local toolset. Idempotent — a second call is a
        no-op (returns the already-published ids). Publishing an empty set is a
        no-op (nothing announced)."""
        if self._handle is not None:
            return self.published
        specs: List[Tuple[str, Optional[str], str]] = []
        for name, description, schema in self._list_tools():
            schema_json = (
                schema if isinstance(schema, str) else json.dumps(schema or {"type": "object"})
            )
            specs.append((name, description, schema_json))
        if not specs:
            logger.info("net plugin: no local tools to publish to the mesh")
            return []
        # Explicit opt-in: these tools exist to be invoked by sibling in-root
        # machines (the binding's default admits only the publishing node
        # itself). Authority stays with this provider — every dangerous invoke
        # still runs the operator-approval gate in `_callback`.
        self._handle = self._mesh.publish_tools(specs, self._callback, allow_any_caller=True)
        logger.info(
            "net plugin: published %d local tools to the mesh (%s)",
            len(self.published),
            ", ".join(self.published),
        )
        return self.published

    def stop(self) -> None:
        """Withdraw the publication (best-effort, idempotent)."""
        handle, self._handle = self._handle, None
        if handle is not None:
            try:
                handle.stop()
            except Exception:  # noqa: BLE001 — teardown must not fail the session
                logger.debug("net plugin: local-tool publication stop failed", exc_info=True)

    @property
    def published(self) -> List[str]:
        """The tool ids currently published (empty when not started)."""
        return list(self._handle.tools) if self._handle is not None else []

    async def _callback(self, name: str, args_json: str):
        """The invoke seam the Rust invoker drives. Returns the tool's text
        output on success, or a ``(json, True)`` error tuple (the binding's
        tool-level-error form) on a denied / unreachable approval or a dispatch
        failure — so a refusal is a `denied` verdict to the caller, never the
        tool running."""
        try:
            args = json.loads(args_json) if args_json else {}
        except (TypeError, ValueError):
            args = {}
        if not isinstance(args, dict):
            args = {}

        # Provider-side approval for dangerous tools — run exactly the flow a
        # local call would. A declined or unreachable approval fails closed.
        if self._is_dangerous(name):
            try:
                decision = await self._approve(name, args)
            except Exception as e:  # noqa: BLE001 — an approval error fails closed
                logger.warning("net plugin: approval for %s errored: %s", name, e)
                return (_deny_body(name, "approval_error", str(e)), True)
            if decision is None:
                return (
                    _deny_body(
                        name,
                        "approval_unreachable",
                        "no operator approval surface is reachable on this machine; "
                        "configure one before invoking dangerous tools remotely",
                    ),
                    True,
                )
            if not decision:
                return (
                    _deny_body(name, "denied", "the operator declined this invocation"),
                    True,
                )

        try:
            result = await self._dispatch(name, args)
        except Exception as e:  # noqa: BLE001 — surface a dispatch failure in-band
            logger.warning("net plugin: dispatch of %s failed: %s", name, e)
            return (_deny_body(name, "error", str(e)), True)
        return result if isinstance(result, str) else json.dumps(result)


def _deny_body(name: str, status: str, message: str) -> str:
    return json.dumps({"status": status, "tool": name, "message": message})


# ---------------------------------------------------------------------------
# Production wiring — the Hermes-facing adapters. Isolated here so the exact
# Hermes surface is threaded in one place (and stubbed in tests). Best-effort:
# a wiring failure logs and publishes nothing rather than breaking the session,
# and this path is validated under a real Hermes (the DI'd core above is what
# the plugin tests cover).
# ---------------------------------------------------------------------------

#: Toolsets this plugin publishes onto the mesh itself — never republish them
#: (a mesh capability re-announced as a local tool would loop).
_OWN_TOOLSETS = frozenset({"net", "net-pinned"})

#: Name-substring heuristic for "dangerous" tools when Hermes exposes no
#: explicit per-tool approval flag. Fail-safe: an *unclassified* tool is treated
#: as dangerous (approval required), matching the "unknown is gated" doctrine.
_DANGEROUS_HINTS = (
    "terminal",
    "shell",
    "exec",
    "command",
    "run",
    "process",
    "write",
    "edit",
    "delete",
    "remove",
    "desktop",
    "computer",
    "browser",
    "click",
    "keyboard",
)


def name_looks_dangerous(name: str) -> bool:
    """Heuristic classifier used when Hermes offers no explicit approval flag.
    Fail-safe: only tools whose names clearly read as read-only (``read`` /
    ``get`` / ``list`` / ``search`` / ``describe``) are treated as safe; every
    other tool is gated."""
    low = name.lower()
    safe_hints = ("read", "get", "list", "search", "describe", "status", "info")
    if any(h in low for h in _DANGEROUS_HINTS):
        return True
    if any(low.startswith(h) or f"_{h}" in low for h in safe_hints):
        return False
    return True  # unknown → dangerous (gated)


def start_local_tool_provider(mesh: Any) -> Optional[LocalToolProvider]:
    """Wire the production provider to Hermes's registry + approval surface and
    start it. Opt-in — the caller gates on ``NET_MESH_PUBLISH_LOCAL_TOOLS``.
    Returns the running provider, or ``None`` if the local toolset can't be read
    (best-effort — a wiring failure never breaks the session)."""
    try:
        adapters = _hermes_adapters()
    except Exception as e:  # noqa: BLE001 — never break the session on wiring
        logger.warning(
            "net plugin: local-tool publishing not started — could not wire the "
            "Hermes registry/approval adapters (%s). Publishing OWN tools to the "
            "mesh needs a real Hermes host; the mesh consume/enroll features are "
            "unaffected.",
            e,
        )
        return None
    provider = LocalToolProvider(mesh, *adapters)
    try:
        provider.start()
    except Exception as e:  # noqa: BLE001
        logger.warning("net plugin: local-tool publication failed to start: %s", e)
        return None
    return provider


def _hermes_adapters() -> Tuple[ListTools, Dispatch, Approve, IsDangerous]:
    """Build the (list_tools, dispatch, approve, is_dangerous) seams from
    Hermes's own APIs. **Real-Hermes integration point** — the exact registry /
    dispatch / approval surface is validated under a running Hermes; the guards
    in :func:`start_local_tool_provider` keep a mismatch safe (publishes
    nothing). Import Hermes lazily so this module imports without Hermes.
    """
    from tools.registry import registry  # Hermes's global tool registry

    def list_tools() -> Sequence[ToolSpec]:
        specs: List[ToolSpec] = []
        # Enumerate the registry's own tool entries, skipping this plugin's mesh
        # toolsets (loop prevention) and anything without an object-shaped
        # schema. `registry.tools` is a name→entry mapping on Hermes's registry.
        for name, entry in dict(getattr(registry, "tools", {})).items():
            toolset = getattr(entry, "toolset", None)
            if toolset in _OWN_TOOLSETS:
                continue
            schema = getattr(entry, "schema", None) or {}
            params = schema.get("parameters") if isinstance(schema, dict) else None
            if not isinstance(params, dict):
                params = {"type": "object", "properties": {}, "additionalProperties": True}
            description = getattr(entry, "description", None) or (
                schema.get("description") if isinstance(schema, dict) else None
            )
            specs.append((name, description, params))
        return specs

    async def dispatch(name: str, args: Dict[str, Any]) -> Any:
        # Run the tool through Hermes's own dispatch so provider-local semantics
        # (working dir, credentials, event loop) match a local call exactly.
        return await registry.dispatch(name, args)

    async def approve(name: str, args: Dict[str, Any]) -> Optional[bool]:
        # Route to the operator's approval surface. Returns True/False, or None
        # when no surface is reachable (fail closed). Real-Hermes wiring:
        # `tools/approval.py` / the gateway approval path.
        try:
            from tools import approval  # type: ignore
        except Exception:  # noqa: BLE001 — no approval surface compiled in
            return None
        request = getattr(approval, "request_operator_approval", None)
        if request is None:
            return None
        return await request(name, args)

    def is_dangerous(name: str) -> bool:
        entry = dict(getattr(registry, "tools", {})).get(name)
        flag = getattr(entry, "requires_approval", None) if entry is not None else None
        if isinstance(flag, bool):
            return flag
        return name_looks_dangerous(name)

    return list_tools, dispatch, approve, is_dangerous
