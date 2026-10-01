"""Constructor parity: ``net_sdk.MeshNode`` vs the native ``NetMesh``.

`PYTHON_SDK_WRAPPER_PARITY_PLAN.md` S1a. The sdk-py wrapper has dropped
native constructor options twice: the SSDK P4 fix (topology kwargs) and
G6 (six options, including ``require_signed_capabilities``). The stub
drifted the same way (four ``subnet_*`` kwargs missing). This guard
reads the native signature from the extension itself, so it can't be
satisfied by updating a hand-written list.

Lives here, not in ``sdk-py/tests/``: it needs the real extension, and
the sdk-py job runs against a stub of it. CI's main pytest run happens
before the sdk-py wrapper is installed, so ``net_sdk`` is imported from
the in-repo source when it isn't installed. The test never skips: a
skipped drift guard is a guard that can't fail.
"""

from __future__ import annotations

import ast
import importlib
import inspect
import re
import sys
from pathlib import Path

import pytest

THIS_DIR = Path(__file__).parent
BINDING_ROOT = THIS_DIR.parent
PYI_PATH = BINDING_ROOT / "python" / "net" / "_net.pyi"
LIB_RS = BINDING_ROOT / "src" / "lib.rs"
SDK_SRC = BINDING_ROOT.parent.parent / "sdk-py" / "src"

# Leading positional parameters both sides take; not keyword options.
POSITIONAL = ("bind_addr", "psk")

# Native options the SDK deliberately does not expose, each with a
# reason. Starts empty; an entry needs a reason a reviewer can check.
NOT_IN_SDK: dict[str, str] = {}


def _net_mesh_cls():
    _net = importlib.import_module("net._net")
    return _net.NetMesh


def _sdk_mesh_module():
    try:
        return importlib.import_module("net_sdk.mesh")
    except ImportError:
        sys.path.insert(0, str(SDK_SRC))
        return importlib.import_module("net_sdk.mesh")


def _native_params() -> list[str]:
    """The native constructor's parameter names, in order. Prefer the
    extension's own ``__text_signature__``; fall back to the
    ``#[pyo3(signature = (…))]`` block on ``NetMesh::new``. Fail if
    neither yields a list."""
    try:
        sig = inspect.signature(_net_mesh_cls())
        names = [p.name for p in sig.parameters.values()]
        if names:
            return names
    except (TypeError, ValueError):
        pass
    src = LIB_RS.read_text(encoding="utf-8")
    m = re.search(
        r"#\[new\]\s*#\[pyo3\(signature = \((.*?)\)\)\]\s*"
        r"(?:#\[[^\]]*\]\s*)*fn new\(",
        src,
        re.S,
    )
    assert m, (
        "could not read NetMesh's native signature from __text_signature__ "
        "or from the #[pyo3(signature)] block in src/lib.rs"
    )
    return [
        part.split("=")[0].strip()
        for part in m.group(1).split(",")
        if part.strip()
    ]


def _sdk_params() -> list[str]:
    init = _sdk_mesh_module().MeshNode.__init__
    return [p for p in inspect.signature(init).parameters if p != "self"]


def _stub_params() -> list[str]:
    tree = ast.parse(PYI_PATH.read_text(encoding="utf-8"))
    for node in tree.body:
        if isinstance(node, ast.ClassDef) and node.name == "NetMesh":
            for child in node.body:
                if isinstance(child, ast.FunctionDef) and child.name == "__init__":
                    args = child.args
                    return [a.arg for a in args.args + args.kwonlyargs][1:]
    raise AssertionError("no NetMesh.__init__ in _net.pyi")


def test_native_signature_is_readable() -> None:
    params = _native_params()
    assert params[: len(POSITIONAL)] == list(POSITIONAL), params
    assert len(params) > len(POSITIONAL)


def test_sdk_meshnode_declares_every_native_option() -> None:
    native = set(_native_params())
    sdk = set(_sdk_params())
    missing = sorted(native - sdk - set(NOT_IN_SDK))
    extra = sorted(sdk - native)
    assert not missing, (
        f"net_sdk.MeshNode.__init__ drops native options {missing}; "
        "declare and forward them, or list them in NOT_IN_SDK with a reason"
    )
    assert not extra, f"net_sdk.MeshNode.__init__ has non-native options {extra}"


def test_stub_declares_every_native_option() -> None:
    native = _native_params()
    stub = _stub_params()
    assert sorted(stub) == sorted(native), (
        f"_net.pyi NetMesh.__init__ is missing {sorted(set(native) - set(stub))} "
        f"and has extra {sorted(set(stub) - set(native))}"
    )


def test_sdk_meshnode_forwards_every_option_unchanged(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Declaration isn't enough: each option must reach the native call
    with the caller's value. A distinct sentinel per option catches a
    dropped, renamed or swapped argument."""
    mesh_mod = _sdk_mesh_module()
    seen: dict[str, object] = {}

    class Recorder:
        def __init__(self, *args: object, **kwargs: object) -> None:
            seen["args"] = args
            seen.update(kwargs)

    monkeypatch.setattr(mesh_mod, "_NetMesh", Recorder)
    options = [p for p in _sdk_params() if p not in POSITIONAL]
    sentinels = {name: object() for name in options}
    mesh_mod.MeshNode("127.0.0.1:0", "00" * 32, **sentinels)

    assert seen["args"] == ("127.0.0.1:0", "00" * 32)
    for name, sentinel in sentinels.items():
        assert name in seen, f"{name} was not forwarded to NetMesh"
        assert seen[name] is sentinel, f"{name} was forwarded with the wrong value"


def test_sdk_meshnode_accepts_real_values_for_the_g6_options() -> None:
    """The forwarded values are accepted by the native constructor, not
    just passed along. What ``require_signed_capabilities`` *does* is
    witnessed in the Rust suite (`tests/capability_broadcast.rs`); a
    Python node always signs its own announcements, so it can't be
    re-witnessed here."""
    mesh_mod = _sdk_mesh_module()
    node = mesh_mod.MeshNode(
        "127.0.0.1:0",
        "42" * 32,
        capability_gc_interval_ms=120_000,
        require_signed_capabilities=True,
        try_port_mapping=False,
        auto_direct_upgrade=False,
        permissive_channels=False,
    )
    try:
        assert node.node_id != 0
    finally:
        node.shutdown()
