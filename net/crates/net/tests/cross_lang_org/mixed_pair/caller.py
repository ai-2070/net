"""The mixed-pair CALLER — Python side, cross-process against `provider.py`.

Isolation + the mixed pair's Python half: a caller in one OS process against a
provider in another, over a real mesh, driven by
``streaming_opening_vectors.json``'s ``scenarios.mixed_pair`` vector and the
fresh ``gen_org_scenario`` chain.

Usage::

    python caller.py --manifest <gen_org_scenario outdir> --vectors <fixture>

It spawns ``provider.py`` beside itself (same interpreter), performs the Go
orchestrator's exact protocol (READY line in, RESULT line out) and exits 0 only
when every vector-pinned assertion held on BOTH sides. Any mismatch, or a
handler that never fires (callback loss), exits non-zero — decoder
disagreement and callback loss must not become success.
"""

from __future__ import annotations

import argparse
import json
import os
import socket
import subprocess
import sys
import threading
import time

_HERE = os.path.dirname(os.path.abspath(__file__))
_PROVIDER = os.path.join(_HERE, "provider.py")
_WATCHDOG = 120.0


def _free_addr() -> str:
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        s.bind(("127.0.0.1", 0))
        return "127.0.0.1:%d" % s.getsockname()[1]
    finally:
        s.close()


def _fail(detail: str) -> None:
    print(f"CALLER FAIL {detail}", flush=True)
    sys.exit(1)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--manifest", required=True)
    ap.add_argument("--vectors", required=True)
    args = ap.parse_args()

    with open(args.vectors, encoding="utf-8") as f:
        vectors = json.load(f)
    sc = vectors["scenarios"]["mixed_pair"]
    with open(os.path.join(args.manifest, "manifest.json"), encoding="utf-8") as f:
        manifest = json.load(f)

    import net

    psk = manifest["psk_hex"]
    caller_role = manifest["caller"]
    path = lambda rel: os.path.join(args.manifest, rel)  # noqa: E731

    mesh = net.NetMesh(
        bind_addr=_free_addr(),
        psk=psk,
        identity_seed=bytes.fromhex(caller_role["seed_hex"]),
        heartbeat_interval_ms=200,
        permissive_channels=True,
    )

    with open(path(caller_role["membership_path"]), "rb") as f:
        membership = f.read()
    with open(path(caller_role["dispatcher_path"]), "rb") as f:
        dispatcher = f.read()
    with open(path(caller_role["grant_path"]), "rb") as f:
        grant = f.read()
    credentials = net.OrgCredentials(
        membership, dispatcher, [grant], [path(caller_role["grant_secret_path"])]
    )
    net.install_org_authority(mesh, path(caller_role["authority_dir"]))
    client = net.OrgClient.bind(mesh, credentials)

    proc = subprocess.Popen(
        [
            sys.executable,
            _PROVIDER,
            "--manifest", args.manifest,
            "--vectors", args.vectors,
            "--caller-node-id", str(mesh.node_id),
        ],
        stdout=subprocess.PIPE,
        stderr=sys.stderr,
        text=True,
    )
    try:
        ready = proc.stdout.readline().strip()
        fields = ready.split()
        if len(fields) != 4 or fields[0] != "READY":
            _fail(f"bad READY line: {ready!r}")
        provider_addr, provider_pub, provider_node = fields[1], fields[2], int(fields[3])

        errors: list = []

        def _accept() -> None:
            try:
                mesh.accept(provider_node)
            except Exception as e:  # noqa: BLE001
                errors.append(e)

        t = threading.Thread(target=_accept, daemon=True)
        t.start()
        time.sleep(0.05)
        mesh.connect(provider_addr, provider_pub, provider_node)
        t.join(timeout=10)
        if errors:
            _fail(f"accept: {errors[0]!r}")
        mesh.start()

        request = bytes.fromhex(sc["request_hex"])
        chunks_expected = [bytes.fromhex(h) for h in sc["chunks_hex"]]

        stream = None
        end = time.time() + 60.0
        last = None
        while time.time() < end:
            mesh.announce_capabilities({})
            try:
                stream = client.call_streaming(sc["service"], request)
                break
            except Exception as e:  # noqa: BLE001
                last = e
                time.sleep(1)
        if stream is None:
            # Localize the drop: which layer lost the provider?
            try:
                print(f"probe: peer_count={mesh.peer_count}", file=sys.stderr, flush=True)
            except Exception as e:  # noqa: BLE001
                print(f"probe: peer_count err {e!r}", file=sys.stderr, flush=True)
            try:
                print(f"probe: discovered_nodes={mesh.discovered_nodes()}", file=sys.stderr, flush=True)
            except Exception as e:  # noqa: BLE001
                print(f"probe: discovered_nodes err {e!r}", file=sys.stderr, flush=True)
            try:
                tag = sc["granted_capability_tag"]
                print(f"probe: find_nodes={mesh.find_nodes(tag)}", file=sys.stderr, flush=True)
            except Exception as e:  # noqa: BLE001
                print(f"probe: find_nodes err {e!r}", file=sys.stderr, flush=True)
            try:
                tag = sc["granted_capability_tag"]
                print(f"probe: find_nodes_scoped={mesh.find_nodes_scoped(tag)}", file=sys.stderr, flush=True)
            except Exception as e:  # noqa: BLE001
                print(f"probe: find_nodes_scoped err {e!r}", file=sys.stderr, flush=True)
            _fail(f"the call never converged: {last!r}")

        try:
            chunks = list(stream)
        finally:
            stream.close()
            client.close()

        if chunks != chunks_expected:
            _fail(f"chunks {chunks!r} != pinned {chunks_expected!r}")

        result = proc.stdout.readline().strip()
        want = f"RESULT ok calls=1 chunks={len(chunks_expected)}"
        if result != want:
            _fail(f"provider verdict {result!r} != {want!r}")
        print(
            f"CALLER PASS shape={sc['shape']} chunks={len(chunks)} "
            f"bytes={sum(len(c) for c in chunks)}",
            flush=True,
        )
    finally:
        mesh.shutdown()
        if proc.poll() is None:
            proc.kill()
        proc.wait(timeout=10)


if __name__ == "__main__":
    main()
