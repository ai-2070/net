"""MeshOS daemon-author SDK — ergonomic Python wrapper.

Sits on top of the PyO3 binding at ``net._net``. Adds:

- A :class:`MeshOsDaemon` protocol class so type checkers can verify
  daemon implementations against the trait contract.
- :class:`DaemonControl` / :class:`MaintenanceState` typed-dict
  shapes for the dict envelopes the binding emits (the binding
  itself returns plain ``dict`` for forward-compatibility with new
  variants; these are read aliases for editor + checker support).
- :class:`MeshOsSdkError` re-export with ``.kind`` helper.

The binding handles register / control receive / publish_log /
graceful_shutdown; this module reshapes them with context managers
and string-keyed enums.

Example::

    import net_sdk.meshos as meshos
    from net import Identity

    class Telemetry(meshos.MeshOsDaemon):
        def name(self): return "telemetry"
        def process(self, event): return [b"out"]

    with meshos.MeshOsDaemonSdk.start() as sdk:
        with sdk.register_daemon(Telemetry(), Identity.generate()) as handle:
            handle.publish_log("info", "started")
            ev = handle.next_control(timeout_ms=1000)
            if ev and ev["kind"] == "Shutdown":
                pass  # __exit__ drains gracefully
"""

from __future__ import annotations

from typing import Any, Iterable, Literal, Optional, Protocol, TypedDict, Union, runtime_checkable

# The PyO3 module exports these only when the binding was built with
# the `meshos` Cargo feature. Importing the symbols from `net` (not
# `net._net`) keeps the public surface single-source.
try:
    from net import (  # type: ignore[attr-defined]
        MeshOsDaemonHandle as _RawHandle,
        MeshOsDaemonSdk as _RawSdk,
        MeshOsSdkError,
        meshos_sdk_error_kind,
    )
except ImportError as e:  # pragma: no cover — surface a clean message
    raise ImportError(
        "MeshOS SDK symbols not present in `net._net`. Rebuild the "
        "wheel with `--features meshos`, e.g. `maturin develop "
        "--features meshos`."
    ) from e


LogLevel = Literal["trace", "debug", "info", "warn", "error"]

# Control-event poll cadence used by both the sync `anext_control`
# helper and the async `__anext__` iterator. Chosen to keep the
# pyo3 `&mut self` borrow held for ~1ms at a time so other tasks
# (graceful_shutdown, publish_log) can grab the lock between polls.
_CONTROL_POLL_INTERVAL_MS = 10

# Longest single native wait `next_control` makes while holding the
# handle's borrow lock.
_NEXT_CONTROL_SLICE_MS = 50

# Pause between `next_control` slices so a thread waiting for the borrow
# lock gets it (see `next_control`). Small against the 50 ms slice.
_LOCK_HANDOFF_S = 0.001

# The native default grace period (`DEFAULT_GRACEFUL_SHUTDOWN`), and the
# cadence at which `graceful_shutdown` checks whether the daemon left.
_DEFAULT_GRACE_MS = 5000
_SHUTDOWN_POLL_S = 0.05


# =========================================================================
# Typed dict envelopes — match the PyO3 binding's emitted shape.
# =========================================================================


class DaemonControlShutdown(TypedDict):
    kind: Literal["Shutdown"]
    grace_period_ms: int


class DaemonControlDrainStart(TypedDict):
    kind: Literal["DrainStart"]
    grace_period_ms: int


class DaemonControlDrainFinish(TypedDict):
    kind: Literal["DrainFinish"]


class DaemonControlBackpressureOn(TypedDict):
    kind: Literal["BackpressureOn"]
    level: float


class DaemonControlBackpressureOff(TypedDict):
    kind: Literal["BackpressureOff"]


class DaemonControlUnknown(TypedDict):
    """Forward-compatibility envelope for substrate-side variants
    the wrapper version doesn't know about. The cross-binding
    convention is to pass unknown kinds through unchanged so
    consumers can write tolerant matchers."""

    kind: Literal["Unknown"]


DaemonControl = Union[
    DaemonControlShutdown,
    DaemonControlDrainStart,
    DaemonControlDrainFinish,
    DaemonControlBackpressureOn,
    DaemonControlBackpressureOff,
    DaemonControlUnknown,
]


