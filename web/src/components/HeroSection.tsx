"use client";

import Link from "next/link";
import { ArrowDown, ArrowRight, ArrowDownRight } from "@phosphor-icons/react";
import { useState, useEffect } from "react";
import { FieldCanvas } from "./field/FieldCanvas";
import { useRepoInfo } from "./RepoInfoProvider";
import { buildQuarter } from "@/lib/utils";
import globals from "@/lib/globals";

// The proximity readout that used to sit beside a small graph now names the
// field itself: the graph is the shards and threads behind the title.
function ProximitySlab() {
  return (
    <div className="slab p-5">
      <div className="flex justify-between items-center border-b border-line pb-2.5 mb-4 text-[10px] tracking-[0.16em] text-ink-dim uppercase font-mono">
        <span className="text-ink">mesh.proximity</span>
        <span className="flex items-center gap-1.5">
          <span className="w-1.5 h-1.5 bg-accent inline-block" />
          OBSERVABLE
        </span>
      </div>
      <div className="grid grid-cols-2 gap-x-6 gap-y-4">
        <MeshStat label="forward / hop" value="0.57" unit="ns" />
        <MeshStat label="recovery cycle" value="291" unit="ns" />
        <MeshStat label="cdylib size (base)" value="2.60" unit="mb" />
        <MeshStat label="throughput" value="26.5" unit="M ops/s" />
      </div>
    </div>
  );
}

function MeshStat({
  label,
  value,
  unit,
}: {
  label: string;
  value: string;
  unit: string;
}) {
  return (
    <div>
      <div className="text-ink-dim text-[9.5px] tracking-[0.14em] uppercase mb-1.5 font-mono">
        {label}
      </div>
      <div className="font-display font-bold text-ink text-[26px] leading-none tabular-nums">
        {value}
        <span className="font-mono font-normal text-ink-dim text-[10px] ml-1.5 tracking-[0.06em]">
          {unit}
        </span>
      </div>
    </div>
  );
}

type EventHighlight = "accent" | "warn" | "cyan";

interface LiveEvent {
  id: number;
  ts: string;
  type: string;
  typeColor: string;
  body: string;
  metric: string;
  metricColor: string;
  highlight?: EventHighlight;
}

interface EventTemplate {
  weight: number;
  gen: () => Omit<LiveEvent, "id" | "ts">;
}

let liveEventCounter = 0;

function liveTs(): string {
  const d = new Date();
  const m = String(d.getMinutes()).padStart(2, "0");
  const s = String(d.getSeconds()).padStart(2, "0");
  const ms = String(d.getMilliseconds()).padStart(3, "0");
  return `${m}:${s}.${ms}`;
}

function hex4(): string {
  return Math.floor(Math.random() * 0x10000)
    .toString(16)
    .padStart(4, "0");
}

function randInt(min: number, max: number): number {
  return Math.floor(Math.random() * (max - min + 1)) + min;
}

function pick<T>(arr: ReadonlyArray<T>, fallback: T): T {
  return arr[randInt(0, arr.length - 1)] ?? fallback;
}

const CAP_TAGS: ReadonlyArray<string> = [
  "gpu:rtx-4090",
  "vram:24gb",
  "region:eu-west",
  "tag:floor-A",
  "lat:<200μs",
  "tag:nyse-colo",
];

const DAEMON_NAMES: ReadonlyArray<string> = [
  "trader",
  "fusion",
  "infer",
  "plc",
  "agent",
  "router",
];

const WALL_REASONS: ReadonlyArray<string> = [
  "cap-mismatch",
  "ttl-expired",
  "rate-limit",
  "unsigned",
  "loop-detected",
];

