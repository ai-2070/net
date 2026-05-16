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
    it. The substrate-side metadata's `node_id` is hardcoded to `0`
    today (substrate `runtime_this_node()` is a placeholder pending
    a `runtime.this_node()` accessor — `sdk.rs:738`). Once that
    lands, switch this back to asserting the configured value.
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
            # Today's substrate placeholder; pin to keep the test
            # honest as the substrate matures.
            assert md["node_id"] == 0
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


def test_metadata_view_carries_active_maintenance_state_and_peers_list() -> None:
    with MeshOsDaemonSdk.start() as sdk:
        with sdk.register_daemon(_EchoDaemon(), Identity.generate()) as handle:
            md = handle.metadata()
            assert md["maintenance_state"]["kind"] == "Active"
            assert isinstance(md["peers"], list)
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
