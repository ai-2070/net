"use client";

import Link from "next/link";
import { useEffect, useRef, useState, useSyncExternalStore } from "react";

// Drop a looping reel of multiplayer gameplay into `public/games/` and point
// this at it (e.g. "/games/hero-reel.mp4") and the hero plays it. While it is
// null no <video> is rendered, so the page makes no request for a missing
// file; a rendered stand-in (below) cuts between camera angles over a small
// networked battle instead, so the hero never sits empty.
export const HERO_REEL_SRC: string | null = null;

// ── rendered stand-in reel ──────────────────────────────────────────────────

interface Ship {
  x: number;
  z: number;
  heading: number;
  speed: number;
  turn: number;
  color: string;
  trail: Array<{ x: number; z: number }>;
  hitT: number;
}

interface Shot {
  x: number;
  z: number;
  dx: number;
  dz: number;
  life: number;
  color: string;
}

interface Cam {
  height: number;
  dist: number;
  pitch: number;
  orbit: number;
}

const CAMS: ReadonlyArray<Cam> = [
  { height: 5, dist: 20, pitch: 0.28, orbit: 0.05 }, // low sweep
  { height: 34, dist: 8, pitch: 1.2, orbit: 0.02 }, // overhead
  { height: 12, dist: 26, pitch: 0.45, orbit: -0.08 }, // wide orbit
];

const ARENA = 30;
const PALETTE = [
  "#c4ff3d",
  "#3df0ff",
  "#d4dcd0",
  "#ff5e3d",
  "#c4ff3d",
  "#3df0ff",
];

function makeShips(n: number): Ship[] {
  return Array.from({ length: n }, (_, i) => ({
    x: (Math.random() - 0.5) * ARENA,
    z: (Math.random() - 0.5) * ARENA,
    heading: Math.random() * Math.PI * 2,
    speed: 4 + Math.random() * 3,
    turn: (Math.random() - 0.5) * 1.2,
    color: PALETTE[i % PALETTE.length] ?? "#c4ff3d",
    trail: [],
    hitT: 0,
  }));
}

