#!/usr/bin/env node
// Shim that locates the platform-specific @net-mesh/cli-<triple>
// package npm installed alongside this one (via the parent's
// optionalDependencies) and execs its bundled `net-mesh` binary,
// forwarding argv + stdio.
//
// Mirrors the pattern used by esbuild / swc / biome:
//   - One parent package (this file) is the only thing users
//     reference (`npm i @net-mesh/cli`, `npx net-mesh`).
//   - One per-platform binary package per supported triple. Each
//     pins `os` / `cpu` / `libc` so npm refuses to install it on
//     a non-matching host; only the matching one ends up in
//     node_modules.
//   - This shim resolves whichever one landed and execs the
//     binary. No build step, no postinstall, no compile-time
//     download.

'use strict';

const child_process = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');

// Detect musl vs glibc on Linux. Mirrors the napi-rs heuristic:
// the official Node.js binaries are linked against glibc, so
// `process.report.getReport()` exposes the libc family. Fall back
// to inspecting `/usr/bin/ldd` since alpine's musl ldd prints
// 'musl' in its output, and finally to assuming glibc.
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
  // Default to glibc — covers Debian/Ubuntu/RHEL/Fedora out of the
  // box; alpine users hit the musl branch above.
  return 'gnu';
}

function resolvePlatformPackage() {
  const platform = process.platform;
  const arch = process.arch;
  const libc = detectLibc();
  // Mapping from (platform, arch[, libc]) to the npm package name.
  // Stays in sync with the parent's optionalDependencies list and
  // the release workflow's matrix.
  if (platform === 'linux') {
    if (arch === 'x64') return `@net-mesh/cli-linux-x64-${libc}`;
    if (arch === 'arm64') return `@net-mesh/cli-linux-arm64-${libc}`;
  }
  if (platform === 'darwin') {
    if (arch === 'x64') return '@net-mesh/cli-darwin-x64';
    if (arch === 'arm64') return '@net-mesh/cli-darwin-arm64';
  }
  if (platform === 'win32') {
    if (arch === 'x64') return '@net-mesh/cli-win32-x64';
    if (arch === 'arm64') return '@net-mesh/cli-win32-arm64';
  }
  throw new Error(
    `Unsupported platform: ${platform}-${arch}${libc ? `-${libc}` : ''}. ` +
      'See https://github.com/ai-2070/net for the list of supported targets.',
  );
}

function findBinary() {
  const pkg = resolvePlatformPackage();
  const binName = process.platform === 'win32' ? 'net-mesh.exe' : 'net-mesh';
  // `require.resolve` finds the package.json of the platform
  // package; its directory contains the bin/.
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
    process.stderr.write(`net-mesh: ${err.message}\n`);
    process.exit(127);
  }
  // `spawnSync` with stdio: 'inherit' gives us transparent
  // stdin/stdout/stderr forwarding + signal propagation.
  const result = child_process.spawnSync(binPath, process.argv.slice(2), {
    stdio: 'inherit',
    windowsHide: false,
  });
  if (result.error) {
    process.stderr.write(`net-mesh: ${result.error.message}\n`);
    process.exit(1);
  }
  process.exit(result.status === null ? 1 : result.status);
}

main();
