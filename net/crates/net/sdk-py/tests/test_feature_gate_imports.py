"""``net_sdk.blob`` / ``net_sdk.transport`` tell a missing Cargo feature apart
from a ``net`` that failed to load.

A missing feature (the extension loaded, the symbols aren't there) gets the
"rebuild with ``--features dataforts``" message. A load failure (no
``_net``, a bad linked library) must surface as itself: relabeling it as a
missing feature sends the reader to rebuild a wheel that isn't the problem.
"""

from __future__ import annotations

import importlib
import sys
import types

import pytest

MODULES = ("net_sdk.blob", "net_sdk.transport")


@pytest.fixture
def swap_net(monkeypatch: pytest.MonkeyPatch):
    """Install a replacement for ``net`` and drop the cached SDK modules, so
    each import runs fresh against it. Monkeypatch restores everything."""

    # The package itself imports the real (or conftest-stubbed) `net`; load it
    # first, so only the module under test runs against the replacement.
    importlib.import_module("net_sdk")

    def _install(replacement):
        monkeypatch.setitem(sys.modules, "net", replacement)
        for name in MODULES:
            monkeypatch.delitem(sys.modules, name, raising=False)

    return _install


@pytest.mark.parametrize("module", MODULES)
def test_a_missing_feature_names_the_feature_and_the_symbols(swap_net, module) -> None:
    swap_net(types.ModuleType("net"))  # loads fine, exports nothing
    with pytest.raises(ImportError, match=r"--features dataforts.*Missing: "):
        importlib.import_module(module)


class _BrokenNet(types.ModuleType):
    """A ``net`` whose attribute access fails the way a half-loaded
    extension does."""

    def __getattr__(self, name):
        raise ImportError("DLL load failed while importing _net: the specified module could not be found")


@pytest.mark.parametrize("module", MODULES)
def test_a_load_failure_is_not_relabelled_as_a_missing_feature(swap_net, module) -> None:
    swap_net(_BrokenNet("net"))
    with pytest.raises(ImportError) as exc_info:
        importlib.import_module(module)
    assert "DLL load failed" in str(exc_info.value)
    assert "--features dataforts" not in str(exc_info.value)