function ReelCanvas() {
  const ref = useRef<HTMLCanvasElement | null>(null);

  useEffect(() => {
    const canvas = ref.current;
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    const reduce = window.matchMedia(
      "(prefers-reduced-motion: reduce)",
    ).matches;
    const ships = makeShips(14);
    const shots: Shot[] = [];
    let camIdx = 0;
    let camStart = 0;
    let flash = 0;
    let angle = 0;
    let raf = 0;
    let last = performance.now();
    let w = 0;
    let h = 0;

    const resize = (): void => {
      const dpr = Math.min(window.devicePixelRatio || 1, 2);
      w = canvas.clientWidth;
      h = canvas.clientHeight;
      canvas.width = Math.round(w * dpr);
      canvas.height = Math.round(h * dpr);
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    };
    resize();

    const project = (
      x: number,
      y: number,
      z: number,
      cam: Cam,
    ): { sx: number; sy: number; s: number } | null => {
      // orbit the camera around the arena centre
      const ca = Math.cos(angle);
      const sa = Math.sin(angle);
      const rx = x * ca - z * sa;
      const rz = x * sa + z * ca;
      // camera sits at (0, height, dist) looking down by `pitch`
      const dy = y - cam.height;
      const dz = rz - cam.dist;
      const cp = Math.cos(cam.pitch);
      const sp = Math.sin(cam.pitch);
      const vy = dy * cp - dz * sp;
      const vz = -(dy * sp + dz * cp);
      if (vz < 0.5) return null;
      const f = (Math.min(w, h * 1.6) * 0.9) / vz;
      return { sx: w / 2 + rx * f, sy: h * 0.52 - vy * f, s: f };
    };

    const step = (dt: number, now: number): void => {
      angle += dt * (CAMS[camIdx]?.orbit ?? 0);
      if (now - camStart > 4200) {
        camIdx = (camIdx + 1) % CAMS.length;
        camStart = now;
        flash = 1;
        angle = Math.random() * Math.PI * 2;
      }
      flash = Math.max(0, flash - dt * 3);

      for (const s of ships) {
        s.heading += s.turn * dt;
        if (Math.random() < dt * 0.4) s.turn = (Math.random() - 0.5) * 1.6;
        s.x += Math.cos(s.heading) * s.speed * dt;
        s.z += Math.sin(s.heading) * s.speed * dt;
        // steer back inside the arena
        if (Math.abs(s.x) > ARENA / 2 || Math.abs(s.z) > ARENA / 2) {
          s.heading = Math.atan2(-s.z, -s.x) + (Math.random() - 0.5) * 0.6;
        }
        s.trail.push({ x: s.x, z: s.z });
        if (s.trail.length > 26) s.trail.shift();
        s.hitT = Math.max(0, s.hitT - dt * 2.5);
        if (Math.random() < dt * 0.9) {
          shots.push({
            x: s.x,
            z: s.z,
            dx: Math.cos(s.heading) * 22,
            dz: Math.sin(s.heading) * 22,
            life: 0.7,
            color: s.color,
          });
        }
      }
      for (let i = shots.length - 1; i >= 0; i--) {
        const sh = shots[i];
        if (!sh) continue;
        sh.x += sh.dx * dt;
        sh.z += sh.dz * dt;
        sh.life -= dt;
        for (const s of ships) {
          if (Math.hypot(s.x - sh.x, s.z - sh.z) < 0.9 && sh.life < 0.6) {
            s.hitT = 1;
            sh.life = 0;
          }
        }
        if (sh.life <= 0) shots.splice(i, 1);
      }
    };

    const draw = (): void => {
      const cam = CAMS[camIdx] ?? CAMS[0];
      if (!cam) return;
      ctx.fillStyle = "#070907";
      ctx.fillRect(0, 0, w, h);

      // floor grid
      ctx.lineWidth = 1;
      for (let i = -ARENA; i <= ARENA; i += 3) {
        const edge = i === -ARENA || i === ARENA;
        ctx.strokeStyle = edge
          ? "rgba(196,255,61,0.35)"
          : "rgba(196,255,61,0.07)";
        for (const [ax, az, bx, bz] of [
          [i, -ARENA, i, ARENA],
          [-ARENA, i, ARENA, i],
        ] as const) {
          const segs = 16;
          ctx.beginPath();
          let started = false;
          for (let k = 0; k <= segs; k++) {
            const t = k / segs;
            const p = project(ax + (bx - ax) * t, 0, az + (bz - az) * t, cam);
            if (!p) {
              started = false;
              continue;
            }
            if (!started) ctx.moveTo(p.sx, p.sy);
            else ctx.lineTo(p.sx, p.sy);
            started = true;
          }
          ctx.stroke();
        }
      }

      // trails
      for (const s of ships) {
        ctx.strokeStyle = s.color;
        for (let i = 1; i < s.trail.length; i++) {
          const a = s.trail[i - 1];
          const b = s.trail[i];
          if (!a || !b) continue;
          const pa = project(a.x, 0.3, a.z, cam);
          const pb = project(b.x, 0.3, b.z, cam);
          if (!pa || !pb) continue;
          ctx.globalAlpha = (i / s.trail.length) * 0.45;
          ctx.lineWidth = Math.max(0.5, pb.s * 0.12);
          ctx.beginPath();
          ctx.moveTo(pa.sx, pa.sy);
          ctx.lineTo(pb.sx, pb.sy);
          ctx.stroke();
        }
      }
      ctx.globalAlpha = 1;

      // shots
      for (const sh of shots) {
        const p = project(sh.x, 0.5, sh.z, cam);
        const q = project(sh.x - sh.dx * 0.04, 0.5, sh.z - sh.dz * 0.04, cam);
        if (!p || !q) continue;
        ctx.strokeStyle = sh.color;
        ctx.lineWidth = Math.max(1, p.s * 0.1);
        ctx.beginPath();
        ctx.moveTo(q.sx, q.sy);
        ctx.lineTo(p.sx, p.sy);
        ctx.stroke();
      }

      // ships, far to near
      const ordered = [...ships]
        .map((s) => ({ s, p: project(s.x, 0.5, s.z, cam) }))
        .filter(
          (o): o is { s: Ship; p: { sx: number; sy: number; s: number } } =>
            o.p !== null,
        )
        .sort((a, b) => a.p.s - b.p.s);
      for (const { s, p } of ordered) {
        const tip = project(
          s.x + Math.cos(s.heading) * 1.4,
          0.5,
          s.z + Math.sin(s.heading) * 1.4,
          cam,
        );
        const l = project(
          s.x + Math.cos(s.heading + 2.5) * 1.05,
          0.5,
          s.z + Math.sin(s.heading + 2.5) * 1.05,
          cam,
        );
        const r = project(
          s.x + Math.cos(s.heading - 2.5) * 1.05,
          0.5,
          s.z + Math.sin(s.heading - 2.5) * 1.05,
          cam,
        );
        if (!tip || !l || !r) continue;
        ctx.shadowColor = s.color;
        ctx.shadowBlur = 12 + s.hitT * 30;
        ctx.fillStyle = s.hitT > 0.5 ? "#ffffff" : s.color;
        ctx.beginPath();
        ctx.moveTo(tip.sx, tip.sy);
        ctx.lineTo(l.sx, l.sy);
        ctx.lineTo(p.sx, p.sy);
        ctx.lineTo(r.sx, r.sy);
        ctx.closePath();
        ctx.fill();
        ctx.shadowBlur = 0;
      }

      if (flash > 0) {
        ctx.fillStyle = `rgba(196,255,61,${flash * 0.12})`;
        ctx.fillRect(0, 0, w, h);
      }
    };

    const loop = (now: number): void => {
      const dt = Math.min(0.05, (now - last) / 1000);
      last = now;
      step(dt, now);
      draw();
      raf = requestAnimationFrame(loop);
    };

    // Resizing clears the bitmap. The loop repaints on its next frame; the
    // reduced-motion still frame has no loop, so it is repainted here.
    const onResize = (): void => {
      resize();
      if (reduce) draw();
    };
    window.addEventListener("resize", onResize);

    if (reduce) {
      for (let i = 0; i < 60; i++) step(1 / 30, 0);
      draw();
    } else {
      camStart = performance.now();
      raf = requestAnimationFrame(loop);
    }

    return () => {
      cancelAnimationFrame(raf);
      window.removeEventListener("resize", onResize);
    };
  }, []);

  return (
    <canvas ref={ref} className="absolute inset-0 w-full h-full" aria-hidden />
  );
}

