#!/usr/bin/env python
"""The Q6 ``net_sdk``-only consumer run for the organization streaming surface
(``ORG_SCOPED_STREAMING_PLAN`` Stage 4).

This program IS the Q6 evidence vehicle: it CALLS and SERVES every org shape
through ``net_sdk`` alone — its imports are the standard library and
``net_sdk`` ONLY (no ``net`` import, no private binding access; ``net_sdk``'s
own facade unwraps whatever it needs). Each invocation drives ONE live
two-mesh round trip over real transport for one cell, asserts the round trip
AND the serve-side verified-caller attribution at the named assertions below,
prints one ``CELL_OK`` line with the observed facts, and exits 0. Any failed
assertion exits non-zero with its named traceback line.

Usage::

    python org_streaming_consumer.py --scenario-dir DIR \
        --kind same_org|granted --cell CELL

where ``DIR`` holds a generated issuance chain (``manifest.json`` + the
credential files it names — minted by the ``gen_org_scenario`` /
``gen_org_same_scenario`` examples; a CONSUMER loads credentials, it does not
mint them) and CELL is one of: ``stream_sync``, ``stream_async``,
``rollup_sync``, ``rollup_async``, ``mirror_sync``, ``mirror_async``,
``unary``, ``midstream``, ``cancel``.

The cells mirror the wheel binding's live witnesses (``test_org_live.py``)
one layer up: same scenarios, same assertions, the calls and serves routed
through ``net_sdk``'s org facade instead of the ``net`` package.

**Handler-drop contract (F-S3.1-2, Specification §2.2) — the level these
cells hold.** A protected call runs under a per-call retire supervisor; on
retirement it drops the handler future WITHOUT a final poll. Handlers are
never assumed to observe a cancellation EVENT — only the retirement
observables (request input fencing to EOF; library-controlled sinks stopping)
are contracts. See ``net_sdk.org.HANDLER_DROP_CONTRACT`` and the ``cancel``
cell's docstring for exactly what cancellation CAN and CANNOT be observed.
"""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import json
import os
import socket
import sys
import threading
import time

import net_sdk
import net_sdk.org as org


def _free_addr() -> str:
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        s.bind(("127.0.0.1", 0))
        return "127.0.0.1:%d" % s.getsockname()[1]
    finally:
        s.close()


class _Scenario:
    """One generated issuance chain + the accessors every cell shares."""

    def __init__(self, kind: str, outdir: str, manifest: dict) -> None:
        self.kind = kind  # "granted" | "same_org"
        self.outdir = outdir
        self.manifest = manifest

    def path(self, rel: str) -> str:
        return os.path.join(self.outdir, rel)

    @property
    def psk(self) -> str:
        return self.manifest["psk_hex"]

    @property
    def provider(self) -> dict:
        return self.manifest["provider"]

    @property
    def caller(self) -> dict:
        return self.manifest["caller"]

    def service(self, shape: str) -> str:
        # The granted grant names exactly `nrpc:customer.read`, so every
        # granted cell registers that one name (the shape is fixed by the
        # REGISTRATION, not the name). Same-org cells derive names per shape
        # so cross-shape confusion is observable.
        if self.kind == "granted":
            return self.manifest["granted_service"]
        return f"internal.{shape}"


def _load_scenario(kind: str, outdir: str) -> _Scenario:
    with open(os.path.join(outdir, "manifest.json"), encoding="utf-8") as f:
        return _Scenario(kind, outdir, json.load(f))


def _mesh(seed_hex: str, psk_hex: str) -> net_sdk.MeshNode:
    return net_sdk.MeshNode(
        bind_addr=_free_addr(),
        psk=psk_hex,
        identity_seed=bytes.fromhex(seed_hex),
        heartbeat_interval_ms=200,
    )


