#!/usr/bin/env bash
#
# Compile floor for `.claude/skills/net-event-bus/examples/`.
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
# Run locally:  .github/scripts/check-skill-examples.sh
# Set REQUIRE_ALL=1 to turn a missing toolchain into a failure (CI does this, so
# a runner that silently lost a toolchain cannot look green).

set -uo pipefail

cd "$(dirname "$0")/../.."
ROOT=$(pwd)
EX="$ROOT/.claude/skills/net-event-bus/examples"
REQUIRE_ALL="${REQUIRE_ALL:-0}"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

fail=0
note() { printf '  \033[31m✗\033[0m %s\n' "$1"; fail=1; }
ok()   { printf '  \033[32m✓\033[0m %s\n' "$1"; }
skip() {
  if [ "$REQUIRE_ALL" = "1" ]; then
    note "$1 (REQUIRE_ALL=1)"
  else
    printf '  \033[33m–\033[0m skipped: %s\n' "$1"
  fi
}

run() { # <label> <logfile> <cmd...>
  local label=$1 log=$2; shift 2
  if "$@" >"$log" 2>&1; then
    ok "$label"
  else
    note "$label"
    sed 's/^/      /' "$log" | head -25
  fi
}

# ------------------------------------------------------------------------ C
echo "==> C — syntax check against the public headers"
if command -v gcc >/dev/null 2>&1 || command -v cc >/dev/null 2>&1; then
  CC=$(command -v gcc || command -v cc)
  run "hello.c" "$WORK/c.log" \
    "$CC" -fsyntax-only -I "$ROOT/net/crates/net/include" "$EX/hello.c"
else
  skip "no C compiler on PATH"
fi

# ----------------------------------------------------------------------- Go
# `go vet` and not `go build`: the Go binding is cgo and *links* against the
# Rust cdylibs (libnet, libnet_compute, ...), so a build needs a full release
# build of the crate first. `go vet` type-checks against the real binding API —
# it catches a wrong signature, which is what this floor is for — without
# linking.
echo "==> Go — type check (vet) against the local module"
if command -v go >/dev/null 2>&1; then
  mkdir -p "$WORK/go" && cp "$EX/hello.go" "$WORK/go/"
  cat > "$WORK/go/go.mod" <<EOF
module skillexample

go 1.26

require github.com/ai-2070/net/go v0.0.0

replace github.com/ai-2070/net/go => $ROOT/go
EOF
  ( cd "$WORK/go" && go vet . ) >"$WORK/go.log" 2>&1 \
    && ok "hello.go" \
    || { note "hello.go"; sed 's/^/      /' "$WORK/go.log" | head -25; }
else
  skip "no go on PATH"
fi

# --------------------------------------------------------------------- Rust
# The canonical file stays in the skill — a scratch crate pulls it in, so CI
# proves the exact file users are handed. Moving it under `sdk/examples/` was
# the alternative and would have either removed it from the published skill or
# duplicated it, and `--examples` would have widened the job to unrelated
# SDK examples.
echo "==> Rust — build the example against the workspace SDK"
if command -v cargo >/dev/null 2>&1; then
  mkdir -p "$WORK/rust/examples" "$WORK/rust/src"
  cp "$EX/hello.rs" "$WORK/rust/examples/"
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
  ( cd "$WORK/rust" && cargo build --example hello ) >"$WORK/rust.log" 2>&1 \
    && ok "hello.rs" \
    || { note "hello.rs"; sed 's/^/      /' "$WORK/rust.log" | tail -25; }
else
  skip "no cargo on PATH"
fi

# --------------------------------------------------------------- TypeScript
# Type-checked against `sdk-ts/src` directly rather than a built `dist/`: the
# napi `index.d.ts` is tracked, so the whole surface resolves with no build.
echo "==> TypeScript — type check against the SDK source"
if command -v npm >/dev/null 2>&1; then
  mkdir -p "$WORK/ts" && cp "$EX/hello.ts" "$WORK/ts/"
  echo '{ "name": "skill-examples-ts", "private": true, "type": "module" }' > "$WORK/ts/package.json"
  cat > "$WORK/ts/tsconfig.json" <<EOF
{
  "compilerOptions": {
    "target": "ES2022",
    "module": "ESNext",
    "moduleResolution": "bundler",
    "strict": true,
    "noEmit": true,
    "skipLibCheck": true,
    "baseUrl": ".",
    "paths": {
      "@net-mesh/sdk": ["$ROOT/net/crates/net/sdk-ts/src/index.ts"],
      "@net-mesh/core": ["$ROOT/net/crates/net/bindings/node/index.d.ts"],
      "@net-mesh/core/*": ["$ROOT/net/crates/net/bindings/node/*"]
    }
  },
  "include": ["hello.ts"]
}
EOF
  if ( cd "$WORK/ts" && npm i --silent --no-audit --no-fund typescript@5 @types/node ) \
       >"$WORK/ts-install.log" 2>&1; then
    ( cd "$WORK/ts" && ./node_modules/.bin/tsc --noEmit -p tsconfig.json ) >"$WORK/ts.log" 2>&1 \
      && ok "hello.ts" \
      || { note "hello.ts"; sed 's/^/      /' "$WORK/ts.log" | head -25; }
  else
    note "hello.ts — npm install failed"
    sed 's/^/      /' "$WORK/ts-install.log" | tail -10
  fi
else
  skip "no npm on PATH"
fi

# ------------------------------------------------------------------- Python
# mypy, not `py_compile`. py_compile proves the file parses and nothing else —
# not that net_sdk imports, that referenced members exist, or that signatures
# match. `--follow-imports=silent` resolves the SDK for types without reporting
# the SDK's own pre-existing errors, which are not this example's problem.
echo "==> Python — type check against the SDK source"
if command -v mypy >/dev/null 2>&1 || python3 -c "import mypy" >/dev/null 2>&1; then
  MYPY=$(command -v mypy || echo "python3 -m mypy")
  mkdir -p "$WORK/py" && cp "$EX/hello.py" "$WORK/py/"
  MYPYPATH="$ROOT/net/crates/net/sdk-py/src" $MYPY \
    --ignore-missing-imports --follow-imports=silent --no-error-summary \
    --cache-dir "$WORK/mypy-cache" "$WORK/py/hello.py" >"$WORK/py.log" 2>&1 \
    && ok "hello.py" \
    || { note "hello.py"; sed 's/^/      /' "$WORK/py.log" | head -25; }
else
  skip "no mypy available"
fi

echo
if [ "$fail" -eq 0 ]; then
  echo "Examples compile against the current tree."
  echo "(Compile floor only — nothing here proves they run. See the header.)"
else
  echo "Examples do not build — see above."
fi
exit "$fail"
