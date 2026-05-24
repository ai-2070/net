import { Fragment, type JSX, type ReactNode } from "react";
import type { Metadata } from "next";
import { NavBar } from "@/components/NavBar";
import { FooterDivider } from "@/components/FooterDivider";
import { Footer } from "@/components/Footer";

export const metadata: Metadata = {
  title: "NET — Substrate brief for premium hyperscale workloads",
  description:
    "NET extends CapabilityFold with a real-time aggregator surface. Premium-tier ceiling +17 points. +$230M/yr per Colossus-class site. Only Nvidia hardware qualifies for the top tier.",
  openGraph: {
    title: "NET — Substrate brief for premium hyperscale workloads",
    description:
      "Same MIG. Same QoS. Same silicon. A sub-µs control plane routes premium demand to warm capacity before the local ceiling breaks. Only Nvidia hardware qualifies for the top tier.",
    type: "website",
    url: "https://ai2070.net/hyperscale",
  },
};

export default function HyperscalePage(): JSX.Element {
  return (
    <>
      <NavBar />
      <main className="pt-20 max-w-[1440px] mx-auto">
        <TitleSection />
        <NumbersSection />
        <CompareSection />
        <GapSection />
        <LeversSection />
        <MappingSection />
        <AnatomySection />
        <ClosingStrip />
        <FooterDivider />
        <Footer />
      </main>
    </>
  );
}

/* ───────────────────────── §01 — TITLE + BOTTOM LINE ───────────────────────── */

function TitleSection(): JSX.Element {
  return (
    <section className="border-b border-line px-6 pt-[60px] pb-20">
      <div className="text-[10px] text-ink-dim tracking-[0.15em] mb-7 flex flex-wrap gap-[18px] items-center">
        <span className="text-accent border border-accent-dim px-2 py-[3px]">
          RFC-NET-SUB-001
        </span>
        <span className="text-ink-faint font-mono">PROTOCOL.0x4E45·54</span>
        <span className="text-ink-dim">SUBSTRATE BRIEF / Q2 2026</span>
      </div>

      <h1
        className="font-display leading-[0.95] tracking-[-0.02em] text-ink mb-7 max-w-[1200px]"
        style={{ fontSize: "clamp(40px, 6.4vw, 88px)" }}
      >
        <span className="text-accent">net.</span> compute settlement substrate.
        <br />
        <span className="text-accent">premium bursts</span> across fleets.
      </h1>

      <p className="text-[18px] text-ink mt-8 max-w-[760px] leading-[1.5] font-light">
        Same MIG. Same QoS. Same silicon. A{" "}
        <em className="not-italic text-accent bg-accent/10 px-1">
          sub-µs control plane
        </em>{" "}
        routes demand to warm capacity before the local ceiling breaks.
      </p>

      <div className="border-l-2 border-accent pl-8 pr-8 py-6 bg-accent/[0.02] mt-12 max-w-[1100px]">
        <div className="text-[10px] text-accent tracking-[0.18em] mb-2.5">
          // bottom line
        </div>
        <div
          className="font-display text-ink leading-[1.2]"
          style={{ fontSize: "clamp(20px, 2.4vw, 30px)" }}
        >
          Net turns every attested Nvidia card into a premium-tier capability
          that prices itself above commodity,
          <br />
          while still capturing every idle minute as commodity downshift
          revenue.
          <br />
          <span className="text-accent">
            AMD cannot enter the premium tier without parallel attestation
            infrastructure.
          </span>
          <br />
          <span className="text-accent">They have not built it.</span>
        </div>
      </div>
    </section>
  );
}

/* ───────────────────────── §02 — NUMBERS ───────────────────────── */

interface NumberCard {
  label: string;
  big: string;
  caption: string;
}

