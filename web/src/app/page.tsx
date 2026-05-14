"use client";

import Link from "next/link";
import { Fragment, useEffect, useMemo, useRef, useState, JSX } from "react";
import globals from "@/lib/globals";
import { useRepoInfo } from "@/components/RepoInfoProvider";
import { cn } from "@/lib/cn";

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

const PACKET_RAIN_TOKENS: readonly string[] = [
  "node.0x2c",
  "node.0x7a",
  "node.0xff",
  "node.0x4e",
  "node.0x91",
  "→ relay.04",
  "→ relay.0a",
  "→ relay.1f",
  "→ gw.00",
  "pingwave",
  "mycelia",
  "engram",
  "mikoshi",
  "superpose",
  "0xc4ff3d",
  "0x4e4554",
  "0x7af301",
  "0xdeadbe",
  "0xfeedfa",
  "rt:hit",
  "rt:miss",
  "hb.ack",
  "hb.syn",
  "fwd.0",
  "fwd.1",
  "fwd.2",
  "cap.read",
  "cap.write",
  "cap.exec",
  "route()",
  "forward()",
  "38ns",
  "57ns",
  "113ns",
  "288ns",
  "0.93ns",
  "▸▸▸",
  "◀◀◀",
  "── ──",
  "·· ··",
  "01001110",
  "01000101",
  "01010100",
  "A→B",
  "B→G",
  "G→R1",
  "R2→B",
  "ACK",
  "SYN",
  "FIN",
  "NAK",
  "OK",
  "ERR",
];

interface RainColumn {
  x: number;
  y: number;
  speed: number;
  gap: number;
  len: number;
  tokens: string[];
  tick: number;
}

function pickToken(): string {
  const idx = Math.floor(Math.random() * PACKET_RAIN_TOKENS.length);
  return PACKET_RAIN_TOKENS[idx] ?? "";
}

function PacketRain() {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    const FONT_PX = 11;
    const COL_W = 90;
    let W = 0;
    let H = 0;
    let cols: RainColumn[] = [];
    let rafId = 0;

    const resize = (): void => {
      const rect = canvas.getBoundingClientRect();
      W = rect.width;
      H = rect.height;
      canvas.width = Math.max(1, Math.floor(W * dpr));
      canvas.height = Math.max(1, Math.floor(H * dpr));
      ctx.setTransform(1, 0, 0, 1, 0, 0);
      ctx.scale(dpr, dpr);
      ctx.font = `${FONT_PX}px "JetBrains Mono", ui-monospace, monospace`;
      ctx.textBaseline = "top";

      const colCount = Math.max(6, Math.ceil(W / COL_W));
      cols = Array.from({ length: colCount }, (_, i) => ({
        x: (i + 0.5) * (W / colCount) + (Math.random() - 0.5) * 20,
        y: -Math.random() * H,
        speed: 0.5 + Math.random() * 1.4,
        gap: 16 + Math.random() * 10,
        len: 8 + Math.floor(Math.random() * 14),
        tokens: Array.from({ length: 22 }, pickToken),
        tick: 0,
      }));
    };

    const frame = (): void => {
      ctx.fillStyle = "rgba(11, 13, 11, 0.18)";
      ctx.fillRect(0, 0, W, H);

      for (const c of cols) {
        c.y += c.speed;
        c.tick++;
        if (c.tick % 7 === 0) {
          c.tokens[Math.floor(Math.random() * c.tokens.length)] = pickToken();
        }

        for (let i = 0; i < c.len; i++) {
          const drawY = c.y - i * c.gap;
          if (drawY < -FONT_PX || drawY > H) continue;

          const headIdx = Math.floor(c.y / c.gap);
          const idx = headIdx - i;
          if (idx < 0) continue;
          const tok = c.tokens[idx % c.tokens.length] ?? "";

          if (i === 0) {
            ctx.fillStyle = "rgba(220, 255, 200, 0.95)";
          } else {
            const a = Math.max(0, 1 - i / c.len);
            ctx.fillStyle = `rgba(196, 255, 61, ${a * 0.55})`;
          }
          ctx.fillText(tok, c.x, drawY);
        }

        if (c.y - c.len * c.gap > H + 40) {
          c.y = -Math.random() * H * 0.5;
          c.speed = 0.5 + Math.random() * 1.4;
          c.gap = 16 + Math.random() * 10;
          c.len = 8 + Math.floor(Math.random() * 14);
          c.tokens = c.tokens.map(() => pickToken());
        }
      }
      rafId = requestAnimationFrame(frame);
    };

    const onResize = (): void => {
      ctx.setTransform(1, 0, 0, 1, 0, 0);
      resize();
      ctx.fillStyle = "#0b0d0b";
      ctx.fillRect(0, 0, W, H);
    };

    resize();
    ctx.fillStyle = "#0b0d0b";
    ctx.fillRect(0, 0, W, H);
    rafId = requestAnimationFrame(frame);
    window.addEventListener("resize", onResize);

    return () => {
      cancelAnimationFrame(rafId);
      window.removeEventListener("resize", onResize);
    };
  }, []);

  return (
    <canvas
      ref={canvasRef}
      className="packet-rain absolute inset-0 w-full h-full pointer-events-none"
      aria-hidden
    />
  );
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
        <MeshStat label="recovery cycle" value="288" unit="ns" />
        <MeshStat label="cdylib size" value="1.92" unit="mb" />
        <MeshStat label="throughput" value="26.7" unit="M ops/s" />
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

const NAV_LINKS: ReadonlyArray<{ href: string; label: string }> = [
  { href: "#what", label: "SPEC" },
  { href: "#bench", label: "BENCH" },
  { href: "#runtime", label: "RUNTIME" },
  { href: "#apps", label: "APPS" },
  { href: "#install", label: "SDKS" },
  { href: "#wall", label: "BLACKWALL" },
];

function NavBar() {
  return (
    <nav className="fixed top-7 left-0 right-0 h-[52px] nav-glass border-b border-line flex items-center px-6 z-[99]">
      <Link
        href="/"
        className="logo-mark font-display text-[22px] text-ink tracking-[0.1em] flex items-baseline gap-2.5"
      >
        net{" "}
        <span className="font-mono text-[9px] text-accent tracking-[0.15em] font-semibold">
          // AI 2070
        </span>
      </Link>
      <ul className="hidden lg:flex list-none gap-7 ml-auto items-center">
        {NAV_LINKS.map((l) => (
          <li key={l.href}>
            <a
              href={l.href}
              className="text-ink-dim text-[11px] tracking-[0.08em] uppercase hover:text-accent transition-colors"
            >
              {l.label}
            </a>
          </li>
        ))}
        <li>
          <a
            href="#install"
            className="install-btn bg-accent text-bg border border-accent px-3.5 py-1.5 text-[11px] tracking-[0.08em] uppercase font-semibold transition-colors"
          >
            ↓ INSTALL
          </a>
        </li>
      </ul>
      <a
        href="#install"
        className="lg:hidden ml-auto install-btn bg-accent text-bg border border-accent px-3.5 py-1.5 text-[11px] tracking-[0.08em] uppercase font-semibold transition-colors"
      >
        ↓ INSTALL
      </a>
    </nav>
  );
}

function SectionLabel({ children }: { children: React.ReactNode }) {
  return (
    <div className="sec-label text-[10px] tracking-[0.2em] text-accent uppercase mb-3 flex items-center">
      {children}
    </div>
  );
}

function DisplayHeading({ children }: { children: React.ReactNode }) {
  return (
    <h2
      className="font-display leading-none tracking-[-0.01em] text-ink mb-8 max-w-[900px]"
      style={{ fontSize: "clamp(36px, 5vw, 60px)" }}
    >
      {children}
    </h2>
  );
}

function buildQuarter(buildDate: string): string {
  const parts = buildDate.split(".");
  const year = parts[0];
  const monthStr = parts[1];
  if (!year || !monthStr) return buildDate;
  const month = Number.parseInt(monthStr, 10);
  if (Number.isNaN(month) || month < 1 || month > 12) return year;
  return `Q${Math.ceil(month / 3)} ${year}`;
}

