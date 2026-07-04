"use client";

import { useEffect, useMemo, useRef, useState } from "react";

// A simplified take on the index hero's 3D mesh graphic: same rotating
// proximity graph and travelling packet, but the nodes are labelled with
// real-world names (laptop, phone, gpu box…), the benchmark stats are gone,
// and a plain-language event.tail sits in their place.

interface Node3D {
  x: number;
  y: number;
  z: number;
  label?: string;
}

// 12 nodes; the 7 "named" ones carry a real-world label.
const NODES_3D: Record<string, Node3D> = {
  N0: { x: -0.85, y: -0.18, z: 0.3, label: "laptop" },
  N1: { x: -0.55, y: 0.4, z: -0.25, label: "phone" },
  N2: { x: -0.15, y: -0.55, z: 0.55 },
  N3: { x: 0.05, y: -0.05, z: 0.1, label: "desktop" },
  N4: { x: 0.2, y: 0.55, z: -0.4 },
  N5: { x: 0.5, y: -0.3, z: 0.6, label: "gpu box" },
  N6: { x: 0.85, y: 0.2, z: 0.15, label: "server" },
  N7: { x: 0.7, y: -0.55, z: -0.3 },
  N8: { x: -0.4, y: -0.65, z: -0.2 },
  N9: { x: -0.05, y: 0.65, z: 0.4, label: "files" },
  N10: { x: 0.5, y: 0.55, z: 0.2 },
  N11: { x: -0.75, y: 0.1, z: -0.55, label: "camera" },
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
const VIEW_H = 210;
const CENTER_X = VIEW_W / 2;
const CENTER_Y = VIEW_H / 2;
const CAMERA_DIST = 2.1;
const PROJECT_SCALE = 270;
const TILT = 0.18;

function project3D(n: Node3D, angle: number): Projected {
  const cosA = Math.cos(angle);
  const sinA = Math.sin(angle);
  const x = n.x * cosA + n.z * sinA;
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

function MeshGraph() {
  const [angle, setAngle] = useState(0);
  const [packetT, setPacketT] = useState(0);
  const [activePath, setActivePath] = useState<string[]>(["N0", "N6"]);
  const pausedRef = useRef(false);

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
    <svg
      className="mesh-svg w-full aspect-[320/210] block"
      viewBox="0 0 320 210"
      preserveAspectRatio="xMidYMid meet"
      onMouseEnter={() => {
        pausedRef.current = true;
      }}
      onMouseLeave={() => {
        pausedRef.current = false;
      }}
    >
      <defs>
        <pattern
          id="hmgrid"
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
      <rect width={VIEW_W} height={VIEW_H} fill="url(#hmgrid)" />

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
        const labelText = NODES_3D[id]?.label;
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
            {labelText ? (
              <text
                x={p.sx + r + 3}
                y={p.sy + 2}
                fontFamily="JetBrains Mono"
                fontSize="6"
                fill="#c4ff3d"
                fillOpacity={0.4 + fade * 0.45}
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
  );
}

// ---- plain event.tail ----

interface TailLine {
  id: number;
  ts: string;
  type: string;
  typeColor: string;
  body: string;
  metric: string;
  metricColor: string;
}

interface TailTemplate {
  weight: number;
  gen: () => Omit<TailLine, "id" | "ts">;
}

const DEVICE_NAMES: ReadonlyArray<string> = [
  "laptop",
  "phone",
  "desktop",
  "server",
  "files",
];

const COMPUTE_NAMES: ReadonlyArray<string> = ["gpu box", "server"];

let tailCounter = 0;

function tailTs(): string {
  const d = new Date();
  const m = String(d.getMinutes()).padStart(2, "0");
  const s = String(d.getSeconds()).padStart(2, "0");
  const ms = String(d.getMilliseconds()).padStart(3, "0");
  return `${m}:${s}.${ms}`;
}

function pick<T>(arr: ReadonlyArray<T>, fallback: T): T {
  return arr[Math.floor(Math.random() * arr.length)] ?? fallback;
}

const TAIL_TEMPLATES: ReadonlyArray<TailTemplate> = [
  {
    weight: 4,
    gen: () => ({
      type: "join",
      typeColor: "text-accent-dim",
      body: `${pick(DEVICE_NAMES, "laptop")} joined`,
      metric: "↑",
      metricColor: "text-accent",
    }),
  },
  {
    weight: 4,
    gen: () => ({
      type: "route",
      typeColor: "text-accent",
      body: `agent → ${pick(COMPUTE_NAMES, "gpu box")}`,
      metric: "ok",
      metricColor: "text-accent",
    }),
  },
  {
    weight: 3,
    gen: () => ({
      type: "claim",
      typeColor: "text-accent",
      body: "agent reserved gpu box",
      metric: "held",
      metricColor: "text-accent",
    }),
  },
  {
    weight: 3,
    gen: () => ({
      type: "move",
      typeColor: "text-cyan",
      body: `files → ${pick(DEVICE_NAMES, "laptop")}`,
      metric: "file",
      metricColor: "text-ink-dim",
    }),
  },
  {
    weight: 3,
    gen: () => ({
      type: "stream",
      typeColor: "text-cyan",
      body: "camera streaming live",
      metric: "≈",
      metricColor: "text-cyan",
    }),
  },
  {
    weight: 2,
    gen: () => ({
      type: "offer",
      typeColor: "text-ink-dim",
      body: `${pick(DEVICE_NAMES, "laptop")} ▸ browser · files`,
      metric: "—",
      metricColor: "text-ink-faint",
    }),
  },
  {
    weight: 1,
    gen: () => ({
      type: "recover",
      typeColor: "text-warn",
      body: `${pick(DEVICE_NAMES, "phone")} reconnected`,
      metric: "ok",
      metricColor: "text-accent",
    }),
  },
];

const TAIL_WEIGHT = TAIL_TEMPLATES.reduce((s, t) => s + t.weight, 0);

function makeTailLine(): TailLine {
  let r = Math.random() * TAIL_WEIGHT;
  let tpl = TAIL_TEMPLATES[0]!;
  for (const t of TAIL_TEMPLATES) {
    r -= t.weight;
    if (r <= 0) {
      tpl = t;
      break;
    }
  }
  return { id: tailCounter++, ts: tailTs(), ...tpl.gen() };
}

const TAIL_LINES = 6;

function EventTail() {
  const [lines, setLines] = useState<readonly TailLine[]>([]);

  useEffect(() => {
    setLines(Array.from({ length: TAIL_LINES }, () => makeTailLine()));
    const id = window.setInterval(() => {
      const burst = Math.random() < 0.15 ? 2 : 1;
      const fresh = Array.from({ length: burst }, () => makeTailLine());
      setLines((prev) => [...prev, ...fresh].slice(-TAIL_LINES));
    }, 2600);
    return () => window.clearInterval(id);
  }, []);

  return (
    <div className="border-t border-line pt-3 mt-3.5">
      <div className="flex justify-between items-center text-[10px] tracking-[0.12em] text-ink-dim uppercase mb-2.5">
        <span>
          <span className="text-accent">▸</span> event.tail
        </span>
        <span className="flex items-center gap-1.5">
          <span className="w-1.5 h-1.5 rounded-full bg-accent inline-block animate-pulse-dot" />
          LIVE
        </span>
      </div>
      <div className="font-mono text-[10px] leading-[1.55] overflow-hidden min-h-[108px]">
        {lines.map((e) => (
          <div
            key={e.id}
            className="event-line-in flex items-baseline gap-2 whitespace-nowrap overflow-hidden"
          >
            <span
              className="text-ink-faint shrink-0"
              style={{ minWidth: "9ch" }}
            >
              {e.ts}
            </span>
            <span
              className={`${e.typeColor} shrink-0`}
              style={{ minWidth: "7ch" }}
            >
              {e.type}
            </span>
            <span className="text-ink-faint shrink-0">▸</span>
            <span className="text-ink-dim flex-1 truncate">{e.body}</span>
            <span className={`${e.metricColor} shrink-0`}>{e.metric}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

export function HeroMeshViz() {
  return (
    <div className="border border-line bg-bg-2 p-4">
      <div className="flex justify-between items-center border-b border-line pb-2 mb-3.5 text-[10px] tracking-[0.12em] text-ink-dim uppercase">
        <span>
          <span className="text-accent">▸</span> your devices, connected
        </span>
        <span>LIVE</span>
      </div>
      <MeshGraph />
      <EventTail />
    </div>
  );
}
