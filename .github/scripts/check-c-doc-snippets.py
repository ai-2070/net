#!/usr/bin/env python3
"""Compile every complete C program published in the docs.

The C quickstart shipped for a long time calling `free(cursor)` while
including only `net.h`, `<stdio.h>` and `<string.h>`. Copying it into a
clean project and building it the way the page implies fails before it
links:

    app.c:24:9: error: call to undeclared library function 'free'
    note: include the header <stdlib.h> ...

Nothing caught it because the snippets live in markdown. `.github/scripts/
check-skill-examples.sh` syntax-checks the *skill* examples, which are real
files on disk; the doc-site fences had no equivalent floor. This script is
that floor: it pulls every ```c fence that contains a `main` out of the
published C pages and compiles it against the real public headers.

Strict ISO C11 with warnings as errors, because that is what a careful
reader compiles a quickstart with, and because it is the setting that
catches the missing-header class at all. Note the consequence: POSIX-only
functions (`strdup`, `strndup`) are not declared under `-std=c11`, so
snippets must stay within ISO C or be marked `no-compile`.

A fence is skipped when it has no `main` (a fragment) or when the line
before it is `<!-- c-doc-snippets: no-compile -->`.

Usage: check-c-doc-snippets.py [--cc gcc] [--self-test]
Exits 0 if every complete program compiles, 1 otherwise.

`--self-test` guards the checker rather than the docs: it plants the exact
defect that shipped — a program that calls `free` with no `<stdlib.h>` — and
requires the checker to report it. A checker that silently extracts nothing
passes every run while checking nothing.
"""

from __future__ import annotations

import argparse
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

_ROOT = Path(__file__).resolve().parents[2]
_INCLUDE = _ROOT / "net" / "crates" / "net" / "include"

# Published surfaces that carry copy-and-run C programs. Adding a new C page
# means adding it here — the check is explicit rather than a glob so that a
# page of deliberate fragments doesn't silently start failing the build.
_SOURCES = [
    Path("web/src/content/docs/sdk/c/quickstart.md"),
    Path("web/src/content/docs/sdk/c/headers-and-linking.md"),
    Path("web/src/content/docs/sdk/c/memory-and-threading.md"),
    Path("net/crates/net/include/README.md"),
]

_OPT_OUT = "<!-- c-doc-snippets: no-compile -->"
_FENCE = re.compile(r"^```c\s*$")
_FENCE_END = re.compile(r"^```\s*$")
_HAS_MAIN = re.compile(r"\bmain\s*\(")

_CFLAGS = ["-std=c11", "-Wall", "-Wextra", "-Werror", "-fsyntax-only"]


class Snippet:
    def __init__(self, source: Path, start_line: int, code: str) -> None:
        self.source = source
        self.start_line = start_line
        self.code = code

    def __str__(self) -> str:
        return f"{self.source}:{self.start_line}"


def extract(path: Path) -> list[Snippet]:
    """Pull complete C programs out of a markdown file's ```c fences."""
    lines = path.read_text(encoding="utf-8").splitlines()
    out: list[Snippet] = []
    i = 0
    while i < len(lines):
        if not _FENCE.match(lines[i]):
            i += 1
            continue
        opt_out = i > 0 and lines[i - 1].strip() == _OPT_OUT
        fence_line = i + 1  # 1-indexed line of the opening fence
        i += 1
        body: list[str] = []
        while i < len(lines) and not _FENCE_END.match(lines[i]):
            body.append(lines[i])
            i += 1
        i += 1  # step past the closing fence
        code = "\n".join(body)
        if opt_out or not _HAS_MAIN.search(code):
            continue
        out.append(Snippet(path, fence_line, code + "\n"))
    return out


