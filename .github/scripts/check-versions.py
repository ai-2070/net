#!/usr/bin/env python3
"""Every published artifact of this repository carries ONE version.

THE SOURCE OF TRUTH is `[workspace.package] version` in
`net/crates/net/Cargo.toml`. Every workspace crate inherits it
(`version.workspace = true`), so a bump is one line there -- plus the
places a version has to be written out because the ecosystem cannot inherit
it: npm `package.json`, Python `pyproject.toml` and `__version__`, the
standalone leaf crate, and every `version = "..."` on a path dependency onto
one of our own crates (crates.io requires those when publishing).

This check fails on any of them that disagrees. It exists because a manual
bump missed exactly one of those dependency lines -- `net-rpc-ffi` kept
`net-mesh = "0.36.0"` while the crate said 0.37.0 -- and the workspace
stopped resolving. A global find-and-replace is the other way to get this
wrong: it also rewrites history and fixtures that merely mention a version.
So this check reads each field structurally and names every disagreement.

What it checks (W = the workspace version):

  * every workspace member: `version.workspace = true`, or exactly W;
  * the published standalone crates (`PUBLISHED_STANDALONE_CRATES`): W;
  * every tracked `Cargo.toml`: a dependency on one of our published crates
    that states a `version` requires exactly W (`^`/`=`/`~` stripped);
  * the published npm packages: `version` is W, and every `@net-mesh/*`
    entry in `dependencies` / `optionalDependencies` / `peerDependencies`
    names W (a `file:` / `workspace:` link is not a version and is skipped);
  * the published Python projects: `project.version` is W, and a dependency
    on one of our Python distributions has lower bound W and, when capped,
    the cap is the next minor;
  * the `__version__` strings: W.

The install pages are deliberately NOT here: they name the newest release
TAG, not the next candidate (`check-install-version.py`).

Usage: check-versions.py [--self-test]
"""

from __future__ import annotations

import json
import re
import subprocess
import sys
from pathlib import Path

try:
    import tomllib
except ModuleNotFoundError:  # pragma: no cover - Python < 3.11
    sys.stderr.write("check-versions.py needs Python 3.11+ (tomllib)\n")
    sys.exit(2)

ROOT = Path(__file__).resolve().parents[2]
CRATES = ROOT / "net" / "crates" / "net"

# Crates published from outside the workspace (they cannot inherit).
PUBLISHED_STANDALONE_CRATES = ["leaf/Cargo.toml"]

PUBLISHED_NPM = [
    "bindings/node/package.json",
    "browser-ts/package.json",
    "cli/npm/package.json",
    "deck/npm/package.json",
    "sdk-ts/package.json",
]

PUBLISHED_PYPROJECT = [
    "bindings/python/pyproject.toml",
    "sdk-py/pyproject.toml",
    "cli/python/pyproject.toml",
    "deck/python/pyproject.toml",
]

DUNDER_VERSION = [
    "bindings/python/python/net/__init__.py",
    "sdk-py/src/net_sdk/__init__.py",
]

_DEP_TABLES = ("dependencies", "dev-dependencies", "build-dependencies")
_VERSION_TOKEN = re.compile(r"\d+\.\d+(?:\.\d+)?")


def workspace_version() -> str:
    doc = tomllib.loads((CRATES / "Cargo.toml").read_text(encoding="utf-8"))
    version = doc.get("workspace", {}).get("package", {}).get("version")
    if not isinstance(version, str):
        raise SystemExit("net/crates/net/Cargo.toml has no [workspace.package] version")
    return version


def next_minor(version: str) -> str:
    major, minor, *_ = (int(p) for p in version.split("."))
    return f"{major}.{minor + 1}.0"


def requirement_version(req: str) -> str | None:
    """The version a single Cargo requirement names (`^0.37.0` -> `0.37.0`)."""
    m = re.fullmatch(r"\s*[\^=~]?\s*(\d+\.\d+(?:\.\d+)?)\s*", req)
    return m.group(1) if m else None


def dependency_tables(doc: dict):
    """Every dependency table in a manifest, target-specific ones included."""
    for key in _DEP_TABLES:
        if isinstance(doc.get(key), dict):
            yield key, doc[key]
    for target, body in (doc.get("target") or {}).items():
        for key in _DEP_TABLES:
            if isinstance(body.get(key), dict):
                yield f"target.{target}.{key}", body[key]
    if isinstance(doc.get("workspace", {}).get("dependencies"), dict):
        yield "workspace.dependencies", doc["workspace"]["dependencies"]


def tracked(pattern: str) -> list[Path]:
    out = subprocess.run(
        ["git", "ls-files", pattern], cwd=ROOT, capture_output=True, text=True, check=True
    ).stdout.split()
    return [ROOT / p for p in out if "node_modules" not in p]