const NUMBER_CARDS: ReadonlyArray<NumberCard> = [
  { label: "// ceiling moves", big: "75% → 92%+", caption: "premium billable." },
  {
    label: "// per colossus-class site",
    big: "+$230M/yr",
    caption: "recovered premium capacity.",
  },
  { label: "// premium vs. bulk", big: "5–20×", caption: "per GPU-hour." },
  {
    label: "// sovereign tier clip",
    big: "6–8%",
    caption: "only Nvidia qualifies.",
  },
  {
    label: "// idle min / H100 / day",
    big: "single digits",
    caption: "with bidirectional fallback.",
  },
];

function NumbersSection(): JSX.Element {
  return (
    <section id="numbers" className="border-b border-line px-6 py-20">
      <div className="sec-label text-[10px] tracking-[0.2em] text-accent uppercase mb-3 flex items-center">
        §02 / the numbers
      </div>
      <h2
        className="font-display leading-none tracking-[-0.01em] text-ink mb-8 max-w-[900px]"
        style={{ fontSize: "clamp(36px, 5vw, 60px)" }}
      >
        five numbers. one substrate.
      </h2>

      <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-5 gap-px bg-line border border-line">
        {NUMBER_CARDS.map((c) => (
          <div
            key={c.label}
            className="bg-bg p-7 transition-colors hover:bg-bg-2 flex flex-col"
          >
            <div className="text-[10px] text-ink-dim tracking-[0.18em] uppercase mb-4 font-medium">
              {c.label}
            </div>
            <div
              className="font-display text-accent leading-none mb-1.5"
              style={{ fontSize: "clamp(28px, 3.4vw, 44px)" }}
            >
              {c.big}
            </div>
            <div className="text-ink-dim text-[12px] leading-[1.6] mt-3">
              {c.caption}
            </div>
          </div>
        ))}
      </div>
    </section>
  );
}

/* ───────────────────────── §03 — WITHOUT / WITH NET ───────────────────────── */

interface CompareCard {
  icon: ReactNode;
  text: ReactNode;
}

const WITHOUT_CARDS: ReadonlyArray<CompareCard> = [
  {
    icon: (
      <path d="M3 17a9 9 0 1 1 18 0M12 17l5-6" />
    ),
    text: "demand hits local ceiling.",
  },
  {
    icon: <path d="M4 20V10M9 20V6M14 20V13M19 20V8" />,
    text: "qos preempts bulk.",
  },
  {
    icon: <path d="M12 3l10 17H2L12 3zM12 10v5M12 18v.5" />,
    text: "premium slos degrade.",
  },
  {
    icon: (
      <>
        <circle cx="12" cy="12" r="9" />
        <path d="M8 8l8 8M16 8l-8 8" />
      </>
    ),
    text: "downstream collapse.",
  },
];

const WITH_CARDS: ReadonlyArray<CompareCard> = [
  {
    icon: (
      <>
        <circle cx="5" cy="5" r="1.8" fill="currentColor" />
        <circle cx="19" cy="5" r="1.8" fill="currentColor" />
        <circle cx="5" cy="19" r="1.8" fill="currentColor" />
        <circle cx="19" cy="19" r="1.8" fill="currentColor" />
        <circle cx="12" cy="12" r="1.8" fill="currentColor" />
        <path d="M5 5l7 7 7-7M5 19l7-7 7 7M5 5v14M19 5v14" />
      </>
    ),
    text: "premium burst matched to attested remote capacity.",
  },
  {
    icon: (
      <>
        <rect x="4" y="10" width="16" height="11" rx="1.5" />
        <path d="M8 10V7a4 4 0 0 1 8 0v3" />
      </>
    ),
    text: (
      <>
        org-verified isolation, auth, settlement at{" "}
        <span className="text-accent">protocol speed</span>.
      </>
    ),
  },
  {
    icon: (
      <>
        <circle cx="12" cy="12" r="9" />
        <path d="M8 12.5l3 3 5-6" />
      </>
    ),
    text: "local fleet stays slo-compliant.",
  },
  {
    icon: (
      <>
        <circle cx="12" cy="12" r="9" />
        <path d="M15 9.5a3 3 0 0 0-3-1.5c-1.7 0-3 1-3 2.5s1.3 2 3 2.5 3 1 3 2.5-1.3 2.5-3 2.5a3 3 0 0 1-3-1.5" />
        <path d="M12 6.5v11" />
      </>
    ),
    text: "remote operator gets paid at the qualifying tier.",
  },
];

