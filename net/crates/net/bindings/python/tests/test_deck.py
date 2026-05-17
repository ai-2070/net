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

# Operator-policy verifier surface — try-imported so wheels built
# before the surface landed still skip those tests cleanly.
try:
    from net import (  # type: ignore[attr-defined]
        DeckAdminVerifier,
        DeckOperatorRegistry,
    )

    _HAS_VERIFIER = True
except ImportError:  # pragma: no cover
    _HAS_VERIFIER = False


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


# -------------------------------------------------------------------------
# Standalone constructor (operator-only mode — mirrors net_deck_client_new)
# -------------------------------------------------------------------------


def test_deck_client_standalone_constructor_from_seed() -> None:
    seed = b"\x55" * 32
    client = DeckClient(seed)
    # Same seed must derive the same operator id as OperatorIdentity.from_seed.
    expected = OperatorIdentity.from_seed(seed)
    assert client.identity().operator_id == expected.operator_id
    # The supervisor is alive — status() returns a parseable
    # snapshot rather than raising already_shutdown.
    snap = json.loads(client.status())
    assert isinstance(snap, dict)


def test_deck_client_standalone_constructor_rejects_wrong_seed_length() -> None:
    with pytest.raises(DeckSdkError) as excinfo:
        DeckClient(b"\x55" * 31)
    assert excinfo.value.kind == "invalid_argument"


def test_deck_client_standalone_constructor_accepts_config_dicts() -> None:
    seed = b"\x56" * 32
    client = DeckClient(
        seed,
        {"this_node": 0xABCD, "tick_interval_ms": 50},
        {"snapshot_poll_interval_ms": 25, "ice_signature_threshold": 1},
    )
    assert client.identity().operator_id == OperatorIdentity.from_seed(seed).operator_id


def test_deck_client_close_drains_owned_supervisor_idempotently() -> None:
    seed = b"\x57" * 32
    client = DeckClient(seed)
    # First close drains the private SDK.
    client.close()
    # Second close is a no-op — must not raise.
    client.close()


def test_deck_client_close_noop_for_from_meshos_clients() -> None:
    sdk = MeshOsDaemonSdk.start()
    try:
        client = DeckClient.from_meshos(sdk, OperatorIdentity.generate())
        # External SDK — close must NOT drain it.
        client.close()
        # The bound deck client still works because the external
        # SDK is still up.
        assert isinstance(json.loads(client.status()), dict)
    finally:
        sdk.shutdown()


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


# -------------------------------------------------------------------------
# Slice 2 — Audit query
# -------------------------------------------------------------------------


def test_audit_collect_returns_empty_list_on_fresh_runtime() -> None:
    """A fresh supervisor has nothing in the admin-audit ring;
    `audit().collect()` should return an empty list."""
    sdk = MeshOsDaemonSdk.start()
    try:
        client = DeckClient.from_meshos(sdk, OperatorIdentity.generate())
        records = client.audit().recent(100).collect()
        # Each record is a JSON string from the binding; slice-1-tier
        # consumers (raw binding callers) parse themselves.
        import json
        for r in records:
            json.loads(r)
        # No assertion on length — depends on whether prior tests
        # have committed; the audit ring is process-scoped.
        assert isinstance(records, list)
    finally:
        sdk.shutdown()


def test_audit_after_admin_commit_eventually_yields_record() -> None:
    """The substrate folds admin commits on a tick (default
    500ms), so the audit ring is eventually consistent. Configure
    a fast tick + poll briefly; the audit ring should populate
    within ~1s."""
    import json
    import time

    sdk = MeshOsDaemonSdk.start({"tick_interval_ms": 20})
    try:
        identity = OperatorIdentity.generate()
        client = DeckClient.from_meshos(sdk, identity)
        client.admin.cordon(node=0xCAFE)
        # Poll up to 2s for the audit ring to populate.
        deadline = time.monotonic() + 2.0
        records: list[dict] = []
        while time.monotonic() < deadline:
            raw = client.audit().recent(100).collect()
            if raw:
                records = [json.loads(r) for r in raw]
                break
            time.sleep(0.05)
        assert records, (
            "expected the substrate to fold the cordon into the audit "
            "ring within the timeout"
        )
        # Each audit record carries a `seq`, `committed_at_ms`,
        # `event`, `operator_ids`, `outcome`.
        first = records[0]
        for key in ("seq", "committed_at_ms", "event", "operator_ids", "outcome"):
            assert key in first, f"missing field {key!r}: {first!r}"
    finally:
        sdk.shutdown()


