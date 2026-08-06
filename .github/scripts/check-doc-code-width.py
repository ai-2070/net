#!/usr/bin/env python3
"""No line in a docs code block may be wider than the column that renders it.

WHY THIS EXISTS. Phase 1A made code blocks scroll horizontally instead of
wrapping, because a wrapped ASCII diagram shreds. That was the right call and it
has a cost nobody was watching: a line wider than the column does not look
wrong, it just quietly goes off the right edge, and a reader who does not think
to scroll sideways reads a truncated statement as if it were the whole thing.

The limit is MEASURED, not guessed. In the rendered page a code line is 12.5px
monospace at 7.5px per character in a 706px content box — 94 characters fit
exactly at the widest layout. The limit here is 90, so a line at the limit is not
flush against the scroll edge and authors get a round number to aim at.

WHAT IT DOES NOT DO. It does not reformat anything. Running a real formatter over
the corpus was considered and rejected: prettier reaches only TypeScript and
JSON — 10% of the blocks, with Rust alone at 58% — so it would have made the
corpus less consistent rather than more, and its one visible change to the blocks
it can reach is collapsing the deliberately aligned trailing comments that carry
half the teaching in a snippet. Wrapping is an authoring decision; only the
consequence of getting it wrong is mechanical, so only that is checked.

  .github/scripts/check-doc-code-width.py              # check
  .github/scripts/check-doc-code-width.py --self-test  # plant defects
"""

from __future__ import annotations

import os
import re
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

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
DOCS = os.environ.get("DOCS_CONTENT_DIR", "web/src/content/docs")

# 94 characters fit; 90 leaves margin. See the module docstring.
#
# DERIVED FROM CSS, so it can go stale. The three values that produce it are the
# `text-[12.5px]`, `font-mono` and `px-4` on the `pre` in
# `web/src/components/DocsContent.tsx`, against the `max-w-[740px]` reading
# column. That site carries the same note pointing back here. Re-measure in a
# browser after changing either:
#
#   const pre = document.querySelector('pre[data-language]');
#   const cs = getComputedStyle(pre);
#   const probe = document.createElement('span');
#   probe.style.cssText =
#     `position:absolute;visibility:hidden;white-space:pre;font:${cs.font}`;
#   probe.textContent = 'M'.repeat(100);
#   pre.appendChild(probe);
#   const ch = probe.getBoundingClientRect().width / 100;
#   probe.remove();
#   Math.floor((pre.clientWidth - parseFloat(cs.paddingLeft)
#               - parseFloat(cs.paddingRight)) / ch);   // -> 94
MAX_WIDTH = int(os.environ.get("DOC_CODE_MAX_WIDTH", "90"))

# `text` fences are diagrams, not code. They are box-drawing art whose width is
# the drawing's own business, they render through `Diagram` with an explicit
# scroll affordance rather than through `CodeBlock`, and rewrapping one destroys
# it. Excluded by language rather than by looking at the content.
DIAGRAM_LANGS = {"text"}

# `releases/` is GENERATED from the crate by `web/scripts/sync-releases.mjs`, and
# CI proves the copy matches. Editing the site copy to satisfy this checker would
# fail that sync check on the next run, so an over-wide line in a release note is
# a defect with an address — `net/crates/net/docs/releases/` — and not one this
# tree can fix. Reported as a note, never as a failure.
GENERATED_DIRS = ("releases/",)

FENCE = re.compile(r"^```([a-zA-Z0-9+-]+)([^\n]*)\n(.*?)^```", re.S | re.M)

RED, GREEN, DIM, YELLOW, OFF = (
    "\033[31m", "\033[32m", "\033[2m", "\033[33m", "\033[0m",
)


def offenders(docs_dir: str) -> tuple[list[tuple[str, int, int, str]], list[str]]:
    """(path, line, width, lang) for every over-wide line, plus generated ones."""
    bad: list[tuple[str, int, int, str]] = []
    generated: list[str] = []
    for dirpath, _dirs, files in os.walk(docs_dir):
        for name in sorted(files):
            if not name.endswith(".md"):
                continue
            path = os.path.join(dirpath, name)
            rel = os.path.relpath(path, docs_dir).replace(os.sep, "/")
            with open(path, encoding="utf-8") as fh:
                src = fh.read()
            for m in FENCE.finditer(src):
                lang = m.group(1)
                if lang in DIAGRAM_LANGS:
                    continue
                first = src[: m.start()].count("\n") + 2
                for i, line in enumerate(m.group(3).split("\n")):
                    if len(line) <= MAX_WIDTH:
                        continue
                    if rel.startswith(GENERATED_DIRS):
                        generated.append(f"{rel}:{first + i} ({len(line)} chars)")
                    else:
                        bad.append((rel, first + i, len(line), lang))
    return bad, generated


