"""Outbound HTTP-402 client binding tests (PAYMENTS_LANGUAGE_SDKS_PLAN WS-P2).

Build the extension with the opt-in ``payments-http`` feature::

    maturin develop --features net,cortex,consent,mcp,payments,payments-http,extension-module

These exercise the *binding contract*: construction from the payment kwargs
(the spend gate is the caller's own policy), and that ``fetch_paid`` returns a
``(status_json, body)`` tuple rather than raising for a payment outcome. The
payment *decisions* (probe -> 402 -> spend policy -> sign -> retry) are pinned
in Rust (net-payments ``http402_outbound`` + the binding's ``payment_http``
projection tests); the module skips cleanly when the feature is absent.
"""

from __future__ import annotations

import asyncio
import json

import pytest

pytest.importorskip("net._net")

import net as _net  # noqa: E402

PaymentHttpClient = _net.__dict__.get("PaymentHttpClient")
AsyncPaymentHttpClient = _net.__dict__.get("AsyncPaymentHttpClient")
if PaymentHttpClient is None:
    pytest.skip(
        "PaymentHttpClient not built (needs the payments-http feature)",
        allow_module_level=True,
    )

# A port that refuses connections, so the unpaid probe fails at the transport
# without any network dependency — the client projects `transport_error`.
UNREACHABLE = "http://127.0.0.1:1/nope"


@pytest.fixture()
def client(tmp_path):
    return PaymentHttpClient(
        payment_policy_path=str(tmp_path / "spend-policy.json"),
        payment_profile="dev_test",
    )


def test_construction_requires_a_policy_path(tmp_path):
    # payment_policy_path is the spend gate — it is a required positional.
    with pytest.raises(TypeError):
        PaymentHttpClient()  # type: ignore[call-arg]


def test_repr_round_trips(client):
    assert "PaymentHttpClient" in repr(client)


def test_fetch_paid_returns_a_status_json_and_body_tuple(client):
    status_json, body = client.fetch_paid(UNREACHABLE)
    parsed = json.loads(status_json)
    assert parsed["status"] == "transport_error"
    assert "error" in parsed
    assert isinstance(body, bytes)
    assert body == b""


def test_signer_pair_is_both_or_neither(tmp_path):
    policy = str(tmp_path / "spend-policy.json")
    with pytest.raises(ValueError):
        PaymentHttpClient(policy, payment_signer_address="0xpayer")
    with pytest.raises(ValueError):
        PaymentHttpClient(policy, payment_signer=lambda t: "0x")


def test_unknown_profile_is_a_config_error(tmp_path):
    with pytest.raises(ValueError):
        PaymentHttpClient(str(tmp_path / "spend-policy.json"), payment_profile="yolo")


def test_no_kwarg_accepts_key_material(tmp_path):
    """The key-invariant negative test: a private key is unrepresentable on
    this surface, exactly as on the gateway."""
    for kwarg in ("payment_private_key", "payment_secret", "payment_key_bytes"):
        with pytest.raises(TypeError):
            PaymentHttpClient(
                str(tmp_path / "spend-policy.json"),
                **{kwarg: b"\x11" * 32},
            )


def test_async_fetch_paid_returns_a_tuple(tmp_path):
    if AsyncPaymentHttpClient is None:
        pytest.skip("AsyncPaymentHttpClient not built")
    gw = AsyncPaymentHttpClient(
        payment_policy_path=str(tmp_path / "spend-policy.json"),
        payment_profile="dev_test",
    )

    async def body():
        return await gw.fetch_paid(UNREACHABLE)

    status_json, payload = asyncio.run(body())
    parsed = json.loads(status_json)
    assert parsed["status"] == "transport_error"
    assert isinstance(payload, bytes) and payload == b""
