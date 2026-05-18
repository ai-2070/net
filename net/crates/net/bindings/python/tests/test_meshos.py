"""Tests for the MeshOS daemon-author SDK — Phase 2 slice 1.

Mirrors the in-Rust integration test pattern at
`crates/net/src/adapter/net/behavior/meshos/sdk.rs` tests module.
Covers register / control receive / publish_log / graceful_shutdown
end-to-end against the substrate's `LoggingDispatcher`.

The `meshos` feature is mandatory — these tests will fail to import
if the wheel was built without `--features meshos`. Skip cleanly so
non-meshos CI runs aren't broken.
"""

from __future__ import annotations

import pytest

# Skip the whole module if the wheel wasn't built with `--features meshos`.
try:
    from net import (  # type: ignore[attr-defined]
        Identity,
        MeshOsDaemonHandle,
        MeshOsDaemonSdk,
        MeshOsSdkError,
        meshos_sdk_error_kind,
    )
except ImportError:  # pragma: no cover
    pytest.skip("MeshOS SDK not compiled into this wheel", allow_module_level=True)


# -------------------------------------------------------------------------
# Fixture daemons
# -------------------------------------------------------------------------


class _EchoDaemon:
    """Minimal daemon — returns one output per input event."""

    def name(self) -> str:
        return "echo"

    def process(self, event):
        # Slice 1: events arrive as dicts with payload bytes.
        return [event["payload"]]


# -------------------------------------------------------------------------
# Lifecycle
# -------------------------------------------------------------------------


def test_start_and_shutdown_with_defaults() -> None:
    sdk = MeshOsDaemonSdk.start()
    sdk.shutdown()


def test_start_and_shutdown_via_context_manager() -> None:
    with MeshOsDaemonSdk.start() as sdk:
        assert sdk.dropped_control_events() == 0


def test_start_with_config_dict_does_not_raise() -> None:
    """The binding accepts a config dict and the SDK builds against
    it. The substrate now plumbs `runtime.this_node()` through to the
    daemon metadata, so the configured `this_node` round-trips.
    """
    cfg = {
        "this_node": 0xABCD_1234,
        "tick_interval_ms": 50,
        "event_queue_capacity": 64,
        "action_queue_capacity": 64,
    }
    with MeshOsDaemonSdk.start(cfg) as sdk:
        with sdk.register_daemon(_EchoDaemon(), Identity.generate()) as handle:
            md = handle.metadata()
            assert md["node_id"] == 0xABCD_1234
            assert md["daemon_name"] == "echo"


def test_start_rejects_bad_config_with_typed_error() -> None:
    with pytest.raises(MeshOsSdkError) as excinfo:
        MeshOsDaemonSdk.start({"this_node": "not an int"})
    err = excinfo.value
    assert err.kind == "invalid_config"


def test_double_shutdown_raises_already_shutdown() -> None:
    sdk = MeshOsDaemonSdk.start()
    sdk.shutdown()
    with pytest.raises(MeshOsSdkError) as excinfo:
        sdk.shutdown()
    assert excinfo.value.kind == "already_shutdown"


# -------------------------------------------------------------------------
# Registration + metadata
# -------------------------------------------------------------------------


def test_register_daemon_returns_handle_with_correct_identity() -> None:
    identity = Identity.generate()
    with MeshOsDaemonSdk.start() as sdk:
        handle = sdk.register_daemon(_EchoDaemon(), identity)
        try:
            assert handle.daemon_id == identity.origin_hash
            assert handle.daemon_name == "echo"
        finally:
            handle.graceful_shutdown(grace_ms=10)


def test_metadata_view_carries_active_maintenance_state_and_peers() -> None:
    with MeshOsDaemonSdk.start() as sdk:
        with sdk.register_daemon(_EchoDaemon(), Identity.generate()) as handle:
            md = handle.metadata()
            assert md["maintenance_state"]["kind"] == "Active"
            # Slice 2 emits peers as a `{node_id: PeerSnapshot}` dict.
            # Empty here since there are no peers in a single-node fixture.
            assert isinstance(md["peers"], dict)
            assert md["daemon_name"] == "echo"


