#!/usr/bin/env bash
# Stage 4b — the Chromium harness, one command.
#
#   net/crates/net/tests/rtc_browser/run.sh [/path/to/chrome]
#
# The runner does everything else: it builds the wasm leaf against
# `net-mesh-wire`, issues a CA + `localhost` leaf, installs the CA into
# the NSS database Chromium reads (`$HOME/.pki/nssdb`, removed on
# exit), starts the anchor + the impostor + the real bootstrap
# listeners, serves the page on http://localhost, launches Chromium,
# and prints one `RTCB PASS`/`RTCB FAIL` line per witness. Exits
# non-zero on any failed witness.
#
# Requires: `wasm-bindgen-cli` 0.2.128, the `wasm32-unknown-unknown`
# target, `certutil` (libnss3-tools) and a Chromium.
#
# There is no `--ignore-certificate-errors` here or in the runner.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if [ $# -gt 0 ]; then
  exec cargo run --release --manifest-path "$root/runner/Cargo.toml" -- --chrome "$1"
fi
exec cargo run --release --manifest-path "$root/runner/Cargo.toml"
