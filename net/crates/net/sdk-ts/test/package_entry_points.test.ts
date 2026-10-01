// Every SDK source file must be reachable from a published entry point, and
// every `@net-mesh/sdk/<subpath>` a doc tells a reader to import must exist.
//
// NODE_SDK_GAPS_PLAN.md S1. `src/deck.ts` shipped unreachable: `package.json`
// `exports` had no `./deck` and `index.ts` never re-exported it, while its
// own docs said `import … from '@net-mesh/sdk/deck'`. CI stayed green
// because `deck.test.ts` imported `../src/deck` directly. These checks read
// the source; `scripts/package-smoke.mjs` checks the built, packed package.

import { readFileSync, readdirSync, statSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';

import { describe, expect, it } from 'vitest';

const ROOT = resolve(__dirname, '..');
const SRC = join(ROOT, 'src');

interface ExportEntry {
  types?: string;
  default?: string;
}

const pkg = JSON.parse(readFileSync(join(ROOT, 'package.json'), 'utf8')) as {
  exports: Record<string, ExportEntry>;
};

/** `./dist/org/index.js` → `src/org/index.ts`. */
function distToSrc(distPath: string): string {
  return join(ROOT, distPath.replace(/^\.\/dist\//, 'src/').replace(/\.js$/, '.ts'));
}

function allSourceFiles(dir: string): string[] {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) return allSourceFiles(path);
    return path.endsWith('.ts') ? [path] : [];
  });
}

/** Resolve a relative import specifier from `fromFile` to a source file. */
function resolveRelative(fromFile: string, spec: string): string | undefined {
  // `src/` imports siblings both bare (`'./mesh'`) and with the emitted
  // extension (`'./_internal.js'`); the latter names a `.ts` source.
  const base = resolve(dirname(fromFile), spec.replace(/\.js$/, ''));
  for (const candidate of [`${base}.ts`, join(base, 'index.ts')]) {
    try {
      if (statSync(candidate).isFile()) return candidate;
    } catch {
      // not this shape; try the next
    }
  }
  return undefined;
}

const IMPORT_RE = /\bfrom\s+'(\.[^']+)'|\bimport\s+'(\.[^']+)'/g;

function reachableFrom(roots: string[]): Set<string> {
  const seen = new Set<string>();
  const stack = [...roots];
  while (stack.length > 0) {
    const file = stack.pop()!;
    if (seen.has(file)) continue;
    seen.add(file);
    for (const match of readFileSync(file, 'utf8').matchAll(IMPORT_RE)) {
      const target = resolveRelative(file, match[1] ?? match[2]);
      if (target) stack.push(target);
    }
  }
  return seen;
}

describe('package entry points', () => {
  const exportRoots = Object.values(pkg.exports).map((entry) => distToSrc(entry.default!));

  it('every exports entry points at a real source file', () => {
    for (const root of exportRoots) {
      expect(statSync(root).isFile(), relative(ROOT, root)).toBe(true);
    }
  });

  it('every src file is reachable from a published entry point', () => {
    const reachable = reachableFrom(exportRoots);
    const unreachable = allSourceFiles(SRC)
      .filter((file) => !reachable.has(file))
      .map((file) => relative(ROOT, file).replace(/\\/g, '/'));
    expect(unreachable).toEqual([]);
  });

  it('every @net-mesh/sdk/<subpath> named in the docs is exported', () => {
    const exported = new Set(Object.keys(pkg.exports));
    const docs = [...allSourceFiles(SRC), join(ROOT, 'README.md')];
    const missing: string[] = [];
    for (const file of docs) {
      const text = readFileSync(file, 'utf8');
      for (const match of text.matchAll(/@net-mesh\/sdk\/([a-z][a-z0-9-]*)/g)) {
        if (!exported.has(`./${match[1]}`)) {
          missing.push(`${relative(ROOT, file).replace(/\\/g, '/')}: @net-mesh/sdk/${match[1]}`);
        }
      }
    }
    // The README names one deliberately, as the example of what fails.
    // Exempt exactly that entry, so the same subpath named elsewhere fails.
    expect(missing.filter((m) => m !== 'README.md: @net-mesh/sdk/mesh')).toEqual([]);
  });
});
