import "server-only";
import { readFileSync, readdirSync, statSync, existsSync } from "node:fs";
import { join, resolve } from "node:path";
import title from "title";
import { DOCS_ORDER } from "@/docs.order";

const DOCS_ROOT = resolve(process.cwd(), ".docs-mirror");

export type DocsOrderConfig = {
  /** Order of top-level folders (sections). Unlisted append alpha after. */
  sections?: string[];
  /** Order of children within a folder, keyed by full slug path joined by `/`
   * (e.g. `"releases"`, `"plans/nested"`). Unlisted append alpha after. */
  folders?: Record<string, string[]>;
};

// Reorders `items` by the slugs listed in `order`. Listed items come first
// in the given order; unlisted items keep their incoming (alpha) order and
// are appended after. Slug comparison is case-insensitive so the config
// can use any casing.
function applyOrder<T>(
  items: T[],
  order: string[] | undefined,
  key: (item: T) => string,
): T[] {
  if (!order || order.length === 0) return items;
  const map = new Map<string, T>();
  for (const item of items) map.set(key(item).toLowerCase(), item);
  const out: T[] = [];
  const used = new Set<string>();
  for (const k of order) {
    const lower = k.toLowerCase();
    const item = map.get(lower);
    if (item !== undefined) {
      out.push(item);
      used.add(lower);
    }
  }
  for (const item of items) {
    if (!used.has(key(item).toLowerCase())) out.push(item);
  }
  return out;
}

// Case-insensitive lookup of a per-folder order list. The config keys are
// user-authored so we tolerate any casing (`releases` / `Releases` / `RELEASES`
// all work for the same folder).
function folderOrder(folderKey: string): string[] | undefined {
  const cfg = DOCS_ORDER.folders;
  if (!cfg) return undefined;
  const lower = folderKey.toLowerCase();
  for (const k of Object.keys(cfg)) {
    if (k.toLowerCase() === lower) return cfg[k];
  }
  return undefined;
}

export type DocFile = {
  kind: "file";
  slug: string[];
  title: string;
  filePath: string;
};

export type DocFolder = {
  kind: "folder";
  slug: string[];
  title: string;
  readme: DocFile | null;
  children: DocNode[];
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
  return name.replace(/\.md$/i, "");
}

function slugSegment(name: string): string {
  return stripMdExt(name).toLowerCase();
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
  return /^readme\.md$/i.test(name);
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
      folders.push(buildFolder(entryPath, [...slugChain, entry.toLowerCase()]));
    } else if (stat.isFile() && entry.toLowerCase().endsWith(".md")) {
      const file: DocFile = {
        kind: "file",
        slug: [...slugChain, slugSegment(entry)],
        title: titleize(entry),
        filePath: entryPath,
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
    title: titleize(folderName),
    readme,
    children: orderedChildren,
  };
}

let cached: DocTree | null = null;

export function getDocTree(): DocTree {
  if (cached) return cached;
  if (!existsSync(DOCS_ROOT)) {
    cached = { rootReadme: null, rootFiles: [], folders: [] };
    return cached;
  }
  const entries = readdirSync(DOCS_ROOT).sort((a, b) => a.localeCompare(b));
  const folders: DocFolder[] = [];
  const rootFiles: DocFile[] = [];
  let rootReadme: DocFile | null = null;

  for (const entry of entries) {
    const entryPath = join(DOCS_ROOT, entry);
    const stat = statSync(entryPath);
    if (stat.isDirectory()) {
      folders.push(buildFolder(entryPath, [entry.toLowerCase()]));
    } else if (stat.isFile() && entry.toLowerCase().endsWith(".md")) {
      const file: DocFile = {
        kind: "file",
        slug: [slugSegment(entry)],
        title: titleize(entry),
        filePath: entryPath,
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

  cached = { rootReadme, rootFiles, folders: orderedFolders };
  return cached;
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

  let folders: DocFolder[] = tree.folders;
  let files: DocFile[] = tree.rootFiles;
  let currentFolder: DocFolder | undefined;

  for (let i = 0; i < slug.length; i++) {
    const segment = slug[i]!;
    const isLast = i === slug.length - 1;

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

// Client-safe view of the tree (no fs paths).
export type ClientDocFile = {
  kind: "file";
  slug: string[];
  title: string;
};

export type ClientDocFolder = {
  kind: "folder";
  slug: string[];
  title: string;
  hasReadme: boolean;
  children: ClientDocNode[];
};

export type ClientDocNode = ClientDocFile | ClientDocFolder;

export type ClientDocTree = {
  hasRootReadme: boolean;
  rootFiles: ClientDocFile[];
  folders: ClientDocFolder[];
};

function toClientFile(f: DocFile): ClientDocFile {
  return { kind: "file", slug: f.slug, title: f.title };
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
