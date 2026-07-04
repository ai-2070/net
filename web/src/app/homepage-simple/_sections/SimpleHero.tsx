import Link from "next/link";
import { PacketRain } from "@/components/PacketRain";
import { HeroMeshViz } from "./HeroMeshViz";

export function SimpleHero() {
  return (
    <section
      id="hero"
      className="hero relative overflow-hidden border-b border-line px-6 pt-[60px] pb-20"
    >
      <PacketRain />
      <div className="relative grid grid-cols-1 lg:grid-cols-[1fr_520px] gap-12 items-start">
        <div>
          <div className="text-[10px] text-ink-dim tracking-[0.15em] mb-7 flex flex-wrap gap-[18px] items-center">
            <span className="text-accent border border-accent-dim px-2 py-[3px]">
              THE OPERATING LAYER FOR AI AGENTS
            </span>
          </div>

          <h1
            className="font-display leading-[0.9] tracking-[-0.02em] text-ink mb-7"
            style={{ fontSize: "clamp(46px, 7.6vw, 100px)" }}
          >
            a network
            <br />
            where agents
            <br />
            <span className="text-accent">operate.</span>
          </h1>

          <p className="text-[19px] md:text-[21px] text-ink mt-7 max-w-[640px] leading-[1.4]">
            Net is the operating layer that makes every device{" "}
            <span className="text-accent">a tool.</span>
          </p>

          <p className="text-[16px] text-ink mt-7 max-w-[620px] leading-[1.55] font-light">
            AI agents are starting to do real work. They need to use tools, open
            files, watch live information, run jobs, and move between devices.
            Without Net, every agent company has to rebuild this layer for
            itself.
          </p>

          <p className="text-[15px] text-ink mt-6 max-w-[620px] leading-[1.6] border-l-2 border-accent-dim pl-4">
            <strong className="text-ink font-medium">
              Net is the operating layer that lets agents work across real
              machines, tools, files, apps, and compute.
            </strong>
          </p>

          <div className="mt-11 flex gap-3 flex-wrap items-center">
            <Link
              href="/docs/start/install"
              className="btn-primary inline-flex items-center gap-2.5 px-5 py-3 text-[11px] tracking-[0.12em] uppercase font-semibold no-underline border border-accent bg-accent text-bg transition-all"
            >
              ↓ Install Net <span className="text-sm">→</span>
            </Link>
            <Link
              href="/docs/start/what-is-net"
              className="btn-ghost inline-flex items-center gap-2.5 px-5 py-3 text-[11px] tracking-[0.12em] uppercase font-semibold no-underline border border-ink-faint text-ink transition-all"
            >
              Read the overview
            </Link>
            <a
              href="#example"
              className="btn-ghost inline-flex items-center gap-2.5 py-3 text-[11px] tracking-[0.12em] uppercase font-semibold no-underline text-ink transition-all"
            >
              // see how it works ↘
            </a>
          </div>
        </div>

        <div className="hidden lg:block self-start">
          <HeroMeshViz />
        </div>
      </div>
    </section>
  );
}