function CompareSection(): JSX.Element {
  return (
    <section id="compare" className="border-b border-line px-6 py-20">
      <div className="sec-label text-[10px] tracking-[0.2em] text-accent uppercase mb-3 flex items-center">
        §03 / without net / with net
      </div>
      <h2
        className="font-display leading-none tracking-[-0.01em] text-ink mb-8 max-w-[900px]"
        style={{ fontSize: "clamp(36px, 5vw, 60px)" }}
      >
        local ceiling. <span className="text-accent">mesh burst.</span>
      </h2>

      <div className="grid grid-cols-1 lg:grid-cols-[1fr_60px_1fr] items-stretch gap-4 lg:gap-0">
        <div className="border border-line">
          <div className="px-6 py-4 border-b border-line">
            <div className="text-[10px] tracking-[0.18em] uppercase font-medium text-warn">
              // without net — local ceiling
            </div>
          </div>
          <div className="grid grid-cols-1 sm:grid-cols-2 gap-px bg-line">
            {WITHOUT_CARDS.map((c, i) => (
              <div key={i} className="bg-bg p-7 flex flex-col text-warn">
                <svg
                  className="w-5 h-5 mb-4"
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth={1.4}
                  strokeLinecap="round"
                  strokeLinejoin="round"
                >
                  {c.icon}
                </svg>
                <div className="font-head text-[16px] leading-tight mb-3.5 tracking-[0.04em] lowercase">
                  {c.text}
                </div>
              </div>
            ))}
          </div>
        </div>

        <div className="flex items-center justify-center font-mono text-[28px] text-accent">
          →
        </div>

        <div className="border border-line">
          <div className="px-6 py-4 border-b border-line">
            <div className="text-[10px] text-accent tracking-[0.18em] uppercase font-semibold">
              // with net — mesh burst
            </div>
          </div>
          <div className="grid grid-cols-1 sm:grid-cols-2 gap-px bg-line">
            {WITH_CARDS.map((c, i) => (
              <div
                key={i}
                className="bg-bg p-7 flex flex-col transition-colors hover:bg-bg-2"
              >
                <svg
                  className="w-5 h-5 text-accent mb-4"
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth={1.4}
                  strokeLinecap="round"
                  strokeLinejoin="round"
                >
                  {c.icon}
                </svg>
                <div className="font-head text-[16px] leading-tight text-ink mb-3.5 tracking-[0.04em] lowercase">
                  {c.text}
                </div>
              </div>
            ))}
          </div>
        </div>
      </div>
    </section>
  );
}

/* ───────────────────────── §04 — GAP ───────────────────────── */

