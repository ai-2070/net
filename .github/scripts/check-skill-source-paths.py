#!/usr/bin/env python3
"""Verify every repository path — and line anchor — the skills cite.

WHY THIS EXISTS. The skills cite 200+ source paths and 86 line-anchored
locations (`config.rs:660-664`), and until now nothing checked a single one. A
file rename or a refactor above line N is invisible to the skill corpus, so a
citation rots silently and an agent following it reports "that file does not
exist" and starts guessing. That is the failure mode the whole verification
programme exists to prevent.

It got sharper with `source-access.md`. A citation is no longer just a hint for
someone already sitting in the Net checkout — it is an instruction an external
agent executes against an `opensrc` cache. A path that does not resolve is now
a dead end in somebody else's terminal.

THE ROOT MAP IS THE POINT. Only about 90 of the citations are repo-rooted
(`net/crates/net/sdk/src/mesh.rs`). The rest are shorthand relative to a
subsystem — `x402/mod.rs`, `core/quote.rs`, `checker/svm.rs` — which reads fine
in context and is unresolvable to anyone who does not already know the
subsystem root. ROOTS below is that map, and `source-access.md` publishes the
same list to the agent. One list, two consumers: the checker keeps the
published map honest.

WHAT A LINE ANCHOR CAN AND CANNOT PROVE. We check that the file exists and that
the cited line is inside it. We cannot check that line 1890 still holds what the
skill says it holds — that needs a symbol, not a number. So an in-range anchor
is a floor, not a guarantee; an out-of-range one is proof of rot.

RELATIONSHIP TO `check-skills.sh`. That script already checks the repo-rooted
citations (``net/…``, ``go/…``, ``web/…``) against **git**, deliberately: a
developer's tree holds build output a clean CI checkout does not, so an
`os.path.exists` test passes where it should fail. This script keeps the same
git-tracked semantics — two gates that disagreed about what "exists" means would
be worse than one. What it adds is the forms that one cannot see: subsystem
shorthand, skill-relative refs, filename globs, and every line anchor. Anchors
in particular were invisible there, because its character class excludes `:`, so
a citation carrying one never matched in the first place.

  .github/scripts/check-skill-source-paths.py              # check
  .github/scripts/check-skill-source-paths.py --self-test  # plant defects
  SKILLS_DIR=/tmp/copy .github/scripts/check-skill-source-paths.py
"""

from __future__ import annotations

import fnmatch
import os
import re
import shutil
import subprocess
import sys
import tempfile

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
SKILLS = os.environ.get("SKILLS_DIR", ".claude/skills")

# Where a shorthand citation is allowed to be rooted. Ordered: the first root
# that resolves wins, so the most specific answer is not shadowed by a
# coincidence higher up. Every entry earns its place by being the implicit
# subject of at least one chapter — a reader of `x402.md` is inside
# `payments/src/`, and writing the full prefix on every line would bury the
# name being cited.
ROOTS = [
    "",  # repo-rooted — the unambiguous form, and the majority
    "net/crates/net/",  # core crate: sdk/, sdk-ts/, bindings/, include/, tests/
    "net/crates/net/src/",  # core internals: bus.rs, config.rs, adapter/
    "net/crates/net/src/adapter/net/",  # mesh behaviour: behavior/, channel/, cortex/
    "net/crates/net/payments/",  # payments crate: src/, tests/
    "net/crates/net/payments/src/",  # payments modules: core/, x402/, engine/, flow/
    "net/crates/net/cli/src/",  # `net-mesh` command tree: commands/
    "net/crates/net/bindings/",  # go/rpc-ffi/, node/src/, python/src/
    "net/crates/net/bindings/python/",  # pytest suites under tests/
    "net/crates/net/tests/cross_lang_payments/",  # x402 conformance fixtures
]

# Cited files that a clean checkout does not contain because a build step
# produces them. Existence is not required; a present copy is still range
# checked. Each must be genuinely git-ignored — verified below, so that if one
# ever gets committed this list stops lying about it.
GENERATED = {
    "net/crates/net/bindings/node/index.d.ts": (
        "napi-generated. Absent from every clean checkout and from an opensrc "
        "cache. source-access.md names the readable substitute: the #[napi] "
        "declarations in bindings/node/src/*.rs."
    ),
}

