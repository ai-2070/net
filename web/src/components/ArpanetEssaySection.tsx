import { ArpanetMapBg } from "./ArpanetMapBg";
import { DisplayHeading } from "./DisplayHeading";
import { SectionLabel } from "./SectionLabel";

interface EraItem {
  label: string;
  body: string;
}

const ERA_ITEMS: readonly EraItem[] = [
  { label: "THEN", body: "Packets scarce, delivery sacred" },
  { label: "NOW", body: "Data infinite, processing scarce" },
  { label: "NET", body: "Drop, route around, observe, derive" },
];

export function ArpanetEssaySection() {
  return (
    <section
      id="why-now"
      className="relative overflow-hidden border-b border-line px-6 py-20"
    >
      <ArpanetMapBg />
      <div className="relative">
        <SectionLabel>§06 / why now</SectionLabel>
        <DisplayHeading>
          arpanet assumed scarcity.
          <br />
          <span className="text-accent">net assumes abundance.</span>
        </DisplayHeading>

        <p className="text-[16px] text-ink max-w-[740px] leading-[1.6] font-light mb-10">
          TCP was built for 1969: packets were precious, so the network promised
          delivery. Today the firehose has no pause button — sensors don&apos;t
          stop, streams don&apos;t wait. Guaranteed delivery just guarantees
          burying the receiver. Arrival doesn&apos;t equal usefulness.
        </p>

        <div className="grid grid-cols-1 sm:grid-cols-3 gap-8 mb-12">
          {ERA_ITEMS.map((e) => (
            <div key={e.label} className="border-t border-accent-dim pt-4">
              <div className="text-[11px] text-accent uppercase tracking-[0.15em] mb-2">
                {e.label}
              </div>
              <div className="text-ink-dim text-[12px] leading-[1.6]">
                {e.body}
              </div>
            </div>
          ))}
        </div>

        <p className="text-[16px] text-ink max-w-[740px] leading-[1.6] font-light">
          What&apos;s left is physics: NIC, wire, speed of light. Machines 5 km
          apart could coordinate in ~33 μs — most systems run hundreds of times
          slower. The bottleneck is software.
        </p>
      </div>
    </section>
  );
}
