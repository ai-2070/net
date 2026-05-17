"""Deck SDK — operator-side ergonomic Python wrapper.

Sits on top of the PyO3 binding at ``net._net``. Adds:

- :class:`DeckClient` constructor that takes a
  :class:`net_sdk.meshos.MeshOsDaemonSdk` and an
  :class:`OperatorIdentity`.
- Automatic JSON parsing for snapshot streams + ``status()``.
- :class:`DeckSdkError` re-export with ``.kind`` helper.

Slice 1 covers client + admin (all 9 methods) + snapshot/status
streams + operator identity. Audit / logs / failures land in
slice 2; ICE in slice 3.

Example::

    import net_sdk.deck as deck
    import net_sdk.meshos as meshos

    with meshos.MeshOsDaemonSdk.start() as sdk:
        identity = deck.OperatorIdentity.generate()
        client = deck.DeckClient(sdk, identity)
        commit = client.admin.enter_maintenance(node=0xABCD, drain_for_ms=600_000)
        print(f"commit_id={commit['commit_id']:#x}")
        for snap in client.snapshots():
            # snap is a parsed dict
            print(snap)
            break
"""

from __future__ import annotations

import json
from typing import Any, Iterator, Literal, Optional, Union

try:
    from net import (  # type: ignore[attr-defined]
        DeckAdminCommands as _RawAdmin,
        DeckClient as _RawClient,
        DeckSdkError,
        DeckSnapshotStream as _RawSnapshotStream,
        DeckStatusSummaryStream as _RawStatusStream,
        OperatorIdentity,
        deck_sdk_error_kind,
    )
    # Slice 2 — present iff the wheel includes the audit/logs/
    # failures surface. Optional so slice-1-only wheels (early
    # CI builds) still import cleanly.
    try:
        from net import (  # type: ignore[attr-defined]
            AuditQuery as _RawAuditQuery,
            AuditStream as _RawAuditStream,
            FailureStream as _RawFailureStream,
            LogStream as _RawLogStream,
        )

        _HAS_SLICE_2 = True
    except ImportError:  # pragma: no cover
        _HAS_SLICE_2 = False
    # Operator-policy surface — `OperatorRegistry` + `AdminVerifier`.
    # Try-import so older wheels still load. Re-exported under the
    # plain names from this module.
    try:
        from net import (  # type: ignore[attr-defined]
            DeckAdminVerifier as AdminVerifier,
            DeckOperatorRegistry as OperatorRegistry,
        )

        _HAS_VERIFIER = True
    except ImportError:  # pragma: no cover
        _HAS_VERIFIER = False
except ImportError as e:  # pragma: no cover
    raise ImportError(
        "Deck SDK symbols not present in `net._net`. Rebuild the "
        "wheel with `--features deck`, e.g. `maturin develop "
        "--features deck`."
    ) from e

# Re-export so users can `import net_sdk.deck as deck` and use
# `deck.OperatorIdentity` without reaching for the binding module.
__all__ = [
    "DeckClient",
    "AdminCommands",
    "SnapshotStream",
    "StatusSummaryStream",
    "DeckSdkError",
    "OperatorIdentity",
    "deck_sdk_error_kind",
    "ChainCommit",
    "StatusSummary",
    # Slice 2.
    "AuditQuery",
    "LogStream",
    "FailureStream",
    "LogRecord",
    "FailureRecord",
    "AdminAuditRecord",
    "LogLevel",
    "LogFilter",
    # Slice 3 — ICE.
    "IceCommands",
    "IceProposal",
    "SimulatedIceProposal",
    "OperatorSignature",
    "BlastRadius",
    "AvoidScope",
    # Operator-policy verifier surface.
    "OperatorRegistry",
    "AdminVerifier",
]


# =========================================================================
# Typed-dict envelopes
# =========================================================================


# `ChainCommit` shape returned by every admin commit. Documented
# here as a `TypedDict` for editor/checker support; the binding
# returns a plain ``dict`` with these keys.
from typing import TypedDict


class ChainCommit(TypedDict):
    commit_id: int
    operator_id: int
    event_kind: str
    committed_at_ms: int


class PeerCounts(TypedDict):
    healthy: int
    degraded: int
    unreachable: int
    unknown: int


