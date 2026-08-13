#!/usr/bin/env python3
"""The wheel CI tests must be the wheel users install.

`net-mesh`'s crate defaults omit `org` and `deck`. The release workflow
built with `--features redis,extension-module`, so published wheels had
neither — while the capability record marked Python organization auth
and Deck `supported`, the v0.34 release notes claimed the authority
verbs in five languages, and the SDK README said PyPI wheels ship every
feature enabled.

Nothing caught it, because CI's own `maturin develop` enabled both
explicitly. The tested artifact and the published artifact had
different surfaces, and every test was green against the one nobody
installs.

This compares the two feature lists and fails when the release build
would omit something CI tested. CI may enable *more* — it builds
`--no-default-features` and has to name defaults explicitly — so the
check is a subset test, not equality, computed over resolved features.

Run locally:  .github/scripts/check-python-wheel-features.py
"""

from __future__ import annotations

import re
import sys
import tomllib
from pathlib import Path

CARGO = Path("net/crates/net/bindings/python/Cargo.toml")
RELEASE_WF = Path(".github/workflows/release-python.yml")
CI_WF = Path(".github/workflows/ci.yml")

# `PY_RELEASE_FEATURES: redis,org,deck,extension-module`
RELEASE_RE = re.compile(r"^\s*PY_RELEASE_FEATURES:\s*(\S+)\s*$", re.M)
# `maturin develop --no-default-features --features a,b,c`
CI_RE = re.compile(r"maturin develop\s+(?P<flags>[^\n]*)")

# Not a capability; it controls whether the cdylib links libpython.
BUILD_ONLY = {"extension-module"}


def crate_features() -> tuple[dict[str, list[str]], set[str]]:
    data = tomllib.loads(CARGO.read_text(encoding="utf-8"))
    table = data.get("features", {})
    return table, set(table.get("default", []))


def resolve(names: set[str], table: dict[str, list[str]]) -> set[str]:
    """Expand feature names through their transitive `features` deps."""
    seen: set[str] = set()
    stack = list(names)
    while stack:
        name = stack.pop()
        if name in seen:
            continue
        seen.add(name)
        for dep in table.get(name, []):
            # `dep:foo` and `pkg?/feat` are dependency activations, not
            # features of this crate.
            if "/" in dep or dep.startswith("dep:"):
                continue
            stack.append(dep)
    return seen


def main() -> int:
    table, defaults = crate_features()

    m = RELEASE_RE.search(RELEASE_WF.read_text(encoding="utf-8"))
    if not m:
        print(f"{RELEASE_WF}: no PY_RELEASE_FEATURES — the canonical set moved")
        return 1
    release_named = {f.strip() for f in m.group(1).split(",") if f.strip()}
    # The release build keeps crate defaults; CI does not.
    release = resolve(release_named | defaults, table) - BUILD_ONLY

    ci_text = CI_WF.read_text(encoding="utf-8")
    ci_named: set[str] = set()
    for match in CI_RE.finditer(ci_text):
        flags = match.group("flags")
        feat = re.search(r"--features\s+(\S+)", flags)
        if not feat:
            continue
        named = {f.strip() for f in feat.group(1).split(",") if f.strip()}
        if "--no-default-features" not in flags:
            named |= defaults
        ci_named |= named
    if not ci_named:
        print(f"{CI_WF}: no `maturin develop --features` found — the job moved")
        return 1
    ci = resolve(ci_named, table) - BUILD_ONLY

    missing = sorted(ci - release)
    if missing:
        print(
            f"{RELEASE_WF}: the published wheel would omit "
            f"{', '.join(missing)}, which CI builds and tests. Add them to "
            f"PY_RELEASE_FEATURES, or stop testing them."
        )
        return 1

    rc = check_profile(RELEASE_WF.read_text(encoding="utf-8"), ci_text)
    return rc or check_probe_absent(
        table, defaults, RELEASE_WF.read_text(encoding="utf-8"), ci_text
    )


