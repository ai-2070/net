import Link from "next/link";
import { SectionLabel } from "@/components/SectionLabel";
import { DisplayHeading } from "@/components/DisplayHeading";
import globals from "@/lib/globals";

const CONTACT = `mailto:${globals.email}?subject=NET%20%2F%2F%20investor%20intro`;

export function SimpleCtaSection() {
  return (
    <section id="cta" className="border-b border-line px-6 py-20">
      <SectionLabel>§12 / close</SectionLabel>
      <DisplayHeading>
        agents need more than tools.
        <br />
        they need <span className="text-accent">a way to work.</span>
      </DisplayHeading>

      <p className="text-[16px] text-ink max-w-[780px] leading-[1.65] font-light mb-3">
        The next generation of AI products will not live inside one chat window.
        They will use machines, files, apps, GPUs, streams, devices, and
        services — and they will need to know what exists, what is allowed,
        where work should run, and how results move back.
      </p>
      <p className="text-[15px] text-ink max-w-[780px] leading-[1.6] mb-12">
        <strong className="text-ink font-medium">
          Net is the operating layer for that world.
        </strong>
      </p>

      <div className="grid grid-cols-1 md:grid-cols-2 gap-px bg-line border border-line">
        <div className="bg-bg p-8 flex flex-col">
          <div className="text-[10px] text-accent tracking-[0.14em] uppercase mb-3">
            ▸ for investors &amp; partners
          </div>
          <p className="text-[13px] text-ink-dim leading-[1.6] mb-6 flex-1">
            As AI moves from answering to operating, this layer becomes part of
            the stack. Let&apos;s talk about building it.
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
              Watch the demo
            </a>
          </div>
        </div>

        <div className="bg-bg p-8 flex flex-col">
          <div className="text-[10px] text-cyan tracking-[0.14em] uppercase mb-3">
            ▸ for builders
          </div>
          <p className="text-[13px] text-ink-dim leading-[1.6] mb-6 flex-1">
            Want the technical depth? The architecture and source explain how
            the operating layer actually works.
          </p>
          <div className="flex flex-wrap gap-3">
            <Link
              href="/docs/start/what-is-net"
              className="btn-ghost inline-flex items-center gap-2.5 px-5 py-3 text-[11px] tracking-[0.12em] uppercase font-semibold no-underline border border-ink-faint text-ink transition-all"
            >
              Read the overview
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
