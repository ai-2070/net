"""OSDK-L X2 — a live admitted cross-org call through the Python binding.

The Python twin of ``bindings/node/test/org_live.test.ts``: a provider node
serves a Granted capability that a caller node — in a different organization —
invokes over real transport, using credentials MINTED BY RUST and loaded from
disk. The ``gen_org_scenario`` example writes the whole issuance chain (adopted
authorities, credential bytes, 0600 audience-secret files, a ``manifest.json``);
this consumes the SAME manifest a Go / Node harness loads.

Closes the "live admitted call owed with X2" gap the plan flags for Python:
``test_org_binding.py`` proves the refusal paths; this proves the admitted path.

Env: needs a Rust toolchain (to generate the scenario) and the wheel built with
the ``org`` feature; skips cleanly otherwise.
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

if not hasattr(net, "install_org_authority"):
    pytest.skip("net built without the org feature", allow_module_level=True)

_HERE = os.path.dirname(os.path.abspath(__file__))
# bindings/python/tests -> crates/net (the cargo workspace root).
_CRATE_ROOT = os.path.abspath(os.path.join(_HERE, "..", "..", ".."))


def _free_addr() -> str:
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        s.bind(("127.0.0.1", 0))
        return "127.0.0.1:%d" % s.getsockname()[1]
    finally:
        s.close()


def _gen_scenario(outdir: str) -> dict:
    subprocess.run(
        [
            "cargo", "run", "-q", "-p", "net-mesh-sdk", "--features", "net,cortex,fixtures",
            "--example", "gen_org_scenario", "--", outdir,
        ],
        cwd=_CRATE_ROOT,
        check=True,
    )
    with open(os.path.join(outdir, "manifest.json"), encoding="utf-8") as f:
        return json.load(f)


def _mesh(seed_hex: str, psk_hex: str):
    return net.NetMesh(
        bind_addr=_free_addr(),
        psk=psk_hex,
        identity_seed=bytes.fromhex(seed_hex),
        heartbeat_interval_ms=200,
        permissive_channels=True,
    )


def _handshake(connector, acceptor) -> None:
    # b.accept on a thread while a.connect fires — the conftest mesh_pair shape.
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


def test_live_cross_org_call_from_a_generated_scenario() -> None:
    # `os.makedirs` under the system temp dir — NOT `tempfile.mkdtemp`, which on
    # Windows stamps an owner-only S-1-3-4 (Owner Rights) full-access ACE that
    # the audience-secret loader (rightly) refuses on the inherited secret file.
    # A plain makedirs inherits the standard owner+SYSTEM+Admins ACL, the way the
    # Rust and Node cells' temp dirs do.
    outdir = os.path.join(tempfile.gettempdir(), f"x2-py-{uuid.uuid4().hex}")
    os.makedirs(outdir)
    manifest = _gen_scenario(outdir)

    def path(rel: str) -> str:
        return os.path.join(outdir, rel)

    prov = manifest["provider"]
    call = manifest["caller"]
    psk = manifest["psk_hex"]
    service = manifest["granted_service"]

    provider = _mesh(prov["seed_hex"], psk)
    caller = _mesh(call["seed_hex"], psk)
    client = None
    handle = None
    try:
        # Both nodes load their adopted authority (the binding startup step).
        net.install_org_authority(provider, path(prov["authority_dir"]))
        net.install_org_authority(caller, path(call["authority_dir"]))

        _handshake(caller, provider)
        provider.start()
        caller.start()

        seen = {"cross_org": False}

        def _handler(caller_facts: dict, request: bytes) -> bytes:
            seen["cross_org"] = (
                caller_facts["is_same_org"] is False
                and len(caller_facts["entity"]) == 32
            )
            body = json.loads(request.decode("utf-8"))
            return json.dumps({"n": body["n"] + 1, "servedBy": "py-provider"}).encode("utf-8")

        handle = net.serve_org(provider, service, "granted", _handler, None)

        with open(path(prov["grant_path"]), "rb") as f:
            provider_grant = f.read()
        net.install_provider_grant_audience(provider, provider_grant, path(prov["grant_secret_path"]))

        with open(path(call["membership_path"]), "rb") as f:
            membership = f.read()
        with open(path(call["dispatcher_path"]), "rb") as f:
            dispatcher = f.read()
        with open(path(call["grant_path"]), "rb") as f:
            caller_grant = f.read()
        credentials = net.OrgCredentials(
            membership, dispatcher, [caller_grant], [path(call["grant_secret_path"])]
        )
        client = net.OrgClient.bind(caller, credentials)

        request = json.dumps({"n": 7}).encode("utf-8")
        reply = None
        last_err = None
        # The Python mesh can't lower min_announce_interval (10s default), so the
        # scoped emission is throttled — wait through a few cycles.
        deadline = time.time() + 45
        while time.time() < deadline and reply is None:
            try:
                provider.announce_capabilities({})
                caller.announce_capabilities({})
            except Exception:  # noqa: BLE001
                pass
            try:
                reply = client.call(service, request)
            except Exception as e:  # noqa: BLE001
                last_err = e
                time.sleep(1)

        assert reply is not None, f"the cross-org call was never admitted; last error: {last_err}"
        assert json.loads(reply.decode("utf-8")) == {"n": 8, "servedBy": "py-provider"}
        assert seen["cross_org"], "four-party attribution reached the handler"
    finally:
        if client is not None:
            try:
                client.close()
            except Exception:  # noqa: BLE001
                pass
        if handle is not None:
            try:
                handle.close()
            except Exception:  # noqa: BLE001
                pass
        provider.shutdown()
        caller.shutdown()
        shutil.rmtree(outdir, ignore_errors=True)
