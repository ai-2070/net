"""Stub-vs-runtime drift test.

The Python binding exposes ``net._net`` classes that are typed in
``net/_net.pyi``. This test asserts that every class declared in
the stub exists at runtime in the same name, and that for a
sampled subset every method declared in the stub is present as a
callable attribute on the runtime class.

The wheel may not have been built for the local feature set; this
test gracefully skips when ``net`` (or a specific class) can't be
imported. The first test still runs on a partial wheel — it only
asserts on names that *are* importable at runtime.
"""

from __future__ import annotations

import ast
import importlib
from pathlib import Path

import pytest

THIS_DIR = Path(__file__).parent
PKG_ROOT = THIS_DIR.parent / "python" / "net"
PYI_PATH = PKG_ROOT / "_net.pyi"


# Skip the entire module when the built wheel isn't importable —
# typical local-dev state when only `cargo check` has run, not
# `maturin develop`. Stub-only static tests live in
# `test_pyi_stub_coverage.py`.
net = pytest.importorskip("net", reason="net wheel not built locally")
_net = pytest.importorskip("net._net", reason="net._net not importable")


def _collect_stub_class_names() -> list[str]:
    """Return every class name declared at module scope in the
    stub. Order matches source order in ``_net.pyi``."""
    tree = ast.parse(PYI_PATH.read_text())
    names: list[str] = []
    for node in tree.body:
        if isinstance(node, ast.ClassDef):
            names.append(node.name)
    return names


def _collect_stub_methods(class_name: str) -> list[str]:
    """Return every ``def`` declared inside ``class_name``'s body
    in the stub. Includes properties, staticmethods, dunders."""
    tree = ast.parse(PYI_PATH.read_text())
    for node in tree.body:
        if isinstance(node, ast.ClassDef) and node.name == class_name:
            return [
                child.name
                for child in node.body
                if isinstance(
                    child, (ast.FunctionDef, ast.AsyncFunctionDef)
                )
            ]
    return []


@pytest.mark.parametrize("class_name", _collect_stub_class_names())
def test_stub_class_exists_at_runtime(class_name: str) -> None:
    """For every class declared in the stub, assert the runtime
    ``net._net`` module exposes a class with the same name.

    Some classes only build with specific Cargo features (cortex,
    meshdb, meshos, deck, ...). When the local wheel was built
    without that feature the attribute is simply absent; we skip
    rather than fail so the test runs on partial wheels."""
    runtime_attr = getattr(_net, class_name, None)
    if runtime_attr is None:
        pytest.skip(
            f"{class_name} not present in net._net "
            "(wheel likely built without the relevant feature)"
        )
    assert isinstance(runtime_attr, type) or callable(runtime_attr), (
        f"net._net.{class_name} exists but is not a class / callable"
    )


# Sampled subset — one representative class per major feature
# region (MeshOS / MeshDB / Deck). The runtime surface for these
# is most likely to drift relative to the Rust source.
SAMPLED_CLASSES = ["MeshOsDaemonSdk", "DeckClient", "MeshQueryRunner"]


@pytest.mark.parametrize("class_name", SAMPLED_CLASSES)
def test_sampled_class_methods_present(class_name: str) -> None:
    """For each sampled class, assert every method declared in
    the stub exists at runtime as a callable attribute.

    Skips the class if the runtime doesn't expose it (feature
    gating). Properties are checked as plain attributes — PyO3's
    ``#[getter]`` machinery exposes them as descriptors on the
    class object."""
    runtime_cls = getattr(_net, class_name, None)
    if runtime_cls is None:
        pytest.skip(f"{class_name} not present at runtime")
    declared = _collect_stub_methods(class_name)
    assert declared, f"stub declares no methods for {class_name}"
    missing: list[str] = []
    for name in declared:
        if not hasattr(runtime_cls, name):
            missing.append(name)
    assert not missing, (
        f"{class_name}: stub declares {missing} but runtime "
        f"class has no such attribute(s)"
    )


def test_at_least_one_class_collected() -> None:
    """Guard: the AST walker is supposed to find dozens of
    classes. A regression that empties the list (e.g. a malformed
    stub) would silently neutralize every parametrized test
    above; assert on the collected count directly."""
    names = _collect_stub_class_names()
    assert len(names) > 20, (
        f"Expected the stub to declare 20+ classes; got {len(names)}: "
        f"{names}"
    )
