"""Tests for the groups surface — Stage 3 of SDK_GROUPS_SURFACE_PLAN.md.

Mirrors `sdk-ts/test/groups.test.ts` and the Rust SDK
`groups_surface.rs`. Covers ReplicaGroup / ForkGroup / StandbyGroup
spawn, routing, scaling, health, and error paths.
"""

from __future__ import annotations


import pytest

# The groups surface is only present when the native module was
# built with the `groups` feature (see
# `bindings/python/Cargo.toml`). A CI matrix that omits it — e.g.
# the `--features net,cortex,compute` compute-only build — still
# imports the `net` package successfully but without `ForkGroup`
# / `ReplicaGroup` / `StandbyGroup` / `GroupError`. Collect-time
# failure there is a false negative; tolerate it by skipping the
# whole module when any of the groups symbols are missing.
try:
    from net import (  # noqa: I001 — conditional to match feature gate
        DaemonError,
        DaemonRuntime,
        ForkGroup,
        GroupError,
        Identity,
        NetMesh,
        ReplicaGroup,
        StandbyGroup,
        group_error_kind,
    )
except ImportError as _groups_import_err:
    pytestmark = pytest.mark.skip(
        reason=(
            "net bindings built without the `groups` feature; rebuild with "
            "`maturin develop --features net,cortex,compute,groups` to run "
            f"these tests ({_groups_import_err})"
        )
    )

PSK = "42" * 32

def _next_port() -> str:
    """A bind address the OS picks a port for.

    Port ``0`` asks the kernel for a free ephemeral port. A fixed
    counter only avoids collisions with the rest of *this* suite, and
    CI hit ``EADDRINUSE`` on a port no test here claims. A caller that
    has to dial the node reads ``local_addr`` back after construction.
    """
    return "127.0.0.1:0"


def _mesh() -> NetMesh:
    return NetMesh(bind_addr=_next_port(), psk=PSK)


class NoopDaemon:
    """Minimal stateless daemon used as the factory target for
    every group in this suite. Factory lookups / invocation go
    through the existing compute-feature Python::attach machinery;
    the groups layer just asks for a fresh instance per member."""

    name = "noop"

    def process(self, _event):  # type: ignore[no-untyped-def]
        return []


def _runtime_with_peers(extra_peers: int) -> tuple[DaemonRuntime, NetMesh]:
    """Build a started DaemonRuntime with `extra_peers` synthetic
    capability entries so `place_with_spread` has enough
    candidates for groups with > 1 member."""
    mesh = _mesh()
    for i in range(1, extra_peers + 1):
        # Synthetic node IDs above 0x1000_0000 so they never
        # collide with real node IDs derived from ed25519 pubkeys.
        mesh._test_inject_synthetic_peer(0x1000_0000_0000_0000 + i)
    rt = DaemonRuntime(mesh)
    rt.register_factory("noop", lambda: NoopDaemon())
    rt.start()
    return rt, mesh


def _seed(byte: int) -> bytes:
    return bytes([byte]) * 32


# -------------------------------------------------------------------------
# ReplicaGroup
# -------------------------------------------------------------------------


def test_replica_group_spawn_registers_members_and_reports_healthy() -> None:
    rt, mesh = _runtime_with_peers(3)
    try:
        group = ReplicaGroup.spawn(
            rt,
            "noop",
            replica_count=3,
            group_seed=_seed(0x11),
            lb_strategy="round-robin",
        )
        assert group.replica_count == 3
        assert group.healthy_count == 3
        assert group.health["status"] == "healthy"
        assert rt.daemon_count() == 3
        replicas = group.replicas
        assert len(replicas) == 3
        for r in replicas:
            assert r["healthy"] is True
            assert r["origin_hash"] != 0
    finally:
        rt.shutdown()
        mesh.shutdown()


def test_replica_group_route_event_returns_live_member_origin() -> None:
    rt, mesh = _runtime_with_peers(3)
    try:
        group = ReplicaGroup.spawn(
            rt,
            "noop",
            replica_count=3,
            group_seed=_seed(0x22),
            lb_strategy="consistent-hash",
        )
        live = {m["origin_hash"] for m in group.replicas}
        for i in range(30):
            origin = group.route_event(ctx={"routing_key": f"req-{i}"})
            assert origin in live
    finally:
        rt.shutdown()
        mesh.shutdown()


def test_replica_group_scale_up_and_down_tracks_daemon_count() -> None:
    rt, mesh = _runtime_with_peers(5)
    try:
        group = ReplicaGroup.spawn(
            rt,
            "noop",
            replica_count=2,
            group_seed=_seed(0x33),
            lb_strategy="round-robin",
        )
        assert rt.daemon_count() == 2

        group.scale_to(5)
        assert group.replica_count == 5
        assert rt.daemon_count() == 5

        group.scale_to(1)
        assert group.replica_count == 1
        assert rt.daemon_count() == 1
    finally:
        rt.shutdown()
        mesh.shutdown()


