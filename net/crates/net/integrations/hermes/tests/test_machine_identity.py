"""Per-machine identity tests that run WITHOUT the native ``net`` wheel.

Every other test module here skips wholesale when ``net`` is not importable
(``pytest.importorskip`` at module level). These cover pure-Python identity
plumbing — the persistent machine id (HERMES-08) and the node's transport
identity (HERMES-02) — so they run in any interpreter with pytest, against
stub ``net`` / ``net_sdk`` modules where the node build needs them.
"""

from __future__ import annotations

import threading

import pytest


# ---------------------------------------------------------------------------
# HERMES-08 — the persistent machine id is published complete, and a losing
# first-starter only ever returns the committed value.
# ---------------------------------------------------------------------------


@pytest.fixture()
def idfile(plugin, monkeypatch, tmp_path):
    monkeypatch.setenv("NET_MESH_PIN_STORE", str(tmp_path / "pins.json"))
    monkeypatch.delenv("NET_MESH_MACHINE_ID", raising=False)
    # A pre-existing empty file is polled briefly for a concurrent writer to
    # finish; keep that wait short so the suite stays fast.
    monkeypatch.setattr(plugin.delegation, "_MACHINE_ID_WAIT_SECONDS", 0.05, raising=False)
    return tmp_path / "machine-id"


def test_loser_never_returns_an_unpersisted_machine_id(plugin, idfile):
    # The audited schedule: starter A has created the id file but not yet
    # written it. Starter B must not invent (and return) its own id that is
    # never persisted — that leaves two live machine identities in one
    # namespace while every later start uses A's.
    idfile.write_text("", encoding="utf-8")
    got = plugin.delegation._persistent_machine_id()
    on_disk = idfile.read_text(encoding="utf-8").strip()
    assert got == on_disk, (got, on_disk)


_COMMITTED = "0123456789abcdef" * 2


def test_loser_adopts_the_committed_machine_id(plugin, idfile):
    idfile.write_text(_COMMITTED, encoding="utf-8")
    assert plugin.delegation._persistent_machine_id() == _COMMITTED


def test_a_partial_machine_id_is_not_adopted(plugin, idfile):
    # A fallback writer that died mid-write leaves a PREFIX of an id. It is
    # not a committed id: adopting it would make every later start a
    # different machine from the one whose write completes (or none).
    idfile.write_text(_COMMITTED[:10], encoding="utf-8")
    assert plugin.delegation._persistent_machine_id() == ""


def _age(path, seconds):
    import os
    import time

    then = time.time() - seconds
    os.utime(path, (then, then))


@pytest.mark.parametrize(
    "legacy",
    ["Build-Box-07", "0123456789ABCDEF0123456789ABCDEF", "lab-node", "abc123-custom"],
)
def test_a_legacy_or_hand_set_machine_id_is_kept(plugin, idfile, legacy, monkeypatch):
    # Upgrading must not change the node's identity: an id an older plugin
    # accepted (any non-empty value — hand-set, uppercase) is still the id. It
    # can never be a partial write of a current writer, which only writes
    # lowercase hex, so it is adopted at once, with no wait.
    monkeypatch.setattr(plugin.delegation, "_MACHINE_ID_WAIT_SECONDS", 30.0)
    idfile.write_text(legacy + "\n", encoding="utf-8")
    assert plugin.delegation._persistent_machine_id() == legacy
    assert plugin.delegation._machine_labels()[1] == legacy


def test_a_settled_prefix_is_the_persisted_id(plugin, idfile):
    # A fallback writer that died mid-write long ago left a prefix; it is all
    # that was ever persisted, and every start must agree on it.
    idfile.write_text(_COMMITTED[:10], encoding="utf-8")
    _age(idfile, plugin.delegation._MACHINE_ID_SETTLE_SECONDS + 5)
    assert plugin.delegation._persistent_machine_id() == _COMMITTED[:10]


def test_a_settled_empty_file_does_not_stall_every_start(plugin, idfile, monkeypatch):
    # An empty file abandoned long ago has no writer to wait for: the start
    # falls back without the wait, every time.
    import time

    monkeypatch.setattr(plugin.delegation, "_MACHINE_ID_WAIT_SECONDS", 30.0)
    idfile.write_text("", encoding="utf-8")
    _age(idfile, plugin.delegation._MACHINE_ID_SETTLE_SECONDS + 5)
    started = time.monotonic()
    assert plugin.delegation._persistent_machine_id() == ""
    assert time.monotonic() - started < 5.0