def _handshake(connector: net_sdk.MeshNode, acceptor: net_sdk.MeshNode) -> None:
    # accept on a thread while connect fires — both halves in flight at once.
    errors: list = []

    def _accept() -> None:
        try:
            acceptor.accept(connector.node_id)
        except Exception as e:  # noqa: BLE001
            errors.append(e)

    t = threading.Thread(target=_accept, daemon=True)
    t.start()
    time.sleep(0.05)
    connector.connect(acceptor.local_addr, acceptor.public_key, acceptor.node_id)
    t.join(timeout=5)
    if errors:
        raise errors[0]


class _Pair:
    """A live provider/caller mesh pair under one authorization mode."""

    def __init__(self, provider: net_sdk.MeshNode, caller: net_sdk.MeshNode) -> None:
        self.provider = provider
        self.caller = caller

    def announce(self) -> None:
        # Force scoped-catalog emission on both sides each convergence pass.
        for m in (self.provider, self.caller):
            try:
                m.announce_capabilities({})
            except Exception:  # noqa: BLE001
                pass


@contextlib.contextmanager
def _live_pair(sc: _Scenario):
    provider = _mesh(sc.provider["seed_hex"], sc.psk)
    caller = _mesh(sc.caller["seed_hex"], sc.psk)
    try:
        org.install_org_authority(provider, sc.path(sc.provider["authority_dir"]))
        org.install_org_authority(caller, sc.path(sc.caller["authority_dir"]))
        if sc.kind == "granted":
            with open(sc.path(sc.provider["grant_path"]), "rb") as f:
                provider_grant = f.read()
            org.install_provider_grant_audience(
                provider, provider_grant, sc.path(sc.provider["grant_secret_path"])
            )
        _handshake(caller, provider)
        provider.start()
        caller.start()
        yield _Pair(provider, caller)
    finally:
        caller.shutdown()
        provider.shutdown()


def _credentials(sc: _Scenario) -> org.OrgCredentials:
    with open(sc.path(sc.caller["membership_path"]), "rb") as f:
        membership = f.read()
    with open(sc.path(sc.caller["dispatcher_path"]), "rb") as f:
        dispatcher = f.read()
    if sc.kind == "granted":
        with open(sc.path(sc.caller["grant_path"]), "rb") as f:
            grant = f.read()
        return org.OrgCredentials(
            membership, dispatcher, [grant], [sc.path(sc.caller["grant_secret_path"])]
        )
    # Same-org: membership + dispatcher only. The shared owner audience
    # travels through the adopted authority dirs (§3.4) — no audience
    # secret paths.
    return org.OrgCredentials(membership, dispatcher, [], [])


def _bind(sc: _Scenario, caller_mesh, *, async_: bool = False):
    cls = org.AsyncOrgClient if async_ else org.OrgClient
    return cls.bind(caller_mesh, _credentials(sc))


def _facts_dict(facts: dict) -> dict:
    return {
        "is_same_org": facts["is_same_org"],
        "entity": facts["entity"].hex(),
        "acting_org": facts["acting_org"].hex(),
        "provider_org": facts["provider_org"].hex(),
        "capability": facts["capability"].hex(),
        "provider": facts["provider"].hex(),
    }


def _assert_facts(facts: dict, sc: _Scenario) -> None:
    # The five verified fields, none caller-claimed — the serve-side evidence.
    assert facts["is_same_org"] is (sc.kind == "same_org")
    assert len(facts["entity"]) == 32
    assert facts["acting_org"].hex() == sc.caller["org_id_hex"]
    assert facts["provider_org"].hex() == sc.provider["org_id_hex"]
    assert (
        facts["acting_org"] == facts["provider_org"]
        if sc.kind == "same_org"
        else (facts["acting_org"] != facts["provider_org"])
    )
    assert len(facts["capability"]) == 32
    assert facts["provider"] and len(facts["provider"]) == 32


