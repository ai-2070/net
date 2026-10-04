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
import c_abi_model as M  # noqa: E402  (the C call finder c-surface-record.py uses)

# Every check in this suite prints its verdict with U+2713 / U+2717, and some
# of the identifiers it echoes carry em-dashes. Python picks stdout's encoding
# from the platform, so on a cp1252 console those characters raise
# UnicodeEncodeError mid-report — the checker dies partway through and its
# caller sees a truncated run rather than a verdict. Force UTF-8 so the output
# is the same everywhere the checker runs.
if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8")
    sys.stderr.reconfigure(encoding="utf-8")

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
DOCS = os.environ.get("DOCS_CONTENT_DIR", DEFAULT_DOCS)
RECORDS = os.environ.get("CAPABILITY_RECORDS", "docs/data/capabilities")
SKILLS = os.environ.get("SKILLS_DIR", ".claude/skills")
# Generated, equality-checked JSON so the static site can read the record.
# The site has no YAML parser and the docs build is fully static, so this is
# the bridge D5 needs to render an absence state from the record rather than
# from prose a page author typed.
BRIDGE = os.environ.get("CAPABILITY_BRIDGE",
                        "web/src/lib/generated/capability-record.json")

# domain -> the skill whose bindings/coverage.md is generated from it.
DOMAIN_SKILL = {
    "event-bus": "net-event-bus",
    "payments": "net-payments",
}

STATUS_MARK = "<!-- coverage:status -->"
ANCHOR_MARK = "<!-- coverage:anchors -->"
C_EVIDENCE_MARK = "<!-- coverage:c-evidence -->"

# C has no binding test suite: its evidence is the C programs CI runs. So a
# positive C cell must name one that calls its anchor (C SDK consumer
# verification plan, C6). A consumer program in examples/c is run by
# run-c-consumers.py in every lane; a skill example counts when
# docs/data/examples.yaml runs its C file.
C_CONSUMERS = "net/crates/net/examples/c/"
EXAMPLES_INDEX = os.environ.get("EXAMPLES_INDEX", "docs/data/examples.yaml")

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
# evidence  C cells only: the C programs CI runs that call the anchor (a
#           consumer program in net/crates/net/examples/c/, or a skill example
#           docs/data/examples.yaml runs). A `supported` C cell needs one.
# gap       C cells only: what no C program exercises yet. A positive C cell
#           with no evidence is `partial` and states its gap.
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


C_EVIDENCE_HEADER = ("| Operation | C status | Run by CI | Not yet exercised from C |", "|---|---|---|---|")


def render_c_evidence(record: dict) -> str | None:
    """The C evidence table, or None when the record has no positive C cell."""
    rows = []
    for op in record["operations"]:
        b = op["bindings"].get("C") or {}
        if b.get("status") not in ("supported", "partial", "experimental"):
            continue
        evidence = b.get("evidence") or []
        if isinstance(evidence, str):  # one path, as check() accepts it
            evidence = [evidence]
        ev = ", ".join(f"`{p.rsplit('/', 1)[-1]}`" for p in evidence) or "—"
        rows.append(f"| {op['operation']} | {b['status']} | {ev} | {b.get('gap') or '—'} |")
    if not rows:
        return None
    return "\n".join([*C_EVIDENCE_HEADER, *rows])


def spliced(current: str, record: dict) -> str:
    """`current` with every generated table replaced from `record`."""
    status_tbl, anchor_tbl = render(record)
    out = splice(splice(current, STATUS_MARK, status_tbl), ANCHOR_MARK, anchor_tbl)
    if C_EVIDENCE_MARK in out:
        # With no positive C cell left, the table keeps its header and loses
        # every row: a stale row must not survive a regeneration, and a
        # header-only table splices idempotently.
        c_tbl = render_c_evidence(record) or "\n".join(C_EVIDENCE_HEADER)
        out = splice(out, C_EVIDENCE_MARK, c_tbl)
    return out