def test_a_failed_fallback_write_leaves_no_partial_id(plugin, idfile, monkeypatch):
    # POSIX without hard links: exclusive create, then write. A write that
    # fails after the create must remove the name it created.
    delegation = plugin.delegation
    monkeypatch.setattr(delegation, "_RENAME_NEVER_REPLACES", False)

    def _no_links(*_a, **_kw):
        raise OSError(1, "hard links not supported")

    real_fdopen = delegation.os.fdopen

    def _failing_fdopen(fd, *a, **kw):
        f = real_fdopen(fd, *a, **kw)

        class _Broken:
            def __enter__(self):
                return self

            def __exit__(self, *exc):
                f.close()
                return False

            def write(self, _data):
                raise OSError(28, "No space left on device")

        return _Broken()

    monkeypatch.setattr(delegation.os, "link", _no_links)
    monkeypatch.setattr(delegation.os, "fdopen", _failing_fdopen)
    assert delegation._persistent_machine_id() == ""
    assert not idfile.exists(), "a failed write left the id file behind"


def test_a_published_machine_id_syncs_its_directory(plugin, idfile, monkeypatch):
    synced = []
    monkeypatch.setattr(plugin.delegation, "_sync_dir", lambda p: synced.append(p))
    got = plugin.delegation._persistent_machine_id()
    assert got and synced == [idfile.parent]


def test_machine_id_is_published_with_its_contents(plugin, idfile, monkeypatch):
    # The id file must never be observable without its value: whatever
    # publishes it must already hold the complete id at that moment.
    delegation = plugin.delegation
    real_link = delegation.os.link
    seen = []

    def _checking_link(src, dst, *a, **kw):
        seen.append(open(src, encoding="utf-8").read())
        return real_link(src, dst, *a, **kw)

    monkeypatch.setattr(delegation.os, "link", _checking_link)
    got = delegation._persistent_machine_id()
    assert got and seen == [got]
    assert idfile.read_text(encoding="utf-8") == got
    # No temp file is left behind next to the id.
    assert sorted(p.name for p in idfile.parent.iterdir()) == ["machine-id"]


def test_machine_id_publishes_without_hard_link_support(plugin, idfile, monkeypatch):
    # A filesystem without hard links takes the fallback publish path; it
    # still persists the value exactly once and a second start adopts it.
    delegation = plugin.delegation

    def _no_links(*_a, **_kw):
        raise OSError(1, "hard links not supported")

    monkeypatch.setattr(delegation.os, "link", _no_links)
    got = delegation._persistent_machine_id()
    assert got and idfile.read_text(encoding="utf-8") == got
    assert sorted(p.name for p in idfile.parent.iterdir()) == ["machine-id"]
    assert delegation._persistent_machine_id() == got