def _converge(pair: _Pair, sc: _Scenario, service: str, open_fn, timeout: float = 45.0):
    """Retry the OPEN verb until private discovery converges. Each attempt is
    a distinct call (the facade itself never retries — a signed proof is bound
    to one call id); attempts that fail planning send nothing."""
    last = None
    end = time.time() + timeout
    while time.time() < end:
        pair.announce()
        try:
            return open_fn()
        except Exception as e:  # noqa: BLE001
            last = e
            time.sleep(1)
    raise AssertionError(f"the {sc.kind} call never converged for {service!r}: {last}")


async def _aconverge(pair: _Pair, sc: _Scenario, service: str, open_fn, timeout: float = 45.0):
    last = None
    end = time.time() + timeout
    while time.time() < end:
        pair.announce()
        try:
            return await open_fn()
        except Exception as e:  # noqa: BLE001
            last = e
            await asyncio.sleep(1)
    raise AssertionError(f"the {sc.kind} call never converged for {service!r}: {last}")


# =========================================================================
# Server-streaming — `call_streaming` + `serve_org_streaming`.
# =========================================================================


def cell_stream_sync(sc: _Scenario) -> dict:
    svc = sc.service("stream")
    with _live_pair(sc) as pair:
        seen: dict = {}

        def handler(caller: dict, request: bytes, sink) -> None:
            seen["facts"] = caller
            sink.send(b"one:" + request)
            sink.send(b"two:" + request)

        handle = org.serve_org_streaming(pair.provider, svc, sc.kind, handler, None)
        client = _bind(sc, pair.caller)
        stream = None
        try:
            stream = _converge(pair, sc, svc, lambda: client.call_streaming(svc, b"hi"))
            chunks = list(stream)
        finally:
            if stream is not None:
                stream.close()
            client.close()
            handle.close()
        assert chunks == [b"one:hi", b"two:hi"]
        _assert_facts(seen["facts"], sc)
        return {"chunks": [c.decode() for c in chunks], "facts": _facts_dict(seen["facts"])}


def cell_stream_async(sc: _Scenario) -> dict:
    svc = sc.service("stream")
    with _live_pair(sc) as pair:
        seen: dict = {}

        async def handler(caller: dict, request: bytes, sink) -> None:
            seen["facts"] = caller
            sink.send(b"one:" + request)
            sink.send(b"two:" + request)

        handle = org.serve_org_streaming(pair.provider, svc, sc.kind, handler, None)
        client = _bind(sc, pair.caller, async_=True)

        async def _run() -> list:
            stream = await _aconverge(
                pair, sc, svc, lambda: client.call_streaming(svc, b"hi")
            )
            chunks = []
            async for chunk in stream:
                chunks.append(chunk)
            await stream.aclose()
            return chunks

        try:
            chunks = asyncio.run(_run())
        finally:
            client.close()
            handle.close()
        assert chunks == [b"one:hi", b"two:hi"]
        _assert_facts(seen["facts"], sc)
        return {"chunks": [c.decode() for c in chunks], "facts": _facts_dict(seen["facts"])}


# =========================================================================
# Client-streaming — `call_client_stream` + `serve_org_client_stream`.
# =========================================================================


def cell_rollup_sync(sc: _Scenario) -> dict:
    svc = sc.service("rollup")
    with _live_pair(sc) as pair:
        seen: dict = {}

        def handler(caller: dict, stream) -> bytes:
            seen["facts"] = caller
            total = 0
            for chunk in stream:
                total += len(chunk)
            return json.dumps({"chunks": total}).encode("utf-8")

        handle = org.serve_org_client_stream(pair.provider, svc, sc.kind, handler, None)
        client = _bind(sc, pair.caller)
        call = None
        try:
            call = _converge(pair, sc, svc, lambda: client.call_client_stream(svc))
            for part in (b"a", b"bb", b"ccc"):
                call.send(part)
            reply = call.finish()
        finally:
            if call is not None:
                call.close()
            client.close()
            handle.close()
        assert json.loads(reply.decode("utf-8")) == {"chunks": 6}
        _assert_facts(seen["facts"], sc)
        return {"reply": json.loads(reply.decode("utf-8")), "facts": _facts_dict(seen["facts"])}


