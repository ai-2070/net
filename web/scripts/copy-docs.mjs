// Mirror the canonical Net docs from the Rust crate into the Next.js
// build tree so /docs can render them statically.
//
// Source: ../net/crates/net/docs
// Target: ./.docs-mirror
//
// Runs as `prebuild` and `predev`. Vercel only ships the `web/` directory,
// so the mirror needs to be committed-or-built before next build runs;
// `prebuild` covers both local and CI as long as the upstream tree is
// available on disk during the build.

import { existsSync, mkdirSync, readdirSync, statSync, copyFileSync, rmSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const ROOT = resolve(__dirname, "..");
const SRC = resolve(ROOT, "..", "net", "crates", "net", "docs");
const DEST = resolve(ROOT, ".docs-mirror");

function walk(srcDir, destDir) {
  if (!existsSync(srcDir)) return 0;
  mkdirSync(destDir, { recursive: true });
  let count = 0;
  for (const entry of readdirSync(srcDir)) {
    const srcPath = join(srcDir, entry);
    const destPath = join(destDir, entry);
    const s = statSync(srcPath);
    if (s.isDirectory()) {
      count += walk(srcPath, destPath);
    } else if (s.isFile() && entry.toLowerCase().endsWith(".md")) {
      copyFileSync(srcPath, destPath);
      count += 1;
    }
  }
  return count;
}

if (!existsSync(SRC)) {
  // Soft-fail: in environments without the crate (e.g. an isolated web-only
  // checkout) we leave .docs-mirror empty and let the route render an empty
  // index rather than failing the whole build.
  console.warn(
    `[copy-docs] source not found at ${SRC} — skipping; /docs will be empty.`,
  );
  mkdirSync(DEST, { recursive: true });
  process.exit(0);
}

if (existsSync(DEST)) rmSync(DEST, { recursive: true, force: true });
const n = walk(SRC, DEST);
console.log(`[copy-docs] mirrored ${n} markdown files → ${DEST}`);