#: A deliberately panicking export, compiled only under this feature.
#: It exists to prove what panic strategy a built extension has; it must
#: never reach a wheel anyone installs.
PROBE_FEATURE = "panic-probe"

#: The one job allowed to enable it, in its own workflow.
PROBE_JOB = "panic-probe-witness"
WITNESS_WF = Path(".github/workflows/panic-probe-witness.yml")

#: What that job must actually do. Finding a job by name proves nothing
#: — an empty job with the right name would satisfy it while the
#: extension's panic strategy went unmeasured.
REQUIRED_WITNESS_STEPS = {
    "an abort-profile probe build": "maturin build --release",
    "an unwind-profile probe build": "maturin build --profile python-release",
    "the abort assertion": "--expect abort",
    "the unwind assertion": "--expect unwind",
    "the ordinary-wheel absence assertion": "_panic_strategy_probe",
}


def witness_problems() -> list[str]:
    """The witness must exist and must do the five things it claims.

    A green run has to mean the same thing next year as it does today.
    """
    if not WITNESS_WF.is_file():
        return [
            f"{WITNESS_WF}: missing. The `{PROBE_FEATURE}` feature exists to be "
            f"exercised by this workflow; without it nothing measures the "
            f"shipped extension's panic strategy and this check guards an "
            f"unused feature."
        ]
    text = WITNESS_WF.read_text(encoding="utf-8")
    out: list[str] = []
    if PROBE_JOB not in text:
        out.append(f"{WITNESS_WF}: no `{PROBE_JOB}` job")
    for what, needle in REQUIRED_WITNESS_STEPS.items():
        if needle not in text:
            out.append(
                f"{WITNESS_WF}: no {what} (looked for {needle!r}). The witness "
                f"would pass while proving less than it claims."
            )
    # Triggers it declares must be triggers it can actually receive. The
    # previous version of this job sat in `ci.yml` behind an `if:`
    # naming `workflow_dispatch` and `schedule`, neither of which that
    # workflow fires — two thirds of the condition was dead.
    for event in ("workflow_dispatch", "schedule", "push"):
        if event not in text:
            out.append(
                f"{WITNESS_WF}: does not declare `{event}`. Two expensive "
                f"builds need a manual and a scheduled path, not only a "
                f"push-triggered one."
            )
    return out


def ci_jobs(text: str) -> dict[str, str]:
    """Split a workflow into `{job name: body}` by indentation.

    Deliberately textual: this runs before any toolchain setup, and
    depending on PyYAML for a check whose whole job is to not be
    bypassed would make it skippable on a machine without it.
    """
    jobs: dict[str, str] = {}
    current: str | None = None
    lines: list[str] = []
    in_jobs = False
    for line in text.splitlines():
        if line.startswith("jobs:"):
            in_jobs = True
            continue
        if not in_jobs:
            continue
        if line and not line.startswith(" ") and not line.startswith("#"):
            break  # a new top-level key ends the jobs block
        m = re.match(r"^  ([A-Za-z0-9_-]+):\s*$", line)
        if m:
            if current:
                jobs[current] = chr(10).join(lines)
            current = m.group(1)
            lines = []
            continue
        if current:
            lines.append(line)
    if current:
        jobs[current] = chr(10).join(lines)
    return jobs


