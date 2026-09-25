"""S4Vectors — Python's consumer of the streaming OPENING conformance vectors.

Loads ``tests/cross_lang_org/streaming_opening_vectors.json`` — the SAME
fixture Rust generates (``gen_org_error_fixtures``) and consumes — and pins
this binding's view of it byte for byte: the opening envelope's dual wire
encodings (``wire_hex`` + ``wire_base64`` over the same bytes), the
caller/provider role split over the shared pre-signature prefix and the
33-byte stream suffix, the decoder and signature rejects as REAL mutations of
their base wire, and the frozen ``org:`` error vocabulary. Every byte row is
flip-sensitive on the vector bytes: one flipped byte in a vector's
``wire_hex``/``wire``/``wire_base64`` must redden its named row. Malformed or
unknown errors, narrowed ids, and decoder disagreement must never become
success.

Pure-Python: ``net.org.parse_org_error`` needs no compiled extension, so this
runs even on a partial or unbuilt wheel, following ``test_org_error_vectors.py``.
"""

from __future__ import annotations

import base64
import json
import re
from pathlib import Path

import pytest

# The classifier is pure Python and importable without the native module.
org = pytest.importorskip("net.org", reason="net.org module not importable")
parse_org_error = org.parse_org_error

_FIXTURE = (
    Path(__file__).resolve().parents[3]
    / "tests"
    / "cross_lang_org"
    / "streaming_opening_vectors.json"
)


def _load() -> dict:
    return json.loads(_FIXTURE.read_text())


# The parametrized rows are named at collection time, so the fixture is loaded
# once here; the shape test still re-reads it through ``_load``.
_DOC = _load()
_LAYOUT = _DOC["layout"]
_OPENINGS = _DOC["opening_vectors"]
_REJECTS = _DOC["decoder_rejects"] + _DOC["signature_rejects"]
_VOCAB = _DOC["error_vocabulary"]["vectors"]
_UNCLASSIFIED = _DOC["error_vocabulary"]["unclassified_cases"]
_DOMAIN_TOKENS = {d["token"] for d in _DOC["error_vocabulary"]["domains"]}

_NARROWED = re.compile(r"[0-9a-f]{16}\.\.\.")
_FULL_ID = re.compile(r"[0-9a-f]{64}")

# The 65-byte signature field is Postcard's bytes-with-length: 1 length byte
# (0x40 = varint 64) + the 64-byte signature. The frozen fixture carries 0x40
# in every wire, matching the generator's comment on the flipped byte.
_SIG_LEN_BYTE = 0x40


def _byte_rows() -> list[tuple[str, dict]]:
    """(row label, vector) for every vector carrying ``wire_*`` pins: 48 rows
    (6 opening + 8 decoder rejects + 1 signature reject + 29 vocabulary +
    4 unclassified)."""
    rows = [(v["id"], v) for v in _OPENINGS + _REJECTS]
    rows += [(f"vocab-{v['domain']}.{v['kind']}", v) for v in _VOCAB]
    rows += [(f"unclassified-{i}", v) for i, v in enumerate(_UNCLASSIFIED)]
    return rows


_BYTE_ROWS = _byte_rows()
_VOCAB_LABELS = [f"{v['domain']}.{v['kind']}" for v in _VOCAB]


def test_fixture_has_the_expected_shape() -> None:
    doc = _load()
    assert doc["version"] == 1
    assert doc["prefix"] == "org:"
    assert len(doc["opening_vectors"]) == 6
    assert len(doc["decoder_rejects"]) == 8
    assert len(doc["signature_rejects"]) == 1
    assert len(doc["error_vocabulary"]["vectors"]) == 29
    assert len(doc["error_vocabulary"]["unclassified_cases"]) == 4


@pytest.mark.parametrize(
    "label,v", _BYTE_ROWS, ids=[f"bytes-{label}" for label, _ in _BYTE_ROWS]
)
def test_wire_bytes_pin(label: str, v: dict) -> None:
    """The byte-for-byte pin: the SAME bytes must be recoverable from both of
    the fixture's encodings before any other handling of a vector. A flipped
    byte on either side disagrees with the fixture and must not become
    success."""
    if "wire_hex" in v:  # envelope rows: the wire bytes ride as hex + base64
        b = bytes.fromhex(v["wire_hex"])
    else:  # vocabulary rows: the bytes are the ``wire`` string's UTF-8
        b = v["wire"].encode("utf-8")
    assert len(b) == v["wire_len"], label
    if "wire_hex" in v:  # envelope rows only: catches non-canonical hex
        assert b.hex() == v["wire_hex"], label
    assert base64.b64encode(b).decode() == v["wire_base64"], label


