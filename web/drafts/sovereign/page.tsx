import { Fragment, type JSX, type ReactNode } from "react";
import type { Metadata } from "next";
import { NavBar } from "@/components/NavBar";
import { FooterDivider } from "@/components/FooterDivider";
import { Footer } from "@/components/Footer";

export const metadata: Metadata = {
  title: "NET — Private brief",
  robots: { index: false, follow: false },
};

export default function SovereignPage(): JSX.Element {
  return (
    <>
      <NavBar />
      <main className="pt-20 max-w-[1440px] mx-auto">
        <PrivateBanner />
        <HeroSection />
        <ConvergenceSection />
        <TripletSection />
        <CardFlowSection />
        <MoatSection />
        <SovereignTierSection />
        <ProductsSection />
        <SettlementSection />
        <ClosingSection />
        <FooterDivider />
        <Footer />
      </main>
    </>
  );
}

/* ───────────────────────── Private-brief banner ───────────────────────── */

function PrivateBanner(): JSX.Element {
  return (
    <div className="border-b border-line px-6 py-2 text-[10px] tracking-[0.15em] uppercase flex flex-wrap gap-x-3 gap-y-1 items-center bg-bg">
      <span className="live-dot inline-flex items-center gap-1.5 text-accent">
        mesh online
      </span>
      <span className="text-ink-faint">│</span>
      <span className="text-ink-dim">
        net <span className="text-accent font-semibold">// compute settlement substrate</span>
      </span>
      <span className="text-ink-faint">│</span>
      <span className="text-accent font-semibold">sovereign</span>
      <span className="text-ink-faint">│</span>
      <span className="text-ink-dim">private brief</span>
    </div>
  );
}

/* ───────────────────────── Hero ───────────────────────── */

function HeroSection(): JSX.Element {
  return (
    <section className="border-b border-line px-6 pt-[60px] pb-20">
      <div className="text-[10px] text-ink-dim tracking-[0.15em] mb-7 flex flex-wrap gap-[18px] items-center">
        <span className="text-accent border border-accent-dim px-2 py-[3px]">
          RFC-NET-SOV-001
        </span>
        <span className="text-ink-faint font-mono">PROTOCOL.0x4E45·54</span>
        <span className="text-ink-dim">SOVEREIGN BRIEF Q2 2026</span>
      </div>

      <p className="text-ink-dim font-mono text-[13px] tracking-[0.04em] mb-12 max-w-[820px]">
        Energy at 1,000×. Silicon and grid arrive in years. Utilization is
        addressable this quarter.
      </p>

      <h1
        className="font-display leading-[0.95] tracking-[-0.04em] text-accent mb-7 lowercase"
        style={{ fontSize: "clamp(80px, 14vw, 200px)" }}
      >
        sovereign
      </h1>

      <p
        className="font-display text-ink leading-[1.15] tracking-[-0.01em] max-w-[1000px] mb-16 lowercase"
        style={{ fontSize: "clamp(22px, 2.6vw, 34px)" }}
      >
        Market intelligence layer + matching substrate + settlement layer in
        one.
      </p>

      <div className="border-l-2 border-accent pl-8 pr-8 py-10 bg-accent/[0.02] max-w-[1200px]">
        <div className="text-[10px] text-accent tracking-[0.18em] mb-4">
          // hero quote
        </div>
        <blockquote
          className="font-display text-ink leading-[1.3] tracking-[-0.01em] lowercase"
          style={{ fontSize: "clamp(14px, 1.7vw, 22px)" }}
        >
          &ldquo;I would cut $1 billion of it right away and give it to
          somebody as a cloud service.&rdquo;
        </blockquote>
        <p className="text-ink-dim font-mono text-[11px] tracking-[0.05em] mt-5">
          — Jensen Huang, Stanford CS153 Frontier Systems
        </p>
      </div>

      <p
        className="font-display text-accent leading-[1.2] tracking-[-0.01em] mt-12 max-w-[1100px] lowercase"
        style={{ fontSize: "clamp(22px, 3vw, 38px)" }}
      >
        The substrate to do this should already exist. This is it.
      </p>
    </section>
  );
}

/* ───────────────────────── §01 — Convergence ───────────────────────── */

interface Quadrant {
  tag: string;
  body: ReactNode;
  attribution?: string;
}

const QUADRANTS: ReadonlyArray<Quadrant> = [
  {
    tag: "// stanford cs153",
    body: '"I would cut $1 billion of it right away and give it to somebody as a cloud service."',
    attribution: "— Jensen Huang, Stanford endowment question",
  },
  {
    tag: "// workload-side placement",
    body: "$30M to OpenAI. Direct strategic placement on the workload side.",
    attribution:
      '"We weren\'t in a position to make the multi-billion dollar investment into Anthropic so they could use our compute." — Dwarkesh, 2026',
  },
  {
    tag: "// dgx cloud",
    body: "Nvidia rents from Nvidia. Owning the rail the silicon ships on.",
  },
  {
    tag: "// project digits",
    body: "Compute pushed to the edge. Residential and small-site deployment.",
  },
];

