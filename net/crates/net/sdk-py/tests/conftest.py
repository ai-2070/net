"""Pytest fixtures for the SDK.

The SDK imports `from net import Net, StoredEvent` at module load. Those
names come from the Rust-built `net` extension, which is not always
available when running pure-Python SDK tests (e.g., in CI where the
extension hasn't been built yet, or in editor lints). Tests in this
directory only need the SDK's pure-Python logic, so we install a thin
stub for the `net` module before any SDK module is imported.

This is a *test-only* shim — production callers continue to import the
real `net` extension.
"""

from __future__ import annotations

import sys
import types


def _make_auto_stub_module(name: str) -> types.ModuleType:
    """Build a stub module whose unknown attribute access returns an
    opaque placeholder class. Names ending in ``Error`` become
    ``Exception`` subclasses so ``except StubError:`` clauses
    type-check.
    """
    stub = types.ModuleType(name)

    def __getattr__(attr_name: str) -> type:
        # Dunders like ``__path__`` must NOT be auto-stubbed — Python's
        # import machinery treats ``__path__`` as a sequence of search
        # directories, and returning a class type triggers
        # ``TypeError: 'type' object is not iterable`` inside
        # ``_get_spec``. Letting AttributeError bubble lets the import
        # machinery treat the module as a non-package, which is fine
        # since we pre-stub each known submodule directly in
        # ``sys.modules`` below.
        if attr_name.startswith("__") and attr_name.endswith("__"):
            raise AttributeError(attr_name)
        if attr_name.endswith("Error"):
            cls = type(attr_name, (Exception,), {})
        else:
            cls = type(attr_name, (), {})
        setattr(stub, attr_name, cls)
        return cls

    stub.__getattr__ = __getattr__  # type: ignore[attr-defined]
    return stub


def _ensure_net_stub() -> None:
    if "net" in sys.modules:
        return

    stub = _make_auto_stub_module("net")

    # Symbols the SDK imports from `net` at module load. Each is a
    # placeholder class; tests that need richer behavior should mock
    # via attribute injection on the per-test bus instance.
    _names = (
        "Net",
        "NetMesh",
        "StoredEvent",
        "IngestResult",
        "PollResponse",
        "Stats",
        "BackpressureError",
        "NotConnectedError",
    )
    for name in _names:
        # Errors must be exception subclasses so `except ...` clauses
        # in the SDK still type-check at import time.
        if name.endswith("Error"):
            setattr(stub, name, type(name, (Exception,), {}))
        else:
            setattr(stub, name, type(name, (), {}))

    sys.modules["net"] = stub

    # Pre-stub each submodule the SDK imports from. `net_sdk.tool`
    # does ``from net.tool import ...`` at module load, which kicks
    # the import machinery into ``_get_spec(net, 'tool')`` and reads
    # ``net.__path__``. Installing the submodule directly in
    # ``sys.modules`` short-circuits that lookup so we don't have to
    # fake a package layout.
    tool_stub = _make_auto_stub_module("net.tool")
    sys.modules["net.tool"] = tool_stub
    stub.tool = tool_stub  # type: ignore[attr-defined]


_ensure_net_stub()
