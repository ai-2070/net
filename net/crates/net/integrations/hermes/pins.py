"""Subscription-driven pin promotion for the ``net`` Hermes plugin (Phase 2).

When the operator approves a capability's pin — via ``net mcp pin approve`` or
anywhere on the machine — it graduates from "reachable through
``net_invoke_capability`` with an approval prompt" to a **first-class, typed
Hermes tool** the model can call directly. On unpin / revoke it is retired.

This mirrors ``tools/mcp_tool.py``'s register-diff pattern, but driven by the
SDK's pin-change **subscription** (``AsyncPinStore.snapshot_and_watch`` — an OS
file watcher, not polling), so a cross-process approval promotes the tool within
about a second, no restart. Registration is diff-based: the initial snapshot is
promoted, then each :class:`PinChange` promotes ``added`` and retires
``removed`` — unchanged pins are left in place.

The consent gate still runs on every invoke (the gateway re-reads the shared
store), so a revoked pin fails closed even if its tool is momentarily still
registered before the retire event lands.
"""

from __future__ import annotations

import asyncio
import json
import logging
import re
import threading
from typing import Any, Callable, Dict, Optional

from . import node

logger = logging.getLogger(__name__)

#: The toolset promoted pins live in (distinct from the five meta-tools' "net").
PINNED_TOOLSET = "net-pinned"

_NAME_UNSAFE = re.compile(r"[^a-zA-Z0-9_]+")


def pinned_tool_name(cap_id: str) -> str:
    """A stable, Hermes-safe tool name for a pinned capability.

    Deterministic — the same ``provider/capability`` id always maps to the same
    name — so a promoted tool keeps its identity across sessions (the plan's
    "pinned tool names are stable"). Interim scheme until the SDK allocates the
    canonical name; collisions are avoided by including both id halves.
    """
    safe = _NAME_UNSAFE.sub("_", cap_id).strip("_").lower()
    return f"net_pinned__{safe}"[:110]


def _risk_note(credential_status: str) -> str:
    cs = (credential_status or "").lower()
    if cs == "credentialed":
        return " [risk: runs against stored credentials on the provider]"
    if cs == "external_api":
        return " [risk: calls an external API from the provider]"
    if cs == "unknown":
        return " [risk: credential exposure unknown]"
    return ""


def build_pinned_schema(cap_id: str, detail: Dict[str, Any]) -> Dict[str, Any]:
    """OpenAI-function schema for a promoted pin, from its describe result:
    the live input schema, a provider/risk-tagged description, and the stable
    name."""
    base_desc = detail.get("description") or detail.get("name") or cap_id
    params = detail.get("input_schema")
    if not isinstance(params, dict) or params.get("type") != "object":
        params = {"type": "object", "properties": {}, "additionalProperties": True}
    description = (
        f"[pinned Net capability `{cap_id}`] {base_desc}"
        f"{_risk_note(detail.get('credential_status', ''))}. Runs on another "
        "machine via the Net mesh; consent was pre-approved, so invoke it "
        "directly. If approval was since revoked you'll get requires_approval."
    )
    return {"name": pinned_tool_name(cap_id), "description": description, "parameters": params}


def make_pinned_handler(cap_id: str) -> Callable:
    """An async tool handler bound to one capability id. Invokes it through the
    consent-gated gateway (so a revoked pin fails closed here)."""

    async def handler(args: Dict[str, Any], **_kw) -> str:
        return await node.gateway().invoke(cap_id, json.dumps(args or {}))

    handler.__name__ = f"pinned_{pinned_tool_name(cap_id)}"
    return handler


class PinPromoter:
    """The diff engine: consumes the pin-change subscription and (de)registers
    promoted tools through a :class:`Registrar`. Transport-agnostic and
    dependency-injected (a ``describe`` coroutine + a registrar), so it is
    testable without Hermes or a live provider.
    """

    def __init__(
        self,
        registrar: "Registrar",
        describe: Callable[[str], "asyncio.Future"],
        check_fn: Callable[[], bool],
        pin_store_path: str,
    ) -> None:
        self._registrar = registrar
        self._describe = describe  # async (cap_id) -> describe dict
        self._check_fn = check_fn
        self._path = pin_store_path
        self._registered: Dict[str, str] = {}  # cap_id -> tool name

    async def _promote(self, cap_id: str) -> None:
        if cap_id in self._registered:
            return
        # The WHOLE promote is guarded — not just describe. A bad describe
        # response, an invalid schema, or a registry rejection (e.g. a name
        # collision) must not propagate out of the watcher loop and silently
        # stop all future promotion for the session.
        try:
            detail = await self._describe(cap_id)
            status = detail.get("status")
            if status not in (None, "ok"):
                logger.warning(
                    "net plugin: cannot promote %s yet (describe: %s)", cap_id, status
                )
                return
            schema = build_pinned_schema(cap_id, detail)
            name = schema["name"]
            self._registrar.register_pinned(
                name=name,
                schema=schema,
                handler=make_pinned_handler(cap_id),
                check_fn=self._check_fn,
                description=schema["description"],
            )
        except Exception as e:  # noqa: BLE001 — one bad capability must not sink the loop
            logger.warning("net plugin: could not promote %s: %s", cap_id, e)
            return
        self._registered[cap_id] = name
        logger.info("net plugin: promoted pinned capability %s -> tool %s", cap_id, name)

    def _retire(self, cap_id: str) -> None:
        name = self._registered.pop(cap_id, None)
        if name is None:
            return
        try:
            self._registrar.deregister_pinned(name)
        except Exception as e:  # noqa: BLE001 — a failed retire must not sink the loop
            logger.warning("net plugin: could not retire %s (tool %s): %s", cap_id, name, e)
            return
        logger.info("net plugin: retired pinned capability %s (tool %s)", cap_id, name)

    async def run(self) -> None:
        """Promote the current snapshot, then apply each subscription delta until
        cancelled. The snapshot + watch is atomic, so nothing is missed between
        the initial read and the first event."""
        from net_sdk import AsyncPinStore

        store = AsyncPinStore(self._path)
        snapshot, watcher = await store.snapshot_and_watch()
        for cap_id in snapshot:
            await self._promote(cap_id)
        async for change in watcher:
            for cap_id in change.removed:
                self._retire(cap_id)
            for cap_id in change.added:
                await self._promote(cap_id)


