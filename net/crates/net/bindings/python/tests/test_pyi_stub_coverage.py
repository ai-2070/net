"""Type-stub coverage regression test.

The ``net._net`` extension exposes a large surface (cortex,
meshdb, meshos, dataforts, compute, groups, deck, ...). The
high-level ``net.__init__`` re-exports each family conditionally
(``try: from ._net import ...``) so consumers can ``from net
import MeshOsDaemonSdk``.

Type-checkers (mypy / pyright) read ``net/_net.pyi`` to resolve
those names. A symbol that the runtime exports but the stub
doesn't declare becomes a typed dead-zone: editors flag
``MeshOsDaemonSdk`` as "module has no attribute" and consumers
hit Any. Drift in this direction has happened repeatedly as new
PyO3 features land.

This test parses both files at the AST level — no extension
import, no maturin build required — and asserts every name
re-exported by ``__init__`` has a matching class / function
declaration in ``_net.pyi``. A new PyO3 export without a stub
addition fails here.
"""

from __future__ import annotations

import ast
from pathlib import Path

THIS_DIR = Path(__file__).parent
PKG_ROOT = THIS_DIR.parent / "python" / "net"


def _collect_reexported_names(init_path: Path) -> set[str]:
    """Walk ``__init__.py`` and return every name appearing in a
    ``from ._net import (...)`` statement. The conditional try /
    except blocks mean any of these may be absent at runtime;
    they're declared anyway in the stub for type-checker support."""
    tree = ast.parse(init_path.read_text())
    names: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.ImportFrom) and node.module == "._net":
            for alias in node.names:
                # `from X import (a, b as c)` — record the imported
                # name (not the alias) since the stub declares the
                # original.
                names.add(alias.name)
    return names


def _collect_stub_decls(pyi_path: Path) -> set[str]:
    """Return the set of names declared at module scope in the
    type stub — classes, functions, and TypeAlias/Constant
    assignments."""
    tree = ast.parse(pyi_path.read_text())
    decls: set[str] = set()
    for node in tree.body:
        if isinstance(node, (ast.ClassDef, ast.FunctionDef, ast.AsyncFunctionDef)):
            decls.add(node.name)
        elif isinstance(node, ast.Assign):
            for target in node.targets:
                if isinstance(target, ast.Name):
                    decls.add(target.id)
        elif isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name):
            decls.add(node.target.id)
    return decls


def test_every_init_reexport_is_declared_in_stub() -> None:
    """The stub must cover every name imported from ``._net`` in
    ``__init__.py``. Catches drift when a new PyO3 export lands
    without the matching stub addition."""
    init_path = PKG_ROOT / "__init__.py"
    pyi_path = PKG_ROOT / "_net.pyi"
    assert init_path.exists(), f"Missing {init_path}"
    assert pyi_path.exists(), f"Missing {pyi_path}"

    reexported = _collect_reexported_names(init_path)
    declared = _collect_stub_decls(pyi_path)

    # Some symbols are re-exported under an alias from __init__ but
    # the underlying ``_net`` name doesn't always match the public
    # one. The set below is the documented allow-list of legitimate
    # alias-but-no-direct-stub-declaration cases.
    # Add entries here when a re-export legitimately has no matching
    # stub class (e.g. Rust-side renamed alias).
    aliased_only: set[str] = set()

    missing = reexported - declared - aliased_only
    assert not missing, (
        f"Stub _net.pyi missing declarations for {sorted(missing)}. "
        "Either add a class/function declaration to the stub, or, "
        "if the symbol is re-exported under an alias from the "
        "Rust crate, add it to the aliased_only allow-list above."
    )


def test_stub_covers_the_documented_families() -> None:
    """Quick sanity check: every major family advertised in the
    survey doc (MeshOS / MeshDB / Dataforts / Compute / Groups /
    Deck) has at least one class declared in the stub."""
    pyi_path = PKG_ROOT / "_net.pyi"
    declared = _collect_stub_decls(pyi_path)

    families = {
        "MeshOS": "MeshOsDaemonSdk",
        "MeshDB": "MeshQuery",
        "Dataforts": "BlobRef",
        "Compute": "DaemonRuntime",
        "Groups": "ReplicaGroup",
        "Deck": "DeckClient",
        "Redis-dedup": "RedisStreamDedup",
    }
    for family, sentinel in families.items():
        assert sentinel in declared, (
            f"{family} family unreachable from the stub: "
            f"expected {sentinel!r} to be declared"
        )
