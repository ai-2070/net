"""Live organization RPC through the Python wheel — unary (OSDK-L X2) and the
Stage 4 streaming siblings (§4.4).

The original cell here proved one live admitted cross-org UNARY call. The S4
siblings extend it per shape over the SAME two-mesh live transport and the
SAME Rust-minted credential chains:

* ``test_org_streaming_sync_call_and_serve[same_org|granted]``,
  ``test_org_streaming_async_call_and_serve[...]`` — server-streaming, the
  sync ``OrgClient.call_streaming`` and the async ``AsyncOrgClient`` form,
  against ``serve_org_streaming`` (a ``def`` handler for the sync cells, an
  ``async def`` handler for the async cells — both handler drives).
* ``test_org_client_stream_sync_call_and_serve[...]`` /
  ``_async_...`` — client-streaming over ``call_client_stream`` +
  ``serve_org_client_stream``.
* ``test_org_duplex_sync_call_and_serve[...]`` / ``_async_...`` — duplex over
  ``call_duplex`` + ``serve_org_duplex``.
* ``test_task_cancel_propagates_to_retirement_observables`` — the
  ``task.cancel()`` propagation witness (see its docstring for the exact
  F-S3.1-2 handler-drop level it claims).
* ``test_streaming_midstream_error_surfaces_the_org_vocabulary`` — midstream
  errors on org handles classify through ``org_err_to_py`` (the ``org:``
  vocabulary), never the ``RpcError`` family.

Scenarios: ``granted`` consumes the ``gen_org_scenario`` manifest (org A's
caller invoking org B's granted capability); ``same_org`` consumes the
``gen_org_same_scenario`` package example's manifest (one organization, a
provider and a caller sharing the ONE owner audience pre-staged into both
adopted authority dirs). Both are generated ONCE per module run — each
generation is a cold cargo build.

Env: needs a Rust toolchain (to generate the scenarios) and the wheel built
with the ``org`` feature; skips cleanly otherwise.

**Handler-drop contract (F-S3.1-2, Specification §2.2) — the level these
witnesses hold.** A protected call runs under a per-call retire supervisor;
on retirement it drops the handler future WITHOUT a final poll. Handlers are
never assumed to observe a cancellation EVENT — only the retirement
observables (request input fencing to EOF; library-controlled sinks stopping)
are contracts. See
``test_task_cancel_propagates_to_retirement_observables`` for what
cancellation CAN and CANNOT be observed, per shape.
"""

from __future__ import annotations

import asyncio
import contextlib
import json
import os
import shutil
import socket
import subprocess
import tempfile
import threading
import time
import uuid

import pytest

net = pytest.importorskip("net", reason="net wheel not built")

if not hasattr(net, "install_org_authority"):
    pytest.skip("net built without the org feature", allow_module_level=True)

# The S4 surface is the subject of these witnesses: a wheel without it must
# FAIL LOUDLY, not skip (a skip would make every witness vacuous on exactly
# the machines that run it).
assert hasattr(net, "serve_org_streaming"), "wheel lacks serve_org_streaming (stale build?)"
assert hasattr(net, "AsyncOrgClient"), "wheel lacks AsyncOrgClient (stale build?)"

from net.org import parse_org_error  # noqa: E402

_HERE = os.path.dirname(os.path.abspath(__file__))
# bindings/python/tests -> crates/net (the cargo workspace root).
_CRATE_ROOT = os.path.abspath(os.path.join(_HERE, "..", "..", ".."))


def _free_addr() -> str:
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        s.bind(("127.0.0.1", 0))
        return "127.0.0.1:%d" % s.getsockname()[1]
    finally:
        s.close()


def _gen_scenario(outdir: str) -> dict:
    """Mint the cross-org (granted) chain — the SAME manifest a Go / Node
    harness loads (`gen_org_scenario` = `write_cross_org_scenario`)."""
    subprocess.run(
        [
            "cargo", "run", "-q", "-p", "net-mesh-sdk", "--features", "net,cortex,fixtures",
            "--example", "gen_org_scenario", "--", outdir,
        ],
        cwd=_CRATE_ROOT,
        check=True,
        env={**os.environ, "CARGO_INCREMENTAL": "0"},
    )
    with open(os.path.join(outdir, "manifest.json"), encoding="utf-8") as f:
        return json.load(f)


