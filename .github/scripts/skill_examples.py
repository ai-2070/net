#!/usr/bin/env python3
"""Read and validate `docs/data/examples.yaml`.

One place that knows which examples exist, so the shell checkers do not each
carry their own hardcoded list and drift apart. Three modes:

  --validate   structural checks; exit 1 with reasons
  --list       `<lang>\\t<repo-relative path>\\t<example id>` per present file
  --report     the human-readable coverage grid, with explicit absences

THE INVARIANT THIS FILE EXISTS FOR
Every binding must appear in either `files` or `absent` for every route. A
binding in neither is an error. Without that rule a route covering three
languages renders identically to one covering five, and the checker's green
tick means something different per route without saying so.

`absent` is not a to-do list. A route that simply has not been written for a
binding yet must say that in its reason, so "no example" never reads as "no
support" — the coverage matrices in `.claude/skills/*/bindings/coverage.md` are
where support is recorded, and the two answer different questions.
"""

import argparse
import os
import pathlib
import re
import subprocess
import sys

# Every check in this suite prints its verdict with U+2713 / U+2717, and some
# of the identifiers it echoes carry em-dashes. Python picks stdout's encoding
# from the platform, so on a cp1252 console those characters raise
# UnicodeEncodeError mid-report — the checker dies partway through and its
# caller sees a truncated run rather than a verdict. Force UTF-8 so the output
# is the same everywhere the checker runs.
if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8")
    sys.stderr.reconfigure(encoding="utf-8")

try:
    import yaml
except ModuleNotFoundError:  # pragma: no cover
    sys.exit("PyYAML is required: python3 -m pip install pyyaml")

ROOT = pathlib.Path(__file__).resolve().parents[2]
# The index moved out of `.github/` and became YAML. It is product metadata, not
# CI configuration: the docs transclude from the paths it names, and the skills
# consume it rather than owning it. YAML because the prose that explains a level
# or an absence belongs beside the entry, and JSON needed a fake `_comment` array
# to carry any of it.
MANIFEST = pathlib.Path(
    os.environ.get("EXAMPLES_MANIFEST", ROOT / "docs" / "data" / "examples.yaml")
)

# Extension per binding, so a manifest cannot list `hello.py` under `go`.
EXT = {
    "rust": ".rs",
    "typescript": ".ts",
    "python": ".py",
    "go": ".go",
    "c": ".c",
}


def load():
    return yaml.safe_load(MANIFEST.read_text(encoding="utf-8"))


def tracked():
    out = subprocess.run(
        ["git", "ls-files"], capture_output=True, text=True, cwd=ROOT
    ).stdout.split()
    return set(out)


