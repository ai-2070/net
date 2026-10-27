"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";
import { useEffect, useState } from "react";
import { ArrowDown } from "@phosphor-icons/react";
import globals from "@/lib/globals";
import { BLACKSITE_DOWNLOAD_URL } from "@/components/blacksite/links";

type NavLink = { href: string; label: string };

// Links every page shows on the right: the other products and the docs.
const SITE_LINKS: ReadonlyArray<NavLink> = [
  { href: "/games", label: "GAMES" },
  { href: "/blacksite", label: "BLACKSITE" },
  { href: "/docs", label: "DOCS" },
];

// Each landing page has its own numbered menu (its sections, in page order)
// and its own button. Pages without an entry, like the docs, use Net's.
const PAGE_NAVS: Record<
  string,
  { sections: ReadonlyArray<NavLink>; cta: NavLink }
> = {
  "/": {
    sections: [
      { href: "/#hero", label: "HOME" },
      { href: "/#bench", label: "BENCH" },
      { href: "/#runtime", label: "RUNTIME" },
      { href: "/#dataforts", label: "DATAFORTS" },
      { href: "/#meshos", label: "MESHOS" },
      { href: "/#install", label: "SDKS" },
      { href: "/#apps", label: "APPS" },
      { href: "/#wall", label: "BLACKWALL" },
    ],
    cta: { href: globals.links.install, label: "INSTALL" },
  },
  "/games": {
    sections: [
      { href: "/games#hero", label: "HOME" },
      { href: "/games#speed", label: "SPEED" },
      { href: "/games#demo", label: "PLAY" },
      { href: "/games#fps", label: "FPS" },
      { href: "/games#modes", label: "MODES" },
      { href: "/games#hosts", label: "HOSTS" },
      { href: "/games#code", label: "CODE" },
      { href: "/games#engines", label: "ENGINES" },
    ],
    cta: { href: "/docs/sdk/browser/quickstart", label: "INSTALL" },
  },
  "/blacksite": {
    sections: [
      { href: "/blacksite#hero", label: "HOME" },
      { href: "/blacksite#station", label: "STATION" },
      { href: "/blacksite#brain", label: "BRAIN" },
      { href: "/blacksite#library", label: "LIBRARY" },
      { href: "/blacksite#models", label: "MODELS" },
      { href: "/blacksite#chats", label: "CHATS" },
      { href: "/blacksite#programs", label: "PROGRAMS" },
      { href: "/blacksite#review", label: "REVIEW" },
    ],
    cta: { href: BLACKSITE_DOWNLOAD_URL, label: "DOWNLOAD" },
  },
};

// The section id a link points at on `page`, or null if it goes elsewhere.
const sectionOf = (href: string, page: string): string | null => {
  const [path, id] = href.split("#");
  return id && (path || "/") === page ? id : null;
};

