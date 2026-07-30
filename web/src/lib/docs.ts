import "server-only";
import { readFileSync, readdirSync, statSync, existsSync } from "node:fs";
import { join, resolve } from "node:path";
import title from "title";
import GithubSlugger from "github-slugger";
import { DOCS_ORDER } from "@/docs.order";

// Docs are co-located with the source tree now (MDX-capable). Both `.md`
// and `.mdx` files are accepted; the renderer picks parsing mode per file.
const DOCS_ROOT = resolve(process.cwd(), "src", "content", "docs");
const DOC_EXT_RE = /\.mdx?$/i;

// Language taxonomy lives in its own client-safe module; re-export here
// so server callers (this file's internals, `docs.order.ts` consumers via
// the `DocsOrderConfig` type) have a single import surface.
export {
  LANGUAGES,
  DEFAULT_LANGUAGE,
  isLanguage,
  type Language,
} from "./docs-language";
import type { Language, Lens } from "./docs-language";
import {
  LANGUAGES,
  LENSES,
  LENS_SLUG,
  entryVisibleIn,
  isBoundarySlug,
  lensFromSlug,
  slugLanguage,
} from "./docs-language";

export type DocsOrderConfig = {
  /** Order of top-level folders (sections). Unlisted append alpha after. */
  sections?: string[];
  /** Order of children within a folder, keyed by full slug path joined by `/`
   * (e.g. `"releases"`, `"plans/nested"`). Unlisted append alpha after. */
  folders?: Record<string, string[]>;
  /** Slug paths to omit from the sidebar entirely. Hidden folders cascade —
   * their children are unreachable too. Matching is case-insensitive. */
  hide?: string[];
  /** Custom display labels keyed by slug path. Overrides the auto-titleized
   * name in the sidebar, breadcrumbs, and page heading. */
  labels?: Record<string, string>;
  /** Per-entry language gating, keyed by slug path. An entry whose key is
   * absent (or whose value is an empty array) is universal — visible in
   * every language. An entry with a non-empty list is only shown when the
   * current language is in the list. Applies to both files and folders;
   * gating a folder hides its whole subtree. */
  languages?: Record<string, Language[]>;
};

// Reorders `items` by the slugs listed in `order`. Listed items come first
// in the given order; unlisted items keep their incoming (alpha) order and
// are appended after. Slug comparison is normalized (case-insensitive and
// `_`/`-` interchangeable) so the config can be authored in either form.
function applyOrder<T>(
  items: T[],
  order: string[] | undefined,
  key: (item: T) => string,
): T[] {
  if (!order || order.length === 0) return items;
  const map = new Map<string, T>();
  for (const item of items) map.set(normalizeSlug(key(item)), item);
  const out: T[] = [];
  const used = new Set<string>();
  for (const k of order) {
    const nk = normalizeSlug(k);
    const item = map.get(nk);
    if (item !== undefined) {
      out.push(item);
      used.add(nk);
    }
  }
  for (const item of items) {
    if (!used.has(normalizeSlug(key(item)))) out.push(item);
  }
  return out;
}

// Normalized lookup of a per-folder order list. Config keys are user-authored
// so we tolerate any casing and either `_` or `-` as separators (`Releases`,
// `releases`, `RELEASES`, and `release-notes` vs `release_notes` all match
// equivalently).
function folderOrder(folderKey: string): string[] | undefined {
  const cfg = DOCS_ORDER.folders;
  if (!cfg) return undefined;
  const target = normalizeSlug(folderKey);
  for (const k of Object.keys(cfg)) {
    if (normalizeSlug(k) === target) return cfg[k];
  }
  return undefined;
}

function isHidden(slug: string[]): boolean {
  const cfg = DOCS_ORDER.hide;
  if (!cfg || cfg.length === 0) return false;
  const target = normalizeSlug(slug.join("/"));
  return cfg.some((h) => normalizeSlug(h) === target);
}

function customLabel(slug: string[]): string | undefined {
  const cfg = DOCS_ORDER.labels;
  if (!cfg) return undefined;
  const target = normalizeSlug(slug.join("/"));
  for (const k of Object.keys(cfg)) {
    if (normalizeSlug(k) === target) return cfg[k];
  }
  return undefined;
}

