"""Capability sensing through the Python binding (SENSING_OLB_COMPLETION_PLAN B1).

Every behaviour asserted here is the Rust SDK's, forwarded. The binding adds
no classification and no wait semantics of its own:

- errors: each fixture vector in ``tests/cross_lang_sensing/error_vectors.json``
  classifies through ``sensing_error_class`` exactly as Rust classifies it, and
  a live refusal raises ``SensingError`` carrying the Rust wire string (C5);
- provider: ``sensing_provide`` is the SDK's value-backed provider, and a
  ``set()`` reaches a consumer's snapshot through real signed attestations;
- consumer: a ``changed()`` timeout returns False and loses no edge, and
  ``close()`` from another thread ends a blocked ``changed()`` with
  ``SensingError`` (watch_closed), never a wake (B1-pre).

The live cell mints a same-org scenario from ``gen_subnet_scenario`` (as
``test_subnet_live.py`` does). It needs a Rust toolchain and an org-feature
wheel, and skips cleanly otherwise.
"""

from __future__ import annotations

import json
import os
import shutil
import socket
import subprocess
import tempfile
import threading
import time
import uuid

import pytest

net = pytest.importorskip("net", reason="net wheel not built")

if not hasattr(net, "sensing_watch"):
    pytest.skip("net built without the org feature", allow_module_level=True)

_HERE = os.path.dirname(os.path.abspath(__file__))
_CRATE_ROOT = os.path.abspath(os.path.join(_HERE, "..", "..", ".."))
_FIXTURE = os.path.join(_CRATE_ROOT, "tests", "cross_lang_sensing", "error_vectors.json")


def _vectors() -> list[dict]:
    with open(_FIXTURE, encoding="utf-8") as f:
        return json.load(f)["vectors"]


def test_every_fixture_vector_classifies_exactly_as_rust_does() -> None:
    vectors = _vectors()
    assert vectors
    for v in vectors:
        assert net.sensing_error_class(v["wire"]) == {
            "kind": v["kind"],
            "terminal": v["terminal"],
        }, v["wire"]
    assert net.sensing_error_class("org:rpc:timeout: x") is None
    assert net.sensing_error_class("sensing:not_a_kind: x") is None


def _free_addr() -> str:
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        s.bind(("127.0.0.1", 0))
        return "127.0.0.1:%d" % s.getsockname()[1]
    finally:
        s.close()


def test_a_live_refusal_raises_the_rust_wire_string() -> None:
    mesh = net.NetMesh(bind_addr=_free_addr(), psk="42" * 32)
    try:
        with pytest.raises(net.SensingError) as caught:
            net.sensing_watch(mesh, "nrpc:anything")
        disabled = next(v for v in _vectors() if v["kind"] == "disabled")
        assert str(caught.value) == disabled["wire"]
        assert net.sensing_error_class(str(caught.value)) == {"kind": "disabled", "terminal": True}
    finally:
        mesh.shutdown()


def _gen_scenario(outdir: str) -> dict:
    subprocess.run(
        [
            "cargo", "run", "-q", "-p", "net-mesh-sdk", "--features", "net,cortex,fixtures",
            "--example", "gen_subnet_scenario", "--", outdir,
        ],
        cwd=_CRATE_ROOT,
        check=True,
    )
    with open(os.path.join(outdir, "manifest.json"), encoding="utf-8") as f:
        return json.load(f)


def _handshake(connector, acceptor) -> None:
    errors: list[Exception] = []

    def _accept() -> None:
        try:
            acceptor.accept(connector.node_id)
        except Exception as e:  # noqa: BLE001
            errors.append(e)

    t = threading.Thread(target=_accept, daemon=True)
    t.start()
    time.sleep(0.05)
    connector.connect(acceptor.local_addr, acceptor.public_key, acceptor.node_id)
    t.join(timeout=5)
    if errors:
        raise errors[0]


SERVICE = "sensing.py.live"
TAG = f"nrpc:{SERVICE}"


