"""Pure-Python tests for the typed nRPC wrapper layer.

Exercises:
  - JSON codec round-trip + encode / decode failure
  - RetryPolicy backoff math (true ceiling, jitter range)
  - run_retry / default_retryable predicate behavior
  - run_hedge ordering (primary error wins deterministically)
  - CircuitBreaker state machine (Closed → Open → HalfOpen)

These tests don't require a live mesh or the rebuilt native
extension to exercise — they test ``net.mesh_rpc``'s pure-Python
surface against in-test stub objects.

End-to-end tests against a real mesh land alongside the cross-
language B7 integration coverage.
"""

from __future__ import annotations

import threading
import time

import pytest

# These tests exercise the pure-Python wrapper layer
# (`net.mesh_rpc`) WITHOUT needing the native module to have been
# rebuilt. The wrapper's predicates discriminate by
# ``type(err).__name__`` (a runtime string), so test-local
# stand-in classes with the right names do the job — see
# `_StandinRpc*Error` below.
#
# End-to-end tests that exercise the real bound MeshRpc class
# land alongside the cross-language B7 integration coverage and
# require a fresh `maturin develop` before they pass.

from net.mesh_rpc import (
    BreakerOpenError,
    Cancellable,
    CircuitBreaker,
    HedgePolicy,
    NRPC_TYPED_BAD_REQUEST,
    NRPC_TYPED_HANDLER_ERROR,
    RetryPolicy,
    RpcCodecError,
    RpcError,
    RpcNoRouteError,
    RpcServerError,
    RpcTimeoutError,
    RpcTransportError,
    RpcCallEvent,
    RpcCallStatusCanceled,
    RpcCallStatusError,
    RpcCallStatusOk,
    RpcCallStatusTimeout,
    RpcMetricsSnapshot,
    ServiceMetrics,
    TypedClientStreamCall,
    TypedDuplexCall,
    TypedDuplexSink,
    TypedDuplexStream,
    TypedMeshRpc,
    TypedRequestStream,
    TypedResponseSink,
    classify_error,
    default_breaker_failure,
    default_retryable,
    run_hedge,
    run_retry,
)

# Note: mesh_rpc.py re-exports the error classes from net._net
# when the cortex feature is compiled in; otherwise it exposes
# fallback aliases (all == Exception). The predicates discriminate
# by ``type(err).__name__``, so a fallback `Exception` instance
# won't be classified as e.g. RpcTimeoutError. To make the
# predicate tests work in BOTH paths, the test instantiates each
# error class via a small wrapper that injects the right
# `__class__.__name__` into the exception. The dataclass-style
# wrapper below works whether the real native class is present
# or whether mesh_rpc.py fell back to plain Exception.


def _err(name: str, message: str = "") -> Exception:
    """Build an exception whose ``type(...).__name__`` is exactly
    ``name``. When the native binding is available, we use it
    directly; otherwise we synthesize a one-off subclass so
    ``default_retryable`` can match by name."""
    cls = {
        "RpcError": RpcError,
        "RpcNoRouteError": RpcNoRouteError,
        "RpcTimeoutError": RpcTimeoutError,
        "RpcServerError": RpcServerError,
        "RpcTransportError": RpcTransportError,
        "RpcCodecError": RpcCodecError,
    }[name]
    if cls.__name__ == name:
        return cls(message)
    # Fallback path: synthesize a class with the right __name__.
    synth = type(name, (cls,), {})
    return synth(message)


# =========================================================================
# default_retryable / default_breaker_failure
# =========================================================================


def test_default_retryable_skips_no_route_and_codec() -> None:
    assert default_retryable(_err("RpcNoRouteError", "x")) is False
    assert default_retryable(_err("RpcCodecError", "x")) is False


def test_default_retryable_retries_timeout_and_transport() -> None:
    assert default_retryable(_err("RpcTimeoutError", "elapsed_ms=100")) is True
    assert default_retryable(_err("RpcTransportError", "x")) is True


def test_default_retryable_server_error_status_classification() -> None:
    # Internal (0x0006), Backpressure (0x0004), server-Timeout (0x0003) → retry.
    # Both legacy (`status 0x...`) and canonical (`status=0x...`)
    # message shapes parse the same way — the regex tolerates both
    # so a future formatter change doesn't silently break retry.
    assert default_retryable(_err("RpcServerError", "status 0x0006: oops")) is True
    assert default_retryable(_err("RpcServerError", "status=0x0006 message=oops")) is True
    assert default_retryable(_err("RpcServerError", "status 0x0004: bp")) is True
    assert default_retryable(_err("RpcServerError", "status=0x0004 message=bp")) is True
    assert default_retryable(_err("RpcServerError", "status 0x0003: timeout")) is True
    assert default_retryable(_err("RpcServerError", "status=0x0003 message=timeout")) is True
    # Application range (0x8000+) → skip.
    assert default_retryable(_err("RpcServerError", "status 0x8001: app")) is False
    assert default_retryable(_err("RpcServerError", "status=0x8001 message=app")) is False
    assert default_retryable(_err("RpcServerError", "status 0x8000: app")) is False
    # Canonical RPC binding format: `nrpc:server_error: status=0x... message=...`.
    assert (
        default_retryable(
            _err("RpcServerError", "nrpc:server_error: status=0x0006 message=oops")
        )
        is True
    )
    assert (
        default_retryable(
            _err("RpcServerError", "nrpc:server_error: status=0x8001 message=app")
        )
        is False
    )
    # NotFound (0x0001), Unauthorized (0x0002) → skip.
    assert default_retryable(_err("RpcServerError", "status 0x0001: nf")) is False
    assert default_retryable(_err("RpcServerError", "status 0x0002: u")) is False