def test_replica_group_spawn_before_start_errors_not_ready() -> None:
    mesh = _mesh()
    rt = DaemonRuntime(mesh)
    rt.register_factory("noop", lambda: NoopDaemon())
    # Intentionally skip rt.start()
    try:
        with pytest.raises(GroupError) as exc_info:
            ReplicaGroup.spawn(
                rt,
                "noop",
                replica_count=2,
                group_seed=_seed(0x44),
                lb_strategy="round-robin",
            )
        assert isinstance(exc_info.value, DaemonError)
        assert group_error_kind(exc_info.value) == "not-ready"
    finally:
        rt.shutdown()
        mesh.shutdown()


def test_replica_group_spawn_unknown_kind_errors_factory_not_found() -> None:
    rt, mesh = _runtime_with_peers(2)
    try:
        with pytest.raises(GroupError) as exc_info:
            ReplicaGroup.spawn(
                rt,
                "never-registered",
                replica_count=2,
                group_seed=_seed(0x55),
                lb_strategy="round-robin",
            )
        assert group_error_kind(exc_info.value) == "factory-not-found"
    finally:
        rt.shutdown()
        mesh.shutdown()


def test_replica_group_invalid_seed_errors() -> None:
    rt, mesh = _runtime_with_peers(2)
    try:
        with pytest.raises(DaemonError) as exc_info:
            ReplicaGroup.spawn(
                rt,
                "noop",
                replica_count=2,
                group_seed=b"too-short",
                lb_strategy="round-robin",
            )
        # Invalid-config may surface as GroupError or the base
        # DaemonError depending on where the check fires. The
        # important thing is the message identifies the cause.
        assert "group_seed must be 32 bytes" in str(exc_info.value)
    finally:
        rt.shutdown()
        mesh.shutdown()


# -------------------------------------------------------------------------
# ForkGroup
# -------------------------------------------------------------------------


def test_fork_group_produces_unique_origins_with_verifiable_lineage() -> None:
    rt, mesh = _runtime_with_peers(4)
    try:
        group = ForkGroup.fork(
            rt,
            "noop",
            parent_origin=0xABCD_EF01,
            fork_seq=42,
            fork_count=3,
            lb_strategy="round-robin",
        )
        assert group.fork_count == 3
        assert group.parent_origin == 0xABCD_EF01
        assert group.fork_seq == 42
        assert group.verify_lineage() is True
        origins = {m["origin_hash"] for m in group.members}
        assert len(origins) == 3
        assert len(group.fork_records) == 3
    finally:
        rt.shutdown()
        mesh.shutdown()


def test_fork_group_fork_before_start_errors_not_ready() -> None:
    mesh = _mesh()
    rt = DaemonRuntime(mesh)
    rt.register_factory("noop", lambda: NoopDaemon())
    try:
        with pytest.raises(GroupError) as exc_info:
            ForkGroup.fork(
                rt,
                "noop",
                parent_origin=0x1234,
                fork_seq=1,
                fork_count=2,
                lb_strategy="round-robin",
            )
        assert group_error_kind(exc_info.value) == "not-ready"
    finally:
        rt.shutdown()
        mesh.shutdown()


# -------------------------------------------------------------------------
# StandbyGroup
# -------------------------------------------------------------------------


def test_standby_group_member_zero_is_active() -> None:
    rt, mesh = _runtime_with_peers(3)
    try:
        group = StandbyGroup.spawn(
            rt,
            "noop",
            member_count=3,
            group_seed=_seed(0x77),
        )
        assert group.member_count == 3
        assert group.standby_count == 2
        assert group.active_index == 0
        assert group.active_healthy is True
        assert group.active_origin != 0
        assert group.buffered_event_count == 0
        assert group.member_role(0) == "active"
        assert group.member_role(1) == "standby"
        assert group.member_role(99) is None
    finally:
        rt.shutdown()
        mesh.shutdown()


def test_standby_group_member_count_below_two_rejected() -> None:
    rt, mesh = _runtime_with_peers(3)
    try:
        with pytest.raises(GroupError) as exc_info:
            StandbyGroup.spawn(
                rt,
                "noop",
                member_count=1,
                group_seed=_seed(0x88),
            )
        assert group_error_kind(exc_info.value) == "invalid-config"
    finally:
        rt.shutdown()
        mesh.shutdown()


def test_standby_group_unknown_kind_errors_factory_not_found() -> None:
    rt, mesh = _runtime_with_peers(3)
    try:
        with pytest.raises(GroupError) as exc_info:
            StandbyGroup.spawn(
                rt,
                "never-registered",
                member_count=2,
                group_seed=_seed(0x99),
            )
        assert group_error_kind(exc_info.value) == "factory-not-found"
    finally:
        rt.shutdown()
        mesh.shutdown()
