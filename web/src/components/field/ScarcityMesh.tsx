"use client";

import { useEffect, useRef } from "react";

/**
 * §01, drawn: the first four ARPANET nodes (December 1969, with their real
 * links) grow, as the section scrolls past, into a dense flat mesh in which
 * those four are no longer special: no centre, every node equal. Once the
 * mesh is full, pingwaves ripple through it.
 *
 * Canvas, scroll-linked. Paused off-screen; reduced motion shows the 1969
 * graph and the full mesh as one still, with no ripples.
 */

interface MeshNode {
  x: number;
  y: number;
  order: number; // 0..1, when it joins as the section scrolls
  links: number[];
  seed: boolean;
  label?: string;
}

// December 1969: UCLA, SRI, UCSB and Utah, linked UCLA–SRI, UCLA–UCSB,
// SRI–UCSB, SRI–Utah. Positions are loosely geographic (west coast left).
const SEEDS: ReadonlyArray<{ label: string; x: number; y: number }> = [
  { label: "UCLA", x: 0.4, y: 0.6 },
  { label: "SRI", x: 0.46, y: 0.4 },
  { label: "UCSB", x: 0.33, y: 0.5 },
  { label: "UTAH", x: 0.62, y: 0.42 },
];
const SEED_LINKS: ReadonlyArray<readonly [number, number]> = [
  [0, 1],
  [0, 2],
  [1, 2],
  [1, 3],
];

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

