"""Teardown with a live org handler must never wedge the interpreter.

The defect this pins (fixed in ``src/runtime_guard.rs``): a mesh's tokio
``Runtime`` is freed by CPython during dealloc / cyclic GC on the Python
thread — **holding the GIL**. ``Runtime::drop`` blocks until its blocking
pool winds down, and a Python org handler runs on that pool inside
``Python::attach`` (``serve_org_streaming`` -> ``run_py_org_handler`` ->
``spawn_blocking``). The handler-drop contract says a handler is NOT
interrupted, so a handler that outlives retirement must re-acquire the GIL
to return: the dropping thread holds the GIL and waits for the handler, the
handler waits for the GIL, and neither side can move. Because the GIL is
never released, no other Python thread can break the deadlock — including
``pytest-timeout``'s ``thread``-method watchdog, which needs the GIL to dump
stacks and ``os._exit``. In CI that surfaced as a silent 20/60-minute hang at
a GC-dependent test boundary, not as a named timeout.

So the witness cannot be an in-process test: on the buggy build it would hang,
and pytest-timeout cannot fire. The property — "tearing down a mesh while a
handler is still live never wedges the interpreter" — is proven in a CHILD
process whose completion is bounded by ``subprocess.run(timeout=...)``. A
regression therefore fails HERE with a named ``TimeoutExpired`` that quotes
the child's output, instead of stalling the job to its ceiling.

Shape: the child mints a same-org chain (the sibling ``test_org_live`` cell's
generator), builds the pair, serves a server-streaming handler that HOLDS for
a few seconds, opens a call so the handler is genuinely live on the blocking
pool, retires and closes every handle, shuts the meshes down, then ``del``s
them and ``gc.collect()``s to force the runtime drop. The generator is warmed
by the parent first, so the child's ``cargo run`` is a freshness check; that
keeps the child's own 45 s bound well under CI's pytest timeout even on a
cold cache.

The property is qualitative: no wall-clock duration is asserted.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tempfile
import uuid

import pytest

net = pytest.importorskip("net", reason="net wheel not built")

if not hasattr(net, "serve_org_streaming"):
    pytest.skip("net built without the org feature", allow_module_level=True)

# The child must finish on its own; if it does not, it is deadlocked and only
# this bound (not pytest-timeout, which cannot run) will end it. 45 s is well
# under the job's per-test ceiling and comfortably above a warm mesh call.
_SUBPROCESS_TIMEOUT = 45

_CRATE_ROOT = os.path.abspath(
    os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "..")
)

# Written out by the child so the parent can prove each phase ran. The same
# literals are asserted below; they are not a timing measurement.
_MARK_HANDLER_LIVE = "MARK handler-live"
_MARK_DROPPED = "MARK dropped"
_MARK_END = "MARK end"

# The child program. Run with ``python -c <this> <outdir> <crate_root>`` and
# the ambient interpreter/env (so it imports whichever ``net`` the parent is
# testing). It prints a flush-per-line marker before each phase; the critical
# assertion is that it reaches ``MARK end`` and exits 0 at all.
_CHILD = r'''
import gc
import json
import os
import socket
import subprocess
import sys
import threading
import time

import net
from net.org import parse_org_error  # noqa: F401  (proves net.org is present)

OUT = sys.argv[1]
CRATE_ROOT = sys.argv[2]


def mark(m):
    print(m, flush=True)


mark("MARK start")

# Mint the same-org chain the sibling org_live cells use: one organization, a
# provider and a caller sharing the owner audience pre-staged into both adopted
# authority dirs. The parent warmed this example, so this is a freshness check,
# not a cold build.
subprocess.run(
    [
        "cargo", "run", "-q", "-p", "net-python", "--no-default-features",
        "--features", "org", "--example", "gen_org_same_scenario", "--", OUT,
    ],
    cwd=CRATE_ROOT,
    check=True,
)
mark("MARK minted")

with open(os.path.join(OUT, "manifest.json"), encoding="utf-8") as f:
    manifest = json.load(f)
psk = manifest["psk_hex"]
prov = manifest["provider"]
cal = manifest["caller"]


def path(rel):
    return os.path.join(OUT, rel)


def free_addr():
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        s.bind(("127.0.0.1", 0))
        return "127.0.0.1:%d" % s.getsockname()[1]
    finally:
        s.close()


def mesh(seed_hex, psk_hex):
    return net.NetMesh(
        bind_addr=free_addr(),
        psk=psk_hex,
        identity_seed=bytes.fromhex(seed_hex),
        heartbeat_interval_ms=200,
        permissive_channels=True,
    )


def handshake(conn, acc):
    errors = []

    def _accept():
        try:
            acc.accept(conn.node_id)
        except Exception as e:  # noqa: BLE001
            errors.append(e)

    t = threading.Thread(target=_accept, daemon=True)
    t.start()
    time.sleep(0.05)
    conn.connect(acc.local_addr, acc.public_key, acc.node_id)
    t.join(timeout=5)
    if errors:
        raise errors[0]


provider = mesh(prov["seed_hex"], psk)
caller = mesh(cal["seed_hex"], psk)
net.install_org_authority(provider, path(prov["authority_dir"]))
net.install_org_authority(caller, path(cal["authority_dir"]))
handshake(caller, provider)
provider.start()
caller.start()
mark("MARK pair-up")

svc = "internal.stream"
handler_live = threading.Event()


def handler(caller_facts, request, sink):
    # Live the moment the call reaches us; hold the blocking-pool thread well
    # past teardown so the runtime drop genuinely races a running handler.
    handler_live.set()
    sink.send(b"first")
    time.sleep(6)


handle = net.serve_org_streaming(provider, svc, "same_org", handler, None)
with open(path(cal["membership_path"]), "rb") as f:
    membership = f.read()
with open(path(cal["dispatcher_path"]), "rb") as f:
    dispatcher = f.read()
creds = net.OrgCredentials(membership, dispatcher, [], [])
client = net.OrgClient.bind(caller, creds)

stream = None
last = None
end = time.time() + 60
while time.time() < end:
    provider.announce_capabilities({})
    caller.announce_capabilities({})
    try:
        stream = client.call_streaming(svc, b"hi", 1500)
        break
    except Exception as e:  # noqa: BLE001
        last = e
        time.sleep(1)
if stream is None:
    raise SystemExit("the same-org call never converged: %r" % (last,))
assert next(stream) == b"first", "the handler never delivered its first chunk"
assert handler_live.wait(5), "the handler never became live"
mark("MARK handler-live")

try:
    next(stream)
except Exception:  # noqa: BLE001  (deadline retirement or EOF: either is fine)
    pass

stream.close()
client.close()
handle.close()
caller.shutdown()
provider.shutdown()

# The whole point: the handler still holds its blocking-pool thread, so
# freeing the meshes (and their tokio runtimes) must not wedge the process.
del stream, client, handle, caller, provider, creds
gc.collect()
mark("MARK dropped")
mark("MARK end")
'''


def _warm_generator() -> None:
    """Build the same-org generator the child runs, so the child's own bound
    covers only the deadlock window — not a cold cargo build. In CI the org
    suite has already built it; standalone this is the one slow step and the
    per-test ``@pytest.mark.timeout`` covers it."""
    subprocess.run(
        [
            "cargo",
            "build",
            "-q",
            "-p",
            "net-python",
            "--no-default-features",
            "--features",
            "org",
            "--example",
            "gen_org_same_scenario",
        ],
        cwd=_CRATE_ROOT,
        check=True,
    )


@pytest.mark.timeout(600)
def test_teardown_with_a_live_handler_does_not_deadlock() -> None:
    """Freeing the meshes while an org handler is still running must complete.

    Bounded by construction: the work runs in a child process, so a regression
    that re-introduces the GIL-held blocking runtime drop becomes a named
    ``TimeoutExpired`` failure quoting the child's output — not a wedged
    interpreter surviving to the job ceiling.
    """
    _warm_generator()

    # ``os.makedirs`` under the system temp dir — not ``tempfile.mkdtemp``,
    # whose owner-only ACE the audience-secret loader legitimately refuses
    # (same reason the sibling org_live fixture mints there).
    outdir = os.path.join(tempfile.gettempdir(), "s4py-teardown-" + uuid.uuid4().hex)
    os.makedirs(outdir)
    try:
        try:
            proc = subprocess.run(
                [sys.executable, "-c", _CHILD, outdir, _CRATE_ROOT],
                capture_output=True,
                text=True,
                timeout=_SUBPROCESS_TIMEOUT,
                env=os.environ.copy(),
            )
        except subprocess.TimeoutExpired as exc:
            captured = (exc.stdout or "") + (exc.stderr or "")
            pytest.fail(
                "teardown with a live org handler wedged the interpreter: the child "
                f"did not finish within {_SUBPROCESS_TIMEOUT}s. The runtime drop is "
                "blocking while the GIL is held, so pytest-timeout cannot break it. "
                "Child output:\n" + captured
            )

        out = (proc.stdout or "") + (proc.stderr or "")
        assert proc.returncode == 0, f"child exited {proc.returncode}:\n{out}"
        assert _MARK_HANDLER_LIVE in out, f"handler never went live:\n{out}"
        assert _MARK_DROPPED in out, f"the drop phase never completed:\n{out}"
        assert _MARK_END in out, f"the child never reached the end:\n{out}"
    finally:
        shutil.rmtree(outdir, ignore_errors=True)
