"""Tests for the plugin's agent-to-agent surface (`HERMES_INTEGRATION_PLAN_V2.md`
Phase 3, Slice D): the ``net_a2a_*`` requester tools + the executor-side
:class:`a2a.A2aService`.

The protocol / registry / cancellation are the Rust SDK's (proven in the
binding's ``test_a2a.py``); here we prove the plugin's tool arg-handling +
JSON-shaping, the DI'd service lifecycle, and — live — that the service runs a
submitted task end-to-end.
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

PSK = "9f" * 32


def _run(coro):
    return asyncio.run(coro)


class FakeMesh:
    """Records the SDK A2A calls the tool handlers make."""

    def __init__(self, status_json=None, cancelled=True):
        self.calls = []
        self._status = status_json
        self._cancelled = cancelled

    def submit_task(self, node_id, prompt, refs):
        self.calls.append(("submit", node_id, prompt, refs))
        return "task-abc"

    def task_status(self, node_id, task_id):
        self.calls.append(("status", node_id, task_id))
        return self._status

    def cancel_task(self, node_id, task_id):
        self.calls.append(("cancel", node_id, task_id))
        return self._cancelled


# ---------------------------------------------------------------------------
# Requester-side tools.
# ---------------------------------------------------------------------------


def test_submit_tool_forwards_and_shapes(plugin, monkeypatch):
    fake = FakeMesh()
    monkeypatch.setattr(plugin.node, "mesh", lambda: fake)
    res = json.loads(
        _run(
            plugin.tools.handle_net_a2a_submit(
                {"target_node_id": 5, "prompt": "do it", "context_refs": ["blob://x"]}
            )
        )
    )
    assert res["status"] == "ok"
    assert res["task_id"] == "task-abc"
    assert fake.calls == [("submit", 5, "do it", ["blob://x"])]


def test_submit_tool_validates(plugin):
    missing_target = json.loads(_run(plugin.tools.handle_net_a2a_submit({"prompt": "x"})))
    assert missing_target["status"] == "error"
    missing_prompt = json.loads(
        _run(plugin.tools.handle_net_a2a_submit({"target_node_id": 5}))
    )
    assert missing_prompt["status"] == "error"
    bad_target = json.loads(
        _run(plugin.tools.handle_net_a2a_submit({"target_node_id": "nope", "prompt": "x"}))
    )
    assert bad_target["status"] == "error"


def test_status_tool_ok_and_unknown(plugin, monkeypatch):
    record = json.dumps(
        {"brief": {"prompt": "p"}, "state": {"state": "running"}, "updated_at": 1}
    )
    monkeypatch.setattr(plugin.node, "mesh", lambda: FakeMesh(status_json=record))
    res = json.loads(
        _run(plugin.tools.handle_net_a2a_status({"target_node_id": 5, "task_id": "t"}))
    )
    assert res["status"] == "ok"
    assert res["record"]["state"]["state"] == "running"

    monkeypatch.setattr(plugin.node, "mesh", lambda: FakeMesh(status_json=None))
    unknown = json.loads(
        _run(plugin.tools.handle_net_a2a_status({"target_node_id": 5, "task_id": "t"}))
    )
    assert unknown["status"] == "unknown"


def test_cancel_tool(plugin, monkeypatch):
    monkeypatch.setattr(plugin.node, "mesh", lambda: FakeMesh(cancelled=True))
    res = json.loads(
        _run(plugin.tools.handle_net_a2a_cancel({"target_node_id": 5, "task_id": "t"}))
    )
    assert res["status"] == "ok"
    assert res["cancelled"] is True


def test_a2a_tools_registered(plugin):
    names = {t[0] for t in plugin.tools.TOOLS}
    assert {"net_a2a_submit", "net_a2a_status", "net_a2a_cancel"} <= names


# ---------------------------------------------------------------------------
# Executor-side service (DI'd).
# ---------------------------------------------------------------------------


def test_service_start_stop_is_idempotent(plugin):
    class FakeHandle:
        def __init__(self):
            self.serving = True

        def stop(self):
            self.serving = False

    class ServeMesh:
        def __init__(self):
            self.served = None

        def serve_a2a(self, callback):
            self.served = callback
            return FakeHandle()

    async def execute(task_id, prompt, refs, tags):
        return "ref"

    mesh = ServeMesh()
    svc = plugin.a2a.A2aService(mesh, execute)
    assert svc.serving is False
    svc.start()
    assert svc.serving is True
    assert mesh.served is execute
    svc.start()  # idempotent — one serve call
    svc.stop()
    assert svc.serving is False
    svc.stop()  # idempotent


# ---------------------------------------------------------------------------
# Live 2-node: the service runs a submitted task end-to-end.
# ---------------------------------------------------------------------------


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
        except Exception as e:  # noqa: BLE001
            last = e
            time.sleep(0.1)
    raise last


def _wait_state(requester, exec_id, task_id, want, timeout=6.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        raw = requester.task_status(exec_id, task_id)
        if raw is not None:
            rec = json.loads(raw)
            if rec["state"]["state"] == want:
                return rec
        time.sleep(0.05)
    raise AssertionError(f"task never reached {want!r}")


def test_service_runs_a_submitted_task_end_to_end(plugin):
    executor = net.NetMesh("127.0.0.1:0", PSK, permissive_channels=True)
    requester = net.NetMesh("127.0.0.1:0", PSK, permissive_channels=True)

    async def execute(task_id, prompt, context_refs, tags):
        # Promote the result home as an artifact ref.
        return f"blob://done-{prompt}"

    svc = None
    try:
        _handshake(requester, executor)
        executor.start()
        requester.start()
        svc = plugin.a2a.A2aService(executor, execute).start()

        task_id = _submit_retry(requester, executor.node_id, "job", [])
        rec = _wait_state(requester, executor.node_id, task_id, "completed")
        assert rec["state"]["result_ref"] == "blob://done-job"
    finally:
        if svc is not None:
            svc.stop()
        for m in (requester, executor):
            try:
                m.shutdown()
            except Exception:  # noqa: BLE001
                pass