class MaintenanceActive(TypedDict):
    kind: Literal["Active"]


class MaintenanceEntering(TypedDict):
    kind: Literal["EnteringMaintenance"]
    since_ms: int
    deadline_remaining_ms: Optional[int]


class MaintenanceSteady(TypedDict):
    kind: Literal["Maintenance"]
    since_ms: int


class MaintenanceExiting(TypedDict):
    kind: Literal["ExitingMaintenance"]
    since_ms: int


class MaintenanceDrainFailed(TypedDict):
    kind: Literal["DrainFailed"]
    since_ms: int
    reason: str


class MaintenanceRecovery(TypedDict):
    kind: Literal["Recovery"]
    since_ms: int


MaintenanceState = Union[
    MaintenanceActive,
    MaintenanceEntering,
    MaintenanceSteady,
    MaintenanceExiting,
    MaintenanceDrainFailed,
    MaintenanceRecovery,
]


PeerHealth = Literal["Healthy", "Degraded", "Unreachable", "Unknown"]
PeerMaintenance = Literal[
    "Active",
    "EnteringMaintenance",
    "Maintenance",
    "ExitingMaintenance",
    "DrainFailed",
    "Recovery",
    "Unknown",
]


class PeerSnapshot(TypedDict):
    rtt_ms: Optional[int]
    health: Optional[PeerHealth]
    maintenance: Optional[PeerMaintenance]
    cpu_load_1m: Optional[float]
    mem_used_bytes: Optional[int]
    mem_total_bytes: Optional[int]
    disk_used_bytes: Optional[int]
    disk_total_bytes: Optional[int]
    saturation_trend: Optional[float]
    capability_set: list[str]
    software_version: Optional[str]
    forked_from: Optional[int]


class MetadataView(TypedDict):
    node_id: int
    daemon_id: int
    daemon_name: str
    maintenance_state: MaintenanceState
    # Keyed by peer node id; each value is a full PeerSnapshot
    # projection. Slice 2 promoted this from a bare list of ids to
    # the full dict so consumers can read rtt/health/maintenance
    # without a follow-up call.
    peers: dict[int, PeerSnapshot]


# =========================================================================
# Daemon protocol — what a Python daemon implementor satisfies.
# =========================================================================


@runtime_checkable
class MeshOsDaemon(Protocol):
    """Protocol for a Python-side MeshOS daemon.

    Required:
        - ``name`` — a string (or zero-arg method returning one).
        - ``process(event) -> Iterable[bytes] | None`` — handle one
          inbound causal event; return zero or more output payloads.

    Optional (the binding tolerates absence and falls back to defaults):
        - ``snapshot() -> bytes | None``
        - ``restore(state: bytes) -> None``
        - ``on_control(event: DaemonControl) -> None``
        - ``health() -> str | dict`` — ``"healthy"`` |
          ``"degraded"`` | ``"unhealthy"`` or
          ``{"kind": "...", "reason": "..."}``.
        - ``saturation() -> float`` — value in ``[0.0, 1.0]``.
    """

    name: Any  # str OR () -> str — both shapes accepted by the binding.

    def process(self, event: dict) -> Optional[Iterable[bytes]]: ...


# =========================================================================
# MeshOsDaemonSdk — ergonomic wrapper around the PyO3 binding.
# =========================================================================


