"use client";

import { useEffect, useRef, useState } from "react";

import { US_STATES } from "./us-map";

type NodeId = "A" | "G" | "R1" | "R2" | "B" | "R3" | "R4";

const NODE_POS: Record<NodeId, readonly [number, number]> = {
  A: [60, 40],
  G: [160, 80],
  R1: [260, 50],
  R2: [100, 160],
  B: [220, 170],
  R3: [290, 130],
  R4: [40, 110],
};

const NODE_ADJ: Record<NodeId, readonly NodeId[]> = {
  A: ["G", "R2", "R4"],
  G: ["A", "R1", "R2", "B"],
  R1: ["G", "R3"],
  R2: ["G", "A", "B", "R4"],
  B: ["G", "R2", "R3"],
  R3: ["R1", "B"],
  R4: ["R2", "A"],
};

const EDGE_ALIAS: Record<string, string> = {
  "A-G": "A-G",
  "G-R1": "G-R1",
  "G-R2": "G-R2",
  "B-G": "G-B",
  "B-R2": "R2-B",
  "A-R2": "A-R2",
  "R1-R3": "R1-R3",
  "B-R3": "R3-B",
  "R2-R4": "R4-R2",
  "A-R4": "R4-A",
};

const NODE_LABEL_ORDER: readonly NodeId[] = [
  "A",
  "G",
  "R1",
  "R2",
  "B",
  "R3",
  "R4",
];

const ENDPOINT_PAIRS: ReadonlyArray<readonly [NodeId, NodeId]> = [
  ["A", "B"],
  ["B", "A"],
  ["A", "R3"],
  ["R4", "B"],
  ["A", "R1"],
];

function edgeKey(a: NodeId, b: NodeId): string {
  return [a, b].sort().join("-");
}

