"""Agent-to-agent (A2A) task handoff for the ``net`` Hermes plugin
(``HERMES_INTEGRATION_PLAN_V2.md`` Phase 3; frozen plan Phase 5).

Two sides, both thin over the Rust SDK (`net_sdk::{a2a,mesh_a2a}`, H2):

* **Executor** — :class:`A2aService` serves the A2A lifecycle on the embedded
  node, running each incoming task through the **host agent loop** (an injected
  ``execute`` coroutine), cancellable. In-root A2A is for *parallelism*: a
  sibling machine hands this Hermes a long job while the operator keeps
  chatting; a cancel stops it. Opt-in via ``NET_MESH_A2A_EXECUTOR`` — accepting
  work from the mesh must be deliberate.
* **Requester** — the ``net_a2a_*`` tools (in ``tools.py``) hand off a job to a
  peer node, poll its status, and cancel it.

The task protocol + registry + cooperative cancellation live in the Rust SDK;
this module is the dependency-injected orchestration, so the executor wiring is
testable without a running Hermes. ``start_a2a_service`` wires the production
executor to Hermes's agent runtime (best-effort, guarded; validated under a
real Hermes).
"""

from __future__ import annotations

import logging
from typing import Any, Awaitable, Callable, List, Optional

logger = logging.getLogger(__name__)

# The executor coroutine: run an incoming task and return its result as an
# artifact (Datafort) ref. `async (task_id, prompt, context_refs, tags) -> str`.
Execute = Callable[[str, str, List[str], List[str]], Awaitable[str]]


class A2aService:
    """Serves the A2A task lifecycle on ``mesh``, running each incoming task
    through the injected ``execute`` coroutine. Dependency-injected so the
    executor wiring is testable without Hermes.
    """

    def __init__(self, mesh: Any, execute: Execute) -> None:
        self._mesh = mesh
        self._execute = execute
        self._handle: Any = None

    def start(self) -> "A2aService":
        """Begin accepting A2A tasks (idempotent)."""
        if self._handle is None:
            self._handle = self._mesh.serve_a2a(self._execute)
            logger.info("net plugin: A2A executor serving (accepting tasks from the mesh)")
        return self

    def stop(self) -> None:
        """Stop accepting A2A tasks (best-effort, idempotent)."""
        handle, self._handle = self._handle, None
        if handle is not None:
            try:
                handle.stop()
            except Exception:  # noqa: BLE001 — teardown must not fail the session
                logger.debug("net plugin: A2A serve-handle stop failed", exc_info=True)

    @property
    def serving(self) -> bool:
        """Whether the services are currently registered."""
        return self._handle is not None and bool(getattr(self._handle, "serving", True))


def start_a2a_service(mesh: Any) -> Optional[A2aService]:
    """Wire the production A2A executor to Hermes's agent runtime and start
    serving. Opt-in — the caller gates on ``NET_MESH_A2A_EXECUTOR``. Returns the
    running service, or ``None`` if the host agent runtime can't be wired
    (best-effort — never breaks the session)."""
    try:
        execute = _hermes_executor()
    except Exception as e:  # noqa: BLE001 — never break the session on wiring
        logger.warning(
            "net plugin: A2A executor not started — could not wire the Hermes "
            "agent runtime (%s). Accepting mesh tasks needs a real Hermes host; "
            "the requester-side net_a2a_* tools are unaffected.",
            e,
        )
        return None
    service = A2aService(mesh, execute)
    try:
        service.start()
    except Exception as e:  # noqa: BLE001
        logger.warning("net plugin: A2A executor failed to start: %s", e)
        return None
    return service


def _hermes_executor() -> Execute:
    """Build the ``execute`` coroutine that runs an incoming task through
    Hermes's own agent loop, cancellable via its interrupt machinery, promoting
    the result home as a Datafort artifact ref. **Real-Hermes integration
    point** — the exact agent-runtime / interrupt / artifact surface is
    validated under a running Hermes; the DI'd :class:`A2aService` is what the
    tests cover.
    """
    # Import Hermes lazily so this module imports without it.
    from agent import runtime  # type: ignore  # Hermes's agent runtime

    run_session = getattr(runtime, "run_a2a_session", None)
    if run_session is None:
        raise RuntimeError("Hermes agent runtime exposes no run_a2a_session hook")

    async def execute(task_id: str, prompt: str, context_refs: List[str], tags: List[str]) -> str:
        # Run a fresh agent session on the brief — inbound A2A renders as a
        # Hermes conversation (it already multiplexes platforms; the mesh is one
        # more channel). Cancellation surfaces as an ``asyncio.CancelledError``
        # in this await, which Hermes maps to its interrupt machinery. The result
        # is written to an artifact and its ref returned (never inlined).
        return await run_session(
            task_id=task_id, prompt=prompt, context_refs=context_refs, tags=tags
        )

    return execute