export function ScarcityMesh() {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const countRef = useRef<HTMLSpanElement>(null);
  const yearRef = useRef<HTMLSpanElement>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    const reduce = window.matchMedia(
      "(prefers-reduced-motion: reduce)",
    ).matches;
    const mono =
      getComputedStyle(document.documentElement)
        .getPropertyValue("--font-jetbrains-mono")
        .trim() || "monospace";

    let W = 0;
    let H = 0;
    let dpr = 1;
    let nodes: MeshNode[] = [];
    let progress = reduce ? 1 : 0;
    let shown = progress;
    const waves: Array<{ x: number; y: number; t: number }> = [];
    let lastWave = 0;

    const build = (): void => {
      const r = canvas.getBoundingClientRect();
      W = Math.max(1, r.width);
      H = Math.max(1, r.height);
      dpr = Math.min(window.devicePixelRatio || 1, 2);
      canvas.width = Math.round(W * dpr);
      canvas.height = Math.round(H * dpr);

      const rand = rng(1969);
      nodes = SEEDS.map((s) => ({
        x: s.x * W,
        y: s.y * H,
        order: 0,
        links: [],
        seed: true,
        label: s.label,
      }));
      SEED_LINKS.forEach(([a, b]) => nodes[b]?.links.push(a));

      // The grown mesh: a jittered lattice, joining from the 1969 seed
      // outward, so the network visibly spreads from where it began.
      const gap = W < 420 ? 17 : 21;
      const cx = 0.45 * W;
      const cy = 0.5 * H;
      const maxD = Math.hypot(Math.max(cx, W - cx), Math.max(cy, H - cy));
      for (let y = gap * 0.6; y < H - gap * 0.4; y += gap * 0.87) {
        const row = Math.round(y / (gap * 0.87));
        for (
          let x = gap * 0.5 + (row % 2) * gap * 0.5;
          x < W - gap * 0.3;
          x += gap
        ) {
          const px = x + (rand() - 0.5) * gap * 0.55;
          const py = y + (rand() - 0.5) * gap * 0.55;
          if (nodes.some((n) => n.seed && Math.hypot(n.x - px, n.y - py) < gap))
            continue;
          const d = Math.hypot(px - cx, py - cy) / maxD;
          nodes.push({
            x: px,
            y: py,
            order: Math.min(1, 0.06 + d * 0.8 + rand() * 0.14),
            links: [],
            seed: false,
          });
        }
      }
      // each node links to its two or three nearest neighbours that joined
      // before it: no hub, only peers
      for (let i = SEEDS.length; i < nodes.length; i++) {
        const n = nodes[i]!;
        const near: Array<[number, number]> = [];
        for (let j = 0; j < nodes.length; j++) {
          if (j === i) continue;
          const m = nodes[j]!;
          if (m.order > n.order) continue;
          const d = Math.hypot(m.x - n.x, m.y - n.y);
          if (d < gap * 1.9) near.push([d, j]);
        }
        near.sort((a, b) => a[0] - b[0]);
        n.links = near.slice(0, 2 + (i % 2)).map((p) => p[1]);
      }
    };

    const ease = (t: number): number => 1 - Math.pow(1 - t, 3);

    const draw = (now: number): void => {
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      ctx.clearRect(0, 0, W, H);
      const p = shown;
      const seedEmphasis = reduce
        ? 0.7
        : 1 - ease(Math.min(1, Math.max(0, (p - 0.35) / 0.45)));

      // links
      for (let i = 0; i < nodes.length; i++) {
        const n = nodes[i]!;
        const vis = Math.min(1, Math.max(0, (p - n.order) / 0.08));
        if (vis <= 0) continue;
        for (const j of n.links) {
          const m = nodes[j]!;
          const both = n.seed && m.seed;
          const a = both ? 0.25 + 0.6 * seedEmphasis : 0.24 * vis;
          ctx.strokeStyle = both
            ? `rgba(196,255,61,${a.toFixed(3)})`
            : `rgba(150,154,145,${a.toFixed(3)})`;
          ctx.lineWidth = both ? 1 + seedEmphasis : 0.7;
          ctx.beginPath();
          ctx.moveTo(m.x, m.y);
          ctx.lineTo(n.x, n.y);
          ctx.stroke();
        }
      }

      // pingwaves
      for (const w of waves) {
        const age = (now - w.t) / 1600;
        const rad = age * Math.max(W, H) * 0.7;
        ctx.strokeStyle = `rgba(196,255,61,${(0.35 * (1 - age)).toFixed(3)})`;
        ctx.lineWidth = 1;
        ctx.beginPath();
        ctx.arc(w.x, w.y, rad, 0, Math.PI * 2);
        ctx.stroke();
      }

      // nodes
      let count = 0;
      for (const n of nodes) {
        const vis = Math.min(1, Math.max(0, (p - n.order) / 0.08));
        if (vis <= 0) continue;
        count++;
        let lit = 0;
        for (const w of waves) {
          const age = (now - w.t) / 1600;
          const rad = age * Math.max(W, H) * 0.7;
          const d = Math.hypot(n.x - w.x, n.y - w.y);
          lit = Math.max(
            lit,
            Math.max(0, 1 - Math.abs(d - rad) / 18) * (1 - age),
          );
        }
        if (n.seed) {
          const s = 2 + 3.5 * seedEmphasis;
          ctx.fillStyle = `rgba(196,255,61,${(0.55 + 0.45 * seedEmphasis).toFixed(3)})`;
          ctx.fillRect(n.x - s, n.y - s, s * 2, s * 2);
          if (seedEmphasis > 0.02 && n.label) {
            ctx.font = `600 11px ${mono}`;
            ctx.fillStyle = `rgba(230,232,224,${(0.9 * seedEmphasis).toFixed(3)})`;
            ctx.fillText(n.label, n.x + 9, n.y - 7);
          }
        } else {
          const s = 2 + lit * 1.5;
          ctx.fillStyle =
            lit > 0.05
              ? `rgba(196,255,61,${(0.4 + 0.6 * lit).toFixed(3)})`
              : `rgba(230,232,224,${(0.6 * vis).toFixed(3)})`;
          ctx.fillRect(n.x - s / 2, n.y - s / 2, s, s);
        }
      }

      if (countRef.current) countRef.current.textContent = String(count);
      if (yearRef.current)
        yearRef.current.textContent = p < 0.5 ? "1969" : "now";
    };

    // progress follows the section through the viewport
    const section = canvas.closest("section") ?? canvas;
    const readProgress = (): void => {
      if (reduce) return;
      const r = section.getBoundingClientRect();
      const vh = window.innerHeight;
      const t = (vh * 0.5 - r.top) / (r.height * 0.8);
      progress = Math.min(1, Math.max(0, t));
    };

    let raf = 0;
    let running = false;
    const frame = (now: number): void => {
      shown += (progress - shown) * 0.08;
      if (Math.abs(progress - shown) < 0.0005) shown = progress;
      if (shown > 0.92 && now - lastWave > 1400) {
        const pick = nodes[Math.floor(Math.random() * nodes.length)];
        if (pick) waves.push({ x: pick.x, y: pick.y, t: now });
        lastWave = now;
      }
      for (let i = waves.length - 1; i >= 0; i--) {
        if (now - waves[i]!.t > 1600) waves.splice(i, 1);
      }
      draw(now);
      raf = requestAnimationFrame(frame);
    };
    const start = (): void => {
      if (running || reduce) return;
      running = true;
      raf = requestAnimationFrame(frame);
    };
    const stop = (): void => {
      running = false;
      cancelAnimationFrame(raf);
    };

    build();
    readProgress();
    shown = progress;
    draw(performance.now());
    if (!reduce) start();

    const ro = new ResizeObserver(() => {
      build();
      draw(performance.now());
    });
    ro.observe(canvas);
    const io = new IntersectionObserver(
      ([e]) => (e?.isIntersecting ? start() : stop()),
      { rootMargin: "100px" },
    );
    io.observe(canvas);
    window.addEventListener("scroll", readProgress, { passive: true });

    return () => {
      stop();
      ro.disconnect();
      io.disconnect();
      window.removeEventListener("scroll", readProgress);
    };
  }, []);

  return (
    <div data-ui className="slab overflow-hidden">
      <div className="flex items-center justify-between border-b border-line px-5 py-3 font-mono text-[12px] tracking-[0.16em] uppercase text-ink-dim">
        <span>
          arpanet <span className="text-ink-faint">→</span> net
        </span>
        <span className="tabular-nums">
          <span ref={yearRef} className="text-ink">
            1969
          </span>
          <span className="text-ink-faint"> · </span>
          nodes{" "}
          <span ref={countRef} className="text-accent">
            4
          </span>
        </span>
      </div>
      <canvas
        ref={canvasRef}
        aria-label="The four nodes of the 1969 ARPANET grow into a flat mesh of many equal nodes."
        role="img"
        className="block w-full aspect-[5/4]"
      />
    </div>
  );
}