function GapSection(): JSX.Element {
  return (
    <section id="gap" className="border-b border-line px-6 py-20">
      <div className="sec-label text-[10px] tracking-[0.2em] text-accent uppercase mb-3 flex items-center">
        §04 / what hyperscalers do not have today
      </div>

      <h2
        className="font-display leading-[1.08] tracking-[-0.01em] mb-3 max-w-[1000px]"
        style={{ fontSize: "clamp(32px, 4.4vw, 56px)" }}
      >
        <span className="text-accent">
          Nvidia internally does not have this.
        </span>
        <br />
        <span className="text-ink">CoreWeave would kill for this.</span>
      </h2>
      <p
        className="text-ink-dim italic font-mono mt-3 max-w-[820px] leading-[1.65]"
        style={{ fontSize: "clamp(13px, 1.3vw, 15px)" }}
      >
        AWS fakes it with EMR and batch telemetry.
      </p>

      <p className="text-[15px] text-ink-dim mt-12 max-w-[820px] leading-[1.7]">
        NET extends the existing CapabilityFold with a generic aggregator
        surface — TagMatcher × GroupBy × Aggregation — that turns the fold into
        a materialized, real-time capability-and-demand OLAP cube. The same
        matcher answers supply, demand, capacity, fallback, model variants,
        quantizations, and regions. No new query language. No secondary
        database. No second substrate plane.
      </p>

      <p className="text-[15px] text-ink-dim mt-7 max-w-[820px] leading-[1.7]">
        The result is a real-time GPU inventory and demand graph at fleet
        scale, hierarchical from rack to DC to region to global. At any moment
        you can see exactly how much model capacity exists, where it lives,
        what is being requested, and how to route workloads to maximize
        utilization. Hyperscalers structurally do not have this because they do
        not own a neutral cross-fleet substrate. NET does.
      </p>

      <div className="border-l-2 border-accent pl-8 pr-8 py-6 bg-accent/[0.02] mt-12 max-w-[900px]">
        <div className="text-[10px] text-accent tracking-[0.18em] mb-2.5">
          // pull quote
        </div>
        <div
          className="font-display text-ink leading-[1.1]"
          style={{ fontSize: "clamp(22px, 2.4vw, 32px)" }}
        >
          the substrate hyperscalers{" "}
          <span className="text-accent">don&apos;t have</span> and{" "}
          <span className="text-accent">can&apos;t easily build.</span>
        </div>
      </div>
    </section>
  );
}

/* ───────────────────────── §05 — LEVERS (six primitives) ───────────────────────── */

interface Primitive {
  label: string;
  head: string;
  body: string;
  icon: ReactNode;
}

const PRIMITIVES: ReadonlyArray<Primitive> = [
  {
    label: "// proximity graph",
    head: "real network distance.",
    body: "Meas. pingwave latencies drive routing. Edges carry real RTT.",
    icon: (
      <>
        <circle cx="12" cy="12" r="3" />
        <circle cx="12" cy="12" r="7" />
        <circle cx="12" cy="12" r="11" />
      </>
    ),
  },
  {
    label: "// capability index + aggregator",
    head: "tag × group-by × aggregation.",
    body: "Sub-µs queries over GPU, VRAM, attestation, model + version + weights hash, quant, TTFT, TPS. Same surface for supply, demand, capacity, fallback.",
    icon: (
      <>
        <circle cx="10.5" cy="10.5" r="6.5" />
        <path d="M15.5 15.5L21 21" />
      </>
    ),
  },
  {
    label: "// bloom auth + org chain",
    head: "line-rate authorization.",
    body: "<10ns per packet membership checks. Cryptographic org verification at line rate. Settlement records the verified org pair.",
    icon: (
      <g fill="currentColor" stroke="none">
        <circle cx="6" cy="6" r="1.4" />
        <circle cx="12" cy="6" r="1.4" />
        <circle cx="18" cy="6" r="1.4" />
        <circle cx="6" cy="12" r="1.4" />
        <circle cx="12" cy="12" r="1.4" />
        <circle cx="18" cy="12" r="1.4" />
        <circle cx="6" cy="18" r="1.4" />
        <circle cx="12" cy="18" r="1.4" />
        <circle cx="18" cy="18" r="1.4" />
      </g>
    ),
  },
  {
    label: "// encrypted udp",
    head: "zero-alloc transport.",
    body: "64-byte headers. Zero-alloc pools. Multi-hop forwarding.",
    icon: (
      <>
        <rect x="4" y="10" width="16" height="11" rx="1.5" />
        <path d="M8 10V7a4 4 0 0 1 8 0v3" />
      </>
    ),
  },
  {
    label: "// replication & ha",
    head: "cross-region replica groups.",
    body: "Replica groups spread across regions. Auto-heal, no migration required.",
    icon: (
      <>
        <ellipse cx="12" cy="5.5" rx="8" ry="2.5" />
        <path d="M4 5.5v6c0 1.4 3.6 2.5 8 2.5s8-1.1 8-2.5v-6" />
        <path d="M4 11.5v6c0 1.4 3.6 2.5 8 2.5s8-1.1 8-2.5v-6" />
      </>
    ),
  },
  {
    label: "// settlement rail",
    head: "three tiers, three clips.",
    body: "Per-transaction settlement at protocol layer. 1–2% commodity. 3–4% attested. 6–8% sovereign.",
    icon: (
      <>
        <circle cx="12" cy="12" r="9" />
        <path d="M15 9.5a3 3 0 0 0-3-1.5c-1.7 0-3 1-3 2.5s1.3 2 3 2.5 3 1 3 2.5-1.3 2.5-3 2.5a3 3 0 0 1-3-1.5" />
        <path d="M12 6.5v11" />
      </>
    ),
  },
];

