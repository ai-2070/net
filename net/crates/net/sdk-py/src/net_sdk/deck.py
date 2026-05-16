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
from typing import Any, Iterator, Literal, Optional

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
# DeckClient — wraps the raw napi client + auto-parses snapshots
# =========================================================================


class DeckClient:
    """Operator-side client to the cluster's admin / snapshot /
    status surfaces.

    Construct against a running :class:`net_sdk.meshos.MeshOsDaemonSdk`::

        sdk = meshos.MeshOsDaemonSdk.start()
        identity = deck.OperatorIdentity.generate()
        client = deck.DeckClient(sdk, identity)
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

    def __repr__(self) -> str:
        return repr(self._raw)
