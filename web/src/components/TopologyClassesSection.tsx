import { DisplayHeading } from "./DisplayHeading";
import { LatencySpectrum } from "./LatencySpectrum";
import { SectionLabel } from "./SectionLabel";

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
    <section id="topology" className="border-b border-line px-6 py-20">
      <SectionLabel>§02 / topology classes</SectionLabel>
      <DisplayHeading>a new class of system.</DisplayHeading>

      <p className="text-[16px] text-ink max-w-[740px] leading-[1.6] font-light mb-12">
        Existing networking falls into two categories. NET is neither.
      </p>

      <div className="grid grid-cols-1 lg:grid-cols-3 border border-line border-b-0">
        {TOPOLOGY_CLASSES.map((c, i) => (
          <div
            key={c.title}
            className={`bg-bg-2 ${c.headerColor === "accent" ? "text-accent" : "text-ink-dim"} text-[10px] tracking-[0.18em] uppercase px-6 py-3 border-b border-line ${i < 2 ? "lg:border-r" : ""}`}
          >
            {c.header}
          </div>
        ))}
        {TOPOLOGY_CLASSES.map((c, i) => (
          <div
            key={c.title + "-body"}
            className={`flex flex-col px-6 py-7 border-b border-line ${i < 2 ? "lg:border-r" : ""}`}
          >
            <div
              className={`font-head text-[18px] leading-tight ${c.titleColor === "accent" ? "text-accent" : "text-ink"} mb-3.5 tracking-[0.04em] lowercase`}
            >
              {c.title}
            </div>
            <div className="text-ink-dim text-[12px] leading-[1.6] flex-1">
              {c.body}
            </div>
            <div className="mt-4 text-[11px] text-ink-dim border-t border-dashed border-ink-faint pt-3 space-y-1">
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
          </div>
        ))}
      </div>

      <LatencySpectrum />
    </section>
  );
}