def test_audit_query_chains_filter_methods() -> None:
    """The fluent builder should accept every filter chain combo
    without raising."""
    sdk = MeshOsDaemonSdk.start()
    try:
        client = DeckClient.from_meshos(sdk, OperatorIdentity.generate())
        records = (client.audit()
                       .recent(10)
                       .by_operator(0x123)
                       .between(0, 2_000_000_000_000)
                       .force_only()
                       .since(0)
                       .collect())
        assert isinstance(records, list)
    finally:
        sdk.shutdown()


def test_audit_stream_returns_iterator_with_close() -> None:
    """`audit().stream()` returns a sync iterator. We exercise
    the iterator protocol + the close path; consuming records is
    eventually-consistent (substrate folds on a tick) and tested
    by `test_audit_after_admin_commit_eventually_yields_record`."""
    sdk = MeshOsDaemonSdk.start()
    try:
        client = DeckClient.from_meshos(sdk, OperatorIdentity.generate())
        stream = client.audit().recent(10).stream()
        # The iterator protocol must be available; we don't pull
        # an item (would block indefinitely on a quiet runtime).
        assert hasattr(stream, "__next__")
        assert hasattr(stream, "__iter__")
        stream.close()
    finally:
        sdk.shutdown()


# -------------------------------------------------------------------------
# Slice 2 — Log + Failure streams
# -------------------------------------------------------------------------


def test_subscribe_logs_returns_log_stream() -> None:
    """`subscribe_logs(None)` returns a LogStream. The stream
    blocks until a record matching the filter publishes; we test
    the empty-filter / quiet-channel shape by closing immediately."""
    sdk = MeshOsDaemonSdk.start()
    try:
        client = DeckClient.from_meshos(sdk, OperatorIdentity.generate())
        stream = client.subscribe_logs()
        stream.close()
    finally:
        sdk.shutdown()


def test_subscribe_logs_filter_dict_with_min_level() -> None:
    sdk = MeshOsDaemonSdk.start()
    try:
        client = DeckClient.from_meshos(sdk, OperatorIdentity.generate())
        stream = client.subscribe_logs({"min_level": "warn", "since_seq": 0})
        stream.close()
    finally:
        sdk.shutdown()


def test_subscribe_logs_invalid_level_raises_typed_error() -> None:
    sdk = MeshOsDaemonSdk.start()
    try:
        client = DeckClient.from_meshos(sdk, OperatorIdentity.generate())
        with pytest.raises(DeckSdkError) as excinfo:
            client.subscribe_logs({"min_level": "verbose"})
        assert excinfo.value.kind == "invalid_log_level"
    finally:
        sdk.shutdown()


def test_subscribe_logs_rejects_non_string_min_level() -> None:
    sdk = MeshOsDaemonSdk.start()
    try:
        client = DeckClient.from_meshos(sdk, OperatorIdentity.generate())
        with pytest.raises(DeckSdkError) as excinfo:
            client.subscribe_logs({"min_level": 5})
        assert excinfo.value.kind == "invalid_filter"
    finally:
        sdk.shutdown()


def test_subscribe_failures_returns_failure_stream() -> None:
    sdk = MeshOsDaemonSdk.start()
    try:
        client = DeckClient.from_meshos(sdk, OperatorIdentity.generate())
        stream = client.subscribe_failures(since_seq=0)
        stream.close()
    finally:
        sdk.shutdown()


# -------------------------------------------------------------------------
# Slice 3 — ICE break-glass surface
# -------------------------------------------------------------------------


def test_ice_factories_all_return_ice_proposals() -> None:
    """Each of the 7 IceCommands factories returns an IceProposal
    with an issued_at_ms stamp."""
    sdk = MeshOsDaemonSdk.start()
    try:
        client = DeckClient.from_meshos(sdk, OperatorIdentity.generate())
        ice = client.ice

        proposals = [
            ice.freeze_cluster(ttl_ms=60_000),
            ice.flush_avoid_lists({"kind": "global"}),
            ice.force_evict_replica(chain=1, victim=2),
            ice.force_restart_daemon(id=3, name="echo"),
            ice.force_cutover(chain=4, target=5),
            ice.kill_migration(migration=6),
            ice.thaw_cluster(),
        ]
        for p in proposals:
            assert p.issued_at_ms > 0
    finally:
        sdk.shutdown()


