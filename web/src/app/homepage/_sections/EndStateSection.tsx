import { SectionLabel } from "@/components/SectionLabel";
import { DisplayHeading } from "@/components/DisplayHeading";

interface Step {
  n: string;
  body: string;
}

const WALKTHROUGH: ReadonlyArray<Step> = [
  { n: "01", body: "Agent discovers trusted devices." },
  { n: "02", body: "Claims a GPU worker before anyone else can." },
  { n: "03", body: "Streams browser and sensor context into its view." },
  { n: "04", body: "Moves the input artifact through CAS." },
  { n: "05", body: "Launches a durable task that outlives the request." },
  { n: "06", body: "Receives a live progress stream." },
  { n: "07", body: "Retrieves the output artifact by hash." },
  { n: "08", body: "Recovers when one node drops off the mesh." },
];

export function EndStateSection() {
  return (
    <section id="end-state" className="border-b border-line px-6 py-20">
      <SectionLabel>§05 / end state</SectionLabel>
      <DisplayHeading>
        an operating fabric
        <br />
        for <span className="text-accent">autonomous work.</span>
      </DisplayHeading>

      <div className="grid grid-cols-1 lg:grid-cols-2 gap-10 mt-6 mb-12">
        <div>
          <p className="text-[13px] text-ink-dim leading-[1.7] mb-4">
            In the current stack, every integration is a bespoke bridge. The
            desktop agent needs one path to files, another to the browser,
            another to terminal tools, another to remote GPUs, another to cloud
            services, another to a phone, another to a NAS, another to
            long-running jobs, and another to live streams.
          </p>
          <p className="text-[15px] text-ink leading-[1.6]">
            Net collapses those into{" "}
            <span className="text-accent">one coordination model.</span>
          </p>
        </div>
        <div className="border-l-2 border-accent-dim pl-5 self-center">
          <p className="text-[15px] text-ink leading-[1.6] mb-2">
            The end result is not another agent harness.
          </p>
          <p className="text-[20px] text-accent leading-[1.3] font-medium">
            It is the street grid agents operate on.
          </p>
        </div>
      </div>

      <div className="border border-line">
        <div className="border-b border-line px-5 py-3 text-[10px] tracking-[0.14em] uppercase text-ink-dim bg-bg-2 flex justify-between">
          <span>
            <span className="text-accent">▸</span> end-state.walkthrough
          </span>
          <span>one agent · one fabric</span>
        </div>
        <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 border-t border-l border-line">
          {WALKTHROUGH.map((s) => (
            <div
              key={s.n}
              className="p-6 flex flex-col gap-3 border-r border-b border-line transition-colors hover:bg-bg-2"
            >
              <span className="font-display text-[22px] text-accent leading-none">
                {s.n}
              </span>
              <span className="text-[12px] text-ink-dim leading-[1.55]">
                {s.body}
              </span>
            </div>
          ))}
        </div>
      </div>
    </section>
  );
}
