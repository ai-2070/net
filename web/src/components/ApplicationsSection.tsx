import { SectionLabel } from "./SectionLabel";
import { DisplayHeading } from "./DisplayHeading";

interface AppCard {
  tag: string;
  title: string;
  body: string;
}

const APPS: readonly AppCard[] = [
  {
    tag: "▸ 0x01 ─ ai agents",
    title: "AI Agents",
    body: "Tool calls, state, and memory transfer between heterogeneous GPU nodes. Token streams flow through the mesh; an agent's working memory follows it from node to node mid-conversation. The mesh is the runtime.",
  },
  {
    tag: "▸ 0x02 ─ vehicular mesh",
    title: "Vehicular Sensor Mesh",
    body: "Cars sharing LIDAR, radar, camera. Vehicles sync intent — braking, turning, route changes. The car behind doesn't react to braking. It knows about the braking before the brake pads touch the rotor.",
  },
  {
    tag: "▸ 0x03 ─ factory floor",
    title: "Robotics Factory Floor",
    body: "Robots don't need line-of-sight for networking. The mesh routes through whatever nodes are reachable. Reroute scheduled in sub-microsecond time. The assembly line doesn't stop.",
  },
  {
    tag: "▸ 0x04 ─ energy grids & extraction",
    title: "Energy Grids & Extraction",
    body: "Electrical substations, oil and gas pipelines, drilling rigs, mine haul trucks, distributed solar — coordinating in real time across geographies that fiber doesn't reach. Protective relays trip in single-digit milliseconds; the mesh isolates faults before they cascade. Routes through whatever radios and edge boxes survive.",
  },
  {
    tag: "▸ 0x05 ─ remote surgery",
    title: "Remote Surgery",
    body: "Control signals and haptic feedback routed across the mesh. If the primary compute node lags, the mesh reroutes mid-operation. The surgeon doesn't notice. The patient doesn't notice. The scalpel doesn't stop.",
  },
  {
    tag: "▸ 0x06 ─ drone swarms",
    title: "Drone Swarms",
    body: "Coordinated flight without a ground controller. A drone that loses a motor broadcasts the failure; the swarm adjusts formation before the drone has begun to fall.",
  },
  {
    tag: "▸ 0x07 ─ live performance",
    title: "Live Performance",
    body: "Lighting, audio, video, pyro synchronized across hundreds of nodes. A DMX controller dies, another node picks up the cue list. Audio sync tighter than the speed of sound across the venue.",
  },
  {
    tag: "▸ 0x08 ─ medical nanorobotics",
    title: "Medical Nanorobotics",
    body: "Swarms of nanoscale machines coordinating in vivo — drug-delivery vectors, targeted ablation, vascular monitoring. Sub-microsecond reroute when a node leaves the swarm. No cloud round-trip; the patient is the network.",
  },
];

export function ApplicationsSection({
  id = "apps",
  label = "§11 / target applications",
}: {
  id?: string;
  label?: string;
} = {}) {
  return (
    <section id={id} className="border-b border-line px-6 py-20">
      <SectionLabel>{label}</SectionLabel>
      <DisplayHeading>
        everything that
        <br />
        can&apos;t wait.
      </DisplayHeading>

      <p className="text-[16px] text-ink max-w-[740px] leading-[1.6] font-light mb-12">
        Anywhere latency matters. Anywhere the cloud round-trip is too slow.
        Anywhere there&apos;s no central infrastructure to route through.
      </p>

      <div className="grid grid-cols-1 md:grid-cols-2 border-t border-l border-line">
        {APPS.map((a) => (
          <div
            key={a.title}
            className="border-r border-b border-line p-7 transition-colors hover:bg-bg-2 relative"
          >
            <div className="text-accent text-[10px] tracking-[0.15em] mb-2">
              {a.tag}
            </div>
            <h3 className="font-head text-[20px] leading-tight mb-2.5 tracking-[0.04em] text-ink lowercase">
              {a.title}
            </h3>
            <p className="text-ink-dim text-[12px] leading-[1.6]">{a.body}</p>
          </div>
        ))}
      </div>
    </section>
  );
}
