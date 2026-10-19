import { FooterColumn } from "./FooterColumn";
import globals from "@/lib/globals";
import fieldCredits from "@/lib/field-credits.json";
import { useHomeHref } from "./field/WorldVariant";

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

export function Footer({ world }: { world?: "field" } = {}) {
  if (world === "field") return <FieldFooter />;
  return (
    <footer className="px-6 pt-16 pb-7 border-t border-accent-dim">
      <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-[2fr_1fr_1fr_1fr] gap-8 mb-12">
        <div>
          <div className="logo-mark font-display text-[22px] text-ink tracking-[0.1em] flex items-baseline gap-2.5 mb-4">
            net{" "}
            <span className="font-mono text-[11px] tracking-[0.15em] font-semibold">
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

      <div className="border-t border-line pt-6 flex justify-between text-[11px] text-ink-dim tracking-[0.1em] flex-wrap gap-4">
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

// Landing-page footer: the floor of the hall. A long monolith edge, then
// the directory cast into it.
function FieldFooter() {
  const homeHref = useHomeHref();
  const spec = FOOTER_SPEC.map((it) => ({ ...it, href: homeHref(it.href) }));
  return (
    <footer className="relative px-6 lg:px-12 pt-10 pb-8">
      <div aria-hidden className="h-px bg-line -mx-6 lg:-mx-12 mb-16" />
      <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-[2fr_1fr_1fr_1fr] gap-10 mb-16">
        <div>
          <div className="flex items-baseline gap-3 mb-5">
            <span className="font-display font-bold uppercase text-[40px] leading-none tracking-[-0.03em] text-ink">
              net
            </span>
            <span className="font-mono text-[11px] tracking-[0.18em] font-semibold text-ink-dim">
              {"// AI 2070"}
            </span>
          </div>
          <p className="text-ink-dim text-[15px] leading-[1.6] max-w-[24em]">
            Network Event Transport. A latency-first encrypted protocol for
            compute.
          </p>
        </div>
        <FooterColumn title="Spec" items={spec} world="field" />
        <FooterColumn title="Docs" items={FOOTER_DOCS} world="field" />
        <FooterColumn
          title="Resources"
          items={FOOTER_RESOURCES}
          world="field"
        />
      </div>

      <p className="text-[13px] leading-[1.6] text-ink-dim mb-8">
        Pictures:{" "}
        {fieldCredits.map((c, i) => (
          <span key={c.file}>
            <a
              href={c.page}
              target="_blank"
              rel="noopener noreferrer"
              className="hover:text-ink transition-colors"
            >
              {c.title}
            </a>
            {c.creator ? ` by ${c.creator}` : ""}, {c.license}
            {i < fieldCredits.length - 1 ? "; " : "."}
          </span>
        ))}
      </p>

      <div className="border-t border-line pt-6 flex justify-between stencil flex-wrap gap-4">
        <span>© {ET_YEAR} — NET // PROTOCOL.0x4E45·54</span>
        <span className="flex items-center gap-2">
          <span aria-hidden className="w-1.5 h-1.5 bg-accent inline-block" />
          NET status: <span className="text-accent">ONLINE</span>
        </span>
        <span className="text-ink">{"// AI 2070"}</span>
      </div>
    </footer>
  );
}