def _gen_same_org_scenario(outdir: str) -> dict:
    """Mint the same-org chain — the package's `gen_org_same_scenario` example
    (one organization, shared owner audience pre-staged per §3.4)."""
    subprocess.run(
        [
            "cargo", "run", "-q", "-p", "net-python", "--no-default-features",
            "--features", "org", "--example", "gen_org_same_scenario", "--", outdir,
        ],
        cwd=_CRATE_ROOT,
        check=True,
        env={**os.environ, "CARGO_INCREMENTAL": "0"},
    )
    with open(os.path.join(outdir, "manifest.json"), encoding="utf-8") as f:
        return json.load(f)


class _Scenario:
    """One generated issuance chain + the accessors every test shares."""

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


@pytest.fixture(scope="module")
def scenarios():
    """Generate BOTH issuance chains once per module run.

    Each generation shells out to `cargo run --example ...` with a feature set
    distinct from the wheel's, so it is a cold build inside the first test
    that requests this fixture — hence the per-test `timeout(600)` markers
    (matching the original X2 cell's budget).
    """
    out = {}
    for kind, gen in (("granted", _gen_scenario), ("same_org", _gen_same_org_scenario)):
        # `os.makedirs` under the system temp dir — NOT `tempfile.mkdtemp`, which
        # on Windows stamps an owner-only (Owner Rights) ACE that the
        # audience-secret loader (rightly) refuses on inherited secret files.
        outdir = os.path.join(tempfile.gettempdir(), f"s4py-{kind}-{uuid.uuid4().hex}")
        os.makedirs(outdir)
        out[kind] = _Scenario(kind, outdir, gen(outdir))
    yield out
    for sc in out.values():
        shutil.rmtree(sc.outdir, ignore_errors=True)


def _mesh(seed_hex: str, psk_hex: str):
    return net.NetMesh(
        bind_addr=_free_addr(),
        psk=psk_hex,
        identity_seed=bytes.fromhex(seed_hex),
        heartbeat_interval_ms=200,
        permissive_channels=True,
    )


def _handshake(connector, acceptor) -> None:
    # b.accept on a thread while a.connect fires — the conftest mesh_pair shape.
    errors: list[Exception] = []

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

    def __init__(self, provider, caller) -> None:
        self.provider = provider
        self.caller = caller

    def announce(self) -> None:
        # Force scoped-catalog emission on both sides each convergence pass
        # (the Python mesh cannot lower `min_announce_interval`, so the scoped
        # emission is throttled to ~10 s cycles — the retry loop waits them
        # out; the WORST case is 45 s, the usual case is one or two passes).
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
        net.install_org_authority(provider, sc.path(sc.provider["authority_dir"]))
        net.install_org_authority(caller, sc.path(sc.caller["authority_dir"]))
        if sc.kind == "granted":
            with open(sc.path(sc.provider["grant_path"]), "rb") as f:
                provider_grant = f.read()
            net.install_provider_grant_audience(
                provider, provider_grant, sc.path(sc.provider["grant_secret_path"])
            )
        _handshake(caller, provider)
        provider.start()
        caller.start()
        yield _Pair(provider, caller)
    finally:
        caller.shutdown()
        provider.shutdown()


def _bind(sc: _Scenario, caller_mesh, *, async_: bool = False):
    with open(sc.path(sc.caller["membership_path"]), "rb") as f:
        membership = f.read()
    with open(sc.path(sc.caller["dispatcher_path"]), "rb") as f:
        dispatcher = f.read()
    if sc.kind == "granted":
        with open(sc.path(sc.caller["grant_path"]), "rb") as f:
            grant = f.read()
        credentials = net.OrgCredentials(
            membership, dispatcher, [grant], [sc.path(sc.caller["grant_secret_path"])]
        )
    else:
        # Same-org: membership + dispatcher only. The shared owner audience
        # travels through the adopted authority dirs (§3.4) — no audience
        # secret paths, mirroring the Rust `OrgCredentials::new(cert, dg,
        # vec![], vec![])` same-org fixture.
        credentials = net.OrgCredentials(membership, dispatcher, [], [])
    cls = net.AsyncOrgClient if async_ else net.OrgClient
    return cls.bind(caller_mesh, credentials)


