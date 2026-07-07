"""Tests for the device-enrollment (mesh admin) meta-tools
(`HERMES_INTEGRATION_PLAN_V2.md` Phase 1): ``net_mesh_invite`` /
``net_mesh_devices`` / ``net_mesh_revoke``.

These prove the plugin orchestrates the operator device-lifecycle over the
exposed ``net_sdk`` enrollment surface: mint an invite (serving enrollment on
the node), list the inventory, and revoke a device. The crypto + stores live in
the Rust SDK; here we exercise the tool handlers + the ``node.operator()``
lifecycle, and that the tools fail cleanly with no root identity.
"""

from __future__ import annotations

import asyncio
import json
import os

import pytest

pytest.importorskip("net")
pytest.importorskip("net_sdk")

import net  # noqa: E402

# A fixed root seed so the operator/root is deterministic across the test.
_ROOT_SEED_HEX = "11" * 32


def _run(coro) -> dict:
    return json.loads(asyncio.run(coro))


def _set_env(**vals):
    """Set/clear env vars, returning the prior values for restore."""
    saved = {k: os.environ.get(k) for k in vals}
    for k, v in vals.items():
        if v is None:
            os.environ.pop(k, None)
        else:
            os.environ[k] = v
    return saved


def _restore_env(saved):
    for k, v in saved.items():
        if v is None:
            os.environ.pop(k, None)
        else:
            os.environ[k] = v


@pytest.fixture()
def rooted_node(plugin, tmp_path):
    """Rebuild the embedded node under a fixed root seed + temp stores, so the
    mesh-admin tools have an operator to work with and never touch the real
    machine inventory."""
    node = plugin.node
    saved = _set_env(
        NET_MESH_IDENTITY_SEED=_ROOT_SEED_HEX,
        NET_MESH_DEVICE_STORE=str(tmp_path / "devices.json"),
        NET_MESH_REVOCATION_STORE=str(tmp_path / "revocations.json"),
        NET_MESH_PIN_STORE=str(tmp_path / "pins.json"),
        NET_MESH_PSK=None,
        NET_MESH_PEERS=None,
    )
    node.shutdown()  # clear any node another test built under different env
    try:
        yield node
    finally:
        node.shutdown()
        _restore_env(saved)
        node.shutdown()  # ensure the next test rebuilds under restored env


def test_invite_mints_a_shareable_string(rooted_node, plugin):
    res = _run(plugin.tools.handle_net_mesh_invite({"ttl_seconds": 300}))
    assert res["status"] == "ok"
    assert res["invite"].startswith("net-invite:")
    # The invite decodes and anchors at our root.
    root = net.Identity.from_seed(bytes.fromhex(_ROOT_SEED_HEX))
    parsed = net.InviteToken.decode(res["invite"])
    assert parsed.root == root.entity_id
    assert res["root_fingerprint"] == net.fingerprint(root.entity_id)


def test_devices_and_revoke_over_the_facade(rooted_node, plugin):
    node = plugin.node
    # Enroll a device directly through the same operator the tools use, so it
    # appears in the inventory without needing a second live node.
    operator = node.operator()
    invite = operator.invite(node.mesh().rendezvous_string(), 300)
    device = net.Identity.generate()
    req = net.JoinRequest.create(device, "pc", ["region:office"], invite)
    operator.approve(req, 3600)

    res = _run(plugin.tools.handle_net_mesh_devices({}))
    assert res["status"] == "ok"
    assert len(res["devices"]) == 1
    rec = res["devices"][0]
    assert rec["name"] == "pc"
    assert rec["device_id"] == device.entity_id.hex()
    assert rec["tags"] == ["region:office"]
    assert rec["revoked"] is False
    # A fresh enrollment is far from the annual-grant expiry.
    assert rec["expires_in_days"] >= 360
    assert rec["renewal_recommended"] is False
    assert "warning" not in res

    # Revoke it and confirm the inventory reflects it.
    res = _run(plugin.tools.handle_net_mesh_revoke({"device_id": device.entity_id.hex()}))
    assert res["status"] == "ok"
    res = _run(plugin.tools.handle_net_mesh_devices({}))
    assert res["devices"][0]["revoked"] is True


