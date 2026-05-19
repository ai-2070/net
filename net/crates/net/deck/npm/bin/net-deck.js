#!/usr/bin/env node
// Shim that locates the platform-specific @net-mesh/deck-<triple>
// package and execs its bundled `net-deck` binary. See
// `../../cli/npm/bin/net-mesh.js` for the design notes — the two
// shims are deliberately parallel.

'use strict';

const child_process = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');

function detectLibc() {
  if (process.platform !== 'linux') return null;
  try {
    const report = process.report.getReport();
    if (report && report.header && report.header.glibcVersionRuntime) {
      return 'gnu';
    }
  } catch (_) {}
  try {
    const ldd = child_process.execFileSync('/usr/bin/ldd', ['--version'], {
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    if (/musl/i.test(ldd)) return 'musl';
  } catch (_) {}
  return 'gnu';
}

function resolvePlatformPackage() {
  const platform = process.platform;
  const arch = process.arch;
  const libc = detectLibc();
  if (platform === 'linux') {
    if (arch === 'x64') return `@net-mesh/deck-linux-x64-${libc}`;
    if (arch === 'arm64') return `@net-mesh/deck-linux-arm64-${libc}`;
  }
  if (platform === 'darwin') {
    if (arch === 'x64') return '@net-mesh/deck-darwin-x64';
    if (arch === 'arm64') return '@net-mesh/deck-darwin-arm64';
  }
  if (platform === 'win32') {
    if (arch === 'x64') return '@net-mesh/deck-win32-x64';
    if (arch === 'arm64') return '@net-mesh/deck-win32-arm64';
  }
  throw new Error(
    `Unsupported platform: ${platform}-${arch}${libc ? `-${libc}` : ''}. ` +
      'See https://github.com/ai-2070/net for the list of supported targets.',
  );
}

function findBinary() {
  const pkg = resolvePlatformPackage();
  const binName = process.platform === 'win32' ? 'net-deck.exe' : 'net-deck';
  let pkgJsonPath;
  try {
    pkgJsonPath = require.resolve(`${pkg}/package.json`);
  } catch (err) {
    throw new Error(
      `Failed to locate ${pkg}. The optional dependency may not have ` +
        'installed — check your npm install logs. ' +
        `Underlying error: ${err.message}`,
    );
  }
  const binPath = path.join(path.dirname(pkgJsonPath), 'bin', binName);
  if (!fs.existsSync(binPath)) {
    throw new Error(
      `Binary missing at ${binPath}. The platform package is malformed.`,
    );
  }
  return binPath;
}

function main() {
  let binPath;
  try {
    binPath = findBinary();
  } catch (err) {
    process.stderr.write(`net-deck: ${err.message}\n`);
    process.exit(127);
  }
  const result = child_process.spawnSync(binPath, process.argv.slice(2), {
    stdio: 'inherit',
    windowsHide: false,
  });
  if (result.error) {
    process.stderr.write(`net-deck: ${result.error.message}\n`);
    process.exit(1);
  }
  process.exit(result.status === null ? 1 : result.status);
}

main();
