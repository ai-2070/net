import { FooterColumn } from "./FooterColumn";
import globals from "@/lib/globals";

const FOOTER_SPEC: ReadonlyArray<{
  href: string;
  label: string;
  class?: string;
}> = [
  { href: "#topology", label: "Topology classes" },
  { href: "#properties", label: "Protocol properties" },
  { href: "#mikoshi", label: "Mikoshi" },
  { href: "#runtime", label: "Compute runtime" },
  { href: "#apps", label: "Applications" },
  { href: "#wall", label: "The Blackwall" },
  { href: "#releases", label: "Releases" },
];

const FOOTER_DOCS: ReadonlyArray<{
  href: string;
  label: string;
  class?: string;
}> = [
  {
    href: "https://github.com/ai-2070/net/blob/master/net/crates/net/README.md",
    label: "README.md",
  },
  {
    href: "https://github.com/ai-2070/net/blob/master/net/crates/net/docs/COMPUTE.md",
    label: "COMPUTE.md",
  },
  {
    href: "https://github.com/ai-2070/net/blob/master/net/crates/net/docs/CHANNELS.md",
    label: "CHANNELS.md",
  },
  {
    href: "https://github.com/ai-2070/net/blob/master/net/crates/net/docs/SUBNETS.md",
    label: "SUBNETS.md",
  },
  {
    href: "https://github.com/ai-2070/net/blob/master/net/crates/net/docs/SUBPROTOCOLS.md",
    label: "SUBPROTOCOLS.md",
  },
  {
    href: "https://github.com/ai-2070/net/blob/master/net/crates/net/BENCHMARKS.md",
    label: "BENCHMARKS.md",
  },
];

const FOOTER_RESOURCES: ReadonlyArray<{
  href: string;
  label: string;
  class?: string;
}> = [
  {
    href: "https://crates.io/crates/ai2070-net",
    label: "Rust // crates.io",
  },
  {
    href: "https://www.npmjs.com/package/@ai2070/net",
    label: "TypeScript // npm",
  },
  { href: "https://pypi.org/project/ai2070-net/", label: "Python // PyPI" },
  {
    href: "https://github.com/ai-2070/net/tree/master/go",
    label: "Go // module",
  },
  {
    href: "https://github.com/ai-2070/net/tree/master/net/crates/net/include",
    label: "C // SDK",
  },
  { href: "https://github.com/ai-2070/net", label: "Source // GitHub" },
  {
    href: `mailto:${globals.email}`,
    label: "▸ Contact",
    class: "text-accent",
  },
];

const ET_YEAR = new Date().toLocaleString("en-US", {
  timeZone: "America/New_York",
  year: "numeric",
});

export function Footer() {
  return (
    <footer className="px-6 pt-16 pb-7 border-t border-accent-dim">
      <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-[2fr_1fr_1fr_1fr] gap-8 mb-12">
        <div>
          <div className="logo-mark font-display text-[22px] text-ink tracking-[0.1em] flex items-baseline gap-2.5 mb-4">
            net{" "}
            <span className="font-mono text-[9px] tracking-[0.15em] font-semibold">
              // AI 2070
            </span>
          </div>
          <p className="text-ink-dim text-[12px] leading-[1.6] max-w-[380px]">
            Network Event Transport. A latency-first encrypted protocol for
            compute.
          </p>
        </div>
        <FooterColumn title="Spec" items={FOOTER_SPEC} />
        <FooterColumn title="Docs" items={FOOTER_DOCS} />
        <FooterColumn title="Resources" items={FOOTER_RESOURCES} />
      </div>

      <div className="border-t border-line pt-6 flex justify-between text-[10px] text-ink-dim tracking-[0.1em] flex-wrap gap-4">
        <span>© {ET_YEAR} — NET // PROTOCOL.0x4E45·54</span>
        <span>
          <span className="text-accent">▸</span> NET status:{" "}
          <span className="text-accent">ONLINE</span>
        </span>
        <span className="shimmer-2070">// AI 2070</span>
      </div>
    </footer>
  );
}
