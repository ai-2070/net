#!/usr/bin/env python3
"""Assert the workflow triggers cover every source path the skills cite.

A check that never runs is worse than no check: it reads as coverage. When this
was written, 36 of the 66 cited-and-tracked source paths fell outside
`skills.yml`'s path globs — the whole Go binding, both non-Rust SDKs, the C
headers, and `bindings/node/*.ts` (which `bindings/*/src/**` misses because those
files sit at the package root, not under `src/`). The enum and identifier checks
could have been extended to those languages in full and still never fired on the
changes that invalidate them.

This closes the loop: cite a path in a skill, and the workflow must watch it.

Deliberately does not import PyYAML — it is not guaranteed on a runner, and the
`paths:` blocks are a fixed, simple shape. Prints one line per uncovered path and
exits 1; silent exit 0 when clean.
"""

import pathlib
import re
import subprocess
import sys

WORKFLOW = pathlib.Path(".github/workflows/skills.yml")
SKILLS = pathlib.Path(".claude/skills")
CITED = re.compile(r"`((?:net|go|web)/[A-Za-z0-9_/.-]+)`")


def trigger_globs():
    """Every quoted entry under a `paths:` block, until the block dedents."""
    globs, in_paths = set(), False
    for line in WORKFLOW.read_text().splitlines():
        if re.match(r"^\s+paths:\s*$", line):
            in_paths = True
            continue
        if in_paths:
            m = re.match(r'^\s+-\s+"([^"]+)"\s*$', line)
            if m:
                globs.add(m.group(1))
            elif line.strip() and not line.strip().startswith("#"):
                in_paths = False
    return globs


def to_regex(glob):
    """GitHub path-glob semantics: ** crosses separators, * does not.

    `foo/**` is treated as covering `foo` itself, not just its contents — the
    skills cite directories (`net/crates/net/sdk-ts/`) as often as files, and a
    change under such a directory is exactly what the glob is there to catch.
    """
    trailing_star_star = glob.endswith("/**")
    if trailing_star_star:
        glob = glob[: -len("/**")]

    out, i = [], 0
    while i < len(glob):
        if glob.startswith("**", i):
            out.append(".*")
            i += 2
        elif glob[i] == "*":
            out.append("[^/]*")
            i += 1
        else:
            out.append(re.escape(glob[i]))
            i += 1

    body = "".join(out)
    return re.compile("^" + body + ("(/.*)?$" if trailing_star_star else "$"))


def cited_paths():
    paths = set()
    for md in SKILLS.rglob("*.md"):
        for hit in CITED.findall(md.read_text()):
            paths.add(re.sub(r":[0-9,-]*$", "", hit))
    return paths


def tracked(path):
    """Only assert on paths git actually has — an untracked citation is the
    `cited path` check's business, not ours, and we should not report it twice."""
    if subprocess.run(
        ["git", "ls-files", "--error-unmatch", path],
        capture_output=True,
    ).returncode == 0:
        return True
    return bool(
        subprocess.run(
            ["git", "ls-files", path], capture_output=True, text=True
        ).stdout.strip()
    )


def main():
    matchers = [to_regex(g) for g in trigger_globs()]
    if not matchers:
        print("could not parse any trigger paths from skills.yml — check the format")
        return 1

    uncovered = sorted(
        p
        for p in cited_paths()
        if tracked(p) and not any(m.match(p.rstrip("/")) for m in matchers)
    )
    for p in uncovered:
        print(f"cited path is not covered by any trigger glob in skills.yml: {p}")
    return 1 if uncovered else 0


if __name__ == "__main__":
    sys.exit(main())