const EVENT_TEMPLATES: ReadonlyArray<EventTemplate> = [
  {
    weight: 6,
    gen: () => ({
      type: "hb.ack",
      typeColor: "text-accent-dim",
      body: `0x${hex4()} ← 0x${hex4()}`,
      metric: `${randInt(28, 60)}ns`,
      metricColor: "text-ink",
    }),
  },
  {
    weight: 5,
    gen: () => ({
      type: "fwd.0",
      typeColor: "text-accent",
      body: `0x${hex4()} → 0x${hex4()}`,
      metric: `${randInt(45, 80)}ns`,
      metricColor: "text-accent",
    }),
  },
  {
    weight: 3,
    gen: () => ({
      type: "fwd.path",
      typeColor: "text-accent",
      body: `0x${hex4()} → 0x${hex4()} → 0x${hex4()}`,
      metric: `${randInt(120, 280)}ns`,
      metricColor: "text-accent",
    }),
  },
  {
    weight: 3,
    gen: () => ({
      type: "rt.hit",
      typeColor: "text-ink-dim",
      body: `0x${hex4()} cached`,
      metric: `${randInt(35, 42)}ns`,
      metricColor: "text-ink",
    }),
  },
  {
    weight: 3,
    gen: () => ({
      type: "cap.read",
      typeColor: "text-cyan",
      body: pick(CAP_TAGS, "gpu:rtx-4090"),
      metric: `${randInt(28, 48)}ns`,
      metricColor: "text-ink-dim",
    }),
  },
  {
    weight: 2,
    gen: () => ({
      type: "route",
      typeColor: "text-ink-dim",
      body: `seq=${randInt(80000, 99999)} hops=${randInt(1, 4)}`,
      metric: "—",
      metricColor: "text-ink-faint",
    }),
  },
  {
    weight: 2,
    gen: () => ({
      type: "pingwave",
      typeColor: "text-accent",
      body: `swarm radius=${randInt(2, 5)}`,
      metric: `${randInt(32, 58)}ns`,
      metricColor: "text-accent",
    }),
  },
  {
    weight: 1,
    gen: () => ({
      type: "peer.new",
      typeColor: "text-cyan",
      body: `0x${hex4()} ↑ joined subnet`,
      metric: `${randInt(95, 150)}ns`,
      metricColor: "text-cyan",
      highlight: "cyan",
    }),
  },
  {
    weight: 1,
    gen: () => ({
      type: "mikoshi",
      typeColor: "text-accent",
      body: `${pick(DAEMON_NAMES, "trader")}-${hex4()} ↗ node.0x${hex4()}`,
      metric: `${randInt(220, 320)}ns`,
      metricColor: "text-accent",
      highlight: "accent",
    }),
  },
  {
    weight: 1,
    gen: () => ({
      type: "superpose",
      typeColor: "text-accent",
      body: `0x${hex4()} dual-active 12–50ns`,
      metric: `${randInt(35, 42)}ns`,
      metricColor: "text-accent",
      highlight: "accent",
    }),
  },
  {
    weight: 1,
    gen: () => ({
      type: "wall.drop",
      typeColor: "text-warn",
      body: `0x${hex4()} ${pick(WALL_REASONS, "cap-mismatch")}`,
      metric: "blocked",
      metricColor: "text-warn",
      highlight: "warn",
    }),
  },
  {
    weight: 1,
    gen: () => ({
      type: "drop",
      typeColor: "text-warn",
      body: `0x${hex4()} buffer full`,
      metric: "evicted",
      metricColor: "text-warn",
    }),
  },
];

const TOTAL_EVENT_WEIGHT = EVENT_TEMPLATES.reduce((s, t) => s + t.weight, 0);

function pickEventTemplate(): EventTemplate {
  let r = Math.random() * TOTAL_EVENT_WEIGHT;
  for (const t of EVENT_TEMPLATES) {
    r -= t.weight;
    if (r <= 0) return t;
  }
  return (
    EVENT_TEMPLATES[0] ?? {
      weight: 1,
      gen: () => ({
        type: "hb.ack",
        typeColor: "text-accent-dim",
        body: "",
        metric: "—",
        metricColor: "text-ink-faint",
      }),
    }
  );
}

function makeLiveEvent(): LiveEvent {
  const tpl = pickEventTemplate();
  return {
    id: liveEventCounter++,
    ts: liveTs(),
    ...tpl.gen(),
  };
}

const EVENT_LOG_LINES = 7;

function MeshEventLog() {
  const [events, setEvents] = useState<readonly LiveEvent[]>([]);

  useEffect(() => {
    setEvents(Array.from({ length: EVENT_LOG_LINES }, () => makeLiveEvent()));

    const id = window.setInterval(() => {
      const r = Math.random();
      const burst = r < 0.05 ? 3 : r < 0.18 ? 2 : 1;
      const fresh = Array.from({ length: burst }, () => makeLiveEvent());
      setEvents((prev) => [...prev, ...fresh].slice(-EVENT_LOG_LINES));
    }, 3000);

    return () => window.clearInterval(id);
  }, []);

  return (
    <div className="slab p-5">
      <div className="flex justify-between items-center border-b border-line pb-2.5 mb-3.5 text-[10px] tracking-[0.16em] text-ink-dim uppercase font-mono">
        <span className="text-ink">event.tail</span>
        <span className="flex items-center gap-1.5">
          <span className="w-1.5 h-1.5 rounded-full bg-accent inline-block animate-pulse-dot" />
          SIMULATED
        </span>
      </div>
      <div className="font-mono text-[10.5px] leading-[1.6] overflow-hidden min-h-[136px] -mx-5">
        {events.map((e) => {
          const bg =
            e.highlight === "accent"
              ? "bg-accent/[0.08]"
              : e.highlight === "warn"
                ? "bg-warn/[0.08]"
                : e.highlight === "cyan"
                  ? "bg-cyan/[0.08]"
                  : "";
          return (
            <div
              key={e.id}
              className={`event-line-in flex items-baseline gap-2 whitespace-nowrap overflow-hidden px-5 ${bg}`}
            >
              <span
                className="text-ink-faint shrink-0"
                style={{ minWidth: "9ch" }}
              >
                {e.ts}
              </span>
              <span
                className={`${e.typeColor} shrink-0`}
                style={{ minWidth: "9ch" }}
              >
                {e.type}
              </span>
              <span className="text-ink-dim flex-1 truncate">{e.body}</span>
              <span className={`${e.metricColor} shrink-0`}>{e.metric}</span>
            </div>
          );
        })}
      </div>
      <div className="border-t border-line mt-3 pt-2.5 flex justify-between text-[9px] text-ink-dim tracking-[0.14em] uppercase font-mono">
        <span>● 450ms tick</span>
      </div>
    </div>
  );
}

