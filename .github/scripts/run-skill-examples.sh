#!/usr/bin/env bash
#
# EXECUTE the skill examples and assert their output.
#
# WHY THIS EXISTS, CONCRETELY. `check-skill-examples.sh` is a compile floor, and
# a compile floor cannot catch an example that builds and then hangs. Both
# `hello.rs` and `hello.ts` did exactly that: they compiled and type-checked
# clean for months while blocking forever on a subscribe that could never yield,
# because `.memory()` selects the Noop adapter and the Noop adapter stores
# nothing. The examples README meanwhile promised "prints exactly one line".
#
# Nothing short of running them would have found it. So: run them, match stdout
# against the contract in `.github/skill-examples.json`, and bound every run
# with a timeout — a hang is the specific failure this exists to catch, and an
# unbounded run would reproduce it rather than report it.
#
#   run-skill-examples.sh --lang rust
#
# `--lang` is required: different CI jobs hold different build artifacts, and
# each runs the subset it can. The manifest records which bindings are executed
# where, and `--report` prints ▶ for executed against ✓ for compiled-only, so a
# partially-executed route can never read as a fully-executed one.

set -uo pipefail

cd "$(dirname "$0")/../.."
ROOT=$(pwd)

LANG_ARG=""
while [ $# -gt 0 ]; do
  case "$1" in
    --lang) LANG_ARG="${2:-}"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
if [ -z "$LANG_ARG" ]; then
  echo "usage: $0 --lang <rust|typescript|python|go|c>" >&2
  exit 2
fi

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

fail=0
note() { printf '  \033[31m✗\033[0m %s\n' "$1"; fail=$((fail + 1)); }
ok()   { printf '  \033[32m▶\033[0m %s\n' "$1"; }

SPEC=$(python3 "$ROOT/.github/scripts/skill_examples.py" --run-spec "$LANG_ARG")
if [ -z "$SPEC" ]; then
  echo "==> No $LANG_ARG examples are wired to run here."
  echo "    (See run.not_wired in .github/skill-examples.json.)"
  exit 0
fi

echo "==> Executing $LANG_ARG examples"

# Run one binary with a hard timeout and match its stdout. Returns non-zero and
# explains itself; never inherits the child's exit code silently.
assert_run() { # <label> <timeout_s> <expect-regex> <cmd...>
  local label=$1 timeout=$2 expect=$3; shift 3
  local out rc
  out=$(python3 - "$timeout" "$@" <<'PY'
import subprocess, sys
timeout = int(sys.argv[1])
try:
    r = subprocess.run(sys.argv[2:], capture_output=True, text=True, timeout=timeout)
    sys.stdout.write(r.stdout)
    if r.returncode != 0:
        sys.stderr.write(r.stderr)
        sys.exit(10 + min(r.returncode, 100))
except subprocess.TimeoutExpired as e:
    sys.stdout.write((e.stdout or b"").decode(errors="replace")
                     if isinstance(e.stdout, bytes) else (e.stdout or ""))
    sys.exit(9)
PY
)
  rc=$?
  if [ "$rc" -eq 9 ]; then
    note "$label — HUNG (no exit within ${timeout}s)"
    printf '%s\n' "$out" | sed 's/^/      /' | head -10
    return
  fi
  if [ "$rc" -ne 0 ]; then
    note "$label — exited non-zero"
    printf '%s\n' "$out" | sed 's/^/      /' | head -15
    return
  fi
  if ! printf '%s' "$out" | grep -qE "$expect"; then
    note "$label — ran, but stdout did not match the contract"
    echo "      expected to match: $expect"
    printf '%s\n' "$out" | sed 's/^/      got: /' | head -10
    return
  fi
  ok "$label"
}

case "$LANG_ARG" in
  rust)
    # One scratch crate holding every example, so the SDK compiles once.
    mkdir -p "$WORK/rust/examples" "$WORK/rust/src"
    echo "fn main() {}" > "$WORK/rust/src/main.rs"
    cat > "$WORK/rust/Cargo.toml" <<EOF
[workspace]

[package]
name = "skill-examples-run"
version = "0.0.0"
edition = "2021"

[dependencies]
# Publishes as net-mesh-sdk, imports as net_sdk.
net-sdk = { package = "net-mesh-sdk", path = "$ROOT/net/crates/net/sdk" }
serde = { version = "1", features = ["derive"] }
tokio = { version = "1", features = ["rt", "macros", "time"] }
futures = "0.3"
EOF
    while IFS=$'\t' read -r path id timeout expect; do
      [ -z "$path" ] && continue
      cp "$ROOT/$path" "$WORK/rust/examples/"
    done <<< "$SPEC"

    if ! ( cd "$WORK/rust" && cargo build --examples ) >"$WORK/build.log" 2>&1; then
      note "the examples did not build — nothing could be executed"
      sed 's/^/      /' "$WORK/build.log" | tail -20
      exit 1
    fi

    while IFS=$'\t' read -r path id timeout expect; do
      [ -z "$path" ] && continue
      stem=$(basename "$path" .rs)
      assert_run "$id: $(basename "$path")" "$timeout" "$expect" \
        "$WORK/rust/target/debug/examples/$stem"
    done <<< "$SPEC"
    ;;

  typescript)
    # Run from inside sdk-ts, where the napi module and node_modules already
    # exist. The examples import `@net-mesh/sdk`, which the package cannot
    # resolve to itself, so the specifier is rewritten to the local source —
    # the same rewrite the type-check does via tsconfig `paths`.
    SDK_TS="$ROOT/net/crates/net/sdk-ts"
    if [ ! -d "$SDK_TS/node_modules" ]; then
      note "sdk-ts/node_modules is absent — run 'npm i' there first"
      exit 1
    fi
    trap 'rm -rf "$WORK"; rm -f "$SDK_TS"/skill-run-*.ts' EXIT
    while IFS=$'\t' read -r path id timeout expect; do
      [ -z "$path" ] && continue
      sed "s|from '@net-mesh/sdk'|from './src/index'|" "$ROOT/$path" \
        > "$SDK_TS/skill-run-$id.ts"
      assert_run "$id: $(basename "$path")" "$timeout" "$expect" \
        npx --prefix "$SDK_TS" tsx "$SDK_TS/skill-run-$id.ts"
    done <<< "$SPEC"
    ;;

  *)
    echo "  no runner implemented for '$LANG_ARG' in this script yet." >&2
    echo "  Add one here, and drop that binding from run.not_wired." >&2
    exit 2
    ;;
esac

echo
if [ "$fail" -eq 0 ]; then
  echo "The $LANG_ARG examples run and match their contracts."
  exit 0
fi
echo "$fail $LANG_ARG example(s) did not behave as documented."
exit 1
