#!/usr/bin/env python3
"""Assert `skills.yml` is wired so its checks actually gate what it publishes.

Two invariants, both about the workflow rather than the skills themselves.

**Trigger coverage.** Assert the triggers cover every source path the skills cite.

A check that never runs is worse than no check: it reads as coverage. When this
was written, 36 of the 66 cited-and-tracked source paths fell outside
`skills.yml`'s path globs — the whole Go binding, both non-Rust SDKs, the C
headers, and `bindings/node/*.ts` (which `bindings/*/src/**` misses because those
files sit at the package root, not under `src/`). The enum and identifier checks
could have been extended to those languages in full and still never fired on the
changes that invalidate them.

This closes the loop: cite a path in a skill, and the workflow must watch it.

**Publish gating.** Assert every job gates the mirror. `publish` copies
`.claude/skills/` to the public repo; a verification job that is not in
`publish.needs` is advisory, so a red result publishes the thing it just proved
broken. Fail-closed by default: every job except `publish` must appear in
`publish.needs`, unless it carries an explicit

    # advisory: <reason>

comment on the line above its name. Making the exception visible and deliberate
is the point — the failure mode this guards is someone adding a job in a later
phase and simply forgetting the dependency.

Deliberately does not import PyYAML — it is not guaranteed on a runner, and the
blocks parsed here are a fixed, simple shape. Prints one line per violation and
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


def jobs_and_gating():
    """(job name -> advisory?, set of names in publish.needs).

    Job names are the only keys indented exactly two spaces under `jobs:`.
    `needs:` is accepted in both flow (`[a, b]`) and block (`- a`) form.
    """
    jobs, needs = {}, set()
    in_jobs = current = None
    advisory_next = in_needs_block = False

    for line in WORKFLOW.read_text().splitlines():
        if re.match(r"^jobs:\s*$", line):
            in_jobs = True
            continue
        if not in_jobs:
            continue

        stripped = line.strip()
        if not stripped:
            continue
        if stripped.startswith("#"):
            advisory_next = advisory_next or bool(re.match(r"#\s*advisory:", stripped))
            continue

        job = re.match(r"^  ([A-Za-z_][\w-]*):\s*$", line)
        if job:
            current = job.group(1)
            jobs[current] = advisory_next
            advisory_next = in_needs_block = False
            continue
        advisory_next = False

        if current != "publish":
            in_needs_block = False
            continue

        if in_needs_block:
            item = re.match(r"^\s+-\s+([A-Za-z_][\w-]*)\s*$", line)
            if item:
                needs.add(item.group(1))
                continue
            in_needs_block = False

        flow = re.match(r"^\s+needs:\s*\[(.+)\]\s*$", line)
        if flow:
            needs |= {n.strip() for n in flow.group(1).split(",") if n.strip()}
        elif re.match(r"^\s+needs:\s*$", line):
            in_needs_block = True
        else:
            scalar = re.match(r"^\s+needs:\s*([A-Za-z_][\w-]*)\s*$", line)
            if scalar:
                needs.add(scalar.group(1))
    return jobs, needs


def main():
    problems = []

    matchers = [to_regex(g) for g in trigger_globs()]
    if not matchers:
        print("could not parse any trigger paths from skills.yml — check the format")
        return 1
    for p in sorted(cited_paths()):
        if tracked(p) and not any(m.match(p.rstrip("/")) for m in matchers):
            problems.append(
                f"cited path is not covered by any trigger glob in skills.yml: {p}"
            )

    jobs, needs = jobs_and_gating()
    if "publish" not in jobs:
        problems.append("skills.yml has no `publish` job — parser or workflow changed")
    else:
        for name, advisory in sorted(jobs.items()):
            if name == "publish" or advisory:
                continue
            if name not in needs:
                problems.append(
                    f"job `{name}` is not in publish.needs — a red run would still "
                    f"mirror to net-claude-skill. Add it, or mark it "
                    f"`# advisory: <reason>`."
                )

    for p in problems:
        print(p)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
