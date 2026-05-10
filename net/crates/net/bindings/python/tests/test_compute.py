"""Tests for the compute surface — Stage 5 of SDK_COMPUTE_SURFACE_PLAN.md.

Mirrors `sdk-ts/test/compute.test.ts`. Sub-step 1 covers lifecycle only:
a Python caller can build a `DaemonRuntime` against a `NetMesh`, register
a factory (stored but not yet invoked), start the runtime, and shut it
down. Event delivery, migration, snapshot/restore land in sub-steps 2-5.
"""

from __future__ import annotations

import itertools
import threading
import time

import pytest

from net import (
    CausalEvent,
    DaemonError,
    DaemonHandle,
    DaemonRuntime,
    Identity,
    MigrationError,
    MigrationHandle,
    NetMesh,
    migration_error_kind,
)

PSK = "42" * 32

# Per-test unique ports so repeated runs don't collide on localhost.
_port_counter = itertools.count(29_400)


def _next_port() -> str:
    return f"127.0.0.1:{next(_port_counter)}"


def _mesh() -> NetMesh:
    return NetMesh(bind_addr=_next_port(), psk=PSK)


# -------------------------------------------------------------------------
# Stage 5 sub-step 1: skeleton + lifecycle
# -------------------------------------------------------------------------


def test_builds_against_mesh_and_reports_not_ready_before_start() -> None:
    mesh = _mesh()
    rt = DaemonRuntime(mesh)
    try:
        assert rt.is_ready() is False
        assert rt.daemon_count() == 0
    finally:
        rt.shutdown()
        mesh.shutdown()


def test_start_flips_to_ready_shutdown_flips_back() -> None:
    mesh = _mesh()
    rt = DaemonRuntime(mesh)
    try:
        rt.start()
        assert rt.is_ready() is True
        rt.shutdown()
        assert rt.is_ready() is False
    finally:
        mesh.shutdown()


def test_register_factory_accepts_a_python_callable() -> None:
    mesh = _mesh()
    rt = DaemonRuntime(mesh)
    try:
        rt.register_factory("echo", lambda: {"name": "echo"})
        # Sub-step 1 stores the factory but doesn't invoke it.
        # Correctness proves itself via the no-exception path here
        # and the duplicate-registration check below.
    finally:
        rt.shutdown()
        mesh.shutdown()


def test_register_factory_second_registration_of_same_kind_fails() -> None:
    mesh = _mesh()
    rt = DaemonRuntime(mesh)
    try:
        rt.register_factory("echo", lambda: {})
        with pytest.raises(DaemonError) as exc_info:
            rt.register_factory("echo", lambda: {})
        assert "already registered" in str(exc_info.value)
        assert str(exc_info.value).startswith("daemon:")
    finally:
        rt.shutdown()
        mesh.shutdown()


def test_register_factory_different_kinds_coexist() -> None:
    mesh = _mesh()
    rt = DaemonRuntime(mesh)
    try:
        rt.register_factory("echo", lambda: {})
        rt.register_factory("counter", lambda: {})
        rt.register_factory("router", lambda: {})
    finally:
        rt.shutdown()
        mesh.shutdown()


def test_shutdown_is_idempotent() -> None:
    mesh = _mesh()
    rt = DaemonRuntime(mesh)
    try:
        rt.start()
        rt.shutdown()
        # Second shutdown is a no-op.
        rt.shutdown()
    finally:
        mesh.shutdown()


def test_daemon_runtime_does_not_shut_down_underlying_mesh() -> None:
    # Shutting down the runtime tears down daemons + migration handler
    # but leaves the NetMesh alive. Caller owns the mesh lifecycle.
    #
    # Liveness check: exercise a real mesh operation (announce +
    # find_nodes self-match) rather than reading `node_id`, since
    # the node_id is a derived identifier that would still be valid
    # even if the underlying mesh runtime had been torn down.
    mesh = _mesh()
    rt = DaemonRuntime(mesh)
    try:
        rt.start()
        rt.shutdown()
        # Post-shutdown: the mesh still accepts capability
        # announcements and self-indexes them, proving the
        # capability subprotocol + local index are still wired up.
        mesh.announce_capabilities({"tags": ["post-runtime-shutdown-probe"]})
        peers = mesh.find_nodes({"require_tags": ["post-runtime-shutdown-probe"]})
        assert mesh.node_id in peers
    finally:
        mesh.shutdown()


