"""Tests for the Deck SDK operator-side bindings — Phase 4 slice 1.

Covers: DeckClient construction against a live MeshOsDaemonSdk,
OperatorIdentity construction, all 9 AdminCommands methods,
status / status_summary / snapshots / status_summary_stream.

Requires the wheel to have been built with `--features deck` (which
implies meshos). Skips cleanly otherwise.
"""

from __future__ import annotations

import json

import pytest

try:
    from net import (  # type: ignore[attr-defined]
        DeckClient,
        DeckSdkError,
        MeshOsDaemonSdk,
        OperatorIdentity,
        deck_sdk_error_kind,
    )
except ImportError:  # pragma: no cover
    pytest.skip("Deck SDK not compiled into this wheel", allow_module_level=True)


# -------------------------------------------------------------------------
# OperatorIdentity
# -------------------------------------------------------------------------


def test_operator_identity_generate_returns_distinct_ids() -> None:
    a = OperatorIdentity.generate()
    b = OperatorIdentity.generate()
    assert a.operator_id != b.operator_id


def test_operator_identity_from_seed_is_deterministic() -> None:
    seed = b"\x42" * 32
    a = OperatorIdentity.from_seed(seed)
    b = OperatorIdentity.from_seed(seed)
    assert a.operator_id == b.operator_id


def test_operator_identity_from_seed_rejects_wrong_length() -> None:
    with pytest.raises(DeckSdkError) as excinfo:
        OperatorIdentity.from_seed(b"\x42" * 31)
    assert excinfo.value.kind == "invalid_argument"


# -------------------------------------------------------------------------
# DeckClient construction
# -------------------------------------------------------------------------


def test_deck_client_constructs_against_running_meshos_sdk() -> None:
    sdk = MeshOsDaemonSdk.start()
    try:
        identity = OperatorIdentity.generate()
        client = DeckClient.from_meshos(sdk, identity)
        assert client.identity().operator_id == identity.operator_id
    finally:
        sdk.shutdown()


def test_deck_client_rejects_shutdown_meshos_sdk() -> None:
    sdk = MeshOsDaemonSdk.start()
    sdk.shutdown()
    with pytest.raises(DeckSdkError) as excinfo:
        DeckClient.from_meshos(sdk, OperatorIdentity.generate())
    assert excinfo.value.kind == "already_shutdown"


def test_deck_client_accepts_config_dict() -> None:
    sdk = MeshOsDaemonSdk.start()
    try:
        identity = OperatorIdentity.generate()
        client = DeckClient.from_meshos(
            sdk,
            identity,
            {"snapshot_poll_interval_ms": 50, "ice_signature_threshold": 2},
        )
        assert client.identity().operator_id == identity.operator_id
    finally:
        sdk.shutdown()


def test_deck_client_rejects_bad_config() -> None:
    sdk = MeshOsDaemonSdk.start()
    try:
        with pytest.raises(DeckSdkError) as excinfo:
            DeckClient.from_meshos(
                sdk,
                OperatorIdentity.generate(),
                {"snapshot_poll_interval_ms": "nope"},
            )
        assert excinfo.value.kind == "invalid_config"
    finally:
        sdk.shutdown()


# -------------------------------------------------------------------------
# status / status_summary — one-shot reads
# -------------------------------------------------------------------------


def test_status_returns_serialized_snapshot_json() -> None:
    sdk = MeshOsDaemonSdk.start()
    try:
        client = DeckClient.from_meshos(sdk, OperatorIdentity.generate())
        snap_json = client.status()
        # The Rust side serializes the MeshOsSnapshot to a JSON
        # string; verify it parses cleanly and has the expected
        # top-level shape (mainly: it's an object).
        snap = json.loads(snap_json)
        assert isinstance(snap, dict)
    finally:
        sdk.shutdown()


def test_status_summary_returns_typed_dict() -> None:
    sdk = MeshOsDaemonSdk.start()
    try:
        client = DeckClient.from_meshos(sdk, OperatorIdentity.generate())
        summary = client.status_summary()
        assert isinstance(summary, dict)
        # Required keys per the slice 1 contract.
        assert "peers" in summary
        assert "daemons" in summary
        assert "replica_chains" in summary
        assert "avoid_list_entries" in summary
        assert "local_maintenance_active" in summary
        # Nested counts have stable shapes.
        for key in ("healthy", "degraded", "unreachable", "unknown"):
            assert key in summary["peers"]
        for key in (
            "running",
            "starting",
            "stopping",
            "stopped",
            "backing_off",
            "crash_looping",
        ):
            assert key in summary["daemons"]
    finally:
        sdk.shutdown()


# -------------------------------------------------------------------------
# AdminCommands — every method commits + returns a ChainCommit dict
# -------------------------------------------------------------------------