def test_default_retryable_passes_through_plain_errors() -> None:
    assert default_retryable(RuntimeError("plain")) is False


def test_default_breaker_failure_matches_default_retryable() -> None:
    cases = [
        (_err("RpcNoRouteError", "x"), False),
        (_err("RpcTimeoutError", "x"), True),
        (_err("RpcCodecError", "x"), False),
        (_err("RpcServerError", "status 0x0006: x"), True),
    ]
    for err, expected in cases:
        assert default_breaker_failure(err) is expected, repr(err)


# =========================================================================
# classify_error — canonical nrpc: prefix dispatch
# =========================================================================


def test_classify_error_recognizes_each_canonical_kind() -> None:
    """Every kind produced by ``rpc_error_to_pyerr`` (Rust binding)
    or ``BreakerOpenError`` (Python wrapper) must be recognized.
    Drift here breaks every cross-binding fallback path that
    discriminates on kind without ``isinstance``.
    """
    cases = [
        ("nrpc:no_route: target=0x1 reason=x", "no_route"),
        ("nrpc:timeout: elapsed_ms=200", "timeout"),
        ("nrpc:server_error: status=0x8001 message=app", "server_error"),
        ("nrpc:transport: connection error: x", "transport"),
        ("nrpc:codec_encode: bad json", "codec_encode"),
        ("nrpc:codec_decode: trailing", "codec_decode"),
        ("nrpc:breaker_open: circuit breaker is open", "breaker_open"),
    ]
    for msg, kind in cases:
        assert classify_error(Exception(msg)) == kind, msg


def test_classify_error_returns_none_for_non_nrpc_messages() -> None:
    assert classify_error(Exception("plain runtime error")) is None
    assert classify_error(Exception("traversal: peer-not-reachable")) is None
    assert classify_error(Exception("nrpc:")) is None  # no body
    assert classify_error(Exception("nrpc:bogus_kind: foo")) is None


def test_breaker_open_error_carries_canonical_prefix() -> None:
    """``BreakerOpenError`` participates in the same kind-based
    dispatch by carrying the canonical ``nrpc:breaker_open:``
    prefix in its message — pinned because the Node binding's
    breaker emits the same prefix and a divergence would split
    cross-binding error handling."""
    err = BreakerOpenError()
    assert str(err).startswith("nrpc:breaker_open:")
    assert classify_error(err) == "breaker_open"


# =========================================================================
# RetryPolicy backoff math
# =========================================================================


def test_retry_policy_grows_exponentially_to_max() -> None:
    p = RetryPolicy(
        initial_backoff_ms=10,
        max_backoff_ms=100,
        backoff_multiplier=2.0,
        jitter=False,
    )
    assert p.compute_backoff_ms(1) == 10  # 10 * 2^0
    assert p.compute_backoff_ms(2) == 20
    assert p.compute_backoff_ms(3) == 40
    assert p.compute_backoff_ms(4) == 80
    assert p.compute_backoff_ms(5) == 100  # capped
    assert p.compute_backoff_ms(6) == 100


def test_retry_policy_jitter_keeps_within_cap() -> None:
    p = RetryPolicy(
        initial_backoff_ms=100,
        max_backoff_ms=100,
        backoff_multiplier=2.0,
        jitter=True,
    )
    # Sample many times — every result must lie in [50, 100]
    # (full-half jitter on a value already at the cap).
    for _ in range(50):
        ms = p.compute_backoff_ms(1)
        assert 50.0 <= ms <= 100.0


def test_retry_policy_clamps_invalid_inputs() -> None:
    p = RetryPolicy(max_attempts=0, initial_backoff_ms=-50, backoff_multiplier=0.5)
    assert p.max_attempts == 1
    assert p.initial_backoff_ms == 0
    assert p.backoff_multiplier == 1.0


# =========================================================================
# run_retry orchestration
# =========================================================================


def test_run_retry_succeeds_after_transient_failures() -> None:
    """Server fails the first 2 times then succeeds — wrapper
    absorbs the failures and surfaces the third call's success."""

    calls = [0]

    def op() -> str:
        calls[0] += 1
        if calls[0] <= 2:
            raise _err("RpcTimeoutError", f"failure #{calls[0]}")
        return "ok"

    p = RetryPolicy(max_attempts=5, initial_backoff_ms=1, jitter=False)
    result = run_retry(p, op)
    assert result == "ok"
    assert calls[0] == 3


def test_run_retry_does_not_retry_non_retryable() -> None:
    """A NoRoute failure is not retried — wrapper surfaces after one attempt."""

    calls = [0]

    def op() -> None:
        calls[0] += 1
        raise _err("RpcNoRouteError", "no session")

    p = RetryPolicy(max_attempts=5, initial_backoff_ms=1, jitter=False)
    with pytest.raises(Exception, match="no session"):
        run_retry(p, op)
    assert calls[0] == 1


