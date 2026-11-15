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


def test_loser_adopts_the_committed_machine_id(plugin, idfile):
    idfile.write_text("abc123", encoding="utf-8")
    assert plugin.delegation._persistent_machine_id() == "abc123"


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


def test_concurrent_first_starts_agree_on_one_machine_id(plugin, monkeypatch, tmp_path):
    delegation = plugin.delegation
    monkeypatch.delenv("NET_MESH_MACHINE_ID", raising=False)
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
