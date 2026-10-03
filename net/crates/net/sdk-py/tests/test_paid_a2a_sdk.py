"""Paid A2A through ``net_sdk`` (``NODE_A2A_PAID_ADMISSION_PLAN.md`` WS-G).

The vehicle is the executed consumer ``examples/paid_a2a_consumer.py``, run
in its **own interpreter**. ``conftest.py`` installs an auto-stub ``net``
module for the native-free SDK suite, so an in-process test here would
exercise the stub, not the wheel (review finding S3). The org facade suite
(``test_org_streaming.py``) is the precedent this follows.

Before any cell runs, a probe subprocess decides between three verdicts that
must not share one quiet outcome:

- no loadable native extension → **skip** (the native-free ``sdk-py-tests``
  job, whose skip behavior is unchanged);
- an extension whose ``net`` facade fails to import → **fail**;
- an extension that loads but lacks any paid-A2A name → **fail**, naming each
  missing one (a stale or partial build must not pass as "skipped").

The live leg runs in CI's ``python-tests`` job, after the wrapper is
installed, where a roster + passed-count floor makes a silent skip fail.
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
_SDK_PY = _HERE.parents[1]
_CRATE_ROOT = _SDK_PY.parent
_CONSUMER = _SDK_PY / "examples" / "paid_a2a_consumer.py"
_NET_PKG = _CRATE_ROOT / "bindings" / "python" / "python"

_PROBE = r"""
import importlib.util
import os
import sys
try:
    import net._net
except BaseException as e:
    absent = isinstance(e, ImportError) and getattr(e, "name", None) in ("net", "net._net")
    spec = None if absent else importlib.util.find_spec("net")
    locations = (spec.submodule_search_locations or []) if spec else []
    ships_native = any(f.startswith("_net.") for loc in locations for f in os.listdir(loc))
    if ships_native:
        print("NET_FACADE_IMPORT_FAIL:", type(e).__name__, e)
        sys.exit(4)
    print("NET_IMPORT_FAIL:", type(e).__name__, e)
    sys.exit(1)
import net
native_file = getattr(net._net, "__file__", None)
if not native_file or not os.path.isfile(native_file):
    print("NOT_A_REAL_EXTENSION:", native_file)
    sys.exit(2)
# Checked one by one, against the native module AND the facade.
missing = []
for name in ("PaymentProvider", "CapabilityGateway", "NetMesh", "PaymentRefused", "JournalOwnedElsewhere"):
    if not hasattr(net._net, name):
        missing.append("native:" + name)
    if not hasattr(net, name):
        missing.append("facade:" + name)
for owner, method in (
    ("CapabilityGateway", "prepare_task"),
    ("CapabilityGateway", "set_a2a_org_caller"),
    ("PaymentProvider", "serve_a2a_configured"),
    ("NetMesh", "set_a2a_org_caller"),
    ("NetMesh", "submit_task_paid"),
):
    cls = getattr(net._net, owner, None)
    if cls is None or not hasattr(cls, method):
        missing.append(owner + "." + method)
if missing:
    print("PARTIAL_PAID_SURFACE:", missing)
    sys.exit(2)
print("OK", native_file)
"""


def _child_env() -> dict:
    env = dict(os.environ)
    env["PYTHONPATH"] = str(_NET_PKG)
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
            f"the `net` wheel loads but its paid-A2A surface is partial or stubbed: {detail}"
        )
    if result.returncode == 4:
        raise AssertionError(
            f"the `net` wheel's native extension imports but its facade does not: {detail}"
        )
    pytest.skip(f"no usable `net` wheel here: {detail}", allow_module_level=True)


_probe_wheel()


def _run(cell: str, *extra: str) -> dict:
    result = subprocess.run(
        [sys.executable, str(_CONSUMER), "--cell", cell, *extra],
        env=_child_env(),
        capture_output=True,
        text=True,
        timeout=300,
    )
    ok = [line for line in result.stdout.splitlines() if line.startswith("CELL_OK ")]
    assert result.returncode == 0 and ok, (
        f"consumer cell {cell!r} failed (exit {result.returncode})\n"
        f"--- stdout ---\n{result.stdout}\n--- stderr ---\n{result.stderr}"
    )
    return json.loads(ok[-1][len("CELL_OK "):])


def test_paid_a2a_runs_once_from_sdk_mesh_nodes():
    receipt = _run("sync")
    assert receipt == {"billing_events": 1, "ran": ["sdk-sync-1"]}


def test_paid_a2a_runs_from_async_mesh_nodes_via_to_thread():
    assert _run("async") == {"ran": ["sdk-async-1"]}


def test_the_factories_accept_a_native_net_mesh():
    assert _run("native") == {"registry_version": "net-default-1"}


def test_the_boundary_refusals_are_the_native_ones():
    r = _run("refusals")
    assert "NetMesh" in r["native_refuses_meshnode"], r
    assert "no_such_option" in r["unknown_kwarg"], r
    assert r["not_a_mesh"].startswith("expected a net_sdk.MeshNode or a net.NetMesh"), r
    assert "a2a_purchase_path" in r["async_refuses_paid"], r
    assert r["async_not_an_org_target"].startswith(
        "set_a2a_org_caller: AsyncCapabilityGateway has no paid-A2A lifecycle"
    ), r
    assert r["purchase_needs_policy"].startswith("a2a_purchase_path requires payment_policy_path"), r


@pytest.fixture(scope="module")
def same_org_scenario():
    """Mint the same-org artifacts (`gen_subnet_scenario`'s org half) and
    stage the shared owner audience — the Node twin's setup. Fresh per run:
    the credentials expire. Plain ``makedirs``, not ``mkdtemp``: on Windows
    an mkdtemp directory's owner-only ACL is refused by the audience-secret
    loader."""
    if shutil.which("cargo") is None:
        pytest.skip("cargo not on PATH (scenario generation needs a Rust toolchain)")
    outdir = os.path.join(tempfile.gettempdir(), f"paid-a2a-org-{uuid.uuid4().hex}")
    os.makedirs(outdir)
    try:
        _mint_same_org(outdir)
    except BaseException:
        # A failed mint (broken build, timeout, missing manifest key) must not
        # leave a scenario directory behind on every failed run.
        shutil.rmtree(outdir, ignore_errors=True)
        raise
    yield outdir
    shutil.rmtree(outdir, ignore_errors=True)


def _mint_same_org(outdir: str) -> None:
    subprocess.run(
        [
            "cargo", "run", "-q", "-p", "net-mesh-sdk",
            "--features", "net,cortex,fixtures",
            "--example", "gen_subnet_scenario", "--", outdir,
        ],
        cwd=str(_CRATE_ROOT),
        check=True,
        env=dict(_child_env(), CARGO_INCREMENTAL="0"),
        timeout=1200,
    )
    with open(os.path.join(outdir, "manifest.json"), encoding="utf-8") as f:
        m = json.load(f)
    shutil.copyfile(
        os.path.join(outdir, m["provider"]["authority_dir"], "owner-audience.key"),
        os.path.join(outdir, m["caller"]["authority_dir"], "owner-audience.key"),
    )


def test_a_same_org_identity_reaches_the_paid_lifecycle_on_both_slots(same_org_scenario):
    """Review R6 from the SDK: an SDK OrgClient on the MeshNode's slot and
    the gateway's, a paid task through a PROTECTED catalog, the preflight
    seeing the admitted entity, and clearing both slots denying before
    launch (the consumer asserts each step)."""
    assert _run("same_org", "--scenario-dir", same_org_scenario) == {
        "owner_kind": "entity",
        "ran": ["org-task-1"],
    }
