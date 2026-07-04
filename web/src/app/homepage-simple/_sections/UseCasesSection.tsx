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
    tag: "▸ 01 · the wedge",
    title: "AI agent & tool federation",
    body: "Every agent, tool, and app becomes discoverable and callable by every other. Agents work across your trusted machines instead of each product wiring its own private bridges.",
    wedge: true,
  },
  {
    tag: "▸ 02",
    title: "API discovery",
    body: "The internet had no map until Google. APIs have none until Net. Services announce what they do, and agents find and call them by capability — not hardcoded endpoints.",
  },
  {
    tag: "▸ 03",
    title: "GPU & server efficiency",
    body: "Idle compute anywhere becomes usable capacity. Work flows to whichever machine has a free GPU, so expensive hardware runs hot instead of sitting reserved and idle.",
  },
  {
    tag: "▸ 04",
    title: "Drones, vehicles & factory robotics",
    body: "Machines coordinate faster than they can physically react. A robot behind a steel column relays through one that isn't; cars share intent before the brake pads touch the rotor.",
  },
  {
    tag: "▸ 05",
    title: "Nanorobotics & sensory meshes",
    body: "Swarms of tiny machines and sensors — in the body, in a field, across a city — publish observations and converge on shared state with no cloud round-trip.",
  },
  {
    tag: "▸ 06",
    title: "Orbital & edge compute",
    body: "Satellites, ships, and remote sites coordinate under latency, bandwidth, and contact-window limits that break cloud-first designs. Work moves to where the data and power already are.",
  },
  {
    tag: "▸ 07",
    title: "Remote surgery & telemedicine",
    body: "Control signals and haptics route across the mesh. If a compute node lags, the work reroutes mid-operation. The scalpel doesn't stop.",
  },
  {
    tag: "▸ 08",
    title: "Personal sovereign compute",
    body: "Your laptop, phone, NAS, home server, and GPU box become one fabric an agent can operate with permission — no SaaS account, no cloud middleman.",
  },
  {
    tag: "▸ 09",
    title: "Energy & infrastructure",
    body: "Grids, pipelines, plants, and ports coordinate in real time across geographies fiber doesn't reach. Faults get isolated before they cascade.",
  },
];

export function UseCasesSection() {
  return (
    <section id="use-cases" className="border-b border-line px-6 py-20">
      <SectionLabel>§02 / use cases</SectionLabel>
      <DisplayHeading>
        from agents
        <br />
        <span className="text-accent">to orbit.</span>
      </DisplayHeading>

      <p className="text-[16px] text-ink max-w-[820px] leading-[1.6] font-light mb-12">
        It starts with AI agents working across your trusted machines — then the
        same coordination layer expands everywhere machines meet: robots,
        sensors, GPUs, factories, and satellites.
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
            <h3 className="font-head text-[19px] leading-tight mb-2.5 tracking-[0.03em] text-ink lowercase">
              {u.title}
            </h3>
            <p className="text-ink-dim text-[12px] leading-[1.6]">{u.body}</p>
          </div>
        ))}
      </div>
    </section>
  );
}