function lookupLanguages(slug: string[]): Language[] | undefined {
  const cfg = DOCS_ORDER.languages;
  if (!cfg) return undefined;
  const target = normalizeSlug(slug.join("/"));
  for (const k of Object.keys(cfg)) {
    if (normalizeSlug(k) === target) {
      const langs = cfg[k];
      if (!langs || langs.length === 0) return undefined;
      return langs;
    }
  }
  return undefined;
}

// Precedence: the page's own frontmatter, then a `docs.order.ts` label
// override, then the titleized filename. Frontmatter wins because the page
// is the thing being named; the config override survives for entries with
// no file of their own (folders without a README).
function resolveTitle(
  slug: string[],
  rawName: string,
  frontmatter?: DocFrontmatter,
): string {
  return frontmatter?.title ?? customLabel(slug) ?? titleize(rawName);
}

export type DocFile = {
  kind: "file";
  slug: string[];
  title: string;
  /** One-line summary from frontmatter. Feeds page metadata and the
   *  folder index; absent when the page hasn't declared one. */
  description?: string;
  filePath: string;
  ext: "md" | "mdx";
  /** Languages this doc is gated to, per `DocsOrderConfig.languages`.
   * Absent = universal (visible in every language). */
  languages?: Language[];
};

export type DocFolder = {
  kind: "folder";
  slug: string[];
  title: string;
  /** One-line summary, taken from the folder README's frontmatter. */
  description?: string;
  readme: DocFile | null;
  children: DocNode[];
  /** Languages this folder is gated to. Absent = universal. Gating a
   * folder hides its whole subtree when the current language doesn't
   * match. */
  languages?: Language[];
  /** Set when this folder is an adaptive page rather than a section: a
   *  `_shared.md` universal body plus one fragment per lens. Its renditions are
   *  deliberately NOT in `children` — the sidebar shows one entry for the page,
   *  not five for its language variants. */
  adaptive?: AdaptivePage;
};

/** An adaptive page: one universal body, composed with a selected fragment.
 *
 * The universal text exists ONCE. That is the whole cost model — five authored
 * copies of a page is what this replaces — and it is why there is no hash
 * anywhere proving the copies match: there are no copies. */
export type AdaptivePage = {
  shared: DocFile;
  /** From the universal body's frontmatter — see `DocFrontmatter.boundary`. */
  boundary?: { href: string; label: string };
  /** Present lenses only. A lens with nothing to teach has no fragment, and the
   *  route renders the honest absence state rather than a Rust fallback. */
  fragments: Partial<Record<Lens, DocFile>>;
};

export type DocNode = DocFile | DocFolder;

export type DocTree = {
  rootReadme: DocFile | null;
  rootFiles: DocFile[];
  folders: DocFolder[];
};

export type ResolvedDoc =
  | { kind: "file"; file: DocFile; folder?: DocFolder }
  | { kind: "folder-index"; folder: DocFolder }
  /** One language's reading of an adaptive page: shared body + fragment. */
  | { kind: "rendition"; folder: DocFolder; page: AdaptivePage; lens: Lens }
  /** The C segment. Never an authored fragment — a generated projection over the
   *  universal body, because C is a boundary surface and not a fifth lens. */
  | { kind: "boundary"; folder: DocFolder; page: AdaptivePage }
  /** The bare adaptive URL. A neutral router, NOT the Rust rendition: rendering
   *  Rust here would keep Rust as the page's privileged public meaning, which is
   *  the framing this whole project removes. */
  | { kind: "adaptive-router"; folder: DocFolder; page: AdaptivePage };

function stripMdExt(name: string): string {
  return name.replace(DOC_EXT_RE, "");
}

function extOf(name: string): "md" | "mdx" {
  return /\.mdx$/i.test(name) ? "mdx" : "md";
}

function isDocFile(name: string): boolean {
  return DOC_EXT_RE.test(name);
}

