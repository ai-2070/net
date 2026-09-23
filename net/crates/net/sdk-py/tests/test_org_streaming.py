"""The Stage-4 Q6 pure-SDK witnesses: every org shape CALLED and SERVED
through ``net_sdk`` alone (``ORG_SCOPED_STREAMING_PLAN`` §4.4, row "Pure
SDKs (Q6)").

The vehicle is the executed ``net_sdk``-only consumer run
(``examples/org_streaming_consumer.py`` — its imports are the standard
library and ``net_sdk`` ONLY, no ``net`` import and no private binding
access). Each row drives ONE live two-mesh round trip over real transport
through that consumer and asserts the round trip, the serve-side
verified-caller attribution (asserted BOTH inside the consumer and again
here against the scenario manifest), and the row's own contract on the
consumer's observable output. Nothing here is an import check.

Roster (15 items / 9 names — the ``[same_org|granted]`` parametrize pair
yields the 12 shape × form × authorization matrix cells):

* ``test_org_streaming_sync_call_and_serve[same_org|granted]`` /
  ``test_org_streaming_async_call_and_serve[...]`` — server-streaming over
  ``call_streaming`` + ``serve_org_streaming``.
* ``test_org_client_stream_sync_call_and_serve[...]`` /
  ``_async_...`` — client-streaming over ``call_client_stream`` +
  ``serve_org_client_stream``.
* ``test_org_duplex_sync_call_and_serve[...]`` / ``_async_...`` — duplex over
  ``call_duplex`` + ``serve_org_duplex``.
* ``test_task_cancel_propagates_to_retirement_observables`` — the
  ``task.cancel()`` propagation links (see the consumer's ``cancel`` cell
  docstring for the exact F-S3.1-2 handler-drop level it claims).
* ``test_streaming_midstream_error_surfaces_the_org_vocabulary`` — midstream
  errors on org handles surface through the ``org_err_to_py`` mirror at the
  wrapper level (``net_sdk.org``'s ``OrgError`` family), never the
  ``RpcError`` family.
* ``test_unary_call_and_serve_preserved_through_net_sdk`` — the preserved
  unary, raw (``call``/``serve_org``) AND over the typed wrappers
  (``TypedOrgClient``/``serve_org_typed``).

Scenarios: ``granted`` consumes the ``gen_org_scenario`` manifest (org A's
caller invoking org B's granted capability); ``same_org`` consumes the
``gen_org_same_scenario`` manifest (one organization, provider and caller
sharing the ONE owner audience). Both are generated ONCE per module run —
each generation is a cold cargo build on first use.

Env: needs a Rust toolchain (to generate the scenarios) and the ``net``
wheel built with the ``org`` feature + the S4 surface; skips cleanly
otherwise — except a wheel that HAS ``org`` but LACKS the S4 verbs, which
fails loudly (a skip would make every witness vacuous on exactly the
machines that run it).

**Handler-drop contract (F-S3.1-2, Specification §2.2) — the level these
witnesses hold.** A protected call runs under a per-call retire supervisor;
on retirement it drops the handler future WITHOUT a final poll. Handlers are
never assumed to observe a cancellation EVENT — only the retirement
observables (request input fencing to EOF; library-controlled sinks
stopping) are contracts. See ``net_sdk.org.HANDLER_DROP_CONTRACT`` and
``test_task_cancel_propagates_to_retirement_observables``'s consumer cell
for what cancellation CAN and CANNOT be observed, per shape.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
import uuid
from pathlib import Path

import pytest

_HERE = Path(__file__).resolve()
_SDK_PY = _HERE.parents[1]  # tests/ -> sdk-py
_CRATE_ROOT = _SDK_PY.parent  # sdk-py -> net/crates/net
_CONSUMER = _SDK_PY / "examples" / "org_streaming_consumer.py"
_NET_PKG = _CRATE_ROOT / "bindings" / "python" / "python"
_RECEIPTS = _SDK_PY / ".s4receipts"

# The wheel probe distinguishes the three env states that must not share one
# quiet verdict: no usable wheel here (skip), a wheel without the org feature
# (skip), and a wheel that has org but lacks the S4 surface (FAIL LOUDLY —
# the witnesses' subject is exactly that surface).
_PROBE = r"""
import sys
try:
    import net._net  # the native extension itself
except BaseException as e:
    print("NET_IMPORT_FAIL:", type(e).__name__, e)
    sys.exit(1)
import net
if not hasattr(net, "install_org_authority"):
    print("NO_ORG_FEATURE")
    sys.exit(3)
missing = [n for n in (
    "serve_org_streaming", "serve_org_client_stream", "serve_org_duplex",
    "AsyncOrgClient", "OrgCredentials",
) if not hasattr(net, n)]
if missing:
    print("STALE_BUILD_MISSING:", missing)
    sys.exit(2)
