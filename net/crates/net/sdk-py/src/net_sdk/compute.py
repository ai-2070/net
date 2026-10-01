"""Compute — daemons hosted on the mesh, with snapshot and live migration.

Wraps the wheel's daemon runtime (``net.DaemonRuntime``) so it can be
built from a :class:`net_sdk.MeshNode` directly, and adds the typed
vocabulary the TypeScript SDK ships: the :class:`MeshDaemon` protocol a
daemon implements, :class:`DaemonHostConfig`, and the
:data:`MigrationPhase` / :data:`MigrationErrorKind` strings.

Example:
    >>> from net_sdk import MeshNode
    >>> from net_sdk.compute import CausalEvent, DaemonRuntime
    >>> from net import Identity
    >>>
    >>> class Echo:
    ...     def process(self, event: CausalEvent) -> list[bytes]:
    ...         return [event.payload]
    >>>
    >>> node = MeshNode("127.0.0.1:0", "42" * 32)
    >>> rt = DaemonRuntime(node)
    >>> rt.register_factory("echo", Echo)
    >>> rt.start()
    >>> ident = Identity.generate()
    >>> handle = rt.spawn("echo", ident)
    >>> rt.deliver(handle.origin_hash, CausalEvent(ident.origin_hash, 1, b"hi"))
    [b'hi']

**Latency.** A Python daemon's ``process`` runs under the GIL, so it does
not meet the core's microsecond contract, and daemons in one process
serialize on one GIL. Use Python daemons for control-plane work; write
hot-loop daemons in Rust (``SDK_COMPUTE_SURFACE_PLAN.md`` § Risks).

Present iff the wheel was built with the ``compute`` feature (the
default build is).
"""

from __future__ import annotations

from typing import Any, Callable, List, Literal, Optional, Protocol, TypedDict

from net import (  # type: ignore[attr-defined]
    CausalEvent,
    DaemonError,
    DaemonHandle,
    DaemonRuntime as _NativeDaemonRuntime,
    MigrationError,
    MigrationHandle,
    migration_error_kind,
)

from net_sdk.mesh import _native_mesh

MigrationPhase = Literal["snapshot", "transfer", "restore", "replay", "cutover", "complete"]
"""What :meth:`DaemonRuntime.migration_phase` and
:meth:`MigrationHandle.phase` report. Exactly the strings the wheel's
``migration_phase_str`` produces."""

MigrationErrorKind = Literal[
    # A target's refusal (`MigrationFailureReason`).
    "not-ready",
    "factory-not-found",
    "compute-not-supported",
    "state-failed",
    "already-migrating",
    "identity-transport-failed",
    "not-ready-timeout",
    # An orchestrator error (`MigrationError`).
    "daemon-not-found",
    "target-unavailable",
    "no-target-available",
    "wrong-phase",
    "snapshot-too-large",
    "buffer-full",
    "wrong-peer",
]
"""What :func:`migration_error_kind` returns for a ``MigrationError``.
Taken from the wheel's ``format_migration_failure_reason`` and
``format_migration_error``. Only ``"not-ready"`` is retriable; the source
retries it by default."""


class MeshDaemon(Protocol):
    """What a daemon factory must return.

    Only ``process`` is required. The runtime also looks up, and uses when
    present:

    - ``snapshot() -> bytes | None``: serialize state, for snapshots and
      migration. A daemon without it is stateless.
    - ``restore(state: bytes) -> None``: rebuild state from a snapshot.
    - ``required_capabilities`` / ``optional_capabilities``: capability
      dicts used for placement.

    A ``process`` that raises surfaces as :class:`DaemonError`; it doesn't
    crash the host.
    """

    def process(self, event: CausalEvent) -> List[bytes]:
        """Handle one event; return zero or more output payloads."""
        ...


DaemonFactory = Callable[[], MeshDaemon]
"""A zero-argument callable (often the daemon class itself) that builds
one daemon instance. Registered per kind with
:meth:`DaemonRuntime.register_factory`."""


class DaemonHostConfig(TypedDict, total=False):
    """Per-daemon host options for :meth:`DaemonRuntime.spawn`. Unset keys
    take the runtime defaults."""

    auto_snapshot_interval: int
    max_log_entries: int


class MigrationOptions(TypedDict, total=False):
    """Options for :meth:`DaemonRuntime.start_migration_with`."""

    transport_identity: bool
    """Carry the daemon's identity in the migration snapshot (default).
    ``False`` requires the target to call
    :meth:`DaemonRuntime.register_migration_target_identity`."""
    retry_not_ready_ms: int