def test_refresh_metadata_returns_consistent_node_id() -> None:
    cfg = {"this_node": 0xBEEF}
    with MeshOsDaemonSdk.start(cfg) as sdk:
        with sdk.register_daemon(_EchoDaemon(), Identity.generate()) as handle:
            md1 = handle.metadata()
            md2 = handle.refresh_metadata()
            # Substrate placeholder (see
            # `test_start_with_config_dict_does_not_raise`); the two
            # views agree regardless of what the value is.
            assert md1["node_id"] == md2["node_id"]


# -------------------------------------------------------------------------
# Control events — the graceful_shutdown path is the canonical
# end-to-end test (fires a Shutdown event the daemon observes).
# -------------------------------------------------------------------------


def test_graceful_shutdown_completes_without_error() -> None:
    """`graceful_shutdown` pushes `DaemonControl::Shutdown` onto the
    daemon's control channel via the router, parks for `grace_ms`,
    then unregisters. Substrate-side, the trait's `on_control`
    callback is NOT invoked from this path — the supervisor's
    executor is the trait-callback driver, not the SDK's shutdown
    sequence. So we verify only the explicit contract: the call
    returns without error and subsequent operations on the handle
    raise `already_shutdown`."""
    with MeshOsDaemonSdk.start() as sdk:
        handle = sdk.register_daemon(_EchoDaemon(), Identity.generate())
        handle.graceful_shutdown(grace_ms=50)
        # The handle is consumed — subsequent ops raise the typed
        # error rather than silently no-op'ing.
        with pytest.raises(MeshOsSdkError) as excinfo:
            handle.graceful_shutdown(grace_ms=10)
        assert excinfo.value.kind == "already_shutdown"


def test_zero_grace_shutdown_returns_immediately() -> None:
    """A `grace_ms=0` shutdown should complete promptly — the
    substrate parks for `tokio::time::sleep(Duration::ZERO)` which
    yields immediately. Useful for tests + drain-aborted paths."""
    import time

    with MeshOsDaemonSdk.start() as sdk:
        handle = sdk.register_daemon(_EchoDaemon(), Identity.generate())
        start = time.perf_counter()
        handle.graceful_shutdown(grace_ms=0)
        elapsed_ms = (time.perf_counter() - start) * 1000
        # Generous bound — slow CI / VMs shouldn't flake. The shutdown
        # path is dominated by an `await sleep(0)` plus channel
        # teardown; well under 500ms.
        assert elapsed_ms < 500, f"shutdown took {elapsed_ms:.1f} ms"


def test_try_next_control_returns_none_on_empty_channel() -> None:
    with MeshOsDaemonSdk.start() as sdk:
        with sdk.register_daemon(_EchoDaemon(), Identity.generate()) as handle:
            assert handle.try_next_control() is None


def test_next_control_with_timeout_returns_none_on_quiet_channel() -> None:
    with MeshOsDaemonSdk.start() as sdk:
        with sdk.register_daemon(_EchoDaemon(), Identity.generate()) as handle:
            # 100 ms is enough to confirm the timeout path returns
            # without waiting for a forever-quiet supervisor.
            assert handle.next_control(timeout_ms=100) is None


# -------------------------------------------------------------------------
# publish_log — exercises the loop's log ring without asserting on
# the resulting LogRecord (that's a Deck-SDK concern). Verifies the
# call succeeds + emits no error.
# -------------------------------------------------------------------------


def test_publish_log_at_each_level_succeeds() -> None:
    with MeshOsDaemonSdk.start() as sdk:
        with sdk.register_daemon(_EchoDaemon(), Identity.generate()) as handle:
            for level in ("trace", "debug", "info", "warn", "error"):
                handle.publish_log(level, f"hello from {level}")


def test_publish_log_rejects_invalid_level_with_typed_error() -> None:
    with MeshOsDaemonSdk.start() as sdk:
        with sdk.register_daemon(_EchoDaemon(), Identity.generate()) as handle:
            with pytest.raises(MeshOsSdkError) as excinfo:
                handle.publish_log("verbose", "nope")
            assert excinfo.value.kind == "invalid_log_level"


# -------------------------------------------------------------------------
# publish_capabilities — substrate-side stub. Confirm the surface
# exists + returns without raising; the chain commit lands later.
# -------------------------------------------------------------------------


def test_publish_capabilities_stub_returns_without_error() -> None:
    with MeshOsDaemonSdk.start() as sdk:
        with sdk.register_daemon(_EchoDaemon(), Identity.generate()) as handle:
            handle.publish_capabilities({"tags": ["software.telemetry"]})
            # No assertion on side effect — slice 1 is a no-op stub.