export function NavBar({ world }: { world?: "field" } = {}) {
  const pathname = usePathname();
  const page = PAGE_NAVS[pathname] ? pathname : "/";
  const { sections: pageLinks, cta } = PAGE_NAVS[page];
  // The section in view, tagged with its page so a stale one from the last
  // page never lights up.
  const [spy, setSpy] = useState<{ page: string; id: string } | null>(null);
  const activeSection = spy?.page === pathname ? spy.id : null;

  // Scroll-spy: track which section is in view on a page with its own menu.
  // A thin band near the top of the viewport decides the "current" section
  // as the user scrolls.
  useEffect(() => {
    if (!PAGE_NAVS[pathname]) return;
    const ids = PAGE_NAVS[pathname].sections.flatMap((l) => {
      const id = sectionOf(l.href, pathname);
      return id ? [id] : [];
    });
    const sections = ids
      .map((id) => document.getElementById(id))
      .filter((el): el is HTMLElement => el !== null);
    if (sections.length === 0) return;

    const visible = new Set<string>();
    const observer = new IntersectionObserver(
      (entries) => {
        for (const e of entries) {
          if (e.isIntersecting) visible.add(e.target.id);
          else visible.delete(e.target.id);
        }
        const topmost = ids.find((id) => visible.has(id));
        if (topmost) setSpy({ page: pathname, id: topmost });
      },
      { rootMargin: "-45% 0px -50% 0px" },
    );
    sections.forEach((s) => observer.observe(s));
    return () => observer.disconnect();
  }, [pathname]);

  const isActive = (href: string): boolean => {
    if (href === "/docs") {
      return pathname === "/docs" || pathname.startsWith("/docs/");
    }
    if (href.includes("#")) {
      const id = sectionOf(href, pathname);
      return id !== null && activeSection === id;
    }
    return pathname === href;
  };

  if (world === "field") {
    return (
      <nav className="fixed top-7 left-0 right-0 h-[56px] bg-[rgba(11,12,11,0.92)] border-b border-line flex items-center px-6 lg:px-12 z-[99]">
        <Link
          href="/"
          className="flex items-baseline gap-3 no-underline shrink-0 whitespace-nowrap"
          aria-label="NET home"
        >
          <span className="font-display font-bold uppercase text-[24px] leading-none tracking-[-0.02em] text-ink">
            net
          </span>
          <span className="font-mono text-[9px] text-ink-dim tracking-[0.18em] font-semibold">
            {"// AI 2070"}
          </span>
        </Link>
        <ul className="hidden xl:flex list-none ml-6 min-[1400px]:ml-10 items-stretch h-[30px] border border-line divide-x divide-line">
          {pageLinks.map((l, i) => {
            const active = isActive(l.href);
            return (
              <li key={l.href} className="flex">
                <Link
                  href={l.href}
                  aria-current={active ? "page" : undefined}
                  className={`flex items-center gap-2 px-2 min-[1400px]:gap-2.5 min-[1400px]:px-3 font-mono text-[10.5px] tracking-[0.14em] uppercase transition-colors ${
                    active
                      ? "bg-ink text-bg"
                      : "text-ink-dim hover:text-ink hover:bg-[#151915]"
                  }`}
                >
                  {l.label}
                  <span
                    className={`tabular-nums ${active ? "text-bg/60" : "text-ink-faint"}`}
                  >
                    {String(i + 1).padStart(2, "0")}
                  </span>
                </Link>
              </li>
            );
          })}
        </ul>
        <ul className="hidden lg:flex list-none gap-4 min-[1400px]:gap-5 ml-auto pl-4 items-center">
          {SITE_LINKS.map((l) => {
            const active = isActive(l.href);
            return (
              <li key={l.href}>
                <Link
                  href={l.href}
                  aria-current={active ? "page" : undefined}
                  className={`relative block py-1 font-mono text-[10.5px] tracking-[0.16em] uppercase transition-colors hover:text-ink ${
                    active ? "text-ink" : "text-ink-dim"
                  }`}
                >
                  {l.label}
                  {active ? (
                    <span
                      aria-hidden
                      className="absolute left-0 right-0 -bottom-[3px] h-px bg-accent"
                    />
                  ) : null}
                </Link>
              </li>
            );
          })}
          <li>
            <Link
              href={cta.href}
              className="btn-field !py-2 !px-3.5 !text-[10.5px]"
            >
              <ArrowDown size={12} weight="bold" aria-hidden />
              {cta.label}
            </Link>
          </li>
        </ul>
        <Link
          href={cta.href}
          className="lg:hidden ml-auto btn-field !py-2.5 !px-4 !text-[10.5px]"
        >
          <ArrowDown size={12} weight="bold" aria-hidden />
          {cta.label}
        </Link>
      </nav>
    );
  }

  return (
    <nav className="fixed top-7 left-0 right-0 h-[52px] nav-glass border-b border-line flex items-center px-6 z-[99]">
      <Link
        href="/"
        className="logo-mark font-display text-[22px] text-ink tracking-[0.1em] flex items-baseline gap-2.5"
      >
        net{" "}
        <span className="font-mono text-[9px] text-accent tracking-[0.15em] font-semibold">
          {"// AI 2070"}
        </span>
      </Link>
      <ul className="hidden lg:flex list-none gap-7 ml-auto items-center">
        {[...PAGE_NAVS["/"].sections, ...SITE_LINKS].map((l) => {
          const active = isActive(l.href);
          return (
            <li key={l.href}>
              <Link
                href={l.href}
                aria-current={active ? "page" : undefined}
                className={`text-[11px] tracking-[0.08em] uppercase transition-colors hover:text-accent ${
                  active ? "text-accent font-semibold" : "text-ink-dim"
                }`}
              >
                {l.label}
              </Link>
            </li>
          );
        })}
        <li>
          <Link
            href={globals.links.install}
            className="install-btn bg-accent text-bg border border-accent px-3.5 py-1.5 text-[11px] tracking-[0.08em] uppercase font-semibold transition-colors"
          >
            ↓ INSTALL
          </Link>
        </li>
      </ul>
      <Link
        href={globals.links.install}
        className="lg:hidden ml-auto install-btn bg-accent text-bg border border-accent px-3.5 py-1.5 text-[11px] tracking-[0.08em] uppercase font-semibold transition-colors"
      >
        ↓ INSTALL
      </Link>
    </nav>
  );
}
