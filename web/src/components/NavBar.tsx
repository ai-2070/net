"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";
import { useEffect, useState } from "react";
import globals from "@/lib/globals";

const NAV_LINKS: ReadonlyArray<{ href: string; label: string }> = [
  { href: "/#what", label: "HOME" },
  { href: "/#bench", label: "BENCH" },
  { href: "/#runtime", label: "RUNTIME" },
  { href: "/#dataforts", label: "DATAFORTS" },
  { href: "/#meshos", label: "MESHOS" },
  { href: "/#install", label: "SDKS" },
  { href: "/#apps", label: "APPS" },
  { href: "/#wall", label: "BLACKWALL" },
  { href: "/docs", label: "DOCS" },
];

// Section ids referenced by the on-page hash links, in document order.
const SECTION_IDS: ReadonlyArray<string> = NAV_LINKS.flatMap((l) =>
  l.href.startsWith("/#") ? [l.href.slice(2)] : [],
);

export function NavBar() {
  const pathname = usePathname();
  const [activeSection, setActiveSection] = useState<string | null>(null);

  // Scroll-spy: track which section is in view, but only on the home page
  // where the hash links resolve. A thin band near the top of the viewport
  // decides the "current" section as the user scrolls.
  useEffect(() => {
    if (pathname !== "/") {
      setActiveSection(null);
      return;
    }
    const sections = SECTION_IDS.map((id) =>
      document.getElementById(id),
    ).filter((el): el is HTMLElement => el !== null);
    if (sections.length === 0) return;

    const visible = new Set<string>();
    const observer = new IntersectionObserver(
      (entries) => {
        for (const e of entries) {
          if (e.isIntersecting) visible.add(e.target.id);
          else visible.delete(e.target.id);
        }
        const topmost = SECTION_IDS.find((id) => visible.has(id));
        if (topmost) setActiveSection(topmost);
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
    if (href.startsWith("/#")) {
      return pathname === "/" && activeSection === href.slice(2);
    }
    return pathname === href;
  };

  return (
    <nav className="fixed top-7 left-0 right-0 h-[52px] nav-glass border-b border-line flex items-center px-6 z-[99]">
      <Link
        href="/"
        className="logo-mark font-display text-[22px] text-ink tracking-[0.1em] flex items-baseline gap-2.5"
      >
        net{" "}
        <span className="font-mono text-[9px] text-accent tracking-[0.15em] font-semibold">
          // AI 2070
        </span>
      </Link>
      <ul className="hidden lg:flex list-none gap-7 ml-auto items-center">
        {NAV_LINKS.map((l) => {
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