# -------------------------------------------------------------------------
# Stage 5 sub-step 2: spawn + stop + event dispatch
# -------------------------------------------------------------------------


class EchoDaemon:
    """Trivial stateless daemon — echoes the event payload."""

    def process(self, event: CausalEvent) -> list[bytes]:
        return [event.payload]


class CounterDaemon:
    """Stateful daemon — increments on every event, exposes state via
    snapshot/restore. Factory closure captures a fresh `count = 0` per
    instance."""

    def __init__(self) -> None:
        self._count = 0

    def process(self, event: CausalEvent) -> list[bytes]:
        self._count += 1
        return [self._count.to_bytes(4, "little")]

    def snapshot(self) -> bytes:
        return self._count.to_bytes(4, "little")

    def restore(self, state: bytes) -> None:
        self._count = int.from_bytes(state, "little")


def test_spawn_returns_handle_with_origin_hash_and_entity_id() -> None:
    mesh = _mesh()
    rt = DaemonRuntime(mesh)
    try:
        rt.register_factory("echo", EchoDaemon)
        rt.start()
        ident = Identity.generate()
        handle = rt.spawn("echo", ident)
        assert isinstance(handle, DaemonHandle)
        assert handle.origin_hash == ident.origin_hash
        assert handle.entity_id == ident.entity_id
    finally:
        rt.shutdown()
        mesh.shutdown()


def test_spawn_unregistered_kind_raises() -> None:
    mesh = _mesh()
    rt = DaemonRuntime(mesh)
    try:
        rt.start()
        ident = Identity.generate()
        with pytest.raises(DaemonError) as exc_info:
            rt.spawn("never-registered", ident)
        assert "no factory registered" in str(exc_info.value)
    finally:
        rt.shutdown()
        mesh.shutdown()


class BrokenCapsDaemon:
    """Daemon whose `required_capabilities` getter raises. Used to
    pin the post-fix contract: only `AttributeError` (i.e. "the
    attribute isn't declared") is silently treated as empty caps;
    every other exception must propagate so operators see the real
    failure instead of a daemon spawning with phantom empty caps.

    Pre-fix, `extract_optional_caps` swallowed every `getattr`
    error and returned `CapabilitySet::default()` — a property
    getter that raised `RuntimeError` would result in the daemon
    being indexed with an empty cap set, silently masking
    placement-plane bugs.
    """

    @property
    def required_capabilities(self):  # type: ignore[no-untyped-def]
        raise RuntimeError("broken caps getter")

    def process(self, event: CausalEvent) -> list[bytes]:
        return [event.payload]


def test_spawn_propagates_property_getter_errors_for_capabilities() -> None:
    mesh = _mesh()
    rt = DaemonRuntime(mesh)
    try:
        rt.register_factory("broken_caps", BrokenCapsDaemon)
        rt.start()
        ident = Identity.generate()
        # The getter raising RuntimeError must surface during
        # spawn, not silently collapse into empty caps.
        with pytest.raises((DaemonError, RuntimeError)) as exc_info:
            rt.spawn("broken_caps", ident)
        assert "broken caps getter" in str(exc_info.value)
    finally:
        rt.shutdown()
        mesh.shutdown()


def test_spawn_stop_reduces_daemon_count() -> None:
    mesh = _mesh()
    rt = DaemonRuntime(mesh)
    try:
        rt.register_factory("echo", EchoDaemon)
        rt.start()
        ident = Identity.generate()
        handle = rt.spawn("echo", ident)
        assert rt.daemon_count() == 1
        rt.stop(handle.origin_hash)
        assert rt.daemon_count() == 0
    finally:
        rt.shutdown()
        mesh.shutdown()