@pytest.mark.parametrize("hard_links", [True, False])
def test_concurrent_first_starts_agree_on_one_machine_id(
    plugin, monkeypatch, tmp_path, hard_links
):
    # Also without hard links: the fallback publish is what the loser's
    # wait-for-the-value path exists for.
    delegation = plugin.delegation
    monkeypatch.delenv("NET_MESH_MACHINE_ID", raising=False)
    if not hard_links:

        def _no_links(*_a, **_kw):
            raise OSError(1, "hard links not supported")

        monkeypatch.setattr(delegation.os, "link", _no_links)
    for round_ in range(10):
        d = tmp_path / f"r{round_}"
        d.mkdir()
        monkeypatch.setenv("NET_MESH_PIN_STORE", str(d / "pins.json"))
        n = 12
        barrier = threading.Barrier(n)
        out = []

        def _start():
            barrier.wait()
            out.append(delegation._persistent_machine_id())

        threads = [threading.Thread(target=_start) for _ in range(n)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()
        committed = (d / "machine-id").read_text(encoding="utf-8")
        assert out == [committed] * n, (round_, set(out), committed)


# ---------------------------------------------------------------------------
# HERMES-02 — the operator root seed is authority, not a transport identity:
# machines under one root must run as distinct, stable mesh nodes.
# ---------------------------------------------------------------------------

_ROOT_SEED = bytes(range(32))


class _FakeMesh:
    built: list = []

    def __init__(self, bind_addr, psk, **kwargs):
        self.kwargs = kwargs
        _FakeMesh.built.append(self)

    def connect(self, *_a):
        pass

    def start(self):
        pass

    def shutdown(self):
        pass


class _FakeGateway:
    def __init__(self, mesh, **kwargs):
        self.mesh = mesh
        self.kwargs = kwargs


@pytest.fixture()
def stub_native(plugin, monkeypatch, tmp_path):
    """Stand-in ``net`` / ``net_sdk`` modules for ``node._build``. The stub
    ``net`` has no delegation surface, so the node takes its documented
    un-delegated path unless a test substitutes ``GatewayDelegation``."""
    import sys
    import types

    net = types.ModuleType("net")
    net.NetMesh = _FakeMesh
    sdk = types.ModuleType("net_sdk")
    sdk.AsyncCapabilityGateway = _FakeGateway
    sdk.default_pin_store_path = lambda: str(tmp_path / "pins.json")
    monkeypatch.setitem(sys.modules, "net", net)
    monkeypatch.setitem(sys.modules, "net_sdk", sdk)
    for k in ("NET_MESH_PSK", "NET_MESH_PEERS", "NET_MESH_DEVICE_ENROLLMENT", "NET_MESH_NODE_SEED"):
        monkeypatch.delenv(k, raising=False)
    monkeypatch.setenv("NET_MESH_PIN_STORE", str(tmp_path / "pins.json"))
    monkeypatch.setenv("NET_MESH_IDENTITY_SEED", _ROOT_SEED.hex())
    _FakeMesh.built = []
    return _FakeMesh


def _transport_seed_for(plugin, monkeypatch, machine_id):
    monkeypatch.setenv("NET_MESH_MACHINE_ID", machine_id)
    mesh = plugin.node._build()[0]
    return mesh.kwargs.get("identity_seed")


def test_machines_under_one_root_get_distinct_transport_identities(
    plugin, stub_native, monkeypatch
):
    a = _transport_seed_for(plugin, monkeypatch, "machine-a")
    b = _transport_seed_for(plugin, monkeypatch, "machine-b")
    assert a is not None and b is not None, "a rooted node keeps a stable seeded identity"
    assert a != b, "two machines under one root must not share a node identity"
    # The root (signing authority for delegation / enrollment) is never the
    # node's transport key.
    assert _ROOT_SEED not in (a, b)
    assert len(a) == len(b) == 32


def test_transport_identity_is_stable_per_machine(plugin, stub_native, monkeypatch):
    first = _transport_seed_for(plugin, monkeypatch, "machine-a")
    again = _transport_seed_for(plugin, monkeypatch, "machine-a")
    assert first == again, "a restart must keep the node's identity"


def test_transport_identity_and_delegation_share_one_machine_label(
    plugin, stub_native, monkeypatch
):
    # The node identity and the gateway delegation must describe the SAME
    # machine — one label read, not two that could disagree.
    labels = []

    class _FakeDelegation:
        def __init__(self, seed, *, machine_label=None, **_kw):
            labels.append(machine_label)
            self._machine_label = machine_label
            self.gateway_identity = object()
            self.gateway_id = b"\x01" * 32

        def chain_bytes(self):
            return b"chain"

    monkeypatch.setattr(plugin.delegation, "GatewayDelegation", _FakeDelegation)
    seed = _transport_seed_for(plugin, monkeypatch, "machine-a")
    assert labels == ["machine-a"]
    assert seed == plugin.delegation.transport_identity_seed(_ROOT_SEED, "machine-a")


def test_renaming_the_host_keeps_the_node_identity(plugin, stub_native, monkeypatch):
    # The persistent machine id, not the hostname, names the node: a host
    # rename must not rotate the mesh identity peers and allow-lists know.
    monkeypatch.delenv("NET_MESH_MACHINE_ID", raising=False)
    monkeypatch.setattr(plugin.delegation, "_hostname", lambda: "host-a")
    before = plugin.node._build()[0].kwargs.get("identity_seed")
    monkeypatch.setattr(plugin.delegation, "_hostname", lambda: "host-b")
    after = plugin.node._build()[0].kwargs.get("identity_seed")
    assert before is not None and before == after


def test_a_node_seed_equal_to_the_root_is_refused(plugin, stub_native, monkeypatch):
    monkeypatch.setenv("NET_MESH_NODE_SEED", _ROOT_SEED.hex())
    with pytest.raises(RuntimeError, match="NET_MESH_NODE_SEED must differ"):
        plugin.node._build()
    assert stub_native.built == []


def test_explicit_node_seed_overrides_the_derived_identity(plugin, stub_native, monkeypatch):
    explicit = bytes([7]) * 32
    monkeypatch.setenv("NET_MESH_NODE_SEED", explicit.hex())
    assert _transport_seed_for(plugin, monkeypatch, "machine-a") == explicit


def test_wrong_length_root_seed_is_a_clear_error(plugin, stub_native, monkeypatch):
    # The transport seed is derived (always 32 bytes), so the root seed's own
    # length must be checked explicitly — NetMesh no longer sees it.
    monkeypatch.setenv("NET_MESH_IDENTITY_SEED", "ab" * 31)
    with pytest.raises(RuntimeError, match="NET_MESH_IDENTITY_SEED"):
        plugin.node._build()
    assert stub_native.built == []


def test_unseeded_node_stays_ephemeral(plugin, stub_native, monkeypatch):
    monkeypatch.delenv("NET_MESH_IDENTITY_SEED", raising=False)
    mesh = plugin.node._build()[0]
    assert mesh.kwargs.get("identity_seed") is None
