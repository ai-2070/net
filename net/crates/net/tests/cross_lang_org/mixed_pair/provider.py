"""The mixed-pair PROVIDER for the S4Vectors cross-language row.

A Python org server-streaming provider driven by
``tests/cross_lang_org/streaming_opening_vectors.json``'s ``scenarios.mixed_pair``
vector, over the fresh ``gen_org_scenario`` cross-org chain — a provider in ONE
language serving a caller in ANOTHER, end to end over a real mesh. The Go
orchestrator (``go/org_streaming_opening_vectors_test.go``) spawns this script
and calls it.

Protocol with the orchestrator (stdout / stdin):

* ``READY <local_addr> <public_key_hex> <node_id>`` once the mesh node is
  built, the handler registered and ``accept(caller_node_id)`` is armed.
* ``DRAINED`` (stdin, from the orchestrator) once the caller's stream has
  drained to the clean eof — the serve handle's Drop retires LIVE protected
  streams (``ServeHandle::drop``, ``mesh_rpc.rs:487``), so teardown must wait
  for the caller's drain or the terminal becomes ``0x0005`` CANCEL instead of
  eof across a process boundary.
* ``RESULT ok calls=<n>`` after one call served, every vector-pinned assertion
  held AND ``DRAINED`` arrived; then the process exits 0.
* ``RESULT fail <detail>`` on any mismatch (exit 1) — a decoder disagreement
  must not become success.
* ``RESULT fail callback-loss`` (exit 2) if no call arrives before the
  watchdog, or ``DRAINED`` never arrives — callback loss must not become
  success.

The handler below is the lane's handler surface, so the F-S3.1-2 level is
stated at it:

**Handler-drop contract (Specification §2.2 — the F-S3.1-2 level).** A protected
call runs under a per-call retire supervisor. On retirement the supervisor drops
the handler future **without a final poll**. Cancellation is observed ONLY
through the retirement observables (the request input fencing to EOF where the
shape has one; library-controlled sinks stop admitting output) and NEVER as a
handler-side event — do not assume a handler-side cancel event, a resumption,
or a ``finally``.
"""

from __future__ import annotations

import argparse
import json
import os
import queue
import socket
import sys
import threading
import time

WATCHDOG_SECS = 120.0


def _fail(detail: str, code: int = 1) -> None:
    line = f"RESULT fail {detail}"
    print(line, flush=True)
    print(f"provider: {line}", file=sys.stderr, flush=True)  # visible in the orchestrator's log
    sys.exit(code)