def test_echo_daemon_round_trip_via_deliver() -> None:
    mesh = _mesh()
    rt = DaemonRuntime(mesh)
    try:
        rt.register_factory("echo", EchoDaemon)
        rt.start()
        ident = Identity.generate()
        handle = rt.spawn("echo", ident)

        payload = b"hello from python"
        event = CausalEvent(ident.origin_hash, 1, payload)
        outputs = rt.deliver(handle.origin_hash, event)
        assert len(outputs) == 1
        assert outputs[0] == payload
    finally:
        rt.shutdown()
        mesh.shutdown()


def test_counter_daemon_accumulates_state_across_deliveries() -> None:
    mesh = _mesh()
    rt = DaemonRuntime(mesh)
    try:
        rt.register_factory("counter", CounterDaemon)
        rt.start()
        ident = Identity.generate()
        handle = rt.spawn("counter", ident)

        for i in range(1, 6):
            event = CausalEvent(ident.origin_hash, i, b"")
            outputs = rt.deliver(handle.origin_hash, event)
            assert len(outputs) == 1
            assert int.from_bytes(outputs[0], "little") == i
    finally:
        rt.shutdown()
        mesh.shutdown()


def test_process_returning_multiple_buffers_is_fanout() -> None:
    class Fanout:
        def process(self, _event: CausalEvent) -> list[bytes]:
            return [b"a", b"bb", b"ccc"]

    mesh = _mesh()
    rt = DaemonRuntime(mesh)
    try:
        rt.register_factory("fanout", Fanout)
        rt.start()
        ident = Identity.generate()
        handle = rt.spawn("fanout", ident)
        outputs = rt.deliver(handle.origin_hash, CausalEvent(ident.origin_hash, 1, b""))
        assert outputs == [b"a", b"bb", b"ccc"]
    finally:
        rt.shutdown()
        mesh.shutdown()


def test_process_raising_surfaces_as_daemon_error() -> None:
    class Buggy:
        def process(self, _event: CausalEvent) -> list[bytes]:
            raise ValueError("deliberate process failure")

    mesh = _mesh()
    rt = DaemonRuntime(mesh)
    try:
        rt.register_factory("buggy", Buggy)
        rt.start()
        ident = Identity.generate()
        handle = rt.spawn("buggy", ident)
        with pytest.raises(DaemonError) as exc_info:
            rt.deliver(handle.origin_hash, CausalEvent(ident.origin_hash, 1, b""))
        assert str(exc_info.value).startswith("daemon:")
    finally:
        rt.shutdown()
        mesh.shutdown()


def test_deliver_to_unknown_origin_raises() -> None:
    mesh = _mesh()
    rt = DaemonRuntime(mesh)
    try:
        rt.register_factory("echo", EchoDaemon)
        rt.start()
        with pytest.raises(DaemonError) as exc_info:
            rt.deliver(0xDEADBEEF, CausalEvent(0xDEADBEEF, 1, b"x"))
        assert str(exc_info.value).startswith("daemon:")
    finally:
        rt.shutdown()
        mesh.shutdown()


# -------------------------------------------------------------------------
# Stage 5 sub-step 3: snapshot + restore round-trip
# -------------------------------------------------------------------------


def test_counter_snapshot_then_spawn_from_snapshot_restores_state() -> None:
    mesh = _mesh()
    rt = DaemonRuntime(mesh)
    try:
        rt.register_factory("counter", CounterDaemon)
        rt.start()
        ident = Identity.generate()
        handle = rt.spawn("counter", ident)

        # Drive the counter to 3.
        for i in range(1, 4):
            rt.deliver(handle.origin_hash, CausalEvent(ident.origin_hash, i, b""))

        snap = rt.snapshot(handle.origin_hash)
        assert snap is not None
        assert isinstance(snap, bytes)
        assert len(snap) > 0

        # Tear the original daemon down — the restored instance
        # must pick up purely from the snapshot, not from live
        # state.
        rt.stop(handle.origin_hash)
        assert rt.daemon_count() == 0

        restored = rt.spawn_from_snapshot("counter", ident, snap)
        assert rt.daemon_count() == 1
        assert restored.origin_hash == handle.origin_hash

        # One more delivery — counter steps from 3 to 4, proving
        # the snapshot's state survived the round-trip.
        out = rt.deliver(restored.origin_hash, CausalEvent(ident.origin_hash, 4, b""))
        assert int.from_bytes(out[0], "little") == 4
    finally:
        rt.shutdown()
        mesh.shutdown()