print("OK")
"""


def _child_env() -> dict:
    env = dict(os.environ)
    # The `net` wheel package for the consumer subprocess. `net_sdk` itself
    # comes from the installed `net-mesh-sdk` artifact (see the module gate).
    env["PYTHONPATH"] = str(_NET_PKG)
    env["CARGO_INCREMENTAL"] = "0"
    env["PYTHONUNBUFFERED"] = "1"
    return env


def _probe_wheel() -> None:
    result = subprocess.run(
        [sys.executable, "-c", _PROBE],
        env=_child_env(),
        capture_output=True,
        text=True,
        timeout=120,
    )
    if result.returncode == 0:
        return
    detail = (result.stdout + result.stderr).strip()
    if result.returncode == 2:
        raise AssertionError(
            "the `net` wheel has the org feature but lacks the S4 org "
            f"streaming surface (stale build?): {detail}"
        )
    if result.returncode == 3:
        pytest.skip(f"net built without the org feature: {detail}", allow_module_level=True)
    pytest.skip(f"no usable `net` wheel here: {detail}", allow_module_level=True)


_probe_wheel()
if shutil.which("cargo") is None:
    pytest.skip(
        "cargo not on PATH (scenario generation needs a Rust toolchain)",
        allow_module_level=True,
    )


def _gen_scenario(kind: str, outdir: str) -> dict:
    """Mint the issuance chain the consumer loads (a CONSUMER loads
    credentials; minting is test infrastructure)."""
    if kind == "granted":
        cmd = [
            "cargo", "run", "-q", "-p", "net-mesh-sdk",
            "--features", "net,cortex,fixtures",
            "--example", "gen_org_scenario", "--", outdir,
        ]
    else:
        cmd = [
            "cargo", "run", "-q", "-p", "net-python", "--no-default-features",
            "--features", "org", "--example", "gen_org_same_scenario", "--", outdir,
        ]
    subprocess.run(
        cmd, cwd=str(_CRATE_ROOT), check=True, env=_child_env(), timeout=1200
    )
    with open(os.path.join(outdir, "manifest.json"), encoding="utf-8") as f:
        return json.load(f)


class _Scenario:
    def __init__(self, kind: str, outdir: str, manifest: dict) -> None:
        self.kind = kind
        self.outdir = outdir
        self.manifest = manifest

    def service(self, shape: str) -> str:
        if self.kind == "granted":
            return self.manifest["granted_service"]
        return f"internal.{shape}"


@pytest.fixture(scope="module")
def scenarios():
    """Generate BOTH issuance chains once per module run."""
    out = {}
    for kind in ("granted", "same_org"):
        # `os.makedirs` under the system temp dir — NOT `tempfile.mkdtemp`,
        # which on Windows stamps an owner-only ACE the audience-secret
        # loader (rightly) refuses on inherited secret files.
        outdir = os.path.join(tempfile.gettempdir(), f"s4pysdk-{kind}-{uuid.uuid4().hex}")
        os.makedirs(outdir)
        out[kind] = _Scenario(kind, outdir, _gen_scenario(kind, outdir))
    yield out
    for sc in out.values():
        shutil.rmtree(sc.outdir, ignore_errors=True)


def _run_consumer(cell: str, sc: _Scenario) -> dict:
    """Run one live two-mesh cell through the ``net_sdk``-only consumer and
    return its ``CELL_OK`` payload. The consumer's own named assertions fire
    first (its traceback is quoted verbatim into the failure below)."""
    result = subprocess.run(
        [
            sys.executable, str(_CONSUMER),
            "--scenario-dir", sc.outdir,
            "--kind", sc.kind,
            "--cell", cell,
        ],
        env=_child_env(),
        capture_output=True,
        text=True,
        timeout=540,
    )
    ok_lines = [
        line for line in result.stdout.splitlines() if line.startswith("CELL_OK ")
    ]
    assert result.returncode == 0 and ok_lines, (
        f"consumer cell {cell!r} ({sc.kind}) failed (exit {result.returncode})\n"
        f"--- stdout ---\n{result.stdout}\n--- stderr ---\n{result.stderr}"
    )
    return json.loads(ok_lines[-1][len("CELL_OK "):])


def _assert_attribution(facts: dict, sc: _Scenario) -> None:
    # The five verified fields, none caller-claimed — asserted here AGAINST
    # THE MANIFEST (the consumer asserts them against its loaded scenario).
    assert facts["is_same_org"] is (sc.kind == "same_org")
    assert len(bytes.fromhex(facts["entity"])) == 32
    assert facts["acting_org"] == sc.manifest["caller"]["org_id_hex"]
    assert facts["provider_org"] == sc.manifest["provider"]["org_id_hex"]
    if sc.kind == "same_org":
        assert facts["acting_org"] == facts["provider_org"]
    else:
        assert facts["acting_org"] != facts["provider_org"]
    assert len(bytes.fromhex(facts["capability"])) == 32
    assert len(bytes.fromhex(facts["provider"])) == 32


# =========================================================================
# Server-streaming — `call_streaming` + `serve_org_streaming`.
# =========================================================================


@pytest.mark.timeout(600)
@pytest.mark.parametrize("access", ["same_org", "granted"])
def test_org_streaming_sync_call_and_serve(scenarios, access) -> None:
    sc = scenarios[access]
    payload = _run_consumer("stream_sync", sc)
    assert payload["chunks"] == ["one:hi", "two:hi"]
    _assert_attribution(payload["facts"], sc)


@pytest.mark.timeout(600)
@pytest.mark.parametrize("access", ["same_org", "granted"])
def test_org_streaming_async_call_and_serve(scenarios, access) -> None:
    sc = scenarios[access]
    payload = _run_consumer("stream_async", sc)
    assert payload["chunks"] == ["one:hi", "two:hi"]
    _assert_attribution(payload["facts"], sc)


# =========================================================================
# Client-streaming — `call_client_stream` + `serve_org_client_stream`.
# =========================================================================


@pytest.mark.timeout(600)
@pytest.mark.parametrize("access", ["same_org", "granted"])
def test_org_client_stream_sync_call_and_serve(scenarios, access) -> None:
    sc = scenarios[access]
    payload = _run_consumer("rollup_sync", sc)
    assert payload["reply"] == {"chunks": 6}
    _assert_attribution(payload["facts"], sc)


@pytest.mark.timeout(600)
@pytest.mark.parametrize("access", ["same_org", "granted"])
def test_org_client_stream_async_call_and_serve(scenarios, access) -> None:
    sc = scenarios[access]
    payload = _run_consumer("rollup_async", sc)
    assert payload["reply"] == {"chunks": 6}
    _assert_attribution(payload["facts"], sc)


# =========================================================================
# Duplex — `call_duplex` + `serve_org_duplex`.
# =========================================================================


@pytest.mark.timeout(600)
@pytest.mark.parametrize("access", ["same_org", "granted"])
def test_org_duplex_sync_call_and_serve(scenarios, access) -> None:
    sc = scenarios[access]
    payload = _run_consumer("mirror_sync", sc)
    assert payload["echoed"] == ["echo:a", "echo:b", "echo:c"]
    _assert_attribution(payload["facts"], sc)


@pytest.mark.timeout(600)
@pytest.mark.parametrize("access", ["same_org", "granted"])
def test_org_duplex_async_call_and_serve(scenarios, access) -> None:
    sc = scenarios[access]
    payload = _run_consumer("mirror_async", sc)
    assert payload["echoed"] == ["echo:a", "echo:b", "echo:c"]
    _assert_attribution(payload["facts"], sc)


# =========================================================================
# The preserved unary — raw and typed, both through `net_sdk`.
# =========================================================================


@pytest.mark.timeout(600)
def test_unary_call_and_serve_preserved_through_net_sdk(scenarios) -> None:
    sc = scenarios["granted"]
    payload = _run_consumer("unary", sc)
    # The preserved raw unary (the X2 shape) and the typed wrappers' unary.
    assert payload["raw"] == {"n": 8, "servedBy": "pysdk-consumer"}
    assert payload["typed"] == {"n": 71, "servedBy": "pysdk-consumer-typed"}
    _assert_attribution(payload["facts"], sc)
    _assert_attribution(payload["typed_facts"], sc)


# =========================================================================
# Midstream-error vocabulary — the `org_err_to_py` mirror (§4.4).
# =========================================================================


@pytest.mark.timeout(600)
def test_streaming_midstream_error_surfaces_the_org_vocabulary(scenarios) -> None:
    sc = scenarios["same_org"]
    payload = _run_consumer("midstream", sc)
    # Deadline retirement is the stream's final error: `org:rpc:...` via the
    # net_sdk mirror (the consumer asserts the class identity), never an
    # admission denial and never the RpcError family.
    assert payload["domain"] == "rpc", payload
    assert payload["message"].startswith("org:rpc:"), payload
    _assert_attribution(payload["facts"], sc)


# =========================================================================
# The `task.cancel()` propagation links (the F-S3.1-2 named item).
# =========================================================================


@pytest.mark.timeout(600)
def test_task_cancel_propagates_to_retirement_observables(scenarios) -> None:
    """The three cancellation links — asserted at their exact contract
    levels inside the consumer's ``cancel`` cell (whose docstring IS the
    F-S3.1-2 level statement: caller-side ``CancelledError``; the LOCAL
    teardown observable with the handle ALIVE; the provider-side retirement
    observable riding the handle's ``close()``/drop). This row pins their
    observability through the ``net_sdk`` facade: all three must arrive."""
    sc = scenarios["same_org"]
    payload = _run_consumer("cancel", sc)
    # The consumer's CELLOK envelope carries `cell`/`kind` metadata beside the
    # links (established across every cell); the contract claim is the links
    # themselves. (Main takeover fix — F-S4PySdk-5: the links-only expectation
    # against the envelope; the three links matched exactly as designed.)
    assert {k: v for k, v in payload.items() if k.startswith("link")} == {
        "link1": "CancelledError",
        "link2": "drain_ended",
        "link3": "input_eof",
    }
