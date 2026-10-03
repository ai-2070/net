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
#
# Optional environment:
#   GO_TEST_RACE=1   build the test binary with -race. The ordinary runs do
#                    not, so they are not race evidence; a step that claims
#                    race coverage sets this.
#   GO_TEST_RUN=re   pass -test.run=re. A filter that matches no test makes
#                    a Go test binary exit 0 ("no tests to run"), which is the
#                    silent-skip hazard AGENTS.md warns about, so this script
#                    fails the run instead.

set -euo pipefail

TAGS="${1:-}"
RACE="${GO_TEST_RACE:-}"
RUN="${GO_TEST_RUN:-}"
# Distinct binary AND distinct core name per tag set (and per race mode) — `%e`
# in the core pattern is the executable name, so symbolizing one build's core
# against another's binary is exactly the mix-up this avoids.
NAME="go-net${TAGS:+-${TAGS}}${RACE:+-race}"
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
build_flags=()
[ -n "$TAGS" ] && build_flags+=(-tags "$TAGS")
[ -n "$RACE" ] && build_flags+=(-race)
go test -c "${build_flags[@]}" -o "$BIN" .

run_flags=(-test.v -test.timeout 10m)
[ -n "$RUN" ] && run_flags+=("-test.run=$RUN")

LOG="/tmp/${NAME}.log"
set +e
"$BIN" "${run_flags[@]}" 2>&1 | tee "$LOG"
rc=${PIPESTATUS[0]}
set -e
if [ $rc -eq 0 ] && [ -n "$RUN" ] && ! grep -q '^=== RUN' "$LOG"; then
  echo "::error::-test.run='$RUN' matched no tests; refusing a vacuous green"
  exit 1
fi
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
  # `info sharedlibrary` first, and it is not incidental: it is the check that
  # the one-cdylib layout still holds. The binding used to link EIGHT cdylibs
  # that each embedded and re-exported a full copy of net-mesh's `net::ffi`,
  # and the duplicate per-object `static`s — specifically `parking_lot_core`'s
  # parked-thread registry — are what produced the announce_mu hang this
  # harness was built to catch. `libnet_go.so` should now be the only net
  # library in the map; anything else beside it means the collapse regressed.
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