function ConvergenceSection(): JSX.Element {
  return (
    <section id="convergence" className="border-b border-line px-6 py-20">
      <div className="sec-label text-[10px] tracking-[0.2em] text-accent uppercase mb-3 flex items-center">
        §01 / four moves. one substrate.
      </div>
      <h2
        className="font-display leading-none tracking-[-0.01em] text-ink mb-12 max-w-[900px] lowercase"
        style={{ fontSize: "clamp(36px, 5vw, 60px)" }}
      >
        the convergence.
      </h2>

      <div className="relative max-w-[1200px] mx-auto">
        {/* X convergence lines */}
        <svg
          className="absolute inset-0 w-full h-full pointer-events-none hidden md:block"
          viewBox="0 0 100 100"
          preserveAspectRatio="none"
          aria-hidden="true"
        >
          <line x1="0" y1="0" x2="50" y2="50" stroke="#c4ff3d" strokeWidth="0.15" />
          <line x1="100" y1="0" x2="50" y2="50" stroke="#c4ff3d" strokeWidth="0.15" />
          <line x1="0" y1="100" x2="50" y2="50" stroke="#c4ff3d" strokeWidth="0.15" />
          <line x1="100" y1="100" x2="50" y2="50" stroke="#c4ff3d" strokeWidth="0.15" />
        </svg>

        <div className="grid grid-cols-1 md:grid-cols-2 gap-px bg-line border border-line relative">
          {QUADRANTS.map((q, i) => {
            const centerPad = [
              "md:pr-20 md:pb-16",
              "md:pl-20 md:pb-16",
              "md:pr-20 md:pt-16",
              "md:pl-20 md:pt-16",
            ][i];
            return (
              <div
                key={q.tag}
                className={`bg-bg p-7 flex flex-col min-h-[240px] ${centerPad}`}
              >
                <div className="text-[10px] text-accent tracking-[0.18em] uppercase mb-4 font-semibold">
                  {q.tag}
                </div>
                <div className="text-ink text-[14px] leading-[1.6] mb-3 flex-1">
                  {q.body}
                </div>
                {q.attribution ? (
                  <div className="text-ink-dim font-mono text-[11px] leading-[1.55] tracking-[0.02em]">
                    {q.attribution}
                  </div>
                ) : null}
              </div>
            );
          })}
        </div>

        {/* Central convergence point */}
        <div className="md:absolute md:inset-0 flex items-center justify-center pointer-events-none mt-6 md:mt-0 z-10">
          <div
            className="bg-bg border border-accent px-3 py-1.5 text-accent font-display lowercase text-center whitespace-nowrap"
            style={{ fontSize: "clamp(10px, 0.95vw, 13px)" }}
          >
            sovereign substrate
          </div>
        </div>
      </div>

      <p
        className="font-display leading-[1.15] tracking-[-0.01em] mt-16 text-center lowercase mx-auto max-w-[900px]"
        style={{ fontSize: "clamp(28px, 4vw, 52px)" }}
      >
        <span className="text-ink">Four moves.</span>{" "}
        <span className="text-accent">One substrate.</span>
      </p>
    </section>
  );
}

/* ───────────────────────── §02 — Triplet table ───────────────────────── */

interface TripletRow {
  moveQuote: ReactNode;
  moveAttribution?: string;
  requires: ReadonlyArray<string>;
  /** Array of groups; each group is a list of bullets. Adjacent groups are
   * separated by a thin lime divider line. Most rows have a single group. */
  provides: ReadonlyArray<ReadonlyArray<ReactNode>>;
}

const TRIPLET_ROWS: ReadonlyArray<TripletRow> = [
  {
    moveQuote:
      '"I would cut $1 billion of it right away and give it to somebody as a cloud service."',
    moveAttribution: "— Jensen Huang, Stanford CS153",
    requires: [
      "A neutral cross-fleet substrate institutional buyers can hand a billion dollars to.",
      "Real-time GPU inventory and demand graph at fleet scale.",
      "Workloads route to nearest capability match without per-region operator stitching.",
    ],
    provides: [
      [
        "Mesh substrate, sub-µs control plane",
        "CapabilityFold + aggregator surface (Tag × GroupBy × Aggregation)",
        "Hierarchical aggregators (rack → DC → region → global)",
        "One matcher answers supply, demand, capacity, fallback",
        "The substrate hyperscalers don't have and can't easily build",
      ],
    ],
  },
  {
    moveQuote:
      "$30M to OpenAI. The strategic regret on Anthropic was the missing piece — workload-side ownership.",
    requires: [
      "Workload identity that travels with the job",
      "Attested execution path on the silicon",
      "Premium-tier qualification at the protocol layer, not just contractual",
      "Org-level trust chain so workloads can target verified silicon",
    ],
    provides: [
      [
        "Cryptographic organizational verification — signed credentials, trust chains",
        "Trust roots: Anthropic, OpenAI, DoD, JP Morgan, NHS, Bundeswehr",
        "Bloom-filter authorization at <10ns per packet",
        "Premium-tier capability filter — attested HW + measured envelope + provenance chain on weights",
      ],
    ],
  },
  {
    moveQuote: "DGX Cloud. Nvidia rents from Nvidia. The rail Nvidia owns.",
    requires: [
      "The premium tier filter at protocol level",
      "Two revenue streams on one card",
      "Cross-operator burst absorption when local fleet saturates",
      "Cryptographic settlement that doesn't depend on AWS, Stripe, or banking rails",
      "Bidirectional graceful degradation",
      "Mesh-wide failover instead of duplicated reserves",
      "Model warm-pool composition across operators",
      "Loaded-model GPUs stop going to waste",
      "Premium SLO protection during contention",
    ],
    provides: [
      // Group 1 — Fleet & burst primitives
      [
        <>Mesh absorbs premium bursts across fleets. Premium tier ceiling moves 75% → 92%+.</>,
        <>Model Discovery — every served model exposed as a tuple: model name + weights_hash + quant + TTFT + TPS. Apps route on perf envelope, not model name.</>,
        <>CapacityEnvelope — what each GPU can hold, before placement. No trial-and-error.</>,
      ],
      // Group 2 — Unit economics primitives
      [
        <><strong className="text-accent font-semibold">Loaded-model fallback chain.</strong> An H100 with 70B FP8 loaded also serves 34B, 13B, 7B, embedding, MIG partition, tokenizer-only. Every loaded model becomes a stack of usable capabilities instead of stranded VRAM.</>,
        <><strong className="text-accent font-semibold">Two revenue streams on one card.</strong> Premium tier slot earns the high margin; downshift fills spare cycles. Net is the only substrate that prices both simultaneously off the same physical loaded state.</>,
        <>Bidirectional graceful degradation. 4 supply-side downshift tiers × 4 demand-side fallback tiers → idle minutes per H100/day drop to single digits.</>,
        <>Memory churn recovered. Reload costs 30–300s, 10–80 GB PCIe, NVLink thrash — NET routes around it.</>,
      ],
      // Group 3 — Settlement & routing primitives
      [
        <>Cryptographic settlement at the protocol layer. Beyond anything AWS, GCP, Cloudflare, or CoreWeave do.</>,
        <>Cross-operator premium pool. Shared standby capacity. Capability-matched burst routing.</>,
      ],
    ],
  },
  {
    moveQuote:
      "Project Digits. Compute at the edge. Residential and small-site deployment.",
    requires: [
      "Identity that survives across heterogeneous nodes — datacenter, residential, edge",
      "Sovereignty enforcement natively",
      "Aggregation at every tier from one residential node to global",
      "Zero-reload scheduling across the heterogeneous topology",
    ],
    provides: [
      [
        "4-level subnet hierarchy in a u32 — rack → DC → region → continent",
        "AggregatorDaemons via ReplicaGroup — no new architecture tier",
        "Subnet-scoped capability routing — sovereignty natively enforced",
        "Deterministic per-replica identity that survives migration",
        "Same matcher at every tier",
      ],
    ],
  },
];

