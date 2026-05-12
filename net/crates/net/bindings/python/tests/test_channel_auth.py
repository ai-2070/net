"""Tests for the channel-auth surface on `NetMesh` (Stage F-4).

Single-mesh tests mirror the discipline of `test_channels.py` — full
two-mesh handshake coverage lives in the Rust integration suite
(`tests/channel_auth.rs`) and the TS SDK's `channel_auth.test.ts`
until the Python bindings surface a `local_addr()` helper for a
stable handshake fixture (flagged in `test_channels.py`). What we can
verify here:

  1. `register_channel(publish_caps=..., subscribe_caps=...)` accepts
     the documented filter-dict shape.
  2. Publisher-side publish denial fires when the node's own caps
     don't satisfy `publish_caps` — this is enforced before fan-out
     and needs no peer.
  3. `subscribe_channel(..., token=bytes)` parses and validates the
     token up-front; malformed bytes raise `TokenError` *before* any
     network I/O.
"""

from __future__ import annotations

import pytest

from net import ChannelError, Identity, NetMesh, TokenError


PSK = "42" * 32


def _port(seed: int) -> str:
    return f"127.0.0.1:{30000 + seed}"


# -------------------------------------------------------------------------
# register_channel shape
# -------------------------------------------------------------------------


def test_register_channel_accepts_publish_and_subscribe_caps() -> None:
    m = NetMesh(_port(1), PSK)
    try:
        m.register_channel(
            "auth/both",
            publish_caps={"require_tags": ["admin"]},
            subscribe_caps={"require_tags": ["reader"]},
        )
    finally:
        m.shutdown()


def test_register_channel_accepts_subscribe_caps_with_gpu_filter() -> None:
    m = NetMesh(_port(2), PSK)
    try:
        m.register_channel(
            "gpu/only",
            subscribe_caps={
                "require_gpu": True,
                "gpu_vendor": "nvidia",
                "min_vram_gb": 16,
            },
        )
    finally:
        m.shutdown()


def test_register_channel_publish_caps_wrong_type_raises() -> None:
    m = NetMesh(_port(3), PSK)
    try:
        with pytest.raises(TypeError):
            m.register_channel("bad", publish_caps="not-a-dict")  # type: ignore[arg-type]
    finally:
        m.shutdown()


# -------------------------------------------------------------------------
# Publisher-side publish denial (single-mesh, enforced pre fan-out)
# -------------------------------------------------------------------------


def test_publish_denied_by_own_publish_caps() -> None:
    m = NetMesh(_port(4), PSK)
    try:
        # Node has no announced caps; channel requires `admin` tag.
        m.announce_capabilities({})
        m.register_channel(
            "admin/only", publish_caps={"require_tags": ["admin"]}
        )
        with pytest.raises(ChannelError):
            m.publish("admin/only", b"x", reliability="fire_and_forget")
    finally:
        m.shutdown()


def test_publish_allowed_when_own_caps_match() -> None:
    m = NetMesh(_port(5), PSK)
    try:
        m.announce_capabilities({"tags": ["admin"]})
        m.register_channel(
            "admin/only", publish_caps={"require_tags": ["admin"]}
        )
        # No subscribers — returns a report with attempted=0.
        report = m.publish(
            "admin/only", b"x", reliability="fire_and_forget"
        )
        assert report["attempted"] == 0
    finally:
        m.shutdown()


def test_publish_open_channel_no_caps_enforced() -> None:
    # Regression: no publish_caps + no require_token ⇒ open.
    m = NetMesh(_port(6), PSK)
    try:
        m.register_channel("open/anyone")
        report = m.publish(
            "open/anyone", b"x", reliability="fire_and_forget"
        )
        assert report["attempted"] == 0
    finally:
        m.shutdown()


# -------------------------------------------------------------------------
# subscribe_channel token parsing
# -------------------------------------------------------------------------


def test_subscribe_channel_rejects_malformed_token_bytes() -> None:
    m = NetMesh(_port(7), PSK)
    try:
        # 16 bytes is far too short for a 161-byte PermissionToken —
        # `from_bytes` must reject with `TokenError(invalid_format)`
        # *before* any membership request is dispatched, so there's
        # no network timeout to wait through.
        with pytest.raises(TokenError) as exc_info:
            m.subscribe_channel(0, "some/channel", token=b"\x00" * 16)
        assert str(exc_info.value).removeprefix("token: ") == "invalid_format"
    finally:
        m.shutdown()


def test_subscribe_channel_accepts_structurally_valid_token() -> None:
    # A well-formed, signed 161-byte token reaches the transport —
    # structural parse succeeds client-side, so the failure we see
    # is a `ChannelError` for the missing peer rather than a
    # `TokenError`. Signature verification happens server-side; full
    # coverage of that path lives in the Rust integration suite.
    issuer = Identity.generate()
    subject = Identity.generate()
    token = issuer.issue_token(
        subject.entity_id, ["subscribe"], "c", ttl_seconds=60
    )

    m = NetMesh(_port(8), PSK)
    try:
        with pytest.raises(ChannelError) as exc_info:
            m.subscribe_channel(0, "c", token=token)
        # Not a TokenError — the bytes parsed fine.
        assert not isinstance(exc_info.value, TokenError)
    finally:
        m.shutdown()


def test_subscribe_channel_with_no_token_errors_at_transport() -> None:
    # A dangling subscribe (no peer connected with that node id)
    # should fail as a ChannelError (transport failure) rather than
    # crashing — ensures the None-token path is wired even when the
    # call itself cannot succeed.
    m = NetMesh(_port(9), PSK)
    try:
        with pytest.raises(ChannelError):
            m.subscribe_channel(
                publisher_node_id=12345,
                channel="anywhere",
            )
    finally:
        m.shutdown()


# -------------------------------------------------------------------------
# Full multi-mesh subscribe-denied / token-round-trip coverage lives
# in `tests/channel_auth.rs` (Rust) and
# `sdk-ts/test/channel_auth.test.ts` (TS) — see module docstring.
# -------------------------------------------------------------------------
