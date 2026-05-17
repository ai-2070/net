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


def _ensure_net_stub() -> None:
    if "net" in sys.modules:
        return

    stub = types.ModuleType("net")

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

    # Future-proof against new top-level imports: any unknown attribute
    # access on the stub module returns an opaque class. Names ending
    # in `Error` are made Exception subclasses so wrapper modules
    # whose `except` clauses reference them (`except CortexError:`)
    # don't trip `TypeError: catching classes that do not inherit
    # from BaseException`.
    def __getattr__(attr_name: str) -> type:
        if attr_name.endswith("Error"):
            cls = type(attr_name, (Exception,), {})
        else:
            cls = type(attr_name, (), {})
        setattr(stub, attr_name, cls)
        return cls

    stub.__getattr__ = __getattr__  # type: ignore[attr-defined]
    sys.modules["net"] = stub


_ensure_net_stub()
