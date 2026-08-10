#!/usr/bin/env python3
"""A workflow script invoked by path must be executable in git.

A `run:` step that names a script directly — `.github/scripts/foo.sh` rather
than `bash .github/scripts/foo.sh` — needs the executable bit, or the runner
stops before the script does anything:

    /home/runner/.../4955329b.sh: line 1:
        /home/runner/work/net/net/.github/scripts/check-ts-consumer.sh: Permission denied
    Error: Process completed with exit code 126.

The bit lives in git's index, not the working tree, and a Windows checkout
has `core.filemode` off — so `chmod +x` there is a no-op against the index
and a new script commits as 100644 with nothing local to show for it. Every
test can pass on the author's machine and the step still cannot start.

This reads each workflow's `run:` blocks only. Scanning raw YAML text would
also match `paths:` filter entries, which name the same scripts and mean
nothing about how they are invoked.

Usage: check-script-permissions.py [--self-test]
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path

try:
    import yaml
except ImportError:  # pragma: no cover - CI installs pyyaml
    # Skipping is right on a contributor's machine; it is wrong in CI, where a
    # missing dependency means this gate stopped reading any workflow while
    # still reporting a green step. `--require-yaml` is how CI says which of
    # the two it is.
    if "--require-yaml" in sys.argv:
        print(
            "FAIL  pyyaml is not installed, and --require-yaml was passed.\n"
            "      This gate parses every workflow's `run:` blocks; without\n"
            "      pyyaml it checks nothing and would report success."
        )
        sys.exit(1)
    print("SKIP  pyyaml is not installed (pass --require-yaml to make this fatal)")
    sys.exit(0)

_ROOT = Path(__file__).resolve().parents[2]
_WORKFLOWS = _ROOT / ".github" / "workflows"

_SCRIPT = r"\.github/scripts/[\w.-]+\.(?:sh|py)"
# A path invoked on its own — optionally via $GITHUB_WORKSPACE or ./ — with no
# interpreter in front of it.
_DIRECT = re.compile(rf'(?<![\w./-])(?:\$GITHUB_WORKSPACE/|"\$GITHUB_WORKSPACE"/|\./)?({_SCRIPT})')
# Anything run through an explicit interpreter does NOT need the bit.
_VIA_INTERPRETER = re.compile(rf"(?:python3?|bash|sh|node|npx)\s+[^\n|;&]*?({_SCRIPT})")


def _run_blocks(path: Path) -> list[str]:
    """Every `run:` string in a workflow, including defaults and matrices."""
    doc = yaml.safe_load(path.read_text(encoding="utf-8"))
    out: list[str] = []

    def walk(node: object) -> None:
        if isinstance(node, dict):
            for key, value in node.items():
                if key == "run" and isinstance(value, str):
                    out.append(value)
                else:
                    walk(value)
        elif isinstance(node, list):
            for item in node:
                walk(item)

    walk(doc)
    return out


def directly_invoked() -> dict[str, set[str]]:
    """script path -> workflows that invoke it by path."""
    found: dict[str, set[str]] = {}
    for wf in sorted(_WORKFLOWS.glob("*.yml")):
        for block in _run_blocks(wf):
            for line in block.splitlines():
                # Strip interpreter-led invocations before looking for bare ones,
                # so `python3 .github/scripts/x.py` does not count.
                stripped = _VIA_INTERPRETER.sub("", line)
                for m in _DIRECT.finditer(stripped):
                    found.setdefault(m.group(1), set()).add(wf.name)
    return found


def index_modes() -> dict[str, str]:
    proc = subprocess.run(
        ["git", "ls-files", "-s", ".github/scripts"],
        capture_output=True,
        text=True,
        cwd=_ROOT,
    )
    modes: dict[str, str] = {}
    for line in proc.stdout.splitlines():
        parts = line.replace("\t", " ").split(None, 3)
        if len(parts) == 4:
            modes[parts[3].strip()] = parts[0]
    return modes


def check() -> int:
    invoked = directly_invoked()
    if not invoked:
        print("FAIL  found no directly-invoked scripts; the matcher is broken")
        return 1

    modes = index_modes()
    problems: list[str] = []
    for script, workflows in sorted(invoked.items()):
        mode = modes.get(script, "MISSING")
        where = ", ".join(sorted(workflows))
        if mode == "100755":
            print(f"  ok    {script}  <- {where}")
        else:
            problems.append(
                f"  {script} is {mode} but {where} runs it by path.\n"
                f"      Fix with: git update-index --chmod=+x {script}"
            )

    if problems:
        print(f"\n{len(problems)} script(s) would fail with exit 126:")
        print("\n".join(problems))
        return 1

    print(f"\nAll {len(invoked)} directly-invoked scripts are executable.")
    return 0


_SELF_TEST_WORKFLOW = """\
name: sample
on: push
jobs:
  a:
    runs-on: ubuntu-latest
    steps:
      # Direct — needs the bit.
      - run: .github/scripts/planted-direct.sh
      # Via an interpreter — does not.
      - run: python3 .github/scripts/planted-interpreted.py
      - name: workspace-prefixed, still direct
        run: $GITHUB_WORKSPACE/.github/scripts/planted-workspace.sh
    # A paths filter naming a script means nothing about invocation.
"""


def self_test() -> int:
    """The matcher must separate the three invocation forms."""
    print("==> self-test")
    import tempfile

    global _WORKFLOWS
    original = _WORKFLOWS
    try:
        with tempfile.TemporaryDirectory() as tmp:
            wf_dir = Path(tmp)
            (wf_dir / "sample.yml").write_text(_SELF_TEST_WORKFLOW, encoding="utf-8")
            _WORKFLOWS = wf_dir
            found = directly_invoked()
    finally:
        _WORKFLOWS = original

    names = set(found)
    expected = {
        ".github/scripts/planted-direct.sh",
        ".github/scripts/planted-workspace.sh",
    }
    unexpected = ".github/scripts/planted-interpreted.py"

    if unexpected in names:
        print(f"FAIL  flagged {unexpected}, which runs through an interpreter")
        return 1
    if names != expected:
        print(f"FAIL  matched {sorted(names)}, expected {sorted(expected)}")
        return 1

    print("  ok    direct and $GITHUB_WORKSPACE forms caught, interpreted form ignored")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--self-test", action="store_true")
    # Consumed above, at the import, before argparse exists. Declared here so
    # it appears in `--help` and is not rejected as an unknown flag once
    # pyyaml IS installed and the early exit never runs.
    parser.add_argument(
        "--require-yaml",
        action="store_true",
        help="fail instead of skipping when pyyaml is missing (use in CI)",
    )
    args = parser.parse_args()
    return self_test() if args.self_test else check()


if __name__ == "__main__":
    sys.exit(main())
