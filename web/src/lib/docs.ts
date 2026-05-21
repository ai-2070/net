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
import type { Language } from "./docs-language";

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

function resolveTitle(slug: string[], rawName: string): string {
  return customLabel(slug) ?? titleize(rawName);
}

export type DocFile = {
  kind: "file";
  slug: string[];
  title: string;
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
  readme: DocFile | null;
  children: DocNode[];
  /** Languages this folder is gated to. Absent = universal. Gating a
   * folder hides its whole subtree when the current language doesn't
   * match. */
  languages?: Language[];
};

export type DocNode = DocFile | DocFolder;

export type DocTree = {
  rootReadme: DocFile | null;
  rootFiles: DocFile[];
  folders: DocFolder[];
};

export type ResolvedDoc =
  | { kind: "file"; file: DocFile; folder?: DocFolder }
  | { kind: "folder-index"; folder: DocFolder };

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

function buildFolder(absPath: string, slugChain: string[]): DocFolder {
  const folderName = slugChain[slugChain.length - 1] ?? "";
  const entries = readdirSync(absPath).sort((a, b) => a.localeCompare(b));
  const folders: DocFolder[] = [];
  const files: DocFile[] = [];
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
      const file: DocFile = {
        kind: "file",
        slug: childSlug,
        title: resolveTitle(childSlug, entry),
        filePath: entryPath,
        ext: extOf(entry),
        languages: lookupLanguages(childSlug),
      };
      if (isReadme(entry)) readme = file;
      else files.push(file);
    }
  }

  const folderKey = slugChain.join("/");
  const orderedChildren = applyOrder<DocNode>(
    [...folders, ...files],
    folderOrder(folderKey),
    (n) => n.slug[n.slug.length - 1] ?? "",
  );

  return {
    kind: "folder",
    slug: slugChain,
    title: resolveTitle(slugChain, folderName),
    readme,
    children: orderedChildren,
    languages: lookupLanguages(slugChain),
  };
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
      const file: DocFile = {
        kind: "file",
        slug: childSlug,
        title: resolveTitle(childSlug, entry),
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

export function resolveDoc(slug: string[]): ResolvedDoc | null {
  const tree = getDocTree();
  if (slug.length === 0) {
    if (tree.rootReadme) return { kind: "file", file: tree.rootReadme };
    return null;
  }

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
        if (folder.readme)
          return { kind: "file", file: folder.readme, folder };
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

export function readDocSource(file: DocFile): string {
  return readFileSync(file.filePath, "utf8");
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
function flattenForLinearOrder(tree: DocTree): LinearDoc[] {
  const out: LinearDoc[] = [];

  if (tree.rootReadme) {
    out.push({ slug: [], title: tree.rootReadme.title });
  }
  for (const f of tree.rootFiles) {
    out.push({ slug: f.slug, title: f.title });
  }
  for (const folder of tree.folders) {
    addFolder(folder, out, folder.title);
  }
  return out;
}

function addFolder(
  folder: DocFolder,
  out: LinearDoc[],
  section: string,
): void {
  if (folder.readme) {
    out.push({ slug: folder.slug, title: folder.title, section });
  }
  for (const child of folder.children) {
    if (child.kind === "file") {
      if (folder.readme && child === folder.readme) continue;
      out.push({ slug: child.slug, title: child.title, section });
    } else {
      addFolder(child, out, child.title);
    }
  }
}

let cachedLinear: LinearDoc[] | null = null;
function getLinearDocs(): LinearDoc[] {
  if (!IS_DEV && cachedLinear) return cachedLinear;
  const linear = flattenForLinearOrder(getDocTree());
  if (!IS_DEV) cachedLinear = linear;
  return linear;
}

// Look up the previous + next page in the sidebar order for a given slug.
// `currentSlug` is the URL slug ([] for /docs root, ["foo"] for /docs/foo).
// Returns nulls when there is no neighbor in that direction.
export function getPrevNext(currentSlug: string[]): {
  prev: LinearDoc | null;
  next: LinearDoc | null;
} {
  const list = getLinearDocs();
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
