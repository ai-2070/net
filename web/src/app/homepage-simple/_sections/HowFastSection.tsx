import { SectionLabel } from "@/components/SectionLabel";
import { DisplayHeading } from "@/components/DisplayHeading";
import { LatencySpectrum } from "@/components/LatencySpectrum";

interface NetworkClass {
  tag: string;
  name: string;
  body: string;
  speed: string;
  relative: string;
  accent?: boolean;
}

const CLASSES: ReadonlyArray<NetworkClass> = [
  {
    tag: "net",
    name: "the operating layer",
    body: "Hits the same real-time speeds on ordinary computers and networks — no special hardware, no central coordinator.",
    speed: "nanoseconds",
    relative: "the baseline",
    accent: true,
  },
  {
    tag: "industrial real-time",
    name: "CAN · EtherCAT · factory buses",
    body: "Reaches near-instant speeds — but only by owning dedicated wiring and special hardware. It cannot run on the open internet.",
    speed: "microseconds",
    relative: "~1,000× slower",
  },
  {
    tag: "today's internet",
    name: "TCP · HTTP · the cloud",
    body: "Built to deliver data reliably, not quickly. Data waits in queues, slows down under load, and often travels to a data center and back.",
    speed: "milliseconds",
    relative: "~1,000,000× slower",
  },
];

interface Stat {
  value: string;
  unit: string;
  note: string;
}

const STATS: ReadonlyArray<Stat> = [
  {
    value: "0.20",
    unit: "ns",
    note: "to route one packet between machines — far faster than a single step a normal program takes.",
  },
  {
    value: "~5",
    unit: "billion/s",
    note: "packets handled per second, on a single processor core.",
  },
  {
    value: "2.6",
    unit: "mb",
    note: "the entire engine — small enough to run on a tiny sensor or device.",
  },
];

export function HowFastSection() {
  return (
    <section id="speed" className="border-b border-line px-6 py-20">
      <SectionLabel>§09 / how fast it is</SectionLabel>
      <DisplayHeading>
        a new class of network.
        <br />
        <span className="text-accent">nanosecond speed.</span>
      </DisplayHeading>

      <p className="text-[16px] text-ink max-w-[760px] leading-[1.6] font-light mb-3">
        There are two kinds of networks today. One is built for reach, the other
        for speed — and normally you cannot have both.
      </p>
      <p className="text-[16px] text-ink max-w-[760px] leading-[1.6] font-light mb-12">
        Net is a third kind:{" "}
        <span className="text-accent">
          real-time speed on ordinary computers and networks.
        </span>
      </p>

      {/* three classes, fastest to slowest — Net first */}
      <div className="grid grid-cols-1 lg:grid-cols-3 border-t border-l border-line">
        {CLASSES.map((c) => (
          <div
            key={c.name}
            className={`border-r border-b border-line p-7 flex flex-col ${
              c.accent ? "bg-accent/[0.03]" : ""
            }`}
          >
            <div
              className={`text-[10px] tracking-[0.16em] uppercase mb-3 ${
                c.accent ? "text-accent" : "text-ink-dim"
              }`}
            >
              {c.tag}
            </div>
            <h3
              className={`font-head text-[17px] leading-tight mb-3 tracking-[0.03em] lowercase ${
                c.accent ? "text-accent" : "text-ink"
              }`}
            >
              {c.name}
            </h3>
            <p className="text-[12px] text-ink-dim leading-[1.6] flex-1">
              {c.body}
            </p>
            <div className="border-t border-dashed border-line mt-4 pt-3 flex items-baseline justify-between gap-2">
              <span
                className={`text-[14px] lowercase ${
                  c.accent ? "text-accent font-semibold" : "text-ink"
                }`}
              >
                {c.speed}
              </span>
              <span
                className={`text-[10px] tracking-[0.04em] ${
                  c.accent ? "text-accent" : "text-ink-dim"
                }`}
              >
                {c.relative}
              </span>
            </div>
          </div>
        ))}
      </div>

      {/* the "compared to others" visual — reused from the index page */}
      <LatencySpectrum />

      {/* plain proof numbers, instead of the dense benchmark table */}
      <div className="mt-10">
        <div className="text-[10px] text-ink-dim tracking-[0.16em] uppercase mb-5">
          existence proofs — measured, not promised
        </div>
        <div className="grid grid-cols-1 sm:grid-cols-3 gap-px bg-line border border-line">
          {STATS.map((s) => (
            <div key={s.note} className="bg-bg p-7">
              <div className="font-display text-accent leading-none mb-3 text-[44px]">
                {s.value}
                <span className="text-ink-dim text-[18px] ml-1">{s.unit}</span>
              </div>
              <p className="text-[12px] text-ink-dim leading-[1.6]">{s.note}</p>
            </div>
          ))}
        </div>
        <div className="mt-5 flex flex-wrap items-center justify-between gap-x-6 gap-y-2">
          <p className="text-[11px] text-ink-dim leading-[1.6] max-w-[620px]">
            The takeaway is simple: the software is no longer the bottleneck.
            What is left is physics — the wire and the speed of light.
          </p>
          <a
            href="https://github.com/ai-2070/net/blob/master/net/crates/net/BENCHMARKS.md"
            target="_blank"
            rel="noopener noreferrer"
            className="inline-flex items-center gap-1.5 text-[11px] font-mono text-accent tracking-[0.05em] hover:text-ink transition-colors shrink-0"
          >
            ▸ See the full benchmarks <span className="text-ink-faint">↗</span>
          </a>
        </div>
      </div>
    </section>
  );
}
