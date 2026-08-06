#!/usr/bin/env bash
#
# Regression test for `lib/checker.sh`'s verdict classification.
#
# Why this exists: `run_checker` is the one place that decides whether a Python
# checker's silence means "the corpus is clean" or "the checker never ran". Get
# that wrong in the permissive direction and every checker downstream reports a
# green tick it has not earned — which is the exact failure that produced the
# library in the first place, on a cp1252 shell where two checkers raised
# UnicodeDecodeError on the first em-dash of every run and were reported as
# success.
#
# The rules are cheap to relax by accident. Someone softening the stderr rule
# for a noisy dependency, or reordering the branches, can reopen it without any
# test failing — nothing in the real corpus is broken, so nothing goes red. So
# the classification is pinned here against planted checkers instead of trusted.
#
# HOW IT AVOIDS DEPENDING ON THE REAL CORPUS
# Every case is a purpose-built Python file staged under the library's own
# scratch directory, with `CHECKER_DIR` pointed at it. No real checker runs, so
# this test's verdict cannot move when the corpus changes — and a checker that
# legitimately starts reporting findings does not make it red.
#
# Run locally:  .github/scripts/check-checker-lib.sh

set -uo pipefail

cd "$(dirname "$0")/../.."

# Source by literal path: `CHECKER_DIR` is set AFTER, because here it points at
# the fixtures rather than at the real checkers. The library only reads it
# inside `run_checker`, at call time.
# shellcheck source=.github/scripts/lib/checker.sh
. .github/scripts/lib/checker.sh

# Staged inside the library's own `$TMP` so its `EXIT` trap cleans them up.
# Installing a second trap here would silently replace that one.
#
# `pwd -W` is Git Bash's "give me that as a Windows path". It matters because
# `mktemp -d` hands back an MSYS path (`/tmp/tmp.XXXX`) that the native Python
# interpreter reads as `C:\tmp\tmp.XXXX` and cannot open — and the library
# exports `MSYS_NO_PATHCONV`, so the shell will not translate it on the way
# past either. The real `CHECKER_DIR` is a repo-relative path and never hits
# this; the fixtures are the one absolute path in the system. On any other
# platform `pwd -W` is not a thing and the plain `pwd` is already right.
mkdir -p "$TMP/fixtures"
CHECKER_DIR=$(cd "$TMP/fixtures" && { pwd -W 2>/dev/null || pwd; })

cat > "$CHECKER_DIR/clean.py" <<'PY'
raise SystemExit(0)
PY

cat > "$CHECKER_DIR/findings.py" <<'PY'
print("docs/a.md:1: says the thing that is not so")
print("docs/b.md:2: says another")
raise SystemExit(1)
PY

# Exit 1 with a traceback and no findings — the shape a UnicodeDecodeError takes
# when a checker dies on the first file it opens.
cat > "$CHECKER_DIR/dies.py" <<'PY'
raise ValueError("planted: died before auditing anything")
PY

# Dies with a status and no diagnostic at all. Nothing on either stream, so only
# the status distinguishes it from a clean run.
cat > "$CHECKER_DIR/silent_death.py" <<'PY'
raise SystemExit(2)
PY

# Warns at module level in `__main__`, where Python's default filters do NOT
# hide a DeprecationWarning, then audits its corpus and reports it clean.
cat > "$CHECKER_DIR/warns.py" <<'PY'
import warnings
warnings.warn("planted: the interpreter is grumbling", DeprecationWarning)
raise SystemExit(0)
PY

# Reports clean while telling stderr it skipped something. Status is in
# contract; the diagnostic is the only evidence the verdict is partial.
cat > "$CHECKER_DIR/quiet_skip.py" <<'PY'
import sys
print("planted: could not read 3 files, skipping them", file=sys.stderr)
raise SystemExit(0)
PY

# Real findings AND a diagnostic: the findings are worth reporting, and the
# diagnostic still means the sweep was incomplete.
cat > "$CHECKER_DIR/findings_and_noise.py" <<'PY'
import sys
print("docs/a.md:1: says the thing that is not so")
print("planted: and stopped early", file=sys.stderr)
raise SystemExit(1)
PY

