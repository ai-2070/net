# The merged browser runner — Stage 4b + Stage 5, one command.
#
#   pwsh -File net/crates/net/tests/rtc_browser/run.ps1 `
#        [-Engine chromium|firefox|webkit] [-BrowserPath <exe>] [-NoStage5] `
#        [-Stage7] `
#        [-UseRoutableInterface]
#
# The runner does everything else: it builds the Stage 4b wasm leaf
# against `net-mesh-wire`, issues a CA + `localhost` leaf, makes the
# engine trust that ONE leaf key without touching the platform
# certificate store (Chromium: an SPKI pin; Firefox: the profile's own
# cert9.db), starts the anchor + the impostor + the real bootstrap
# listeners, serves the page on http://localhost, launches the
# requested engine through Playwright (`driver/driver.mjs`, whose one
# dependency it installs on first use), and prints one `RTCB
# PASS`/`RTCB FAIL` line per witness. Exits non-zero on any failed
# witness.
#
# No OS security dialog is raised and no firewall prompt: every socket
# binds 127.0.0.1 on Windows. `-UseRoutableInterface` opts into the
# routable-interface topology and the Windows Firewall prompt that
# comes with it.
#
# Requires: Node >= 20, `wasm-bindgen-cli` 0.2.129 and the
# `wasm32-unknown-unknown` target for the Stage 4b leaf, and — for the
# Stage 5 half — a built `net/crates/net/leaf/pkg` and
# `net/crates/net/browser-ts/dist`. When those are absent the Stage 5
# witnesses FAIL with the build commands; they are never skipped.
#
# `-Engine webkit` is the recorded best-effort Safari leg.
#
# There is no `--ignore-certificate-errors` here or in the runner.

param(
  [string]$Engine = "chromium",
  [string]$BrowserPath = "",
  [switch]$NoStage5,
  # Opt in to the Stage 7 store witnesses. Off by default so a bare
  # run keeps the 47-witness surface; CI's Chromium leg passes this
  # and holds them at floor 55. See `runner/src/stage7.rs`.
  [switch]$Stage7,
  [switch]$UseRoutableInterface
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $MyInvocation.MyCommand.Path

$harnessArgs = @("run", "--release", "--manifest-path", (Join-Path $root "runner/Cargo.toml"), "--")
$harnessArgs += @("--engine", $Engine)
if ($BrowserPath) { $harnessArgs += @("--browser-path", $BrowserPath) }
if ($NoStage5) { $harnessArgs += @("--no-stage5") }
if ($Stage7) { $harnessArgs += @("--stage7") }
if ($UseRoutableInterface) { $harnessArgs += @("--use-routable-interface") }

& cargo @harnessArgs
exit $LASTEXITCODE
