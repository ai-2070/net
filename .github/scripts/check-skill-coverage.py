#!/usr/bin/env python3
"""Verify the binding-coverage matrices in `.claude/skills/*/bindings/coverage.md`.

WHAT A MATRIX IS FOR
An agent asked to "add payments to our Go service" needs to know, before writing
a line, that there is no Go payments API. Today that fact is prose scattered
across eight files, and one of those claims had already drifted — `bindings.md`
said billing was Rust + Python when Node has `read_billing` too, found only
because someone read it.

WHAT THIS CHECK CAN HONESTLY ASSERT — AND WHAT IT CANNOT
Not completeness. A resolved symbol proves one narrow thing: *an expected anchor
exists on this surface*. It cannot prove `supported`. And absence proves even
less — a binding may alias, project under another name, expose dynamically
(Python), surface through generated declarations (TypeScript), or carry a
low-level FFI symbol the ergonomic SDK deliberately withholds. Every one of
those happened while the first matrix was being written: `call_typed` in Rust is
`call` on `TypedMeshRpc` everywhere else, Go's blob fetch is
`MeshBlobAdapter.Fetch`, and Go's RedEX lives in `cortex.go` with no `redex.go`
at all. A checker that inferred absence from a missing symbol would have
reported four bindings as broken.

So: **the matrix is editorially authoritative; this verifies its declared
evidence.** Three obligations, matching the three kinds of cell:

  positive (supported / partial / experimental)
      must name an anchor, and every backticked symbol in it must resolve in
      that binding's tree.
  negative (not exposed / n/a)
      must NOT name a symbol anchor. Its rationale is prose, and prose is not
      machine-checked — asserting a global absence rule is exactly the mistake
      described above.
  selected critical negatives
      may carry a path anchor instead — a conformance test or fixture that pins
      the behaviour even though no API exists. The path must be tracked in git.
      Chosen case by case, never inferred.

Prints one line per violation and exits 1; silent exit 0 when clean.
"""

import pathlib
import re
import subprocess
import sys

SKILLS = pathlib.Path(".claude/skills")

# Where each binding's public surface lives. A union per language: an anchor has
# to resolve *somewhere* in the binding, and the wrapper-vs-core distinction is
# carried editorially by the `core-only` mode rather than by this lookup.
TREES = {
    "Rust": ["net/crates/net/sdk/src", "net/crates/net/payments/src", "net/crates/net/src"],
    "Node / TS": ["net/crates/net/sdk-ts/src", "net/crates/net/bindings/node"],
    "Python": ["net/crates/net/sdk-py/src", "net/crates/net/bindings/python"],
    "Go": ["go"],
    "C": ["net/crates/net/include"],
}
EXTS = {
    "Rust": (".rs",),
    "Node / TS": (".ts", ".rs"),   # napi declares the binding surface in Rust
    "Python": (".py", ".rs"),      # pyo3 likewise
    "Go": (".go",),
    "C": (".h",),
}

STATUSES = {"supported", "partial", "experimental", "not exposed", "n/a"}
POSITIVE = {"supported", "partial", "experimental"}
MODES = {"poll", "verify-only", "core-only"}
NONE = {"—", "-", ""}

STATUS_MARK = "<!-- coverage:status -->"
ANCHOR_MARK = "<!-- coverage:anchors -->"


def load_trees():
    files = subprocess.run(
        ["git", "ls-files"], capture_output=True, text=True
    ).stdout.split()
    tracked = set(files)
    blobs = {}
    for lang, roots in TREES.items():
        text = []
        for f in files:
            if not f.endswith(EXTS[lang]):
                continue
            if any(f.startswith(r + "/") for r in roots):
                try:
                    text.append(open(f, errors="replace").read())
                except OSError:
                    pass
        blobs[lang] = "\n".join(text)
    return blobs, tracked


def table_after(text, marker):
    """The first markdown table following `marker`. Returns (headers, rows)."""
    i = text.find(marker)
    if i < 0:
        return None
    lines = text[i + len(marker):].splitlines()
    rows = []
    for line in lines:
        s = line.strip()
        if not s:
            if rows:
                break
            continue
        if not s.startswith("|"):
            if rows:
                break
            continue
        cells = [c.strip() for c in s.strip("|").split("|")]
        if all(re.fullmatch(r":?-{2,}:?", c) for c in cells):
            continue
        rows.append(cells)
    if not rows:
        return None
    return rows[0], rows[1:]