class Registrar:
    """The seam PinPromoter (de)registers through. Production wires it to
    Hermes's tool registry; tests pass a recording double."""

    def register_pinned(self, *, name, schema, handler, check_fn, description) -> None:
        raise NotImplementedError

    def deregister_pinned(self, name: str) -> None:
        raise NotImplementedError


class HermesRegistrar(Registrar):
    """Registers promoted pins as real, async, check-gated ToolEntries directly
    on Hermes's global registry (as ``tools/mcp_tool.py`` does) — this needs the
    per-tool ``check_fn`` / ``is_async`` fields that ``ctx.register_tool``
    forwards but which we set explicitly here for the pinned toolset."""

    def register_pinned(self, *, name, schema, handler, check_fn, description) -> None:
        from tools.registry import registry

        registry.register(
            name=name,
            toolset=PINNED_TOOLSET,
            schema=schema,
            handler=handler,
            check_fn=check_fn,
            is_async=True,
            description=description,
            emoji="\U0001f4cc",
        )

    def deregister_pinned(self, name: str) -> None:
        from tools.registry import registry

        registry.deregister(name)


class PinPromotionService:
    """Owns the background thread + event loop that runs a :class:`PinPromoter`.

    Runs off the main loop (a daemon thread with its own asyncio loop), so it
    doesn't depend on Hermes's loop being live at plugin-load time; the promoted
    tools' handlers still run in Hermes's own dispatch loop.
    """

    def __init__(self, promoter: PinPromoter) -> None:
        self._promoter = promoter
        self._thread: Optional[threading.Thread] = None
        self._loop: Optional[asyncio.AbstractEventLoop] = None
        self._task: Optional[asyncio.Task] = None

    def start(self) -> None:
        if self._thread is not None:
            return

        def _main() -> None:
            loop = asyncio.new_event_loop()
            asyncio.set_event_loop(loop)
            self._loop = loop
            task = loop.create_task(self._promoter.run())
            self._task = task
            try:
                loop.run_until_complete(task)
            except asyncio.CancelledError:
                pass
            except Exception as e:  # noqa: BLE001 — never crash the host process
                logger.debug("net plugin: pin promotion loop exited: %s", e)
            finally:
                loop.close()

        self._thread = threading.Thread(target=_main, name="net-pin-promoter", daemon=True)
        self._thread.start()

    def stop(self) -> None:
        loop, task = self._loop, self._task
        if loop is not None and task is not None:
            loop.call_soon_threadsafe(task.cancel)
        if self._thread is not None:
            self._thread.join(timeout=2.0)
            # Only clear the handle if the thread actually stopped — otherwise a
            # later start() would see `_thread is None` and spawn a SECOND
            # promotion loop alongside the still-running one.
            if self._thread.is_alive():
                logger.warning(
                    "net plugin: pin promotion thread did not stop within 2s; "
                    "keeping the handle so start() won't spawn a second loop"
                )
            else:
                self._thread = None


def start_pin_promotion() -> PinPromotionService:
    """Build the production promoter (Hermes registrar + gateway-backed describe
    + node health check) and start it. Returns a handle to stop at session end.
    """

    async def _describe(cap_id: str) -> Dict[str, Any]:
        return json.loads(await node.gateway().describe(cap_id))

    promoter = PinPromoter(
        registrar=HermesRegistrar(),
        describe=_describe,
        check_fn=node.check_net_available,
        pin_store_path=node.pin_store_path(),
    )
    service = PinPromotionService(promoter)
    service.start()
    logger.info("net plugin: pin promotion subscribed (toolset '%s')", PINNED_TOOLSET)
    return service
