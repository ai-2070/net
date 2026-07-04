"use client";

import { PacketRain } from "./PacketRain";
import { useRepoInfo } from "./RepoInfoProvider";
import { buildQuarter } from "@/lib/utils";
import { useState, useEffect, useRef, useMemo } from "react";
import Link from "next/link";
import globals from "@/lib/globals";

interface Node3D {
  x: number;
  y: number;
  z: number;
  hasLabel?: boolean;
}

const NODES_3D: Record<string, Node3D> = {
  N0: { x: -0.85, y: -0.18, z: 0.3, hasLabel: true },
  N1: { x: -0.55, y: 0.4, z: -0.25, hasLabel: true },
  N2: { x: -0.15, y: -0.55, z: 0.55 },
  N3: { x: 0.05, y: -0.05, z: 0.1, hasLabel: true },
  N4: { x: 0.2, y: 0.55, z: -0.4 },
  N5: { x: 0.5, y: -0.3, z: 0.6, hasLabel: true },
  N6: { x: 0.85, y: 0.2, z: 0.15, hasLabel: true },
  N7: { x: 0.7, y: -0.55, z: -0.3 },
  N8: { x: -0.4, y: -0.65, z: -0.2 },
  N9: { x: -0.05, y: 0.65, z: 0.4, hasLabel: true },
  N10: { x: 0.5, y: 0.55, z: 0.2 },
  N11: { x: -0.75, y: 0.1, z: -0.55, hasLabel: true },
};

const NODE_IDS_3D: ReadonlyArray<string> = Object.keys(NODES_3D);

const ADJ_3D: Record<string, ReadonlyArray<string>> = {
  N0: ["N1", "N2", "N11"],
  N1: ["N0", "N3", "N9", "N11"],
  N2: ["N0", "N3", "N5", "N8"],
  N3: ["N1", "N2", "N4", "N6", "N9"],
  N4: ["N3", "N9", "N10"],
  N5: ["N2", "N6", "N7"],
  N6: ["N3", "N5", "N7", "N10"],
  N7: ["N5", "N6", "N8", "N11"],
  N8: ["N2", "N7", "N11"],
  N9: ["N1", "N3", "N4"],
  N10: ["N4", "N6"],
  N11: ["N0", "N1", "N7", "N8"],
};

function edgeKey3D(a: string, b: string): string {
  return a < b ? `${a}|${b}` : `${b}|${a}`;
}

const EDGES_3D: ReadonlyArray<readonly [string, string]> = (() => {
  const seen = new Set<string>();
  const out: Array<readonly [string, string]> = [];
  for (const [a, neighbors] of Object.entries(ADJ_3D)) {
    for (const b of neighbors) {
      const key = edgeKey3D(a, b);
      if (seen.has(key)) continue;
      seen.add(key);
      out.push([a, b] as const);
    }
  }
  return out;
})();

const ENDPOINT_POOL_3D: ReadonlyArray<string> = [
  "N0",
  "N5",
  "N6",
  "N7",
  "N9",
  "N10",
  "N11",
];

function shortestPath3D(from: string, to: string): string[] {
  const queue: string[][] = [[from]];
  const seen = new Set<string>([from]);
  while (queue.length > 0) {
    const path = queue.shift();
    if (!path) break;
    const last = path[path.length - 1];
    if (!last) continue;
    if (last === to) return path;
    const adj = ADJ_3D[last];
    if (!adj) continue;
    for (const n of adj) {
      if (!seen.has(n)) {
        seen.add(n);
        queue.push([...path, n]);
      }
    }
  }
  return [from, to];
}

interface Projected {
  sx: number;
  sy: number;
  depth: number;
}

const VIEW_W = 320;
const VIEW_H = 220;
const CENTER_X = VIEW_W / 2;
const CENTER_Y = VIEW_H / 2;
const CAMERA_DIST = 2.1;
const PROJECT_SCALE = 280;
const TILT = 0.18;

