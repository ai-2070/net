#!/usr/bin/env bash
#
# Run the Go binding's test suite so that a HANG reports what is holding.
#
# The binding calls into Rust through cgo, and Go's test-timeout panic stops at
# the cgo boundary: `TestLiveSubnetExportedCallFromAGeneratedScenario` has now
# reported ~9 minutes inside `_Cfunc_net_mesh_announce_capabilities` several
# times with nothing underneath it. The frames it does not print — the tokio
# workers and every other thread Rust created — are the only ones that can say
# what is stuck, and no goroutine dump can reach them.
#
# So: run under GOTRACEBACK=crash, which turns the timeout panic into a SIGABRT
# that leaves a core, and read the native frames back out of it with gdb.
#
# Every invocation of the Go suite goes through here. The first instrumented run
# passed while the UNinstrumented `-tags test_helpers` run hung the same way, in
# the same test, and reported the same unusable trace — the instrumentation has
# to cover every entry point or the flake just relocates to the one it missed.
#
# Usage: go-test-with-native-stacks.sh [build-tags]
# Run from the `go` module directory. Exits with the test binary's status.

set -euo pipefail

TAGS="${1:-}"
# Distinct binary AND distinct core name per tag set — `%e` in the core pattern
# is the executable name, so symbolizing one tag set's core against the other's
# binary is exactly the mix-up this avoids.
NAME="go-net${TAGS:+-${TAGS}}"
BIN="/tmp/${NAME}.test"

# `crash` is what promotes the timeout panic to an abort; without it the panic
# exits 1 and leaves no core. Set here rather than in the workflow so a new
# caller cannot forget it and silently lose the whole mechanism.
export GOTRACEBACK=crash

ulimit -c unlimited || true
sudo sysctl -w kernel.core_pattern=/tmp/core.%e.%p >/dev/null
# Old cores from an earlier step in the same job would otherwise be re-dumped
# here against the wrong binary, reporting a stale hang as this one's.
rm -f /tmp/core.* || true

# Compiled to a fixed path instead of run through `go test`, because postmortem
# symbolization needs the binary to outlive the run — `go test` builds into a
# temp dir and removes it, leaving gdb a core and nothing to map it against.
#
# `./...` resolved to this one package anyway (`example/` carries its own
# go.mod), so coverage is unchanged. `-test.timeout` must be spelled out: a test
# binary run directly has NO timeout by default, and inheriting `go test`'s
# implicit 10m silently is what would stop the alarm from ever firing.
if [ -n "$TAGS" ]; then
  go test -c -tags "$TAGS" -o "$BIN" .
else
  go test -c -o "$BIN" .
fi

set +e
"$BIN" -test.v -test.timeout 10m
rc=$?
set -e
[ $rc -eq 0 ] && exit 0

shopt -s nullglob
cores=(/tmp/core.*)
if [ ${#cores[@]} -eq 0 ]; then
  echo "No core file — this was an ordinary test failure, not a hang."
  exit $rc
fi

# Installed lazily: a green run never pays for it.
sudo apt-get update -qq || true
sudo apt-get install -y -qq gdb

for c in "${cores[@]}"; do
  # `info sharedlibrary` first, and it is not incidental. This binary links
  # EIGHT cdylibs that each embed and re-export a full copy of net-mesh's
  # `net::ffi` — verified by parsing the export tables: libnet_org defines all
  # 57 `net_mesh_*` entry points itself, alongside its own 24. Link order
  # resolves the FUNCTIONS to one copy, but every internal `static` stays
  # per-object, including `ffi::mesh::runtime()`'s OnceLock and tokio's own
  # TLS — which is what `block_on`'s runtime-in-runtime guard reads, so that
  # guard cannot see across the boundary. The map says how many runtimes could
  # be in play before the backtrace says which one is stuck.
  #
  # `info threads` next, because the threads that matter are the ones Rust
  # created, which is the entire reason any of this exists.
  echo "===== shared libraries + threads ($c) ====="
  gdb -batch -n \
    -ex 'info sharedlibrary' \
    -ex 'info threads' \
    "$BIN" "$c" 2>&1 | head -300 || true
  # Not truncated to a few hundred lines: Go's own threads print first and
  # there are dozens of them, so a short cut lands before the Rust ones every
  # time — throwing away the only frames worth having.
  echo "===== native backtrace ($c) ====="
  gdb -batch -n -ex 'thread apply all bt' "$BIN" "$c" 2>&1 | head -4000 || true
done

exit $rc