# -------------------------------------------------------------------------
# Slice 2: real CapabilitySet conversion. The substrate-side commit
# remains a stub; the conversion still runs so a malformed dict
# surfaces a typed error immediately.
# -------------------------------------------------------------------------


def test_publish_capabilities_accepts_full_cap_set_dict() -> None:
    with MeshOsDaemonSdk.start() as sdk:
        with sdk.register_daemon(_EchoDaemon(), Identity.generate()) as handle:
            handle.publish_capabilities(
                {
                    "hardware": {"cpu_cores": 8, "ram_bytes": 16 * 1024**3},
                    "software": {"runtime": "rust-1.78"},
                    "tags": ["software.telemetry", "scope:trusted"],
                }
            )


def test_publish_capabilities_with_none_clears_to_default() -> None:
    with MeshOsDaemonSdk.start() as sdk:
        with sdk.register_daemon(_EchoDaemon(), Identity.generate()) as handle:
            handle.publish_capabilities(None)
            handle.publish_capabilities()  # no-arg path


def test_publish_capabilities_rejects_malformed_dict_with_typed_error() -> None:
    with MeshOsDaemonSdk.start() as sdk:
        with sdk.register_daemon(_EchoDaemon(), Identity.generate()) as handle:
            with pytest.raises(MeshOsSdkError) as excinfo:
                # `models` must be a list of dicts per
                # capabilities::capability_set_from_py; a bare
                # string violates the schema.
                handle.publish_capabilities({"models": "not a list"})
            assert excinfo.value.kind == "invalid_capabilities"


# -------------------------------------------------------------------------
# Slice 2: peer snapshot decoding. No peers exist in a single-node
# fixture, but the shape is verifiable — `peers` is a dict (not a
# list) and gains structured `PeerSnapshot` projections per id.
# -------------------------------------------------------------------------


def test_metadata_peers_is_a_dict_not_a_list() -> None:
    """Slice 2 returns `peers` as a `{node_id: PeerSnapshot}` dict.
    The slice 1 form was `peers: [node_id, ...]`; consumers that
    upgraded must adjust. Pin the shape so regressions surface."""
    with MeshOsDaemonSdk.start() as sdk:
        with sdk.register_daemon(_EchoDaemon(), Identity.generate()) as handle:
            md = handle.metadata()
            assert isinstance(md["peers"], dict)


# -------------------------------------------------------------------------
# Slice 2: snapshot/restore wiring. The supervisor invokes the
# bridge's snapshot/restore on migration; from the daemon-side SDK
# we can't drive migration directly, but we *can* verify a daemon
# with snapshot+restore methods registers cleanly without raising
# (the bridge resolves the optional callables eagerly at registration
# time, so an invalid method signature would surface here).
# -------------------------------------------------------------------------


class _StatefulDaemon:
    def __init__(self) -> None:
        self.value = 0
        self.restored_from: bytes | None = None

    def name(self) -> str:
        return "stateful"

    def process(self, event):
        self.value += 1
        return [b"v=%d" % self.value]

    def snapshot(self) -> bytes | None:
        return self.value.to_bytes(8, "little")

    def restore(self, state: bytes) -> None:
        self.restored_from = bytes(state)
        self.value = int.from_bytes(state, "little")


def test_stateful_daemon_with_snapshot_and_restore_registers_cleanly() -> None:
    daemon = _StatefulDaemon()
    with MeshOsDaemonSdk.start() as sdk:
        with sdk.register_daemon(daemon, Identity.generate()) as handle:
            assert handle.daemon_name == "stateful"


class _DaemonWithSnapshotReturningNone:
    """Stateless daemons return `None` from `snapshot()`. The bridge
    must treat this as 'no snapshot to capture' rather than
    crashing on the missing return value."""

    def name(self) -> str:
        return "stateless-with-snapshot-method"

    def process(self, event):
        return []

    def snapshot(self) -> None:
        return None


def test_daemon_with_explicit_none_snapshot_registers_cleanly() -> None:
    with MeshOsDaemonSdk.start() as sdk:
        with sdk.register_daemon(
            _DaemonWithSnapshotReturningNone(), Identity.generate()
        ) as handle:
            assert handle.daemon_name == "stateless-with-snapshot-method"


