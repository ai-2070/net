import "server-only";
import { readFileSync } from "node:fs";
import GithubSlugger from "github-slugger";
import {
  getDocTree,
  type DocFile,
  type DocFolder,
} from "@/lib/docs";

// One contiguous chunk of a doc, anchored at an h2 (or the page intro
// before the first h2). The client scores each block against the query
// and renders a result that links to `<page>#<heading-id>`.
export type SearchBlock = {
  /** Heading text; omitted for the intro block before the first h2. */
  heading?: string;
  /** Heading anchor id; matches the rehype-slug ID rendered on the page. */
  headingId?: string;
  /** Body text under this heading, formatting + code fences stripped. */
  text: string;
};

export type SearchEntry = {
  slug: string[];
  title: string;
  /** Parent section label, mirroring `LinearDoc.section`. */
  section?: string;
  blocks: SearchBlock[];
};

export type SearchIndex = SearchEntry[];

// Strip the inline markdown features that would otherwise turn into noise
// in the search corpus. The same rules `extractToc` uses.
function stripInline(s: string): string {
  return s
    .replace(/\*\*(.*?)\*\*/g, "$1")
    .replace(/__(.*?)__/g, "$1")
    .replace(/\*([^*]+)\*/g, "$1")
    .replace(/_([^_]+)_/g, "$1")
    .replace(/`([^`]+)`/g, "$1")
    .replace(/\[([^\]]+)\]\([^)]+\)/g, "$1")
    .replace(/<[^>]+>/g, "");
}

function collapseSpace(s: string): string {
  return s.replace(/\s+/g, " ").trim();
}

// Walk the markdown line-by-line, collecting text under each h2 into its own
// block. h3 / h4 headings are folded into the parent h2's body (they're still
// queryable but click-through always lands on the enclosing h2). Code fences
// are skipped — indexing identifiers is noisier than it's worth.
function extractBlocks(source: string): SearchBlock[] {
  const slugger = new GithubSlugger();
  const blocks: SearchBlock[] = [];
  let current: SearchBlock = { text: "" };
  let inFence = false;
  let fenceChar = "";

  for (const line of source.split("\n")) {
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

    const m = /^(#{1,4})\s+(.+?)\s*#*\s*$/.exec(line);
    if (m) {
      const level = m[1]!.length;
      const text = stripInline(m[2]!.trim());
      if (level === 1) continue; // h1 is the page title, captured separately
      if (level === 2) {
        if (current.text.trim() || current.heading) {
          blocks.push({ ...current, text: collapseSpace(current.text) });
        }
        current = { heading: text, headingId: slugger.slug(text), text: "" };
        continue;
      }
      // h3 / h4 — fold the heading text into the current block as plain content
      current.text += ` ${text}`;
      continue;
    }

    current.text += ` ${stripInline(line)}`;
  }

  if (current.text.trim() || current.heading) {
    blocks.push({ ...current, text: collapseSpace(current.text) });
  }
  return blocks;
}

function pageTitle(source: string, fallback: string): string {
  const m = /^#\s+(.+?)\s*#*\s*$/m.exec(source);
  return m ? stripInline(m[1]!.trim()) : fallback;
}

function readFileText(file: DocFile): string {
  try {
    return readFileSync(file.filePath, "utf8");
  } catch {
    return "";
  }
}

function visitFolder(
  folder: DocFolder,
  out: SearchEntry[],
  parentSection: string | undefined,
): void {
  if (folder.readme) {
    const source = readFileText(folder.readme);
    out.push({
      slug: folder.slug,
      title: pageTitle(source, folder.title),
      section: parentSection,
      blocks: extractBlocks(source),
    });
  }
  for (const child of folder.children) {
    if (child.kind === "file") {
      if (folder.readme && child === folder.readme) continue;
      const source = readFileText(child);
      out.push({
        slug: child.slug,
        title: pageTitle(source, child.title),
        section: folder.title,
        blocks: extractBlocks(source),
      });
    } else {
      visitFolder(child, out, folder.title);
    }
  }
}

// Builds the full search index from the same tree the sidebar walks, so
// the index can never include a doc the sidebar doesn't (and vice versa).
// Called from the static `/api/search-index` route handler; result is baked
// at build time.
export function buildSearchIndex(): SearchIndex {
  const tree = getDocTree();
  const out: SearchEntry[] = [];

  if (tree.rootReadme) {
    const source = readFileText(tree.rootReadme);
    out.push({
      slug: [],
      title: pageTitle(source, tree.rootReadme.title),
      blocks: extractBlocks(source),
    });
  }
  for (const f of tree.rootFiles) {
    const source = readFileText(f);
    out.push({
      slug: f.slug,
      title: pageTitle(source, f.title),
      blocks: extractBlocks(source),
    });
  }
  for (const folder of tree.folders) {
    visitFolder(folder, out, undefined);
  }
  return out;
}