class DaemonCounts(TypedDict):
    running: int
    starting: int
    stopping: int
    stopped: int
    backing_off: int
    crash_looping: int


class StatusSummary(TypedDict):
    peers: PeerCounts
    daemons: DaemonCounts
    replica_chains: int
    avoid_list_entries: int
    recently_emitted_count: int
    recent_failure_count: int
    admin_audit_ring_depth: int
    freeze_remaining_ms: Optional[int]
    local_maintenance_active: bool


# =========================================================================
# Slice 2 — typed envelopes
# =========================================================================


LogLevel = Literal["trace", "debug", "info", "warn", "error"]


class LogFilter(TypedDict, total=False):
    """Subset filter accepted by :meth:`DeckClient.subscribe_logs`.

    Every key is optional — missing keys match every record.
    """

    min_level: LogLevel
    daemon_id: int
    node_id: int
    since_seq: int


class LogRecord(TypedDict):
    """One log line. `daemon_id` / `node_id` are ``None`` for
    substrate-level messages."""

    seq: int
    ts_ms: int
    level: LogLevel
    daemon_id: Optional[int]
    node_id: Optional[int]
    message: str


class FailureRecord(TypedDict):
    """One executor-failure record. `source` looks like
    ``"daemon:foo"`` / ``"drain:node_x"``; `reason` is the
    operator-readable detail."""

    seq: int
    source: str
    reason: str
    recorded_at_ms: int


# `AdminAuditRecord` carries a nested `AdminEvent` enum which is
# JSON-shaped on the binding side — type as a generic mapping for
# now. Per-variant typed wrappers can land in a future slice when
# consumers ask.
AdminAuditRecord = dict[str, Any]


# =========================================================================
# AdminCommands — thin pass-through
# =========================================================================


class AdminCommands:
    """Operator-side admin event surface. Each method commits an
    ``AdminEvent`` variant and returns the resulting
    :data:`ChainCommit` for audit correlation. Phase 1 substrate
    constraint: non-signing today.
    """

    __slots__ = ("_raw",)

    def __init__(self, raw: _RawAdmin) -> None:
        self._raw = raw

    def drain(self, node: int, drain_for_ms: int) -> ChainCommit:
        return self._raw.drain(node, drain_for_ms)  # type: ignore[return-value]

    def enter_maintenance(
        self, node: int, drain_for_ms: Optional[int] = None
    ) -> ChainCommit:
        return self._raw.enter_maintenance(node, drain_for_ms=drain_for_ms)  # type: ignore[return-value]

    def exit_maintenance(self, node: int) -> ChainCommit:
        return self._raw.exit_maintenance(node)  # type: ignore[return-value]

    def cordon(self, node: int) -> ChainCommit:
        return self._raw.cordon(node)  # type: ignore[return-value]

    def uncordon(self, node: int) -> ChainCommit:
        return self._raw.uncordon(node)  # type: ignore[return-value]

    def drop_replicas(self, node: int, chains: list[int]) -> ChainCommit:
        return self._raw.drop_replicas(node, chains)  # type: ignore[return-value]

    def invalidate_placement(self, node: int) -> ChainCommit:
        return self._raw.invalidate_placement(node)  # type: ignore[return-value]

    def restart_all_daemons(self, node: int) -> ChainCommit:
        return self._raw.restart_all_daemons(node)  # type: ignore[return-value]

    def clear_avoid_list(self, node: int) -> ChainCommit:
        return self._raw.clear_avoid_list(node)  # type: ignore[return-value]


# =========================================================================
# Streams — JSON parsing wrappers
# =========================================================================


class SnapshotStream:
    """Sync iterator over :class:`MeshOsSnapshot` dicts. The PyO3
    layer emits each snapshot as a JSON string; we parse here so
    the caller gets a native dict. Cadence =
    ``DeckClientConfig.snapshot_poll_interval_ms`` (default 100 ms).
    """

    __slots__ = ("_raw",)

    def __init__(self, raw: _RawSnapshotStream) -> None:
        self._raw = raw

    def __iter__(self) -> Iterator[dict[str, Any]]:
        return self

    def __next__(self) -> dict[str, Any]:
        return json.loads(next(self._raw))

    def close(self) -> None:
        """Close the stream. Subsequent ``__next__`` raises StopIteration."""
        self._raw.close()


