import { FooterColumn } from "./FooterColumn";
import globals from "@/lib/globals";

const FOOTER_SPEC: ReadonlyArray<{
  href: string;
  label: string;
  class?: string;
}> = [
  { href: "/#topology", label: "Topology classes" },
  { href: "/#properties", label: "Protocol properties" },
  { href: "/#mikoshi", label: "Mikoshi" },
  { href: "/#runtime", label: "Compute runtime" },
  { href: "/#apps", label: "Applications" },
  { href: "/#meshos", label: "MeshOS" },
  { href: "/#wall", label: "The Blackwall" },
  { href: "/#releases", label: "Releases" },
];

const FOOTER_DOCS: ReadonlyArray<{
  href: string;
  label: string;
  class?: string;
}> = [
  {
    href: "/docs/start/quickstart",
    label: "Quickstart",
  },
  {
    href: "/docs/concepts/architecture",
    label: "Architecture",
  },
  {
    href: "/docs/concepts/channels",
    label: "Channels",
  },
  {
    href: "/docs/concepts/events-and-causality",
    label: "Events and Causality",
  },
  {
    href: "/docs/concepts/identity",
    label: "Identity",
  },
  {
    href: "/docs/concepts/capabilities",
    label: "Capabilities",
  },
  {
    href: "/docs/concepts/subnets",
    label: "Subnets",
  },
  {
    href: "/docs/concepts/storage-stack",
    label: "Storage",
  },
];

const FOOTER_RESOURCES: ReadonlyArray<{
  href: string;
  label: string;
  class?: string;
}> = [
  {
    href: "https://crates.io/crates/net-mesh",
    label: "Rust // crates.io",
  },
  {
    href: "https://www.npmjs.com/package/@net-mesh/core",
    label: "TypeScript // npm",
  },
  { href: "https://pypi.org/project/net-mesh/", label: "Python // PyPI" },
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