def test_run_retry_exhausts_then_raises_last() -> None:
    calls = [0]

    def op() -> None:
        calls[0] += 1
        raise _err("RpcTimeoutError", f"attempt {calls[0]}")

    p = RetryPolicy(max_attempts=3, initial_backoff_ms=1, jitter=False)
    with pytest.raises(Exception, match="attempt 3"):
        run_retry(p, op)
    assert calls[0] == 3


# =========================================================================
# HedgePolicy / run_hedge
# =========================================================================


def test_hedge_empty_targets_raises_no_route() -> None:
    p = HedgePolicy()
    # `run_hedge` raises `RpcNoRouteError("hedge: empty targets list")`
    # (the class is the native one when cortex is built in;
    # otherwise the wrapper's fallback alias resolves to
    # plain Exception). Match by message; `"empty targets"` is
    # in the diagnostic regardless.
    with pytest.raises(Exception, match="empty targets"):
        run_hedge(p, [], lambda t: "x")


def test_hedge_zero_degrades_to_single_call() -> None:
    p = HedgePolicy(hedges=0)
    seen: list[int] = []

    def op(target: int) -> str:
        seen.append(target)
        return f"ok-{target}"

    result = run_hedge(p, [10, 20, 30], op)
    assert result == "ok-10"
    assert seen == [10]


def test_hedge_first_success_wins() -> None:
    """All targets succeed; the FIRST one to return wins."""
    p = HedgePolicy(delay_ms=0, hedges=2)

    barrier = threading.Barrier(3)
    return_order: list[int] = []
    lock = threading.Lock()

    def op(target: int) -> str:
        # All threads sync at the barrier so the race is genuine.
        barrier.wait()
        # Stagger the returns by target id so the test is deterministic
        # even though all three could theoretically tie.
        time.sleep(target / 1000.0)  # 10ms, 20ms, 30ms
        with lock:
            return_order.append(target)
        return f"ok-{target}"

    result = run_hedge(p, [10, 20, 30], op)
    assert result == "ok-10"  # smallest target sleeps shortest


def test_hedge_all_failing_surfaces_primary_error_deterministically() -> None:
    """When every target raises, the surfaced exception is the
    primary's (target index 0) regardless of completion order.
    Pin: 5 iterations to make the determinism check fail-stop loud."""

    def make_op(slow_primary: bool):
        def op(target: int) -> None:
            if target == 1 and slow_primary:
                # Make the primary slowest so naive last-completer
                # wins surfaces backup-error.
                time.sleep(0.05)
            raise _err(
                "RpcServerError", f"status 0x0006: error from target {target}"
            )

        return op

    p = HedgePolicy(delay_ms=10, hedges=1)
    for _ in range(5):
        with pytest.raises(Exception, match="error from target 1"):
            run_hedge(p, [1, 2], make_op(slow_primary=True))


# =========================================================================
# CircuitBreaker state machine
# =========================================================================


def _raise(err_name: str, msg: str = ""):
    """Helper: lambda-friendly raiser. Used in `b.call(...)`
    where lambdas can't directly contain a `raise` statement."""

    def _op():
        raise _err(err_name, msg)

    return _op


def test_breaker_starts_closed_trips_after_threshold() -> None:
    b = CircuitBreaker(failure_threshold=3, reset_after_ms=10_000)
    assert b.state() == "closed"
    for i in range(2):
        with pytest.raises(Exception):
            b.call(_raise("RpcTimeoutError", f"x{i}"))
        assert b.state() == "closed"
    # Third consecutive failure trips.
    with pytest.raises(Exception):
        b.call(_raise("RpcTimeoutError", "x3"))
    assert b.state() == "open"


def test_breaker_open_short_circuits_without_invoking_op() -> None:
    b = CircuitBreaker(failure_threshold=1, reset_after_ms=10_000)
    with pytest.raises(Exception):
        b.call(_raise("RpcTimeoutError", "x"))
    assert b.state() == "open"

    invoked = [False]

    def never_invoked() -> str:
        invoked[0] = True
        return "never"

    with pytest.raises(BreakerOpenError):
        b.call(never_invoked)
    assert invoked[0] is False


def test_breaker_recovers_through_half_open_after_cooldown() -> None:
    b = CircuitBreaker(
        failure_threshold=1, reset_after_ms=10, success_threshold=1,
    )
    with pytest.raises(Exception):
        b.call(_raise("RpcTimeoutError", "x"))
    assert b.state() == "open"
    time.sleep(0.025)
    # Next call probes successfully → state closes.
    result = b.call(lambda: "recovered")
    assert result == "recovered"
    assert b.state() == "closed"


def test_breaker_application_errors_do_not_trip() -> None:
    b = CircuitBreaker(failure_threshold=2)
    # Application errors (0x8001) are NOT in defaultBreakerFailure
    # → 5 of them in a row leave state closed.
    for i in range(5):
        with pytest.raises(Exception):
            b.call(_raise("RpcServerError", f"status 0x8001: app{i}"))
    assert b.state() == "closed"
    assert b.consecutive_failures() == 0


