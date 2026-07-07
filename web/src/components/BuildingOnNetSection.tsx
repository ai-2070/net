import { SectionLabel } from "./SectionLabel";
import { DisplayHeading } from "./DisplayHeading";
import globals from "@/lib/globals";

interface RingItem {
  label: string;
  body: string;
}

const RING_ITEMS: readonly RingItem[] = [
  { label: "N=2", body: "Your machines, today" },
  { label: "TEAM", body: "Same substrate, more trust" },
  { label: "ORG", body: "Policy-bound, org-wide" },
  { label: "STRANGERS", body: "Attested, paid, the last ring" },
];

interface BuildItem {
  label: string;
  title: string;
  body: string;
}

const BUILD_ITEMS: readonly BuildItem[] = [
  {
    label: "LIVE",
    title: "Hermes",
    body: "Runs on Net today. Cross-machine tool calling, on PyPI.",
  },
  {
    label: "BUILDING",
    title: "MCP Bridge",
    body: "Existing MCP servers join the mesh. Net capabilities reachable from any MCP host.",
  },
  {
    label: "NEXT",
    title: "OpenClaw",
    body: "Integration next in queue.",
  },
];

export function BuildingOnNetSection() {
  return (
    <section id="building" className="border-b border-line px-6 py-20">
      <SectionLabel>§08 / building on net</SectionLabel>
      <DisplayHeading>
        works at n = 2 machines.
        <br />
        <span className="text-accent">scales to an economy.</span>
      </DisplayHeading>

      <p className="text-[16px] text-ink max-w-[740px] leading-[1.6] font-light mb-8">
        The network is useful at two machines — yours. No marketplace cold
        start, no strangers required. Trust widens outward on the same
        substrate: your machines, your team, your org, and finally paid
        capabilities from attested strangers. That last ring is native to the
        protocol — not a marketplace bolted on later.
      </p>

      <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-8 mb-12">
        {RING_ITEMS.map((it) => (
          <div key={it.label} className="border-t border-accent-dim pt-4">
            <div className="text-[11px] text-accent uppercase tracking-[0.15em] mb-2">
              {it.label}
            </div>
            <div className="text-ink-dim text-[12px] leading-[1.6]">
              {it.body}
            </div>
          </div>
        ))}
      </div>

      <p className="text-[16px] text-ink max-w-[740px] leading-[1.6] font-light mb-12">
        Every agent company is rebuilding pieces of this layer — tool
        federation, identity, permissions, transport, state. Net turns those
        repeated problems into one reusable substrate.
      </p>

      <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 gap-8 mt-10">
        {BUILD_ITEMS.map((item) => (
          <div key={item.label} className="border-t border-accent-dim pt-4">
            <div className="text-[11px] text-accent uppercase tracking-[0.15em] mb-2.5">
              {item.label}
            </div>
            <h3 className="font-head text-[18px] text-ink lowercase tracking-[0.04em] mb-2">
              {item.title}
            </h3>
            <p className="text-ink-dim text-[12px] leading-[1.6]">{item.body}</p>
          </div>
        ))}

        <div className="border-t border-accent-dim pt-4">
          <div className="text-[11px] text-accent uppercase tracking-[0.15em] mb-2.5">
            YOU
          </div>
          <h3 className="font-head text-[18px] text-ink lowercase tracking-[0.04em] mb-2">
            Your harness
          </h3>
          <p className="text-ink-dim text-[12px] leading-[1.6]">
            Building a framework, harness, or coordination tool that needs
            federation?{" "}
            <a
              href={`mailto:${globals.email}`}
              className="text-accent hover:text-ink transition-colors"
            >
              talk to us →
            </a>
          </p>
        </div>
      </div>

      <div className="border-l-2 border-accent pl-8 pr-8 py-6 bg-accent/[0.02] mt-12 max-w-[900px]">
        <p className="text-[18px] text-ink leading-[1.5] font-light">
          Your harness is the brain.{" "}
          <span className="text-accent">
            This is the nervous system it&apos;s been missing.
          </span>
        </p>
      </div>
    </section>
  );
}
