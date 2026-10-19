"use client";

import { useEffect, useRef } from "react";

/**
 * The suspended field: concrete shards held weightless in a cold light
 * shaft. Some shards are mesh nodes, linked by faint field threads, with a
 * packet hopping the shortest path between two of them. The pointer pushes
 * nearby shards away (telekinesis); a press launches them, and the field
 * pulls everything back to where it was held.
 *
 * Plain 2D canvas, painter-sorted cuboids. Paused off-screen and on hidden
 * tabs; renders one still frame under prefers-reduced-motion.
 */

type Variant = "hero" | "closing";

interface Shard {
  hx: number;
  hy: number;
  hz: number;
  x: number;
  y: number;
  z: number;
  vx: number;
  vy: number;
  w: number;
  h: number;
  d: number;
  rx: number;
  ry: number;
  rz: number;
  wrx: number;
  wry: number;
  wrz: number;
  spin: number;
  tone: number;
  bobA: number;
  bobW: number;
  bobP: number;
  node: number; // -1 = plain debris
  pulse: number;
}

interface Mote {
  x: number;
  y: number;
  r: number;
  a: number;
  s: number;
  p: number;
}

interface Ring {
  x: number;
  y: number;
  t: number;
}

// Node layout borrowed from the old proximity graph, so the topology the
// page has always shown is the one held in the field.
const NODE_POS: ReadonlyArray<readonly [number, number, number]> = [
  [-0.85, -0.18, 0.3],
  [-0.55, 0.4, -0.25],
  [-0.15, -0.55, 0.55],
  [0.05, -0.05, 0.1],
  [0.2, 0.55, -0.4],
  [0.5, -0.3, 0.6],
  [0.85, 0.2, 0.15],
  [0.7, -0.55, -0.3],
  [-0.4, -0.65, -0.2],
  [-0.05, 0.65, 0.4],
  [0.5, 0.55, 0.2],
  [-0.75, 0.1, -0.55],
];
const ADJ: ReadonlyArray<ReadonlyArray<number>> = [
  [1, 2, 11],
  [0, 3, 9, 11],
  [0, 3, 5, 8],
  [1, 2, 4, 6, 9],
  [3, 9, 10],
  [2, 6, 7],
  [3, 5, 7, 10],
  [5, 6, 8, 11],
  [2, 7, 11],
  [1, 3, 4],
  [4, 6],
  [0, 1, 7, 8],
];
const ENDPOINTS = [0, 5, 6, 7, 9, 10, 11];

const EDGES: ReadonlyArray<readonly [number, number]> = (() => {
  const out: Array<readonly [number, number]> = [];
  ADJ.forEach((ns, a) =>
    ns.forEach((b) => {
      if (a < b) out.push([a, b] as const);
    }),
  );
  return out;
})();

function bfs(from: number, to: number): number[] {
  const q: number[][] = [[from]];
  const seen = new Set([from]);
  while (q.length) {
    const path = q.shift() as number[];
    const last = path[path.length - 1] as number;
    if (last === to) return path;
    for (const n of ADJ[last] ?? []) {
      if (!seen.has(n)) {
        seen.add(n);
        q.push([...path, n]);
      }
    }
  }
  return [from, to];
}