def _assert_facts(facts: dict, sc: _Scenario) -> None:
    # The five verified fields, none caller-claimed — the serve-side evidence.
    assert facts["is_same_org"] is (sc.kind == "same_org")
    assert len(facts["entity"]) == 32
    assert facts["acting_org"].hex() == sc.caller["org_id_hex"]
    assert facts["provider_org"].hex() == sc.provider["org_id_hex"]
    assert facts["acting_org"] == facts["provider_org"] if sc.kind == "same_org" else (
        facts["acting_org"] != facts["provider_org"]
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


@pytest.mark.timeout(600)
@pytest.mark.parametrize("access", ["same_org", "granted"])
def test_org_streaming_sync_call_and_serve(scenarios, access) -> None:
    sc = scenarios[access]
    svc = sc.service("stream")
    with _live_pair(sc) as pair:
        seen: dict = {}

        def handler(caller: dict, request: bytes, sink) -> None:
            seen["facts"] = caller
            sink.send(b"one:" + request)
            sink.send(b"two:" + request)

        handle = net.serve_org_streaming(pair.provider, svc, access, handler, None)
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


@pytest.mark.timeout(600)
@pytest.mark.parametrize("access", ["same_org", "granted"])
def test_org_streaming_async_call_and_serve(scenarios, access) -> None:
    sc = scenarios[access]
    svc = sc.service("stream")
    with _live_pair(sc) as pair:
        seen: dict = {}

        async def handler(caller: dict, request: bytes, sink) -> None:
            seen["facts"] = caller
            sink.send(b"one:" + request)
            sink.send(b"two:" + request)

        handle = net.serve_org_streaming(pair.provider, svc, access, handler, None)
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


# =========================================================================
# Client-streaming — `call_client_stream` + `serve_org_client_stream`.
# =========================================================================


@pytest.mark.timeout(600)
@pytest.mark.parametrize("access", ["same_org", "granted"])
def test_org_client_stream_sync_call_and_serve(scenarios, access) -> None:
    sc = scenarios[access]
    svc = sc.service("rollup")
    with _live_pair(sc) as pair:
        seen: dict = {}

        def handler(caller: dict, stream) -> bytes:
            seen["facts"] = caller
            total = 0
            for chunk in stream:
                total += len(chunk)
            return json.dumps({"chunks": total}).encode("utf-8")

        handle = net.serve_org_client_stream(pair.provider, svc, access, handler, None)
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


@pytest.mark.timeout(600)
@pytest.mark.parametrize("access", ["same_org", "granted"])
def test_org_client_stream_async_call_and_serve(scenarios, access) -> None:
    sc = scenarios[access]
    svc = sc.service("rollup")
    with _live_pair(sc) as pair:
        seen: dict = {}

        async def handler(caller: dict, stream) -> bytes:
            seen["facts"] = caller
            total = 0
            for chunk in stream:
                total += len(chunk)
            return json.dumps({"chunks": total}).encode("utf-8")

        handle = net.serve_org_client_stream(pair.provider, svc, access, handler, None)
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


# =========================================================================
# Duplex — `call_duplex` + `serve_org_duplex`.
# =========================================================================


@pytest.mark.timeout(600)
@pytest.mark.parametrize("access", ["same_org", "granted"])
def test_org_duplex_sync_call_and_serve(scenarios, access) -> None:
    sc = scenarios[access]
    svc = sc.service("mirror")
    with _live_pair(sc) as pair:
        seen: dict = {}

        def handler(caller: dict, stream, sink) -> None:
            seen["facts"] = caller
            for chunk in stream:
                sink.send(b"echo:" + chunk)

        handle = net.serve_org_duplex(pair.provider, svc, access, handler, None)
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


@pytest.mark.timeout(600)
@pytest.mark.parametrize("access", ["same_org", "granted"])
def test_org_duplex_async_call_and_serve(scenarios, access) -> None:
    sc = scenarios[access]
    svc = sc.service("mirror")
    with _live_pair(sc) as pair:
        seen: dict = {}

        async def handler(caller: dict, stream, sink) -> None:
            seen["facts"] = caller
            for chunk in stream:
                sink.send(b"echo:" + chunk)

        handle = net.serve_org_duplex(pair.provider, svc, access, handler, None)
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


# =========================================================================
# The `task.cancel()` propagation witness (the F-S3.1-2 named item).
# =========================================================================


@pytest.mark.timeout(600)
def test_task_cancel_propagates_to_retirement_observables(scenarios) -> None:
    """``asyncio.Task.cancel()`` propagates through the async org handles into
    the substrate's cancel machinery — asserted at each observable's REAL
    contract level (this docstring IS the F-S3.1-2 level statement).

    **What cancellation CAN be observed — the three links this witness
    asserts, in order.**

    1. Caller side: the awaiting task raises ``asyncio.CancelledError``
       promptly (the bridge drops the pull and fires ``mesh.cancel(token)``).
    2. Substrate side, LOCAL: the per-stream cancel watcher tears the call's
       pending entry down, so the response side EOFs — observed here as the
       ``async for`` ending — **while the caller handle is still alive**. This
       is the link that discriminates the cancel-token path from ordinary
       teardown: with no token threaded the drain would park until the 120 s
       call deadline.
    3. Provider side: retirement is observable as the handler's request input
       fencing to EOF (its ``for chunk in stream:`` ends with NO final item
       and no exception) — the shape's library-controlled input, closed by the
       §2.2 retire supervisor. **This link arrives with the WIRE CANCEL, which
       the substrate publishes from the caller handle's ``close()``/drop (its
       per-shape Drop contract — the stream-cancel watcher's teardown is local
       by design).** The witness therefore closes the handle before asserting
       it; ``task.cancel()`` sets the chain in motion, and the provider-side
       retirement observables land at handle teardown.

    **What cancellation CANNOT be observed — and what this witness does NOT
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
      but only as best-effort teardown machinery: the supervisor may drop the
      future without a final poll, and the coroutine may never be resumed to
      observe anything. Never rely on it for correctness.
    """
    sc = scenarios["same_org"]
    svc = "internal.cancel"
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

        handle = net.serve_org_duplex(pair.provider, svc, "same_org", handler, None)
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
                with pytest.raises(asyncio.CancelledError):
                    await pull

                # ---- link 2: the LOCAL teardown observable, handle ALIVE ----
                async def _drain() -> None:
                    async for _ in call:
                        pass

                # The token watcher already closed the call's response side;
                # this drain ends immediately. Without the cancel token it
                # would park until the 120 s deadline (bounded out at 5 s).
                await asyncio.wait_for(_drain(), 5)

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


# =========================================================================
# Midstream-error vocabulary — `org_err_to_py` on stream errors (§4.4).
# =========================================================================


@pytest.mark.timeout(600)
def test_streaming_midstream_error_surfaces_the_org_vocabulary(scenarios) -> None:
    """A midstream/terminal error on an org-opened handle classifies through
    ``org_err_to_py``: the ``org:`` wire vocabulary (the ``OrgError`` family),
    NOT the ``RpcError`` family a public ``MeshRpc.call_streaming`` raises
    (§4.4 — "midstream errors through `org_err_to_py`").

    The error here is deadline retirement arriving as the stream's final
    error — ``org:rpc:...`` per the facade contract — while an nRPC handle
    would raise ``RpcTimeoutError``.
    """
    sc = scenarios["same_org"]
    svc = "internal.slow"
    with _live_pair(sc) as pair:
        seen: dict = {}

        def handler(caller: dict, request: bytes, sink) -> None:
            seen["facts"] = caller
            sink.send(b"first")
            time.sleep(6)  # hold the call past the caller's 1.5 s deadline

        handle = net.serve_org_streaming(pair.provider, svc, "same_org", handler, None)
        client = _bind(sc, pair.caller)
        stream = None
        try:
            stream = _converge(
                pair, sc, svc, lambda: client.call_streaming(svc, b"hi", 1500)
            )
            assert next(stream) == b"first"
            with pytest.raises(net.OrgError) as ei:
                next(stream)  # deadline retirement = the stream's final error
        finally:
            if stream is not None:
                stream.close()
            client.close()
            handle.close()
        # The org vocabulary, classified where the facade contracts put it.
        assert not isinstance(ei.value, net.RpcError), (
            "org handles must not surface the nRPC exception family"
        )
        parsed = parse_org_error(str(ei.value))
        assert parsed.domain == "rpc", parsed
        assert str(ei.value).startswith("org:rpc:"), str(ei.value)
        _assert_facts(seen["facts"], sc)


# =========================================================================
# The original X2 cell — one live admitted cross-org UNARY call.
# =========================================================================


@pytest.mark.timeout(600)
def test_live_cross_org_call_from_a_generated_scenario(scenarios) -> None:
    sc = scenarios["granted"]
    svc = sc.manifest["granted_service"]
    with _live_pair(sc) as pair:
        seen: dict = {}

        def _handler(caller_facts: dict, request: bytes) -> bytes:
            seen["cross_org"] = (
                caller_facts["is_same_org"] is False
                and len(caller_facts["entity"]) == 32
            )
            body = json.loads(request.decode("utf-8"))
            return json.dumps({"n": body["n"] + 1, "servedBy": "py-provider"}).encode("utf-8")

        handle = net.serve_org(pair.provider, svc, "granted", _handler, None)
        client = _bind(sc, pair.caller)
        try:
            request = json.dumps({"n": 7}).encode("utf-8")
            reply = _converge(pair, sc, svc, lambda: client.call(svc, request))
        finally:
            client.close()
            handle.close()
        assert json.loads(reply.decode("utf-8")) == {"n": 8, "servedBy": "py-provider"}
        assert seen["cross_org"], "four-party attribution reached the handler"