def c_run_files(tracked: set[str]) -> dict[str, str]:
    """Repo path -> how CI runs it, for every C file that counts as evidence."""
    out = {p: "consumer program (run-c-consumers.py)" for p in tracked
           if p.startswith(C_CONSUMERS) and p.endswith(".c") and "/" not in p[len(C_CONSUMERS):]}
    with open(os.path.join(ROOT, EXAMPLES_INDEX), encoding="utf-8") as fh:
        index = yaml.safe_load(fh)
    for ex in index.get("examples", []):
        name = (ex.get("files") or {}).get("c")
        not_wired = ((ex.get("run") or {}).get("not_wired") or {})
        if name and ex.get("level") == "run" and "c" not in not_wired:
            out[f"{ex['dir'].rstrip('/')}/{name}"] = f"skill example {ex['id']} (run)"
    return out


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

        # 2b. C evidence. An anchor proves a symbol exists; for C, a positive
        #     cell must also name a C program CI runs that CALLS it, because C
        #     has no binding test suite and the programs are its only proof.
        #     A cell nothing exercises is `partial` and states its gap.
        runnable = c_run_files(tracked)
        c_cells = c_backed = 0
        for op in record["operations"]:
            b = op["bindings"].get("C") or {}
            where = f"{op['operation']} / C"
            if b.get("status") not in ("supported", "partial", "experimental"):
                if b.get("evidence") or b.get("gap"):
                    print(f"  {RED}✗{OFF} {where}: {b.get('status')} carries evidence or a gap; "
                          f"only positive cells do")
                    fail += 1
                continue
            c_cells += 1
            evidence = b.get("evidence") or []
            if isinstance(evidence, str):
                evidence = [evidence]
            anchor = b.get("anchor") or ""
            ok_files = 0
            for path in evidence:
                if path not in tracked:
                    print(f"  {RED}✗{OFF} {where}: evidence {path} is not tracked in git")
                    fail += 1
                    continue
                if path not in runnable:
                    print(f"  {RED}✗{OFF} {where}: evidence {path} is not a C program CI runs "
                          f"(a consumer program in {C_CONSUMERS}, or a skill example "
                          f"{EXAMPLES_INDEX} runs)")
                    fail += 1
                    continue
                with open(os.path.join(ROOT, path), encoding="utf-8") as fh:
                    text = fh.read()
                calls = M.c_calls(text)
                uses = anchor in calls if anchor.startswith("net_") else bool(
                    re.search(rf"\b{re.escape(anchor)}\b", M.c_code(text)))
                if not uses:
                    print(f"  {RED}✗{OFF} {where}: evidence {path} never "
                          f"{'calls' if anchor.startswith('net_') else 'uses'} the anchor `{anchor}`")
                    fail += 1
                    continue
                ok_files += 1
            if b["status"] == "supported" and not evidence:
                print(f"  {RED}✗{OFF} {where}: supported with no evidence — name the C program "
                      f"CI runs that calls `{anchor}`, or make it partial and state the gap")
                fail += 1
            elif not evidence and not b.get("gap"):
                print(f"  {RED}✗{OFF} {where}: {b['status']} with neither evidence nor a gap")
                fail += 1
            elif evidence and ok_files == len(evidence):
                c_backed += 1
        if c_cells:
            print(f"  {GREEN}✓{OFF} {c_backed}/{c_cells} positive C cells name a C program CI "
                  f"runs that calls their anchor; the rest state their gap")
            md = os.path.join(ROOT, SKILLS, skill, "bindings", "coverage.md")
            if C_EVIDENCE_MARK not in open(md, encoding="utf-8").read():
                print(f"  {RED}✗{OFF} {skill}/bindings/coverage.md has positive C cells but no "
                      f"{C_EVIDENCE_MARK} table")
                fail += 1

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
        want = spliced(current, record)
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

    # 5. the site's JSON bridge matches. The site is a static Next build with no
    #    YAML parser, so a rendition that wants to show a parity badge needs the
    #    record as JSON. Generating it and checking equality here is the same
    #    discipline as the skill copies: one authored source, every derivative
    #    proved rather than trusted.
    print("==> Site bridge (web/src/lib/generated/capability-record.json)")
    want = bridge_json()
    bridge_path = os.path.join(ROOT, BRIDGE)
    if not os.path.exists(bridge_path):
        print(f"  {RED}✗{OFF} the bridge is missing")
        print(f"      Generate: capability_records.py --write")
        fail += 1
    else:
        current = open(bridge_path, encoding="utf-8").read()
        if current == want:
            ops = sum(len(load_record(d)["operations"]) for d in DOMAIN_SKILL)
            print(f"  {GREEN}✓{OFF} {ops} operations readable by the site, "
                  f"byte-identical to the record")
        else:
            print(f"  {RED}✗{OFF} the bridge has drifted from the record")
            print(f"      Regenerate: capability_records.py --write")
            for line in _diff(current, want)[:12]:
                print(f"      {line}")
            fail += 1

    # 6. every `capability:` a page declares names a real operation. A page that
    #    points at an operation the record does not have would render an empty
    #    badge row, which reads as "no support anywhere" rather than as a typo.
    known = {op["operation"] for d in DOMAIN_SKILL
             for op in load_record(d)["operations"]}
    declared = page_capabilities(os.path.join(ROOT, DOCS))
    bad = sorted((page, cap) for page, cap in declared.items() if cap not in known)
    for page, cap in bad:
        print(f"  {RED}✗{OFF} {page} declares capability {cap!r}, which is not "
              f"an operation in any record")
        fail += 1
    if declared and not bad:
        print(f"  {GREEN}✓{OFF} {len(declared)} page(s) declare a capability, "
              f"all resolving")
    print()

    if fail == 0:
        print("Capability records are the source, and every copy matches.")
        return 0
    print(f"{fail} capability-record problem(s).")
    return 1


