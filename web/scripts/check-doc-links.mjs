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

import { readFileSync, readdirSync, statSync, existsSync } from "node:fs";
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

// Adaptive pages: a directory holding `_shared.md` plus one fragment per lens.
// They need their own handling because the URL structure does not match the file
// structure — `_shared.md` is not a page, and a rendition's anchor set is the
// SHARED body plus that one fragment, composed.
//
// Without this, `/docs/start/install` registered as an empty folder index and the
// fragment check below skips empty anchor sets, so a link to
// `/docs/start/install#feature-flags` passed while pointing at a heading that had
// moved into the Rust fragment. Found by converting the first adaptive page.
const SHARED_BODY = "_shared.md";
const LENS_SLUGS = ["rust", "typescript", "python", "go"];
const BOUNDARY_SLUG = "c";

const adaptiveDirs = new Set();
for (const file of files) {
  const rel = relative(DOCS, file).split("\\").join("/");
  if (rel.endsWith(`/${SHARED_BODY}`)) {
    adaptiveDirs.add(rel.slice(0, -(SHARED_BODY.length + 1)));
  }
}

const headingsOf = (src) => {
  // Fenced code blocks contain `#` comments, not headings.
  const prose = src.replace(/^```[\s\S]*?^```/gm, "");
  const slugger = new GithubSlugger();
  const anchors = new Set();
  for (const m of prose.matchAll(/^#{1,6}\s+(.+?)\s*$/gm)) {
    anchors.add(slugger.slug(m[1].replace(/[`*_]/g, "")));
  }
  return anchors;
};

// url → set of heading anchors on that page.
const pages = new Map();
const sources = new Map();

// Two URL shapes, and this checker has to know both or it validates links
// against routes that do not exist. D8 froze the second one: the SDK spine
// composes in place, so `/docs/sdk/python/announce` keeps its URL while its
// source moved to `sdk/announce/`.
//
// The rule mirrors `projectLensSections` in `web/src/lib/docs.ts`: an adaptive
// page whose parent section ALSO holds lens-named directories is projected, and
// its renditions live under the lens prefix instead of the page suffix. If those
// two rules ever disagree, this checker passes links the router will 404.
const sectionHasLensDirs = (dir) => {
  const section = dir.includes("/") ? dir.slice(0, dir.lastIndexOf("/")) : "";
  return LENS_SLUGS.some((lens) =>
    existsSync(join(DOCS, section, lens)),
  );
};

for (const dir of adaptiveDirs) {
  const sharedSrc = readFileSync(join(DOCS, dir, SHARED_BODY), "utf8");
  const base = `/docs/${toSlug(dir)}`;
  const projected = sectionHasLensDirs(dir);
  const section = dir.includes("/") ? dir.slice(0, dir.lastIndexOf("/")) : "";
  const page = dir.slice(dir.lastIndexOf("/") + 1);
  const renditionUrl = (lens) =>
    projected
      ? `/docs/${toSlug(`${section}/${lens}/${page}`)}`
      : `${base}/${lens}`;

  // The bare route is the neutral router in both shapes: universal body only.
  pages.set(base, headingsOf(sharedSrc));
  sources.set(`${dir}/${SHARED_BODY}`, { src: sharedSrc, url: base });

  // A projected page has NO `/c` route — the spine's C reader is served by the
  // annex, which has real pages of its own. Only the suffix shape generates a
  // boundary projection over the universal body.
  if (!projected) {
    pages.set(`${base}/${BOUNDARY_SLUG}`, headingsOf(sharedSrc));
  }

  for (const lens of LENS_SLUGS) {
    const fragPath = join(DOCS, dir, `${lens}.md`);
    const url = renditionUrl(lens);
    let composed = sharedSrc;
    if (existsSync(fragPath)) {
      const fragSrc = readFileSync(fragPath, "utf8");
      composed = `${sharedSrc}\n\n${fragSrc}`;
      sources.set(`${dir}/${lens}.md`, { src: fragSrc, url });
    }
    // A lens with no fragment still has a route — it renders the absence state
    // over the universal body — so register it either way.
    pages.set(url, headingsOf(composed));
  }
}

for (const file of files) {
  const relCheck = relative(DOCS, file).split("\\").join("/");
  if ([...adaptiveDirs].some((d) => relCheck.startsWith(`${d}/`))) continue;
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
