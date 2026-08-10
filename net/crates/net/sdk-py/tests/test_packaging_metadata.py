"""Packaging-metadata guards for the Python distributions.

`net-mesh-sdk` is a thin wrapper over the native `net-mesh` extension and
`web/src/content/docs/reference/versioning.md` requires the two to move
together. That policy is expressed as a version specifier in
`pyproject.toml`, and a specifier is easy to get *silently* wrong: the
0.35.0 candidate shipped `net-mesh>=0.35.0,<0.35.0`, an empty set. Nothing
imports it, no wheel build rejects it, and the only symptom is that every
`pip install net-mesh-sdk` fails in the resolver before a wheel is fetched.

These tests read the checked-in metadata directly rather than an installed
distribution, so they fail in CI on the commit that introduces the mistake.
"""

from __future__ import annotations

import tomllib
from pathlib import Path

import pytest
from packaging.requirements import Requirement
from packaging.version import Version

# tests/ -> sdk-py/ -> net/ (i.e. net/crates/net)
_SDK_PY = Path(__file__).resolve().parents[1]
_CRATE_ROOT = _SDK_PY.parent

# Every Python distribution built out of this crate.
_PYPROJECTS = {
    "net-mesh-sdk": _SDK_PY / "pyproject.toml",
    "net-mesh": _CRATE_ROOT / "bindings" / "python" / "pyproject.toml",
    "net-mesh-cli": _CRATE_ROOT / "cli" / "python" / "pyproject.toml",
    "net-deck": _CRATE_ROOT / "deck" / "python" / "pyproject.toml",
}


def _load(path: Path) -> dict:
    return tomllib.loads(path.read_text(encoding="utf-8"))


def _project(name: str) -> dict:
    path = _PYPROJECTS[name]
    if not path.is_file():
        pytest.skip(f"{name}: {path} is not present in this checkout")
    return _load(path)["project"]


def _requirement(project: dict, dist: str) -> Requirement:
    for raw in project.get("dependencies", []):
        req = Requirement(raw)
        if req.name == dist:
            return req
    raise AssertionError(
        f"{project['name']} no longer declares a dependency on {dist}; "
        "the wrapper must pin the native extension it wraps"
    )


def _probe_versions(requirement: Requirement) -> list[Version]:
    """Candidate versions to test a specifier against.

    Emptiness of an arbitrary specifier set is not decidable by
    inspection, but the failure mode we care about — bounds that exclude
    each other — is always witnessed near the bounds themselves. Probe
    every version named in the specifier plus its neighbours.
    """
    seen: set[Version] = set()
    for spec in requirement.specifier:
        try:
            base = Version(spec.version.rstrip(".*"))
        except Exception:  # noqa: BLE001 - non-version specifiers (URLs, etc.)
            continue
        major, minor, micro = base.major, base.minor, base.micro
        for candidate in (
            base,
            Version(f"{major}.{minor}.{micro + 1}"),
            Version(f"{major}.{minor + 1}.0"),
            Version(f"{major + 1}.0.0"),
        ):
            seen.add(candidate)
        if micro:
            seen.add(Version(f"{major}.{minor}.{micro - 1}"))
        if minor:
            seen.add(Version(f"{major}.{minor - 1}.0"))
    return sorted(seen)


@pytest.mark.parametrize("name", sorted(_PYPROJECTS))
def test_every_declared_requirement_is_satisfiable(name: str) -> None:
    """No dependency may declare a range that admits no version at all."""
    project = _project(name)
    for raw in project.get("dependencies", []):
        req = Requirement(raw)
        if not req.specifier:
            continue
        probes = _probe_versions(req)
        satisfied = [v for v in probes if req.specifier.contains(v, prereleases=True)]
        assert satisfied, (
            f"{name} declares {raw!r}, which admits no version near its own "
            f"bounds (probed {[str(v) for v in probes]}). A mutually "
            "exclusive lower/upper bound makes the package uninstallable."
        )


def test_sdk_pin_admits_the_native_extension_it_ships_with() -> None:
    """The wrapper's pin must accept the sibling extension's version.

    Both distributions are built from this commit, so a pin that excludes
    the sibling is unsatisfiable in practice even when the range itself is
    non-empty.
    """
    sdk = _project("net-mesh-sdk")
    native_version = Version(_project("net-mesh")["version"])
    req = _requirement(sdk, "net-mesh")

    assert req.specifier.contains(native_version, prereleases=True), (
        f"net-mesh-sdk {sdk['version']} pins {req}, which excludes the "
        f"net-mesh {native_version} built from this same commit"
    )


def test_sdk_pin_excludes_the_next_breaking_minor() -> None:
    """Pre-1.0 minors may break APIs, so the pin must stop at the next one."""
    sdk = _project("net-mesh-sdk")
    native_version = Version(_project("net-mesh")["version"])
    req = _requirement(sdk, "net-mesh")

    next_minor = Version(f"{native_version.major}.{native_version.minor + 1}.0")
    assert not req.specifier.contains(next_minor, prereleases=True), (
        f"net-mesh-sdk pins {req}, which would resolve against net-mesh "
        f"{next_minor}. Versioning policy says a pre-1.0 minor bump may "
        "break this wrapper, so the upper bound must exclude it."
    )
