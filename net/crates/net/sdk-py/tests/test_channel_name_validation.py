"""Channel names must satisfy the canonical Net grammar at construction.

`ChannelName::new` (Rust) is the only constructor for a distributed mesh
channel name and has no `From<&str>` escape hatch. The ergonomic tagged
topic wrapper never reaches it — `TypedChannel.publish` embeds the name
in generic EventBus JSON as `_channel` and calls `ingest_raw` — so every
invalid name used to be accepted here and diverge from the mesh only
later.

These are the inverse tests for each Rust boundary in
`net/crates/net/src/adapter/net/channel/name.rs:108-156`.
"""

from __future__ import annotations

import pytest

from net_sdk.channel import (
    MAX_CHANNEL_NAME_LEN,
    ChannelNameError,
    TypedChannel,
    validate_channel_name,
)


class _FakeBus:
    """Minimal stand-in for `net.Net`; construction never touches it."""


# --- Accepted names -------------------------------------------------


@pytest.mark.parametrize(
    "name",
    [
        "sensors",
        "sensors/temperature",
        "sensors/lidar/front",
        "a",
        "a1",
        "with-dash",
        "with_underscore",
        "with.dot",
        "net.rpc.v1/call",
        "0",
        "..dots-but-not-a-segment",
        "a" * MAX_CHANNEL_NAME_LEN,
    ],
)
def test_valid_names_are_accepted(name: str) -> None:
    assert validate_channel_name(name) == name
    assert TypedChannel(_FakeBus(), name).name == name


# --- Rejected names, one case per Rust boundary ---------------------


@pytest.mark.parametrize(
    ("name", "reason"),
    [
        ("", "empty"),
        ("a" * (MAX_CHANNEL_NAME_LEN + 1), "too long"),
        # 256 bytes from 128 two-byte code points: the bound is bytes,
        # not characters, so a name that is only 128 chars still fails.
        ("é" * 128, "too long"),
        ("/leading", "leading slash"),
        ("trailing/", "trailing slash"),
        ("/", "slash only"),
        ("bad//name", "double slash"),
        ("Upper", "uppercase"),
        ("sensors/Temp", "uppercase in later segment"),
        ("has space", "space"),
        ("has\ttab", "tab"),
        ("has\nnewline", "newline"),
        ("colon:name", "colon"),
        ("star*", "wildcard"),
        ("plus+name", "plus"),
        ("hash#name", "hash"),
        ("emoji\U0001f600", "non-ascii"),
        ("café", "non-ascii letter"),
        (".", "dot segment"),
        ("..", "dot-dot segment"),
        ("a/./b", "interior dot segment"),
        ("a/../b", "interior dot-dot segment"),
        ("../escape", "leading traversal"),
        ("a/..", "trailing traversal"),
    ],
)
def test_invalid_names_are_rejected(name: str, reason: str) -> None:
    with pytest.raises(ChannelNameError):
        validate_channel_name(name)


@pytest.mark.parametrize(
    "name",
    ["", "bad//name", "/leading", "trailing/", "Upper", "has space", ".", ".."],
)
def test_typed_channel_constructor_rejects(name: str) -> None:
    """Direct construction is covered, not just `NetNode.channel()`."""
    with pytest.raises(ChannelNameError):
        TypedChannel(_FakeBus(), name)


def test_node_channel_rejects_invalid_name() -> None:
    """The documented entrypoint rejects too — `NetNode.channel()` must
    not be a way around the `TypedChannel` constructor check."""
    from net_sdk.node import NetNode

    node = NetNode.__new__(NetNode)  # bypass native bus construction
    node._bus = _FakeBus()  # type: ignore[attr-defined]

    with pytest.raises(ChannelNameError):
        node.channel("Bad//Name")


def test_channel_name_error_is_a_value_error() -> None:
    """`ChannelNameError` subclasses `ValueError` so existing callers
    that catch `ValueError` around channel construction keep working."""
    assert issubclass(ChannelNameError, ValueError)


def test_error_message_names_the_violation() -> None:
    with pytest.raises(ChannelNameError, match="lowercase only"):
        validate_channel_name("Sensors/Temp")
    with pytest.raises(ChannelNameError, match="must not be empty"):
        validate_channel_name("")
    with pytest.raises(ChannelNameError, match="reserved"):
        validate_channel_name("a/../b")