# Reports back the argument it was handed, so the caller can see what actually
# arrived rather than what was written.
cat > "$CHECKER_DIR/echo_argv.py" <<'PY'
import sys
print("got=" + " ".join(sys.argv[1:]))
raise SystemExit(1)
PY

# ---------------------------------------------------------------- the harness
# `run_checker` is run in a subshell, so its `note` calls cannot move this
# script's own `fail` counter — the assertions read the notes it PRINTED, which
# is also what a human reading CI output would go on. Counting the red-tick
# escape rather than the glyph keeps this working on a console that cannot
# render U+2717.
notes_in() { printf '%s' "$1" | grep -c "$(printf '\033\[31m')"; }

expect() { # <label> <script> <expected-notes> <regex or "-"> [checker args...]
  local label=$1 script=$2 want=$3 pattern=$4; shift 4
  local out got
  out=$(run_checker "$script" "$@" 2>/dev/null)
  got=$(notes_in "$out")
  if [ "$got" -ne "$want" ]; then
    note "$label — expected $want note(s), got $got"
    printf '%s\n' "$out" | sed 's/^/      /'
    return
  fi
  if [ "$pattern" != "-" ] && ! printf '%s' "$out" | grep -qE "$pattern"; then
    note "$label — note does not name the cause (expected to match: $pattern)"
    printf '%s\n' "$out" | sed 's/^/      /'
    return
  fi
  ok "$label"
}

echo "==> A checker's verdict is classified by status first, stderr second"

expect "clean run reports nothing" \
  clean.py 0 -

expect "findings become one note each, verbatim" \
  findings.py 2 'says the thing that is not so'

# The load-bearing one. This is the defect the library exists to prevent, and it
# must be caught by the STATUS rule — so that softening the stderr rule for a
# noisy dependency cannot reopen it.
expect "a checker that dies with no findings is not silence" \
  dies.py 1 'exited 1 without writing a finding'

expect "a checker that dies without a diagnostic is not silence" \
  silent_death.py 1 'exited 2, which is neither clean nor findings'

expect "a clean run that skipped work is not clean" \
  quiet_skip.py 1 'wrote to stderr'

expect "findings and a diagnostic are both reported" \
  findings_and_noise.py 2 'wrote to stderr'

# ------------------------------------------------------- the warning softening
# Both halves are needed. The first proves the fixture actually warns — without
# it the second passes against a fixture that was simply quiet, and would keep
# passing if `PYTHON_WARN_FLAGS` were deleted.
echo "==> Benign interpreter warnings do not read as a dead checker"

if "$PYTHON" "$CHECKER_DIR/warns.py" 2>&1 >/dev/null | grep -q DeprecationWarning; then
  ok "the fixture does reach stderr when the filters are not applied"
else
  note "the fixture does not warn — the assertion below proves nothing"
fi

expect "a suppressed-category warning leaves the verdict standing" \
  warns.py 0 -

# --------------------------------------------------- the path-conversion scope
# `check-docs.sh` passes `--exclude /releases/`. Under Git Bash that reached
# Python as `C:/Program Files/Git/releases/`, matched nothing, and the dated
# release notes were audited as current claims — 78 phantom findings on a tree
# that is clean in CI. The suppression is applied by `py`, per invocation.
echo "==> Checker arguments are not rewritten on the way in"

expect "a unix-absolute argument arrives as written" \
  echo_argv.py 1 'got=--exclude /releases/$' --exclude /releases/

# The other half of "per invocation". Exporting the suppression would also work
# for the case above, and would silently disarm path translation for every other
# native command the sourcing script runs, and everything those spawn. This is
# the assertion that keeps the fix scoped; it holds on every platform, because
# what it checks is that the library did not export anything.
if [ -z "${MSYS_NO_PATHCONV:-}" ] && [ -z "${MSYS2_ARG_CONV_EXCL:-}" ]; then
  ok "the suppression is not exported into the sourcing script's environment"
else
  note "the suppression leaked into the environment — every native command in \
the sourcing script now runs without path translation, not just the checkers"
fi

echo
if [ "$fail" -eq 0 ]; then
  echo "Checker verdicts are classified correctly."
else
  echo "Verdict classification has regressed — a checker that did not run can"
  echo "now report success, or one that did can report failure."
fi
exit "$fail"