function TripletSection(): JSX.Element {
  return (
    <section id="triplet" className="border-b border-line px-6 py-20">
      <div className="sec-label text-[10px] tracking-[0.2em] text-accent uppercase mb-3 flex items-center">
        §02 / what each move requires. what net provides.
      </div>
      <h2
        className="font-display leading-none tracking-[-0.01em] text-ink mb-10 max-w-[900px] lowercase"
        style={{ fontSize: "clamp(36px, 5vw, 60px)" }}
      >
        move <span className="text-accent">×</span> requires{" "}
        <span className="text-accent">×</span> provides.
      </h2>

      <div className="border border-line">
        {/* Desktop header */}
        <div className="hidden md:grid md:grid-cols-[1.1fr_1fr_1.4fr] bg-bg-2 border-b border-line">
          <div className="text-[10px] text-accent tracking-[0.18em] uppercase px-6 py-3 border-r border-line font-semibold">
            // move
          </div>
          <div className="text-[10px] text-accent tracking-[0.18em] uppercase px-6 py-3 border-r border-line font-semibold">
            // requires
          </div>
          <div className="text-[10px] text-accent tracking-[0.18em] uppercase px-6 py-3 font-semibold">
            // net provides
          </div>
        </div>

        {TRIPLET_ROWS.map((row, idx) => {
          const last = idx === TRIPLET_ROWS.length - 1;
          const isRow3 = idx === 2;
          return (
            <div
              key={idx}
              className={`grid grid-cols-1 md:grid-cols-[1.1fr_1fr_1.4fr] ${
                last ? "" : "border-b border-line"
              } ${isRow3 ? "shadow-[inset_3px_0_0_0_var(--color-accent)]" : ""}`}
            >
              <div className="px-6 py-7 border-b md:border-b-0 md:border-r border-line bg-bg">
                <div className="md:hidden text-[10px] text-accent tracking-[0.18em] uppercase mb-3 font-semibold">
                  // move
                </div>
                {idx === 0 ? (
                  <>
                    <div
                      className="font-mono italic text-ink leading-[1.55] mb-4"
                      style={{ fontSize: "clamp(14px, 1.35vw, 16px)" }}
                    >
                      {row.moveQuote}
                    </div>
                    {row.moveAttribution ? (
                      <div className="text-ink-dim font-mono text-[10px] leading-[1.55] tracking-[0.04em]">
                        {row.moveAttribution}
                      </div>
                    ) : null}
                  </>
                ) : (
                  <>
                    <div
                      className="font-display text-ink leading-[1.3] tracking-[-0.01em] lowercase mb-3"
                      style={{ fontSize: "clamp(16px, 1.6vw, 20px)" }}
                    >
                      {row.moveQuote}
                    </div>
                    {row.moveAttribution ? (
                      <div className="text-ink-dim font-mono text-[11px] leading-[1.55] tracking-[0.02em]">
                        {row.moveAttribution}
                      </div>
                    ) : null}
                  </>
                )}
              </div>

              <div className="px-6 py-7 border-b md:border-b-0 md:border-r border-line bg-bg">
                <div className="md:hidden text-[10px] text-accent tracking-[0.18em] uppercase mb-3 font-semibold">
                  // requires
                </div>
                <ul className="space-y-2.5 text-ink-dim text-[12px] leading-[1.6]">
                  {row.requires.map((r, i) => (
                    <li key={i} className="flex gap-2">
                      <span className="text-accent flex-shrink-0">▸</span>
                      <span>{r}</span>
                    </li>
                  ))}
                </ul>
              </div>

              <div className="px-6 py-7 bg-bg">
                <div className="md:hidden text-[10px] text-accent tracking-[0.18em] uppercase mb-3 font-semibold">
                  // net provides
                </div>
                <div className="space-y-4">
                  {row.provides.map((group, gi) => (
                    <Fragment key={gi}>
                      {gi > 0 ? (
                        <div className="border-t border-accent/50" />
                      ) : null}
                      <ul className="space-y-2.5 text-ink text-[12px] leading-[1.6]">
                        {group.map((p, i) => (
                          <li key={i} className="flex gap-2">
                            <span className="text-accent flex-shrink-0">▸</span>
                            <span>{p}</span>
                          </li>
                        ))}
                      </ul>
                    </Fragment>
                  ))}
                </div>
              </div>
            </div>
          );
        })}
      </div>
    </section>
  );
}

