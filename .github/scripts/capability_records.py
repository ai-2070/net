#!/usr/bin/env python3
"""The capability records: author once, generate the skill-local copies.

WHY. `does Go support this` had two homes — one matrix per domain skill — and the
docs were about to become a third. Two hand-maintained copies of the same fact
diverge; within a quarter you have two answers and no way to tell which is
current. So one authored record per domain under `docs/data/capabilities/`, and
every portable copy is generated and equality-checked here.

WHY THE RECORDS LIVE UNDER `docs/` AND NOT UNDER `.claude/skills/`. Docs are
canonical product truth; skills are compact executable guidance derived from it.
A skill corpus that owned the parity record would be authoritative over the docs.
The skills still ship their own domain-local matrix, because a skill installs
standalone (`npx skills add … --skill net-payments`) — that copy is mechanical,
committed, and diffed.

SUBCOMMANDS
  --extract        bootstrap the records FROM the current coverage.md files.
                   Run once; kept so the bootstrap is reproducible and reviewable
                   rather than a hand-transcription nobody can audit. Hand
                   transcription is not a hypothetical risk here: writing these
                   cells by hand the first time produced ten invented symbol
                   anchors, caught only by the checker.
  --render DOMAIN  print the generated markdown tables for one domain.
  --check          regenerate every skill copy and diff it against what is
                   committed; validate the closed vocabulary; resolve every
                   positive cell's anchor in that binding's tree.
  --self-test      plant defects and require each to be reported.
"""

from __future__ import annotations

import os
import re
import subprocess
import sys

try:
    import yaml
except ModuleNotFoundError:  # pragma: no cover
    sys.exit("PyYAML is required: python3 -m pip install pyyaml")

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from docs_pages import DEFAULT_DOCS, page_slugs  # noqa: E402  (after sys.path)

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
DOCS = os.environ.get("DOCS_CONTENT_DIR", DEFAULT_DOCS)
RECORDS = os.environ.get("CAPABILITY_RECORDS", "docs/data/capabilities")
SKILLS = os.environ.get("SKILLS_DIR", ".claude/skills")

# domain -> the skill whose bindings/coverage.md is generated from it.
DOMAIN_SKILL = {
    "event-bus": "net-event-bus",
    "payments": "net-payments",
}

STATUS_MARK = "<!-- coverage:status -->"
ANCHOR_MARK = "<!-- coverage:anchors -->"

STATUSES = {"supported", "partial", "experimental", "not exposed", "n/a"}
MODES = {"poll", "verify-only", "core-only"}

# Column order is part of the contract: the generated table has to match the
# committed one exactly, and readers compare columns left to right.
BINDINGS = ["Rust", "Node / TS", "Python", "Go", "C"]

RED, GREEN, DIM, OFF = "\033[31m", "\033[32m", "\033[2m", "\033[0m"


# --------------------------------------------------------------- markdown I/O
def table_after(text: str, marker: str) -> list[list[str]]:
    """Rows of the first markdown table after `marker`, cells stripped."""
    idx = text.index(marker) + len(marker)
    rows = []
    for line in text[idx:].splitlines():
        s = line.strip()
        if not s:
            if rows:
                break
            continue
        if not s.startswith("|"):
            break
        cells = [c.strip() for c in s.strip("|").split("|")]
        if all(set(c) <= set("-: ") for c in cells):
            continue  # separator row
        rows.append(cells)
    return rows


def parse_status(cell: str) -> tuple[str, str | None]:
    """`supported · core-only` -> ("supported", "core-only")."""
    parts = [p.strip() for p in cell.split("·")]
    return parts[0], (parts[1] if len(parts) > 1 else None)


def unbacktick(cell: str) -> str | None:
    """`` `emit` `` -> "emit"; an em dash or empty cell -> None."""
    s = cell.strip().strip("`").strip()
    return None if s in ("", "—", "-") else s


