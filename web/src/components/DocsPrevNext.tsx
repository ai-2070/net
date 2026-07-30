"use client";

import Link from "next/link";
import type { LinearDoc } from "@/lib/docs";
import { DEFAULT_LANGUAGE, type Language } from "@/lib/docs-language";
import { useLanguageStore } from "@/store/useLanguageStore";

/** Neighbours for every language, baked at build time. */
export type NeighboursByLanguage = Record<
  Language,
  { prev: LinearDoc | null; next: LinearDoc | null }
>;

/** The reader's neighbours.
 *
 * Gated on `hydrated` so the server-rendered HTML and the client's first paint
 * agree on DEFAULT_LANGUAGE — the same discipline the switcher uses. Without it,
 * a Python reader gets a hydration mismatch on every page.
 */
function useNeighbours(all: NeighboursByLanguage) {
  const language = useLanguageStore((s) => s.language);
  const hydrated = useLanguageStore((s) => s.hydrated);
  return all[hydrated ? language : DEFAULT_LANGUAGE];
}

function hrefFor(slug: string[]): string {
  return slug.length === 0 ? "/docs" : `/docs/${slug.join("/")}`;
}

// Compact top bar — single line, just arrow + abbreviated title each side.
// Sits below the breadcrumb so readers know where they came from / where
// they're headed without taking up much vertical space.
export function DocsPrevNextTop({
  neighbours,
}: {
  neighbours: NeighboursByLanguage;
}) {
  const { prev, next } = useNeighbours(neighbours);
  if (!prev && !next) return null;
  return (
    <div className="flex items-center justify-between gap-4 mb-8 font-mono text-[11px] tracking-[0.02em]">
      {prev ? (
        <Link
          href={hrefFor(prev.slug)}
          className="group flex items-center gap-2 text-ink-dim hover:text-accent transition-colors min-w-0"
        >
          <span aria-hidden className="text-accent shrink-0">
            ←
          </span>
          <span className="truncate">{prev.title}</span>
        </Link>
      ) : (
        <span />
      )}
      {next ? (
        <Link
          href={hrefFor(next.slug)}
          className="group flex items-center gap-2 text-ink-dim hover:text-accent transition-colors min-w-0 justify-end"
        >
          <span className="truncate">{next.title}</span>
          <span aria-hidden className="text-accent shrink-0">
            →
          </span>
        </Link>
      ) : (
        <span />
      )}
    </div>
  );
}

// Bottom cards — full surface with section context + title, more prominent
// after a long read. Lights the border accent on hover.
export function DocsPrevNextBottom({
  neighbours,
}: {
  neighbours: NeighboursByLanguage;
}) {
  const { prev, next } = useNeighbours(neighbours);
  if (!prev && !next) return null;
  return (
    <div className="mt-14 pt-8 border-t border-line grid grid-cols-1 sm:grid-cols-2 gap-4">
      {prev ? (
        <Link
          href={hrefFor(prev.slug)}
          className="group flex flex-col border border-line bg-bg-2/30 px-4 py-4 transition-colors hover:border-accent-dim hover:bg-bg-2/60"
        >
          <div className="flex items-center gap-2 font-mono text-[10px] tracking-[0.18em] uppercase text-ink-faint group-hover:text-accent mb-1.5 transition-colors">
            <span aria-hidden>←</span> previous
          </div>
          {/* `mt-auto` pushes section + title to the bottom of the card so
              titles line up across prev / next even when one card has a
              section label and the other doesn't. */}
          <div className="mt-auto">
            {prev.section ? (
              <div className="font-mono text-[10px] text-ink-dim tracking-[0.06em] mb-1">
                {prev.section}
              </div>
            ) : null}
            <div className="font-mono text-[14px] text-ink group-hover:text-accent transition-colors font-semibold leading-snug truncate">
              {prev.title}
            </div>
          </div>
        </Link>
      ) : (
        <div className="hidden sm:block" />
      )}
      {next ? (
        <Link
          href={hrefFor(next.slug)}
          className="group flex flex-col border border-line bg-bg-2/30 px-4 py-4 transition-colors hover:border-accent-dim hover:bg-bg-2/60 sm:text-right"
        >
          <div className="flex items-center gap-2 font-mono text-[10px] tracking-[0.18em] uppercase text-ink-faint group-hover:text-accent mb-1.5 transition-colors sm:justify-end">
            next <span aria-hidden>→</span>
          </div>
          <div className="mt-auto">
            {next.section ? (
              <div className="font-mono text-[10px] text-ink-dim tracking-[0.06em] mb-1">
                {next.section}
              </div>
            ) : null}
            <div className="font-mono text-[14px] text-ink group-hover:text-accent transition-colors font-semibold leading-snug truncate">
              {next.title}
            </div>
          </div>
        </Link>
      ) : (
        <div className="hidden sm:block" />
      )}
    </div>
  );
}
