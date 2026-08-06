#!/usr/bin/env bash
#
# Compile floor for the checked skill examples listed in
# `docs/data/examples.yaml`.
#
# The examples README calls these "the first thing a developer runs after
# install." Nothing built them until this script. A broken hello-world is the
# worst possible first contact with a library, and it is the only content in
# either skill with a mechanical success criterion.
#
# THIS IS A COMPILE FLOOR, NOT AN EXECUTION PROOF. It answers "does this still
# build against the current tree?" It does NOT answer "does it run and print one
# line?" — that needs built artifacts (napi module, wheel, cdylib) and belongs in
# the release workflows. Do not let a green run here be reported as "the examples
# work."
#
# Building it turned up the defect it was written to find: the skills told users
# to `pip install net-sdk` and depend on `net-sdk = "..."`, but both packages
# publish as `net-mesh-sdk` (only the *import* name is `net_sdk`). Neither
# install command would have worked.
#
# WHY A MANIFEST. This script used to hardcode five `hello.*` paths, so adding a
# route meant editing shell. Worse, a route covering three languages would have
# looked exactly like one covering five. The manifest requires every binding to
# be listed as either a file or an explicit absence, and the coverage report at
# the end prints both — so a gap is stated rather than inferred.
#
# TypeScript is checked by `check-skill-example-ts.sh`, from the same manifest:
# it needs the napi-generated `index.d.ts`, which is gitignored and costs a full
# `napi build` to produce.
#
# Run locally:  .github/scripts/check-skill-examples.sh
# Set REQUIRE_ALL=1 to turn a missing toolchain into a failure (CI does this, so
# a runner that silently lost a toolchain cannot look green).

set -uo pipefail

cd "$(dirname "$0")/../.."
ROOT=$(pwd)
MANIFEST="$ROOT/docs/data/examples.yaml"
REQUIRE_ALL="${REQUIRE_ALL:-0}"

# Relative to the repo root we just entered — see `check-skills.sh` for why an
# absolute path breaks the Python checkers under Git Bash. `$ROOT` stays for the
# example paths below, which are handed to compilers directly.
CHECKER_DIR=".github/scripts"
# `note`, `ok`, `fail`, `$TMP`, a resolved `$PYTHON`, and `py`.
. "$CHECKER_DIR/lib/checker.sh"

# Under the library's scratch directory, so its EXIT trap cleans this up too.
WORK="$TMP/work"
mkdir -p "$WORK"

skip() {
  if [ "$REQUIRE_ALL" = "1" ]; then
    note "$1 (REQUIRE_ALL=1)"
  else
    printf '  \033[33m–\033[0m skipped: %s\n' "$1"
  fi
}

# ------------------------------------------------------------ manifest loading
# Validated before anything is compiled: a manifest that silently drops a
# binding would make this whole script report coverage it does not have.
echo "==> Manifest"
if ! py "$CHECKER_DIR/skill_examples.py" --validate; then
  echo "  manifest is invalid — refusing to report coverage from it"
  exit 1
fi
ok "every binding is listed as a file or an explicit absence"

# `<lang>\t<path>\t<id>` for each present file.
# The status is checked, not just the output: an empty `ENTRIES` makes every
# `files_for` below return nothing, and every language section then prints "no
# <lang> examples in the manifest" and exits green having compiled none of
# them. `--validate` above catches an unreadable manifest, but not a lister
# that dies between the two calls.
entries_err="$TMP/entries.err"
ENTRIES=$(py "$CHECKER_DIR/skill_examples.py" --list 2>"$entries_err")
entries_status=$?
if [ "$entries_status" -ne 0 ] || [ -s "$entries_err" ]; then
  note "skill_examples.py --list did not run (exit $entries_status) — no example was compiled"
  printf '%s
' "$ENTRIES" | sed 's/^/      /'
  sed 's/^/      /' "$entries_err" >&2
  exit 1
fi

files_for() { # <lang>  -> newline-separated "<path>\t<id>"
  printf '%s\n' "$ENTRIES" | awk -F'\t' -v l="$1" '$1 == l { print $2 "\t" $3 }'
}

# ------------------------------------------------------------------------ C
echo "==> C — syntax check against the public headers"
C_FILES=$(files_for c)
if [ -z "$C_FILES" ]; then
  ok "no C examples in the manifest"
elif command -v gcc >/dev/null 2>&1 || command -v cc >/dev/null 2>&1; then
  CC=$(command -v gcc || command -v cc)
  while IFS=$'\t' read -r path id; do
    [ -z "$path" ] && continue
    if "$CC" -fsyntax-only -I "$ROOT/net/crates/net/include" "$ROOT/$path" \
         >"$WORK/c.log" 2>&1; then
      ok "$id: $(basename "$path")"
    else
      note "$id: $(basename "$path")"
      sed 's/^/      /' "$WORK/c.log" | head -25
    fi
  done <<< "$C_FILES"
else
  skip "no C compiler on PATH"
fi

# ----------------------------------------------------------------------- Go
# `go vet` and not `go build`: the Go binding is cgo and *links* against the
# Rust cdylibs (libnet, libnet_compute, ...), so a build needs a full release
# build of the crate first. `go vet` type-checks against the real binding API —
# it catches a wrong signature, which is what this floor is for — without
# linking.
#
# One scratch module per example: two files each declaring `func main()` in the
# same package do not compile together.
echo "==> Go — type check (vet) against the local module"
GO_FILES=$(files_for go)
if [ -z "$GO_FILES" ]; then
  ok "no Go examples in the manifest"
