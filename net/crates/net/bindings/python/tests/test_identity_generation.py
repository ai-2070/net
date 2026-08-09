"""Durable issuer state through the Python binding (decision 4b).

Mirrors ``bindings/node/test/identity_generation.test.ts`` — every
assertion has a direct TS counterpart, and the state bytes are core's
encoding, so a state file written by any binding is readable by every
other one.

``issuer_generation`` rides on every token and drives revocation, but
the binding had no way to set it: ``Identity`` carried a keypair and a
cache and nothing else, so every token it minted was generation zero.
The field was visible on ``parse_token`` output and settable by nobody.
"""

from __future__ import annotations

import pytest

from net import Identity, IdentityError, delegate_token, parse_token

CHANNEL = "issuer/rotation"
STATE_SIZE = 37


def _issue(signer: Identity, subject: Identity) -> bytes:
    return signer.issue_token(subject.entity_id, ["publish"], CHANNEL, 3600, 0)


def test_starts_at_zero_and_stamps_every_issued_token() -> None:
    ident = Identity.generate()
    subject = Identity.generate()
    assert ident.issuer_generation == 0
    assert parse_token(_issue(ident, subject))["issuer_generation"] == 0


def test_rotation_returns_a_new_identity_and_leaves_the_original() -> None:
    ident = Identity.generate()
    subject = Identity.generate()

    rotated = ident.at_generation(3)
    assert rotated.issuer_generation == 3
    assert ident.issuer_generation == 0
    assert rotated.entity_id == ident.entity_id

    assert parse_token(_issue(rotated, subject))["issuer_generation"] == 3
    assert parse_token(_issue(ident, subject))["issuer_generation"] == 0


def test_state_bytes_round_trip_key_and_generation() -> None:
    ident = Identity.generate().at_generation(6)
    state = ident.to_state_bytes()
    assert len(state) == STATE_SIZE
    # The layout is a cross-SDK contract, not an implementation detail.
    assert state[0] == 1
    assert state[33:37] == b"\x06\x00\x00\x00"

    restored = Identity.from_state_bytes(state)
    assert restored.issuer_generation == 6
    assert restored.entity_id == ident.entity_id

    subject = Identity.generate()
    assert parse_token(_issue(restored, subject))["issuer_generation"] == 6


def test_key_only_restoration_returns_generation_zero() -> None:
    # The documented cost of `to_bytes` / `from_bytes`. An issuer that
    # rotated to 4 and published floor 4 comes back here unable to mint
    # anything a verifier will accept.
    ident = Identity.generate().at_generation(4)
    seed_only = Identity.from_bytes(ident.to_bytes())
    assert seed_only.entity_id == ident.entity_id
    assert seed_only.issuer_generation == 0


def test_generation_may_not_go_backwards() -> None:
    ident = Identity.generate().at_generation(5)
    with pytest.raises(IdentityError) as excinfo:
        ident.at_generation(4)
    assert "generation_went_backwards" in str(excinfo.value)

    # Re-applying a persisted generation on restart is not an error.
    assert ident.at_generation(5).issuer_generation == 5
    assert ident.at_generation(6).issuer_generation == 6


def test_ceiling_demands_a_key_rotation() -> None:
    ceiling = Identity.generate().at_generation(0xFFFFFFFF)
    assert ceiling.issuer_generation == 0xFFFFFFFF
    with pytest.raises(IdentityError) as excinfo:
        ceiling.at_generation(0xFFFFFFFF)
    assert "generation_exhausted" in str(excinfo.value)


def test_malformed_and_future_state_is_refused() -> None:
    good = Identity.generate().at_generation(2).to_state_bytes()

    with pytest.raises(IdentityError) as excinfo:
        Identity.from_state_bytes(good[:-1])
    assert "invalid_state_length" in str(excinfo.value)

    # A bare seed is not identity state — accepting it would put the
    # generation-zero trap back through the versioned door.
    with pytest.raises(IdentityError) as excinfo:
        Identity.from_state_bytes(b"\x00" * 32)
    assert "invalid_state_length" in str(excinfo.value)

    future = bytearray(good)
    future[0] = 2
    with pytest.raises(IdentityError) as excinfo:
        Identity.from_state_bytes(bytes(future))
    assert "unsupported_state_version" in str(excinfo.value)


def test_delegation_stamps_the_signer_generation() -> None:
    # `delegate_token` used to copy the parent's generation onto the
    # child. The child's issuer is the signer, so that stamped an epoch
    # belonging to an entity that had not signed it.
    root = Identity.generate().at_generation(3)
    machine = Identity.generate().at_generation(7)
    leaf = Identity.generate()

    root_link = root.issue_token(
        machine.entity_id, ["publish", "delegate"], CHANNEL, 3600, 2
    )
    assert parse_token(root_link)["issuer_generation"] == 3

    machine_link = delegate_token(machine, root_link, leaf.entity_id, ["publish"])
    assert parse_token(machine_link)["issuer_generation"] == 7
