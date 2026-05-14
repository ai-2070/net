import { DisplayHeading } from "./DisplayHeading";
import { SectionLabel } from "./SectionLabel";

interface BenchRow {
  op: string;
  m1: { ns: string; rate: string };
  i9: { ns: string; rate: string };
}

interface BenchGroup {
  group: string;
  rows: readonly BenchRow[];
}

const BENCH_GROUPS: readonly BenchGroup[] = [
  {
    group: "▸ routing",
    rows: [
      {
        op: "routing header forward",
        m1: { ns: "0.57 ns", rate: "1.75G/s" },
        i9: { ns: "0.20 ns", rate: "5.06G/s" },
      },
      {
        op: "header serialize",
        m1: { ns: "1.98 ns", rate: "505M/s" },
        i9: { ns: "1.31 ns", rate: "762M/s" },
      },
      {
        op: "routing lookup (hit)",
        m1: { ns: "38 ns", rate: "26.3M/s" },
        i9: { ns: "38 ns", rate: "26.7M/s" },
      },
    ],
  },
  {
    group: "▸ multi-hop forwarding",
    rows: [
      {
        op: "1 hop",
        m1: { ns: "59 ns", rate: "16.9M/s" },
        i9: { ns: "53 ns", rate: "18.7M/s" },
      },
      {
        op: "3 hops",
        m1: { ns: "163 ns", rate: "6.13M/s" },
        i9: { ns: "121 ns", rate: "8.29M/s" },
      },
      {
        op: "5 hops",
        m1: { ns: "274 ns", rate: "3.66M/s" },
        i9: { ns: "190 ns", rate: "5.27M/s" },
      },
    ],
  },
  {
    group: "▸ failure detection & recovery",
    rows: [
      {
        op: "heartbeat",
        m1: { ns: "29 ns", rate: "34.5M/s" },
        i9: { ns: "35 ns", rate: "28.4M/s" },
      },
      {
        op: "circuit breaker check",
        m1: { ns: "13 ns", rate: "74.4M/s" },
        i9: { ns: "10 ns", rate: "98.4M/s" },
      },
      {
        op: "full fail + recover",
        m1: { ns: "288 ns", rate: "3.47M/s" },
        i9: { ns: "255 ns", rate: "3.92M/s" },
      },
    ],
  },
  {
    group: "▸ swarm / discovery",
    rows: [
      {
        op: "pingwave roundtrip",
        m1: { ns: "0.93 ns", rate: "1.07G/s" },
        i9: { ns: "0.65 ns", rate: "1.55G/s" },
      },
      {
        op: "new peer discovery",
        m1: { ns: "113 ns", rate: "8.83M/s" },
        i9: { ns: "152 ns", rate: "6.59M/s" },
      },
    ],
  },
  {
    group: "▸ capability system",
    rows: [
      {
        op: "filter (require GPU)",
        m1: { ns: "4.05 ns", rate: "247M/s" },
        i9: { ns: "1.78 ns", rate: "561M/s" },
      },
      {
        op: "GPU check",
        m1: { ns: "0.31 ns", rate: "3.21G/s" },
        i9: { ns: "0.20 ns", rate: "5.01G/s" },
      },
    ],
  },
];