// Lowercase + collapse any run of `_` or `-` into a single `-`. This is what
// appears in URLs under `/docs/...`. Used for both filenames and folder names
// so the URL form stays consistent regardless of how files were named on disk.
function normalizeSlug(s: string): string {
  return s.toLowerCase().replace(/[_-]+/g, "-");
}

function slugSegment(name: string): string {
  return normalizeSlug(stripMdExt(name));
}

// "releases" → "Releases", "example-title" → "Example Title",
// "EYE_OF_THE_TIGER" → "Eye of the Tiger". The `title` lib handles small-word
// rules (of/the/and/etc.) but doesn't split underscores/hyphens, so we
// pre-normalize separators first.
export function titleize(name: string): string {
  const cleaned = stripMdExt(name).replace(/[_-]+/g, " ").trim();
  if (!cleaned) return "";
  return title(cleaned);
}

function isReadme(name: string): boolean {
  return /^readme\.mdx?$/i.test(name);
}

// ---- Frontmatter ----------------------------------------------------------
//
// A doc may open with a `---` fenced block carrying `title` and
// `description`. That's the entire supported schema, which is why this is
// twenty lines of parser rather than a YAML dependency: keys are known,
// values are single-line scalars, and anything else is ignored rather than
// guessed at. Titles used to live in `docs.order.ts` as a hand-maintained
// label per page — a second file to remember to edit, and nothing caught
// you when you forgot.

export type DocFrontmatter = {
  title?: string;
  description?: string;
  /** Where an adaptive page's C content lives in the boundary annex.
   *
   *  C never gets an authored fragment — it is a boundary surface documented by
   *  boundary concern, not by capability. But a page whose C material moved into
   *  the annex should send the reader to the RIGHT annex page rather than to the
   *  section index, and only the page knows where that is.
   *
   *  This is the seam the capability record fills later: D5 renders the absence
   *  state from `alternative.href`, and the site cannot read YAML yet. Until then
   *  the page declares it.
   */
  boundary?: string;
  boundaryLabel?: string;
};

const FRONTMATTER = /^---\r?\n([\s\S]*?)\r?\n---\r?\n?/;

function parseFrontmatter(source: string): {
  data: DocFrontmatter;
  body: string;
} {
  const match = FRONTMATTER.exec(source);
  if (!match) return { data: {}, body: source };

  const data: DocFrontmatter = {};
  for (const line of match[1].split(/\r?\n/)) {
    const at = line.indexOf(":");
    if (at < 0) continue;
    const key = line.slice(0, at).trim();
    if (
      key !== "title" &&
      key !== "description" &&
      key !== "boundary" &&
      key !== "boundaryLabel"
    ) {
      continue;
    }
    let value = line.slice(at + 1).trim();
    // Strip one layer of matching quotes; a description with a colon in it
    // has to be quoted, and we shouldn't hand the quotes to the renderer.
    if (
      (value.startsWith('"') && value.endsWith('"')) ||
      (value.startsWith("'") && value.endsWith("'"))
    ) {
      value = value.slice(1, -1);
    }
    if (value) data[key] = value;
  }
  return { data, body: source.slice(match[0].length) };
}

// Frontmatter is read once per file during the tree walk and again when a
// page renders. In production the tree is memoized, so this is a handful of
// reads per build rather than per request.
function readFrontmatter(filePath: string): DocFrontmatter {
  try {
    return parseFrontmatter(readFileSync(filePath, "utf8")).data;
  } catch {
    return {};
  }
}