elif command -v go >/dev/null 2>&1; then
  while IFS=$'\t' read -r path id; do
    [ -z "$path" ] && continue
    d="$WORK/go-$id"
    mkdir -p "$d" && cp "$ROOT/$path" "$d/"
    cat > "$d/go.mod" <<EOF
module skillexample

go 1.26

require github.com/ai-2070/net/go v0.0.0

replace github.com/ai-2070/net/go => $ROOT/go
EOF
    if ( cd "$d" && go vet . ) >"$WORK/go-$id.log" 2>&1; then
      ok "$id: $(basename "$path")"
    else
      note "$id: $(basename "$path")"
      sed 's/^/      /' "$WORK/go-$id.log" | head -25
    fi
  done <<< "$GO_FILES"
else
  skip "no go on PATH"
fi

# --------------------------------------------------------------------- Rust
# The canonical file stays in the skill — a scratch crate pulls it in, so CI
# proves the exact file users are handed. Moving it under `sdk/examples/` was
# the alternative and would have either removed it from the published skill or
# duplicated it, and `--examples` would have widened the job to unrelated
# SDK examples.
echo "==> Rust — build the examples against the workspace SDK"
RS_FILES=$(files_for rust)
if [ -z "$RS_FILES" ]; then
  ok "no Rust examples in the manifest"
elif command -v cargo >/dev/null 2>&1; then
  mkdir -p "$WORK/rust/examples" "$WORK/rust/src"
  echo "fn main() {}" > "$WORK/rust/src/main.rs"
  cat > "$WORK/rust/Cargo.toml" <<EOF
[workspace]

[package]
name = "skill-examples"
version = "0.0.0"
edition = "2021"

[dependencies]
# Publishes as net-mesh-sdk, imports as net_sdk.
net-sdk = { package = "net-mesh-sdk", path = "$ROOT/net/crates/net/sdk" }
serde = { version = "1", features = ["derive"] }
tokio = { version = "1", features = ["rt", "macros", "time"] }
futures = "0.3"
EOF
  while IFS=$'\t' read -r path id; do
    [ -z "$path" ] && continue
    stem=$(basename "$path" .rs)
    cp "$ROOT/$path" "$WORK/rust/examples/"
    if ( cd "$WORK/rust" && cargo build --example "$stem" ) \
         >"$WORK/rust-$id.log" 2>&1; then
      ok "$id: $(basename "$path")"
    else
      note "$id: $(basename "$path")"
      sed 's/^/      /' "$WORK/rust-$id.log" | tail -25
    fi
  done <<< "$RS_FILES"
else
  skip "no cargo on PATH"
fi

# ------------------------------------------------------------------- Python
# mypy, not `py_compile`. py_compile proves the file parses and nothing else —
# not that net_sdk imports, that referenced members exist, or that signatures
# match. `--follow-imports=silent` resolves the SDK for types without reporting
# the SDK's own pre-existing errors, which are not this example's problem.
echo "==> Python — type check against the SDK source"
PY_FILES=$(files_for python)
if [ -z "$PY_FILES" ]; then
  ok "no Python examples in the manifest"
elif command -v mypy >/dev/null 2>&1 || "$PYTHON" -c "import mypy" >/dev/null 2>&1; then
  MYPY=$(command -v mypy || echo "$PYTHON -m mypy")
  while IFS=$'\t' read -r path id; do
    [ -z "$path" ] && continue
    if MYPYPATH="$ROOT/net/crates/net/sdk-py/src" $MYPY \
         --ignore-missing-imports --follow-imports=silent --no-error-summary \
         --cache-dir "$WORK/mypy-cache" "$ROOT/$path" >"$WORK/py-$id.log" 2>&1; then
      ok "$id: $(basename "$path")"
    else
      note "$id: $(basename "$path")"
      sed 's/^/      /' "$WORK/py-$id.log" | head -25
    fi
  done <<< "$PY_FILES"
else
  skip "no mypy available"
fi

# --------------------------------------------------------------- TypeScript
# HARD PREREQUISITE: `bindings/node/index.d.ts`, which is napi-GENERATED and
# gitignored. Every module under `sdk-ts/src` imports types from
# `@net-mesh/core`, which resolves to that file — without it tsc emits ~20
# TS2307s from the SDK's own source and says nothing about the example.
#
# Producing it needs a full `napi build`, far too expensive to duplicate here.
# So the check lives in `check-skill-example-ts.sh`, run by two jobs that
# already build napi: ci.yml's "TypeScript SDK tests" and skills.yml's
# `typescript` gate. Not a REQUIRE_ALL failure — the prerequisite legitimately
# does not exist in this job.
echo "==> TypeScript"
printf '  \033[33m–\033[0m delegated: check-skill-example-ts.sh, run where napi is built\n'
printf '      (ci.yml "TypeScript SDK tests" and skills.yml "typescript")\n'

# -------------------------------------------------------------------- report
# Printed unconditionally, including on success: a green run that quietly
# covered three languages out of five is the failure mode this replaces.
echo
py "$CHECKER_DIR/skill_examples.py" --report

echo
if [ "$fail" -eq 0 ]; then
  echo "Examples compile against the current tree."
  echo "(Compile floor only — nothing here proves they run. See the header.)"
  exit 0
fi
echo "Examples do not build — $fail problem(s) above."
exit 1