function LeversSection(): JSX.Element {
  return (
    <section id="levers" className="border-b border-line px-6 py-20">
      <div className="sec-label text-[10px] tracking-[0.2em] text-accent uppercase mb-3 flex items-center">
        §05 / how net makes the ceiling soft
      </div>
      <h2
        className="font-display leading-none tracking-[-0.01em] text-ink mb-8 max-w-[900px]"
        style={{ fontSize: "clamp(36px, 5vw, 60px)" }}
      >
        six primitives. one substrate.
      </h2>

      <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-px bg-line border border-line">
        {PRIMITIVES.map((p) => (
          <div
            key={p.label}
            className="bg-bg p-7 transition-colors hover:bg-bg-2 flex flex-col"
          >
            <svg
              className="w-5 h-5 text-accent mb-4"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth={1.4}
            >
              {p.icon}
            </svg>
            <div className="text-[10px] text-ink-dim tracking-[0.18em] uppercase mb-4 font-medium">
              {p.label}
            </div>
            <div className="font-head text-[18px] leading-tight text-ink mb-3.5 tracking-[0.04em] lowercase">
              {p.head}
            </div>
            <div className="text-ink-dim text-[12px] leading-[1.6] flex-1">
              {p.body}
            </div>
          </div>
        ))}
      </div>
    </section>
  );
}

/* ───────────────────────── §06 — MAPPING ───────────────────────── */

interface MappingRow {
  fromHead: string;
  fromBody: string;
  toHead: string;
  toBody: string;
  outcome: string;
  last?: boolean;
}

const MAPPING_ROWS: ReadonlyArray<MappingRow> = [
  {
    fromHead: "mig static slice shapes",
    fromBody: "Static shapes, local GPU boundary.",
    toHead: "capability-matched burst routing",
    toBody:
      "Workload routes to a fleet where the right MIG shape is already live. No local drain, reset, or tenant eviction.",
    outcome: "Right shape. Right place. Right time.",
  },
  {
    fromHead: "time-slicing contention",
    fromBody: "Premium p99 degrades. Downstream tiers collapse.",
    toHead: "warm remote overflow",
    toBody:
      "When local contention threatens premium p99, work bursts to attested mesh capacity. Control path sub-µs; remaining cost is network distance.",
    outcome: "Premium p99 protected. Downstream tiers untouched.",
  },
  {
    fromHead: "model warm-pool fragmentation",
    fromBody:
      "Each variant requires its own warm pool per operator. Reload costs 30–300s, 10–80 GB PCIe.",
    toHead: "aggregator-driven model routing",
    toBody:
      "Models exposed as tuples: name, version, weights hash, quant, TTFT, TPS. Aggregator routes supply to demand via the same matcher. Bidirectional fallback: supply downshifts, demand relaxes.",
    outcome: "Zero reload waste. Idle minutes per H100 / day drop to single digits.",
  },
  {
    fromHead: "local qos ceiling",
    fromBody:
      "Priority queue helps within one fleet. Regulated workloads cannot legally cross the boundary.",
    toHead: "cross-operator premium pool",
    toBody:
      "When one CSP saturates, demand routes to another operator's attested tier. Cryptographic org chain + per-transaction settlement make the handoff legally viable.",
    outcome: "Operator boundary becomes soft. Compliance preserved.",
  },
  {
    fromHead: "failover reserve",
    fromBody:
      "Premium reserve duplicated per operator. Counted as utilized, produces zero revenue.",
    toHead: "shared standby capacity",
    toBody:
      "Reserve becomes mesh-wide instead of duplicated per operator. Standby and replica groups absorb failure without every fleet holding a full passive copy.",
    outcome: "Less stranded reserve. More productive capacity.",
    last: true,
  },
];

