"""Consume-side tool federation for the ``net`` Hermes plugin
(``HERMES_INTEGRATION_PLAN_V2.md`` Phase 2, Slice D).

The *consumer* half of "federate everything": every capability discovered on the
operator mesh is auto-surfaced as a **machine-namespaced, first-class Hermes
tool** (``mesh__<provider>__<tool>``), so the model reaches a sibling machine's
tools by calling them directly — no manual ``net_search_capabilities`` /
``net_invoke_capability`` dance. Dedup is already done in the SDK (the gateway
collapses ``provider_equivalent`` capabilities into one group and lists its
providers); this module surfaces the groups and keeps the promoted tool set
reconciled with what's reachable, so lid-close / reopen adds and removes tools
at the next poll (and ``check_fn`` gates finer availability).

This mirrors ``pins.py``'s promotion pattern, but driven by the gateway's
discovery (a periodic reconcile) rather than the pin-change subscription, and
over **all** discovered capabilities, not just pinned ones. Invocation goes
through the same consent-gated gateway: a capability the operator hasn't
approved still answers ``requires_approval`` on invoke, so surfacing it is not
granting it. **Doctrine note:** true *no-ceremony* in-root invocation (trusting a
cryptographically-verified same-root provider without a per-tool pin) is the
deferred "owner root-identity" refinement — a wire-declared ``credential_status``
is untrusted (``consent.rs``: ``"none"`` → gated), so today the consume side
surfaces + namespaces, and the existing consent gate still governs invoke.
"""

from __future__ import annotations

import asyncio
import json
import logging
import re
import threading
from typing import Any, Awaitable, Callable, Dict, List, Optional, Sequence

logger = logging.getLogger(__name__)

#: The toolset federated capabilities live in (distinct from the meta-tools'
#: "net" and the pin fast-path's "net-pinned"). A federation-heavy mesh puts
#: hundreds of tools here; Hermes's tool_search deferral curates the prompt (the
#: toolset is the deferral unit — "total on the mesh, curated in the prompt").
FEDERATED_TOOLSET = "net-federated"

_NAME_UNSAFE = re.compile(r"[^a-zA-Z0-9_]+")


def federated_tool_name(cap_id: str) -> str:
    """A stable, Hermes-safe tool name for a discovered capability.

    Deterministic — the same ``provider/capability`` id always maps to the same
    name — so a promoted tool keeps its identity across polls / sessions. The id
    is machine-namespaced (``<provider>/<tool>``): the ``provider/tool`` boundary
    becomes ``__`` so the machine scope stays legible in the name
    (``pc/terminal.run`` → ``net_mesh__pc__terminal_run``), and any other unsafe
    run folds to a single ``_``."""
    scoped = cap_id.replace("/", "__")
    safe = _NAME_UNSAFE.sub("_", scoped).strip("_").lower()
    return f"net_mesh__{safe}"[:110]


def build_federated_schema(
    cap_id: str, detail: Dict[str, Any], providers: Sequence[Any]
) -> Dict[str, Any]:
    """OpenAI-function schema for a federated capability, from its describe
    result: the live input schema, a machine-namespaced + provider-tagged
    description, and the stable name."""
    base_desc = detail.get("description") or detail.get("name") or cap_id
    params = detail.get("input_schema")
    if not isinstance(params, dict) or params.get("type") != "object":
        params = {"type": "object", "properties": {}, "additionalProperties": True}
    prov = ""
    if providers:
        prov = f" (providers: {', '.join(str(p) for p in providers)})"
    description = (
        f"[mesh capability `{cap_id}`{prov}] {base_desc}. Runs on another machine "
        "in your operator mesh via Net. Call it directly. If it returns "
        "status=requires_approval, the capability needs local approval first "
        "(net_request_pin) — surfacing it here does not grant it."
    )
    return {
        "name": federated_tool_name(cap_id),
        "description": description,
        "parameters": params,
    }


class Registrar:
    """The seam FederationPromoter (de)registers through. Production wires it to
    Hermes's tool registry; tests pass a recording double."""

    def register_federated(self, *, name, schema, handler, check_fn, description) -> None:
        raise NotImplementedError

    def deregister_federated(self, name: str) -> None:
        raise NotImplementedError


