// Post-`tsc` build step: a single-file ESM bundle for pages that
// prefer one script over a served directory, plus the leaf's
// wasm-bindgen output copied next to the entry point so the default
// loader (`import(new URL('./net_leaf.js', import.meta.url))`) and the
// glue's own relative `.wasm` fetch both resolve without an import map.
//
//   node scripts/bundle.mjs
//
// The leaf `pkg/` is found at $NET_LEAF_PKG, else ../leaf/pkg. It is a
// build artifact of a Rust crate that is not a workspace member, so it
// is legitimately absent on a fresh clone: the bundle is still built,
// and the copy step reports what it skipped.

import { build } from 'esbuild';
import { cp, mkdir, readdir, stat } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const dist = join(packageRoot, 'dist');
const leafPkg = process.env.NET_LEAF_PKG
  ? resolve(process.env.NET_LEAF_PKG)
  : resolve(packageRoot, '..', 'leaf', 'pkg');

await mkdir(dist, { recursive: true });

const result = await build({
  entryPoints: [join(packageRoot, 'src', 'index.ts')],
  outfile: join(dist, 'index.bundle.js'),
  bundle: true,
  format: 'esm',
  platform: 'browser',
  target: ['es2022'],
  minify: true,
  sourcemap: false,
  legalComments: 'none',
  metafile: true,
});

const bundled = Object.entries(result.metafile.outputs)
  .filter(([file]) => file.endsWith('index.bundle.js'))
  .map(([, meta]) => meta.bytes)[0];
console.log(`bundle: dist/index.bundle.js (${bundled} B)`);

const copied = await copyLeafPkg();
if (copied.length === 0) {
  console.log(`leaf wasm: none copied — ${leafPkg} not built (set NET_LEAF_PKG to override)`);
} else {
  console.log(`leaf wasm: copied ${copied.join(', ')} from ${leafPkg}`);
}

async function copyLeafPkg() {
  let entries;
  try {
    entries = await readdir(leafPkg);
  } catch {
    return [];
  }
  const wanted = entries.filter(
    (name) => name.endsWith('.wasm') || (name.endsWith('.js') && !name.endsWith('.test.js')) || name.endsWith('.d.ts'),
  );
  for (const name of wanted) {
    const from = join(leafPkg, name);
    if (!(await stat(from)).isFile()) continue;
    await cp(from, join(dist, name));
  }
  return wanted;
}