# Live cells can exceed the suite-wide 30 s watchdog during scenario generation
# and convergence, exactly like the other live cells (test_org_live.py).
@pytest.mark.timeout(600)
def test_a_value_set_reaches_the_consumer_and_close_ends_a_blocked_wait() -> None:
    # os.makedirs, NOT tempfile.mkdtemp: on Windows mkdtemp's owner-only ACL is
    # refused by the audience-secret loader (see test_subnet_live.py).
    outdir = os.path.join(tempfile.gettempdir(), f"sensing-py-{uuid.uuid4().hex}")
    os.makedirs(outdir)
    meshes = []
    try:
        m = _gen_scenario(outdir)

        def make(seed_hex: str, name: str):
            mesh = net.NetMesh(
                bind_addr=_free_addr(),
                psk=m["psk_hex"],
                identity_seed=bytes.fromhex(seed_hex),
                heartbeat_interval_ms=200,
                permissive_channels=True,
                enable_sensing=True,
                sensing_incarnation_path=os.path.join(outdir, f"{name}.incarnation"),
            )
            meshes.append(mesh)
            return mesh

        provider = make(m["provider"]["seed_hex"], "provider")
        consumer = make(m["caller"]["seed_hex"], "consumer")
        quiet = make(m["foreign_caller"]["seed_hex"], "quiet")
        prov_dir = os.path.join(outdir, m["provider"]["authority_dir"])
        cons_dir = os.path.join(outdir, m["caller"]["authority_dir"])
        net.install_org_authority(provider, prov_dir)
        shutil.copyfile(
            os.path.join(prov_dir, "owner-audience.key"),
            os.path.join(cons_dir, "owner-audience.key"),
        )
        net.install_org_authority(consumer, cons_dir)
        net.install_org_authority(quiet, os.path.join(outdir, m["foreign_caller"]["authority_dir"]))
        _handshake(consumer, provider)
        provider.start()
        consumer.start()
        quiet.start()
        handle = net.serve_org(provider, SERVICE, "same_org", lambda caller, req: b"ok", None)

        ready = net.sensing_provide(provider, TAG, "ready", estimated_start_ms=40)
        assert ready.get() == {"state": "ready", "estimated_start_ms": 40.0}
        provider_id = provider.node_id

        watch = net.sensing_watch(consumer, TAG)

        def until(what: str, predicate) -> None:
            deadline = time.monotonic() + 60
            rows = None
            while time.monotonic() < deadline:
                rows = watch.snapshot().providers
                if predicate(rows):
                    return
                provider.announce_capabilities({})
                consumer.announce_capabilities({})
                watch.changed(timeout_ms=500)
            raise AssertionError(f"{what}: last rows {rows}")

        until(
            "the provider never read ready",
            lambda rows: any(r["node_id"] == provider_id and r["readiness"] == "ready" for r in rows),
        )
        ready.set("not_ready", reason=3)
        until(
            "a not_ready set never reached the consumer",
            lambda rows: any(
                r["node_id"] == provider_id and r["viability"] == "not_viable" for r in rows
            ),
        )

        # A timeout too large for a Duration is a ValueError, never a panic.
        with pytest.raises(ValueError):
            watch.changed(timeout_ms=1e308)
        assert not watch.closed

        # After close(), every wait raises closed. (A close while a wait is
        # still BLOCKED is witnessed on the quiet node below: here a live
        # provider keeps producing real edges, so "still blocked" is not
        # observable.)
        assert watch.close() is True
        with pytest.raises(net.SensingError) as closed:
            watch.changed(timeout_ms=1000)
        assert net.sensing_error_class(str(closed.value)) == {
            "kind": "watch_closed",
            "terminal": True,
        }
        assert watch.close() is False

        # B1-pre on the QUIET node: no peers and no providers, so only the
        # 1 s population floor could wake the wait. A close() from another
        # thread ends the blocked changed() as closed, never as a wake.
        quiet_watch = net.sensing_watch(quiet, "nrpc:quiet.capability")
        quiet_watch.snapshot()
        # B1-pre: on a quiet watch (no edges, and the population floor is 1 s)
        # a short timeout returns False, and the watch stays open.
        assert quiet_watch.changed(timeout_ms=50) is False
        assert not quiet_watch.closed
        outcome: list = []

        def blocked() -> None:
            try:
                outcome.append(("woke", quiet_watch.changed()))
            except net.SensingError as e:
                outcome.append(("closed", str(e)))

        t = threading.Thread(target=blocked, daemon=True)
        t.start()
        time.sleep(0.1)
        assert not outcome, "precondition: the wait is blocked"
        assert quiet_watch.close() is True
        t.join(timeout=5)
        assert outcome, "a blocked wait must end promptly on close()"
        assert outcome[0][0] == "closed", f"a wait blocked at close() must not wake: {outcome}"
        assert net.sensing_error_class(outcome[0][1]) == {"kind": "watch_closed", "terminal": True}

        assert ready.close() is True
        assert ready.set("ready") is False
        handle.close()
    finally:
        for mesh in meshes:
            try:
                mesh.shutdown()
            except Exception:  # noqa: BLE001
                pass
        shutil.rmtree(outdir, ignore_errors=True)
