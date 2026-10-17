"""The Python driver for the cross-process, cross-language org matrix.

Implements BOTH roles (``provider`` / ``caller``) for BOTH shapes (``unary`` /
``server-streaming``) over the ``net`` wheel, exactly as ``PROTOCOL.md`` (this
directory) specifies. ``run_matrix.py`` is the only thing that knows about
more than one language; this script knows only the protocol.

Usage::

    python driver.py provider --manifest <dir> --shape <unary|server-streaming> --caller-node-id <u64>
    python driver.py caller   --manifest <dir> --shape <unary|server-streaming>

stdout carries ONLY protocol lines (each flushed at once); every diagnostic
goes to stderr.

Lessons this inherits from ``../mixed_pair/`` (the first cross-process Python
pair):

* The caller only ``connect()``s and never arms ``accept()``: a pending accept
  makes ``start()`` refuse.
* The provider keeps its serve handle alive until ``DRAINED``: the handle's
  drop retires LIVE protected streams, so a stream still draining across the
  process boundary would end with a CANCEL terminal instead of a clean end.
* The provider re-announces in a loop: the Python mesh cannot lower the
  scoped-announce interval, so without it cross-process discovery waits out
  ~10 s emission cycles.
"""

from __future__ import annotations

import argparse
import json
import os
import queue
import sys
import threading
import time

SHAPES = ("unary", "server-streaming")

REQUEST = {"n": 7}
UNARY_RESPONSE = {"n": 8}
STREAM_ITEMS = [{"i": 0}, {"i": 1}, {"i": 2}]

BIND_ADDR = "127.0.0.1:0"
HEARTBEAT_MS = 200

ACCEPT_TIMEOUT_S = 30.0
HANDLER_WATCHDOG_S = 120.0
DRAINED_TIMEOUT_S = 120.0
PROVIDER_LINE_TIMEOUT_S = 120.0
DISCOVERY_RETRY_S = 0.5
DISCOVERY_BUDGET_S = 60.0
STREAM_DRAIN_TIMEOUT_S = 120.0
ANNOUNCE_EVERY_S = 1.0


# ---------------------------------------------------------------------------
# Small helpers
# ---------------------------------------------------------------------------


def _emit(line: str) -> None:
    """One protocol line on stdout, flushed immediately."""
    print(line, flush=True)


def _diag(role: str, msg: str) -> None:
    print(f"{role}: {msg}", file=sys.stderr, flush=True)


def _one_line(text: object) -> str:
    """Collapse a detail to one line (a protocol line must not span lines)."""
    return " ".join(str(text).split()) or "<empty>"


def _encode(value: object) -> bytes:
    # Compact JSON, no spaces — identical bytes in every language.
    return json.dumps(value, separators=(",", ":")).encode("utf-8")


def _decode(data: object) -> object:
    return json.loads(bytes(data).decode("utf-8"))


def _hex(value: object) -> str:
    return value if isinstance(value, str) else bytes(value).hex()


class _Timeout(Exception):
    pass


def _bounded(fn, phase: str, timeout: float):
    """Run ``fn()`` on a daemon thread with a real deadline (Windows pipes are
    not selectable, so threads rather than ``select``). Re-raises ``fn``'s own
    exception on this side."""
    box: queue.Queue = queue.Queue(maxsize=1)

    def _pump() -> None:
        try:
            box.put((True, fn()))
        except BaseException as exc:  # noqa: BLE001 - re-raised below
            box.put((False, exc))

    threading.Thread(target=_pump, daemon=True).start()
    try:
        ok, value = box.get(timeout=timeout)
    except queue.Empty:
        raise _Timeout(f"{phase}: nothing within {timeout:.0f}s") from None
    if not ok:
        raise value
    return value


def _load_manifest(scenario_dir: str) -> dict:
    with open(os.path.join(scenario_dir, "manifest.json"), encoding="utf-8") as f:
        return json.load(f)


def _read_bytes(path: str) -> bytes:
    with open(path, "rb") as f:
        return f.read()


def _build_mesh(net, seed_hex: str, psk_hex: str):
    return net.NetMesh(
        bind_addr=BIND_ADDR,
        psk=psk_hex,
        identity_seed=bytes.fromhex(seed_hex),
        heartbeat_interval_ms=HEARTBEAT_MS,
        permissive_channels=True,
    )


