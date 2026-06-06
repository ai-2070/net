"use client";

import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
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

type Result = {
  slug: string[];
  title: string;
  section?: string;
  heading?: string;
  headingId?: string;
  snippet: string;
  score: number;
};

const MAX_RESULTS = 20;
const SNIPPET_LEN = 140;

function hrefFor(slug: string[], headingId?: string): string {
  const base = slug.length === 0 ? "/docs" : `/docs/${slug.join("/")}`;
  return headingId ? `${base}#${headingId}` : base;
}

// Extract a ~140-char window around the first matching token. Falls back to
// the head of the text when no token matches (rare — only fires for entries
// surfaced purely on title score).
function extractSnippet(text: string, tokens: string[], len: number): string {
  if (!text) return "";
  const lower = text.toLowerCase();
  let pos = -1;
  for (const t of tokens) {
    const i = lower.indexOf(t);
    if (i >= 0 && (pos < 0 || i < pos)) pos = i;
  }
  if (pos < 0) {
    return text.slice(0, len) + (text.length > len ? "…" : "");
  }
  const start = Math.max(0, pos - 40);
  const end = Math.min(text.length, start + len);
  const prefix = start > 0 ? "…" : "";
  const suffix = end < text.length ? "…" : "";
  return prefix + text.slice(start, end) + suffix;
}

function scoreEntry(entry: SearchEntry, tokens: string[]): Result[] {
  const titleLower = entry.title.toLowerCase();
  let titleScore = 0;
  for (const t of tokens) {
    if (titleLower.includes(t)) titleScore += 10;
  }
  // All-tokens-in-title bonus — promotes the exact-name case ("nrpc" finding
  // the nRPC guide first).
  if (tokens.length > 0 && tokens.every((t) => titleLower.includes(t))) {
    titleScore += 5;
  }

  const out: Result[] = [];
  for (const block of entry.blocks) {
    const headingLower = (block.heading ?? "").toLowerCase();
    const textLower = block.text.toLowerCase();
    let score = 0;
    for (const t of tokens) {
      if (headingLower.includes(t)) score += 5;
      if (textLower.includes(t)) score += 1;
    }
    if (score === 0) continue;
    out.push({
      slug: entry.slug,
      title: entry.title,
      section: entry.section,
      heading: block.heading,
      headingId: block.headingId,
      snippet: extractSnippet(block.text, tokens, SNIPPET_LEN),
      score: score + titleScore,
    });
  }

  // Title-only fallback: if no block matched but the title did, still emit a
  // page-level result so the user lands at the page top.
  if (out.length === 0 && titleScore > 0) {
    out.push({
      slug: entry.slug,
      title: entry.title,
      section: entry.section,
      snippet: "",
      score: titleScore,
    });
  }
  return out;
}

function search(index: SearchIndex, query: string): Result[] {
  const q = query.trim().toLowerCase();
  if (q.length < 2) return [];
  const tokens = q.split(/\s+/);
  const all: Result[] = [];
  for (const entry of index) {
    all.push(...scoreEntry(entry, tokens));
  }
  all.sort((a, b) => b.score - a.score);
  return all.slice(0, MAX_RESULTS);
}

export function DocsSearchModal() {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [index, setIndex] = useState<SearchIndex | null>(null);
  const [loading, setLoading] = useState(false);
  const [selected, setSelected] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const router = useRouter();

  // Lazy-fetch the index on first open. Cached for the page lifetime.
  useEffect(() => {
    if (!open || index || loading) return;
    setLoading(true);
    fetch("/api/search-index")
      .then((r) => (r.ok ? r.json() : Promise.reject(r.status)))
      .then((d: SearchIndex) => setIndex(d))
      .catch(() => setIndex([]))
      .finally(() => setLoading(false));
  }, [open, index, loading]);

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

  // Reset highlight when the query changes.
  useEffect(() => {
    setSelected(0);
  }, [query]);

  const results = useMemo(() => {
    if (!index) return [];
    return search(index, query);
  }, [index, query]);

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
      {/* Backdrop — closes on click. Rendered as a button for keyboard reach. */}
      <button
        type="button"
        aria-label="Close search"
        onClick={close}
        tabIndex={-1}
        className="absolute inset-0 bg-bg/80 backdrop-blur-sm cursor-default"
      />
      <div className="relative w-full max-w-[640px] border border-line bg-bg-2 shadow-2xl flex flex-col max-h-[80vh]">
        {/* Input row */}
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

        {/* Results */}
        <div ref={listRef} className="overflow-y-auto grow">
          {query.length < 2 && (
            <div className="px-3 py-4 font-mono text-[11px] text-ink-faint">
              Type at least two characters.
            </div>
          )}
          {query.length >= 2 && loading && !index && (
            <div className="px-3 py-4 font-mono text-[11px] text-ink-faint">
              Loading index…
            </div>
          )}
          {query.length >= 2 && index && results.length === 0 && (
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

        {/* Footer hint row */}
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
