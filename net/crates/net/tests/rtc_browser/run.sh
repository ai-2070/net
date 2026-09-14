#!/usr/bin/env bash
# The merged browser runner — Stage 4b + Stage 5, one command.
#
#   net/crates/net/tests/rtc_browser/run.sh [--engine chromium|firefox|webkit] \
#        [--browser-path <exe>] [--no-stage5]
#
# The runner does everything else: it builds the Stage 4b wasm leaf
# against `net-mesh-wire`, issues a CA + `localhost` leaf, installs the
# CA into the NSS database Chromium reads (`$HOME/.pki/nssdb`, removed
# on exit) and — for Firefox — into the profile's own `cert9.db`,
# starts the anchor + the impostor + the real bootstrap listeners,
# serves the page on http://localhost, launches the requested engine
# through Playwright (`driver/driver.mjs`, whose one dependency it
# installs on first use), and prints one `RTCB PASS`/`RTCB FAIL` line
# per witness. Exits non-zero on any failed witness.
#
# Requires: Node >= 20, `wasm-bindgen-cli` 0.2.128, the
# `wasm32-unknown-unknown` target, `certutil` (libnss3-tools) and the
# Playwright browser for the chosen engine. The Stage 5 half
# additionally needs `net/crates/net/leaf/pkg` and
# `net/crates/net/browser-ts/dist`; when they are absent the Stage 5
# witnesses FAIL with the build commands — never skipped.
#
# The UDP-blocked witness installs an `nft`/`iptables` rule pair for
# the anchor's `rtc_addr` and removes it again. It needs root or
# passwordless sudo; without it the witness FAILS with the refusal.
#
# `--engine webkit` is the recorded best-effort Safari leg.
#
# There is no `--ignore-certificate-errors` here or in the runner.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Bare legacy invocation: `run.sh /path/to/chrome`.
if [ $# -eq 1 ] && [ -x "$1" ]; then
  exec cargo run --release --manifest-path "$root/runner/Cargo.toml" -- --browser-path "$1"
fi

exec cargo run --release --manifest-path "$root/runner/Cargo.toml" -- "$@"
