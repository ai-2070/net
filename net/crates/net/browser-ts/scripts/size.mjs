// What a page actually downloads, in bytes, raw and gzipped.
//
//   node scripts/size.mjs              # table + machine-readable lines
//   node scripts/size.mjs --json       # JSON only
//   node scripts/size.mjs --assert     # non-zero exit if the wasm exceeds the gz limit
//   node scripts/size.mjs --require-leaf   # also fail if the real leaf wasm is absent
//
// Stage 5's exit criterion is `wasm <= 1.5 MB gzipped`. 1.5 MB is read
// decimally (1 500 000 B), the stricter of the two readings.
//
// The wasm is looked for in this order: $NET_LEAF_WASM, the copy
// `npm run build` puts in dist/, the leaf crate's own pkg/, then — only
// as a labelled INTERIM, because the real leaf is a sibling slice's
// artifact — the Stage 4b harness leaf. Every line says which one it
// measured, so an interim number can never be mistaken for the exit
// criterion being met.

import { gzipSync } from 'node:zlib';
import { readFile, readdir } from 'node:fs/promises';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const LIMIT_GZ = 1_500_000;

// S0a (docs/internal/spikes/S0A_WIRE_BOUNDARY.md §5): the wire crate
// alone, cdylib, opt-level=z + LTO + strip, *before* a wasm-bindgen CLI
// pass — so it still carries the 180 737 B `__wasm_bindgen_unstable`
// section that the CLI consumes.
const S0A_BASELINE = { label: 'S0a wire-only cdylib (pre-wasm-bindgen)', raw: 576_461, gz: 160_883 };

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const dist = join(packageRoot, 'dist');
// <repo>/net/crates/net/browser-ts -> <repo>
const repoRoot = resolve(packageRoot, '..', '..', '..', '..');
const argv = new Set(process.argv.slice(2));

const wasm = await findWasm();
const distFiles = await listDist();

const glue = distFiles.filter((name) => name.endsWith('.js') && isLeafGlue(name));
const ours = distFiles.filter((name) => name.endsWith('.js') && !isLeafGlue(name) && name !== 'index.bundle.js');

const rows = [];
if (wasm) rows.push({ name: `wasm (${wasm.origin})`, key: 'wasm', ...measure(wasm.bytes) });
if (glue.length > 0) rows.push({ name: 'wasm-bindgen glue', key: 'glue', ...(await measureFiles(glue)) });
if (ours.length > 0) {
  rows.push({ name: `package ESM (${ours.length} files)`, key: 'esm', ...(await measureFiles(ours)) });
}
if (distFiles.includes('index.bundle.js')) {
  rows.push({ name: 'package single-file bundle', key: 'bundle', ...(await measureFiles(['index.bundle.js'])) });
}

const shipped = rows.filter((row) => row.key !== 'esm');
const total = {
  name: 'total a page downloads (wasm + glue + bundle)',
  key: 'total',
  raw: shipped.reduce((sum, row) => sum + row.raw, 0),
  gz: shipped.reduce((sum, row) => sum + row.gz, 0),
};

const report = {
  limitGz: LIMIT_GZ,
  wasmSource: wasm ? { path: relative(repoRoot, wasm.path).replaceAll('\\', '/'), interim: wasm.interim } : null,
  baseline: S0A_BASELINE,
  rows: [...rows, total].map(({ name, key, raw, gz }) => ({ name, key, raw, gz })),
  wasmVsBaseline: wasm
    ? { rawDelta: rows[0].raw - S0A_BASELINE.raw, gzDelta: rows[0].gz - S0A_BASELINE.gz }
    : null,
  pass: wasm ? rows[0].gz <= LIMIT_GZ : false,
};

if (argv.has('--json')) {
  console.log(JSON.stringify(report, null, 2));
} else {
  print(report, [...rows, total]);
}

