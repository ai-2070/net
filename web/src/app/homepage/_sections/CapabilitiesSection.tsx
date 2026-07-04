import { SectionLabel } from "@/components/SectionLabel";
import { DisplayHeading } from "@/components/DisplayHeading";

interface Primitive {
  need: string;
  primitive: string;
  why: string;
}

const PRIMITIVES: ReadonlyArray<Primitive> = [
  {
    need: "Find machines, services, and agents",
    primitive: "Discovery",
    why: "Agents cannot operate what they cannot find.",
  },
  {
    need: "Know what each participant can do",
    primitive: "Typed capabilities",
    why: "Capabilities become contracts, not labels.",
  },
  {
    need: "Call services across the mesh",
    primitive: "nRPC",
    why: "Remote work becomes typed and observable.",
  },
  {
    need: "Move files, results, and model artifacts",
    primitive: "CAS / artifacts",
    why: "Outputs become durable, addressable, and transferable.",
  },
  {
    need: "Watch live state",
    primitive: "Streams",
    why: "Agents can react to the world as it changes.",
  },
  {
    need: "Track long-running work",
    primitive: "Durable tasks",
    why: "Work survives beyond a single request.",
  },
  {
    need: "Avoid double-booking scarce resources",
    primitive: "Claims",
    why: "GPUs, files, devices, and tools can be reserved before use.",
  },
  {
    need: "Keep operating through churn",
    primitive: "Replayable state",
    why: "Participants recover after disconnects and partitions.",
  },
  {
    need: "Scale across teams, places, and trust zones",
    primitive: "Subnets",
    why: "Coordination can be scoped instead of globally flattened.",
  },
  {
    need: "Keep authority local",
    primitive: "Resource-bound policy",
    why: "The owner of the consequence makes the decision.",
  },
];

export function CapabilitiesSection() {
  return (
    <section id="capabilities" className="border-b border-line px-6 py-20">
      <SectionLabel>§04 / capabilities</SectionLabel>
      <DisplayHeading>
        the primitives
        <br />
        agents need <span className="text-accent">after chat.</span>
      </DisplayHeading>

      <p className="text-[16px] text-ink max-w-[760px] leading-[1.6] font-light mb-12">
        Not a worldview. A concrete set of primitives — each one a thing an
        operating agent does the moment it leaves a single process.
      </p>

      <div className="border border-line">
        {/* table header — desktop only; rows stack on mobile */}
        <div className="hidden md:grid grid-cols-[1.4fr_1fr_1.6fr] border-b border-line text-[10px] tracking-[0.14em] uppercase text-ink-dim bg-bg-2">
          <div className="px-5 py-3">Product need</div>
          <div className="px-5 py-3 border-l border-line">Net primitive</div>
          <div className="px-5 py-3 border-l border-line">Why it matters</div>
        </div>

        {PRIMITIVES.map((p, i) => (
          <div
            key={p.primitive}
            className={`flex flex-col md:grid md:grid-cols-[1.4fr_1fr_1.6fr] transition-colors hover:bg-bg-2 ${
              i > 0 ? "border-t border-line" : ""
            }`}
          >
            <div className="px-5 py-4 text-[13px] text-ink-dim leading-[1.5]">
              <span className="md:hidden text-[9px] tracking-[0.14em] uppercase text-ink-faint block mb-1">
                need
              </span>
              {p.need}
            </div>
            <div className="px-5 py-4 text-[14px] text-accent font-medium lowercase tracking-[0.03em] leading-[1.4] md:border-l md:border-line">
              {p.primitive}
            </div>
            <div className="px-5 py-4 text-[12px] text-ink-dim leading-[1.55] md:border-l md:border-line">
              {p.why}
            </div>
          </div>
        ))}
      </div>
    </section>
  );
}
