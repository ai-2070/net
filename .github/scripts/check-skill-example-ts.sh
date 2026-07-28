#!/usr/bin/env bash
#
# Type-check the skill's TypeScript hello-world against the SDK source.
#
# Split out of ci.yml so `skills.yml` can run the *same* check as a publication
# gate. It used to live only in ci.yml, and `needs` does not cross workflows —
# so a red TypeScript job and a green skills workflow published the mirror
# anyway, advertising a first-class TypeScript binding whose one example did not
# compile. One definition, two callers, no copy to drift.
#
# PREREQUISITES, both assumed already done by the caller:
#   1. `net/crates/net/bindings/node` built with napi (produces `index.d.ts`,
#      which is gitignored — a clean checkout does not have it). The feature set
#      must include `redis`, or `sdk-ts/src/redis-dedup.ts` fails to resolve
#      `RedisStreamDedup` and the SDK's own source stops type-checking.
#   2. `npm i` in `net/crates/net/sdk-ts`.
#
# Run locally:  .github/scripts/check-skill-example-ts.sh

set -uo pipefail

cd "$(dirname "$0")/../.."
ROOT=$(pwd)
SDK_TS="$ROOT/net/crates/net/sdk-ts"
NAPI_DTS="$ROOT/net/crates/net/bindings/node/index.d.ts"

fail=0
note() { printf '  \033[31m✗\033[0m %s\n' "$1"; fail=$((fail + 1)); }
ok()   { printf '  \033[32m✓\033[0m %s\n' "$1"; }

echo "==> TypeScript — skill hello-world against the SDK source"

# Say which prerequisite is missing. Without this the failure is ~20 TS2307s
# from the SDK's own modules and not one word about the example.
if [ ! -f "$NAPI_DTS" ]; then
  note "bindings/node/index.d.ts is absent — run 'npx napi build' in bindings/node first"
  echo "      It is napi-generated and gitignored, so a clean checkout never has it."
  exit 1
fi
if [ ! -d "$SDK_TS/node_modules" ]; then
  note "sdk-ts/node_modules is absent — run 'npm i' in $SDK_TS first"
  exit 1
fi

cleanup() { rm -f "$SDK_TS/skill-hello.ts" "$SDK_TS/tsconfig.skill-example.json"; }
trap cleanup EXIT

cp "$ROOT/.claude/skills/net-event-bus/examples/hello.ts" "$SDK_TS/skill-hello.ts"

# `extends` the SDK's own tsconfig so `types: ["node"]` and the installed
# @types/node resolve naturally. Two overrides are load-bearing: `rootDir` must
# widen to `..` (the example and `bindings/node/*.ts` both sit outside `src/`),
# and `baseUrl` is deliberately absent — TypeScript 6 makes it a hard error, and
# the `paths` below are already relative to this file.
cat > "$SDK_TS/tsconfig.skill-example.json" <<'JSON'
{
  "extends": "./tsconfig.json",
  "compilerOptions": {
    "noEmit": true,
    "rootDir": "..",
    "paths": {
      "@net-mesh/sdk": ["./src/index.ts"],
      "@net-mesh/core": ["../bindings/node/index.d.ts"],
      "@net-mesh/core/*": ["../bindings/node/*"]
    }
  },
  "include": ["skill-hello.ts"]
}
JSON

if ( cd "$SDK_TS" && npx tsc --noEmit -p tsconfig.skill-example.json ); then
  ok "hello.ts"
else
  note "hello.ts does not type-check against the SDK source"
fi

echo
if [ "$fail" -eq 0 ]; then
  echo "The TypeScript example type-checks."
  echo "(Type-check only — nothing here proves it runs.)"
  exit 0
fi
echo "The TypeScript example drifted from the SDK — see above."
exit 1