@pytest.mark.parametrize("v", _OPENINGS, ids=[f"as-caller-{v['id']}" for v in _OPENINGS])
def test_opening_wire_as_caller(v: dict) -> None:
    """The caller's view: the streaming wire is the unary encoding plus the
    33-byte suffix, both encodings share the pre-signature prefix, and the
    65-byte signature field (length byte + signature) signs the STREAM
    transcript — a different signature than the unary transcript's."""
    pfx_hex = v["pre_sig_prefix_hex"]
    sig_len = _LAYOUT["signature_wire_len"]
    assert v["wire_len"] == v["unary_wire_len"] + _LAYOUT["stream_suffix_len"], v["id"]
    assert v["wire_hex"].startswith(pfx_hex), v["id"]
    assert v["unary_wire_hex"].startswith(pfx_hex), v["id"]
    off = len(pfx_hex) // 2
    for wire_hex, sig_hex in (
        (v["wire_hex"], v["call_binding_sig_hex"]),
        (v["unary_wire_hex"], v["unary_call_binding_sig_hex"]),
    ):
        field = bytes.fromhex(wire_hex)[off : off + sig_len]
        assert field == bytes([_SIG_LEN_BYTE]) + bytes.fromhex(sig_hex), v["id"]
    assert v["call_binding_sig_hex"] != v["unary_call_binding_sig_hex"], v["id"]
    assert v["expect"]["sig_domain_separated"] is True, v["id"]


@pytest.mark.parametrize("v", _OPENINGS, ids=[f"as-provider-{v['id']}" for v in _OPENINGS])
def test_opening_wire_as_provider(v: dict) -> None:
    """The provider's view: the 33-byte suffix is the kind byte plus the
    session binding, the kind matches the declared kind value, and the
    envelope decodes and verifies."""
    expect = v["expect"]
    suffix = bytes([expect["kind"]]) + bytes.fromhex(expect["session_binding_hex"])
    assert len(suffix) == _LAYOUT["stream_suffix_len"], v["id"]
    assert bytes.fromhex(v["wire_hex"])[-len(suffix) :] == suffix, v["id"]
    assert expect["kind"] == _LAYOUT["kind_values"][v["kind_name"]], v["id"]
    assert expect["session_binding_hex"] == v["session_binding_hex"], v["id"]
    assert expect["decode"] == "ok", v["id"]
    assert expect["verify"] == "ok", v["id"]


@pytest.mark.parametrize(
    "r",
    _REJECTS,
    ids=[
        f"as-provider-reject-{r['id'].removeprefix('reject.')}" for r in _REJECTS
    ],
)
def test_reject_mutation_is_real_against_the_base_wire(r: dict) -> None:
    """Each reject must be a REAL mutation of its base opening wire at the
    declared spot. A mutation the decoder nonetheless accepts — or a stub that
    merely relabels the base bytes — would let decoder disagreement become
    success."""
    base = next(v for v in _OPENINGS if v["id"] == r["base"])
    base_b = bytes.fromhex(base["wire_hex"])
    b = bytes.fromhex(r["wire_hex"])
    base_len = len(base_b)
    assert base_len == base["wire_len"], r["id"]
    suffix_at = base_len - _LAYOUT["stream_suffix_len"]  # the kind byte
    rid = r["id"]
    if rid == "reject.trailing_byte":
        assert len(b) == base_len + 1, rid
        assert b.startswith(base_b), rid
    elif rid in (
        "reject.truncated_len_minus_1",
        "reject.truncated_len_minus_17",
        "reject.truncated_len_minus_33",
    ):
        assert len(b) < base_len, rid
        assert base_b.startswith(b), rid
    elif rid in ("reject.kind_0", "reject.kind_4", "reject.kind_255"):
        assert len(b) == base_len, rid
        diffs = [i for i, (x, y) in enumerate(zip(b, base_b)) if x != y]
        assert diffs == [suffix_at], rid
        assert b[suffix_at] not in (1, 2, 3), rid
    elif rid == "reject.over_cap":
        assert len(b) == _LAYOUT["max_proof_bytes"] + 1, rid
        assert b.startswith(base_b), rid
    elif rid == "reject.signature_flipped":
        assert len(b) == base_len, rid
        diffs = [i for i, (x, y) in enumerate(zip(b, base_b)) if x != y]
        assert len(diffs) == 1, rid
        # exactly one byte flipped inside the 65-byte signature field
        assert suffix_at - _LAYOUT["signature_wire_len"] + 1 <= diffs[0] < suffix_at, rid
        assert r["expect_decode"] == "ok", rid
        assert r["expect_verify_error_display"] == "invalid signature", rid
        return
    else:
        raise AssertionError(f"unmapped reject mutation: {rid}")
    assert r.get("expect_error_display") == "invalid wire format", rid