class StatusSummaryStream:
    """Sync iterator over :data:`StatusSummary` dicts. Cadence
    matches :class:`SnapshotStream`."""

    __slots__ = ("_raw",)

    def __init__(self, raw: _RawStatusStream) -> None:
        self._raw = raw

    def __iter__(self) -> Iterator[StatusSummary]:
        return self

    def __next__(self) -> StatusSummary:
        return next(self._raw)  # already a dict from the binding

    def close(self) -> None:
        self._raw.close()


# =========================================================================
# Slice 2 — Log + Failure + Audit streams
# =========================================================================


class LogStream:
    """Sync iterator over :class:`LogRecord` dicts. Filter applied
    at the substrate side; consumers can pass an empty filter to
    tail every record on the runtime's log ring."""

    __slots__ = ("_raw",)

    def __init__(self, raw: Any) -> None:
        self._raw = raw

    def __iter__(self) -> Iterator[LogRecord]:
        return self

    def __next__(self) -> LogRecord:
        return next(self._raw)  # typed dict from the binding

    def close(self) -> None:
        self._raw.close()


class FailureStream:
    """Sync iterator over :class:`FailureRecord` dicts. Tail starts
    at the `since_seq + 1` watermark passed to
    :meth:`DeckClient.subscribe_failures`."""

    __slots__ = ("_raw",)

    def __init__(self, raw: Any) -> None:
        self._raw = raw

    def __iter__(self) -> Iterator[FailureRecord]:
        return self

    def __next__(self) -> FailureRecord:
        return next(self._raw)

    def close(self) -> None:
        self._raw.close()


class _AuditStreamWrapper:
    """Sync iterator that JSON-parses raw audit records emitted by
    the binding's `AuditStream`."""

    __slots__ = ("_raw",)

    def __init__(self, raw: Any) -> None:
        self._raw = raw

    def __iter__(self) -> Iterator[AdminAuditRecord]:
        return self

    def __next__(self) -> AdminAuditRecord:
        return json.loads(next(self._raw))

    def close(self) -> None:
        self._raw.close()


class AuditQuery:
    """Fluent admin-audit query builder. Chain filter methods
    before calling :meth:`collect` (eager list) or :meth:`stream`
    (sync iterator).

    Example::

        records = (client.audit()
                       .recent(100)
                       .by_operator(op_id)
                       .force_only()
                       .collect())

        for record in client.audit().since(last_seq).stream():
            handle(record)
    """

    __slots__ = ("_raw",)

    def __init__(self, raw: Any) -> None:
        self._raw = raw

    def recent(self, limit: int) -> "AuditQuery":
        self._raw.recent(limit)
        return self

    def by_operator(self, operator_id: int) -> "AuditQuery":
        self._raw.by_operator(operator_id)
        return self

    def between(self, start_ms: int, end_ms: int) -> "AuditQuery":
        self._raw.between(start_ms, end_ms)
        return self

    def force_only(self) -> "AuditQuery":
        self._raw.force_only()
        return self

    def since(self, seq: int) -> "AuditQuery":
        self._raw.since(seq)
        return self

    def collect(self) -> list[AdminAuditRecord]:
        """Eager — returns a list of audit records (JSON-parsed
        into native dicts)."""
        return [json.loads(s) for s in self._raw.collect()]

    def stream(self) -> _AuditStreamWrapper:
        """Returns a sync iterator over audit records."""
        return _AuditStreamWrapper(self._raw.stream())


# =========================================================================
# DeckClient — wraps the raw napi client + auto-parses snapshots
# =========================================================================


