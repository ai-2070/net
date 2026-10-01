"""Groups — HA and scaling overlays over :mod:`net_sdk.compute` daemons.

Three shapes, each built on a started :class:`net_sdk.compute.DaemonRuntime`
(or the wheel's raw ``net.DaemonRuntime``):

- :class:`ReplicaGroup`: N interchangeable copies with load-balanced routing
  and auto-replacement on node failure.
- :class:`ForkGroup`: N daemons forked from one parent at a sequence point,
  with verifiable lineage.
- :class:`StandbyGroup`: one active member plus warm standbys kept in sync
  by snapshot; promote on failure.

Every class wraps the wheel's group (available as ``.native``) and adds the
typed vocabulary the TypeScript SDK ships. Routing returns an
``origin_hash``; hand it to ``runtime.deliver(...)``.

Example:
    >>> from net_sdk.compute import DaemonRuntime
    >>> from net_sdk.groups import ReplicaGroup
    >>> rt = DaemonRuntime(node)
    >>> rt.register_factory("echo", Echo)
    >>> rt.start()
    >>> group = ReplicaGroup.spawn(rt, "echo", 3, b"\\x01" * 32, "round-robin")
    >>> target = group.route_event({"routing_key": "user-42"})
    >>> rt.deliver(target, event)

Present iff the wheel was built with the ``groups`` feature (the default
build is).
"""

from __future__ import annotations

from typing import Any, List, Literal, Optional, TypedDict

from net import (  # type: ignore[attr-defined]
    ForkGroup as _NativeForkGroup,
    GroupError,
    ReplicaGroup as _NativeReplicaGroup,
    StandbyGroup as _NativeStandbyGroup,
    group_error_kind,
)

from net_sdk.compute import DaemonHostConfig, _native_runtime

LoadBalanceStrategy = Literal[
    "round-robin", "consistent-hash", "least-load", "least-connections", "random"
]
"""Exactly the strings the wheel's ``parse_strategy`` accepts; anything else
raises :class:`GroupError` (``invalid-config``)."""

GroupErrorKind = Literal[
    "not-ready",
    "factory-not-found",
    "daemon",
    "no-healthy-member",
    "placement-failed",
    "registry-failed",
    "invalid-config",
]
"""What :func:`group_error_kind` returns. Taken from the wheel's
``group_err_str`` / ``core_group_err_str``."""

GroupStatus = Literal["healthy", "degraded", "dead"]


class GroupHealth(TypedDict, total=False):
    """A group's ``health``. ``healthy`` / ``total`` are present only when
    ``status == "degraded"``."""

    status: GroupStatus
    healthy: int
    total: int


class GroupMemberInfo(TypedDict):
    """One entry of a group's ``replicas`` / ``members``."""

    index: int
    origin_hash: int
    node_id: int
    entity_id: bytes
    healthy: bool


class ForkRecord(TypedDict):
    """One entry of :attr:`ForkGroup.fork_records`."""

    original_origin: int
    forked_origin: int
    fork_seq: int
    from_snapshot_seq: int


class RequestContext(TypedDict, total=False):
    """Routing hints for ``route_event``. ``routing_key`` drives
    ``consistent-hash``; ``session_id`` pins a session to a member."""

    routing_key: str
    session_id: str
    request_id: str


class ReplicaGroup:
    """N interchangeable replicas. Build with :meth:`spawn`."""

    def __init__(self, native: Any) -> None:
        self._native = native

    @classmethod
    def spawn(
        cls,
        runtime: Any,
        kind: str,
        replica_count: int,
        group_seed: bytes,
        lb_strategy: LoadBalanceStrategy,
        host_config: Optional[DaemonHostConfig] = None,
    ) -> "ReplicaGroup":
        """Spawn ``replica_count`` replicas of ``kind`` (registered on
        ``runtime``). ``group_seed`` (32 bytes) derives each replica's
        identity deterministically."""
        native_rt = _native_runtime(runtime)
        return cls(
            _NativeReplicaGroup.spawn(
                native_rt,
                kind,
                replica_count,
                group_seed,
                lb_strategy,
                host_config,
            )
        )

    @property
    def native(self) -> Any:
        return self._native

    def route_event(self, ctx: Optional[RequestContext] = None) -> int:
        """The ``origin_hash`` of the replica to deliver to."""
        return self._native.route_event(ctx)

    def scale_to(self, n: int) -> None:
        self._native.scale_to(n)

    def on_node_failure(self, failed_node_id: int) -> List[int]:
        """Re-spawn the replicas hosted on ``failed_node_id``; returns their
        indices."""
        return self._native.on_node_failure(failed_node_id)

    def on_node_recovery(self, recovered_node_id: int) -> None:
        self._native.on_node_recovery(recovered_node_id)

    @property
    def health(self) -> GroupHealth:
        return self._native.health

    @property
    def group_id(self) -> int:
        return self._native.group_id

    @property
    def replicas(self) -> List[GroupMemberInfo]:
        return self._native.replicas

    @property
    def replica_count(self) -> int:
        return self._native.replica_count

    @property
    def healthy_count(self) -> int:
        return self._native.healthy_count

    def __repr__(self) -> str:
        return f"net_sdk.groups.{self._native!r}"


