#!/usr/bin/env python3
"""Named witnesses must have RUN and PASSED in a real nextest run.

WHY THIS EXISTS. Several CI steps pin security- and ordering-critical tests
by name. The original shape proved that by re-invoking cargo once per name
(`cargo nextest run ... -E 'test(=<name>)'`, 58 times in `unit-tests`, 27 + 22
in `rust-sdk-tests`, ~33 in `integration-cortex`, and 87 `cargo nextest list`
calls in `webrtc-feature`). That is correct but it re-walks the build graph
and spawns a runner per name, and the suite it is re-checking has already run
those same tests in the same job.

This checker replaces the fan-out with one verified RESULT SET. It reads the
JUnit XML nextest writes for a run (`[profile.default.junit]` in
`net/crates/net/.config/nextest.toml`, stored at
`target/nextest/<profile>/junit.xml`) and proves, from that one artifact:

  1. IDENTITY — each required name is present exactly once, as a `<testcase>`
     of the named `<testsuite>`, matched on the full test path with no
     substring or prefix latitude. `<name>_extra` is a different string.
  2. EXECUTION AND VERDICT — a listed name is not enough: the testcase must
     carry no `<failure>`, `<error>`, `<skipped>` or `<rerunFailure>` child,
     and the suite's own `failures`/`errors` attributes must be zero. A test
     that was filtered out, skipped or that failed is simply not in the
     artifact as a passing case.
  3. NO RETRY WAS CONSUMED — a `<flakyFailure>` child means nextest re-ran a
     test that had failed. For the suites this guards, a flake IS the defect
     (`.config/nextest.toml` keeps them at `retries = 0` for exactly that
     reason), so a witness that only passed on attempt 2 is rejected here
     rather than being laundered into a green run. `--allow-flaky` opts out
     where a suite legitimately rides the default retry budget.
  4. NO SILENT SHRINK — `--min` is the floor on executed cases in the suite,
     the same anti-vacuity guard the per-name loops carried. The floor is
     checked against COUNTED `<testcase>` elements and cross-checked against
     the suite's `tests` attribute, so a doctored attribute cannot satisfy it.

Fail-closed on every axis: a missing file, an unparseable file, a suite that
is absent, a zero-case suite, or a required name that is absent all exit 1.
The caller is still responsible for checking the runner's own exit status and
for deleting a stale artifact before the run; `--run-marker` makes the second
part enforceable here too.

Self-test: `check-witness-results.py --self-test` drives every rejection path
against synthetic XML, so the predicates are proven per run rather than
asserted.
"""

from __future__ import annotations

import argparse
import sys
import tempfile
import xml.etree.ElementTree as ET
from pathlib import Path

# Children that mean "this case did not simply pass".
_BAD_CHILDREN = ("failure", "error", "skipped", "rerunFailure")
_FLAKY_CHILD = "flakyFailure"


class WitnessError(Exception):
    """A verification failure with a rendered CI error message."""


def _error(message: str, *detail: str) -> None:
    print(f"::error::{message}")
    for line in detail:
        print(line)


def load_suite(junit: Path, suite: str | None) -> ET.Element:
    """The one `<testsuite>` to verify, or raise."""
    if not junit.is_file():
        raise WitnessError(
            f"no JUnit artifact at {junit} — the run did not produce results",
            "nextest writes it only when a run executes; check the runner's",
            "own exit status and that the step deleted a stale copy first.",
        )
    try:
        root = ET.parse(junit).getroot()
    except ET.ParseError as exc:  # unparseable == unverified
        raise WitnessError(f"could not parse {junit}: {exc}") from exc

    suites = root.findall("testsuite") if root.tag == "testsuites" else [root]
    if suite is not None:
        # nextest names a suite `<package>::<binary>` (and a lib suite just
        # `<package>`). Accept either the full name or the bare binary, but
        # only when the bare form is unambiguous — a name that matches two
        # suites is a caller error, not something to resolve by guessing.
        exact = [s for s in suites if s.get("name") == suite]
        short = [s for s in suites if (s.get("name") or "").split("::")[-1] == suite]
        suites = exact or short
        if not suites:
            present = ", ".join(sorted(s.get("name") or "?" for s in root)) or "<none>"
            raise WitnessError(
                f"no testsuite named {suite!r} in {junit}",
                f"suites present: {present}",
                "A suite is absent when its binary did not run at all — a",
                "renamed test target or a feature set that compiled it away.",
            )
    if len(suites) != 1:
        raise WitnessError(
            f"expected exactly one testsuite in {junit}, found {len(suites)}",
            "Pass --suite to name the binary whose witnesses are being verified.",
        )
    return suites[0]


