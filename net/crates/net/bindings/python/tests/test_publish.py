"""Live tests for local tool publication (`HERMES_INTEGRATION_PLAN_V2.md`
Phase 2, Slice B): ``NetMesh.publish_tools`` announces a node's OWN tools —
backed by a Python **async** callback — and a second node invokes them over the
wire through the ordinary direct-addressed nRPC path.

Mirrors the Rust ``publish_tools_end_to_end.rs`` at the binding layer: the whole
publish/announce/serve machinery lives in ``net_mcp::wrap`` (H2); here we prove
the PyO3 bridge — that the Python callback actually runs when a remote invoke
lands, and its result round-trips.
"""

from __future__ import annotations

import json
import threading
import time

import pytest

pytest.importorskip("net")
pytest.importorskip("net._net")

import net  # noqa: E402

PSK = "5b" * 32
ECHO_SCHEMA = json.dumps(
    {"type": "object", "properties": {"message": {"type": "string"}}}
)


def _mesh() -> "net.NetMesh":
    m = net.NetMesh("127.0.0.1:0", PSK, permissive_channels=True)
    m.start()
    return m


def _handshake(connector, acceptor) -> None:
    """`connector` dials `acceptor`; `acceptor` accepts concurrently. Both
    release the GIL while blocking, so running one in a thread is deadlock-free
    (the SDK cross-node idiom)."""
    errs = []

    def _accept():
        try:
            acceptor.accept(connector.node_id)
        except Exception as e:  # noqa: BLE001
            errs.append(e)

    t = threading.Thread(target=_accept, daemon=True)
    t.start()
    time.sleep(0.05)  # let the accept register before the connect lands
    connector.connect(acceptor.local_addr, acceptor.public_key, acceptor.node_id)
    t.join(timeout=5)
    if errs:
        raise errs[0]


def _call_retry(rpc, target, service, body, attempts=6):
    """Retry a direct nRPC call — the first call to a freshly-served handler can
    lose its reply before the per-caller reply subscription propagates."""
    last = None
    for _ in range(attempts):
        try:
            return rpc.call(target, service, body)
        except Exception as e:  # noqa: BLE001
            last = e
            time.sleep(0.1)
    raise last


def test_publish_tools_is_invoked_over_the_wire():
    # Build both nodes UNSTARTED: the SDK cross-node idiom is accept/connect
    # first, then start (a started node's receive-loop auto-accept would race a
    # manual accept, timing the handshake out).
    provider = net.NetMesh("127.0.0.1:0", PSK, permissive_channels=True)
    consumer = net.NetMesh("127.0.0.1:0", PSK, permissive_channels=True)

    calls = []
    handle = None

    async def echo_handler(name, args_json):
        calls.append(name)
        args = json.loads(args_json)
        return args.get("message", "")

    try:
        _handshake(consumer, provider)
        provider.start()
        consumer.start()

        # Cross-node invocation needs the explicit opt-in — the default scope
        # admits only the publishing node itself.
        handle = provider.publish_tools(
            [("echo", "echo it back", ECHO_SCHEMA)], echo_handler, allow_any_caller=True
        )
        assert "echo" in handle.tools
        assert handle.serving is True

        rpc = net.MeshRpc(consumer)
        body = json.dumps({"message": "hi over the wire"}).encode("utf-8")
        result_bytes = _call_retry(rpc, provider.node_id, "echo", body)

        # The wire body is a CallToolResult (camelCase): text_ok content, no error.
        result = json.loads(result_bytes.decode("utf-8"))
        assert result["isError"] is False
        text = "".join(
            b.get("text", "") for b in result["content"] if b.get("type") == "text"
        )
        assert text == "hi over the wire"
        # The proof: the Python callback ran on a remote invoke.
        assert calls, "the Python async callback was invoked"
    finally:
        if handle is not None:
            handle.stop()
        for m in (consumer, provider):
            try:
                m.shutdown()
            except Exception:  # noqa: BLE001
                pass


def test_publish_tools_default_scope_denies_remote_callers():
    """Fail-closed by default: without ``allow_any_caller`` (or an explicit
    ``owner_origin``), a *remote* node's invoke is rejected by the owner scope
    and the Python callback never runs."""
    provider = net.NetMesh("127.0.0.1:0", PSK, permissive_channels=True)
    consumer = net.NetMesh("127.0.0.1:0", PSK, permissive_channels=True)

    calls = []
    handle = None

    async def echo_handler(name, args_json):
        calls.append(name)
        return "should never run"

    try:
        _handshake(consumer, provider)
        provider.start()
        consumer.start()

        handle = provider.publish_tools(
            [("echo", "echo it back", ECHO_SCHEMA)], echo_handler
        )
        assert "echo" in handle.tools

        rpc = net.MeshRpc(consumer)
        body = json.dumps({"message": "sneaky"}).encode("utf-8")
        with pytest.raises(Exception):
            _call_retry(rpc, provider.node_id, "echo", body, attempts=3)
        assert calls == [], "the callback must not run for a denied caller"
    finally:
        if handle is not None:
            handle.stop()
        for m in (consumer, provider):
            try:
                m.shutdown()
            except Exception:  # noqa: BLE001
                pass


def test_publish_tools_reports_tools_skips_and_withdraws():
    # No network: the publish surface itself (tools / skipped / serving /
    # withdraw) is exercisable on a single node.
    provider = _mesh()

    async def handler(name, args_json):
        return "ok"

    handle = provider.publish_tools(
        [
            ("alpha", None, ECHO_SCHEMA),
            ("beta", "second tool", ECHO_SCHEMA),
            ("   ", None, ECHO_SCHEMA),  # whitespace-only name → skipped
        ],
        handler,
    )
    try:
        assert set(handle.tools) == {"alpha", "beta"}
        assert handle.skipped_tools == ["   "]
        assert handle.serving is True
        handle.withdraw()
        assert handle.serving is False
        handle.withdraw()  # idempotent
        assert handle.tools == []
    finally:
        try:
            provider.shutdown()
        except Exception:  # noqa: BLE001
            pass