@pytest.mark.parametrize("v", _VOCAB, ids=[f"as-caller-classify-{l}" for l in _VOCAB_LABELS])
def test_vocabulary_row_classifies_back_to_declared_domain_and_kind(v: dict) -> None:
    """A binding MUST recover the declared domain, kind, and local/remote
    verdict from the ``org:`` wire string."""
    parsed = parse_org_error(v["wire"])
    assert parsed.domain == v["domain"], v["wire"]
    assert parsed.kind == v["kind"], v["wire"]
    assert parsed.is_local == v["is_local"], v["wire"]


@pytest.mark.parametrize("v", _VOCAB, ids=[f"as-provider-grammar-{l}" for l in _VOCAB_LABELS])
def test_vocabulary_wire_grammar(v: dict) -> None:
    """``org:<domain>:<kind>`` or ``org:<domain>:<kind>: <detail>`` with
    declared tokens — nothing else may pose as the vocabulary."""
    wire = v["wire"]
    head = f"org:{v['domain']}:{v['kind']}"
    segments = wire.split(":", 3)
    assert segments[:3] == ["org", v["domain"], v["kind"]], wire
    assert v["domain"] in _DOMAIN_TOKENS, wire
    assert wire == head or wire.startswith(head + ": "), wire
    if v["domain"] == "admission_denied":
        # Coarse bucket ONLY — exactly 3 colon-separated segments; a precise
        # remote reason would be a credential oracle.
        assert wire.count(":") == 2, wire
        assert len(segments) == 3, wire


@pytest.mark.parametrize(
    "c",
    _UNCLASSIFIED,
    ids=[f"never-success-unclassified-{i}" for i in range(len(_UNCLASSIFIED))],
)
def test_unclassified_case_never_becomes_success(c: dict) -> None:
    """The property a misclassification would destroy: an unparseable or
    unknown-vocabulary string must classify as ``unknown``, never one of the
    four canonical domains — that would assert a request reached a provider."""
    parsed = parse_org_error(c["wire"])
    assert parsed.domain == "unknown" == c["expect_domain"], c["wire"]
    assert parsed.kind is None, c["wire"]
    assert parsed.is_local is False, c["wire"]


def test_narrowed_ids_never_match_a_full_id() -> None:
    """Narrowing IDs must not become success: a narrowed display (16 hex
    chars + ``...``) must never equal — or complete to — a full id, and
    classification must surface no id fields at all."""
    doc = _load()
    full_ids = {v for v in doc["ids"].values() if _FULL_ID.fullmatch(v)}
    assert full_ids  # the fixture declares full ids to compare against
    wires = [v["wire"] for v in _VOCAB] + [c["wire"] for c in _UNCLASSIFIED]
    narrowed = []
    for w in wires:
        for m in _NARROWED.finditer(w):
            # exactly 16 hex chars — never the tail of a longer id display
            assert m.start() == 0 or w[m.start() - 1] not in "0123456789abcdef", w
            narrowed.append(m.group())
    assert narrowed  # the fixture does carry narrowed displays
    for n in narrowed:
        assert re.fullmatch(r"[0-9a-f]{16}\.\.\.", n), n
        truncated = n[:-3]
        for f in full_ids:
            assert f != n, (f, n)  # a narrowed display never equals a full id
            assert f != truncated, (f, n)  # nor its truncated form
            assert not f.startswith(truncated), (f, n)  # and never completes one
    for w in wires:
        parsed = parse_org_error(w)
        assert not any(
            "id" in name for name in dir(parsed) if not name.startswith("_")
        ), w
        for surfaced in (parsed.domain, parsed.kind, parsed.message, repr(parsed)):
            # and never surfaces one of the fixture's full ids (a wire's own
            # dummy placeholder ids are the sender's words, not surfaced ids)
            assert not any(f in str(surfaced) for f in full_ids), (w, surfaced)


def test_u64_strings_round_trip_exactly() -> None:
    """u64 values travel as decimal STRINGS and MUST round-trip exactly."""
    for v in _OPENINGS:
        for field in ("call_id", "proof_expires_at_unix_ns"):
            s = v[field]
            assert isinstance(s, str), (v["id"], field)
            assert re.fullmatch(r"\d+", s), (v["id"], field, s)
            assert str(int(s)) == s, (v["id"], field, s)
