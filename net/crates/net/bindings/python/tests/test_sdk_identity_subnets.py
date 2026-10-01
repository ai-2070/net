"""``net_sdk.identity`` and ``net_sdk.subnets``, live
(`PYTHON_SDK_WRAPPER_PARITY_PLAN.md` S5).

The subnet helpers are checked against the native constructor on the
same inputs: ``subnet_id`` must reject exactly what ``NetMesh(subnet=…)``
rejects, no more and no less. Multi-node subnet visibility is covered by
the Rust suite (``tests/subnet_*.rs``), not here.

Imports ``net_sdk`` from the in-repo source when the wrapper isn't
installed (CI's main run); it never skips for that.
"""

from __future__ import annotations

import importlib
import importlib.util
import sys
import typing
from pathlib import Path

import pytest

pytest.importorskip("net._net")

import net  # noqa: E402
from net import IdentityError, NetMesh  # noqa: E402

SDK_SRC = Path(__file__).resolve().parents[3] / "sdk-py" / "src"
PSK = "42" * 32


def _sdk(module: str):
    # Fall back to the checkout only when `net_sdk` is not installed at
    # all. An ImportError raised *inside* an installed package is a real
    # failure and must surface, not be papered over with source.
    if importlib.util.find_spec("net_sdk") is None:
        sys.path.insert(0, str(SDK_SRC))
    return importlib.import_module(module)


def test_identity_names_are_the_wheels_own_objects() -> None:
    identity = _sdk("net_sdk.identity")
    for name in identity.__all__:
        if name == "TokenScope":
            continue
        assert getattr(identity, name) is getattr(net, name), name


def test_every_token_scope_literal_is_accepted_and_round_trips() -> None:
    identity = _sdk("net_sdk.identity")
    scopes = list(typing.get_args(identity.TokenScope))
    issuer = identity.Identity.generate()
    subject = identity.Identity.generate().entity_id
    for scope in scopes:
        token = issuer.issue_token(subject, [scope], "sensors/temp", 60)
        parsed = identity.parse_token(token)
        assert scope in parsed["scope"], scope
        assert parsed["channel_hash"] == identity.channel_hash("sensors/temp")
    with pytest.raises(IdentityError):
        issuer.issue_token(subject, ["not-a-scope"], "sensors/temp", 60)


CANDIDATE_SUBNETS = [
    [0],
    [3, 7],
    [255, 255, 255, 255],
    [],
    [1, 2, 3, 4, 5],
    [256],
    [0, 300],
    [True],  # bool is an int subclass; native takes it as 1
    [False],
    [1.5],
]


@pytest.mark.parametrize("levels", CANDIDATE_SUBNETS)
def test_subnet_id_rejects_exactly_what_native_rejects(levels) -> None:
    subnets = _sdk("net_sdk.subnets")
    try:
        mesh = NetMesh("127.0.0.1:0", PSK, subnet=levels)
    except (IdentityError, TypeError, OverflowError):
        # IdentityError for range/length; TypeError / OverflowError when a
        # value isn't an int at all, or is negative (extracted as u32).
        native_ok = False
    else:
        native_ok = True
        mesh.shutdown()
    try:
        subnets.subnet_id(*levels)
    except ValueError:
        helper_ok = False
    else:
        helper_ok = True
    assert helper_ok == native_ok, (levels, helper_ok, native_ok)


def test_a_node_built_from_the_helpers_constructs() -> None:
    net_sdk = _sdk("net_sdk")
    subnets = _sdk("net_sdk.subnets")
    policy = subnets.subnet_policy(
        {"tag_prefix": "region:", "level": 0, "values": {"eu": 1, "us": 2}},
        {"tag_prefix": "zone:", "level": 1, "values": {"a": 1}},
    )
    node = net_sdk.MeshNode(
        "127.0.0.1:0", PSK, subnet=subnets.subnet_id(3, 7), subnet_policy=policy
    )
    try:
        node.register_channel("sdk/local", visibility="subnet-local")
    finally:
        node.shutdown()
    global_node = net_sdk.MeshNode("127.0.0.1:0", PSK, subnet=subnets.GLOBAL_SUBNET)
    global_node.shutdown()


def test_a_policy_the_native_layer_rejects_is_still_rejected() -> None:
    """``subnet_policy`` only shapes; validation stays native."""
    net_sdk = _sdk("net_sdk")
    subnets = _sdk("net_sdk.subnets")
    bad = subnets.subnet_policy({"tag_prefix": "region:", "level": 0, "values": {"eu": 0}})
    with pytest.raises(IdentityError):
        net_sdk.MeshNode("127.0.0.1:0", PSK, subnet_policy=bad)
