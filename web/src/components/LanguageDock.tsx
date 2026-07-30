"use client";

import { useRouter } from "next/navigation";
import { LANGUAGES, type Language } from "@/lib/docs-language";
import { useLanguageStore } from "@/store/useLanguageStore";

const LABELS: Record<Language, string> = {
  rust: "Rust",
  ts: "TypeScript",
  python: "Python",
  go: "Go",
  c: "C",
};

// At 375px the five full labels overflow the bar and it becomes a horizontal
// scroller — so the first pill, Rust, is the one that scrolls out of view. Only
// TypeScript is long enough to cause it, so only TypeScript gets a short form.
const SHORT_LABELS: Partial<Record<Language, string>> = { ts: "TS" };

/** The persistent language context, as floating chrome.
 *
 * WHY IT FLOATS. The switcher used to live above the sidebar tree, inside an
 * `<aside>` that is `hidden lg:block` — so on mobile it was reachable only after
 * opening the nav drawer, and on desktop it sat above the fold of a scrolling
 * column and left the viewport as soon as a reader started reading. The plan's
 * word for what this should be is "persistent context at every breakpoint", and
 * a control that scrolls away is not that.
 *
 * WHAT A CLICK DOES, AND WHY IT IS TWO THINGS. On a page with per-language
 * readings it NAVIGATES to the reader's language — the URL is the rendition, so
 * changing language changes page. On every other page it sets reader context:
 * the sidebar, the search ranking and prev/next all follow, and the next
 * adaptive page the reader opens will already be theirs.
 *
 * The hrefs come from the page (see `RenditionLinks`) rather than from rewriting
 * the current path, because two URL shapes are live and one has holes — there is
 * no `/docs/sdk/c/announce`, so a pattern rewrite would 404 a C reader on five of
 * seven spine pages.
 *
 * C IS THE FIFTH PILL AND BEHAVES DIFFERENTLY, VISIBLY. Selecting C selects a
 * boundary surface, not a fifth ergonomic SDK. The plan asks for that asymmetry
 * to be visible in the chrome rather than a surprise the reader discovers on
 * arrival, so C is separated by a rule and marked.
 */
export function LanguageDock() {
  const router = useRouter();
  const language = useLanguageStore((s) => s.language);
  const setLanguage = useLanguageStore((s) => s.setLanguage);
  const renditions = useLanguageStore((s) => s.renditions);
  const adaptive = renditions !== null;

  const pick = (l: Language) => {
    setLanguage(l);
    const href = renditions?.[l];
    if (href) router.push(href);
  };

  const pill = (l: Language) => {
    const on = l === language;
    // On an adaptive page a language with no href is a language this page has no
    // reading for. It stays clickable — it still sets context, and the reader
    // should be able to choose it — but it does not pretend to be a destination.
    const routable = !adaptive || Boolean(renditions?.[l]);
    return (
      <button
        key={l}
        type="button"
        role="radio"
        aria-checked={on}
        onClick={() => pick(l)}
        className={`cursor-pointer font-mono text-[11px] tracking-[0.06em] px-2.5 py-1 border transition-colors whitespace-nowrap ${
          on
            ? "border-accent text-accent bg-accent/[0.10]"
            : routable
              ? "border-line text-ink-dim hover:text-ink hover:border-accent-dim"
              : "border-line/50 text-ink-faint hover:text-ink-dim"
        }`}
      >
        {SHORT_LABELS[l] ? (
          <>
            <span className="sm:hidden">{SHORT_LABELS[l]}</span>
            <span className="hidden sm:inline">{LABELS[l]}</span>
          </>
        ) : (
          LABELS[l]
        )}
      </button>
    );
  };

  const lenses = LANGUAGES.filter((l) => l !== "c");

  return (
    <div
      className="fixed inset-x-0 bottom-0 z-40 flex justify-center px-3 pb-3 pointer-events-none"
      // The dock is chrome over content, so it must not eat clicks outside its
      // own box — hence `pointer-events-none` here and `auto` on the bar.
    >
      <div
        className="pointer-events-auto flex items-center gap-1.5 border border-line bg-bg-2/90 backdrop-blur-md px-2.5 py-1.5 max-w-full overflow-x-auto shadow-[0_0_24px_rgba(0,0,0,0.6)]"
        role="radiogroup"
        aria-label="Documentation language"
      >
        <span className="hidden sm:inline font-mono text-[9px] tracking-[0.22em] uppercase text-ink-faint shrink-0 pr-1">
          <span className="text-accent">$</span> lang
        </span>
        {lenses.map(pill)}
        <span className="w-px self-stretch bg-line shrink-0 mx-0.5" aria-hidden />
        {pill("c")}
        <span className="hidden md:inline font-mono text-[9px] tracking-[0.14em] uppercase text-ink-faint shrink-0 pl-0.5">
          boundary
        </span>
      </div>
    </div>
  );
}