def _free_addr() -> str:
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        s.bind(("127.0.0.1", 0))
        return "127.0.0.1:%d" % s.getsockname()[1]
    finally:
        s.close()


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--manifest", required=True, help="the gen_org_scenario outdir")
    ap.add_argument("--vectors", required=True, help="streaming_opening_vectors.json")
    ap.add_argument("--caller-node-id", required=True, type=int)
    args = ap.parse_args()

    with open(args.vectors, encoding="utf-8") as f:
        vectors = json.load(f)
    sc = vectors["scenarios"]["mixed_pair"]
    expect = sc["expect_handler"]

    # Consume the vector file byte-exactly (the pin) before acting on it.
    chunks = [bytes.fromhex(h) for h in sc["chunks_hex"]]
    for raw, h in zip(chunks, sc["chunks_hex"]):
        if raw.hex() != h:
            _fail(f"chunk bytes do not round-trip: {h[:16]}...")
    request = bytes.fromhex(sc["request_hex"])
    if request.hex() != sc["request_hex"]:
        _fail("request bytes do not round-trip")

    with open(os.path.join(args.manifest, "manifest.json"), encoding="utf-8") as f:
        manifest = json.load(f)

    # The fresh chain must be the SAME four-party world the vectors pin.
    if manifest["caller"]["org_id_hex"] != expect["acting_org_hex"]:
        _fail("manifest acting org disagrees with the vectors")
    if manifest["provider"]["org_id_hex"] != expect["provider_org_hex"]:
        _fail("manifest provider org disagrees with the vectors")
    if manifest["granted_service"] != sc["service"]:
        _fail("manifest service disagrees with the vectors")

    import net  # the built wheel (org + streaming serve surface)

    if not hasattr(net, "serve_org_streaming"):
        _fail("the net wheel lacks serve_org_streaming (stale build?)")

    psk = manifest["psk_hex"]
    provider = manifest["provider"]
    path = lambda rel: os.path.join(args.manifest, rel)  # noqa: E731

    mesh = net.NetMesh(
        bind_addr=_free_addr(),
        psk=psk,
        identity_seed=bytes.fromhex(provider["seed_hex"]),
        heartbeat_interval_ms=200,
        permissive_channels=True,
    )
    served: dict = {}
    done = threading.Event()
    handle = None

    def handler(caller: dict, request_got: bytes, sink) -> None:
        """The org server-streaming handler (see the handler-drop contract in
        this module's docstring — the F-S3.1-2 level applies HERE)."""
        served["calls"] = served.get("calls", 0) + 1
        served["facts"] = caller
        served["request_ok"] = request_got == request
        for chunk in chunks:
            sink.send(chunk)
        done.set()

    try:
        net.install_org_authority(mesh, path(provider["authority_dir"]))
        with open(path(provider["grant_path"]), "rb") as f:
            provider_grant = f.read()
        net.install_provider_grant_audience(
            mesh, provider_grant, path(provider["grant_secret_path"])
        )

        accept_errors: list = []

        def _accept() -> None:
            try:
                mesh.accept(args.caller_node_id)
            except Exception as e:  # noqa: BLE001
                accept_errors.append(e)

        t = threading.Thread(target=_accept, daemon=True)
        t.start()

        pk = mesh.public_key
        pk_hex = pk if isinstance(pk, str) else bytes(pk).hex()

        drained: queue.Queue = queue.Queue()

        def _stdin_watch() -> None:
            for raw in sys.stdin:
                drained.put(raw.strip())
            drained.put(None)  # EOF

        threading.Thread(target=_stdin_watch, daemon=True).start()
        print(
            f"READY {mesh.local_addr} {pk_hex} {mesh.node_id}",
            flush=True,
        )

        t.join(timeout=30)
        if accept_errors:
            _fail(f"accept: {accept_errors[0]!r}")
        if t.is_alive():
            _fail("accept-thread did not complete within 30s (handshake half lost)")
        print("provider: handshake done, starting mesh + announce loop", file=sys.stderr, flush=True)
        mesh.start()
        # Register AFTER start(), mirroring every working live cell (Go
        # `runStreamingLive` and Python `test_org_live` both serve on an
        # already-started pair): the scoped-emission cache is built at
        # registration and must see the running node.
        #
        # The handle MUST stay bound for the serve lifetime: its Drop
        # unregisters the service (RAII), so a discarded handle deregisters
        # before the first announcement and the granted scoped envelope never
        # seals — the service must outlive every call it serves.
        handle = net.serve_org_streaming(mesh, sc["service"], "granted", handler, None)
        print("provider: serving, announcing", file=sys.stderr, flush=True)

        def _announce() -> None:
            # The Python mesh cannot lower min_announce_interval (scoped
            # catalog emission is throttled to ~10 s cycles) — force emission
            # so cross-process discovery converges fast (the `_Pair.announce`
            # discipline of the Python live cells).
            while not done.is_set():
                try:
                    mesh.announce_capabilities({})
                except Exception as e:  # noqa: BLE001
                    print(f"provider: announce error: {e!r}", file=sys.stderr, flush=True)
                time.sleep(1)

        threading.Thread(target=_announce, daemon=True).start()

        if not done.wait(timeout=WATCHDOG_SECS):
            _fail("callback-loss: the handler never fired", code=2)

        facts = served.get("facts", {})
        got = {
            "entity_hex": bytes(facts["entity"]).hex(),
            "acting_org_hex": bytes(facts["acting_org"]).hex(),
            "provider_org_hex": bytes(facts["provider_org"]).hex(),
            "capability_hex": bytes(facts["capability"]).hex(),
            "is_same_org": bool(facts["is_same_org"]),
        }
        for key in ("entity_hex", "acting_org_hex", "provider_org_hex", "capability_hex"):
            if got[key] != expect[key]:
                _fail(f"handler facts {key}={got[key]} want {expect[key]}")
        if got["is_same_org"] != expect["is_same_org"]:
            _fail(f"handler facts is_same_org={got['is_same_org']} want {expect['is_same_org']}")
        if not served.get("request_ok"):
            _fail("request bytes disagreed with request_hex")
        if served.get("calls", 0) != 1:
            _fail(f"calls={served.get('calls', 0)} want exactly 1")

        # Lifetime rule: the serve handle's Drop retires LIVE protected
        # streams, and the chunk queue crossing a process boundary can still
        # be draining here — wait for the caller's drain confirmation before
        # any teardown (the 0x0005-lesson of the protocol's DRAINED line).
        print("provider: awaiting DRAINED", file=sys.stderr, flush=True)
        try:
            line = drained.get(timeout=WATCHDOG_SECS)
        except queue.Empty:
            _fail("callback-loss: no DRAINED confirmation from the orchestrator", code=2)
        if line != "DRAINED":
            _fail(f"unexpected stdin line before teardown: {line!r}")

        print(f"RESULT ok calls=1 chunks={len(chunks)}", flush=True)
    finally:
        if handle is not None:
            try:
                handle.close()
            except Exception:  # noqa: BLE001
                pass
        try:
            mesh.shutdown()
        except Exception:  # noqa: BLE001
            pass


if __name__ == "__main__":
    main()