def run(docs_dir: str) -> int:
    bad, generated = offenders(docs_dir)
    print(f"==> Code-block width (max {MAX_WIDTH})")

    for rel, line, width, lang in bad:
        print(f"  {RED}✗{OFF} {rel}:{line} — {width} chars ({lang}) "
              f"runs {width - MAX_WIDTH} past the column")

    if generated:
        print(f"  {YELLOW}!{OFF} {len(generated)} over-wide line(s) in generated "
              f"release notes, not fixable here:")
        for g in generated[:5]:
            print(f"      {DIM}{g}{OFF}")
        print(f"      {DIM}Fix upstream in net/crates/net/docs/releases/ and "
              f"re-run `npm run sync:releases`.{OFF}")

    if bad:
        print()
        print(f"{len(bad)} line(s) wider than the column that renders them.")
        return 1

    print(f"  {GREEN}✓{OFF} every code line fits the {MAX_WIDTH}-character column")
    print()
    print("No reader has to scroll sideways to finish a line.")
    return 0


def self_test() -> int:
    """Plant defects in a scratch copy of the docs and require each report."""
    import shutil
    import tempfile
    import contextlib
    import io

    print("==> Self-test — planting defects in a scratch docs tree")
    src_dir = os.path.join(ROOT, DOCS)

    cases = [
        (
            "an over-wide line in a normal page",
            "runs",
            lambda d: _write(d, "concepts/probe.md",
                             "```rust\nlet x = " + "y" * 120 + ";\n```\n"),
        ),
        (
            "a line one character past the limit",
            "runs 1 past",
            lambda d: _write(d, "concepts/probe.md",
                             "```go\n" + "z" * (MAX_WIDTH + 1) + "\n```\n"),
        ),
    ]

    failures = 0
    for label, expect, mutate in cases:
        with tempfile.TemporaryDirectory() as tmp:
            scratch = os.path.join(tmp, "docs")
            shutil.copytree(src_dir, scratch)
            mutate(scratch)
            buf = io.StringIO()
            with contextlib.redirect_stdout(buf):
                code = run(scratch)
            out = buf.getvalue()
        if code != 0 and expect in out:
            print(f"  {GREEN}✓{OFF} reported {label}")
        else:
            print(f"  {RED}✗{OFF} did NOT report {label}")
            failures += 1

    # A diagram is exempt BY LANGUAGE, so a wide `text` fence must NOT fail —
    # otherwise the checker would demand the one thing Phase 1B exists to allow.
    with tempfile.TemporaryDirectory() as tmp:
        scratch = os.path.join(tmp, "docs")
        shutil.copytree(src_dir, scratch)
        _write(scratch, "concepts/probe.md",
               "```text title=\"wide\"\n" + "─" * 140 + "\n```\n")
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            code = run(scratch)
    if code == 0:
        print(f"  {GREEN}✓{OFF} left a wide diagram alone")
    else:
        print(f"  {RED}✗{OFF} FAILED a wide diagram, which is exempt by design")
        failures += 1

    # And a generated release note is a note, not a failure.
    with tempfile.TemporaryDirectory() as tmp:
        scratch = os.path.join(tmp, "docs")
        shutil.copytree(src_dir, scratch)
        _write(scratch, "releases/probe.md",
               "```rust\nlet x = " + "y" * 120 + ";\n```\n")
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            code = run(scratch)
        out = buf.getvalue()
    if code == 0 and "not fixable here" in out:
        print(f"  {GREEN}✓{OFF} reported a generated release note without failing")
    else:
        print(f"  {RED}✗{OFF} mishandled a generated release note")
        failures += 1

    buf = io.StringIO()
    with contextlib.redirect_stdout(buf):
        code = run(src_dir)
    if code == 0:
        print(f"  {GREEN}✓{OFF} the unmodified tree passes")
    else:
        print(f"  {RED}✗{OFF} the unmodified tree FAILS")
        print(buf.getvalue())
        failures += 1

    print()
    if failures:
        print(f"{failures} self-test failure(s).")
        return 1
    print("The checker reports every planted defect.")
    return 0


def _write(docs_dir: str, rel: str, body: str) -> None:
    path = os.path.join(docs_dir, rel)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as fh:
        fh.write(f"---\ntitle: Probe\n---\n\n{body}")


def main() -> int:
    os.chdir(ROOT)
    if "--self-test" in sys.argv:
        return self_test()
    return run(os.path.join(ROOT, DOCS))


if __name__ == "__main__":
    sys.exit(main())