def test_snapshot_of_stateless_daemon_returns_none() -> None:
    mesh = _mesh()
    rt = DaemonRuntime(mesh)
    try:
        rt.register_factory("echo", EchoDaemon)
        rt.start()
        ident = Identity.generate()
        handle = rt.spawn("echo", ident)
        assert rt.snapshot(handle.origin_hash) is None
    finally:
        rt.shutdown()
        mesh.shutdown()


def test_snapshot_of_unknown_origin_raises() -> None:
    mesh = _mesh()
    rt = DaemonRuntime(mesh)
    try:
        rt.register_factory("counter", CounterDaemon)
        rt.start()
        with pytest.raises(DaemonError) as exc_info:
            rt.snapshot(0xDEADBEEF)
        assert str(exc_info.value).startswith("daemon:")
    finally:
        rt.shutdown()
        mesh.shutdown()


def test_spawn_from_snapshot_with_corrupted_bytes_raises() -> None:
    mesh = _mesh()
    rt = DaemonRuntime(mesh)
    try:
        rt.register_factory("counter", CounterDaemon)
        rt.start()
        ident = Identity.generate()
        with pytest.raises(DaemonError) as exc_info:
            rt.spawn_from_snapshot("counter", ident, b"not a real snapshot")
        assert "snapshot decode failed" in str(exc_info.value)
    finally:
        rt.shutdown()
        mesh.shutdown()


def test_spawn_from_snapshot_with_wrong_identity_raises() -> None:
    mesh = _mesh()
    rt = DaemonRuntime(mesh)
    try:
        rt.register_factory("counter", CounterDaemon)
        rt.start()
        original = Identity.generate()
        handle = rt.spawn("counter", original)
        rt.deliver(handle.origin_hash, CausalEvent(original.origin_hash, 1, b""))
        snap = rt.snapshot(handle.origin_hash)
        assert snap is not None
        rt.stop(handle.origin_hash)

        # Different identity — snapshot's entity_id doesn't match.
        other = Identity.generate()
        with pytest.raises(DaemonError) as exc_info:
            rt.spawn_from_snapshot("counter", other, snap)
        assert str(exc_info.value).startswith("daemon:")
    finally:
        rt.shutdown()
        mesh.shutdown()


def test_snapshot_modify_snapshot_captures_newer_state() -> None:
    # Restoring an earlier vs later snapshot yields different
    # counter values. Proves snapshot captures the state at the
    # moment it was taken.
    mesh = _mesh()
    rt = DaemonRuntime(mesh)
    try:
        rt.register_factory("counter", CounterDaemon)
        rt.start()
        ident = Identity.generate()
        handle = rt.spawn("counter", ident)

        for i in range(1, 3):
            rt.deliver(handle.origin_hash, CausalEvent(ident.origin_hash, i, b""))
        snap_at_2 = rt.snapshot(handle.origin_hash)
        for i in range(3, 6):
            rt.deliver(handle.origin_hash, CausalEvent(ident.origin_hash, i, b""))
        snap_at_5 = rt.snapshot(handle.origin_hash)

        rt.stop(handle.origin_hash)

        # Restore earlier snapshot; next event steps to 3.
        h2 = rt.spawn_from_snapshot("counter", ident, snap_at_2)
        out = rt.deliver(h2.origin_hash, CausalEvent(ident.origin_hash, 6, b""))
        assert int.from_bytes(out[0], "little") == 3
        rt.stop(h2.origin_hash)

        # Restore later snapshot; next event steps to 6.
        h5 = rt.spawn_from_snapshot("counter", ident, snap_at_5)
        out = rt.deliver(h5.origin_hash, CausalEvent(ident.origin_hash, 7, b""))
        assert int.from_bytes(out[0], "little") == 6
    finally:
        rt.shutdown()
        mesh.shutdown()