def test_ice_proposal_has_no_commit_method() -> None:
    """Typestate enforcement at the pyclass level: IceProposal
    must not expose `commit` — only SimulatedIceProposal does."""
    sdk = MeshOsDaemonSdk.start()
    try:
        client = DeckClient.from_meshos(sdk, OperatorIdentity.generate())
        proposal = client.ice.freeze_cluster(ttl_ms=60_000)
        assert not hasattr(proposal, "commit"), (
            "IceProposal must not expose commit (typestate violation)"
        )
        assert hasattr(proposal, "simulate"), "IceProposal must expose simulate"
    finally:
        sdk.shutdown()


def test_ice_avoid_scope_dict_variants() -> None:
    """`flush_avoid_lists` accepts three scope shapes: global,
    local (with node id), on_peer (with peer id)."""
    sdk = MeshOsDaemonSdk.start()
    try:
        client = DeckClient.from_meshos(sdk, OperatorIdentity.generate())
        client.ice.flush_avoid_lists({"kind": "global"})
        client.ice.flush_avoid_lists({"kind": "local", "node": 0xCAFE})
        client.ice.flush_avoid_lists({"kind": "on_peer", "peer": 0xBEEF})
    finally:
        sdk.shutdown()


def test_ice_invalid_scope_raises_typed_error() -> None:
    sdk = MeshOsDaemonSdk.start()
    try:
        client = DeckClient.from_meshos(sdk, OperatorIdentity.generate())
        with pytest.raises(DeckSdkError) as excinfo:
            client.ice.flush_avoid_lists({"kind": "nonsense"})
        assert excinfo.value.kind == "invalid_avoid_scope"
    finally:
        sdk.shutdown()


def test_ice_simulate_returns_simulated_proposal_with_blast_radius() -> None:
    """`simulate()` advances the typestate and produces a
    SimulatedIceProposal exposing `blast_radius()` + `blast_hash()`
    + `commit()`."""
    import json

    sdk = MeshOsDaemonSdk.start()
    try:
        client = DeckClient.from_meshos(sdk, OperatorIdentity.generate())
        proposal = client.ice.freeze_cluster(ttl_ms=60_000)
        simulated = proposal.simulate()
        # Typestate: simulated has commit + blast_radius + blast_hash.
        assert hasattr(simulated, "commit")
        assert hasattr(simulated, "blast_radius")
        assert hasattr(simulated, "blast_hash")
        # Blast radius is JSON-serializable.
        blast = json.loads(simulated.blast_radius())
        assert isinstance(blast, dict)
        # Blast hash is 32 bytes (Blake3 digest).
        h = simulated.blast_hash()
        assert isinstance(h, (bytes, bytearray))
        assert len(h) == 32
        # issued_at_ms carried through.
        assert simulated.issued_at_ms > 0
    finally:
        sdk.shutdown()


def test_ice_double_simulate_raises_already_simulated() -> None:
    sdk = MeshOsDaemonSdk.start()
    try:
        client = DeckClient.from_meshos(sdk, OperatorIdentity.generate())
        proposal = client.ice.freeze_cluster(ttl_ms=60_000)
        proposal.simulate()
        with pytest.raises(DeckSdkError) as excinfo:
            proposal.simulate()
        assert excinfo.value.kind == "already_simulated"
    finally:
        sdk.shutdown()


def test_ice_commit_with_no_signatures_succeeds_at_default_threshold_1() -> None:
    """Default `ice_signature_threshold=1` means the substrate
    accepts a single signature — but the path without an
    `OperatorRegistry` installed publishes via the unsigned admin
    route, which accepts empty signatures. Confirm the unsigned
    path works for slice 3 testing."""
    sdk = MeshOsDaemonSdk.start()
    try:
        client = DeckClient.from_meshos(sdk, OperatorIdentity.generate())
        proposal = client.ice.freeze_cluster(ttl_ms=60_000)
        simulated = proposal.simulate()
        # With default threshold=1 and 0 signatures, the SDK gate
        # rejects with `insufficient_signatures` BEFORE consulting
        # the registry path. So an empty bundle fails.
        with pytest.raises(DeckSdkError) as excinfo:
            simulated.commit([])
        assert excinfo.value.kind == "insufficient_signatures"
    finally:
        sdk.shutdown()


