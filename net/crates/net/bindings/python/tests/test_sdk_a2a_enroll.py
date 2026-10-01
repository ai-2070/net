"""A2A and enrollment through ``net_sdk.MeshNode``, live
(`PYTHON_SDK_WRAPPER_PARITY_PLAN.md` S6). The verbs' behaviour is covered at
the binding layer (``test_a2a.py``, ``test_enrollment.py``); this proves the
SDK node reaches them, including the forward-only-what-was-set handling of
optional arguments whose defaults live in Rust.

Imports ``net_sdk`` from the in-repo source when the wrapper isn't
installed (CI's main run); it never skips for that.
"""

from __future__ import annotations

import importlib
import json
import sys
import threading
import time
from pathlib import Path

import pytest

pytest.importorskip("net._net")

SDK_SRC = Path(__file__).resolve().parents[3] / "sdk-py" / "src"
PSK = "8e" * 32


def _sdk(module: str):
    try:
        return importlib.import_module(module)
    except ImportError:
        sys.path.insert(0, str(SDK_SRC))
        return importlib.import_module(module)


@pytest.fixture
def pair():
    """``(requester, executor)``: connected, started SDK nodes."""
    net_sdk = _sdk("net_sdk")
    requester = net_sdk.MeshNode("127.0.0.1:0", PSK, permissive_channels=True)
    executor = net_sdk.MeshNode("127.0.0.1:0", PSK, permissive_channels=True)
    errors: list[Exception] = []

    def _accept() -> None:
        try:
            executor.accept(requester.node_id)
        except Exception as e:  # noqa: BLE001
            errors.append(e)

    t = threading.Thread(target=_accept, daemon=True)
    t.start()
    time.sleep(0.05)
    requester.connect(executor.local_addr, executor.public_key, executor.node_id)
    t.join(timeout=5)
    if errors:
        raise errors[0]
    requester.start()
    executor.start()
    try:
        yield requester, executor
    finally:
        requester.shutdown()
        executor.shutdown()


def _submit_retry(requester, exec_id, prompt, **kwargs):
    last = None
    for _ in range(5):
        try:
            return requester.submit_task(exec_id, prompt, **kwargs)
        except Exception as e:  # noqa: BLE001 — the first call can lose its reply
            last = e
            time.sleep(0.1)
    raise last


def _wait_state(requester, exec_id, task_id, want, timeout=6.0):
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        raw = requester.task_status(exec_id, task_id)
        if raw is not None:
            rec = json.loads(raw)
            last = rec["state"]["state"]
            if last == want:
                return rec
        time.sleep(0.05)
    raise AssertionError(f"task {task_id} never reached {want!r} (last={last!r})")


def test_a2a_task_completes_through_sdk_nodes(pair) -> None:
    requester, executor = pair
    seen: list[tuple] = []

    async def run_task(task_id, prompt, context_refs, tags):
        seen.append((prompt, list(context_refs), list(tags)))
        return "blob://sdk-result"

    handle = executor.serve_a2a(run_task)
    try:
        # Only `prompt` set: context_refs / tags fall to the wheel's defaults.
        task_id = _submit_retry(requester, executor.node_id, "summarize")
        rec = _wait_state(requester, executor.node_id, task_id, "completed")
        assert rec["state"]["result_ref"] == "blob://sdk-result"
        assert seen[0] == ("summarize", [], [])

        # A caller-chosen id is kept.
        chosen = _submit_retry(
            requester,
            executor.node_id,
            "again",
            context_refs=["blob://ctx"],
            tags=["t1"],
            task_id="sdk-task-42",
        )
        assert chosen == "sdk-task-42"
        _wait_state(requester, executor.node_id, chosen, "completed")
        assert ("again", ["blob://ctx"], ["t1"]) in seen

        assert requester.cancel_task(executor.node_id, chosen) is False
        assert requester.task_status(executor.node_id, "no-such-task") is None
    finally:
        handle.stop()


def test_rendezvous_string_names_this_node(pair) -> None:
    _, executor = pair
    locator = executor.rendezvous_string()
    assert isinstance(locator, str) and locator