def check_probe_absent(
    table: dict[str, list[str]],
    defaults: set[str],
    release_text: str,
    ci_text: str,
) -> int:
    """The panic probe must be absent from everything that ships.

    It is compile-time gated, so an absent feature means an absent
    symbol — but only if nothing quietly enables it. A default feature,
    a stray addition to `PY_RELEASE_FEATURES`, or one publish job out of
    three drifting would put a function whose entire purpose is to
    crash the interpreter into a published wheel.
    """
    problems: list[str] = []

    if PROBE_FEATURE in defaults:
        problems.append(
            f"{CARGO}: `{PROBE_FEATURE}` is a default feature. It compiles a "
            f"function that deliberately panics; default-on puts it in every wheel."
        )
    # A feature that transitively enables the probe is the same hazard.
    for name, deps in table.items():
        if name == PROBE_FEATURE:
            continue
        if PROBE_FEATURE in resolve({name}, table):
            problems.append(
                f"{CARGO}: feature `{name}` transitively enables "
                f"`{PROBE_FEATURE}`; the probe must be reachable only by naming "
                f"it explicitly."
            )

    m = RELEASE_RE.search(release_text)
    if m and PROBE_FEATURE in {f.strip() for f in m.group(1).split(",")}:
        problems.append(
            f"{RELEASE_WF}: PY_RELEASE_FEATURES contains `{PROBE_FEATURE}` — that "
            f"is the published wheel's feature set."
        )
    for build in re.findall(r"^\s*args:\s*(--\S+.*)$", release_text, re.M):
        if PROBE_FEATURE in build:
            problems.append(
                f"{RELEASE_WF}: a wheel build enables `{PROBE_FEATURE}`: "
                f"{build.strip()}"
            )

    # In CI the probe is allowed, but only inside the job that exists to
    # run it. Any other build enabling it is testing something users
    # cannot install.
    #
    # Scoped by job block rather than by line: the `--features` line that
    # legitimately names the probe does not itself mention the job.
    for job, body in ci_jobs(ci_text).items():
        if PROBE_FEATURE not in body or job == PROBE_JOB:
            continue
        offending = [
            ln.strip()
            for ln in body.splitlines()
            if PROBE_FEATURE in ln and not ln.lstrip().startswith("#")
        ]
        for ln in offending:
            problems.append(
                f"{CI_WF}: job `{job}` enables `{PROBE_FEATURE}`: {ln}"
            )

    problems.extend(witness_problems())

    for p in problems:
        print(p)
    return 1 if problems else 0


#: The Cargo profile the wheel must ship with. `release` sets
#: `panic = "abort"`, which for a Python extension means an internal
#: Rust panic kills the host process — a Jupyter kernel, a web worker —
#: with no traceback. `python-release` is `release` plus unwinding.
WHEEL_PROFILE = "python-release"


def check_profile(release_text: str, ci_text: str) -> int:
    """The wheel ships one profile and CI must test that same one.

    Features were not the only way these two drifted. The published
    wheel was built `--release` while CI tested a `maturin develop`
    build (debug, unwind), so the two disagreed about whether a panic is
    recoverable — and only the recoverable one was ever run. A tokio
    runtime dropped in an async context passed 884 tests that way, and
    nobody saw it.

    Stated as policy rather than history: a pyo3 extension under an
    effective abort profile kills its host process on an internal panic.
    The one panic actually observed on a `--release` wheel did *not*
    abort, so what that artifact's strategy really was — and whether it
    relates to the separate 0xC0000409 termination — is unestablished.
    The gate stands on the mismatch itself, which is not in doubt.
    """
    problems: list[str] = []

    builds = re.findall(r"^\s*args:\s*(--\S+.*)$", release_text, re.M)
    wheel_builds = [b for b in builds if "--out dist" in b and "--features" in b]
    if not wheel_builds:
        problems.append(
            f"{RELEASE_WF}: found no wheel build args — the publish jobs moved, "
            f"and this check is now inspecting nothing"
        )
    for b in wheel_builds:
        if f"--profile {WHEEL_PROFILE}" not in b:
            problems.append(
                f"{RELEASE_WF}: a wheel is built with `{b.strip()}` rather than "
                f"`--profile {WHEEL_PROFILE}`. `--release` gives the extension "
                f"`panic = \"abort\"`, so a Rust panic takes the host process down "
                f"with no traceback."
            )

    # CI has to build and test that same profile somewhere, or the
    # shipped configuration is untested no matter what the profile says.
    if f"--profile {WHEEL_PROFILE}" not in ci_text:
        problems.append(
            f"{CI_WF}: nothing builds `--profile {WHEEL_PROFILE}`. The wheel that "
            f"ships is then never exercised — a `maturin develop` build has a "
            f"different panic strategy."
        )

    for p in problems:
        print(p)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