def _is_local_discovery(net, exc: BaseException) -> bool:
    """True only when the failure is the LOCAL discovery domain (no request
    was sent). The native call raises ``OrgDiscoveryError`` directly; the
    vocabulary parse is a belt-and-braces fallback for the same domain."""
    disc = getattr(net, "OrgDiscoveryError", None)
    if disc is not None and isinstance(exc, disc):
        return True
    try:
        from net.org import parse_org_error
    except ImportError:
        return False
    return parse_org_error(str(exc)).domain == "discovery"


# ---------------------------------------------------------------------------
# Caller
# ---------------------------------------------------------------------------


def run_caller(args) -> int:
    role = "caller"
    import net

    manifest = _load_manifest(args.manifest)
    me = manifest["caller"]
    service = manifest["granted_service"]
    path = lambda rel: os.path.join(args.manifest, rel)  # noqa: E731

    # 1. Build (do NOT start).
    mesh = _build_mesh(net, me["seed_hex"], manifest["psk_hex"])
    client = None
    stream = None
    code = 1
    try:
        # 2. ID line.
        _emit(f"ID {mesh.node_id}")

        # 3. PROVIDER line from stdin.
        try:
            line = _bounded(sys.stdin.readline, "PROVIDER line", PROVIDER_LINE_TIMEOUT_S)
        except _Timeout as exc:
            _emit(f"RESULT fail {_one_line(exc)}")
            return 1
        fields = line.split()
        if len(fields) != 4 or fields[0] != "PROVIDER":
            _emit(f"RESULT fail bad PROVIDER line: {_one_line(repr(line))}")
            return 1
        p_addr, p_pub, p_node = fields[1], fields[2], int(fields[3])
        _diag(role, f"provider at {p_addr} node {p_node}")

        # 4. connect (never accept), start, install the authority.
        mesh.connect(p_addr, p_pub, p_node)
        mesh.start()
        net.install_org_authority(mesh, path(me["authority_dir"]))

        # 5. Bind the credentials.
        credentials = net.OrgCredentials(
            _read_bytes(path(me["membership_path"])),
            _read_bytes(path(me["dispatcher_path"])),
            [_read_bytes(path(me["grant_path"]))],
            [path(me["grant_secret_path"])],
        )
        client = net.OrgClient.bind(mesh, credentials)

        # 6. Call, retrying ONLY local discovery failures.
        request = _encode(REQUEST)
        if args.shape == "unary":
            open_call = lambda: client.call(service, request)  # noqa: E731
        else:
            open_call = lambda: client.call_streaming(service, request)  # noqa: E731

        deadline = time.monotonic() + DISCOVERY_BUDGET_S
        attempts = 0
        opened = None
        while True:
            attempts += 1
            try:
                mesh.announce_capabilities({})
            except Exception as exc:  # noqa: BLE001 - diagnostics only
                _diag(role, f"announce error: {exc!r}")
            try:
                opened = open_call()
                break
            except Exception as exc:  # noqa: BLE001 - classified below
                if not _is_local_discovery(net, exc):
                    _diag(role, f"attempt {attempts}: non-retryable {type(exc).__name__}: {exc}")
                    _emit(f"RESULT fail {type(exc).__name__}: {_one_line(exc)}")
                    return 1
                if time.monotonic() >= deadline:
                    _emit(
                        f"RESULT fail discovery did not converge in "
                        f"{DISCOVERY_BUDGET_S:.0f}s ({attempts} attempts): {_one_line(exc)}"
                    )
                    return 1
                if attempts == 1 or attempts % 10 == 0:
                    _diag(role, f"attempt {attempts}: discovery not yet converged: {exc}")
                time.sleep(DISCOVERY_RETRY_S)
        _diag(role, f"call opened after {attempts} attempt(s)")

        # 7. Check the response for the shape.
        if args.shape == "unary":
            try:
                got = _decode(opened)
            except Exception as exc:  # noqa: BLE001
                _emit(f"RESULT fail unary response is not JSON: {_one_line(exc)}")
                return 1
            if got != UNARY_RESPONSE:
                _emit(f"RESULT fail unary response {_one_line(json.dumps(got))} want {json.dumps(UNARY_RESPONSE)}")
                return 1
        else:
            stream = opened
            try:
                # Iteration ends at the clean end; a non-clean terminal raises.
                raw = _bounded(lambda: list(stream), "stream drain", STREAM_DRAIN_TIMEOUT_S)
            except _Timeout as exc:
                _emit(f"RESULT fail {_one_line(exc)}")
                return 1
            except Exception as exc:  # noqa: BLE001
                _emit(f"RESULT fail stream ended with {type(exc).__name__}: {_one_line(exc)}")
                return 1
            try:
                items = [_decode(c) for c in raw]
            except Exception as exc:  # noqa: BLE001
                _emit(f"RESULT fail stream item is not JSON: {_one_line(exc)}")
                return 1
            if items != STREAM_ITEMS:
                _emit(f"RESULT fail stream items {_one_line(json.dumps(items))} want {json.dumps(STREAM_ITEMS)}")
                return 1

        # 8. Verdict.
        _emit("RESULT ok")
        code = 0
        return 0
    except Exception as exc:  # noqa: BLE001 - every failure is a RESULT line
        _emit(f"RESULT fail {type(exc).__name__}: {_one_line(exc)}")
        return 1
    finally:
        # Teardown order: stream -> org client -> mesh (an unclosed OrgClient
        # makes shutdown fail).
        if stream is not None:
            try:
                stream.close()
            except Exception as exc:  # noqa: BLE001
                _diag(role, f"stream.close: {exc!r}")
        if client is not None:
            try:
                client.close()
            except Exception as exc:  # noqa: BLE001
                _diag(role, f"client.close: {exc!r}")
        try:
            mesh.shutdown()
        except Exception as exc:  # noqa: BLE001
            _diag(role, f"shutdown: {exc!r}")
        _diag(role, f"exit {code}")