def test_two_daemons_keep_independent_counter_state() -> None:
    mesh = _mesh()
    rt = DaemonRuntime(mesh)
    try:
        rt.register_factory("counter", CounterDaemon)
        rt.start()
        id_a = Identity.generate()
        id_b = Identity.generate()
        h_a = rt.spawn("counter", id_a)
        h_b = rt.spawn("counter", id_b)

        for i in range(1, 4):
            out = rt.deliver(h_a.origin_hash, CausalEvent(id_a.origin_hash, i, b""))
            assert int.from_bytes(out[0], "little") == i

        out = rt.deliver(h_b.origin_hash, CausalEvent(id_b.origin_hash, 1, b""))
        assert int.from_bytes(out[0], "little") == 1

        # A advances one more; B advances one more. Independent.
        out_a = rt.deliver(h_a.origin_hash, CausalEvent(id_a.origin_hash, 4, b""))
        out_b = rt.deliver(h_b.origin_hash, CausalEvent(id_b.origin_hash, 2, b""))
        assert int.from_bytes(out_a[0], "little") == 4
        assert int.from_bytes(out_b[0], "little") == 2
    finally:
        rt.shutdown()
        mesh.shutdown()


# -------------------------------------------------------------------------
# Stage 5 sub-step 4: migration
# -------------------------------------------------------------------------


def _mesh_pair() -> tuple[NetMesh, NetMesh]:
    """Build two connected meshes for migration tests.

    Handshake: B accepts on a thread while A connects; both start their
    receive loops afterwards. Returns ``(a, b)``.
    """
    a_addr = _next_port()
    b_addr = _next_port()
    a = NetMesh(bind_addr=a_addr, psk=PSK)
    b = NetMesh(bind_addr=b_addr, psk=PSK)

    errors: list[Exception] = []

    def _accept() -> None:
        try:
            b.accept(a.node_id)
        except Exception as e:
            errors.append(e)

    # `daemon=True` so a wedged accept thread doesn't block
    # interpreter shutdown when a test fails or CI gets interrupted.
    t = threading.Thread(target=_accept, daemon=True)
    t.start()
    # Small beat so the accept-side is primed before connect fires.
    time.sleep(0.05)
    a.connect(b_addr, b.public_key, b.node_id)
    t.join(timeout=5)
    # `join(timeout=...)` silently returns when it times out —
    # detect via `is_alive()` so a hung handshake fails the test
    # with a clear message instead of masquerading as success.
    if t.is_alive():
        raise RuntimeError(
            "mesh-pair handshake: accept thread still alive after 5 s timeout"
        )
    if errors:
        raise errors[0]
    a.start()
    b.start()
    return a, b


def test_start_migration_unknown_origin_raises_migration_error() -> None:
    mesh = _mesh()
    rt = DaemonRuntime(mesh)
    try:
        rt.register_factory("counter", CounterDaemon)
        rt.start()
        self_id = mesh.node_id
        with pytest.raises(MigrationError):
            rt.start_migration(0xDEADBEEF, self_id, self_id)
    finally:
        rt.shutdown()
        mesh.shutdown()


def test_start_migration_not_ready_raises() -> None:
    mesh = _mesh()
    rt = DaemonRuntime(mesh)
    try:
        rt.register_factory("counter", CounterDaemon)
        # Intentionally skip rt.start()
        ident = Identity.generate()
        with pytest.raises(DaemonError):
            rt.start_migration(ident.origin_hash, mesh.node_id, mesh.node_id)
    finally:
        rt.shutdown()
        mesh.shutdown()


def test_expect_migration_requires_kind_registered() -> None:
    mesh = _mesh()
    rt = DaemonRuntime(mesh)
    try:
        rt.start()
        with pytest.raises(DaemonError):
            rt.expect_migration("never-registered", 0x1234)
    finally:
        rt.shutdown()
        mesh.shutdown()