class DeckClient:
    """Operator-side client to the cluster's admin / snapshot /
    status surfaces.

    Two construction paths:

    - Against an externally-managed SDK (default `__init__`)::

        sdk = meshos.MeshOsDaemonSdk.start()
        identity = deck.OperatorIdentity.generate()
        client = deck.DeckClient(sdk, identity)

    - Standalone with a private supervisor (`from_seed`)::

        with deck.DeckClient.from_seed(b"\\x42" * 32) as client:
            ...  # supervisor drained on __exit__

    Context-manager support (`with` / `__enter__` / `__exit__`)
    drains the supervisor on scope exit when the client owns one
    (i.e. constructed via `from_seed`). For `__init__`-built
    clients, context exit is a no-op — the caller owns the
    externally-managed SDK's lifecycle.
    """

    __slots__ = ("_raw",)

    def __init__(
        self,
        meshos_sdk: Any,
        identity: OperatorIdentity,
        config: Optional[dict[str, Any]] = None,
    ) -> None:
        # `meshos_sdk` is a `MeshOsDaemonSdk` (the ergonomic wrapper
        # at `net_sdk.meshos.MeshOsDaemonSdk`); we reach the raw
        # napi class via its `_raw` slot.
        raw_sdk = getattr(meshos_sdk, "_raw", meshos_sdk)
        self._raw = _RawClient.from_meshos(raw_sdk, identity, config)

    @classmethod
    def from_seed(
        cls,
        operator_seed: bytes,
        meshos_config: Optional[dict[str, Any]] = None,
        deck_config: Optional[dict[str, Any]] = None,
    ) -> "DeckClient":
        """Construct a standalone client owning a private MeshOS
        supervisor runtime, mirroring the cdylib's
        ``net_deck_client_new`` (operator-only mode).

        ``operator_seed`` must be exactly 32 bytes of ed25519 seed
        material. The supervisor is drained on :meth:`close` or
        context-manager exit; if neither is called the runtime
        releases on GC.
        """
        inst = cls.__new__(cls)
        inst._raw = _RawClient(operator_seed, meshos_config, deck_config)
        return inst

    def close(self) -> None:
        """Drain the private supervisor runtime if the client
        owns one (constructed via :meth:`from_seed`). No-op
        otherwise. Idempotent — calling twice doesn't raise."""
        self._raw.close()

    def __enter__(self) -> "DeckClient":
        return self

    def __exit__(self, exc_type, exc_value, traceback) -> Literal[False]:
        self.close()
        return False

    def identity(self) -> OperatorIdentity:
        return self._raw.identity()

    @property
    def admin(self) -> AdminCommands:
        return AdminCommands(self._raw.admin)

    def status(self) -> dict[str, Any]:
        """One-shot read of the latest :class:`MeshOsSnapshot`,
        parsed into a native dict from the binding's JSON form."""
        return json.loads(self._raw.status())

    def status_summary(self) -> StatusSummary:
        """One-shot read of the rolled-up :data:`StatusSummary` —
        peer counts, daemon counts, freeze remaining, etc."""
        return self._raw.status_summary()  # type: ignore[return-value]

    def snapshots(self) -> SnapshotStream:
        """Live :class:`MeshOsSnapshot` stream as a sync iterator
        over native dicts."""
        return SnapshotStream(self._raw.snapshots())

    def status_summary_stream(self) -> StatusSummaryStream:
        """Live :data:`StatusSummary` stream as a sync iterator
        over native dicts."""
        return StatusSummaryStream(self._raw.status_summary_stream())

    # =====================================================================
    # Slice 2 — audit + logs + failures
    # =====================================================================

    def audit(self) -> AuditQuery:
        """Fluent admin-audit query builder. Chain
        :meth:`AuditQuery.recent` / :meth:`by_operator` /
        :meth:`between` / :meth:`force_only` / :meth:`since` and
        call :meth:`collect` (eager list) or :meth:`stream`."""
        return AuditQuery(self._raw.audit())

    def subscribe_logs(
        self, filter: Optional[LogFilter] = None
    ) -> LogStream:
        """Subscribe to the runtime's log ring. ``filter`` is an
        optional dict with keys ``min_level`` (str),
        ``daemon_id`` (int), ``node_id`` (int), ``since_seq``
        (int). Missing keys match every record."""
        # Cast the LogFilter TypedDict to a plain dict[str, Any] —
        # the pyo3 binding accepts the underlying mapping shape.
        raw_filter = dict(filter) if filter is not None else None
        return LogStream(self._raw.subscribe_logs(raw_filter))

    def subscribe_failures(self, since_seq: int = 0) -> FailureStream:
        """Subscribe to the executor-failure ring starting at
        ``since_seq + 1``. Pass ``0`` to start from whatever is
        still in the ring."""
        return FailureStream(self._raw.subscribe_failures(since_seq))

    # =====================================================================
    # Slice 3 — ICE break-glass surface
    # =====================================================================

    @property
    def ice(self) -> "IceCommands":
        """Operator-side break-glass surface. Every method
        constructs an :class:`IceProposal` that must be
        :meth:`IceProposal.simulate`-d before commit."""
        return IceCommands(self._raw.ice)

    def __repr__(self) -> str:
        return repr(self._raw)