// ── hero ────────────────────────────────────────────────────────────────────

const REDUCED_MOTION = "(prefers-reduced-motion: reduce)";

function subscribeReducedMotion(onChange: () => void): () => void {
  const mq = window.matchMedia(REDUCED_MOTION);
  mq.addEventListener("change", onChange);
  return () => mq.removeEventListener("change", onChange);
}

// The server snapshot says "reduce", so a background video is never in the
// server render and only mounts once the client has seen the visitor's setting.
export function usePrefersReducedMotion(): boolean {
  return useSyncExternalStore(
    subscribeReducedMotion,
    () => window.matchMedia(REDUCED_MOTION).matches,
    () => true,
  );
}

export function GamesHero() {
  const reduceMotion = usePrefersReducedMotion();
  const reelOn = HERO_REEL_SRC !== null && !reduceMotion;
  const [reelReady, setReelReady] = useState(false);
  // Set once the reel has fully faded in. The canvas underneath is hidden for
  // good from then on, so it is unmounted and its frame loop stops.
  const [reelShown, setReelShown] = useState(false);
  const showReel = reelOn && reelReady;

  return (
    <section
      id="hero"
      className="relative overflow-hidden border-b border-line min-h-[calc(100svh-80px)] flex flex-col"
    >
      {/* background reel */}
      <div className="absolute inset-0" aria-hidden>
        {reelOn && reelShown ? null : <ReelCanvas />}
        {reelOn ? (
          <video
            className={`absolute inset-0 w-full h-full object-cover transition-opacity duration-700 ${
              showReel ? "opacity-100" : "opacity-0"
            }`}
            src={HERO_REEL_SRC ?? undefined}
            autoPlay
            muted
            loop
            playsInline
            preload="metadata"
            onLoadedData={() => setReelReady(true)}
            onTransitionEnd={() => setReelShown(reelReady)}
            onError={() => {
              setReelReady(false);
              setReelShown(false);
            }}
          />
        ) : null}
        {/* legibility: darken left + bottom, keep the action visible on the right */}
        <div className="absolute inset-0 bg-[linear-gradient(90deg,rgba(10,12,10,0.94)_0%,rgba(10,12,10,0.78)_38%,rgba(10,12,10,0.25)_75%,rgba(10,12,10,0.4)_100%)]" />
        <div className="absolute inset-x-0 bottom-0 h-40 bg-[linear-gradient(0deg,#0a0c0a,transparent)]" />
        <div className="absolute inset-0 hero-scanlines pointer-events-none" />
      </div>

      {/* HUD corners */}
      <HudCorners />

      <div className="relative flex-1 flex flex-col justify-center w-full max-w-[1440px] mx-auto px-6 pt-12 pb-10">
        <div className="text-[10px] tracking-[0.15em] mb-7 flex flex-wrap gap-3 items-center">
          <span className="text-bg bg-accent px-2 py-[3px] font-semibold">
            NEW
          </span>
          <span className="text-accent border border-accent-dim px-2 py-[3px]">
            PLUG AND PLAY FOR THREE.JS
          </span>
        </div>

        <h1
          className="font-display leading-[0.9] tracking-[-0.02em] text-ink mb-6"
          style={{ fontSize: "clamp(48px, 7.4vw, 112px)" }}
        >
          your game.
          <br />
          <span className="text-accent game-glow">no server.</span>
        </h1>

        <p className="font-sans text-[20px] md:text-[22px] text-ink max-w-[620px] leading-[1.45]">
          Plug and play support for Three.js: turn any Three.js game into a
          multiplayer game, a co-op game, or an MMO.
        </p>

        <p className="font-sans text-[15px] text-ink-dim mt-4 max-w-[580px] leading-[1.6]">
          Players connect straight to each other whenever the network allows.
          There is no game server running the match. Free and open source.
        </p>

        <div className="mt-8 flex gap-3 flex-wrap items-center">
          <a
            href="#demo"
            className="btn-primary game-btn inline-flex items-center gap-2.5 px-6 py-3.5 text-[12px] tracking-[0.14em] uppercase font-semibold no-underline border border-accent bg-accent text-bg transition-all"
          >
            ▶ See the demos
          </a>
          <Link
            href="/docs/sdk/browser/quickstart"
            className="btn-ghost inline-flex items-center gap-2.5 px-6 py-3.5 text-[12px] tracking-[0.14em] uppercase font-semibold no-underline border border-ink-faint bg-bg/60 backdrop-blur-sm text-ink transition-all"
          >
            Start building <span className="text-sm">→</span>
          </Link>
        </div>
      </div>

      <div className="absolute top-5 right-6 hidden md:flex items-center gap-2 text-[10px] tracking-[0.14em] uppercase text-ink-dim">
        <span className="w-1.5 h-1.5 rounded-full bg-warn inline-block animate-pulse-dot" />
        {showReel ? "gameplay reel" : "preview render"}
      </div>
    </section>
  );
}

export function HudCorners({ className = "" }: { className?: string }) {
  const c = "absolute w-5 h-5 border-accent pointer-events-none";
  return (
    <div
      className={`absolute inset-3 pointer-events-none ${className}`}
      aria-hidden
    >
      <span className={`${c} top-0 left-0 border-t-2 border-l-2`} />
      <span className={`${c} top-0 right-0 border-t-2 border-r-2`} />
      <span className={`${c} bottom-0 left-0 border-b-2 border-l-2`} />
      <span className={`${c} bottom-0 right-0 border-b-2 border-r-2`} />
    </div>
  );
}