class DaemonRuntime:
    """The daemon supervisor for one mesh node.

    Accepts a :class:`net_sdk.MeshNode` or a raw ``net.NetMesh``. Every
    method forwards to the wheel's runtime, available as :attr:`native`
    (pass that to ``net.AsyncDaemonRuntime`` or to the
    :mod:`net_sdk.groups` constructors).
    """

    def __init__(self, mesh: Any) -> None:
        self._native = _NativeDaemonRuntime(_native_mesh(mesh))

    @property
    def native(self) -> Any:
        """The wheel's ``net.DaemonRuntime``."""
        return self._native

    def start(self) -> None:
        """Become ``Ready`` and install the migration handler. Idempotent."""
        self._native.start()

    def shutdown(self) -> None:
        """Drain daemons and uninstall the migration handler. The mesh node
        itself is untouched."""
        self._native.shutdown()

    def is_ready(self) -> bool:
        return self._native.is_ready()

    def daemon_count(self) -> int:
        return self._native.daemon_count()

    def register_factory(self, kind: str, factory: DaemonFactory) -> None:
        """Register ``factory`` under ``kind``. Registering a kind twice
        raises :class:`DaemonError`."""
        self._native.register_factory(kind, factory)

    def spawn(
        self, kind: str, identity: Any, config: Optional[DaemonHostConfig] = None
    ) -> DaemonHandle:
        """Spawn a daemon of ``kind`` under ``identity`` (a ``net.Identity``)."""
        return self._native.spawn(kind, identity, config)

    def spawn_from_snapshot(
        self,
        kind: str,
        identity: Any,
        snapshot_bytes: bytes,
        config: Optional[DaemonHostConfig] = None,
    ) -> DaemonHandle:
        """Like :meth:`spawn`, restoring state from ``snapshot_bytes``
        before any event lands."""
        return self._native.spawn_from_snapshot(kind, identity, snapshot_bytes, config)

    def stop(self, origin_hash: int) -> None:
        self._native.stop(origin_hash)

    def snapshot(self, origin_hash: int) -> Optional[bytes]:
        """An opaque snapshot (the core's ``StateSnapshot`` encoding, which
        wraps what the daemon's ``snapshot()`` returned), or ``None`` for a
        stateless daemon. Feed it back through :meth:`spawn_from_snapshot`;
        it is not the daemon's own bytes."""
        return self._native.snapshot(origin_hash)

    def deliver(self, origin_hash: int, event: CausalEvent) -> List[bytes]:
        """Deliver one event; returns the daemon's output payloads."""
        return self._native.deliver(origin_hash, event)

    def start_migration(
        self, origin_hash: int, source_node: int, target_node: int
    ) -> MigrationHandle:
        return self._native.start_migration(origin_hash, source_node, target_node)

    def start_migration_with(
        self,
        origin_hash: int,
        source_node: int,
        target_node: int,
        opts: MigrationOptions,
    ) -> MigrationHandle:
        return self._native.start_migration_with(origin_hash, source_node, target_node, opts)

    def expect_migration(
        self, kind: str, origin_hash: int, config: Optional[DaemonHostConfig] = None
    ) -> None:
        """On the target: declare that ``origin_hash`` of ``kind`` will
        migrate here."""
        self._native.expect_migration(kind, origin_hash, config)

    def register_migration_target_identity(
        self, kind: str, identity: Any, config: Optional[DaemonHostConfig] = None
    ) -> None:
        """On the target, for a migration without an identity envelope
        (``transport_identity=False``)."""
        self._native.register_migration_target_identity(kind, identity, config)

    def migration_phase(self, origin_hash: int) -> Optional[MigrationPhase]:
        return self._native.migration_phase(origin_hash)

    def __repr__(self) -> str:
        return f"net_sdk.compute.{self._native!r}"


def _native_runtime(rt: Any) -> Any:
    """The wheel's runtime behind ``rt``: accepts this module's
    :class:`DaemonRuntime` or a raw ``net.DaemonRuntime``."""
    if isinstance(rt, DaemonRuntime):
        return rt.native
    if isinstance(rt, _NativeDaemonRuntime):
        return rt
    raise TypeError(
        "expected a net_sdk.compute.DaemonRuntime or a net.DaemonRuntime, "
        f"got {type(rt).__name__}"
    )


__all__ = [
    "CausalEvent",
    "DaemonError",
    "DaemonFactory",
    "DaemonHandle",
    "DaemonHostConfig",
    "DaemonRuntime",
    "MeshDaemon",
    "MigrationError",
    "MigrationErrorKind",
    "MigrationHandle",
    "MigrationOptions",
    "MigrationPhase",
    "migration_error_kind",
]