def cell_rollup_async(sc: _Scenario) -> dict:
    svc = sc.service("rollup")
    with _live_pair(sc) as pair:
        seen: dict = {}

        async def handler(caller: dict, stream) -> bytes:
            seen["facts"] = caller
            total = 0
            for chunk in stream:
                total += len(chunk)
            return json.dumps({"chunks": total}).encode("utf-8")

        handle = org.serve_org_client_stream(pair.provider, svc, sc.kind, handler, None)
        client = _bind(sc, pair.caller, async_=True)

        async def _run() -> bytes:
            call = await _aconverge(
                pair, sc, svc, lambda: client.call_client_stream(svc)
            )
            for part in (b"a", b"bb", b"ccc"):
                await call.send(part)
            return await call.finish()

        try:
            reply = asyncio.run(_run())
        finally:
            client.close()
            handle.close()
        assert json.loads(reply.decode("utf-8")) == {"chunks": 6}
        _assert_facts(seen["facts"], sc)
        return {"reply": json.loads(reply.decode("utf-8")), "facts": _facts_dict(seen["facts"])}


# =========================================================================
# Duplex — `call_duplex` + `serve_org_duplex`.
# =========================================================================


def cell_mirror_sync(sc: _Scenario) -> dict:
    svc = sc.service("mirror")
    with _live_pair(sc) as pair:
        seen: dict = {}

        def handler(caller: dict, stream, sink) -> None:
            seen["facts"] = caller
            for chunk in stream:
                sink.send(b"echo:" + chunk)

        handle = org.serve_org_duplex(pair.provider, svc, sc.kind, handler, None)
        client = _bind(sc, pair.caller)
        call = None
        try:
            call = _converge(pair, sc, svc, lambda: client.call_duplex(svc))
            for part in (b"a", b"b", b"c"):
                call.send(part)
            call.finish_sending()
            echoed = list(call)
        finally:
            if call is not None:
                call.close()
            client.close()
            handle.close()
        assert echoed == [b"echo:a", b"echo:b", b"echo:c"]
        _assert_facts(seen["facts"], sc)
        return {"echoed": [c.decode() for c in echoed], "facts": _facts_dict(seen["facts"])}


def cell_mirror_async(sc: _Scenario) -> dict:
    svc = sc.service("mirror")
    with _live_pair(sc) as pair:
        seen: dict = {}

        async def handler(caller: dict, stream, sink) -> None:
            seen["facts"] = caller
            for chunk in stream:
                sink.send(b"echo:" + chunk)

        handle = org.serve_org_duplex(pair.provider, svc, sc.kind, handler, None)
        client = _bind(sc, pair.caller, async_=True)

        async def _run() -> list:
            call = await _aconverge(pair, sc, svc, lambda: client.call_duplex(svc))
            # Exercise the split halves (§4.4: the existing handle types).
            up, down = call.into_split()
            for part in (b"a", b"b", b"c"):
                await up.send(part)
            await up.finish()
            echoed = []
            async for chunk in down:
                echoed.append(chunk)
            await down.aclose()
            return echoed

        try:
            echoed = asyncio.run(_run())
        finally:
            client.close()
            handle.close()
        assert echoed == [b"echo:a", b"echo:b", b"echo:c"]
        _assert_facts(seen["facts"], sc)
        return {"echoed": [c.decode() for c in echoed], "facts": _facts_dict(seen["facts"])}


# =========================================================================
# The preserved unary — raw `call`/`serve_org` AND the typed wrappers
# (`TypedOrgClient`/`serve_org_typed`), both through `net_sdk`.
# =========================================================================


