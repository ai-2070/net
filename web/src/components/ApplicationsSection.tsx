import { FieldLabel as SectionLabel } from "./field/FieldLabel";
import { FieldHeading as DisplayHeading } from "./field/FieldHeading";

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

export function ApplicationsSection() {
  return (
    <section
      id="apps"
      className="relative pl-6 pr-12 lg:pl-10 lg:pr-24 py-24 lg:py-32"
    >
      <SectionLabel>§11 / target applications</SectionLabel>
      <DisplayHeading>
        everything that
        <br />
        can&apos;t wait.
      </DisplayHeading>

      <p className="text-[19px] md:text-[21px] text-ink max-w-[34em] leading-[1.5] font-light mb-14">
        Anywhere latency matters. Anywhere the cloud round-trip is too slow.
        Anywhere there&apos;s no central infrastructure to route through.
      </p>

      <div className="grid grid-cols-1 md:grid-cols-2 gap-5 lg:gap-6">
        {APPS.map((a) => (
          <div key={a.title} className="slab slab-lift p-8 flex flex-col">
            <h3 className="font-display font-bold text-[24px] leading-[1.05] mb-3 tracking-[-0.015em] text-ink">
              {a.title}
            </h3>
            <p className="text-ink-dim text-[14px] leading-[1.6] max-w-[38em] mb-6">
              {a.body}
            </p>
            <div className="stencil mt-auto pt-4 border-t border-line normal-case tracking-[0.16em]">
              {a.tag}
            </div>
          </div>
        ))}
      </div>
    </section>
  );
}
