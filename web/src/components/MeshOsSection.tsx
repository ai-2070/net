"use client";

import { useEffect, useRef, useState } from "react";
import { SectionLabel } from "@/components/SectionHeadings";
import { DisplayHeading } from "./DisplayHeading";

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
          <span className="text-accent">monitoring</span>
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

export function MeshOsSection() {
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

      <div className="mt-12 grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 border-t border-l border-line">
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

      <div className="border-l-2 border-accent pl-8 pr-8 py-6 bg-accent/[0.02] mt-12 max-w-[900px]">
        <p className="text-[18px] text-ink leading-[1.5] font-light">
          <strong className="text-accent font-medium">
            MeshOS turns your mesh into a living system.
          </strong>{" "}
          The cluster adapts. The daemons move.
        </p>
      </div>
    </section>
  );
}