# ------------------------------------------------------------------- extract
def extract(domain: str) -> dict:
    skill = DOMAIN_SKILL[domain]
    path = os.path.join(ROOT, SKILLS, skill, "bindings", "coverage.md")
    text = open(path, encoding="utf-8").read()

    status_rows = table_after(text, STATUS_MARK)
    anchor_rows = table_after(text, ANCHOR_MARK)
    header = status_rows[0][1:]
    if header != BINDINGS:
        sys.exit(f"{path}: unexpected column order {header!r}")

    anchors = {r[0]: r[1:] for r in anchor_rows[1:]}

    operations = []
    for row in status_rows[1:]:
        op, cells = row[0], row[1:]
        anchor_cells = anchors.get(op, [""] * len(BINDINGS))
        entry: dict = {"operation": op, "bindings": {}}
        for binding, cell, anchor_cell in zip(BINDINGS, cells, anchor_cells):
            status, mode = parse_status(cell)
            b: dict = {"status": status}
            if mode:
                b["mode"] = mode
            anchor = unbacktick(anchor_cell)
            if anchor:
                b["anchor"] = anchor
            entry["bindings"][binding] = b
        operations.append(entry)

    return {"domain": domain, "skill": skill, "operations": operations}


HEADER = """\
# Capability record — {domain}
#
# CANONICAL. This is the only authored answer to "does binding X support
# operation Y" for this domain. `.claude/skills/{skill}/bindings/coverage.md`
# is GENERATED from this file; edit here, run
# `.github/scripts/capability_records.py --check` to see the diff, and commit
# both.
#
# status  supported | partial | experimental | not exposed | n/a
#           `n/a` means the operation makes no sense on this binding — a
#           permanent non-concept, not a gap. `not exposed` means buildable and
#           not built: a roadmap entry. The difference decides whether a reader
#           should stop asking.
# mode    poll | verify-only | core-only        (qualifies a status)
#           `core-only` is the most load-bearing: the operation exists, but only
#           on the low-level binding (`@net-mesh/core`, `net`), not the
#           ergonomic wrapper. It is the single most common way to be wrong
#           about Net in Node and Python.
# anchor  one symbol CI resolves in that binding's tree. Positive cells must
#           carry one; negative cells must not. An anchor proves a symbol
#           exists, NOT that the operation is supported — the status is
#           editorial, the anchor is its evidence.
#
# NOT YET POPULATED: `reason` and `alternative` per negative cell, which D5's
# generated absence state needs. Until Phase 3/4 renders that state, the prose
# rationale still lives in the skill's own "Why the negative cells are negative"
# section, which is authored and not generated.
#
# Governed by docs/internal/plans/DOCS_POLYGLOT_LENS_PLAN.md
"""


def dump(record: dict) -> str:
    body = yaml.safe_dump(record, sort_keys=False, allow_unicode=True, width=100)
    return HEADER.format(**record) + "\n" + body


# -------------------------------------------------------------------- render
def render(record: dict) -> tuple[str, str]:
    """(status table, anchor table) as markdown, matching the committed shape."""
    head = "| Operation | " + " | ".join(BINDINGS) + " |"
    sep = "|" + "---|" * (len(BINDINGS) + 1)

    status_lines = [head, sep]
    anchor_lines = [head, sep]
    for op in record["operations"]:
        scells, acells = [], []
        for binding in BINDINGS:
            b = op["bindings"][binding]
            cell = b["status"] + (f" · {b['mode']}" if b.get("mode") else "")
            scells.append(cell)
            anchor = b.get("anchor")
            acells.append(f"`{anchor}`" if anchor else "—")
        status_lines.append(f"| {op['operation']} | " + " | ".join(scells) + " |")
        anchor_lines.append(f"| {op['operation']} | " + " | ".join(acells) + " |")
    return "\n".join(status_lines), "\n".join(anchor_lines)


def splice(text: str, marker: str, table: str) -> str:
    """Replace the table following `marker`, leaving everything else alone."""
    start = text.index(marker) + len(marker)
    lines = text[start:].splitlines(keepends=True)
    i = 0
    while i < len(lines) and not lines[i].strip():
        i += 1
    j = i
    while j < len(lines) and lines[j].strip().startswith("|"):
        j += 1
    return text[:start] + "\n\n" + table + "\n" + "".join(lines[j:])


