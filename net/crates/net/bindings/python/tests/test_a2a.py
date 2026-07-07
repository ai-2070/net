"""Live tests for agent-to-agent task handoff (`HERMES_INTEGRATION_PLAN_V2.md`
Phase 3): ``NetMesh.serve_a2a`` serves the task lifecycle backed by a Python
async task executor, and a second node submits / polls / cancels it over the
wire.

Mirrors the Rust ``mesh_a2a`` tests at the binding layer — the couch test: hand
off a long job, watch it run, cancel mid-run, and the remote executor
demonstrably stops (the Python coroutine is cancelled); plus a completed path
where the result comes back as an artifact ref.
"""

from __future__ import annotations

import asyncio
import json
import threading
import time

import pytest

pytest.importorskip("net")
pytest.importorskip("net._net")

import net  # noqa: E402

PSK = "8e" * 32


def _mesh_unstarted():
    return net.NetMesh("127.0.0.1:0", PSK, permissive_channels=True)


def _handshake(connector, acceptor):
    errs = []

    def _accept():
        try:
            acceptor.accept(connector.node_id)
        except Exception as e:  # noqa: BLE001
            errs.append(e)

    t = threading.Thread(target=_accept, daemon=True)
    t.start()
    time.sleep(0.05)
    connector.connect(acceptor.local_addr, acceptor.public_key, acceptor.node_id)
    t.join(timeout=5)
    if errs:
        raise errs[0]


def _submit_retry(requester, exec_id, prompt, refs, attempts=5):
    last = None
    for _ in range(attempts):
        try:
            return requester.submit_task(exec_id, prompt, refs)
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


def test_a2a_submit_run_and_cancel_over_the_wire():
    executor = _mesh_unstarted()
    requester = _mesh_unstarted()

    cancelled_seen = []

    async def run_task(task_id, prompt, context_refs, tags):
        try:
            await asyncio.sleep(30)  # a long job
            return "blob://done"
        except asyncio.CancelledError:
            cancelled_seen.append(task_id)
            raise

    handle = None
    try:
        _handshake(requester, executor)
        executor.start()
        requester.start()
        handle = executor.serve_a2a(run_task)
        exec_id = executor.node_id

        task_id = _submit_retry(
            requester, exec_id, "grind a long job", ["blob://ctx"]
        )
        assert isinstance(task_id, str) and task_id

        _wait_state(requester, exec_id, task_id, "running")

        assert requester.cancel_task(exec_id, task_id) is True
        _wait_state(requester, exec_id, task_id, "cancelled")

        # The remote Python coroutine was actually cancelled (best-effort — the
        # cancellation is scheduled on the dispatcher loop after the state flips).
        for _ in range(40):
            if cancelled_seen:
                break
            time.sleep(0.05)
        assert cancelled_seen == [task_id], "the remote executor coroutine was cancelled"
    finally:
        if handle is not None:
            handle.stop()
        for m in (requester, executor):
            try:
                m.shutdown()
            except Exception:  # noqa: BLE001
                pass


def test_a2a_immediate_cancel_leaves_no_zombie_coroutine():
    """Cancel racing the dispatch itself (the guard-armed-at-dispatch window):
    a cancel accepted right after submit must actually stop the handler — the
    coroutine either never runs or is cancelled, never runs to completion as a
    zombie behind a reported ``cancelled`` state."""
    executor = _mesh_unstarted()
    requester = _mesh_unstarted()

    completed = []

    async def run_task(task_id, prompt, context_refs, tags):
        await asyncio.sleep(1.0)
        completed.append(task_id)  # a zombie would reach this after the cancel
        return "blob://zombie"

    handle = None
    try:
        _handshake(requester, executor)
        executor.start()
        requester.start()
        handle = executor.serve_a2a(run_task)
        exec_id = executor.node_id

        task_id = _submit_retry(requester, exec_id, "job to kill instantly", [])
        # No wait for "running": cancel as close to the dispatch as the wire
        # allows, so the token can trip before the executor's select ever polls.
        assert requester.cancel_task(exec_id, task_id) is True
        _wait_state(requester, exec_id, task_id, "cancelled")

        # Past the handler's natural completion point: a zombie would have
        # appended by now.
        time.sleep(1.5)
        assert completed == [], "cancelled task's coroutine ran to completion"
    finally:
        if handle is not None:
            handle.stop()
        for m in (requester, executor):
            try:
                m.shutdown()
            except Exception:  # noqa: BLE001
                pass


def test_a2a_task_completes_with_an_artifact_ref():
    executor = _mesh_unstarted()
    requester = _mesh_unstarted()

    async def run_task(task_id, prompt, context_refs, tags):
        # The result is promoted home as an artifact ref, not inlined.
        return "blob://summary-99"

    handle = None
    try:
        _handshake(requester, executor)
        executor.start()
        requester.start()
        handle = executor.serve_a2a(run_task)
        exec_id = executor.node_id

        task_id = _submit_retry(requester, exec_id, "summarize", [])
        rec = _wait_state(requester, exec_id, task_id, "completed")
        assert rec["state"]["result_ref"] == "blob://summary-99"
        assert rec["brief"]["prompt"] == "summarize"

        # A finished task cancels to False; an unknown task is None.
        assert requester.cancel_task(exec_id, task_id) is False
        assert requester.task_status(exec_id, "nope") is None
    finally:
        if handle is not None:
            handle.stop()
        for m in (requester, executor):
            try:
                m.shutdown()
            except Exception:  # noqa: BLE001
                pass