function buildFolder(absPath: string, slugChain: string[]): DocFolder {
  const folderName = slugChain[slugChain.length - 1] ?? "";
  const entries = readdirSync(absPath).sort((a, b) => a.localeCompare(b));
  const folders: DocFolder[] = [];
  let files: DocFile[] = [];
  let readme: DocFile | null = null;

  for (const entry of entries) {
    const entryPath = join(absPath, entry);
    const stat = statSync(entryPath);
    if (stat.isDirectory()) {
      const childSlug = [...slugChain, normalizeSlug(entry)];
      if (isHidden(childSlug)) continue;
      folders.push(buildFolder(entryPath, childSlug));
    } else if (stat.isFile() && isDocFile(entry)) {
      const childSlug = [...slugChain, slugSegment(entry)];
      if (isHidden(childSlug)) continue;
      const fm = readFrontmatter(entryPath);
      const file: DocFile = {
        kind: "file",
        slug: childSlug,
        title: resolveTitle(childSlug, entry, fm),
        description: fm.description,
        filePath: entryPath,
        ext: extOf(entry),
        languages: lookupLanguages(childSlug),
      };
      if (isReadme(entry)) readme = file;
      else files.push(file);
    }
  }

  // Adaptive shape: a `_shared.md` universal body beside one file per lens.
  // Detected structurally rather than declared in frontmatter, so a directory
  // cannot claim to be an adaptive page while missing its universal body.
  const shared = files.find((f) => baseName(f.filePath) === SHARED_BODY);
  let adaptive: AdaptivePage | undefined;
  if (shared) {
    const fragments: Partial<Record<Lens, DocFile>> = {};
    for (const lens of LENSES) {
      const match = files.find(
        (f) => baseName(f.filePath) === `${LENS_SLUG[lens]}.md`,
      );
      if (match) fragments[lens] = match;
    }
    const fm = sharedFm(shared);
    adaptive = {
      shared,
      fragments,
      boundary: fm?.boundary
        ? { href: fm.boundary, label: fm.boundaryLabel ?? "the C ABI section" }
        : undefined,
    };
    // The page is one sidebar entry, not five. Its parts are reached through
    // `adaptive`, so drop them from `children` — otherwise every adaptive page
    // would expand into `_shared`, `rust`, `typescript`, `python`, `go` in the
    // nav, which is the five-manuals shape in navigation form.
    const parts = new Set<DocFile>([shared, ...Object.values(fragments)]);
    files = files.filter((f) => !parts.has(f));
  }

  const folderKey = slugChain.join("/");
  const orderedChildren = applyOrder<DocNode>(
    [...folders, ...files],
    folderOrder(folderKey),
    (n) => n.slug[n.slug.length - 1] ?? "",
  );

  // A folder renders at its README's URL, so the README's frontmatter names
  // the section. Folders without one fall back to the config label.
  const readmeFm = readme ? readFrontmatter(readme.filePath) : undefined;

  return {
    kind: "folder",
    slug: slugChain,
    title: resolveTitle(slugChain, folderName, readmeFm),
    // An adaptive page's title and description come from its universal body,
    // which is the only part every reader sees.
    description: readmeFm?.description ?? sharedFm(shared)?.description,
    readme,
    children: orderedChildren,
    languages: lookupLanguages(slugChain),
    adaptive,
  };
}

const SHARED_BODY = "_shared.md";

function baseName(filePath: string): string {
  return filePath.slice(filePath.lastIndexOf("/") + 1);
}

function sharedFm(shared: DocFile | undefined): DocFrontmatter | undefined {
  return shared ? readFrontmatter(shared.filePath) : undefined;
}

// In production every doc path is enumerated at build time, so the tree is
// safe to memoize once. In dev we always re-walk so additions / renames /
// deletions show up on the next request — content files aren't ES modules,
// so Next.js's HMR doesn't watch them, and a stale cache here is why
// `npm run dev` looked like it had stopped picking up MDX changes.
const IS_DEV = process.env.NODE_ENV !== "production";
let cached: DocTree | null = null;

export function getDocTree(): DocTree {
  if (!IS_DEV && cached) return cached;
  const tree = buildDocTree();
  if (!IS_DEV) cached = tree;
  return tree;
}

