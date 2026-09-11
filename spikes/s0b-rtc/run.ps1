# S0b — build both sides, start the native anchor, run headless
# Chromium against it, wait for the verdict lines, exit 0/1.
#
#   pwsh -File spikes/s0b-rtc/run.ps1 [-Chrome <path to chrome.exe>]
#
# Chromium: defaults to Playwright's bundled build if present, else the
# system Chrome. The exact binary and version are printed and end up in
# the report.

param(
  [string]$Chrome = "",
  [int]$HttpPort = 8088,
  [int]$TimeoutSec = 240,
  # S0c: run the double-AEAD measurement instead of the S0b scenario
  # sequence. Takes ~13 minutes (7 cells x 30 s x 3 runs).
  [switch]$Bench
)

if ($Bench -and $TimeoutSec -lt 1800) { $TimeoutSec = 1800 }

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
Push-Location $root
try {
  # --- resolve Chromium ------------------------------------------------
  if (-not $Chrome) {
    $candidates = @(
      (Get-ChildItem -Path "$env:LOCALAPPDATA\ms-playwright" -Filter "chrome.exe" -Recurse -ErrorAction SilentlyContinue |
        Sort-Object FullName -Descending | Select-Object -First 1 -ExpandProperty FullName),
      "$env:ProgramFiles\Google\Chrome\Application\chrome.exe",
      "${env:ProgramFiles(x86)}\Google\Chrome\Application\chrome.exe"
    ) | Where-Object { $_ -and (Test-Path $_) }
    if (-not $candidates) { throw "No Chromium found; pass -Chrome <path>" }
    $Chrome = $candidates[0]
  }
  $ver = (Get-Item $Chrome).VersionInfo.ProductVersion
  Write-Host "[run] chromium: $Chrome ($ver)"

  # --- build -----------------------------------------------------------
  Write-Host "[run] building wasm leaf"
  Push-Location web
  cargo build --release --target wasm32-unknown-unknown
  if ($LASTEXITCODE -ne 0) { throw "wasm build failed" }
  wasm-bindgen --target web --out-dir dist --out-name s0b `
    target/wasm32-unknown-unknown/release/s0b_web.wasm
  if ($LASTEXITCODE -ne 0) { throw "wasm-bindgen failed" }
  Copy-Item page/index.html dist/ -Force
  Copy-Item page/app.js dist/ -Force
  Copy-Item page/bench.js dist/ -Force
  Pop-Location

  Write-Host "[run] building native anchor"
  Push-Location native
  cargo build --release
  if ($LASTEXITCODE -ne 0) { throw "native build failed" }
  Pop-Location

  # --- run -------------------------------------------------------------
  $logDir = Join-Path $root "logs"
  New-Item -ItemType Directory -Force -Path $logDir | Out-Null
  $nativeLog = Join-Path $logDir "native.log"
  $chromeLog = Join-Path $logDir "chrome.log"
  $profileDir = Join-Path $logDir "chrome-profile"
  Remove-Item -Recurse -Force $profileDir -ErrorAction SilentlyContinue

  $nativeExe = Join-Path $root "native/target/release/s0b-native.exe"
  $native = Start-Process -FilePath $nativeExe `
    -ArgumentList @("--web-root", (Join-Path $root "web/dist"), "--http-port", "$HttpPort") `
    -RedirectStandardOutput $nativeLog -RedirectStandardError "$nativeLog.err" `
    -NoNewWindow -PassThru

  # wait for READY
  $deadline = (Get-Date).AddSeconds(30)
  while ((Get-Date) -lt $deadline) {
    if ((Test-Path $nativeLog) -and (Select-String -Path $nativeLog -Pattern "READY" -Quiet)) { break }
    Start-Sleep -Milliseconds 200
  }

  # `--disable-features=WebRtcHideLocalIpsWithMdns` is required: without
  # it Chrome publishes host candidates as `.local` mDNS names that
  # str0m cannot resolve.
  $chromeArgs = @(
    "--headless=new",
    "--no-sandbox",
    "--disable-gpu",
    "--user-data-dir=$profileDir",
    "--enable-logging=stderr",
    "--v=0",
    "--autoplay-policy=no-user-gesture-required",
    "--disable-features=WebRtcHideLocalIpsWithMdns",
    "--allow-running-insecure-content",
    "--unsafely-treat-insecure-origin-as-secure=http://127.0.0.1:$HttpPort",
    $(if ($Bench) { "http://127.0.0.1:$HttpPort/?bench=1" } else { "http://127.0.0.1:$HttpPort/" })
  )
  $chromeProc = Start-Process -FilePath $Chrome -ArgumentList $chromeArgs `
    -RedirectStandardError $chromeLog -RedirectStandardOutput "$chromeLog.out" `
    -NoNewWindow -PassThru

  $deadline = (Get-Date).AddSeconds($TimeoutSec)
  $done = $false
  while ((Get-Date) -lt $deadline) {
    if (Select-String -Path $nativeLog -Pattern "DONE with" -Quiet -ErrorAction SilentlyContinue) {
      $done = $true; break
    }
    if ($native.HasExited) { break }
    Start-Sleep -Milliseconds 500
  }

  Start-Sleep -Milliseconds 800
  if (-not $chromeProc.HasExited) { Stop-Process -Id $chromeProc.Id -Force -ErrorAction SilentlyContinue }
  if (-not $native.HasExited) { Stop-Process -Id $native.Id -Force -ErrorAction SilentlyContinue }

  Write-Host ""
  Write-Host "=== $(if ($Bench) { 'S0C bench lines' } else { 'verdict lines' }) (from the browser console, relayed via /result) ==="
  $pattern = if ($Bench) { "^\[browser\] S0C " } else { "^\[browser\] S0B " }
  $verdicts = Select-String -Path $nativeLog -Pattern $pattern | ForEach-Object { $_.Line }
  $verdicts | ForEach-Object { Write-Host $_ }
  Write-Host ""
  Write-Host "=== the same lines as Chromium logged them to its own console ==="
  Select-String -Path $chromeLog -Pattern $(if ($Bench) { "S0C " } else { "S0B " }) -ErrorAction SilentlyContinue |
    ForEach-Object { Write-Host $_.Line }

  if ($Bench) {
    $cells = @($verdicts | Where-Object { $_ -match "S0C (60hz|bulk|rx|saturate) run=" })
    Write-Host ""
    Write-Host "[run] bench cells: $($cells.Count) (expect 27)"
    if (-not $done) { Write-Host "[run] TIMED OUT waiting for the page"; exit 1 }
    if ($cells.Count -lt 27) { Write-Host "[run] MISSING bench cells"; exit 1 }
    Write-Host "[run] OK"
    exit 0
  }

  $ok = ($verdicts | Where-Object { $_ -match "S0B OK role=answerer dir=b2n" }) -and
        ($verdicts | Where-Object { $_ -match "S0B OK role=answerer dir=n2b" }) -and
        ($verdicts | Where-Object { $_ -match "S0B OK role=offerer dir=b2n" }) -and
        ($verdicts | Where-Object { $_ -match "S0B OK role=offerer dir=n2b" })
  if (-not $done) { Write-Host "[run] TIMED OUT waiting for the page"; exit 1 }
  if (-not $ok) { Write-Host "[run] MISSING round-trip verdict lines"; exit 1 }
  Write-Host "[run] OK"
  exit 0
} finally {
  Pop-Location
}