def cell_unary(sc: _Scenario) -> dict:
    svc = sc.service("unary")
    with _live_pair(sc) as pair:
        # (1) The preserved raw unary — the original X2 shape.
        seen_raw: dict = {}

        def _handler(caller_facts: dict, request: bytes) -> bytes:
            seen_raw["facts"] = caller_facts
            body = json.loads(request.decode("utf-8"))
            return json.dumps({"n": body["n"] + 1, "servedBy": "pysdk-consumer"}).encode(
                "utf-8"
            )

        handle = org.serve_org(pair.provider, svc, sc.kind, _handler, None)
        client = _bind(sc, pair.caller)
        try:
            request = json.dumps({"n": 7}).encode("utf-8")
            reply = _converge(pair, sc, svc, lambda: client.call(svc, request))
        finally:
            client.close()
            handle.close()
        assert json.loads(reply.decode("utf-8")) == {"n": 8, "servedBy": "pysdk-consumer"}
        _assert_facts(seen_raw["facts"], sc)

        # (2) The typed wrappers' unary (JSON codec), same service name re-
        # registered sequentially — the `net_sdk.org` pass-through over
        # `TypedOrgClient` / `serve_org_typed`.
        seen_typed: dict = {}

        def _typed_handler(caller_facts: dict, body):
            seen_typed["facts"] = caller_facts
            return {"n": body["n"] + 1, "servedBy": "pysdk-consumer-typed"}

        typed_handle = org.serve_org_typed(
            pair.provider, svc, sc.kind, _typed_handler, None
        )
        typed_client = org.TypedOrgClient.bind(pair.caller, _credentials(sc))
        try:
            typed_reply = _converge(
                pair, sc, svc, lambda: typed_client.call(svc, {"n": 70})
            )
        finally:
            typed_client.close()
            typed_handle.close()
        assert typed_reply == {"n": 71, "servedBy": "pysdk-consumer-typed"}
        _assert_facts(seen_typed["facts"], sc)
        return {
            "raw": json.loads(reply.decode("utf-8")),
            "typed": typed_reply,
            "facts": _facts_dict(seen_raw["facts"]),
            "typed_facts": _facts_dict(seen_typed["facts"]),
        }


# =========================================================================
# Midstream-error vocabulary — the `org_err_to_py` mirror at the wrapper
# level (§4.4): `org:` vocabulary (the `OrgError` family), never `RpcError`.
# =========================================================================


def cell_midstream(sc: _Scenario) -> dict:
    svc = sc.service("slow")
    with _live_pair(sc) as pair:
        seen: dict = {}

        def handler(caller: dict, request: bytes, sink) -> None:
            seen["facts"] = caller
            sink.send(b"first")
            # Inducement: the handler's own mid-stream failure = the stream's
            # final error. (The deadline-retirement variant — sleep past the
            # caller's 1.5 s deadline — is blocked by the F-S4B-7 core gap:
            # CallOptions::deadline does not terminate an in-flight stream.
            # Both variants assert the SAME property: a midstream terminal
            # surfaces as the org:rpc: vocabulary, never a false clean EOF.
            # Main takeover fix — F-S4PySdk-6.)
            raise RuntimeError("midstream inducement")

        handle = org.serve_org_streaming(pair.provider, svc, sc.kind, handler, None)
        client = _bind(sc, pair.caller)
        stream = None
        try:
            stream = _converge(
                pair, sc, svc, lambda: client.call_streaming(svc, b"hi", 1500)
            )
            assert next(stream) == b"first"
            with _raises() as ei:
                next(stream)  # deadline retirement = the stream's final error
        finally:
            if stream is not None:
                stream.close()
            client.close()
            handle.close()
        exc = ei.value
        # The org vocabulary through the net_sdk mirror — the asserted class
        # identity (this is the property the midstream pin owns).
        assert isinstance(exc, org.OrgError), (
            "org handles must surface the mirrored org: vocabulary, "
            f"got {type(exc).__name__}: {exc}"
        )
        assert not isinstance(exc, org.OrgAdmissionDeniedError), (
            "deadline retirement is `org:rpc:`, never an admission denial"
        )
        parsed = org.parse_org_error(str(exc))
        assert parsed.domain == "rpc", parsed
        assert str(exc).startswith("org:rpc:"), str(exc)
        _assert_facts(seen["facts"], sc)
        return {
            "error_type": type(exc).__name__,
            "message": str(exc),
            "domain": parsed.domain,
            "facts": _facts_dict(seen["facts"]),
        }