# -------------------------------------------------------------------------
# Slice 2: optional health / saturation callbacks resolved at register.
# -------------------------------------------------------------------------


class _DaemonWithHealth:
    def __init__(self, queue_depth: int) -> None:
        self.queue_depth = queue_depth

    def name(self) -> str:
        return "with-health"

    def process(self, event):
        return []

    def health(self):
        if self.queue_depth < 1000:
            return "healthy"
        return {"kind": "degraded", "reason": "queue depth"}

    def saturation(self) -> float:
        return min(1.0, self.queue_depth / 1000.0)


def test_daemon_with_health_and_saturation_callbacks_registers_cleanly() -> None:
    with MeshOsDaemonSdk.start() as sdk:
        with sdk.register_daemon(_DaemonWithHealth(0), Identity.generate()) as handle:
            assert handle.daemon_name == "with-health"


# -------------------------------------------------------------------------
# Drop-without-shutdown still cleans up (Rust-side Drop impl)
# -------------------------------------------------------------------------


def test_handle_drop_without_graceful_shutdown_still_unregisters() -> None:
    with MeshOsDaemonSdk.start() as sdk:
        identity = Identity.generate()
        handle = sdk.register_daemon(_EchoDaemon(), identity)
        # Drop by going out of scope — the Rust-side Drop impl runs
        # `unregister_inner`. Without that, a follow-up register of
        # the same identity would fail with `register_failed`.
        del handle

        # The substrate's `register_daemon` would surface
        # `register_failed` if the registry slot were still occupied
        # by the dropped handle; a clean re-register confirms the
        # Drop path released the slot.
        handle2 = sdk.register_daemon(_EchoDaemon(), identity)
        handle2.graceful_shutdown(grace_ms=10)


def test_already_shutdown_handle_raises_typed_error_on_method_call() -> None:
    with MeshOsDaemonSdk.start() as sdk:
        handle = sdk.register_daemon(_EchoDaemon(), Identity.generate())
        handle.graceful_shutdown(grace_ms=10)
        with pytest.raises(MeshOsSdkError) as excinfo:
            handle.publish_log("info", "after shutdown")
        assert excinfo.value.kind == "already_shutdown"


# -------------------------------------------------------------------------
# Error envelope parser — kind discriminator survives the
# `<<meshos-sdk-kind:KIND>>MSG` envelope.
# -------------------------------------------------------------------------


def test_meshos_sdk_error_kind_helper_parses_envelope() -> None:
    """The `.kind` attribute should be the canonical source; the
    `meshos_sdk_error_kind` helper is the fallback. Exercise both
    to keep them in sync."""
    with MeshOsDaemonSdk.start() as sdk:
        with sdk.register_daemon(_EchoDaemon(), Identity.generate()) as handle:
            try:
                handle.publish_log("verbose", "nope")
            except MeshOsSdkError as e:
                assert e.kind == "invalid_log_level"
                assert meshos_sdk_error_kind(e) == "invalid_log_level"
                # The envelope is preserved in the message.
                assert "<<meshos-sdk-kind:invalid_log_level>>" in str(e)
            else:  # pragma: no cover
                pytest.fail("expected publish_log to raise")


# -------------------------------------------------------------------------
# Capability advertisement — `required_capabilities` /
# `optional_capabilities` routed through the bridge.
# -------------------------------------------------------------------------


class _CapabilityDaemon:
    """Daemon advertising static capability tag lists. The Python
    surface accepts either a method or a property; this test
    exercises the method form."""

    def name(self) -> str:
        return "cap-daemon"

    def process(self, event):
        return [event["payload"]]

    def required_capabilities(self) -> list[str]:
        return ["hardware.gpu", "software.model.foo=llama-3.1-70b"]

    def optional_capabilities(self) -> list[str]:
        return ["heat:cold"]


class _BadCapabilityDaemon:
    """Daemon whose `required_capabilities` returns the wrong shape
    — should raise `invalid_daemon` at registration time."""

    def name(self) -> str:
        return "bad-cap-daemon"

    def process(self, event):
        return []

    def required_capabilities(self) -> int:  # type: ignore[override]
        return 42


