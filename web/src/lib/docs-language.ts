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