def test_devices_expiry_warning_surfaces(rooted_node, plugin, tmp_path):
    # A device whose annual grant is nearly up (silent renewal didn't refresh
    # it) surfaces a renewal warning — the "expiry warning before annual grant
    # expiry" acceptance.
    import time

    node = plugin.node
    operator = node.operator()
    invite = operator.invite(node.mesh().rendezvous_string(), 300)
    device = net.Identity.generate()
    operator.approve(net.JoinRequest.create(device, "pc", [], invite), 3600)

    # Backdate the record directly in the store to ~20 days before the 1-year
    # expiry (within the 30-day renewal window).
    store = tmp_path / "devices.json"
    data = json.loads(store.read_text())
    data["devices"][0]["enrolled_at"] = int(time.time()) - (365 - 20) * 86400
    store.write_text(json.dumps(data))

    res = _run(plugin.tools.handle_net_mesh_devices({}))
    assert res["status"] == "ok"
    assert res["devices"][0]["renewal_recommended"] is True
    assert res["devices"][0]["expires_in_days"] <= 21
    assert "warning" in res


def test_revoke_rejects_a_bad_device_id(rooted_node, plugin):
    res = _run(plugin.tools.handle_net_mesh_revoke({"device_id": "not-hex"}))
    assert res["status"] == "error"
    res = _run(plugin.tools.handle_net_mesh_revoke({"device_id": ""}))
    assert res["status"] == "error"
    res = _run(plugin.tools.handle_net_mesh_revoke({"device_id": "ab12"}))
    assert res["status"] == "error"  # wrong length


@pytest.mark.parametrize("set_var", ["NET_MESH_DEVICE_STORE", "NET_MESH_REVOCATION_STORE"])
def test_half_store_override_fails_loudly(plugin, tmp_path, monkeypatch, set_var):
    # Setting only one of the two store overrides used to silently fall back to
    # the machine-shared production files for BOTH stores — an "isolated" test
    # would write the real inventory. It must refuse loudly instead.
    other = (
        "NET_MESH_REVOCATION_STORE"
        if set_var == "NET_MESH_DEVICE_STORE"
        else "NET_MESH_DEVICE_STORE"
    )
    monkeypatch.setenv("NET_MESH_IDENTITY_SEED", _ROOT_SEED_HEX)
    monkeypatch.setenv(set_var, str(tmp_path / "store.json"))
    monkeypatch.delenv(other, raising=False)
    # The refusal fires before the mesh handle is touched: a dummy suffices.
    with pytest.raises(RuntimeError, match=other):
        plugin.node._build_operator(object())


def test_mesh_tools_registered_in_toolset(plugin):
    names = {name for name, _schema, _handler, _emoji in plugin.tools.TOOLS}
    assert {"net_mesh_invite", "net_mesh_devices", "net_mesh_revoke"} <= names


def test_mesh_tools_error_without_a_root_identity(plugin, tmp_path):
    node = plugin.node
    saved = _set_env(
        NET_MESH_IDENTITY_SEED=None,  # no root => enrollment has nobody to sign as
        NET_MESH_DEVICE_STORE=str(tmp_path / "devices.json"),
        NET_MESH_REVOCATION_STORE=str(tmp_path / "revocations.json"),
        NET_MESH_PIN_STORE=str(tmp_path / "pins.json"),
        NET_MESH_PSK=None,
        NET_MESH_PEERS=None,
    )
    node.shutdown()
    try:
        res = _run(plugin.tools.handle_net_mesh_invite({}))
        assert res["status"] == "error"
        assert "root" in res["error"].lower()
        res = _run(plugin.tools.handle_net_mesh_devices({}))
        assert res["status"] == "error"
    finally:
        node.shutdown()
        _restore_env(saved)
        node.shutdown()