def validate(m):
    problems = []
    bindings = m.get("bindings") or []
    if not bindings:
        problems.append("manifest has no `bindings` list")
        return problems

    unknown = [b for b in bindings if b not in EXT]
    if unknown:
        problems.append(f"unknown binding(s) in `bindings`: {', '.join(unknown)}")

    seen_ids = set()
    tr = tracked()

    for ex in m.get("examples") or []:
        eid = ex.get("id", "<no id>")
        where = f"example `{eid}`"
        if eid in seen_ids:
            problems.append(f"{where}: duplicate id")
        seen_ids.add(eid)

        for key in ("skill", "route", "dir"):
            if not ex.get(key):
                problems.append(f"{where}: missing `{key}`")

        # Evidence level. A level below `run` is a weaker claim about the same
        # example, so it has to be argued rather than chosen — otherwise levels
        # drift down to whatever was convenient that week.
        levels = set(m.get("levels") or [])
        level = ex.get("level")
        if not level:
            problems.append(f"{where}: missing `level` (one of {sorted(levels)})")
        elif level not in levels:
            problems.append(
                f"{where}: unknown level `{level}` (closed set: {sorted(levels)})"
            )
        overrides = ex.get("level_overrides") or {}
        reasons = ex.get("level_reasons") or {}
        for binding, lv in overrides.items():
            if binding not in bindings:
                problems.append(f"{where}: level override for unknown binding "
                                f"`{binding}`")
            if lv not in levels:
                problems.append(f"{where}: unknown level `{lv}` for `{binding}`")
        for binding, lv in sorted(overrides.items()):
            if lv != "run" and not reasons.get(binding):
                problems.append(
                    f"{where}: `{binding}` is at level `{lv}`, below `run`, with "
                    f"no `level_reasons.{binding}` — say why it is not executed"
                )
        if level and level != "run" and not ex.get("level_reason"):
            problems.append(
                f"{where}: level `{level}` is below `run` with no `level_reason`"
            )
        if level == "run" and not (ex.get("run") or {}).get("expect"):
            problems.append(
                f"{where}: level `run` needs a `run.expect` contract — executing "
                f"without asserting output proves only that it did not crash"
            )
        if level != "run" and ex.get("run"):
            problems.append(
                f"{where}: level `{level}` but a `run` block is present — one of "
                f"the two is wrong"
            )

        files = ex.get("files") or {}
        absent = ex.get("absent") or {}

        both = sorted(set(files) & set(absent))
        if both:
            problems.append(
                f"{where}: binding(s) listed as both present and absent: "
                f"{', '.join(both)}"
            )

        missing = [b for b in bindings if b not in files and b not in absent]
        if missing:
            problems.append(
                f"{where}: binding(s) in neither `files` nor `absent`: "
                f"{', '.join(missing)}. A route that covers some bindings must "
                f"say which ones it does not, or its green tick overstates it."
            )

        extra = [b for b in list(files) + list(absent) if b not in bindings]
        if extra:
            problems.append(f"{where}: binding(s) not in `bindings`: {', '.join(extra)}")

        for b, reason in absent.items():
            if not str(reason).strip():
                problems.append(
                    f"{where}: `absent.{b}` has no reason. An unexplained absence "
                    f"is indistinguishable from an oversight."
                )

        d = ex.get("dir", "")
        for b, name in files.items():
            if b in EXT and not str(name).endswith(EXT[b]):
                problems.append(
                    f"{where}: `files.{b}` is {name!r}, which is not a {EXT[b]} file"
                )
            rel = f"{d}/{name}"
            if rel not in tr:
                problems.append(f"{where}: `files.{b}` -> {rel} is not tracked in git")

        # The execution contract. A compile floor cannot catch an example that
        # builds and then hangs, so anything with a `run` block is executed —
        # and any binding NOT executed has to say why, for the same reason
        # `absent` does.
        run = ex.get("run")
        if run is not None:
            if not str(run.get("expect", "")).strip():
                problems.append(f"{where}: `run` has no `expect` pattern")
            else:
                try:
                    re.compile(run["expect"])
                except re.error as e:
                    problems.append(f"{where}: `run.expect` is not a valid regex: {e}")
            if not isinstance(run.get("timeout_s"), int) or run["timeout_s"] <= 0:
                problems.append(
                    f"{where}: `run.timeout_s` must be a positive integer — a "
                    f"hanging example is the failure this catches, so it needs a "
                    f"bound"
                )
            nw = run.get("not_wired") or {}
            for b, reason in nw.items():
                if b not in files:
                    problems.append(
                        f"{where}: `run.not_wired.{b}` is not a binding with a file"
                    )
                if not str(reason).strip():
                    problems.append(
                        f"{where}: `run.not_wired.{b}` has no reason. An unexplained "
                        f"gap in execution reads like full coverage."
                    )

    # The reverse direction, and the one that fails quietly: a source file added
    # to an example directory but never listed here is published to users and
    # compiled by nothing. The manifest is only a coverage record if it is the
    # complete one.
    listed = {
        f"{ex['dir']}/{name}"
        for ex in (m.get("examples") or [])
        for name in (ex.get("files") or {}).values()
    }
    known_exts = tuple(EXT.values())
    for d in {ex.get("dir") for ex in (m.get("examples") or []) if ex.get("dir")}:
        for f in sorted(p for p in tr if p.startswith(d + "/")):
            if f.endswith(known_exts) and f not in listed:
                problems.append(
                    f"{f} is in an example directory but not in the manifest, so "
                    f"nothing compiles it — while it still ships to users. Add it "
                    f"to a route's `files`, or move it out of {d}/."
                )

    return problems


def entries(m):
    for ex in m.get("examples") or []:
        for b, name in (ex.get("files") or {}).items():
            yield b, f"{ex['dir']}/{name}", ex["id"]


def run_entries(m, lang):
    """`<path>\\t<id>\\t<timeout>\\t<expect>` for each example runnable in `lang`."""
    for ex in m.get("examples") or []:
        run = ex.get("run")
        if not run:
            continue
        name = (ex.get("files") or {}).get(lang)
        if not name or lang in (run.get("not_wired") or {}):
            continue
        yield f"{ex['dir']}/{name}", ex["id"], run["timeout_s"], run["expect"]