def verify(
    suite_el: ET.Element,
    required: list[str],
    minimum: int,
    allow_flaky: bool,
) -> list[str]:
    """Every failure found, as rendered lines. Empty means verified."""
    failures: list[str] = []
    name = suite_el.get("name") or "?"
    cases = suite_el.findall("testcase")

    counted = len(cases)
    if counted == 0:
        failures.append(f"suite {name!r} executed no tests at all")

    declared = suite_el.get("tests")
    if declared is not None and declared.isdigit() and int(declared) != counted:
        failures.append(
            f"suite {name!r} declares tests={declared} but carries {counted} "
            "testcase element(s) — the artifact disagrees with itself"
        )

    for attr in ("failures", "errors"):
        value = suite_el.get(attr)
        if value is not None and value.isdigit() and int(value) != 0:
            failures.append(f"suite {name!r} reports {attr}={value}")

    if counted < minimum:
        failures.append(
            f"suite {name!r} executed {counted} test(s), below the floor of {minimum} "
            "— if witnesses were intentionally retired, lower the floor in the "
            "same commit rather than letting the gate pass vacuously"
        )

    by_name: dict[str, list[ET.Element]] = {}
    for case in cases:
        by_name.setdefault(case.get("name") or "", []).append(case)

    for case_name, hits in sorted(by_name.items()):
        for case in hits:
            for child in _BAD_CHILDREN:
                if case.find(child) is not None:
                    failures.append(f"{name}::{case_name} carries <{child}>")
            if not allow_flaky and case.find(_FLAKY_CHILD) is not None:
                failures.append(
                    f"{name}::{case_name} passed only after a retry "
                    "(<flakyFailure>) — this suite's regression signal is the "
                    "interleaving itself, so a retried pass is a defect"
                )

    for want in required:
        hits = by_name.get(want, [])
        if len(hits) != 1:
            failures.append(
                f"required witness {name}::{want} did not run exactly once — matched "
                f"{len(hits)} executed testcase(s)"
            )

    return failures


def read_required(args: argparse.Namespace) -> list[str]:
    names: list[str] = list(args.name)
    if args.required_file:
        text = (
            sys.stdin.read()
            if args.required_file == "-"
            else Path(args.required_file).read_text(encoding="utf-8")
        )
        names += [
            line.strip()
            for line in text.splitlines()
            if line.strip() and not line.strip().startswith("#")
        ]
    seen: dict[str, int] = {}
    for n in names:
        seen[n] = seen.get(n, 0) + 1
    dupes = sorted(n for n, c in seen.items() if c > 1)
    if dupes:
        raise WitnessError(
            "the required roster names the same witness twice: " + ", ".join(dupes),
            "A duplicated pin hides a retirement — the roster is the record.",
        )
    return names


def parse_floors(spec: list[str]) -> dict[str, int]:
    """`--floor rtc_loopback=5` entries, as a suite -> floor map."""
    floors: dict[str, int] = {}
    for item in spec:
        for part in item.split(","):
            part = part.strip()
            if not part:
                continue
            suite, _, value = part.partition("=")
            if not suite or not value.isdigit():
                raise WitnessError(f"malformed --floor entry: {part!r} (want suite=N)")
            floors[suite] = int(value)
    return floors