class HermesRegistrar(Registrar):
    """Registers federated capabilities as real async, check-gated tools on
    Hermes's global registry (as ``pins.py`` does), in the ``net-federated``
    toolset so Hermes's tool_search deferral can curate them out of the prompt
    until searched."""

    def register_federated(self, *, name, schema, handler, check_fn, description) -> None:
        from tools.registry import registry

        registry.register(
            name=name,
            toolset=FEDERATED_TOOLSET,
            schema=schema,
            handler=handler,
            check_fn=check_fn,
            is_async=True,
            description=description,
            emoji="\U0001f310",  # globe — a tool on another machine
        )

    def deregister_federated(self, name: str) -> None:
        from tools.registry import registry

        registry.deregister(name)


class FederationPromoter:
    """The diff engine: reconciles the promoted tool set against the mesh's
    currently-discovered capabilities. Dependency-injected (a ``search`` result,
    a ``describe`` coroutine, an ``invoke`` coroutine, a registrar) so it is
    testable without Hermes or a live mesh.
    """

    def __init__(
        self,
        registrar: Registrar,
        describe: Callable[[str], "Awaitable[Dict[str, Any]]"],
        invoke: Callable[[str, Dict[str, Any]], "Awaitable[str]"],
        check_fn: Callable[[], bool],
    ) -> None:
        self._registrar = registrar
        self._describe = describe  # async (cap_id) -> describe dict
        self._invoke = invoke  # async (cap_id, args) -> result JSON string
        self._check_fn = check_fn
        self._registered: Dict[str, str] = {}  # cap_id -> tool name

    async def reconcile(self, capabilities: Sequence[Dict[str, Any]]) -> None:
        """Given the mesh's currently-discovered capability rows (from the
        gateway search — already deduped into groups), promote newly-seen ones
        and retire ones that have vanished. Idempotent per steady state."""
        seen = set()
        for row in capabilities:
            cap_id = row.get("cap_id")
            if not cap_id:
                continue
            seen.add(cap_id)
            if cap_id not in self._registered:
                await self._promote(cap_id, row)
        for cap_id in list(self._registered):
            if cap_id not in seen:
                self._retire(cap_id)

    async def _promote(self, cap_id: str, row: Dict[str, Any]) -> None:
        # The WHOLE promote is guarded — a bad describe, an invalid schema, or a
        # registry rejection (e.g. a name collision) must not stop the reconcile
        # loop for the rest of the session.
        try:
            detail = await self._describe(cap_id)
            status = detail.get("status")
            if status not in (None, "ok"):
                logger.info("net plugin: cannot federate %s yet (describe: %s)", cap_id, status)
                return
            schema = build_federated_schema(cap_id, detail, row.get("providers") or [])
            name = schema["name"]
            self._registrar.register_federated(
                name=name,
                schema=schema,
                handler=self._make_handler(cap_id),
                check_fn=self._check_fn,
                description=schema["description"],
            )
        except Exception as e:  # noqa: BLE001 — one bad capability must not sink the loop
            logger.warning("net plugin: could not federate %s: %s", cap_id, e)
            return
        self._registered[cap_id] = name
        logger.info("net plugin: federated mesh capability %s -> tool %s", cap_id, name)

    def _retire(self, cap_id: str) -> None:
        name = self._registered.pop(cap_id, None)
        if name is None:
            return
        try:
            self._registrar.deregister_federated(name)
        except Exception as e:  # noqa: BLE001 — a failed retire must not sink the loop
            logger.warning("net plugin: could not retire federated %s (%s): %s", cap_id, name, e)
            return
        logger.info("net plugin: retired federated capability %s (tool %s)", cap_id, name)

    def _make_handler(self, cap_id: str) -> Callable:
        """An async tool handler bound to one capability id. Invokes it through
        the consent-gated gateway — so an un-approved capability still answers
        requires_approval here, and the model relays that."""

        async def handler(args: Dict[str, Any], **_kw) -> str:
            return await self._invoke(cap_id, args or {})

        handler.__name__ = f"federated_{federated_tool_name(cap_id)}"
        return handler


