#!/usr/bin/env node
// Package-boundary smoke test for @net-mesh/sdk (NODE_SDK_GAPS_PLAN.md S1).
//
// The vitest suite resolves `../src/*`, so it never exercises what a consumer
// actually gets: the `exports` map, `dist/`, the `files` list, and the `.d.ts`
// declarations. `src/deck.ts` shipped unreachable for exactly that reason.
// This script packs the SDK and the in-tree @net-mesh/core, installs both
// tarballs into a throwaway consumer, and checks from there:
//
//   1. CommonJS `require` of the root and of every subpath in `exports`;
//   2. ESM `import()` of the same;
//   3. `tsc --noEmit` of a consumer importing them, under `moduleResolution`
//      `node16` and `bundler` (legacy `node`/node10 ignores `exports` and is
//      documented as unsupported for subpaths);
//   4. a negative control: an unexported subpath must fail with
//      ERR_PACKAGE_PATH_NOT_EXPORTED, proving the real `exports` map is read.
//
// Run after `npm run build` (needs `dist/`) and after the native core is built.

import { execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const SDK = resolve(fileURLToPath(new URL('..', import.meta.url)));
const CORE = resolve(SDK, '../bindings/node');
const NPM = process.platform === 'win32' ? 'npm.cmd' : 'npm';

const fail = (msg) => {
  console.error(`FAIL  ${msg}`);
  process.exitCode = 1;
};
const ok = (msg) => console.log(`ok    ${msg}`);

for (const [what, path] of [
  ['SDK dist (run `npm run build`)', join(SDK, 'dist', 'index.js')],
  ['native core (run `npx napi build`)', join(CORE, 'index.js')],
  ["core's JS entry points (run `npm run build:ts` in bindings/node)", join(CORE, 'errors.js')],
]) {
  if (!existsSync(path)) {
    console.error(`FAIL  missing ${what}: ${path}`);
    process.exit(1);
  }
}

const pkg = JSON.parse(readFileSync(join(SDK, 'package.json'), 'utf8'));
const subpaths = Object.keys(pkg.exports).map((key) =>
  key === '.' ? '@net-mesh/sdk' : `@net-mesh/sdk/${key.slice(2)}`,
);

// One representative named export per entry point, checked at runtime and in
// the type-check. A subpath that resolves but exports nothing useful fails.
const PROBES = {
  '@net-mesh/sdk': ['MeshNode', 'DeckClient', 'OperatorIdentity'],
  '@net-mesh/sdk/tool': ['serveTool'],
  '@net-mesh/sdk/org': ['OrgClient'],
  '@net-mesh/sdk/deck': ['DeckClient', 'OperatorIdentity'],
};
for (const spec of subpaths) {
  if (!PROBES[spec]) fail(`no probe names for exported subpath ${spec}; add some to PROBES`);
}

const work = mkdtempSync(join(tmpdir(), 'net-sdk-smoke-'));
// `npm` is `npm.cmd` on Windows, which only runs through a shell; nothing
// else needs one, so only npm gets it.
const run = (cmd, args, opts = {}) =>
  execFileSync(cmd, args, {
    cwd: work,
    encoding: 'utf8',
    shell: cmd === NPM && process.platform === 'win32',
    ...opts,
  });

try {
  // Pack both packages exactly as they would publish.
  const coreTgz = join(work, run(NPM, ['pack', CORE, '--silent', '--pack-destination', work]).trim().split(/\r?\n/).pop());
  const sdkTgz = join(work, run(NPM, ['pack', SDK, '--silent', '--pack-destination', work]).trim().split(/\r?\n/).pop());
  writeFileSync(join(work, 'package.json'), JSON.stringify({ name: 'smoke-consumer', private: true }));
  run(NPM, ['install', '--no-audit', '--no-fund', '--ignore-scripts', coreTgz, sdkTgz]);
  ok('packed and installed @net-mesh/core + @net-mesh/sdk');

  // 1 + 2 — runtime, CommonJS and ESM.
  const expectations = JSON.stringify(PROBES);
  writeFileSync(
    join(work, 'probe.cjs'),
    `const probes = ${expectations};
     for (const [spec, names] of Object.entries(probes)) {
       const mod = require(spec);
       for (const n of names) if (mod[n] === undefined) throw new Error(spec + ' lacks ' + n);
     }
     let threw = '';
     try { require('@net-mesh/sdk/mesh'); } catch (e) { threw = e.code || String(e); }
     if (threw !== 'ERR_PACKAGE_PATH_NOT_EXPORTED') throw new Error('negative control: ' + threw);`,
  );
  run(process.execPath, ['probe.cjs']);
  ok(`CommonJS require of ${subpaths.join(', ')}; unexported subpath refused`);

  writeFileSync(
    join(work, 'probe.mjs'),
    `const probes = ${expectations};
     for (const [spec, names] of Object.entries(probes)) {
       const mod = await import(spec);
       for (const n of names) if (mod[n] === undefined) throw new Error(spec + ' lacks ' + n);
     }`,
  );
  run(process.execPath, ['probe.mjs']);
  ok('ESM import() of every entry point');

  // 3 — declarations, under the resolution modes that honour `exports`.
  const imports = Object.entries(PROBES)
    .map(([spec, names], i) => `import { ${names.map((n) => `${n} as ${n}_${i}`).join(', ')} } from '${spec}';\n` +
      `export const used_${i} = [${names.map((n) => `${n}_${i}`).join(', ')}];`)
    .join('\n');
  writeFileSync(join(work, 'consumer.ts'), imports + '\n');
  const tsc = join(SDK, 'node_modules', 'typescript', 'bin', 'tsc');
  for (const [module, resolution] of [['node16', 'node16'], ['esnext', 'bundler']]) {
    writeFileSync(
      join(work, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: {
          target: 'ES2022',
          module,
          moduleResolution: resolution,
          strict: true,
          noEmit: true,
          skipLibCheck: false,
          types: ['node'],
          typeRoots: [join(SDK, 'node_modules', '@types')],
        },
        files: ['consumer.ts'],
      }),
    );
    run(process.execPath, [tsc, '-p', 'tsconfig.json']);
    ok(`declarations type-check (moduleResolution: ${resolution})`);
  }
} catch (err) {
  fail(String(err.stdout || '') + String(err.stderr || '') + String(err.message || err));
} finally {
  rmSync(work, { recursive: true, force: true });
}

if (process.exitCode) console.error('package smoke test FAILED');
else console.log('package smoke test passed');
