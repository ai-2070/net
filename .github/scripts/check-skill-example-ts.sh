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

cleanup() { rm -f "$SDK_TS"/skill-example-*.ts "$SDK_TS/tsconfig.skill-example.json"; }
trap cleanup EXIT

# Driven from the same manifest as check-skill-examples.sh, so the two cannot
# disagree about which examples exist.
TS_FILES=$(python3 "$ROOT/.github/scripts/skill_examples.py" --list \
           | awk -F'\t' '$1 == "typescript" { print $2 "\t" $3 }')

if [ -z "$TS_FILES" ]; then
  ok "no TypeScript examples in the manifest"
  echo
  echo "Nothing to check."
  exit 0
fi

INCLUDE=""
while IFS=$'\t' read -r path id; do
  [ -z "$path" ] && continue
  cp "$ROOT/$path" "$SDK_TS/skill-example-$id.ts"
  INCLUDE="$INCLUDE\"skill-example-$id.ts\", "
done <<< "$TS_FILES"
INCLUDE=${INCLUDE%, }

# `extends` the SDK's own tsconfig so `types: ["node"]` and the installed
# @types/node resolve naturally. Two overrides are load-bearing: `rootDir` must
# widen to `..` (the example and `bindings/node/*.ts` both sit outside `src/`),
# and `baseUrl` is deliberately absent — TypeScript 6 makes it a hard error, and
# the `paths` below are already relative to this file.
cat > "$SDK_TS/tsconfig.skill-example.json" <<JSON
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
  "include": [$INCLUDE]
}
JSON

# One tsc invocation over every example: they share the compiler options, and a
# per-file loop would pay npx startup once per example for no extra signal.
if ( cd "$SDK_TS" && npx tsc --noEmit -p tsconfig.skill-example.json ); then
  while IFS=$'\t' read -r path id; do
    [ -n "$path" ] && ok "$id: $(basename "$path")"
  done <<< "$TS_FILES"
else
  note "one or more TypeScript examples do not type-check against the SDK source"
  echo "      (tsc names the file above; the copies are skill-example-<id>.ts)"
fi

echo
if [ "$fail" -eq 0 ]; then
  echo "The TypeScript example type-checks."
  echo "(Type-check only — nothing here proves it runs.)"
  exit 0
fi
echo "The TypeScript example drifted from the SDK — see above."
exit 1