def check(w: str) -> list[str]:
    problems: list[str] = []
    rel = lambda p: p.relative_to(ROOT).as_posix()  # noqa: E731

    # --- Rust: the workspace members and the published crate names.
    root_doc = tomllib.loads((CRATES / "Cargo.toml").read_text(encoding="utf-8"))
    published: set[str] = set()
    for member in root_doc["workspace"]["members"]:
        manifest = CRATES / member / "Cargo.toml"
        pkg = tomllib.loads(manifest.read_text(encoding="utf-8"))["package"]
        published.add(pkg["name"])
        v = pkg.get("version")
        if v != {"workspace": True} and v != w:
            problems.append(
                f"{rel(manifest)}: version is {v!r}; use `version.workspace = true` (W = {w})"
            )
    for path in PUBLISHED_STANDALONE_CRATES:
        manifest = CRATES / path
        pkg = tomllib.loads(manifest.read_text(encoding="utf-8"))["package"]
        published.add(pkg["name"])
        if pkg.get("version") != w:
            problems.append(f"{rel(manifest)}: version is {pkg.get('version')!r}, not {w}")

    for manifest in tracked("*Cargo.toml"):
        doc = tomllib.loads(manifest.read_text(encoding="utf-8"))
        for table, deps in dependency_tables(doc):
            for key, spec in deps.items():
                if not isinstance(spec, dict):
                    spec = {"version": spec}
                crate = spec.get("package", key)
                if crate not in published or "version" not in spec:
                    continue
                named = requirement_version(spec["version"])
                if named != w:
                    problems.append(
                        f"{rel(manifest)}: [{table}] {key} (= {crate}) requires "
                        f"{spec['version']!r}, not {w}"
                    )

    # --- npm.
    for path in PUBLISHED_NPM:
        manifest = CRATES / path
        doc = json.loads(manifest.read_text(encoding="utf-8"))
        if doc.get("version") != w:
            problems.append(f"{rel(manifest)}: version is {doc.get('version')!r}, not {w}")
        for table in ("dependencies", "optionalDependencies", "peerDependencies"):
            for name, spec in (doc.get(table) or {}).items():
                if not name.startswith("@net-mesh/") or spec.startswith(("file:", "workspace:")):
                    continue
                tokens = _VERSION_TOKEN.findall(spec)
                if not tokens or tokens[0] != w:
                    problems.append(f"{rel(manifest)}: {table}.{name} is {spec!r}, not {w}")

    # --- Python.
    python_names: set[str] = set()
    docs = {}
    for path in PUBLISHED_PYPROJECT:
        manifest = CRATES / path
        doc = tomllib.loads(manifest.read_text(encoding="utf-8"))
        docs[manifest] = doc
        python_names.add(doc["project"]["name"])
    for manifest, doc in docs.items():
        project = doc["project"]
        if project.get("version") != w:
            problems.append(f"{rel(manifest)}: version is {project.get('version')!r}, not {w}")
        deps = list(project.get("dependencies", []))
        for extra in (project.get("optional-dependencies") or {}).values():
            deps.extend(extra)
        for dep in deps:
            m = re.match(r"\s*([A-Za-z0-9_.\-]+)\s*(.*)", dep)
            if not m or m.group(1) not in python_names:
                continue
            spec = m.group(2)
            lower = re.search(r"(?:>=|==|~=)\s*(\d+\.\d+(?:\.\d+)?)", spec)
            upper = re.search(r"<\s*(\d+\.\d+(?:\.\d+)?)", spec)
            if not lower or lower.group(1) != w:
                problems.append(f"{rel(manifest)}: dependency {dep!r} does not start at {w}")
            if upper and upper.group(1) != next_minor(w):
                problems.append(
                    f"{rel(manifest)}: dependency {dep!r} caps at {upper.group(1)}, "
                    f"not {next_minor(w)}"
                )

    for path in DUNDER_VERSION:
        source = (CRATES / path).read_text(encoding="utf-8")
        m = re.search(r'^__version__\s*=\s*["\']([^"\']+)["\']', source, re.M)
        if not m or m.group(1) != w:
            got = m.group(1) if m else "missing"
            problems.append(f"{rel(CRATES / path)}: __version__ is {got!r}, not {w}")

    return problems


def self_test() -> int:
    failures = []
    for req, want in [
        ("0.37.0", "0.37.0"),
        ("^0.37.0", "0.37.0"),
        ("=0.37.0", "0.37.0"),
        (" ~0.37 ", "0.37"),
        (">=0.37.0", None),
        ("0.37.0, <0.38", None),
    ]:
        got = requirement_version(req)
        if got != want:
            failures.append(f"requirement_version({req!r}) = {got!r}, expected {want!r}")
    if next_minor("0.37.0") != "0.38.0" or next_minor("1.9.3") != "1.10.0":
        failures.append("next_minor is wrong")
    for f in failures:
        print(f"FAIL  {f}")
    print("self-test ok" if not failures else f"self-test: {len(failures)} failure(s)")
    return 1 if failures else 0


def main(argv: list[str]) -> int:
    if argv[1:] == ["--self-test"]:
        return self_test()
    w = workspace_version()
    problems = check(w)
    if problems:
        print(f"Version check failed (workspace version {w}):")
        for p in problems:
            print(f"  - {p}")
        return 1
    print(f"All published versions agree: {w}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
