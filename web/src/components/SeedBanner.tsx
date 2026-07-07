import globals from "@/lib/globals";
import { GlitchText } from "./GlitchText";

export function SeedBanner() {
  return (
    <a
      href={`mailto:${globals.email}`}
      className="group block border-b border-line bg-accent/[0.06] hover:bg-accent/[0.12] transition-colors overflow-hidden"
    >
      <div className="glitch-banner px-6 py-3 flex items-center justify-center gap-3 text-[11px] font-mono tracking-[0.08em] flex-wrap">
        <span className="bg-accent text-bg px-2 py-0.5 font-bold tracking-[0.18em] text-[10px]">
          <GlitchText text="SEED ROUND" intervalMs={5400} offsetMs={2700} />
        </span>
        <span className="text-ink">
          <b className="text-accent">
            <GlitchText text="AI 2070" intervalMs={5400} />
          </b>{" "}
          is raising seed funding to build the distributed mesh runtime.
        </span>
        <span className="text-accent inline-flex items-center gap-1">
          get in touch
          <span className="transition-transform group-hover:translate-x-0.5">
            →
          </span>
        </span>
      </div>
    </a>
  );
}
