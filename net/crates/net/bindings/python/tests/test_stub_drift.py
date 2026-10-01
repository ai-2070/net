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
import inspect
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
# region (MeshOS / MeshDB / Deck), plus every class a `net_sdk`
# wrapper forwards to (`PYTHON_SDK_WRAPPER_PARITY_PLAN.md` S0). Add a
# class here when an sdk-py wrapper starts forwarding to it: the two
# tests below then hold its stub to the runtime in BOTH directions.
SAMPLED_CLASSES = [
    "MeshOsDaemonSdk",
    "DeckClient",
    "MeshQueryRunner",
    "NetMesh",
    "CausalEvent",
    "DaemonRuntime",
    "DaemonHandle",
    "MigrationHandle",
    "ReplicaGroup",
    "ForkGroup",
    "StandbyGroup",
]


# Methods the stub declares that exist only under a Cargo feature the
# wheel may be built without. Each is named with its feature, so a
# missing method is excused only when it is KNOWN to be gated — never
# because it is merely absent. `(class, method) -> feature`.
FEATURE_GATED: dict[tuple[str, str], str] = {
    ("NetMesh", "traversal_stats"): "nat-traversal",
    ("NetMesh", "connect_direct"): "nat-traversal",
    ("NetMesh", "connect_direct_auto"): "nat-traversal",
    ("NetMesh", "nat_type"): "nat-traversal",
    ("NetMesh", "reflex_addr"): "nat-traversal",
    ("NetMesh", "peer_nat_type"): "nat-traversal",
    ("NetMesh", "probe_reflex"): "nat-traversal",
    ("NetMesh", "reclassify_nat"): "nat-traversal",
    ("NetMesh", "set_reflex_override"): "nat-traversal",
    ("NetMesh", "clear_reflex_override"): "nat-traversal",
    ("NetMesh", "list_tools"): "tool",
    ("NetMesh", "watch_tools"): "tool",
    ("NetMesh", "set_a2a_org_caller"): "a2a+org",
    ("NetMesh", "a2a_org_caller"): "a2a+org",
}


def _features_compiled_in() -> set[str]:
    """The `FEATURE_GATED` features this wheel was built with, inferred
    from the gated methods themselves: a feature is on if ANY of its
    methods exists on the runtime class. The wheel exports no feature
    list, and this is enough to catch one method going missing from a
    build where its feature is on. (It can't catch every method of a
    feature disappearing at once; that reads as "feature off".)"""
    return {
        feature
        for (cls_name, method), feature in FEATURE_GATED.items()
        if hasattr(getattr(_net, cls_name, None), method)
    }


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
        if hasattr(runtime_cls, name):
            continue
        feature = FEATURE_GATED.get((class_name, name))
        if feature is not None and feature not in _features_compiled_in():
            continue  # gated, and this wheel was built without the feature
        missing.append(name)
    assert not missing, (
        f"{class_name}: stub declares {missing} but runtime "
        f"class has no such attribute(s)"
    )


@pytest.mark.parametrize("class_name", SAMPLED_CLASSES)
def test_sampled_class_runtime_methods_are_stubbed(class_name: str) -> None:
    """The reverse of :func:`test_sampled_class_methods_present`: every
    public attribute the runtime class exposes must be declared in the
    stub. A method the wheel has but the stub omits is invisible to
    type checkers, and nothing else in the suite looks for one — that
    is how ``NetMesh.capability_aggregate`` went missing unnoticed.

    "Public" = no leading underscore, defined on the class itself (not
    inherited from ``object``)."""
    runtime_cls = getattr(_net, class_name, None)
    if runtime_cls is None:
        pytest.skip(f"{class_name} not present at runtime")
    declared = set(_collect_stub_methods(class_name)) | set(
        _collect_stub_attributes(class_name)
    )
    public = sorted(
        name
        for name in vars(runtime_cls)
        if not name.startswith("_")
    )
    unstubbed = [name for name in public if name not in declared]
    assert not unstubbed, (
        f"{class_name}: runtime exposes {unstubbed} but the stub does "
        f"not declare them; add them to net/_net.pyi"
    )


@pytest.mark.parametrize("class_name", SAMPLED_CLASSES)
def test_sampled_class_method_parameters_match(class_name: str) -> None:
    """Names are not enough: a stubbed method whose parameters differ
    from the runtime's type-checks wrong calls and rejects right ones.
    That shipped once — ``NetMesh.publish_island_topology`` was stubbed
    without its ``p50_latency_us`` parameter — and both name-level tests
    above passed. Compares parameter names, in order, against
    ``inspect.signature`` of the runtime (PyO3 publishes
    ``__text_signature__``). Properties are skipped; a method whose
    runtime signature is unreadable fails rather than skips."""
    runtime_cls = getattr(_net, class_name, None)
    if runtime_cls is None:
        pytest.skip(f"{class_name} not present at runtime")
    tree = ast.parse(PYI_PATH.read_text())
    cls_node = next(
        node
        for node in tree.body
        if isinstance(node, ast.ClassDef) and node.name == class_name
    )
    mismatched: list[str] = []
    for fn in cls_node.body:
        if not isinstance(fn, ast.FunctionDef):
            continue
        if fn.name.startswith("__") and fn.name != "__init__":
            continue
        decorators = {d.id for d in fn.decorator_list if isinstance(d, ast.Name)}
        if "property" in decorators:
            continue
        runtime_attr = (
            runtime_cls if fn.name == "__init__" else getattr(runtime_cls, fn.name, None)
        )
        if runtime_attr is None:
            continue  # absence is the name-level tests' concern
        try:
            runtime_params = [
                (p.name, p.kind.name)
                for p in inspect.signature(runtime_attr).parameters.values()
                if p.name not in ("self", "$self", "cls")
            ]
        except (TypeError, ValueError):
            mismatched.append(f"{fn.name}: runtime signature unreadable")
            continue
        # Name AND kind: a keyword-only native option stubbed as positional
        # (or the reverse) type-checks calls the runtime rejects.
        args = fn.args
        P = inspect.Parameter
        stub_params = (
            [(a.arg, P.POSITIONAL_ONLY.name) for a in args.posonlyargs]
            + [(a.arg, P.POSITIONAL_OR_KEYWORD.name) for a in args.args]
            + ([(args.vararg.arg, P.VAR_POSITIONAL.name)] if args.vararg else [])
            + [(a.arg, P.KEYWORD_ONLY.name) for a in args.kwonlyargs]
            + ([(args.kwarg.arg, P.VAR_KEYWORD.name)] if args.kwarg else [])
        )
        if "staticmethod" not in decorators and stub_params[:1] and stub_params[0][0] == "self":
            stub_params = stub_params[1:]
        if stub_params != runtime_params:
            mismatched.append(f"{fn.name}: runtime={runtime_params} stub={stub_params}")
    assert not mismatched, f"{class_name} stub parameters drift: {mismatched}"


def _collect_stub_attributes(class_name: str) -> list[str]:
    """Annotated attributes (``name: type``) declared in
    ``class_name``'s stub body — the stub form of a PyO3 ``#[pyo3(get)]``
    field."""
    tree = ast.parse(PYI_PATH.read_text())
    for node in tree.body:
        if isinstance(node, ast.ClassDef) and node.name == class_name:
            return [
                child.target.id
                for child in node.body
                if isinstance(child, ast.AnnAssign)
                and isinstance(child.target, ast.Name)
            ]
    return []


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