class FederationService:
    """Owns the background thread + event loop that periodically reconciles the
    federated tool set against the gateway's discovery. Runs off the main loop
    (a daemon thread with its own asyncio loop), mirroring
    ``pins.PinPromotionService``; the promoted tools' handlers still run in
    Hermes's own dispatch loop.
    """

    def __init__(
        self,
        promoter: FederationPromoter,
        search: Callable[[], "Awaitable[Optional[Sequence[Dict[str, Any]]]]"],
        interval_seconds: float = 30.0,
    ) -> None:
        self._promoter = promoter
        self._search = search  # async () -> list of capability rows
        self._interval = max(1.0, float(interval_seconds))
        self._thread: Optional[threading.Thread] = None
        self._loop: Optional[asyncio.AbstractEventLoop] = None
        self._task: Optional[asyncio.Task] = None

    async def _poll_once(self) -> None:
        """One search → reconcile round. A search that yields ``None`` (errored
        — see :func:`parse_search_result`) is NOT an empty mesh: reconciling
        against ``[]`` would retire every federated tool on a transient
        gateway/fold blip, only for the next poll to re-describe and re-promote
        them all (tool flicker + a describe storm). Keep the current set and
        let the next poll retry instead."""
        caps = await self._search()
        if caps is None:
            logger.debug(
                "net plugin: federation search unavailable; keeping the current tool set"
            )
            return
        await self._promoter.reconcile(caps)

    async def _run(self) -> None:
        while True:
            try:
                await self._poll_once()
            except asyncio.CancelledError:
                raise
            except Exception as e:  # noqa: BLE001 — a poll failure must not kill the loop
                logger.debug("net plugin: federation reconcile failed: %s", e)
            await asyncio.sleep(self._interval)

    def start(self) -> None:
        if self._thread is not None:
            return

        def _main() -> None:
            loop = asyncio.new_event_loop()
            asyncio.set_event_loop(loop)
            self._loop = loop
            task = loop.create_task(self._run())
            self._task = task
            try:
                loop.run_until_complete(task)
            except asyncio.CancelledError:
                pass
            except Exception as e:  # noqa: BLE001 — never crash the host process
                logger.debug("net plugin: federation loop exited: %s", e)
            finally:
                loop.close()

        self._thread = threading.Thread(target=_main, name="net-federation", daemon=True)
        self._thread.start()

    def stop(self) -> None:
        loop, task = self._loop, self._task
        # Guard on is_closed(): after a prior stop the loop is closed, and
        # call_soon_threadsafe on a closed loop raises (this teardown is
        # documented idempotent).
        if loop is not None and task is not None and not loop.is_closed():
            loop.call_soon_threadsafe(task.cancel)
        if self._thread is not None:
            self._thread.join(timeout=2.0)
            if self._thread.is_alive():
                logger.warning(
                    "net plugin: federation thread did not stop within 2s; keeping "
                    "the handle so start() won't spawn a second loop"
                )
                return
            self._thread = None
        self._loop = None
        self._task = None


def parse_search_result(raw_json: str) -> Optional[List[Dict[str, Any]]]:
    """Map a gateway search reply to capability rows — or ``None`` when the
    search itself errored (``status != "ok"``). ``None`` and ``[]`` are
    deliberately distinct: an errored search says nothing about the mesh, while
    an ok-but-empty result genuinely means "no capabilities" (and retires
    everything)."""
    raw = json.loads(raw_json)
    if raw.get("status") != "ok":
        logger.debug(
            "net plugin: federation search errored (status=%s)", raw.get("status")
        )
        return None
    return raw.get("capabilities", [])


def start_federation() -> FederationService:
    """Build the production federation service (Hermes registrar + gateway-backed
    search/describe/invoke + node health check) and start it."""
    from . import node

    async def _search() -> Optional[List[Dict[str, Any]]]:
        return parse_search_result(await node.gateway().search(""))

    async def _describe(cap_id: str) -> Dict[str, Any]:
        return json.loads(await node.gateway().describe(cap_id))

    async def _invoke(cap_id: str, args: Dict[str, Any]) -> str:
        if not node.delegation_valid_for_invoke():
            return json.dumps(
                {
                    "status": "denied",
                    "error": "the gateway delegation is revoked or expired; "
                    "re-approve or renew it before invoking",
                }
            )
        return await node.gateway().invoke(cap_id, json.dumps(args))

    promoter = FederationPromoter(
        registrar=HermesRegistrar(),
        describe=_describe,
        invoke=_invoke,
        check_fn=node.check_net_available,
    )
    service = FederationService(promoter, _search)
    service.start()
    logger.info("net plugin: tool federation started (toolset '%s')", FEDERATED_TOOLSET)
    return service