# =========================================================================
# The `task.cancel()` propagation links (the F-S3.1-2 named item).
# =========================================================================


def cell_cancel(sc: _Scenario) -> dict:
    """``asyncio.Task.cancel()`` propagates through the async org handles
    into the substrate's cancel machinery — asserted at each observable's
    REAL contract level (this docstring IS the F-S3.1-2 level statement).

    **What cancellation CAN be observed — the three links this cell asserts,
    in order.**

    1. Caller side: the awaiting task raises ``asyncio.CancelledError``
       promptly (the bridge drops the pull and fires ``mesh.cancel(token)``).
    2. Substrate side, LOCAL: the per-stream cancel watcher tears the call's
       pending entry down, so the response side terminates — observed here
       as the ``async for`` raising the TYPED cancellation terminal
       ``org:rpc:cancelled`` (the repair pass's documented typed terminal
       vocabulary: a cancel ends a fold with the typed error through the
       ``org_err_to_py`` mirror, never a swallowed clean end) — **while the
       caller handle is still alive**. This is the link that discriminates
       the cancel-token path from ordinary teardown: with no token threaded
       the drain would park until the 120 s call deadline.
    3. Provider side: retirement is observable as the handler's request
       input fencing to EOF (its ``for chunk in stream:`` ends with NO final
       item and no exception) — the shape's library-controlled input, closed
       by the §2.2 retire supervisor. **This link arrives with the WIRE
       CANCEL, which the substrate publishes from the caller handle's
       ``close()``/drop (its per-shape Drop contract — the stream-cancel
       watcher's teardown is local by design).** The cell therefore closes
       the handle before asserting it; ``task.cancel()`` sets the chain in
       motion, and the provider-side retirement observables land at handle
       teardown.

    **What cancellation CANNOT be observed — and what this cell does NOT
    claim (Specification §2.2, the F-S3.1-2 handler-drop contract).**

    * There is NO handler-side cancel event. The retire supervisor may drop
      the handler future WITHOUT a final poll; a handler must never assume it
      is resumed, and must never treat cancellation as something it receives.
    * The handler below is a ``def`` on a detached blocking thread: dropping
      the Rust handler future cannot interrupt it. It runs to whatever point
      it reaches; its return value is discarded and its already-performed
      effects are not recalled.
    * An ``async def`` handler's dispatched coroutine is cancelled on
      teardown, so ``asyncio.CancelledError`` MAY surface at an ``await`` —
      but only as best-effort teardown machinery: the supervisor may drop
      the future without a final poll, and the coroutine may never be
      resumed to observe anything. Never rely on it for correctness.
    """
    svc = "internal.cancel" if sc.kind == "same_org" else sc.manifest["granted_service"]
    with _live_pair(sc) as pair:
        seen: dict = {}

        def handler(caller: dict, stream, sink) -> None:
            # Drains until EOF and deliberately emits NOTHING back: the pull
            # below must stay parked, so its only completion paths are the
            # cancel or the 120 s call deadline. And without a
            # `finish_sending()` half-close the ONLY thing that can end this
            # drain loop is retirement (the §2.2 input fence) or that same
            # deadline — so `input_eof` is exactly the retirement observable,
            # never a natural completion.
            for chunk in stream:
                pass
            seen["input_eof"] = True

        handle = org.serve_org_duplex(pair.provider, svc, sc.kind, handler, None)
        client = _bind(sc, pair.caller, async_=True)
        try:

            async def _run() -> None:
                call = await _aconverge(
                    pair, sc, svc, lambda: client.call_duplex(svc, 120_000)
                )
                await call.send(b"x")

                async def _pull() -> bytes:
                    # `__anext__` returns a pyo3-async-runtimes Future, not a
                    # coroutine — wrap it so `create_task` (and therefore a
                    # clean task-level `cancel()`) applies; the cancellation
                    # still propagates through the await into the bridge.
                    return await call.__anext__()

                pull = asyncio.create_task(_pull())
                # The pull is parked: the handler emits nothing back, so the
                # only completion paths are the cancel or the 120 s deadline.
                await asyncio.sleep(0.5)

                # ---- link 1: caller-side CancelledError ----
                pull.cancel()
                with _raises() as ei1:
                    await pull
                assert ei1.exc_type is asyncio.CancelledError, ei1.exc_type

                # ---- link 2: the LOCAL teardown observable, handle ALIVE ----
                async def _drain() -> None:
                    async for _ in call:
                        pass

                # The token watcher already closed the call's response side;
                # this drain ends immediately — as the TYPED cancellation
                # terminal `org:rpc:cancelled` (the repair pass's documented
                # typed terminal vocabulary: never a swallowed clean end).
                # Without the cancel token it would park until the 120 s
                # deadline (bounded out at 5 s). DELIBERATE CONTRACT UPDATE
                # (owner Q1): this pin was a clean `async for` end before the
                # typed vocabulary landed. The org-vocabulary assertions
                # mirror `cell_midstream`'s.
                with _raises() as ei2:
                    await asyncio.wait_for(_drain(), 5)
                exc2 = ei2.value
                assert isinstance(exc2, org.OrgError), (
                    "a cancelled org handle must surface the mirrored org: "
                    f"vocabulary, got {type(exc2).__name__}: {exc2}"
                )
                assert not isinstance(exc2, org.OrgAdmissionDeniedError), (
                    "cancellation is `org:rpc:cancelled`, never an admission "
                    f"denial: {exc2}"
                )
                parsed2 = org.parse_org_error(str(exc2))
                assert parsed2.domain == "rpc", parsed2
                assert str(exc2).startswith("org:rpc:cancelled"), str(exc2)

                # ---- link 3 rides the handle's close()/drop ----
                call.close()  # per-shape Drop publishes the wire CANCEL

            asyncio.run(_run())
            deadline = time.time() + 15
            while time.time() < deadline and not seen.get("input_eof"):
                time.sleep(0.1)
            assert seen.get("input_eof"), (
                "after task.cancel() and handle close, the handler's request "
                "input never fenced to EOF — the wire CANCEL did not retire "
                "the provider-side call"
            )
        finally:
            client.close()
            handle.close()
        return {
            "link1": "CancelledError",
            "link2": "drain_ended:org:rpc:cancelled",
            "link3": "input_eof",
        }