# Paths that do not exist and should not. Each needs a reason, because the
# alternative is a growing pile of silently-skipped citations that makes the
# check look thorough while covering less every quarter.
ALLOW = {
    "specs/x402-specification-v2.md": (
        "external — lives in the x402 spec repo, not this one"
    ),
    "tests/cross_lang_nrpc.rs": (
        "proposed, not written — nrpc.md describes it as future work "
        "('add a … driver gated on CROSS_LANG_NRPC=1'). The sibling "
        "tests/cross_lang_nrpc/ fixture directory does exist."
    ),
    ".agents/skills/": "an install target on the reader's machine, not a repo path",
}

FENCE = re.compile(r"^\s*(```|~~~)")
SPAN = re.compile(r"`([^`\n]+)`")
PATHISH = re.compile(r"^[A-Za-z0-9_.][A-Za-z0-9_.*/-]*$")
# `path.ext:12`, `path.ext:12-34`, `path.ext:63,70`
LINEREF = re.compile(r"^([A-Za-z0-9_.][A-Za-z0-9_./-]*\.[A-Za-z]+):([\d,\s-]+)$")
TOP = ("net/", "go/", "web/", "docs/", ".github/")
EXT = (
    ".rs", ".ts", ".tsx", ".py", ".pyi", ".go", ".h", ".c",
    ".json", ".toml", ".md", ".yml", ".yaml", ".sh", ".mjs",
)

RED, GREEN, DIM, OFF = "\033[31m", "\033[32m", "\033[2m", "\033[0m"


def is_citation(s: str) -> bool:
    """Positive identification, deliberately not exhaustive.

    The corpus is full of slash-shaped things that are not paths: channel
    names (`sensors/lidar/front`), MIME types (`application/octet-stream`),
    MCP method names (`tools/call`), Go imports (`encoding/json`), prose
    (`try/catch`, `issued/accepted/expired/declined`). A rule that tried to
    catch every citation would fail the build over a channel name, so this one
    only claims the forms that cannot be anything else.
    """
    if "/" not in s or not PATHISH.match(s):
        return False
    last = s.rsplit("/", 1)[1]
    if s.startswith(TOP) or s.endswith("/") or s.endswith(EXT):
        return True
    # A glob is a citation only if it globs a *filename* — `observe.*` yes,
    # `forward/*` (an MCP method prefix) no.
    return "*" in last and "." in last


def scan(skills_dir: str) -> tuple[dict, dict, int]:
    """Return (path citations, line-anchored citations, bare-anchor count)."""
    paths: dict[str, list[tuple[str, int]]] = {}
    anchors: dict[tuple[str, str], list[tuple[str, int]]] = {}
    bare = 0
    for dirpath, _dirs, files in os.walk(skills_dir):
        for name in sorted(files):
            if not name.endswith(".md"):
                continue
            mdpath = os.path.join(dirpath, name)
            in_fence = False
            with open(mdpath, encoding="utf-8") as fh:
                for lineno, line in enumerate(fh, 1):
                    if FENCE.match(line):
                        in_fence = not in_fence
                        continue
                    if in_fence:
                        continue
                    for span in SPAN.findall(line):
                        span = span.strip()
                        ref = LINEREF.match(span)
                        if ref:
                            target, rng = ref.group(1), ref.group(2)
                            # A bare filename (`mesh.rs:411`) is shorthand for
                            # whichever file the surrounding chapter is about.
                            # Several such basenames exist more than once in the
                            # tree, so resolving them would mean guessing. Counted
                            # and reported rather than silently dropped.
                            if "/" not in target:
                                bare += 1
                                continue
                            anchors.setdefault((target, rng), []).append(
                                (mdpath, lineno)
                            )
                        elif is_citation(span):
                            paths.setdefault(span, []).append((mdpath, lineno))
    return paths, anchors, bare


def tracked_paths() -> tuple[frozenset[str], tuple[str, ...]]:
    """Everything git tracks, as a file set plus a sorted tuple for prefixes."""
    out = subprocess.run(
        ["git", "ls-files", "-z"], cwd=ROOT, capture_output=True, text=True,
        check=True,
    ).stdout
    files = frozenset(p for p in out.split("\0") if p)
    return files, tuple(sorted(files))


TRACKED, TRACKED_SORTED = tracked_paths()


def is_tracked(target: str) -> bool:
    """True if git tracks `target` as a file, a directory, or a glob match.

    Checked against git rather than the filesystem for the same reason
    `check-skills.sh` does it: a citation to a generated file passes on the
    machine where it was written and fails on a clean checkout, which is exactly
    backwards. Files that only exist after a build belong in GENERATED, with a
    reason, not in a green tick.
    """
    if target in TRACKED:
        return True
    if "*" in target:
        return any(fnmatch.fnmatch(p, target) for p in TRACKED_SORTED)
    prefix = target if target.endswith("/") else target + "/"
    return any(p.startswith(prefix) for p in TRACKED_SORTED)


