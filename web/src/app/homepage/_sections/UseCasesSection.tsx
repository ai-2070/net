import { SectionLabel } from "@/components/SectionLabel";
import { DisplayHeading } from "@/components/DisplayHeading";

interface UseCase {
  tag: string;
  title: string;
  body: string;
  wedge?: boolean;
}

const USE_CASES: ReadonlyArray<UseCase> = [
  {
    tag: "▸ wedge ─ first market",
    title: "AI agent meshes",
    body: "One agent operating across a user's trusted devices: desktop, laptop, browser, files, terminals, GPUs, local tools, and live streams. The agent doesn't need every capability inside one process. It discovers what exists, asks for bounded access, moves artifacts, and delegates work across the user's fabric.",
    wedge: true,
  },
  {
    tag: "▸ 0x02 ─ sovereign compute",
    title: "Personal sovereign compute",
    body: "A person's machines become one operable surface. Laptop, desktop, NAS, GPU box, phone, cloud VM, and home server coordinate without turning everything into a SaaS account or one brittle control plane.",
  },
  {
    tag: "▸ 0x03 ─ paid inference",
    title: "GPU workers & edge compute",
    body: "Inference capacity advertises models, VRAM, price, queue state, and policy. Agents discover available workers, request bounded compute, stream progress, and retrieve artifacts without hardcoding every endpoint.",
  },
  {
    tag: "▸ 0x04 ─ field systems",
    title: "Robotics & field systems",
    body: "Machines in the field do not get perfect networks. They need local authority, live streams, task lifecycle, artifact movement, and coordination that continues under degraded connectivity.",
  },
  {
    tag: "▸ 0x05 ─ observation",
    title: "Sensor & observation meshes",
    body: "Cameras, microphones, drones, RF sensors, vehicles, and edge processors publish observations, claim resources, bridge subnets, and converge on shared state.",
  },
  {
    tag: "▸ 0x06 ─ physical edge",
    title: "Edge & orbital compute",
    body: "The same primitives extend to edge data centers and orbital or field systems, where latency, bandwidth, energy, and contact windows are physical constraints rather than configuration.",
  },
];

export function UseCasesSection() {
  return (
    <section id="use-cases" className="border-b border-line px-6 py-20">
      <SectionLabel>§06 / wedge &amp; expansion</SectionLabel>
      <DisplayHeading>
        start with agent meshes.
        <br />
        expand to the <span className="text-accent">machine economy.</span>
      </DisplayHeading>

      <p className="text-[16px] text-ink max-w-[820px] leading-[1.6] font-light mb-4">
        The first wedge is immediate: agents need to operate across real
        machines. Hermes needs remote tools, file transfer, script execution,
        live desktop and browser context, and trusted-device federation.
        OpenClaw needs the same fabric for local AI applications.
      </p>
      <p className="text-[13px] text-ink-dim max-w-[820px] leading-[1.7] mb-12">
        Those are not separate infrastructure problems. They are the same
        substrate appearing through different products — and the same primitives
        extend into personal compute, GPU marketplaces, robotics, sensor
        networks, and physical edge.
      </p>

      <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 border-t border-l border-line">
        {USE_CASES.map((u) => (
          <div
            key={u.title}
            className={`border-r border-b border-line p-7 transition-colors hover:bg-bg-2 ${
              u.wedge ? "bg-accent/[0.03]" : ""
            }`}
          >
            <div
              className={`text-[10px] tracking-[0.15em] mb-2 ${
                u.wedge ? "text-accent" : "text-accent-dim"
              }`}
            >
              {u.tag}
            </div>
            <h3 className="font-head text-[19px] leading-tight mb-2.5 tracking-[0.04em] text-ink lowercase">
              {u.title}
            </h3>
            <p className="text-ink-dim text-[12px] leading-[1.6]">{u.body}</p>
          </div>
        ))}
      </div>
    </section>
  );
}
