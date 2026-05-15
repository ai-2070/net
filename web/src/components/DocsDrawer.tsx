"use client";

import { useEffect, useState } from "react";
import { usePathname } from "next/navigation";
import { DocsSidebar } from "@/components/DocsSidebar";
import type { ClientDocTree } from "@/lib/docs";

// Mobile + tablet docs nav. Renders a sticky toggle bar below the main
// NavBar and a slide-in drawer panel. Hidden at `lg` and above (the
// layout shows the sidebar inline at that breakpoint).
export function DocsDrawer({ tree }: { tree: ClientDocTree }) {
  const [open, setOpen] = useState(false);
  const pathname = usePathname() ?? "/docs";

  // Close on route change so navigating from inside the drawer dismisses it.
  useEffect(() => {
    setOpen(false);
  }, [pathname]);

  // ESC to close.
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent): void => {
      if (e.key === "Escape") setOpen(false);
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [open]);

  // Lock body scroll while the drawer is open.
  useEffect(() => {
    if (!open) return;
    const prev = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    return () => {
      document.body.style.overflow = prev;
    };
  }, [open]);

  // The "current section" hint in the toggle bar.
  const crumb = pathname
    .replace(/^\/docs\/?/, "")
    .split("/")
    .filter(Boolean);
  const here =
    crumb.length === 0
      ? "index"
      : crumb[crumb.length - 1]!.replace(/-/g, " ");

  return (
    <>
      {/* Toggle bar — sticky just below the global NavBar, hidden at lg+. */}
      <div className="lg:hidden sticky top-20 z-40 bg-bg/95 backdrop-blur border-b border-line">
        <button
          type="button"
          onClick={() => setOpen(true)}
          aria-label="Open docs navigation"
          aria-expanded={open}
          aria-controls="docs-drawer"
          className="flex items-center gap-3 w-full px-6 py-3 font-mono text-[11px] tracking-[0.18em] uppercase text-ink-dim hover:text-ink transition-colors"
        >
          <span className="text-accent">§</span>
          <span>docs</span>
          <span className="text-ink-faint normal-case tracking-normal truncate">
            / {here}
          </span>
          <span className="ml-auto flex items-center gap-1 text-[10px] tracking-[0.14em] text-accent">
            <span aria-hidden>≡</span> nav
          </span>
        </button>
      </div>

      {/* Drawer overlay (backdrop + panel). Always rendered so the slide
          animation has something to transition; pointer-events gated by open. */}
      <div
        id="docs-drawer"
        role="dialog"
        aria-modal="true"
        aria-label="Docs navigation"
        className={`lg:hidden fixed inset-0 z-[100] ${
          open ? "pointer-events-auto" : "pointer-events-none"
        }`}
      >
        {/* Backdrop */}
        <button
          type="button"
          tabIndex={open ? 0 : -1}
          aria-label="Close docs navigation"
          onClick={() => setOpen(false)}
          className={`absolute inset-0 bg-bg/80 backdrop-blur-sm transition-opacity duration-200 ${
            open ? "opacity-100" : "opacity-0"
          }`}
        />
        {/* Drawer panel */}
        <aside
          className={`absolute top-0 left-0 bottom-0 w-[320px] max-w-[88vw] bg-bg border-r border-line shadow-2xl flex flex-col transition-transform duration-200 ease-out ${
            open ? "translate-x-0" : "-translate-x-full"
          }`}
        >
          <div className="flex items-center justify-between border-b border-line px-5 py-3 shrink-0">
            <span className="font-mono text-[10px] tracking-[0.22em] uppercase text-accent flex items-center gap-2">
              <span className="inline-flex gap-1">
                <span className="frame-dot-r w-[6px] h-[6px] rounded-full" />
                <span className="frame-dot-y w-[6px] h-[6px] rounded-full" />
                <span className="frame-dot-g w-[6px] h-[6px] rounded-full" />
              </span>
              docs.nav
            </span>
            <button
              type="button"
              onClick={() => setOpen(false)}
              aria-label="Close"
              className="font-mono text-ink-dim hover:text-accent transition-colors text-[18px] leading-none px-2 -mr-2"
            >
              ×
            </button>
          </div>
          <div className="overflow-y-auto px-5 py-5 grow">
            <DocsSidebar tree={tree} chrome={false} />
          </div>
        </aside>
      </div>
    </>
  );
}