function buildDocTree(): DocTree {
  if (!existsSync(DOCS_ROOT)) {
    return { rootReadme: null, rootFiles: [], folders: [] };
  }
  const entries = readdirSync(DOCS_ROOT).sort((a, b) => a.localeCompare(b));
  const folders: DocFolder[] = [];
  const rootFiles: DocFile[] = [];
  let rootReadme: DocFile | null = null;

  for (const entry of entries) {
    const entryPath = join(DOCS_ROOT, entry);
    const stat = statSync(entryPath);
    if (stat.isDirectory()) {
      const childSlug = [normalizeSlug(entry)];
      if (isHidden(childSlug)) continue;
      folders.push(buildFolder(entryPath, childSlug));
    } else if (stat.isFile() && isDocFile(entry)) {
      const childSlug = [slugSegment(entry)];
      if (isHidden(childSlug)) continue;
      const fm = readFrontmatter(entryPath);
      const file: DocFile = {
        kind: "file",
        slug: childSlug,
        title: resolveTitle(childSlug, entry, fm),
        description: fm.description,
        filePath: entryPath,
        ext: extOf(entry),
        languages: lookupLanguages(childSlug),
      };
      if (isReadme(entry)) rootReadme = file;
      else rootFiles.push(file);
    }
  }

  const orderedFolders = applyOrder(
    folders,
    DOCS_ORDER.sections,
    (f) => f.slug[f.slug.length - 1] ?? "",
  );

  return { rootReadme, rootFiles, folders: orderedFolders };
}

function lastSlug(n: DocNode): string {
  return n.slug[n.slug.length - 1] ?? "";
}

/** Walk folders only, returning the folder at `slug` if there is one. */
function folderAt(slug: string[]): DocFolder | null {
  const tree = getDocTree();
  let folders = tree.folders;
  let found: DocFolder | null = null;
  for (const raw of slug) {
    const segment = normalizeSlug(raw);
    const next: DocFolder | undefined = folders.find(
      (f) => lastSlug(f) === segment,
    );
    if (!next) return null;
    found = next;
    folders = next.children.filter((c): c is DocFolder => c.kind === "folder");
  }
  return found;
}

/** Adaptive routes, resolved before the generic file/folder walk.
 *
 * Three shapes at one folder: the bare URL (a neutral router), a lens segment
 * (shared body + that fragment), and `c` (a generated boundary projection).
 * Handled here rather than inside the generic loop because an adaptive page's
 * parts are intentionally absent from `children`.
 */
function resolveAdaptive(slug: string[]): ResolvedDoc | null {
  const whole = folderAt(slug);
  if (whole?.adaptive) {
    return { kind: "adaptive-router", folder: whole, page: whole.adaptive };
  }
  if (slug.length < 2) return null;
  const parent = folderAt(slug.slice(0, -1));
  if (!parent?.adaptive) return null;
  const last = normalizeSlug(slug[slug.length - 1]!);
  if (isBoundarySlug(last)) {
    return { kind: "boundary", folder: parent, page: parent.adaptive };
  }
  const lens = lensFromSlug(last);
  // A lens with no fragment is not a 404 — the route exists and renders the
  // honest absence state. Falling back to another language here is the specific
  // failure the doctrine forbids.
  if (lens) {
    return { kind: "rendition", folder: parent, page: parent.adaptive, lens };
  }
  return null;
}

export function resolveDoc(slug: string[]): ResolvedDoc | null {
  const tree = getDocTree();
  if (slug.length === 0) {
    if (tree.rootReadme) return { kind: "file", file: tree.rootReadme };
    return null;
  }

  const adaptive = resolveAdaptive(slug);
  if (adaptive) return adaptive;

  // Normalize incoming segments so callers can pass either underscore or
  // dash forms (defensive — static-param-generated URLs are already
  // normalized).
  const norm = slug.map(normalizeSlug);
  let folders: DocFolder[] = tree.folders;
  let files: DocFile[] = tree.rootFiles;
  let currentFolder: DocFolder | undefined;

  for (let i = 0; i < norm.length; i++) {
    const segment = norm[i]!;
    const isLast = i === norm.length - 1;

    if (isLast) {
      const file = files.find((f) => lastSlug(f) === segment);
      if (file) return { kind: "file", file, folder: currentFolder };

      const folder = folders.find((f) => lastSlug(f) === segment);
      if (folder) {
        if (folder.readme) return { kind: "file", file: folder.readme, folder };
        return { kind: "folder-index", folder };
      }
      return null;
    }

    const next = folders.find((f) => lastSlug(f) === segment);
    if (!next) return null;
    currentFolder = next;
    folders = next.children.filter((c): c is DocFolder => c.kind === "folder");
    files = next.children.filter((c): c is DocFile => c.kind === "file");
    if (next.readme) files = [next.readme, ...files];
  }

  return null;
}

