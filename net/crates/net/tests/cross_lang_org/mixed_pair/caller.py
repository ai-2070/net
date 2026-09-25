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
import queue
import socket
import subprocess
import sys
import threading
import time

_HERE = os.path.dirname(os.path.abspath(__file__))
_PROVIDER = os.path.join(_HERE, "provider.py")
# The orchestrator-pipe watchdog (matches the Go row's readLine deadline):
# EVERY provider read — the pipe lines AND the stream drain — is bounded by
# it, so a provider hang fails the row instead of wedging the caller on
# exactly the failure class this harness reproduces.
_WATCHDOG = 120.0


class _PipeTimeout(Exception):
    """An orchestrator read exceeded its bound — the peer hung mid-protocol."""


def _bounded(read, phase: str, timeout: float = _WATCHDOG):
    """Run ``read()`` to completion, bounded by ``timeout`` seconds.

    A pipe ``readline()`` — and the ``list(stream)`` drain — block forever
    when the peer stops writing; the pump thread hands the result over with a
    real deadline (threads, not ``select``: Windows pipes are not selectable).
    A read error is re-raised on the caller's side — never swallowed into a
    line of text.
    """
    box: queue.Queue = queue.Queue(maxsize=1)

    def _pump() -> None:
        try:
            box.put(read())
        except BaseException as exc:  # re-raised below, on the caller's side
            box.put(exc)

    threading.Thread(target=_pump, daemon=True).start()
    try:
        got = box.get(timeout=timeout)
    except queue.Empty:
        raise _PipeTimeout(
            f"{phase}: no orchestrator read within {timeout}s — "
            "a hung provider must not become success"
        ) from None
    if isinstance(got, BaseException):
        raise got
    return got


def _readline(stream, phase: str, timeout: float = _WATCHDOG) -> str:
    """One line from ``stream``, bounded by ``timeout`` seconds."""
    return _bounded(stream.readline, phase, timeout)


def _drain_stream(stream, phase: str = "stream drain", timeout: float = _WATCHDOG) -> list:
    """Every chunk of ``stream``, bounded by ``timeout`` seconds.

    The drain is a read of provider-emitted data like the pipe lines: a
    provider that hangs mid-stream must fail loudly here, not wedge the caller
    until the call deadline.
    """
    return _bounded(lambda: list(stream), phase, timeout)


def _probe_find_nodes(mesh, tag: str):
    """Failure-path state probe. The call shape MUST match
    ``NetMesh.find_nodes(filter: dict)`` (``_net.pyi``) — a wrong shape is a
    TypeError that must surface and name the real failure, never be swallowed
    into a printed error string."""
    return mesh.find_nodes({"require_tags": [tag]})


def _probe_find_nodes_scoped(mesh, tag: str):
    """Failure-path state probe. The call shape MUST match
    ``NetMesh.find_nodes_scoped(filter: dict, scope: dict)`` (``_net.pyi``);
    same no-TypeError-swallowing rule as :func:`_probe_find_nodes`."""
    return mesh.find_nodes_scoped({"require_tags": [tag]}, {"kind": "any"})


def _check_pins(sc: dict, chunks: list) -> None:
    """The vector's pinned verdict — the SAME values the Go row asserts: the
    terminal is the clean eof and the observed chunk count equals
    ``chunk_count``. Deriving either from ``chunks_hex`` would follow a fixture
    flip instead of reddening it."""
    if sc["expect_terminal"] != "eof":
        raise AssertionError(f"expect_terminal {sc['expect_terminal']!r} != 'eof'")
    if len(chunks) != sc["chunk_count"]:
        raise AssertionError(f"chunks = {len(chunks)}, want chunk_count {sc['chunk_count']}")


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
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=sys.stderr,
        text=True,
    )
    try:
        ready = _readline(proc.stdout, "READY").strip()
        fields = ready.split()
        if len(fields) != 4 or fields[0] != "READY":
            _fail(f"bad READY line: {ready!r}")
        provider_addr, provider_pub, provider_node = fields[1], fields[2], int(fields[3])

        # The CALLER only connects (the `_handshake` / Go-row shape: the
        # acceptor accepts, the connector connects). A caller-side accept()
        # arm that never completes (the provider never initiates) leaves
        # `accept_in_flight` at `start()`, and the core then refuses to spawn
        # the dispatch loop — warn-only, so the caller would silently process
        # ZERO inbound packets (the discovery starve this harness hit once).
        mesh.connect(provider_addr, provider_pub, provider_node)
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
            # The catalog probes call the REAL `_net.pyi` signatures and are
            # deliberately NOT wrapped: a wrong call shape is a TypeError that
            # must surface and name the real failure, not be swallowed into a
            # printed error instead of state.
            tag = sc["granted_capability_tag"]
            print(f"probe: find_nodes={_probe_find_nodes(mesh, tag)}", file=sys.stderr, flush=True)
            print(f"probe: find_nodes_scoped={_probe_find_nodes_scoped(mesh, tag)}", file=sys.stderr, flush=True)
            _fail(f"the call never converged: {last!r}")

        try:
            # Bounded like the pipe lines: the `list(stream)` drain reads
            # provider-emitted data, and a provider hanging mid-stream is the
            # exact failure class this harness reproduces — it must time out
            # loudly, not wedge the caller until the call deadline.
            chunks = _drain_stream(stream)
        finally:
            stream.close()
            client.close()

        if chunks != chunks_expected:
            _fail(f"chunks {chunks!r} != pinned {chunks_expected!r}")

        # The vector's pinned verdict (the Go row asserts the same values).
        try:
            _check_pins(sc, chunks)
        except AssertionError as exc:
            _fail(str(exc))

        # Lifetime contract: the provider sequences its teardown AFTER this
        # drain confirmation — its serve handle's Drop retires live protected
        # streams (a premature close becomes a 0x0005 CANCEL terminal instead
        # of the clean eof across a process boundary).
        proc.stdin.write("DRAINED\n")
        proc.stdin.flush()

        result = _readline(proc.stdout, "RESULT").strip()
        print(f"provider: {result}", flush=True)  # the two-sided line (echo for receipts)
        want = f"RESULT ok calls=1 chunks={sc['chunk_count']}"
        if result != want:
            _fail(f"provider verdict {result!r} != {want!r}")
        print(
            f"CALLER PASS shape={sc['shape']} chunks={len(chunks)} "
            f"bytes={sum(len(c) for c in chunks)}",
            flush=True,
        )
    except _PipeTimeout as exc:
        _fail(str(exc))  # a hung provider is a failed row, not a wedged caller
    finally:
        mesh.shutdown()
        if proc.poll() is None:
            proc.kill()
        proc.wait(timeout=10)


if __name__ == "__main__":
    main()
