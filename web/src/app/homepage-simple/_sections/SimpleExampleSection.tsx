import { SectionLabel } from "@/components/SectionLabel";
import { DisplayHeading } from "@/components/DisplayHeading";

interface Step {
  n: string;
  body: string;
}

const STEPS: ReadonlyArray<Step> = [
  { n: "01", body: "The agent sees which trusted machines are available." },
  { n: "02", body: "Each machine says what it can safely provide." },
  {
    n: "03",
    body: "The agent requests the file, browser, script, or GPU it needs.",
  },
  {
    n: "04",
    body: "The right machine accepts or rejects the request based on local rules.",
  },
  { n: "05", body: "Work runs where it makes sense." },
  { n: "06", body: "Results move back as durable files or artifacts." },
  { n: "07", body: "Progress can stream live." },
  {
    n: "08",
    body: "If one machine disconnects, the system recovers instead of losing the whole job.",
  },
];

export function SimpleExampleSection() {
  return (
    <section id="example" className="border-b border-line px-6 py-20">
      <SectionLabel>§04 / a simple example</SectionLabel>
      <DisplayHeading>
        one agent. many machines.
        <br />
        <span className="text-accent">one way to work.</span>
      </DisplayHeading>

      <div className="border border-line bg-bg-2 p-7 max-w-[860px] mb-12">
        <p className="text-[15px] text-ink leading-[1.7]">
          Imagine an AI agent helping with a real project. It starts on your{" "}
          <span className="text-accent">laptop</span>. It needs a design file
          from your <span className="text-accent">desktop</span>, a browser
          session from your main machine, a script running on a{" "}
          <span className="text-accent">server</span>, and a GPU job on another
          box.
        </p>
        <p className="text-[13px] text-ink-dim leading-[1.6] mt-4">
          Without Net, each connection is a custom integration. With Net, it is
          one flow:
        </p>
      </div>

      <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 border-t border-l border-line">
        {STEPS.map((s) => (
          <div
            key={s.n}
            className="border-r border-b border-line p-6 flex flex-col gap-3 transition-colors hover:bg-bg-2"
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

      <div className="mt-12 text-center py-12 border-t border-b border-accent-dim bg-accent/[0.02]">
        <p className="text-[11px] text-ink-dim tracking-[0.16em] uppercase mb-3">
          the basic idea
        </p>
        <div
          className="font-display text-ink leading-[1.15]"
          style={{ fontSize: "clamp(24px, 3.4vw, 40px)" }}
        >
          agents get a shared operating layer
          <br />
          for the real world of{" "}
          <span className="text-accent">machines and resources.</span>
        </div>
      </div>
    </section>
  );
}
