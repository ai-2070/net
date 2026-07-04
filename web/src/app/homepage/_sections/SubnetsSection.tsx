import { SectionLabel } from "@/components/SectionLabel";
import { DisplayHeading } from "@/components/DisplayHeading";

const SCOPES: ReadonlyArray<string> = [
  "user subnet",
  "gpu subnet",
  "robotics subnet",
  "cloud subnet",
  "partner subnet",
];

export function SubnetsSection() {
  return (
    <section id="subnets" className="border-b border-line px-6 py-20">
      <SectionLabel>§08 / subnets &amp; scaling</SectionLabel>
      <DisplayHeading>
        scale by adding scopes,
        <br />
        not <span className="text-accent">flattening the world.</span>
      </DisplayHeading>

      <p className="text-[16px] text-ink max-w-[820px] leading-[1.6] font-light mb-4">
        Most distributed systems eventually invent boundaries: teams, regions,
        devices, trust zones, missions, buildings, fleets, customers,
        datacenters, orbital windows. Net treats those boundaries as
        first-class.
      </p>
      <p className="text-[13px] text-ink-dim max-w-[820px] leading-[1.7] mb-10">
        Subnets are coordination scopes — physical, logical, organizational,
        latency-based, trust-based, or application-specific. Participants
        coordinate locally inside a subnet. Bridges expose selected
        capabilities, streams, and artifacts across boundaries.
      </p>

      <div className="border border-line bg-bg-2 p-7">
        <div className="text-[10px] text-ink-dim tracking-[0.14em] uppercase mb-5">
          a mesh of scopes, joined by bridges
        </div>
        <div className="flex flex-wrap items-center gap-3">
          {SCOPES.map((s, i) => (
            <div key={s} className="flex items-center gap-3">
              <span className="border border-accent-dim text-accent px-4 py-2 text-[12px] lowercase tracking-[0.03em] bg-accent/[0.03]">
                {s}
              </span>
              {i < SCOPES.length - 1 && (
                <span className="text-ink-faint font-mono text-[12px]">
                  ╫ bridge
                </span>
              )}
            </div>
          ))}
        </div>
        <div className="border-t border-dashed border-line mt-6 pt-4 max-w-[760px]">
          <p className="text-[13px] text-ink-dim leading-[1.65]">
            The result is not one global registry trying to know everything. It
            is a mesh of scopes that can grow{" "}
            <span className="text-accent">without losing local authority.</span>
          </p>
        </div>
      </div>
    </section>
  );
}