def check_multi(args: argparse.Namespace) -> int:
    """Verify MANY suites from one run's result set.

    Required names are `<suite>:<test>`; floors come from `--floor`. Every
    suite that carries a floor or a pin must be present in the artifact, so a
    binary that vanished from the run is an error rather than a silent pass —
    the same property the per-binary `nextest list` calls used to give, taken
    from EXECUTED results instead of a non-executing inventory.
    """
    try:
        entries = read_required(args)
        floors = parse_floors(args.floor)
    except WitnessError as exc:
        _error(str(exc.args[0]), *exc.args[1:])
        return 1

    junit = Path(args.junit)
    if not junit.is_file():
        _error(f"no JUnit artifact at {junit} — the run did not produce results")
        return 1
    try:
        root = ET.parse(junit).getroot()
    except ET.ParseError as exc:
        _error(f"could not parse {junit}: {exc}")
        return 1

    suites: dict[str, ET.Element] = {}
    for el in root.findall("testsuite"):
        full = el.get("name") or ""
        suites[full] = el
        suites.setdefault(full.split("::")[-1], el)

    per_suite: dict[str, list[str]] = {}
    malformed = [e for e in entries if ":" not in e]
    if malformed:
        _error(
            "--multi requires every required entry to be <suite>:<test>",
            *malformed,
        )
        return 1
    for entry in entries:
        suite, _, test = entry.partition(":")
        per_suite.setdefault(suite, []).append(test)

    failures: list[str] = []
    verified = 0
    for suite in sorted(set(per_suite) | set(floors)):
        el = suites.get(suite)
        if el is None:
            failures.append(
                f"suite {suite!r} is absent from the run — the binary did not "
                "execute at all (renamed target, or a feature set that "
                "compiled it away)"
            )
            continue
        names = per_suite.get(suite, [])
        found = verify(el, names, floors.get(suite, 0), args.allow_flaky)
        failures.extend(found)
        if not found:
            executed = len(el.findall("testcase"))
            verified += len(names)
            print(
                f"  {el.get('name')}: {executed} executed and passed, "
                f"{len(names)} pinned, floor {floors.get(suite, 0)}"
            )

    if failures:
        _error(f"witness verification failed in {junit}", *failures)
        return 1
    print(f"{len(set(per_suite) | set(floors))} suite(s) verified, {verified} pinned by name")
    return 0


def check(args: argparse.Namespace) -> int:
    try:
        required = read_required(args)
        suite_el = load_suite(Path(args.junit), args.suite)
    except WitnessError as exc:
        _error(str(exc.args[0]), *exc.args[1:])
        return 1

    if args.run_marker:
        marker = Path(args.run_marker)
        if not marker.is_file():
            _error(f"run marker {marker} is missing — cannot prove the artifact is fresh")
            return 1
        if Path(args.junit).stat().st_mtime < marker.stat().st_mtime:
            _error(
                f"{args.junit} is older than {marker} — this is a stale artifact "
                "from an earlier step, not this run's result set"
            )
            return 1

    failures = verify(suite_el, required, args.min, args.allow_flaky)
    if failures:
        _error(
            f"witness verification failed for suite {suite_el.get('name')!r}",
            *failures,
        )
        return 1

    executed = len(suite_el.findall("testcase"))
    print(
        f"{suite_el.get('name')}: {executed} test(s) executed and passed, "
        f"{len(required)} pinned by name, floor {args.min}"
    )
    return 0


# --------------------------------------------------------------------------
# Self-test: every rejection path, against synthetic XML.
# --------------------------------------------------------------------------

