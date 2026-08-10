#!/usr/bin/env python3
"""The npm aggregate packages must actually deliver a binary.

`@net-mesh/cli` and `@net-mesh/deck` ship nothing themselves. Each is a
launcher plus a list of per-platform packages in `optionalDependencies`, and
the launcher execs whichever one npm installed. That arrangement has one
failure mode, and it is silent:

    $ npm install
    added 1 package, and audited 2 packages in 2s
    found 0 vulnerabilities
    [exit=0]

    $ ./node_modules/.bin/net-mesh --help
    net-mesh: Failed to locate @net-mesh/cli-win32-x64.
    [exit=127]

npm skips an optional dependency it cannot resolve WITHOUT failing the
install, so a missing platform package reports total success and only shows
up when someone runs the command. The user is told the install worked.

Three lists have to agree for that not to happen: the aggregate's
`optionalDependencies`, the triples the launcher knows how to resolve, and
the release workflow's build matrix. Drop a target from any one of them and
the other two keep claiming it exists.

Local mode (default) cross-checks those three from the tree — no network,
fast enough for every push. `--registry` additionally asks npm whether each
platform package is really published at the aggregate's version, which is the
check that belongs immediately before publishing the aggregate.

Usage:
  check-npm-platform-packages.py [--registry] [--self-test]
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

_ROOT = Path(__file__).resolve().parents[2]

# (label, aggregate package dir, launcher script, release workflow)
_AGGREGATES = [
    (
        "cli",
        _ROOT / "net" / "crates" / "net" / "cli" / "npm",
        _ROOT / "net" / "crates" / "net" / "cli" / "npm" / "bin" / "net-mesh.js",
        _ROOT / ".github" / "workflows" / "release-npm-cli.yml",
    ),
    (
        "deck",
        _ROOT / "net" / "crates" / "net" / "deck" / "npm",
        _ROOT / "net" / "crates" / "net" / "deck" / "npm" / "bin" / "net-deck.js",
        _ROOT / ".github" / "workflows" / "release-npm-deck.yml",
    ),
]

# `@net-mesh/cli-win32-x64` -> `win32-x64`
_SCOPED = re.compile(r"^@net-mesh/(?:cli|deck)-(.+)$")
# `npm-suffix: win32-x64`
_MATRIX_SUFFIX = re.compile(r"^\s*npm-suffix:\s*(\S+)\s*$", re.MULTILINE)


def _suffixes_from_optional_deps(pkg: dict) -> dict[str, str]:
    """triple -> declared version, from optionalDependencies."""
    out: dict[str, str] = {}
    for name, version in (pkg.get("optionalDependencies") or {}).items():
        m = _SCOPED.match(name)
        if m:
            out[m.group(1)] = version
    return out


def _suffixes_from_launcher(path: Path) -> set[str]:
    """Triples the launcher can resolve.

    The mapping is written as string literals — including the `${libc}`
    template for Linux — so read them out of the source rather than trying to
    execute the resolver for every platform.
    """
    text = path.read_text(encoding="utf-8")
    found: set[str] = set()
    for m in re.finditer(r"['\"`]@net-mesh/(?:cli|deck)-([a-z0-9-]+(?:\$\{libc\})?)['\"`]", text):
        suffix = m.group(1)
        if suffix.endswith("${libc}"):
            base = suffix[: -len("${libc}")]
            found.add(f"{base}gnu")
            found.add(f"{base}musl")
        else:
            found.add(suffix)
    return found


def _suffixes_from_workflow(path: Path) -> set[str]:
    return set(_MATRIX_SUFFIX.findall(path.read_text(encoding="utf-8")))


def _check_one(label: str, pkg_dir: Path, launcher: Path, workflow: Path) -> list[str]:
    problems: list[str] = []
    pkg = json.loads((pkg_dir / "package.json").read_text(encoding="utf-8"))
    version = pkg["version"]

    declared = _suffixes_from_optional_deps(pkg)
    if not declared:
        return [f"{label}: package.json declares no @net-mesh/{label}-* optionalDependencies"]

    launcher_triples = _suffixes_from_launcher(launcher)
    matrix_triples = _suffixes_from_workflow(workflow)
    if not launcher_triples:
        problems.append(f"{label}: parsed no triples out of {launcher.name} — the matcher is broken")
    if not matrix_triples:
        problems.append(f"{label}: parsed no npm-suffix entries out of {workflow.name}")

    declared_set = set(declared)

    for missing in sorted(declared_set - launcher_triples):
        problems.append(
            f"{label}: optionalDependencies has {missing}, but {launcher.name} "
            "cannot resolve it — npm would install a package the launcher never looks for"
        )
    for missing in sorted(launcher_triples - declared_set):
        problems.append(
            f"{label}: {launcher.name} resolves {missing}, but it is not in "
            "optionalDependencies — npm will never install it and the first run exits 127"
        )
    for missing in sorted(declared_set - matrix_triples):
        problems.append(
            f"{label}: optionalDependencies has {missing}, but {workflow.name} "
            "never builds it — the aggregate would publish naming a package that does not exist"
        )
    for missing in sorted(matrix_triples - declared_set):
        problems.append(
            f"{label}: {workflow.name} builds {missing}, but the aggregate does not "
            "depend on it — it would publish and never be installed"
        )

    for triple, declared_version in sorted(declared.items()):
        if declared_version != version:
            problems.append(
                f"{label}: depends on {triple} at {declared_version} but is itself "
                f"{version} — the versions are published together and must match"
            )

    return problems


def _check_registry() -> list[str]:
    """Ask npm whether each platform package really exists at that version."""
    problems: list[str] = []
    for label, pkg_dir, _launcher, _workflow in _AGGREGATES:
        pkg = json.loads((pkg_dir / "package.json").read_text(encoding="utf-8"))
        version = pkg["version"]
        for name in sorted((pkg.get("optionalDependencies") or {})):
            spec = f"{name}@{version}"
            proc = subprocess.run(
                ["npm", "view", spec, "version"],
                capture_output=True,
                text=True,
                shell=(sys.platform == "win32"),
            )
            if proc.returncode != 0 or not proc.stdout.strip():
                problems.append(
                    f"{label}: {spec} is not on the registry. Publishing the "
                    "aggregate now would produce an install that exits 0 and a "
                    "first command that exits 127."
                )
            else:
                print(f"  ok    {spec}")
    return problems


def _self_test() -> int:
    """The parsers must actually find things, and disagreement must be caught."""
    print("==> self-test")
    for label, pkg_dir, launcher, workflow in _AGGREGATES:
        pkg = json.loads((pkg_dir / "package.json").read_text(encoding="utf-8"))
        declared = _suffixes_from_optional_deps(pkg)
        from_launcher = _suffixes_from_launcher(launcher)
        from_matrix = _suffixes_from_workflow(workflow)
        for name, found in (
            ("optionalDependencies", set(declared)),
            (launcher.name, from_launcher),
            (workflow.name, from_matrix),
        ):
            if not found:
                print(f"FAIL  {label}: parsed nothing from {name}")
                return 1
        print(f"  ok    {label}: {len(declared)} triples parsed from all three sources")

    # Plant a disagreement and require it to be reported.
    label, pkg_dir, launcher, workflow = _AGGREGATES[0]
    pkg = json.loads((pkg_dir / "package.json").read_text(encoding="utf-8"))
    pkg["optionalDependencies"].pop("@net-mesh/cli-win32-x64", None)
    import tempfile

    with tempfile.TemporaryDirectory() as tmp:
        fake = Path(tmp)
        (fake / "package.json").write_text(json.dumps(pkg), encoding="utf-8")
        problems = _check_one(label, fake, launcher, workflow)

    if not any("win32-x64" in p for p in problems):
        print("FAIL  dropping a platform package from optionalDependencies was not reported")
        return 1
    print("  ok    a dropped platform package is reported")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--registry",
        action="store_true",
        help="also verify each platform package is published (needs network)",
    )
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()

    if args.self_test:
        return _self_test()

    print("==> npm aggregates: optionalDependencies vs launcher vs release matrix")
    problems: list[str] = []
    for label, pkg_dir, launcher, workflow in _AGGREGATES:
        found = _check_one(label, pkg_dir, launcher, workflow)
        if found:
            problems.extend(found)
        else:
            pkg = json.loads((pkg_dir / "package.json").read_text(encoding="utf-8"))
            n = len(_suffixes_from_optional_deps(pkg))
            print(f"  ok    {label}: {n} platform packages agree across all three")

    if args.registry:
        print("==> npm registry: every platform package is published")
        problems.extend(_check_registry())

    if problems:
        print(f"\n{len(problems)} problem(s):")
        for p in problems:
            print(f"  {p}")
        print(
            "\nAn aggregate whose platform package is missing installs with exit 0 "
            "and reports success. The absence only surfaces when someone runs the "
            "command and gets exit 127."
        )
        return 1

    return 0


if __name__ == "__main__":
    sys.exit(main())
