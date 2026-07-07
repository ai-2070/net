"""Cross-language payments golden vectors (PAYMENTS_IMPLEMENTATION_PLAN.md
Workstream 1).

Loads ``crates/net/tests/cross_lang_payments/payment_vectors.json`` — the
fixture the Rust source-of-truth verifier
(``payments/tests/payments_golden_vectors.rs``) validates — and asserts the
canonical-encoding regime holds byte-identically from Python:

- canonical form: one JSON object, all keys sorted bytewise, compact
  separators, raw UTF-8 (``ensure_ascii=False``), integers only
- signed payload = canonical form with the top-level ``signature`` key
  absent; ed25519 over those exact bytes (when ``cryptography`` is
  available)
- x402 documents ride as base64 of their preserved original bytes — the
  captured v2 fixtures must survive untouched

CAIP / amount / decimals grammar tables are enforced by the Rust verifier
(the grammar lives in the Rust core; no payments binding exists yet —
logic never lives in bindings, so nothing is re-implemented here).
"""

from __future__ import annotations

import base64
import json
from pathlib import Path

import pytest

FIXTURE_DIR = (
    Path(__file__).resolve().parent.parent.parent.parent / "tests" / "cross_lang_payments"
)
FIXTURE = json.loads((FIXTURE_DIR / "payment_vectors.json").read_text(encoding="utf-8"))

ENVELOPES = FIXTURE["envelopes"]
PRESERVATION = FIXTURE["x402_byte_preservation"]


def canonicalize(value) -> str:
    """The payments canonical writer: sorted keys, compact, raw UTF-8.

    Floats are a schema bug in the money path — reject, never encode.
    """
    _assert_no_floats(value)
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def _assert_no_floats(value) -> None:
    if isinstance(value, float):
        raise ValueError(f"float in envelope: {value}")
    if isinstance(value, bool) or value is None or isinstance(value, (int, str)):
        return
    if isinstance(value, list):
        for item in value:
            _assert_no_floats(item)
        return
    if isinstance(value, dict):
        for item in value.values():
            _assert_no_floats(item)
        return
    raise TypeError(f"unexpected type in envelope: {type(value)!r}")


@pytest.mark.parametrize("env", ENVELOPES, ids=[e["name"] for e in ENVELOPES])
def test_canonical_emission_is_a_fixed_point(env):
    parsed = json.loads(env["canonical"])
    assert canonicalize(parsed) == env["canonical"]


@pytest.mark.parametrize(
    "env",
    [e for e in ENVELOPES if e["signature_hex"] is not None],
    ids=[e["name"] for e in ENVELOPES if e["signature_hex"] is not None],
)
def test_signed_payload_derives_by_dropping_the_signature_key(env):
    parsed = json.loads(env["canonical"])
    parsed.pop("signature", None)
    assert canonicalize(parsed) == env["signed_payload"]


@pytest.mark.parametrize(
    "env",
    [e for e in ENVELOPES if e["signature_hex"] is not None],
    ids=[e["name"] for e in ENVELOPES if e["signature_hex"] is not None],
)
def test_ed25519_signatures_verify(env):
    ed25519 = pytest.importorskip(
        "cryptography.hazmat.primitives.asymmetric.ed25519",
        reason="signature checks need the `cryptography` package; canonical checks above still ran",
    )
    key = ed25519.Ed25519PublicKey.from_public_bytes(bytes.fromhex(env["signer_hex"]))
    key.verify(
        bytes.fromhex(env["signature_hex"]),
        env["signed_payload"].encode("utf-8"),
    )  # raises on mismatch
    with pytest.raises(Exception):
        key.verify(
            bytes.fromhex(env["signature_hex"]),
            (env["signed_payload"] + " ").encode("utf-8"),
        )


@pytest.mark.parametrize("p", PRESERVATION, ids=[p["name"] for p in PRESERVATION])
def test_captured_x402_fixtures_survive_untouched(p):
    file_bytes = (FIXTURE_DIR / Path(*p["file"].split("/"))).read_bytes()
    assert base64.b64decode(p["base64"]) == file_bytes
    assert base64.b64encode(file_bytes).decode("ascii") == p["base64"]

    if p["embedded_in"] is not None:
        env = next(e for e in ENVELOPES if e["name"] == p["embedded_in"])
        parsed = json.loads(env["canonical"])
        assert parsed[p["envelope_field"]] == p["base64"]


def test_floats_are_rejected_by_the_canonical_writer():
    with pytest.raises(ValueError):
        canonicalize({"price": 1.5})


def test_fixture_names_are_unique():
    names = [e["name"] for e in ENVELOPES]
    assert len(names) == len(set(names))
