import { SectionLabel } from "@/components/SectionLabel";
import { DisplayHeading } from "@/components/DisplayHeading";

interface Capability {
  num: string;
  title: string;
  body: string;
}

const CAPABILITIES: ReadonlyArray<Capability> = [
  {
    num: "01",
    title: "Find what is available",
    body: "Agents discover trusted machines, tools, apps, and compute instead of relying on hardcoded integrations.",
  },
  {
    num: "02",
    title: "Understand what each resource can do",
    body: "Each resource describes its capabilities clearly: run a script, open a file, use a GPU, stream data, control a browser.",
  },
  {
    num: "03",
    title: "Ask for access",
    body: "Agents request specific actions. They do not automatically own every machine or tool they can see.",
  },
  {
    num: "04",
    title: "Run work in the right place",
    body: "A task can run on the laptop, desktop, server, GPU box, or edge device best suited for it.",
  },
  {
    num: "05",
    title: "Move files and results",
    body: "Inputs, outputs, logs, model files, and generated media move between machines in a durable way.",
  },
  {
    num: "06",
    title: "Watch live information",
    body: "Agents react to streams: browser state, sensor feeds, process logs, task progress, or live app data.",
  },
  {
    num: "07",
    title: "Reserve scarce resources",
    body: "Expensive resources like GPUs can be claimed before work starts, so two agents do not fight over them.",
  },
  {
    num: "08",
    title: "Keep long work alive",
    body: "Jobs do not have to die just because a chat message ended or a device briefly disconnected.",
  },
  {
    num: "09",
    title: "Keep control local",
    body: "The machine or service that owns the resource decides what is allowed. Net coordinates; the resource enforces.",
  },
];

export function CapabilityCardsSection() {
  return (
    <section id="capabilities" className="border-b border-line px-6 py-20">
      <SectionLabel>§05 / capabilities</SectionLabel>
      <DisplayHeading>
        what net
        <br />
        makes <span className="text-accent">possible.</span>
      </DisplayHeading>

      <p className="text-[16px] text-ink max-w-[720px] leading-[1.6] font-light mb-12">
        Nine things an agent can do the moment it is connected — each one in
        plain terms, no networking knowledge required.
      </p>

      <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 border-t border-l border-line">
        {CAPABILITIES.map((c) => (
          <div
            key={c.num}
            className="border-r border-b border-line p-7 transition-colors hover:bg-bg-2 flex flex-col"
          >
            <div className="text-accent text-[10px] tracking-[0.18em] mb-2.5">
              ▸ {c.num}
            </div>
            <h3 className="font-head text-[18px] leading-tight text-ink mb-3 tracking-[0.03em] lowercase">
              {c.title}
            </h3>
            <p className="text-[12px] text-ink-dim leading-[1.6]">{c.body}</p>
          </div>
        ))}
      </div>
    </section>
  );
}