/* ───────────────────────── §03 — Structural moat ───────────────────────── */

function MoatSection(): JSX.Element {
  return (
    <section id="moat" className="border-b border-line px-6 py-20">
      <div className="sec-label text-[10px] tracking-[0.2em] text-accent uppercase mb-3 flex items-center">
        §04 / the structural moat.
      </div>
      <h2
        className="font-display leading-none tracking-[-0.01em] text-ink mb-10 max-w-[900px] lowercase"
        style={{ fontSize: "clamp(36px, 5vw, 60px)" }}
      >
        the moat.
      </h2>

      <p className="text-[16px] mt-8 max-w-[920px] leading-[1.7] text-ink">
        <span className="text-accent font-semibold">
          Only Nvidia hardware can hit every box today.
        </span>{" "}
        AMD MI300X has no equivalent attested CC mode shipping at fleet scale.
        Consumer and datacenter Ada/Ampere can hit some boxes, not all. The
        premium tier filter is a structural Nvidia moat baked into the
        protocol.
      </p>

      <p className="text-[16px] mt-7 max-w-[920px] leading-[1.7] text-ink">
        Nvidia&apos;s hardware advantages — TEE, NVLink, FP8 native,
        deterministic latency — are invisible at the pricing layer today and
        therefore unmonetizable. NET makes the hardware queryable, attested,
        and priced into the match.
      </p>

      <div className="border-l-2 border-accent pl-8 pr-8 py-8 bg-accent/[0.02] mt-14 max-w-[1100px] mx-auto">
        <div className="text-[10px] text-accent tracking-[0.18em] mb-3">
          // pull quote
        </div>
        <blockquote
          className="font-display text-ink leading-[1.25] tracking-[-0.01em] lowercase"
          style={{ fontSize: "clamp(20px, 2.4vw, 32px)" }}
        >
          &ldquo;Net turns every attested NVIDIA card into a premium-tier
          capability that prices itself above commodity,{" "}
          <span className="text-accent">
            while still capturing every idle minute as commodity downshift
            revenue.
          </span>
          &rdquo;
        </blockquote>
      </div>

      <p
        className="font-display text-accent leading-[1.2] tracking-[-0.01em] mt-10 max-w-[1100px] mx-auto text-center lowercase"
        style={{ fontSize: "clamp(20px, 2.4vw, 32px)" }}
      >
        &ldquo;AMD can&apos;t enter the premium tier without parallel
        attestation infrastructure they haven&apos;t built.&rdquo;
      </p>
    </section>
  );
}

/* ───────────────────────── §04 — Cryptographic settlement ───────────────────────── */

interface FlowNode {
  label: string;
  caption: string;
}

const SETTLEMENT_NODES: ReadonlyArray<FlowNode> = [
  {
    label: "centralized credit authority",
    caption: "cryptographically signed envelope",
  },
  { label: "buyer", caption: "forwards proof of funds" },
  { label: "seller", caption: "adds contract signature" },
  { label: "combined signature", caption: "atomic capacity-for-payment" },
];

function SettlementSection(): JSX.Element {
  return (
    <section id="settlement" className="border-b border-line px-6 py-20">
      <div className="sec-label text-[10px] tracking-[0.2em] text-accent uppercase mb-3 flex items-center">
        §07 / cryptographically provable settlement authorization.
      </div>
      <h2
        className="font-display leading-none tracking-[-0.01em] text-ink mb-12 max-w-[900px] lowercase"
        style={{ fontSize: "clamp(36px, 5vw, 60px)" }}
      >
        nobody else does this.
      </h2>

      {/* Flow diagram */}
      <div className="border border-line p-6 md:p-10 bg-bg-2/40 mb-14">
        <div className="flex flex-col md:flex-row items-stretch gap-4 md:gap-0 justify-between">
          {SETTLEMENT_NODES.map((n, i) => (
            <div
              key={n.label}
              className="flex flex-col md:flex-row items-center gap-4 md:gap-3 flex-1"
            >
              <div className="flex-1 border border-accent bg-bg p-4 min-h-[100px] flex flex-col items-center justify-center text-center w-full">
                <div className="text-accent font-display lowercase text-[13px] md:text-[14px] leading-[1.2] mb-2 tracking-[-0.01em]">
                  {n.label}
                </div>
                <div className="text-ink-dim font-mono text-[10px] tracking-[0.04em] leading-[1.4]">
                  {n.caption}
                </div>
              </div>
              {i < SETTLEMENT_NODES.length - 1 ? (
                <div className="text-accent font-mono text-[24px] flex items-center justify-center md:px-1 rotate-90 md:rotate-0">
                  →
                </div>
              ) : null}
            </div>
          ))}
        </div>
      </div>

      {/* Two-column properties */}
      <div className="grid grid-cols-1 md:grid-cols-2 gap-px bg-line border border-line">
        <PropertyColumn
          label="// what this gives you"
          items={[
            "Delayed settlement without trust",
            "Pre-cleared credit without PCI exposure",
            "Provable authorization without central control plane",
            "Atomic delivery of capacity-for-payment",
            "Trustless matching between anonymous parties",
          ]}
        />
        <PropertyColumn
          label="// what this avoids"
          items={[
            "Zero fraud window",
            "No arbitration requirement",
            "No per-job escrow",
            "No dependency on online banking rails",
            "No buyer/seller repudiation",
          ]}
        />
      </div>

      <p className="text-[15px] mt-12 max-w-[980px] mx-auto text-ink leading-[1.7]">
        This is exactly what modern securities exchanges do, except you&apos;re
        doing it without a central matching engine, without a global ledger,
        inside the mesh substrate, with signed envelopes, using deterministic
        identity.
      </p>

      <div className="mt-16 text-center py-12 border-t border-b border-accent-dim bg-accent/[0.02]">
        <div
          className="font-display leading-[1.2] tracking-[-0.01em] max-w-[1100px] mx-auto lowercase"
          style={{ fontSize: "clamp(24px, 3.4vw, 44px)" }}
        >
          <span className="text-accent">Beyond anything</span>{" "}
          <span className="text-ink">
            AWS, GCP, Cloudflare, or CoreWeave do.
          </span>
        </div>
      </div>
    </section>
  );
}