function shortestPath(from: NodeId, to: NodeId): NodeId[] {
  const queue: NodeId[][] = [[from]];
  const seen = new Set<NodeId>([from]);
  while (queue.length > 0) {
    const path = queue.shift();
    if (!path) break;
    const last = path[path.length - 1];
    if (last === to) return path;
    for (const n of NODE_ADJ[last]) {
      if (!seen.has(n)) {
        seen.add(n);
        queue.push([...path, n]);
      }
    }
  }
  return [from, to];
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
  const linksRef = useRef<SVGGElement | null>(null);
  const motionRef = useRef<SVGAnimateMotionElement | null>(null);
  const labelsRef = useRef<SVGGElement | null>(null);

  useEffect(() => {
    const linksGroup = linksRef.current;
    const motion = motionRef.current;
    const labelsGroup = labelsRef.current;
    if (!linksGroup || !motion) return;

    const hex = (): string =>
      Math.floor(Math.random() * 256)
        .toString(16)
        .padStart(2, "0");

    const ids = {} as Record<NodeId, string>;
    for (const k of NODE_LABEL_ORDER) {
      ids[k] = "node.0x" + hex() + hex();
    }

    if (labelsGroup) {
      const texts = labelsGroup.querySelectorAll("text");
      texts.forEach((t, i) => {
        const id = NODE_LABEL_ORDER[i];
        if (id) t.textContent = ids[id];
      });
    }

    const tick = (): void => {
      const pair =
        ENDPOINT_PAIRS[Math.floor(Math.random() * ENDPOINT_PAIRS.length)];
      if (!pair) return;
      const [src, dst] = pair;
      const path = shortestPath(src, dst);

      linksGroup
        .querySelectorAll("line")
        .forEach((l) => l.setAttribute("class", "link"));

      for (let i = 0; i < path.length - 1; i++) {
        const a = path[i];
        const b = path[i + 1];
        if (!a || !b) continue;
        const key = edgeKey(a, b);
        const alias = EDGE_ALIAS[key] ?? key;
        const edge = linksGroup.querySelector(`[data-edge="${alias}"]`);
        if (edge) edge.setAttribute("class", "link link-active");
      }

      const d = path
        .map((n, i) => {
          const [x, y] = NODE_POS[n];
          return (i === 0 ? "M " : "L ") + x + " " + y;
        })
        .join(" ");
      motion.setAttribute("path", d);
      motion.setAttribute(
        "dur",
        (0.4 + (path.length - 1) * 0.55).toFixed(2) + "s",
      );
      motion.beginElement?.();
    };

    tick();
    const id = window.setInterval(tick, 2600);
    return () => window.clearInterval(id);
  }, []);

  return (
    <div className="hidden lg:block border border-line bg-bg-2 p-4 self-start">
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
        <rect width="320" height="220" fill="url(#grid)" />
        <g ref={linksRef}>
          <line
            className="link"
            data-edge="A-G"
            x1="60"
            y1="40"
            x2="160"
            y2="80"
          />
          <line
            className="link"
            data-edge="G-R1"
            x1="160"
            y1="80"
            x2="260"
            y2="50"
          />
          <line
            className="link"
            data-edge="G-R2"
            x1="160"
            y1="80"
            x2="100"
            y2="160"
          />
          <line
            className="link"
            data-edge="G-B"
            x1="160"
            y1="80"
            x2="220"
            y2="170"
          />
          <line
            className="link"
            data-edge="R2-B"
            x1="100"
            y1="160"
            x2="220"
            y2="170"
          />
          <line
            className="link"
            data-edge="A-R2"
            x1="60"
            y1="40"
            x2="100"
            y2="160"
          />
          <line
            className="link"
            data-edge="R1-R3"
            x1="260"
            y1="50"
            x2="290"
            y2="130"
          />
          <line
            className="link"
            data-edge="R3-B"
            x1="290"
            y1="130"
            x2="220"
            y2="170"
          />
          <line
            className="link"
            data-edge="R4-R2"
            x1="40"
            y1="110"
            x2="100"
            y2="160"
          />
          <line
            className="link"
            data-edge="R4-A"
            x1="40"
            y1="110"
            x2="60"
            y2="40"
          />
        </g>
        <g>
          <circle className="node" cx="60" cy="40" r="4" />
          <circle className="node" cx="160" cy="80" r="4" />
          <circle className="node" cx="260" cy="50" r="4" />
          <circle className="node" cx="100" cy="160" r="4" />
          <circle className="node" cx="220" cy="170" r="4" />
          <circle className="node" cx="290" cy="130" r="4" />
          <circle className="node" cx="40" cy="110" r="4" />
        </g>
        <g
          ref={labelsRef}
          fontFamily="JetBrains Mono"
          fontSize="6"
          fill="#6b7568"
        >
          <text x="68" y="36" />
          <text x="168" y="76" />
          <text x="266" y="46" />
          <text x="106" y="158" />
          <text x="226" y="166" />
          <text x="296" y="128" />
          <text x="6" y="106" />
        </g>
        <circle r="2" fill="#c4ff3d">
          <animateMotion
            ref={motionRef}
            dur="2s"
            repeatCount="indefinite"
            path="M 60 40 L 160 80 L 220 170"
          />
        </circle>
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

function TopStatusBar() {
  const [evt, setEvt] = useState<string>("8.4M");
  const [p50, setP50] = useState<string>("38ns");

  useEffect(() => {
    const evtId = window.setInterval(() => {
      const base = 8_400_000;
      const jitter = Math.floor((Math.random() - 0.5) * 200_000);
      const n = base + jitter;
      setEvt((n / 1_000_000).toFixed(1) + "M");
    }, 2000);

    const p50Id = window.setInterval(() => {
      const base = 38;
      const j = Math.floor((Math.random() - 0.5) * 6);
      setP50(base + j + "ns");
    }, 2400);

    return () => {
      window.clearInterval(evtId);
      window.clearInterval(p50Id);
    };
  }, []);

  return (
    <div className="fixed top-0 left-0 right-0 h-7 bg-bg border-b border-line flex items-center px-4 text-[10px] text-ink-dim z-[100] tracking-[0.05em]">
      <span className="live-dot inline-flex items-center gap-1.5 text-accent">
        MESH ONLINE
      </span>
      <span className="text-ink-faint mx-3">│</span>
      <span>
        NODES: <b className="text-ink font-semibold">14,872</b>
      </span>
      <span className="text-ink-faint mx-3">│</span>
      <span>
        EVT/SEC: <b className="text-ink font-semibold">{evt}</b>
      </span>
      <span className="text-ink-faint mx-3">│</span>
      <span>
        P50: <b className="text-ink font-semibold">{p50}</b>
      </span>
      <div className="ml-auto hidden md:flex gap-4">
        <span>v0.9.4-rc1</span>
        <span>BUILD: 2026.04.27</span>
        <span>
          SHA: <span className="text-accent">a7f9c2e</span>
        </span>
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
      <div className="logo-mark font-display text-[22px] text-ink tracking-[0.1em] flex items-baseline gap-2.5">
        net{" "}
        <span className="font-mono text-[9px] text-accent tracking-[0.15em] font-semibold">
          // NET 2070
        </span>
      </div>
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

function HeroSection() {
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
            <span className="text-ink-dim">REV 04 / Q2 2026</span>
          </div>

          <h1
            className="font-display leading-[0.88] tracking-[-0.02em] text-ink mb-5"
            style={{ fontSize: "clamp(56px, 10vw, 144px)" }}
          >
            net.
            <br />
            <span className="text-accent">moves</span>
            <br />
            at light.
          </h1>

          <p className="text-[18px] text-ink mt-8 max-w-[580px] leading-[1.5] font-light">
            A latency-first encrypted mesh where every device is a first-class
            node. Existing networks operate in milliseconds{" "}
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
              href="#what"
              className="btn-ghost inline-flex items-center gap-2.5 py-3 text-[11px] tracking-[0.12em] uppercase font-semibold no-underline text-ink transition-all"
            >
              // view spec ↘
            </a>
          </div>
        </div>

        <MeshViz />
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
  UCLA: { x: 215, y: 360, label: "UCLA" },
  SRI: { x: 175, y: 280, label: "SRI" },
  UCSB: { x: 200, y: 345 },
  RAND: { x: 222, y: 365 },
  UTAH: { x: 330, y: 260, label: "UTAH" },
  ILL: { x: 685, y: 245, label: "UIUC" },
  CASE: { x: 790, y: 215 },
  CMU: { x: 820, y: 230, label: "CMU" },
  MITRE: { x: 880, y: 260, label: "MITRE" },
  BBN: { x: 945, y: 160, label: "BBN" },
  MIT: { x: 940, y: 165 },
  HVD: { x: 948, y: 152 },
  LINC: { x: 952, y: 158 },
  BURR: { x: 935, y: 170 },
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
    <svg
      className="absolute right-0 top-12 w-full max-w-[820px] aspect-[1000/589] pointer-events-none opacity-[0.22]"
      viewBox="0 0 1000 589"
      preserveAspectRatio="xMidYMid meet"
      aria-hidden
      style={{
        WebkitMaskImage:
          "radial-gradient(ellipse 80% 80% at 60% 50%, #000 30%, transparent 95%)",
        maskImage:
          "radial-gradient(ellipse 80% 80% at 60% 50%, #000 30%, transparent 95%)",
      }}
    >
      <g
        fill="none"
        stroke="#c4ff3d"
        strokeWidth="0.6"
        strokeOpacity="1"
        strokeLinejoin="round"
        strokeLinecap="round"
      >
        {US_STATES.map((s) => (
          <path key={s.id} d={s.d} />
        ))}
      </g>
      {ARPANET_EDGES.map(([a, b]) => {
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
      ))}
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
          net assumes abundance.
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
  },
  {
    header: "// real-time",
    headerColor: "ink-dim",
    title: "CAN / EtherCAT / TSN",
    titleColor: "ink",
    body: "Optimized for deterministic timing. Fixed topologies. Dedicated hardware. Time-slotted access. Guarantees only because you own the wire.",
    floor: "microseconds*",
    floorColor: "ink",
  },
  {
    header: "// net",
    headerColor: "accent",
    title: "NET → mesh transport",
    titleColor: "accent",
    body: "Real-time latencies on commodity hardware over commodity networks. Drop instead of queue. Route around instead of wait. Observe instead of coordinate. Derive instead of query.",
    floor: "nanoseconds",
    floorColor: "accent",
  },
];

function TopologyClassesSection() {
  return (
    <section className="border-b border-line px-6 py-20">
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
            className={`px-6 py-7 border-b border-line ${i < 2 ? "lg:border-r" : ""}`}
          >
            <div
              className={`font-head text-[18px] leading-tight ${c.titleColor === "accent" ? "text-accent" : "text-ink"} mb-3.5 tracking-[0.04em] lowercase`}
            >
              {c.title}
            </div>
            <div className="text-ink-dim text-[12px] leading-[1.6]">
              {c.body}
            </div>
            <div className="mt-4 text-[11px] text-ink-dim border-t border-dashed border-ink-faint pt-3">
              latency floor:{" "}
              <b
                className={`${c.floorColor === "accent" ? "text-accent" : "text-ink"} font-semibold`}
              >
                {c.floor}
              </b>
            </div>
          </div>
        ))}
      </div>
    </section>
  );
}

