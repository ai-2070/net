"""S4 — the Python live cell for the subnet-exported surface.

The Python twin of ``bindings/node/test/subnet_live.test.ts``: a provider inside
a protected subnet serves a NAMED export over real transport, a same-org caller
invokes it with organization authority only, and a foreign-org caller is refused
— all from artifacts MINTED BY RUST and loaded from disk. The
``gen_subnet_scenario`` example writes the whole chain (subnet authority root, an
EXPORT credential at the exact crossing, the boundary declaration, adopted org
authorities, both callers' credentials, a ``manifest.json``); this consumes the
SAME manifest the Node, Go, and C harnesses load.

Ten points, all proven here:

     1 provider construction: roots, attachment, named exports
     2 local refusal of an unknown export, before announcement
     3 serve through the frozen named-export API
     4 caller construction from real generated org credentials
     5 live public discovery
     6 a successful call_exported
     7 verified caller + organization attribution at the handler
     8 fail-closed for a foreign-org caller
     9 that denial is not retried
    10 clean close, with no callback racing teardown

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

if not hasattr(net, "serve_subnet_exported"):
    pytest.skip("net built without the org/subnet feature", allow_module_level=True)

from net import subnet as net_subnet  # noqa: E402

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
            "--example", "gen_subnet_scenario", "--", outdir,
        ],
        cwd=_CRATE_ROOT,
        check=True,
    )
    with open(os.path.join(outdir, "manifest.json"), encoding="utf-8") as f:
        return json.load(f)


def _plain_mesh(seed_hex: str, psk_hex: str):
    """A caller node — no subnet configuration at all.

    A caller presents organization authority only: it names no subnet, joins no
    subnet, and needs no trust anchor of its own.
    """
    return net.NetMesh(
        bind_addr=_free_addr(),
        psk=psk_hex,
        identity_seed=bytes.fromhex(seed_hex),
        heartbeat_interval_ms=200,
        permissive_channels=True,
    )


def _provider_mesh(manifest: dict):
    """(1) Provider construction: trust anchors, attachment, named exports.

    Every subnet input is CONSTRUCTION state, validated by Rust before the node
    exists. Application code names them; it builds no authority object.
    """
    prov = manifest["provider"]
    binding = manifest["export_binding"]
    return net.NetMesh(
        bind_addr=_free_addr(),
        psk=manifest["psk_hex"],
        identity_seed=bytes.fromhex(prov["seed_hex"]),
        heartbeat_interval_ms=200,
        permissive_channels=True,
        subnet_authorities=[
            {
                "authority_hex": a["authority_hex"],
                "root_hexes": a["root_hexes"],
                "maximum_grant_lifetime_secs": a["maximum_grant_lifetime_secs"],
            }
            for a in manifest["subnet_authorities"]
        ],
        subnet_attachment=list(prov["attachment"]),
        subnet_exports=[
            {
                "name": manifest["export_name"],
                "access": manifest["export_access"],
                "binding": {
                    "subnet": {
                        "authority_hex": binding["authority_hex"],
                        "path": {"levels": list(binding["path"])},
                    },
                    "topology_epoch": binding["topology_epoch"],
                },
            }
        ],
    )


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


def _must_not_be_served(client, service: str, payload: dict, seconds: float = 8.0) -> None:
    """Assert a call is NOT served, bounded.

    A refused caller fails locally and fast, but a call after teardown is
    different: the provider is still an announced candidate for a while, so the
    request goes out and simply gets no reply. Unbounded, that turns a correct
    refusal into a hung test. Either outcome — an exception, or nothing within
    the bound — proves "not served"; a RETURNED reply is the failure.

    Deliberately a DAEMON thread rather than a ThreadPoolExecutor: the pool's
    context manager (and the interpreter's atexit hook for pool threads) joins
    its workers, so a call that never returns hangs the suite at teardown even
    though the timeout above already elapsed. That is exactly how this helper
    first hung. A daemon thread is abandoned cleanly.
    """
    request = json.dumps(payload).encode("utf-8")
    box: dict = {}

    def _call() -> None:
        try:
            box["reply"] = client.call_exported(service, request)
        except Exception as e:  # noqa: BLE001
            box["error"] = e

    t = threading.Thread(target=_call, daemon=True)
    t.start()
    t.join(timeout=seconds)
    if "reply" in box:
        raise AssertionError(f"the call must not be served, got {box['reply']!r}")
    # An exception, or nothing within the bound — both mean "not served".


# Shells out to `cargo run --example gen_subnet_scenario` (a cold build inside
# the test), then drives a live call whose public discovery is announcement-
# throttled. The CI-wide 60 s per-test timeout is far too short; match the Node
# twin's budget.
@pytest.mark.timeout(600)
def test_live_subnet_exported_call_from_a_generated_scenario() -> None:
    # `os.makedirs` under the system temp dir — NOT `tempfile.mkdtemp`, which on
    # Windows stamps an owner-only ACE the credential loaders refuse. See the
    # org live cell for the full note.
    outdir = os.path.join(tempfile.gettempdir(), f"s4-py-{uuid.uuid4().hex}")
    os.makedirs(outdir)
    manifest = _gen_scenario(outdir)

    def path(rel: str) -> str:
        return os.path.join(outdir, rel)

    def read(rel: str) -> bytes:
        with open(path(rel), "rb") as f:
            return f.read()

    prov = manifest["provider"]
    call = manifest["caller"]
    foreign_role = manifest["foreign_caller"]
    service = manifest["exported_service"]

    provider = _provider_mesh(manifest)
    caller = _plain_mesh(call["seed_hex"], manifest["psk_hex"])
    foreign = _plain_mesh(foreign_role["seed_hex"], manifest["psk_hex"])
    client = None
    foreign_client = None
    handle = None
    try:
        net.install_org_authority(provider, path(prov["authority_dir"]))
        net.install_org_authority(caller, path(call["authority_dir"]))
        net.install_org_authority(foreign, path(foreign_role["authority_dir"]))

        # Gateway provisioning from the generated artifacts — wholesale.
        net_subnet.admin.install_gateway_credentials(
            provider, [read(prov["gateway_credentials_path"])]
        )
        net_subnet.admin.declare_boundaries(
            provider,
            {
                "authority_hex": manifest["export_binding"]["authority_hex"],
                "topology_epoch": manifest["export_binding"]["topology_epoch"],
                "boundaries": [list(p) for p in prov["boundary_paths"]],
            },
        )

        # Every accept() must complete before start().
        _handshake(caller, provider)
        _handshake(foreign, provider)
        provider.start()
        caller.start()
        foreign.start()

        # ---- (2) an unknown export is refused LOCALLY, before the service is
        #          registered or announced ----
        with pytest.raises(net.SubnetProvisionError) as ei:
            net_subnet.serve_subnet_exported(
                provider, service, manifest["unknown_export_name"], lambda c, r: r
            )
        assert ei.value.kind == "unknown_export_name"
        assert net_subnet.parse_subnet_kind(ei.value) == "unknown_export_name"

        # ---- (3) serve through the frozen named-export API ----
        state = {"calls": 0, "attribution_ok": False}

        def _handler(caller_facts: dict, request):
            state["calls"] += 1
            # ---- (7) attribution: the provider's VERIFIED view, checked
            # against the identities the manifest itself declares.
            state["attribution_ok"] = (
                caller_facts["entity"].hex() == call["entity_id_hex"]
                and caller_facts["acting_org"].hex() == call["org_id_hex"]
                and caller_facts["provider_org"].hex() == prov["org_id_hex"]
                and caller_facts["is_same_org"] is True
            )
            return {"n": request["n"] + 1, "servedBy": "py-s4"}

        handle = net_subnet.serve_subnet_exported(
            provider, service, manifest["export_name"], _handler
        )

        # ---- (4) caller credentials, from the generated files ----
        credentials = net.OrgCredentials(
            read(call["membership_path"]), read(call["dispatcher_path"]), [], []
        )
        client = net.OrgClient.bind(caller, credentials)

        # ---- (5) live public discovery, and (6) the call ----
        request = json.dumps({"n": 1}).encode("utf-8")
        reply = None
        last_err = None
        deadline = time.time() + 45
        while time.time() < deadline and reply is None:
            try:
                provider.announce_capabilities({})
                caller.announce_capabilities({})
            except Exception:  # noqa: BLE001
                pass
            try:
                reply = client.call_exported(service, request)
            except Exception as e:  # noqa: BLE001
                last_err = e
                time.sleep(0.5)

        assert reply is not None, f"the exported call was never admitted; last error: {last_err}"
        assert json.loads(reply.decode("utf-8")) == {"n": 2, "servedBy": "py-s4"}
        assert state["calls"] == 1, "handler ran exactly once"
        # ---- (7) ----
        assert state["attribution_ok"], "the handler saw the verified caller and org attribution"

        # ---- (8) fail-closed: a FOREIGN-org caller with valid credentials ----
        #
        # Its membership and dispatcher grant are correctly signed — by the
        # WRONG organization. That is what makes this a boundary test rather
        # than a decoder test.
        foreign_client = net.OrgClient.bind(
            foreign,
            net.OrgCredentials(
                read(foreign_role["membership_path"]),
                read(foreign_role["dispatcher_path"]),
                [],
                [],
            ),
        )
        before = state["calls"]
        _must_not_be_served(foreign_client, service, {"n": 50})
        assert state["calls"] == before, "the handler must never run for a refused caller"

        # ---- (9) the denial is not retried ----
        #
        # A signed proof is never resent. Observed provider-side: a second
        # refused call still never reaches the handler.
        _must_not_be_served(foreign_client, service, {"n": 51})
        assert state["calls"] == before, "no retry may smuggle a refused caller into the handler"

        # ---- (10) clean close, no callback racing teardown ----
        handle.close()
        handle.close()  # idempotent
        handle = None
        _must_not_be_served(client, service, {"n": 99})
        assert state["calls"] == 1, "no handler invocation may land after close"
    finally:
        for c in (client, foreign_client):
            if c is not None:
                try:
                    c.close()
                except Exception:  # noqa: BLE001
                    pass
        if handle is not None:
            try:
                handle.close()
            except Exception:  # noqa: BLE001
                pass
        for m in (provider, caller, foreign):
            try:
                m.shutdown()
            except Exception:  # noqa: BLE001
                pass
        shutil.rmtree(outdir, ignore_errors=True)
