#!/usr/bin/env python3
"""Read and validate `.github/skill-examples.json`.

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
import json
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
MANIFEST = ROOT / ".github" / "skill-examples.json"

# Extension per binding, so a manifest cannot list `hello.py` under `go`.
EXT = {
    "rust": ".rs",
    "typescript": ".ts",
    "python": ".py",
    "go": ".go",
    "c": ".c",
}


def load():
    return json.loads(MANIFEST.read_text())


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


def report(m):
    bindings = m["bindings"]
    # name + space + mark, then a two-column gutter.
    width = max(len(b) for b in bindings) + 4
    print("Checked-example coverage")
    print(
        "  A tick means the file compiles or type-checks against the current "
        "tree.\n  It does not mean the example runs, and it does not mean the "
        "binding\n  lacks the feature — that is bindings/coverage.md."
    )
    for ex in m.get("examples") or []:
        print(f"\n  {ex['id']} ({ex['skill']}) — {ex['route']}")
        files, absent = ex.get("files") or {}, ex.get("absent") or {}
        cells = []
        for b in bindings:
            cells.append(f"{b} {'✓' if b in files else '—'}".ljust(width))
        print("    " + "".join(cells))
        for b, reason in absent.items():
            print(f"    — {b}: {reason}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--validate", action="store_true")
    ap.add_argument("--list", action="store_true")
    ap.add_argument("--report", action="store_true")
    args = ap.parse_args()

    try:
        m = load()
    except (OSError, json.JSONDecodeError) as e:
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

    ap.print_help()
    return 1


if __name__ == "__main__":
    sys.exit(main())