function HeroSection() {
  const { version, buildDate } = useRepoInfo();
  const rev =
    buildDate === "—" ? version : `${version} / ${buildQuarter(buildDate)}`;

  return (
    <section className="hero relative overflow-hidden border-b border-line px-6 pt-[60px] pb-20">
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
            . Net operates in nanoseconds{" "}
            <em className="not-italic text-accent bg-accent/10 px-1">(10⁻⁹)</em>
            .
          </p>

          <p className="text-[13px] text-ink-dim mt-[18px] max-w-[580px] leading-[1.65]">
            No clients. No servers. No coordinators. The mesh propagates state,
            not connections.
          </p>

          <div className="mt-11 flex gap-3 flex-wrap items-center">
            <a
              href="#install"
              className="btn-primary inline-flex items-center gap-2.5 px-5 py-3 text-[11px] tracking-[0.12em] uppercase font-semibold no-underline border border-accent bg-accent text-bg transition-all"
            >
              ↓ Install Net <span className="text-sm">→</span>
            </a>
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

interface ArpanetNode {
  x: number;
  y: number;
  label?: string;
}

const ARPANET_NODES: Record<string, ArpanetNode> = {
  UCLA: { x: 165, y: 380, label: "UCLA" },
  SRI: { x: 110, y: 270, label: "SRI" },
  UCSB: { x: 140, y: 410, label: "UCSB" },
  RAND: { x: 220, y: 360, label: "RAND" },
  UTAH: { x: 320, y: 245, label: "UTAH" },
  ILL: { x: 640, y: 230, label: "UIUC" },
  CASE: { x: 790, y: 195, label: "CASE" },
  CMU: { x: 820, y: 245, label: "CMU" },
  MITRE: { x: 870, y: 275, label: "MITRE" },
  BBN: { x: 880, y: 160, label: "BBN" },
  MIT: { x: 920, y: 130, label: "MIT" },
  HVD: { x: 945, y: 105, label: "HVD" },
  LINC: { x: 920, y: 200, label: "LINC" },
  BURR: { x: 850, y: 105, label: "BURR" },
};

const ARPANET_EDGES: ReadonlyArray<readonly [string, string]> = [
  ["UCLA", "SRI"],
  ["UCLA", "UCSB"],
  ["UCLA", "RAND"],
  ["UCSB", "SRI"],
  ["SRI", "UTAH"],
  ["UTAH", "ILL"],
  ["UTAH", "CASE"],
  ["CASE", "CMU"],
  ["CASE", "MIT"],
  ["CMU", "HVD"],
  ["MIT", "BBN"],
  ["BBN", "HVD"],
  ["HVD", "LINC"],
  ["LINC", "BURR"],
  ["ILL", "MITRE"],
  ["MITRE", "BBN"],
  ["RAND", "BBN"],
];

function ArpanetMapBg() {
  return (
    <div
      className="crt-scanlines-dense absolute inset-0 pointer-events-none"
      aria-hidden
      style={{
        WebkitMaskImage:
          "radial-gradient(ellipse 90% 80% at 50% 50%, #000 35%, transparent 95%)",
        maskImage:
          "radial-gradient(ellipse 90% 80% at 50% 50%, #000 35%, transparent 95%)",
      }}
    >
      <span className="absolute top-4 left-4 w-3 h-3 border-t border-l border-accent/55" />
      <span className="absolute top-4 right-4 w-3 h-3 border-t border-r border-accent/55" />
      <span className="absolute bottom-4 left-4 w-3 h-3 border-b border-l border-accent/55" />
      <span className="absolute bottom-4 right-4 w-3 h-3 border-b border-r border-accent/55" />
      <svg
        className="w-full h-full opacity-[0.4]"
        viewBox="0 0 1000 589"
        preserveAspectRatio="xMidYMid meet"
      >
        <defs>
          <pattern
            id="arpanet-grid"
            width="40"
            height="40"
            patternUnits="userSpaceOnUse"
          >
            <path
              d="M 40 0 L 0 0 0 40"
              fill="none"
              stroke="#c4ff3d"
              strokeWidth="0.3"
              strokeOpacity="0.18"
            />
          </pattern>
        </defs>
        <rect
          x="40"
          y="60"
          width="920"
          height="490"
          fill="url(#arpanet-grid)"
        />

        {/* latitude tick marks (left edge) */}
        <g
          fontFamily="JetBrains Mono"
          fontSize="7"
          fill="#c4ff3d"
          fillOpacity="0.5"
        >
          {[
            { y: 120, lat: "50°N" },
            { y: 220, lat: "45°N" },
            { y: 320, lat: "40°N" },
            { y: 420, lat: "35°N" },
            { y: 510, lat: "30°N" },
          ].map((t) => (
            <g key={t.lat}>
              <line
                x1="40"
                y1={t.y}
                x2="50"
                y2={t.y}
                stroke="#c4ff3d"
                strokeOpacity="0.5"
                strokeWidth="0.6"
              />
              <text x="14" y={t.y + 2.5} letterSpacing="0.5">
                {t.lat}
              </text>
            </g>
          ))}
        </g>

        {/* longitude tick marks (bottom edge) */}
        <g
          fontFamily="JetBrains Mono"
          fontSize="7"
          fill="#c4ff3d"
          fillOpacity="0.5"
        >
          {[
            { x: 130, lon: "120°W" },
            { x: 320, lon: "110°W" },
            { x: 510, lon: "100°W" },
            { x: 700, lon: "90°W" },
            { x: 890, lon: "80°W" },
          ].map((t) => (
            <g key={t.lon}>
              <line
                x1={t.x}
                y1="550"
                x2={t.x}
                y2="540"
                stroke="#c4ff3d"
                strokeOpacity="0.5"
                strokeWidth="0.6"
              />
              <text x={t.x} y="565" letterSpacing="0.5" textAnchor="middle">
                {t.lon}
              </text>
            </g>
          ))}
        </g>

        {/* compass — top right */}
        <g transform="translate(905,100)">
          <circle
            cx="0"
            cy="0"
            r="16"
            fill="none"
            stroke="#c4ff3d"
            strokeOpacity="0.45"
            strokeWidth="0.6"
          />
          <line
            x1="0"
            y1="-16"
            x2="0"
            y2="-22"
            stroke="#c4ff3d"
            strokeOpacity="0.7"
            strokeWidth="0.8"
          />
          <text
            x="0"
            y="-26"
            fontFamily="JetBrains Mono"
            fontSize="8"
            fill="#c4ff3d"
            fillOpacity="0.75"
            textAnchor="middle"
            fontWeight="600"
          >
            N
          </text>
          <line
            x1="0"
            y1="-12"
            x2="0"
            y2="12"
            stroke="#c4ff3d"
            strokeOpacity="0.55"
            strokeWidth="0.5"
          />
          <line
            x1="-12"
            y1="0"
            x2="12"
            y2="0"
            stroke="#c4ff3d"
            strokeOpacity="0.55"
            strokeWidth="0.5"
          />
        </g>

        {/* scale bar — bottom right */}
        <g transform="translate(820,500)">
          <line
            x1="0"
            y1="0"
            x2="120"
            y2="0"
            stroke="#c4ff3d"
            strokeOpacity="0.55"
            strokeWidth="0.7"
          />
          <line
            x1="0"
            y1="-3"
            x2="0"
            y2="3"
            stroke="#c4ff3d"
            strokeOpacity="0.55"
            strokeWidth="0.7"
          />
          <line
            x1="60"
            y1="-2"
            x2="60"
            y2="2"
            stroke="#c4ff3d"
            strokeOpacity="0.55"
            strokeWidth="0.6"
          />
          <line
            x1="120"
            y1="-3"
            x2="120"
            y2="3"
            stroke="#c4ff3d"
            strokeOpacity="0.55"
            strokeWidth="0.7"
          />
          <text
            x="0"
            y="14"
            fontFamily="JetBrains Mono"
            fontSize="7"
            fill="#c4ff3d"
            fillOpacity="0.55"
          >
            0
          </text>
          <text
            x="60"
            y="14"
            fontFamily="JetBrains Mono"
            fontSize="7"
            fill="#c4ff3d"
            fillOpacity="0.55"
            textAnchor="middle"
          >
            500 MI
          </text>
          <text
            x="120"
            y="14"
            fontFamily="JetBrains Mono"
            fontSize="7"
            fill="#c4ff3d"
            fillOpacity="0.55"
            textAnchor="end"
          >
            1000
          </text>
        </g>

        {/* topology stats — top left */}
        <g
          fontFamily="JetBrains Mono"
          fontSize="8"
          fill="#c4ff3d"
          fillOpacity="0.6"
          letterSpacing="1.2"
        >
          <text x="60" y="86">
            RFC-1 · IMP TOPOLOGY
          </text>
          <text x="60" y="100" fillOpacity="0.4">
            NODES: 14 · LINKS: 17
          </text>
          <text x="60" y="114" fillOpacity="0.4">
            PROTO: NCP · 50 KBPS LINES
          </text>
        </g>

        {/*{ARPANET_EDGES.map(([a, b]) => {
          const na = ARPANET_NODES[a];
          const nb = ARPANET_NODES[b];
          if (!na || !nb) return null;
          return (
            <line
              key={`${a}-${b}`}
              x1={na.x}
              y1={na.y}
              x2={nb.x}
              y2={nb.y}
              stroke="#c4ff3d"
              strokeWidth="0.7"
              strokeOpacity="0.5"
            />
          );
        })}
        {ARPANET_EDGES.map(([a, b], i) => {
          const na = ARPANET_NODES[a];
          const nb = ARPANET_NODES[b];
          if (!na || !nb) return null;
          const dist = Math.hypot(nb.x - na.x, nb.y - na.y);
          const dur = Math.max(1.4, dist / 90).toFixed(2) + "s";
          const begin = (i * 0.31).toFixed(2) + "s";
          const reverse = i % 2 === 0;
          const path = reverse
            ? `M ${nb.x} ${nb.y} L ${na.x} ${na.y}`
            : `M ${na.x} ${na.y} L ${nb.x} ${nb.y}`;
          return (
            <circle
              key={`pkt-${a}-${b}`}
              r="1.6"
              fill="#c4ff3d"
              opacity="0.9"
              style={{ filter: "drop-shadow(0 0 3px #c4ff3d)" }}
            >
              <animateMotion
                dur={dur}
                begin={begin}
                repeatCount="indefinite"
                path={path}
                rotate="auto"
              />
            </circle>
          );
        })}
        {Object.entries(ARPANET_NODES).map(([id, n]) => (
          <g key={id}>
            <circle
              cx={n.x}
              cy={n.y}
              r="6"
              fill="none"
              stroke="#c4ff3d"
              strokeOpacity="0.35"
            />
            <circle cx={n.x} cy={n.y} r="2.5" fill="#c4ff3d" />
            {n.label ? (
              <text
                x={n.x + 9}
                y={n.y + 3}
                fontFamily="JetBrains Mono"
                fontSize="9"
                fill="#c4ff3d"
                fillOpacity="0.7"
                letterSpacing="0.5"
              >
                {n.label}
              </text>
            ) : null}
          </g>
        ))}*/}
        <text
          x="80"
          y="540"
          fontFamily="JetBrains Mono"
          fontSize="10"
          fill="#c4ff3d"
          fillOpacity="0.55"
          letterSpacing="2"
        >
          ARPANET · IMP BACKBONE · DEC 1971
        </text>
      </svg>
    </div>
  );
}

function WhyNotBestEffortSection() {
  return (
    <section
      id="what"
      className="relative overflow-hidden border-b border-line px-6 py-20"
    >
      <ArpanetMapBg />
      <div className="relative">
        <SectionLabel>§01 / why not best-effort</SectionLabel>
        <DisplayHeading>
          arpanet assumed scarcity.
          <br />
          <span className="text-accent">net assumes abundance.</span>
        </DisplayHeading>

        <p className="text-[16px] text-ink max-w-[740px] leading-[1.6] font-light mb-12">
          TCP was designed when nuclear war was a real possibility. Packets were
          precious. The network had to guarantee delivery because the next
          packet might not get through.
        </p>

        <div className="grid grid-cols-1 md:grid-cols-2 gap-8 mt-6">
          <div>
            <p className="text-ink-dim text-[13px] leading-[1.7] mb-4">
              <strong className="text-ink font-medium">
                That was the right design for 1969.
              </strong>{" "}
              It&apos;s the wrong design now. Sensors don&apos;t pause. Token
              streams don&apos;t wait. Market feeds don&apos;t care that your
              queue is full. The firehose doesn&apos;t have a pause button.
            </p>
            <p className="text-ink-dim text-[13px] leading-[1.7] mb-4">
              In a world of abundance, guaranteeing delivery is a threat —
              you&apos;re promising to deliver data that will bury the receiver.
              The bottleneck isn&apos;t delivery. It&apos;s processing. Arrival
              doesn&apos;t equal usefulness.
            </p>
          </div>
          <div>
            <p className="text-ink-dim text-[13px] leading-[1.7] mb-4">
              <strong className="text-ink font-medium">
                Net inverts the default.
              </strong>{" "}
              TCP starts with trust and detects abuse. Net starts with zero
              assumptions and lets trust emerge from consistent behavior.
            </p>
            <p className="text-ink-dim text-[13px] leading-[1.7] mb-4">
              Nodes reject work they can&apos;t process within a time window.
              Dropping a packet and re-requesting from a faster node costs
              nanoseconds. Waiting for a congested node&apos;s guaranteed
              response costs milliseconds.{" "}
              <strong className="text-ink font-medium">
                When dropping is cheaper than waiting, delivery guarantees
                become overhead.
              </strong>
            </p>
          </div>
        </div>

        <div className="border-l-2 border-accent pl-8 pr-8 py-6 bg-accent/[0.02] my-12 max-w-[900px]">
          <p className="text-[18px] text-ink leading-[1.5] font-light">
            The remaining latency is physics: NIC, wire, speed of light.{" "}
            <span className="text-accent">
              The software got out of the way.
            </span>
          </p>
        </div>
      </div>
    </section>
  );
}

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
    floor: "microseconds*",
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

function TopologyClassesSection() {
  return (
    <section id="topology" className="border-b border-line px-6 py-20">
      <SectionLabel>§02 / topology classes</SectionLabel>
      <DisplayHeading>a new class of system.</DisplayHeading>

      <p className="text-[16px] text-ink max-w-[740px] leading-[1.6] font-light mb-12">
        Existing networking falls into two categories. Net is neither.
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

interface SpectrumTick {
  exp: number;
  x: number;
  unit: string;
}

const SPECTRUM_TICKS: ReadonlyArray<SpectrumTick> = [
  { exp: -9, x: 80, unit: "1 ns" },
  { exp: -8, x: 253, unit: "10 ns" },
  { exp: -7, x: 427, unit: "100 ns" },
  { exp: -6, x: 600, unit: "1 μs" },
  { exp: -5, x: 773, unit: "10 μs" },
  { exp: -4, x: 947, unit: "100 μs" },
  { exp: -3, x: 1120, unit: "1 ms" },
];

interface SpectrumMarker {
  x: number;
  label: string;
  sub: string;
  color: string;
  faint: string;
  glow?: boolean;
  slower?: string;
}

const SPECTRUM_MARKERS: ReadonlyArray<SpectrumMarker> = [
  {
    x: 80,
    label: "NET*",
    sub: "mesh transport",
    color: "#c4ff3d",
    faint: "#6b8a1e",
    glow: true,
  },
  {
    x: 687,
    label: "REAL-TIME†",
    sub: "CAN · EtherCAT · TSN",
    color: "#d4dcd0",
    faint: "#6b7568",
    slower: "~1,000× slower",
  },
  {
    x: 1120,
    label: "BEST-EFFORT",
    sub: "cloud · TCP · gRPC",
    color: "#6b7568",
    faint: "#4a5249",
    slower: "~1,000,000× slower",
  },
];

function LatencySpectrum() {
  return (
    <div className="mt-10 border border-line bg-bg-2 px-6 py-7 hidden md:block">
      <div className="text-[10px] tracking-[0.18em] uppercase text-ink-dim mb-4 flex items-center justify-between flex-wrap gap-2">
        <span>// latency spectrum · log scale</span>
        <span className="text-ink-faint">10⁻⁹ → 10⁻³</span>
      </div>

      <div className="overflow-x-auto">
        <svg
          viewBox="0 0 1200 108"
          className="w-full"
          style={{ minWidth: 720 }}
          preserveAspectRatio="xMidYMid meet"
        >
          {SPECTRUM_MARKERS.map((m) => (
            <g key={m.label}>
              <text
                x={m.x}
                y="14"
                fontFamily="JetBrains Mono"
                fontSize="11"
                fill={m.color}
                textAnchor="middle"
                letterSpacing="1.4"
                fontWeight="600"
              >
                {m.label}
              </text>
              {m.slower ? (
                <>
                  <rect
                    x={m.x - (m.slower.length * 5.5) / 2 - 6}
                    y={18}
                    width={m.slower.length * 5.5 + 12}
                    height={13}
                    fill="#0a0c0a"
                    rx="2"
                  />
                  <text
                    x={m.x}
                    y="28"
                    fontFamily="JetBrains Mono"
                    fontSize="9"
                    fill={m.color}
                    textAnchor="middle"
                    letterSpacing="0.4"
                    fontStyle="italic"
                  >
                    {m.slower}
                  </text>
                </>
              ) : null}
              <text
                x={m.x}
                y="38"
                fontFamily="JetBrains Mono"
                fontSize="8"
                fill={m.faint}
                textAnchor="middle"
                letterSpacing="0.4"
              >
                {m.sub}
              </text>
              <line
                x1={m.x}
                y1="44"
                x2={m.x}
                y2="62"
                stroke={m.color}
                strokeWidth="0.7"
                strokeOpacity="0.55"
                strokeDasharray="2 2"
              />
              <polygon
                points={`${m.x - 4},58 ${m.x + 4},58 ${m.x},64`}
                fill={m.color}
                opacity="0.85"
              />
            </g>
          ))}

          <line
            x1="80"
            y1="70"
            x2="1120"
            y2="70"
            stroke="#2d352c"
            strokeWidth="1"
          />

          {SPECTRUM_TICKS.map((t) => (
            <g key={t.exp}>
              <line
                x1={t.x}
                y1="66"
                x2={t.x}
                y2="74"
                stroke="#6b7568"
                strokeWidth="0.7"
              />
              <text
                x={t.x}
                y="86"
                fontFamily="JetBrains Mono"
                fontSize="10"
                fill="#6b7568"
                textAnchor="middle"
              >
                10
                <tspan fontSize="7" baselineShift="super">
                  {t.exp}
                </tspan>
              </text>
              <text
                x={t.x}
                y="100"
                fontFamily="JetBrains Mono"
                fontSize="8"
                fill="#4a5249"
                textAnchor="middle"
                letterSpacing="0.4"
              >
                {t.unit}
              </text>
            </g>
          ))}

          {SPECTRUM_MARKERS.map((m) => (
            <g key={m.label + "-dot"}>
              {m.glow ? (
                <circle
                  cx={m.x}
                  cy="70"
                  r="9"
                  fill="none"
                  stroke={m.color}
                  strokeOpacity="0.4"
                />
              ) : null}
              <circle
                cx={m.x}
                cy="70"
                r={m.glow ? 5 : 4}
                fill={m.color}
                style={
                  m.glow
                    ? { filter: `drop-shadow(0 0 6px ${m.color})` }
                    : undefined
                }
              />
            </g>
          ))}
        </svg>
      </div>

      <p className="text-[10px] text-ink-faint mt-4 leading-[1.6] tracking-[0.04em] font-mono">
        * forward 0.20 ns · cap check 1.78 ns · pingwave 0.65 ns · header
        serialize 1.31 ns (i9-14900K)
        <br />† real-time guarantees only on dedicated hardware. Net hits the
        nanosecond range on commodity wire.
      </p>
    </div>
  );
}

interface AxiomCard {
  id: string;
  title: string;
  body: string;
  ascii: React.ReactNode;
}

const SUBSCRIPT_DIGITS = "₀₁₂₃₄₅₆₇₈₉";

function subscript(n: number): string {
  return String(n)
    .split("")
    .map((d) => SUBSCRIPT_DIGITS[Number.parseInt(d, 10)] ?? d)
    .join("");
}

function MarchingArrows() {
  const [pos, setPos] = useState(0);
  useEffect(() => {
    const id = window.setInterval(() => {
      setPos((p) => (p + 1) % 14);
    }, 180);
    return () => window.clearInterval(id);
  }, []);
  return (
    <>
      {Array.from({ length: 14 }, (_, i) => {
        const trail = (pos - i + 14) % 14;
        if (trail < 3) {
          const op = trail === 0 ? 1 : trail === 1 ? 0.7 : 0.35;
          return (
            <span key={i} className="text-accent" style={{ opacity: op }}>
              ▶
            </span>
          );
        }
        return <span key={i}>▶</span>;
      })}
      {"\n"}
      ░░░░░░░░░░░░░░
    </>
  );
}

function SequenceAdvance() {
  const [seq, setSeq] = useState(1);
  useEffect(() => {
    const id = window.setInterval(() => {
      setSeq((s) => (s % 99) + 1);
    }, 1200);
    return () => window.clearInterval(id);
  }, []);
  return (
    <>
      e{subscript(seq)} → e{subscript(seq + 1)} → e{subscript(seq + 2)}
      {"\n"}
      chain.verify()
    </>
  );
}

interface LatencySample {
  value: string;
  label: string;
}

const LATENCY_SAMPLES: ReadonlyArray<LatencySample> = [
  { value: "0.20 ns", label: "fwd" },
  { value: "1.31 ns", label: "serialize" },
  { value: "0.93 ns", label: "pingwave" },
  { value: "0.31 ns", label: "gpu check" },
];

function LatencyPulse() {
  const [idx, setIdx] = useState(0);
  useEffect(() => {
    const id = window.setInterval(() => {
      setIdx((i) => (i + 1) % LATENCY_SAMPLES.length);
    }, 1500);
    return () => window.clearInterval(id);
  }, []);
  const sample = LATENCY_SAMPLES[idx];
  if (!sample) return null;
  return (
    <>
      <span className="text-accent">{sample.value}</span>
      {"  ▸  "}
      {sample.label}
      {"\nsub-ns floor"}
    </>
  );
}

const TRUST_VALUES: ReadonlyArray<string> = [
  "observation",
  "evidence",
  "behavior",
  "proof",
];

function TrustCycle() {
  const [idx, setIdx] = useState(0);
  useEffect(() => {
    const id = window.setInterval(() => {
      setIdx((i) => (i + 1) % TRUST_VALUES.length);
    }, 1600);
    return () => window.clearInterval(id);
  }, []);
  return (
    <>
      {"trust := "}
      <span className="text-accent">{TRUST_VALUES[idx]}</span>
      {"\nnot assumption"}
    </>
  );
}

const PAYLOAD_CHARS: ReadonlyArray<string> = ["░", "▒", "▓", "█", "▓", "▒"];

function SchemaPayload() {
  const [shift, setShift] = useState(0);
  useEffect(() => {
    const id = window.setInterval(() => {
      setShift((s) => (s + 1) % PAYLOAD_CHARS.length);
    }, 190);
    return () => window.clearInterval(id);
  }, []);
  const display = Array.from(
    { length: 5 },
    (_, i) => PAYLOAD_CHARS[(i + shift) % PAYLOAD_CHARS.length] ?? "░",
  ).join("");
  return (
    <>
      {"[hdr][hash]["}
      <span className="text-accent">{display}</span>
      {"]\nopaque payload"}
    </>
  );
}

const TYPE_VALUES: ReadonlyArray<string> = [
  "peer-pair",
  "{Token, Result}",
  "{Cmd, Ack}",
  "runtime",
];

function TypedCycle() {
  const [idx, setIdx] = useState(0);
  useEffect(() => {
    const id = window.setInterval(() => {
      setIdx((i) => (i + 1) % TYPE_VALUES.length);
    }, 1700);
    return () => window.clearInterval(id);
  }, []);
  return (
    <>
      {"type ∈ "}
      <span className="text-accent">{TYPE_VALUES[idx]}</span>
      {"\nnot network"}
    </>
  );
}

function BackpressureFlow() {
  const [active, setActive] = useState(0);
  useEffect(() => {
    const id = window.setInterval(() => {
      setActive((a) => (a + 1) % 3);
    }, 1300);
    return () => window.clearInterval(id);
  }, []);
  return (
    <>
      <span className={active === 0 ? "text-accent" : undefined}>silent</span>
      {" → "}
      <span className={active === 1 ? "text-accent" : undefined}>suspect</span>
      {"\nsuspect → "}
      <span className={active === 2 ? "text-accent" : undefined}>reroute</span>
    </>
  );
}

const AXIOMS: readonly AxiomCard[] = [
  {
    id: "P.01",
    title: "Latency-first",
    body: "Sub-nanosecond header serialization. Nanosecond heartbeats, hops, recovery. Packet scheduling at timescales reserved for local function calls.",
    ascii: <LatencyPulse />,
  },
  {
    id: "P.02",
    title: "Streaming-first",
    body: "Data is continuous flow, not documents. Sharded ring buffers, adaptive batching. No requests and responses — everything is a stream.",
    ascii: <MarchingArrows />,
  },
  {
    id: "P.03",
    title: "Zero-copy",
    body: "Ring buffers, no garbage collector, native Rust. No unsafe. Forwarding doesn't allocate or copy payload data. Design principle, not optimization.",
    ascii: "[mem]──refs──▶[wire]\n   no alloc",
  },
  {
    id: "P.04",
    title: "Encrypted E2E",
    body: "Noise protocol handshakes. ChaCha20-Poly1305 AEAD with counter nonces. Every packet encrypted source→dest. Intermediate nodes never see plaintext.",
    ascii: "A ─ChaCha20──▶ B\n    relay sees ░░░",
  },
  {
    id: "P.05",
    title: "Untrusted relay",
    body: "Nodes forward packets without decrypting payloads. The mesh routes through infrastructure you don't trust. Networks grow through adversarial nodes.",
    ascii: <TrustCycle />,
  },
  {
    id: "P.06",
    title: "Schema-agnostic",
    body: "Transport moves bytes, not structures. Raw event = payload + hash. Protocol never inspects content. Structure emerges where participants agree.",
    ascii: <SchemaPayload />,
  },
  {
    id: "P.07",
    title: "Optionally ordered",
    body: "Ordering is per-stream, not global. Unordered path is the fast path. Causal ordering available where streams need it. Cost paid only by streams that require it.",
    ascii: <SequenceAdvance />,
  },
  {
    id: "P.08",
    title: "Optionally typed",
    body: "The protocol doesn't care what's in the payload. Behavior plane can. Typing is a local agreement between nodes, not a network requirement.",
    ascii: <TypedCycle />,
  },
  {
    id: "P.09",
    title: "Native backpressure",
    body: "Nodes drop without reply. Not a failure mode — the design. The proximity graph makes silence a signal. Automatic rerouting.",
    ascii: <BackpressureFlow />,
  },
];

function PropertiesSection() {
  return (
    <section id="properties" className="border-b border-line px-6 py-20">
      <SectionLabel>§03 / protocol properties</SectionLabel>
      <DisplayHeading>
        nine axioms.
        <br />
        one runtime.
      </DisplayHeading>

      <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-px bg-line border border-line">
        {AXIOMS.map((p) => (
          <div key={p.id} className="bg-bg p-7 transition-colors hover:bg-bg-2">
            <div className="text-[10px] text-accent tracking-[0.15em] mb-4">
              {p.id}
            </div>
            <h3 className="font-mono text-[14px] font-semibold tracking-[0.05em] text-ink mb-3 uppercase">
              {p.title}
            </h3>
            <p className="text-ink-dim text-[12px] leading-[1.65]">{p.body}</p>
            <pre className="text-accent-dim text-[10px] mt-4 leading-[1.2] whitespace-pre opacity-70">
              {p.ascii}
            </pre>
          </div>
        ))}
      </div>
    </section>
  );
}

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

function BenchmarksSection() {
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

function MikoshiSection() {
  return (
    <section id="mikoshi" className="border-b border-line px-6 py-20">
      <SectionLabel>§05 / mikoshi // engram transit</SectionLabel>
      <DisplayHeading>
        state moves.
        <br />
        connections don&apos;t.
      </DisplayHeading>

      <div className="grid grid-cols-1 md:grid-cols-2 gap-8 mt-6">
        <div>
          <p className="text-ink-dim text-[13px] leading-[1.7] mb-4">
            <strong className="text-ink font-medium">
              In Cyberpunk, Mikoshi is Arasaka&apos;s construct for storing
              engrams
            </strong>{" "}
            — consciousness held in digital space, minds persisting outside
            their original hardware.
          </p>
          <p className="text-ink-dim text-[13px] leading-[1.7] mb-4">
            Mikoshi in Net is how daemons move between machines. A running
            program on one node becomes a running program on another without
            losing its history, its pending work, or its place in the
            conversation. The source packages its state, the target unpacks it,
            and for a brief moment the entity exists on both nodes at once —
            spreading, superposed, then collapsed onto the target as routing
            cuts over.
          </p>
        </div>
        <div>
          <p className="text-ink-dim text-[13px] leading-[1.7] mb-4">
            <strong className="text-ink font-medium">
              The daemon doesn&apos;t know it moved.
            </strong>{" "}
            Neither does anything talking to it. Observer nodes watching the
            stream see the same causal chain continue uninterrupted, the same
            sequence numbers, the same entity speaking. The hardware underneath
            shifted. The stream didn&apos;t notice.
          </p>
          <p className="text-ink-dim text-[13px] leading-[1.7] mb-4">
            A factory controller hops from a dying edge box to a healthy one
            mid-shift. An inference daemon follows its user from laptop to
            desktop. A trading agent migrates to a node closer to the exchange{" "}
            <strong className="text-ink font-medium">
              without dropping a single tick
            </strong>
            .
          </p>
        </div>
      </div>

      <div className="border-l-2 border-accent pl-8 pr-8 py-6 bg-accent/[0.02] my-12 max-w-[900px]">
        <p className="text-[18px] text-ink leading-[1.5] font-light">
          It doesn&apos;t move a copy.{" "}
          <strong className="text-accent font-medium">
            Mikoshi carries the thing itself across.
          </strong>
        </p>
      </div>
    </section>
  );
}

const MIGRATION_PHASES: ReadonlyArray<{
  num: string;
  name: string;
  body: string;
}> = [
  {
    num: "01",
    name: "snapshot",
    body: "source serializes daemon state into transferable bytes.",
  },
  {
    num: "02",
    name: "transfer",
    body: "snapshot moves source → target over subprotocol 0x0500.",
  },
  {
    num: "03",
    name: "restore",
    body: "target reconstructs daemon, identity preserved.",
  },
  {
    num: "04",
    name: "replay",
    body: "target catches up on events between snapshot and now.",
  },
  {
    num: "05",
    name: "cutover",
    body: "routing table updates atomically. next packet → target.",
  },
  {
    num: "06",
    name: "complete",
    body: "source releases daemon. target is sole authority.",
  },
];

interface GroupCard {
  id: string;
  name: string;
  meta: string;
  ascii: React.ReactNode;
  body: React.ReactNode;
  spec: ReadonlyArray<readonly [string, string]>;
}

interface AsciiPhase {
  rows: ReadonlyArray<React.ReactNode>;
  caption: React.ReactNode;
}

const STANDBY_PHASES: ReadonlyArray<AsciiPhase> = [
  {
    rows: [
      <>
        active{"   "}
        <span className="text-accent">●</span> processing seq=102
      </>,
      <>
        standby{"  "}
        <span className="text-ink-faint">○</span> synced_through=98
      </>,
      <>
        standby{"  "}
        <span className="text-ink-faint">○</span> synced_through=101
      </>,
    ],
    caption: <>all healthy · 3 nodes online</>,
  },
  {
    rows: [
      <>
        active{"   "}
        <span className="text-warn">×</span> failed @ seq=102
      </>,
      <>
        standby{"  "}
        <span className="text-accent">↑</span> promoting (sync=101)
      </>,
      <>
        standby{"  "}
        <span className="text-ink-faint">○</span> synced_through=101
      </>,
    ],
    caption: <>replaying buffered events ▸ seq=103</>,
  },
  {
    rows: [
      <>
        active{"   "}
        <span className="text-accent">●</span> processing seq=104{"  "}
        <span className="text-ink-faint">(was member 1)</span>
      </>,
      <>
        standby{"  "}
        <span className="text-ink-faint">○</span> synced_through=104
      </>,
      <>
        standby{"  "}
        <span className="text-ink-faint">○</span> synced_through=104
      </>,
    ],
    caption: <>continuity preserved · 0 events lost</>,
  },
];

const REPLICA_PHASES: ReadonlyArray<AsciiPhase> = [
  {
    rows: [
      <>
        member 0{"  "}
        <span className="text-accent">●</span> event #58 → result
      </>,
      <>
        member 1{"  "}
        <span className="text-ink-faint">○</span> idle
      </>,
      <>
        member 2{"  "}
        <span className="text-ink-faint">○</span> idle
      </>,
    ],
    caption: <>round-robin · seq=58</>,
  },
  {
    rows: [
      <>
        member 0{"  "}
        <span className="text-ink-faint">○</span> idle
      </>,
      <>
        member 1{"  "}
        <span className="text-accent">●</span> event #59 → result
      </>,
      <>
        member 2{"  "}
        <span className="text-ink-faint">○</span> idle
      </>,
    ],
    caption: <>round-robin · seq=59</>,
  },
  {
    rows: [
      <>
        member 0{"  "}
        <span className="text-ink-faint">○</span> idle
      </>,
      <>
        member 1{"  "}
        <span className="text-ink-faint">○</span> idle
      </>,
      <>
        member 2{"  "}
        <span className="text-accent">●</span> event #60 → result
      </>,
    ],
    caption: <>round-robin · seq=60</>,
  },
];

const FORK_PHASES: ReadonlyArray<AsciiPhase> = [
  {
    rows: [
      <>parent @ seq=42</>,
      <>
        {"   "}
        <span className="text-ink-faint">·</span> single chain, no divergence
      </>,
      <>
        {"   "}
        <span className="text-ink-faint">·</span> awaiting fork directive
      </>,
    ],
    caption: <>pre-fork · monitoring</>,
  },
  {
    rows: [
      <>
        {"   ├─▶ "}fork.A <span className="text-accent">(sentinel)</span>
      </>,
      <>
        {"   ├─▶ "}fork.B <span className="text-accent">(sentinel)</span>
      </>,
      <>
        {"   └─▶ "}fork.C <span className="text-accent">(sentinel)</span>
      </>,
    ],
    caption: <>forking · sentinels written from seq=42</>,
  },
  {
    rows: [
      <>
        fork.A @ seq=58 <span className="text-ink-faint">─ diverged</span>
      </>,
      <>
        fork.B @ seq=53 <span className="text-ink-faint">─ diverged</span>
      </>,
      <>
        fork.C @ seq=61 <span className="text-ink-faint">─ diverged</span>
      </>,
    ],
    caption: <>verifiable lineage → parent @ 42</>,
  },
];

function AsciiCycle({
  phases,
  intervalMs = 3500,
}: {
  phases: ReadonlyArray<AsciiPhase>;
  intervalMs?: number;
}) {
  const [phase, setPhase] = useState(0);

  useEffect(() => {
    const id = window.setInterval(() => {
      setPhase((p) => (p + 1) % phases.length);
    }, intervalMs);
    return () => window.clearInterval(id);
  }, [phases.length, intervalMs]);

  const current = phases[phase];
  if (!current) return null;

  return (
    <>
      {current.rows.map((row, i) => (
        <Fragment key={i}>
          {row}
          {"\n"}
        </Fragment>
      ))}
      {"\n"}
      {current.caption}
    </>
  );
}

const GROUP_CARDS: readonly GroupCard[] = [
  {
    id: "▸ GRP.01",
    name: "replica",
    meta: "N interchangeable copies · load-balanced",
    ascii: <AsciiCycle phases={REPLICA_PHASES} intervalMs={2200} />,
    body: (
      <>
        For horizontal scale on stateless workloads.{" "}
        <b className="text-ink font-medium">
          Each replica has its own causal chain
        </b>{" "}
        derived from a deterministic seed — fail one, spawn another with the
        same identity. No state to transfer.
      </>
    ),
    spec: [
      ["identity", "deterministic from seed"],
      ["routing", "round-robin"],
      ["state", "stateless"],
      ["recovery", "respawn"],
    ],
  },
  {
    id: "▸ GRP.02",
    name: "fork",
    meta: "independent siblings · documented lineage",
    ascii: <AsciiCycle phases={FORK_PHASES} intervalMs={3500} />,
    body: (
      <>
        For experiments, A/B testing, scenario branching.{" "}
        <b className="text-ink font-medium">
          Each fork carries a cryptographic sentinel
        </b>{" "}
        linking back to the parent at the fork point. Forks share a past but not
        a future.
      </>
    ),
    spec: [
      ["identity", "divergent from sentinel"],
      ["routing", "per-fork"],
      ["state", "independent"],
      ["recovery", "resnapshot from origin"],
    ],
  },
  {
    id: "▸ GRP.03",
    name: "standby",
    meta: "1 active · N-1 warm · zero duplicate compute",
    ascii: <AsciiCycle phases={STANDBY_PHASES} intervalMs={3500} />,
    body: (
      <>
        For stateful daemons that need fault tolerance without paying for
        duplicate compute.{" "}
        <b className="text-ink font-medium">Only the active processes events</b>{" "}
        — standbys are warm, not hot. Periodic snapshots track{" "}
        <code className="font-mono">synced_through</code> for each standby. On
        active failure, the standby with the highest sync point promotes and
        replays the gap using{" "}
        <b className="text-ink font-medium">
          the same replay machinery migration uses
        </b>
        .
      </>
    ),
    spec: [
      ["identity", "deterministic from seed"],
      ["routing", "active only"],
      ["state", "stateful, synced"],
      ["recovery", "promote + replay gap"],
    ],
  },
];

function ComputeRuntimeSection() {
  return (
    <section
      id="runtime"
      className="compute-bg border-b border-line px-6 py-20"
    >
      <SectionLabel>§06 / daemon runtime // new</SectionLabel>
      <DisplayHeading>
        compute
        <br />
        <span className="text-accent">
          lives on
          <br />
          the wire.
        </span>
      </DisplayHeading>

      <div className="border border-accent-dim bg-accent/[0.03] px-5 py-4 mb-10 flex items-center gap-[18px] text-[11px] text-ink-dim tracking-[0.05em] flex-wrap">
        <span className="bg-accent text-bg px-2.5 py-1 font-bold tracking-[0.18em] text-[10px]">
          NEW
        </span>
        <span>
          <b className="text-ink font-medium">
            Stateful programs that live on the mesh, not on a machine.
          </b>{" "}
          They have cryptographic identity, a verifiable history, and they move
          between nodes mid-execution without anyone noticing.
        </span>
        <span className="ml-auto">
          subprotocol{" "}
          <code className="text-accent bg-accent/[0.06] px-1.5 py-0.5 font-mono">
            0x0500
          </code>
        </span>
      </div>

      <p className="text-[16px] text-ink max-w-[740px] leading-[1.6] font-light mb-12">
        A program on Net is called a{" "}
        <em className="not-italic text-accent bg-accent/[0.08] px-1">daemon</em>
        . Its identity is a public key — an{" "}
        <code className="text-accent bg-accent/[0.06] px-1.5 py-0.5 font-mono">
          origin_hash
        </code>{" "}
        derived from ed25519, which doesn&apos;t change when the daemon moves.
        Its history is a causal chain — every event it produces is signed and
        links to the previous one, verifiable by any node. Its location is
        wherever in the mesh has the capabilities it asked for.{" "}
        <strong className="text-ink font-medium">
          When that location goes away, the daemon doesn&apos;t.
        </strong>
      </p>

      <DaemonCaseBlock />
      <MigrationPipeline />
      {/*<SuperpositionViz />*/}
      <GroupCards />
      <SpecStrip />

      {/*<p className="mt-8 text-[11px] text-ink-dim text-center tracking-[0.05em]">
        // see <span className="text-accent">compute/daemon.rs</span> ·{" "}
        <span className="text-accent">compute/orchestrator.rs</span> ·{" "}
        <span className="text-accent">
          compute/{"{replica,fork,standby}"}_group.rs
        </span>
      </p>*/}
    </section>
  );
}

interface DaemonCase {
  subtitle: string;
  code: React.ReactNode;
}

const DAEMON_CASES: readonly DaemonCase[] = [
  {
    subtitle: "trading agent · NYSE colo",
    code: (
      <>
        <span className="cm">
          // node A is failing — daemon migrates to node B
        </span>
        {"\n"}
        <span className="kw">let</span> daemon = Daemon::
        <span className="fn">new</span>(<span className="ty">TraderConfig</span>{" "}
        {"{"}
        {"\n    "}
        <span className="fn">requirements</span>:{" "}
        <span className="kw">vec</span>![ <span className="ty">Cap</span>::
        <span className="fn">Latency</span>(
        <span className="st">&quot;&lt;200μs to NYSE&quot;</span>) ],
        {"\n    "}
        <span className="fn">snapshot_interval</span>:{" "}
        <span className="ty">Duration</span>::
        <span className="fn">millis</span>(<span className="st">100</span>),
        {"\n"}
        {"});"}
        {"\n\n"}
        <span className="kw">match</span> daemon.
        <span className="fn">tick</span>
        (event).<span className="kw">await</span>? {"{"}
        {"\n    "}
        <span className="ty">Outcome</span>::
        <span className="fn">Order</span>(o) =&gt; bus.
        <span className="fn">publish</span>(o).
        <span className="kw">await</span>?,
        {"\n    "}
        <span className="ty">Outcome</span>::
        <span className="fn">Migrate</span>(target) =&gt;{" "}
        <span className="cm">// state moves with us</span>
        {"\n"}
        {"}"}
        {"\n\n"}
        <span className="cm">
          // origin_hash unchanged. subscribers don&apos;t notice.
        </span>
      </>
    ),
  },
  {
    subtitle: "inference daemon · follows user",
    code: (
      <>
        <span className="cm">
          // user moves laptop → desktop. session continues.
        </span>
        {"\n"}
        <span className="kw">let</span> daemon = Daemon::
        <span className="fn">new</span>(
        <span className="ty">InferenceConfig</span> {"{"}
        {"\n    "}
        <span className="fn">requirements</span>:{" "}
        <span className="kw">vec</span>![ <span className="ty">Cap</span>::
        <span className="fn">Gpu</span>(
        <span className="st">&quot;vram&gt;=24gb&quot;</span>),{" "}
        <span className="ty">Cap</span>::<span className="fn">Tag</span>(
        <span className="st">&quot;user:7af3&quot;</span>) ],
        {"\n    "}
        <span className="fn">snapshot_interval</span>:{" "}
        <span className="ty">Duration</span>::
        <span className="fn">millis</span>(<span className="st">250</span>),
        {"\n"}
        {"});"}
        {"\n\n"}
        <span className="kw">match</span> daemon.
        <span className="fn">tick</span>
        (event).<span className="kw">await</span>? {"{"}
        {"\n    "}
        <span className="ty">Outcome</span>::
        <span className="fn">Token</span>(t) =&gt; stream.
        <span className="fn">push</span>(t).
        <span className="kw">await</span>?,
        {"\n    "}
        <span className="ty">Outcome</span>::
        <span className="fn">Migrate</span>(target) =&gt;{" "}
        <span className="cm">// kv cache moves with us</span>
        {"\n"}
        {"}"}
        {"\n\n"}
        <span className="cm">
          // conversation context preserved across hardware change.
        </span>
      </>
    ),
  },
  {
    subtitle: "factory controller · plant-04",
    code: (
      <>
        <span className="cm">
          // edge box thermal alarm → migrate to standby
        </span>
        {"\n"}
        <span className="kw">let</span> daemon = Daemon::
        <span className="fn">new</span>(<span className="ty">PlcConfig</span>{" "}
        {"{"}
        {"\n    "}
        <span className="fn">requirements</span>:{" "}
        <span className="kw">vec</span>![ <span className="ty">Cap</span>::
        <span className="fn">Latency</span>(
        <span className="st">&quot;&lt;5ms to actuator&quot;</span>),{" "}
        <span className="ty">Cap</span>::<span className="fn">Tag</span>(
        <span className="st">&quot;floor-A&quot;</span>) ],
        {"\n    "}
        <span className="fn">snapshot_interval</span>:{" "}
        <span className="ty">Duration</span>::
        <span className="fn">millis</span>(<span className="st">50</span>),
        {"\n"}
        {"});"}
        {"\n\n"}
        <span className="kw">match</span> daemon.
        <span className="fn">tick</span>
        (event).<span className="kw">await</span>? {"{"}
        {"\n    "}
        <span className="ty">Outcome</span>::
        <span className="fn">Command</span>(c) =&gt; actuator.
        <span className="fn">send</span>(c).
        <span className="kw">await</span>?,
        {"\n    "}
        <span className="ty">Outcome</span>::
        <span className="fn">Migrate</span>(target) =&gt;{" "}
        <span className="cm">// control loop unaffected</span>
        {"\n"}
        {"}"}
        {"\n\n"}
        <span className="cm">
          // torque feedback never breaks. assembly line keeps moving.
        </span>
      </>
    ),
  },
  {
    subtitle: "sensor fusion · vehicle-07",
    code: (
      <>
        <span className="cm">
          // LIDAR + radar + camera, mesh-routed perception
        </span>
        {"\n"}
        <span className="kw">let</span> daemon = Daemon::
        <span className="fn">new</span>(<span className="ty">FusionConfig</span>{" "}
        {"{"}
        {"\n    "}
        <span className="fn">requirements</span>:{" "}
        <span className="kw">vec</span>![ <span className="ty">Cap</span>::
        <span className="fn">Latency</span>(
        <span className="st">&quot;&lt;1ms&quot;</span>),{" "}
        <span className="ty">Cap</span>::<span className="fn">Tag</span>(
        <span className="st">&quot;vehicle-07&quot;</span>) ],
        {"\n    "}
        <span className="fn">snapshot_interval</span>:{" "}
        <span className="ty">Duration</span>::
        <span className="fn">millis</span>(<span className="st">20</span>),
        {"\n"}
        {"});"}
        {"\n\n"}
        <span className="kw">match</span> daemon.
        <span className="fn">tick</span>
        (event).<span className="kw">await</span>? {"{"}
        {"\n    "}
        <span className="ty">Outcome</span>::
        <span className="fn">Detection</span>(d) =&gt; bus.
        <span className="fn">publish</span>(d).
        <span className="kw">await</span>?,
        {"\n    "}
        <span className="ty">Outcome</span>::
        <span className="fn">Migrate</span>(target) =&gt;{" "}
        <span className="cm">// perception state moves</span>
        {"\n"}
        {"}"}
        {"\n\n"}
        <span className="cm">
          // neighboring vehicles see continuous track.
        </span>
      </>
    ),
  },
];

type CodeTokenCls = "kw" | "ty" | "fn" | "cm" | "st";

interface CodeToken {
  text: string;
  cls?: CodeTokenCls;
}

const CODE_TOKEN_CLASSES = new Set<string>(["kw", "ty", "fn", "cm", "st"]);

function isCodeTokenCls(s: unknown): s is CodeTokenCls {
  return typeof s === "string" && CODE_TOKEN_CLASSES.has(s);
}

function flattenCodeJsx(node: React.ReactNode): CodeToken[] {
  const out: CodeToken[] = [];
  const walk = (n: React.ReactNode, cls?: CodeTokenCls): void => {
    if (n == null || typeof n === "boolean") return;
    if (typeof n === "string" || typeof n === "number") {
      out.push({ text: String(n), cls });
      return;
    }
    if (Array.isArray(n)) {
      for (const child of n) walk(child, cls);
      return;
    }
    if (typeof n === "object" && "props" in n) {
      const elem = n as React.ReactElement<{
        children?: React.ReactNode;
        className?: string;
      }>;
      const cn = elem.props.className;
      const newCls = isCodeTokenCls(cn) ? cn : cls;
      walk(elem.props.children, newCls);
    }
  };
  walk(node);
  return out;
}

function totalChars(tokens: readonly CodeToken[]): number {
  let n = 0;
  for (const t of tokens) n += t.text.length;
  return n;
}

function renderTypedTokens(
  tokens: readonly CodeToken[],
  charLimit: number,
): React.ReactNode[] {
  const out: React.ReactNode[] = [];
  let remaining = charLimit;
  for (let i = 0; i < tokens.length; i++) {
    const t = tokens[i];
    if (!t || remaining <= 0) break;
    const slice =
      t.text.length <= remaining ? t.text : t.text.slice(0, remaining);
    if (t.cls) {
      out.push(
        <span key={i} className={t.cls}>
          {slice}
        </span>,
      );
    } else {
      out.push(<Fragment key={i}>{slice}</Fragment>);
    }
    remaining -= slice.length;
  }
  return out;
}

const TYPING_CPS = 95;
const DWELL_SECONDS = 1.4;

function DaemonCaseBlock() {
  const caseIdxRef = useRef(0);
  const charIdxRef = useRef(0);
  const dwellRef = useRef(0);
  const [, forceUpdate] = useState(0);

  const caseTokens = useMemo(
    () => DAEMON_CASES.map((c) => flattenCodeJsx(c.code)),
    [],
  );

  useEffect(() => {
    let rafId = 0;
    let last = performance.now();
    const loop = (now: number): void => {
      const dt = (now - last) / 1000;
      last = now;
      const tokens = caseTokens[caseIdxRef.current] ?? [];
      const total = totalChars(tokens);
      if (charIdxRef.current < total) {
        charIdxRef.current = Math.min(
          total,
          charIdxRef.current + dt * TYPING_CPS,
        );
      } else {
        dwellRef.current += dt;
        if (dwellRef.current >= DWELL_SECONDS) {
          dwellRef.current = 0;
          caseIdxRef.current = (caseIdxRef.current + 1) % DAEMON_CASES.length;
          charIdxRef.current = 0;
        }
      }
      forceUpdate((n) => n + 1);
      rafId = requestAnimationFrame(loop);
    };
    rafId = requestAnimationFrame(loop);
    return () => cancelAnimationFrame(rafId);
  }, [caseTokens]);

  const idx = caseIdxRef.current;
  const current = DAEMON_CASES[idx];
  const tokens = caseTokens[idx] ?? [];
  if (!current) return null;
  const limit = Math.floor(charIdxRef.current);
  const isTyping = limit < totalChars(tokens);

  return (
    <div className="grid grid-cols-1 lg:grid-cols-[1.1fr_0.9fr] gap-8 my-12 items-start">
      <div className="border border-line bg-bg-2 overflow-hidden">
        <div className="bg-bg border-b border-line px-3.5 py-2 text-[10px] text-ink-dim tracking-[0.12em] uppercase flex justify-between items-center">
          <span key={`title-${idx}`} className="daemon-fade">
            <span className="text-accent font-semibold">CASE</span> ·{" "}
            {current.subtitle}
          </span>
          <span className="inline-flex gap-1.5 items-center">
            {DAEMON_CASES.map((_, i) => (
              <button
                key={i}
                type="button"
                aria-label={`Show case ${i + 1}`}
                onClick={() => {
                  caseIdxRef.current = i;
                  charIdxRef.current = 0;
                  dwellRef.current = 0;
                  forceUpdate((n) => n + 1);
                }}
                className={`w-1.5 h-1.5 rounded-full transition-colors cursor-pointer ${
                  i === idx ? "bg-accent" : "bg-ink-faint hover:bg-ink-dim"
                }`}
              />
            ))}
          </span>
        </div>
        <pre className="px-5 py-4 text-[12px] leading-[1.7] text-ink overflow-x-auto font-mono min-h-[260px]">
          {renderTypedTokens(tokens, limit)}
          <span
            className={isTyping ? "text-accent" : "cursor-blink"}
            aria-hidden
          >
            ▋
          </span>
        </pre>
      </div>

      <div>
        <h3 className="text-accent font-mono text-[14px] font-semibold tracking-[0.05em] uppercase mb-3.5">
          // what is a daemon
        </h3>
        <p className="text-ink-dim text-[13px] leading-[1.7] mb-4">
          A daemon is a stateful event processor whose identity is a keypair. It
          holds working state, snapshots periodically, and exposes five trait
          methods. Everything else — placement, migration, durability — is the
          runtime.
        </p>
        <ul className="daemon-list list-none mt-4">
          <li className="py-2.5 pl-5 border-b border-line text-ink-dim text-[12px] leading-[1.5] relative">
            <b className="text-ink font-medium">cryptographic identity</b> —
            origin_hash from ed25519. survives moves.
          </li>
          <li className="py-2.5 pl-5 border-b border-line text-ink-dim text-[12px] leading-[1.5] relative">
            <b className="text-ink font-medium">causal chain</b> — every event
            signed, links to parent. self-authenticating.
          </li>
          <li className="py-2.5 pl-5 border-b border-line text-ink-dim text-[12px] leading-[1.5] relative">
            <b className="text-ink font-medium">capability requirements</b> —
            daemon declares needs. mesh finds matching node.
          </li>
          <li className="py-2.5 pl-5 border-b border-line text-ink-dim text-[12px] leading-[1.5] relative">
            <b className="text-ink font-medium">snapshot + replay</b> — state
            captured periodically. gap replayed on restore.
          </li>
          <li className="py-2.5 pl-5 text-ink-dim text-[12px] leading-[1.5] relative">
            <b className="text-ink font-medium">opaque to mesh</b> — what the
            daemon does is its business. mesh just hosts.
          </li>
        </ul>
      </div>
    </div>
  );
}

function MigrationPipeline() {
  return (
    <div className="my-12">
      <div className="flex justify-between items-baseline mb-6 border-b border-line pb-3">
        <h3 className="font-head text-[22px] leading-tight text-ink tracking-[0.04em] lowercase">
          Mikoshi migration · 6 phases
        </h3>
        <span className="text-[10px] text-ink-dim tracking-[0.12em] uppercase">
          zero-downtime cutover · <b className="text-accent">~280ns total</b>
        </span>
      </div>

      <div className="phase-track-md grid grid-cols-1 md:grid-cols-3 lg:grid-cols-6 gap-0 border border-line bg-bg-2">
        {MIGRATION_PHASES.map((p, i) => (
          <div
            key={p.num}
            className={`phase-arrow relative px-4 py-5 ${i < 5 ? "border-r border-line" : ""} transition-colors hover:bg-accent/[0.04]`}
          >
            <div className="font-display text-[28px] text-accent leading-none mb-2">
              {p.num}
            </div>
            <div className="text-[11px] text-ink uppercase tracking-[0.1em] mb-2.5 font-semibold">
              {p.name}
            </div>
            <div className="text-[10px] text-ink-dim leading-[1.55]">
              {p.body}
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}

function SuperpositionViz() {
  return (
    <div className="mt-7 border border-line bg-bg-2">
      <div className="flex items-center justify-between border-b border-line px-5 py-2.5 text-[10px] tracking-[0.14em] text-ink-dim uppercase">
        <div className="flex items-center gap-3">
          <span className="text-accent">▸</span>
          <span>identity transfer · timeline</span>
        </div>
        <div className="flex items-center gap-4 font-mono normal-case tracking-normal">
          <span>
            <span className="text-ink-faint">window</span>{" "}
            <span className="text-accent">≈ 38ns</span>
          </span>
          <span>
            <span className="text-ink-faint">drop</span>{" "}
            <span className="text-accent">0</span>
          </span>
          <span className="flex items-center gap-1.5">
            <span className="w-1.5 h-1.5 rounded-full bg-accent inline-block animate-pulse-dot" />
            <span className="uppercase tracking-[0.12em]">live</span>
          </span>
        </div>
      </div>

      <div className="px-5 pt-6 pb-5">
        <svg
          className="w-full max-w-[600px] mx-auto aspect-[600/210] block"
          viewBox="0 0 600 210"
          preserveAspectRatio="xMidYMid meet"
        >
          <defs>
            <linearGradient id="superGrad" x1="0%" y1="0%" x2="100%" y2="0%">
              <stop offset="0%" stopColor="#c4ff3d" stopOpacity="1" />
              <stop offset="65%" stopColor="#c4ff3d" stopOpacity="0.55" />
              <stop offset="100%" stopColor="#c4ff3d" stopOpacity="0.1" />
            </linearGradient>
            <linearGradient id="superGrad2" x1="0%" y1="0%" x2="100%" y2="0%">
              <stop offset="0%" stopColor="#c4ff3d" stopOpacity="0.1" />
              <stop offset="35%" stopColor="#c4ff3d" stopOpacity="0.55" />
              <stop offset="100%" stopColor="#c4ff3d" stopOpacity="1" />
            </linearGradient>
            <linearGradient id="zoneFill" x1="0%" y1="0%" x2="0%" y2="100%">
              <stop offset="0%" stopColor="#c4ff3d" stopOpacity="0.03" />
              <stop offset="50%" stopColor="#c4ff3d" stopOpacity="0.09" />
              <stop offset="100%" stopColor="#c4ff3d" stopOpacity="0.03" />
            </linearGradient>
            <pattern
              id="superGrid"
              width="60"
              height="20"
              patternUnits="userSpaceOnUse"
            >
              <path
                d="M 60 0 L 0 0 0 20"
                fill="none"
                stroke="#1a1f1a"
                strokeWidth="0.4"
              />
            </pattern>
          </defs>

          <rect
            x="60"
            y="40"
            width="520"
            height="120"
            fill="url(#superGrid)"
            opacity="0.6"
          />

          <rect
            x="60"
            y="55"
            width="520"
            height="22"
            fill="#0e120e"
            opacity="0.6"
          />
          <rect
            x="60"
            y="123"
            width="520"
            height="22"
            fill="#0e120e"
            opacity="0.6"
          />

          <text
            x="54"
            y="69"
            textAnchor="end"
            fontFamily="JetBrains Mono"
            fontSize="9"
            fill="#c4ff3d"
            fontWeight="600"
          >
            node.A
          </text>
          <text
            x="54"
            y="79"
            textAnchor="end"
            fontFamily="JetBrains Mono"
            fontSize="7"
            fill="#4a5249"
          >
            0x7af3
          </text>
          <text
            x="54"
            y="138"
            textAnchor="end"
            fontFamily="JetBrains Mono"
            fontSize="9"
            fill="#c4ff3d"
            fontWeight="600"
          >
            node.B
          </text>
          <text
            x="54"
            y="148"
            textAnchor="end"
            fontFamily="JetBrains Mono"
            fontSize="7"
            fill="#4a5249"
          >
            0x2c91
          </text>

          <line
            x1="60"
            y1="40"
            x2="60"
            y2="160"
            stroke="#2d352c"
            strokeWidth="0.6"
          />

          <line
            x1="60"
            y1="66"
            x2="580"
            y2="66"
            stroke="#1a1f1a"
            strokeWidth="1"
          />
          <line
            x1="60"
            y1="66"
            x2="400"
            y2="66"
            stroke="url(#superGrad)"
            strokeWidth="2.5"
            strokeLinecap="round"
          />
          <circle cx="60" cy="66" r="4" fill="#c4ff3d" />
          <circle
            cx="60"
            cy="66"
            r="7"
            fill="none"
            stroke="#c4ff3d"
            strokeOpacity="0.3"
          />
          <g fontFamily="JetBrains Mono" fontSize="7" fill="#6b7568">
            <text x="80" y="62">
              ▸ exec
            </text>
            <text x="140" y="62">
              heap.alloc
            </text>
            <text x="220" y="62">
              cap.read
            </text>
            <text x="300" y="62">
              snap.encode
            </text>
          </g>

          <line
            x1="60"
            y1="134"
            x2="580"
            y2="134"
            stroke="#1a1f1a"
            strokeWidth="1"
          />
          <line
            x1="240"
            y1="134"
            x2="580"
            y2="134"
            stroke="url(#superGrad2)"
            strokeWidth="2.5"
            strokeLinecap="round"
          />
          <circle cx="580" cy="134" r="4" fill="#c4ff3d" />
          <circle
            cx="580"
            cy="134"
            r="7"
            fill="none"
            stroke="#c4ff3d"
            strokeOpacity="0.3"
          />
          <g fontFamily="JetBrains Mono" fontSize="7" fill="#6b7568">
            <text x="260" y="148">
              unpack
            </text>
            <text x="320" y="148">
              replay
            </text>
            <text x="390" y="148">
              ▸ exec
            </text>
            <text x="470" y="148">
              cap.write
            </text>
          </g>

          <rect
            className="superpose-zone"
            x="240"
            y="40"
            width="180"
            height="120"
            fill="url(#zoneFill)"
            stroke="#6b8a1e"
            strokeDasharray="3 3"
            strokeWidth="0.8"
          />
          <line
            x1="240"
            y1="36"
            x2="240"
            y2="40"
            stroke="#c4ff3d"
            strokeWidth="1"
          />
          <line
            x1="420"
            y1="36"
            x2="420"
            y2="40"
            stroke="#c4ff3d"
            strokeWidth="1"
          />
          <line
            x1="240"
            y1="160"
            x2="240"
            y2="164"
            stroke="#c4ff3d"
            strokeWidth="1"
          />
          <line
            x1="420"
            y1="160"
            x2="420"
            y2="164"
            stroke="#c4ff3d"
            strokeWidth="1"
          />

          <text
            x="330"
            y="100"
            fontFamily="Major Mono Display"
            fontSize="13"
            fill="#c4ff3d"
            textAnchor="middle"
            letterSpacing="2"
          >
            superposed
          </text>
          <text
            x="330"
            y="113"
            fontFamily="JetBrains Mono"
            fontSize="7"
            fill="#8a9482"
            textAnchor="middle"
            letterSpacing="1"
          >
            both nodes hold authority
          </text>

          <line
            x1="330"
            y1="40"
            x2="330"
            y2="160"
            stroke="#c4ff3d"
            strokeWidth="0.6"
            strokeDasharray="1 3"
            opacity="0.45"
          />

          <line
            x1="60"
            y1="180"
            x2="580"
            y2="180"
            stroke="#2d352c"
            strokeWidth="0.6"
          />
          <g fontFamily="JetBrains Mono" fontSize="7" fill="#4a5249">
            <line
              x1="60"
              y1="176"
              x2="60"
              y2="184"
              stroke="#4a5249"
              strokeWidth="0.6"
            />
            <text x="60" y="196" textAnchor="middle">
              0ns
            </text>
            <line
              x1="150"
              y1="178"
              x2="150"
              y2="182"
              stroke="#3a423a"
              strokeWidth="0.4"
            />
            <line
              x1="240"
              y1="174"
              x2="240"
              y2="186"
              stroke="#c4ff3d"
              strokeWidth="1"
            />
            <text
              x="240"
              y="196"
              textAnchor="middle"
              fill="#c4ff3d"
              fontWeight="600"
            >
              12ns
            </text>
            <line
              x1="330"
              y1="178"
              x2="330"
              y2="182"
              stroke="#3a423a"
              strokeWidth="0.4"
            />
            <line
              x1="420"
              y1="174"
              x2="420"
              y2="186"
              stroke="#c4ff3d"
              strokeWidth="1"
            />
            <text
              x="420"
              y="196"
              textAnchor="middle"
              fill="#c4ff3d"
              fontWeight="600"
            >
              50ns
            </text>
            <line
              x1="500"
              y1="178"
              x2="500"
              y2="182"
              stroke="#3a423a"
              strokeWidth="0.4"
            />
            <line
              x1="580"
              y1="176"
              x2="580"
              y2="184"
              stroke="#4a5249"
              strokeWidth="0.6"
            />
            <text x="580" y="196" textAnchor="middle">
              ~1µs
            </text>
          </g>

          <circle
            className="superpose-pkt-a"
            cx="60"
            cy="66"
            r="3.5"
            fill="#c4ff3d"
          />
          <circle
            className="superpose-pkt-b"
            cx="60"
            cy="134"
            r="3.5"
            fill="#c4ff3d"
          />
        </svg>

        <div className="grid grid-cols-3 gap-px bg-line border border-line mt-4 text-[10px]">
          <div className="bg-bg-2 px-3.5 py-3">
            <div className="flex items-baseline justify-between gap-2 mb-1">
              <span className="text-ink-dim tracking-[0.14em] uppercase">
                T₀
              </span>
              <span className="font-mono text-ink-faint">0–12ns</span>
            </div>
            <div className="text-ink mb-1">source running</div>
            <div className="text-ink-faint leading-[1.45]">
              A is sole authority. B is dark.
            </div>
          </div>
          <div className="bg-bg-2 px-3.5 py-3 relative">
            <div className="absolute inset-0 bg-accent/[0.05] pointer-events-none" />
            <div className="relative flex items-baseline justify-between gap-2 mb-1">
              <span className="text-accent tracking-[0.14em] uppercase font-semibold">
                T_super
              </span>
              <span className="font-mono text-accent">12–50ns</span>
            </div>
            <div className="relative text-ink mb-1">superposition</div>
            <div className="relative text-ink-faint leading-[1.45]">
              both A and B execute. routing flips.
            </div>
          </div>
          <div className="bg-bg-2 px-3.5 py-3">
            <div className="flex items-baseline justify-between gap-2 mb-1">
              <span className="text-ink-dim tracking-[0.14em] uppercase">
                T₁
              </span>
              <span className="font-mono text-ink-faint">50ns+</span>
            </div>
            <div className="text-ink mb-1">target authoritative</div>
            <div className="text-ink-faint leading-[1.45]">
              A releases. B holds identity.
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

function GroupCards() {
  return (
    <div className="grid grid-cols-1 md:grid-cols-3 gap-px bg-accent-dim mt-8 border border-accent-dim">
      {GROUP_CARDS.map((g) => (
        <div
          key={g.id}
          className="bg-bg p-7 transition-colors hover:bg-bg-2 relative"
        >
          <div className="text-[10px] text-accent tracking-[0.18em] mb-1.5">
            {g.id}
          </div>
          <h3 className="font-head text-[22px] leading-tight text-ink mb-1.5 tracking-[0.04em] lowercase">
            {g.name}
          </h3>
          <div className="text-[10px] text-ink-dim mb-4 tracking-[0.05em]">
            {g.meta}
          </div>
          <pre className="font-mono text-[10px] text-ink-dim leading-[1.5] bg-bg-2 p-3.5 border-l-2 border-accent mb-4 whitespace-pre overflow-x-auto">
            {g.ascii}
          </pre>
          <p className="text-[12px] text-ink-dim leading-[1.6] mb-4">
            {g.body}
          </p>
          <div className="border-t border-line pt-3.5 grid grid-cols-2 gap-2.5 text-[10px]">
            {g.spec.map(([k, v]) => (
              <span key={k} className="contents">
                <span className="text-ink-dim uppercase tracking-[0.1em]">
                  {k}
                </span>
                <span className="text-ink">{v}</span>
              </span>
            ))}
          </div>
        </div>
      ))}
    </div>
  );
}

const SPEC_STRIP: ReadonlyArray<{
  label: string;
  value: string;
  unit: string;
  body: React.ReactNode;
}> = [
  {
    label: "// trait surface",
    value: "5",
    unit: "methods",
    body: "name · requirements · process · snapshot · restore",
  },
  {
    label: "// migration phases",
    value: "6",
    unit: "strict",
    body: "snapshot → transfer → restore → replay → cutover → complete",
  },
  {
    label: "// wire messages",
    value: "10",
    unit: "types",
    body: (
      <>
        orchestrator + source + target over{" "}
        <code className="text-accent">0x0500</code>
      </>
    ),
  },
  {
    label: "// cycle time",
    value: "~280",
    unit: "ns",
    body: "full snapshot → activate, faster than a context switch",
  },
];

function SpecStrip() {
  return (
    <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 border border-line mt-10">
      {SPEC_STRIP.map((s, i) => (
        <div
          key={s.label}
          className={`px-6 py-5 bg-bg-2 ${i < SPEC_STRIP.length - 1 ? "border-b lg:border-b-0 lg:border-r border-line" : ""}`}
        >
          <div className="text-[10px] text-ink-dim tracking-[0.12em] uppercase mb-2">
            {s.label}
          </div>
          <div className="font-display text-[22px] text-accent leading-[1.1] mb-1">
            {s.value}
            <span className="text-[13px] text-ink-dim ml-1">{s.unit}</span>
          </div>
          <div className="text-[10px] text-ink-dim leading-[1.4]">{s.body}</div>
        </div>
      ))}
    </div>
  );
}

interface InstallCard {
  lang: string;
  ext: string;
  cmd: string;
  copy: string;
  meta: string;
}

const INSTALL_CARDS: readonly InstallCard[] = [
  {
    lang: "Rust",
    ext: ".rs",
    cmd: "$ cargo add ai2070-net-sdk",
    copy: "cargo add ai2070-net-sdk",
    meta: "crate: ai2070-net-sdk",
  },
  {
    lang: "TypeScript",
    ext: ".ts",
    cmd: "$ npm i @ai2070/net-sdk\n       @ai2070/net",
    copy: "npm i @ai2070/net-sdk @ai2070/net",
    meta: "scope: @ai2070",
  },
  {
    lang: "Python",
    ext: ".py",
    cmd: "$ pip install ai2070-net-sdk",
    copy: "pip install ai2070-net-sdk",
    meta: "dist: ai2070-net-sdk",
  },
  {
    lang: "Go",
    ext: ".go",
    cmd: "$ go get github.com/\n  ai-2070/net/go",
    copy: "go get github.com/ai-2070/net/go",
    meta: "module: ai-2070/net/go",
  },
];

interface ComponentSpec {
  num: string;
  name: string;
  tagline: string;
  body: React.ReactNode;
  stats: string;
}

const COMPONENTS: readonly ComponentSpec[] = [
  {
    num: "▸ component.01",
    name: "nRPC",
    tagline: "// typed request/response",
    body: (
      <>
        Request/response semantics built from a pair of streams. A server
        registers a handler with{" "}
        <code className="font-mono text-accent">serve_rpc</code>; clients
        dispatch with <code className="font-mono text-accent">call_typed</code>.
        The streams stay primitive — nRPC just wraps them in a typed handle and
        completes when the response lands.
      </>
    ),
    stats: "TypedMeshRpc · paired streams · zero new wire",
  },
  {
    num: "▸ component.02",
    name: "RedEX",
    tagline: "// append-only event log",
    body: (
      <>
        The log unbundled and local. 20-byte index records, optional disk
        persistence per channel, atomic backfill-then-live tailing. A Pi keeps a
        tiny log of its own readings; a server keeps a huge one. No cluster
        consensus — log is local, replay is local, retention is local.
      </>
    ),
    stats: "21.3 M append/s · 138 ns tail",
  },
  {
    num: "▸ component.03",
    name: "CortEX",
    tagline: "// RedEX, folded",
    body: (
      <>
        A reactive, queryable projection of the log, updated event-by-event.
        Your &quot;database&quot; isn&apos;t a process you connect to —
        it&apos;s a{" "}
        <code className="font-mono text-accent">Vec&lt;Task&gt;</code> or{" "}
        <code className="font-mono text-accent">
          HashMap&lt;Uuid, Memory&gt;
        </code>{" "}
        in your code, updating as events fold in. Queries are direct memory
        access.
      </>
    ),
    stats: "8.98 ns find_unique · 8.87 M ingest/s",
  },
  {
    num: "▸ component.04",
    name: "NetDB",
    tagline: "// unified query façade",
    body: (
      <>
        One handle bundling typed collections under{" "}
        <code className="font-mono text-accent">db.tasks</code>,{" "}
        <code className="font-mono text-accent">db.memories</code>, and friends.
        Prisma-style <code className="font-mono text-accent">find_unique</code>{" "}
        / <code className="font-mono text-accent">find_many</code> across Rust,
        TypeScript, and Python — whole-database snapshots round-trip between
        languages.
      </>
    ),
    stats: "6.30 μs open · 48 KB / 1K rows",
  },
];

function ComponentsSection() {
  return (
    <section id="components" className="border-b border-line px-6 py-20">
      <SectionLabel>§09 / components on the mesh</SectionLabel>
      <DisplayHeading>
        four primitives.
        <br />
        one mesh.
      </DisplayHeading>

      <p className="text-[16px] text-ink max-w-[740px] leading-[1.6] font-light mb-12">
        The mesh moves bytes. Everything above is a thin, optional layer —
        local-first, feature-flagged, opt-in. Light up the ones you need; the
        wire doesn&apos;t care which.
      </p>

      <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 gap-px bg-line border border-line">
        {COMPONENTS.map((c) => (
          <div
            key={c.name}
            className="bg-bg p-7 transition-colors hover:bg-bg-2 flex flex-col"
          >
            <div className="text-[10px] text-accent tracking-[0.18em] mb-2.5">
              {c.num}
            </div>
            <h3 className="font-head text-[22px] leading-tight text-ink mb-1.5 tracking-[0.04em] lowercase">
              {c.name}
            </h3>
            <div className="text-[10px] text-ink-dim mb-4 tracking-[0.05em]">
              {c.tagline}
            </div>
            <p className="text-[12px] text-ink-dim leading-[1.6] mb-4 flex-1">
              {c.body}
            </p>
            <div className="border-t border-dashed border-line pt-3 font-mono text-[10px] text-ink-dim tracking-[0.04em]">
              {c.stats}
            </div>
          </div>
        ))}
      </div>
    </section>
  );
}

// `datafortsctl status --live` — fake operator console for the mesh's
// storage pool. Box-drawing chrome, pressure bars per node, animated
// utilization drift, occasional overflow events that fire and scroll
// through the event tail. Pure terminal aesthetic; the data behavior IS
// the message ("data is fluid").

interface PoolNode {
  id: string;
  fill: number;
}

interface PoolEvent {
  id: number;
  ts: string;
  kind: "push" | "heat" | "cool" | "absorb";
  body: string;
}

const POOL_INITIAL: ReadonlyArray<PoolNode> = [
  { id: "node.0x7af3", fill: 0.64 },
  { id: "node.0x2c91", fill: 0.91 },
  { id: "node.0xeb29", fill: 0.31 },
  { id: "node.0xfbb1", fill: 0.78 },
  { id: "node.0x9a3e", fill: 0.22 },
];

const POOL_HIGH = 0.85;
const POOL_LOW = 0.3;

function poolTs(): string {
  const d = new Date();
  const m = String(d.getMinutes()).padStart(2, "0");
  const s = String(d.getSeconds()).padStart(2, "0");
  const ms = String(d.getMilliseconds()).padStart(3, "0");
  return `${m}:${s}.${ms}`;
}

function shortHash(): string {
  return Math.floor(Math.random() * 0xffffff)
    .toString(16)
    .padStart(6, "0");
}

function pickHumanBytes(min: number, max: number): string {
  const mb = min + Math.random() * (max - min);
  return mb >= 1024 ? `${(mb / 1024).toFixed(1)}G` : `${mb.toFixed(1)}M`;
}

function renderBar(fill: number, width: number): string {
  const filled = Math.round(fill * width);
  return "▰".repeat(filled) + "░".repeat(width - filled);
}

const BAR_WIDTH = 22;

function DatafortsConsole() {
  const [nodes, setNodes] = useState<ReadonlyArray<PoolNode>>(POOL_INITIAL);
  const [events, setEvents] = useState<ReadonlyArray<PoolEvent>>([]);

  useEffect(() => {
    let eventCounter = 0;

    const pushEvent = (kind: PoolEvent["kind"], body: string): void => {
      eventCounter += 1;
      const e: PoolEvent = { id: eventCounter, ts: poolTs(), kind, body };
      setEvents((prev) => [...prev.slice(-5), e]);
    };

    // seed initial event log so the panel doesn't look empty
    pushEvent("push", `0x4d8d → 0xeb29 · 8.4M · accepted · 204ms`);
    pushEvent("heat", `0x7e3a · rate 0.78 · gravity → 0x7af3`);
    pushEvent("cool", `0xb547 · rate 0.12 · evictable`);
    pushEvent("absorb", `0x9a3e · free +18% · open`);
    pushEvent("push", `0x2c91 → 0x9a3e · 18.2M · accepted · 156ms`);

    const id = window.setInterval(() => {
      setNodes((prev) => {
        const next = prev.map((n) => {
          const drift = (Math.random() - 0.45) * 0.05;
          return {
            ...n,
            fill: Math.max(0.12, Math.min(0.96, n.fill + drift)),
          };
        });

        // any node over high-water → overflow into lowest neighbor
        const overIdx = next.findIndex((n) => n.fill >= POOL_HIGH);
        if (overIdx >= 0) {
          let toIdx = -1;
          let minFill = 1;
          for (let i = 0; i < next.length; i++) {
            if (i === overIdx) continue;
            const f = next[i]?.fill ?? 1;
            if (f < minFill) {
              minFill = f;
              toIdx = i;
            }
          }
          if (toIdx >= 0) {
            const amount = 0.1;
            const from = next[overIdx];
            const to = next[toIdx];
            if (from && to) {
              next[overIdx] = { ...from, fill: from.fill - amount };
              next[toIdx] = { ...to, fill: Math.min(0.96, to.fill + amount) };
              const size = pickHumanBytes(8, 220);
              const ms = Math.floor(150 + Math.random() * 220);
              const fromShort = from.id.slice(-4);
              const toShort = to.id.slice(-4);
              pushEvent(
                "push",
                `0x${shortHash().slice(0, 4)} · 0x${fromShort} → 0x${toShort} · ${size} · ${ms}ms`,
              );
            }
          }
        }

        // occasional heat/cool/absorb events
        const r = Math.random();
        if (r < 0.22) {
          const rate = (0.4 + Math.random() * 0.5).toFixed(2);
          pushEvent(
            "heat",
            `0x${shortHash().slice(0, 4)} · rate ${rate} · gravity active`,
          );
        } else if (r < 0.4) {
          const rate = (0.05 + Math.random() * 0.18).toFixed(2);
          pushEvent(
            "cool",
            `0x${shortHash().slice(0, 4)} · rate ${rate} · evictable`,
          );
        } else if (r < 0.5) {
          const node = next[Math.floor(Math.random() * next.length)];
          if (node && node.fill < 0.5) {
            const freePct = Math.round((1 - node.fill) * 100);
            pushEvent(
              "absorb",
              `${node.id.slice(-6)} · free ${freePct}% · open`,
            );
          }
        }

        return next;
      });
    }, 1400);

    return () => window.clearInterval(id);
  }, []);

  const totalFill =
    nodes.reduce((acc, n) => acc + n.fill, 0) / Math.max(1, nodes.length);
  const totalBar = renderBar(totalFill, 36);

  return (
    <div className="border border-line bg-bg-2 overflow-hidden font-mono text-[12px] leading-[1.75]">
      {/* terminal title bar */}
      <div className="flex items-center justify-between border-b border-line px-4 py-2 text-[10px] tracking-[0.14em] text-ink-dim uppercase">
        <span className="flex items-center gap-3">
          <span className="inline-flex gap-1">
            <span className="frame-dot-r w-[7px] h-[7px] rounded-full" />
            <span className="frame-dot-y w-[7px] h-[7px] rounded-full" />
            <span className="frame-dot-g w-[7px] h-[7px] rounded-full" />
          </span>
          <span className="text-accent">$</span>
          <span>
            net dataforts status{" "}
            <span className="text-ink-faint">--live --pool=mesh</span>
          </span>
        </span>
        <span className="flex items-center gap-1.5 normal-case tracking-normal">
          <span className="w-1.5 h-1.5 rounded-full bg-accent inline-block animate-pulse-dot" />
          <span className="text-accent">live</span>
        </span>
      </div>

      <div className="px-5 py-4">
        <div className="text-ink-dim flex items-center gap-3 whitespace-nowrap">
          <span>┌─ mesh storage pool</span>
          <span className="flex-1 border-t border-dashed border-line-/40 hidden" />
          <span className="text-ink-faint">5 nodes · 892 GB cap</span>
        </div>
        <div className="text-ink mt-1 flex items-center gap-3 whitespace-nowrap">
          <span className="text-ink-faint">│</span>
          <span className="text-ink-dim">pressure</span>
          <span className="text-accent">{totalBar}</span>
          <span className="text-ink">{Math.round(totalFill * 100)}%</span>
          <span className="text-ink-faint">·</span>
          <span
            className={
              totalFill >= POOL_HIGH
                ? "text-warn"
                : totalFill <= POOL_LOW
                  ? "text-cyan"
                  : "text-ink-dim"
            }
          >
            {totalFill >= POOL_HIGH
              ? "OVER"
              : totalFill <= POOL_LOW
                ? "COLD"
                : "STEADY"}
          </span>
        </div>
        <div className="text-ink-dim mt-1">├─ nodes</div>

        {nodes.map((n, i) => {
          const isOver = n.fill >= POOL_HIGH;
          const isUnder = n.fill <= POOL_LOW;
          const tag = isOver ? "PUSH" : isUnder ? "RECV" : "····";
          const barColor = isOver
            ? "text-warn"
            : isUnder
              ? "text-cyan"
              : "text-accent";
          const tagColor = isOver
            ? "text-warn"
            : isUnder
              ? "text-cyan"
              : "text-ink-faint";
          const tree = i === nodes.length - 1 ? "└─" : "├─";
          return (
            <div
              key={n.id}
              className="flex items-center gap-3 whitespace-nowrap"
            >
              <span className="text-ink-faint">│ {tree}</span>
              <span className="text-ink">{n.id}</span>
              <span className={barColor}>{renderBar(n.fill, BAR_WIDTH)}</span>
              <span className="text-ink">
                {String(Math.round(n.fill * 100)).padStart(3, " ")}%
              </span>
              <span className={tagColor}>{tag}</span>
            </div>
          );
        })}

        <div className="text-ink-dim mt-3">├─ recent events</div>
        <div className="min-h-[120px]">
          {events.map((e) => {
            const kindColor =
              e.kind === "push"
                ? "text-accent"
                : e.kind === "heat"
                  ? "text-accent-dim"
                  : e.kind === "cool"
                    ? "text-cyan"
                    : "text-ink-dim";
            return (
              <div
                key={e.id}
                className="event-line-in flex items-baseline gap-3 whitespace-nowrap overflow-hidden"
              >
                <span className="text-ink-faint">│</span>
                <span className="text-ink-faint" style={{ minWidth: "9ch" }}>
                  {e.ts}
                </span>
                <span className={`${kindColor}`} style={{ minWidth: "7ch" }}>
                  [{e.kind}]
                </span>
                <span className="text-ink-dim flex-1 truncate">{e.body}</span>
              </div>
            );
          })}
        </div>
        <div className="text-ink-dim mt-1">└─ end of stream</div>

        <div className="mt-4 text-ink-faint text-[10px] tracking-[0.04em]">
          ▸ press <span className="text-accent">^C</span> to detach · gravity
          recalc every <span className="text-accent">1.4s</span> · watermark
          high <span className="text-accent">·85</span> / low{" "}
          <span className="text-cyan">·30</span>
        </div>
      </div>
    </div>
  );
}

interface FluidPrinciple {
  phrase: string;
  body: React.ReactNode;
}

const CAPABILITY_STRIP: ReadonlyArray<{
  num: string;
  name: string;
  body: string;
  isNew?: boolean;
}> = [
  {
    num: "mesh.storage.1",
    name: "Overflow",
    isNew: true,
    body: "storage doesn't run out. when one disk fills up, the mesh catches the spillover.",
  },
  {
    num: "mesh.storage.2",
    name: "Data Gravity",
    body: "the files aren't moved. files settle near nodes that use them.",
  },
  {
    num: "mesh.storage.3",
    name: "Read-your-writes",
    body: "if you wrote it, you can read it. right now. no coordination lag.",
  },
  {
    num: "mesh.storage.4",
    name: "BlobRef",
    body: "one handle gets you any file. the mesh finds it — wherever it lives.",
  },
];

function DatafortsSection() {
  return (
    <section
      id="dataforts"
      className="relative overflow-hidden border-b border-line px-6 py-20"
    >
      <SectionLabel>§07 / storage // new</SectionLabel>
      <DisplayHeading>
        Dataforts:
        <br />
        <span className="text-accent">
          data became
          <br />a fluid.
        </span>
      </DisplayHeading>

      <p className="text-[16px] text-ink max-w-[740px] leading-[1.7] font-light mb-12">
        For 60 years, files were objects nailed to a location — a disk in a box.
        Traditional storage treats files like permanent objects locked to a
        single machine.
        <br />
        <br />
        <strong className="text-accent font-medium">
          Dataforts treats storage as flow.
        </strong>{" "}
        When a device approaches capacity, it overflows onto the mesh. The
        folder stays local. The capacity is the mesh. Reads create gravity. Hot
        data moves closer. Everything is in motion.
      </p>

      <DatafortsConsole />

      {/* Capability strip — compact horizontal list */}
      <div className="mt-12 grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 border-t border-l border-line">
        {CAPABILITY_STRIP.map((c) => (
          <div
            key={c.name}
            className="border-r border-b border-line bg-bg-2/40 p-5"
          >
            <div className="flex items-baseline justify-between mb-2">
              <span className="font-mono text-[10px] text-accent tracking-[0.14em]">
                ▸ {c.num}
              </span>
              {c.isNew ? (
                <span className="bg-accent text-bg px-1.5 py-0.5 text-[9px] font-bold tracking-[0.18em]">
                  NEW
                </span>
              ) : null}
            </div>
            <h3 className="font-head text-[16px] leading-tight text-ink mb-2 tracking-[0.04em] lowercase">
              {c.name}
            </h3>
            <p className="text-[11px] text-ink-dim leading-[1.55]">{c.body}</p>
          </div>
        ))}
      </div>
    </section>
  );
}

// =========================================================================
// MeshOS — Atomic Playboys. Self-organizing cluster behavior.
// AutoMesh: matrix-style mesh that draws itself in monospace glyphs.
// Nodes spawn from the edges, edges form via flowing dot-streams, daemons
// (mikoshi) hop between hosts trailing char-ghosts, data nodes pull
// workloads via gravity wells, drift correction nudges the formation
// toward a new equilibrium. Faint matrix rain falls behind everything.
// =========================================================================

type MeshCapability = "device" | "compute" | "region" | "daemon" | "datafort";

interface AutoMeshNode {
  id: number;
  hex: string;
  x: number;
  y: number;
  tx: number;
  ty: number;
  vx: number;
  vy: number;
  cap: MeshCapability;
  spawnDelay: number;
  age: number;
  emitted: boolean;
}

interface AutoMeshDaemonTrail {
  x: number;
  y: number;
  age: number;
  ch: string;
}

interface AutoMeshDaemon {
  id: number;
  hex: string;
  hostIdx: number;
  migrating: boolean;
  fromIdx: number;
  toIdx: number;
  migrateT: number;
  trail: AutoMeshDaemonTrail[];
}

interface AutoMeshRainCol {
  x: number;
  y: number;
  speed: number;
  gap: number;
  tokens: string[];
  tick: number;
}

interface AutoMeshLayout {
  count: number;
  daemons: number;
  edgeRadius: number;
  rainCols: number;
}

interface AutoMeshFeedLine {
  id: number;
  ts: string;
  prefix: string;
  prefixColor: string;
  body: React.ReactNode;
}

const AUTOMESH_MONO_FONT =
  '"JetBrains Mono", ui-monospace, SFMono-Regular, monospace';

const AUTOMESH_MATRIX_GLYPHS =
  "01アイウエオカキクケコサシスセソタチツテトナニヌネノハヒフヘホ◢◣◤◥◆◇■□◈◉●○∙·".split(
    "",
  );

function pickMatrixGlyph(): string {
  return AUTOMESH_MATRIX_GLYPHS[
    Math.floor(Math.random() * AUTOMESH_MATRIX_GLYPHS.length)
  ]!;
}

function autoMeshLayout(width: number): AutoMeshLayout {
  if (width < 540)
    return { count: 12, daemons: 4, edgeRadius: 105, rainCols: 14 };
  if (width < 880)
    return { count: 16, daemons: 5, edgeRadius: 130, rainCols: 22 };
  return { count: 22, daemons: 7, edgeRadius: 150, rainCols: 32 };
}

function pickCapability(): MeshCapability {
  const r = Math.random();
  if (r < 0.18) return "datafort";
  if (r < 0.4) return "device";
  if (r < 0.6) return "compute";
  if (r < 0.8) return "region";
  return "daemon";
}

function capabilityRgb(cap: MeshCapability): string {
  switch (cap) {
    case "device":
      return "196, 255, 61";
    case "compute":
      return "107, 138, 30";
    case "region":
      return "61, 240, 255";
    case "daemon":
      return "212, 220, 208";
    case "datafort":
      return "255, 255, 255";
  }
}

function nodeGlyph(cap: MeshCapability): string {
  switch (cap) {
    case "device":
      return "◈";
    case "compute":
      return "▣";
    case "region":
      return "◉";
    case "daemon":
      return "◇";
    case "datafort":
      return "■";
  }
}

function easeInOut(t: number): number {
  return t < 0.5 ? 2 * t * t : 1 - Math.pow(-2 * t + 2, 2) / 2;
}

function autoMeshTs(): string {
  const d = new Date();
  return `${String(d.getMinutes()).padStart(2, "0")}:${String(
    d.getSeconds(),
  ).padStart(2, "0")}.${String(d.getMilliseconds()).padStart(3, "0")}`;
}

function autoMeshHex4(): string {
  return Math.floor(Math.random() * 0x10000)
    .toString(16)
    .padStart(4, "0");
}

const AUTOMESH_FEED_MAX = 7;

// Rich pool of random capability tags emitted on cap.announce. Mixes
// edge hardware (sensors, mcus, radios), datacenter gear (gpu/cpu/nic),
// market colos, runtimes, and identity primitives — the breadth of what
// a heterogeneous mesh actually carries.
const MESH_AD_TAGS: ReadonlyArray<string> = [
  // gpu / accelerator
  "gpu:h100",
  "gpu:h200",
  "gpu:gb200",
  "gpu:gb300",
  "gpu:rtx-5090",
  "gpu:4090",
  "npu:hailo-8",
  // cpu / arm / mcu
  "cpu:epyc-9654",
  "cpu:xeon-8480",
  // memory
  "vram:24gb",
  "vram:80gb",
  "vram:192gb",
  "ram:64gb",
  "ram:512gb",
  "ram:1tb",
  // storage
  "nvme:2tb",
  "nvme:8tb",
  "nvme:30tb",
  "cache:hot-tier",
  "cache:cold-tier",
  // networking
  "nic:100gbe",
  "nic:25gbe",
  "nic:1gbe",
  "rdma:roce-v2",
  "infiniband:hdr-200g",
  "lat:<200μs",
  "lat:<1ms",
  "lat:<50ns",
  // sensors / actuators / devices
  "sensor:lidar",
  "sensor:imu",
  "sensor:gps",
  "sensor:thermal",
  "sensor:radar",
  "sensor:ph",
  "sensor:co2",
  "camera:4k",
  "camera:depth",
  "actuator:servo",
  "actuator:relay",
  "actuator:valve",
  // radio
  "radio:5g-mmwave",
  "radio:wifi7",
  "radio:5g",
  // industrial
  "plc:siemens-s7",
  "plc:rockwell",
  "robot:fanuc-r-30ib",
  // region / dc / colo
  "region:eu-west",
  "region:us-east-1",
  "region:apac-sg",
  "region:sa-east",
  "tag:nyse-colo",
  "tag:cme-floor",
  "tag:nikkei-225",
  "tag:lse-pit",
  "tag:floor-A",
  "tag:rack-12",
  // runtime / os
  "runtime:wasm",
  "runtime:cuda-12.4",
  "runtime:rocm-6.0",
  "kernel:linux-6.8",
  "kernel:rtos",
  "os:ubuntu-24",
  "os:nixos",
  // identity / attestation
  "cert:secure-enclave",
];

function pickAdTags(): string[] {
  const count = Math.random() < 0.55 ? 1 : 2;
  const out: string[] = [];
  // Dedupe by category prefix — never emit two `nic:`/`os:`/`ram:`/
  // `kernel:`/`region:`/etc. tags in the same advertisement.
  const usedCats = new Set<string>();
  let safety = 12;
  while (out.length < count && safety-- > 0) {
    const tag = MESH_AD_TAGS[Math.floor(Math.random() * MESH_AD_TAGS.length)]!;
    const cat = tag.split(":")[0]!;
    if (usedCats.has(cat)) continue;
    usedCats.add(cat);
    out.push(tag);
  }
  return out;
}

function MeshAutoform() {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const [feed, setFeed] = useState<readonly AutoMeshFeedLine[]>([]);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    const NODE_FONT_PX = 18;
    const LABEL_FONT_PX = 10;
    const EDGE_FONT_PX = 11;
    const RAIN_FONT_PX = 11;

    let W = 0;
    let H = 0;
    let layout: AutoMeshLayout = autoMeshLayout(640);
    let nodes: AutoMeshNode[] = [];
    let daemons: AutoMeshDaemon[] = [];
    let rain: AutoMeshRainCol[] = [];
    let lastT = performance.now();
    let driftTimer = 0;
    let migrateTimer = 1.4;
    let advertiseTimer = 0;
    let rafId = 0;
    let feedCounter = 0;

    const pushFeed = (
      prefix: string,
      prefixColor: string,
      body: React.ReactNode,
    ): void => {
      feedCounter += 1;
      const line: AutoMeshFeedLine = {
        id: feedCounter,
        ts: autoMeshTs(),
        prefix,
        prefixColor,
        body,
      };
      setFeed((prev) => [...prev, line].slice(-AUTOMESH_FEED_MAX));
    };

    const spawnFromEdge = (): { x: number; y: number } => {
      const edge = Math.floor(Math.random() * 4);
      if (edge === 0) return { x: -60, y: Math.random() * H };
      if (edge === 1) return { x: W + 60, y: Math.random() * H };
      if (edge === 2) return { x: Math.random() * W, y: -60 };
      return { x: Math.random() * W, y: H + 60 };
    };

    const newTarget = (): { x: number; y: number } => ({
      x: W * 0.1 + Math.random() * W * 0.8,
      y: H * 0.16 + Math.random() * H * 0.68,
    });

    const initNodes = (): void => {
      nodes = [];
      for (let i = 0; i < layout.count; i++) {
        const start = spawnFromEdge();
        const target = newTarget();
        nodes.push({
          id: i,
          hex: autoMeshHex4(),
          x: start.x,
          y: start.y,
          tx: target.x,
          ty: target.y,
          vx: 0,
          vy: 0,
          cap: pickCapability(),
          spawnDelay: i * 100 + Math.random() * 60,
          age: 0,
          emitted: false,
        });
      }
    };

    const initDaemons = (): void => {
      daemons = [];
      for (let i = 0; i < layout.daemons; i++) {
        daemons.push({
          id: i,
          hex: autoMeshHex4().slice(0, 2),
          hostIdx: Math.floor(Math.random() * Math.max(1, nodes.length)),
          migrating: false,
          fromIdx: 0,
          toIdx: 0,
          migrateT: 0,
          trail: [],
        });
      }
    };

    const initRain = (): void => {
      rain = [];
      const cols = layout.rainCols;
      for (let i = 0; i < cols; i++) {
        rain.push({
          x: (i + 0.5) * (W / cols) + (Math.random() - 0.5) * 14,
          y: -Math.random() * H,
          speed: 0.25 + Math.random() * 0.7,
          gap: 15 + Math.random() * 8,
          tokens: Array.from({ length: 22 }, pickMatrixGlyph),
          tick: 0,
        });
      }
    };

    const resize = (): void => {
      const rect = canvas.getBoundingClientRect();
      W = rect.width;
      H = rect.height;
      canvas.width = Math.max(1, Math.floor(W * dpr));
      canvas.height = Math.max(1, Math.floor(H * dpr));
      ctx.setTransform(1, 0, 0, 1, 0, 0);
      ctx.scale(dpr, dpr);
      const nextLayout = autoMeshLayout(W);
      const needFullInit =
        nextLayout.count !== layout.count || nodes.length === 0;
      layout = nextLayout;
      if (needFullInit) {
        initNodes();
        initDaemons();
        initRain();
      } else {
        for (const n of nodes) {
          const t = newTarget();
          n.tx = t.x;
          n.ty = t.y;
        }
        initRain();
      }
    };

    const frame = (): void => {
      const now = performance.now();
      const dt = Math.min(0.05, (now - lastT) / 1000);
      lastT = now;

      // ---- update physics + feed events ----
      for (const n of nodes) {
        n.age += dt * 1000;
        if (n.age < n.spawnDelay) continue;
        if (!n.emitted) {
          n.emitted = true;
          pushFeed(
            "↑",
            "rgb(196, 255, 61)",
            <>
              node.0x{n.hex}{" "}
              <span style={{ color: `rgb(${capabilityRgb(n.cap)})` }}>
                [{n.cap}]
              </span>{" "}
              joined mesh
            </>,
          );
        }
        const dx = n.tx - n.x;
        const dy = n.ty - n.y;
        n.vx += dx * 3.2 * dt;
        n.vy += dy * 3.2 * dt;
        n.vx *= Math.max(0, 1 - 2.4 * dt);
        n.vy *= Math.max(0, 1 - 2.4 * dt);
        n.x += n.vx * dt;
        n.y += n.vy * dt;
      }

      // drift correction
      driftTimer += dt;
      if (driftTimer > 5.5) {
        driftTimer = 0;
        const k = 2 + Math.floor(Math.random() * 2);
        const touched: string[] = [];
        for (let i = 0; i < k; i++) {
          const idx = Math.floor(Math.random() * nodes.length);
          const n = nodes[idx];
          if (n && n.age >= n.spawnDelay) {
            const t = newTarget();
            n.tx = t.x;
            n.ty = t.y;
            touched.push(`0x${n.hex}`);
          }
        }
        if (touched.length > 0) {
          pushFeed(
            "⤴",
            "rgb(212, 220, 208)",
            <>
              drift_correct nodes({touched.length}){" "}
              <span className="text-ink-faint">{touched.join(" ")}</span> reflow
            </>,
          );
        }
      }

      // mikoshi hop
      migrateTimer += dt;
      if (migrateTimer > 2.2) {
        migrateTimer = 0;
        const idle = daemons.filter((d) => !d.migrating);
        if (idle.length > 0 && nodes.length > 1) {
          const d = idle[Math.floor(Math.random() * idle.length)]!;
          let target = -1;
          const gravityPull = Math.random() < 0.6;
          if (gravityPull) {
            const dataNodes: number[] = [];
            for (let i = 0; i < nodes.length; i++) {
              const n = nodes[i]!;
              if (n.cap === "datafort" && i !== d.hostIdx) dataNodes.push(i);
            }
            if (dataNodes.length > 0) {
              target = dataNodes[Math.floor(Math.random() * dataNodes.length)]!;
            }
          }
          if (target < 0) {
            target = Math.floor(Math.random() * nodes.length);
            let safety = 8;
            while (target === d.hostIdx && safety-- > 0) {
              target = Math.floor(Math.random() * nodes.length);
            }
          }
          d.fromIdx = d.hostIdx;
          d.toIdx = target;
          d.migrating = true;
          d.migrateT = 0;
          d.trail = [];
          const fromHex = nodes[d.fromIdx]?.hex ?? "????";
          const toHex = nodes[d.toIdx]?.hex ?? "????";
          const toCap = nodes[d.toIdx]?.cap ?? "daemon";
          const isGravity = toCap === "datafort";
          pushFeed(
            isGravity ? "⤵" : "↗",
            "rgb(61, 240, 255)",
            <>
              {isGravity ? "gravity_pull " : "mikoshi      "}
              daemon.0x{d.hex}
              {"  "}0x{fromHex} → 0x{toHex}{" "}
              <span style={{ color: `rgb(${capabilityRgb(toCap)})` }}>
                [{toCap}]
              </span>
            </>,
          );
        }
      }

      // capability advertisements — periodic flair
      advertiseTimer += dt;
      if (advertiseTimer > 2.6) {
        advertiseTimer = 0;
        const live = nodes.filter((n) => n.age >= n.spawnDelay);
        if (live.length > 0) {
          const n = live[Math.floor(Math.random() * live.length)]!;
          const tags = pickAdTags();
          pushFeed(
            "▸",
            "rgb(107, 138, 30)",
            <>
              cap.announce 0x{n.hex}{" "}
              <span style={{ color: `rgb(${capabilityRgb(n.cap)})` }}>
                [{n.cap}]
              </span>{" "}
              <span className="text-accent-dim">{tags.join(" ")}</span>{" "}
              <span className="text-ink-faint">
                capb ann {Math.floor(12 + Math.random() * 39)}ns
              </span>
            </>,
          );
        }
      }

      // advance migrating daemons
      for (const d of daemons) {
        if (!d.migrating) continue;
        d.migrateT += dt / 1.5;
        if (d.migrateT >= 1) {
          d.migrating = false;
          d.hostIdx = d.toIdx;
          d.migrateT = 0;
        }
      }

      // ============== RENDER ==============
      // matrix-style trail-decay fade
      ctx.fillStyle = "rgba(10, 12, 10, 0.32)";
      ctx.fillRect(0, 0, W, H);

      // ---- background matrix rain (very faint) ----
      ctx.textAlign = "center";
      ctx.textBaseline = "top";
      ctx.font = `${RAIN_FONT_PX}px ${AUTOMESH_MONO_FONT}`;
      for (const c of rain) {
        c.y += c.speed;
        c.tick++;
        if (c.tick % 7 === 0) {
          c.tokens[Math.floor(Math.random() * c.tokens.length)] =
            pickMatrixGlyph();
        }
        const trailLen = 16;
        for (let i = 0; i < trailLen; i++) {
          const drawY = c.y - i * c.gap;
          if (drawY < -RAIN_FONT_PX || drawY > H) continue;
          const headIdx = Math.floor(c.y / c.gap);
          const idx = headIdx - i;
          if (idx < 0) continue;
          const tok = c.tokens[idx % c.tokens.length] ?? "·";
          if (i === 0) {
            ctx.fillStyle = "rgba(220, 255, 200, 0.30)";
          } else {
            const a = Math.max(0, 1 - i / trailLen);
            ctx.fillStyle = `rgba(196, 255, 61, ${a * 0.12})`;
          }
          ctx.fillText(tok, c.x, drawY);
        }
        if (c.y - trailLen * c.gap > H + 40) {
          c.y = -Math.random() * H * 0.5;
          c.speed = 0.25 + Math.random() * 0.7;
        }
      }

      // ---- gravity wells (concentric character rings) ----
      ctx.textBaseline = "middle";
      ctx.textAlign = "center";
      const wellT = now / 1000;
      ctx.font = `${EDGE_FONT_PX}px ${AUTOMESH_MONO_FONT}`;
      for (const n of nodes) {
        if (n.cap !== "datafort" || n.age < n.spawnDelay) continue;
        for (let k = 0; k < 3; k++) {
          const phase = (wellT * 0.4 + n.id * 0.21 + k * 0.33) % 1;
          const r = 16 + phase * 64;
          const a = 0.45 * (1 - phase);
          const num = Math.max(14, Math.floor(r / 5));
          ctx.fillStyle = `rgba(196, 255, 61, ${a})`;
          for (let i = 0; i < num; i++) {
            const ang = (i / num) * Math.PI * 2 + wellT * 0.2;
            const px = n.x + Math.cos(ang) * r;
            const py = n.y + Math.sin(ang) * r;
            ctx.fillText("·", px, py);
          }
        }
      }

      // ---- edges as flowing dot-streams with packet glyph ----
      const edgeFlow = (now / 1000) * 0.35;
      for (let i = 0; i < nodes.length; i++) {
        const a = nodes[i]!;
        if (a.age < a.spawnDelay) continue;
        for (let j = i + 1; j < nodes.length; j++) {
          const b = nodes[j]!;
          if (b.age < b.spawnDelay) continue;
          const dx = b.x - a.x;
          const dy = b.y - a.y;
          const dist = Math.sqrt(dx * dx + dy * dy);
          if (dist > layout.edgeRadius) continue;
          const opacity = 1 - dist / layout.edgeRadius;
          const stepLen = 7;
          const steps = Math.max(2, Math.floor(dist / stepLen));
          ctx.fillStyle = `rgba(196, 255, 61, ${opacity * 0.18})`;
          for (let s = 1; s < steps; s++) {
            const t = s / steps;
            const px = a.x + dx * t;
            const py = a.y + dy * t;
            ctx.fillText("·", px, py);
          }
          // moving packet glyph
          const packetT = (((edgeFlow + i * 0.13 + j * 0.07) % 1) + 1) % 1;
          const pkx = a.x + dx * packetT;
          const pky = a.y + dy * packetT;
          ctx.fillStyle = `rgba(196, 255, 61, ${opacity * 0.9})`;
          ctx.fillText("▸", pkx, pky);
        }
      }

      // ---- nodes as glyphs ----
      for (const n of nodes) {
        if (n.age < n.spawnDelay) continue;
        const rgb = capabilityRgb(n.cap);
        const sinceSpawn = n.age - n.spawnDelay;
        const fade = Math.min(1, sinceSpawn / 600);
        const glyph = nodeGlyph(n.cap);
        // halo glow
        ctx.fillStyle = `rgba(${rgb}, ${0.22 * fade})`;
        ctx.font = `${NODE_FONT_PX + 8}px ${AUTOMESH_MONO_FONT}`;
        ctx.fillText(glyph, n.x, n.y);
        // core glyph
        ctx.fillStyle = `rgba(${rgb}, ${fade})`;
        ctx.font = `bold ${NODE_FONT_PX}px ${AUTOMESH_MONO_FONT}`;
        ctx.fillText(glyph, n.x, n.y);
        // hex tag below
        ctx.fillStyle = `rgba(${rgb}, ${0.75 * fade})`;
        ctx.font = `${LABEL_FONT_PX}px ${AUTOMESH_MONO_FONT}`;
        ctx.fillText(`0x${n.hex}`, n.x, n.y + 16);
      }

      // ---- daemons + migration char-trails ----
      // First pass: advance positions, push new trail dots, age trails,
      // bucket sitters by host so co-resident daemons render as one
      // glyph with a "+"-joined label instead of overlapping.
      const sittingByHost = new Map<number, AutoMeshDaemon[]>();
      const migratingPositions: Array<{
        d: AutoMeshDaemon;
        x: number;
        y: number;
      }> = [];
      for (const d of daemons) {
        const from = nodes[d.fromIdx];
        const to = nodes[d.toIdx];
        const host = nodes[d.hostIdx];
        if (!host) continue;
        if (d.migrating && from && to) {
          const tt = easeInOut(d.migrateT);
          const arcLift = Math.sin(tt * Math.PI) * 14;
          const x = from.x + (to.x - from.x) * tt;
          const y = from.y + (to.y - from.y) * tt - arcLift;
          if (Math.random() < 0.55) {
            d.trail.push({
              x,
              y,
              age: 0,
              ch: Math.random() < 0.5 ? "▒" : "░",
            });
          }
          migratingPositions.push({ d, x, y });
        } else {
          const list = sittingByHost.get(d.hostIdx);
          if (list) list.push(d);
          else sittingByHost.set(d.hostIdx, [d]);
        }
        // age + draw trail (trails may linger after arrival)
        ctx.font = `${EDGE_FONT_PX}px ${AUTOMESH_MONO_FONT}`;
        for (const t of d.trail) {
          t.age += dt;
          const a = Math.max(0, 0.75 - t.age * 2.4);
          if (a <= 0) continue;
          ctx.fillStyle = `rgba(61, 240, 255, ${a})`;
          ctx.fillText(t.ch, t.x, t.y);
        }
        d.trail = d.trail.filter((t) => t.age < 0.5);
      }

      // Migrating daemons render individually (positions diverge).
      for (const { d, x, y } of migratingPositions) {
        ctx.fillStyle = "rgba(61, 240, 255, 0.45)";
        ctx.font = `bold 22px ${AUTOMESH_MONO_FONT}`;
        ctx.fillText("◆", x, y);
        ctx.fillStyle = "rgba(255, 255, 255, 0.98)";
        ctx.font = `bold 15px ${AUTOMESH_MONO_FONT}`;
        ctx.fillText("◆", x, y);
        ctx.fillStyle = "rgba(61, 240, 255, 0.7)";
        ctx.font = `${LABEL_FONT_PX - 1}px ${AUTOMESH_MONO_FONT}`;
        ctx.fillText(`d.${d.hex}`, x, y - 14);
      }

      // Sitters: one glyph per host, labels joined with "+".
      sittingByHost.forEach((group, hostIdx) => {
        const host = nodes[hostIdx];
        if (!host) return;
        const x = host.x;
        const y = host.y - 18;
        // halo intensifies subtly when stacked
        const stackBoost = Math.min(0.25, (group.length - 1) * 0.12);
        ctx.fillStyle = `rgba(61, 240, 255, ${0.45 + stackBoost})`;
        ctx.font = `bold 22px ${AUTOMESH_MONO_FONT}`;
        ctx.fillText("◆", x, y);
        ctx.fillStyle = "rgba(255, 255, 255, 0.98)";
        ctx.font = `bold 15px ${AUTOMESH_MONO_FONT}`;
        ctx.fillText("◆", x, y);
        const parts = group.map((d) => `d.${d.hex}`);
        const label =
          parts.length <= 3
            ? parts.join("+")
            : `${parts.slice(0, 2).join("+")}+${parts.length - 2}`;
        ctx.fillStyle = "rgba(61, 240, 255, 0.7)";
        ctx.font = `${LABEL_FONT_PX - 1}px ${AUTOMESH_MONO_FONT}`;
        ctx.fillText(label, x, y - 14);
      });

      rafId = requestAnimationFrame(frame);
    };

    const onResize = (): void => resize();

    resize();
    ctx.fillStyle = "#0a0c0a";
    ctx.fillRect(0, 0, W, H);
    rafId = requestAnimationFrame(frame);
    window.addEventListener("resize", onResize);
    return () => {
      cancelAnimationFrame(rafId);
      window.removeEventListener("resize", onResize);
    };
  }, []);

  return (
    <div className="relative border border-line bg-bg overflow-hidden font-mono">
      <div className="flex items-center justify-between border-b border-line px-4 py-2 text-[10px] tracking-[0.14em] text-ink-dim uppercase">
        <span className="flex items-center gap-3">
          <span className="inline-flex gap-1">
            <span className="frame-dot-r w-[7px] h-[7px] rounded-full" />
            <span className="frame-dot-y w-[7px] h-[7px] rounded-full" />
            <span className="frame-dot-g w-[7px] h-[7px] rounded-full" />
          </span>
          <span className="text-accent">$</span>
          <span>
            net meshos autoform{" "}
            <span className="text-ink-faint normal-case tracking-normal">
              --live --mesh=local --autoform=true
            </span>
          </span>
        </span>
        <span className="flex items-center gap-1.5 normal-case tracking-normal">
          <span className="w-1.5 h-1.5 rounded-full bg-accent inline-block animate-pulse-dot" />
          <span className="text-accent">forming</span>
        </span>
      </div>

      <div className="relative bg-bg">
        <canvas
          ref={canvasRef}
          className="block w-full h-[380px] md:h-[460px] lg:h-[520px] bg-bg"
          aria-hidden
        />
        {/* CRT scanlines overlay */}
        <div
          className="absolute inset-0 pointer-events-none"
          style={{
            backgroundImage:
              "repeating-linear-gradient(0deg, rgba(0,0,0,0) 0px, rgba(0,0,0,0) 2px, rgba(0,0,0,0.22) 3px, rgba(0,0,0,0.22) 3px)",
            mixBlendMode: "multiply",
          }}
          aria-hidden
        />
        {/* phosphor vignette */}
        <div
          className="absolute inset-0 pointer-events-none"
          style={{
            background:
              "radial-gradient(ellipse at center, transparent 55%, rgba(0,0,0,0.4) 100%)",
          }}
          aria-hidden
        />
      </div>

      {/* terminal event feed */}
      <div className="border-t border-line bg-bg-2 px-4 py-3 text-[11px] leading-[1.75] min-h-[170px]">
        <div className="text-ink-faint text-[10px] tracking-[0.12em] uppercase mb-1.5 flex items-center justify-between">
          <span>
            <span className="text-accent">▸</span> mesh.events
          </span>
          <span className="normal-case tracking-normal">
            tail -f autoform.log
          </span>
        </div>
        <div>
          {feed.length === 0 ? (
            <div className="text-ink-faint">
              <span className="text-accent animate-pulse-dot">█</span> awaiting
              nodes...
            </div>
          ) : null}
          {feed.map((line) => (
            <div
              key={line.id}
              className="event-line-in flex items-baseline gap-3 whitespace-nowrap overflow-hidden"
            >
              <span className="text-ink-faint" style={{ minWidth: "9ch" }}>
                {line.ts}
              </span>
              <span style={{ color: line.prefixColor, minWidth: "2ch" }}>
                {line.prefix}
              </span>
              <span className="text-ink flex-1 truncate">{line.body}</span>
            </div>
          ))}
        </div>
      </div>

      <div className="flex flex-wrap gap-x-5 gap-y-2 border-t border-line px-4 py-3 text-[10px] tracking-[0.12em] text-ink-dim uppercase">
        <span className="flex items-center gap-2">
          <span className="font-mono" style={{ color: "rgb(196, 255, 61)" }}>
            ◈
          </span>
          device
        </span>
        <span className="flex items-center gap-2">
          <span className="font-mono" style={{ color: "rgb(107, 138, 30)" }}>
            ▣
          </span>
          compute
        </span>
        <span className="flex items-center gap-2">
          <span className="font-mono" style={{ color: "rgb(61, 240, 255)" }}>
            ◉
          </span>
          region
        </span>
        <span className="flex items-center gap-2">
          <span className="font-mono" style={{ color: "rgb(212, 220, 208)" }}>
            ◇
          </span>
          daemon
        </span>
        <span className="flex items-center gap-2">
          <span className="font-mono text-ink">■</span> datafort · gravity well
        </span>
        <span className="flex items-center gap-2 sm:ml-auto">
          <span className="font-mono" style={{ color: "rgb(61, 240, 255)" }}>
            ◆
          </span>
          mikoshi · in transit
        </span>
      </div>
    </div>
  );
}

const MESH_OS_CAPABILITY_STRIP: ReadonlyArray<{
  num: string;
  name: string;
  body: string;
  isNew?: boolean;
}> = [
  {
    num: "mesh.os.1",
    name: "Mikoshi Lifecycle",
    body: "spawn, migrate, supervise. daemons hop between machines without losing state, history, or place in the conversation.",
  },
  {
    num: "mesh.os.2",
    name: "Gravity Placement",
    body: "workloads pull toward their data. compute lands near the bytes it touches — gravity-based scoring, not central scheduling.",
  },
  {
    num: "mesh.os.3",
    name: "Daemon Supervision",
    body: "start, drain, restart, gate. exponential backoff. backpressure signals. graceful shutdown or forced.",
  },
  {
    num: "mesh.os.4",
    name: "Capability Match",
    body: "nodes advertise what they are — device, compute, region, daemon, datafort. MeshOS routes daemons to nodes that fit.",
  },
];

function MeshOsSection() {
  return (
    <section
      id="meshos"
      className="relative overflow-hidden border-b border-line px-6 py-20"
    >
      <SectionLabel>§08 / cluster os // new</SectionLabel>
      <DisplayHeading>
        MeshOS:
        <br />
        <span className="text-accent">
          programs move.
          <br />
          clusters think.
        </span>
      </DisplayHeading>

      <p className="text-[16px] text-ink max-w-[740px] leading-[1.7] font-light mb-12">
        Programs move between machines without stopping. Daemons migrate
        seamlessly across the mesh while maintaining full state.
        <br />
        <br />
        <strong className="text-accent font-medium">
          Placement happens intelligently —
        </strong>{" "}
        gravity pulls workloads toward their data, capabilities match tasks to
        nodes, and drift detection triggers automatic rebalancing. No central
        orchestrator. No single point of failure. Just self-organizing
        coordination at nanosecond scale.
      </p>

      <MeshAutoform />

      <div className="border-l-2 border-accent pl-8 pr-8 py-6 bg-accent/[0.02] my-12 max-w-[900px]">
        <p className="text-[18px] text-ink leading-[1.5] font-light">
          The cluster thinks.{" "}
          <strong className="text-accent font-medium">The daemons move.</strong>{" "}
          The work gets done.
        </p>
      </div>

      <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 border-t border-l border-line">
        {MESH_OS_CAPABILITY_STRIP.map((c) => (
          <div
            key={c.name}
            className="border-r border-b border-line bg-bg-2/40 p-5"
          >
            <div className="flex items-baseline justify-between mb-2">
              <span className="font-mono text-[10px] text-accent tracking-[0.14em]">
                ▸ {c.num}
              </span>
              {c.isNew ? (
                <span className="bg-accent text-bg px-1.5 py-0.5 text-[9px] font-bold tracking-[0.18em]">
                  NEW
                </span>
              ) : null}
            </div>
            <h3 className="font-head text-[16px] leading-tight text-ink mb-2 tracking-[0.04em] lowercase">
              {c.name}
            </h3>
            <p className="text-[11px] text-ink-dim leading-[1.55]">{c.body}</p>
          </div>
        ))}
      </div>
    </section>
  );
}

function InstallSection() {
  const [copied, setCopied] = useState<string | null>(null);

  const handleCopy = async (lang: string, text: string): Promise<void> => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(lang);
      window.setTimeout(() => {
        setCopied((current) => (current === lang ? null : current));
      }, 1800);
    } catch {
      // clipboard API can fail in insecure contexts; ignore silently
    }
  };

  return (
    <section id="install" className="bg-bg-2 border-b border-line px-6 py-20">
      <SectionLabel>§10 / install</SectionLabel>
      <DisplayHeading>
        five languages.
        <br />
        one engine.
      </DisplayHeading>

      <p className="text-[16px] text-ink max-w-[740px] leading-[1.6] font-light mb-12">
        All SDKs wrap the same Rust core. The SDK is the developer experience,
        the engine is Rust.
      </p>

      <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-4 mt-5">
        {INSTALL_CARDS.map((c) => {
          const isCopied = copied === c.lang;
          return (
            <button
              key={c.lang}
              type="button"
              onClick={() => handleCopy(c.lang, c.copy)}
              aria-label={`Copy ${c.lang} install command`}
              className="text-left border border-line p-5 bg-bg transition-colors hover:border-accent-dim cursor-pointer focus:outline-none focus:border-accent"
            >
              <div className="flex items-center justify-between mb-4">
                <span className="text-[11px] text-ink tracking-[0.15em] uppercase font-semibold">
                  {c.lang}
                </span>
                <span
                  className={`text-[10px] px-1.5 py-0.5 transition-colors ${
                    isCopied
                      ? "text-bg bg-accent border border-accent font-semibold"
                      : "text-accent border border-accent-dim"
                  }`}
                >
                  {isCopied ? "✓ COPIED" : c.ext}
                </span>
              </div>
              <pre className="bg-bg-2 p-3 text-[11px] text-accent border-l-2 border-accent overflow-x-auto font-mono leading-[1.5]">
                {c.cmd}
              </pre>
              <div className="text-ink-dim text-[10px] mt-2.5">{c.meta}</div>
            </button>
          );
        })}
      </div>

      <p className="mt-7 text-[11px] text-ink-dim">
        <a
          href="https://github.com/ai-2070/net/tree/master/net/crates/net/include"
          target="_blank"
          rel="noopener noreferrer"
          className="hover:text-ink transition-colors"
        >
          // C bindings via <span className="text-accent">net.h</span>
        </a>{" "}
        — build cdylib with{" "}
        <span className="relative inline-block">
          <button
            type="button"
            onClick={() =>
              handleCopy(
                "ffi-build",
                "cargo build --release --features net,ffi,redex,cortex,netdb,redis,jetstream",
              )
            }
            aria-label="Copy cargo build command"
            className="text-accent font-mono cursor-pointer transition-colors hover:text-ink focus:outline-none focus:text-ink"
          >
            cargo build --release --features
            net,ffi,redex,cortex,netdb,redis,jetstream
          </button>
          {copied === "ffi-build" ? (
            <span
              aria-hidden
              className="slide-up-fade absolute left-0 -top-1 text-[10px] text-accent font-mono whitespace-nowrap"
            >
              ✓ copied
            </span>
          ) : null}
        </span>
        . Lower-level bindings (skip SDK ergonomics, talk directly to the
        engine): <span className="text-accent">ai2070-net</span>,{" "}
        <span className="text-accent">@ai2070/net</span>,{" "}
        <span className="text-accent">ai2070-net</span> (PyPI binding).
      </p>
    </section>
  );
}

interface AppCard {
  tag: string;
  title: string;
  body: string;
}

const APPS: readonly AppCard[] = [
  {
    tag: "▸ 0x01 ─ ai agents",
    title: "AI Agents",
    body: "Tool calls, state, and memory transfer between heterogeneous GPU nodes. Token streams flow through the mesh; an agent's working memory follows it from node to node mid-conversation. The mesh is the runtime.",
  },
  {
    tag: "▸ 0x02 ─ vehicular mesh",
    title: "Vehicular Sensor Mesh",
    body: "Cars sharing LIDAR, radar, camera. Vehicles sync intent — braking, turning, route changes. The car behind doesn't react to braking. It knows about the braking before the brake pads touch the rotor.",
  },
  {
    tag: "▸ 0x03 ─ factory floor",
    title: "Robotics Factory Floor",
    body: "Robots don't need line-of-sight for networking. The mesh routes through whatever nodes are reachable. Reroute scheduled in sub-microsecond time. The assembly line doesn't stop.",
  },
  {
    tag: "▸ 0x04 ─ energy grids & extraction",
    title: "Energy Grids & Extraction",
    body: "Electrical substations, oil and gas pipelines, drilling rigs, mine haul trucks, distributed solar — coordinating in real time across geographies that fiber doesn't reach. Protective relays trip in single-digit milliseconds; the mesh isolates faults before they cascade. Routes through whatever radios and edge boxes survive.",
  },
  {
    tag: "▸ 0x05 ─ remote surgery",
    title: "Remote Surgery",
    body: "Control signals and haptic feedback routed across the mesh. If the primary compute node lags, the mesh reroutes mid-operation. The surgeon doesn't notice. The patient doesn't notice. The scalpel doesn't stop.",
  },
  {
    tag: "▸ 0x06 ─ drone swarms",
    title: "Drone Swarms",
    body: "Coordinated flight without a ground controller. A drone that loses a motor broadcasts the failure; the swarm adjusts formation before the drone has begun to fall.",
  },
  {
    tag: "▸ 0x07 ─ live performance",
    title: "Live Performance",
    body: "Lighting, audio, video, pyro synchronized across hundreds of nodes. A DMX controller dies, another node picks up the cue list. Audio sync tighter than the speed of sound across the venue.",
  },
  {
    tag: "▸ 0x08 ─ medical nanorobotics",
    title: "Medical Nanorobotics",
    body: "Swarms of nanoscale machines coordinating in vivo — drug-delivery vectors, targeted ablation, vascular monitoring. Sub-microsecond reroute when a node leaves the swarm. No cloud round-trip; the patient is the network.",
  },
];

function ApplicationsSection() {
  return (
    <section id="apps" className="border-b border-line px-6 py-20">
      <SectionLabel>§11 / target applications</SectionLabel>
      <DisplayHeading>
        everything that
        <br />
        can&apos;t wait.
      </DisplayHeading>

      <p className="text-[16px] text-ink max-w-[740px] leading-[1.6] font-light mb-12">
        Anywhere latency matters. Anywhere the cloud round-trip is too slow.
        Anywhere there&apos;s no central infrastructure to route through.
      </p>

      <div className="grid grid-cols-1 md:grid-cols-2 border-t border-l border-line">
        {APPS.map((a) => (
          <div
            key={a.title}
            className="border-r border-b border-line p-7 transition-colors hover:bg-bg-2 relative"
          >
            <div className="text-accent text-[10px] tracking-[0.15em] mb-2">
              {a.tag}
            </div>
            <h3 className="font-head text-[20px] leading-tight mb-2.5 tracking-[0.04em] text-ink lowercase">
              {a.title}
            </h3>
            <p className="text-ink-dim text-[12px] leading-[1.6]">{a.body}</p>
          </div>
        ))}
      </div>
    </section>
  );
}

interface BlackwallItem {
  tag: string;
  body: string;
}

const BLACKWALL_ITEMS: readonly BlackwallItem[] = [
  {
    tag: "▸ Backpressure",
    body: "Nodes limit in-flight events, prevent overload, and apply pushback by going silent. No node can be forced to accept more than it can process.",
  },
  {
    tag: "▸ Bounded queues",
    body: "No infinite buffers. Ring buffers have explicit capacity limits. A flood fills a buffer and gets evicted, it doesn't grow the buffer.",
  },
  {
    tag: "▸ Fanout limits",
    body: "Events don't propagate to everyone. Dissemination is controlled by the proximity graph and routing table. Prevents O(n²) explosion.",
  },
  {
    tag: "▸ Deduplication",
    body: "The same event doesn't explode repeatedly. Idempotency at the event level protects against loops and amplification.",
  },
  {
    tag: "▸ TTL limits",
    body: "Events expire. Pingwaves have a hop radius. A misbehaving node's traffic dies at the boundary of its TTL, not the edge of the mesh.",
  },
  {
    tag: "▸ Rate limits",
    body: "Per-node, per-peer limits. One node cannot flood the mesh. Its neighbors enforce their own limits independently through device autonomy rules.",
  },
];

function BlackwallViz() {
  return (
    <div
      className="relative w-full h-[220px] md:h-[280px] border border-line bg-black overflow-hidden mb-12"
      aria-hidden
    >
      <div className="absolute inset-0 blackwall-stripes-thick" />
      <div className="absolute inset-0 blackwall-stripes-thin" />
      <div className="absolute inset-0 blackwall-stripes-cyan pointer-events-none" />
      <div className="absolute inset-y-0 right-0 w-1/2 blackwall-burst pointer-events-none" />
      <div className="absolute inset-0 blackwall-scan pointer-events-none" />
      <div className="absolute inset-0 grid place-items-center pointer-events-none">
        <div
          className="font-display text-accent text-[clamp(20px,4vw,42px)] tracking-[0.32em] opacity-70"
          style={{ textShadow: "0 0 18px rgba(196,255,61,0.55)" }}
        >
          BLACKWALL
        </div>
      </div>
    </div>
  );
}

function BlackwallSection() {
  return (
    <section id="wall" className="blackwall-bg border-b border-line px-6 py-20">
      <SectionLabel>§12 / the blackwall</SectionLabel>
      <DisplayHeading>
        safety isn&apos;t declared.
        <br />
        it&apos;s <span className="text-accent">derived.</span>
      </DisplayHeading>

      <BlackwallViz />

      <p className="text-[16px] text-ink max-w-[740px] leading-[1.6] font-light mb-12">
        In Cyberpunk, the Blackwall isn&apos;t a wall around the threats —
        it&apos;s a wall around the safe zone. Net works the same way. The
        &quot;safe mesh&quot; is the part you can observe: nodes that respond
        within heartbeat intervals, honor their capability announcements,
        don&apos;t flood, respect TTL.
      </p>

      <p className="text-[16px] text-accent max-w-[740px] leading-[1.6] font-light -mt-8 mb-12">
        The wall isn&apos;t one mechanism. It&apos;s the emergent effect of
        every constraint working together.
      </p>

      <div className="grid grid-cols-1 md:grid-cols-3 gap-8 mt-10">
        {BLACKWALL_ITEMS.map((item) => (
          <div key={item.tag} className="border-t border-accent-dim pt-4">
            <h4 className="text-[11px] text-accent uppercase tracking-[0.15em] mb-2.5">
              {item.tag}
            </h4>
            <p className="text-ink-dim text-[12px] leading-[1.6]">
              {item.body}
            </p>
          </div>
        ))}
      </div>

      <div className="border-l-2 border-accent pl-8 pr-8 py-6 bg-accent/[0.02] mt-16 max-w-[900px]">
        <p className="text-[18px] text-ink leading-[1.5] font-light">
          Any single mechanism can be overwhelmed. All of them together form the
          wall.{" "}
          <strong className="text-accent font-medium">
            No single point to breach because the Blackwall is the mesh itself.
          </strong>
        </p>
      </div>
    </section>
  );
}

function formatReleaseDate(iso: string): string {
  if (!iso) return "—";
  return iso.slice(0, 10).replace(/-/g, ".");
}

function ReleasesSection() {
  const { releases } = useRepoInfo();
  if (releases.length === 0) return null;

  return (
    <section id="releases" className="border-b border-line px-6 py-20">
      <SectionLabel>§13 / releases</SectionLabel>
      <DisplayHeading>net releases.</DisplayHeading>

      <p className="text-[16px] text-ink max-w-[740px] leading-[1.6] font-light mb-12">
        Every tagged release pulled directly from{" "}
        <a
          href="https://github.com/ai-2070/net/releases"
          target="_blank"
          rel="noopener noreferrer"
          className="text-accent hover:text-ink transition-colors"
        >
          ai-2070/net
        </a>
        .
      </p>

      <div className="border border-line bg-bg-2 max-h-[640px] overflow-y-auto">
        {releases.map((r, i) => (
          <article
            key={r.tag}
            className={
              i % 2 ? "px-6 py-6 border-t border-line bg-black" : "px-6 py-6"
            }
          >
            <header className="flex items-baseline justify-between gap-4 mb-3 flex-wrap">
              <div className="flex items-baseline gap-3 flex-wrap">
                <span className="font-mono text-[16px] text-accent font-semibold">
                  {r.tag}
                </span>
                {r.codename ? (
                  <span className="font-mono text-[14px] text-ink uppercase tracking-[0.12em]">
                    <span className="text-ink-faint">Codename:</span> &ldquo;
                    <span className="font-semibold">{r.codename}</span>&rdquo;
                  </span>
                ) : null}
                {r.prerelease ? (
                  <span className="text-[10px] text-warn uppercase tracking-[0.15em] border border-warn px-1.5 py-0.5">
                    pre-release
                  </span>
                ) : null}
              </div>
              <a
                href={r.htmlUrl}
                target="_blank"
                rel="noopener noreferrer"
                className="text-[10px] text-ink-dim font-mono tracking-[0.05em] hover:text-accent transition-colors"
              >
                {formatReleaseDate(r.publishedAt)} ↗
              </a>
            </header>
            {r.bodyHtml ? (
              <div
                className="prose prose-invert prose-sm max-w-none prose-headings:text-ink prose-headings:font-semibold prose-headings:tracking-tight prose-h1:text-[18px] prose-h2:text-[15px] prose-h3:text-[13px] prose-p:text-ink-dim prose-strong:text-ink prose-strong:font-medium prose-a:text-accent prose-a:no-underline hover:prose-a:underline prose-code:text-accent prose-code:font-mono prose-code:before:content-none prose-code:after:content-none prose-code:bg-bg prose-code:px-1 prose-code:py-0.5 prose-pre:bg-bg prose-pre:border prose-pre:border-line prose-ul:list-[square] prose-li:text-ink-dim prose-ul:marker:text-line prose-ol:text-ink-dim prose-hr:border-line prose-code:rounded-none"
                // Trusted: bodies come from repo maintainers' release notes via GitHub API.
                dangerouslySetInnerHTML={{ __html: r.bodyHtml }}
              />
            ) : (
              <p className="font-mono text-[12px] text-ink-faint italic">
                no notes
              </p>
            )}
          </article>
        ))}
      </div>
    </section>
  );
}

function ClosingSection() {
  return (
    <section id="post-cloud" className="border-b border-line px-6 py-20">
      <SectionLabel>§14 / post-cloud</SectionLabel>
      <DisplayHeading>
        not anti-cloud.
        <br />
        <span className="text-accent">post-cloud.</span>
      </DisplayHeading>

      <div className="grid grid-cols-1 md:grid-cols-2 gap-8 mt-6">
        <div>
          <p className="text-ink-dim text-[13px] leading-[1.7] mb-4">
            Cloud infrastructure solves the wrong problem. It moves compute
            closer to a central provider.{" "}
            <strong className="text-ink font-medium">
              Net decouples storage and compute from hardware and location.
            </strong>
          </p>
          <p className="text-ink-dim text-[13px] leading-[1.7] mb-4">
            Cloud adds a trusted intermediary by definition.{" "}
            <strong className="text-ink font-medium">
              Net has no intermediaries.
            </strong>{" "}
            Relay nodes forward encrypted bytes they cannot read. There is no
            Cloudflare, no AWS, no Azure in the path because the path is yours.
          </p>
        </div>
        <div>
          <p className="text-ink-dim text-[13px] leading-[1.7] mb-4">
            <strong className="text-ink font-medium">
              Cloud was the right answer when compute was scarce and hardware
              was expensive.
            </strong>{" "}
            Compute is abundant. Hardware is cheap. The coordination layer
            should reflect that.
          </p>
          <p className="text-ink-dim text-[13px] leading-[1.7] mb-4">
            A manufacturing plant running on Net doesn&apos;t route sensor data
            to AWS us-east-1 and back. The sensor talks directly to the decision
            system on the factory floor.{" "}
            <strong className="text-ink font-medium">
              The latency is physics, not geography plus cloud overhead.
            </strong>
          </p>
        </div>
      </div>

      <div className="mt-16 text-center py-16 border-t border-b border-accent-dim bg-accent/[0.02]">
        <div
          className="font-display text-ink leading-[1.1] mb-5"
          style={{ fontSize: "clamp(28px, 4vw, 48px)" }}
        >
          the mesh is <span className="text-accent">already</span>
          <br />
          running.
        </div>
        <a
          href="#install"
          className="btn-primary inline-flex items-center gap-2.5 px-5 py-3 text-[11px] tracking-[0.12em] uppercase font-semibold no-underline border border-accent bg-accent text-bg transition-all mt-5"
        >
          ↓ Join the Net <span className="text-sm">→</span>
        </a>
      </div>
    </section>
  );
}

function FooterDivider() {
  return (
    <div className="text-ink-faint text-[10px] leading-none whitespace-pre text-center py-10 overflow-hidden">
      ░░░░▒▒▒▒▓▓▓▓████████▓▓▓▓▒▒▒▒░░░░ ░░░░▒▒▒▒▓▓▓▓████████▓▓▓▓▒▒▒▒░░░░
      ░░░░▒▒▒▒▓▓▓▓████████▓▓▓▓▒▒▒▒░░░░
    </div>
  );
}

const FOOTER_SPEC: ReadonlyArray<{
  href: string;
  label: string;
  class?: string;
}> = [
  { href: "#topology", label: "Topology classes" },
  { href: "#properties", label: "Protocol properties" },
  { href: "#mikoshi", label: "Mikoshi" },
  { href: "#runtime", label: "Compute runtime" },
  { href: "#apps", label: "Applications" },
  { href: "#wall", label: "The Blackwall" },
  { href: "#releases", label: "Releases" },
];

const FOOTER_DOCS: ReadonlyArray<{
  href: string;
  label: string;
  class?: string;
}> = [
  {
    href: "https://github.com/ai-2070/net/blob/master/net/crates/net/README.md",
    label: "README.md",
  },
  {
    href: "https://github.com/ai-2070/net/blob/master/net/crates/net/docs/COMPUTE.md",
    label: "COMPUTE.md",
  },
  {
    href: "https://github.com/ai-2070/net/blob/master/net/crates/net/docs/CHANNELS.md",
    label: "CHANNELS.md",
  },
  {
    href: "https://github.com/ai-2070/net/blob/master/net/crates/net/docs/SUBNETS.md",
    label: "SUBNETS.md",
  },
  {
    href: "https://github.com/ai-2070/net/blob/master/net/crates/net/docs/SUBPROTOCOLS.md",
    label: "SUBPROTOCOLS.md",
  },
  {
    href: "https://github.com/ai-2070/net/blob/master/net/crates/net/BENCHMARKS.md",
    label: "BENCHMARKS.md",
  },
];

const FOOTER_RESOURCES: ReadonlyArray<{
  href: string;
  label: string;
  class?: string;
}> = [
  {
    href: "https://crates.io/crates/ai2070-net",
    label: "Rust // crates.io",
  },
  {
    href: "https://www.npmjs.com/package/@ai2070/net",
    label: "TypeScript // npm",
  },
  { href: "https://pypi.org/project/ai2070-net/", label: "Python // PyPI" },
  {
    href: "https://github.com/ai-2070/net/tree/master/go",
    label: "Go // module",
  },
  {
    href: "https://github.com/ai-2070/net/tree/master/net/crates/net/include",
    label: "C // SDK",
  },
  { href: "https://github.com/ai-2070/net", label: "Source // GitHub" },
  {
    href: `mailto:${globals.email}`,
    label: "▸ Contact",
    class: "text-accent",
  },
];

const ET_YEAR = new Date().toLocaleString("en-US", {
  timeZone: "America/New_York",
  year: "numeric",
});

function Footer() {
  return (
    <footer className="px-6 pt-16 pb-7 border-t border-accent-dim">
      <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-[2fr_1fr_1fr_1fr] gap-8 mb-12">
        <div>
          <div className="logo-mark font-display text-[22px] text-ink tracking-[0.1em] flex items-baseline gap-2.5 mb-4">
            net{" "}
            <span className="font-mono text-[9px] tracking-[0.15em] font-semibold">
              // AI 2070
            </span>
          </div>
          <p className="text-ink-dim text-[12px] leading-[1.6] max-w-[380px]">
            Network Event Transport. A latency-first encrypted protocol for
            compute.
          </p>
        </div>
        <FooterColumn title="Spec" items={FOOTER_SPEC} />
        <FooterColumn title="Docs" items={FOOTER_DOCS} />
        <FooterColumn title="Resources" items={FOOTER_RESOURCES} />
      </div>

      <div className="border-t border-line pt-6 flex justify-between text-[10px] text-ink-dim tracking-[0.1em] flex-wrap gap-4">
        <span>© {ET_YEAR} — NET // PROTOCOL.0x4E45·54</span>
        <span>
          <span className="text-accent">▸</span> Net status:{" "}
          <span className="text-accent">ONLINE</span>
        </span>
        <span className="shimmer-2070">// AI 2070</span>
      </div>
    </footer>
  );
}

function FooterColumn({
  title,
  items,
}: {
  title: string;
  items: ReadonlyArray<{ href: string; label: string; class?: string }>;
}) {
  return (
    <div>
      <h5 className="text-[10px] tracking-[0.18em] text-ink-dim uppercase mb-4 font-medium">
        {title}
      </h5>
      <ul className="list-none space-y-2">
        {items.map((it) => {
          const external = /^https?:\/\//i.test(it.href);
          return (
            <li key={it.label}>
              <a
                href={it.href}
                {...(external
                  ? { target: "_blank", rel: "noopener noreferrer" }
                  : {})}
                className={cn(
                  "text-ink no-underline text-[12px] hover:text-accent transition-colors",
                  it.class,
                )}
              >
                {it.label}
              </a>
            </li>
          );
        })}
      </ul>
    </div>
  );
}

const GLITCH_CHARS = "█▓▒░@#$%&*+=<>{}[]|/\\01";

function scrambleText(text: string, intensity: number): string {
  return text
    .split("")
    .map((c) => {
      if (c === " ") return " ";
      if (Math.random() > intensity) return c;
      const i = Math.floor(Math.random() * GLITCH_CHARS.length);
      return GLITCH_CHARS[i] ?? c;
    })
    .join("");
}

function GlitchText({
  text,
  intervalMs = 4500,
  offsetMs = 0,
}: {
  text: string;
  intervalMs?: number;
  offsetMs?: number;
}) {
  const [chars, setChars] = useState(text);

  useEffect(() => {
    const timeouts: number[] = [];

    const cycle = (): void => {
      setChars(scrambleText(text, 0.7));
      timeouts.push(
        window.setTimeout(() => {
          setChars(scrambleText(text, 0.4));
        }, 70),
      );
      timeouts.push(
        window.setTimeout(() => {
          setChars(text);
        }, 160),
      );
    };

    let intervalId = 0;
    const startId = window.setTimeout(() => {
      cycle();
      intervalId = window.setInterval(cycle, intervalMs);
    }, offsetMs);

    return () => {
      window.clearTimeout(startId);
      if (intervalId) window.clearInterval(intervalId);
      for (const t of timeouts) window.clearTimeout(t);
    };
  }, [text, intervalMs, offsetMs]);

  return <>{chars}</>;
}

function SeedBanner() {
  return (
    <a
      href={`mailto:${globals.email}`}
      className="group block border-b border-line bg-accent/[0.06] hover:bg-accent/[0.12] transition-colors overflow-hidden"
    >
      <div className="glitch-banner px-6 py-3 flex items-center justify-center gap-3 text-[11px] font-mono tracking-[0.08em] flex-wrap">
        <span className="bg-accent text-bg px-2 py-0.5 font-bold tracking-[0.18em] text-[10px]">
          <GlitchText text="SEED ROUND" intervalMs={5400} offsetMs={2700} />
        </span>
        <span className="text-ink">
          <b className="text-accent">
            <GlitchText text="AI 2070" intervalMs={5400} />
          </b>{" "}
          is raising seed funding to build post-cloud infrastructure.
        </span>
        <span className="text-accent inline-flex items-center gap-1">
          get in touch
          <span className="transition-transform group-hover:translate-x-0.5">
            →
          </span>
        </span>
      </div>
    </a>
  );
}

export default function Home(): JSX.Element {
  return (
    <>
      <NavBar />
      <main className="pt-20 max-w-[1440px] mx-auto">
        <SeedBanner />
        <HeroSection />
        <WhyNotBestEffortSection />
        <TopologyClassesSection />
        <PropertiesSection />
        <BenchmarksSection />
        <MikoshiSection />
        <ComputeRuntimeSection />
        <DatafortsSection />
        <MeshOsSection />
        <ComponentsSection />
        <InstallSection />
        <ApplicationsSection />
        <BlackwallSection />
        <ReleasesSection />
        <ClosingSection />
        <FooterDivider />
        <Footer />
      </main>
    </>
  );
}
