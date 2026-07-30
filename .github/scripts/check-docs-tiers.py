#!/usr/bin/env python3
"""Prove `docs/data/tiers.yaml` still describes the docs tree, and ratchet it.

WHY THIS EXISTS. The polyglot lens turns on every page declaring a migration
state, and a taxonomy nobody checks is an estimate that ages. The plan's own first
draft carried "~34 adaptive pages" for a corpus that had 29 of them under its own
rules — an approximate table is not a foundation a checker can stand on.

WHAT IT PROVES
  1. Exhaustive and disjoint: every .md under the docs tree has exactly one state,
     and no state names a page that is gone. A new page with no state fails here
     rather than silently defaulting to one.
  2. The states are drawn from a closed set.
  3. The `adaptive_pending` ratchet:
       - every pending page is on the allowlist;
       - nothing off the allowlist is pending;
       - the count does not exceed the committed `max`;
       - a page converted away from pending is removed from the list.

WHY AN ALLOWLIST RATHER THAN A COUNT. A count is not enforceable — a checker sees
one checkout, not yesterday's — and it is too weak even if it were: it would let
one page regress to pending while another converts in the same change, netting
zero. The exact list makes the shrink visible in the diff and reviewable as
content.

  .github/scripts/check-docs-tiers.py              # check
  .github/scripts/check-docs-tiers.py --self-test  # plant defects
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tempfile

try:
    import yaml
except ModuleNotFoundError:  # pragma: no cover - environment problem, not a defect
    sys.exit("PyYAML is required: python3 -m pip install pyyaml")

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
DOCS = os.environ.get("DOCS_CONTENT_DIR", "web/src/content/docs")
TIERS = os.environ.get("TIERS_FILE", "docs/data/tiers.yaml")

STATES = {
    "universal",
    "adaptive",
    "adaptive_pending",
    "sdk_native",
    "boundary_native",
}

RED, GREEN, DIM, OFF = "\033[31m", "\033[32m", "\033[2m", "\033[0m"


def page_slugs(docs_dir: str) -> set[str]:
    """Every page, as the slug its URL uses.

    A folder README renders at the folder's URL (`start/README.md` is served at
    `/docs/start`), so it is keyed by the folder — matching `lib/docs.ts` rather
    than the filesystem.
    """
    out = set()
    for dirpath, _dirs, files in os.walk(docs_dir):
        rel_dir = os.path.relpath(dirpath, docs_dir)
        # Skip dot directories (`.next` lives under the content tree). Tested per
        # path COMPONENT, not as a substring: `os.sep + "." in dirpath` also
        # matches the `/..` in a relative path, which silently pruned the entire
        # walk when the content dir was reached relatively — found by the
        # self-test below, which reported every page as missing.
        if any(part.startswith(".") and part not in (".", "..")
               for part in rel_dir.split(os.sep)):
            continue
        for name in files:
            if not name.endswith(".md"):
                continue
            rel = os.path.relpath(os.path.join(dirpath, name), docs_dir)
            parts = rel[:-3].split(os.sep)
            if parts[-1] == "README":
                parts = parts[:-1]
            out.add("/".join(parts) or "index")
    return out


def run() -> int:
    fail = 0
    with open(os.path.join(ROOT, TIERS), encoding="utf-8") as fh:
        doc = yaml.safe_load(fh)
    pages = doc.get("pages") or {}
    pending_block = doc.get("adaptive_pending") or {}
    allowlist = list(pending_block.get("pages") or [])
    max_pending = pending_block.get("max")

    actual = page_slugs(os.path.join(ROOT, DOCS))
    declared = set(pages)

    print(f"==> Tier manifest ({TIERS})")

    missing = sorted(actual - declared)
    for slug in missing:
        print(f"  {RED}✗{OFF} page has no migration state: {slug}")
        fail += 1
    phantom = sorted(declared - actual)
    for slug in phantom:
        print(f"  {RED}✗{OFF} state declared for a page that does not exist: {slug}")
        fail += 1

    bad_state = sorted(s for s, t in pages.items() if t not in STATES)
    for slug in bad_state:
        print(f"  {RED}✗{OFF} {slug}: unknown state {pages[slug]!r} "
              f"(closed set: {', '.join(sorted(STATES))})")
        fail += 1

    if not (missing or phantom or bad_state):
        counts: dict[str, int] = {}
        for state in pages.values():
            counts[state] = counts.get(state, 0) + 1
        print(f"  {GREEN}✓{OFF} {len(pages)} pages, exhaustive and disjoint")
        for state, count in sorted(counts.items(), key=lambda kv: -kv[1]):
            print(f"      {count:>3} {DIM}{state}{OFF}")

    # ---- the ratchet
    print()
    print("==> adaptive_pending ratchet")
    pending = {s for s, t in pages.items() if t == "adaptive_pending"}
    listed = set(allowlist)

    for slug in sorted(pending - listed):
        print(f"  {RED}✗{OFF} pending but not on the allowlist: {slug}")
        print(f"      A page may not enter adaptive_pending. Convert it, or if it "
              f"is genuinely legacy, add it and say so in review.")
        fail += 1
    for slug in sorted(listed - pending):
        print(f"  {RED}✗{OFF} on the allowlist but not pending: {slug}")
        print(f"      Converted pages come off the list in the same change.")
        fail += 1
    if len(allowlist) != len(listed):
        print(f"  {RED}✗{OFF} the allowlist has duplicate entries")
        fail += 1
    if not isinstance(max_pending, int):
        print(f"  {RED}✗{OFF} adaptive_pending.max is missing or not an integer")
        fail += 1
    elif len(pending) > max_pending:
        print(f"  {RED}✗{OFF} {len(pending)} pending pages exceeds the committed "
              f"max of {max_pending}")
        fail += 1
    elif not (pending - listed or listed - pending):
        headroom = max_pending - len(pending)
        print(f"  {GREEN}✓{OFF} {len(pending)} pending, allowlist matches exactly, "
              f"max {max_pending}")
        if headroom:
            print(f"      {DIM}{headroom} converted since max was last lowered — "
                  f"lower it to keep the ratchet tight{OFF}")

    print()
    if fail == 0:
        print("The tier manifest describes the tree, and the ratchet holds.")
        return 0
    print(f"{fail} tier problem(s).")
    return 1


def self_test() -> int:
    """Plant one defect per rule and require each to be reported."""
    print("==> Self-test — planting defects in a scratch copy")
    cases = [
        ("an undeclared page", "no migration state"),
        ("a state for a deleted page", "does not exist"),
        ("an unknown state name", "unknown state"),
        ("a page pending but not allowlisted", "not on the allowlist"),
        ("an allowlisted page that converted", "not pending"),
        ("more pending pages than max", "exceeds the committed max"),
    ]
    failures = 0
    with tempfile.TemporaryDirectory() as tmp:
        content = os.path.join(tmp, "content")
        shutil.copytree(os.path.join(ROOT, DOCS), content)
        def probe(mutate, content_dir=content):
            doc = yaml.safe_load(open(os.path.join(ROOT, TIERS), encoding="utf-8"))
            mutate(doc)
            path = os.path.join(tmp, "tiers.yaml")
            with open(path, "w", encoding="utf-8") as fh:
                yaml.safe_dump(doc, fh)
            proc = subprocess.run(
                [sys.executable, os.path.abspath(__file__)],
                capture_output=True, text=True, cwd=ROOT,
                # Absolute, deliberately: os.path.join(ROOT, abs) returns abs,
                # and a relative tmp path would be `../../../../../var/...`,
                # which is exactly the shape that broke the walk guard once.
                env={**os.environ, "TIERS_FILE": path,
                     "DOCS_CONTENT_DIR": content_dir},
            )
            return proc.returncode, proc.stdout + proc.stderr

        def drop_a_page(doc):
            del doc["pages"]["concepts/channels"]

        def phantom(doc):
            doc["pages"]["concepts/does-not-exist"] = "universal"

        def bad_state(doc):
            doc["pages"]["concepts/channels"] = "sort_of_adaptive"

        def unlisted_pending(doc):
            doc["pages"]["concepts/channels"] = "adaptive_pending"

        def stale_allowlist(doc):
            doc["pages"]["start/install"] = "adaptive"

        def over_max(doc):
            doc["adaptive_pending"]["max"] = 3

        for (label, needle), mutate in zip(cases, [
            drop_a_page, phantom, bad_state, unlisted_pending, stale_allowlist,
            over_max,
        ]):
            rc, out = probe(mutate)
            if rc != 0 and needle in out:
                print(f"  {GREEN}✓{OFF} reported {label}")
            else:
                print(f"  {RED}✗{OFF} MISSED {label} (rc={rc}, looked for "
                      f"{needle!r})")
                failures += 1

        # And the unmutated manifest against the real tree must pass, or every
        # result above is meaningless.
        rc, out = probe(lambda _doc: None)
        if rc != 0:
            print(f"  {RED}✗{OFF} the UNMODIFIED manifest fails — the checks above "
                  f"prove nothing")
            print(out)
            failures += 1
        else:
            print(f"  {GREEN}✓{OFF} the unmodified manifest passes")

    print()
    if failures:
        print(f"{failures} self-test failure(s).")
        return 1
    print("The checker reports every planted defect.")
    return 0


def main() -> int:
    os.chdir(ROOT)
    if "--self-test" in sys.argv:
        return self_test()
    return run()


if __name__ == "__main__":
    sys.exit(main())