def candidates(cite: str, skill_dir: str):
    """Yield (root label, repo-relative target) for every root worth trying.

    `skill_dir` is tried last so a skill-internal reference (`bindings/rust.md`,
    `../examples/observe.go`) resolves without needing its own ROOTS entry.
    `check-skills.sh` already validates those as cross-references; resolving
    them here too costs nothing and keeps this check from reporting them as
    missing source.

    Targets are normalized because the corpus writes `../examples/hello.rs` and
    git's index has no `..` in it — the un-normalized string matches nothing, so
    a perfectly good reference reads as broken.
    """
    for root in [*ROOTS, skill_dir + "/"]:
        target = os.path.normpath(root + cite)
        if cite.endswith("/"):
            target += "/"
        yield (root or "<repo root>"), target


def resolve(cite: str, skill_dir: str) -> tuple[str, str] | None:
    """Return (root label, resolved target), or None."""
    for label, target in candidates(cite, skill_dir):
        if is_tracked(target):
            return label, target
    return None


def generated_match(cite: str, skill_dir: str) -> bool:
    """True if `cite` names a GENERATED file under any root.

    Matched after rooting, because the corpus cites the same file both ways:
    `net/crates/net/bindings/node/index.d.ts` in one chapter and the shorthand
    `bindings/node/index.d.ts` in another.
    """
    return any(target in GENERATED for _label, target in candidates(cite, skill_dir))


def line_count(path: str) -> int:
    with open(path, encoding="utf-8", errors="replace") as fh:
        return sum(1 for _ in fh)


def check_generated() -> list[str]:
    """Every GENERATED entry must really be git-ignored."""
    problems = []
    for path, reason in GENERATED.items():
        proc = subprocess.run(
            ["git", "check-ignore", "-q", path],
            cwd=ROOT, capture_output=True,
        )
        if proc.returncode != 0:
            problems.append(
                f"{path} is listed as generated ({reason.split('.')[0]}) but "
                f"git does not ignore it — either it is committed now, or the "
                f".gitignore rule went away."
            )
    return problems


def run(skills_dir: str) -> int:
    paths, anchors, bare = scan(skills_dir)
    if not paths:
        print(f"  {RED}✗{OFF} no citations found under {skills_dir} — "
              f"the extractor is broken, not the corpus")
        return 1

    fail = 0
    by_root: dict[str, int] = {}
    allowed: list[str] = []
    generated = 0
    broken: list[tuple[str, str, int]] = []

    for cite, locs in sorted(paths.items()):
        if cite in ALLOW:
            allowed.append(cite)
            continue
        skill_dir = os.path.dirname(locs[0][0])
        if generated_match(cite, skill_dir):
            generated += 1
            continue
        found_root = resolve(cite, skill_dir)
        if found_root is None:
            broken.append((cite, *locs[0]))
        else:
            by_root[found_root[0]] = by_root.get(found_root[0], 0) + 1

    print(f"==> Source paths cited by the skills ({len(paths)} distinct)")
    for root, count in sorted(by_root.items(), key=lambda kv: -kv[1]):
        print(f"  {GREEN}✓{OFF} {count:>3} under {DIM}{root}{OFF}")
    if allowed:
        print(f"  {DIM}    {len(allowed)} allowed non-existent "
              f"(see ALLOW, each with a reason){OFF}")
    if generated:
        print(f"  {DIM}    {generated} in build-generated files "
              f"(see GENERATED){OFF}")

    for cite, mdpath, mdline in broken:
        print(f"  {RED}✗{OFF} {cite}")
        print(f"      cited at {mdpath}:{mdline}")
        fail += 1
    if broken:
        print("      Either the file moved (fix the citation), the root is new")
        print("      (add it to ROOTS *and* to source-access.md), or it should")
        print("      never exist (add it to ALLOW with a reason).")

    # Line anchors.
    print()
    print(f"==> Line anchors ({len(anchors)} distinct)")
    in_range = 0
    skipped_generated = 0
    for (target, rng), locs in sorted(anchors.items()):
        skill_dir = os.path.dirname(locs[0][0])
        if generated_match(target, skill_dir):
            skipped_generated += 1
            continue
        found_root = resolve(target, skill_dir)
        if found_root is None:
            print(f"  {RED}✗{OFF} {target}:{rng} — file does not resolve")
            print(f"      cited at {locs[0][0]}:{locs[0][1]}")
            fail += 1
            continue
        total = line_count(found_root[1])
        cited = [int(n) for n in re.findall(r"\d+", rng)]
        if max(cited) > total:
            print(f"  {RED}✗{OFF} {target}:{rng} — out of range, the file has "
                  f"{total} lines")
            print(f"      cited at {locs[0][0]}:{locs[0][1]}")
            fail += 1
        else:
            in_range += 1
    print(f"  {GREEN}✓{OFF} {in_range:>3} resolve and point inside the file")
    if skipped_generated:
        print(f"  {DIM}    {skipped_generated} in generated files not present "
              f"in this checkout (see GENERATED){OFF}")
    if bare:
        print(f"  {DIM}    {bare} bare-filename anchors NOT checked "
              f"(`mesh.rs:411` — ambiguous by construction){OFF}")

    # An ALLOW entry for a citation nobody makes any more is the same rot this
    # check exists to catch, one level up: it makes the exception list look
    # deliberate while it quietly stops describing the corpus.
    stale = sorted(set(ALLOW) - set(allowed))
    for cite in stale:
        print(f"  {RED}✗{OFF} ALLOW entry is never cited: {cite} — drop it, or "
              f"fix the citation it was written for")
        fail += 1

    for problem in check_generated():
        print(f"  {RED}✗{OFF} {problem}")
        fail += 1

    print()
    if fail == 0:
        print("Every cited source path resolves; every line anchor is in range.")
        return 0
    print(f"{fail} citation problem(s).")
    return 1