function MappingSection(): JSX.Element {
  return (
    <section id="map" className="border-b border-line px-6 py-20">
      <div className="sec-label text-[10px] tracking-[0.2em] text-accent uppercase mb-3 flex items-center">
        §06 / from inside-fleet techniques to mesh burst
      </div>
      <h2
        className="font-display leading-none tracking-[-0.01em] text-ink mb-8 max-w-[900px]"
        style={{ fontSize: "clamp(36px, 5vw, 60px)" }}
      >
        techniques compose. boundaries soften.
      </h2>

      <div className="grid grid-cols-1 lg:grid-cols-[1fr_320px] gap-6 items-start">
        <div className="grid grid-cols-1 lg:grid-cols-[1.1fr_40px_1.3fr_1fr] border border-line">
          <div className="bg-bg-2 text-ink-dim text-[10px] tracking-[0.18em] uppercase px-6 py-3 border-b border-line lg:border-r">
            // inside one fleet (today)
          </div>
          <div className="bg-bg-2 text-ink-faint text-[10px] tracking-[0.18em] uppercase px-6 py-3 border-b border-line lg:border-r text-center">
            →
          </div>
          <div className="bg-bg-2 text-accent text-[10px] tracking-[0.18em] uppercase px-6 py-3 border-b border-line lg:border-r">
            // with net (mesh-wide)
          </div>
          <div className="bg-bg-2 text-ink-dim text-[10px] tracking-[0.18em] uppercase px-6 py-3 border-b border-line">
            // outcome
          </div>

          {MAPPING_ROWS.map((row) => {
            const border = row.last ? "" : "border-b border-line";
            return (
              <Fragment key={row.fromHead}>
                <div className={`px-6 py-7 ${border} lg:border-r`}>
                  <div className="font-head text-[16px] leading-tight text-ink mb-2 tracking-[0.04em] lowercase">
                    {row.fromHead}
                  </div>
                  <div className="text-ink-dim text-[12px] leading-[1.6]">
                    {row.fromBody}
                  </div>
                </div>
                <div
                  className={`text-ink-faint font-mono text-center px-6 py-7 ${border} lg:border-r self-center`}
                >
                  →
                </div>
                <div className={`px-6 py-7 ${border} lg:border-r`}>
                  <div className="font-head text-[16px] leading-tight text-accent mb-2 tracking-[0.04em] lowercase">
                    {row.toHead}
                  </div>
                  <div className="text-ink text-[12px] leading-[1.6]">
                    {row.toBody}
                  </div>
                </div>
                <div
                  className={`px-6 py-7 ${border} text-accent font-mono text-[12px] leading-[1.7]`}
                >
                  {row.outcome}
                </div>
              </Fragment>
            );
          })}
        </div>

        <aside className="border border-cyan/40 bg-cyan/[0.04] p-6 lg:sticky lg:top-20">
          <div className="text-[10px] text-cyan tracking-[0.18em] uppercase mb-5 pb-3.5 border-b border-cyan/30 font-medium">
            // protocol-speed control plane
          </div>
          <Receipt big="< 1 µs" caption="Capability-indexed routing + aggregator query." />
          <Receipt big="< 10 ns" caption="Bloom-filter authorization + org chain check." />
          <Receipt big="24 bytes" caption="Causal link per event." />
          <Receipt big="64 bytes" caption="Cache-line header." last />
        </aside>
      </div>
    </section>
  );
}

