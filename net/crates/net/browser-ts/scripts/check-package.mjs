// Refuse to publish a package that does not carry what it declares.
//
// Every `exports` target in package.json must exist in the built `dist/`,
// and so must the leaf wasm the entry point loads. The list is read from
// the manifest rather than kept here, so a new subpath (`./world` was
// one) cannot ship empty because nobody added it to a hard-coded list.
//
//   npm run build && node scripts/check-package.mjs

import { existsSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const manifest = JSON.parse(readFileSync(join(root, 'package.json'), 'utf8'));

const targets = new Set(['./dist/net_leaf.js', './dist/net_leaf_bg.wasm']);
for (const [subpath, entry] of Object.entries(manifest.exports ?? {})) {
  const conditions = typeof entry === 'string' ? { default: entry } : entry;
  for (const [condition, target] of Object.entries(conditions)) {
    if (typeof target !== 'string') {
      console.error(`exports["${subpath}"].${condition} is not a path`);
      process.exitCode = 1;
      continue;
    }
    targets.add(target);
  }
}

let missing = 0;
for (const target of [...targets].sort()) {
  const present = existsSync(join(root, target));
  if (!present) missing += 1;
  console.log(`${present ? 'ok     ' : 'MISSING'} ${target}`);
}
if (missing > 0) {
  console.error(`\n${missing} declared file(s) missing from dist/: build first, or fix package.json exports`);
  process.exitCode = 1;
} else {
  console.log(`\nevery declared file is present (${targets.size})`);
}
