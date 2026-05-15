"use client";

import { useEffect, useState } from "react";
import type { TocEntry } from "@/lib/docs";

// "On this page" right-rail. Server-extracted heading list comes in via
// `entries`; this client island runs an IntersectionObserver to highlight
// the section currently nearest the top of the viewport as the user scrolls.
export function DocsToc({ entries }: { entries: readonly TocEntry[] }) {
  const [activeId, setActiveId] = useState<string | null>(null);

  useEffect(() => {
    if (entries.length === 0) return;

    const headings: HTMLElement[] = [];
    for (const e of entries) {
      const el = document.getElementById(e.id);
      if (el) headings.push(el);
    }
    if (headings.length === 0) return;

    // Track which headings are currently in the "active band" near the top
    // of the viewport. Pick the first (top-most in source order) that is.
    const intersecting = new Set<string>();
    const observer = new IntersectionObserver(
      (records) => {
        for (const r of records) {
          if (r.isIntersecting) intersecting.add(r.target.id);
          else intersecting.delete(r.target.id);
        }
        for (const e of entries) {
          if (intersecting.has(e.id)) {
            setActiveId(e.id);
            return;
          }
        }
      },
      {
        // Top band: from ~100px below the viewport top to ~35% from the top.
        // Headings within this band count as "active".
        rootMargin: "-100px 0px -65% 0px",
        threshold: 0,
      },
    );

    for (const h of headings) observer.observe(h);
    return () => observer.disconnect();
  }, [entries]);

  if (entries.length === 0) return null;

  return (
    <nav className="font-mono" aria-label="On this page">
      <div className="text-[10px] tracking-[0.18em] uppercase text-ink-dim mb-3 font-mono flex items-center gap-2">
        <span className="text-accent">▸</span> on this page
      </div>
      <ul className="border-l border-line">
        {entries.map((e) => {
          const active = activeId === e.id;
          const depth = Math.max(0, e.level - 2);
          return (
            <li key={`${e.id}-${e.level}`}>
              <a
                href={`#${e.id}`}
                className={`block text-[11px] leading-[1.5] py-[3px] pr-2 -ml-px border-l transition-colors truncate ${
                  active
                    ? "border-accent text-accent"
                    : "border-transparent text-ink-dim hover:text-ink hover:border-accent-dim"
                }`}
                style={{ paddingLeft: `${12 + depth * 12}px` }}
              >
                {e.title}
              </a>
            </li>
          );
        })}
      </ul>
    </nav>
  );
}
