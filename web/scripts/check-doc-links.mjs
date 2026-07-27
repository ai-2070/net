#!/usr/bin/env node
// Validates every internal link in `src/content/docs`.
//
// Three failure classes, all of which have shipped to production before:
//
//   1. Relative links (`./x`, `../x`). A section README renders at its
//      *folder* URL — `start/README.md` is served at `/docs/start` — so the
//      browser resolves `./quickstart` against `/docs/` and 404s. The
//      renderer has a backstop for these, but they stay a hard error here:
//      author intent and browser resolution disagreeing is not something to
//      paper over silently.
//   2. `/docs/…` links pointing at a slug that doesn't exist.
//   3. `#fragment` targets with no matching heading on the destination page.
//
// Run: `npm run check:docs`

import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, relative, dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import GithubSlugger from "github-slugger";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const DOCS = join(ROOT, "src", "content", "docs");

function walk(dir) {
  const out = [];
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) out.push(...walk(full));
    else if (entry.endsWith(".md") || entry.endsWith(".mdx")) out.push(full);
  }
  return out;
}

// Mirrors `lib/docs.ts`: lowercase, `_` → `-`, drop the extension.
const toSlug = (relPath) =>
  relPath
    .replace(/\.mdx?$/, "")
    .split("/")
    .map((s) => s.toLowerCase().replace(/_/g, "-"))
    .join("/");

const files = walk(DOCS);

// url → set of heading anchors on that page.
const pages = new Map();
const sources = new Map();

for (const file of files) {
  const relPath = relative(DOCS, file).split("\\").join("/");
  const slug = toSlug(relPath);
  // A folder README is served at the folder URL, not at `<folder>/readme`.
  const url = slug.endsWith("/readme")
    ? `/docs/${slug.slice(0, -"/readme".length)}`
    : `/docs/${slug}`;

  const src = readFileSync(file, "utf8");
  sources.set(relPath, { src, url });

  // Fenced code blocks contain `#` comments, not headings.
  const prose = src.replace(/^```[\s\S]*?^```/gm, "");
  const slugger = new GithubSlugger();
  const anchors = new Set();
  for (const m of prose.matchAll(/^#{1,6}\s+(.+?)\s*$/gm)) {
    anchors.add(slugger.slug(m[1].replace(/[`*_]/g, "")));
  }
  pages.set(url, anchors);

  // Folder indexes render even without a README.
  let dir = dirname(relPath);
  while (dir && dir !== ".") {
    const folderUrl = `/docs/${toSlug(dir)}`;
    if (!pages.has(folderUrl)) pages.set(folderUrl, new Set());
    dir = dirname(dir);
  }
}
pages.set("/docs", new Set());

const LINK = /\[[^\]]*\]\(([^)\s]+?)(?:\s+"[^"]*")?\)/g;
const errors = [];

for (const [relPath, { src }] of sources) {
  const prose = src.replace(/^```[\s\S]*?^```/gm, "");
  for (const [, href] of prose.matchAll(LINK)) {
    if (/^(https?:|mailto:|#)/.test(href)) continue;

    if (href.startsWith("./") || href.startsWith("../")) {
      errors.push(
        `${relPath}: relative link "${href}" — use an absolute /docs/… path ` +
          `(relative links break on section READMEs, which render at the folder URL)`,
      );
      continue;
    }
    if (!href.startsWith("/")) {
      errors.push(`${relPath}: non-absolute link "${href}"`);
      continue;
    }

    const [path, frag] = href.split("#");
    const target = path.replace(/\/$/, "");
    if (!target.startsWith("/docs")) continue; // site route, not a doc

    const anchors = pages.get(target);
    if (!anchors) {
      errors.push(`${relPath}: link to "${href}" — no such doc page`);
    } else if (frag && anchors.size && !anchors.has(frag)) {
      errors.push(
        `${relPath}: link to "${href}" — no heading "#${frag}" on that page`,
      );
    }
  }
}

if (errors.length) {
  console.error(`✗ ${errors.length} broken link(s):\n`);
  for (const e of errors) console.error(`  ${e}`);
  process.exit(1);
}
console.error(`✓ ${sources.size} docs, all internal links resolve`);