// Every slug a page can be served at — root files, every folder index,
// every nested file. Used by generateStaticParams.
export function getAllSlugs(): string[][] {
  const tree = getDocTree();
  const out: string[][] = [];

  const walkFolder = (folder: DocFolder): void => {
    out.push(folder.slug);
    if (folder.adaptive) {
      // One route per lens WHETHER OR NOT it has a fragment: an absent lens gets
      // a real URL that says so. A missing route would 404 a language the
      // switcher offers, which reads as the docs having forgotten the selection.
      for (const lens of LENSES) out.push([...folder.slug, LENS_SLUG[lens]]);
      out.push([...folder.slug, LENS_SLUG.c]);
    }
    for (const child of folder.children) {
      if (child.kind === "file") {
        // Don't double-count READMEs (they ARE the folder index).
        if (folder.readme && child === folder.readme) continue;
        out.push(child.slug);
      } else {
        walkFolder(child);
      }
    }
  };

  for (const f of tree.rootFiles) out.push(f.slug);
  for (const folder of tree.folders) walkFolder(folder);

  return out;
}

// Returns the body only. Frontmatter is metadata for the tree, not content —
// leaving it in would render a stray `---` rule and a line of `title: …` at
// the top of every page, and would feed the search index too.
export function readDocSource(file: DocFile): string {
  return parseFrontmatter(readFileSync(file.filePath, "utf8")).body;
}

/** The composed source for one rendition: universal body, then the fragment.
 *
 * Composed at render time from two files rather than stored as a fifth copy, so
 * editing the universal text updates every rendition and there is nothing to
 * keep in sync. Headings therefore appear once per document, which is what keeps
 * `rehype-slug` from emitting `verify-it-worked-3` and the TOC from listing the
 * same section four times.
 */
export function composeRendition(
  page: AdaptivePage,
  lens: Lens,
): { source: string; hasFragment: boolean } {
  const shared = readDocSource(page.shared);
  const fragment = page.fragments[lens];
  if (!fragment) return { source: shared, hasFragment: false };
  return {
    source: `${shared.trimEnd()}\n\n${readDocSource(fragment).trimStart()}`,
    hasFragment: true,
  };
}

// Table-of-contents entry for one heading in a doc.
export type TocEntry = {
  id: string;
  title: string;
  level: number; // 2 | 3 | 4 — h1 is page title, intentionally skipped
};