def test_expect_migration_duplicate_origin_fails() -> None:
    mesh = _mesh()
    rt = DaemonRuntime(mesh)
    try:
        rt.register_factory("counter", CounterDaemon)
        rt.start()
        rt.expect_migration("counter", 0xABCD_EF01)
        with pytest.raises(DaemonError):
            rt.expect_migration("counter", 0xABCD_EF01)
    finally:
        rt.shutdown()
        mesh.shutdown()


def test_register_migration_target_identity_duplicate_fails() -> None:
    mesh = _mesh()
    rt = DaemonRuntime(mesh)
    try:
        rt.register_factory("counter", CounterDaemon)
        rt.start()
        ident = Identity.generate()
        rt.register_migration_target_identity("counter", ident)
        with pytest.raises(DaemonError):
            rt.register_migration_target_identity("counter", ident)
    finally:
        rt.shutdown()
        mesh.shutdown()


def test_migration_phase_returns_none_for_unknown_origin() -> None:
    mesh = _mesh()
    rt = DaemonRuntime(mesh)
    try:
        rt.start()
        assert rt.migration_phase(0xDEADBEEF) is None
    finally:
        rt.shutdown()
        mesh.shutdown()


# Regression: invalid `start_migration_with` opts must surface as
# typed `DaemonError`, not a raw PyO3 `TypeError` / `ValueError`.
# The previous implementation used `v.extract()?` which bypassed
# the `daemon:`-prefix convention: callers doing
# `except DaemonError` would silently miss the exception, and the
# error message wouldn't route through the SDK's typed-error
# classifier.
def test_start_migration_with_invalid_transport_identity_raises_daemon_error() -> None:
    mesh = _mesh()
    rt = DaemonRuntime(mesh)
    try:
        rt.register_factory("counter", CounterDaemon)
        rt.start()
        ident = Identity.generate()
        handle = rt.spawn("counter", ident)
        with pytest.raises(DaemonError) as exc_info:
            rt.start_migration_with(
                handle.origin_hash,
                mesh.node_id,
                0x1234_5678_ABCD_0001,
                # `"not-a-bool"` is the wrong type — must route through
                # `daemon_err` rather than a raw PyO3 conversion error.
                {"transport_identity": "not-a-bool"},
            )
        assert "transport_identity must be bool" in str(exc_info.value)
    finally:
        rt.shutdown()
        mesh.shutdown()


def test_start_migration_with_invalid_retry_not_ready_ms_raises_daemon_error() -> None:
    mesh = _mesh()
    rt = DaemonRuntime(mesh)
    try:
        rt.register_factory("counter", CounterDaemon)
        rt.start()
        ident = Identity.generate()
        handle = rt.spawn("counter", ident)
        with pytest.raises(DaemonError) as exc_info:
            rt.start_migration_with(
                handle.origin_hash,
                mesh.node_id,
                0x1234_5678_ABCD_0002,
                # Negative int — `u64::extract()` rejects this; the
                # fix routes the error through `daemon_err` so callers
                # still see a prefixed `DaemonError`.
                {"retry_not_ready_ms": -1},
            )
        assert "retry_not_ready_ms" in str(exc_info.value)
    finally:
        rt.shutdown()
        mesh.shutdown()


def test_start_migration_with_transport_identity_false_returns_handle() -> None:
    # Two-mesh pair, connected. Source spawns, kicks off a migration
    # with `transport_identity=False` and `retry_not_ready_ms=0`.
    # Target has no `expect_migration` set up, so the migration
    # will fail on the target dispatcher — but `start_migration_with`
    # on the source only needs the target peer to be reachable and
    # the envelope skippable. Handle returned; we cancel afterwards.
    a, b = _mesh_pair()
    rt_a = DaemonRuntime(a)
    try:
        rt_a.register_factory("counter", CounterDaemon)
        rt_a.start()
        ident = Identity.generate()
        handle = rt_a.spawn("counter", ident)
        mig = rt_a.start_migration_with(
            handle.origin_hash,
            a.node_id,
            b.node_id,
            {"transport_identity": False, "retry_not_ready_ms": 0},
        )
        assert isinstance(mig, MigrationHandle)
        assert mig.origin_hash == handle.origin_hash
        assert mig.source_node == a.node_id
        assert mig.target_node == b.node_id
        phase = mig.phase()
        assert phase is None or isinstance(phase, str)
        try:
            mig.cancel()
        except DaemonError:
            # Orchestrator may already have cleared the record;
            # cancel() on a missing record raises — that's fine.
            pass
    finally:
        rt_a.shutdown()
        a.shutdown()
        b.shutdown()