// Seeded PRNG so the field composes the same way on every load.
function rng(seed: number): () => number {
  let s = seed >>> 0;
  return () => {
    s = (s + 0x6d2b79f5) >>> 0;
    let t = s;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function hex4(r: () => number): string {
  return Math.floor(r() * 0x10000)
    .toString(16)
    .padStart(4, "0");
}

const FOCAL = 950;
// Light from above, slightly in front and to the left.
const L = (() => {
  const v = [-0.28, -0.9, -0.34];
  const m = Math.hypot(v[0]!, v[1]!, v[2]!);
  return [v[0]! / m, v[1]! / m, v[2]! / m] as const;
})();

// Cuboid faces: vertex indices + outward normal in local space.
const FACES: ReadonlyArray<{
  v: readonly [number, number, number, number];
  n: readonly [number, number, number];
}> = [
  { v: [0, 1, 2, 3], n: [0, 0, -1] },
  { v: [5, 4, 7, 6], n: [0, 0, 1] },
  { v: [4, 0, 3, 7], n: [-1, 0, 0] },
  { v: [1, 5, 6, 2], n: [1, 0, 0] },
  { v: [4, 5, 1, 0], n: [0, -1, 0] },
  { v: [3, 2, 6, 7], n: [0, 1, 0] },
];

export function FieldCanvas({
  variant = "hero",
  className = "",
}: {
  variant?: Variant;
  className?: string;
}) {
  const ref = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const canvas = ref.current;
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    const reduce = window.matchMedia(
      "(prefers-reduced-motion: reduce)",
    ).matches;
    const withNodes = variant === "hero";

    let W = 0;
    let H = 0;
    let dpr = 1;
    let shards: Shard[] = [];
    let motes: Mote[] = [];
    let labels: string[] = [];
    const rings: Ring[] = [];
    let shaft = { x0: 0, x1: 0, w: 0 };
    const mono =
      getComputedStyle(document.documentElement)
        .getPropertyValue("--font-jetbrains-mono")
        .trim() || "monospace";

    const pointer = { x: -9999, y: -9999, active: false };

    let path: number[] = [0, 6];
    let packetT = 0;

    const build = (): void => {
      const rect = canvas.getBoundingClientRect();
      W = Math.max(1, rect.width);
      H = Math.max(1, rect.height);
      dpr = Math.min(window.devicePixelRatio || 1, 1.75);
      canvas.width = Math.round(W * dpr);
      canvas.height = Math.round(H * dpr);

      const r = rng(variant === "hero" ? 2070 : 451);
      const narrow = W < 760;
      const hero = variant === "hero";

      // Where the field lives: the right of the hero (text owns the left),
      // centred on the closing slab.
      const fx0 = hero ? (narrow ? -0.05 * W : 0.4 * W) : 0.02 * W;
      const fx1 = hero ? 1.06 * W : 0.98 * W;
      const fy0 = -0.06 * H;
      const fy1 = 1.04 * H;

      shaft = hero
        ? {
            x0: narrow ? 0.62 * W : 0.66 * W,
            x1: narrow ? 0.4 * W : 0.56 * W,
            w: narrow ? 0.34 * W : 0.2 * W,
          }
        : { x0: 0.52 * W, x1: 0.46 * W, w: 0.28 * W };

      const count = Math.round(
        (hero ? (narrow ? 70 : 150) : narrow ? 40 : 90) *
          Math.min(1.25, Math.max(0.6, (W * H) / (1440 * 820))),
      );

      shards = [];
      for (let i = 0; i < count; i++) {
        const big = i < (hero ? 4 : 2);
        const mid = !big && r() < 0.16;
        const base = big ? 120 + r() * 160 : mid ? 34 + r() * 40 : 7 + r() * 22;
        const w = base * (big ? 0.42 + r() * 0.3 : 0.6 + r() * 0.8);
        const h = base * (big ? 1.6 + r() * 1.2 : 0.5 + r() * 0.9);
        const d = base * (0.25 + r() * 0.6);
        let hx: number;
        let hy: number;
        let hz: number;
        if (big) {
          // Monoliths: hard-cropped at the edges, far back, nearly still.
          const side = i % 2 === 0;
          hx = hero
            ? side
              ? fx1 - r() * 0.06 * W
              : 0.78 * W + r() * 0.1 * W
            : side
              ? 0.9 * W
              : 0.08 * W;
          hy = fy0 + r() * (fy1 - fy0);
          hz = 260 + r() * 260;
        } else {
          hx = fx0 + r() * (fx1 - fx0);
          hy = fy0 + r() * (fy1 - fy0);
          hz = -320 + r() * 640;
        }
        shards.push({
          hx,
          hy,
          hz,
          x: hx,
          y: hy,
          z: hz,
          vx: 0,
          vy: 0,
          w,
          h,
          d,
          rx: r() * Math.PI * 2,
          ry: r() * Math.PI * 2,
          rz: r() * Math.PI * 2,
          wrx: (r() - 0.5) * (big ? 0.03 : 0.16),
          wry: (r() - 0.5) * (big ? 0.04 : 0.2),
          wrz: (r() - 0.5) * (big ? 0.02 : 0.12),
          spin: 0,
          tone: 0.55 + r() * 0.45,
          bobA: big ? 6 + r() * 6 : 3 + r() * 9,
          bobW: 0.25 + r() * 0.4,
          bobP: r() * Math.PI * 2,
          node: -1,
          pulse: 0,
        });
      }

      if (withNodes) {
        const cx = narrow ? 0.7 * W : 0.71 * W;
        const cy = narrow ? 0.14 * H : 0.42 * H;
        const sx = narrow ? 0.3 * W : 0.24 * W;
        const sy = narrow ? 0.09 * H : 0.3 * H;
        labels = NODE_POS.map(() => "node.0x" + hex4(r));
        NODE_POS.forEach(([nx, ny, nz], k) => {
          const s = 11 + r() * 5;
          shards.push({
            hx: cx + nx * sx,
            hy: cy + ny * sy,
            hz: nz * 260,
            x: cx + nx * sx,
            y: cy + ny * sy,
            z: nz * 260,
            vx: 0,
            vy: 0,
            w: s,
            h: s,
            d: s,
            rx: r() * 6,
            ry: r() * 6,
            rz: r() * 6,
            wrx: 0.25 + r() * 0.2,
            wry: 0.3 + r() * 0.2,
            wrz: 0.1,
            spin: 0,
            tone: 1,
            bobA: 5 + r() * 4,
            bobW: 0.3 + r() * 0.3,
            bobP: r() * 6,
            node: k,
            pulse: 0,
          });
        });
      }

      motes = Array.from({ length: hero ? 140 : 80 }, () => ({
        x: r(),
        y: r(),
        r: 0.4 + r() * 1.3,
        a: 0.15 + r() * 0.55,
        s: 0.004 + r() * 0.012,
        p: r() * 6.28,
      }));
    };

    const nodeIndex = (k: number): Shard | undefined =>
      shards.find((s) => s.node === k);

    const pickPath = (): void => {
      const a = ENDPOINTS[Math.floor(Math.random() * ENDPOINTS.length)] ?? 0;
      let b = a;
      while (b === a)
        b = ENDPOINTS[Math.floor(Math.random() * ENDPOINTS.length)] ?? 6;
      path = bfs(a, b);
      packetT = 0;
    };

    const project = (
      x: number,
      y: number,
      z: number,
    ): [number, number, number] => {
      const vpX = W * 0.62;
      const vpY = H * 0.45;
      const p = FOCAL / (FOCAL + z);
      return [vpX + (x - vpX) * p, vpY + (y - vpY) * p, p];
    };

    const shaftBoost = (sx: number, sy: number): number => {
      const t = sy / H;
      const cx = shaft.x0 + (shaft.x1 - shaft.x0) * t;
      const half = shaft.w * (0.45 + t * 0.55);
      const dx = (sx - cx) / half;
      return Math.exp(-dx * dx * 1.6);
    };

    const drawShaft = (time: number): void => {
      const topL = shaft.x0 - shaft.w * 0.22;
      const topR = shaft.x0 + shaft.w * 0.22;
      const botL = shaft.x1 - shaft.w;
      const botR = shaft.x1 + shaft.w;
      ctx.save();
      ctx.globalCompositeOperation = "lighter";
      const g = ctx.createLinearGradient(0, 0, 0, H);
      g.addColorStop(0, "rgba(238,241,232,0.085)");
      g.addColorStop(0.55, "rgba(238,241,232,0.035)");
      g.addColorStop(1, "rgba(238,241,232,0.0)");
      ctx.fillStyle = g;
      ctx.beginPath();
      ctx.moveTo(topL, 0);
      ctx.lineTo(topR, 0);
      ctx.lineTo(botR, H);
      ctx.lineTo(botL, H);
      ctx.closePath();
      ctx.fill();
      // brighter core
      const g2 = ctx.createLinearGradient(0, 0, 0, H * 0.8);
      g2.addColorStop(0, "rgba(238,241,232,0.07)");
      g2.addColorStop(1, "rgba(238,241,232,0)");
      ctx.fillStyle = g2;
      ctx.beginPath();
      ctx.moveTo(shaft.x0 - shaft.w * 0.08, 0);
      ctx.lineTo(shaft.x0 + shaft.w * 0.08, 0);
      ctx.lineTo(shaft.x1 + shaft.w * 0.4, H * 0.8);
      ctx.lineTo(shaft.x1 - shaft.w * 0.4, H * 0.8);
      ctx.closePath();
      ctx.fill();
      // dust
      for (const m of motes) {
        const y = ((m.y + time * m.s * 0.25) % 1) * H;
        const t = y / H;
        const cx = shaft.x0 + (shaft.x1 - shaft.x0) * t;
        const half = shaft.w * (0.45 + t * 0.55);
        const x = cx + (m.x - 0.5) * 2 * half + Math.sin(time * 0.3 + m.p) * 6;
        const b = shaftBoost(x, y);
        const a = m.a * b * (0.55 + 0.45 * Math.sin(time * 1.3 + m.p));
        if (a < 0.02) continue;
        ctx.fillStyle = `rgba(238,241,232,${a.toFixed(3)})`;
        ctx.fillRect(x, y, m.r, m.r);
      }
      ctx.restore();
    };

    const drawCuboid = (s: Shard, time: number): [number, number, number] => {
      const cxr = Math.cos(s.rx);
      const sxr = Math.sin(s.rx);
      const cyr = Math.cos(s.ry);
      const syr = Math.sin(s.ry);
      const czr = Math.cos(s.rz);
      const szr = Math.sin(s.rz);
      const rot = (
        x: number,
        y: number,
        z: number,
      ): [number, number, number] => {
        // X
        let y1 = y * cxr - z * sxr;
        let z1 = y * sxr + z * cxr;
        // Y
        const x2 = x * cyr + z1 * syr;
        z1 = -x * syr + z1 * cyr;
        // Z
        const x3 = x2 * czr - y1 * szr;
        y1 = x2 * szr + y1 * czr;
        return [x3, y1, z1];
      };
      const hw = s.w / 2;
      const hh = s.h / 2;
      const hd = s.d / 2;
      const local: Array<[number, number, number]> = [
        [-hw, -hh, -hd],
        [hw, -hh, -hd],
        [hw, hh, -hd],
        [-hw, hh, -hd],
        [-hw, -hh, hd],
        [hw, -hh, hd],
        [hw, hh, hd],
        [-hw, hh, hd],
      ];
      const pts = local.map(([x, y, z]) => {
        const [a, b, c] = rot(x, y, z);
        return project(s.x + a, s.y + b, s.z + c);
      });
      const [scx, scy, sp] = project(s.x, s.y, s.z);
      const boost = shaftBoost(scx, scy);
      // depth haze: far shards sink into the dark
      const haze = Math.min(1, Math.max(0, (s.z + 320) / 900));
      const isNode = s.node >= 0;

      for (const f of FACES) {
        const [nx, ny, nz] = rot(f.n[0], f.n[1], f.n[2]);
        if (nz >= 0.02) continue; // facing away from the camera
        const lam = Math.max(0, -(nx * L[0] + ny * L[1] + nz * L[2]));
        let v: number;
        if (isNode) {
          v = 0.06 + lam * 0.12;
        } else {
          v = (0.07 + lam * 0.24 + lam * boost * 0.42) * s.tone;
        }
        v = v * (1 - haze * 0.55) + 0.043 * haze * 0.55;
        const R = Math.round(Math.min(1, v * 0.985) * 255);
        const G = Math.round(Math.min(1, v) * 255);
        const B = Math.round(Math.min(1, v * 0.94) * 255);
        ctx.fillStyle = `rgb(${R},${G},${B})`;
        ctx.beginPath();
        const p0 = pts[f.v[0]]!;
        ctx.moveTo(p0[0], p0[1]);
        for (let k = 1; k < 4; k++) {
          const pk = pts[f.v[k]]!;
          ctx.lineTo(pk[0], pk[1]);
        }
        ctx.closePath();
        ctx.fill();
        if (isNode) {
          const a = 0.55 + 0.45 * s.pulse;
          ctx.strokeStyle = `rgba(196,255,61,${(a * (1 - haze * 0.5)).toFixed(3)})`;
          ctx.lineWidth = 1;
          ctx.stroke();
        } else if (lam > 0.55 && boost > 0.25) {
          // sunlit arris
          ctx.strokeStyle = `rgba(238,241,232,${(0.12 * boost).toFixed(3)})`;
          ctx.lineWidth = 0.6;
          ctx.stroke();
        }
      }

      if (isNode) {
        const glow = 0.35 + 0.65 * s.pulse;
        const rad = (5 + 10 * s.pulse) * sp;
        const g = ctx.createRadialGradient(scx, scy, 0, scx, scy, rad * 3);
        g.addColorStop(0, `rgba(196,255,61,${(0.5 * glow).toFixed(3)})`);
        g.addColorStop(1, "rgba(196,255,61,0)");
        ctx.fillStyle = g;
        ctx.beginPath();
        ctx.arc(scx, scy, rad * 3, 0, Math.PI * 2);
        ctx.fill();
        ctx.fillStyle = "#c4ff3d";
        ctx.fillRect(scx - 1.5, scy - 1.5, 3, 3);
        const label = labels[s.node];
        // labels never run under the telemetry slabs on the right
        const underSlabs = scx > W - 480 && scy > H * 0.3;
        if (label && W >= 760 && !underSlabs) {
          ctx.font = `500 ${Math.round(10 * Math.max(0.85, sp))}px ${mono}`;
          ctx.fillStyle = `rgba(196,255,61,${(0.28 + 0.4 * (1 - haze) + 0.3 * s.pulse).toFixed(3)})`;
          ctx.fillText(label, scx + 12 * sp, scy + 3);
        }
      }
      void time;
      return [scx, scy, sp];
    };

    let last = performance.now();
    let time = 0;
    let raf = 0;
    let running = false;
    let visible = true;

    const step = (dt: number): void => {
      time += dt;
      const k = 7.5; // field stiffness: how firmly a shard is held
      const c = 3.2; // damping
      for (const s of shards) {
        const tyHome = s.hy + Math.sin(time * s.bobW + s.bobP) * s.bobA;
        const [sx, sy] = project(s.x, s.y, s.z);
        if (pointer.active) {
          const dx = sx - pointer.x;
          const dy = sy - pointer.y;
          const d = Math.hypot(dx, dy);
          const R = 170;
          if (d < R && d > 0.001) {
            const f = Math.pow(1 - d / R, 2) * 2600;
            s.vx += (dx / d) * f * dt;
            s.vy += (dy / d) * f * dt;
            s.spin = Math.min(6, s.spin + (1 - d / R) * 4 * dt);
          }
        }
        s.vx += (-(s.x - s.hx) * k - s.vx * c) * dt;
        s.vy += (-(s.y - tyHome) * k - s.vy * c) * dt;
        s.x += s.vx * dt;
        s.y += s.vy * dt;
        s.spin *= Math.exp(-1.4 * dt);
        const spinMul = 1 + s.spin;
        s.rx += s.wrx * dt * spinMul;
        s.ry += s.wry * dt * spinMul;
        s.rz += s.wrz * dt * spinMul;
        s.pulse *= Math.exp(-2.2 * dt);
      }
      if (withNodes) {
        packetT += dt * 0.42;
        if (packetT >= 1) {
          const end = nodeIndex(path[path.length - 1] ?? 0);
          if (end) end.pulse = 1;
          pickPath();
        }
      }
      for (let i = rings.length - 1; i >= 0; i--) {
        const ring = rings[i]!;
        ring.t += dt;
        if (ring.t > 1.2) rings.splice(i, 1);
      }
    };

    const draw = (): void => {
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      ctx.clearRect(0, 0, W, H);
      drawShaft(time);

      // painter's order: far to near
      const order = [...shards].sort((a, b) => b.z - a.z);
      const screen = new Map<number, [number, number, number]>();

      // threads sit between the far debris and the near debris
      let threadsDrawn = !withNodes;
      for (const s of order) {
        if (!threadsDrawn && s.z < 60) {
          drawThreads(screen);
          threadsDrawn = true;
        }
        const p = drawCuboid(s, time);
        if (s.node >= 0) screen.set(s.node, p);
      }
      if (!threadsDrawn) drawThreads(screen);
      if (withNodes) drawThreads(screen, true);

      for (const ring of rings) {
        const t = ring.t / 1.2;
        const rad = 30 + t * 320;
        ctx.strokeStyle = `rgba(238,241,232,${(0.22 * (1 - t)).toFixed(3)})`;
        ctx.lineWidth = 1;
        ctx.beginPath();
        ctx.arc(ring.x, ring.y, rad, 0, Math.PI * 2);
        ctx.stroke();
      }
    };

    // Positions of nodes not yet drawn this frame are projected on demand.
    const nodeScreen = (
      screen: Map<number, [number, number, number]>,
      k: number,
    ): [number, number, number] | undefined => {
      const cached = screen.get(k);
      if (cached) return cached;
      const s = nodeIndex(k);
      return s ? project(s.x, s.y, s.z) : undefined;
    };

    const drawThreads = (
      screen: Map<number, [number, number, number]>,
      packetOnly = false,
    ): void => {
      if (!withNodes) return;
      const active = new Set<string>();
      for (let i = 0; i < path.length - 1; i++) {
        const a = path[i]!;
        const b = path[i + 1]!;
        active.add(a < b ? `${a}|${b}` : `${b}|${a}`);
      }
      if (!packetOnly) {
        for (const [a, b] of EDGES) {
          const pa = nodeScreen(screen, a);
          const pb = nodeScreen(screen, b);
          if (!pa || !pb) continue;
          const on = active.has(`${a}|${b}`);
          ctx.strokeStyle = on
            ? "rgba(196,255,61,0.55)"
            : `rgba(196,255,61,${(0.05 + 0.08 * Math.min(pa[2], pb[2])).toFixed(3)})`;
          ctx.lineWidth = on ? 1 : 0.6;
          ctx.setLineDash(on ? [4, 4] : []);
          ctx.lineDashOffset = on ? -time * 18 : 0;
          ctx.beginPath();
          ctx.moveTo(pa[0], pa[1]);
          ctx.lineTo(pb[0], pb[1]);
          ctx.stroke();
        }
        ctx.setLineDash([]);
        return;
      }
      // packet
      const segs = path.length - 1;
      if (segs < 1) return;
      const seg = Math.min(Math.floor(packetT * segs), segs - 1);
      const lt = packetT * segs - seg;
      const pa = nodeScreen(screen, path[seg]!);
      const pb = nodeScreen(screen, path[seg + 1]!);
      if (!pa || !pb) return;
      const x = pa[0] + (pb[0] - pa[0]) * lt;
      const y = pa[1] + (pb[1] - pa[1]) * lt;
      const g = ctx.createRadialGradient(x, y, 0, x, y, 16);
      g.addColorStop(0, "rgba(196,255,61,0.55)");
      g.addColorStop(1, "rgba(196,255,61,0)");
      ctx.fillStyle = g;
      ctx.beginPath();
      ctx.arc(x, y, 16, 0, Math.PI * 2);
      ctx.fill();
      ctx.fillStyle = "#eef7d8";
      ctx.beginPath();
      ctx.arc(x, y, 2.4, 0, Math.PI * 2);
      ctx.fill();
    };

    const frame = (now: number): void => {
      const dt = Math.min(0.05, (now - last) / 1000);
      last = now;
      step(dt);
      draw();
      raf = requestAnimationFrame(frame);
    };

    const start = (): void => {
      if (running || reduce) return;
      running = true;
      last = performance.now();
      raf = requestAnimationFrame(frame);
    };
    const stop = (): void => {
      running = false;
      cancelAnimationFrame(raf);
    };

    build();
    pickPath();
    if (reduce) {
      time = 4;
      step(0);
      draw();
    } else {
      // settle a few seconds so the first paint is already mid-drift
      for (let i = 0; i < 30; i++) step(1 / 30);
      draw();
      start();
    }

    const ro = new ResizeObserver(() => {
      build();
      draw();
    });
    ro.observe(canvas);

    const io = new IntersectionObserver(
      ([e]) => {
        visible = Boolean(e?.isIntersecting);
        if (visible && document.visibilityState === "visible") start();
        else stop();
      },
      { rootMargin: "80px" },
    );
    io.observe(canvas);

    const onVis = (): void => {
      if (document.visibilityState === "visible" && visible) start();
      else stop();
    };
    document.addEventListener("visibilitychange", onVis);

    // Pointer lives on the section, not the canvas, so text above it still
    // receives clicks while the field reacts underneath.
    const host = canvas.closest("section") ?? canvas.parentElement ?? canvas;
    const toLocal = (e: PointerEvent): [number, number] => {
      const r = canvas.getBoundingClientRect();
      return [e.clientX - r.left, e.clientY - r.top];
    };
    const onMove = (e: PointerEvent): void => {
      if (e.pointerType === "touch") return;
      const [x, y] = toLocal(e);
      pointer.x = x;
      pointer.y = y;
      pointer.active = true;
    };
    const onLeave = (): void => {
      pointer.active = false;
    };
    const onDown = (e: PointerEvent): void => {
      const target = e.target as HTMLElement | null;
      if (target?.closest("a,button,[data-field-ignore]")) return;
      const [x, y] = toLocal(e);
      rings.push({ x, y, t: 0 });
      for (const s of shards) {
        const [sx, sy] = project(s.x, s.y, s.z);
        const dx = sx - x;
        const dy = sy - y;
        const d = Math.hypot(dx, dy);
        const R = 320;
        if (d < R && d > 0.001) {
          const f = Math.pow(1 - d / R, 1.5) * 900;
          s.vx += (dx / d) * f;
          s.vy += (dy / d) * f;
          s.spin = Math.min(9, s.spin + (1 - d / R) * 9);
        }
      }
    };
    if (!reduce) {
      host.addEventListener("pointermove", onMove);
      host.addEventListener("pointerleave", onLeave);
      host.addEventListener("pointerdown", onDown);
    }

    return () => {
      stop();
      ro.disconnect();
      io.disconnect();
      document.removeEventListener("visibilitychange", onVis);
      host.removeEventListener("pointermove", onMove);
      host.removeEventListener("pointerleave", onLeave);
      host.removeEventListener("pointerdown", onDown);
    };
  }, [variant]);

  return (
    <canvas
      ref={ref}
      aria-hidden
      className={`block w-full h-full ${className}`}
    />
  );
}
