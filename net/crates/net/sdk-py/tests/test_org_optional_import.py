"""``import net_sdk`` must survive a wheel built without the ``org`` feature.

SDK audit 2026-10-11. ``org`` is an optional binding feature (a plain
``maturin develop`` or an sdist install builds without it), but the org
facade was the one optional surface imported unguarded, so its absence took
down the whole package — ``NetNode`` included.

The guard must swallow ONLY that absence: a wheel that has the org surface
but fails to import part of it is a broken build, and re-raises.
"""

from __future__ import annotations

import importlib
import sys
import types

import pytest

# Whichever `net` is live: the real wheel, or this directory's conftest
# auto-stub (which answers every attribute, org names included).
net = pytest.importorskip("net", reason="net wheel not built")

#: The native names ``net_sdk.org`` imports from ``net``. A wheel built
#: without ``org`` has none of them.
_ORG_NATIVE_NAMES = frozenset(
    {
        "AsyncOrgClient",
        "OrgAdmissionDeniedError",
        "OrgClient",
        "OrgCredentials",
        "OrgCredentialsError",
        "OrgDiscoveryError",
        "OrgError",
        "OrgServeHandle",
        "OrgUnclassifiedError",
        "install_org_authority",
        "install_provider_grant_audience",
        "serve_org",
        "serve_org_client_stream",
        "serve_org_duplex",
        "serve_org_streaming",
    }
)

_HAS_ORG = hasattr(net, "OrgClient")


def _net_without(hidden: frozenset[str]) -> types.ModuleType:
    """A stand-in ``net`` that hides ``hidden`` and forwards everything else
    to the live module. A proxy rather than ``delattr`` because the conftest
    auto-stub regenerates any deleted attribute on access."""
    real = net

    class _Hiding(types.ModuleType):
        def __getattr__(self, name: str):
            if name in hidden:
                raise AttributeError(name)
            return getattr(real, name)

    return _Hiding("net")


def _fresh_sdk_import(monkeypatch: pytest.MonkeyPatch):
    """Re-import ``net_sdk`` from scratch. Every cached ``net_sdk*`` module
    is removed through ``monkeypatch`` so the originals are restored at
    teardown (otherwise the re-imported copy stays cached for later
    tests). ``net_sdk`` itself must already be importable, but
    ``net_sdk.org`` need not be — on a no-``org`` wheel it never is."""
    importlib.import_module("net_sdk")
    for name in [m for m in sys.modules if m == "net_sdk" or m.startswith("net_sdk.")]:
        monkeypatch.delitem(sys.modules, name)
    return lambda: importlib.import_module("net_sdk")


def test_package_imports_without_the_org_surface(monkeypatch: pytest.MonkeyPatch) -> None:
    reimport = _fresh_sdk_import(monkeypatch)
    # Make the native module look exactly like a wheel built without `org`.
    monkeypatch.setitem(sys.modules, "net", _net_without(_ORG_NATIVE_NAMES))

    sdk = reimport()

    assert hasattr(sdk, "NetNode")
    assert "OrgClient" not in sdk.__all__
    assert not hasattr(sdk, "OrgClient")


@pytest.mark.skipif(not _HAS_ORG, reason="wheel built without the org feature")
def test_a_partially_missing_org_surface_is_not_swallowed(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The wheel HAS `org` (``OrgClient`` is present) but one symbol the
    facade needs is gone — a broken build, not an absent feature. The
    import must fail loudly instead of quietly dropping every org name."""
    reimport = _fresh_sdk_import(monkeypatch)
    monkeypatch.setitem(sys.modules, "net", _net_without(frozenset({"serve_org_duplex"})))

    with pytest.raises(ImportError, match="serve_org_duplex"):
        reimport()


def test_a_failure_inside_the_org_facade_is_not_swallowed(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """An ImportError that does not come from ``from net import ...`` (here:
    the facade module itself is unimportable) is not the no-``org`` case."""
    reimport = _fresh_sdk_import(monkeypatch)
    # A `None` entry makes `import net_sdk.org` raise ModuleNotFoundError
    # naming `net_sdk.org`, not `net`.
    monkeypatch.setitem(sys.modules, "net_sdk.org", None)

    with pytest.raises(ImportError):
        reimport()


@pytest.mark.skipif(not _HAS_ORG, reason="wheel built without the org feature")
def test_org_names_are_exported_when_present() -> None:
    sdk = importlib.import_module("net_sdk")
    for name in ("OrgClient", "serve_org", "OrgUnclassifiedError", "HANDLER_DROP_CONTRACT"):
        assert name in sdk.__all__
        assert hasattr(sdk, name)
