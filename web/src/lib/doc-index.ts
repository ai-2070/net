// A slug → page index over the docs tree, used by the keyword auto-linker.
//
// One job: turn a docs slug (`concepts/capabilities`) into the URL the page is
// served at plus its title. `lib/docs.ts` builds the index from the same tree
// the sidebar walks, so a keyword cannot point at a page the nav does not have.
//
// This module is pure — no `fs`, no `server-only` — because the build-time
// assertion in `lib/docs.ts` and the renderer (`remark-keyword-links`) both use
// it, and a second implementation of "which URL is this slug" is how this kind
// of feature rots.

/** One addressable page: its slug path, its route, its display title.
 *
 * Renditions are NOT pages in this sense. A language reading of an adaptive
 * page (`sdk/python/announce`) shares the universal body of the page it
 * projects, so a keyword always points at the neutral URL (`sdk/announce`) and
 * the reader chooses a language there. */
export type DocPage = {
  /** Slug path without the `/docs` prefix, `/`-joined. `""` for the root page. */
  slug: string;
  /** Route path, always beginning with `/docs`. */
  url: string;
  /** Display title, used for the link's tooltip. */
  title: string;
};

export type DocIndex = {
  bySlug: Map<string, DocPage>;
};

export type DocLink = { href: string; page: DocPage };

/** Lowercase and normalize `_`/`-` the same way `lib/docs.ts` builds slugs, so
 *  a map authored as `concepts/capabilities` or `concepts_capabilities` names
 *  the same page. */
export function normalizeDocSlug(raw: string): string {
  return raw
    .trim()
    .replace(/^\/?docs\//i, "")
    .replace(/^\/+|\/+$/g, "")
    .split("/")
    .filter((s) => s.length > 0)
    .map((s) => s.toLowerCase().replace(/[_-]+/g, "-"))
    .join("/");
}

export function buildDocIndex(pages: readonly DocPage[]): DocIndex {
  const bySlug = new Map<string, DocPage>();
  for (const page of pages) {
    if (!bySlug.has(page.slug)) bySlug.set(page.slug, page);
  }
  return { bySlug };
}

/** Resolve a slug to its page, optionally with a heading fragment. Null when
 *  the tree has no such page — the build assertion turns that into a failure so
 *  it cannot ship as a dead keyword. */
export function resolveDocLink(
  index: DocIndex,
  slug: string,
  anchor?: string,
): DocLink | null {
  const page = index.bySlug.get(normalizeDocSlug(slug));
  if (!page) return null;
  return { href: anchor ? `${page.url}#${anchor}` : page.url, page };
}