# --------------------------------------------------------------------- check
def load_record(domain: str) -> dict:
    path = os.path.join(ROOT, RECORDS, f"{domain}.yaml")
    with open(path, encoding="utf-8") as fh:
        return yaml.safe_load(fh)


def tracked_blobs() -> tuple[dict[str, str], set[str]]:
    """Concatenated tree text per binding, plus the set of tracked paths.

    Reuses the tree map `check-skill-coverage.py` established: an anchor has to
    resolve *somewhere* in the binding, because the wrapper-vs-core distinction
    is carried editorially by `core-only` rather than by where the symbol sits.
    """
    trees = {
        "Rust": (["net/crates/net/sdk/src", "net/crates/net/payments/src",
                  "net/crates/net/src"], (".rs",)),
        # napi declares the Node surface in Rust, so .rs counts for TS.
        "Node / TS": (["net/crates/net/sdk-ts/src", "net/crates/net/bindings/node"],
                      (".ts", ".rs")),
        "Python": (["net/crates/net/sdk-py/src", "net/crates/net/bindings/python"],
                   (".py", ".pyi", ".rs")),
        "Go": (["go"], (".go",)),
        "C": (["net/crates/net/include"], (".h",)),
    }
    listing = subprocess.run(["git", "ls-files", "-z"], cwd=ROOT,
                             capture_output=True, text=True, check=True).stdout
    tracked = {p for p in listing.split("\0") if p}
    blobs: dict[str, str] = {}
    for binding, (roots, exts) in trees.items():
        chunks = []
        for path in tracked:
            if not path.endswith(exts):
                continue
            if not any(path.startswith(r + "/") or path == r for r in roots):
                continue
            try:
                with open(os.path.join(ROOT, path), encoding="utf-8",
                          errors="replace") as fh:
                    chunks.append(fh.read())
            except OSError:
                pass
        blobs[binding] = "\n".join(chunks)
    return blobs, tracked