def test_migration_target_unavailable_raises_migration_error() -> None:
    mesh = _mesh()
    rt = DaemonRuntime(mesh)
    try:
        rt.register_factory("counter", CounterDaemon)
        rt.start()
        ident = Identity.generate()
        handle = rt.spawn("counter", ident)
        # Migrate to a node we never handshook with.
        ghost_node = 0x00AA_BBCC_DDEE_FF00
        with pytest.raises(MigrationError) as exc_info:
            rt.start_migration_with(
                handle.origin_hash,
                mesh.node_id,
                ghost_node,
                {"transport_identity": False, "retry_not_ready_ms": 0},
            )
        kind = migration_error_kind(exc_info.value)
        # Either target-unavailable (peer lookup fails up front) or
        # identity-transport-failed / state-failed (envelope seal /
        # orchestrator path). Any is a typed MigrationError.
        assert kind in (
            "target-unavailable",
            "identity-transport-failed",
            "state-failed",
        )
    finally:
        rt.shutdown()
        mesh.shutdown()


def test_migration_error_is_a_daemon_error_subclass() -> None:
    mesh = _mesh()
    rt = DaemonRuntime(mesh)
    try:
        rt.register_factory("counter", CounterDaemon)
        rt.start()
        ident = Identity.generate()
        handle = rt.spawn("counter", ident)
        ghost_node = 0x1122_3344_5566_7788
        try:
            rt.start_migration_with(
                handle.origin_hash,
                mesh.node_id,
                ghost_node,
                {"transport_identity": False, "retry_not_ready_ms": 0},
            )
            pytest.fail("expected MigrationError")
        except MigrationError as e:
            # Subclass check — caller can catch DaemonError too.
            assert isinstance(e, DaemonError)
    finally:
        rt.shutdown()
        mesh.shutdown()


def test_end_to_end_counter_migration_a_to_b() -> None:
    """Stage 5 exit criterion. Mirrors the TS / Rust end-to-end
    migration test: spawn a stateful Python daemon on A, drive the
    counter via deliveries, migrate to B with envelope transport,
    verify counter state survived on B.
    """
    a, b = _mesh_pair()
    rt_a = DaemonRuntime(a)
    rt_b = DaemonRuntime(b)
    try:
        rt_a.register_factory("counter", CounterDaemon)
        rt_a.start()
        rt_b.register_factory("counter", CounterDaemon)
        rt_b.start()

        ident = Identity.generate()
        handle = rt_a.spawn("counter", ident)
        for i in range(1, 4):
            rt_a.deliver(handle.origin_hash, CausalEvent(ident.origin_hash, i, b""))

        rt_b.expect_migration("counter", handle.origin_hash)

        mig = rt_a.start_migration(handle.origin_hash, a.node_id, b.node_id)
        mig.wait_with_timeout(5000)

        # Tail-end ActivateAck race — matches the 200ms beat from the
        # TS test. wait() returns when A's orchestrator record clears;
        # that can slightly precede B's final daemon-registry insert.
        time.sleep(0.2)

        assert rt_a.daemon_count() == 0
        assert rt_b.daemon_count() == 1

        # One more delivery on B. If target-side factory
        # reconstruction worked, the counter is seeded from the
        # snapshot (3) and this delivery steps it to 4.
        out = rt_b.deliver(handle.origin_hash, CausalEvent(ident.origin_hash, 4, b""))
        assert len(out) == 1
        assert int.from_bytes(out[0], "little") == 4
    finally:
        rt_a.shutdown()
        rt_b.shutdown()
        a.shutdown()
        b.shutdown()