// Strip simple markdown formatting from heading text so the TOC label
// reads cleanly (no asterisks, no backticks, no link syntax).
function stripInline(s: string): string {
  return s
    .replace(/\*\*(.*?)\*\*/g, "$1")
    .replace(/__(.*?)__/g, "$1")
    .replace(/\*([^*]+)\*/g, "$1")
    .replace(/_([^_]+)_/g, "$1")
    .replace(/`([^`]+)`/g, "$1")
    .replace(/\[([^\]]+)\]\([^)]+\)/g, "$1");
}

// A single page in the linear prev/next reading order.
export type LinearDoc = {
  slug: string[]; // URL slug array (empty = /docs)
  title: string;
  section?: string; // parent folder title, for context label
};

// Walk the tree in the order it's displayed in the sidebar (sections-config
// first, then per-folder order) and produce a flat list of "readable" pages.
// Auto-generated folder-index pages (folders without a README) are skipped
// since they're just listings; folder READMEs are included as the section's
// landing page.
function flattenForLinearOrder(
  tree: DocTree,
  lang?: Language,
): LinearDoc[] {
  const out: LinearDoc[] = [];

  if (tree.rootReadme) {
    out.push({ slug: [], title: tree.rootReadme.title });
  }
  for (const f of tree.rootFiles) {
    if (lang && !entryVisibleIn(f, lang)) continue;
    out.push({ slug: f.slug, title: f.title });
  }
  for (const folder of tree.folders) {
    // Top-level folders have no parent section to label them with.
    addFolder(folder, out, undefined, lang);
  }
  return out;
}

function addFolder(
  folder: DocFolder,
  out: LinearDoc[],
  parentSection: string | undefined,
  lang?: Language,
): void {
  // A folder gated to other languages takes its whole subtree with it, exactly
  // as the sidebar does. Without this the linear order and the sidebar disagree,
  // and prev/next walks into pages the reader cannot see in the nav.
  if (lang && !entryVisibleIn(folder, lang)) return;
  // A folder README IS the section landing — its context label is the
  // *parent* folder's title (if any), not its own. Using its own title would
  // render the section line and the page title as the same string.
  if (folder.readme) {
    out.push({
      slug: folder.slug,
      title: folder.title,
      section: parentSection,
    });
  }
  for (const child of folder.children) {
    if (child.kind === "file") {
      if (folder.readme && child === folder.readme) continue;
      if (lang && !entryVisibleIn(child, lang)) continue;
      // Children get the *containing* folder's title as their section
      // context, regardless of how deeply nested the folder is.
      out.push({ slug: child.slug, title: child.title, section: folder.title });
    } else {
      addFolder(child, out, folder.title, lang);
    }
  }
}

const cachedLinear = new Map<string, LinearDoc[]>();
function getLinearDocs(lang?: Language): LinearDoc[] {
  const key = lang ?? "*";
  if (!IS_DEV) {
    const hit = cachedLinear.get(key);
    if (hit) return hit;
  }
  const linear = flattenForLinearOrder(getDocTree(), lang);
  if (!IS_DEV) cachedLinear.set(key, linear);
  return linear;
}

// Look up the previous + next page in the sidebar order for a given slug.
// `currentSlug` is the URL slug ([] for /docs root, ["foo"] for /docs/foo).
// Returns nulls when there is no neighbor in that direction.
/** Prev/next for every language, resolved at build time.
 *
 * The reader's language lives in a client store and the pages are static, so the
 * only way for prev/next to respect it is to bake all five answers and let the
 * client pick. It is five small objects per page.
 *
 * This is the fix for the sharpest of the three language defects: the single
 * language-blind order delivered a Python reader who finished
 * `sdk/python/errors` into `sdk/go/quickstart` — a language they did not choose,
 * in a section their own sidebar hides.
 */
export function getPrevNextByLanguage(
  currentSlug: string[],
): Record<Language, { prev: LinearDoc | null; next: LinearDoc | null }> {
  const out = {} as Record<
    Language,
    { prev: LinearDoc | null; next: LinearDoc | null }
  >;
  for (const lang of LANGUAGES) {
    out[lang] = getPrevNext(currentSlug, lang);
  }
  return out;
}

export function getPrevNext(
  currentSlug: string[],
  lang?: Language,
): {
  prev: LinearDoc | null;
  next: LinearDoc | null;
} {
  const list = getLinearDocs(lang);
  const key = currentSlug.join("/");
  const idx = list.findIndex((d) => d.slug.join("/") === key);
  if (idx < 0) return { prev: null, next: null };
  return {
    prev: idx > 0 ? (list[idx - 1] ?? null) : null,
    next: idx < list.length - 1 ? (list[idx + 1] ?? null) : null,
  };
}

// Parse h2/h3/h4 headings out of the raw markdown source. Code fences are
// skipped so `## comments` inside a Rust snippet don't show up. IDs are
// generated with the same slugger rehype-slug uses, so the TOC anchors
// match the rendered DOM IDs exactly.
export function extractToc(source: string): TocEntry[] {
  const slugger = new GithubSlugger();
  const out: TocEntry[] = [];
  const lines = source.split("\n");
  let inFence = false;
  let fenceChar = "";

  for (const line of lines) {
    const fence = /^(```|~~~)/.exec(line);
    if (fence) {
      const ch = fence[1]!;
      if (!inFence) {
        inFence = true;
        fenceChar = ch;
      } else if (line.startsWith(fenceChar)) {
        inFence = false;
        fenceChar = "";
      }
      continue;
    }
    if (inFence) continue;

    const m = /^(#{2,4})\s+(.+?)\s*#*\s*$/.exec(line);
    if (!m) continue;
    const level = m[1]!.length;
    const text = stripInline(m[2]!.trim());
    if (!text) continue;
    const id = slugger.slug(text);
    out.push({ id, title: text, level });
  }
  return out;
}

// Client-safe view of the tree (no fs paths).
export type ClientDocFile = {
  kind: "file";
  slug: string[];
  title: string;
  languages?: Language[];
};

export type ClientDocFolder = {
  kind: "folder";
  slug: string[];
  title: string;
  hasReadme: boolean;
  children: ClientDocNode[];
  languages?: Language[];
};

export type ClientDocNode = ClientDocFile | ClientDocFolder;

export type ClientDocTree = {
  hasRootReadme: boolean;
  rootFiles: ClientDocFile[];
  folders: ClientDocFolder[];
};

function toClientFile(f: DocFile): ClientDocFile {
  return { kind: "file", slug: f.slug, title: f.title, languages: f.languages };
}

function toClientFolder(f: DocFolder): ClientDocFolder {
  return {
    kind: "folder",
    slug: f.slug,
    title: f.title,
    hasReadme: f.readme !== null,
    children: f.children.map((c) =>
      c.kind === "file" ? toClientFile(c) : toClientFolder(c),
    ),
    languages: f.languages,
  };
}

export function getClientDocTree(): ClientDocTree {
  const t = getDocTree();
  return {
    hasRootReadme: t.rootReadme !== null,
    rootFiles: t.rootFiles.map(toClientFile),
    folders: t.folders.map(toClientFolder),
  };
}

// The version the docs describe — derived, not typed.
//
// This existed as a hardcoded `v0.17` in the sidebar footer while the newest
// release page was v0.33: a wrong version on all 149 pages, and precisely the
// kind of claim that rots because nothing points at it. The newest release note
// *is* the version the docs describe, so read it.
//
// Numeric compare, not string: `0.9` sorts above `0.33` lexically, so a string
// max would have reported v0.9 the moment 0.10 shipped.
export function getDocsVersion(): string {
  let best: [number, number] | null = null;
  const dir = join(DOCS_ROOT, "releases");
  if (!existsSync(dir)) return "";
  for (const entry of readdirSync(dir)) {
    const m = /^release[_-]v(\d+)\.(\d+)/i.exec(entry);
    if (!m) continue;
    const v: [number, number] = [Number(m[1]), Number(m[2])];
    if (!best || v[0] > best[0] || (v[0] === best[0] && v[1] > best[1])) best = v;
  }
  return best ? `v${best[0]}.${best[1]}` : "";
}

/** Build-time proof that prev/next never crosses a language boundary.
 *
 * The acceptance criterion for this fix is a test rather than an inspection, and
 * the thing under test is server-only code in a package with no test runner. So
 * the assertion runs where it cannot be skipped: `generateStaticParams` calls it,
 * which means every `next build` — and therefore every CI run — either proves the
 * invariant or fails.
 *
 * The invariant: walking the order a reader of language L actually sees, every
 * page is either universal or L's. A neighbour belonging to another language is
 * exactly the defect this replaced — a Python reader finishing `sdk/python/errors`
 * was handed `sdk/go/quickstart`.
 */
export function assertNoCrossLanguageNeighbours(): void {
  const problems: string[] = [];
  for (const lang of LANGUAGES) {
    for (const doc of getLinearDocs(lang)) {
      const owner = slugLanguage(doc.slug);
      if (owner !== null && owner !== lang) {
        problems.push(
          `reading as \`${lang}\`, the order contains \`/docs/${doc.slug.join("/")}\`, ` +
            `which belongs to \`${owner}\``,
        );
      }
    }
  }
  if (problems.length > 0) {
    throw new Error(
      `prev/next crosses a language boundary (${problems.length} case(s)):\n  ` +
        problems.slice(0, 10).join("\n  "),
    );
  }
}
