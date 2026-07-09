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
FAILURE = FIXTURE["failure_schematic_vectors"]
FAILURE_TAG = FAILURE["tag"]
FAILURE_CASES = FAILURE["cases"]


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


def _failure_header_bytes(case) -> bytes:
    if "header_utf8" in case:
        return case["header_utf8"].encode("utf-8")
    return base64.b64decode(case["header_base64"])


# The required-field shape a `FailureSchematic` deserializes into (its
# non-optional fields). `quote_id` / `tool_id` / `recovery.next_action` are
# ``Option<String>`` and extra keys ride ``#[serde(flatten)]``.
_REQUIRED_STR = ("object", "code", "stage", "reason", "message", "funds_moved", "prior_payment")
_REQUIRED_BOOL = ("retryable", "handler_executed")
_RECOVERY_STR = ("class", "actor")
_RECOVERY_BOOL = ("safe_to_retry", "safe_to_requote")


def _optional_str_ok(obj, key) -> bool:
    """An ``Option<String>`` field: absent or JSON ``null`` (both -> ``None``)
    deserializes to ``None``; any other present type fails the typed serde
    deserialize, so a *present* optional is still type-checked."""
    v = obj.get(key)
    return v is None or isinstance(v, str)


def _has_schematic_shape(obj) -> bool:
    """Presence + JSON type of every required field, plus the type of every
    present optional — the structural half of ``from_header_bytes`` (a full
    typed serde deserialize). A tag-only, mistyped-required, or mistyped-optional
    object does NOT deserialize, so it is not accepted. (``bool`` is a subclass
    of ``int`` in Python but not of ``str``, so the checks don't cross-accept.)"""
    if not all(isinstance(obj.get(k), str) for k in _REQUIRED_STR):
        return False
    if not all(isinstance(obj.get(k), bool) for k in _REQUIRED_BOOL):
        return False
    if not all(_optional_str_ok(obj, k) for k in ("quote_id", "tool_id")):
        return False
    rec = obj.get("recovery")
    if not isinstance(rec, dict):
        return False
    if not all(isinstance(rec.get(k), str) for k in _RECOVERY_STR):
        return False
    if not all(isinstance(rec.get(k), bool) for k in _RECOVERY_BOOL):
        return False
    return _optional_str_ok(rec, "next_action")


def _reject_non_standard(token):
    """``json.loads`` calls this for ``Infinity`` / ``-Infinity`` / ``NaN`` —
    Rust's serde_json, JS ``JSON.parse``, and Go ``encoding/json`` all reject
    these non-standard tokens, so the Python mirror must too (else it would
    over-accept a header the others reject)."""
    raise ValueError(f"non-standard JSON constant: {token}")


def _tolerant_parse(raw: bytes):
    """Mirror ``FailureSchematic::from_header_bytes``: decode the header bytes
    as strict UTF-8 JSON (no ``Infinity``/``NaN``) and accept iff the value
    deserializes to the full schematic shape AND carries the tag — else
    ``None`` (fall back to the human error body)."""
    try:
        obj = json.loads(raw.decode("utf-8"), parse_constant=_reject_non_standard)
    except (UnicodeDecodeError, ValueError):
        return None
    if isinstance(obj, dict) and obj.get("object") == FAILURE_TAG and _has_schematic_shape(obj):
        return obj
    return None


@pytest.mark.parametrize("case", FAILURE_CASES, ids=[c["name"] for c in FAILURE_CASES])
def test_failure_schematic_tolerance(case):
    parsed = _tolerant_parse(_failure_header_bytes(case))
    assert (parsed is not None) == case["accepted"]
    if parsed is None:
        return
    assert parsed["object"] == FAILURE_TAG
    expect = case.get("expect")
    if expect is not None:
        assert parsed["stage"] == expect["stage"]
        assert parsed["reason"] == expect["reason"]
        assert parsed["retryable"] == expect["retryable"]
        assert parsed["funds_moved"] == expect["funds_moved"]
        assert parsed["prior_payment"] == expect["prior_payment"]
        rec = expect["recovery"]
        assert parsed["recovery"]["class"] == rec["class"]
        assert parsed["recovery"]["actor"] == rec["actor"]
        assert parsed["recovery"]["safe_to_retry"] == rec["safe_to_retry"]
        assert parsed["recovery"]["safe_to_requote"] == rec["safe_to_requote"]
    for key in case.get("expect_extra_keys", []):
        assert key in parsed


def test_floats_are_rejected_by_the_canonical_writer():
    with pytest.raises(ValueError):
        canonicalize({"price": 1.5})


def test_fixture_names_are_unique():
    names = [e["name"] for e in ENVELOPES]
    assert len(names) == len(set(names))