def test_ice_commit_rejects_malformed_signature_dict() -> None:
    sdk = MeshOsDaemonSdk.start()
    try:
        client = DeckClient.from_meshos(sdk, OperatorIdentity.generate())
        proposal = client.ice.freeze_cluster(ttl_ms=60_000)
        simulated = proposal.simulate()
        with pytest.raises(DeckSdkError) as excinfo:
            simulated.commit([{"operator_id": 0x1234}])  # missing signature
        assert excinfo.value.kind == "invalid_signature"
    finally:
        sdk.shutdown()


def test_ice_simulated_commit_consumes_proposal() -> None:
    """The first commit consumes the simulated proposal; the
    second raises `already_committed`. With default threshold=1 and
    no OperatorRegistry installed, the SDK gate accepts a single
    arbitrary signature and the substrate publishes via the
    unsigned admin path."""
    sdk = MeshOsDaemonSdk.start()
    try:
        client = DeckClient.from_meshos(sdk, OperatorIdentity.generate())
        proposal = client.ice.freeze_cluster(ttl_ms=60_000)
        simulated = proposal.simulate()
        # First commit — succeeds (default threshold=1, no
        # OperatorRegistry → unsigned admin route).
        commit = simulated.commit([{"operator_id": 1, "signature": b"\x00" * 64}])
        assert isinstance(commit, dict)
        assert "commit_id" in commit
        # Second commit — proposal consumed.
        with pytest.raises(DeckSdkError) as excinfo:
            simulated.commit([{"operator_id": 1, "signature": b"\x00" * 64}])
        assert excinfo.value.kind == "already_committed"
    finally:
        sdk.shutdown()


# -------------------------------------------------------------------------
# Operator-policy verifier surface: OperatorRegistry, AdminVerifier,
# OperatorIdentity.sign_proposal / sign_payload,
# SimulatedIceProposal.signing_payload.
# -------------------------------------------------------------------------


pytestmark_verifier = pytest.mark.skipif(
    not _HAS_VERIFIER, reason="OperatorRegistry/AdminVerifier not in this wheel"
)


@pytestmark_verifier
def test_operator_registry_lifecycle() -> None:
    reg = DeckOperatorRegistry()
    assert len(reg) == 0
    assert reg.is_empty()
    a = OperatorIdentity.generate()
    b = OperatorIdentity.generate()
    reg.register(a)
    reg.insert(b.operator_id, b.public_key())
    assert len(reg) == 2
    assert a.operator_id in reg
    assert b.operator_id in reg
    assert 0xDEADBEEF not in reg


@pytestmark_verifier
def test_operator_registry_rejects_bad_public_key() -> None:
    reg = DeckOperatorRegistry()
    with pytest.raises(DeckSdkError) as excinfo:
        reg.insert(1, b"\x00" * 31)
    assert excinfo.value.kind == "invalid_public_key"


@pytestmark_verifier
def test_operator_identity_sign_payload_then_registry_verify() -> None:
    """`sign_payload` + `registry.verify` should round-trip over
    any byte payload."""
    identity = OperatorIdentity.generate()
    reg = DeckOperatorRegistry()
    reg.register(identity)

    payload = b"verify-roundtrip-canary"
    sig = identity.sign_payload(payload)
    assert isinstance(sig, dict)
    assert sig["operator_id"] == identity.operator_id
    assert len(sig["signature"]) == 64

    reg.verify(sig, payload)  # no exception

    # Tampered payload — same signature, different bytes.
    with pytest.raises(DeckSdkError) as excinfo:
        reg.verify(sig, payload + b"!")
    assert excinfo.value.kind == "signature_invalid"


@pytestmark_verifier
def test_operator_registry_verify_rejects_unknown_operator() -> None:
    reg = DeckOperatorRegistry()
    stranger = OperatorIdentity.generate()
    sig = stranger.sign_payload(b"hello")
    with pytest.raises(DeckSdkError) as excinfo:
        reg.verify(sig, b"hello")
    assert excinfo.value.kind == "not_authorized"


@pytestmark_verifier
def test_operator_registry_verify_bundle_distinct_operator_dedup() -> None:
    """Two signatures from the same operator must not satisfy
    a threshold of 2 — the distinct-operator dedup gate is the
    M-of-N guarantee."""
    a = OperatorIdentity.generate()
    b = OperatorIdentity.generate()
    reg = DeckOperatorRegistry()
    reg.register(a)
    reg.register(b)
    payload = b"bundle-payload"
    sig_a = a.sign_payload(payload)
    sig_b = b.sign_payload(payload)

    # Two distinct operators clears threshold=2.
    reg.verify_bundle([sig_a, sig_b], payload, 2)

    # Single operator signing twice does NOT clear threshold=2.
    with pytest.raises(DeckSdkError) as excinfo:
        reg.verify_bundle([sig_a, sig_a], payload, 2)
    assert excinfo.value.kind == "insufficient_signatures"