function Receipt({
  big,
  caption,
  last,
}: {
  big: string;
  caption: string;
  last?: boolean;
}): JSX.Element {
  return (
    <div
      className={
        last
          ? "pt-3.5"
          : "py-3.5 border-b border-dashed border-cyan/25"
      }
    >
      <div
        className="font-display text-cyan leading-none mb-1"
        style={{ fontSize: "26px" }}
      >
        {big}
      </div>
      <div className="text-ink-dim text-[12px] leading-[1.6] mt-2">
        {caption}
      </div>
    </div>
  );
}

/* ───────────────────────── §07 — ANATOMY ───────────────────────── */

function AnatomySection(): JSX.Element {
  return (
    <section id="anatomy" className="border-b border-line px-6 py-20">
      <div className="sec-label text-[10px] tracking-[0.2em] text-accent uppercase mb-3 flex items-center">
        §07 / premium billable ceiling — anatomy
      </div>

      <div className="grid grid-cols-1 lg:grid-cols-[1fr_56px_1fr] gap-6 lg:gap-10 items-stretch">
        <div className="flex flex-col justify-center">
          <h2
            className="font-display leading-[1.08] tracking-[-0.01em] mb-8 lowercase"
            style={{ fontSize: "clamp(32px, 4.4vw, 56px)" }}
          >
            <span className="text-ink">the premium tier.</span>
            <br />
            <span className="text-accent">unlocking the ceiling.</span>
          </h2>
          <p className="text-ink-dim text-[14px] leading-[1.7] max-w-[540px] font-mono">
            NET converts stranded premium-tier ceiling into settled premium
            capacity, leveraging existing silicon and existing power rather
            than waiting for new fabs and new grid. The recovered capacity
            bills at the attested and sovereign tiers — the tiers only Nvidia
            hardware qualifies for today.
          </p>
        </div>

        <div className="flex flex-col min-h-[540px] order-3 lg:order-2">
          <div
            className="grow-[8] bg-[#3a4035]"
            aria-label="hard architectural ceiling, 8%"
          />
          <div
            className="grow-[17] bg-accent"
            aria-label="stranded ceiling headroom, 17%"
          />
          <div
            className="grow-[75] border border-accent"
            aria-label="billable premium today, 75%"
          />
        </div>

        <div className="grid grid-rows-[8fr_17fr_75fr] min-h-[540px] order-2 lg:order-3">
          <div className="flex flex-col justify-center gap-1.5 border-b border-line py-2">
            <div
              className="font-display leading-none tracking-[-0.02em] lowercase text-ink-dim"
              style={{ fontSize: "clamp(24px, 4.2vw, 44px)" }}
            >
              ~8%
            </div>
            <div className="text-[10px] tracking-[0.18em] uppercase font-medium text-ink-dim">
              // hard architectural ceiling
            </div>
            <div className="text-ink-dim text-[12px] leading-[1.6] max-w-[480px] mt-1">
              Demand exceeds local capacity — queued, rejected, degraded.
            </div>
          </div>
          <div className="flex flex-col justify-center gap-1.5 border-b border-line py-2">
            <div
              className="font-display text-accent leading-none tracking-[-0.02em] lowercase"
              style={{ fontSize: "clamp(36px, 5.8vw, 72px)" }}
            >
              ~17%
            </div>
            <div className="text-[10px] text-accent tracking-[0.18em] uppercase font-semibold">
              // stranded ceiling headroom
            </div>
            <div className="text-accent text-[12px] leading-[1.6] max-w-[520px] mt-1">
              What NET unlocks. SLO headroom, failover reserve, wrong-region
              capacity — routed to attested remote capacity.
            </div>
          </div>
          <div className="flex flex-col justify-center gap-1.5 py-2">
            <div
              className="font-display text-ink leading-none tracking-[-0.02em] lowercase"
              style={{ fontSize: "clamp(36px, 5.8vw, 72px)" }}
            >
              ~75%
            </div>
            <div className="text-[10px] text-ink tracking-[0.18em] uppercase font-medium">
              // billable premium today
            </div>
            <div className="text-ink-dim text-[12px] leading-[1.6] max-w-[480px] mt-1">
              SLO-compliant, revenue-producing.
            </div>
          </div>
        </div>
      </div>

      <div className="text-ink-dim text-[11px] tracking-[0.1em] text-right mt-4 font-mono">
        Source: Hyperscale CSP fleet telemetry, premium tier
      </div>

      <div className="grid grid-cols-1 md:grid-cols-3 gap-px bg-accent-dim mt-12 border border-accent-dim">
        <TierCard
          label="// commodity"
          body="No attestation. No org verification. 1–2% NET clip. Race to the bottom against ROCm farms, consumer 4090 rigs, anyone with cheap power."
        />
        <TierCard
          label="// attested"
          body="Hardware attestation: CC-mode H100/H200, MIG attestation, NVLink topology proof, TEE execution path, signed weights provenance. 3–4% NET clip. Only Nvidia silicon qualifies today."
        />
        <TierCard
          label="// sovereign"
          body="Attested hardware + cryptographic org chain. Anthropic, OpenAI, DoD, JP Morgan, NHS, Bundeswehr. 6–8% NET clip. Only tier regulated industries can legally land on."
        />
      </div>

      <p className="text-[14px] text-ink mt-12 max-w-[920px] leading-[1.7] mx-auto text-center font-mono">
        Regulated workloads pay 5–10× over commodity today through hyperscaler
        private endpoints. On NET they pay 1.5–2× premium directly into the
        mesh.{" "}
        <span className="text-accent">
          Only Nvidia cards are in the eligible set.
        </span>
      </p>
    </section>
  );
}

