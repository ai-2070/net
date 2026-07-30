// Client-safe taxonomy for the docs language switcher. Lives in its own
// module because `lib/docs.ts` is marked `"server-only"` — anything that
// reaches for `LANGUAGES`, `isLanguage`, etc. from a client component
// would otherwise drag the server-only marker into the browser bundle.
// `lib/docs.ts` re-exports these for the server side so there's still one
// canonical source.

/** Programming-language gating for docs that only make sense for one or
 * more SDK bindings. The set is closed — adding a language means adding
 * it here and updating the switcher UI to render a pill for it. */
export const LANGUAGES = ["rust", "ts", "python", "go", "c"] as const;
export type Language = (typeof LANGUAGES)[number];
export const DEFAULT_LANGUAGE: Language = "rust";

export function isLanguage(s: string | null | undefined): s is Language {
  return (LANGUAGES as readonly string[]).includes(s ?? "");
}

/** URL segment and fragment filename per language.
 *
 * Deliberately not the same string as the `Language` id: the store and the
 * switcher have used `ts` since the SDK spine shipped, but `…/typescript` reads
 * as a language and `…/ts` reads as an abbreviation, and the existing spine
 * folder is already `sdk/typescript`. One mapping, so a URL cannot disagree with
 * a fragment filename. */
export const LENS_SLUG: Record<Language, string> = {
  rust: "rust",
  ts: "typescript",
  python: "python",
  go: "go",
  c: "c",
};

/** The lenses an adaptive page may carry a fragment for.
 *
 * C is absent BY DESIGN and this is the type that enforces it: the C ABI is a
 * boundary surface, not a fifth ergonomic SDK, so `/…/c` is a generated
 * projection rather than an authored rendition. Reader contexts are five;
 * authorable fragment languages are four. */
export const LENSES = ["rust", "ts", "python", "go"] as const;
export type Lens = (typeof LENSES)[number];

export function lensFromSlug(segment: string): Lens | null {
  for (const lens of LENSES) if (LENS_SLUG[lens] === segment) return lens;
  return null;
}

/** True for the C boundary segment — a route that exists but is never authored. */
export function isBoundarySlug(segment: string): boolean {
  return segment === LENS_SLUG.c;
}

/** Returns true if an entry is visible under the current language. An
 * entry with no `languages` field (or an empty array) is universal. */
export function entryVisibleIn(
  entry: { languages?: Language[] },
  current: Language,
): boolean {
  if (!entry.languages || entry.languages.length === 0) return true;
  return entry.languages.includes(current);
}

/** The language a docs slug belongs to, or null when it is universal.
 *
 * Two shapes carry a language: the SDK spine (`sdk/<lang>/…`) and an adaptive
 * page's rendition (`…/<page>/<lens>`). Used to keep search from answering
 * "invoke" with five near-identical hits and no clue which one is the reader's.
 */
export function slugLanguage(slug: readonly string[]): Language | null {
  if (slug.length >= 2 && slug[0] === "sdk") {
    for (const lang of LANGUAGES) {
      if (LENS_SLUG[lang] === slug[1]) return lang;
    }
  }
  const last = slug[slug.length - 1];
  if (last !== undefined && slug.length >= 2) {
    for (const lang of LANGUAGES) {
      if (LENS_SLUG[lang] === last) return lang;
    }
  }
  return null;
}