def test_breaker_reset_clears_state() -> None:
    b = CircuitBreaker(failure_threshold=1)
    with pytest.raises(Exception):
        b.call(_raise("RpcTimeoutError", "x"))
    assert b.state() == "open"
    b.reset()
    assert b.state() == "closed"
    assert b.consecutive_failures() == 0


# =========================================================================
# JSON codec — exercised via TypedMeshRpc against a stub raw MeshRpc
# =========================================================================


class _StubRawMeshRpc:
    """Stub that mirrors the napi MeshRpc surface for testing the
    typed wrapper without a live native binding."""

    def __init__(self, response_bytes: bytes) -> None:
        self.response_bytes = response_bytes
        self.last_request: bytes | None = None

    def call(
        self,
        target: int,
        service: str,
        request: bytes,
        opts: dict | None = None,
    ) -> bytes:
        self.last_request = request
        return self.response_bytes

    def call_service(
        self, service: str, request: bytes, opts: dict | None = None
    ) -> bytes:
        self.last_request = request
        return self.response_bytes

    def call_streaming(
        self,
        target: int,
        service: str,
        request: bytes,
        opts: dict | None = None,
    ) -> object:
        raise NotImplementedError

    def serve(self, service: str, handler) -> object:
        raise NotImplementedError

    def find_service_nodes(self, service: str) -> list[int]:
        return []


def test_typed_call_round_trip() -> None:
    import json

    stub = _StubRawMeshRpc(json.dumps({"pong": 42}).encode("utf-8"))
    rpc = TypedMeshRpc(stub)
    reply = rpc.call(0, "echo", {"ping": "hi"})
    assert reply == {"pong": 42}
    assert stub.last_request == json.dumps({"ping": "hi"}, separators=(",", ":")).encode(
        "utf-8"
    )


def test_typed_call_encode_failure_raises_codec_error() -> None:
    stub = _StubRawMeshRpc(b"null")
    rpc = TypedMeshRpc(stub)

    class NotJsonable:
        pass

    with pytest.raises(RpcCodecError):
        rpc.call(0, "echo", NotJsonable())


def test_typed_call_decode_failure_raises_codec_error() -> None:
    stub = _StubRawMeshRpc(b"{not json")  # malformed
    rpc = TypedMeshRpc(stub)
    with pytest.raises(RpcCodecError):
        rpc.call(0, "echo", {"x": 1})


def test_status_constants_are_stable() -> None:
    assert NRPC_TYPED_BAD_REQUEST == 0x8000
    assert NRPC_TYPED_HANDLER_ERROR == 0x8001


# =========================================================================
# TypedMeshRpc.serve — typed-bad-request path
# =========================================================================


class _CapturingServeStub:
    """Stub that captures the inner handler ``TypedMeshRpc.serve``
    passes down to the raw layer, so we can drive the wrapper's
    decode-failure path directly without a live binding."""

    def __init__(self) -> None:
        self.inner = None

    def serve(self, service: str, handler) -> object:  # noqa: ARG002
        self.inner = handler
        return object()


def test_typed_serve_decode_failure_raises_app_error_with_bad_request_status() -> None:
    """Regression: a malformed request body must surface as a
    canonical typed-bad-request — ``RpcAppError(NRPC_TYPED_BAD_REQUEST,
    body)`` — NOT a generic ``RuntimeError`` that the binding
    squashes into ``RpcStatus::Internal``. Pinned because the cross-
    binding compat fixture (``golden_vectors.json``) and the Rust
    integration test ``cross_lang_error_cases_surface_typed_bad_request``
    both assert ``Application(0x8000)`` on this path.
    """
    from net.mesh_rpc import RpcAppError

    stub = _CapturingServeStub()
    rpc = TypedMeshRpc(stub)

    def handler(_req):  # pragma: no cover — not reached for malformed input
        return {"unused": True}

    rpc.serve("echo_sum", handler)
    assert stub.inner is not None

    with pytest.raises(RpcAppError) as exc_info:
        stub.inner(b"{not json")

    err = exc_info.value
    assert err.args[0] == NRPC_TYPED_BAD_REQUEST, (
        "decode-failure must signal NRPC_TYPED_BAD_REQUEST (0x8000)"
    )
    body = err.args[1]
    assert isinstance(body, (bytes, bytearray)), "body must be bytes for Rust to wire-encode"
    import json as _json
    decoded = _json.loads(bytes(body).decode("utf-8"))
    assert decoded["error"] == "invalid_request"
    assert "detail" in decoded


# =========================================================================
# Cancellable — caller-side cancel token
# =========================================================================


def test_cancellable_starts_uncancelled_and_latches_on_cancel() -> None:
    """The pure-Python fallback (and the native class — same API)
    starts unset, flips to cancelled on `cancel()`, and is
    idempotent. Pinned because the Cancellable participates in
    cross-binding cancellation tests."""
    c = Cancellable()
    assert c.is_cancelled() is False
    c.cancel()
    assert c.is_cancelled() is True
    # Idempotent
    c.cancel()
    assert c.is_cancelled() is True


