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
            not connections. Loosely inspired by the Net from Cyberpunk 2077 —
            an engineering take on the concept.
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
          precious. Bandwidth was scarce. Routes were scarce. The network had to
          guarantee delivery because the next packet might not get through.
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
            The benchmark numbers aren&apos;t performance metrics. They&apos;re{" "}
            <strong className="text-accent font-medium">
              existence proofs
            </strong>
            . They demonstrate that the software layer is no longer the
            bottleneck. The remaining latency is physics: NIC, wire, speed of
            light. The software got out of the way.
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
    header: "// best-effort",
    headerColor: "ink-dim",
    title: "TCP / IP / HTTP / gRPC",
    titleColor: "ink",
    body: "Optimized for delivery. Queues absorb bursts. Backpressure negotiated. Connections stateful. Trust assumed. Sender slows down when receiver can't keep up.",
    floor: "milliseconds",
    floorColor: "ink",
    throughput: "~10K req/s · per connection",
  },
  {
    header: "// real-time",
    headerColor: "ink-dim",
    title: "CAN / EtherCAT / TSN",
    titleColor: "ink",
    body: "Optimized for deterministic timing. Fixed topologies. Dedicated hardware. Time-slotted access. Guarantees only because you own the wire.",
    floor: "microseconds*",
    floorColor: "ink",
    throughput: "~100K updates/s · dedicated bus",
  },
  {
    header: "// net",
    headerColor: "accent",
    title: "NET → latency-first",
    titleColor: "accent",
    body: "Real-time latencies on commodity hardware over commodity networks. Drop instead of queue. Route around instead of wait. Observe instead of coordinate. Derive instead of query. Mesh transport.",
    floor: "nanoseconds",
    floorColor: "accent",
    throughput: "~20M events/s · per core",
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
        compute that
        <br />
        lives on
        <br />
        the wire.
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
      <SectionLabel>§07 / components on the mesh</SectionLabel>
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
      <SectionLabel>§08 / install</SectionLabel>
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
      <SectionLabel>§09 / target applications</SectionLabel>
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
      <SectionLabel>§10 / the blackwall</SectionLabel>
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
      <SectionLabel>§11 / releases</SectionLabel>
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
      <SectionLabel>§12 / post-cloud</SectionLabel>
      <DisplayHeading>
        not anti-cloud.
        <br />
        post-cloud.
      </DisplayHeading>

      <div className="grid grid-cols-1 md:grid-cols-2 gap-8 mt-6">
        <div>
          <p className="text-ink-dim text-[13px] leading-[1.7] mb-4">
            Cloud infrastructure solves the wrong problem. It moves compute
            closer to a central provider.{" "}
            <strong className="text-ink font-medium">
              Net decouples compute from hardware and location.
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

function SeedBanner() {
  return (
    <a
      href={`mailto:${globals.email}`}
      className="group block border-b border-line bg-accent/[0.06] hover:bg-accent/[0.12] transition-colors overflow-hidden"
    >
      <div className="glitch-banner px-6 py-3 flex items-center justify-center gap-3 text-[11px] font-mono tracking-[0.08em] flex-wrap">
        <span className="bg-accent text-bg px-2 py-0.5 font-bold tracking-[0.18em] text-[10px]">
          SEED ROUND
        </span>
        <span className="text-ink">
          <b className="text-accent glitch-mark" data-text="AI 2070">
            AI 2070
          </b>{" "}
          is raising seed funding to build post-cloud nanoscale infra.
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
