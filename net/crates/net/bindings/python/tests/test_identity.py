"""Tests for the Identity + PermissionToken surface (Stage F-1).

Mirrors `bindings/node/test/identity.test.ts` — every assertion has a
direct TS counterpart. The Python layer uses string-message `TokenError`
with the `kind` accessible via `str(e).removeprefix("token: ")`.
"""

from __future__ import annotations

import pytest

from net import (
    Identity,
    IdentityError,
    TokenError,
    channel_hash,
    delegate_token,
    parse_token,
    token_is_expired,
    verify_token,
)


def _kind(exc: TokenError) -> str:
    return str(exc).removeprefix("token: ")


# -------------------------------------------------------------------------
# generate / seed round-trip
# -------------------------------------------------------------------------


def test_generate_produces_valid_entity_id() -> None:
    ident = Identity.generate()
    eid = ident.entity_id
    assert isinstance(eid, bytes)
    assert len(eid) == 32
    assert ident.origin_hash != 0 or eid == b"\x00" * 32  # u64 derived
    assert ident.node_id != 0 or eid == b"\x00" * 32


def test_generate_yields_unique_identities() -> None:
    a = Identity.generate()
    b = Identity.generate()
    assert a.entity_id != b.entity_id


def test_seed_round_trip() -> None:
    original = Identity.generate()
    seed = original.to_bytes()
    assert len(seed) == 32
    restored = Identity.from_seed(seed)
    assert restored.entity_id == original.entity_id
    assert restored.origin_hash == original.origin_hash
    assert restored.node_id == original.node_id


def test_from_bytes_alias_of_from_seed() -> None:
    seed = Identity.generate().to_bytes()
    via_seed = Identity.from_seed(seed)
    via_bytes = Identity.from_bytes(seed)
    assert via_seed.entity_id == via_bytes.entity_id


def test_from_seed_rejects_wrong_length() -> None:
    with pytest.raises(IdentityError):
        Identity.from_seed(b"\x00" * 16)
    with pytest.raises(IdentityError):
        Identity.from_seed(b"\x00" * 33)


def test_sign_returns_64_bytes() -> None:
    ident = Identity.generate()
    sig = ident.sign(b"hello world")
    assert isinstance(sig, bytes)
    assert len(sig) == 64


# -------------------------------------------------------------------------
# issue / parse / verify
# -------------------------------------------------------------------------


def test_issue_parse_roundtrip_matches_fields() -> None:
    issuer = Identity.generate()
    subject = Identity.generate()
    token = issuer.issue_token(
        subject.entity_id,
        ["publish", "subscribe"],
        "sensors/temp",
        ttl_seconds=3600,
    )
    assert isinstance(token, bytes)

    parsed = parse_token(token)
    assert parsed["issuer"] == issuer.entity_id
    assert parsed["subject"] == subject.entity_id
    assert set(parsed["scope"]) == {"publish", "subscribe"}
    assert parsed["channel_hash"] == channel_hash("sensors/temp")
    assert parsed["delegation_depth"] == 0
    assert parsed["not_after"] > parsed["not_before"]
    assert len(parsed["signature"]) == 64


def test_verify_token_accepts_valid() -> None:
    issuer = Identity.generate()
    subject = Identity.generate()
    token = issuer.issue_token(
        subject.entity_id,
        ["publish"],
        "topic",
        ttl_seconds=60,
    )
    assert verify_token(token) is True


def test_verify_token_rejects_tampered() -> None:
    issuer = Identity.generate()
    subject = Identity.generate()
    token = bytearray(
        issuer.issue_token(
            subject.entity_id,
            ["publish"],
            "topic",
            ttl_seconds=60,
        )
    )
    # Flip a byte inside the signature region (last 64 bytes).
    token[-1] ^= 0x01
    assert verify_token(bytes(token)) is False


def test_token_is_expired_false_for_fresh_token() -> None:
    issuer = Identity.generate()
    subject = Identity.generate()
    token = issuer.issue_token(
        subject.entity_id, ["publish"], "topic", ttl_seconds=3600
    )
    assert token_is_expired(token) is False


def test_token_is_expired_reports_expired_even_when_signature_tampered() -> None:
    # Regression for a cubic-flagged bug: the previous impl walked
    # `is_valid()` (signature + time) and matched on Err(Expired),
    # which short-circuited on the signature check. A tampered +
    # expired token therefore returned False ("not expired") even
    # though wall-clock was past `not_after`. The docstring says
    # `token_is_expired` is a pure time check — this regression
    # locks that contract in.
    import time

    issuer = Identity.generate()
    subject = Identity.generate()
    # 1-second TTL. Sleep 2.5s below — `not_after` is stored in
    # whole seconds, so 1.3s can leave `current == not_after` on
    # certain issue-time phases (flake). 2.5s reliably crosses a
    # full boundary.
    token = bytearray(
        issuer.issue_token(
            subject.entity_id, ["publish"], "topic", ttl_seconds=1
        )
    )
    # Tamper the signature.
    token[-1] ^= 0xFF

    # Wait past the TTL with margin.
    time.sleep(2.5)

    # Pure time check — tampered-but-expired must report True.
    assert token_is_expired(bytes(token)) is True, (
        "token_is_expired must be a pure time check; short-circuiting "
        "on signature validity is the cubic-flagged bug"
    )


