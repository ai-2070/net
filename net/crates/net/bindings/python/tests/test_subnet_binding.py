"""SSDK S4b — the Python subnet authority surface through the real PyO3
boundary: construction-time validation, decode-before-mutate provisioning, and
named-export resolution ordering. Mirrors the Node ``subnet_binding.test.ts``.

Issuance is absent from every binding (artifacts come from ``net-mesh subnet``);
these cover the construction and refusal paths a Python application reaches. A
full admitted call needs an adopted authority (operator setup) — covered by the
Rust live suite.
"""

from __future__ import annotations

import itertools

import pytest

net = pytest.importorskip("net", reason="net wheel not built")

if not hasattr(net, "serve_subnet_exported"):
    pytest.skip("net built without the org/subnet feature", allow_module_level=True)

from net.subnet import admin, classify_subnet_error, parse_subnet_kind  # noqa: E402

PSK = "42" * 32
AUTHORITY = "d7" * 32
_ports = itertools.count(36_100)


def _addr() -> str:
    return f"127.0.0.1:{next(_ports)}"


def _mesh(**subnet_kwargs):
    return net.NetMesh(
        _addr(),
        PSK,
        identity_seed=bytes([0xA1]) * 32,
        permissive_channels=True,
        **subnet_kwargs,
    )


def _subnet_config():
    return dict(
        subnet_authorities=[
            {"authority_hex": AUTHORITY, "root_hexes": [AUTHORITY], "maximum_grant_lifetime_secs": 604800},
        ],
        subnet_attachment=[3],
        subnet_exports=[
            {
                "name": "factory-export",
                "access": "granted",
                "binding": {
                    "subnet": {"authority_hex": AUTHORITY, "path": {"levels": [3, 9]}},
                    "topology_epoch": 0,
                },
            }
        ],
    )


def test_broken_construction_config_refuses_before_a_node_exists() -> None:
    with pytest.raises(net.SubnetProvisionError) as ei:
        _mesh(subnet_exports=[*_subnet_config()["subnet_exports"], *_subnet_config()["subnet_exports"]])
    assert parse_subnet_kind(str(ei.value)) == "duplicate_export_name"

    with pytest.raises(net.SubnetProvisionError) as ei:
        _mesh(subnet_authorities=[{"authority_hex": AUTHORITY, "root_hexes": [], "maximum_grant_lifetime_secs": 60}])
    assert parse_subnet_kind(str(ei.value)) == "empty_authority_roots"

    with pytest.raises(net.SubnetProvisionError) as ei:
        _mesh(subnet_authorities=[{"authority_hex": "zz", "root_hexes": [AUTHORITY], "maximum_grant_lifetime_secs": 60}])
    assert parse_subnet_kind(str(ei.value)) == "invalid_id_hex"

    with pytest.raises(net.SubnetProvisionError) as ei:
        _mesh(subnet_attachment=[3, 1, 4, 1, 5])
    assert parse_subnet_kind(str(ei.value)) == "path_too_deep"

    with pytest.raises(net.SubnetProvisionError) as ei:
        _mesh(subnet_attachment=[300])
    assert parse_subnet_kind(str(ei.value)) == "invalid_path_level"


def test_provisioning_decodes_before_mutating_and_classifies() -> None:
    mesh = _mesh(**_subnet_config())
    try:
        garbage = b"\xff\xfe\xfd\xfc"
        with pytest.raises(net.SubnetProvisionError) as ei:
            admin.install_gateway_credentials(mesh, [garbage])
        assert parse_subnet_kind(str(ei.value)) == "invalid_format"

        with pytest.raises(net.SubnetProvisionError) as ei:
            admin.apply_control_fact(mesh, garbage)
        assert parse_subnet_kind(str(ei.value)) == "invalid_format"

        # A well-formed boundary declaration is accepted (wholesale,
        # infallible after DTO conversion).
        admin.declare_boundaries(
            mesh,
            {"authority_hex": AUTHORITY, "topology_epoch": 0, "boundaries": [[3, 9]]},
        )
    finally:
        mesh.shutdown()


def test_unknown_export_name_fails_before_registration() -> None:
    mesh = _mesh(**_subnet_config())
    try:
        # Unknown name: refused at resolution, before any registration —
        # even though no org authority is installed on this node. The
        # registration-failure message WRAPS (does not bare-prefix) the
        # stable kind, so classify on the substring, exactly as the Node
        # suite does — the bare `subnet:<kind>` envelope is for the
        # construction/admin errors above.
        with pytest.raises(net.SubnetProvisionError) as ei:
            net.serve_subnet_exported(mesh, "fleet.telemetry", "no-such-export", lambda c, r: b"")
        assert "subnet:unknown_export_name" in str(ei.value)

        # Known name: resolution succeeds and the CORE refusal (no org
        # authority) surfaces instead — proving order.
        with pytest.raises(Exception) as ei:
            net.serve_subnet_exported(mesh, "fleet.telemetry", "factory-export", lambda c, r: b"")
        assert "unknown_export_name" not in str(ei.value)
    finally:
        mesh.shutdown()


def test_classify_subnet_error_wraps_and_passes_through() -> None:
    err = classify_subnet_error("subnet:invalid_format")
    assert isinstance(err, net.SubnetProvisionError)
    assert err.kind == "invalid_format"
    # A non-subnet message passes through unchanged.
    passthrough = "org:credentials:signature_invalid"
    assert classify_subnet_error(passthrough) == passthrough