def report(m):
    bindings = m["bindings"]
    # name + space + mark, then a two-column gutter.
    width = max(len(b) for b in bindings) + 4
    print("Checked-example coverage")
    print(
        "  ▶ executed: it compiles AND runs, and its stdout matched the "
        "contract.\n"
        "  ✓ compiled or type-checked only — nothing proves it runs.\n"
        "  — no example for this binding.\n"
        "  None of this says the binding lacks the feature; that is "
        "bindings/coverage.md."
    )
    for ex in m.get("examples") or []:
        print(f"\n  {ex['id']} ({ex['skill']}) — {ex['route']}")
        files, absent = ex.get("files") or {}, ex.get("absent") or {}
        run = ex.get("run") or {}
        nw = run.get("not_wired") or {}
        cells = []
        for b in bindings:
            if b not in files:
                mark = "—"
            elif run and b not in nw:
                mark = "▶"          # compiled AND executed
            else:
                mark = "✓"          # compiled only
            cells.append(f"{b} {mark}".ljust(width))
        print("    " + "".join(cells))
        for b, reason in absent.items():
            print(f"    — no example: {b} — {reason}")
        for b, reason in nw.items():
            print(f"    ✓ compiled, not executed here: {b} — {reason}")



def self_test():
    """Plant manifest defects and require each to be reported.

    The evidence-level rules are new in this phase, and a rule nobody has watched
    fail is not known to work. The last case is the control: the unmodified
    manifest must validate, or six probes that all report problems prove nothing.
    """
    import copy
    import tempfile

    cases = [
        ("a missing level", "missing `level`",
         lambda m: m["examples"][0].pop("level")),
        ("an unknown level", "unknown level",
         lambda m: m["examples"][0].update(level="mostly-works")),
        ("level run with no output contract", "needs a `run.expect` contract",
         lambda m: m["examples"][0]["run"].pop("expect")),
        ("a level below run with no reason", "with no `level_reason`",
         lambda m: (m["examples"][0].update(level="compile"),
                    m["examples"][0].pop("run"))),
        ("a run block on a non-run level", "but a `run` block is present",
         lambda m: m["examples"][0].update(level="compile")),
        ("a per-binding override below run with no reason",
         "with no `level_reasons.c`",
         lambda m: m["examples"][0].update(level_overrides={"c": "link"})),
        ("an override for an unknown binding", "unknown binding",
         lambda m: m["examples"][0].update(level_overrides={"fortran": "compile"},
                                          level_reasons={"fortran": "why not"})),
    ]

    print("==> Self-test — planting manifest defects")
    failures = 0
    base = load()
    with tempfile.TemporaryDirectory() as tmp:
        for label, needle, mutate in cases:
            m = copy.deepcopy(base)
            mutate(m)
            path = pathlib.Path(tmp) / "examples.yaml"
            path.write_text(yaml.safe_dump(m, sort_keys=False))
            proc = subprocess.run(
                [sys.executable, os.path.abspath(__file__), "--validate"],
                capture_output=True, text=True, cwd=ROOT,
                env={**os.environ, "EXAMPLES_MANIFEST": str(path)},
            )
            out = proc.stdout + proc.stderr
            if proc.returncode != 0 and needle in out:
                print(f"  \033[32m✓\033[0m reported {label}")
            else:
                print(f"  \033[31m✗\033[0m MISSED {label} "
                      f"(rc={proc.returncode}, wanted {needle!r})")
                failures += 1

        path = pathlib.Path(tmp) / "examples.yaml"
        path.write_text(yaml.safe_dump(base, sort_keys=False))
        proc = subprocess.run(
            [sys.executable, os.path.abspath(__file__), "--validate"],
            capture_output=True, text=True, cwd=ROOT,
            env={**os.environ, "EXAMPLES_MANIFEST": str(path)},
        )
        if proc.returncode == 0:
            print("  \033[32m✓\033[0m the unmodified manifest validates")
        else:
            print("  \033[31m✗\033[0m the UNMODIFIED manifest fails — every "
                  "result above proves nothing")
            print(proc.stdout + proc.stderr)
            failures += 1

    print()
    if failures:
        print(f"{failures} self-test failure(s).")
        return 1
    print("The validator reports every planted defect.")
    return 0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--validate", action="store_true")
    ap.add_argument("--list", action="store_true")
    ap.add_argument("--report", action="store_true")
    ap.add_argument("--self-test", action="store_true")
    ap.add_argument("--run-spec", metavar="LANG",
                    help="tab-separated run contracts for LANG")
    args = ap.parse_args()

    if args.self_test:
        return self_test()

    try:
        m = load()
    except (OSError, yaml.YAMLError) as e:
        print(f"cannot read {MANIFEST}: {e}")
        return 1

    if args.validate:
        problems = validate(m)
        for p in problems:
            print(f"  ✗ {p}")
        return 1 if problems else 0
    if args.list:
        for lang, path, eid in entries(m):
            print(f"{lang}\t{path}\t{eid}")
        return 0
    if args.report:
        report(m)
        return 0
    if args.run_spec:
        for path, eid, timeout, expect in run_entries(m, args.run_spec):
            print(f"{path}\t{eid}\t{timeout}\t{expect}")
        return 0

    ap.print_help()
    return 1


if __name__ == "__main__":
    sys.exit(main())