class ForkGroup:
    """N daemons forked from one parent. Build with :meth:`fork`."""

    def __init__(self, native: Any) -> None:
        self._native = native

    @classmethod
    def fork(
        cls,
        runtime: Any,
        kind: str,
        parent_origin: int,
        fork_seq: int,
        fork_count: int,
        lb_strategy: LoadBalanceStrategy,
        host_config: Optional[DaemonHostConfig] = None,
    ) -> "ForkGroup":
        """Fork ``fork_count`` daemons of ``kind`` from ``parent_origin`` at
        ``fork_seq``."""
        native_rt = _native_runtime(runtime)
        return cls(
            _NativeForkGroup.fork(
                native_rt,
                kind,
                parent_origin,
                fork_seq,
                fork_count,
                lb_strategy,
                host_config,
            )
        )

    @property
    def native(self) -> Any:
        return self._native

    def route_event(self, ctx: Optional[RequestContext] = None) -> int:
        return self._native.route_event(ctx)

    def scale_to(self, n: int) -> None:
        self._native.scale_to(n)

    def on_node_failure(self, failed_node_id: int) -> List[int]:
        return self._native.on_node_failure(failed_node_id)

    def on_node_recovery(self, recovered_node_id: int) -> None:
        self._native.on_node_recovery(recovered_node_id)

    def verify_lineage(self) -> bool:
        """Whether every fork's recorded lineage checks out against its
        parent."""
        return self._native.verify_lineage()

    @property
    def health(self) -> GroupHealth:
        return self._native.health

    @property
    def parent_origin(self) -> int:
        return self._native.parent_origin

    @property
    def fork_seq(self) -> int:
        return self._native.fork_seq

    @property
    def fork_records(self) -> List[ForkRecord]:
        return self._native.fork_records

    @property
    def members(self) -> List[GroupMemberInfo]:
        return self._native.members

    @property
    def fork_count(self) -> int:
        return self._native.fork_count

    @property
    def healthy_count(self) -> int:
        return self._native.healthy_count

    def __repr__(self) -> str:
        return f"net_sdk.groups.{self._native!r}"


class StandbyGroup:
    """One active member plus standbys. Build with :meth:`spawn`."""

    def __init__(self, native: Any) -> None:
        self._native = native

    @classmethod
    def spawn(
        cls,
        runtime: Any,
        kind: str,
        member_count: int,
        group_seed: bytes,
        host_config: Optional[DaemonHostConfig] = None,
    ) -> "StandbyGroup":
        """Member 0 starts active; the rest start as standbys with no
        snapshot (``synced_through == 0``). ``kind`` must be stateful
        (implement ``snapshot`` / ``restore``): syncing a stateless active
        member raises :class:`GroupError` (``registry-failed``)."""
        native_rt = _native_runtime(runtime)
        return cls(
            _NativeStandbyGroup.spawn(native_rt, kind, member_count, group_seed, host_config)
        )

    @property
    def native(self) -> Any:
        return self._native

    @property
    def active_origin(self) -> int:
        """The ``origin_hash`` to deliver to."""
        return self._native.active_origin

    def sync_standbys(self) -> int:
        """Snapshot the active member into every standby."""
        return self._native.sync_standbys()

    def promote(self) -> int:
        """Promote a standby to active; returns the new active origin."""
        return self._native.promote()

    def on_node_failure(self, failed_node_id: int) -> Optional[int]:
        return self._native.on_node_failure(failed_node_id)

    def on_node_recovery(self, recovered_node_id: int) -> None:
        self._native.on_node_recovery(recovered_node_id)

    def member_role(self, index: int) -> Optional[Literal["active", "standby"]]:
        """``None`` for an out-of-range index."""
        return self._native.member_role(index)

    def synced_through(self, index: int) -> Optional[int]:
        return self._native.synced_through(index)

    @property
    def health(self) -> GroupHealth:
        return self._native.health

    @property
    def active_healthy(self) -> bool:
        return self._native.active_healthy

    @property
    def active_index(self) -> int:
        return self._native.active_index

    @property
    def buffered_event_count(self) -> int:
        return self._native.buffered_event_count

    @property
    def group_id(self) -> int:
        return self._native.group_id

    @property
    def members(self) -> List[GroupMemberInfo]:
        return self._native.members

    @property
    def member_count(self) -> int:
        return self._native.member_count

    @property
    def standby_count(self) -> int:
        return self._native.standby_count

    def __repr__(self) -> str:
        return f"net_sdk.groups.{self._native!r}"


__all__ = [
    "ForkGroup",
    "ForkRecord",
    "GroupError",
    "GroupErrorKind",
    "GroupHealth",
    "GroupMemberInfo",
    "GroupStatus",
    "LoadBalanceStrategy",
    "ReplicaGroup",
    "RequestContext",
    "StandbyGroup",
    "group_error_kind",
]