_PASS_XML = """<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="nextest-run" tests="3" failures="0" errors="0">
  <testsuite name="sensing_consumer" tests="3" skipped="0" errors="0" failures="0">
    <testcase name="alpha" classname="sensing_consumer" time="0.1"/>
    <testcase name="beta" classname="sensing_consumer" time="0.1"/>
    <testcase name="gamma" classname="sensing_consumer" time="0.1"/>
  </testsuite>
</testsuites>
"""


def _suite_from(text: str, name: str | None = "sensing_consumer") -> ET.Element:
    with tempfile.TemporaryDirectory() as tmp:
        path = Path(tmp) / "junit.xml"
        path.write_text(text, encoding="utf-8")
        return load_suite(path, name)


def self_test() -> int:
    print("==> self-test")
    bad: list[str] = []

    def expect(label: str, condition: bool) -> None:
        print(f"  {'ok  ' if condition else 'FAIL'} {label}")
        if not condition:
            bad.append(label)

    ok_suite = _suite_from(_PASS_XML)
    expect(
        "a clean run with every pin present verifies",
        verify(ok_suite, ["alpha", "beta"], 3, False) == [],
    )
    expect(
        "a missing pin is rejected",
        any("delta" in f for f in verify(ok_suite, ["delta"], 3, False)),
    )
    expect(
        "a substring of a real name is NOT accepted",
        any("alph" in f for f in verify(ok_suite, ["alph"], 3, False)),
    )
    expect(
        "a floor above the executed count is rejected",
        any("below the floor" in f for f in verify(ok_suite, ["alpha"], 4, False)),
    )

    failed = _PASS_XML.replace(
        '<testcase name="beta" classname="sensing_consumer" time="0.1"/>',
        '<testcase name="beta" classname="sensing_consumer" time="0.1">'
        '<failure message="boom"/></testcase>',
    )
    expect(
        "a <failure> child is rejected even when the pin is present",
        any("<failure>" in f for f in verify(_suite_from(failed), ["beta"], 3, False)),
    )

    skipped = _PASS_XML.replace(
        '<testcase name="gamma" classname="sensing_consumer" time="0.1"/>',
        '<testcase name="gamma" classname="sensing_consumer" time="0.1">'
        '<skipped/></testcase>',
    )
    expect(
        "a skipped case is not an executed witness",
        any("<skipped>" in f for f in verify(_suite_from(skipped), ["gamma"], 3, False)),
    )

    flaky = _PASS_XML.replace(
        '<testcase name="alpha" classname="sensing_consumer" time="0.1"/>',
        '<testcase name="alpha" classname="sensing_consumer" time="0.1">'
        '<flakyFailure message="first attempt"/></testcase>',
    )
    expect(
        "a retried pass is rejected by default",
        any("after a retry" in f for f in verify(_suite_from(flaky), ["alpha"], 3, False)),
    )
    expect(
        "--allow-flaky accepts the same artifact",
        verify(_suite_from(flaky), ["alpha"], 3, True) == [],
    )

    lying = _PASS_XML.replace('tests="3" skipped="0"', 'tests="9" skipped="0"')
    expect(
        "a tests= attribute that disagrees with the elements is rejected",
        any("disagrees with itself" in f for f in verify(_suite_from(lying), [], 3, False)),
    )

    reported = _PASS_XML.replace(
        'tests="3" skipped="0" errors="0" failures="0"',
        'tests="3" skipped="0" errors="0" failures="1"',
    )
    expect(
        "a suite-level failures= count is rejected",
        any("reports failures=1" in f for f in verify(_suite_from(reported), [], 3, False)),
    )

    empty = """<?xml version="1.0"?><testsuites><testsuite name="sensing_consumer" tests="0"/></testsuites>"""
    expect(
        "a suite that executed nothing is rejected",
        any("executed no tests" in f for f in verify(_suite_from(empty), [], 0, False)),
    )

    try:
        _suite_from(_PASS_XML, "org_exact_sensing")
        expect("an absent suite raises", False)
    except WitnessError:
        expect("an absent suite raises", True)

    try:
        with tempfile.TemporaryDirectory() as tmp:
            load_suite(Path(tmp) / "nope.xml", "sensing_consumer")
        expect("a missing artifact raises", False)
    except WitnessError:
        expect("a missing artifact raises", True)

    try:
        _suite_from("<testsuites><not-closed>")
        expect("an unparseable artifact raises", False)
    except WitnessError:
        expect("an unparseable artifact raises", True)

    args = argparse.Namespace(name=["alpha", "alpha"], required_file=None)
    try:
        read_required(args)
        expect("a duplicated pin raises", False)
    except WitnessError:
        expect("a duplicated pin raises", True)

    multi_xml = """<?xml version="1.0"?>
<testsuites>
  <testsuite name="net-mesh::rtc_admission" tests="2" skipped="0" errors="0" failures="0">
    <testcase name="a_promotion_inside_the_teardown_window_survives" classname="x"/>
    <testcase name="pingwave_is_denied_while_heartbeat_is_permitted" classname="x"/>
  </testsuite>
  <testsuite name="net-mesh::rtc_classifier" tests="1" skipped="0" errors="0" failures="0">
    <testcase name="an_ice_pair_schedules_the_upgrade_attempt" classname="x"/>
  </testsuite>
</testsuites>
"""

    def run_multi(entries: list[str], floors: list[str]) -> int:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "junit.xml"
            path.write_text(multi_xml, encoding="utf-8")
            return check_multi(
                argparse.Namespace(
                    junit=str(path),
                    name=entries,
                    required_file=None,
                    floor=floors,
                    allow_flaky=False,
                )
            )

    expect(
        "multi: pins across two suites with satisfied floors verify",
        run_multi(
            [
                "rtc_admission:a_promotion_inside_the_teardown_window_survives",
                "rtc_classifier:an_ice_pair_schedules_the_upgrade_attempt",
            ],
            ["rtc_admission=2,rtc_classifier=1"],
        )
        == 0,
    )
    expect(
        "multi: a floor above the executed count is rejected",
        run_multi([], ["rtc_classifier=2"]) == 1,
    )
    expect(
        "multi: a suite absent from the run is rejected",
        run_multi([], ["rtc_loopback=1"]) == 1,
    )
    expect(
        "multi: a pin naming a test that did not run is rejected",
        run_multi(["rtc_admission:a_name_that_was_renamed"], []) == 1,
    )
    expect(
        "multi: a pin in the wrong suite is rejected",
        run_multi(["rtc_classifier:pingwave_is_denied_while_heartbeat_is_permitted"], [])
        == 1,
    )

    if bad:
        _error(f"self-test failed: {len(bad)} predicate(s) wrong", *bad)
        return 1
    print("self-test: every rejection path fires")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--junit", help="path to nextest's JUnit XML")
    parser.add_argument("--suite", help="testsuite (test binary) name to verify")
    parser.add_argument(
        "--min", type=int, default=0, help="floor on executed tests in the suite"
    )
    parser.add_argument(
        "--required-file",
        help="file with one required test name per line, or - for stdin",
    )
    parser.add_argument(
        "--allow-flaky",
        action="store_true",
        help="accept a witness that passed only after a retry",
    )
    parser.add_argument(
        "--run-marker",
        help="file touched immediately before the run; the artifact must be newer",
    )
    parser.add_argument(
        "--multi",
        action="store_true",
        help="verify many suites from one run; required entries are <suite>:<test>",
    )
    parser.add_argument(
        "--floor",
        action="append",
        default=[],
        help="per-suite floor, `suite=N` (repeatable, comma-separated)",
    )
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("name", nargs="*", help="required test names")
    args = parser.parse_args()

    if args.self_test:
        return self_test()
    if not args.junit:
        parser.error("--junit is required unless --self-test is given")
    return check_multi(args) if args.multi else check(args)


if __name__ == "__main__":
    sys.exit(main())
