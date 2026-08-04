"""The high-level SDK's subnet provider facade (review-10 P1-5).

`MeshNode` accepted and retained `subnet_exports` at construction, but its
native handle is private and the only provider function lived in the
low-level `net.subnet` package — so an application using the advertised
ergonomic constructor could not serve a named export without reaching into
`self._native`. These tests pin the facade that closes that: the exact frozen
verb name, on the wrapper, delegating to the low-level seam with the wrapped
handle.

Pure-Python against the conftest stub — the live serve/call cell belongs to
the cross-language S4 matrix, not here.
"""

from __future__ import annotations

import importlib
import inspect
import sys
import types


def test_mesh_node_exposes_the_frozen_provider_verb() -> None:
    mesh_mod = importlib.import_module("net_sdk.mesh")

    assert hasattr(mesh_mod.MeshNode, "serve_subnet_exported"), (
        "the ordinary provider verb must live on the high-level MeshNode; "
        "an application using the ergonomic constructor has no other way in"
    )

    # The signature IS the contract: service, export name, handler — and no
    # authority objects. A `SubnetRef`, epoch, or binding parameter appearing
    # here would mean the facade leaked operator state into application code.
    params = list(
        inspect.signature(mesh_mod.MeshNode.serve_subnet_exported).parameters
    )
    assert params[:4] == ["self", "service", "export_name", "handler"]
    assert "handler_timeout_ms" in params
    forbidden = {"subnet", "subnet_ref", "topology_epoch", "binding", "access"}
    assert not forbidden.intersection(params), (
        f"the provider verb must construct no authority objects, saw {params}"
    )


def test_serve_subnet_exported_delegates_with_the_native_handle() -> None:
    """The facade passes the WRAPPED node's native handle through.

    Delegating with the wrapper instead of `self._native` would reach the
    binding with an object it cannot use; delegating to the wrong function
    would silently bypass the `subnet:` classification. Both are caught here.
    """
    mesh_mod = importlib.import_module("net_sdk.mesh")

    calls: list[tuple] = []
    sentinel_handle = object()

    stub = types.ModuleType("net.subnet")

    def _serve(mesh, service, export_name, handler, handler_timeout_ms):
        calls.append((mesh, service, export_name, handler, handler_timeout_ms))
        return sentinel_handle

    stub.serve_subnet_exported = _serve  # type: ignore[attr-defined]
    previous = sys.modules.get("net.subnet")
    sys.modules["net.subnet"] = stub
    try:
        node = mesh_mod.MeshNode.__new__(mesh_mod.MeshNode)
        native = object()
        node._native = native  # type: ignore[attr-defined]

        def handler(caller: dict, req: object) -> object:
            return req

        result = node.serve_subnet_exported("fleet.telemetry", "factory-export", handler)

        assert result is sentinel_handle, "the serve handle must be returned unchanged"
        assert len(calls) == 1
        got_mesh, got_service, got_export, got_handler, got_timeout = calls[0]
        assert got_mesh is native, "the NATIVE handle must cross, not the wrapper"
        assert got_service == "fleet.telemetry"
        assert got_export == "factory-export"
        assert got_handler is handler
        assert got_timeout is None
    finally:
        if previous is None:
            del sys.modules["net.subnet"]
        else:
            sys.modules["net.subnet"] = previous


def test_constructor_accepts_the_subnet_authority_kwargs() -> None:
    """Construction-time subnet config is part of the frozen surface."""
    mesh_mod = importlib.import_module("net_sdk.mesh")

    params = set(inspect.signature(mesh_mod.MeshNode.__init__).parameters)
    for name in (
        "subnet_authorities",
        "subnet_attachment",
        "subnet_control_channel",
        "subnet_exports",
    ):
        assert name in params, f"MeshNode.__init__ must accept {name}"
