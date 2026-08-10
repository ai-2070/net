#!/usr/bin/env bash
#
# Type-check a trivial consumer against the built @net-mesh/core +
# @net-mesh/sdk pair, with dependency declarations CHECKED.
#
# Every TypeScript gate in this repo compiles the SDK's own sources, and
# sdk-ts/tsconfig.json sets `skipLibCheck: true`. That combination cannot see
# a broken declaration file: a `.d.ts` that names a type nothing declares
# type-checks fine as long as nobody looks inside it. A user's project looks
# inside it by default only if they turn skipLibCheck off — but plenty do,
# and the errors then land in OUR shipped files, where they cannot fix them:
#
#   @net-mesh/core/index.d.ts(507,48):  error TS2304: Cannot find name 'DaemonBridgeTsfns'.
#   @net-mesh/core/index.d.ts(2060,47): error TS2304: Cannot find name 'DuplexHandlerArgs'.
#   @net-mesh/sdk/dist/meshdb.d.ts(67,10): error TS2305: Module '"@net-mesh/core"'
#       has no exported member 'InMemoryChainReader'.
#
# Two distinct causes, both invisible to the existing gates:
#
#   1. a napi type with hand-written impls emits its TypeName into
#      index.d.ts with no declaration behind it (TS2304);
#   2. the core was built with a feature set that omits a module the SDK
#      imports from, so the class is simply absent (TS2305).
#
# This runs the check a careful consumer runs. It expects the core `.node`
# and `index.d.ts` to already exist, and the SDK to already be built.
#
# Usage: check-ts-consumer.sh [<sdk-ts dir>]
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SDK_DIR="${1:-$ROOT/net/crates/net/sdk-ts}"
CORE_DIR="$ROOT/net/crates/net/bindings/node"

if [ ! -f "$CORE_DIR/index.d.ts" ]; then
  echo "FAIL  $CORE_DIR/index.d.ts is missing — build the native module first" >&2
  exit 1
fi
if [ ! -f "$SDK_DIR/dist/index.d.ts" ]; then
  echo "FAIL  $SDK_DIR/dist/index.d.ts is missing — build the SDK first" >&2
  exit 1
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# Start from the SDK's own node_modules so TypeScript and @types/node arrive
# with their transitive dependencies intact (@types/node needs undici-types,
# for one), then overlay fresh copies of the two packages under test.
cp -r "$SDK_DIR/node_modules" "$WORK/node_modules"
mkdir -p "$WORK/node_modules/@net-mesh"
rm -rf "$WORK/node_modules/@net-mesh/core" "$WORK/node_modules/@net-mesh/sdk"

# Copy rather than symlink: TypeScript resolves symlinked packages to their
# realpath, which would make the errors point back into the source tree and
# read as our problem rather than a consumer's.
cp -r "$CORE_DIR" "$WORK/node_modules/@net-mesh/core"
cp -r "$SDK_DIR" "$WORK/node_modules/@net-mesh/sdk"
rm -rf "$WORK/node_modules/@net-mesh/core/node_modules" \
       "$WORK/node_modules/@net-mesh/sdk/node_modules"

cat > "$WORK/tsconfig.json" <<'JSON'
{
  "compilerOptions": {
    "target": "ES2022",
    "module": "node18",
    "moduleResolution": "node16",
    "lib": ["ES2022"],
    "types": ["node"],
    "strict": true,
    "noEmit": true,
    "skipLibCheck": false,
    "esModuleInterop": true
  },
  "files": ["consumer.ts"]
}
JSON

# Mostly boring: the point is to make TypeScript read every declaration both
# packages ship. Nothing here runs — `noEmit` type-checks and stops — so it is
# free to name APIs that would need a live mesh.
#
# The one deliberately un-boring part is `registerDaemon`. A hand-written
# `ts_args_type` fixes TS2304 by spelling the object shape, which trades a
# loud error for a silent one: a declaration that disagrees with the Rust
# decoder type-checks fine until someone writes the object literal. It had
# disagreed in both directions — `name` declared optional though the decoder
# raises without it, and both capability fields missing entirely, so
# TypeScript's excess-property check (TS2353) rejected them even though the
# decoder reads them. `net-node`'s own tests pin the declaration against the
# decoder; this pins that the result is usable from outside.
cat > "$WORK/consumer.ts" <<'TS'
import { NetNode, MeshNode, MeshOsDaemonSdk } from '@net-mesh/sdk';
import type { MeshOsDaemon } from '@net-mesh/sdk';
import { Net } from '@net-mesh/core';

export async function main(): Promise<void> {
  const bus = await NetNode.create({ shards: 1 });
  await bus.shutdown();

  const node = await MeshNode.create({
    bindAddr: '127.0.0.1:0',
    psk: '42'.repeat(32),
  });

  // The tool surface takes the RPC handle, not the node.
  const rpc = node.rpc();
  rpc.raw.close();

  await node.shutdown();

  void Net;
  void declaredDaemon;
}

// An object literal, so excess-property checking applies: every field named
// here must appear in the declaration, or this fails to compile.
const declaredDaemon: MeshOsDaemon = {
  name: 'indexer',
  process: () => [],
  requiredCapabilities: ['gpu:h100'],
  optionalCapabilities: () => ['zone:a'],
};

// The same literal against the RAW napi parameter type, which is where the
// mismatch actually lived — `@net-mesh/sdk` casts on the way through, so a
// broken declaration is invisible from the SDK alone.
export type RawDaemonArg = Parameters<MeshOsDaemonSdk['registerDaemon']>[0];
TS

echo "==> type-checking a trivial consumer with skipLibCheck: false"
if (cd "$WORK" && ./node_modules/typescript/bin/tsc --project tsconfig.json); then
  echo "  ok    @net-mesh/core + @net-mesh/sdk declarations are self-consistent"
  exit 0
fi

cat >&2 <<'MSG'

The published declaration files do not type-check.

A consumer who leaves skipLibCheck at its default, or turns it off
deliberately, sees these errors in OUR files and cannot fix them. Two usual
causes:

  TS2304 "Cannot find name 'X'"
      A napi type with hand-written impls emitted its TypeName with no
      declaration behind it. Add `ts_args_type` / `ts_return_type` to the
      `#[napi]` attribute spelling the real TypeScript shape.

  TS2305 "Module '@net-mesh/core' has no exported member 'X'"
      The core was built without the feature that carries X, while the SDK
      imports it unconditionally. Align the feature list used to build the
      core with what the SDK's sources require.
MSG
exit 1