def test_classify_error_recognizes_cancelled_kind() -> None:
    """Regression: the cancelled kind is part of the canonical
    set; classify_error must dispatch on it. Drift breaks every
    cross-binding fallback path that wants to special-case
    user-driven cancellation."""
    assert (
        classify_error(Exception("nrpc:cancelled: call cancelled by caller"))
        == "cancelled"
    )


def test_typed_serve_handler_runtime_exception_still_surfaces_as_internal() -> None:
    """Sanity: a handler that raises a plain exception is NOT
    coerced to RpcAppError — only decode failures are. The Rust
    side maps the un-RpcAppError exception to RpcStatus::Internal
    (the historical behavior; pinned so a future "everything maps
    to AppError" regression is loud).
    """
    stub = _CapturingServeStub()
    rpc = TypedMeshRpc(stub)

    def handler(_req):
        raise RuntimeError("boom")

    rpc.serve("echo", handler)
    assert stub.inner is not None
    with pytest.raises(RuntimeError, match="boom"):
        stub.inner(b'{"any": "valid_json"}')


# =========================================================================
# TypedClientStreamCall + TypedRequestStream (S2-C1) — stub-level
# round-trip + encode/decode failure coverage. Live tests against a
# real MeshRpc belong in S2-X (cross-language).
# =========================================================================


class _StubRawClientStreamCall:
    """Stub mirroring the pyo3 ``ClientStreamCall`` surface."""

    def __init__(self, finish_response: bytes) -> None:
        self.finish_response = finish_response
        self.sent: list[bytes] = []
        self.closed = False
        self._call_id = 7
        self._flow_controlled = False

    def send(self, body: bytes) -> None:
        self.sent.append(bytes(body))

    def finish(self) -> bytes:
        return self.finish_response

    def call_id(self) -> int:
        return self._call_id

    def flow_controlled(self) -> bool:
        return self._flow_controlled

    def close(self) -> None:
        self.closed = True


def test_typed_client_stream_round_trip() -> None:
    """Round-trip: three typed sends + a decoded terminal response."""
    import json as _json

    raw = _StubRawClientStreamCall(_json.dumps({"sum": 6}).encode("utf-8"))
    call = TypedClientStreamCall(raw)
    call.send({"n": 1})
    call.send({"n": 2})
    call.send({"n": 3})
    assert call.finish() == {"sum": 6}
    assert [bytes(b).decode("utf-8") for b in raw.sent] == [
        '{"n":1}',
        '{"n":2}',
        '{"n":3}',
    ]


def test_typed_client_stream_encode_failure_raises_codec_error() -> None:
    raw = _StubRawClientStreamCall(b"null")
    call = TypedClientStreamCall(raw)

    class NotJsonable:
        pass

    with pytest.raises(RpcCodecError):
        call.send(NotJsonable())
    # Encode happens before reaching the wire — no chunk sent.
    assert raw.sent == []


def test_typed_client_stream_finish_decode_failure_raises_codec_error() -> None:
    raw = _StubRawClientStreamCall(b"{not json")
    call = TypedClientStreamCall(raw)
    with pytest.raises(RpcCodecError):
        call.finish()


def test_typed_client_stream_context_manager_calls_close() -> None:
    raw = _StubRawClientStreamCall(b"null")
    with TypedClientStreamCall(raw):
        pass
    assert raw.closed is True


def test_typed_client_stream_close_swallows_underlying_errors() -> None:
    """``close()`` is a best-effort cleanup — a raw layer that
    raises must NOT propagate; matches TypedRpcStream.close.
    """

    class _RaisingClose(_StubRawClientStreamCall):
        def close(self) -> None:
            raise RuntimeError("boom")

    call = TypedClientStreamCall(_RaisingClose(b"null"))
    call.close()  # must not raise
    call.close()  # idempotent


class _StubRawRequestStream:
    """Stub mirroring the pyo3 ``RequestStreamRecv`` surface —
    iterator protocol + diagnostic getters.
    """

    def __init__(
        self,
        chunks: list[bytes],
        *,
        caller_origin: int = 0xFEEDFACE,
        call_id: int = 42,
        deadline_ns: int = 0,
        headers: list | None = None,
    ) -> None:
        self._chunks = list(chunks)
        self._idx = 0
        self.caller_origin = caller_origin
        self.call_id = call_id
        self.deadline_ns = deadline_ns
        self.headers = headers if headers is not None else []

    def __iter__(self):
        return self

    def __next__(self) -> bytes:
        if self._idx >= len(self._chunks):
            raise StopIteration
        chunk = self._chunks[self._idx]
        self._idx += 1
        return chunk


def test_typed_request_stream_decodes_chunks_until_eof() -> None:
    raw = _StubRawRequestStream(
        [b'{"n":1}', b'{"n":2}', b'{"n":3}'],
    )
    stream = TypedRequestStream(raw)
    decoded = [item["n"] for item in stream]
    assert decoded == [1, 2, 3]


def test_typed_request_stream_decode_failure_marks_done() -> None:
    raw = _StubRawRequestStream([b'{not json'])
    stream = TypedRequestStream(raw)
    with pytest.raises(RpcCodecError):
        next(stream)
    # Subsequent next() returns StopIteration (no re-throw) — the
    # stream is marked done; refuse to continue draining broken
    # framing.
    with pytest.raises(StopIteration):
        next(stream)


