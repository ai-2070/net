import { SectionLabel } from "./SectionLabel";
import { DisplayHeading } from "./DisplayHeading";

interface UseCase {
  id: string;
  domain: string;
  desc: string;
}

const USE_CASES: readonly UseCase[] = [
  {
    id: "0x01",
    domain: "Agents & tools",
    desc: "No registry, no broker. Hermes runs on Net.",
  },
  {
    id: "0x02",
    domain: "Drones · vehicles · robots",
    desc: "Different vendors, no shared cloud.",
  },
  {
    id: "0x03",
    domain: "API discovery",
    desc: "Every capability searchable at runtime.",
  },
  {
    id: "0x04",
    domain: "Idle compute",
    desc: "Work finds the machine. 44 ns capability check.",
  },
  {
    id: "0x05",
    domain: "Sensor meshes",
    desc: "Queries travel to the data.",
  },
  {
    id: "0x06",
    domain: "Orbital & edge",
    desc: "No ground controller.",
  },
];

export function EverywhereSection() {
  return (
    <section id="everywhere" className="border-b border-line px-6 py-20">
      <SectionLabel>§03 / everywhere</SectionLabel>
      <DisplayHeading>
        everything that
        <br />
        <span className="text-accent">can&apos;t wait.</span>
      </DisplayHeading>

      <p className="text-[16px] text-accent max-w-[740px] leading-[1.6] font-light mb-6">
        Starts with AI agents. Expands wherever machines coordinate.
      </p>

      <p className="text-[16px] text-ink max-w-[740px] leading-[1.6] font-light mb-12">
        Anywhere latency matters. Anywhere the cloud round-trip is too slow.
        Anywhere there&apos;s no central infrastructure to route through.
      </p>

      <div id="apps" className="scroll-mt-20 border-t border-line">
        {USE_CASES.map((u) => (
          <div
            key={u.id}
            className="flex flex-wrap items-baseline gap-x-3 border-b border-line px-1 py-3 text-[12px] transition-colors hover:bg-bg-2"
          >
            <span className="text-accent tracking-[0.08em] shrink-0">
              {u.id} {u.domain}
            </span>
            <span className="text-ink-faint shrink-0">—</span>
            <span className="text-ink-dim leading-[1.6]">{u.desc}</span>
          </div>
        ))}
      </div>

      <div className="border-l-2 border-accent pl-8 pr-8 py-6 bg-accent/[0.02] mt-12 max-w-[900px]">
        <p className="text-[18px] text-ink leading-[1.5] font-light">
          <span className="text-accent">Coordination at machine speed.</span>
        </p>
      </div>
    </section>
  );
}
