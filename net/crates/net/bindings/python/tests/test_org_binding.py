"""OSDK-L P — the Python org surface through the real native boundary.

Unlike ``test_org_error_vectors.py`` (pure Python by design), this loads the
compiled extension and exercises the actual PyO3 marshaling: credential bytes
crossing, refusals carrying the ``org:`` vocabulary intact across FFI, and
identity provenance being recorded.

Issuance is deliberately absent from every binding (credentials come from the
``net org`` CLI), so these cover the construction and refusal paths a Python
application can reach. A full admitted call needs an adopted node authority,
which is operator setup — covered by the Rust live suite.
"""

from __future__ import annotations

import itertools

import pytest

net = pytest.importorskip("net", reason="net wheel not built")

# Skip cleanly on a wheel built without the `org` feature.
if not hasattr(net, "OrgCredentials"):
    pytest.skip("net built without the org feature", allow_module_level=True)

from net.org import parse_org_error  # noqa: E402

PSK = "42" * 32
_ports = itertools.count(35_100)


def _addr() -> str:
    return f"127.0.0.1:{next(_ports)}"


def _seed(b: int) -> bytes:
    return bytes([b]) * 32


def test_malformed_credentials_refused_with_the_org_vocabulary() -> None:
    with pytest.raises(net.OrgCredentialsError) as ei:
        net.OrgCredentials(b"\x00" * 8, b"\x00" * 8, [], [])
    parsed = parse_org_error(str(ei.value))
    assert parsed.domain == "credentials"
    # The refusal crossed FFI as the shared vocabulary, not as prose.
    assert parsed.kind == "signature_invalid"
    assert parsed.is_local is True


def test_correctly_sized_but_unsigned_credentials_still_refused() -> None:
    # 156 and 185 are the exact wire lengths, so this proves signature
    # verification runs across the boundary rather than a length check.
    with pytest.raises(net.OrgCredentialsError) as ei:
        net.OrgCredentials(b"\x00" * 156, b"\x00" * 185, [], [])
    parsed = parse_org_error(str(ei.value))
    assert parsed.domain == "credentials"
    assert parsed.kind == "signature_invalid"


def test_the_org_error_hierarchy_is_installed() -> None:
    # The subclasses are real exceptions rooted at OrgError, so a caller can
    # `except net.OrgError` and catch every domain.
    assert issubclass(net.OrgCredentialsError, net.OrgError)
    assert issubclass(net.OrgDiscoveryError, net.OrgError)
    assert issubclass(net.OrgAdmissionDeniedError, net.OrgError)
    assert issubclass(net.OrgUnclassifiedError, net.OrgError)


def test_no_way_to_pass_an_audience_secret_as_bytes() -> None:
    # The constructor takes `audience_secret_paths: list[str]` and no bytes
    # sibling. The raw discovery key can never be a Python bytes, so it can
    # never be in GC'd memory. Passing bytes where paths are expected is a
    # type error, not a silent acceptance.
    with pytest.raises((TypeError, ValueError)):
        net.OrgCredentials(b"\x00" * 8, b"\x00" * 8, [], [b"\x00" * 32])


def test_seeded_meshes_are_stable_ephemeral_are_not() -> None:
    # The property the facade's provenance check enforces: a seeded identity is
    # durable (org membership can name it); an unseeded one changes each time.
    s = _seed(0x7A)
    a = net.NetMesh(_addr(), PSK, identity_seed=s)
    b = net.NetMesh(_addr(), PSK, identity_seed=s)
    try:
        assert a.entity_id == b.entity_id
        assert a.node_id == b.node_id
    finally:
        a.shutdown()
        b.shutdown()

    e1 = net.NetMesh(_addr(), PSK)
    e2 = net.NetMesh(_addr(), PSK)
    try:
        assert e1.entity_id != e2.entity_id
    finally:
        e1.shutdown()
        e2.shutdown()


def test_bind_and_serve_are_reachable_on_a_seeded_mesh() -> None:
    mesh = net.NetMesh(_addr(), PSK, identity_seed=_seed(0x51))
    try:
        # The surface exists with the expected shape; a full bind needs an
        # adopted authority (operator setup, in the Rust live suite).
        assert callable(net.OrgClient.bind)
        assert callable(net.serve_org)
        # Building well-formed-but-unsigned credentials still fails across the
        # boundary — proving the whole marshal -> refuse -> raise chain works.
        with pytest.raises(net.OrgError):
            net.OrgCredentials(b"\x00" * 156, b"\x00" * 185, [], [])
    finally:
        mesh.shutdown()