def bridge_json() -> str:
    """The record, flattened for a site that cannot read YAML.

    Keyed by operation rather than by domain: a docs page cites one operation and
    does not care which skill's record happens to hold it. Sorted and
    2-space-indented so the equality check compares content, not formatting.
    """
    import json
    ops: dict[str, dict] = {}
    for domain in sorted(DOMAIN_SKILL):
        for op in load_record(domain)["operations"]:
            cells = {}
            for binding in BINDINGS:
                b = op["bindings"].get(binding)
                if not b:
                    continue
                cell = {"status": b["status"]}
                if b.get("mode"):
                    cell["mode"] = b["mode"]
                cells[binding] = cell
            ops[op["operation"]] = {"domain": domain, "bindings": cells}
    payload = {
        "_generated": "docs/data/capabilities/*.yaml via "
                      ".github/scripts/capability_records.py --write. "
                      "Do not edit; the check fails on drift.",
        "bindings": BINDINGS,
        "operations": ops,
    }
    return json.dumps(payload, indent=2, sort_keys=True, ensure_ascii=False) + "\n"


def _frontmatter_capability(path: str) -> str | None:
    """The `capability:` value in `path`'s frontmatter, if it declares one."""
    with open(path, encoding="utf-8") as fh:
        in_fm = False
        for line in fh:
            if line.strip() == "---":
                if in_fm:
                    return None
                in_fm = True
                continue
            if in_fm and line.startswith("capability:"):
                return line.split(":", 1)[1].strip()
    return None


def page_capabilities(docs_dir: str) -> dict[str, str]:
    """`capability:` declared by a docs page, by page slug.

    Two shapes declare one, and both are scanned so the value is checked
    wherever it is written:

      * an adaptive page's universal body (`_shared.md`), whose slug is its
        directory — that body IS the page;
      * an ordinary page, in its own frontmatter. Those are not rendered as a
        parity row today (the panel sits on the adaptive render path), so the
        field is metadata there — but an unchecked claim is the defect this
        rule exists to prevent, and `lib/docs.ts` reads the same field off
        `DocFrontmatter` for every page shape.

    Per-language fragments are not separate pages: an adaptive page's
    declaration is its body's, so fragments are skipped when a `_shared.md`
    sits beside them.

    Read with a line scan rather than a YAML parser because `lib/docs.ts` reads
    the same frontmatter with a twenty-line scanner — a checker that accepted
    shapes the site cannot parse would pass a page that renders nothing.

    Release notes are excluded, as everywhere else: they are dated records of
    what shipped, and an operation renamed since is not a typo in the note.
    """
    out: dict[str, str] = {}
    for dirpath, dirs, files in os.walk(docs_dir):
        dirs[:] = [d for d in dirs if d != "releases"]
        rel = os.path.relpath(dirpath, docs_dir).replace(os.sep, "/")
        rel = "" if rel == "." else rel
        # An adaptive page declares in its universal body.
        names = ["_shared.md"] if "_shared.md" in files else [
            n for n in files if n.endswith(".md")]
        for name in names:
            value = _frontmatter_capability(os.path.join(dirpath, name))
            if value is None:
                continue
            if name == "_shared.md" or name == "README.md":
                # A section README renders at the folder's URL.
                out[rel] = value
            else:
                out["/".join(p for p in (rel, name[:-3]) if p)] = value
    return out


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
        out = spliced(current, record)
        if out != current:
            with open(md_path, "w", encoding="utf-8") as fh:
                fh.write(out)
            print(f"  regenerated {skill}/bindings/coverage.md")
        else:
            print(f"  {skill}/bindings/coverage.md already current")

    bridge_path = os.path.join(ROOT, BRIDGE)
    os.makedirs(os.path.dirname(bridge_path), exist_ok=True)
    want = bridge_json()
    current = open(bridge_path, encoding="utf-8").read() if os.path.exists(bridge_path) else None
    if current != want:
        with open(bridge_path, "w", encoding="utf-8") as fh:
            fh.write(want)
        print(f"  regenerated {BRIDGE}")
    else:
        print(f"  {BRIDGE} already current")
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
        ("a supported C cell with no evidence", "supported with no evidence",
         lambda r: r["operations"][0]["bindings"]["C"].pop("evidence", None)),
        ("C evidence that never calls the anchor", "never calls the anchor",
         lambda r: r["operations"][0]["bindings"]["C"].update(
             evidence=[".claude/skills/net-event-bus/examples/registry.c"])),
        ("C evidence CI does not run", "is not a C program CI runs",
         lambda r: r["operations"][0]["bindings"]["C"].update(
             evidence=["net/crates/net/examples/c/support/consumer_util.c"])),
        ("a partial C cell with neither evidence nor a gap", "neither evidence nor a gap",
         lambda r: r["operations"][0]["bindings"]["C"].update(status="partial", evidence=[])),
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