def test_register_daemon_accepts_capability_tag_lists() -> None:
    """Daemons returning capability tag lists register cleanly.
    The cached `CapabilitySet` is built from the tags at construction
    time so the substrate sees the advertised set on every poll
    without re-entering Python."""
    with MeshOsDaemonSdk.start() as sdk:
        handle = sdk.register_daemon(_CapabilityDaemon(), Identity.generate())
        # Substrate doesn't yet expose the resolved CapabilitySet on
        # the handle; the contract is that registration succeeds with
        # a non-default tag list. Verified-by-construction: an
        # invalid shape would raise; a valid one returns a usable
        # handle.
        assert handle.daemon_id != 0
        handle.graceful_shutdown(grace_ms=10)


def test_register_daemon_rejects_invalid_capability_shape() -> None:
    with MeshOsDaemonSdk.start() as sdk:
        with pytest.raises(MeshOsSdkError) as excinfo:
            sdk.register_daemon(_BadCapabilityDaemon(), Identity.generate())
        assert excinfo.value.kind == "invalid_daemon"
        assert "required_capabilities" in str(excinfo.value)


def test_register_daemon_tolerates_missing_capability_methods() -> None:
    """The `_EchoDaemon` fixture has neither `required_capabilities`
    nor `optional_capabilities`. The bridge should fall back to
    empty sets without raising."""
    with MeshOsDaemonSdk.start() as sdk:
        handle = sdk.register_daemon(_EchoDaemon(), Identity.generate())
        assert handle.daemon_id != 0
        handle.graceful_shutdown(grace_ms=10)


# -------------------------------------------------------------------------
# Slice 3 — async control wrappers (`anext_control`, `async for`).
#
# The wrapper lives at `sdk-py/src/net_sdk/meshos.py`; pure-Python
# `run_in_executor`-based bridge over the sync FFI. Verifies the
# wrapper imports + the basic async surface compiles + a Shutdown
# event reaches `__anext__` correctly via `graceful_shutdown`.
# -------------------------------------------------------------------------


import asyncio


def test_async_control_iterator_stops_cleanly_on_graceful_shutdown() -> None:
    """`async for ev in handle` should exit cleanly once
    `graceful_shutdown` runs. The iterator terminates with
    `StopAsyncIteration` (which `async for` swallows) when the
    handle's `try_next_control` raises `already_shutdown`.

    Note: `graceful_shutdown` consumes the inner handle
    synchronously after queuing the Shutdown event, so the
    iterator may or may not observe the Shutdown event itself
    depending on timing — the meaningful contract is "iterator
    stops cleanly, no exception bubbles out"."""
    try:
        from net_sdk.meshos import MeshOsDaemonHandleWrapper, MeshOsDaemonSdk as _Sdk
    except ImportError:
        pytest.skip("net_sdk wrapper not installed (run `pip install -e ../../sdk-py`)")

    async def _run() -> str:
        with _Sdk.start() as sdk:
            handle: MeshOsDaemonHandleWrapper = sdk.register_daemon(
                _EchoDaemon(), Identity.generate()
            )

            seen_kinds: list[str] = []

            async def consume() -> str:
                async for ev in handle:
                    seen_kinds.append(ev["kind"])
                    # Defensive break — if shutdown delivers via the
                    # channel before the handle is consumed, we exit
                    # the loop here. Otherwise the iterator stops via
                    # the `already_shutdown` path.
                    if ev["kind"] == "Shutdown":
                        return "shutdown-seen"
                return "iterator-stopped"

            consumer = asyncio.create_task(consume())
            await asyncio.sleep(0.05)
            await asyncio.get_running_loop().run_in_executor(
                None, lambda: handle.graceful_shutdown(grace_ms=50)
            )
            return await asyncio.wait_for(consumer, timeout=2.0)

    result = asyncio.run(_run())
    # Either path is acceptable — the consumer must exit cleanly.
    assert result in {"shutdown-seen", "iterator-stopped"}


def test_anext_control_returns_none_on_timeout() -> None:
    """`anext_control(timeout_ms=...)` returns `None` on timeout —
    mirrors the sync `next_control` contract."""
    try:
        from net_sdk.meshos import MeshOsDaemonSdk as _Sdk
    except ImportError:
        pytest.skip("net_sdk wrapper not installed")

    async def _run() -> Any:
        with _Sdk.start() as sdk:
            with sdk.register_daemon(_EchoDaemon(), Identity.generate()) as handle:
                ev = await handle.anext_control(timeout_ms=50)
                return ev

    from typing import Any  # noqa: F401 — silences forward-ref

    result = asyncio.run(_run())
    assert result is None
