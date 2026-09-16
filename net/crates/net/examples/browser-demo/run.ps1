# The demo, from a clean checkout, in ONE command.
#
#   net\crates\net\examples\browser-demo\run.ps1            # two windows, 60 Hz, by hand
#   net\crates\net\examples\browser-demo\run.ps1 -Check      # headless, asserts, exits non-zero on failure
#
# Everything below is the documented build: the wasm leaf, the
# `@net-mesh/browser` bundle, the demo's two npm dependencies
# (three.js and playwright-core), and the host. Each step is skipped
# only by cargo/npm's own up-to-date checks, never by this script
# guessing.
[CmdletBinding()]
param(
  [switch]$Check,
  [int]$Seconds = 6,
  [int]$Hz = 60,
  [string]$BrowserPath
)

$ErrorActionPreference = 'Stop'
$demo = $PSScriptRoot
$net = (Resolve-Path (Join-Path $demo '..\..')).Path

function Step($name) { Write-Host "== $name" -ForegroundColor Cyan }

Step 'the wasm leaf'
Push-Location (Join-Path $net 'leaf')
try {
  cargo build --release --target wasm32-unknown-unknown
  if ($LASTEXITCODE -ne 0) { throw 'cargo build (leaf, wasm32) failed' }
  wasm-bindgen --target web --out-dir pkg target/wasm32-unknown-unknown/release/net_leaf.wasm
  if ($LASTEXITCODE -ne 0) { throw 'wasm-bindgen failed — install wasm-bindgen-cli 0.2.128' }
} finally { Pop-Location }

Step '@net-mesh/browser'
Push-Location (Join-Path $net 'browser-ts')
try {
  npm install --no-audit --no-fund
  if ($LASTEXITCODE -ne 0) { throw 'npm install (browser-ts) failed' }
  npm run build
  if ($LASTEXITCODE -ne 0) { throw 'npm run build (browser-ts) failed' }
} finally { Pop-Location }

Step "the demo's own dependencies (three.js, playwright-core)"
Push-Location $demo
try {
  npm install --no-audit --no-fund
  if ($LASTEXITCODE -ne 0) { throw 'npm install (browser-demo) failed' }
} finally { Pop-Location }

Step 'the host'
$hostArgs = @('--hz', $Hz)
if ($Check) { $hostArgs += '--check'; $hostArgs += @('--seconds', $Seconds) }
if ($BrowserPath) { $hostArgs += @('--browser-path', $BrowserPath) }

cargo run --release --manifest-path (Join-Path $demo 'host\Cargo.toml') -- @hostArgs
exit $LASTEXITCODE
