"""C5: the real Python boundary over a production-store-seeded history.

This is the reviewer's own C5 witness, landed and enforced. Her text is kept
assertion-for-assertion; exactly two mechanics changed, both of which were
absolute paths on her review machine and neither of which is an assertion:

* ``sys.path.insert`` of her checkout's tests directory, replaced by the
  ordinary sibling import (pytest already has this directory on the path);
* ``seed-exe.txt``, a file holding the pre-built seeder's path, replaced by
  building the seeder here and reading the executable path out of cargo's own
  JSON output.

**What this proves, and what it does not.** The seeder archives *actual*
mock-rail payment evidence through the production ``retain_superseded`` API,
and removes the live entry through the production locked-store utility. That
removal is explicit fixture supersession: this does not claim Python
manufactured the supersession race, and it is not a real-rail qualification.
What it does prove is the thing C5 is about — that the binding's
generation-scoped resolution reaches the *historical* incarnation and leaves
the live replacement exactly unchanged, on a store where both rows exist and
where the live row is genuinely eligible for the same operation.

**Why the populated shape is required.** The previously landed test
(``test_a_generation_scoped_resolution_never_reaches_the_live_attempt``) is
false-green: it has no historical row at all, and ``Closed`` is illegal
against its live ``Paid`` row, so routing the call to the *wrong* resolver
still produces the expected error and leaves state unchanged. The reviewer
demonstrated this by changing the binding's generation route from
``resolve_superseded_attempt`` to ``resolve_attempt``: her witness failed, the
landed one passed. Here the live replacement is driven to
``PaidUnexecutable`` first, so it is eligible for the very same ``Closed``
operation — wrong routing really would close it, and this test really would
catch that.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

from test_a2a_paid import _topology

_REPO_NET = Path(__file__).resolve().parents[3]  # net/crates/net


def _seeder_executable() -> str:
    """Locate the fixture seeder, building it only if necessary.

    ``NET_A2A_SEEDER_EXE`` is the path CI exports from a dedicated build step:
    the Python job runs pytest under a blanket ``--timeout=30``, and compiling
    a Rust test target inside the test would blow that budget. Locally the
    variable is usually unset, so the build happens here.

    Hard-fails rather than skipping. A skip would make C5's witness vacuous on
    exactly the machines that run it, which is the failure mode this test
    exists to rule out.
    """
    prebuilt = os.environ.get("NET_A2A_SEEDER_EXE")
    if prebuilt:
        if not Path(prebuilt).exists():
            raise AssertionError(f"NET_A2A_SEEDER_EXE points at nothing: {prebuilt}")
        return prebuilt
    build = subprocess.run(
        [
            "cargo", "test", "-p", "net-payments", "--features", "mesh",
            "--test", "review_python_seed", "--no-run",
            "--message-format", "json",
        ],
        cwd=_REPO_NET,
        capture_output=True,
        text=True,
    )
    if build.returncode != 0:
        raise AssertionError(
            "could not build the C5 fixture seeder:\n" + build.stdout + build.stderr
        )
    for line in build.stdout.splitlines():
        try:
            msg = json.loads(line)
        except ValueError:
            continue
        if msg.get("reason") == "compiler-artifact" and msg.get("executable"):
            if "review_python_seed" in msg["target"]["name"]:
                return msg["executable"]
    raise AssertionError("cargo reported no executable for review_python_seed")


@pytest.mark.timeout(900)
def test_review_python_resolves_history_without_touching_live(tmp_path):
    calls = []

    async def preflight(owner_json, offer_json, brief_json):
        calls.append(json.loads(brief_json)["task_id"])
        return None if len(calls) <= 2 else "revoked before execution"

    provider, (caller,) = _topology(tmp_path, preflight=preflight)
    try:
        first = caller.prepare("summarize history", task_id="history-key")
        assert first["status"] == "ok", first
        assert caller.purchase(first["prepared"])["status"] == "paid"
        original = caller.attempts()[0]

        env = os.environ.copy()
        env["NET_REVIEW_PURCHASE_PATH"] = caller.purchase_path
        exe = _seeder_executable()
        seeded = subprocess.run(
            [exe, "--exact", "review_seed_python_history", "--nocapture"],
            env=env,
            capture_output=True,
            text=True,
        )
        assert seeded.returncode == 0, seeded.stdout + seeded.stderr

        second = caller.prepare("summarize history", task_id="history-key")
        assert second["status"] == "ok", second
        assert caller.purchase(second["prepared"])["status"] == "paid"
        assert caller.submit(second["prepared"])["status"] == "unexecutable"

        rows = caller.attempts()
        assert len(rows) == 2, rows
        historical = [r for r in rows if r["retained"]]
        live = [r for r in rows if not r["retained"]]
        assert len(historical) == len(live) == 1, rows
        h, l = historical[0], live[0]
        assert h["key"] == l["key"] == original["key"]
        assert h["generation"] == original["generation"] and h["generation"] != l["generation"]
        assert h["state"]["state"] == l["state"]["state"] == "paid_unexecutable"

        outcome = json.dumps(
            {"resolution": "closed", "outcome": "history-only", "evidence": {"review": True}}
        )
        caller.gateway.a2a_resolve_attempt(
            "history-key",
            outcome,
            provider_node=caller.provider_node,
            generation=json.dumps(h["generation"]),
        )

        after = caller.attempts()
        assert [r for r in after if not r["retained"]] == live, (
            "historical resolution touched live replacement"
        )
        selected = [r for r in after if r["retained"]]
        assert len(selected) == 1 and selected[0]["state"]["state"] == "resolved", selected
        assert selected[0]["state"]["outcome"] == "history-only"
    finally:
        caller.close()
        provider.close()
