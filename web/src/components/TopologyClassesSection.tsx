import { FieldHeading as DisplayHeading } from "./field/FieldHeading";
import { Lede } from "./field/Lede";
import { LatencySpectrum } from "./LatencySpectrum";
import { FieldLabel as SectionLabel } from "./field/FieldLabel";

interface TopologyClass {
  header: string;
  headerColor: "ink-dim" | "accent";
  title: string;
  titleColor: "ink" | "accent";
  body: string;
  floor: string;
  floorColor: "ink" | "accent";
  throughput: string;
}

const TOPOLOGY_CLASSES: readonly TopologyClass[] = [
  {
    header: "// net",
    headerColor: "accent",
    title: "NET → latency-first",
    titleColor: "accent",
    body: "The internet runs in milliseconds. NET runs in nanoseconds. Commodity hardware, commodity networks, no central coordination. Drop, route around, observe, derive.",
    floor: "nanoseconds",
    floorColor: "accent",
    throughput: "~20M events/s · per core",
  },
  {
    header: "// real-time",
    headerColor: "ink-dim",
    title: "CAN / EtherCAT / TSN",
    titleColor: "ink",
    body: "Specialized hardware, optimized for deterministic timing. Fixed topologies. Dedicated hardware. Time-slotted access. Guarantees only because you own the wire.",
    floor: "microseconds†",
    floorColor: "ink",
    throughput: "~100K updates/s · dedicated bus",
  },
  {
    header: "// best-effort",
    headerColor: "ink-dim",
    title: "TCP / IP / HTTP / gRPC",
    titleColor: "ink",
    body: "Optimized for delivery. Queues absorb bursts. Backpressure negotiated. Connections stateful. Trust assumed. Sender slows down when receiver can't keep up.",
    floor: "milliseconds",
    floorColor: "ink",
    throughput: "~10K req/s · per connection",
  },
];

export function TopologyClassesSection() {
  return (
    <section id="topology" className="relative px-6 lg:px-12 py-24 lg:py-32">
      <SectionLabel>§02 / topology classes</SectionLabel>
      <DisplayHeading>a new class of system.</DisplayHeading>

      <Lede>
        Existing networking falls into two categories. NET is neither.
      </Lede>

      <div className="topology-grid grid grid-cols-1 lg:grid-cols-3 gap-5 lg:gap-6 lg:items-end lg:pt-6 lg:border-b lg:border-line">
        {TOPOLOGY_CLASSES.map((c, i) => (
          <div
            key={c.title}
            className={`slab flex flex-col px-7 pt-6 pb-7 ${
              i === 0
                ? "slab-held field-sync lg:mb-24"
                : i === 1
                  ? "lg:mb-10"
                  : ""
            }`}
          >
            <div
              className={`font-display font-bold text-[24px] leading-[1.08] tracking-[-0.015em] ${c.titleColor === "accent" ? "text-accent" : "text-ink"} mb-4`}
            >
              {c.title}
            </div>
            <p className="text-ink-dim text-[16px] leading-[1.55] flex-1">
              {c.body}
            </p>
            <div className="mt-6 font-mono text-[13px] text-ink-dim space-y-1.5">
              <div>
                latency floor:{" "}
                <b
                  className={`${c.floorColor === "accent" ? "text-accent" : "text-ink"} font-semibold`}
                >
                  {c.floor}
                </b>
              </div>
              <div>
                throughput:{" "}
                <b
                  className={`${c.floorColor === "accent" ? "text-accent" : "text-ink"} font-semibold`}
                >
                  {c.throughput}
                </b>
              </div>
            </div>
            <div
              className={`${c.headerColor === "accent" ? "text-accent" : "text-ink-dim"} font-mono text-[12px] tracking-[0.18em] uppercase pt-4 mt-5 border-t border-line`}
            >
              {c.header}
            </div>
          </div>
        ))}
      </div>

      <LatencySpectrum />
    </section>
  );
}