export function BenchmarksSection() {
  return (
    <section id="bench" className="border-b border-line px-6 py-20">
      <SectionLabel>§04 / measured numbers</SectionLabel>
      <DisplayHeading>existence proofs.</DisplayHeading>

      <p className="text-[16px] text-ink max-w-[740px] leading-[1.6] font-light mb-12">
        All numbers measure packet scheduling — the time to process, route,
        encrypt, and queue a packet for transmission. They do not include NIC
        transfer or wire latency.
      </p>

      <div className="grid grid-cols-1 lg:grid-cols-[2fr_1fr] gap-8 items-start">
        <div className="border border-line bg-bg-2">
          <table className="w-full border-collapse text-[12px]">
            <thead>
              <tr>
                <th className="text-left px-4 py-3 bg-bg text-ink-dim text-[10px] tracking-[0.12em] uppercase font-medium border-b border-line">
                  operation
                </th>
                <th className="text-right px-4 py-3 bg-bg text-ink-dim text-[10px] tracking-[0.12em] uppercase font-medium border-b border-line">
                  M1 Max
                </th>
                <th className="text-right px-4 py-3 bg-bg text-ink-dim text-[10px] tracking-[0.12em] uppercase font-medium border-b border-line">
                  i9-14900K
                </th>
              </tr>
            </thead>
            <tbody>
              {BENCH_GROUPS.map((g, gi) => (
                <BenchTableGroup
                  key={g.group}
                  group={g}
                  isLastGroup={gi === BENCH_GROUPS.length - 1}
                />
              ))}
            </tbody>
          </table>
        </div>

        <div className="border border-line p-6 bg-bg-2">
          <BenchKpi
            label="// scheduling floor"
            value="0.20"
            unit="ns"
            note="Routing header forward on i9-14900K. Per-packet overhead. Software is not the bottleneck — physics is."
          />
          <hr className="border-0 border-t border-line my-5" />
          <BenchKpi
            label="// hot path"
            value="5.06"
            unit="G/s"
            note="Operations per second on a single core for the forward path. Five billion. Per second. Per core."
          />
          <hr className="border-0 border-t border-line my-5" />
          <BenchKpi
            label="// SDK ingest"
            value="6.97"
            unit="M/s"
            note='Python via PyO3 batch ingest. The "slow" binding language hits seven million events per second.'
          />
          <hr className="border-0 border-t border-line my-5" />
          <h4 className="text-[10px] tracking-[0.15em] text-ink-dim uppercase mb-4 font-medium">
            // test systems
          </h4>
          <p className="text-[10px] text-ink-dim leading-[1.8] tracking-[0.05em]">
            <b className="text-ink font-medium">► M1 Max</b> macOS, aarch64
            <br />
            <b className="text-ink font-medium">► i9-14900K</b> @5GHz, Win11
            <br />
            <b className="text-ink font-medium">► date</b> 2026-04-27
            <br />
            <b className="text-ink font-medium">► profile</b> release + LTO +
            CG=1
          </p>
          <hr className="border-0 border-t border-line my-5" />
          <a
            href="https://github.com/ai-2070/net/blob/master/net/crates/net/BENCHMARKS.md"
            target="_blank"
            rel="noopener noreferrer"
            className="inline-flex items-center gap-1.5 text-[11px] font-mono text-accent tracking-[0.05em] hover:text-ink transition-colors"
          >
            ▸ BENCHMARKS.md
            <span className="text-ink-faint">↗</span>
          </a>
        </div>
      </div>
    </section>
  );
}

function BenchTableGroup({
  group,
  isLastGroup,
}: {
  group: BenchGroup;
  isLastGroup: boolean;
}) {
  return (
    <>
      <tr>
        <td
          colSpan={3}
          className="bg-bg text-accent text-[10px] tracking-[0.15em] uppercase px-4 py-2.5 font-semibold"
        >
          {group.group}
        </td>
      </tr>
      {group.rows.map((r, i) => {
        const isLastRow = isLastGroup && i === group.rows.length - 1;
        const borderClass = isLastRow ? "" : "border-b border-line";
        return (
          <tr key={r.op} className="hover:bg-accent/[0.03]">
            <td className={`px-4 py-3.5 ${borderClass} text-ink`}>{r.op}</td>
            <td className={`px-4 py-3.5 ${borderClass} text-right`}>
              <span className="text-ink-dim mr-2">{r.m1.ns}</span>
              <span className="text-accent font-semibold">{r.m1.rate}</span>
            </td>
            <td className={`px-4 py-3.5 ${borderClass} text-right`}>
              <span className="text-ink-dim mr-2">{r.i9.ns}</span>
              <span className="text-accent font-semibold">{r.i9.rate}</span>
            </td>
          </tr>
        );
      })}
    </>
  );
}

function BenchKpi({
  label,
  value,
  unit,
  note,
}: {
  label: string;
  value: string;
  unit: string;
  note: string;
}) {
  return (
    <>
      <h4 className="text-[10px] tracking-[0.15em] text-ink-dim uppercase mb-4 font-medium">
        {label}
      </h4>
      <div className="font-display text-[56px] text-accent leading-none mb-1.5">
        {value}
        <span className="text-[22px] text-ink-dim">{unit}</span>
      </div>
      <p className="text-ink-dim text-[11px] leading-[1.6] mt-3 mb-6">{note}</p>
    </>
  );
}