class _raises:
    """A minimal ``pytest.raises`` stand-in (this program imports no test
    framework): ``with _raises() as ei: ...`` captures the raised exception
    and fails the cell if nothing raised."""

    def __init__(self) -> None:
        self.value: BaseException | None = None
        self.exc_type: type | None = None

    def __enter__(self) -> "_raises":
        return self

    def __exit__(self, exc_type, exc, tb) -> bool:
        if exc_type is None:
            raise AssertionError("expected an exception, none was raised")
        self.exc_type = exc_type
        self.value = exc
        return True  # consume the exception; assertions inspect .value


_CELLS = {
    "stream_sync": cell_stream_sync,
    "stream_async": cell_stream_async,
    "rollup_sync": cell_rollup_sync,
    "rollup_async": cell_rollup_async,
    "mirror_sync": cell_mirror_sync,
    "mirror_async": cell_mirror_async,
    "unary": cell_unary,
    "midstream": cell_midstream,
    "cancel": cell_cancel,
}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--scenario-dir", required=True)
    parser.add_argument("--kind", choices=("same_org", "granted"), required=True)
    parser.add_argument("--cell", choices=sorted(_CELLS), required=True)
    args = parser.parse_args()

    sc = _load_scenario(args.kind, args.scenario_dir)
    result = _CELLS[args.cell](sc)
    print("CELL_OK " + json.dumps({"cell": args.cell, "kind": args.kind, **result}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
