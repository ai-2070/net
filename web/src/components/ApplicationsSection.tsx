import Image from "next/image";
import Link from "next/link";
import { FieldLabel as SectionLabel } from "./field/FieldLabel";
import { FieldHeading as DisplayHeading } from "./field/FieldHeading";

interface AppCard {
  tag: string;
  title: string;
  body: string;
  image: string;
  alt: string;
  href?: string;
}

const APPS: readonly AppCard[] = [
  {
    tag: "▸ 0x01 ─ ai agents",
    title: "AI Agents",
    image: "/field/app-ai-agents.jpg",
    alt: "Rows of datacenter server racks.",
    body: "Tool calls, state, and memory transfer between heterogeneous GPU nodes. Token streams flow through the mesh; an agent's working memory follows it from node to node mid-conversation. The mesh is the runtime.",
  },
  {
    tag: "▸ 0x02 ─ vehicular mesh",
    title: "Vehicular Sensor Mesh",
    image: "/field/app-vehicular.jpg",
    alt: "Lidar point cloud of a San Francisco street intersection.",
    body: "Cars sharing LIDAR, radar, camera. Vehicles sync intent — braking, turning, route changes. The car behind doesn't react to braking. It knows about the braking before the brake pads touch the rotor.",
  },
  {
    tag: "▸ 0x03 ─ factory floor",
    title: "Robotics Factory Floor",
    image: "/field/app-robotics.jpg",
    alt: "Industrial robot arms along a car assembly line.",
    body: "Robots don't need line-of-sight for networking. The mesh routes through whatever nodes are reachable. Reroute scheduled in sub-microsecond time. The assembly line doesn't stop.",
  },
  {
    tag: "▸ 0x04 ─ energy grids & extraction",
    title: "Energy Grids & Extraction",
    image: "/field/app-energy.jpg",
    alt: "An electrical substation with transmission towers in winter.",
    body: "Electrical substations, oil and gas pipelines, drilling rigs, mine haul trucks, distributed solar — coordinating in real time across geographies that fiber doesn't reach. Protective relays trip in single-digit milliseconds; the mesh isolates faults before they cascade. Routes through whatever radios and edge boxes survive.",
  },
  {
    tag: "▸ 0x05 ─ remote surgery",
    title: "Remote Surgery",
    image: "/field/app-surgery.jpg",
    alt: "Surgeons working at a robotic surgical system in an operating room.",
    body: "Control signals and haptic feedback routed across the mesh. If the primary compute node lags, the mesh reroutes mid-operation. The surgeon doesn't notice. The patient doesn't notice. The scalpel doesn't stop.",
  },
  {
    tag: "▸ 0x06 ─ drone swarms",
    title: "Drone Swarms",
    image: "/field/app-drones.webp",
    alt: "A quadcopter drone in flight above a forest road.",
    body: "Coordinated flight without a ground controller. A drone that loses a motor broadcasts the failure; the swarm adjusts formation before the drone has begun to fall.",
  },
  {
    tag: "▸ 0x07 ─ live performance",
    title: "Live Performance",
    image: "/field/app-performance.jpg",
    alt: "Rear of a concert mixing console and amplifier racks lit blue.",
    body: "Lighting, audio, video, pyro synchronized across hundreds of nodes. A DMX controller dies, another node picks up the cue list. Audio sync tighter than the speed of sound across the venue.",
  },
  {
    tag: "▸ 0x08 ─ multiplayer games",
    title: "Multiplayer Games",
    image: "/field/games-reel-poster.jpg",
    href: "/games",
    alt: "Rose & Blade, a multiplayer sword fight built on NET: players clashing in a tournament arena.",
    body: "Multiplayer for any game engine, plug and play for Three.js. Players connect directly, one player hosts or several share a match, and there is no game server to run.",
  },
];

export function ApplicationsSection() {
  return (
    <section id="apps" className="relative px-6 lg:px-12 py-24 lg:py-32">
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
        {APPS.map((a) => {
          const still = (
            <Image
              src={a.image}
              alt={a.alt}
              fill
              sizes="(min-width: 768px) 45vw, 92vw"
              className="object-cover grayscale contrast-110 brightness-[0.7] transition-[filter] duration-500 group-hover:grayscale-0 group-hover:brightness-90 group-focus-visible:grayscale-0"
            />
          );
          const media = still;
          const inner = (
            <>
              <div className="relative -mx-8 -mt-8 mb-7 aspect-[16/7] overflow-hidden">
                {media}
              </div>
              <h3 className="font-display font-bold text-[24px] leading-[1.05] mb-3 tracking-[-0.015em] text-ink">
                {a.title}
              </h3>
              <p className="text-ink-dim text-[16px] leading-[1.55] max-w-[38em] mb-6">
                {a.body}
              </p>
              <div className="stencil mt-auto pt-4 border-t border-line normal-case tracking-[0.16em]">
                {a.tag}
              </div>
            </>
          );
          return a.href ? (
            <Link
              key={a.title}
              href={a.href}
              className="group slab slab-lift p-8 flex flex-col no-underline"
            >
              {inner}
            </Link>
          ) : (
            <div
              key={a.title}
              className="group slab slab-lift p-8 flex flex-col"
            >
              {inner}
            </div>
          );
        })}
      </div>
    </section>
  );
}
