"""``net_sdk.subnets`` (pure Python) and the S5 root exports
(`PYTHON_SDK_WRAPPER_PARITY_PLAN.md` S5). That the subnet helpers reject
exactly what the native constructor rejects is checked against the real
extension in ``bindings/python/tests/test_sdk_identity_subnets.py``.
"""

from __future__ import annotations

import pytest

import net_sdk
from net_sdk import subnets


def test_subnet_id_builds_the_native_list_shape() -> None:
    assert subnets.subnet_id(3) == [3]
    assert subnets.subnet_id(1, 2, 3, 255) == [1, 2, 3, 255]
    assert subnets.GLOBAL_SUBNET == [0]


@pytest.mark.parametrize("levels", [(), (1, 2, 3, 4, 5), (256,), (-1,), (1.5,)])
def test_subnet_id_rejects_malformed_levels(levels) -> None:
    with pytest.raises(ValueError):
        subnets.subnet_id(*levels)


def test_subnet_policy_wraps_its_rules() -> None:
    rule: subnets.SubnetRule = {"tag_prefix": "region:", "level": 0, "values": {"eu": 1}}
    assert subnets.subnet_policy(rule) == {"rules": [rule]}
    assert subnets.subnet_policy() == {"rules": []}


def test_s5_names_are_exported_from_the_root() -> None:
    for name in (
        "Identity",
        "IdentityError",
        "TokenError",
        "TokenScope",
        "channel_hash",
        "stream_id_from_label",
        "GLOBAL_SUBNET",
        "SubnetPolicy",
        "subnet_id",
        "subnet_policy",
    ):
        assert name in net_sdk.__all__, name
        assert hasattr(net_sdk, name), name
