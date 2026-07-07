import { BlackwallViz } from "./BlackwallViz";
import { SectionLabel } from "./SectionLabel";
import { DisplayHeading } from "./DisplayHeading";

interface BlackwallItem {
  label: string;
  body: string;
}

const BLACKWALL_ITEMS: readonly BlackwallItem[] = [
  {
    label: "BACKPRESSURE",
    body: "Full nodes push back, floods die at the first hop",
  },
  { label: "DEDUPLICATION", body: "The same message never travels twice" },
  { label: "BOUNDED QUEUES", body: "Nothing waits forever, nothing piles up" },
  { label: "TTL", body: "Every message expires; loops starve" },
  { label: "FANOUT LIMITS", body: "Nothing amplifies beyond its budget" },
  { label: "RATE LIMITS", body: "Every sender has a ceiling" },
];

export function BlackwallSection() {
  return (
    <section id="wall" className="blackwall-bg border-b border-line px-6 py-20">
      <SectionLabel>§05 / the blackwall</SectionLabel>
      <DisplayHeading>
        safety isn&apos;t declared.
        <br />
        it&apos;s <span className="text-accent">derived.</span>
      </DisplayHeading>

      <BlackwallViz />

      <p className="text-[16px] text-ink max-w-[740px] leading-[1.6] font-light mb-10">
        In Cyberpunk, the Blackwall is what holds the rogue AIs back. Ours
        isn&apos;t a thing you can point to. There is no wall component, no
        filter process, no gatekeeper to breach — the wall is what the
        constraints add up to.
      </p>

      <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-8 mb-12">
        {BLACKWALL_ITEMS.map((item) => (
          <div key={item.label} className="border-t border-accent-dim pt-4">
            <div className="text-[11px] text-accent uppercase tracking-[0.15em] mb-2">
              {item.label}
            </div>
            <div className="text-ink-dim text-[12px] leading-[1.6]">
              {item.body}
            </div>
          </div>
        ))}
      </div>

      <p className="text-[16px] text-ink max-w-[740px] leading-[1.6] font-light mb-10">
        Each constraint is small, local, and boring. Together they mean a flood
        can&apos;t propagate, a loop can&apos;t live, and an attack can&apos;t
        amplify — on every node, with no coordinator to compromise.
      </p>

      <div className="border-l-2 border-accent pl-8 pr-8 py-6 bg-accent/[0.02] mt-12 max-w-[900px]">
        <p className="text-[18px] text-ink leading-[1.5] font-light">
          No single wall.{" "}
          <span className="text-accent">Every node is a brick.</span>
        </p>
      </div>
    </section>
  );
}