function TierCard({ label, body }: { label: string; body: string }): JSX.Element {
  return (
    <div className="bg-bg p-7 flex flex-col">
      <div className="text-[10px] text-accent tracking-[0.18em] uppercase mb-4 font-semibold">
        {label}
      </div>
      <div className="text-ink-dim text-[12px] leading-[1.65]">{body}</div>
    </div>
  );
}

/* ───────────────────────── §08 — CLOSING ───────────────────────── */

function ClosingStrip(): JSX.Element {
  return (
    <section className="border-b border-line px-6 py-20">
      <div className="mt-16 text-center py-16 border-t border-b border-accent-dim bg-accent/[0.02]">
        <div className="flex items-center justify-center gap-6 flex-wrap max-w-[1100px] mx-auto px-6">
          <svg
            className="w-9 h-9 text-accent flex-shrink-0"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth={1.5}
            strokeLinecap="round"
            strokeLinejoin="round"
          >
            <circle cx="12" cy="12" r="9" />
            <ellipse cx="12" cy="12" rx="4" ry="9" />
            <path d="M3 12h18" />
          </svg>
          <div
            className="font-display text-ink leading-[1.2] text-left"
            style={{ fontSize: "clamp(20px, 2.6vw, 32px)" }}
          >
            NET does not sell idle gpus.
            <br />
            NET turns{" "}
            <span className="text-accent">stranded premium capacity</span> into{" "}
            <span className="text-accent">settled capacity</span> on the{" "}
            <span className="text-accent">only tier nvidia can populate</span>.
          </div>
        </div>
      </div>
    </section>
  );
}