def test_migration_fails_when_target_has_no_expect_migration() -> None:
    """Mid-flight failure: target has the factory registered but
    never called `expect_migration`, so the inbound SnapshotReady
    hits a dispatcher with no factory_registry entry for the
    origin_hash. wait() rejects with factory-not-found.
    """
    a, b = _mesh_pair()
    rt_a = DaemonRuntime(a)
    rt_b = DaemonRuntime(b)
    try:
        rt_a.register_factory("counter", CounterDaemon)
        rt_a.start()
        rt_b.register_factory("counter", CounterDaemon)
        rt_b.start()
        # No expect_migration on B.

        ident = Identity.generate()
        handle = rt_a.spawn("counter", ident)
        mig = rt_a.start_migration_with(
            handle.origin_hash,
            a.node_id,
            b.node_id,
            {"retry_not_ready_ms": 0},
        )
        with pytest.raises(MigrationError) as exc_info:
            mig.wait_with_timeout(5000)
        assert migration_error_kind(exc_info.value) == "factory-not-found"
    finally:
        rt_a.shutdown()
        rt_b.shutdown()
        a.shutdown()
        b.shutdown()


def test_migration_fails_when_target_restore_throws() -> None:
    """Mid-flight failure: target's restore throws, dispatcher
    surfaces MigrationFailed(StateFailed). wait() rejects with
    state-failed.
    """
    class RestoreFailer:
        """Counter daemon whose restore throws deliberately."""
        def __init__(self) -> None:
            self._count = 0

        def process(self, event: CausalEvent) -> list[bytes]:
            self._count += 1
            return [self._count.to_bytes(4, "little")]

        def snapshot(self) -> bytes:
            return self._count.to_bytes(4, "little")

        def restore(self, state: bytes) -> None:
            raise RuntimeError("deliberate restore failure")

    a, b = _mesh_pair()
    rt_a = DaemonRuntime(a)
    rt_b = DaemonRuntime(b)
    try:
        # Source: normal counter factory (so snapshot emits valid bytes).
        rt_a.register_factory("counter", CounterDaemon)
        rt_a.start()
        # Target: restore-failing factory.
        rt_b.register_factory("counter", RestoreFailer)
        rt_b.start()

        ident = Identity.generate()
        handle = rt_a.spawn("counter", ident)
        for i in range(1, 3):
            rt_a.deliver(handle.origin_hash, CausalEvent(ident.origin_hash, i, b""))

        rt_b.expect_migration("counter", handle.origin_hash)

        mig = rt_a.start_migration(handle.origin_hash, a.node_id, b.node_id)
        with pytest.raises(MigrationError) as exc_info:
            mig.wait_with_timeout(5000)
        assert migration_error_kind(exc_info.value) == "state-failed"
    finally:
        rt_a.shutdown()
        rt_b.shutdown()
        a.shutdown()
        b.shutdown()


def test_migration_phases_iterator_yields_distinct_transitions() -> None:
    """phases() yields each distinct transition and terminates
    when the orchestrator clears the record. We don't pin the
    exact phase sequence (can race with scheduling), but each
    yielded phase must differ from its predecessor and the
    iterator must terminate (proving cleanup ran)."""
    a, b = _mesh_pair()
    rt_a = DaemonRuntime(a)
    try:
        rt_a.register_factory("counter", CounterDaemon)
        rt_a.start()
        ident = Identity.generate()
        handle = rt_a.spawn("counter", ident)
        mig = rt_a.start_migration_with(
            handle.origin_hash,
            a.node_id,
            b.node_id,
            {"transport_identity": False, "retry_not_ready_ms": 0},
        )
        seen = list(mig.phases())  # iterator auto-drains
        for i in range(1, len(seen)):
            assert seen[i] != seen[i - 1]
        # After iterator terminates, phase() returns None.
        assert mig.phase() is None
    finally:
        rt_a.shutdown()
        a.shutdown()
        b.shutdown()