class MeshOsDaemonSdk:
    """Daemon-author entry point.

    Construct via :meth:`start`; register daemons via
    :meth:`register_daemon`; tear down via :meth:`shutdown` or via
    a ``with`` block.
    """

    __slots__ = ("_raw",)

    def __init__(self, raw: _RawSdk) -> None:
        self._raw = raw

    @classmethod
    def start(
        cls,
        config: Optional[dict[str, Any]] = None,
        *,
        control_capacity: Optional[int] = None,
    ) -> "MeshOsDaemonSdk":
        """Start the SDK with the substrate's ``LoggingDispatcher``.

        ``config`` accepts a dict with optional keys ``this_node``
        (int), ``tick_interval_ms`` (int), ``event_queue_capacity``
        (int), ``action_queue_capacity`` (int).

        ``control_capacity`` overrides the per-daemon control-channel
        capacity. Default is the substrate's
        ``DEFAULT_CONTROL_CHANNEL_CAPACITY`` (8 events).
        """
        return cls(_RawSdk.start(config=config, control_capacity=control_capacity))

    def register_daemon(
        self,
        daemon: MeshOsDaemon,
        identity: Any,
    ) -> "MeshOsDaemonHandleWrapper":
        """Register a daemon under the supplied identity.

        ``identity`` must be a ``net.Identity`` (or any object with
        the same ``keypair`` field shape). The binding extracts the
        underlying ``EntityKeypair`` and uses its ``origin_hash`` as
        the daemon's substrate id.
        """
        handle = self._raw.register_daemon(daemon, identity)
        return MeshOsDaemonHandleWrapper(handle)

    def dropped_control_events(self) -> int:
        """Diagnostic counter — total control events the router
        dropped across every registered daemon because a daemon's
        channel was full."""
        return self._raw.dropped_control_events()

    def shutdown(self) -> None:
        """Tear down the wrapped runtime. Idempotent if already
        shut down (the binding raises
        ``MeshOsSdkError(kind="already_shutdown")``; this wrapper
        re-raises rather than swallowing — explicit double-shutdown
        is still a bug)."""
        self._raw.shutdown()

    def __enter__(self) -> "MeshOsDaemonSdk":
        return self

    def __exit__(self, exc_type, exc_value, traceback) -> Literal[False]:
        # Best-effort drain on context exit. If the user already
        # called shutdown the binding raises `already_shutdown`,
        # which we suppress on context exit because the desired
        # post-state ("SDK is torn down") is already true.
        try:
            self._raw.shutdown()
        except MeshOsSdkError as e:
            if getattr(e, "kind", None) != "already_shutdown":
                raise
        return False

    def __repr__(self) -> str:
        return repr(self._raw)