def parse_status(cell):
    """`supported · core-only` -> ('supported', 'core-only')."""
    parts = [p.strip() for p in re.split(r"[·|]", cell) if p.strip()]
    if not parts:
        return "", None
    return parts[0].lower(), (parts[1].lower() if len(parts) > 1 else None)


def check_file(md, blobs, tracked, problems):
    text = md.read_text()
    rel = md.relative_to(SKILLS)

    status = table_after(text, STATUS_MARK)
    anchors = table_after(text, ANCHOR_MARK)
    if not status:
        problems.append(f"{rel}: no table found after {STATUS_MARK}")
        return
    if not anchors:
        problems.append(f"{rel}: no table found after {ANCHOR_MARK}")
        return

    shead, srows = status
    ahead, arows = anchors
    if shead != ahead:
        problems.append(
            f"{rel}: status and anchor tables have different columns\n"
            f"    status:  {shead}\n    anchors: {ahead}"
        )
        return
    langs = shead[1:]
    unknown = [l for l in langs if l not in TREES]
    if unknown:
        problems.append(f"{rel}: unknown binding column(s): {', '.join(unknown)}")
        return

    sops = [r[0] for r in srows]
    aops = [r[0] for r in arows]
    if sops != aops:
        problems.append(
            f"{rel}: the two tables list different operations, so a cell's status "
            f"cannot be matched to its evidence.\n"
            f"    only in status:  {[o for o in sops if o not in aops]}\n"
            f"    only in anchors: {[o for o in aops if o not in sops]}"
        )
        return

    for srow, arow in zip(srows, arows):
        op = srow[0]
        for lang, scell, acell in zip(langs, srow[1:], arow[1:]):
            st, mode = parse_status(scell)
            where = f"{rel} [{op} / {lang}]"

            if st not in STATUSES:
                problems.append(
                    f"{where}: status {scell!r} is not in the closed vocabulary "
                    f"({', '.join(sorted(STATUSES))})"
                )
                continue
            if mode is not None and mode not in MODES:
                problems.append(
                    f"{where}: mode {mode!r} is not in the closed vocabulary "
                    f"({', '.join(sorted(MODES))})"
                )

            syms = re.findall(r"`([^`]+)`", acell)
            bare = acell.strip()

            if st in POSITIVE:
                if not syms:
                    problems.append(
                        f"{where}: {st} but names no evidence anchor. Every "
                        f"positive cell must cite a symbol CI can resolve."
                    )
                    continue
                for sym in syms:
                    if "/" in sym:  # a path anchor
                        if sym not in tracked:
                            problems.append(
                                f"{where}: anchor path {sym} is not tracked in git"
                            )
                        continue
                    if not re.search(r"\b" + re.escape(sym) + r"\b", blobs[lang]):
                        problems.append(
                            f"{where}: anchor `{sym}` does not appear anywhere in "
                            f"the {lang} tree ({', '.join(TREES[lang])})"
                        )
            else:
                # Negative. A symbol anchor here would be a contradiction; a path
                # anchor is the deliberate "critical negative" case.
                for sym in syms:
                    if "/" not in sym:
                        problems.append(
                            f"{where}: {st} but names symbol anchor `{sym}`. A "
                            f"negative cell carries prose rationale, or a path to "
                            f"absence evidence — not a symbol."
                        )
                    elif sym not in tracked:
                        problems.append(
                            f"{where}: absence-evidence path {sym} is not tracked in git"
                        )
                if not syms and bare not in NONE:
                    problems.append(
                        f"{where}: {st} but the anchor cell reads {acell!r}. Use "
                        f"an em dash, or a backticked path to absence evidence."
                    )


def main():
    mds = sorted(SKILLS.glob("*/bindings/coverage.md"))
    if not mds:
        print("no coverage matrices found under .claude/skills/*/bindings/")
        return 1
    blobs, tracked = load_trees()
    problems = []
    for md in mds:
        check_file(md, blobs, tracked, problems)
    for p in problems:
        print(p)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
