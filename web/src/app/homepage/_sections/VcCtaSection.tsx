import Link from "next/link";
import { SectionLabel } from "@/components/SectionLabel";
import { DisplayHeading } from "@/components/DisplayHeading";
import globals from "@/lib/globals";

const NEEDS: ReadonlyArray<string> = [
  "territory",
  "live state",
  "artifacts",
  "durable work",
  "local authority",
];

const CONTACT = `mailto:${globals.email}?subject=NET%20%2F%2F%20investor%20intro`;

export function VcCtaSection() {
  return (
    <section id="cta" className="border-b border-line px-6 py-20">
      <SectionLabel>§11 / close</SectionLabel>
      <DisplayHeading>
        agents need
        <br />
        more than <span className="text-accent">functions.</span>
      </DisplayHeading>

      <div className="flex flex-wrap gap-2.5 mb-8">
        {NEEDS.map((n) => (
          <span
            key={n}
            className="border border-line bg-bg-2 px-4 py-2 text-[13px] text-ink lowercase tracking-[0.03em]"
          >
            they need {n}
          </span>
        ))}
      </div>

      <p className="text-[15px] text-ink-dim max-w-[760px] leading-[1.7] mb-12">
        They need a way to coordinate without pretending the world is one stable
        process.{" "}
        <span className="text-ink">Net is the substrate for that world.</span>
      </p>

      <div className="grid grid-cols-1 md:grid-cols-2 gap-px bg-line border border-line">
        <div className="bg-bg p-8 flex flex-col">
          <div className="text-[10px] text-accent tracking-[0.14em] uppercase mb-3">
            ▸ for investors
          </div>
          <p className="text-[13px] text-ink-dim leading-[1.6] mb-6 flex-1">
            If agents become operators, this layer has to exist. Let&apos;s talk
            about owning it.
          </p>
          <div className="flex flex-wrap gap-3">
            <a
              href={CONTACT}
              className="btn-primary inline-flex items-center gap-2.5 px-5 py-3 text-[11px] tracking-[0.12em] uppercase font-semibold no-underline border border-accent bg-accent text-bg transition-all"
            >
              ▸ Talk to AI-2070 <span className="text-sm">→</span>
            </a>
            <a
              href={CONTACT}
              className="btn-ghost inline-flex items-center gap-2.5 px-5 py-3 text-[11px] tracking-[0.12em] uppercase font-semibold no-underline border border-ink-faint text-ink transition-all"
            >
              Request the deck
            </a>
          </div>
        </div>

        <div className="bg-bg p-8 flex flex-col">
          <div className="text-[10px] text-cyan tracking-[0.14em] uppercase mb-3">
            ▸ for builders
          </div>
          <p className="text-[13px] text-ink-dim leading-[1.6] mb-6 flex-1">
            Read how the substrate works and run it across your own machines.
          </p>
          <div className="flex flex-wrap gap-3">
            <Link
              href="/docs/concepts/architecture"
              className="btn-ghost inline-flex items-center gap-2.5 px-5 py-3 text-[11px] tracking-[0.12em] uppercase font-semibold no-underline border border-ink-faint text-ink transition-all"
            >
              Read the architecture
            </Link>
            <a
              href="https://github.com/ai-2070/net"
              className="btn-ghost inline-flex items-center gap-2.5 px-5 py-3 text-[11px] tracking-[0.12em] uppercase font-semibold no-underline border border-ink-faint text-ink transition-all"
            >
              View GitHub ↗
            </a>
          </div>
        </div>
      </div>
    </section>
  );
}