interface AxiomCard {
  id: string;
  title: string;
  body: string;
  ascii: string;
}

const AXIOMS: readonly AxiomCard[] = [
  {
    id: "P.01",
    title: "Latency-first",
    body: "Sub-nanosecond header serialization. Nanosecond heartbeats, hops, recovery. Packet scheduling at timescales reserved for local function calls.",
    ascii: "┌─────┐\n│ 1ns │ → forward\n└─────┘",
  },
  {
    id: "P.02",
    title: "Streaming-first",
    body: "Data is continuous flow, not documents. Sharded ring buffers, adaptive batching. No requests and responses — there are streams.",
    ascii: "▶▶▶▶▶▶▶▶▶▶▶▶▶▶\n░░░░░░░░░░░░░░",
  },
  {
    id: "P.03",
    title: "Zero-copy",
    body: "Ring buffers, no garbage collector, native Rust. Forwarding doesn't allocate or copy payload data. Design principle, not optimization.",
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
    ascii: "trust := observation\nnot assumption",
  },
  {
    id: "P.06",
    title: "Schema-agnostic",
    body: "Transport moves bytes, not structures. Raw event = payload + hash. Protocol never inspects content. Structure emerges where participants agree.",
    ascii: "[hdr][hash][░░░░░]\nopaque payload",
  },
  {
    id: "P.07",
    title: "Optionally ordered",
    body: "Ordering is per-stream, not global. Unordered path is the fast path. Causal ordering available where streams need it. Cost paid only by streams that require it.",
    ascii: "e₁ → e₂ → e₃\nchain.verify()",
  },
  {
    id: "P.08",
    title: "Optionally typed",
    body: "The protocol doesn't care what's in the payload. Behavior plane can. Typing is a local agreement between nodes, not a network requirement.",
    ascii: "type ∈ peer-pair\nnot network",
  },
  {
    id: "P.09",
    title: "Native backpressure",
    body: "Nodes drop without reply. Not a failure mode — the design. The proximity graph makes silence a signal. Neighbors know within a heartbeat interval.",
    ascii: "silent → suspect\nsuspect → reroute",
  },
];