export function HeroSection() {
  const { version, buildDate } = useRepoInfo();
  const rev =
    buildDate === "—" ? version : `${version} / ${buildQuarter(buildDate)}`;

  return (
    <section
      id="hero"
      className="relative overflow-hidden min-h-[calc(100svh-80px)] flex flex-col"
    >
      <div className="absolute inset-0">
        <FieldCanvas />
      </div>
      {/* the text side of the hall stays dark enough to read */}
      <div
        aria-hidden
        className="absolute inset-0 pointer-events-none"
        style={{
          background:
            "linear-gradient(90deg, rgba(11,12,11,0.94) 0%, rgba(11,12,11,0.82) 30%, rgba(11,12,11,0.2) 58%, rgba(11,12,11,0) 72%), linear-gradient(0deg, rgba(11,12,11,1) 0%, rgba(11,12,11,0) 22%)",
        }}
      />

      {/* phones: the field sits behind every line of text, so dim it more */}
      <div
        aria-hidden
        className="absolute inset-0 pointer-events-none lg:hidden"
        style={{
          background:
            "linear-gradient(180deg, rgba(11,12,11,0.15) 0%, rgba(11,12,11,0.72) 34%, rgba(11,12,11,0.8) 100%)",
        }}
      />

      <div className="relative flex-1 grid grid-cols-1 lg:grid-cols-[minmax(0,1fr)_400px] gap-12 px-6 lg:px-10 pt-12 lg:pt-14 pb-12">
        <div className="flex flex-col">
          <h1
            className="font-display font-bold uppercase leading-[0.86] tracking-[-0.04em] text-ink"
            style={{ fontSize: "clamp(60px, 8.3vw, 132px)" }}
          >
            <span className="field-rise" style={{ animationDelay: "0.1s" }}>
              net.
            </span>
            <span
              className="field-rise text-accent"
              style={{ animationDelay: "0.24s" }}
            >
              moves
            </span>
            <span className="field-rise" style={{ animationDelay: "0.38s" }}>
              at light.
            </span>
          </h1>

          <div className="field-fade" style={{ animationDelay: "0.7s" }}>
            <p className="text-[20px] md:text-[22px] text-ink mt-9 max-w-[30em] leading-[1.45] font-light">
              A latency-first encrypted mesh where every computer, app and
              device is a first-class node. Existing networks operate in
              milliseconds{" "}
              <em className="not-italic font-mono text-[0.82em] text-ink-dim">
                (10⁻³)
              </em>
              . NET operates in nanoseconds{" "}
              <em className="not-italic font-mono text-[0.82em] text-accent">
                (10⁻⁹)
              </em>
              .
            </p>

            <p className="text-[15px] text-ink-dim mt-6 max-w-[36em] leading-[1.6]">
              No clients. No servers. No coordinators. The mesh propagates
              state, not connections.
            </p>

            <p className="text-[15px] text-ink-dim mt-2 max-w-[36em] leading-[1.6]">
              Flagship use:{" "}
              <Link href="/docs/worldview/agentic-mesh" className="link-field">
                agentic capability federation
              </Link>{" "}
              — agents discovering, invoking, and recovering work across a
              trusted mesh.
            </p>

            <div className="mt-9 flex gap-4 flex-wrap items-center">
              <Link href={globals.links.install} className="btn-field">
                <ArrowDown size={14} weight="bold" aria-hidden />
                Install NET
                <ArrowRight size={14} weight="bold" aria-hidden />
              </Link>
              <a href="#bench" className="btn-ghost-field">
                Read benchmarks
              </a>
              <a
                href="#properties"
                className="inline-flex items-center gap-2 py-3 font-mono text-[11px] tracking-[0.16em] uppercase text-ink-dim hover:text-accent transition-colors"
              >
                {"// view spec"}
                <ArrowDownRight size={13} weight="bold" aria-hidden />
              </a>
            </div>
          </div>

          <div className="mt-auto pt-16 stencil flex flex-wrap gap-x-6 gap-y-2 items-center">
            <span className="text-ink">RFC-NET-001</span>
            <span>PROTOCOL.0x4E45·54</span>
            <span>REV {rev}</span>
          </div>
        </div>

        <div className="hidden lg:flex flex-col gap-12 self-end pb-2 field-sync">
          <div>
            <ProximitySlab />
          </div>
          <div>
            <MeshEventLog />
          </div>
        </div>
      </div>
    </section>
  );
}