def test_typed_request_stream_exposes_diagnostic_getters() -> None:
    raw = _StubRawRequestStream(
        [],
        caller_origin=0xDEADBEEF,
        call_id=99,
        deadline_ns=1_700_000_000_000_000_000,
        headers=[("x-trace", b"abc")],
    )
    stream = TypedRequestStream(raw)
    assert stream.caller_origin == 0xDEADBEEF
    assert stream.call_id == 99
    assert stream.deadline_ns == 1_700_000_000_000_000_000
    assert stream.headers == [("x-trace", b"abc")]


class _CapturingClientStreamRpc:
    """Stub that captures ``serve_client_stream`` / ``call_client_stream``
    so we can drive the wrapper without a live binding.
    """

    def __init__(self) -> None:
        self.inner_handler = None
        self.call_args: tuple | None = None
        self.returned_call: _StubRawClientStreamCall | None = None

    def serve_client_stream(self, service: str, handler) -> object:  # noqa: ARG002
        self.inner_handler = handler
        return object()

    def call_client_stream(
        self,
        target: int,
        service: str,
        opts: dict | None,
    ) -> _StubRawClientStreamCall:
        self.call_args = (target, service, opts)
        self.returned_call = _StubRawClientStreamCall(b"null")
        return self.returned_call


def test_typed_serve_client_stream_decodes_and_encodes_round_trip() -> None:
    stub = _CapturingClientStreamRpc()
    rpc = TypedMeshRpc(stub)
    rpc.serve_client_stream("sum", lambda stream: {"sum": sum(r["n"] for r in stream)})
    assert stub.inner_handler is not None
    # Synthesize a raw request stream and run the wrapper's
    # installed inner handler against it. Result should be the
    # JSON-encoded terminal sum.
    raw_stream = _StubRawRequestStream([b'{"n":10}', b'{"n":20}', b'{"n":12}'])
    resp = stub.inner_handler(raw_stream)
    import json as _json

    assert _json.loads(resp.decode("utf-8")) == {"sum": 42}


def test_typed_call_client_stream_wraps_raw_call() -> None:
    stub = _CapturingClientStreamRpc()
    rpc = TypedMeshRpc(stub)
    call = rpc.call_client_stream(0xCAFE, "ingest", {"deadline_ms": 1000})
    assert isinstance(call, TypedClientStreamCall)
    assert stub.call_args == (0xCAFE, "ingest", {"deadline_ms": 1000})
    # Round-trip through the returned typed call to confirm wiring.
    call.send({"k": "v"})
    assert stub.returned_call is not None
    assert stub.returned_call.sent[0].decode("utf-8") == '{"k":"v"}'


# =========================================================================
# TypedDuplexCall / TypedDuplexSink / TypedDuplexStream /
# TypedResponseSink (S2-C2) — stub-level round-trip + split-halves
# coverage. Live tests against a real MeshRpc belong in S2-X.
# =========================================================================


class _StubRawResponseSink:
    def __init__(self) -> None:
        self.sent: list[bytes] = []
        self.closed = False

    def send(self, body: bytes) -> bool:
        if self.closed:
            return False
        self.sent.append(bytes(body))
        return True


class _StubRawDuplexSink:
    def __init__(self) -> None:
        self.sent: list[bytes] = []
        self.finished = False
        self.closed = False

    def send(self, body: bytes) -> None:
        self.sent.append(bytes(body))

    def finish(self) -> None:
        self.finished = True

    def call_id(self) -> int:
        return 11

    def flow_controlled(self) -> bool:
        return False

    def close(self) -> None:
        self.closed = True


class _StubRawDuplexStream:
    def __init__(self, chunks: list[bytes]) -> None:
        self._chunks = list(chunks)
        self._idx = 0
        self.closed = False

    def __iter__(self):
        return self

    def __next__(self) -> bytes:
        if self._idx >= len(self._chunks):
            raise StopIteration
        chunk = self._chunks[self._idx]
        self._idx += 1
        return chunk

    def call_id(self) -> int:
        return 12

    def close(self) -> None:
        self.closed = True


class _StubRawDuplexCall:
    def __init__(self, stream: _StubRawDuplexStream) -> None:
        self.sent: list[bytes] = []
        self.finished_sending = False
        self.closed = False
        self.sink: _StubRawDuplexSink | None = None
        self.stream = stream

    def send(self, body: bytes) -> None:
        self.sent.append(bytes(body))

    def finish_sending(self) -> None:
        self.finished_sending = True

    def __iter__(self):
        return self

    def __next__(self) -> bytes:
        return next(self.stream)

    def into_split(self):
        sink = _StubRawDuplexSink()
        self.sink = sink
        return sink, self.stream

    def call_id(self) -> int:
        return 13

    def flow_controlled(self) -> bool:
        return False

    def close(self) -> None:
        self.closed = True


def test_typed_duplex_round_trip() -> None:
    """Round-trip: typed sends + typed responses interleaved."""
    stream = _StubRawDuplexStream([b'{"r":"a"}', b'{"r":"b"}'])
    raw = _StubRawDuplexCall(stream)
    call = TypedDuplexCall(raw)
    call.send({"q": 1})
    call.send({"q": 2})
    call.finish_sending()
    collected = [item["r"] for item in call]
    assert collected == ["a", "b"]
    assert [b.decode("utf-8") for b in raw.sent] == ['{"q":1}', '{"q":2}']
    assert raw.finished_sending is True


