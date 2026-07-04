import { SectionLabel } from "@/components/SectionLabel";
import { DisplayHeading } from "@/components/DisplayHeading";
import { BlackwallViz } from "@/components/BlackwallViz";

interface Decider {
  want: string;
  decides: string;
}

const DECIDERS: ReadonlyArray<Decider> = [
  { want: "read a file", decides: "the file service decides" },
  { want: "use a GPU", decides: "the GPU worker decides" },
  { want: "control a browser", decides: "the browser decides" },
  { want: "run a script", decides: "the machine decides" },
];

export function ControlSection() {
  return (
    <section
      id="control"
      className="blackwall-bg border-b border-line px-6 py-20"
    >
      <SectionLabel>§07 / control &amp; safety</SectionLabel>
      <DisplayHeading>
        the agent coordinates.
        <br />
        the <span className="text-accent">resource decides.</span>
      </DisplayHeading>

      <BlackwallViz />

      <div className="grid grid-cols-1 lg:grid-cols-2 gap-8 lg:gap-12 mb-12">
        <p className="text-[16px] text-ink leading-[1.6] font-light">
          Net is built around a simple rule:{" "}
          <strong className="text-ink font-medium">
            control stays with the machine or service that owns the consequence.
          </strong>{" "}
          There is no single gatekeeper to get past.
        </p>
        <p className="text-[16px] text-ink-dim leading-[1.6] font-light">
          Think of it like a wall around the safe zone: the mesh only extends
          trust to the parts it can watch behaving well. Safety isn&apos;t
          declared by one authority —{" "}
          <span className="text-accent">
            it is derived from how every machine behaves.
          </span>
        </p>
      </div>

      {/* the rule, as a left-to-right flow */}
      <div className="border border-line bg-bg-2 p-7 mb-8">
        <div className="flex flex-col md:flex-row items-stretch md:items-center gap-3 md:gap-0">
          <div className="flex-1 border border-line bg-bg px-5 py-4 text-center">
            <div className="text-[10px] text-ink-dim tracking-[0.12em] uppercase mb-1">
              step 1
            </div>
            <div className="text-[13px] text-ink">agent requests action</div>
          </div>
          <div className="text-accent text-center px-4 text-[18px] font-mono">
            <span className="hidden md:inline">→</span>
            <span className="md:hidden">↓</span>
          </div>
          <div className="flex-1 border border-line bg-bg px-5 py-4 text-center">
            <div className="text-[10px] text-ink-dim tracking-[0.12em] uppercase mb-1">
              step 2
            </div>
            <div className="text-[13px] text-ink">net routes the request</div>
          </div>
          <div className="text-accent text-center px-4 text-[18px] font-mono">
            <span className="hidden md:inline">→</span>
            <span className="md:hidden">↓</span>
          </div>
          <div className="flex-1 border border-accent-dim bg-accent/[0.04] px-5 py-4 text-center">
            <div className="text-[10px] text-accent tracking-[0.12em] uppercase mb-1">
              step 3
            </div>
            <div className="text-[13px] text-accent">
              resource decides yes / no
            </div>
          </div>
        </div>
      </div>

      <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-px bg-line border border-line mb-12">
        {DECIDERS.map((d) => (
          <div key={d.want} className="bg-bg px-5 py-5">
            <div className="text-[11px] text-ink-dim mb-1.5">
              wants to <span className="text-ink lowercase">{d.want}</span>
            </div>
            <div className="text-[13px] text-accent lowercase leading-[1.4]">
              {d.decides}
            </div>
          </div>
        ))}
      </div>

      <div className="border-l-2 border-accent pl-8 pr-8 py-6 bg-accent/[0.02] max-w-[900px]">
        <p className="text-[18px] text-ink leading-[1.5] font-light">
          There is no giant central permission system to breach. Each machine
          guards its own lane and only exposes what it is willing to provide —{" "}
          <strong className="text-accent font-medium">
            so the safe zone is the mesh itself.
          </strong>
        </p>
      </div>
    </section>
  );
}