function PropertyColumn({
  label,
  items,
}: {
  label: string;
  items: ReadonlyArray<string>;
}): JSX.Element {
  return (
    <div className="bg-bg p-7">
      <div className="text-[10px] text-accent tracking-[0.18em] uppercase mb-5 font-semibold">
        {label}
      </div>
      <ul className="space-y-2.5 text-ink text-[13px] leading-[1.6]">
        {items.map((it) => (
          <li key={it} className="flex gap-2">
            <span className="text-accent flex-shrink-0">▸</span>
            <span>{it}</span>
          </li>
        ))}
      </ul>
    </div>
  );
}

/* ───────────────────────── §05 — Two revenue streams ───────────────────────── */

function CardFlowSection(): JSX.Element {
  return (
    <section id="streams" className="border-b border-line px-6 py-20">
      <div className="sec-label text-[10px] tracking-[0.2em] text-accent uppercase mb-3 flex items-center">
        §03 / two revenue streams. one card.
      </div>
      <h2
        className="font-display leading-none tracking-[-0.01em] text-ink mb-12 max-w-[900px] lowercase"
        style={{ fontSize: "clamp(36px, 5vw, 60px)" }}
      >
        one loaded h100. three flows.
      </h2>

      {/* Memory-mapped H100 + three hierarchied flows + price column + bracket */}
      <div className="border border-line p-6 md:p-8 bg-bg-2/40 mb-12">
        {/* Desktop SVG diagram */}
        <div className="hidden md:block">
          <svg
            viewBox="0 0 1000 460"
            preserveAspectRatio="xMidYMid meet"
            className="w-full h-auto"
            aria-label="One H100, three concurrent revenue flows off the same physical loaded state"
          >
            <defs>
              <pattern
                id="hatch-lime"
                patternUnits="userSpaceOnUse"
                width="8"
                height="8"
                patternTransform="rotate(45)"
              >
                <line
                  x1="0"
                  y1="0"
                  x2="0"
                  y2="8"
                  stroke="#c4ff3d"
                  strokeWidth="1"
                  strokeOpacity="0.4"
                />
              </pattern>
              <marker
                id="arr-lime"
                viewBox="0 0 12 12"
                refX="10"
                refY="6"
                markerWidth="7"
                markerHeight="7"
                orient="auto-start-reverse"
              >
                <path d="M0,0 L12,6 L0,12 z" fill="#c4ff3d" />
              </marker>
              <marker
                id="arr-ink"
                viewBox="0 0 12 12"
                refX="10"
                refY="6"
                markerWidth="7"
                markerHeight="7"
                orient="auto-start-reverse"
              >
                <path d="M0,0 L12,6 L0,12 z" fill="#d4dcd0" />
              </marker>
              <marker
                id="arr-dim"
                viewBox="0 0 12 12"
                refX="10"
                refY="6"
                markerWidth="6"
                markerHeight="6"
                orient="auto-start-reverse"
              >
                <path d="M0,0 L12,6 L0,12 z" fill="#6b7568" />
              </marker>
            </defs>

            {/* H100 memory-mapped card */}
            <rect x="40" y="40" width="160" height="240" fill="#c4ff3d" />
            <rect
              x="40"
              y="280"
              width="160"
              height="80"
              fill="url(#hatch-lime)"
              stroke="#c4ff3d"
              strokeWidth="1.5"
            />
            <rect
              x="40"
              y="40"
              width="160"
              height="320"
              fill="none"
              stroke="#c4ff3d"
              strokeWidth="1.5"
            />

            {/* Primary partition labels */}
            <text x="120" y="148" textAnchor="middle" fontFamily="var(--font-mono)" fontSize="13" fill="#d4dcd0" letterSpacing="0.06em">
              70B FP8
            </text>
            <text x="120" y="168" textAnchor="middle" fontFamily="var(--font-mono)" fontSize="11" fill="#d4dcd0" letterSpacing="0.1em">
              PRIMARY LOAD
            </text>
            <text x="120" y="194" textAnchor="middle" fontFamily="var(--font-mono)" fontSize="10" fill="#d4dcd0" fillOpacity="0.85" letterSpacing="0.06em">
              ~60GB VRAM
            </text>

            {/* Spare partition labels */}
            <text x="120" y="313" textAnchor="middle" fontFamily="var(--font-mono)" fontSize="10" fill="#c4ff3d" letterSpacing="0.06em">
              // spare cycles
            </text>
            <text x="120" y="332" textAnchor="middle" fontFamily="var(--font-mono)" fontSize="9" fill="#6b7568" letterSpacing="0.04em">
              ~20GB VRAM available
            </text>

            {/* Card sub-labels below */}
            <text x="120" y="386" textAnchor="middle" fontFamily="var(--font-mono)" fontSize="11" fill="#c4ff3d" letterSpacing="0.14em">
              H100 SXM 80GB
            </text>
            <text x="120" y="403" textAnchor="middle" fontFamily="var(--font-mono)" fontSize="10" fill="#6b7568" letterSpacing="0.08em">
              70B FP8 LOADED
            </text>

            {/* Flow 1 — Premium tier */}
            <text x="220" y="90" fontFamily="var(--font-mono)" fontSize="10" fill="#c4ff3d" letterSpacing="0.18em">
              // PREMIUM TIER
            </text>
            <text x="220" y="115" fontFamily="var(--font-mono)" fontSize="14" fill="#c4ff3d">
              → 70B at premium tier — full SLA, attested
            </text>
            <line x1="200" y1="160" x2="555" y2="130" stroke="#c4ff3d" strokeWidth="3" markerEnd="url(#arr-lime)" />
            <text x="830" y="138" textAnchor="end" fontFamily="var(--font-display)" fontSize="22" fill="#c4ff3d">
              premium $/Mtok
            </text>

            {/* Flow 2 — Commodity downshift */}
            <text x="220" y="208" fontFamily="var(--font-mono)" fontSize="10" fill="#d4dcd0" letterSpacing="0.18em">
              // COMMODITY TIER — DOWNSHIFT
            </text>
            <text x="220" y="232" fontFamily="var(--font-mono)" fontSize="12" fill="#d4dcd0">
              → 13B downshift — idle minutes
            </text>
            <line x1="200" y1="300" x2="555" y2="250" stroke="#d4dcd0" strokeWidth="2" markerEnd="url(#arr-ink)" />
            <text x="830" y="256" textAnchor="end" fontFamily="var(--font-display)" fontSize="16" fill="#d4dcd0">
              lower $/Mtok
            </text>

            {/* Flow 3 — Embedding fallback */}
            <text x="220" y="328" fontFamily="var(--font-mono)" fontSize="9" fill="#6b7568" letterSpacing="0.18em">
              // COMMODITY TIER — FALLBACK
            </text>
            <text x="220" y="345" fontFamily="var(--font-mono)" fontSize="11" fill="#6b7568">
              → embedding fallback — smallest jobs
            </text>
            <line x1="200" y1="350" x2="555" y2="350" stroke="#6b7568" strokeWidth="1" markerEnd="url(#arr-dim)" />
            <text x="830" y="356" textAnchor="end" fontFamily="var(--font-mono)" fontSize="12" fill="#6b7568">
              commodity $/Mtok
            </text>

            {/* Bracket grouping all three price endpoints */}
            <g stroke="#c4ff3d" strokeWidth="1.5" fill="none">
              <line x1="850" y1="100" x2="870" y2="100" />
              <line x1="870" y1="100" x2="870" y2="370" />
              <line x1="870" y1="370" x2="850" y2="370" />
            </g>

            {/* Bracket annotation label */}
            <text x="500" y="430" textAnchor="middle" fontFamily="var(--font-mono)" fontSize="10" fill="#c4ff3d" letterSpacing="0.18em">
              // SAME PHYSICAL LOADED STATE. THREE CONCURRENT REVENUE STREAMS.
            </text>
          </svg>
        </div>

        {/* Mobile stacked layout */}
        <div className="md:hidden flex flex-col gap-5">
          <div className="border-2 border-accent">
            <div className="bg-accent p-4 text-center">
              <div className="font-mono text-[12px] text-ink tracking-[0.08em]">
                70B FP8 PRIMARY LOAD
              </div>
              <div className="font-mono text-[10px] text-ink/85 mt-1 tracking-[0.06em]">
                ~60GB VRAM
              </div>
            </div>
            <div
              className="border-t-2 border-accent p-3 text-center"
              style={{
                backgroundImage:
                  "repeating-linear-gradient(45deg, transparent 0 6px, rgba(196,255,61,0.35) 6px 7px)",
              }}
            >
              <div className="font-mono text-[10px] text-accent">
                // spare cycles
              </div>
              <div className="font-mono text-[9px] text-ink-dim mt-1">
                ~20GB VRAM available
              </div>
            </div>
          </div>
          <div className="text-center">
            <div className="font-mono text-[11px] text-accent tracking-[0.14em]">
              H100 SXM 80GB
            </div>
            <div className="font-mono text-[10px] text-ink-dim tracking-[0.08em] mt-1">
              70B FP8 LOADED
            </div>
          </div>

          <div className="border-l-[3px] border-accent pl-4 py-2 mt-4">
            <div className="text-accent text-[10px] tracking-[0.18em] uppercase mb-1.5">
              // premium tier
            </div>
            <div className="text-accent font-mono text-[13px] leading-[1.5]">
              → 70B at premium tier — full SLA, attested
            </div>
            <div className="text-accent font-display text-[18px] mt-2 lowercase tracking-[-0.01em]">
              premium $/Mtok
            </div>
          </div>

          <div className="border-l-2 border-ink pl-4 py-2">
            <div className="text-ink text-[10px] tracking-[0.18em] uppercase mb-1.5">
              // commodity tier — downshift
            </div>
            <div className="text-ink font-mono text-[12px] leading-[1.5]">
              → 13B downshift — idle minutes
            </div>
            <div className="text-ink font-display text-[15px] mt-2 lowercase tracking-[-0.01em]">
              lower $/Mtok
            </div>
          </div>

          <div className="border-l border-ink-dim pl-4 py-2">
            <div className="text-ink-dim text-[10px] tracking-[0.18em] uppercase mb-1.5">
              // commodity tier — fallback
            </div>
            <div className="text-ink-dim font-mono text-[11px] leading-[1.5]">
              → embedding fallback — smallest jobs
            </div>
            <div className="text-ink-dim font-mono text-[12px] mt-2">
              commodity $/Mtok
            </div>
          </div>

          <div className="text-accent font-mono text-[10px] tracking-[0.15em] uppercase text-center border-t border-accent-dim pt-4 mt-2">
            // same physical loaded state — three concurrent revenue streams
          </div>
        </div>
      </div>

      <p className="text-[15px] max-w-[980px] leading-[1.7] text-ink">
        The premium tier slot earns the high margin. The downshift fills the
        spare cycles.{" "}
        <span className="text-accent">
          Net is the only substrate that prices both simultaneously off the
          same physical loaded state.
        </span>
      </p>

      <p className="text-[15px] mt-7 max-w-[980px] leading-[1.7] text-ink">
        Nvidia today gets neither. DGX Cloud sells the H100 hour as a single
        unit at one price. Run:ai optimizes internally but doesn&apos;t connect
        to a settlement market.{" "}
        <span className="text-accent">
          The premium-tier-plus-downshift pattern is unbuildable without a
          neutral substrate — and Nvidia doesn&apos;t own one.
        </span>
      </p>

      {/* Three stat callouts */}
      <div className="grid grid-cols-1 sm:grid-cols-3 gap-px bg-line border border-line mt-14">
        <StatCallout
          label="// reload cost avoided"
          big="30–300s"
          caption="per cold start. 10–80 GB PCIe, NVLink thrash."
        />
        <StatCallout
          label="// loaded GPU finds work"
          big="→ 1"
          caption="probability approaches 1 with bidirectional fallback."
        />
        <StatCallout
          label="// idle minutes per H100 / day"
          big="single digits"
          caption="with bidirectional fallback."
          bigSize="clamp(20px, 2.4vw, 30px)"
        />
      </div>
    </section>
  );
}

