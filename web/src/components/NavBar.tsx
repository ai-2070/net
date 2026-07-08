import Link from "next/link";

const NAV_LINKS: ReadonlyArray<{ href: string; label: string }> = [
  { href: "/#bridge", label: "BRIDGE" },
  { href: "/#everywhere", label: "APPS" },
  { href: "/#identity", label: "IDENTITY" },
  { href: "/#wall", label: "BLACKWALL" },
  { href: "/#bench", label: "BENCH" },
  { href: "/#building", label: "BUILD" },
  { href: "/#under-the-hood", label: "HOOD" },
  { href: "/docs", label: "DOCS" },
];

export function NavBar() {
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
        {NAV_LINKS.map((l) => (
          <li key={l.href}>
            <Link
              href={l.href}
              className="text-ink-dim text-[11px] tracking-[0.08em] uppercase hover:text-accent transition-colors"
            >
              {l.label}
            </Link>
          </li>
        ))}
        <li>
          <Link
            href="/#install"
            className="install-btn bg-accent text-bg border border-accent px-3.5 py-1.5 text-[11px] tracking-[0.08em] uppercase font-semibold transition-colors"
          >
            ↓ INSTALL
          </Link>
        </li>
      </ul>
      <Link
        href="/#install"
        className="lg:hidden ml-auto install-btn bg-accent text-bg border border-accent px-3.5 py-1.5 text-[11px] tracking-[0.08em] uppercase font-semibold transition-colors"
      >
        ↓ INSTALL
      </Link>
    </nav>
  );
}
