import type { DocIndex, DocLink } from "@/lib/doc-index";
import { resolveDocLink } from "@/lib/doc-index";
import type { KeywordLink } from "@/lib/keyword-links";

// Renders curated keywords as links inside docs prose, at render time.
//
// The source markdown is untouched: this walks the parsed tree and wraps the
// first occurrence of each keyword in a link to the page the map names. That is
// why it is a remark plugin and not a source transform — only the parser knows
// what is prose and what is code, so `` `RedEX` `` inside a fence (or an inline
// span) is never rewritten, and neither is text already inside a link.
//
// No mdast/unist dependency: the shape it needs is four fields, so it walks
// plain objects rather than pulling visitors in to declare a walker this small.

type MdastNode = {
  type: string;
  value?: string;
  url?: string;
  title?: string | null;
  children?: MdastNode[];
};

// Descendants that are not prose: code (literal by definition), links (a link
// inside a link is invalid), and frontmatter/definitions (not rendered).
const OPAQUE: Record<string, true> = {
  code: true,
  inlineCode: true,
  html: true,
  link: true,
  linkReference: true,
  image: true,
  imageReference: true,
  definition: true,
  yaml: true,
  toml: true,
  // Headings are skipped too, not because text is literal but because a link in
  // a heading lands inside the anchor chrome and leaks into the TOC label.
  // Headings are the page's own structure; prose is where a cross-reference
  // belongs.
  heading: true,
};

export type RemarkKeywordLinksOptions = {
  index: DocIndex;
  keywords: readonly KeywordLink[];
  /** Route of the page being rendered; a keyword pointing at it is skipped so
   *  a page never links to itself. */
  currentPath?: string;
};

type Linker = {
  pattern: RegExp;
  byTerm: Map<string, DocLink>;
  used: Set<string>;
  currentPath?: string;
};

export default function remarkKeywordLinks({
  index,
  keywords,
  currentPath,
}: RemarkKeywordLinksOptions) {
  // Resolve once per page. A term whose slug does not exist is dropped here
  // rather than rendered as a dead link; `assertKeywordLinksResolve` in
  // `lib/docs.ts` is what makes that a build failure instead of a silent gap.
  //
  // Keyed by lowercased term: matching is case-insensitive (so `Redex`,
  // `REDEX`, and `redex` all hit `RedEX`), but the link text is the match as
  // written, so the map stays one entry per term rather than one per casing.
  const byTerm = new Map<string, DocLink>();
  for (const { term, slug } of keywords) {
    const link = resolveDocLink(index, slug);
    if (link) byTerm.set(term.toLowerCase(), link);
  }
  const terms = [...byTerm.keys()].sort((a, b) => b.length - a.length);
  if (terms.length === 0) return () => {};

  // Word-ish boundaries so `RedEX` does not match inside `RedEXFile`, and
  // `MCP` does not match inside `MCPs`. Terms may contain punctuation
  // (`agent-to-agent`, `capability schema`) — only the edges are anchored.
  const pattern = new RegExp(
    `(?<![A-Za-z0-9_])(?:${terms.map(escapeRegExp).join("|")})(?![A-Za-z0-9_])`,
    "gi",
  );

  const linker: Linker = { pattern, byTerm, used: new Set(), currentPath };

  return (tree: MdastNode): void => {
    rewrite(tree, linker);
  };
}

function escapeRegExp(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function rewrite(node: MdastNode, linker: Linker): void {
  const children = node.children;
  if (!children || OPAQUE[node.type]) return;

  const next: MdastNode[] = [];
  for (const child of children) {
    if (child.type === "text" && typeof child.value === "string") {
      pushText(child.value, linker, next);
    } else {
      rewrite(child, linker);
      next.push(child);
    }
  }
  node.children = next;
}

function pushText(text: string, linker: Linker, out: MdastNode[]): void {
  const { pattern, byTerm, used, currentPath } = linker;
  pattern.lastIndex = 0;

  let last = 0;
  let match: RegExpExecArray | null;
  while ((match = pattern.exec(text)) !== null) {
    // `matched` is the spelling in the prose; `key` is the map's casing-free
    // key. The link text keeps the prose spelling.
    const matched = match[0];
    const key = matched.toLowerCase();
    const link = byTerm.get(key);
    // Unreachable via the alternation, but keeps the narrowing honest.
    if (!link) continue;
    // First occurrence only, and never a self-link. Skipping leaves the bytes
    // in the trailing slice below — a later occurrence of the same term is not
    // retried, because the term is used or the page is itself.
    if (used.has(key) || link.page.url === currentPath) continue;

    if (match.index > last) {
      out.push({ type: "text", value: text.slice(last, match.index) });
    }
    out.push({
      type: "link",
      url: link.href,
      title: `${link.page.title} · Net docs`,
      children: [{ type: "text", value: matched }],
    });
    used.add(key);
    last = match.index + matched.length;
  }

  if (last === 0) out.push({ type: "text", value: text });
  else if (last < text.length) out.push({ type: "text", value: text.slice(last) });
}