def test_issue_token_rejects_unknown_scope() -> None:
    issuer = Identity.generate()
    subject = Identity.generate()
    with pytest.raises(IdentityError):
        issuer.issue_token(
            subject.entity_id, ["bogus"], "topic", ttl_seconds=60
        )


def test_issue_token_rejects_invalid_subject_length() -> None:
    issuer = Identity.generate()
    with pytest.raises(IdentityError):
        issuer.issue_token(
            b"\x00" * 16, ["publish"], "topic", ttl_seconds=60
        )


def test_parse_token_rejects_bad_format() -> None:
    with pytest.raises(TokenError) as exc_info:
        parse_token(b"\x00" * 16)
    assert _kind(exc_info.value) == "invalid_format"


# -------------------------------------------------------------------------
# token cache install / lookup
# -------------------------------------------------------------------------


def test_install_and_lookup_token() -> None:
    issuer = Identity.generate()
    subject = Identity.generate()
    token = issuer.issue_token(
        subject.entity_id, ["subscribe"], "sensors/temp", ttl_seconds=600
    )

    holder = Identity.generate()
    assert holder.token_cache_len == 0
    holder.install_token(token)
    assert holder.token_cache_len == 1

    found = holder.lookup_token(subject.entity_id, "sensors/temp")
    assert found == token


def test_lookup_token_miss_returns_none() -> None:
    holder = Identity.generate()
    other = Identity.generate()
    assert holder.lookup_token(other.entity_id, "not/there") is None


def test_install_token_rejects_tampered() -> None:
    issuer = Identity.generate()
    subject = Identity.generate()
    raw = bytearray(
        issuer.issue_token(
            subject.entity_id, ["subscribe"], "c", ttl_seconds=60
        )
    )
    raw[-1] ^= 0x02
    holder = Identity.generate()
    with pytest.raises(TokenError) as exc_info:
        holder.install_token(bytes(raw))
    assert _kind(exc_info.value) == "invalid_signature"


# -------------------------------------------------------------------------
# delegation
# -------------------------------------------------------------------------


def test_delegation_chain_exhausts_at_depth_zero() -> None:
    a = Identity.generate()
    b = Identity.generate()
    c = Identity.generate()
    d = Identity.generate()

    # A issues a depth=2 token to B with delegate scope.
    token_ab = a.issue_token(
        b.entity_id,
        ["publish", "delegate"],
        "chain",
        ttl_seconds=3600,
        delegation_depth=2,
    )

    # B re-delegates to C (depth -> 1).
    token_bc = delegate_token(b, token_ab, c.entity_id, ["publish", "delegate"])
    parsed_bc = parse_token(token_bc)
    assert parsed_bc["delegation_depth"] == 1
    assert parsed_bc["subject"] == c.entity_id

    # C re-delegates to D (depth -> 0). Last permitted hop.
    token_cd = delegate_token(c, token_bc, d.entity_id, ["publish"])
    parsed_cd = parse_token(token_cd)
    assert parsed_cd["delegation_depth"] == 0
    assert parsed_cd["subject"] == d.entity_id

    # D cannot re-delegate further — depth is exhausted.
    e = Identity.generate()
    with pytest.raises(TokenError) as exc_info:
        delegate_token(d, token_cd, e.entity_id, ["publish"])
    assert _kind(exc_info.value) == "delegation_exhausted"


def test_delegation_requires_delegate_scope_on_parent() -> None:
    a = Identity.generate()
    b = Identity.generate()
    c = Identity.generate()

    # A issues to B WITHOUT the delegate scope.
    token_ab = a.issue_token(
        b.entity_id, ["publish"], "topic", ttl_seconds=3600, delegation_depth=2
    )
    with pytest.raises(TokenError) as exc_info:
        delegate_token(b, token_ab, c.entity_id, ["publish"])
    assert _kind(exc_info.value) == "delegation_not_allowed"


def test_delegation_rejects_unauthorized_signer() -> None:
    a = Identity.generate()
    b = Identity.generate()
    c = Identity.generate()
    stranger = Identity.generate()

    # Token is addressed to B, but `stranger` tries to delegate it.
    token_ab = a.issue_token(
        b.entity_id,
        ["publish", "delegate"],
        "topic",
        ttl_seconds=3600,
        delegation_depth=2,
    )
    with pytest.raises(TokenError) as exc_info:
        delegate_token(stranger, token_ab, c.entity_id, ["publish"])
    assert _kind(exc_info.value) == "not_authorized"


# -------------------------------------------------------------------------
# miscellany
# -------------------------------------------------------------------------


def test_channel_hash_is_stable() -> None:
    h1 = channel_hash("sensors/temp")
    h2 = channel_hash("sensors/temp")
    assert h1 == h2
    # Canonical 32-bit ChannelHash range (was u16 before the
    # substrate-wide widening; the wire NetHeader fast-path hint is
    # still u16 and equals the low 16 bits of this value).
    assert 0 <= h1 <= 0xFFFFFFFF


def test_channel_hash_differs_across_names() -> None:
    assert channel_hash("a") != channel_hash("b")


def test_repr_includes_entity_id() -> None:
    ident = Identity.generate()
    rep = repr(ident)
    assert "Identity(" in rep
    assert "entity_id=0x" in rep