def check() -> int:
    fail = 0
    blobs, tracked = tracked_blobs()
    pages = page_slugs(os.path.join(ROOT, DOCS))

    for domain, skill in sorted(DOMAIN_SKILL.items()):
        record = load_record(domain)
        print(f"==> {domain}  ({len(record['operations'])} operations × "
              f"{len(BINDINGS)} bindings)")

        # 1. closed vocabulary
        vocab = 0
        for op in record["operations"]:
            for binding, b in op["bindings"].items():
                if binding not in BINDINGS:
                    print(f"  {RED}✗{OFF} {op['operation']}: unknown binding "
                          f"{binding!r}")
                    fail += 1
                    continue
                if b["status"] not in STATUSES:
                    print(f"  {RED}✗{OFF} {op['operation']} / {binding}: unknown "
                          f"status {b['status']!r}")
                    fail += 1
                if b.get("mode") and b["mode"] not in MODES:
                    print(f"  {RED}✗{OFF} {op['operation']} / {binding}: unknown "
                          f"mode {b['mode']!r}")
                    fail += 1
                vocab += 1
        if vocab:
            print(f"  {GREEN}✓{OFF} {vocab} cells, vocabulary closed")

        # 2. anchors — positive cells carry one and it resolves; negative cells
        #    must not carry a symbol (a path anchor is allowed, and must be
        #    tracked: a conformance fixture can pin behaviour where no API does).
        anchored = resolved = 0
        for op in record["operations"]:
            for binding, b in op["bindings"].items():
                positive = b["status"] in ("supported", "partial", "experimental")
                anchor = b.get("anchor")
                if positive:
                    if not anchor:
                        print(f"  {RED}✗{OFF} {op['operation']} / {binding}: "
                              f"{b['status']} with no anchor")
                        fail += 1
                        continue
                    anchored += 1
                    if "/" in anchor:
                        if anchor not in tracked:
                            print(f"  {RED}✗{OFF} {op['operation']} / {binding}: "
                                  f"path anchor not tracked in git: {anchor}")
                            fail += 1
                        else:
                            resolved += 1
                    elif re.search(rf"\b{re.escape(anchor)}\b", blobs[binding]):
                        resolved += 1
                    else:
                        print(f"  {RED}✗{OFF} {op['operation']} / {binding}: "
                              f"anchor `{anchor}` does not resolve in that "
                              f"binding's tree")
                        fail += 1
                elif anchor and "/" not in anchor:
                    print(f"  {RED}✗{OFF} {op['operation']} / {binding}: "
                          f"{b['status']} must not name a symbol anchor "
                          f"(`{anchor}`) — absence is not machine-checkable, so "
                          f"its rationale is prose")
                    fail += 1
        if anchored:
            print(f"  {GREEN}✓{OFF} {resolved}/{anchored} positive-cell anchors "
                  f"resolve")

        # 3. absence links resolve. D5 renders the generated absence state from
        #    `alternative.href`, which makes a docs link live inside a data file —
        #    the one place a broken link could hide from `check-doc-links.mjs`,
        #    which only reads markdown. Validated here rather than there because
        #    the record checker already owns record validation and the site has no
        #    YAML parser; `docs_pages.page_slugs` is shared so the two checkers
        #    cannot disagree about what a page is.
        hrefs = 0
        for op in record["operations"]:
            for binding, b in op["bindings"].items():
                alt = b.get("alternative") or {}
                href = alt.get("href")
                if not href:
                    continue
                hrefs += 1
                if href.startswith(("http://", "https://")):
                    continue  # external; a link checker does not fetch
                target = href.split("#")[0].rstrip("/")
                if not target.startswith("/docs"):
                    print(f"  {RED}✗{OFF} {op['operation']} / {binding}: "
                          f"alternative.href must be a /docs path or an absolute "
                          f"URL: {href}")
                    fail += 1
                    continue
                slug = target[len("/docs"):].strip("/") or "index"
                if slug not in pages:
                    print(f"  {RED}✗{OFF} {op['operation']} / {binding}: "
                          f"alternative.href points at a page that does not "
                          f"exist: {href}")
                    fail += 1
        if hrefs:
            print(f"  {GREEN}✓{OFF} {hrefs} absence link(s) resolve "
                  f"{DIM}(the page, not the #fragment){OFF}")
        else:
            print(f"  {DIM}    no absence links yet — `alternative` is unpopulated "
                  f"until D5 renders it{OFF}")

        # 4. the generated copy matches what is committed
        md_path = os.path.join(ROOT, SKILLS, skill, "bindings", "coverage.md")
        current = open(md_path, encoding="utf-8").read()
        status_tbl, anchor_tbl = render(record)
        want = splice(splice(current, STATUS_MARK, status_tbl),
                      ANCHOR_MARK, anchor_tbl)
        if want == current:
            print(f"  {GREEN}✓{OFF} {skill}/bindings/coverage.md matches the record")
        else:
            print(f"  {RED}✗{OFF} {skill}/bindings/coverage.md has drifted from "
                  f"the record")
            print(f"      Regenerate: capability_records.py --write")
            for line in _diff(current, want)[:12]:
                print(f"      {line}")
            fail += 1
        print()

    if fail == 0:
        print("Capability records are the source, and every copy matches.")
        return 0
    print(f"{fail} capability-record problem(s).")
    return 1


def _diff(a: str, b: str) -> list[str]:
    import difflib
    return [l.rstrip("\n") for l in difflib.unified_diff(
        a.splitlines(True), b.splitlines(True),
        fromfile="committed", tofile="generated", n=0)]


def write() -> int:
    for domain, skill in sorted(DOMAIN_SKILL.items()):
        record = load_record(domain)
        md_path = os.path.join(ROOT, SKILLS, skill, "bindings", "coverage.md")
        current = open(md_path, encoding="utf-8").read()
        status_tbl, anchor_tbl = render(record)
        out = splice(splice(current, STATUS_MARK, status_tbl),
                     ANCHOR_MARK, anchor_tbl)
        if out != current:
            with open(md_path, "w", encoding="utf-8") as fh:
                fh.write(out)
            print(f"  regenerated {skill}/bindings/coverage.md")
        else:
            print(f"  {skill}/bindings/coverage.md already current")
    return 0


