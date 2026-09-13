# Stage 4b — the Chromium harness, one command.
#
#   pwsh -File net/crates/net/tests/rtc_browser/run.ps1 [-Chrome <chrome.exe>]
#
# The runner does everything else: it builds the wasm leaf against
# `net-mesh-wire`, issues a CA + `localhost` leaf, installs the CA into
# the current user's Root store (and removes it on exit), starts the
# anchor + the impostor + the real bootstrap listeners, serves the
# page on http://localhost, launches Chromium, and prints one
# `RTCB PASS`/`RTCB FAIL` line per witness. Exits non-zero on any
# failed witness.
#
# There is no `--ignore-certificate-errors` here or in the runner.

param([string]$Chrome = "")

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $MyInvocation.MyCommand.Path

$harnessArgs = @("run", "--release", "--manifest-path", (Join-Path $root "runner/Cargo.toml"))
if ($Chrome) { $harnessArgs += @("--", "--chrome", $Chrome) }

& cargo @harnessArgs
exit $LASTEXITCODE