function project3D(n: Node3D, angle: number): Projected {
  const cosA = Math.cos(angle);
  const sinA = Math.sin(angle);
  let x = n.x * cosA + n.z * sinA;
  let z = -n.x * sinA + n.z * cosA;
  let y = n.y;

  const cosT = Math.cos(TILT);
  const sinT = Math.sin(TILT);
  const yT = y * cosT - z * sinT;
  const zT = y * sinT + z * cosT;
  y = yT;
  z = zT;

  const persp = 1 / (CAMERA_DIST + z);
  return {
    sx: CENTER_X + x * persp * PROJECT_SCALE,
    sy: CENTER_Y + y * persp * PROJECT_SCALE,
    depth: z,
  };
}

function MeshViz() {
  const [angle, setAngle] = useState(0);
  const [packetT, setPacketT] = useState(0);
  const [activePath, setActivePath] = useState<string[]>(["N0", "N6"]);
  const [labels, setLabels] = useState<Record<string, string>>({});
  const pausedRef = useRef(false);

  useEffect(() => {
    const hex = (): string =>
      Math.floor(Math.random() * 256)
        .toString(16)
        .padStart(2, "0");
    const map: Record<string, string> = {};
    for (const id of NODE_IDS_3D) {
      map[id] = "node.0x" + hex() + hex();
    }
    setLabels(map);
  }, []);

  useEffect(() => {
    let rafId = 0;
    let last = performance.now();
    const loop = (now: number): void => {
      const dt = (now - last) / 1000;
      last = now;
      if (!pausedRef.current) {
        setAngle((a) => a + dt * 0.08);
        setPacketT((t) => {
          const next = t + dt * 0.45;
          return next >= 1 ? next - 1 : next;
        });
      }
      rafId = requestAnimationFrame(loop);
    };
    rafId = requestAnimationFrame(loop);
    return () => cancelAnimationFrame(rafId);
  }, []);

  useEffect(() => {
    const pickPath = (): void => {
      const i = Math.floor(Math.random() * ENDPOINT_POOL_3D.length);
      let j = Math.floor(Math.random() * ENDPOINT_POOL_3D.length);
      while (j === i) {
        j = Math.floor(Math.random() * ENDPOINT_POOL_3D.length);
      }
      const src = ENDPOINT_POOL_3D[i];
      const dst = ENDPOINT_POOL_3D[j];
      if (!src || !dst) return;
      setActivePath(shortestPath3D(src, dst));
      setPacketT(0);
    };
    pickPath();
    const id = window.setInterval(pickPath, 2800);
    return () => window.clearInterval(id);
  }, []);

  const projected = useMemo(() => {
    const map: Record<string, Projected> = {};
    for (const id of NODE_IDS_3D) {
      const n = NODES_3D[id];
      if (!n) continue;
      map[id] = project3D(n, angle);
    }
    return map;
  }, [angle]);

  const activeEdgeSet = useMemo(() => {
    const s = new Set<string>();
    for (let i = 0; i < activePath.length - 1; i++) {
      const a = activePath[i];
      const b = activePath[i + 1];
      if (!a || !b) continue;
      s.add(edgeKey3D(a, b));
    }
    return s;
  }, [activePath]);

  const packetPos = useMemo(() => {
    const path = activePath;
    if (path.length < 2) return { sx: CENTER_X, sy: CENTER_Y };
    const segCount = path.length - 1;
    const segIdx = Math.min(Math.floor(packetT * segCount), segCount - 1);
    const segT = packetT * segCount - segIdx;
    const a = projected[path[segIdx] ?? ""];
    const b = projected[path[segIdx + 1] ?? ""];
    if (!a || !b) return { sx: CENTER_X, sy: CENTER_Y };
    return {
      sx: a.sx + (b.sx - a.sx) * segT,
      sy: a.sy + (b.sy - a.sy) * segT,
    };
  }, [packetT, activePath, projected]);

  const orderedNodes = useMemo(() => {
    return [...NODE_IDS_3D].sort((a, b) => {
      const da = projected[a]?.depth ?? 0;
      const db = projected[b]?.depth ?? 0;
      return db - da;
    });
  }, [projected]);

  return (
    <div
      className="border border-line bg-bg-2 p-4"
      onMouseEnter={() => {
        pausedRef.current = true;
      }}
      onMouseLeave={() => {
        pausedRef.current = false;
      }}
    >
      <div className="flex justify-between items-center border-b border-line pb-2 mb-3.5 text-[10px] tracking-[0.12em] text-ink-dim uppercase">
        <span>
          <span className="text-accent">▸</span> mesh.proximity
        </span>
        <span>OBSERVABLE</span>
      </div>

      <svg
        className="mesh-svg w-full aspect-[320/220] block"
        viewBox="0 0 320 220"
        preserveAspectRatio="xMidYMid meet"
      >
        <defs>
          <pattern
            id="grid"
            width="20"
            height="20"
            patternUnits="userSpaceOnUse"
          >
            <path
              d="M 20 0 L 0 0 0 20"
              fill="none"
              stroke="#1a1f1a"
              strokeWidth="0.3"
            />
          </pattern>
        </defs>
        <rect width={VIEW_W} height={VIEW_H} fill="url(#grid)" />

        {EDGES_3D.map(([a, b]) => {
          const pa = projected[a];
          const pb = projected[b];
          if (!pa || !pb) return null;
          const key = edgeKey3D(a, b);
          const isActive = activeEdgeSet.has(key);
          if (isActive) {
            return (
              <line
                key={key}
                className="link link-active"
                x1={pa.sx}
                y1={pa.sy}
                x2={pb.sx}
                y2={pb.sy}
              />
            );
          }
          const avgDepth = (pa.depth + pb.depth) / 2;
          const fade = Math.max(0, Math.min(1, (1 - avgDepth) / 2));
          return (
            <line
              key={key}
              x1={pa.sx}
              y1={pa.sy}
              x2={pb.sx}
              y2={pb.sy}
              stroke="#c4ff3d"
              strokeOpacity={0.08 + fade * 0.18}
              strokeWidth={0.4 + fade * 0.4}
            />
          );
        })}

        {orderedNodes.map((id) => {
          const p = projected[id];
          if (!p) return null;
          const fade = Math.max(0.2, Math.min(1, (1 - p.depth) / 2));
          const r = 2.4 + fade * 3.2;
          const showLabel = NODES_3D[id]?.hasLabel === true;
          const labelText = labels[id] ?? "";
          return (
            <g key={id}>
              <circle
                cx={p.sx}
                cy={p.sy}
                r={r * 1.9}
                fill="none"
                stroke="#c4ff3d"
                strokeOpacity={0.08 + fade * 0.15}
                strokeWidth="0.4"
              />
              <circle
                cx={p.sx}
                cy={p.sy}
                r={r}
                fill="#c4ff3d"
                fillOpacity={0.4 + fade * 0.55}
              />
              {showLabel && labelText ? (
                <text
                  x={p.sx + r + 3}
                  y={p.sy + 2}
                  fontFamily="JetBrains Mono"
                  fontSize="5.5"
                  fill="#c4ff3d"
                  fillOpacity={0.35 + fade * 0.4}
                  letterSpacing="0.2"
                >
                  {labelText}
                </text>
              ) : null}
            </g>
          );
        })}

        <circle
          cx={packetPos.sx}
          cy={packetPos.sy}
          r="2.6"
          fill="#c4ff3d"
          opacity="0.95"
        />
      </svg>

      <div className="grid grid-cols-2 gap-3 mt-3.5 pt-3.5 border-t border-line">
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
    <div className="text-[10px]">
      <div className="text-ink-dim tracking-[0.1em] uppercase mb-1">
        {label}
      </div>
      <div className="text-accent text-[18px] font-semibold font-mono">
        {value}
        <span className="text-ink-dim text-[10px] ml-[3px]">{unit}</span>
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
    <div className="border border-line bg-bg-2 p-4">
      <div className="flex justify-between items-center border-b border-line pb-2 mb-3.5 text-[10px] tracking-[0.12em] text-ink-dim uppercase">
        <span>
          <span className="text-accent">▸</span> event.tail
        </span>
        <span className="flex items-center gap-1.5">
          <span className="w-1.5 h-1.5 rounded-full bg-accent inline-block animate-pulse-dot" />
          SIMULATED
        </span>
      </div>
      <div className="font-mono text-[10px] leading-[1.55] overflow-hidden min-h-[126px] -mx-4">
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
              className={`event-line-in flex items-baseline gap-2 whitespace-nowrap overflow-hidden px-4 ${bg}`}
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
              <span className="text-ink-faint shrink-0">▸</span>
              <span className="text-ink-dim flex-1 truncate">{e.body}</span>
              <span className={`${e.metricColor} shrink-0`}>{e.metric}</span>
            </div>
          );
        })}
      </div>
      <div className="border-t border-line mt-3 pt-2 flex justify-between text-[9px] text-ink-faint tracking-[0.1em] uppercase font-mono">
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
      className="hero relative overflow-hidden border-b border-line px-6 pt-[60px] pb-20"
    >
      <PacketRain />
      <div className="relative grid grid-cols-1 lg:grid-cols-[1fr_520px] gap-12">
        <div>
          <div className="text-[10px] text-ink-dim tracking-[0.15em] mb-7 flex flex-wrap gap-[18px] items-center">
            <span className="text-accent border border-accent-dim px-2 py-[3px]">
              RFC-NET-001
            </span>
            <span className="text-ink-faint font-mono">PROTOCOL.0x4E45·54</span>
            <span className="text-ink-dim">REV {rev}</span>
          </div>

          <h1
            className="font-display leading-[0.88] tracking-[-0.02em] text-ink mb-5"
            style={{ fontSize: "clamp(56px, 10vw, 128px)" }}
          >
            net.
            <br />
            <span className="text-accent">moves</span>
            <br />
            at light.
          </h1>

          <p className="text-[18px] text-ink mt-8 max-w-[580px] leading-[1.5] font-light">
            A latency-first encrypted mesh where every computer, app and device
            is a first-class node. Existing networks operate in milliseconds{" "}
            <em className="not-italic text-accent bg-accent/10 px-1">(10⁻³)</em>
            . NET operates in nanoseconds{" "}
            <em className="not-italic text-accent bg-accent/10 px-1">(10⁻⁹)</em>
            .
          </p>

          <p className="text-[13px] text-ink-dim mt-[18px] max-w-[580px] leading-[1.65]">
            No clients. No servers. No coordinators. The mesh propagates state,
            not connections.
          </p>

          <p className="text-[13px] text-ink-dim mt-3 max-w-[580px] leading-[1.65]">
            Flagship use:{" "}
            <Link
              href="/docs/worldview/agentic-mesh"
              className="text-accent no-underline hover:underline"
            >
              agentic capability federation
            </Link>{" "}
            — agents discovering, invoking, and recovering work across a trusted
            mesh.
          </p>

          <div className="mt-11 flex gap-3 flex-wrap items-center">
            <Link
              href={globals.links.install}
              className="btn-primary inline-flex items-center gap-2.5 px-5 py-3 text-[11px] tracking-[0.12em] uppercase font-semibold no-underline border border-accent bg-accent text-bg transition-all"
            >
              ↓ Install NET <span className="text-sm">→</span>
            </Link>
            <a
              href="#bench"
              className="btn-ghost inline-flex items-center gap-2.5 px-5 py-3 text-[11px] tracking-[0.12em] uppercase font-semibold no-underline border border-ink-faint text-ink transition-all"
            >
              Read benchmarks
            </a>
            <a
              href="#properties"
              className="btn-ghost inline-flex items-center gap-2.5 py-3 text-[11px] tracking-[0.12em] uppercase font-semibold no-underline text-ink transition-all"
            >
              // view spec ↘
            </a>
          </div>
        </div>

        <div className="hidden lg:flex flex-col gap-4 self-start">
          <MeshViz />
          <MeshEventLog />
        </div>
      </div>
    </section>
  );
}
