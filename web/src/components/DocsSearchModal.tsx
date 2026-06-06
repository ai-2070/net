"use client";

import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import type Fuse from "fuse.js";
import type { FuseResultMatch, IFuseOptions } from "fuse.js";
import { useRouter } from "next/navigation";

// Mirror of `SearchEntry` / `SearchBlock` from `@/lib/search-index` — declared
// here to avoid pulling the server-only module into the client bundle.
type SearchBlock = {
  heading?: string;
  headingId?: string;
  text: string;
};
type SearchEntry = {
  slug: string[];
  title: string;
  section?: string;
  blocks: SearchBlock[];
};
type SearchIndex = SearchEntry[];

// Flat per-block shape Fuse indexes. One row per (page, h2) — the page intro
// becomes a row with no heading. Title is duplicated across every row of a
// page so a strong title match ranks every block above weaker peers.
type FuseItem = {
  slug: string[];
  title: string;
  section?: string;
  heading?: string;
  headingId?: string;
  text: string;
};

type Result = {
  slug: string[];
  title: string;
  section?: string;
  heading?: string;
  headingId?: string;
  snippet: string;
};

const MAX_RESULTS = 20;
// Soft cap so a title-only match doesn't flood the result list with every
// block of the matched page. Two is enough that strong section-level matches
// still surface alongside the page-top result.
const PER_PAGE_CAP = 2;
const SNIPPET_LEN = 140;

// Fuse tuning. Weights mirror the title > heading > body intuition the
// hand-rolled scorer used; `ignoreLocation` means a match deep in a long
// body block isn't penalized for being far from the start; `threshold` 0.3
// is the conventional starting point — strict enough that random tokens
// don't bubble up, loose enough that one-character typos still hit.
const FUSE_OPTIONS: IFuseOptions<FuseItem> = {
  keys: [
    { name: "title", weight: 3 },
    { name: "heading", weight: 2 },
    { name: "text", weight: 1 },
  ],
  threshold: 0.3,
  ignoreLocation: true,
  includeMatches: true,
  includeScore: true,
  minMatchCharLength: 2,
};

function hrefFor(slug: string[], headingId?: string): string {
  const base = slug.length === 0 ? "/docs" : `/docs/${slug.join("/")}`;
  return headingId ? `${base}#${headingId}` : base;
}

function flatten(index: SearchIndex): FuseItem[] {
  const out: FuseItem[] = [];
  for (const entry of index) {
    if (entry.blocks.length === 0) {
      out.push({
        slug: entry.slug,
        title: entry.title,
        section: entry.section,
        text: "",
      });
      continue;
    }
    for (const b of entry.blocks) {
      out.push({
        slug: entry.slug,
        title: entry.title,
        section: entry.section,
        heading: b.heading,
        headingId: b.headingId,
        text: b.text,
      });
    }
  }
  return out;
}

// Center a ~140-char window on the first body-text match Fuse reported.
// Falls back to the head of the text if Fuse only matched title/heading
// (or if the text is shorter than the snippet length).
function snippetFromMatches(
  text: string,
  matches: ReadonlyArray<FuseResultMatch> | undefined,
  len: number,
): string {
  if (!text) return "";
  const textMatch = matches?.find((m) => m.key === "text");
  const firstIdx =
    textMatch && textMatch.indices.length > 0
      ? textMatch.indices[0]![0]
      : -1;
  if (firstIdx < 0) {
    return text.slice(0, len) + (text.length > len ? "…" : "");
  }
  const start = Math.max(0, firstIdx - 40);
  const end = Math.min(text.length, start + len);
  const prefix = start > 0 ? "…" : "";
  const suffix = end < text.length ? "…" : "";
  return prefix + text.slice(start, end) + suffix;
}