def self_test() -> int:
    """Plant defects in a copy and require each one to be reported.

    A check nobody has watched fail is not known to work. One defect per branch
    of the resolution logic: a repo-rooted path, a shorthand that resolves
    against no root, a filename glob that matches nothing, and a line anchor
    past the end of a real file.
    """
    print("==> Self-test — planting defects in a scratch copy")
    defects = [
        ("net/crates/net/sdk/src/does_not_exist.rs", "repo-rooted path"),
        ("x402/no_such_module.rs", "subsystem shorthand"),
        ("examples/nothing_matches.*", "filename glob"),
        ("net/crates/net/src/config.rs:999999", "out-of-range line anchor"),
    ]
    failures = 0
    with tempfile.TemporaryDirectory() as tmp:
        copy = os.path.join(tmp, "skills")
        shutil.copytree(".claude/skills", copy)
        with open(os.path.join(copy, "net-payments", "PLANTED.md"), "w",
                  encoding="utf-8") as fh:
            fh.write("# planted\n\n")
            for cite, _kind in defects:
                fh.write(f"- see `{cite}`\n")

        proc = subprocess.run(
            [sys.executable, os.path.abspath(__file__)],
            capture_output=True, text=True, cwd=ROOT,
            env={**os.environ, "SKILLS_DIR": copy},
        )
        out = proc.stdout + proc.stderr
        if proc.returncode == 0:
            print(f"  {RED}✗{OFF} the checker passed a corpus with "
                  f"{len(defects)} planted defects")
            failures += 1
        for cite, kind in defects:
            # The out-of-range anchor is reported as `path:range`, so match on
            # the path plus the planted number rather than the raw span.
            needle = cite.split(":")[0]
            number = cite.split(":")[1] if ":" in cite else ""
            if needle in out and (not number or number in out):
                print(f"  {GREEN}✓{OFF} reported the {kind} defect")
            else:
                print(f"  {RED}✗{OFF} MISSED the {kind} defect ({cite})")
                failures += 1

    print()
    if failures:
        print(f"{failures} self-test failure(s) — the checker does not catch "
              f"what it claims to.")
        return 1
    print("The checker reports every planted defect.")
    return 0


def main() -> int:
    os.chdir(ROOT)
    if "--self-test" in sys.argv:
        return self_test()
    if "--generated" in sys.argv:
        # For `check-skills.sh`, which checks cited paths against git and would
        # otherwise fail every citation of a build-generated file. Sharing the
        # list keeps one record: without this, citing `index.d.ts` with a line
        # anchor passed there (its regex cannot match a `:`) while citing the
        # same file without one failed. Same file, two verdicts.
        for path in sorted(GENERATED):
            print(path)
        return 0
    return run(SKILLS)


if __name__ == "__main__":
    sys.exit(main())
