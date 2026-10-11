"""``import net_sdk`` must survive a wheel built without the ``org`` feature.

SDK audit 2026-10-11. ``org`` is an optional binding feature (a plain
``maturin develop`` or an sdist install builds without it), but the org
facade was the one optional surface imported unguarded, so its absence took
down the whole package — ``NetNode`` included.
"""

from __future__ import annotations

import importlib
import sys

import pytest


def test_package_imports_without_the_org_surface(monkeypatch: pytest.MonkeyPatch) -> None:
    # Import the real package first so the deletions below are restored at
    # teardown (otherwise the org-less copy stays cached for later tests).
    importlib.import_module("net_sdk")
    importlib.import_module("net_sdk.org")
    for name in [m for m in sys.modules if m == "net_sdk" or m.startswith("net_sdk.")]:
        monkeypatch.delitem(sys.modules, name)
    # A `None` entry makes `import net_sdk.org` raise ImportError, which is
    # exactly what a build without the `org` feature does.
    monkeypatch.setitem(sys.modules, "net_sdk.org", None)

    sdk = importlib.import_module("net_sdk")

    assert hasattr(sdk, "NetNode")
    assert "OrgClient" not in sdk.__all__
    assert not hasattr(sdk, "OrgClient")


def test_org_names_are_exported_when_present() -> None:
    sdk = importlib.import_module("net_sdk")
    for name in ("OrgClient", "serve_org", "OrgUnclassifiedError", "HANDLER_DROP_CONTRACT"):
        assert name in sdk.__all__
        assert hasattr(sdk, name)