def test_admin_drain_commits_and_returns_chain_commit() -> None:
    sdk = MeshOsDaemonSdk.start()
    try:
        identity = OperatorIdentity.generate()
        client = DeckClient.from_meshos(sdk, identity)
        commit = client.admin.drain(node=0xABCD, drain_for_ms=60_000)
        assert isinstance(commit, dict)
        assert commit["operator_id"] == identity.operator_id
        assert commit["event_kind"] == "drain"
        assert isinstance(commit["commit_id"], int)
        assert commit["commit_id"] > 0
        assert isinstance(commit["committed_at_ms"], int)
    finally:
        sdk.shutdown()


def test_admin_enter_maintenance_with_and_without_deadline() -> None:
    sdk = MeshOsDaemonSdk.start()
    try:
        client = DeckClient.from_meshos(sdk, OperatorIdentity.generate())
        c1 = client.admin.enter_maintenance(node=0x1234)
        c2 = client.admin.enter_maintenance(node=0x5678, drain_for_ms=300_000)
        assert c1["event_kind"] == "enter_maintenance"
        assert c2["event_kind"] == "enter_maintenance"
        assert c2["commit_id"] > c1["commit_id"]
    finally:
        sdk.shutdown()


def test_every_admin_method_commits_distinct_event_kind() -> None:
    """Drive every AdminCommands method and verify each commit's
    `event_kind` matches the variant name. Confirms the slice-1
    contract for every method."""
    sdk = MeshOsDaemonSdk.start()
    try:
        client = DeckClient.from_meshos(sdk, OperatorIdentity.generate())
        admin = client.admin
        node = 0xCAFE

        results = [
            ("drain", admin.drain(node, 1_000)),
            ("enter_maintenance", admin.enter_maintenance(node)),
            ("exit_maintenance", admin.exit_maintenance(node)),
            ("cordon", admin.cordon(node)),
            ("uncordon", admin.uncordon(node)),
            ("drop_replicas", admin.drop_replicas(node, [0xDEAD, 0xBEEF])),
            ("invalidate_placement", admin.invalidate_placement(node)),
            ("restart_all_daemons", admin.restart_all_daemons(node)),
            ("clear_avoid_list", admin.clear_avoid_list(node)),
        ]
        for kind, commit in results:
            assert commit["event_kind"] == kind, (
                f"expected event_kind={kind!r}, got {commit['event_kind']!r}"
            )
            assert commit["commit_id"] > 0
    finally:
        sdk.shutdown()


# -------------------------------------------------------------------------
# Streams — basic shape verification (no real cluster, so we only
# verify the iterator protocol and the close() path).
# -------------------------------------------------------------------------


def test_snapshots_iterator_yields_parseable_json() -> None:
    """SnapshotStream's __next__ blocks for the poll interval (100ms
    default) then yields a JSON-encoded MeshOsSnapshot string. Pull
    one item and verify it parses."""
    sdk = MeshOsDaemonSdk.start()
    try:
        client = DeckClient.from_meshos(sdk, OperatorIdentity.generate())
        stream = client.snapshots()
        try:
            raw = next(stream)
            assert isinstance(raw, str)
            parsed = json.loads(raw)
            assert isinstance(parsed, dict)
        finally:
            stream.close()
    finally:
        sdk.shutdown()


def test_status_summary_stream_yields_typed_dicts() -> None:
    sdk = MeshOsDaemonSdk.start()
    try:
        client = DeckClient.from_meshos(sdk, OperatorIdentity.generate())
        stream = client.status_summary_stream()
        try:
            summary = next(stream)
            assert isinstance(summary, dict)
            assert "peers" in summary
        finally:
            stream.close()
    finally:
        sdk.shutdown()


def test_snapshot_stream_close_raises_stopiteration_on_next() -> None:
    sdk = MeshOsDaemonSdk.start()
    try:
        client = DeckClient.from_meshos(sdk, OperatorIdentity.generate())
        stream = client.snapshots()
        stream.close()
        with pytest.raises((DeckSdkError, StopIteration)):
            next(stream)
    finally:
        sdk.shutdown()


# -------------------------------------------------------------------------
# Error envelope
# -------------------------------------------------------------------------


def test_deck_sdk_error_kind_helper_parses_envelope() -> None:
    try:
        OperatorIdentity.from_seed(b"\x00" * 10)  # wrong length
    except DeckSdkError as e:
        assert e.kind == "invalid_argument"
        assert deck_sdk_error_kind(e) == "invalid_argument"
        assert "<<deck-sdk-kind:invalid_argument>>" in str(e)
    else:  # pragma: no cover
        pytest.fail("expected DeckSdkError")