function PropertiesSection() {
  return (
    <section className="border-b border-line px-6 py-20">
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
        transfer or wire latency. The software layer is what these benchmarks
        prove is no longer the bottleneck.
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
    <section className="border-b border-line px-6 py-20">
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
          What moved wasn&apos;t a copy.{" "}
          <strong className="text-accent font-medium">
            It was the thing itself
          </strong>
          , carried across.
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

const GROUP_CARDS: readonly GroupCard[] = [
  {
    id: "▸ GRP.01",
    name: "replica",
    meta: "N interchangeable copies · load-balanced",
    ascii: (
      <>
        members 0..N{"\n"}
        {"   ▶ "}
        <span className="text-accent">all active</span>
        {"\n"}
        {"   ▶ deterministic identity\n   ▶ stateless work\n"}
        load balancer fans out{"\n"}
        event → any member → result
      </>
    ),
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
    ascii: (
      <>
        parent @ seq=42{"\n"}
        {"   ├─▶ fork.A "}
        <span className="text-accent">(sentinel)</span>
        {"\n"}
        {"   ├─▶ fork.B "}
        <span className="text-accent">(sentinel)</span>
        {"\n"}
        {"   └─▶ fork.C "}
        <span className="text-accent">(sentinel)</span>
        {"\n"}
        each chain diverges{"\n"}
        verifiable lineage to parent
      </>
    ),
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
    ascii: (
      <>
        active{"   "}
        <span className="text-accent">●</span> processing seq=102{"\n"}
        standby{"  "}
        <span className="text-ink-faint">○</span> synced_through=98{"\n"}
        standby{"  "}
        <span className="text-ink-faint">○</span> synced_through=101{"\n\n"}
        active fails → promote(member 1){"\n"}
        {
          "   replays buffered events → seq=103\n   member 1 is now authoritative"
        }
      </>
    ),
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
      <SectionLabel>§06 / compute runtime // new</SectionLabel>
      <DisplayHeading>
        programs whose
        <br />
        identity survives
        <br />
        their hardware.
      </DisplayHeading>

      <div className="border border-accent-dim bg-accent/[0.03] px-5 py-4 mb-10 flex items-center gap-[18px] text-[11px] text-ink-dim tracking-[0.05em] flex-wrap">
        <span className="bg-accent text-bg px-2.5 py-1 font-bold tracking-[0.18em] text-[10px]">
          NEW
        </span>
        <span>
          <b className="text-ink font-medium">The compute runtime is live.</b>{" "}
          Stateful programs that live on the mesh, not on a machine. They have
          cryptographic identity, a verifiable history, and they move between
          nodes mid-execution without anyone noticing.
        </span>
        <span className="ml-auto">
          subprotocol{" "}
          <code className="text-accent bg-accent/[0.06] px-1.5 py-0.5 font-mono">
            0x0500
          </code>
        </span>
      </div>

      <p className="text-[16px] text-ink max-w-[740px] leading-[1.6] font-light mb-12">
        Every existing runtime binds programs to hardware. AWS Lambda is
        stateless because state binds you to a database. Temporal is stateful,
        but the workflow lives inside the cluster you bought. Erlang actors are
        addressable, but only inside one VM.{" "}
        <strong className="text-ink font-medium">
          &quot;Move this program to a different cluster&quot; is not a
          primitive any of them expose.
        </strong>
      </p>

      <p className="text-[16px] text-ink max-w-[740px] leading-[1.6] font-light mb-12 -mt-8">
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
      <SuperpositionViz />
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

function DaemonCaseBlock() {
  return (
    <div className="grid grid-cols-1 lg:grid-cols-[1.1fr_0.9fr] gap-8 my-12 items-start">
      <div className="border border-line bg-bg-2 overflow-hidden">
        <div className="bg-bg border-b border-line px-3.5 py-2 text-[10px] text-ink-dim tracking-[0.12em] uppercase flex justify-between items-center">
          <span>
            <span className="text-accent font-semibold">CASE</span> · trading
            agent · NYSE colo
          </span>
          <span className="inline-flex gap-1">
            <span className="frame-dot-r w-[7px] h-[7px] rounded-full" />
            <span className="frame-dot-y w-[7px] h-[7px] rounded-full" />
            <span className="frame-dot-g w-[7px] h-[7px] rounded-full" />
          </span>
        </div>
        <pre className="px-5 py-4 text-[12px] leading-[1.7] text-ink overflow-x-auto font-mono">
          <span className="cm">
            // node A is failing — daemon migrates to node B
          </span>
          {"\n"}
          <span className="kw">let</span> daemon = Daemon::
          <span className="fn">new</span>(
          <span className="ty">TraderConfig</span> {"{"}
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
          migration · 6 phases
        </h3>
        <span className="text-[10px] text-ink-dim tracking-[0.12em] uppercase">
          strict order · <b className="text-accent">~280ns total</b>
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
  meta: string;
}

const INSTALL_CARDS: readonly InstallCard[] = [
  {
    lang: "Rust",
    ext: ".rs",
    cmd: "$ cargo add ai2070-net-sdk",
    meta: "crate: ai2070-net-sdk",
  },
  {
    lang: "TypeScript",
    ext: ".ts",
    cmd: "$ npm i @ai2070/net-sdk\n       @ai2070/net",
    meta: "scope: @ai2070",
  },
  {
    lang: "Python",
    ext: ".py",
    cmd: "$ pip install ai2070-net-sdk",
    meta: "dist: ai2070-net-sdk",
  },
  {
    lang: "Go",
    ext: ".go",
    cmd: "$ go get github.com/\n  ai-2070/net/go",
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
        one wire.
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
        {INSTALL_CARDS.map((c) => (
          <div
            key={c.lang}
            className="border border-line p-5 bg-bg transition-colors hover:border-accent-dim cursor-pointer"
          >
            <div className="flex items-center justify-between mb-4">
              <span className="text-[11px] text-ink tracking-[0.15em] uppercase font-semibold">
                {c.lang}
              </span>
              <span className="text-[10px] text-accent border border-accent-dim px-1.5 py-0.5">
                {c.ext}
              </span>
            </div>
            <pre className="bg-bg-2 p-3 text-[11px] text-accent border-l-2 border-accent overflow-x-auto font-mono leading-[1.5]">
              {c.cmd}
            </pre>
            <div className="text-ink-dim text-[10px] mt-2.5">{c.meta}</div>
          </div>
        ))}
      </div>

      <p className="mt-7 text-[11px] text-ink-dim">
        // C bindings via <span className="text-accent">net.h</span> — build
        cdylib with{" "}
        <span className="text-accent">
          cargo build --release --features ffi,net
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
    tag: "▸ 0x01 ─ ai runtime",
    title: "AI Runtime",
    body: "The original use case. Token streams, tool-call results, guardrail decisions, and consensus votes flowing across heterogeneous GPU nodes. The mesh is the runtime.",
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
    tag: "▸ 0x04 ─ disaster response",
    title: "Disaster Response",
    body: "Phones, drones, portable radios forming a mesh with no surviving infrastructure. The mesh forms from whatever is present and routes around whatever is gone.",
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
    tag: "▸ 0x08 ─ precision ag",
    title: "Precision Agriculture",
    body: "Tractors, drones, soil sensors, weather stations forming a field mesh. The field is the network. No cloud round-trip required.",
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
        Anywhere coordination latency matters. Anywhere the cloud round-trip is
        too slow. Anywhere there&apos;s no central infrastructure to route
        through.
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
            <h3 className="font-head text-[18px] leading-tight mb-2.5 tracking-[0.04em] text-ink lowercase">
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
            There is no single point to breach because the wall is the mesh
            itself.
          </strong>
        </p>
      </div>
    </section>
  );
}

function ClosingSection() {
  return (
    <section className="border-b border-line px-6 py-20">
      <SectionLabel>§11 / post-cloud</SectionLabel>
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
              Net moves compute closer to the data and the work.
            </strong>
          </p>
          <p className="text-ink-dim text-[13px] leading-[1.7] mb-4">
            Cloud adds a trusted intermediary by definition. Net has no
            intermediaries. Relay nodes forward encrypted bytes they cannot
            read. There is no Cloudflare, no AWS, no Azure in the path because
            the path is yours.
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

const FOOTER_SPEC: ReadonlyArray<{ href: string; label: string }> = [
  { href: "#what", label: "Why not best-effort" },
  { href: "#bench", label: "Benchmarks" },
  { href: "#wall", label: "The Blackwall" },
  { href: "#install", label: "SDKs" },
];

const FOOTER_COMPONENTS: ReadonlyArray<{ href: string; label: string }> = [
  { href: "#components", label: "nRPC // request/response" },
  { href: "#components", label: "RedEX // log" },
  { href: "#components", label: "CortEX // fold" },
  { href: "#components", label: "NetDB // façade" },
  { href: "#runtime", label: "Mikoshi // migration" },
];

const FOOTER_RESOURCES: ReadonlyArray<{ href: string; label: string }> = [
  { href: "#", label: "Crate README" },
  { href: "#", label: "BENCHMARKS.md" },
  { href: "#", label: "RFC archive" },
  { href: "#", label: "Source (GitHub)" },
];

function Footer() {
  return (
    <footer className="px-6 pt-16 pb-7 border-t border-accent-dim">
      <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-[2fr_1fr_1fr_1fr] gap-8 mb-12">
        <div>
          <div className="logo-mark font-display text-[22px] text-ink tracking-[0.1em] flex items-baseline gap-2.5 mb-4">
            net{" "}
            <span className="font-mono text-[9px] text-accent tracking-[0.15em] font-semibold">
              // NET 2070
            </span>
          </div>
          <p className="text-ink-dim text-[12px] leading-[1.6] max-w-[380px]">
            Network Event Transport. A latency-first encrypted mesh protocol.
          </p>
        </div>
        <FooterColumn title="Spec" items={FOOTER_SPEC} />
        <FooterColumn title="Components" items={FOOTER_COMPONENTS} />
        <FooterColumn title="Resources" items={FOOTER_RESOURCES} />
      </div>

      <div className="border-t border-line pt-6 flex justify-between text-[10px] text-ink-dim tracking-[0.1em] flex-wrap gap-4">
        <span>© 2026 — NET // PROTOCOL.0x4E45·54</span>
        <span>
          <span className="text-accent">▸</span> mesh status:{" "}
          <span className="text-accent">ONLINE</span> · 14,872 nodes observable
        </span>
        <span>// AI 2070</span>
      </div>
    </footer>
  );
}

function FooterColumn({
  title,
  items,
}: {
  title: string;
  items: ReadonlyArray<{ href: string; label: string }>;
}) {
  return (
    <div>
      <h5 className="text-[10px] tracking-[0.18em] text-ink-dim uppercase mb-4 font-medium">
        {title}
      </h5>
      <ul className="list-none space-y-2">
        {items.map((it) => (
          <li key={it.label}>
            <a
              href={it.href}
              className="text-ink no-underline text-[12px] hover:text-accent transition-colors"
            >
              {it.label}
            </a>
          </li>
        ))}
      </ul>
    </div>
  );
}

export default function Home() {
  return (
    <>
      <TopStatusBar />
      <NavBar />
      <main className="pt-20 max-w-[1440px] mx-auto">
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
        <ClosingSection />
        <FooterDivider />
        <Footer />
      </main>
    </>
  );
}
