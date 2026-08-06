# Shared plumbing for the drift checkers (`check-skills.sh`, `check-docs.sh`).
#
# Sourced, not executed. Provides `note`, `ok`, the `fail` counter, a resolved
# `$PYTHON`, a scratch `$TMP`, and `run_checker`.
#
# It exists because the two scripts had already drifted apart on exactly the
# thing that matters here: one grew a guarded way to run a Python checker after
# a checker was caught reporting success without running, and the other kept
# calling `python3 ... || true` and piping the result into a `while read`. Same
# defect, same fix, fixed once.

# A counter, not a flag. Each section reports success with
# `[ "$fail" -eq "$before" ]`, which is only meaningful if `note` keeps
# incrementing — as a 0/1 flag the first failure made every *later* section
# print its green tick, because `before` and `fail` were both 1.
fail=0
note() { printf '  \033[31m✗\033[0m %s\n' "$1"; fail=$((fail + 1)); }
ok()   { printf '  \033[32m✓\033[0m %s\n' "$1"; }

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

# Git Bash rewrites any argument that looks like a Unix absolute path into a
# Windows one before handing it to a native executable, so `check-docs.sh`'s
# `--exclude /releases/` reached Python as `C:/Program Files/Git/releases/`,
# matched nothing, and the dated release notes were scanned as current claims —
# 78 findings, every one of them phantom, on a run that is clean in CI. None of
# these checkers takes a Windows path, so the conversion has nothing to do here.
# `MSYS2_ARG_CONV_EXCL` covers the msys2 runtime, `MSYS_NO_PATHCONV` the older
# MSYS one; both are inert everywhere else.
export MSYS_NO_PATHCONV=1
export MSYS2_ARG_CONV_EXCL='*'

# More than half of each checker is Python. Resolve the interpreter ONCE, and
# refuse to run without one — a missing `python3` is not a reason to skip those
# checks, it is a reason to stop. Hard-coding `python3` meant that on a machine
# where Python is installed as `python` (the usual Windows shape, and the shape
# of the Microsoft Store shim that occupies the name `python3` and exits 49
# without running anything), every Python-backed section printed its green tick
# having executed no Python at all: the diagnostic went to stderr, the section's
# `before`/`fail` comparison saw nothing, and the run exited 0.
#
# `python` only counts if it is a Python 3 — the name still means Python 2 on
# older systems, and those cannot parse these checkers. The probe runs the
# candidate rather than trusting `command -v`, which is what distinguishes a
# real interpreter from a shim that merely occupies the name.
PYTHON=""
for _candidate in python3 python; do
  if command -v "$_candidate" >/dev/null 2>&1 &&
     "$_candidate" -c 'import sys; sys.exit(0 if sys.version_info >= (3, 8) else 1)' \
       >/dev/null 2>&1; then
    PYTHON="$_candidate"
    break
  fi
done
unset _candidate
if [ -z "$PYTHON" ]; then
  echo "  ✗ no Python 3.8+ on PATH as python3 or python — most of this checker" >&2
  echo "    is Python, and skipping it would report success for checks that" >&2
  echo "    never ran." >&2
  exit 1
fi

# Run one of the sibling Python checkers, with any extra args passed through.
# Findings arrive on stdout, one per line, and each becomes a `note`.
#
# Three outcomes, and telling them apart is the whole point:
#
#   exit 0, silent      the corpus is clean
#   exit 1 with output  the corpus drifted; every line becomes a `note`
#   anything else       the CHECKER failed, which is also a `note`
#
# A checker exits non-zero precisely when it HAS findings, so under `pipefail`
# these calls need their status suppressed — and suppressing it is exactly what
# hid a checker that died before reporting anything: zero findings written,
# `fail` never moved, and the caller's `[ "$fail" -eq "$before" ]` line printed
# a green tick for a check that did not run. Not hypothetical: on a cp1252 shell
# `check-skill-vocab.py` and `check-skill-refs.py` raised UnicodeDecodeError on
# the first source file containing an em-dash, every run, reported as success.
#
# Deciding from stderr alone is not enough either. A checker can die with a
# status and no diagnostic — `sys.exit(2)`, a segfault in a C extension, an OOM
# kill — so the status is checked too, and any status that is not the documented
# 0-or-1 is a failure of the checker rather than a verdict from it.
run_checker() {
  local script="$1"; shift
  local err out status
  err="$TMP/$(basename "$script").err"
  out=$("$PYTHON" "$CHECKER_DIR/$script" "$@" 2>"$err")
  status=$?
  if [ -n "$out" ]; then
    while IFS= read -r line; do
      [ -n "$line" ] && note "$line"
    done <<<"$out"
  fi
  if [ "$status" -gt 1 ]; then
    note "$script exited $status, which is neither clean nor findings — treat its verdict as unrun"
  elif [ "$status" -ne 0 ] && [ -z "$out" ] && [ ! -s "$err" ]; then
    note "$script exited $status but reported nothing — treat its verdict as unrun"
  fi
  if [ -s "$err" ]; then
    note "$script wrote to stderr — treat its verdict as unrun: $(tail -1 "$err")"
    sed 's/^/      /' "$err" >&2
  fi
}
