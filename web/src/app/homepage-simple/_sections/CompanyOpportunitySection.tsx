import { SectionLabel } from "@/components/SectionLabel";
import { DisplayHeading } from "@/components/DisplayHeading";

const REPEATED: ReadonlyArray<string> = [
  "How does the agent find the right tool?",
  "How does it use a user's actual computer safely?",
  "How does it move files between machines?",
  "How does it run work on a remote GPU?",
  "How does it watch a live process or app?",
  "How does it continue work after a reconnect?",
  "How does it know what it is allowed to do?",
  "How does it coordinate across devices without one cloud account?",
];

export function CompanyOpportunitySection() {
  return (
    <section
      id="opportunity"
      className="compute-bg border-b border-line px-6 py-24"
    >
      <SectionLabel>§06 / the company opportunity</SectionLabel>
      <DisplayHeading>
        every agent company is
        <br />
        rebuilding pieces of <span className="text-accent">this layer.</span>
      </DisplayHeading>

      <p className="text-[16px] text-ink max-w-[760px] leading-[1.6] font-light mb-10">
        Teams building serious AI products keep running into the same
        infrastructure problems — and most solve them one by one, with custom
        bridges, internal routers, file movers, worker systems, and permission
        hacks.
      </p>

      <div className="grid grid-cols-1 md:grid-cols-2 gap-px bg-line border border-line">
        {REPEATED.map((q) => (
          <div
            key={q}
            className="bg-bg px-6 py-5 flex items-start gap-3 transition-colors hover:bg-bg-2"
          >
            <span className="text-warn text-[13px] leading-[1.5] shrink-0">
              ?
            </span>
            <span className="text-[13px] text-ink-dim leading-[1.5]">{q}</span>
          </div>
        ))}
      </div>

      <div className="mt-12 text-center py-14 border-t border-b border-accent-dim bg-accent/[0.02]">
        <div
          className="font-display text-ink leading-[1.15] mb-2"
          style={{ fontSize: "clamp(26px, 4vw, 46px)" }}
        >
          net turns those repeated problems
          <br />
          into{" "}
          <span className="text-accent">one reusable operating layer.</span>
        </div>
        <p className="text-[13px] text-ink-dim mt-6 tracking-[0.06em] font-mono">
          That is the company opportunity.
        </p>
      </div>
    </section>
  );
}