let failed = false;
if (argv.has('--require-leaf') && (wasm === null || wasm.interim)) {
  console.error('SIZE FAIL: no net-mesh-leaf wasm found (build it, or set NET_LEAF_WASM)');
  failed = true;
}
if (argv.has('--assert')) {
  if (wasm === null) {
    console.error('SIZE FAIL: nothing to measure');
    failed = true;
  } else if (!report.pass) {
    console.error(`SIZE FAIL: wasm ${rows[0].gz} B gzipped exceeds the ${LIMIT_GZ} B limit`);
    failed = true;
  }
}
process.exit(failed ? 1 : 0);

function measure(bytes) {
  return { raw: bytes.length, gz: gzipSync(bytes, { level: 9 }).length };
}

// Per-file gzip, summed: each file is a separate HTTP response, so
// each is compressed on its own.
async function measureFiles(names) {
  let raw = 0;
  let gz = 0;
  for (const name of names) {
    const bytes = await readFile(join(dist, name));
    raw += bytes.length;
    gz += gzipSync(bytes, { level: 9 }).length;
  }
  return { raw, gz };
}

function isLeafGlue(name) {
  return name.startsWith('net_leaf') || name.startsWith('leaf.');
}

async function listDist() {
  try {
    return await readdir(dist);
  } catch {
    return [];
  }
}

async function findWasm() {
  const candidates = [];
  if (process.env.NET_LEAF_WASM) {
    candidates.push({ path: resolve(process.env.NET_LEAF_WASM), origin: '$NET_LEAF_WASM', interim: false });
  }
  for (const name of await listDist()) {
    if (name.endsWith('.wasm')) candidates.push({ path: join(dist, name), origin: `dist/${name}`, interim: false });
  }
  candidates.push({
    path: resolve(packageRoot, '..', 'leaf', 'pkg', 'net_leaf_bg.wasm'),
    origin: 'leaf/pkg',
    interim: false,
  });
  candidates.push({
    path: resolve(packageRoot, '..', 'tests', 'rtc_browser', 'leaf', 'dist', 'leaf_bg.wasm'),
    origin: 'INTERIM: Stage 4b harness leaf',
    interim: true,
  });

  for (const candidate of candidates) {
    try {
      return { ...candidate, bytes: await readFile(candidate.path) };
    } catch {
      continue;
    }
  }
  return null;
}

function print(report, rows) {
  const kib = (bytes) => `${(bytes / 1024).toFixed(1)} KiB`;
  console.log('');
  console.log(`@net-mesh/browser download size — limit: wasm <= ${LIMIT_GZ} B gzipped`);
  if (report.wasmSource) {
    console.log(`wasm measured: ${report.wasmSource.path}${report.wasmSource.interim ? '   <-- INTERIM' : ''}`);
  } else {
    console.log('wasm measured: NONE FOUND');
  }
  console.log('');
  console.log('| artifact                                      |        raw |     gzip -9 |');
  console.log('|-----------------------------------------------|------------|-------------|');
  for (const row of rows) {
    console.log(`| ${row.name.padEnd(45)} | ${kib(row.raw).padStart(10)} | ${kib(row.gz).padStart(11)} |`);
  }
  console.log('');
  console.log('total counts the single-file bundle; a page serving the ESM directory');
  console.log('downloads the "package ESM" row in its place, not both.');
  console.log('');
  if (report.wasmVsBaseline) {
    const { rawDelta, gzDelta } = report.wasmVsBaseline;
    const sign = (n) => (n >= 0 ? `+${n}` : `${n}`);
    console.log(
      `vs ${S0A_BASELINE.label}: raw ${sign(rawDelta)} B (${sign(Math.round(rawDelta / 1024))} KiB), ` +
        `gz ${sign(gzDelta)} B (${sign(Math.round(gzDelta / 1024))} KiB)`,
    );
  }
  console.log('');
  // Machine-readable: one line per artifact, plus the verdict. A CI
  // step greps these instead of parsing the table.
  for (const row of rows) console.log(`SIZE ${row.key} raw=${row.raw} gz=${row.gz}`);
  console.log(`SIZE limit_gz=${LIMIT_GZ} wasm_interim=${report.wasmSource?.interim ?? 'none'}`);
  console.log(`SIZE verdict=${report.pass ? 'PASS' : 'FAIL'}`);
}