def compile_one(cc: str, snippet: Snippet, workdir: Path) -> str | None:
    """Compile a snippet; return compiler output on failure, None on success."""
    src = workdir / f"snippet_{abs(hash(str(snippet)))}.c"
    src.write_text(snippet.code, encoding="utf-8")
    proc = subprocess.run(
        [cc, *_CFLAGS, "-I", str(_INCLUDE), str(src)],
        capture_output=True,
        text=True,
    )
    if proc.returncode == 0:
        return None
    return (proc.stdout + proc.stderr).replace(str(src), str(snippet.source))


_SELF_TEST_MD = """\
Prose before the fence.

```c
#include <stdio.h>

int main(void) {
    char *cursor = NULL;
    printf("hi\\n");
    free(cursor);          /* no <stdlib.h> — this is the defect that shipped */
    return 0;
}
```

A fragment with no `main`, which must be skipped:

```c
net_shutdown(node);
```

An explicitly exempt program, which must also be skipped:

<!-- c-doc-snippets: no-compile -->
```c
int main(void) { this is not C at all; }
```
"""


def self_test(cc: str) -> int:
    """Plant the shipped defect and require the checker to report it."""
    print("==> self-test: the checker must catch a missing <stdlib.h>")
    with tempfile.TemporaryDirectory() as tmp:
        workdir = Path(tmp)
        md = workdir / "planted.md"
        md.write_text(_SELF_TEST_MD, encoding="utf-8")

        snippets = extract(md)
        if len(snippets) != 1:
            print(
                f"FAIL  extracted {len(snippets)} program(s), expected exactly 1 "
                "(the fragment and the exempt block must be skipped)"
            )
            return 1

        if compile_one(cc, snippets[0], workdir) is None:
            print("FAIL  the planted missing-<stdlib.h> program compiled clean")
            print("      the compiler flags are too lax to catch what shipped")
            return 1

    print("  ok    defect planted and reported")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--cc", default=None, help="C compiler to use")
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="check that the checker still catches the defect that shipped",
    )
    parser.add_argument(
        "--require-cc",
        action="store_true",
        help="fail instead of skipping when no C compiler is found (use in CI)",
    )
    args = parser.parse_args()

    cc = args.cc or shutil.which("gcc") or shutil.which("cc") or shutil.which("clang")
    if cc is None:
        # Skipping is right on a contributor's machine — not having a C
        # toolchain is a reasonable state to edit markdown in. It is wrong in
        # CI, where a missing compiler means this gate silently stopped
        # checking anything while still reporting a green step. `--require-cc`
        # is how CI says which of the two it is.
        if args.require_cc:
            print(
                "FAIL  no C compiler on PATH, and --require-cc was passed.\n"
                "      This gate compiles every published C program; without a\n"
                "      compiler it verifies nothing and would report success."
            )
            return 1
        print("SKIP  no C compiler on PATH (pass --require-cc to make this fatal)")
        return 0

    if args.self_test:
        return self_test(cc)

    if not _INCLUDE.is_dir():
        print(f"FAIL  public header directory is missing: {_INCLUDE}")
        return 1

    snippets: list[Snippet] = []
    for rel in _SOURCES:
        path = _ROOT / rel
        if not path.is_file():
            print(f"FAIL  documented C source is missing: {rel}")
            return 1
        snippets.extend(extract(path))

    if not snippets:
        print("FAIL  no complete C programs found; the extractor is broken")
        return 1

    print(f"==> C doc snippets: {cc} {' '.join(_CFLAGS)}")
    failures = 0
    with tempfile.TemporaryDirectory() as tmp:
        workdir = Path(tmp)
        for snippet in snippets:
            error = compile_one(cc, snippet, workdir)
            if error is None:
                print(f"  ok    {snippet}")
            else:
                failures += 1
                print(f"  FAIL  {snippet}")
                for line in error.strip().splitlines()[:20]:
                    print(f"        {line}")

    if failures:
        print(
            f"\n{failures} published C program(s) do not compile. A quickstart "
            "that does not build as copied is worse than no quickstart."
        )
        return 1

    print(f"\nAll {len(snippets)} published C program(s) compile.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