function StatCallout({
  label,
  big,
  caption,
  bigSize = "clamp(28px, 3.4vw, 44px)",
}: {
  label: string;
  big: string;
  caption: string;
  bigSize?: string;
}): JSX.Element {
  return (
    <div className="bg-bg p-7 flex flex-col">
      <div className="text-[10px] text-ink-dim tracking-[0.18em] uppercase mb-4 font-medium">
        {label}
      </div>
      <div
        className="font-display text-accent leading-none mb-1.5 lowercase whitespace-nowrap"
        style={{ fontSize: bigSize }}
      >
        {big}
      </div>
      <div className="text-ink-dim text-[12px] leading-[1.6] mt-3">
        {caption}
      </div>
    </div>
  );
}

/* ───────────────────────── §06 — Three products ───────────────────────── */

interface Product {
  label: string;
  head: string;
  body: string;
  icon: ReactNode;
}

const PRODUCTS: ReadonlyArray<Product> = [
  {
    label: "// settlement at the protocol layer",
    head: "Per-flow settlement on attested compute.",
    body: "Cryptographically signed envelopes. Atomic capacity-for-payment. No central matching engine. Moat: protocol effects — more nodes, more matches, more nodes.",
    icon: (
      <>
        <rect x="4" y="6" width="16" height="12" rx="1" />
        <path d="M4 10h16M8 14h3M13 14h3" />
      </>
    ),
  },
  {
    label: "// real-time demand intelligence",
    head: "Bloomberg-terminal-for-compute.",
    body: "Aggregated cross-provider demand signals by region, model class, quant, hardware. Forecast H100/H200/B200 demand before the price moves. Doesn't exist today because no one has a neutral substrate carrying real demand signals across providers. Moat: aggregation effects.",
    icon: (
      <>
        <path d="M3 19h18" />
        <path d="M5 19V10M10 19V5M15 19V13M20 19V8" />
      </>
    ),
  },
  {
    label: "// premium-tier sla escrow",
    head: "SLA insurance at the protocol layer.",
    body: "Penalty clauses on SLA breach paid out of node escrow. Regulated workloads (healthcare, defense, finance, sovereign) cannot legally accept counterparty risk elsewhere. Moat: trust + escrow effects.",
    icon: (
      <>
        <path d="M12 3l9 4v5c0 5-4 8-9 9-5-1-9-4-9-9V7l9-4z" />
        <path d="M8 12l3 3 5-6" />
      </>
    ),
  },
];