def self_test() -> int:
    """Plant defects in copies of the records and require each to be reported."""
    import shutil
    import tempfile
    print("==> Self-test — planting defects in scratch records")
    cases = [
        ("an unknown status", "unknown status",
         lambda r: r["operations"][0]["bindings"]["Rust"].update(status="mostly")),
        ("an unknown mode", "unknown mode",
         lambda r: r["operations"][0]["bindings"]["Rust"].update(mode="sometimes")),
        ("a positive cell with no anchor", "with no anchor",
         lambda r: r["operations"][0]["bindings"]["Rust"].pop("anchor", None)),
        ("an anchor that does not resolve", "does not resolve",
         lambda r: r["operations"][0]["bindings"]["Rust"].update(
             anchor="net_no_such_symbol_anywhere")),
        ("a negative cell claiming a symbol", "must not name a symbol",
         lambda r: r["operations"][0]["bindings"]["Rust"].update(
             status="not exposed", anchor="emit")),
        ("a record the skill copy no longer matches", "has drifted",
         lambda r: r["operations"][0].update(operation="Renamed operation")),
        ("an absence link to a page that does not exist",
         "points at a page that does not exist",
         lambda r: r["operations"][8]["bindings"]["Go"].update(
             alternative={"label": "Use something else",
                          "href": "/docs/sdk/go/no-such-page"})),
        ("an absence link that is not a /docs path", "must be a /docs path",
         lambda r: r["operations"][8]["bindings"]["Go"].update(
             alternative={"label": "Elsewhere", "href": "sdk/go/watch"})),
    ]
    failures = 0
    with tempfile.TemporaryDirectory() as tmp:
        recdir = os.path.join(tmp, "capabilities")
        shutil.copytree(os.path.join(ROOT, RECORDS), recdir)
        pristine = {d: load_record(d) for d in DOMAIN_SKILL}

        def probe(mutate):
            for domain, rec in pristine.items():
                copy = yaml.safe_load(yaml.safe_dump(rec))
                if domain == "event-bus":
                    mutate(copy)
                with open(os.path.join(recdir, f"{domain}.yaml"), "w",
                          encoding="utf-8") as fh:
                    yaml.safe_dump(copy, fh, sort_keys=False, allow_unicode=True)
            proc = subprocess.run(
                [sys.executable, os.path.abspath(__file__), "--check"],
                capture_output=True, text=True, cwd=ROOT,
                env={**os.environ, "CAPABILITY_RECORDS": recdir},
            )
            return proc.returncode, proc.stdout + proc.stderr

        for label, needle, mutate in cases:
            rc, out = probe(mutate)
            if rc != 0 and needle in out:
                print(f"  {GREEN}✓{OFF} reported {label}")
            else:
                print(f"  {RED}✗{OFF} MISSED {label} (rc={rc}, wanted {needle!r})")
                failures += 1

        rc, out = probe(lambda _r: None)
        if rc == 0:
            print(f"  {GREEN}✓{OFF} the unmodified records pass")
        else:
            print(f"  {RED}✗{OFF} the UNMODIFIED records fail — every result "
                  f"above proves nothing")
            print(out)
            failures += 1

    print()
    if failures:
        print(f"{failures} self-test failure(s).")
        return 1
    print("The checker reports every planted defect.")
    return 0


def main() -> int:
    os.chdir(ROOT)
    if "--extract" in sys.argv:
        os.makedirs(os.path.join(ROOT, RECORDS), exist_ok=True)
        for domain in DOMAIN_SKILL:
            path = os.path.join(ROOT, RECORDS, f"{domain}.yaml")
            with open(path, "w", encoding="utf-8") as fh:
                fh.write(dump(extract(domain)))
            print(f"  wrote {path}")
        return 0
    if "--render" in sys.argv:
        domain = sys.argv[sys.argv.index("--render") + 1]
        status_tbl, anchor_tbl = render(load_record(domain))
        print(status_tbl)
        print()
        print(anchor_tbl)
        return 0
    if "--write" in sys.argv:
        return write()
    if "--self-test" in sys.argv:
        return self_test()
    return check()


if __name__ == "__main__":
    sys.exit(main())