class MeshOsDaemonHandleWrapper:
    """Per-daemon handle.

    Thin pass-through around the PyO3 ``MeshOsDaemonHandle`` with
    context-manager sugar. Drop the handle to unregister; use
    :meth:`graceful_shutdown` for an explicit drain.

    Cross-thread serialization. The underlying PyO3 class uses a
    ``RefCell``-style guard for ``&mut self`` methods; concurrent
    callers from a thread-pool executor and the asyncio event
    loop can race and trigger a ``"Already borrowed"`` panic. A
    process-wide ``threading.Lock`` serializes every borrow so
    ``async for ev in handle`` plays nicely with
    ``await loop.run_in_executor(None, handle.graceful_shutdown)``.
    """

    __slots__ = ("_raw", "_borrow_lock", "_shutting_down")

    def __init__(self, raw: _RawHandle) -> None:
        import threading

        self._raw = raw
        self._borrow_lock = threading.Lock()
        self._shutting_down = False

    @property
    def daemon_id(self) -> int:
        return self._raw.daemon_id

    @property
    def daemon_name(self) -> str:
        return self._raw.daemon_name

    def metadata(self) -> MetadataView:
        return self._raw.metadata()  # type: ignore[return-value]

    def refresh_metadata(self) -> MetadataView:
        return self._raw.refresh_metadata()  # type: ignore[return-value]

    def next_control(self, timeout_ms: Optional[int] = None) -> Optional[DaemonControl]:
        """Block until the next control event arrives, or
        ``timeout_ms`` elapses, or the runtime shuts down.

        Returns the event dict, or ``None`` on timeout / runtime
        shutdown.

        Waits in short native slices and releases the borrow lock
        between them. Holding it across one unbounded native wait
        blocked every other call on the handle — including
        :meth:`graceful_shutdown` from another thread, the very call
        that would deliver the ``Shutdown`` event (SDK audit
        2026-10-11)."""
        import math
        import time

        deadline = None if timeout_ms is None else time.monotonic() + timeout_ms / 1000.0
        first = True
        while True:
            if deadline is None:
                slice_ms = _NEXT_CONTROL_SLICE_MS
            else:
                left_ms = max(0, math.ceil((deadline - time.monotonic()) * 1000))
                # Always make one native call: `timeout_ms=0` is a
                # non-blocking receive that still returns a queued event.
                if left_ms <= 0 and not first:
                    return None
                slice_ms = min(_NEXT_CONTROL_SLICE_MS, left_ms)
            first = False
            try:
                with self._borrow_lock:
                    ev = self._raw.next_control(timeout_ms=slice_ms)
            except MeshOsSdkError as e:
                if getattr(e, "kind", None) == "already_shutdown":
                    return None  # consumed mid-wait: the runtime shut down
                raise
            if ev is not None:
                return ev  # type: ignore[return-value]
            if self._control_closed():
                return None  # the runtime shut down: nothing will arrive
            # Hand the borrow lock over before taking it again. A
            # `threading.Lock` is not fair: re-acquiring straight after the
            # release let this loop win every handoff on Linux, starving a
            # `graceful_shutdown` waiting for the lock — so its `Shutdown`
            # was never sent and this reader waited on it forever (sdk-py CI
            # on PR #1228; Windows happened to hand over).
            time.sleep(_LOCK_HANDOFF_S)

    def try_next_control(self) -> Optional[DaemonControl]:
        """Non-blocking variant of :meth:`next_control`."""
        with self._borrow_lock:
            return self._raw.try_next_control()  # type: ignore[return-value]

    async def anext_control(
        self, timeout_ms: Optional[int] = None
    ) -> Optional[DaemonControl]:
        """Async variant of :meth:`next_control`. Polls the
        non-blocking :meth:`try_next_control` on the event loop with
        an ``asyncio.sleep`` between iterations so the loop is never
        parked and the underlying pyclass borrow doesn't block
        concurrent calls from other tasks (e.g.
        :meth:`graceful_shutdown`).

        Returns the event dict, or ``None`` on timeout / runtime
        shutdown — same semantics as the sync variant: with
        ``timeout_ms=None`` it waits until an event arrives (it used to
        return ``None`` after 100 ms).

        Slice-3 helper that lets daemons hosted inside an asyncio
        application drive ``MeshOsDaemonHandle`` from coroutines
        without spawning their own thread.

        Pair with :meth:`__aiter__` for ``async for`` consumption:

        .. code-block:: python

            async for ev in handle:
                if ev["kind"] == "Shutdown":
                    break
        """
        import asyncio

        # See `_CONTROL_POLL_INTERVAL_MS` for the cadence rationale.
        poll_ms = _CONTROL_POLL_INTERVAL_MS
        remaining_ms = timeout_ms
        while True:
            try:
                ev = self._try_next_nonblocking()
            except MeshOsSdkError as e:
                if getattr(e, "kind", None) == "already_shutdown":
                    return None
                raise
            if ev is not None:
                return ev
            if self._control_closed():
                return None  # the runtime shut down: nothing will arrive
            if remaining_ms is not None and remaining_ms <= 0:
                return None
            step = poll_ms if remaining_ms is None else min(poll_ms, remaining_ms)
            await asyncio.sleep(step / 1000.0)
            if remaining_ms is not None:
                remaining_ms -= step

    def _try_next_nonblocking(self) -> Optional[DaemonControl]:
        """``try_next_control`` for the event loop: never WAITS for the
        borrow lock. Another thread can hold it across a blocking native
        call (``graceful_shutdown`` holds it for the grace period), and a
        blocking acquire on the loop thread froze the whole event loop for
        that long (SDK audit 2026-10-11). A busy lock reads as "no event
        yet"; the caller polls again."""
        if not self._borrow_lock.acquire(blocking=False):
            return None
        try:
            return self._raw.try_next_control()  # type: ignore[return-value]
        finally:
            self._borrow_lock.release()

    def _control_closed(self) -> bool:
        """Whether the native control channel is closed and drained (the
        runtime shut down). Never waits for the borrow lock — a busy lock
        reads as "not closed yet" and the caller polls again. A wheel
        that predates ``control_closed`` reads as never closed."""
        probe = getattr(self._raw, "control_closed", None)
        if probe is None or not self._borrow_lock.acquire(blocking=False):
            return False
        try:
            return bool(probe())
        finally:
            self._borrow_lock.release()

    def __aiter__(self) -> "MeshOsDaemonHandleWrapper":
        """``async for ev in handle`` — yields each control event
        as it arrives. Stops iterating when the handle is consumed
        by :meth:`graceful_shutdown` or the substrate shuts down
        (the native control channel closes on ``sdk.shutdown()``)."""
        return self

    async def __anext__(self) -> DaemonControl:
        """Poll until the next control event arrives or the handle
        is shut down.

        ``StopAsyncIteration`` fires when the handle has been
        consumed by :meth:`graceful_shutdown` (a subsequent
        :meth:`try_next_control` raises ``already_shutdown``)."""
        import asyncio

        try:
            while True:
                ev = self._try_next_nonblocking()
                if ev is not None:
                    return ev
                if self._control_closed():
                    # The runtime shut down (`sdk.shutdown()`): the channel
                    # is closed and drained, so iteration ends rather than
                    # polling forever (SDK audit 2026-10-11).
                    raise StopAsyncIteration
                await asyncio.sleep(_CONTROL_POLL_INTERVAL_MS / 1000.0)
        except MeshOsSdkError as e:
            if getattr(e, "kind", None) == "already_shutdown":
                raise StopAsyncIteration from None
            raise

    def publish_log(self, level: LogLevel, message: str) -> None:
        """Publish a log line tagged with this daemon's id.

        Raises :class:`MeshOsSdkError` with ``kind`` ``"queue_full"``
        or ``"loop_closed"`` when the substrate's log ring is
        saturated."""
        self._raw.publish_log(level, message)

    def publish_capabilities(self, caps: Optional[dict[str, Any]] = None) -> None:
        """Publish the daemon's capability set.

        Slice 1 is a substrate-side stub — the call returns without
        committing. The binding accepts the argument for
        forward-compatibility."""
        self._raw.publish_capabilities(caps=caps)

    def graceful_shutdown(self, grace_ms: Optional[int] = None) -> None:
        """Drive a graceful shutdown. Sends
        ``Shutdown { grace_period_ms }`` on the daemon's control
        channel, parks for ``grace_ms``, then unregisters. Consumes
        the handle — subsequent method calls raise
        ``MeshOsSdkError(kind="already_shutdown")``.

        The grace period is waited out WITHOUT the borrow lock, so a
        thread parked in :meth:`next_control` (or an ``async for``)
        receives the ``Shutdown`` event and can drain. The native
        one-shot ``graceful_shutdown`` consumed the handle before the
        grace period, so the event sat in a channel no reader could
        reach (code review 2026-10-11, PY-4)."""
        import time

        signal = getattr(self._raw, "signal_shutdown", None)
        if signal is None:  # a wheel that predates the split shutdown
            with self._borrow_lock:
                self._raw.graceful_shutdown(grace_ms=grace_ms)
            return
        with self._borrow_lock:
            if self._shutting_down:
                # A shutdown is already waiting out its grace period: this
                # call is the daemon finishing its drain (typically its own
                # ``with`` exit). Unregister now so that wait ends early.
                self._raw.close()
                return
            self._shutting_down = True
            signal(grace_ms=grace_ms)
        grace_s = (_DEFAULT_GRACE_MS if grace_ms is None else grace_ms) / 1000.0
        deadline = time.monotonic() + grace_s
        try:
            # End early once the daemon leaves the registry (a clean exit
            # is not a hung daemon), as the native one-shot does.
            while time.monotonic() < deadline:
                with self._borrow_lock:
                    if not self._raw.registered:
                        break
                time.sleep(min(_SHUTDOWN_POLL_S, max(0.0, deadline - time.monotonic())))
        finally:
            with self._borrow_lock:
                self._raw.close()

    def __enter__(self) -> "MeshOsDaemonHandleWrapper":
        return self

    def __exit__(self, exc_type, exc_value, traceback) -> Literal[False]:
        try:
            self.graceful_shutdown(grace_ms=None)
        except MeshOsSdkError as e:
            if getattr(e, "kind", None) != "already_shutdown":
                raise
        return False

    def __repr__(self) -> str:
        return repr(self._raw)


__all__ = [
    "MeshOsDaemon",
    "MeshOsDaemonSdk",
    "MeshOsDaemonHandleWrapper",
    "MeshOsSdkError",
    "meshos_sdk_error_kind",
    "DaemonControl",
    "DaemonControlShutdown",
    "DaemonControlDrainStart",
    "DaemonControlDrainFinish",
    "DaemonControlBackpressureOn",
    "DaemonControlBackpressureOff",
    "DaemonControlUnknown",
    "MaintenanceState",
    "MetadataView",
    "PeerSnapshot",
    "PeerHealth",
    "PeerMaintenance",
    "LogLevel",
]