function ProductsSection(): JSX.Element {
  return (
    <section id="products" className="border-b border-line px-6 py-20">
      <div className="sec-label text-[10px] tracking-[0.2em] text-accent uppercase mb-3 flex items-center">
        §06 / three products. one protocol.
      </div>
      <h2
        className="font-display leading-none tracking-[-0.01em] text-ink mb-10 max-w-[900px] lowercase"
        style={{ fontSize: "clamp(36px, 5vw, 60px)" }}
      >
        three products. one protocol.
      </h2>

      <div className="grid grid-cols-1 md:grid-cols-3 gap-px bg-line border border-line">
        {PRODUCTS.map((p) => (
          <div
            key={p.label}
            className="bg-bg p-7 flex flex-col border border-accent-dim/40 transition-colors hover:bg-bg-2"
          >
            <svg
              className="w-6 h-6 text-accent mb-5"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth={1.4}
              strokeLinecap="round"
              strokeLinejoin="round"
            >
              {p.icon}
            </svg>
            <div className="text-[10px] text-accent tracking-[0.18em] uppercase mb-4 font-semibold">
              {p.label}
            </div>
            <div className="font-display text-ink text-[18px] leading-[1.25] tracking-[-0.01em] mb-4 lowercase">
              {p.head}
            </div>
            <div className="text-ink-dim text-[13px] leading-[1.65]">
              {p.body}
            </div>
          </div>
        ))}
      </div>

      <p className="text-[13px] mt-10 text-center font-mono leading-[1.7]">
        <span className="text-accent">Three separate monetization paths.</span>{" "}
        <span className="text-ink">
          One neutral coordination layer. Each a defensible competitive moat.
        </span>{" "}
        <span className="text-accent">
          Creating new multi-billion-dollar revenue surfaces.
        </span>
      </p>
    </section>
  );
}

