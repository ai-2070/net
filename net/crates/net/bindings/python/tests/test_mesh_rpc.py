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
    TypedMeshRpc,
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
    # Internal (0x0006), Backpressure (0x0004), server-Timeout (0x0003) → retry
    assert default_retryable(_err("RpcServerError", "status 0x0006: oops")) is True
    assert default_retryable(_err("RpcServerError", "status 0x0004: bp")) is True
    assert default_retryable(_err("RpcServerError", "status 0x0003: timeout")) is True
    # Application range (0x8000+) → skip
    assert default_retryable(_err("RpcServerError", "status 0x8001: app")) is False
    assert default_retryable(_err("RpcServerError", "status 0x8000: app")) is False
    # NotFound (0x0001), Unauthorized (0x0002) → skip
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