function runSearch(fuse: Fuse<FuseItem>, query: string): Result[] {
  const q = query.trim();
  if (q.length < 2) return [];
  const raw = fuse.search(q);
  const perPage = new Map<string, number>();
  const out: Result[] = [];
  for (const r of raw) {
    const key = r.item.slug.join("/");
    const seen = perPage.get(key) ?? 0;
    if (seen >= PER_PAGE_CAP) continue;
    perPage.set(key, seen + 1);
    out.push({
      slug: r.item.slug,
      title: r.item.title,
      section: r.item.section,
      heading: r.item.heading,
      headingId: r.item.headingId,
      snippet: snippetFromMatches(r.item.text, r.matches, SNIPPET_LEN),
    });
    if (out.length >= MAX_RESULTS) break;
  }
  return out;
}

export function DocsSearchModal() {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [fuse, setFuse] = useState<Fuse<FuseItem> | null>(null);
  const [loading, setLoading] = useState(false);
  const [selected, setSelected] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const router = useRouter();

  // Lazy-load both the index and Fuse on first open. Fuse comes via
  // `import("fuse.js")` so its ~10 KB minified payload doesn't ship with
  // the docs layout chunk — it's only fetched when a user hits `/` for
  // the first time.
  //
  // `loading` is NOT in the dep array and NOT used as a guard: it's set
  // inside the effect and would otherwise re-trigger the effect when it
  // flips to true. React would then fire the first run's cleanup
  // (`cancelled = true`), and the in-flight `await` would no-op out — so
  // `setLoading(false)` never runs and the modal sticks at "Loading…".
  // `!open || fuse` is sufficient: `fuse` only goes truthy after a
  // successful fetch, so a single in-flight load per open cycle is the
  // most we can have.
  useEffect(() => {
    if (!open || fuse) return;
    let cancelled = false;
    setLoading(true);
    (async () => {
      try {
        const [indexData, FuseModule] = await Promise.all([
          fetch("/api/search-index").then((r) =>
            r.ok
              ? (r.json() as Promise<SearchIndex>)
              : Promise.reject(r.status),
          ),
          import("fuse.js"),
        ]);
        if (cancelled) return;
        setFuse(new FuseModule.default(flatten(indexData), FUSE_OPTIONS));
      } catch {
        // Best-effort fallback so the modal shows "No results" instead
        // of staying stuck. If Fuse itself can't be loaded either, the
        // user gets no search — no recovery from here.
        try {
          const FuseModule = await import("fuse.js");
          if (!cancelled) setFuse(new FuseModule.default([], FUSE_OPTIONS));
        } catch {
          /* swallow */
        }
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [open, fuse]);

  // Global `/` opens the modal. Skipped while focus is in any text-entry
  // field (so typing a literal `/` works) and while modifiers are held.
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.key !== "/") return;
      if (e.ctrlKey || e.metaKey || e.altKey) return;
      const t = e.target as HTMLElement | null;
      if (
        t &&
        (t.tagName === "INPUT" ||
          t.tagName === "TEXTAREA" ||
          t.isContentEditable)
      ) {
        return;
      }
      e.preventDefault();
      setOpen(true);
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // Focus the input on open; reset state on close.
  useEffect(() => {
    if (open) {
      const id = window.setTimeout(() => inputRef.current?.focus(), 0);
      return () => window.clearTimeout(id);
    }
    setQuery("");
    setSelected(0);
  }, [open]);

  // Lock body scroll while the modal is open.
  useEffect(() => {
    if (!open) return;
    const prev = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    return () => {
      document.body.style.overflow = prev;
    };
  }, [open]);

  useEffect(() => {
    setSelected(0);
  }, [query]);

  const results = useMemo(() => {
    if (!fuse) return [];
    return runSearch(fuse, query);
  }, [fuse, query]);

  // Keep the selected row visible while arrow-keying through a long list.
  useEffect(() => {
    if (!listRef.current) return;
    const el = listRef.current.querySelector<HTMLElement>(
      `[data-result-index="${selected}"]`,
    );
    el?.scrollIntoView({ block: "nearest" });
  }, [selected, results.length]);

  const close = useCallback(() => setOpen(false), []);

  const navigate = useCallback(
    (href: string) => {
      setOpen(false);
      router.push(href);
    },
    [router],
  );

  function onInputKeyDown(e: React.KeyboardEvent<HTMLInputElement>) {
    if (e.key === "Escape") {
      e.preventDefault();
      close();
      return;
    }
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setSelected((s) => Math.min(results.length - 1, s + 1));
      return;
    }
    if (e.key === "ArrowUp") {
      e.preventDefault();
      setSelected((s) => Math.max(0, s - 1));
      return;
    }
    if (e.key === "Enter" && results[selected]) {
      e.preventDefault();
      const r = results[selected];
      navigate(hrefFor(r.slug, r.headingId));
    }
  }

  if (!open) return null;

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-label="Search docs"
      className="fixed inset-0 z-[200] flex items-start justify-center pt-[10vh] px-4"
    >
      <button
        type="button"
        aria-label="Close search"
        onClick={close}
        tabIndex={-1}
        className="absolute inset-0 bg-bg/80 backdrop-blur-sm cursor-default"
      />
      <div className="relative w-full max-w-[640px] border border-line bg-bg-2 shadow-2xl flex flex-col max-h-[80vh]">
        <div className="border-b border-line px-3 py-2 flex items-center gap-2 shrink-0">
          <span
            aria-hidden
            className="font-mono text-[12px] text-accent shrink-0"
          >
            /
          </span>
          <input
            ref={inputRef}
            type="search"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={onInputKeyDown}
            placeholder="Search docs"
            spellCheck={false}
            autoComplete="off"
            className="flex-1 bg-transparent outline-none text-ink placeholder:text-ink-faint font-mono text-[14px]"
          />
          <span className="font-mono text-[9px] tracking-[0.18em] uppercase text-ink-faint shrink-0">
            ESC
          </span>
        </div>

        <div ref={listRef} className="overflow-y-auto grow">
          {query.length < 2 && (
            <div className="px-3 py-4 font-mono text-[11px] text-ink-faint">
              Type at least two characters.
            </div>
          )}
          {query.length >= 2 && loading && !fuse && (
            <div className="px-3 py-4 font-mono text-[11px] text-ink-faint">
              Loading index…
            </div>
          )}
          {query.length >= 2 && fuse && results.length === 0 && (
            <div className="px-3 py-4 font-mono text-[11px] text-ink-faint">
              No results for{" "}
              <span className="text-ink">&quot;{query}&quot;</span>.
            </div>
          )}
          {results.map((r, i) => {
            const on = i === selected;
            return (
              <button
                key={`${r.slug.join("/")}#${r.headingId ?? ""}-${i}`}
                type="button"
                data-result-index={i}
                onMouseEnter={() => setSelected(i)}
                onClick={() => navigate(hrefFor(r.slug, r.headingId))}
                className={`w-full text-left block border-b border-line px-3 py-2.5 transition-colors ${
                  on ? "bg-accent/[0.08]" : "hover:bg-bg-2/60"
                }`}
              >
                <div className="font-mono text-[10px] text-ink-faint tracking-[0.06em] mb-0.5 truncate">
                  {r.section ? `${r.section} · ` : ""}
                  {r.title}
                </div>
                <div
                  className={`font-mono text-[13px] leading-snug truncate ${
                    on ? "text-accent" : "text-ink"
                  }`}
                >
                  {r.heading ?? r.title}
                </div>
                {r.snippet ? (
                  <div className="text-[12px] text-ink-dim mt-0.5 line-clamp-2">
                    {r.snippet}
                  </div>
                ) : null}
              </button>
            );
          })}
        </div>

        <div className="border-t border-line px-3 py-1.5 flex items-center justify-between font-mono text-[9px] tracking-[0.18em] uppercase text-ink-faint shrink-0">
          <span>
            <span className="text-accent-dim">↑↓</span> nav
            <span className="ml-3 text-accent-dim">↵</span> open
            <span className="ml-3 text-accent-dim">esc</span> close
          </span>
          <span className="text-accent-dim tabular-nums">
            {results.length > 0
              ? `${String(results.length).padStart(2, "0")} hits`
              : ""}
          </span>
        </div>
      </div>
    </div>
  );
}
