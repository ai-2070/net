"""Cross-binding nRPC wire-format compat — Phase B7.

Loads the shared ``tests/cross_lang_nrpc/golden_vectors.json``
fixture (the same one driving the Rust + Node tests) and asserts
the canonical ``cross_lang_echo_sum`` service round-trips
correctly through the Python binding's ``TypedMeshRpc`` surface.

The handler is implemented inline (no live mesh required); the
test exercises the JSON codec + typed-error mapping by piping the
encoded request through a stub raw MeshRpc that runs the handler
in-process and returns the encoded response — same wire-format-
compat pattern documented at
``net/crates/net/README.md#nrpc``.
"""

from __future__ import annotations

import json
import re
from pathlib import Path
from typing import Any

import pytest

from net.mesh_rpc import (
    NRPC_TYPED_BAD_REQUEST,
    RpcServerError,
    TypedMeshRpc,
)

# ---------------------------------------------------------------------------
# Fixture
# ---------------------------------------------------------------------------

_FIXTURE_PATH = (
    Path(__file__).resolve().parents[3]
    / "tests"
    / "cross_lang_nrpc"
    / "golden_vectors.json"
)
with _FIXTURE_PATH.open("r", encoding="utf-8") as _fp:
    FIXTURE: dict[str, Any] = json.load(_fp)


# ---------------------------------------------------------------------------
# Canonical handler (Python implementation of the contract)
# ---------------------------------------------------------------------------


def _is_valid_request(value: Any) -> bool:
    if not isinstance(value, dict):
        return False
    if not isinstance(value.get("text"), str):
        return False
    nums = value.get("numbers")
    if not isinstance(nums, list):
        return False
    return all(isinstance(n, (int, float)) and not isinstance(n, bool) for n in nums)


def _handle_echo_sum(req: dict[str, Any]) -> dict[str, Any]:
    return {"echo": req["text"], "sum": sum(int(n) for n in req["numbers"])}


# ---------------------------------------------------------------------------
# Stub raw MeshRpc that loops back through the canonical handler.
# Mirrors the Node binding's `LoopbackHandlerRpc` — same shape so
# the two binding-side tests stay parallel.
# ---------------------------------------------------------------------------


class _LoopbackHandlerRpc:
    def call(
        self,
        target_node_id: int,  # noqa: ARG002
        service: str,
        req_bytes: bytes,
        opts: Any = None,  # noqa: ARG002
    ) -> bytes:
        return self._dispatch(service, req_bytes)

    def call_service(
        self,
        service: str,
        req_bytes: bytes,
        opts: Any = None,  # noqa: ARG002
    ) -> bytes:
        return self._dispatch(service, req_bytes)

    def call_streaming(self, *_: Any, **__: Any) -> Any:
        raise NotImplementedError("streaming not exercised by cross-lang compat")

    def serve(self, *_: Any, **__: Any) -> Any:
        raise NotImplementedError("serve not exercised by cross-lang compat")

    def find_service_nodes(self, _service: str) -> list[int]:
        return []

    def _dispatch(self, service: str, req_bytes: bytes) -> bytes:
        if service != FIXTURE["service"]:
            raise RpcServerError(f"nrpc:no_route: unknown service {service}")
        try:
            parsed = json.loads(req_bytes.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as e:
            raise RpcServerError(
                f"nrpc:server_error: status=0x{NRPC_TYPED_BAD_REQUEST:04x} "
                f"message=invalid_json: {e}"
            ) from e
        if not _is_valid_request(parsed):
            raise RpcServerError(
                f"nrpc:server_error: status=0x{NRPC_TYPED_BAD_REQUEST:04x} "
                "message=invalid_request_shape"
            )
        resp = _handle_echo_sum(parsed)
        return json.dumps(resp, separators=(",", ":")).encode("utf-8")


def _parse_status(msg: str) -> int | None:
    """Mirror of ``net.mesh_rpc._parse_status_from_message`` — extracts
    the status integer from an ``RpcServerError`` message string.
    Inlined here to keep the test independent of a private helper."""
    m = re.search(r"status\s*=?\s*0x([0-9a-fA-F]+)", msg)
    return int(m.group(1), 16) if m else None


# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------


def test_fixture_metadata_matches_canonical_contract() -> None:
    assert FIXTURE["service"] == "cross_lang_echo_sum"
    assert FIXTURE["abi_version_expected"] == 0x0001
    assert NRPC_TYPED_BAD_REQUEST == 0x8000
    assert len(FIXTURE["ok_cases"]) > 0
    assert len(FIXTURE["error_cases"]) > 0


@pytest.mark.parametrize("case", FIXTURE["ok_cases"], ids=lambda c: c["name"])
def test_ok_cases_round_trip_via_call(case: dict[str, Any]) -> None:
    rpc = TypedMeshRpc(_LoopbackHandlerRpc())
    reply = rpc.call(0, FIXTURE["service"], case["request_json"])
    assert reply == case["expected_response_json"]


@pytest.mark.parametrize("case", FIXTURE["ok_cases"], ids=lambda c: c["name"])
def test_ok_cases_round_trip_via_call_service(case: dict[str, Any]) -> None:
    rpc = TypedMeshRpc(_LoopbackHandlerRpc())
    reply = rpc.call_service(FIXTURE["service"], case["request_json"])
    assert reply == case["expected_response_json"]


@pytest.mark.parametrize("case", FIXTURE["error_cases"], ids=lambda c: c["name"])
def test_error_cases_surface_typed_bad_request(case: dict[str, Any]) -> None:
    rpc = TypedMeshRpc(_LoopbackHandlerRpc())
    with pytest.raises(Exception) as exc_info:  # noqa: BLE001 — capture any RpcError flavor
        rpc.call(0, FIXTURE["service"], case["request_json"])
    msg = str(exc_info.value)
    assert "nrpc:server_error" in msg, f"error-case '{case['name']}' message: {msg!r}"
    status = _parse_status(msg)
    assert status == case["expected_status"], (
        f"error-case '{case['name']}' status mismatch: "
        f"expected {case['expected_status']:#06x}, got {status}"
    )