# =========================================================================
# Slice 3 — ICE wrappers + typed envelopes
# =========================================================================


AvoidScopeGlobal = TypedDict("AvoidScopeGlobal", {"kind": Literal["global"]})
AvoidScopeLocal = TypedDict("AvoidScopeLocal", {"kind": Literal["local"], "node": int})
AvoidScopeOnPeer = TypedDict("AvoidScopeOnPeer", {"kind": Literal["on_peer"], "peer": int})
AvoidScope = Union[AvoidScopeGlobal, AvoidScopeLocal, AvoidScopeOnPeer]


class OperatorSignature(TypedDict):
    """Signature pair carried by ICE commits."""

    operator_id: int
    signature: bytes


# `BlastRadius` is JSON-shaped at the binding boundary. Type
# loosely; the substrate's serde shape is the authoritative wire
# form.
BlastRadius = dict[str, Any]


class IceCommands:
    """Operator-side break-glass surface. Each method returns an
    :class:`IceProposal` that must be `.simulate()`-d before
    `.commit()`."""

    __slots__ = ("_raw",)

    def __init__(self, raw: Any) -> None:
        self._raw = raw

    def freeze_cluster(self, ttl_ms: int) -> "IceProposal":
        return IceProposal(self._raw.freeze_cluster(ttl_ms))

    def flush_avoid_lists(self, scope: AvoidScope) -> "IceProposal":
        return IceProposal(self._raw.flush_avoid_lists(dict(scope)))

    def force_evict_replica(self, chain: int, victim: int) -> "IceProposal":
        return IceProposal(self._raw.force_evict_replica(chain, victim))

    def force_restart_daemon(self, id: int, name: str) -> "IceProposal":
        return IceProposal(self._raw.force_restart_daemon(id, name))

    def force_cutover(self, chain: int, target: int) -> "IceProposal":
        return IceProposal(self._raw.force_cutover(chain, target))

    def kill_migration(self, migration: int) -> "IceProposal":
        return IceProposal(self._raw.kill_migration(migration))

    def thaw_cluster(self) -> "IceProposal":
        return IceProposal(self._raw.thaw_cluster())


class IceProposal:
    """Pre-simulation ICE proposal. No ``commit`` method —
    typestate enforces ``simulate()`` first."""

    __slots__ = ("_raw",)

    def __init__(self, raw: Any) -> None:
        self._raw = raw

    @property
    def issued_at_ms(self) -> int:
        return self._raw.issued_at_ms

    def simulate(self) -> "SimulatedIceProposal":
        """Pre-execution preview. Consumes the proposal —
        subsequent calls raise ``DeckSdkError(kind="already_simulated")``."""
        return SimulatedIceProposal(self._raw.simulate())


class SimulatedIceProposal:
    """A simulated ICE proposal. Carries the substrate's blast
    radius preview; call :meth:`commit` with operator signatures
    to publish."""

    __slots__ = ("_raw",)

    def __init__(self, raw: Any) -> None:
        self._raw = raw

    @property
    def issued_at_ms(self) -> int:
        return self._raw.issued_at_ms

    def blast_radius(self) -> BlastRadius:
        """Pre-execution preview, parsed from the binding's JSON."""
        return json.loads(self._raw.blast_radius())

    def blast_hash(self) -> bytes:
        """Blake3 digest of the blast radius. Signers must cover
        this exact hash."""
        return self._raw.blast_hash()

    def commit(self, signatures: list[OperatorSignature]) -> ChainCommit:
        """Commit with the supplied operator signatures. Consumes
        the simulated proposal."""
        return self._raw.commit([dict(s) for s in signatures])  # type: ignore[return-value]