# ---------------------------------------------------------------------------
# Provider
# ---------------------------------------------------------------------------


def run_provider(args) -> int:
    role = "provider"
    import net

    manifest = _load_manifest(args.manifest)
    me = manifest["provider"]
    want_acting = manifest["caller"]["org_id_hex"]
    want_provider = me["org_id_hex"]
    service = manifest["granted_service"]
    path = lambda rel: os.path.join(args.manifest, rel)  # noqa: E731

    state: dict = {"calls": 0, "facts": None, "errors": []}
    lock = threading.Lock()
    ran = threading.Event()

    def _record(caller: dict, request: bytes) -> None:
        with lock:
            state["calls"] += 1
            if state["facts"] is None:
                state["facts"] = caller
            try:
                body = _decode(request)
                if not isinstance(body, dict) or body.get("n") != 7:
                    state["errors"].append(f"request {_one_line(json.dumps(body))} want n==7")
            except Exception as exc:  # noqa: BLE001
                state["errors"].append(f"request is not JSON: {_one_line(exc)}")

    def unary_handler(caller: dict, request: bytes) -> bytes:
        try:
            _record(caller, request)
            return _encode(UNARY_RESPONSE)
        finally:
            ran.set()

    def streaming_handler(caller: dict, request: bytes, sink) -> None:
        try:
            _record(caller, request)
            for item in STREAM_ITEMS:
                sink.send(_encode(item))
        finally:
            ran.set()

    # 1. Build.
    mesh = _build_mesh(net, me["seed_hex"], manifest["psk_hex"])
    handle = None
    code = 1
    try:
        # 2. Org authority.
        net.install_org_authority(mesh, path(me["authority_dir"]))

        # 3. accept(<caller>) in the background.
        accept_errors: list = []

        def _accept() -> None:
            try:
                mesh.accept(args.caller_node_id)
            except Exception as exc:  # noqa: BLE001
                accept_errors.append(exc)

        accept_thread = threading.Thread(target=_accept, daemon=True)
        accept_thread.start()

        # stdin watcher (DRAINED arrives much later; start it now so a closed
        # pipe is observed as EOF rather than a hang).
        stdin_lines: queue.Queue = queue.Queue()

        def _stdin_watch() -> None:
            for raw in sys.stdin:
                stdin_lines.put(raw.strip())
            stdin_lines.put(None)  # EOF

        threading.Thread(target=_stdin_watch, daemon=True).start()

        # 4. READY.
        _emit(f"READY {mesh.local_addr} {_hex(mesh.public_key).lower()} {mesh.node_id}")

        # 5. Wait for the accept, then start.
        accept_thread.join(timeout=ACCEPT_TIMEOUT_S)
        if accept_errors:
            _emit(f"RESULT fail accept: {type(accept_errors[0]).__name__}: {_one_line(accept_errors[0])}")
            return 1
        if accept_thread.is_alive():
            _emit(f"RESULT fail accept did not complete within {ACCEPT_TIMEOUT_S:.0f}s")
            return 1
        mesh.start()
        _diag(role, "handshake done, started")

        # 6. Serve as Granted, THEN install the grant audience. The handle
        #    stays bound until DRAINED (step 9).
        if args.shape == "unary":
            handle = net.serve_org(mesh, service, "granted", unary_handler, None)
        else:
            handle = net.serve_org_streaming(mesh, service, "granted", streaming_handler, None)
        net.install_provider_grant_audience(
            mesh, _read_bytes(path(me["grant_path"])), path(me["grant_secret_path"])
        )
        _diag(role, f"serving {service!r} ({args.shape}), announcing")

        # 7. Re-announce about once a second until the handler has run.
        def _announce() -> None:
            while not ran.is_set():
                try:
                    mesh.announce_capabilities({})
                except Exception as exc:  # noqa: BLE001
                    _diag(role, f"announce error: {exc!r}")
                ran.wait(ANNOUNCE_EVERY_S)

        threading.Thread(target=_announce, daemon=True).start()

        if not ran.wait(timeout=HANDLER_WATCHDOG_S):
            _emit("RESULT fail callback-loss")
            code = 2
            return 2

        # 8. Facts.
        with lock:
            facts = state["facts"]
        fact_problems = []
        try:
            acting = bytes(facts["acting_org"]).hex()
            provider_org = bytes(facts["provider_org"]).hex()
            same = bool(facts["is_same_org"])
            if acting != want_acting:
                fact_problems.append(f"acting_org={acting} want {want_acting}")
            if provider_org != want_provider:
                fact_problems.append(f"provider_org={provider_org} want {want_provider}")
            if same:
                fact_problems.append("is_same_org=true want false")
        except Exception as exc:  # noqa: BLE001
            fact_problems.append(f"unreadable caller facts: {type(exc).__name__}: {_one_line(exc)}")
        if fact_problems:
            _emit(f"FACTS fail {_one_line('; '.join(fact_problems))}")
        else:
            _emit("FACTS ok")

        # 9. Wait for DRAINED before dropping the serve handle.
        try:
            line = stdin_lines.get(timeout=DRAINED_TIMEOUT_S)
        except queue.Empty:
            _emit(f"RESULT fail no DRAINED within {DRAINED_TIMEOUT_S:.0f}s")
            return 1
        if line != "DRAINED":
            _emit(f"RESULT fail expected DRAINED, got {_one_line(repr(line))}")
            return 1

        # Teardown now (step 9), verdict after (step 10).
        handle = _close_handle(role, handle)
        _shutdown(role, mesh)
        mesh = None

        # 10. Verdict.
        with lock:
            calls = state["calls"]
            errors = list(state["errors"])
        problems = list(fact_problems)
        if calls != 1:
            problems.append(f"calls={calls} want 1")
        problems.extend(errors)
        if problems:
            _emit(f"RESULT fail {_one_line('; '.join(problems))}")
            return 1
        _emit("RESULT ok calls=1")
        code = 0
        return 0
    except Exception as exc:  # noqa: BLE001 - every failure is a RESULT line
        _emit(f"RESULT fail {type(exc).__name__}: {_one_line(exc)}")
        return 1
    finally:
        _close_handle(role, handle)
        if mesh is not None:
            _shutdown(role, mesh)
        _diag(role, f"exit {code}")