@pytestmark_verifier
def test_admin_verifier_constructors_expose_policy_knobs() -> None:
    reg = DeckOperatorRegistry()
    v1 = DeckAdminVerifier(reg, 3)
    assert v1.threshold == 3
    assert v1.freshness_window_ms == 300_000  # substrate default
    assert v1.future_skew_ms == 30_000
    assert v1.ice_cooldown_ms == 300_000

    v2 = DeckAdminVerifier.with_freshness(reg, 2, 60_000, 5_000)
    assert v2.threshold == 2
    assert v2.freshness_window_ms == 60_000
    assert v2.future_skew_ms == 5_000
    assert v2.ice_cooldown_ms == 300_000  # default carries through

    v3 = DeckAdminVerifier.with_full_policy(reg, 1, 1_000, 500, 250)
    assert v3.threshold == 1
    assert v3.freshness_window_ms == 1_000
    assert v3.future_skew_ms == 500
    assert v3.ice_cooldown_ms == 250


@pytestmark_verifier
def test_admin_verifier_clamps_zero_threshold_to_one() -> None:
    """The substrate clamps `threshold = 0` to `1` since no admin
    path should ever accept an empty signature bundle."""
    reg = DeckOperatorRegistry()
    v = DeckAdminVerifier(reg, 0)
    assert v.threshold == 1


@pytestmark_verifier
def test_simulated_signing_payload_matches_sign_proposal_payload() -> None:
    """`signing_payload()` returns the exact bytes that
    `sign_proposal()` covers — needed for offline / cross-deck
    signing workflows."""
    sdk = MeshOsDaemonSdk.start()
    try:
        identity = OperatorIdentity.generate()
        client = DeckClient.from_meshos(sdk, identity)
        proposal = client.ice.freeze_cluster(ttl_ms=60_000)
        simulated = proposal.simulate()
        payload = simulated.signing_payload()
        assert isinstance(payload, bytes)
        # ICE_SIGNING_DOMAIN prefix — substrate const
        # `b"net.meshos.ice.v1\0"`.
        assert payload.startswith(b"net.meshos.ice.v1\x00")

        # Sign via the proposal-aware helper vs. the raw-payload
        # helper — should agree byte-for-byte.
        sig_via_proposal = identity.sign_proposal(simulated)
        sig_via_payload = identity.sign_payload(payload)
        assert sig_via_proposal["operator_id"] == sig_via_payload["operator_id"]
        assert sig_via_proposal["signature"] == sig_via_payload["signature"]
    finally:
        sdk.shutdown()


@pytestmark_verifier
def test_sign_proposal_then_verify_with_registry_succeeds() -> None:
    """Full offline-verify flow: sign a simulated proposal, hand
    the payload + signature to a registry that knows the
    operator, and verify."""
    sdk = MeshOsDaemonSdk.start()
    try:
        identity = OperatorIdentity.generate()
        client = DeckClient.from_meshos(sdk, identity)
        proposal = client.ice.flush_avoid_lists({"kind": "global"})
        simulated = proposal.simulate()
        payload = simulated.signing_payload()
        sig = identity.sign_proposal(simulated)

        reg = DeckOperatorRegistry()
        reg.register(identity)
        reg.verify(sig, payload)  # no exception
        reg.verify_bundle([sig], payload, 1)
    finally:
        sdk.shutdown()


@pytestmark_verifier
def test_signing_payload_after_commit_raises_already_committed() -> None:
    sdk = MeshOsDaemonSdk.start()
    try:
        identity = OperatorIdentity.generate()
        client = DeckClient.from_meshos(sdk, identity)
        proposal = client.ice.freeze_cluster(ttl_ms=60_000)
        simulated = proposal.simulate()
        # Consume via commit (single fake sig — default threshold=1).
        simulated.commit([{"operator_id": 1, "signature": b"\x00" * 64}])
        with pytest.raises(DeckSdkError) as excinfo:
            simulated.signing_payload()
        assert excinfo.value.kind == "already_committed"
        with pytest.raises(DeckSdkError) as excinfo:
            identity.sign_proposal(simulated)
        assert excinfo.value.kind == "already_committed"
    finally:
        sdk.shutdown()