/* ───────────────────────── §07 — Sovereign hook ───────────────────────── */

function SovereignTierSection(): JSX.Element {
  return (
    <section id="sovereign-tier" className="border-b border-line px-6 py-20">
      <div className="sec-label text-[10px] tracking-[0.2em] text-accent uppercase mb-3 flex items-center">
        §05 / sovereign. the next-decade play.
      </div>
      <h2
        className="font-display leading-none tracking-[-0.01em] text-ink mb-10 max-w-[900px] lowercase"
        style={{ fontSize: "clamp(36px, 5vw, 60px)" }}
      >
        the next-decade play.
      </h2>

      <p className="text-[16px] mt-6 max-w-[980px] leading-[1.7] text-ink">
        DGX Cloud Sovereign regions already segment by sovereignty — US-Gov,
        EU, and the regional sovereign clouds being built with Mistral, G42,
        Saudi. No substrate carries verified org identity across regions.
        Run:ai doesn&apos;t. They built region-level isolation as a workaround.
      </p>

      <p className="text-[16px] mt-7 max-w-[980px] leading-[1.7] text-ink">
        <span className="text-accent font-semibold">
          NET carries org verification on every packet
        </span>{" "}
        via SubnetId and identity primitives the substrate already has.
      </p>

      <p className="text-[16px] mt-7 max-w-[980px] leading-[1.7] text-ink">
        DGX Cloud Sovereign regions plug into a global mesh and still enforce
        cryptographic org boundaries — no bespoke per-region integration.{" "}
        <span className="text-accent">
          The org-verified tier prices itself above commodity in every region
          simultaneously.
        </span>
      </p>

      <div className="grid grid-cols-1 md:grid-cols-2 gap-px bg-line border border-line mt-12">
        <div className="bg-bg p-7">
          <div className="text-[10px] text-accent tracking-[0.18em] uppercase mb-4 font-semibold">
            // trust roots today
          </div>
          <div className="text-ink text-[13px] leading-[1.7] font-mono">
            Anthropic · OpenAI · DoD · JP Morgan · NHS · Bundeswehr
          </div>
        </div>
        <div className="bg-bg p-7">
          <div className="text-[10px] text-accent tracking-[0.18em] uppercase mb-4 font-semibold">
            // sovereign clouds being built
          </div>
          <div className="text-ink text-[13px] leading-[1.7] font-mono">
            Mistral (EU) · G42 (UAE) · Saudi · US-Gov · regional sovereign
          </div>
        </div>
      </div>
    </section>
  );
}

/* ───────────────────────── §08 — Closing ───────────────────────── */

function ClosingSection(): JSX.Element {
  return (
    <section className="border-b border-line px-6 py-20">
      <div className="mt-12 text-center py-16 border-t border-b border-accent-dim bg-accent/[0.02]">
        <div className="flex items-center justify-center gap-6 flex-wrap max-w-[1200px] mx-auto px-6">
          <svg
            className="w-10 h-10 text-accent flex-shrink-0"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth={1.5}
            strokeLinecap="round"
            strokeLinejoin="round"
            aria-hidden="true"
          >
            <circle cx="12" cy="12" r="9" />
            <ellipse cx="12" cy="12" rx="4" ry="9" />
            <path d="M3 12h18" />
          </svg>
          <div
            className="font-display text-ink leading-[1.2] text-left lowercase"
            style={{ fontSize: "clamp(22px, 3vw, 38px)" }}
          >
            NET does not sell idle GPUs.
            <br />
            NET turns{" "}
            <span className="text-accent">stranded premium capacity</span> into{" "}
            <span className="text-accent">settled capacity</span> on{" "}
            <span className="text-accent">
              the only tier nvidia can populate
            </span>
            .
          </div>
        </div>

        <p className="font-mono text-[13px] leading-[1.5] mt-10 max-w-[900px] mx-auto px-6">
          <span className="text-ink">
            Utilization is a feature; structural pricing power for their
            hardware against AMD is a thesis.
          </span>
          <br />
          <span className="text-ink">
            Strategic substrate power. GPU liquidity. Fleet-level arbitrage.
          </span>
          <br />
          <span className="text-accent">
            A new multi-billion-dollar revenue surface.
          </span>
          <br />
          <span className="text-accent">
            How NET creates a secondary GPU market.
          </span>
        </p>

        <p
          className="font-display text-accent leading-[1.1] tracking-[-0.01em] mt-10 lowercase"
          style={{ fontSize: "clamp(28px, 4vw, 56px)" }}
        >
          That&apos;s the angle.
        </p>
      </div>
    </section>
  );
}