def _close_handle(role: str, handle):
    if handle is not None:
        try:
            handle.close()
        except Exception as exc:  # noqa: BLE001
            _diag(role, f"handle.close: {exc!r}")
    return None


def _shutdown(role: str, mesh) -> None:
    try:
        mesh.shutdown()
    except Exception as exc:  # noqa: BLE001
        _diag(role, f"shutdown: {exc!r}")


# ---------------------------------------------------------------------------
# Entry
# ---------------------------------------------------------------------------


def _parse(argv):
    ap = argparse.ArgumentParser(description="cross-language org matrix driver (Python)")
    sub = ap.add_subparsers(dest="role", required=True)

    p = sub.add_parser("provider")
    p.add_argument("--manifest", required=True)
    p.add_argument("--shape", required=True, choices=SHAPES)
    p.add_argument("--caller-node-id", required=True, type=int)

    c = sub.add_parser("caller")
    c.add_argument("--manifest", required=True)
    c.add_argument("--shape", required=True, choices=SHAPES)

    # parse_args (not parse_known_args): extra flags are an error.
    return ap.parse_args(argv)


def main(argv=None) -> int:
    args = _parse(argv)
    if args.role == "provider":
        return run_provider(args)
    return run_caller(args)


if __name__ == "__main__":
    sys.exit(main())