def test_typed_duplex_decode_failure_closes_call() -> None:
    stream = _StubRawDuplexStream([b'{not json'])
    raw = _StubRawDuplexCall(stream)
    call = TypedDuplexCall(raw)
    with pytest.raises(RpcCodecError):
        next(call)
    assert raw.closed is True
    # Subsequent next() returns StopIteration (no re-throw).
    with pytest.raises(StopIteration):
        next(call)


def test_typed_duplex_into_split_yields_typed_halves() -> None:
    stream = _StubRawDuplexStream([b'{"r":"x"}', b'{"r":"y"}'])
    raw = _StubRawDuplexCall(stream)
    call = TypedDuplexCall(raw)
    sink, recv = call.into_split()
    assert isinstance(sink, TypedDuplexSink)
    assert isinstance(recv, TypedDuplexStream)
    sink.send({"q": 7})
    sink.finish()
    assert raw.sink is not None
    assert raw.sink.sent[0].decode("utf-8") == '{"q":7}'
    assert raw.sink.finished is True
    collected = [item["r"] for item in recv]
    assert collected == ["x", "y"]
    # The original call is consumed after into_split.
    with pytest.raises(StopIteration):
        next(call)


def test_typed_duplex_close_idempotent_swallows_errors() -> None:
    class _RaisingClose(_StubRawDuplexCall):
        def close(self) -> None:
            raise RuntimeError("boom")

    call = TypedDuplexCall(_RaisingClose(_StubRawDuplexStream([])))
    call.close()  # must not raise
    call.close()  # idempotent


def test_typed_response_sink_round_trip() -> None:
    raw = _StubRawResponseSink()
    sink = TypedResponseSink(raw)
    assert sink.send({"r": 1}) is True
    assert sink.send({"r": 2}) is True
    assert [b.decode("utf-8") for b in raw.sent] == ['{"r":1}', '{"r":2}']


def test_typed_response_sink_returns_false_when_closed() -> None:
    raw = _StubRawResponseSink()
    raw.closed = True
    sink = TypedResponseSink(raw)
    assert sink.send({"r": 1}) is False
    assert raw.sent == []


def test_typed_response_sink_encode_failure_does_not_enqueue() -> None:
    raw = _StubRawResponseSink()
    sink = TypedResponseSink(raw)

    class NotJsonable:
        pass

    with pytest.raises(RpcCodecError):
        sink.send(NotJsonable())
    assert raw.sent == []


class _CapturingDuplexRpc:
    def __init__(self) -> None:
        self.inner_handler = None

    def serve_duplex(self, service: str, handler) -> object:  # noqa: ARG002
        self.inner_handler = handler
        return object()


def test_typed_serve_duplex_destructures_stream_and_sink() -> None:
    stub = _CapturingDuplexRpc()
    rpc = TypedMeshRpc(stub)

    observed: dict = {}

    def handler(stream: TypedRequestStream, sink: TypedResponseSink) -> None:
        reqs = []
        for req in stream:
            reqs.append(req["q"])
            sink.send({"r": f"echo:{req['q']}"})
        observed["reqs"] = reqs

    rpc.serve_duplex("echo", handler)
    assert stub.inner_handler is not None

    raw_stream = _StubRawRequestStream([b'{"q":1}', b'{"q":2}', b'{"q":3}'])
    raw_sink = _StubRawResponseSink()
    result = stub.inner_handler(raw_stream, raw_sink)
    assert result is None
    assert observed["reqs"] == [1, 2, 3]
    assert [b.decode("utf-8") for b in raw_sink.sent] == [
        '{"r":"echo:1"}',
        '{"r":"echo:2"}',
        '{"r":"echo:3"}',
    ]


# =========================================================================
# TypedMeshRpc.set_observer + metrics_snapshot (S2-C3) — stub-level
# normalization + forwarding tests. Live observer firing belongs in S2-X.
# =========================================================================


class _RawEvent:
    """Stand-in for the pyo3 ``RpcCallEvent`` POD with flat
    ``status_kind`` / ``status_message`` fields.
    """

    def __init__(
        self,
        *,
        caller: int = 0x1,
        callee: int = 0x2,
        method: str = "echo",
        latency_ms: int = 7,
        status_kind: str = "ok",
        status_message: str | None = None,
        request_bytes: int = 10,
        response_bytes: int = 20,
        direction: str = "outbound",
        ts_unix_ms: int = 1_000_000,
    ) -> None:
        self.caller = caller
        self.callee = callee
        self.method = method
        self.latency_ms = latency_ms
        self.status_kind = status_kind
        self.status_message = status_message
        self.request_bytes = request_bytes
        self.response_bytes = response_bytes
        self.direction = direction
        self.ts_unix_ms = ts_unix_ms


def test_typed_event_ok_status() -> None:
    from net.mesh_rpc import _raw_event_to_typed

    evt = _raw_event_to_typed(_RawEvent(status_kind="ok"))
    assert isinstance(evt, RpcCallEvent)
    assert isinstance(evt.status, RpcCallStatusOk)


def test_typed_event_error_status_carries_message() -> None:
    from net.mesh_rpc import _raw_event_to_typed

    evt = _raw_event_to_typed(
        _RawEvent(status_kind="error", status_message="connection lost"),
    )
    assert isinstance(evt.status, RpcCallStatusError)
    assert evt.status.message == "connection lost"


def test_typed_event_error_with_no_message_defaults_to_empty() -> None:
    from net.mesh_rpc import _raw_event_to_typed

    evt = _raw_event_to_typed(_RawEvent(status_kind="error"))
    assert isinstance(evt.status, RpcCallStatusError)
    assert evt.status.message == ""


def test_typed_event_timeout_and_canceled_bare_tags() -> None:
    from net.mesh_rpc import _raw_event_to_typed

    t = _raw_event_to_typed(_RawEvent(status_kind="timeout"))
    c = _raw_event_to_typed(_RawEvent(status_kind="canceled"))
    assert isinstance(t.status, RpcCallStatusTimeout)
    assert isinstance(c.status, RpcCallStatusCanceled)


def test_typed_event_preserves_other_fields() -> None:
    from net.mesh_rpc import _raw_event_to_typed

    evt = _raw_event_to_typed(
        _RawEvent(
            caller=0xAA00,
            callee=0xBB00,
            method="svc.foo",
            latency_ms=3,
            request_bytes=8,
            response_bytes=4,
            direction="outbound",
            ts_unix_ms=1234,
        ),
    )
    assert evt.caller == 0xAA00
    assert evt.callee == 0xBB00
    assert evt.method == "svc.foo"
    assert evt.latency_ms == 3
    assert evt.request_bytes == 8
    assert evt.response_bytes == 4
    assert evt.direction == "outbound"
    assert evt.ts_unix_ms == 1234


class _CapturingObserverRpc:
    def __init__(self) -> None:
        self.installed = None

    def set_observer(self, callable_or_none) -> None:
        self.installed = callable_or_none


def test_typed_set_observer_decodes_events_and_forwards() -> None:
    stub = _CapturingObserverRpc()
    rpc = TypedMeshRpc(stub)
    seen: list = []
    rpc.set_observer(lambda evt: seen.append(evt))
    assert stub.installed is not None
    stub.installed(
        _RawEvent(
            method="svc.foo",
            status_kind="error",
            status_message="no_route",
        ),
    )
    assert len(seen) == 1
    assert seen[0].method == "svc.foo"
    assert isinstance(seen[0].status, RpcCallStatusError)
    assert seen[0].status.message == "no_route"


def test_typed_set_observer_none_clears_raw_observer() -> None:
    stub = _CapturingObserverRpc()
    rpc = TypedMeshRpc(stub)
    rpc.set_observer(lambda evt: None)
    assert stub.installed is not None
    rpc.set_observer(None)
    assert stub.installed is None


class _RawServiceMetrics:
    """Stand-in for the pyo3 ``ServiceMetrics`` POD."""

    def __init__(self) -> None:
        self.service = "echo"
        self.calls_total = 42
        self.errors_no_route = 0
        self.errors_timeout = 1
        self.errors_server = 0
        self.errors_transport = 0
        self.in_flight = 0
        self.latency_sum_ns = 1234567
        self.latency_count = 42
        self.latency_buckets = [10, 22, 30]
        self.handler_invocations_total = 0
        self.handler_panics_total = 0
        self.handler_in_flight = 0
        self.handler_duration_sum_ns = 0
        self.handler_duration_count = 0
        self.handler_duration_buckets = [0, 0, 0]
        self.streaming_chunks_emitted_total = 0
        self.streaming_chunks_dropped_total = 0
        self.capability_denied_total = 0


class _RawSnapshot:
    def __init__(self, services: list) -> None:
        self.services = services


class _CapturingMetricsRpc:
    def __init__(self, snapshot: _RawSnapshot) -> None:
        self._snapshot = snapshot

    def metrics_snapshot(self) -> _RawSnapshot:
        return self._snapshot


def test_typed_metrics_snapshot_decodes_raw_to_dataclass() -> None:
    raw = _RawSnapshot([_RawServiceMetrics()])
    stub = _CapturingMetricsRpc(raw)
    rpc = TypedMeshRpc(stub)
    snapshot = rpc.metrics_snapshot()
    assert isinstance(snapshot, RpcMetricsSnapshot)
    assert len(snapshot.services) == 1
    svc = snapshot.services[0]
    assert isinstance(svc, ServiceMetrics)
    assert svc.service == "echo"
    assert svc.calls_total == 42
    assert svc.errors_timeout == 1
    assert svc.latency_sum_ns == 1234567
    assert svc.latency_buckets == [10, 22, 30]


def test_typed_metrics_snapshot_empty_services() -> None:
    """No services iterated since the mesh was created → empty list,
    still wrapped in the typed dataclass.
    """
    stub = _CapturingMetricsRpc(_RawSnapshot([]))
    rpc = TypedMeshRpc(stub)
    snapshot = rpc.metrics_snapshot()
    assert isinstance(snapshot, RpcMetricsSnapshot)
    assert snapshot.services == []
